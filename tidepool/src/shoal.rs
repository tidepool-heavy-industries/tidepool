//! Shoal process composition and one-command tmux bootstrap.
//!
//! One host process owns every resident Haskell actor. Interactive actors are
//! ordinary interactive-agent TUIs launched through exact supervisors in tmux panes. Each actor
//! receives its resident tools through an actor-scoped Unix socket.

use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Mutex;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use tidepool_actor::ActorRef;
use tidepool_agent::{
    copy_interactive_binding, read_interactive_binding, BackendThreadId,
    InteractiveAgentInstallation, InteractiveLaunchMode, ReasoningEffort,
};
use tidepool_node::{TmuxLaunch, TmuxSession};
use tokio::io::AsyncReadExt;
use tokio::sync::mpsc;
use tracing::Instrument;
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;
use tracing_subscriber::Layer;

use crate::actor_host::ACTOR_PROJECT_ROOT;

pub(crate) const STATUS_VERSION: u32 = 5;
const PREVIOUS_STATUS_VERSION: u32 = 4;
// Root startup includes up to five minutes of resource admission before launch.
const INTERACTIVE_START_TIMEOUT: Duration = Duration::from_secs(420);
pub mod resources;
mod scaffold;
pub(crate) mod source;
pub mod workspace;

pub use scaffold::{FlakeLock, NewRefusal, NixLock};

const SHOAL_CONFIG: &str = ".shoal/config.toml";
const ENV_PACKAGED_CODEX_CLOSURE: &str = "TIDEPOOL_SHOAL_CODEX_CLOSURE";
const ENV_NIX_STORE_BIN: &str = "TIDEPOOL_SHOAL_NIX_STORE_BIN";
/// The `nix` executable that fetches the project's flake inputs when
/// `[haskell.flake_sources]` pins Haskell source outside the workspace. Set by
/// the packaged `shoal` wrapper and the dev shell; otherwise the one on `PATH`.
const ENV_NIX_BIN: &str = "TIDEPOOL_SHOAL_NIX_BIN";
const GC_ROOT_TIMEOUT: Duration = Duration::from_secs(30);
const GC_ROOT_ERROR_LIMIT: usize = 16 * 1024;
const BOUNDARY_PROBE_TIMEOUT: Duration = Duration::from_secs(10);
const BOUNDARY_PROBE_ERROR_LIMIT: usize = 16 * 1024;
const COMPILER_DAEMON_START_TIMEOUT: Duration = Duration::from_secs(30);
const COMPILER_DAEMON_POLL_INTERVAL: Duration = Duration::from_millis(50);

pub struct NewOptions {
    pub path: Option<PathBuf>,
    /// How the `flake.nix` the scaffolding writes is locked. Production locks
    /// it with `nix`; a caller that must not reach the network supplies its
    /// own.
    pub lock: Box<dyn FlakeLock>,
}

impl Default for NewOptions {
    fn default() -> Self {
        Self {
            path: None,
            lock: Box::new(NixLock),
        }
    }
}

pub struct InitOptions {
    pub workspace: Option<PathBuf>,
    pub session: Option<String>,
    pub recreate: bool,
    pub no_attach: bool,
    pub model: Option<String>,
    pub effort: Option<ShoalEffort>,
}

pub struct HostOptions {
    pub workspace: PathBuf,
    pub session: String,
    pub run_id: String,
    pub run_root: PathBuf,
    pub status_path: PathBuf,
    pub root_binding_path: PathBuf,
    pub interactive_agent: InteractiveAgentInstallation,
    pub resume_root: bool,
    pub agent: ShoalAgentDefaults,
}

/// Early CLI boundary for the private per-launch process supervisor.
///
/// This dispatch is called by the binary before it constructs Tokio, compiler,
/// provider, or actor-host state. The node helper owns exactly one manifest and
/// never falls back to pane/PID supervision.
pub fn process_supervisor(manifest: PathBuf) -> Result<(), Box<dyn std::error::Error>> {
    tidepool_node::run_process_supervisor(&manifest).map_err(Into::into)
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ShoalAgentDefaults {
    pub model: String,
    #[serde(default)]
    pub effort: ShoalEffort,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ShoalEffort {
    #[default]
    Low,
    Medium,
    High,
}

impl From<ReasoningEffort> for ShoalEffort {
    fn from(value: ReasoningEffort) -> Self {
        match value {
            ReasoningEffort::Low => Self::Low,
            ReasoningEffort::Medium => Self::Medium,
            ReasoningEffort::High => Self::High,
        }
    }
}

impl From<ShoalEffort> for ReasoningEffort {
    fn from(value: ShoalEffort) -> Self {
        match value {
            ShoalEffort::Low => Self::Low,
            ShoalEffort::Medium => Self::Medium,
            ShoalEffort::High => Self::High,
        }
    }
}

impl std::fmt::Display for ShoalEffort {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(match self {
            Self::Low => "low",
            Self::Medium => "medium",
            Self::High => "high",
        })
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ShoalConfig {
    #[serde(default)]
    pub(crate) launch: LaunchConfig,
    #[serde(default)]
    pub(crate) resources: tidepool_node::command_resources::CommandResourcePolicy,
    pub(crate) defaults: ShoalAgentDefaults,
    #[serde(default)]
    pub(crate) research: tidepool_actor::ResearchPolicy,
    #[serde(default)]
    pub(crate) models: std::collections::BTreeMap<String, String>,
    #[serde(default)]
    haskell: workspace::HaskellConfig,
    #[serde(default)]
    prompts: workspace::PromptConfig,
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub(crate) struct LaunchConfig {
    pub(crate) systemd_slice: tidepool_node::systemd_slice::SystemdSlice,
    pub(crate) source_exclude: Vec<String>,
}

impl LaunchConfig {
    pub(crate) fn validate(&self) -> std::io::Result<()> {
        let mut seen = std::collections::BTreeSet::new();
        for name in &self.source_exclude {
            if name.is_empty()
                || matches!(name.as_str(), "." | ".." | ".git" | ".shoal")
                || name.contains(['/', '\0', '*', '?', '[', ']'])
                || !seen.insert(name)
            {
                return Err(std::io::Error::other(format!(
                    "invalid [launch].source_exclude directory name {name:?}"
                )));
            }
        }
        Ok(())
    }
}

fn validate_tracked_exclusions(workspace: &Path, excluded: &[String]) -> std::io::Result<()> {
    if excluded.is_empty() {
        return Ok(());
    }
    let git = tidepool_worktree::GitCli::new();
    for name in excluded {
        if source_directory_has_tracked(&git, workspace, name)? {
            return Err(std::io::Error::other(format!(
                "[launch].source_exclude {name:?} contains tracked source"
            )));
        }
    }
    Ok(())
}

pub(crate) fn source_directory_has_tracked(
    git: &tidepool_worktree::GitCli,
    workspace: &Path,
    name: &str,
) -> std::io::Result<bool> {
    let path = format!("{name}/");
    let staged = git
        .try_run(workspace, &["ls-files", "--cached", "-z", "--", &path])
        .map_err(std::io::Error::other)?;
    if !staged.stdout.is_empty() {
        return Ok(true);
    }
    if git
        .try_run(workspace, &["rev-parse", "--verify", "HEAD"])
        .is_err()
    {
        return Ok(false);
    }
    let committed = git
        .try_run(
            workspace,
            &["ls-tree", "-r", "-z", "--name-only", "HEAD", "--", &path],
        )
        .map_err(std::io::Error::other)?;
    Ok(!committed.stdout.is_empty())
}

/// Scaffold a Shoal workspace package.
///
/// This is the one command that writes a `.shoal/config.toml`. It takes an
/// empty directory, which it makes a repository and commits, or the root of an
/// existing one, which it stages and leaves for the project to commit.
/// Authored configuration is committed normally; only runtime artifacts are
/// excluded locally through Git metadata.
pub fn new(options: NewOptions) -> Result<(), Box<dyn std::error::Error>> {
    let workspace = match options.path {
        Some(path) => path,
        None => std::env::current_dir()?,
    };
    let report = scaffold::scaffold(&workspace, options.lock.as_ref())?;
    let workspace = std::fs::canonicalize(&workspace)?;
    println!("Shoal workspace package at {}", workspace.display());
    for path in &report.written {
        println!("  {}", path.display());
    }
    match report.jev {
        scaffold::JevPin::Locked => {}
        scaffold::JevPin::Unlocked(error) => {
            print!("{}", scaffold::unlocked_message(&workspace, error.as_ref()))
        }
        scaffold::JevPin::ProjectFlake => print!("{}", scaffold::project_flake_hint(&workspace)),
    }
    if report.target == scaffold::Target::Repository {
        println!(
            "The package is staged, not committed. Child actors are launched from committed checkouts, so commit it before delegating."
        );
    }
    println!("Next: shoal check --workspace {}", workspace.display());
    println!("Then: shoal init");
    Ok(())
}

/// Why a project's Shoal configuration could not be read.
#[derive(Debug)]
pub enum ConfigError {
    /// The path carries no Shoal workspace. Every command but `shoal new`
    /// stops here rather than inventing one.
    NoWorkspace {
        workspace: PathBuf,
        config: PathBuf,
        /// The launch `cwd`, set only when `workspace` was auto-detected (no
        /// `--workspace` given) by walking up from it — so a failure caused
        /// by the walk settling on an unrelated ancestor names both the
        /// directory the operator was actually in and the one Shoal decided
        /// to use, instead of only the latter.
        searched_from: Option<PathBuf>,
    },
    /// A workspace whose configuration is unreadable, or describes a run that
    /// cannot start.
    Rejected(Box<dyn std::error::Error>),
}

impl std::fmt::Display for ConfigError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NoWorkspace {
                workspace,
                config,
                searched_from,
            } => {
                write!(
                    formatter,
                    "no Shoal workspace in {}: there is no {}. `shoal new` creates one.",
                    workspace.display(),
                    config.display()
                )?;
                if let Some(cwd) = searched_from {
                    if cwd != workspace {
                        write!(
                            formatter,
                            " (searched upward from {} and settled on {}; pass --workspace to target a specific directory)",
                            cwd.display(),
                            workspace.display()
                        )?;
                    }
                }
                Ok(())
            }
            Self::Rejected(error) => error.fmt(formatter),
        }
    }
}

impl std::error::Error for ConfigError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::NoWorkspace { .. } => None,
            Self::Rejected(error) => Some(error.as_ref()),
        }
    }
}

/// Read a project's configuration. A pure read: a project without one is a
/// project `shoal new` has not been run in, not a project to scaffold here.
fn read_project_config(workspace: &Path) -> Result<(ShoalConfig, String), ConfigError> {
    let path = workspace.join(SHOAL_CONFIG);
    let text = match std::fs::read_to_string(&path) {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Err(ConfigError::NoWorkspace {
                workspace: workspace.to_path_buf(),
                config: path,
                // This call site only ever sees an already-resolved
                // workspace path (explicit `--workspace`, or one
                // `resolve_workspace` has already vetted); the cwd-detection
                // gap this field exists for is caught earlier, in
                // `resolve_workspace` itself.
                searched_from: None,
            });
        }
        Err(error) => {
            return Err(ConfigError::Rejected(runtime_error(format!(
                "cannot read Shoal configuration {}: {error}",
                path.display()
            ))))
        }
    };
    parse_project_config(workspace, &path, text).map_err(ConfigError::Rejected)
}

fn parse_project_config(
    workspace: &Path,
    path: &Path,
    text: String,
) -> Result<(ShoalConfig, String), Box<dyn std::error::Error>> {
    let mut config: ShoalConfig = toml::from_str(&text).map_err(|error| {
        runtime_error(format!(
            "invalid Shoal configuration {}: {error}",
            path.display()
        ))
    })?;
    config.launch.validate()?;
    validate_tracked_exclusions(workspace, &config.launch.source_exclude)?;
    config.resources.validate().map_err(|error| {
        runtime_error(format!(
            "invalid command resources in {}: {error}",
            path.display()
        ))
    })?;
    config.defaults = validate_agent_defaults(
        config.defaults,
        &format!("Shoal configuration {}", path.display()),
    )?;
    Ok((config, text))
}

fn validate_agent_defaults(
    mut defaults: ShoalAgentDefaults,
    source: &str,
) -> Result<ShoalAgentDefaults, Box<dyn std::error::Error>> {
    let model = defaults.model.trim();
    if model.is_empty() {
        return Err(runtime_error(format!("{source} selects an empty model")));
    }
    defaults.model = model.to_owned();
    Ok(defaults)
}

