#![warn(clippy::unwrap_used, clippy::expect_used)]
use std::net::SocketAddr;

use tidepool_handlers::HandlerConfig;
use tidepool_mcp::server_common;

mod config;
mod listen_client;
mod prelude;
mod setup;
mod stack;

use config::Config;

// ---------------------------------------------------------------------------
// CLI
// ---------------------------------------------------------------------------

#[derive(clap::Subcommand)]
enum Command {
    /// Connect to a resident harness's listen channel and print each frame
    /// it publishes to stdout, acking after each flush. See
    /// `tidepool_harness::listen` for the channel this talks to.
    Listen(listen_client::ListenArgs),
}

#[derive(clap::Parser)]
#[command(name = "tidepool", about = "Tidepool MCP server")]
struct Args {
    #[command(subcommand)]
    command: Option<Command>,

    /// Serve over streamable HTTP instead of stdio. Example: --http 0.0.0.0:8080
    #[arg(long, conflicts_with = "port")]
    http: Option<SocketAddr>,

    /// Serve over HTTP on 0.0.0.0:<PORT>. Shorthand for --http 0.0.0.0:<PORT>
    #[arg(long, conflicts_with = "http")]
    port: Option<u16>,

    /// Enable debug effects (Meta introspection)
    #[arg(long)]
    debug: bool,

    /// Advertise the `help` tool — reference docs (the same content as the
    /// `tidepool://…` MCP resources) via a plain tool call. Enable for clients
    /// that don't support MCP resources; resource-capable clients don't need it.
    #[arg(long)]
    help_tool: bool,

    /// LLM model for the Llm effect. genai routes the provider from the
    /// name: gpt-4o-mini → OpenAI, claude-haiku-4-5 → Anthropic, gemini-*
    /// → Gemini, unknown names → Ollama (e.g. qwen2.5:7b); `ns::model`
    /// forces a namespace. API keys come from the standard env vars
    /// (OPENAI_API_KEY, ...) or from `.tidepool/secrets/<ENV_VAR_NAME>`
    /// files.
    /// (Unset → falls back to `config.toml` `llm_model`, then the built-in default.)
    #[arg(long, env = "TIDEPOOL_LLM_MODEL")]
    llm: Option<String>,

    /// Record the deploy stamp — the bound compiler producer identity and the
    /// stdlib content fingerprint this binary resolves — then exit. Run by
    /// `scripts/redeploy.sh` as its final step, so that every later server
    /// startup can detect an extract/stdlib pair that did NOT move together.
    /// Writer and checker share one implementation
    /// (`tidepool_runtime::toolchain`), so they cannot drift.
    #[arg(long)]
    write_toolchain_stamp: bool,
}

// ---------------------------------------------------------------------------
// Main
// ---------------------------------------------------------------------------

/// Install a panic hook that writes crash dumps to `<cwd>/.tidepool/crash.log`
/// — the SAME path the JIT signal handler writes to
/// (`tidepool_codegen::signal_safety::install`) and the Crashed-outcome
/// forensics reader (`tidepool-mcp/src/server.rs`) reads from. HAZARD: a
/// home-relative path here means a Rust-side panic's forensics never
/// surfaces unless CWD happens to equal $HOME.
fn install_panic_hook() {
    std::panic::set_hook(Box::new(|info| {
        let msg = format!("{}\n{:?}\n", info, std::backtrace::Backtrace::capture());
        let path = std::env::current_dir()
            .unwrap_or_default()
            .join(".tidepool/crash.log");
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let _ = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
            .and_then(|mut f| std::io::Write::write_all(&mut f, msg.as_bytes()));
        tracing::debug!("PANIC — see {}", path.display());
    }));
}

