use std::net::SocketAddr;

use clap::Parser;
use tracing_subscriber::EnvFilter;
use url::Url;

mod proxy;

const DEFAULT_LISTEN: &str = "127.0.0.1:8000";
const DEFAULT_UPSTREAM_BASE_URL: &str = "https://chatgpt.claudecode.store";

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
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    init_tracing();

    let args = Args::parse();
    proxy::serve(proxy::ProxyConfig {
        listen: args.listen,
        upstream_base_url: args.upstream_base_url,
        upstream_prefix: args.upstream_prefix,
    })
    .await
}

fn init_tracing() {
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));
    tracing_subscriber::fmt().with_env_filter(filter).init();
}