fn resolve_agent_defaults(
    configured: ShoalAgentDefaults,
    model: Option<String>,
    effort: Option<ShoalEffort>,
) -> Result<ShoalAgentDefaults, Box<dyn std::error::Error>> {
    let resolved = ShoalAgentDefaults {
        model: model.unwrap_or(configured.model),
        effort: effort.unwrap_or(configured.effort),
    };
    validate_agent_defaults(resolved, "resolved Shoal agent defaults")
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RunStatus {
    pub version: u32,
    #[serde(default)]
    pub host_generation: u64,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub unavailable_actors: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub recovered_actors: Vec<RecoveredActorObservation>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub lost_state: Vec<String>,
    #[serde(default)]
    pub resource_service: ResourceServiceObservation,
    #[serde(default)]
    pub run_storage: BoundedStorageObservation,
    pub run_id: String,
    pub workspace: PathBuf,
    pub session: String,
    pub agent: ShoalAgentDefaults,
    pub phase: RunPhase,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RecoveredActorObservation {
    pub predecessor: ActorRef,
    pub actor: ActorRef,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResourceServiceObservation {
    /// `None` means no observation was attempted for this status record.
    pub healthy: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resources: Option<tidepool_node::command_resources::CommandResourceObservation>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct BoundedStorageObservation {
    pub bytes: u64,
    pub entries: u64,
    pub unreadable: u64,
    pub truncated: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum RunPhase {
    Starting,
    Recovering {
        stage: RecoveryStage,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        restored_source: Option<String>,
    },
    AwaitingBinding {
        root_actor: ActorRef,
    },
    Ready {
        root_actor: ActorRef,
        root_thread: BackendThreadId,
    },
    Failed {
        error: String,
    },
    Exited,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RecoveryStage {
    ReopeningSource,
    StoppingPredecessors,
    ReconcilingResources,
    RestoringActors,
}

/// Check the authored next-swarm selection without touching native execution.
/// The capture is temporary; compilation uses the normal toolchain cache.
pub async fn check(
    workspace: Option<PathBuf>,
    recipes: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    let workspace = resolve_workspace(workspace)?;
    let scratch = tempfile::tempdir()?;
    let selected = workspace::FrozenWorkspace::load(&workspace, scratch.path())?;
    crate::actor_host::validate_workspace_program(&selected, scratch.path())?;
    println!("Workspace definitions compile: {}", selected.identity());
    println!(
        "Modules: {}",
        selected.import_modules().collect::<Vec<_>>().join(", ")
    );
    if recipes {
        crate::actor_host::recipe_checks::run(&workspace, &selected).await?;
        println!("Recipe checks finished in isolated resident sessions; no native workers or providers launched.");
    } else {
        println!("No actors or providers launched; edits activate at the next swarm boundary.");
    }
    Ok(())
}

fn resolve_workspace(workspace: Option<PathBuf>) -> Result<PathBuf, Box<dyn std::error::Error>> {
    // Auto-detection (no `--workspace`) walks up from the launch cwd looking
    // for a Shoal workspace: `.shoal/`, NOT `tidepool_runtime::paths`'s own
    // `.tidepool/` project marker. Reusing that marker previously meant a
    // workspace nested under an unrelated ancestor that happens to carry a
    // `.tidepool/` (the user-global legacy `~/.tidepool`, in particular)
    // silently resolved to that ancestor instead of the intended cwd, with no
    // `.shoal/` in sight — see `find_root_with_marker`'s doc comment.
    let (workspace, searched_from) = match workspace {
        Some(workspace) => (workspace, None),
        None => {
            let cwd = std::env::current_dir()?;
            let root = tidepool_runtime::paths::find_root_with_marker(&cwd, ".shoal")
                .unwrap_or_else(|| cwd.clone());
            (root, Some(cwd))
        }
    };
    let workspace = std::fs::canonicalize(workspace)?;
    // Auto-detection can still climb to an ancestor `.shoal/` that carries no
    // `config.toml` (a directory that predates `shoal new`, or a `.shoal/`
    // left by something else entirely). Catch that here, with full context,
    // rather than letting a downstream `read_project_config` raise the same
    // error without knowing a walk-up ever happened.
    if let Some(cwd) = searched_from {
        let config = workspace.join(SHOAL_CONFIG);
        if !config.is_file() {
            return Err(Box::new(ConfigError::NoWorkspace {
                workspace,
                config,
                searched_from: Some(cwd),
            }));
        }
    }
    Ok(workspace)
}

pub async fn init(options: InitOptions) -> Result<(), Box<dyn std::error::Error>> {
    let workspace = resolve_workspace(options.workspace)?;
    // A run starts nothing before its workspace is known to exist: no build,
    // no tmux session, no scaffolding.
    let configuration = read_project_config(&workspace)?.0;
    let slice = configuration.launch.systemd_slice;
    let limits = slice.inspect().await?;
    if slice.current_membership().is_err() {
        use std::os::unix::process::CommandExt;
        let executable = std::env::current_exe()?;
        let command = slice.scope(slice.verified_command(
            &executable,
            tidepool_node::ProcessInvocation {
                program: executable.display().to_string(),
                args: std::env::args().skip(1).collect(),
            },
        ));
        return Err(std::process::Command::new(command.program)
            .args(command.args)
            .exec()
            .into());
    }
    tracing::info!(slice = slice.as_str(), ?limits, "selected swarm budget");
    tidepool_worktree::GitCli::new().ensure_shoal_local_exclude(&workspace)?;
    let agent = resolve_agent_defaults(configuration.defaults, options.model, options.effort)?;
    retain_packaged_interactive_agent(&workspace).await?;
    let session_name = options
        .session
        .unwrap_or_else(|| default_session_name(&workspace));
    let tmux = TmuxSession::new(session_name.clone())?;

    if tmux.exists().await? && !options.recreate {
        return Err(runtime_error(format!(
            "tmux session {session_name:?} already exists; attach with `tmux attach -t {session_name}` or replace it with `shoal init --recreate`"
        )));
    }
    let session_root = shoal_state_root(&workspace)
        .join("sessions")
        .join(&session_name);
    std::fs::create_dir_all(&session_root)?;
    let root_binding_path = session_root.join("root-binding.json");
    let interactive_agent = preflight(&workspace).await?;
    let run_id = uuid::Uuid::new_v4().to_string();
    let log_path = shoal_log_path(&workspace, &run_id);
    let compiler_log_path = shoal_compiler_log_path(&workspace, &run_id);
    let run_root = tidepool_runtime::paths::cache_dir()
        .join("shoal")
        .join("runs")
        .join(&run_id);
    ensure_private_run_root(&run_root)?;
    let selected = workspace::FrozenWorkspace::load(&workspace, &run_root)?;
    crate::actor_host::validate_workspace_program(&selected, &run_root)?;
    if options.recreate {
        // Validate continuity before stopping a currently healthy session.
        // The host repeats this check at launch so a later disappearance also
        // fails closed.
        resolve_root_launch_mode(true, &root_binding_path).await?;
        let previous_run = std::fs::read_to_string(session_root.join("run-id")).map_err(|error| {
            runtime_error(format!(
                "cannot safely replace supervised session {session_name:?} without its recorded run identity: {error}"
            ))
        })?;
        stop(previous_run.trim(), &session_name).await?;
    } else {
        clear_fresh_root_binding(&root_binding_path)?;
    }
    tidepool_atomic_write::write_durable(&session_root.join("run-id"), run_id.as_bytes())?;

    let status_path = run_root.join("status.json");
    write_status(
        &status_path,
        &RunStatus::new(
            &run_id,
            &workspace,
            &session_name,
            agent.clone(),
            RunPhase::Starting,
        ),
    )?;

    let executable = retain_run_executable(&run_root, "shoal", &std::env::current_exe()?)?;
    let compiler_socket = run_root.join("compiler.sock");
    // `tidepool_extract_cmd::resolve_bin()` intentionally returns the bare
    // `tidepool-extract` name when unset, deferring the PATH search to the
    // OS at actual spawn time (see its doc comment). This call site needs an
    // absolute path up front, to retain (copy) the binary into the run root
    // below — so it goes through `locate_extract`, the existing mechanism
    // that already does that PATH search and produces a typed, actionable
    // error (also used by `preflight`'s `bind_extract_endpoint` check below)
    // instead of reinventing (and, as `.canonicalize()` did here, getting
    // wrong: canonicalize resolves a bare name against the CWD, never PATH,
    // so it failed even when `tidepool-extract` WAS on PATH).
    let compiler_source = tidepool_runtime::toolchain::locate_extract()
        .map_err(|error| {
            runtime_error(format!(
                "{error} Shoal also needs TIDEPOOL_EXTRACT_WORKER for the Haskell compiler \
                 worker. Launch through `just shoal-console` or `just shoal-init` (or the \
                 packaged `nix build .#shoal` wrapper), which set both."
            ))
        })?
        .path;
    let worker_source = tidepool_extract_cmd::frontend::worker_for_frontend(&compiler_source);
    let compiler_bin = retain_run_executable(&run_root, "tidepool-extract", &compiler_source)?;
    let mut selected_environment = std::collections::BTreeMap::from([(
        "TIDEPOOL_EXTRACT".to_owned(),
        compiler_bin.display().to_string(),
    )]);
    let worker = retain_run_executable(&run_root, "tidepool-extract-worker", &worker_source)?;
    selected_environment.insert(
        "TIDEPOOL_EXTRACT_WORKER".into(),
        worker.display().to_string(),
    );
    println!(
        "launch: agent={} version={} model={} effort={} extractor={}",
        interactive_agent.executable().display(),
        interactive_agent.version(),
        agent.model,
        agent.effort,
        compiler_bin.display()
    );
    let compiler_program = compiler_bin
        .to_str()
        .ok_or_else(|| runtime_error("compiler executable path is not UTF-8"))?
        .to_owned();
    let mut compiler_launch = compiler_daemon_launch(
        &workspace,
        &compiler_socket,
        compiler_program,
        &run_id,
        &compiler_log_path,
    );
    compiler_launch
        .environment
        .extend(selected_environment.clone());
    // Timing lines are debug-level and cheap (log_compile_timing in
    // tidepool-extract-cmd/src/daemon.rs forwards them to the compiler log);
    // keep phase timing on unconditionally for Shoal runs.
    compiler_launch
        .environment
        .insert(tidepool_runtime::timing::TIMING_ENV.into(), "1".into());
    let scoped_compiler = slice.scope(slice.verified_command(
        &executable,
        tidepool_node::ProcessInvocation {
            program: compiler_launch.program,
            args: compiler_launch.args,
        },
    ));
    compiler_launch.program = scoped_compiler.program;
    compiler_launch.args = scoped_compiler.args;
    let daemon_launch = tmux.create(&compiler_launch).await;
    if let Err(error) = daemon_launch {
        write_startup_failure(
            &status_path,
            &run_id,
            &workspace,
            &session_name,
            &agent,
            &error,
        )?;
        return Err(error.into());
    }
    if let Err(error) = wait_until_compiler_daemon(&tmux, &compiler_socket).await {
        write_startup_failure(
            &status_path,
            &run_id,
            &workspace,
            &session_name,
            &agent,
            error.as_ref(),
        )?;
        let _ = tmux.kill().await;
        return Err(error);
    }

    let mut args = vec![
        "host".into(),
        "--workspace".into(),
        workspace.display().to_string(),
        "--session".into(),
        session_name.clone(),
        "--run-id".into(),
        run_id.clone(),
        "--run-root".into(),
        run_root.display().to_string(),
        "--status-path".into(),
        status_path.display().to_string(),
        "--root-binding-path".into(),
        root_binding_path.display().to_string(),
        "--interactive-agent-bin".into(),
        interactive_agent.executable().display().to_string(),
        "--interactive-agent-version".into(),
        interactive_agent.version().to_owned(),
    ];
    // Replacing a session resumes its root conversation. A session whose first
    // launch never got as far as a conversation has no binding at all: there is
    // nothing to resume and nothing to lose, so it starts fresh. A binding that
    // exists and cannot be read is a different case, and the host fails closed
    // on it rather than quietly abandoning a conversation.
    if options.recreate {
        if root_binding_path.exists() {
            args.push("--resume-root".into());
        } else {
            println!(
                "no earlier root conversation in session {session_name:?}; starting a new one"
            );
        }
    }
    args.extend(["--model".into(), agent.model.clone()]);
    args.extend(["--effort".into(), agent.effort.to_string()]);
    let host_launch = slice.supervised_service(
        &format!("shoal-host-{run_id}"),
        slice.verified_command(
            &executable,
            tidepool_node::ProcessInvocation {
                program: executable.display().to_string(),
                args,
            },
        ),
    );
    let launch = tmux
        .spawn_window(&TmuxLaunch {
            window_name: "Host".into(),
            cwd: workspace.clone(),
            program: host_launch.program,
            args: host_launch.args,
            environment: {
                let mut environment = host_environment(&compiler_socket);
                environment.extend(selected_environment);
                environment
            },
            unset_environment: std::collections::BTreeSet::new(),
        })
        .await;
    if let Err(error) = launch {
        write_startup_failure(
            &status_path,
            &run_id,
            &workspace,
            &session_name,
            &agent,
            &error,
        )?;
        let _ = tmux.kill().await;
        return Err(error.into());
    }
    println!("log:    {}", log_path.display());
    println!(
        "compiler: {} (tmux window Compiler)",
        compiler_socket.display()
    );
    println!(
        "interactive agent: {} (version {}, sha256 {}, package {})",
        interactive_agent.executable().display(),
        interactive_agent.version(),
        interactive_agent.executable_sha256(),
        interactive_agent
            .package_root()
            .map_or_else(|| "unpackaged".into(), |path| path.display().to_string())
    );

    let interactive = match wait_until_interactive(&tmux, &status_path, &run_id).await {
        Ok(interactive) => interactive,
        Err(failure) => {
            return Err(runtime_error(format!(
                "{failure}; session {session_name:?} retained for inspection; native execution may still be running. Status: {}",
                status_path.display()
            )));
        }
    };
    match &interactive.phase {
        RunPhase::AwaitingBinding { root_actor } => println!(
            "Shoal launched in tmux session {session_name:?}: actor {root_actor:?} is waiting for its queue-ready session handshake"
        ),
        RunPhase::Ready {
            root_actor,
            root_thread,
        } => println!(
            "Shoal ready in tmux session {session_name:?}: actor {root_actor:?}, thread {}",
            root_thread.0
        ),
        RunPhase::Starting | RunPhase::Recovering { .. } | RunPhase::Failed { .. } | RunPhase::Exited => {
            unreachable!("wait_until_interactive returns only interactive phases")
        }
    }
    println!("status: {}", status_path.display());
    println!(
        "operator socket: {}",
        run_root.join("operator/operator.sock").display()
    );
    println!("agent:  model={} effort={}", agent.model, agent.effort);
    println!("config: {}", workspace.join(SHOAL_CONFIG).display());
    if options.no_attach {
        println!("attach: tmux attach -t {session_name}");
        println!("stop:   shoal stop --run-id {run_id} --session {session_name}");
        Ok(())
    } else {
        tmux.attach_or_switch()
            .await
            .map_err(|failure| Box::new(failure) as Box<dyn std::error::Error>)
    }
}

pub async fn stop(run_id: &str, session: &str) -> Result<(), Box<dyn std::error::Error>> {
    if run_id.is_empty()
        || !run_id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
    {
        return Err(runtime_error("invalid Shoal run identity"));
    }
    let unit = format!("shoal-host-{run_id}.service");
    let status = tokio::process::Command::new("systemctl")
        .args(["--user", "stop", &unit])
        .status()
        .await?;
    if !status.success() {
        return Err(runtime_error(format!(
            "could not stop supervised host unit {unit}"
        )));
    }
    let tmux = TmuxSession::new(session)?;
    if tmux.exists().await? {
        tmux.kill().await?;
    }
    Ok(())
}

fn compiler_daemon_launch(
    workspace: &Path,
    socket: &Path,
    program: String,
    run_id: &str,
    log_path: &Path,
) -> TmuxLaunch {
    TmuxLaunch {
        window_name: "Compiler".into(),
        cwd: workspace.into(),
        program,
        args: vec![
            "--daemon".into(),
            "--socket".into(),
            socket.display().to_string(),
            "--persistent".into(),
            "--run-id".into(),
            run_id.into(),
            "--log-path".into(),
            log_path.display().to_string(),
        ],
        environment: pane_environment(),
        unset_environment: std::collections::BTreeSet::new(),
    }
}

fn host_environment(compiler_socket: &Path) -> std::collections::BTreeMap<String, String> {
    let mut environment = pane_environment();
    environment.insert(
        tidepool_extract_cmd::DAEMON_SOCKET_ENV.into(),
        compiler_socket.display().to_string(),
    );
    environment
}

fn write_startup_failure(
    status_path: &Path,
    run_id: &str,
    workspace: &Path,
    session_name: &str,
    agent: &ShoalAgentDefaults,
    error: &dyn std::fmt::Display,
) -> Result<(), Box<dyn std::error::Error>> {
    write_status(
        status_path,
        &RunStatus::new(
            run_id,
            workspace,
            session_name,
            agent.clone(),
            RunPhase::Failed {
                error: error.to_string(),
            },
        ),
    )
}

async fn wait_until_compiler_daemon(
    tmux: &TmuxSession,
    socket: &Path,
) -> Result<(), Box<dyn std::error::Error>> {
    let socket = socket.to_owned();
    tokio::time::timeout(COMPILER_DAEMON_START_TIMEOUT, async {
        loop {
            let candidate = socket.clone();
            let ready = tokio::task::spawn_blocking(move || {
                tidepool_extract_cmd::preflight_compiler_daemon(&candidate)
            })
            .await
            .map_err(|error| runtime_error(format!("compiler preflight task failed: {error}")))?;
            if ready.is_ok() {
                return Ok(());
            }
            if !tmux.exists().await? {
                return Err(runtime_error(
                    "compiler daemon exited before becoming ready",
                ));
            }
            tokio::time::sleep(COMPILER_DAEMON_POLL_INTERVAL).await;
        }
    })
    .await
    .map_err(|_| {
        runtime_error(format!(
            "compiler daemon did not become ready within {COMPILER_DAEMON_START_TIMEOUT:?}"
        ))
    })?
}

/// Retain the Nix-packaged private interactive-agent closure without placing
/// its executable on the user's ordinary PATH. The project-local symlink is a
/// durable GC root under Shoal's runtime-owned state directory.
async fn retain_packaged_interactive_agent(
    workspace: &Path,
) -> Result<(), Box<dyn std::error::Error>> {
    retain_packaged_interactive_agent_from(
        workspace,
        std::env::var_os(ENV_PACKAGED_CODEX_CLOSURE).map(PathBuf::from),
        std::env::var_os(ENV_NIX_STORE_BIN).map(PathBuf::from),
    )
    .await
}

async fn retain_packaged_interactive_agent_from(
    workspace: &Path,
    target: Option<PathBuf>,
    nix_store: Option<PathBuf>,
) -> Result<(), Box<dyn std::error::Error>> {
    let (target, nix_store) = match (target, nix_store) {
        (None, None) => return Ok(()),
        (Some(target), Some(nix_store)) => (target, nix_store),
        _ => {
            return Err(runtime_error(format!(
                "{ENV_PACKAGED_CODEX_CLOSURE} and {ENV_NIX_STORE_BIN} must be supplied together"
            )))
        }
    };
    if !target.is_absolute() || !target.is_dir() {
        return Err(runtime_error(format!(
            "{ENV_PACKAGED_CODEX_CLOSURE} is not an absolute package directory: {}",
            target.display()
        )));
    }
    if !nix_store.is_absolute() || !nix_store.is_file() {
        return Err(runtime_error(format!(
            "{ENV_NIX_STORE_BIN} is not an absolute executable file: {}",
            nix_store.display()
        )));
    }
    let runtime = workspace.join(".shoal/runtime");
    std::fs::create_dir_all(&runtime)?;
    let link = runtime.join("interactive-agent");
    let mut child = tokio::process::Command::new(&nix_store)
        .arg("--realise")
        .arg(&target)
        .arg("--add-root")
        .arg(&link)
        .kill_on_drop(true)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|error| {
            runtime_error(format!(
                "cannot start {ENV_NIX_STORE_BIN} {}: {error}",
                nix_store.display()
            ))
        })?;
    let stderr = child.stderr.take().ok_or_else(|| {
        runtime_error(format!(
            "{ENV_NIX_STORE_BIN} did not expose its diagnostic stream"
        ))
    })?;
    let result = tokio::time::timeout(GC_ROOT_TIMEOUT, async {
        tokio::try_join!(
            read_bounded_diagnostics(stderr, GC_ROOT_ERROR_LIMIT, "Nix GC-root creation"),
            child.wait()
        )
    })
    .await
    .map_err(|_| runtime_error(format!("Nix GC-root creation exceeded {GC_ROOT_TIMEOUT:?}")))?;
    let (stderr, status) = result.map_err(|error| {
        runtime_error(format!(
            "cannot register Nix GC root {}: {error}",
            link.display()
        ))
    })?;
    if !status.success() {
        return Err(runtime_error(format!(
            "cannot register Nix GC root {} ({status}): {}",
            link.display(),
            String::from_utf8_lossy(&stderr).trim()
        )));
    }
    Ok(())
}

async fn read_bounded_diagnostics(
    reader: impl tokio::io::AsyncRead + Unpin,
    limit: usize,
    operation: &'static str,
) -> std::io::Result<Vec<u8>> {
    let mut bytes = Vec::new();
    reader
        .take((limit + 1) as u64)
        .read_to_end(&mut bytes)
        .await?;
    if bytes.len() > limit {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!("{operation} diagnostics exceeded {limit} bytes"),
        ));
    }
    Ok(bytes)
}

pub async fn host(options: HostOptions) -> Result<(), Box<dyn std::error::Error>> {
    ensure_private_run_root(&options.run_root)?;
    let host_incarnation = crate::actor_host::HostIncarnationLease::claim(&options.run_root)?;
    let generation = host_incarnation.incarnation().0;
    if generation > 1 {
        let mut status = RunStatus::new(
            &options.run_id,
            &options.workspace,
            &options.session,
            options.agent.clone(),
            RunPhase::Recovering {
                stage: RecoveryStage::ReopeningSource,
                restored_source: None,
            },
        )
        .at_generation(generation);
        status.lost_state = recovery_lost_state(generation);
        write_status(&options.status_path, &status)?;
    }
    let log_path = shoal_log_path(&options.workspace, &options.run_id);
    tracing::info!(
        run_id = %options.run_id,
        session = %options.session,
        workspace = %options.workspace.display(),
        interactive_agent = %options.interactive_agent.executable().display(),
        interactive_agent_version = options.interactive_agent.version(),
        interactive_agent_sha256 = options.interactive_agent.executable_sha256(),
        interactive_agent_package = %options.interactive_agent.package_root().map_or_else(|| "unpackaged".into(), |path| path.display().to_string()),
        model = %options.agent.model,
        effort = %options.agent.effort,
        detailed_log = %log_path.display(),
        host_generation = generation,
        "starting Shoal actor host"
    );
    let result = run_host(&options, generation, host_incarnation).await;
    let settled = settle_host_result(result, &options, generation);
    if let Err(error) = &settled {
        tracing::error!(run_id = %options.run_id, error = %error, "Shoal actor host failed");
    } else {
        tracing::info!(run_id = %options.run_id, "Shoal actor host stopped");
    }
    settled
}

async fn run_host(
    options: &HostOptions,
    host_generation: u64,
    host_incarnation: crate::actor_host::HostIncarnationLease,
) -> Result<(), Box<dyn std::error::Error>> {
    ensure_private_run_root(&options.run_root)?;
    if let Some(parent) = options.root_binding_path.parent() {
        std::fs::create_dir_all(parent)?;
    }

    let recovered_binding = options.run_root.join("root-binding.json");
    let root_launch_mode = if host_generation > 1 {
        resolve_root_launch_mode(true, &recovered_binding)
            .await
            .map_err(|error| {
                runtime_error(format!(
                    "host generation {host_generation} cannot prove a resumable root; actor remains unavailable: {error}"
                ))
            })?
    } else {
        resolve_root_launch_mode(options.resume_root, &options.root_binding_path).await?
    };

    let workspace_inputs = workspace::FrozenWorkspace::load(&options.workspace, &options.run_root)?;
    let accepted_source = source::SourceLayer::new(&options.run_root)
        .ensure_active(&workspace_inputs)?
        .identity;
    let mut unavailable_actors = Vec::new();
    let lost_state = recovery_lost_state(host_generation);
    if host_generation > 1 {
        let mut status = RunStatus::new(
            &options.run_id,
            &options.workspace,
            &options.session,
            options.agent.clone(),
            RunPhase::Recovering {
                stage: RecoveryStage::StoppingPredecessors,
                restored_source: Some(accepted_source.clone()),
            },
        )
        .at_generation(host_generation);
        status.lost_state = lost_state.clone();
        write_status(&options.status_path, &status)?;
        let predecessors = crate::actor_host::stop_predecessor_processes(&options.run_root).map_err(
            |error| {
                runtime_error(format!(
                    "host recovery cannot prove predecessor native applications stopped; actors remain unavailable: {error}"
                ))
            },
        )?;
        if !predecessors.root_available() {
            return Err(runtime_error(format!(
                "host recovery cannot prove the predecessor root stopped; actor remains unavailable: {}",
                predecessors.unavailable.join(", ")
            )));
        }
        let unavailable = if predecessors.unavailable.is_empty() {
            "none".into()
        } else {
            predecessors.unavailable.join(", ")
        };
        unavailable_actors = predecessors.unavailable.clone();
        let notice = format!(
            "Recovery notice [{}:{}]. Restored accepted source {}. Live Haskell computations, requests, watches, and bindings from the prior host were lost. Native work was interrupted. The recorded conversation is being resumed without replaying unresolved tool calls. Unavailable predecessor actors: {unavailable}. Inspect Shoal status and retained command jobs before starting new work.",
            options.run_id,
            host_generation,
            accepted_source,
        );
        tidepool_atomic_write::write_durable(
            &options.run_root.join("host-recovery-notice.txt"),
            notice.as_bytes(),
        )?;
        tracing::info!(
            host_generation,
            stopped = predecessors.stopped,
            unavailable = ?predecessors.unavailable,
            "predecessor native applications reconciled"
        );
    }
    let configuration = workspace_inputs.config()?;
    let slice = configuration.launch.systemd_slice;
    slice.current_membership()?;
    let limits = slice.inspect().await?;
    tidepool_atomic_write::write_best_effort(
        &options.run_root.join("resource-budget.json"),
        &serde_json::to_vec_pretty(&limits)?,
    )?;
    let haskell_root = crate::haskell_sources::ensure_shoal_haskell()?;
    let research_policy = configuration.research;
    if host_generation > 1 {
        let mut status = RunStatus::new(
            &options.run_id,
            &options.workspace,
            &options.session,
            options.agent.clone(),
            RunPhase::Recovering {
                stage: RecoveryStage::ReconcilingResources,
                restored_source: Some(accepted_source.clone()),
            },
        )
        .at_generation(host_generation)
        .with_unavailable_actors(unavailable_actors.clone());
        status.lost_state = lost_state.clone();
        write_status(&options.status_path, &status)?;
    }
    let command_resources =
        resources::connect(configuration.resources, &options.run_id, &slice).await?;
    let mut recovered_actors = Vec::new();
    if host_generation > 1 {
        let status = observe_run_runtime(
            RunStatus::new(
                &options.run_id,
                &options.workspace,
                &options.session,
                options.agent.clone(),
                RunPhase::Recovering {
                    stage: RecoveryStage::RestoringActors,
                    restored_source: Some(accepted_source.clone()),
                },
            )
            .at_generation(host_generation)
            .with_unavailable_actors(unavailable_actors.clone()),
            &command_resources,
            &recovered_actors,
            &lost_state,
        )
        .await;
        write_status(&options.status_path, &status)?;
    }
    let (readiness_tx, mut readiness_rx) = mpsc::unbounded_channel();
    let run = crate::actor_host::run(
        crate::actor_host::ActorHostConfig {
            systemd_slice: Some(slice),
            source_exclude: configuration.launch.source_exclude,
            command_resources: Some(std::sync::Arc::clone(&command_resources)),
            shoal_executable: std::env::current_exe()?,
            workspace: options.workspace.clone(),
            haskell_root,
            run_root: options.run_root.clone(),
            root_binding_path: options.run_root.join("root-binding.json"),
            interactive_agent: options.interactive_agent.clone(),
            tmux_session: options.session.clone(),
            model: options.agent.model.clone(),
            effort: options.agent.effort.into(),
            research_policy,
            workspace_inputs: Some(workspace_inputs),
            root_launch_mode,
            pane_environment: pane_environment(),
            jev: None,
        },
        readiness_tx,
        host_incarnation,
    )
    .instrument(tracing::info_span!(
        "shoal_host",
        run_id = %options.run_id
    ));

    tokio::pin!(run);
    loop {
        tokio::select! {
            readiness = readiness_rx.recv() => match readiness {
                Some(crate::actor_host::ActorHostReadiness::CoordinationFailed { root, error }) => {
                    tracing::error!(actor = ?root, %error, "root coordination failed; preserving native application");
                    if let Err(error) = write_startup_failure(
                        &options.status_path,
                        &options.run_id,
                        &options.workspace,
                        &options.session,
                        &options.agent,
                        &error,
                    ) {
                        tracing::error!(%error, "could not publish root failure; host remains active");
                    }
                }
                Some(crate::actor_host::ActorHostReadiness::AwaitingBinding { root }) => {
                    let status = observe_run_runtime(RunStatus::new(
                        &options.run_id,
                        &options.workspace,
                        &options.session,
                        options.agent.clone(),
                        RunPhase::AwaitingBinding { root_actor: root },
                    ).at_generation(host_generation).with_unavailable_actors(unavailable_actors.clone()),
                        &command_resources,
                        &recovered_actors,
                        &lost_state,
                    ).await;
                    if let Err(error) = write_status(&options.status_path, &status) {
                        tracing::error!(%error, "could not publish pending root status; host remains active");
                    }
                    tracing::info!(
                        run_id = %options.run_id,
                        actor = ?root,
                        "Shoal root application launched; queue-ready session handshake pending"
                    );
                }
                Some(crate::actor_host::ActorHostReadiness::Ready { root, thread }) => {
                    let thread_id = thread.id().0.clone();
                    if let Err(error) = copy_interactive_binding(&options.root_binding_path, &thread).await {
                        tracing::error!(%error, "could not publish root binding; host remains active");
                    }
                    let status = observe_run_runtime(RunStatus::new(
                        &options.run_id,
                        &options.workspace,
                        &options.session,
                        options.agent.clone(),
                        RunPhase::Ready {
                            root_actor: root,
                            root_thread: thread.id().clone(),
                        },
                    ).at_generation(host_generation).with_unavailable_actors(unavailable_actors.clone()),
                        &command_resources,
                        &recovered_actors,
                        &lost_state,
                    ).await;
                    if let Err(error) = write_status(&options.status_path, &status) {
                        tracing::error!(%error, "could not publish ready root status; host remains active");
                    }
                    tracing::info!(
                        run_id = %options.run_id,
                        actor = ?root,
                        thread = %thread_id,
                        "root interactive application ready"
                    );
                }
                Some(crate::actor_host::ActorHostReadiness::ActorRecovered { predecessor, actor }) => {
                    let observation = RecoveredActorObservation { predecessor, actor };
                    if !recovered_actors.contains(&observation) {
                        recovered_actors.push(observation);
                        recovered_actors.sort_by_key(|recovered| {
                            (recovered.actor.id, recovered.actor.incarnation)
                        });
                    }
                    unavailable_actors.retain(|unavailable| {
                        unavailable
                            .split_once('-')
                            .and_then(|(id, _)| id.parse::<u64>().ok())
                            != Some(predecessor.id.0)
                    });
                    match std::fs::read(&options.status_path)
                        .map_err(Box::<dyn std::error::Error>::from)
                        .and_then(|bytes| decode_run_status(&bytes))
                    {
                        Ok(mut status) if status.host_generation == host_generation => {
                            status.unavailable_actors = unavailable_actors.clone();
                            let status = observe_run_runtime(
                                status,
                                &command_resources,
                                &recovered_actors,
                                &lost_state,
                            ).await;
                            if let Err(error) = write_status(&options.status_path, &status) {
                                tracing::error!(%error, "could not publish recovered actor status");
                            }
                        }
                        Ok(_) => {}
                        Err(error) => {
                            tracing::warn!(%error, "recovered actor became ready before run status could be updated");
                        }
                    }
                    tracing::info!(
                        predecessor = %predecessor,
                        actor = %actor,
                        "actor conversation recovered in a fresh incarnation"
                    );
                }
                Some(crate::actor_host::ActorHostReadiness::ActorUnavailable { predecessor, reason }) => {
                    let label = format!("{}-{}", predecessor.id.0, predecessor.incarnation.0);
                    if !unavailable_actors.contains(&label) {
                        unavailable_actors.push(label);
                        unavailable_actors.sort();
                    }
                    match std::fs::read(&options.status_path)
                        .map_err(Box::<dyn std::error::Error>::from)
                        .and_then(|bytes| decode_run_status(&bytes))
                    {
                        Ok(mut status) if status.host_generation == host_generation => {
                            status.unavailable_actors = unavailable_actors.clone();
                            let status = observe_run_runtime(
                                status,
                                &command_resources,
                                &recovered_actors,
                                &lost_state,
                            ).await;
                            if let Err(error) = write_status(&options.status_path, &status) {
                                tracing::error!(%error, "could not publish unavailable actor status");
                            }
                        }
                        Ok(_) => {}
                        Err(error) => {
                            tracing::debug!(%error, "unavailable actor preceded initial run status");
                        }
                    }
                    tracing::warn!(actor = %predecessor, %reason, "durable actor remains unavailable");
                }
                None => return run.await,
            },
            result = &mut run => return result,
        }
    }
}

fn recovery_lost_state(host_generation: u64) -> Vec<String> {
    if host_generation <= 1 {
        return Vec::new();
    }
    vec![
        "live Haskell computations".into(),
        "resident requests and watches".into(),
        "live Haskell bindings".into(),
        "interrupted native work".into(),
    ]
}

fn ensure_private_run_root(path: &Path) -> std::io::Result<()> {
    use std::os::unix::fs::PermissionsExt as _;

    std::fs::create_dir_all(path)?;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700))
}

async fn resolve_root_launch_mode(
    resume: bool,
    binding_path: &Path,
) -> Result<InteractiveLaunchMode, Box<dyn std::error::Error>> {
    if !resume {
        return Ok(InteractiveLaunchMode::Fresh);
    }
    read_interactive_binding(binding_path)
        .await
        .map(|thread| InteractiveLaunchMode::Resume(thread.id().clone()))
        .map_err(|error| {
            runtime_error(format!(
                "cannot resume the requested root conversation from {}: {error}",
                binding_path.display()
            ))
        })
}

fn clear_fresh_root_binding(path: &Path) -> Result<(), Box<dyn std::error::Error>> {
    match std::fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(runtime_error(format!(
            "cannot clear stale root conversation binding {}: {error}",
            path.display()
        ))),
    }
}

