use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;

use rmcp::{model::*, service::RequestContext, ErrorData as McpError, RoleServer, ServerHandler};
use tidepool_handlers::{
    ConsoleHandler, ExecHandler, FsHandler, HandlerConfig, HttpHandler, KvHandler, LlmHandler,
    LspHandler, MetaHandler, SgHandler,
};
use tidepool_mcp::TidepoolMcpServer;

mod config;
use config::Config;

// ---------------------------------------------------------------------------
// Bundled Haskell stdlib — embedded at build time (build.rs walks the whole
// haskell/lib/Tidepool tree), materialized to a content-addressed cache dir.
// ---------------------------------------------------------------------------

// `EMBEDDED_STDLIB: &[(&str, &str)]` — (Tidepool/<rel>, contents) for every
// `.hs` module in the tree (Internal/ + Prelude_cbor excluded). @generated.
include!(concat!(env!("OUT_DIR"), "/embedded_stdlib.rs"));

/// Deterministic hash of the embedded stdlib content. Stable across runs
/// (`DefaultHasher` has fixed keys) so a given binary always maps to the same
/// cache dir; a changed binary maps to a fresh one.
fn stdlib_content_hash() -> String {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    for (rel, content) in EMBEDDED_STDLIB {
        rel.hash(&mut h);
        content.hash(&mut h);
    }
    format!("{:016x}", h.finish())
}

/// Resolve the directory holding the Tidepool stdlib (an include root for GHC).
/// Precedence: `TIDEPOOL_PRELUDE_DIR` → in-repo `haskell/lib` → materialized
/// bundle in the content-addressed cache dir. The bundle is the COMPLETE tree
/// and is keyed on content, so it can't go stale across binary versions (the
/// old `.version` stamp froze it) and can't drift from a hand-maintained subset.
fn ensure_prelude() -> Result<PathBuf, Box<dyn std::error::Error>> {
    if let Some(dir) = std::env::var_os("TIDEPOOL_PRELUDE_DIR") {
        return Ok(PathBuf::from(dir));
    }

    // In-repo development: use haskell/lib/ directly if present
    if let Ok(cwd) = std::env::current_dir() {
        let from_root = cwd.join("haskell").join("lib");
        if from_root.join("Tidepool").join("Prelude.hs").exists() {
            return Ok(from_root);
        }
        let from_haskell = cwd.join("lib");
        if from_haskell.join("Tidepool").join("Prelude.hs").exists() {
            return Ok(from_haskell);
        }
    }

    // Installed mode: materialize the bundled stdlib to a content-addressed dir.
    let hash = stdlib_content_hash();
    let base = tidepool_runtime::paths::stdlib_dir(&hash);
    // Sentinel marks a COMPLETE write — guards against serving a half-written dir
    // (e.g. a crash mid-materialization) and against the macOS cache reaper.
    let sentinel = base.join(".complete");
    if !sentinel.exists() {
        for (rel, content) in EMBEDDED_STDLIB {
            let full = base.join(rel);
            if let Some(parent) = full.parent() {
                std::fs::create_dir_all(parent)?;
            }
            std::fs::write(&full, content)?;
        }
        std::fs::write(&sentinel, hash.as_bytes())?;
    }
    Ok(base)
}

/// Check if tidepool-extract is available.
fn find_tidepool_extract() -> Option<PathBuf> {
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
// Secrets loader
// ---------------------------------------------------------------------------

fn load_secrets_from(dir: &std::path::Path) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return; // no secrets dir — nothing to do
    };
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        let valid_name = name.ends_with("_API_KEY")
            && name
                .chars()
                .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_');
        if !valid_name {
            tracing::warn!(
                "{}/{}: ignored (filename must be an env-var name ending in _API_KEY)",
                dir.display(),
                name
            );
            continue;
        }
        if std::env::var_os(&name).is_some_and(|v| !v.is_empty()) {
            tracing::info!("{name} already set in environment; secrets file ignored");
            continue;
        }
        match std::fs::read_to_string(entry.path()) {
            Ok(contents) => {
                let key = contents.trim();
                if key.is_empty() {
                    tracing::warn!("{}/{}: empty; ignored", dir.display(), name);
                } else {
                    std::env::set_var(&name, key);
                    tracing::info!("loaded {name} from {}", dir.display());
                }
            }
            Err(e) => tracing::warn!("{}/{}: unreadable: {e}", dir.display(), name),
        }
    }
}

