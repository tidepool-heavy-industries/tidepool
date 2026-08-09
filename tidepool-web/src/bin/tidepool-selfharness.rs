//! `tidepool-selfharness` — boots the self-iterating harness driver
//! (`plans/self-iterating-harness/`): the outer `render`/`loop` alternation
//! over a nested [`tidepool_harness::Harness`], per
//! `plans/self-iterating-harness/07-impl-orchestration.md`.
//!
//! Provider select (OAuth default, `--replay <log>` for deterministic
//! replay, `--api-key <ENV_VAR>` for a non-interactive API-key provider),
//! engine config, a fresh run log, then `.await`s
//! [`tidepool_harness::SelfHarnessDriver::run_loop`]. Multi-thread tokio
//! runtime required: the driver's turn loop is `async fn`, but the one place
//! it still blocks a worker thread is the sync-blocking `OperatorGate` park
//! (`tokio::task::block_in_place`, see `driver.rs`'s module doc) — that
//! requires the multi-thread runtime flavor.
//!
//! Boots the operator GUI ([`tidepool_web::spawn_operator_server`]) and wires
//! its [`tidepool_web::WebGate`] into the driver before `run_loop` — UNLESS
//! `--yes`/`--auto`/`--replay` is set, in which case the driver keeps its
//! default headless `StdinGate` (no browser needed for CI/replay/unattended
//! runs).

use std::path::PathBuf;
use std::sync::Arc;

use tidepool_harness::engine::EngineConfig;
use tidepool_harness::log::{LogHeader, LogWriter};
use tidepool_harness::provider::api_key::{ApiKeyConfig, ApiKeyProvider};
use tidepool_harness::provider::oauth::{OauthConfig, OauthProvider};
use tidepool_harness::provider::DynModelProvider;
use tidepool_harness::replay::ReplayProvider;
use tidepool_harness::selfharness::persistence;
use tidepool_harness::{
    answerer_decls, load_harness_source, Event, Harness, JsonlObserver, LogObserver, Observer,
    SelfHarnessDriver,
};

/// Dispatches every driver [`Event`] to each of several observers — lets the
/// bin wire both stderr logging and the durable transcript without
/// `SelfHarnessDriver` itself knowing about more than one [`Observer`].
struct FanoutObserver {
    observers: Vec<Arc<dyn Observer>>,
}

impl Observer for FanoutObserver {
    fn on_event(&self, event: &Event) {
        for o in &self.observers {
            o.on_event(event);
        }
    }
}

#[tokio::main(flavor = "multi_thread")]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .init();

    let args: Vec<String> = std::env::args().collect();

    let replay_log = arg_value(&args, "--replay");
    let api_key_env = arg_str(&args, "--api-key");
    let harness_source_path =
        arg_value(&args, "--harness").unwrap_or_else(default_harness_source_path);
    // Skip the between-loops "press Enter" human gate (W1 runaway cap 3) — for
    // CI/replay/unattended runs. Replay mode implies `--auto` (no operator to
    // press Enter against a recorded run).
    let auto = args.iter().any(|a| a == "--yes" || a == "--auto") || replay_log.is_some();

    tracing::info!(
        target: "tidepool_web",
        path = %harness_source_path.display(),
        "loading harness source"
    );
    let source = load_harness_source(&harness_source_path)?;

    let prelude_dir = prelude_dir();
    let project_lib = project_lib_dir();
    // The nested answerer's SCOPED stack (gui + finalize, base effects dropped
    // — W1 effect-scoping), not the full Agent stack.
    let mut cfg = EngineConfig::from_decls(answerer_decls(), prelude_dir, project_lib)?;
    // So a sibling `HarnessTypes` module the harness source depends on
    // resolves under the answerer's own compile too (mirrors the outer
    // session's `outer_cfg.include.push(source.source_dir.clone())` in
    // `driver.rs::bootstrap`).
    cfg.include.push(source.source_dir.clone());

    let provider: Arc<dyn DynModelProvider> = match (&replay_log, &api_key_env) {
        (Some(log), _) => {
            tracing::info!(target: "tidepool_web", path = %log.display(), "replay mode");
            Arc::new(ReplayProvider::from_log(log)?)
        }
        (None, Some(env_var)) => {
            let model =
                std::env::var("TIDEPOOL_LLM_MODEL").unwrap_or_else(|_| "gpt-4o-mini".to_string());
            tracing::info!(target: "tidepool_web", env_var, model, "API-key mode");
            Arc::new(ApiKeyProvider::new(ApiKeyConfig::new(
                env_var.clone(),
                model,
            )))
        }
        (None, None) => {
            // OAuth (ChatGPT/Codex account) rejects standard-API model names like
            // gpt-4o-mini; default to a Codex-supported model.
            let model =
                std::env::var("TIDEPOOL_LLM_MODEL").unwrap_or_else(|_| "gpt-5.6-terra".to_string());
            Arc::new(OauthProvider::new(OauthConfig::new(model)))
        }
    };

    // The per-node durable log sits beside transcript.jsonl + checkpoint.json under
    // <cache>/selfharness/. LogWriter refuses to overwrite an existing run's
    // log, so each run gets a fresh timestamped file; tail the newest.
    let log_dir = persistence::default_log_path()
        .parent()
        .map(std::path::Path::to_path_buf)
        .unwrap_or_else(std::env::temp_dir);
    let _ = std::fs::create_dir_all(&log_dir);
    let ts = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let log_path = log_dir.join(format!("log-{ts}.jsonl"));
    let header = LogHeader {
        prelude_hash: "self-harness".to_string(),
        extract_fingerprint: cfg.extract_bin.clone(),
        harness_version: env!("CARGO_PKG_VERSION").to_string(),
    };
    let writer = LogWriter::create(&log_path, &header)?;
    tracing::info!(target: "tidepool_web", path = %log_path.display(), "run log");

    // The nested Harness: an ordinary node-tree orchestrator, used ONLY to
    // answer `runLLMTurn` holes by driving an Agent turn loop to `finalize`
    // (WS-A + WS-B).
    let agent = Arc::new(Harness::new(writer, cfg, provider)?);

    let transcript_path = persistence::default_transcript_path();
    let jsonl = JsonlObserver::create(&transcript_path)?;
    tracing::info!(target: "tidepool_web", path = %transcript_path.display(), "transcript");
    let observer: Arc<dyn Observer> = Arc::new(FanoutObserver {
        observers: vec![Arc::new(LogObserver), Arc::new(jsonl)],
    });
    let mut driver = SelfHarnessDriver::new(agent, observer);

    if !auto {
        let port: u16 = arg_str(&args, "--port")
            .and_then(|s| s.parse().ok())
            .unwrap_or(4600);
        let gate = tidepool_web::spawn_operator_server(port).await?;
        driver.set_gate(gate);
    }

    driver.run_loop(&source, auto).await?;

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
