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

use tidepool_handlers::{
    ConsoleHandler, EventConfig, ExecHandler, RepoEventHandler, WorktreeHandler,
};
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
use tidepool_worktree::{EventJournal, GitCli, WorktreeMonitor};

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

    let prelude_dir = prelude_dir()?;
    let project_lib = project_lib_dir();
    // The nested answerer's SCOPED stack (gui + finalize, base effects dropped
    // — W1 effect-scoping), not the full Agent stack.
    let mut cfg = EngineConfig::from_decls(answerer_decls(), prelude_dir, project_lib)?;
    // So a sibling `HarnessTypes` module the harness source depends on
    // resolves under the answerer's own compile too.
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
        // `ResolvedExtractBin`'s `Display` renders the same path text a
        // plain `String` did before that type existed.
        extract_fingerprint: cfg.extract_bin.to_string(),
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

    // PRD 20 S1-L4: the concurrency cap for concurrently-serviced
    // fanout/fork `RunLLMTurn` windows (default 8 — see
    // `SelfHarnessDriver::set_concurrency_cap`'s doc).
    if let Some(cap) = arg_str(&args, "--concurrency").and_then(|s| s.parse().ok()) {
        driver.set_concurrency_cap(cap);
    }

    if !auto {
        let port: u16 = arg_str(&args, "--port")
            .and_then(|s| s.parse().ok())
            .unwrap_or(4600);
        let gate = tidepool_web::spawn_operator_server(port).await?;
        driver.set_gate(gate);
    }

    // S1-L1 (plans/self-iterating-harness/20-exomonad-v3-prd.md): the
    // Console/Worktree/RepoEvent/Exec seams for the AUTHORED outer loop —
    // always wired (Console has no external state; Worktree/RepoEvent/Exec
    // are scoped to TIDEPOOL_SOURCE_REPO, defaulting to the repo this process
    // runs in), unlike the optional Subagent seam below.
    let source_repo = match std::env::var_os("TIDEPOOL_SOURCE_REPO") {
        Some(p) => PathBuf::from(p),
        None => std::env::current_dir()?,
    };
    let (console_handler, worktree_handler, event_handler, exec_handler) =
        build_outer_handlers(&source_repo)?;
    driver.set_console_handler(console_handler);
    driver.set_worktree_handler(worktree_handler);
    driver.set_event_handler(event_handler);
    driver.set_exec_handler(exec_handler);
    // The run journal (PRD 20 S1-L5): identity comes from the RUN LEASE, not
    // from this process. `acquire_lease` resumes the run a prior process left
    // behind (a crash leaves the lease on disk) or mints a fresh one — either
    // way one journal file per RUN, appended across however many processes the
    // run takes. Per-process naming would fold nothing and orphan the prior
    // file, which is exactly what resume exists to avoid.
    //
    // `open_run_journal` is the ONE seam: it loads that journal, folds it, and
    // builds the appending handler seeded past what is already on disk — all
    // from the lease's single path, so the fold and the appends cannot desync.
    let acquired = tidepool_harness::acquire_lease(&log_dir)?;
    let folded = driver.open_run_journal(&acquired.lease)?;
    tracing::info!(
        target: "tidepool_web",
        repo = %source_repo.display(),
        journal = %acquired.lease.journal.display(),
        run_id = %acquired.lease.run_id,
        resumed = acquired.resumed,
        folded_entries = folded,
        "outer effect seam wired (Console/Worktree/RepoEvent/Exec/Journal)"
    );

    // The subagent seam (plans/companion-memory.md): when TIDEPOOL_MEMORY_REPO
    // names the companion's memory store, wire a driver-owned SubagentHandler
    // over it so the authored loop's `spawnAgent` (the memory curator) is
    // serviced. Absent env → absent handler → a Subagent suspension fails
    // with the legible wiring error, exactly as before.
    if let Some(repo) = std::env::var_os("TIDEPOOL_MEMORY_REPO").map(PathBuf::from) {
        let handler = build_subagent_handler(&repo)?;
        driver.set_subagent_handler(handler);
        tracing::info!(
            target: "tidepool_web",
            repo = %repo.display(),
            "subagent seam wired (memory curator; Codex backend, operator credentials)"
        );
    }

    driver.run_loop(&source, auto).await?;

    // A NORMAL return retires the lease — renamed to `run-<runId>.json`, kept
    // beside the journal, never deleted — so the next boot mints a fresh run
    // instead of resuming a finished one. A crash skips this by construction,
    // which is precisely how the next boot knows to resume.
    if let Some(retired) = tidepool_harness::retire_lease(&log_dir)? {
        tracing::info!(
            target: "tidepool_web",
            lease = %retired.display(),
            "run completed; lease retired"
        );
    }

    Ok(())
}

