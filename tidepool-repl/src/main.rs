//! The `tidepool-repl` MCP server binary.
//!
//! A SEPARATE server from `tidepool` (the eval server), but it builds the SAME
//! full effect suite from the shared `tidepool-handlers` crate
//! (`build_base_stack`): Console, KV, Fs, ast-grep/SG, Http, Exec/run, Lsp, Llm
//! — plus the `Ask` suspend interposed by the session worker. What makes this a
//! distinct server is the STATE: a resident JIT machine holds the value heap
//! across `session_run` turns and Lane-A declarations accumulate, so the
//! effects compose over persistent, typed session state.

use std::net::SocketAddr;
use std::path::PathBuf;

use tidepool_handlers::{
    base_decls_with_ask, build_base_stack, HandlerConfig, DEFAULT_OPENAI_MODEL,
};
use tidepool_repl::{ReplServerConfig, TidepoolReplServer};

/// Derive the KV backing path for a given session name.
///
/// `tidepool_dir` is the project's `.tidepool/` directory (or the fallback cache dir).
///
/// - `"default"` maps to `<tidepool_dir>/kv.json` (unchanged from the pre-multi-session
///   path) so existing callers and tests keep working without migration.
/// - Any other name maps to `<tidepool_dir>/kv/<safe_name>.json` in a dedicated
///   sub-directory, isolating each session's KV namespace.
fn kv_path_for_session(tidepool_dir: &std::path::Path, session_name: &str) -> std::path::PathBuf {
    if session_name == "default" {
        tidepool_dir.join("kv.json")
    } else {
        let safe: String = session_name
            .chars()
            .map(|c| {
                if c.is_alphanumeric() || c == '-' || c == '_' {
                    c
                } else {
                    '_'
                }
            })
            .collect();
        tidepool_dir.join("kv").join(format!("{}.json", safe))
    }
}

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
    let tidepool_dir = match &project_root {
        Some(root) => root.join(".tidepool"),
        None => tidepool_runtime::paths::cache_dir(),
    };
    let llm_model =
        std::env::var("TIDEPOOL_LLM_MODEL").unwrap_or_else(|_| DEFAULT_OPENAI_MODEL.to_string());

    // Build a representative stack to derive effect declarations and ask_tag.
    // The kv_path here doesn't matter for decls (they depend only on handler types).
    let sample_cfg = HandlerConfig {
        cwd: cwd.clone(),
        kv_path: tidepool_dir.join("kv.json"),
        llm_model: llm_model.clone(),
    };
    let stack = build_base_stack(&sample_cfg);
    // Decls derive from the stack (in HList/tag order) + Ask appended.
    let (decls, ask_tag) = base_decls_with_ask(&stack);
    drop(stack); // the per-session builder owns each session's stack

    // The generated Tidepool.Effects module must be on the include path.
    let effects_dir = tidepool_mcp::ensure_effects_module(&decls)?;
    let prelude_dir = resolve_prelude_dir();
    let mut base_include = vec![effects_dir, prelude_dir];

    // Verb libraries (parity with the eval server): project `.tidepool/lib`
    // first, then user-global, AFTER the stdlib so `Tidepool.*` still resolves
    // from the bundle and a project `Library` shadows the global one. With these
    // on the include path, the preamble auto-imports `Library` (see
    // `has_user_library`) so `.tidepool/lib` verbs (vocab/gitS/census/…) are in
    // scope — previously the REPL listed them in `:vocab` but couldn't call them.
    if let Some(root) = &project_root {
        let project_lib = root.join(".tidepool").join("lib");
        if project_lib.is_dir() {
            base_include.push(project_lib);
        }
    }
    base_include.extend(tidepool_runtime::paths::global_lib_dirs());

    // Mirrors `has_user_library` (server.rs) — computed here too since
    // `base_include` is about to move into `cfg` and `session_decl_module_env`
    // needs the flag before that.
    let user_library = base_include.iter().any(|d| d.join("Library.hs").exists());

    // Per-session include trees live under a process-scoped temp dir.
    let session_root_base =
        std::env::temp_dir().join(format!("tidepool-repl-{}", std::process::id()));

    let cfg = ReplServerConfig {
        decls,
        ask_tag,
        base_include,
        // Full effect stack + with-packages GHC: give Lane-A decls the SAME
        // pragmas/imports an `eval` expression sees, so declaration-item helpers
        // can use `M`, the effect verbs, the Prelude shadows, and `L.`/`Set.`/… —
        // not just the lens-free T+Map of `standalone_default`.
        module_env: tidepool_mcp::session_decl_module_env(user_library),
        session_root_base,
        nursery_size: None,
        // Reap a parked `ask` (or a wedged turn) abandoned for 30 min, so an
        // agent that suspends and disconnects doesn't leak a worker thread.
        continuation_ttl: Some(std::time::Duration::from_secs(30 * 60)),
        // Default 120 s turn budget (see `TURN_TIMEOUT_SECS`).
        turn_timeout: None,
    };

    // Per-session builder: each session_open gets its own KvHandler backed by a
    // session-scoped file so kvKeys/kvGet/kvSet in session X cannot see session Y's keys.
    let cwd_b = cwd.clone();
    let llm_b = llm_model.clone();
    let tidepool_dir_b = tidepool_dir.clone();
    let builder = move |session_name: &str| {
        let kv_path = kv_path_for_session(&tidepool_dir_b, session_name);
        let hcfg = HandlerConfig {
            cwd: cwd_b.clone(),
            kv_path,
            llm_model: llm_b.clone(),
        };
        build_base_stack(&hcfg)
    };
    let server = TidepoolReplServer::new_with_session_builder(builder, cfg);
    if let Some(addr) = http_addr {
        server.serve_http(addr).await
    } else {
        server.serve_stdio().await
    }
}