/// Body of `--write-toolchain-stamp`. Prints the recorded identities so the
/// deploy log carries which pair was blessed.
fn write_toolchain_stamp(prelude_dir: &std::path::Path) -> Result<(), Box<dyn std::error::Error>> {
    use tidepool_runtime::toolchain;
    let (endpoint, extract) = toolchain::bind_extract_endpoint()?;
    let stamp = toolchain::write_stamp(&endpoint, &extract.path, prelude_dir)?;
    println!(
        "toolchain stamp written to {}\n  extract {} ({})\n  stdlib  {} ({})",
        toolchain::stamp_path().display(),
        &stamp.extract[..12],
        stamp.extract_path,
        &stamp.stdlib[..12],
        stamp.stdlib_path,
    );
    Ok(())
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    install_panic_hook();

    use clap::Parser;
    let args = Args::parse();

    if let Some(Command::Listen(listen_args)) = &args.command {
        return listen_client::run(listen_args).await;
    }

    let http_addr = args
        .http
        .or(args.port.map(|p| SocketAddr::from(([0, 0, 0, 0], p))));

    server_common::init_tracing();

    // Fill missing *_API_KEY env vars from .tidepool/secrets/ (drop a key
    // file in, restart, done). Must run before any handler reads the env.
    server_common::load_secrets_logged();

    let prelude_dir = prelude::ensure_prelude()?;

    // `--write-toolchain-stamp`: record the pair and exit, before any server
    // machinery starts. Deliberately runs AFTER `ensure_prelude` so the stamp
    // records exactly the stdlib a real startup would resolve.
    if args.write_toolchain_stamp {
        return write_toolchain_stamp(&prelude_dir);
    }

    // Startup handshake: refuse to serve an extract/stdlib pair that was not
    // deployed together. Placed before the degraded-setup fallback so a skew is
    // reported as a skew, not masked as "extract unavailable".
    server_common::handshake_logged(&prelude_dir)?;

    // If tidepool-extract is not available, serve the degraded setup server.
    if setup::maybe_serve_degraded(http_addr).await? {
        return Ok(());
    }

    // Install sigsetjmp/siglongjmp signal handlers early so SIGILL/SIGSEGV
    // from JIT code returns clean errors instead of killing the server.
    tidepool_codegen::signal_safety::install();

    let cwd = std::env::current_dir()?;
    let project_root = tidepool_runtime::paths::find_project_root(&cwd);

    // Layered config: defaults < global config.toml < project config.toml < env.
    let cfg = Config::load(project_root.as_deref());
    // Model precedence: --llm / TIDEPOOL_LLM_MODEL (args.llm) > config > default.
    let model = args
        .llm
        .clone()
        .or(cfg.llm_model)
        .unwrap_or_else(|| tidepool_handlers::DEFAULT_OPENAI_MODEL.to_string());
    // Bridge the configured default eval timeout into the env knob the server
    // reads, unless it's already set explicitly (env stays authoritative).
    if std::env::var_os("TIDEPOOL_EVAL_TIMEOUT_SECS").is_none() {
        if let Some(t) = cfg.eval_timeout_secs {
            std::env::set_var("TIDEPOOL_EVAL_TIMEOUT_SECS", t.to_string());
        }
    }

    // KV persists in the project's `.tidepool/` if we're inside one (walk up from
    // CWD), so it's found from any subdir; otherwise a global store under the
    // cache dir, so state survives even when launched outside any project.
    let kv_path = match &project_root {
        Some(root) => root.join(".tidepool").join("kv.json"),
        None => tidepool_runtime::paths::cache_dir().join("kv.json"),
    };

    let handler_cfg = HandlerConfig {
        cwd: cwd.clone(),
        kv_path,
        llm_model: model.clone(),
    };

    if args.debug {
        stack::run_debug(handler_cfg, prelude_dir, args.help_tool, http_addr).await
    } else {
        stack::run_base(handler_cfg, prelude_dir, args.help_tool, http_addr).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// See the hazard note on [`install_panic_hook`]: must land cwd-relative,
    /// not under `$HOME`.
    #[test]
    fn panic_hook_writes_crash_log_relative_to_cwd() {
        let dir = tempfile::tempdir().unwrap();
        let orig_cwd = std::env::current_dir().unwrap();
        std::env::set_current_dir(dir.path()).unwrap();
        install_panic_hook();
        let result = std::panic::catch_unwind(|| panic!("f4 crash-log probe"));
        std::env::set_current_dir(&orig_cwd).unwrap();
        assert!(result.is_err(), "the probe panic must have been caught");

        let log_path = dir.path().join(".tidepool/crash.log");
        let content = std::fs::read_to_string(&log_path)
            .unwrap_or_else(|e| panic!("crash log missing at cwd-relative path {log_path:?}: {e}"));
        assert!(content.contains("f4 crash-log probe"), "{content}");
    }
}