fn load_secrets() {
    // Project-local first (walk up from CWD for `.tidepool/secrets`), then
    // user-global (`~/.config/tidepool/secrets`, legacy `~/.tidepool/secrets`).
    // `load_secrets_from` lets an already-set var win, so the FIRST source to
    // provide a key takes precedence — project overrides global.
    if let Ok(cwd) = std::env::current_dir() {
        if let Some(root) = tidepool_runtime::paths::find_project_root(&cwd) {
            load_secrets_from(&root.join(".tidepool").join("secrets"));
        }
    }
    for dir in tidepool_runtime::paths::global_secrets_dirs() {
        load_secrets_from(&dir);
    }
}

// ---------------------------------------------------------------------------
// Degraded MCP server — served when tidepool-extract is missing
// ---------------------------------------------------------------------------

#[derive(Clone)]
struct SetupServer;

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

    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("warn")),
        )
        .with_writer(std::io::stderr)
        .init();

    // Initialize the `log`-crate diagnostic logging for the JIT subsystems
    // (tidepool::calls / scope / heap / effects / fp). Routed to stderr via
    // env_logger; honors RUST_LOG plus the legacy TIDEPOOL_TRACE* env vars.
    // Independent of the tracing subscriber above (which owns the `tracing::`
    // macros at the MCP layer); env_logger owns the `log::` global logger.
    tidepool_codegen::debug::init_logging();

    // Fill missing *_API_KEY env vars from .tidepool/secrets/ (drop a key
    // file in, restart, done). Must run before any handler reads the env.
    load_secrets();

    let prelude_dir = ensure_prelude()?;

    // If tidepool-extract is not available, serve the degraded setup server.
    if find_tidepool_extract().is_none() {
        tracing::warn!(
            "tidepool-extract not found — serving setup-only MCP server. \
             Install via: nix profile install github:tidepool-heavy-industries/tidepool#tidepool-extract"
        );
        use rmcp::ServiceExt;
        if let Some(addr) = http_addr {
            use rmcp::transport::streamable_http_server::{
                session::local::LocalSessionManager, StreamableHttpServerConfig,
                StreamableHttpService,
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
            SgHandler::new(handler_cfg.cwd.clone()),
            HttpHandler,
            ExecHandler::new(handler_cfg.cwd.clone()),
            LspHandler::new(handler_cfg.cwd.clone()),
            MetaHandler::new(effect_names, helper_sigs),
            LlmHandler::new(model.clone())
        ];
        let server = TidepoolMcpServer::new(handlers)
            .with_prelude(prelude_dir)
            .with_help_tool(args.help_tool);
        if let Some(addr) = http_addr {
            server.serve_http(addr).await
        } else {
            server.serve_stdio().await
        }
    } else {
        let handlers = tidepool_handlers::build_base_stack(&handler_cfg);
        let server = TidepoolMcpServer::new(handlers)
            .with_prelude(prelude_dir)
            .with_help_tool(args.help_tool);
        if let Some(addr) = http_addr {
            server.serve_http(addr).await
        } else {
            server.serve_stdio().await
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_load_secrets_from() {
        let dir =
            std::env::temp_dir().join(format!("tidepool-secrets-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();

        // Valid key file, var unset -> loaded (trimmed)
        std::env::remove_var("TIDEPOOL_TEST_DUMMY_API_KEY");
        std::fs::write(dir.join("TIDEPOOL_TEST_DUMMY_API_KEY"), "sk-test-123\n").unwrap();
        // Invalid names / non-key files -> ignored
        std::fs::write(dir.join("notes.txt"), "not a key").unwrap();
        std::fs::write(dir.join("lower_api_key"), "nope").unwrap();
        // Env already set -> file does NOT override
        std::env::set_var("TIDEPOOL_TEST_PRESET_API_KEY", "from-env");
        std::fs::write(dir.join("TIDEPOOL_TEST_PRESET_API_KEY"), "from-file").unwrap();

        load_secrets_from(&dir);

        assert_eq!(
            std::env::var("TIDEPOOL_TEST_DUMMY_API_KEY").unwrap(),
            "sk-test-123"
        );
        assert_eq!(
            std::env::var("TIDEPOOL_TEST_PRESET_API_KEY").unwrap(),
            "from-env"
        );
        assert!(std::env::var("notes.txt").is_err());

        std::env::remove_var("TIDEPOOL_TEST_DUMMY_API_KEY");
        std::env::remove_var("TIDEPOOL_TEST_PRESET_API_KEY");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
