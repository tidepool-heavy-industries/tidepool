use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;

use rmcp::{model::*, service::RequestContext, ErrorData as McpError, RoleServer, ServerHandler};

/// Check if tidepool-extract is available.
pub(crate) fn find_tidepool_extract() -> Option<PathBuf> {
    // 1. TIDEPOOL_EXTRACT env var
    if let Ok(p) = std::env::var("TIDEPOOL_EXTRACT") {
        let path = PathBuf::from(&p);
        if path.exists() {
            return Some(path);
        }
    }
    // 2. On PATH
    which::which("tidepool-extract").ok()
}

// ---------------------------------------------------------------------------
// Degraded MCP server — served when tidepool-extract is missing
// ---------------------------------------------------------------------------

#[derive(Clone)]
pub(crate) struct SetupServer;

const INSTALL_INSTRUCTIONS: &str = "\
Tidepool MCP server is running but the GHC toolchain is not installed.
The Haskell compiler is needed to evaluate code.

Install it with Nix:

  1. Install Nix (if needed):
     curl --proto '=https' --tlsv1.2 -sSf -L https://install.determinate.systems/nix | sh -s -- install

  2. Install the tidepool GHC toolchain:
     nix profile install github:tidepool-heavy-industries/tidepool#tidepool-extract

  3. Restart this MCP server.

Alternatively, set TIDEPOOL_EXTRACT to point to an existing tidepool-extract binary.";

impl ServerHandler for SetupServer {
    fn get_info(&self) -> ServerInfo {
        ServerInfo {
            instructions: Some(
                "Tidepool MCP server (setup mode). The GHC toolchain is not installed. \
                 Call the install_instructions tool for setup steps."
                    .into(),
            ),
            capabilities: ServerCapabilities::builder().enable_tools().build(),
            ..Default::default()
        }
    }

    async fn list_tools(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListToolsResult, McpError> {
        let schema = serde_json::json!({
            "type": "object",
            "properties": {},
        });
        let input_schema = match schema {
            serde_json::Value::Object(o) => Arc::new(o),
            _ => Arc::new(serde_json::Map::new()),
        };
        Ok(ListToolsResult {
            tools: vec![Tool {
                name: "install_instructions".into(),
                title: None,
                description: Some(
                    "Get instructions for installing the GHC toolchain required by Tidepool."
                        .into(),
                ),
                input_schema,
                output_schema: None,
                annotations: None,
                icons: None,
                meta: None,
                execution: None,
            }],
            next_cursor: None,
            meta: None,
        })
    }

    async fn call_tool(
        &self,
        request: CallToolRequestParams,
        _context: RequestContext<RoleServer>,
    ) -> Result<CallToolResult, McpError> {
        match request.name.as_ref() {
            "install_instructions" => Ok(CallToolResult {
                content: vec![Content::text(INSTALL_INSTRUCTIONS)],
                structured_content: None,
                is_error: Some(false),
                meta: None,
            }),
            _ => Err(McpError {
                code: ErrorCode::METHOD_NOT_FOUND,
                message: format!("Tool not found: {}", request.name).into(),
                data: None,
            }),
        }
    }
}

/// If `tidepool-extract` is not available, serve the degraded setup-only MCP
/// server and return `Ok(true)` (caller should then return). Returns
/// `Ok(false)` if `tidepool-extract` was found (caller should continue).
pub(crate) async fn maybe_serve_degraded(
    http_addr: Option<SocketAddr>,
) -> Result<bool, Box<dyn std::error::Error>> {
    if find_tidepool_extract().is_some() {
        return Ok(false);
    }

    tracing::warn!(
        "tidepool-extract not found — serving setup-only MCP server. \
         Install via: nix profile install github:tidepool-heavy-industries/tidepool#tidepool-extract"
    );
    use rmcp::ServiceExt;
    if let Some(addr) = http_addr {
        use rmcp::transport::streamable_http_server::{
            session::local::LocalSessionManager, StreamableHttpServerConfig, StreamableHttpService,
        };
        let config = StreamableHttpServerConfig::default();
        let cancel = config.cancellation_token.clone();
        let service = StreamableHttpService::new(
            || Ok(SetupServer),
            Arc::new(LocalSessionManager::default()),
            config,
        );
        async fn health() -> axum::Json<serde_json::Value> {
            axum::Json(serde_json::json!({"status": "ok"}))
        }
        let router = axum::Router::new()
            .route("/health", axum::routing::get(health))
            .nest_service("/mcp", service);
        let listener = tokio::net::TcpListener::bind(addr).await?;
        tracing::debug!(
            "Tidepool MCP v{} listening on http://{}/mcp (setup mode)",
            env!("CARGO_PKG_VERSION"),
            addr,
        );
        axum::serve(listener, router)
            .with_graceful_shutdown(async move {
                tokio::signal::ctrl_c().await.ok();
                cancel.cancel();
            })
            .await?;
    } else {
        SetupServer
            .serve((tokio::io::stdin(), tokio::io::stdout()))
            .await?
            .waiting()
            .await?;
    }
    Ok(true)
}
