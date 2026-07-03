use std::net::SocketAddr;

use tidepool_handlers::HandlerConfig;
use tidepool_mcp::server_common;

mod config;
mod prelude;
mod setup;
mod stack;

use config::Config;

// ---------------------------------------------------------------------------
// CLI
// ---------------------------------------------------------------------------

#[derive(clap::Parser)]
#[command(name = "tidepool", about = "Tidepool MCP server")]
struct Args {
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
}

// ---------------------------------------------------------------------------
// Main
// ---------------------------------------------------------------------------

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Install panic hook that writes crash dumps to ~/.tidepool/crash.log
    std::panic::set_hook(Box::new(|info| {
        let msg = format!("{}\n{:?}\n", info, std::backtrace::Backtrace::capture());
        let path = dirs::home_dir()
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

    use clap::Parser;
    let args = Args::parse();
    let http_addr = args
        .http
        .or(args.port.map(|p| SocketAddr::from(([0, 0, 0, 0], p))));

    server_common::init_tracing();

    // Fill missing *_API_KEY env vars from .tidepool/secrets/ (drop a key
    // file in, restart, done). Must run before any handler reads the env.
    server_common::load_secrets_logged();

    let prelude_dir = prelude::ensure_prelude()?;

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
        stack::run_debug(handler_cfg, model, prelude_dir, args.help_tool, http_addr).await
    } else {
        stack::run_base(handler_cfg, prelude_dir, args.help_tool, http_addr).await
    }
}