fn settle_host_result(
    result: Result<(), Box<dyn std::error::Error>>,
    options: &HostOptions,
    host_generation: u64,
) -> Result<(), Box<dyn std::error::Error>> {
    let phase = match &result {
        Ok(()) => RunPhase::Exited,
        Err(failure) => RunPhase::Failed {
            error: failure.to_string(),
        },
    };
    let mut status = RunStatus::new(
        &options.run_id,
        &options.workspace,
        &options.session,
        options.agent.clone(),
        phase,
    );
    status.host_generation = host_generation;
    if let Ok(previous) = std::fs::read(&options.status_path)
        .map_err(Box::<dyn std::error::Error>::from)
        .and_then(|bytes| decode_run_status(&bytes))
    {
        if previous.host_generation == host_generation {
            status.unavailable_actors = previous.unavailable_actors;
            status.recovered_actors = previous.recovered_actors;
            status.lost_state = previous.lost_state;
            status.resource_service = previous.resource_service;
        }
    }
    match (result, write_status(&options.status_path, &status)) {
        (result, Ok(())) => result,
        (Ok(()), Err(status_error)) => Err(status_error),
        (Err(run_error), Err(status_error)) => {
            tracing::error!(error = %status_error, "could not publish terminal Shoal status");
            Err(run_error)
        }
    }
}

