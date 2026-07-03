use std::net::SocketAddr;
use std::path::PathBuf;

use tidepool_handlers::{
    ConsoleHandler, ExecHandler, FsHandler, HandlerConfig, HttpHandler, KvHandler, LlmHandler,
    LspHandler, MetaHandler,
};
use tidepool_mcp::TidepoolMcpServer;

pub(crate) async fn run_debug(
    handler_cfg: HandlerConfig,
    model: String,
    prelude_dir: PathBuf,
    help_tool: bool,
    http_addr: Option<SocketAddr>,
) -> Result<(), Box<dyn std::error::Error>> {
    // Build Meta's effect_names/helper_sigs from full decls (standard + meta)
    let mut decls = tidepool_mcp::standard_decls();
    decls.insert(decls.len() - 2, tidepool_mcp::meta_decl()); // before Llm, Ask
    let effect_names: Vec<String> = decls.iter().map(|d| d.type_name.to_string()).collect();
    let mut helper_sigs: Vec<String> = Vec::new();
    helper_sigs.push("putStrLn :: Text -> M ()".into());
    helper_sigs.push("showI :: Int -> Text".into());
    for decl in &decls {
        for h in decl.helpers {
            if let Some(sig) = h.lines().next() {
                helper_sigs.push(sig.to_string());
            }
        }
    }
    let handlers = frunk::hlist![
        ConsoleHandler,
        KvHandler::new(handler_cfg.kv_path.clone()),
        FsHandler::new(handler_cfg.cwd.clone()),
        HttpHandler,
        ExecHandler::new(handler_cfg.cwd.clone()),
        LspHandler::new(handler_cfg.cwd.clone()),
        MetaHandler::new(effect_names, helper_sigs),
        LlmHandler::new(model.clone())
    ];
    let server = TidepoolMcpServer::new(handlers)
        .with_prelude(prelude_dir)
        .with_help_tool(help_tool);
    if let Some(addr) = http_addr {
        server.serve_http(addr).await
    } else {
        server.serve_stdio().await
    }
}

pub(crate) async fn run_base(
    handler_cfg: HandlerConfig,
    prelude_dir: PathBuf,
    help_tool: bool,
    http_addr: Option<SocketAddr>,
) -> Result<(), Box<dyn std::error::Error>> {
    let handlers = tidepool_handlers::build_base_stack(&handler_cfg);
    let server = TidepoolMcpServer::new(handlers)
        .with_prelude(prelude_dir)
        .with_help_tool(help_tool);
    if let Some(addr) = http_addr {
        server.serve_http(addr).await
    } else {
        server.serve_stdio().await
    }
}
