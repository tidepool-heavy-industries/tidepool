//! The `tidepool-repl` MCP server binary.
//!
//! A SEPARATE server from `tidepool` (the eval server), but it builds the SAME
//! full effect suite from the shared `tidepool-handlers` crate
//! (`build_base_stack`): Console, KV, Fs, ast-grep/SG, Http, Exec/run, Lsp, Llm
//! — plus the `Ask` suspend interposed by the session worker. What makes this a
//! distinct server is the STATE: a resident JIT machine holds the value heap
//! across `session_eval` turns and Lane-A declarations accumulate, so the
//! effects compose over persistent, typed session state.

use std::net::SocketAddr;
use std::path::PathBuf;

use tidepool_handlers::{
    base_decls_with_ask, build_base_stack, HandlerConfig, DEFAULT_OPENAI_MODEL,
};
use tidepool_repl::{ReplServerConfig, TidepoolReplServer};
use tidepool_runtime::session::ModuleEnv;

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

/// Resolve the bundled Haskell prelude/stdlib dir (`Tidepool.*` modules).
/// Honors `TIDEPOOL_PRELUDE_DIR`; falls back to the in-repo `haskell/lib`.
fn resolve_prelude_dir() -> PathBuf {
    if let Some(dir) = std::env::var_os("TIDEPOOL_PRELUDE_DIR") {
        return PathBuf::from(dir);
    }
    let repo_lib = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .map(|p| p.join("haskell").join("lib"))
        .unwrap_or_default();
    if repo_lib.is_dir() {
        return repo_lib;
    }
    repo_lib
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("warn")),
        )
        .with_writer(std::io::stderr)
        .init();
    tidepool_codegen::debug::init_logging();
    tidepool_codegen::signal_safety::install();

    use clap::Parser;
    let args = Args::parse();
    let http_addr = args
        .http
        .or(args.port.map(|p| SocketAddr::from(([0, 0, 0, 0], p))));

    // Full effect suite, shared with the eval server via `tidepool-handlers`.
    // HandlerConfig resolution mirrors `tidepool/src/main.rs` (cwd sandbox for
    // Fs/SG/Exec/Lsp, the KV backing file, the LLM model). NOTE: unlike the eval
    // binary we don't layer `config.toml` for the model (that `Config` lives in
    // the `tidepool` binary, not a shared lib) — env + default only; factoring
    // it out is a follow-up. `build_base_stack` must run in a tokio context (Llm
    // captures `Handle::current()`), which `#[tokio::main]` provides.
    let cwd = std::env::current_dir()?;
    let project_root = tidepool_runtime::paths::find_project_root(&cwd);
    let kv_path = match &project_root {
        Some(root) => root.join(".tidepool").join("kv.json"),
        None => tidepool_runtime::paths::cache_dir().join("kv.json"),
    };
    let llm_model =
        std::env::var("TIDEPOOL_LLM_MODEL").unwrap_or_else(|_| DEFAULT_OPENAI_MODEL.to_string());
    let handler_cfg = HandlerConfig {
        cwd,
        kv_path,
        llm_model,
    };
    let stack = build_base_stack(&handler_cfg);
    // Decls derive from the stack (in HList/tag order) + Ask appended.
    let (decls, ask_tag) = base_decls_with_ask(&stack);

    // The generated Tidepool.Effects module must be on the include path.
    let effects_dir = tidepool_mcp::ensure_effects_module(&decls)?;
    let prelude_dir = resolve_prelude_dir();
    let base_include = vec![effects_dir, prelude_dir];

    // Per-session include trees live under a process-scoped temp dir.
    let session_root_base =
        std::env::temp_dir().join(format!("tidepool-repl-{}", std::process::id()));

    let cfg = ReplServerConfig {
        decls,
        ask_tag,
        base_include,
        module_env: ModuleEnv::standalone_default(),
        session_root_base,
        nursery_size: None,
    };

    let server = TidepoolReplServer::new(stack, cfg);
    if let Some(addr) = http_addr {
        server.serve_http(addr).await
    } else {
        server.serve_stdio().await
    }
}