async fn preflight(
    workspace: &Path,
) -> Result<InteractiveAgentInstallation, Box<dyn std::error::Error>> {
    tidepool_runtime::toolchain::bind_extract_endpoint().map_err(|error| {
        runtime_error(format!(
            "{error}. Launch through `just shoal-console` or `just shoal-init` for the matched local toolchain."
        ))
    })?;

    // Codex keys its interactive trust decision by the path visible inside
    // its process. Every actor gets an isolated repository mounted at this one
    // stable slot, so this creates one durable entry rather than one per run.
    std::fs::create_dir_all(ACTOR_PROJECT_ROOT)?;
    tidepool_agent::trust_interactive_project(Path::new(ACTOR_PROJECT_ROOT))?;

    let interactive_agent = tidepool_agent::resolve_native_interactive_agent().await?;

    let mut boundary = tokio::process::Command::new(tidepool_node::BUBBLEWRAP_PROGRAM);
    boundary
        .args(["--bind", "/", "/", "--ro-bind"])
        .arg(workspace)
        .arg(workspace)
        .args(["--chdir"])
        .arg(workspace)
        // Resolve through the dev/runtime PATH. NixOS deliberately does not
        // provide the FHS `/bin/true` path.
        .args(["--", "true"])
        .kill_on_drop(true)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped());
    let mut boundary = boundary.spawn().map_err(|source| {
        runtime_error(format!(
            "Bubblewrap is required for Shoal actor worktrees: {source}. Launch through `just shoal-console` or `just shoal-init` to enter the supported environment."
        ))
    })?;
    let stderr = boundary.stderr.take().ok_or_else(|| {
        runtime_error("Bubblewrap capability probe did not expose its diagnostic stream")
    })?;
    let result = tokio::time::timeout(BOUNDARY_PROBE_TIMEOUT, async {
        tokio::try_join!(
            read_bounded_diagnostics(
                stderr,
                BOUNDARY_PROBE_ERROR_LIMIT,
                "Bubblewrap capability probe"
            ),
            boundary.wait()
        )
    })
    .await
    .map_err(|_| {
        runtime_error(format!(
            "Bubblewrap capability probe exceeded {BOUNDARY_PROBE_TIMEOUT:?}"
        ))
    })?;
    let (stderr, status) = result.map_err(|source| {
        runtime_error(format!(
            "Bubblewrap capability probe could not complete: {source}"
        ))
    })?;
    if !status.success() {
        return Err(runtime_error(format!(
            "Bubblewrap cannot establish the Shoal process boundary: {}",
            String::from_utf8_lossy(&stderr).trim()
        )));
    }
    Ok(interactive_agent)
}

