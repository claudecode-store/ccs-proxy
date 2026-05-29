use std::net::SocketAddr;
use std::sync::Arc;

use anyhow::Context;
use axum::Router;
use axum::body::Body;
use axum::extract::FromRequestParts;
use axum::extract::Request;
use axum::extract::State;
use axum::extract::WebSocketUpgrade;
use axum::extract::ws::Message as AxumWsMessage;
use axum::extract::ws::WebSocket;
use axum::http::HeaderMap;
use axum::http::HeaderName;
use axum::http::HeaderValue;
use axum::http::Method;
use axum::http::StatusCode;
use axum::http::Uri;
use axum::http::header;
use axum::response::IntoResponse;
use axum::routing::any;
use futures_util::SinkExt;
use futures_util::StreamExt;
use reqwest::Client;
use tokio::net::TcpListener;
use tokio::net::TcpStream;
use tokio::sync::RwLock;
use tokio_tungstenite::MaybeTlsStream;
use tokio_tungstenite::WebSocketStream;
use tokio_tungstenite::connect_async;
use tokio_tungstenite::tungstenite;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::error::Error as TungsteniteError;
use tokio_tungstenite::tungstenite::protocol::Message as TungsteniteMessage;
use tracing::debug;
use tracing::error;
use tracing::info;
use tracing::warn;
use url::Url;

const HOP_BY_HOP_HEADERS: &[&str] = &[
    "connection",
    "keep-alive",
    "proxy-authenticate",
    "proxy-authorization",
    "te",
    "trailer",
    "transfer-encoding",
    "upgrade",
];
const CHATGPT_ACCOUNT_ID_HEADER: &str = "chatgpt-account-id";

#[derive(Clone)]
pub struct ProxyConfig {
    pub listen: SocketAddr,
    pub upstream_base_url: Url,
    pub upstream_prefix: String,
}

#[derive(Clone)]
struct AppState {
    config: ProxyConfig,
    client: Client,
    auth_headers: Arc<AuthHeaderCache>,
}

#[derive(Default)]
struct AuthHeaderCache {
    inner: RwLock<CachedAuthHeaders>,
}

#[derive(Default)]
struct CachedAuthHeaders {
    authorization: Option<HeaderValue>,
    chatgpt_account_id: Option<HeaderValue>,
}

#[derive(Debug, Clone, Copy)]
struct AuthHeaderState {
    authorization_present: bool,
    authorization_injected: bool,
    chatgpt_account_id_present: bool,
    chatgpt_account_id_injected: bool,
}

impl AuthHeaderState {
    fn authorization_effective(self) -> bool {
        self.authorization_present || self.authorization_injected
    }

    fn chatgpt_account_id_effective(self) -> bool {
        self.chatgpt_account_id_present || self.chatgpt_account_id_injected
    }
}

pub async fn serve(config: ProxyConfig) -> anyhow::Result<()> {
    let listener = TcpListener::bind(config.listen).await?;
    let local_addr = listener.local_addr()?;
    info!(
        listen = %local_addr,
        upstream = %config.upstream_base_url,
        upstream_prefix = %config.upstream_prefix,
        "ccs-proxy listening"
    );

    let state = AppState {
        config,
        client: Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .build()?,
        auth_headers: Arc::new(AuthHeaderCache::default()),
    };

    axum::serve(listener, app(state)).await?;
    Ok(())
}

fn app(state: AppState) -> Router {
    Router::new()
        .route("/healthz", any(healthz))
        .route("/", any(proxy_handler))
        .route("/{*path}", any(proxy_handler))
        .with_state(Arc::new(state))
}

async fn healthz() -> impl IntoResponse {
    (StatusCode::OK, "ok\n")
}

