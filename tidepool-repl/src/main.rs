//! The `tidepool-repl` MCP server binary.
//!
//! A SEPARATE server from `tidepool` (the eval server). It uses a deliberately
//! minimal effect stack — `[Console, Ask]` — sufficient for Wave 2: the session
//! mechanism (resident machine across turns + Lane-A decl accumulation), not
//! full effect parity. Richer stacks are a later concern.

use std::net::SocketAddr;
use std::path::PathBuf;

use tidepool_repl::{default_decls, ConsoleHandler, ReplServerConfig, TidepoolReplServer};
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

    // Minimal effect stack: [Console, Ask]. Ask is tag 1 (its index in decls).
    let (decls, ask_tag) = default_decls();

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
    };

    let server = TidepoolReplServer::new(frunk::hlist![ConsoleHandler], cfg);
    if let Some(addr) = http_addr {
        server.serve_http(addr).await
    } else {
        server.serve_stdio().await
    }
}