async fn wait_until_interactive(
    tmux: &TmuxSession,
    status_path: &Path,
    run_id: &str,
) -> Result<RunStatus, Box<dyn std::error::Error>> {
    tokio::time::timeout(INTERACTIVE_START_TIMEOUT, async {
        loop {
            if let Ok(bytes) = tokio::fs::read(status_path).await {
                let status = decode_run_status(&bytes)?;
                if status.run_id == run_id {
                    match &status.phase {
                        RunPhase::AwaitingBinding { .. } | RunPhase::Ready { .. } => {
                            return Ok(status)
                        }
                        RunPhase::Failed { error } => {
                            return Err(runtime_error(format!("Shoal host failed: {error}")))
                        }
                        RunPhase::Exited => {
                            return Err(runtime_error(
                                "Shoal host exited before becoming interactive",
                            ))
                        }
                        RunPhase::Starting | RunPhase::Recovering { .. } => {}
                    }
                }
            }
            if !tmux.exists().await? {
                return Err(runtime_error(
                    "Shoal tmux session exited before becoming interactive",
                ));
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    })
    .await
    .map_err(|_| {
        runtime_error(format!(
            "Shoal did not become interactive within {INTERACTIVE_START_TIMEOUT:?}"
        ))
    })?
}

pub(crate) fn decode_run_status(bytes: &[u8]) -> Result<RunStatus, Box<dyn std::error::Error>> {
    let mut value: serde_json::Value = serde_json::from_slice(bytes)?;
    let version = value
        .get("version")
        .and_then(serde_json::Value::as_u64)
        .and_then(|version| u32::try_from(version).ok())
        .ok_or_else(|| runtime_error("Shoal run status has no valid version"))?;
    if version == PREVIOUS_STATUS_VERSION {
        let object = value
            .as_object_mut()
            .ok_or_else(|| runtime_error("Shoal run status is not an object"))?;
        object.insert("version".into(), STATUS_VERSION.into());
    } else if version != STATUS_VERSION {
        return Err(runtime_error(format!(
            "unsupported Shoal run status version {version} (expected {STATUS_VERSION})"
        )));
    }
    Ok(serde_json::from_value(value)?)
}

impl RunStatus {
    fn new(
        run_id: &str,
        workspace: &Path,
        session: &str,
        agent: ShoalAgentDefaults,
        phase: RunPhase,
    ) -> Self {
        Self {
            version: STATUS_VERSION,
            host_generation: 0,
            unavailable_actors: Vec::new(),
            recovered_actors: Vec::new(),
            lost_state: Vec::new(),
            resource_service: ResourceServiceObservation::default(),
            run_storage: BoundedStorageObservation::default(),
            run_id: run_id.into(),
            workspace: workspace.into(),
            session: session.into(),
            agent,
            phase,
        }
    }

    fn at_generation(mut self, generation: u64) -> Self {
        self.host_generation = generation;
        self
    }

    fn with_unavailable_actors(mut self, actors: Vec<String>) -> Self {
        self.unavailable_actors = actors;
        self
    }
}

async fn observe_run_runtime(
    mut status: RunStatus,
    resources: &std::sync::Arc<tidepool_node::command_resources::CommandResourceClient>,
    recovered_actors: &[RecoveredActorObservation],
    lost_state: &[String],
) -> RunStatus {
    status.recovered_actors = recovered_actors.to_vec();
    status.lost_state = lost_state.to_vec();
    status.resource_service =
        match tokio::time::timeout(Duration::from_secs(2), resources.observation()).await {
            Ok(Ok(observation)) => ResourceServiceObservation {
                healthy: Some(true),
                resources: Some(observation),
                detail: None,
            },
            Ok(Err(error)) => ResourceServiceObservation {
                healthy: Some(false),
                resources: None,
                detail: Some(format!(
                    "resource service unavailable; accepted commands remain retained: {error}"
                )),
            },
            Err(_) => ResourceServiceObservation {
                healthy: Some(false),
                resources: None,
                detail: Some(
                    "resource service observation timed out; accepted commands remain retained"
                        .into(),
                ),
            },
        };
    status
}

fn write_status(path: &Path, status: &RunStatus) -> Result<(), Box<dyn std::error::Error>> {
    let mut status = status.clone();
    if let Some(run_root) = path.parent() {
        status.run_storage = observe_storage(run_root, 8_192);
    }
    let bytes = serde_json::to_vec_pretty(&status)?;
    tidepool_atomic_write::write_best_effort(path, &bytes)?;
    Ok(())
}

fn observe_storage(root: &Path, maximum_entries: usize) -> BoundedStorageObservation {
    let mut observation = BoundedStorageObservation::default();
    let mut pending = vec![root.to_path_buf()];
    while let Some(path) = pending.pop() {
        if observation.entries as usize >= maximum_entries {
            observation.truncated = true;
            break;
        }
        observation.entries += 1;
        match std::fs::symlink_metadata(&path) {
            Ok(metadata) if metadata.is_file() => {
                observation.bytes = observation.bytes.saturating_add(metadata.len());
            }
            Ok(metadata) if metadata.is_dir() => match std::fs::read_dir(&path) {
                Ok(entries) => {
                    for entry in entries {
                        match entry {
                            Ok(entry)
                                if observation.entries as usize + pending.len()
                                    < maximum_entries =>
                            {
                                pending.push(entry.path());
                            }
                            Ok(_) => {
                                observation.truncated = true;
                                break;
                            }
                            Err(_) => observation.unreadable += 1,
                        }
                    }
                }
                Err(_) => observation.unreadable += 1,
            },
            Ok(_) => {}
            Err(_) => observation.unreadable += 1,
        }
    }
    observation
}

fn shoal_state_root(workspace: &Path) -> PathBuf {
    workspace.join(".shoal")
}

pub fn shoal_log_path(workspace: &Path, run_id: &str) -> PathBuf {
    shoal_state_root(workspace)
        .join("logs")
        .join(format!("{run_id}.log"))
}

pub fn shoal_compiler_log_path(workspace: &Path, run_id: &str) -> PathBuf {
    shoal_state_root(workspace)
        .join("logs")
        .join(format!("{run_id}-compiler.log"))
}

/// The run-local structured trace: one JSON object per line, holding the span
/// tree (run, actor, tool call, cell, input unit) and the `shoal::content`
/// target that carries cell source, receipts, lookup traffic and diagnostics.
pub fn shoal_trace_path(workspace: &Path, run_id: &str) -> PathBuf {
    shoal_state_root(workspace)
        .join("logs")
        .join(format!("{run_id}.jsonl"))
}

/// Target for the text a cell actually carried. It is written to the run-local
/// JSONL file and to nothing else: the human log and the tmux pane switch it
/// off explicitly, and no request-update payload is ever routed here.
pub const CONTENT_TARGET: &str = "shoal::content";

fn host_pane_filter() -> tracing_subscriber::EnvFilter {
    tracing_subscriber::EnvFilter::new(
        "warn,tidepool::shoal=info,tidepool::actor_host=info,shoal::content=off",
    )
}

/// Directives for the JSON trace. Spans and their fields at `info`, the
/// content target in full. `SHOAL_TRACE` replaces the whole set when a run
/// wants more (or less).
fn host_trace_filter() -> tracing_subscriber::EnvFilter {
    let configured = std::env::var("SHOAL_TRACE").ok();
    let directives = configured
        .as_deref()
        .filter(|value| !value.is_empty())
        // Cranelift logs the full text of every function it defines at
        // `info`, hundreds of kilobytes each; one cell wrote 390 MB of it.
        .unwrap_or("info,cranelift_jit=warn,cranelift_codegen=warn,shoal::content=trace");
    tracing_subscriber::EnvFilter::new(directives)
}

fn host_tracing_subscriber<D, P, J>(
    detailed_writer: D,
    pane_writer: P,
    trace_writer: J,
    detailed_filter: tracing_subscriber::EnvFilter,
) -> impl tracing::Subscriber + Send + Sync
where
    D: for<'writer> tracing_subscriber::fmt::MakeWriter<'writer> + Send + Sync + 'static,
    P: for<'writer> tracing_subscriber::fmt::MakeWriter<'writer> + Send + Sync + 'static,
    J: for<'writer> tracing_subscriber::fmt::MakeWriter<'writer> + Send + Sync + 'static,
{
    let detailed = tracing_subscriber::fmt::layer()
        .with_ansi(false)
        .with_writer(detailed_writer)
        .with_filter(detailed_filter);
    let pane = tracing_subscriber::fmt::layer()
        .compact()
        .without_time()
        .with_ansi(false)
        .with_target(false)
        .with_writer(pane_writer)
        .with_filter(host_pane_filter());
    // Span close carries the duration and every field recorded during the
    // span, which is what makes one cell reconstructable from this file
    // alone.
    let trace = tracing_subscriber::fmt::layer()
        .json()
        .with_current_span(true)
        .with_span_list(true)
        .with_span_events(tracing_subscriber::fmt::format::FmtSpan::CLOSE)
        .with_ansi(false)
        .with_writer(trace_writer)
        .with_filter(host_trace_filter());
    tracing_subscriber::registry()
        .with(detailed)
        .with(pane)
        .with(trace)
}

/// The returned guard owns the trace appender's flush thread. Bind it for the
/// host process's whole life: dropping it closes the channel and every later
/// span is lost.
pub fn init_host_tracing(
    workspace: &Path,
    run_id: &str,
) -> Result<(PathBuf, tracing_appender::non_blocking::WorkerGuard), Box<dyn std::error::Error>> {
    let path = shoal_log_path(workspace, run_id);
    let parent = path
        .parent()
        .ok_or_else(|| runtime_error("Shoal log path has no parent directory"))?;
    std::fs::create_dir_all(parent)?;
    let file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)?;
    let trace_file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(shoal_trace_path(workspace, run_id))?;
    let (trace_writer, guard) = tracing_appender::non_blocking(trace_file);
    // Added after the environment's own directives so that content stays out
    // of the human log even when `RUST_LOG` asks for it.
    let detailed_filter = tidepool_codegen::debug::tracing_env_filter("info").add_directive(
        format!("{CONTENT_TARGET}=off")
            .parse()
            .map_err(|error| runtime_error(format!("invalid content directive: {error}")))?,
    );
    host_tracing_subscriber(
        Mutex::new(file),
        std::io::stderr,
        trace_writer,
        detailed_filter,
    )
    .try_init()
    .map_err(|error| runtime_error(format!("could not initialize Shoal tracing: {error}")))?;
    tidepool_codegen::debug::init_logging();
    Ok((path, guard))
}

fn default_session_name(workspace: &Path) -> String {
    let source = workspace
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("workspace");
    let mut slug = String::new();
    let mut separator = false;
    for character in source.chars() {
        if character.is_ascii_alphanumeric() {
            slug.push(character.to_ascii_lowercase());
            separator = false;
        } else if !separator && !slug.is_empty() {
            slug.push('-');
            separator = true;
        }
    }
    while slug.ends_with('-') {
        slug.pop();
    }
    if slug.is_empty() {
        slug.push_str("workspace");
    }
    format!("shoal-{slug}")
}

fn retain_run_executable(run_root: &Path, name: &str, source: &Path) -> std::io::Result<PathBuf> {
    use std::io::Write;
    use std::os::unix::fs::PermissionsExt;
    let directory = run_root.join("bin");
    std::fs::create_dir_all(&directory)?;
    let bytes = std::fs::read(source)?;
    let destination = directory.join(name);
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&destination)?;
    file.write_all(&bytes)?;
    file.set_permissions(std::fs::Permissions::from_mode(0o755))?;
    file.sync_all()?;
    tidepool_atomic_write::write_durable(
        &directory.join(format!("{name}.blake3")),
        blake3::hash(&bytes).to_hex().as_bytes(),
    )?;
    Ok(destination)
}

/// Variables whose current values must override a possibly older tmux server
/// environment. Tmux supplies fresh `TMUX`/`TMUX_PANE` identities itself.
fn pane_environment() -> std::collections::BTreeMap<String, String> {
    const NAMES: &[&str] = &[
        "PATH",
        "HOME",
        "USER",
        "LOGNAME",
        "SHELL",
        "LANG",
        "LC_ALL",
        "TMPDIR",
        "XDG_CACHE_HOME",
        "XDG_CONFIG_HOME",
        "TIDEPOOL_EXTRACT",
        "TIDEPOOL_EXTRACT_WORKER",
        "TIDEPOOL_PRELUDE_DIR",
        "TIDEPOOL_GHC_LIBDIR",
        "TIDEPOOL_COMPILE_CACHE_DIR",
        "TIDEPOOL_BUILD_PRODUCTS_DIR",
        "TIDEPOOL_CONFIG_DIR",
        "TIDEPOOL_TOOLCHAIN_STAMP",
        "TIDEPOOL_TOOLCHAIN_HANDSHAKE",
        "CODEX_HOME",
        "CODEX_ROLLOUT_TRACE_ROOT",
        "OPENAI_API_KEY",
        "ANTHROPIC_API_KEY",
        "GEMINI_API_KEY",
        "TYPESAFE_API_KEY",
        "SSL_CERT_FILE",
        "NIX_SSL_CERT_FILE",
        "SSH_AUTH_SOCK",
        "RUST_LOG",
    ];
    NAMES
        .iter()
        .filter_map(|name| {
            std::env::var(name)
                .ok()
                .map(|value| ((*name).into(), value))
        })
        .collect()
}

fn runtime_error(message: impl Into<String>) -> Box<dyn std::error::Error> {
    Box::new(std::io::Error::other(message.into()))
}

#[cfg(test)]
mod tests {
    #[test]
    fn selected_runner_survives_disposable_target_removal() {
        let directory = tempfile::tempdir().unwrap();
        let target = directory.path().join("target");
        std::fs::create_dir(&target).unwrap();
        let source = target.join("runner");
        std::fs::write(&source, b"#!/bin/sh\nexit 0\n").unwrap();
        let run = directory.path().join("run");
        let selected = super::retain_run_executable(&run, "runner", &source).unwrap();
        std::fs::remove_dir_all(target).unwrap();
        assert!(std::process::Command::new(selected)
            .status()
            .unwrap()
            .success());
        assert!(run.join("bin/runner.blake3").is_file());
    }

