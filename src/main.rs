use std::net::SocketAddr;
use std::path::PathBuf;

use clap::Parser;
use tracing_subscriber::EnvFilter;
use url::Url;

mod proxy;
mod tls;

const DEFAULT_LISTEN: &str = "127.0.0.1:8000";
const DEFAULT_UPSTREAM_BASE_URL: &str = "https://api.claudecode.store";

#[derive(Debug, Parser)]
#[command(author, version, about)]
struct Args {
    /// Local address to listen on.
    #[arg(long, env = "CCS_PROXY_LISTEN", default_value = DEFAULT_LISTEN)]
    listen: SocketAddr,

    /// Upstream proxy base URL.
    #[arg(
        long,
        env = "CCS_PROXY_UPSTREAM_BASE_URL",
        default_value = DEFAULT_UPSTREAM_BASE_URL
    )]
    upstream_base_url: Url,

    /// Optional path prefix inserted before the local request path.
    #[arg(long, env = "CCS_PROXY_UPSTREAM_PREFIX", default_value = "")]
    upstream_prefix: String,

    /// PEM certificate chain for the local HTTPS/WSS listener.
    #[arg(long, env = "CCS_PROXY_TLS_CERT", requires = "tls_key")]
    tls_cert: Option<PathBuf>,

    /// PEM private key for the local HTTPS/WSS listener.
    #[arg(long, env = "CCS_PROXY_TLS_KEY", requires = "tls_cert")]
    tls_key: Option<PathBuf>,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    init_tracing();

    let args = Args::parse();
    // Bind before certificate setup so an occupied port never prompts for trust.
    let listener = tokio::net::TcpListener::bind(args.listen).await?;
    let _ = rustls::crypto::ring::default_provider().install_default();
    let tls = match (args.tls_cert, args.tls_key) {
        (Some(cert), Some(key)) => {
            Some(axum_server::tls_rustls::RustlsConfig::from_pem_file(cert, key).await?)
        }
        (None, None) => {
            let paths =
                tokio::task::spawn_blocking(|| tls::prepare(&tls::default_directory()?)).await??;
            Some(axum_server::tls_rustls::RustlsConfig::from_pem_file(paths.cert, paths.key).await?)
        }
        _ => unreachable!("clap requires both TLS certificate and key"),
    };
    proxy::serve_listener(
        proxy::ProxyConfig {
            upstream_base_url: args.upstream_base_url,
            upstream_prefix: args.upstream_prefix,
        },
        tls,
        listener,
    )
    .await
}

fn init_tracing() {
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));
    tracing_subscriber::fmt().with_env_filter(filter).init();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tls_requires_certificate_and_key_together() {
        assert!(Args::try_parse_from(["ccs-proxy", "--tls-cert", "cert.pem"]).is_err());
        assert!(Args::try_parse_from(["ccs-proxy", "--tls-key", "key.pem"]).is_err());
        assert!(
            Args::try_parse_from([
                "ccs-proxy",
                "--tls-cert",
                "cert.pem",
                "--tls-key",
                "key.pem"
            ])
            .is_ok()
        );
        assert!(Args::try_parse_from(["ccs-proxy"]).is_ok());
    }
}
