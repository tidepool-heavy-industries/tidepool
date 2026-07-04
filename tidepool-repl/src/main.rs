//! The `tidepool-repl` MCP server binary.
//!
//! A SEPARATE server from `tidepool` (the eval server), but it builds the SAME
//! full effect suite from the shared `tidepool-handlers` crate
//! (`build_base_stack`): Console, KV, Fs, Http, Exec/run, Lsp, Llm
//! — plus the `Ask` suspend interposed by the session worker. What makes this a
//! distinct server is the STATE: a resident JIT machine holds the value heap
//! across `session_run` turns and Lane-A declarations accumulate, so the
//! effects compose over persistent, typed session state.

use std::net::SocketAddr;

use tidepool_handlers::{build_base_stack, HandlerConfig};
use tidepool_repl::TidepoolReplServer;

mod main_setup;

#[derive(clap::Parser)]
#[command(
    name = "tidepool-repl",
    about = "GHCi-style stateful Haskell session MCP server"
)]
struct Args {
    /// Serve streamable HTTP on this socket address instead of stdio.
    #[arg(long)]
    http: Option<SocketAddr>,
    /// Serve streamable HTTP on 0.0.0.0:<port> instead of stdio.
    #[arg(long)]
    port: Option<u16>,
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    tidepool_mcp::server_common::init_tracing();
    tidepool_codegen::signal_safety::install();

    // Same secrets surface as the oneshot server: .tidepool/secrets/*_API_KEY
    // (project, then global) into env, so the Llm/Http handlers find keys.
    tidepool_mcp::server_common::load_secrets_logged();

    use clap::Parser;
    let args = Args::parse();
    let http_addr = args
        .http
        .or(args.port.map(|p| SocketAddr::from(([0, 0, 0, 0], p))));

    // Full effect suite, shared with the eval server via `tidepool-handlers`.
    // HandlerConfig resolution mirrors `tidepool/src/main.rs` (cwd sandbox for
    // Fs/Exec/Lsp, the KV backing file, the LLM model). `build_base_stack` must
    // run in a tokio context (Llm captures `Handle::current()`), which
    // `#[tokio::main]` provides.
    let cwd = std::env::current_dir()?;
    let project_root = tidepool_runtime::paths::find_project_root(&cwd);
    let startup = main_setup::build(cwd, project_root)?;

    // Session builder: invoked per session open (auto-open / reset) to build the
    // effect stack. The single implicit session's KV is backed by `kv.json` in
    // the project `.tidepool/` (or the cache dir), persisting across resets.
    let cwd_b = startup.cwd.clone();
    let llm_b = startup.llm_model.clone();
    let kv_path = startup.tidepool_dir.join("kv.json");
    let builder = move || {
        let hcfg = HandlerConfig {
            cwd: cwd_b.clone(),
            kv_path: kv_path.clone(),
            llm_model: llm_b.clone(),
        };
        build_base_stack(&hcfg)
    };
    let server = TidepoolReplServer::new_with_session_builder(builder, startup.cfg);
    if let Some(addr) = http_addr {
        server.serve_http(addr).await
    } else {
        server.serve_stdio().await
    }
}