/// The durable data root (NOT the regenerable cache — worktree state must
/// survive cache clears): `$XDG_DATA_HOME`, or `$HOME/.local/share` when
/// unset.
fn xdg_data_root() -> Result<PathBuf, Box<dyn std::error::Error>> {
    std::env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".local/share")))
        .ok_or_else(|| "neither XDG_DATA_HOME nor HOME is set".into())
}

/// The registry/worktree roots EVERY worktree-adjacent handler shares
/// (`WorktreeHandler`, `RepoEventHandler`'s monitor, and — when
/// TIDEPOOL_MEMORY_REPO is set — the Subagent handler): a `WorktreeId` minted
/// through one resolves through the others. Outside any git work tree (the
/// registry refuses otherwise) — same paths [`build_subagent_handler`] always
/// used, just factored out so the two call sites cannot drift apart.
fn shared_worktree_roots() -> Result<(PathBuf, PathBuf), Box<dyn std::error::Error>> {
    let base = xdg_data_root()?.join("tidepool/subagent");
    Ok((base.join("registry"), base.join("worktrees")))
}

/// Build the S1-L1 Console/Worktree/RepoEvent/Exec handlers: Console has no
/// external state; Worktree and RepoEvent's `WorktreeMonitor` share their
/// registry/worktree roots ([`shared_worktree_roots`]) and are scoped to
/// `source_repo`; Exec's sandbox roots at the managed-worktrees root so `run`/
/// `runIn` can operate inside a worktree by relative path.
fn build_outer_handlers(
    source_repo: &std::path::Path,
) -> Result<
    (
        ConsoleHandler,
        WorktreeHandler,
        RepoEventHandler,
        ExecHandler,
    ),
    Box<dyn std::error::Error>,
> {
    let (registry_root, worktree_root) = shared_worktree_roots()?;
    let worktree_handler = WorktreeHandler::new(
        registry_root,
        worktree_root.clone(),
        source_repo.to_path_buf(),
    )?;
    let journal_path = xdg_data_root()?.join("tidepool/repo-events.jsonl");
    let journal = EventJournal::open(&journal_path)?;
    let monitor = WorktreeMonitor::new(GitCli::new(), journal);
    let event_handler = RepoEventHandler::new(monitor, EventConfig::default());
    let exec_handler = ExecHandler::new(worktree_root);
    Ok((
        ConsoleHandler,
        worktree_handler,
        event_handler,
        exec_handler,
    ))
}

/// Build the memory curator's [`tidepool_handlers::SubagentHandler`]: source
/// repository = the memory store; registry/worktree/binding roots under the
/// durable data dir (NOT the regenerable cache — worktree state must survive
/// cache clears — and outside any git work tree, which the registry refuses).
/// Backend: the live Codex adapter over the operator's own `~/.codex`
/// credentials, at the default cheap-plumbing model policy.
fn build_subagent_handler(
    repo: &std::path::Path,
) -> Result<tidepool_handlers::SubagentHandler, Box<dyn std::error::Error>> {
    if !repo.join(".git").exists() {
        return Err(format!(
            "TIDEPOOL_MEMORY_REPO={} is not a git repository (no .git). Bootstrap the \
             memory store first: scripts/companion-memory-init.sh {}",
            repo.display(),
            repo.display()
        )
        .into());
    }
    let (registry_root, worktree_root) = shared_worktree_roots()?;
    let binding_root = xdg_data_root()?.join("tidepool/subagent/bindings");
    let backend = tidepool_agent::backend::codex::CodexAgentBackend::new()?;
    let handler = tidepool_handlers::SubagentHandler::new(
        registry_root,
        worktree_root,
        binding_root,
        repo.to_path_buf(),
        Box::new(backend),
    )?;
    Ok(handler)
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

/// The stdlib include dir, via the ONE locator
/// ([`tidepool_runtime::toolchain::locate_stdlib`], whose module docs carry the
/// precedence table). This driver embeds no stdlib, so it contributes only the
/// build-tree tail step — the same shape as `tidepool-repl`.
///
/// # Errors
/// [`tidepool_runtime::toolchain::ToolchainError`] when no step of the table
/// finds a stdlib root.
fn prelude_dir() -> Result<PathBuf, tidepool_runtime::toolchain::ToolchainError> {
    let fallbacks = tidepool_runtime::toolchain::StdlibFallbacks {
        bundle: None,
        build_tree: PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .map(|r| r.join("haskell/lib")),
    };
    Ok(tidepool_runtime::toolchain::locate_stdlib(&fallbacks)?.dir)
}

fn project_lib_dir() -> Option<PathBuf> {
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let lib = manifest.parent().map(|r| r.join(".tidepool/lib"))?;
    lib.exists().then_some(lib)
}