async fn proxy_handler(State(state): State<Arc<AppState>>, request: Request) -> impl IntoResponse {
    if is_websocket_upgrade(request.headers()) {
        let (mut parts, _body) = request.into_parts();
        let uri = parts.uri.clone();
        let mut headers = parts.headers.clone();
        let auth_header_state = state.auth_headers.apply(&mut headers).await;
        let upstream_url = match build_upstream_websocket_url(
            &state.config.upstream_base_url,
            &state.config.upstream_prefix,
            &uri,
        ) {
            Ok(url) => url,
            Err(err) => {
                error!(error = %err, "failed to build websocket upstream url");
                return (
                    StatusCode::BAD_GATEWAY,
                    format!("invalid upstream websocket url: {err}\n"),
                )
                    .into_response();
            }
        };
        let upstream_request = match build_upstream_websocket_request(
            &upstream_url,
            &headers,
            &state.config.upstream_base_url,
        ) {
            Ok(request) => request,
            Err(err) => {
                error!(error = %err, "failed to build websocket request");
                return (
                    StatusCode::BAD_GATEWAY,
                    format!("invalid upstream websocket request: {err}\n"),
                )
                    .into_response();
            }
        };
        debug!(upstream = %upstream_url, "connecting upstream websocket");
        let upstream = match connect_async(upstream_request).await {
            Ok((socket, _response)) => socket,
            Err(TungsteniteError::Http(response)) => {
                let status = response.status();
                let body_preview = websocket_error_body_preview(response.body().as_deref());
                let response = websocket_handshake_error_response(*response);
                warn!(
                    status = %status,
                    upstream = %upstream_url,
                    authorization_present = auth_header_state.authorization_effective(),
                    authorization_injected = auth_header_state.authorization_injected,
                    chatgpt_account_id_present = auth_header_state.chatgpt_account_id_effective(),
                    chatgpt_account_id_injected = auth_header_state.chatgpt_account_id_injected,
                    body = %body_preview,
                    "upstream websocket handshake returned HTTP response"
                );
                return response.into_response();
            }
            Err(err) => {
                error!(error = %err, upstream = %upstream_url, "failed to connect upstream websocket");
                return (
                    StatusCode::BAD_GATEWAY,
                    format!("failed to connect upstream websocket: {err}\n"),
                )
                    .into_response();
            }
        };

        let ws = match WebSocketUpgrade::from_request_parts(&mut parts, &()).await {
            Ok(ws) => ws,
            Err(err) => {
                return (
                    StatusCode::BAD_REQUEST,
                    format!("invalid websocket upgrade request: {err}\n"),
                )
                    .into_response();
            }
        };

        return ws
            .on_upgrade(move |socket| proxy_websocket(socket, upstream))
            .into_response();
    }

    match proxy_http(state, request).await {
        Ok(response) => response.into_response(),
        Err(err) => {
            let error_chain = format_error_chain(&err);
            error!(error = %error_chain, "http proxy request failed");
            (
                StatusCode::BAD_GATEWAY,
                format!("upstream request failed: {error_chain}\n"),
            )
                .into_response()
        }
    }
}

async fn proxy_http(
    state: Arc<AppState>,
    request: Request,
) -> anyhow::Result<axum::http::Response<Body>> {
    let (parts, body) = request.into_parts();
    let mut headers = parts.headers.clone();
    let auth_header_state = state.auth_headers.apply(&mut headers).await;
    let upstream_url = build_upstream_url(
        &state.config.upstream_base_url,
        &state.config.upstream_prefix,
        &parts.uri,
    )?;
    debug!(method = %parts.method, upstream = %upstream_url, "proxying http request");

    let upstream_url_for_error = upstream_url.clone();
    let mut builder = state.client.request(parts.method.clone(), upstream_url);
    builder = builder.headers(proxy_request_headers(
        &headers,
        &state.config.upstream_base_url,
    )?);
    if request_should_forward_body(&parts.method, &headers) {
        builder = builder.body(reqwest::Body::wrap_stream(body.into_data_stream()));
    }

    let upstream_response = builder
        .send()
        .await
        .with_context(|| format!("failed to send upstream request to {upstream_url_for_error}"))?;
    let status = upstream_response.status();
    if !status.is_success() {
        let headers = proxy_response_headers(upstream_response.headers());
        let body = upstream_response.bytes().await.with_context(|| {
            format!("failed to read upstream error response body from {upstream_url_for_error}")
        })?;
        warn!(
            method = %parts.method,
            status = %status,
            upstream = %upstream_url_for_error,
            authorization_present = auth_header_state.authorization_effective(),
            authorization_injected = auth_header_state.authorization_injected,
            chatgpt_account_id_present = auth_header_state.chatgpt_account_id_effective(),
            chatgpt_account_id_injected = auth_header_state.chatgpt_account_id_injected,
            body = %body_preview(&body),
            "upstream http response returned non-success status"
        );

        let mut response = axum::http::Response::builder().status(status);
        for (name, value) in headers.iter() {
            response = response.header(name, value);
        }
        return Ok(response.body(Body::from(body))?);
    }
    let headers = proxy_response_headers(upstream_response.headers());
    let body = Body::from_stream(upstream_response.bytes_stream());

    let mut response = axum::http::Response::builder().status(status);
    for (name, value) in headers.iter() {
        response = response.header(name, value);
    }

    Ok(response.body(body)?)
}

impl AuthHeaderCache {
    async fn apply(&self, headers: &mut HeaderMap) -> AuthHeaderState {
        let incoming_authorization = non_empty_header(headers, header::AUTHORIZATION).cloned();
        let incoming_chatgpt_account_id =
            non_empty_header(headers, CHATGPT_ACCOUNT_ID_HEADER).cloned();
        let authorization_present = incoming_authorization.is_some();
        let chatgpt_account_id_present = incoming_chatgpt_account_id.is_some();

        let mut cached = self.inner.write().await;
        if let Some(authorization) = incoming_authorization.as_ref() {
            let authorization_changed = cached
                .authorization
                .as_ref()
                .is_some_and(|cached| cached != authorization);
            cached.authorization = Some(authorization.clone());
            if authorization_changed && incoming_chatgpt_account_id.is_none() {
                cached.chatgpt_account_id = None;
            }
        }
        if let Some(chatgpt_account_id) = incoming_chatgpt_account_id.as_ref() {
            cached.chatgpt_account_id = Some(chatgpt_account_id.clone());
        }

        let mut state = AuthHeaderState {
            authorization_present,
            authorization_injected: false,
            chatgpt_account_id_present,
            chatgpt_account_id_injected: false,
        };

        if !authorization_present && let Some(authorization) = cached.authorization.as_ref() {
            headers.insert(header::AUTHORIZATION, authorization.clone());
            state.authorization_injected = true;
        }
        if !chatgpt_account_id_present
            && let Some(chatgpt_account_id) = cached.chatgpt_account_id.as_ref()
        {
            headers.insert(
                HeaderName::from_static(CHATGPT_ACCOUNT_ID_HEADER),
                chatgpt_account_id.clone(),
            );
            state.chatgpt_account_id_injected = true;
        }

        state
    }
}

