//! `tidepool-selfharness` — boots the self-iterating harness driver
//! (`plans/self-iterating-harness/`): the outer `render`/`loop` alternation
//! over a nested [`tidepool_harness::Harness`], per
//! `plans/self-iterating-harness/07-impl-orchestration.md`.
//!
//! Modeled on `tidepool-harness.rs` (this crate's other binary)'s boot
//! sequence — provider select (OAuth default, `--replay <log>` for
//! deterministic replay, `--api-key <ENV_VAR>` for a non-interactive
//! API-key provider), engine config, a fresh run log — but drives
//! [`tidepool_harness::SelfHarnessDriver::run_loop`] instead of serving the
//! observatory. Multi-thread tokio runtime required: `run_loop` services
//! each `runLLMTurn` hole via `block_in_place` + `Handle::current().block_on`
//! (see `driver.rs`'s module doc).

use std::path::PathBuf;
use std::sync::Arc;

use tidepool_harness::engine::EngineConfig;
use tidepool_harness::log::{LogHeader, LogWriter};
use tidepool_harness::provider::api_key::{ApiKeyConfig, ApiKeyProvider};
use tidepool_harness::provider::oauth::{OauthConfig, OauthProvider};
use tidepool_harness::provider::DynModelProvider;
use tidepool_harness::replay::ReplayProvider;
use tidepool_harness::{load_harness_source, Harness, LogObserver, SelfHarnessDriver};

#[tokio::main(flavor = "multi_thread")]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = std::env::args().collect();

    let replay_log = arg_value(&args, "--replay");
    let api_key_env = arg_str(&args, "--api-key");
    let harness_source_path =
        arg_value(&args, "--harness").unwrap_or_else(default_harness_source_path);

    let prelude_dir = prelude_dir();
    let project_lib = project_lib_dir();
    let cfg = EngineConfig::standard(prelude_dir, project_lib)?;

    let provider: Arc<dyn DynModelProvider> = match (&replay_log, &api_key_env) {
        (Some(log), _) => {
            eprintln!("[boot] replay mode over {}", log.display());
            Arc::new(ReplayProvider::from_log(log)?)
        }
        (None, Some(env_var)) => {
            let model =
                std::env::var("TIDEPOOL_LLM_MODEL").unwrap_or_else(|_| "gpt-4o-mini".to_string());
            eprintln!("[boot] API-key mode ({env_var}), model {model}");
            Arc::new(ApiKeyProvider::new(ApiKeyConfig::new(
                env_var.clone(),
                model,
            )))
        }
        (None, None) => {
            let model =
                std::env::var("TIDEPOOL_LLM_MODEL").unwrap_or_else(|_| "gpt-4o-mini".to_string());
            Arc::new(OauthProvider::new(OauthConfig::new(model)))
        }
    };

    let log_path = new_log_path();
    let header = LogHeader {
        prelude_hash: "self-harness".to_string(),
        extract_fingerprint: cfg.extract_bin.clone(),
        harness_version: env!("CARGO_PKG_VERSION").to_string(),
    };
    let writer = LogWriter::create(&log_path, &header)?;
    eprintln!("[boot] run log: {}", log_path.display());

    // The nested Harness: an ordinary node-tree orchestrator, used ONLY to
    // answer `runLLMTurn` holes by driving an Agent turn loop to `finalize`
    // (WS-A + WS-B).
    let agent = Arc::new(Harness::new(writer, cfg, provider)?);

    let observer = Arc::new(LogObserver);
    let mut driver = SelfHarnessDriver::new(agent, observer);

    eprintln!(
        "[boot] loading harness source from {}",
        harness_source_path.display()
    );
    let source = load_harness_source(&harness_source_path)?;
    driver.run_loop(&source)?;

    Ok(())
}

fn arg_value(args: &[String], flag: &str) -> Option<PathBuf> {
    arg_str(args, flag).map(PathBuf::from)
}

fn arg_str(args: &[String], flag: &str) -> Option<String> {
    let idx = args.iter().position(|a| a == flag)?;
    args.get(idx + 1).cloned()
}

fn default_harness_source_path() -> PathBuf {
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    manifest
        .parent()
        .map(|r| r.join("examples/harness/Harness.hs"))
        .unwrap_or_else(|| PathBuf::from("examples/harness/Harness.hs"))
}

/// The stdlib include dir: `TIDEPOOL_PRELUDE_DIR`, else the in-repo
/// `haskell/lib` relative to the manifest.
fn prelude_dir() -> PathBuf {
    if let Ok(dir) = std::env::var("TIDEPOOL_PRELUDE_DIR") {
        return PathBuf::from(dir);
    }
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
    dir.join(format!("selfharness-run-{ts}.jsonl"))
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
