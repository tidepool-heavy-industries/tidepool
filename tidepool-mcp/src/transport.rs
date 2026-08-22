//! Generic MCP transport launch helpers, shared by every server in this
//! workspace (`tidepool`'s stateless eval server, `tidepool-repl`'s resident
//! session server): stdio and streamable-HTTP wiring that does not vary by
//! server, only by which [`ServerHandler`] impl drives it. Each server keeps
//! its own tool dispatch and `get_info` — only the transport plumbing around
//! them was ever duplicated.

use std::net::SocketAddr;

use rmcp::{ServerHandler, ServiceExt};
use tokio::io::{stdin, stdout};

/// Start `service` on stdio transport — the one stdio-wiring shape every
/// server here uses.
pub async fn serve_stdio<S: ServerHandler>(service: S) -> Result<(), Box<dyn std::error::Error>> {
    service.serve((stdin(), stdout())).await?.waiting().await?;
    Ok(())
}

/// Start `service` on streamable-HTTP transport at `addr`, with a `/health`
/// probe alongside `/mcp`.
///
/// `display_name`/`version` are baked into the startup banner only —
/// `env!("CARGO_PKG_VERSION")` must be read at each CALLER's own compile
/// time (this crate's own version would be wrong for a caller), so it is a
/// parameter here rather than resolved inside this function.
pub async fn serve_streamable_http<S>(
    service: S,
    addr: SocketAddr,
    display_name: &str,
    version: &str,
) -> Result<(), Box<dyn std::error::Error>>
where
    S: ServerHandler + Clone,
{
    use rmcp::transport::streamable_http_server::{
        session::local::LocalSessionManager, StreamableHttpServerConfig, StreamableHttpService,
    };
    use std::sync::Arc;

    let config = StreamableHttpServerConfig::default();
    let cancel = config.cancellation_token.clone();
    let http_service = StreamableHttpService::new(
        move || Ok(service.clone()),
        Arc::new(LocalSessionManager::default()),
        config,
    );
    async fn health() -> axum::Json<serde_json::Value> {
        axum::Json(serde_json::json!({"status": "ok"}))
    }
    let router = axum::Router::new()
        .route("/health", axum::routing::get(health))
        .nest_service("/mcp", http_service);
    let listener = tokio::net::TcpListener::bind(addr).await?;
    eprintln!("{display_name} v{version} listening on http://{addr}/mcp");
    axum::serve(listener, router)
        .with_graceful_shutdown(async move {
            tokio::signal::ctrl_c().await.ok();
            cancel.cancel();
        })
        .await?;
    Ok(())
}