    use super::*;

    fn test_agent_defaults() -> ShoalAgentDefaults {
        ShoalAgentDefaults {
            model: "test-model".into(),
            effort: ShoalEffort::Medium,
        }
    }

    async fn git_stdout(workspace: &Path, arguments: &[&str]) -> String {
        let output = tokio::process::Command::new("git")
            .args(arguments)
            .current_dir(workspace)
            .output()
            .await
            .unwrap();
        assert!(
            output.status.success(),
            "git {}: {}",
            arguments.join(" "),
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8(output.stdout).unwrap()
    }

    #[test]
    fn config_effort_defaults_low_and_preserves_explicit_values() {
        let absent: ShoalConfig = toml::from_str("[defaults]\nmodel = \"test\"\n").unwrap();
        assert_eq!(absent.defaults.effort, ShoalEffort::Low);
        for effort in [ShoalEffort::Low, ShoalEffort::Medium, ShoalEffort::High] {
            let config: ShoalConfig = toml::from_str(&format!(
                "[defaults]\nmodel = \"test\"\neffort = \"{effort}\"\n"
            ))
            .unwrap();
            assert_eq!(config.defaults.effort, effort);
        }
        assert!(toml::from_str::<ShoalConfig>(
            "[defaults]\nmodel = \"test\"\neffort = \"invalid\"\n"
        )
        .is_err());
    }

    /// A lock step that produces what `nix flake lock` would, so scaffolding
    /// is exercised on a machine with neither `nix` nor a network.
    struct StubLock;

    impl FlakeLock for StubLock {
        fn lock(&self, workspace: &Path) -> Result<(), Box<dyn std::error::Error>> {
            std::fs::write(
                workspace.join("flake.lock"),
                "{\n  \"nodes\": { \"root\": {} },\n  \"root\": \"root\",\n  \"version\": 7\n}\n",
            )?;
            Ok(())
        }
    }

    /// A machine that cannot lock: no `nix`, or no network.
    struct UnavailableLock;

    impl FlakeLock for UnavailableLock {
        fn lock(&self, _workspace: &Path) -> Result<(), Box<dyn std::error::Error>> {
            Err("cannot start nix to lock the project's flake".into())
        }
    }

    fn scaffold_workspace(path: &Path) -> Result<(), Box<dyn std::error::Error>> {
        new(NewOptions {
            path: Some(path.to_path_buf()),
            lock: Box::new(StubLock),
        })
    }

    fn example_skills() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../examples/shoal-workspace/.shoal/skills")
    }

    #[tokio::test]
    async fn new_commits_a_whole_package_into_a_fresh_repository() {
        let parent = tempfile::tempdir().unwrap();
        let workspace = parent.path().join("project");

        scaffold_workspace(&workspace).unwrap();

        assert!(workspace.join(".shoal/logs").is_dir());
        assert!(workspace.join(".shoal/sessions").is_dir());
        let config = read_project_config(&workspace).unwrap().0;
        assert_eq!(
            config.defaults,
            ShoalAgentDefaults {
                model: "gpt-5.6-sol".into(),
                effort: ShoalEffort::Medium,
            }
        );
        assert_eq!(
            config.haskell.flake_sources.get("jev-dsl").unwrap(),
            &[PathBuf::from("core")]
        );
        assert_eq!(config.haskell.source_roots, [PathBuf::from(".")]);
        let installed = std::fs::read_to_string(workspace.join(".git/info/exclude")).unwrap();
        for exclusion in tidepool_worktree::git::SHOAL_LOCAL_EXCLUDES {
            assert!(installed.lines().any(|line| line == *exclusion));
        }
        assert!(!installed.lines().any(|line| line == "/.shoal/"));
        assert_eq!(git_stdout(&workspace, &["status", "--short"]).await, "");
        assert_eq!(
            git_stdout(&workspace, &["show", "--format=%s", "--no-patch", "HEAD"])
                .await
                .trim(),
            "Initialize Shoal workspace"
        );
        assert_eq!(
            git_stdout(&workspace, &["ls-tree", "--name-only", "HEAD"]).await,
            ".agents\n.shoal\nflake.lock\nflake.nix\n"
        );
    }

    /// The scaffolded Haskell is the repository's own, byte for byte. A copy
    /// that drifted would compile against a different pinned revision than the
    /// one the example workspace is checked with.
    #[test]
    fn the_scaffolded_haskell_is_the_repositorys_own() {
        let workspace = tempfile::tempdir().unwrap();
        scaffold_workspace(workspace.path()).unwrap();
        for (relative, expected) in [
            (
                ".shoal/Jev/Operators.hs",
                include_str!("../../examples/shoal-workspace/.shoal/Jev/Operators.hs"),
            ),
            (
                ".shoal/AgentSpec.hs",
                include_str!("../../examples/shoal-workspace/.shoal/AgentSpec.hs"),
            ),
            (
                ".shoal/Project/Tools.hs",
                include_str!("../../examples/shoal-workspace/.shoal/Project/Tools.hs"),
            ),
            (
                ".shoal/Project/Watchdog.hs",
                include_str!("../../.shoal/Project/Watchdog.hs"),
            ),
        ] {
            assert_eq!(
                std::fs::read_to_string(workspace.path().join(relative)).unwrap(),
                expected,
                "{relative}"
            );
        }
        let spec = std::fs::read_to_string(workspace.path().join(".shoal/AgentSpec.hs")).unwrap();
        assert!(spec.contains("specTools = Tools.tools"), "{spec}");
        let tools =
            std::fs::read_to_string(workspace.path().join(".shoal/Project/Tools.hs")).unwrap();
        assert!(
            tools.contains("shell :: Command.ShellTools mode"),
            "{tools}"
        );
        assert!(tools.contains("inspection = Lookup.tools"), "{tools}");
    }

    /// Every workspace skill lands, and the links a client discovers them
    /// through resolve inside the new workspace.
    #[test]
    fn every_workspace_skill_lands_and_its_client_link_resolves() {
        let workspace = tempfile::tempdir().unwrap();
        scaffold_workspace(workspace.path()).unwrap();
        let source = example_skills();
        let mut checked = 0;
        for skill in std::fs::read_dir(&source).unwrap() {
            let skill = skill.unwrap().path();
            let name = skill.file_name().unwrap();
            let link = workspace.path().join(".agents/skills").join(name);
            assert_eq!(
                std::fs::read_link(&link).unwrap(),
                Path::new("../../.shoal/skills").join(name),
                "{}",
                link.display()
            );
            assert_eq!(
                std::fs::canonicalize(&link).unwrap(),
                std::fs::canonicalize(workspace.path().join(".shoal/skills").join(name)).unwrap()
            );
            for file in walk_files(&skill) {
                let relative = file.strip_prefix(&source).unwrap();
                assert_eq!(
                    std::fs::read_to_string(workspace.path().join(".shoal/skills").join(relative))
                        .unwrap(),
                    std::fs::read_to_string(&file).unwrap(),
                    "{}",
                    relative.display()
                );
                checked += 1;
            }
        }
        assert!(
            checked >= 11,
            "expected the shipped skill set, saw {checked}"
        );
    }

