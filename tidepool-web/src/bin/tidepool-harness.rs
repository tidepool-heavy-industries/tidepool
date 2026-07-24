//! `tidepool-harness` — the observatory binary. Boots the harness over a fresh
//! (or restored) event log, signs in via the ChatGPT-subscription OAuth
//! provider, and serves the loopback observatory.
//!
//! # Boot modes
//!
//! - Fresh run (default): a new timestamped log file, a live OAuth provider.
//! - `--replay <log>`: crash-replay + record-replay. Fold the existing log to
//!   report the terminal tree state, then serve a NEW run driven by a
//!   [`ReplayProvider`] over the recorded assistant turns — zero live calls.
//!   This is the golden-path CI shape (also usable by hand).
//!
//! Loopback bind only (127.0.0.1) — reachability is the authorization boundary.

use std::path::PathBuf;
use std::sync::Arc;

use tidepool_harness::engine::EngineConfig;
use tidepool_harness::log::{LogHeader, LogWriter};
use tidepool_harness::provider::oauth::{OauthConfig, OauthProvider};
use tidepool_harness::provider::DynModelProvider;
use tidepool_harness::replay::{fold_tree_state, ReplayProvider};
use tidepool_harness::Harness;
use tidepool_web::server::AppState;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = std::env::args().collect();
    let replay_log = arg_value(&args, "--replay");
    let port: u16 = arg_str(&args, "--port")
        .and_then(|p| p.parse().ok())
        .unwrap_or(4600);

    // Resolve the include paths: the stdlib (haskell/lib in-repo, or
    // TIDEPOOL_PRELUDE_DIR) and, if present, a project .tidepool/lib.
    let prelude_dir = prelude_dir();
    let project_lib = project_lib_dir();
    let cfg = EngineConfig::standard(prelude_dir, project_lib)?;

    // The provider: replay (recorded turns) or live OAuth.
    let (provider, model): (Arc<dyn DynModelProvider>, String) = match &replay_log {
        Some(log) => {
            eprintln!("[boot] replay mode over {}", log.display());
            let folded = fold_tree_state(log)?;
            eprintln!(
                "[boot] folded {} nodes from the prior log; terminal states:",
                folded.states.len()
            );
            for (node, state) in &folded.states {
                eprintln!("        n{} -> {state:?}", node.0);
            }
            let replay = ReplayProvider::from_log(log)?;
            eprintln!("[boot] {} recorded assistant turns queued", replay.remaining());
            (Arc::new(replay), "replay".to_string())
        }
        None => {
            let model = std::env::var("TIDEPOOL_LLM_MODEL")
                .unwrap_or_else(|_| "gpt-5".to_string());
            let oauth = OauthConfig::new(model.clone());
            (Arc::new(OauthProvider::new(oauth)), model)
        }
    };

    // A fresh run log (always new — a run is never appended to an old one).
    let log_path = new_log_path();
    let header = LogHeader {
        prelude_hash: "harness-r0".to_string(),
        extract_fingerprint: cfg.extract_bin.clone(),
        harness_version: env!("CARGO_PKG_VERSION").to_string(),
    };
    let writer = LogWriter::create(&log_path, &header)?;
    eprintln!("[boot] run log: {}", log_path.display());

    let harness = Arc::new(Harness::new(writer, cfg, provider)?);

    let oauth_cfg = OauthConfig::new(model);
    let state = AppState::new(harness, log_path, oauth_cfg);
    let app = tidepool_web::server::router(state);

    let addr = format!("127.0.0.1:{port}");
    let listener = tokio::net::TcpListener::bind(&addr).await?;
    eprintln!("[boot] observatory on http://{addr}");
    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal())
        .await?;
    Ok(())
}

async fn shutdown_signal() {
    let _ = tokio::signal::ctrl_c().await;
    eprintln!("\n[boot] shutting down");
}

fn arg_value(args: &[String], flag: &str) -> Option<PathBuf> {
    arg_str(args, flag).map(PathBuf::from)
}

fn arg_str(args: &[String], flag: &str) -> Option<String> {
    let idx = args.iter().position(|a| a == flag)?;
    args.get(idx + 1).cloned()
}

/// The stdlib include dir: `TIDEPOOL_PRELUDE_DIR`, else the in-repo
/// `haskell/lib` relative to the manifest.
fn prelude_dir() -> PathBuf {
    if let Ok(dir) = std::env::var("TIDEPOOL_PRELUDE_DIR") {
        return PathBuf::from(dir);
    }
    // From tidepool-web/, the repo root is the parent; haskell/lib under it.
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    manifest
        .parent()
        .map(|r| r.join("haskell/lib"))
        .unwrap_or_else(|| PathBuf::from("haskell/lib"))
}

fn project_lib_dir() -> Option<PathBuf> {
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let lib = manifest.parent().map(|r| r.join(".tidepool/lib"))?;
    lib.exists().then_some(lib)
}

fn new_log_path() -> PathBuf {
    let ts = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let dir = tidepool_runtime_paths_logs();
    let _ = std::fs::create_dir_all(&dir);
    dir.join(format!("harness-run-{ts}.jsonl"))
}

/// Log dir: `$XDG_STATE_HOME/tidepool/logs` or `~/.local/state/tidepool/logs`,
/// falling back to a temp dir.
fn tidepool_runtime_paths_logs() -> PathBuf {
    if let Ok(state) = std::env::var("XDG_STATE_HOME") {
        return PathBuf::from(state).join("tidepool/logs");
    }
    if let Ok(home) = std::env::var("HOME") {
        return PathBuf::from(home).join(".local/state/tidepool/logs");
    }
    std::env::temp_dir().join("tidepool-logs")
}