fn non_empty_header<'a>(
    headers: &'a HeaderMap,
    name: impl axum::http::header::AsHeaderName,
) -> Option<&'a HeaderValue> {
    headers
        .get(name)
        .filter(|value| !value.as_bytes().is_empty())
}

fn websocket_handshake_error_response(
    response: tungstenite::http::Response<Option<Vec<u8>>>,
) -> axum::http::Response<Body> {
    let (parts, body) = response.into_parts();
    let status = StatusCode::from_u16(parts.status.as_u16()).unwrap_or(StatusCode::BAD_GATEWAY);
    let headers = proxy_response_headers(&parts.headers);

    let mut builder = axum::http::Response::builder().status(status);
    for (name, value) in headers.iter() {
        builder = builder.header(name, value);
    }

    builder
        .body(Body::from(body.unwrap_or_default()))
        .unwrap_or_else(|_| {
            axum::http::Response::builder()
                .status(StatusCode::BAD_GATEWAY)
                .body(Body::from("invalid upstream websocket error response\n"))
                .expect("static response builds")
        })
}

async fn proxy_websocket(
    downstream: WebSocket,
    upstream: WebSocketStream<MaybeTlsStream<TcpStream>>,
) {
    let (mut upstream_tx, mut upstream_rx) = upstream.split();
    let (mut downstream_tx, mut downstream_rx) = downstream.split();

    let upstream_to_downstream = async {
        while let Some(message) = upstream_rx.next().await {
            let message = message?;
            downstream_tx.send(to_axum_ws_message(message)).await?;
        }
        anyhow::Ok(())
    };

    let downstream_to_upstream = async {
        while let Some(message) = downstream_rx.next().await {
            let message = message?;
            upstream_tx.send(to_tungstenite_message(message)).await?;
        }
        anyhow::Ok(())
    };

    tokio::select! {
        result = upstream_to_downstream => {
            if let Err(err) = result {
                debug!(error = %err, "websocket upstream-to-downstream forwarding ended");
            }
        }
        result = downstream_to_upstream => {
            if let Err(err) = result {
                debug!(error = %err, "websocket downstream-to-upstream forwarding ended");
            }
        }
    }
}

fn is_websocket_upgrade(headers: &HeaderMap) -> bool {
    headers
        .get(header::UPGRADE)
        .and_then(|value| value.to_str().ok())
        .is_some_and(|value| value.eq_ignore_ascii_case("websocket"))
        && headers
            .get(header::CONNECTION)
            .and_then(|value| value.to_str().ok())
            .is_some_and(|value| {
                value
                    .split(',')
                    .any(|part| part.trim().eq_ignore_ascii_case("upgrade"))
            })
}

fn build_upstream_websocket_request(
    upstream_url: &Url,
    downstream_headers: &HeaderMap,
    upstream_base_url: &Url,
) -> anyhow::Result<tungstenite::http::Request<()>> {
    let mut request = upstream_url.as_str().into_client_request()?;
    let headers = request.headers_mut();
    let proxied_headers = proxy_websocket_headers(downstream_headers, upstream_base_url)?;
    for (name, value) in proxied_headers.iter() {
        headers.insert(name.clone(), value.clone());
    }
    Ok(request)
}

fn proxy_request_headers(
    headers: &HeaderMap,
    upstream_base_url: &Url,
) -> anyhow::Result<HeaderMap> {
    let mut result = HeaderMap::new();
    copy_end_to_end_headers(headers, &mut result);
    set_host_header(&mut result, upstream_base_url)?;
    Ok(result)
}

fn proxy_response_headers(headers: &HeaderMap) -> HeaderMap {
    let mut result = HeaderMap::new();
    copy_end_to_end_headers(headers, &mut result);
    result
}

fn proxy_websocket_headers(
    headers: &HeaderMap,
    upstream_base_url: &Url,
) -> anyhow::Result<HeaderMap> {
    let mut result = HeaderMap::new();
    for (name, value) in headers {
        if name == header::HOST || header_is_hop_by_hop(name) {
            continue;
        }
        result.append(name.clone(), value.clone());
    }
    set_host_header(&mut result, upstream_base_url)?;
    Ok(result)
}