    fn walk_files(directory: &Path) -> Vec<PathBuf> {
        let mut found = Vec::new();
        for entry in std::fs::read_dir(directory).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                found.extend(walk_files(&path));
            } else {
                found.push(path);
            }
        }
        found
    }

    /// An existing repository keeps its files and its history: the package is
    /// written and staged, and the project makes the commit.
    #[test]
    fn new_stages_but_does_not_commit_inside_an_existing_repository() {
        let repo = tidepool_worktree::testing::TestRepo::init().unwrap();
        repo.writer()
            .commit_file("src/main.rs", "fn main() {}\n", "seed")
            .unwrap();
        let head = repo.path().to_path_buf();

        scaffold_workspace(&head).unwrap();

        assert_eq!(
            std::fs::read_to_string(head.join("src/main.rs")).unwrap(),
            "fn main() {}\n"
        );
        let git = tidepool_worktree::GitCli::new();
        let committed = git
            .try_run(&head, &["ls-tree", "--name-only", "HEAD"])
            .unwrap();
        assert_eq!(committed.trimmed(), "src");
        let staged = git
            .try_run(&head, &["diff", "--cached", "--name-only"])
            .unwrap();
        for path in [
            ".shoal/config.toml",
            ".shoal/AgentSpec.hs",
            ".shoal/Project/Tools.hs",
            ".shoal/Project/Watchdog.hs",
            ".shoal/Jev/Operators.hs",
            ".agents/skills/shoal-jev",
            "flake.nix",
            "flake.lock",
        ] {
            assert!(
                staged.lines().contains(&path),
                "{path} in {}",
                staged.trimmed()
            );
        }
    }

    #[test]
    fn new_refuses_a_directory_that_is_already_a_shoal_workspace() {
        let workspace = tempfile::tempdir().unwrap();
        scaffold_workspace(workspace.path()).unwrap();
        let before = std::fs::read_to_string(workspace.path().join(SHOAL_CONFIG)).unwrap();
        std::fs::remove_dir_all(workspace.path().join(".agents")).unwrap();

        let error = scaffold_workspace(workspace.path()).unwrap_err();

        assert!(
            matches!(
                error.downcast_ref::<NewRefusal>(),
                Some(NewRefusal::AlreadyAWorkspace(_))
            ),
            "{error}"
        );
        assert!(error.to_string().contains(SHOAL_CONFIG), "{error}");
        assert_eq!(
            std::fs::read_to_string(workspace.path().join(SHOAL_CONFIG)).unwrap(),
            before
        );
        assert!(!workspace.path().join(".agents").exists());
    }

    #[test]
    fn new_refuses_a_nonempty_directory_that_git_does_not_own() {
        let workspace = tempfile::tempdir().unwrap();
        std::fs::write(workspace.path().join("owned.txt"), "user data").unwrap();

        let error = scaffold_workspace(workspace.path()).unwrap_err();

        assert!(
            matches!(
                error.downcast_ref::<NewRefusal>(),
                Some(NewRefusal::NotARepositoryRoot(_))
            ),
            "{error}"
        );
        assert!(!workspace.path().join(".git").exists());
        assert!(!workspace.path().join(".shoal").exists());
        assert_eq!(
            std::fs::read_to_string(workspace.path().join("owned.txt")).unwrap(),
            "user data"
        );
    }

    /// A project that brought its own flake keeps it exactly, and is told the
    /// one input and the one command that install Jev.
    #[test]
    fn an_existing_flake_is_left_alone_and_its_project_is_told_what_to_add() {
        let repo = tidepool_worktree::testing::TestRepo::init().unwrap();
        let authored = "{ outputs = _: { }; }\n";
        repo.writer()
            .commit_file("flake.nix", authored, "seed")
            .unwrap();

        let report = scaffold::scaffold(repo.path(), &StubLock).unwrap();

        assert!(matches!(report.jev, scaffold::JevPin::ProjectFlake));
        assert_eq!(
            std::fs::read_to_string(repo.path().join("flake.nix")).unwrap(),
            authored
        );
        assert!(!repo.path().join("flake.lock").exists());
        let hint = scaffold::project_flake_hint(repo.path());
        assert!(hint.contains("inputs.jev-dsl"), "{hint}");
        assert!(hint.contains("nix flake lock"), "{hint}");
        assert!(hint.contains("Jev is unavailable"), "{hint}");
        assert!(repo.path().join(".shoal/config.toml").is_file());
    }

    /// Locking is the one step that needs a network. When it fails the pin is
    /// still on disk and the message says what finishing it takes.
    #[test]
    fn a_failed_lock_keeps_the_pin_and_says_jev_is_unavailable() {
        let workspace = tempfile::tempdir().unwrap();

        let report = scaffold::scaffold(workspace.path(), &UnavailableLock).unwrap();

        let scaffold::JevPin::Unlocked(error) = &report.jev else {
            panic!("an unavailable lock leaves the flake unlocked");
        };
        let message = scaffold::unlocked_message(workspace.path(), error.as_ref());
        assert!(message.contains("Jev is unavailable"), "{message}");
        assert!(message.contains("nix flake lock"), "{message}");
        let flake = std::fs::read_to_string(workspace.path().join("flake.nix")).unwrap();
        assert!(flake.contains("inputs.jev-dsl"), "{flake}");
        assert!(!workspace.path().join("flake.lock").exists());
        assert!(workspace.path().join(".shoal/config.toml").is_file());
    }

    /// `shoal init` starts a run; it does not create a workspace. A project
    /// that has none is told which command does, and keeps its directory.
    #[tokio::test]
    async fn init_without_a_workspace_names_the_command_that_creates_one() {
        let workspace = tempfile::tempdir().unwrap();

        let error = init(InitOptions {
            workspace: Some(workspace.path().to_path_buf()),
            session: None,
            recreate: false,
            no_attach: true,
            model: None,
            effort: None,
        })
        .await
        .unwrap_err();

        assert!(
            matches!(
                error.downcast_ref::<ConfigError>(),
                Some(ConfigError::NoWorkspace { .. })
            ),
            "{error}"
        );
        assert!(error.to_string().contains("shoal new"), "{error}");
        assert_eq!(std::fs::read_dir(workspace.path()).unwrap().count(), 0);
    }

    /// `resolve_workspace`'s cwd auto-detection (no `--workspace`) must look
    /// for `.shoal/`, not `tidepool_runtime::paths`'s own `.tidepool/`
    /// project marker — regression coverage for `shoal init` silently
    /// resolving to an unrelated ancestor (often `$HOME`, via its
    /// `~/.tidepool` legacy config dir) instead of the intended cwd. Mutates
    /// the process cwd, which is safe only because nextest gives each test
    /// its own process.
    #[test]
    fn resolve_workspace_auto_detection_uses_the_shoal_marker_not_tidepool() {
        let original_cwd = std::env::current_dir().unwrap();
        let root = tempfile::tempdir().unwrap();
        // An unrelated `.tidepool/` sits closer (at `root`) than any `.shoal/`
        // — the OLD (`.tidepool`-marker) walk would have stopped here.
        std::fs::create_dir_all(root.path().join(".tidepool")).unwrap();
        let workspace = root.path().join("project");
        scaffold_workspace(&workspace).unwrap();
        let nested = workspace.join("deep").join("nested");
        std::fs::create_dir_all(&nested).unwrap();

        let restore = |dir: &Path| std::env::set_current_dir(dir).unwrap();

        // From the workspace root itself: resolves to the workspace, not the
        // `.tidepool`-carrying `root` two levels up.
        std::env::set_current_dir(&workspace).unwrap();
        assert_eq!(
            resolve_workspace(None).unwrap(),
            std::fs::canonicalize(&workspace).unwrap()
        );

        // From a subdirectory: walks up to find the workspace's `.shoal/`,
        // same as git-style discovery — not past it to `root`.
        std::env::set_current_dir(&nested).unwrap();
        assert_eq!(
            resolve_workspace(None).unwrap(),
            std::fs::canonicalize(&workspace).unwrap()
        );

        // From `root` itself: no `.shoal/` anywhere in reach (its own
        // `.tidepool/` is irrelevant to Shoal), so resolution stays at `root`
        // and fails with a self-explaining error rather than silently
        // inventing a workspace.
        std::env::set_current_dir(root.path()).unwrap();
        let error = resolve_workspace(None).unwrap_err();
        assert!(
            matches!(
                error.downcast_ref::<ConfigError>(),
                Some(ConfigError::NoWorkspace { .. })
            ),
            "{error}"
        );
        assert!(error.to_string().contains("shoal new"), "{error}");

        restore(&original_cwd);
    }

    #[tokio::test]
    async fn local_excludes_work_in_linked_worktrees() {
        let parent = tempfile::tempdir().unwrap();
        let workspace = parent.path().join("project");
        scaffold_workspace(&workspace).unwrap();
        let linked = parent.path().join("linked");
        git_stdout(
            &workspace,
            &["worktree", "add", "-b", "linked", linked.to_str().unwrap()],
        )
        .await;
        let exclude = workspace.join(".git/info/exclude");
        std::fs::write(&exclude, "/user-local-file\n/.shoal/\n").unwrap();
        let git = tidepool_worktree::GitCli::new();
        git.ensure_shoal_local_exclude(&linked).unwrap();
        let first = std::fs::read_to_string(&exclude).unwrap();
        assert!(first.contains("/user-local-file\n"));
        for exclusion in tidepool_worktree::git::SHOAL_LOCAL_EXCLUDES {
            assert!(first.lines().any(|line| line == *exclusion));
        }
        assert!(!first.lines().any(|line| line == "/.shoal/"));
        git.ensure_shoal_local_exclude(&linked).unwrap();
        assert_eq!(first, std::fs::read_to_string(&exclude).unwrap());
        assert!(linked.join(".git").is_file());
    }

    #[test]
    fn project_names_become_valid_stable_session_names() {
        assert_eq!(
            default_session_name(Path::new("/tmp/My Project...")),
            "shoal-my-project"
        );
        assert_eq!(
            default_session_name(Path::new("/tmp/🐟")),
            "shoal-workspace"
        );
    }

    #[test]
    fn project_agent_defaults_are_explicit_and_cli_overrides_are_per_field() {
        let workspace = tempfile::tempdir().unwrap();
        scaffold_workspace(workspace.path()).unwrap();
        let configured = read_project_config(workspace.path()).unwrap().0.defaults;
        assert!(workspace.path().join(SHOAL_CONFIG).is_file());

        assert_eq!(
            resolve_agent_defaults(configured.clone(), None, None).unwrap(),
            configured
        );
        assert_eq!(
            resolve_agent_defaults(
                configured,
                Some("  override-model  ".into()),
                Some(ShoalEffort::High),
            )
            .unwrap(),
            ShoalAgentDefaults {
                model: "override-model".into(),
                effort: ShoalEffort::High,
            }
        );
    }

    #[test]
    fn research_policy_is_optional_configured_and_validated_at_load() {
        let workspace = tempfile::tempdir().unwrap();
        let path = workspace.path().join(SHOAL_CONFIG);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        let base = "[defaults]\nmodel = \"test-model\"\neffort = \"low\"\n";
        std::fs::write(&path, base).unwrap();
        assert_eq!(
            read_project_config(workspace.path()).unwrap().0.research,
            tidepool_actor::ResearchPolicy::default()
        );
        std::fs::write(
            &path,
            format!("{base}\n[research]\nmaximum_depth = 3\nmaximum_active_children = 2\n"),
        )
        .unwrap();
        assert_eq!(
            read_project_config(workspace.path()).unwrap().0.research,
            tidepool_actor::ResearchPolicy {
                maximum_depth: 3,
                maximum_active_children: Some(2),
                default_depth: 1
            }
        );
        for invalid in [
            "maximum_depth = -1",
            "maximum_active_children = 65536",
            "depth = 3",
        ] {
            std::fs::write(&path, format!("{base}\n[research]\n{invalid}\n")).unwrap();
            assert!(read_project_config(workspace.path())
                .unwrap_err()
                .to_string()
                .contains("invalid Shoal configuration"));
        }
    }

    #[test]
    fn source_exclusion_rejects_tracked_files_at_config_load() {
        let repo = tidepool_worktree::testing::TestRepo::init().unwrap();
        repo.writer()
            .commit_file("tracked/file", "source", "seed")
            .unwrap();
        let config = repo.path().join(SHOAL_CONFIG);
        std::fs::create_dir_all(config.parent().unwrap()).unwrap();
        let write_config = |excluded: &str| {
            std::fs::write(
                &config,
                format!(
                    "[defaults]\nmodel = \"test-model\"\n[launch]\nsource_exclude = [\"{excluded}\"]\n"
                ),
            )
            .unwrap();
        };
        write_config("tracked");
        assert!(read_project_config(repo.path())
            .unwrap_err()
            .to_string()
            .contains("contains tracked source"));
        write_config("scratch");
        assert_eq!(
            read_project_config(repo.path())
                .unwrap()
                .0
                .launch
                .source_exclude,
            ["scratch"]
        );
        write_config("../outside");
        assert!(read_project_config(repo.path()).is_err());
    }

    #[test]
    fn invalid_project_agent_defaults_fail_at_the_configuration_boundary() {
        let workspace = tempfile::tempdir().unwrap();
        let path = workspace.path().join(SHOAL_CONFIG);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, "[defaults]\nmodel = \"\"\neffort = \"low\"\n").unwrap();
        let error = read_project_config(workspace.path()).unwrap_err();
        assert!(error.to_string().contains("selects an empty model"));

        std::fs::write(
            &path,
            "[defaults]\nmodel = \"test-model\"\neffort = \"furious\"\n",
        )
        .unwrap();
        let error = read_project_config(workspace.path()).unwrap_err();
        assert!(error.to_string().contains("invalid Shoal configuration"));
    }

    #[test]
    fn shoal_logs_live_under_the_repository_local_state_directory() {
        assert_eq!(
            shoal_log_path(Path::new("/tmp/project"), "run-1"),
            Path::new("/tmp/project/.shoal/logs/run-1.log")
        );
        assert_eq!(
            shoal_compiler_log_path(Path::new("/tmp/project"), "run-1"),
            Path::new("/tmp/project/.shoal/logs/run-1-compiler.log")
        );
        assert_eq!(
            shoal_trace_path(Path::new("/tmp/project"), "run-1"),
            Path::new("/tmp/project/.shoal/logs/run-1.jsonl")
        );
    }

    #[derive(Clone, Default)]
    struct CapturedWriter(std::sync::Arc<std::sync::Mutex<Vec<u8>>>);

    struct CapturedGuard(std::sync::Arc<std::sync::Mutex<Vec<u8>>>);

    impl std::io::Write for CapturedGuard {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(bytes);
            Ok(bytes.len())
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    impl<'writer> tracing_subscriber::fmt::MakeWriter<'writer> for CapturedWriter {
        type Writer = CapturedGuard;

        fn make_writer(&'writer self) -> Self::Writer {
            CapturedGuard(std::sync::Arc::clone(&self.0))
        }
    }

    impl CapturedWriter {
        fn text(&self) -> String {
            String::from_utf8(self.0.lock().unwrap().clone()).unwrap()
        }
    }

    fn detailed_filter_without_content() -> tracing_subscriber::EnvFilter {
        tracing_subscriber::EnvFilter::new("debug")
            .add_directive(format!("{CONTENT_TARGET}=off").parse().unwrap())
    }

    fn trace_lines(trace: &CapturedWriter) -> Vec<serde_json::Value> {
        trace
            .text()
            .lines()
            .map(|line| {
                serde_json::from_str(line).unwrap_or_else(|error| {
                    panic!("trace line is not JSON ({error}): {line}");
                })
            })
            .collect()
    }

    fn span_names(line: &serde_json::Value) -> Vec<String> {
        line["spans"]
            .as_array()
            .expect("a trace line carries its span list")
            .iter()
            .map(|span| span["name"].as_str().unwrap_or_default().to_owned())
            .collect()
    }

    #[test]
    fn host_trace_nests_one_cell_and_keeps_content_out_of_the_human_log() {
        let detailed = CapturedWriter::default();
        let pane = CapturedWriter::default();
        let trace = CapturedWriter::default();
        let subscriber = host_tracing_subscriber(
            detailed.clone(),
            pane.clone(),
            trace.clone(),
            detailed_filter_without_content(),
        );

        tracing::subscriber::with_default(subscriber, || {
            let run = tracing::info_span!("shoal_host", run_id = "run-1");
            let _run = run.enter();
            let actor = tracing::info_span!("actor", actor = "3@1", incarnation = 1_u64);
            let _actor = actor.enter();
            let call = tracing::info_span!("tool_call", call_id = "call-x", tool = "haskell");
            let _call = call.enter();
            let cell = tracing::info_span!("cell", execution = "exec-9");
            let _cell = cell.enter();
            let unit = tracing::info_span!("unit", index = 0_u64, kind = "cell");
            let _unit = unit.enter();
            tracing::info!(
                target: CONTENT_TARGET,
                source = "putStrLn \"secret cell source\"",
                "cell input unit source"
            );
        });

        let lines = trace_lines(&trace);
        let content = lines
            .iter()
            .find(|line| line["target"] == CONTENT_TARGET)
            .expect("the content event reaches the run-local trace");
        assert_eq!(
            span_names(content),
            ["shoal_host", "actor", "tool_call", "cell", "unit"]
        );
        assert_eq!(content["spans"][0]["run_id"], "run-1");
        assert_eq!(content["spans"][2]["call_id"], "call-x");
        assert_eq!(content["spans"][3]["execution"], "exec-9");
        assert_eq!(content["span"]["kind"], "cell");
        assert_eq!(
            content["fields"]["source"],
            "putStrLn \"secret cell source\""
        );

        let closed: Vec<String> = lines
            .iter()
            .filter(|line| line["fields"]["message"] == "close")
            .map(|line| line["span"]["name"].as_str().unwrap_or_default().to_owned())
            .collect();
        assert_eq!(closed, ["unit", "cell", "tool_call", "actor", "shoal_host"]);

        assert!(!detailed.text().contains("secret cell source"));
        assert!(!pane.text().contains("secret cell source"));
    }

    /// The request-update proxy keeps its payload private by never passing it
    /// to a logging call site: `not_presented` logs a bounded failure reason
    /// and nothing else (`actor_host.rs`, the "Private baseline
    /// clarification" assertion). That test runs against a `debug`-filtered
    /// log; the trace layer added here is `info`-filtered over the same
    /// events, so it can never show more. This pins the shape: what the call
    /// site passes is what every layer gets.
    #[test]
    fn the_json_trace_shows_only_what_the_failure_call_site_passed_it() {
        let detailed = CapturedWriter::default();
        let pane = CapturedWriter::default();
        let trace = CapturedWriter::default();
        let subscriber = host_tracing_subscriber(
            detailed.clone(),
            pane.clone(),
            trace.clone(),
            detailed_filter_without_content(),
        );

        tracing::subscriber::with_default(subscriber, || {
            tracing::warn!(
                target: "tidepool::actor_host",
                actor = "3@1",
                update = 1,
                error = "connecting update proxy: controlled transport failure",
                "request update not presented"
            );
        });

        let line = trace_lines(&trace)
            .into_iter()
            .find(|line| line["fields"]["message"] == "request update not presented")
            .expect("the failure reaches the run-local trace");
        assert_eq!(
            line["fields"]["error"],
            "connecting update proxy: controlled transport failure"
        );
        for rendered in [detailed.text(), pane.text(), trace.text()] {
            assert!(rendered.contains("connecting update proxy"));
            assert!(!rendered.contains("Private baseline clarification"));
        }
    }

    #[test]
    fn host_trace_reaches_the_jsonl_file_once_the_appender_guard_drops() {
        let directory = tempfile::tempdir().unwrap();
        let path = shoal_trace_path(directory.path(), "run-flush");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        let file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
            .unwrap();
        let (writer, guard) = tracing_appender::non_blocking(file);
        let subscriber = host_tracing_subscriber(
            CapturedWriter::default(),
            CapturedWriter::default(),
            writer,
            detailed_filter_without_content(),
        );
        tracing::subscriber::with_default(subscriber, || {
            let run = tracing::info_span!("shoal_host", run_id = "run-flush");
            let _run = run.enter();
            tracing::info!(target: CONTENT_TARGET, receipt = "committed", "input unit receipt");
        });
        drop(guard);

        let written = std::fs::read_to_string(&path).unwrap();
        let line: serde_json::Value =
            serde_json::from_str(written.lines().next().unwrap()).unwrap();
        assert_eq!(line["target"], CONTENT_TARGET);
        assert_eq!(line["spans"][0]["run_id"], "run-flush");
    }

    #[test]
    fn host_tracing_fans_out_safe_info_but_keeps_source_debug_in_the_file() {
        let detailed = CapturedWriter::default();
        let pane = CapturedWriter::default();
        let subscriber = host_tracing_subscriber(
            detailed.clone(),
            pane.clone(),
            CapturedWriter::default(),
            tracing_subscriber::EnvFilter::new("debug"),
        );

        tracing::subscriber::with_default(subscriber, || {
            tracing::info!(
                target: "tidepool::shoal",
                run_id = "run-visible",
                "host lifecycle visible"
            );
            tracing::debug!(
                target: "tidepool::shoal",
                source = "sensitive Haskell source",
                "host diagnostic detail"
            );
        });

        let detailed = detailed.text();
        let pane = pane.text();
        assert!(detailed.contains("host lifecycle visible"));
        assert!(pane.contains("host lifecycle visible"));
        assert!(detailed.contains("sensitive Haskell source"));
        assert!(!pane.contains("sensitive Haskell source"));
        assert!(!detailed.contains('\u{1b}'));
        assert!(!pane.contains('\u{1b}'));
    }

    #[test]
    fn run_status_round_trips_as_a_closed_sum() {
        let root_actor = ActorRef::first(tidepool_actor::ActorId(9));
        for phase in [
            RunPhase::AwaitingBinding { root_actor },
            RunPhase::Ready {
                root_actor,
                root_thread: BackendThreadId("thread".into()),
            },
        ] {
            let status = RunStatus::new(
                "run-1",
                Path::new("/tmp/work"),
                "shoal-work",
                test_agent_defaults(),
                phase,
            );
            let encoded = serde_json::to_vec(&status).unwrap();
            assert_eq!(status.version, STATUS_VERSION);
            assert_eq!(decode_run_status(&encoded).unwrap(), status);
        }

        let current = RunStatus::new(
            "run-1",
            Path::new("/tmp/work"),
            "shoal-work",
            test_agent_defaults(),
            RunPhase::AwaitingBinding { root_actor },
        );
        let mut legacy = serde_json::to_value(current).unwrap();
        let object = legacy.as_object_mut().unwrap();
        object.insert("version".into(), PREVIOUS_STATUS_VERSION.into());
        object.remove("recovered_actors");
        object.remove("lost_state");
        object.remove("resource_service");
        let migrated = decode_run_status(&serde_json::to_vec(&legacy).unwrap()).unwrap();
        assert_eq!(migrated.version, STATUS_VERSION);
        assert!(migrated.recovered_actors.is_empty());
        assert!(migrated.lost_state.is_empty());
        assert_eq!(migrated.resource_service.healthy, None);

        let old = serde_json::json!({
            "version": 3,
            "run_id": "run-1",
            "workspace": "/tmp/work",
            "session": "shoal-work",
            "phase": {"state": "awaiting_input", "root_actor": root_actor},
        });
        assert!(decode_run_status(&serde_json::to_vec(&old).unwrap())
            .unwrap_err()
            .to_string()
            .contains("unsupported Shoal run status version 3"));
    }

    #[test]
    fn recovery_status_retains_lost_actor_and_resource_evidence() {
        let predecessor = ActorRef::first(tidepool_actor::ActorId(7));
        let actor = ActorRef {
            id: predecessor.id,
            incarnation: tidepool_actor::Incarnation(2),
        };
        let mut status = RunStatus::new(
            "run-1",
            Path::new("/tmp/work"),
            "shoal-work",
            test_agent_defaults(),
            RunPhase::Recovering {
                stage: RecoveryStage::RestoringActors,
                restored_source: Some("source-revision".into()),
            },
        )
        .at_generation(2);
        status.recovered_actors = vec![RecoveredActorObservation { predecessor, actor }];
        status.lost_state = recovery_lost_state(2);
        status.resource_service = ResourceServiceObservation {
            healthy: Some(true),
            resources: Some(
                tidepool_node::command_resources::CommandResourceObservation {
                    active: 2,
                    historical: 9,
                    retained_allocations: 1,
                    cleanup_failures: 1,
                    ..Default::default()
                },
            ),
            detail: None,
        };

        let decoded = decode_run_status(&serde_json::to_vec(&status).unwrap()).unwrap();
        assert_eq!(decoded, status);
        assert_eq!(decoded.lost_state.len(), 4);
        let resources = decoded.resource_service.resources.unwrap();
        assert_eq!((resources.active, resources.historical), (2, 9));
        assert_eq!(
            (resources.retained_allocations, resources.cleanup_failures),
            (1, 1)
        );
    }

    #[test]
    fn run_storage_observation_is_bounded_and_does_not_follow_symlinks() {
        let root = tempfile::tempdir().unwrap();
        std::fs::write(root.path().join("one"), b"1234").unwrap();
        std::fs::create_dir(root.path().join("nested")).unwrap();
        std::fs::write(root.path().join("nested/two"), b"12").unwrap();
        #[cfg(unix)]
        std::os::unix::fs::symlink(root.path(), root.path().join("loop")).unwrap();

        let complete = observe_storage(root.path(), 16);
        assert_eq!(complete.bytes, 6);
        assert!(!complete.truncated);
        let bounded = observe_storage(root.path(), 2);
        assert!(bounded.truncated);
        assert_eq!(bounded.entries, 2);
    }

    #[test]
    #[ignore = "measurement harness; run explicitly at integration boundaries"]
    fn run_storage_observation_measurement() {
        let root = tempfile::tempdir().unwrap();
        for ordinal in 0..8_000 {
            std::fs::write(root.path().join(format!("entry-{ordinal}")), b"12345678").unwrap();
        }
        let iterations = 100_u128;
        let started = std::time::Instant::now();
        let mut observation = BoundedStorageObservation::default();
        for _ in 0..iterations {
            observation = observe_storage(root.path(), 8_192);
        }
        eprintln!(
            "run_storage entries={} bytes={} sample_micros={}",
            observation.entries,
            observation.bytes,
            started.elapsed().as_micros() / iterations,
        );
        assert!(!observation.truncated);
    }

    #[test]
    fn compiler_daemon_is_tmux_owned_and_only_the_host_receives_its_socket() {
        let workspace = Path::new("/tmp/workspace");
        let socket = Path::new("/tmp/run/compiler.sock");
        let log_path = Path::new("/tmp/workspace/.shoal/logs/run-1-compiler.log");
        let launch = compiler_daemon_launch(
            workspace,
            socket,
            "/tmp/tidepool-extract".into(),
            "run-1",
            log_path,
        );

        assert_eq!(launch.window_name, "Compiler");
        assert_eq!(launch.cwd, workspace);
        assert_eq!(launch.program, "/tmp/tidepool-extract");
        assert_eq!(
            launch.args,
            [
                "--daemon",
                "--socket",
                "/tmp/run/compiler.sock",
                "--persistent",
                "--run-id",
                "run-1",
                "--log-path",
                "/tmp/workspace/.shoal/logs/run-1-compiler.log"
            ]
        );
        assert!(!launch
            .environment
            .contains_key(tidepool_extract_cmd::DAEMON_SOCKET_ENV));
        assert_eq!(
            host_environment(socket)
                .get(tidepool_extract_cmd::DAEMON_SOCKET_ENV)
                .map(String::as_str),
            Some("/tmp/run/compiler.sock")
        );
    }

    #[test]
    fn host_failure_publishes_its_exact_terminal_diagnostic() {
        let root = tempfile::tempdir().unwrap();
        let status_path = root.path().join("status.json");
        let options = HostOptions {
            workspace: root.path().into(),
            session: "shoal-test".into(),
            run_id: "run-failed".into(),
            run_root: root.path().join("run"),
            status_path: status_path.clone(),
            root_binding_path: root.path().join("binding.json"),
            interactive_agent: tidepool_agent::native_interactive_agent_from_parts(
                std::env::current_exe().unwrap(),
                "test installation".into(),
            )
            .unwrap(),
            resume_root: false,
            agent: test_agent_defaults(),
        };
        let mut recovering = RunStatus::new(
            "run-failed",
            root.path(),
            "shoal-test",
            test_agent_defaults(),
            RunPhase::Recovering {
                stage: RecoveryStage::RestoringActors,
                restored_source: Some("source-revision".into()),
            },
        )
        .at_generation(7);
        recovering.lost_state = recovery_lost_state(7);
        recovering.resource_service.healthy = Some(false);
        recovering.resource_service.detail = Some("resource observation unavailable".into());
        write_status(&status_path, &recovering).unwrap();
        let result = settle_host_result(Err(runtime_error("compile exploded")), &options, 7);
        assert!(result.is_err());
        let status: RunStatus =
            serde_json::from_slice(&std::fs::read(status_path).unwrap()).unwrap();
        assert_eq!(
            status.phase,
            RunPhase::Failed {
                error: "compile exploded".into()
            }
        );
        assert_eq!(status.host_generation, 7);
        assert_eq!(status.lost_state, recovering.lost_state);
        assert_eq!(status.resource_service, recovering.resource_service);
    }

    #[tokio::test]
    async fn requested_resume_fails_closed_without_a_valid_retained_binding() {
        let root = tempfile::tempdir().unwrap();
        let missing = root.path().join("missing-binding.json");
        let error = resolve_root_launch_mode(true, &missing)
            .await
            .expect_err("resume must not silently become fresh");
        assert!(error
            .to_string()
            .contains("cannot resume the requested root conversation"));
        assert_eq!(
            resolve_root_launch_mode(false, &missing).await.unwrap(),
            InteractiveLaunchMode::Fresh
        );

        let legacy = root.path().join("v3-binding.json");
        tokio::fs::write(
            &legacy,
            r#"{"version":3,"thread":"01a05a16-97f5-7722-aa8d-467e01e2e5b4"}"#,
        )
        .await
        .unwrap();
        let error = resolve_root_launch_mode(true, &legacy)
            .await
            .expect_err("v3 cannot certify queue readiness");
        assert!(error.to_string().contains("start a fresh Shoal root"));
    }

    #[test]
    fn fresh_launch_discards_a_previous_runs_conversation_binding() {
        let root = tempfile::tempdir().unwrap();
        let binding = root.path().join("root-binding.json");
        std::fs::write(&binding, "stale").unwrap();

        clear_fresh_root_binding(&binding).unwrap();
        assert!(!binding.exists());
        clear_fresh_root_binding(&binding).unwrap();
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn packaged_interactive_agent_gets_a_replaceable_project_gc_root() {
        use std::os::unix::fs::PermissionsExt;

        let workspace = tempfile::tempdir().unwrap();
        let first = tempfile::tempdir().unwrap();
        let second = tempfile::tempdir().unwrap();
        let fake_nix_store = workspace.path().join("nix-store");
        std::fs::write(&fake_nix_store, "#!/bin/sh\nln -sfn \"$2\" \"$4\"\n").unwrap();
        std::fs::set_permissions(&fake_nix_store, std::fs::Permissions::from_mode(0o700)).unwrap();
        retain_packaged_interactive_agent_from(
            workspace.path(),
            Some(first.path().to_path_buf()),
            Some(fake_nix_store.clone()),
        )
        .await
        .unwrap();
        let link = workspace.path().join(".shoal/runtime/interactive-agent");
        assert_eq!(std::fs::read_link(&link).unwrap(), first.path());

        retain_packaged_interactive_agent_from(
            workspace.path(),
            Some(second.path().to_path_buf()),
            Some(fake_nix_store),
        )
        .await
        .unwrap();
        assert_eq!(std::fs::read_link(link).unwrap(), second.path());
    }
}