fn request_should_forward_body(method: &Method, headers: &HeaderMap) -> bool {
    if *method == Method::GET || *method == Method::HEAD {
        return request_has_declared_body(headers);
    }
    true
}

fn request_has_declared_body(headers: &HeaderMap) -> bool {
    if headers.contains_key(header::TRANSFER_ENCODING) {
        return true;
    }

    headers.get_all(header::CONTENT_LENGTH).iter().any(|value| {
        value
            .to_str()
            .ok()
            .and_then(|value| value.trim().parse::<u64>().ok())
            .map_or(true, |content_length| content_length > 0)
    })
}

fn copy_end_to_end_headers(from: &HeaderMap, to: &mut HeaderMap) {
    let connection_tokens = connection_header_tokens(from);
    for (name, value) in from {
        if name == header::HOST
            || header_is_hop_by_hop(name)
            || connection_tokens
                .iter()
                .any(|token| token.eq_ignore_ascii_case(name.as_str()))
        {
            continue;
        }
        to.append(name.clone(), value.clone());
    }
}

fn connection_header_tokens(headers: &HeaderMap) -> Vec<String> {
    headers
        .get_all(header::CONNECTION)
        .iter()
        .filter_map(|value| value.to_str().ok())
        .flat_map(|value| value.split(','))
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_owned)
        .collect()
}

fn header_is_hop_by_hop(name: &HeaderName) -> bool {
    HOP_BY_HOP_HEADERS
        .iter()
        .any(|header| name.as_str().eq_ignore_ascii_case(header))
}

fn set_host_header(headers: &mut HeaderMap, upstream_base_url: &Url) -> anyhow::Result<()> {
    let host = upstream_host_header(upstream_base_url)?;
    headers.insert(header::HOST, HeaderValue::from_str(&host)?);
    Ok(())
}

fn upstream_host_header(upstream_base_url: &Url) -> anyhow::Result<String> {
    let host = upstream_base_url
        .host_str()
        .ok_or_else(|| anyhow::anyhow!("upstream base url is missing host"))?;
    Ok(match upstream_base_url.port() {
        Some(port) => format!("{host}:{port}"),
        None => host.to_string(),
    })
}

fn build_upstream_url(
    upstream_base_url: &Url,
    upstream_prefix: &str,
    uri: &Uri,
) -> anyhow::Result<Url> {
    let mut url = upstream_base_url.clone();
    let path = combine_paths(upstream_base_url.path(), upstream_prefix, uri.path());
    url.set_path(&path);
    url.set_query(uri.query());
    Ok(url)
}

fn build_upstream_websocket_url(
    upstream_base_url: &Url,
    upstream_prefix: &str,
    uri: &Uri,
) -> anyhow::Result<Url> {
    let mut url = build_upstream_url(upstream_base_url, upstream_prefix, uri)?;
    let scheme = match upstream_base_url.scheme() {
        "https" => "wss",
        "http" => "ws",
        other => anyhow::bail!("unsupported upstream scheme `{other}`"),
    };
    url.set_scheme(scheme)
        .map_err(|_| anyhow::anyhow!("failed to set websocket scheme"))?;
    Ok(url)
}

fn combine_paths(base_path: &str, upstream_prefix: &str, request_path: &str) -> String {
    let mut segments = Vec::new();
    push_path_segment(&mut segments, base_path);
    push_path_segment(&mut segments, upstream_prefix);
    push_path_segment(&mut segments, request_path);
    if segments.is_empty() {
        "/".to_string()
    } else {
        format!("/{}", segments.join("/"))
    }
}

fn push_path_segment(segments: &mut Vec<String>, path: &str) {
    let trimmed = path.trim_matches('/');
    if !trimmed.is_empty() {
        segments.push(trimmed.to_string());
    }
}

fn format_error_chain(err: &anyhow::Error) -> String {
    err.chain()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join(": ")
}

fn websocket_error_body_preview(body: Option<&[u8]>) -> String {
    let Some(body) = body else {
        return String::new();
    };
    body_preview(body)
}

fn body_preview(body: &[u8]) -> String {
    const MAX_PREVIEW_BYTES: usize = 256;
    let mut preview = String::from_utf8_lossy(&body[..body.len().min(MAX_PREVIEW_BYTES)])
        .replace('\n', "\\n")
        .replace('\r', "\\r");
    if body.len() > MAX_PREVIEW_BYTES {
        preview.push_str("...");
    }
    preview
}

fn to_tungstenite_message(message: AxumWsMessage) -> TungsteniteMessage {
    match message {
        AxumWsMessage::Text(text) => TungsteniteMessage::Text(text.to_string().into()),
        AxumWsMessage::Binary(bytes) => TungsteniteMessage::Binary(bytes),
        AxumWsMessage::Ping(bytes) => TungsteniteMessage::Ping(bytes),
        AxumWsMessage::Pong(bytes) => TungsteniteMessage::Pong(bytes),
        AxumWsMessage::Close(frame) => {
            TungsteniteMessage::Close(frame.map(|frame| tungstenite::protocol::CloseFrame {
                code: tungstenite::protocol::frame::coding::CloseCode::from(frame.code),
                reason: frame.reason.to_string().into(),
            }))
        }
    }
}

fn to_axum_ws_message(message: TungsteniteMessage) -> AxumWsMessage {
    match message {
        TungsteniteMessage::Text(text) => AxumWsMessage::Text(text.to_string().into()),
        TungsteniteMessage::Binary(bytes) => AxumWsMessage::Binary(bytes),
        TungsteniteMessage::Ping(bytes) => AxumWsMessage::Ping(bytes),
        TungsteniteMessage::Pong(bytes) => AxumWsMessage::Pong(bytes),
        TungsteniteMessage::Close(frame) => {
            AxumWsMessage::Close(frame.map(|frame| axum::extract::ws::CloseFrame {
                code: u16::from(frame.code),
                reason: frame.reason.to_string().into(),
            }))
        }
        TungsteniteMessage::Frame(_) => AxumWsMessage::Close(Some(axum::extract::ws::CloseFrame {
            code: axum::extract::ws::close_code::ERROR,
            reason: "unsupported raw websocket frame".into(),
        })),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::Method;
    use axum::response::Response;
    use axum::routing::{any, get};
    use http_body_util::BodyExt;
    use serde_json::json;
    use tokio::net::TcpListener;
    use tower::ServiceExt;

    #[test]
    fn build_upstream_url_preserves_path_and_query() {
        let upstream = Url::parse("https://example.com/root").unwrap();
        let uri: Uri = "/backend-api/codex/responses?limit=100".parse().unwrap();

        let url = build_upstream_url(&upstream, "/routing-prefix", &uri).unwrap();

        assert_eq!(
            url.as_str(),
            "https://example.com/root/routing-prefix/backend-api/codex/responses?limit=100"
        );
    }

    #[test]
    fn build_upstream_url_does_not_rewrite_request_path() {
        let upstream = Url::parse("https://example.com").unwrap();
        let uri: Uri =
            "/agents/codex-room/room-1/backend-api/codex/wham/remote/control/server?cursor=1"
                .parse()
                .unwrap();

        let url = build_upstream_url(&upstream, "", &uri).unwrap();

        assert_eq!(
            url.as_str(),
            "https://example.com/agents/codex-room/room-1/backend-api/codex/wham/remote/control/server?cursor=1"
        );
    }

    #[test]
    fn build_upstream_url_works_without_prefix() {
        let upstream = Url::parse("https://example.com").unwrap();
        let uri: Uri = "/backend-api/codex".parse().unwrap();

        let url = build_upstream_url(&upstream, "", &uri).unwrap();

        assert_eq!(url.as_str(), "https://example.com/backend-api/codex");
    }

    #[test]
    fn build_upstream_websocket_url_switches_scheme() {
        let https = Url::parse("https://example.com").unwrap();
        let http = Url::parse("http://example.com").unwrap();
        let uri: Uri = "/backend-api/wham/remote/control/server".parse().unwrap();

        assert_eq!(
            build_upstream_websocket_url(&https, "", &uri)
                .unwrap()
                .as_str(),
            "wss://example.com/backend-api/wham/remote/control/server"
        );
        assert_eq!(
            build_upstream_websocket_url(&http, "", &uri)
                .unwrap()
                .as_str(),
            "ws://example.com/backend-api/wham/remote/control/server"
        );
    }

    #[test]
    fn proxy_headers_remove_hop_by_hop_and_connection_tokens() {
        let upstream = Url::parse("https://example.com:8443").unwrap();
        let mut headers = HeaderMap::new();
        headers.insert(header::HOST, HeaderValue::from_static("localhost:8000"));
        headers.insert(
            header::CONNECTION,
            HeaderValue::from_static("keep-alive, x-remove-me"),
        );
        headers.insert(header::UPGRADE, HeaderValue::from_static("websocket"));
        headers.insert(
            header::AUTHORIZATION,
            HeaderValue::from_static("Bearer token"),
        );
        headers.insert("x-remove-me", HeaderValue::from_static("remove"));
        headers.insert("x-keep-me", HeaderValue::from_static("keep"));

        let result = proxy_request_headers(&headers, &upstream).unwrap();

        assert_eq!(result.get(header::HOST).unwrap(), "example.com:8443");
        assert_eq!(result.get(header::AUTHORIZATION).unwrap(), "Bearer token");
        assert_eq!(result.get("x-keep-me").unwrap(), "keep");
        assert!(!result.contains_key(header::CONNECTION));
        assert!(!result.contains_key(header::UPGRADE));
        assert!(!result.contains_key("x-remove-me"));
    }

    #[test]
    fn proxy_websocket_headers_keep_authorization() {
        let upstream = Url::parse("https://example.com").unwrap();
        let mut headers = HeaderMap::new();
        headers.insert(header::HOST, HeaderValue::from_static("localhost:8000"));
        headers.insert(header::CONNECTION, HeaderValue::from_static("upgrade"));
        headers.insert(header::UPGRADE, HeaderValue::from_static("websocket"));
        headers.insert(
            header::AUTHORIZATION,
            HeaderValue::from_static("Bearer token"),
        );
        headers.insert("x-keep-me", HeaderValue::from_static("keep"));

        let result = proxy_websocket_headers(&headers, &upstream).unwrap();

        assert_eq!(result.get(header::HOST).unwrap(), "example.com");
        assert_eq!(result.get(header::AUTHORIZATION).unwrap(), "Bearer token");
        assert_eq!(result.get("x-keep-me").unwrap(), "keep");
        assert!(!result.contains_key(header::CONNECTION));
        assert!(!result.contains_key(header::UPGRADE));
    }

    #[tokio::test]
    async fn auth_header_cache_replays_observed_headers() {
        let cache = AuthHeaderCache::default();
        let mut first_headers = HeaderMap::new();
        first_headers.insert(
            header::AUTHORIZATION,
            HeaderValue::from_static("Bearer token-1"),
        );
        first_headers.insert(
            CHATGPT_ACCOUNT_ID_HEADER,
            HeaderValue::from_static("account-1"),
        );

        let first_state = cache.apply(&mut first_headers).await;

        assert!(first_state.authorization_present);
        assert!(!first_state.authorization_injected);
        assert!(first_state.chatgpt_account_id_present);
        assert!(!first_state.chatgpt_account_id_injected);

        let mut second_headers = HeaderMap::new();
        let second_state = cache.apply(&mut second_headers).await;

        assert!(!second_state.authorization_present);
        assert!(second_state.authorization_injected);
        assert!(!second_state.chatgpt_account_id_present);
        assert!(second_state.chatgpt_account_id_injected);
        assert_eq!(
            second_headers.get(header::AUTHORIZATION).unwrap(),
            "Bearer token-1"
        );
        assert_eq!(
            second_headers.get(CHATGPT_ACCOUNT_ID_HEADER).unwrap(),
            "account-1"
        );
    }

    #[tokio::test]
    async fn auth_header_cache_does_not_pair_new_authorization_with_old_account_id() {
        let cache = AuthHeaderCache::default();
        let mut first_headers = HeaderMap::new();
        first_headers.insert(
            header::AUTHORIZATION,
            HeaderValue::from_static("Bearer token-1"),
        );
        first_headers.insert(
            CHATGPT_ACCOUNT_ID_HEADER,
            HeaderValue::from_static("account-1"),
        );
        cache.apply(&mut first_headers).await;

        let mut changed_headers = HeaderMap::new();
        changed_headers.insert(
            header::AUTHORIZATION,
            HeaderValue::from_static("Bearer token-2"),
        );
        cache.apply(&mut changed_headers).await;

        let mut replay_headers = HeaderMap::new();
        let replay_state = cache.apply(&mut replay_headers).await;

        assert!(replay_state.authorization_injected);
        assert!(!replay_state.chatgpt_account_id_injected);
        assert_eq!(
            replay_headers.get(header::AUTHORIZATION).unwrap(),
            "Bearer token-2"
        );
        assert!(!replay_headers.contains_key(CHATGPT_ACCOUNT_ID_HEADER));
    }

    #[test]
    fn body_forwarding_skips_undeclared_get_and_head_bodies() {
        let headers = HeaderMap::new();
        assert!(!request_should_forward_body(&Method::GET, &headers));
        assert!(!request_should_forward_body(&Method::HEAD, &headers));
        assert!(request_should_forward_body(&Method::POST, &headers));

        let mut zero_length_headers = HeaderMap::new();
        zero_length_headers.insert(header::CONTENT_LENGTH, HeaderValue::from_static("0"));
        assert!(!request_should_forward_body(
            &Method::GET,
            &zero_length_headers
        ));

        let mut nonzero_length_headers = HeaderMap::new();
        nonzero_length_headers.insert(header::CONTENT_LENGTH, HeaderValue::from_static("5"));
        assert!(request_should_forward_body(
            &Method::GET,
            &nonzero_length_headers
        ));

        let mut transfer_encoding_headers = HeaderMap::new();
        transfer_encoding_headers.insert(
            header::TRANSFER_ENCODING,
            HeaderValue::from_static("chunked"),
        );
        assert!(request_should_forward_body(
            &Method::GET,
            &transfer_encoding_headers
        ));
    }

    #[tokio::test]
    async fn healthz_is_local() {
        let state = AppState {
            config: ProxyConfig {
                listen: "127.0.0.1:0".parse().unwrap(),
                upstream_base_url: Url::parse("https://example.com").unwrap(),
                upstream_prefix: String::new(),
            },
            client: Client::new(),
            auth_headers: Arc::new(AuthHeaderCache::default()),
        };

        let response = app(state)
            .oneshot(
                Request::builder()
                    .uri("/healthz")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
        let body = response.into_body().collect().await.unwrap().to_bytes();
        assert_eq!(&body[..], b"ok\n");
    }

    #[tokio::test]
    async fn http_proxy_forwards_method_path_query_headers_and_body() {
        let upstream = Router::new().route("/api/echo", any(echo_request));
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let upstream_addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(listener, upstream).await.unwrap();
        });

        let state = AppState {
            config: ProxyConfig {
                listen: "127.0.0.1:0".parse().unwrap(),
                upstream_base_url: Url::parse(&format!("http://{upstream_addr}")).unwrap(),
                upstream_prefix: "/api".to_string(),
            },
            client: Client::new(),
            auth_headers: Arc::new(AuthHeaderCache::default()),
        };

        let response = app(state)
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri("/echo?name=codex")
                    .header("x-test", "yes")
                    .body(Body::from("hello"))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
        let body = response.into_body().collect().await.unwrap().to_bytes();
        let value: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(value["method"], "POST");
        assert_eq!(value["path"], "/api/echo");
        assert_eq!(value["query"], "name=codex");
        assert_eq!(value["x_test"], "yes");
        assert_eq!(value["body"], "hello");
    }

    #[tokio::test]
    async fn http_proxy_replays_cached_auth_headers_when_request_omits_them() {
        let upstream = Router::new().route("/api/echo", any(echo_request));
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let upstream_addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(listener, upstream).await.unwrap();
        });

        let state = AppState {
            config: ProxyConfig {
                listen: "127.0.0.1:0".parse().unwrap(),
                upstream_base_url: Url::parse(&format!("http://{upstream_addr}")).unwrap(),
                upstream_prefix: "/api".to_string(),
            },
            client: Client::new(),
            auth_headers: Arc::new(AuthHeaderCache::default()),
        };
        let proxy = app(state);

        let first_response = proxy
            .clone()
            .oneshot(
                Request::builder()
                    .method(Method::GET)
                    .uri("/echo")
                    .header(header::AUTHORIZATION, "Bearer token-1")
                    .header(CHATGPT_ACCOUNT_ID_HEADER, "account-1")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(first_response.status(), StatusCode::OK);

        let second_response = proxy
            .oneshot(
                Request::builder()
                    .method(Method::GET)
                    .uri("/echo")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(second_response.status(), StatusCode::OK);
        let body = second_response
            .into_body()
            .collect()
            .await
            .unwrap()
            .to_bytes();
        let value: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(value["authorization"], "Bearer token-1");
        assert_eq!(value["chatgpt_account_id"], "account-1");
    }

    #[tokio::test]
    async fn http_proxy_get_without_declared_body_does_not_send_body_headers() {
        let upstream = Router::new().route("/api/echo", any(echo_request));
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let upstream_addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(listener, upstream).await.unwrap();
        });

        let state = AppState {
            config: ProxyConfig {
                listen: "127.0.0.1:0".parse().unwrap(),
                upstream_base_url: Url::parse(&format!("http://{upstream_addr}")).unwrap(),
                upstream_prefix: "/api".to_string(),
            },
            client: Client::new(),
            auth_headers: Arc::new(AuthHeaderCache::default()),
        };

        let response = app(state)
            .oneshot(
                Request::builder()
                    .method(Method::GET)
                    .uri("/echo?name=codex")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
        let body = response.into_body().collect().await.unwrap().to_bytes();
        let value: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(value["method"], "GET");
        assert_eq!(value["path"], "/api/echo");
        assert_eq!(value["body"], "");
        assert!(value["content_length"].is_null());
        assert!(value["transfer_encoding"].is_null());
    }

    #[tokio::test]
    async fn websocket_proxy_forwards_messages_bidirectionally() {
        let upstream = Router::new().route("/up/ws", get(ws_echo));
        let upstream_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let upstream_addr = upstream_listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(upstream_listener, upstream).await.unwrap();
        });

        let state = AppState {
            config: ProxyConfig {
                listen: "127.0.0.1:0".parse().unwrap(),
                upstream_base_url: Url::parse(&format!("http://{upstream_addr}")).unwrap(),
                upstream_prefix: "/up".to_string(),
            },
            client: Client::new(),
            auth_headers: Arc::new(AuthHeaderCache::default()),
        };
        let proxy_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let proxy_addr = proxy_listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(proxy_listener, app(state)).await.unwrap();
        });

        let (mut socket, _response) = connect_async(format!("ws://{proxy_addr}/ws"))
            .await
            .unwrap();
        socket
            .send(TungsteniteMessage::Text("hello".into()))
            .await
            .unwrap();

        let message = socket.next().await.unwrap().unwrap();
        assert_eq!(message, TungsteniteMessage::Text("upstream:hello".into()));
    }

    #[tokio::test]
    async fn websocket_proxy_replays_cached_auth_headers_when_request_omits_them() {
        let upstream = Router::new()
            .route("/up/echo", any(echo_request))
            .route("/up/ws", get(ws_auth_echo));
        let upstream_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let upstream_addr = upstream_listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(upstream_listener, upstream).await.unwrap();
        });

        let state = AppState {
            config: ProxyConfig {
                listen: "127.0.0.1:0".parse().unwrap(),
                upstream_base_url: Url::parse(&format!("http://{upstream_addr}")).unwrap(),
                upstream_prefix: "/up".to_string(),
            },
            client: Client::new(),
            auth_headers: Arc::new(AuthHeaderCache::default()),
        };
        let proxy_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let proxy_addr = proxy_listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(proxy_listener, app(state)).await.unwrap();
        });

        let client = Client::new();
        let seed_response = client
            .get(format!("http://{proxy_addr}/echo"))
            .header(header::AUTHORIZATION, "Bearer token-1")
            .header(CHATGPT_ACCOUNT_ID_HEADER, "account-1")
            .send()
            .await
            .unwrap();
        assert_eq!(seed_response.status(), StatusCode::OK);

        let (mut socket, _response) = connect_async(format!("ws://{proxy_addr}/ws"))
            .await
            .unwrap();
        socket
            .send(TungsteniteMessage::Text("hello".into()))
            .await
            .unwrap();

        let message = socket.next().await.unwrap().unwrap();
        assert_eq!(message, TungsteniteMessage::Text("upstream:hello".into()));
    }

    #[tokio::test]
    async fn websocket_proxy_forwards_upstream_handshake_http_error() {
        let upstream = Router::new().route("/up/ws", get(|| async { StatusCode::NOT_FOUND }));
        let upstream_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let upstream_addr = upstream_listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(upstream_listener, upstream).await.unwrap();
        });

        let state = AppState {
            config: ProxyConfig {
                listen: "127.0.0.1:0".parse().unwrap(),
                upstream_base_url: Url::parse(&format!("http://{upstream_addr}")).unwrap(),
                upstream_prefix: "/up".to_string(),
            },
            client: Client::new(),
            auth_headers: Arc::new(AuthHeaderCache::default()),
        };
        let proxy_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let proxy_addr = proxy_listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(proxy_listener, app(state)).await.unwrap();
        });

        let err = connect_async(format!("ws://{proxy_addr}/ws"))
            .await
            .expect_err("upstream HTTP error should reject the downstream handshake");

        match err {
            TungsteniteError::Http(response) => {
                assert_eq!(response.status(), StatusCode::NOT_FOUND);
            }
            other => panic!("expected HTTP websocket error, got {other:?}"),
        }
    }

    async fn echo_request(request: Request) -> Response {
        let (parts, body) = request.into_parts();
        let body = body.collect().await.unwrap().to_bytes();
        let payload = json!({
            "method": parts.method.as_str(),
            "path": parts.uri.path(),
            "query": parts.uri.query().unwrap_or_default(),
            "x_test": parts.headers.get("x-test").and_then(|value| value.to_str().ok()).unwrap_or_default(),
            "authorization": parts.headers.get(header::AUTHORIZATION).and_then(|value| value.to_str().ok()).unwrap_or_default(),
            "chatgpt_account_id": parts.headers.get(CHATGPT_ACCOUNT_ID_HEADER).and_then(|value| value.to_str().ok()).unwrap_or_default(),
            "body": String::from_utf8_lossy(&body),
            "content_length": parts.headers.get(header::CONTENT_LENGTH).and_then(|value| value.to_str().ok()),
            "transfer_encoding": parts.headers.get(header::TRANSFER_ENCODING).and_then(|value| value.to_str().ok()),
        });
        Response::builder()
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(payload.to_string()))
            .unwrap()
    }

    async fn ws_echo(ws: WebSocketUpgrade) -> impl IntoResponse {
        ws.on_upgrade(|mut socket| async move {
            while let Some(Ok(message)) = socket.recv().await {
                match message {
                    AxumWsMessage::Text(text) => {
                        socket
                            .send(AxumWsMessage::Text(format!("upstream:{text}").into()))
                            .await
                            .unwrap();
                    }
                    AxumWsMessage::Binary(bytes) => {
                        socket.send(AxumWsMessage::Binary(bytes)).await.unwrap();
                    }
                    AxumWsMessage::Ping(bytes) => {
                        socket.send(AxumWsMessage::Pong(bytes)).await.unwrap();
                    }
                    AxumWsMessage::Pong(_) => {}
                    AxumWsMessage::Close(frame) => {
                        socket.send(AxumWsMessage::Close(frame)).await.unwrap();
                        break;
                    }
                }
            }
        })
    }

    async fn ws_auth_echo(headers: HeaderMap, ws: WebSocketUpgrade) -> Response {
        let auth_ok = headers
            .get(header::AUTHORIZATION)
            .and_then(|value| value.to_str().ok())
            == Some("Bearer token-1");
        let account_ok = headers
            .get(CHATGPT_ACCOUNT_ID_HEADER)
            .and_then(|value| value.to_str().ok())
            == Some("account-1");

        if !auth_ok || !account_ok {
            return StatusCode::UNAUTHORIZED.into_response();
        }

        ws_echo(ws).await.into_response()
    }
}
