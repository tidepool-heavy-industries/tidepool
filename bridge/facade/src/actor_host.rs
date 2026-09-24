//! Composition root for the first actor-native interactive swarm.
//!
//! The daemon owns resident Haskell scheduling and exact actor lifecycle. One
//! stock interactive agent is attached to each installed Haskell tool policy;
//! tmux is process ownership and observability, never message transport.

#[cfg(test)]
mod agent_spec_tests;
#[cfg(test)]
mod call_timing_tests;
#[cfg(test)]
mod cell_compile_cost_tests;
#[cfg(test)]
mod command_jobs_tests;
mod commands;
#[cfg(test)]
mod custody_tests;
#[cfg(test)]
mod documentation_tests;
mod host_incarnation;
#[allow(dead_code)] // Full retained domain evidence is richer than current UI rendering.
mod hosted_retirement;
#[cfg(test)]
mod hosted_tools_tests;
#[cfg(test)]
mod jev_tests;
#[cfg(test)]
mod lookup_availability_tests;
#[cfg(test)]
mod observation_budget_tests;
mod overlay_resource;
#[cfg(test)]
mod source_reload_tests;
#[cfg(test)]
#[path = "host_dynamic_tools/tui_resource_tests.rs"]
mod tui_resource_tests;
#[cfg(test)]
#[path = "host_dynamic_tools/tui_sleep_tests.rs"]
mod tui_sleep_tests;
mod workspace;
pub mod workspace_cleanup;
mod workspace_publication;
pub(crate) use hosted_retirement::{CompletionBoundary, HostedObservation};
use workspace::{ActiveWorkspace, PreparedWorkspace, WorkspaceLayout};
mod model_free;
mod prompt_catalog;
pub(crate) mod recipe_checks;
#[cfg(test)]
mod research_policy_tests;
#[cfg(test)]
mod resource_tests;
mod scoped_custody;
mod socket_directory;
#[cfg(test)]
mod test_campaign;
#[cfg(test)]
mod tests;

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::fmt;
use std::panic::AssertUnwindSafe;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use exomonad_actor::{
    ActorDescriptor, ActorEffectProfile, ActorExitKind, ActorPlacement, ActorRef, ActorTerminal,
    ActorWorkbenchSource, ExternalApplicationFailure, ExternalApplicationFailureClass,
    ExternalFailureDisposition, ForkWorkspaceAdmission, ForkWorkspaceAdmissionError,
    ForkWorkspaceSeed, LocalActorRef, LocalResidentDeployment, LocalResidentInstallation,
    ResidentActorRoot, ResidentForest,
};
use exomonad_agent::interactive::InputProducerId;
use exomonad_agent::{
    copy_interactive_binding, native_interactive_backend, read_interactive_binding,
    BackendThreadId, InputOperationId, InputPurpose, InteractiveAgentBackend,
    InteractiveAgentInstallation, InteractiveAgentSpec, InteractiveInputEnvelope,
    InteractiveInputMode, InteractiveInputTarget, InteractiveLaunchMode, InteractiveNativeSandbox,
    InteractiveNativeToolPolicy, InteractivePolicyMount, QueueReadyThread, ReasoningEffort,
};
use frunk::{hlist, HCons, HNil};
use futures_util::FutureExt;
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};

use exomonad_node::{
    DurableInbox, ProcessInvocation, ProcessMountBoundary, ProcessSupervisorClient,
    ProcessSupervisorManifest, ServiceEnvironment, TmuxLaunch, TmuxPaneId, TmuxSession,
    BUBBLEWRAP_PROGRAM,
};
use exomonad_worktree::{
    ActiveBinding, AgentRef as WorktreePrincipal, BindingTable, EventJournal, GitCli,
    WorktreeHandle, WorktreeId, WorktreeManager, WorktreeMonitor, WorktreeRegistry,
};
use tidepool_effect::{EffectRunPolicy, LivePayloadPolicy};
use tidepool_handlers::{
    ActorBoundWorktreeHandler, ActorWorktreeAllocationHandler, ActorWorktreeAuthority,
    ActorWorktreeGrant, ActorWorktreeHandler, ActorWorktreeIntegrationHandler,
    ActorWorktreeRegistryHandler, EventConfig, RepoEventHandler, WorktreeHandler,
};
use tidepool_mcp::CapturedOutput;
use tidepool_runtime::session::{
    insert_preamble_imports, resident_workbench_templates, run_turn, ResidentSession,
    ResidentSessionState, SessionLib, TurnRequest as HaskellTurnRequest, TurnResult,
};
use tidepool_runtime::DEFAULT_NURSERY_SIZE;
use tokio::net::UnixListener;
use tokio::sync::{mpsc, oneshot, watch};
use tokio::task::JoinSet;

pub(crate) use self::host_incarnation::HostIncarnationLease;
use self::overlay_resource::{OverlayResourceLease, OverlaySnapshot, SharedOverlayResource};
use self::prompt_catalog::{FrozenBasePrompt, PromptId};
use self::socket_directory::SocketDirectory;

/// Every interactive actor sees its own repository at this path. Bubblewrap
/// mount namespaces make the shared name safe across concurrent actors, while
/// Codex needs only one persisted project-trust decision.
pub(crate) const ACTOR_PROJECT_ROOT: &str = "/tmp/exomonad-actor-workspace";
const ACTOR_BUILD_TARGET: &str = ".exomonad/build/cargo";

const DRIVER_MODULE: &str = "Tidepool.Actors.Internal.ExomonadDriver";
const WORKBENCH_SURFACE_MODULE: &str = "Tidepool.Actors.Exomonad";
const DRIVER_ENTRY: &str = "rootDriver";
const DRIVER_EFFECTS: &str = "RootEffects";
const EXOMONAD_REPLACED_EFFECT_NAMES: &[&str] = &[
    "boundWorktree",
    "createWorktree",
    "listWorktrees",
    "lookupWorktree",
    "observeSubmission",
    "tryMerge",
    "worktreeBranch",
    "worktreeHead",
    "worktreeId",
];
const CHILD_LIFECYCLE_NOTICE: &str = "A child actor changed lifecycle state.";
const APPLICATION_SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(15);
const APPLICATION_TASK_GRACE_TIMEOUT: Duration = Duration::from_secs(5);
const PROCESS_OPERATION_TIMEOUT: Duration = Duration::from_secs(30);

type ExomonadHandlerStack = HCons<
    tidepool_handlers::SourceHandler,
    HCons<
        tidepool_handlers::JournalHandler,
        HCons<
            RepoEventHandler,
            HCons<
                ActorBoundWorktreeHandler,
                HCons<
                    ActorWorktreeRegistryHandler,
                    HCons<
                        ActorWorktreeAllocationHandler,
                        HCons<ActorWorktreeIntegrationHandler, HCons<ActorWorktreeHandler, HNil>>,
                    >,
                >,
            >,
        >,
    >,
>;
type ExomonadRoot = ResidentActorRoot<ExomonadHandlerStack, CapturedOutput>;

#[derive(Clone)]
struct ActorForkWorkspaceAdmission {
    worktrees: Arc<Mutex<ActorWorktreeHandler>>,
    authority: ActorWorktreeAuthority,
    manager: WorktreeManager,
    bindings: Arc<Mutex<BindingTable>>,
    runtime: String,
    native: Option<NativeForkAdmission>,
}

struct ActorWorkspaceCustody {
    runtime: String,
    bindings: Arc<Mutex<BindingTable>>,
    binding: Mutex<Option<ActiveBinding>>,
    actor: ActorRef,
    state: Arc<Mutex<scoped_custody::CustodyState>>,
    workspace: Option<Arc<PreparedWorkspace>>,
    inheritance_notice: Option<String>,
}

impl exomonad_actor::ForkWorkspaceCustody for ActorWorkspaceCustody {
    fn transfer_to(
        &self,
        successor: ActorRef,
    ) -> Result<Arc<dyn exomonad_actor::ForkWorkspaceCustody>, ForkWorkspaceAdmissionError> {
        let state = self.state.lock();
        let process_absent = match state.launch {
            scoped_custody::LaunchCustody::Unclaimed => true,
            #[cfg(test)]
            scoped_custody::LaunchCustody::ScopedNotSpawned => true,
            scoped_custody::LaunchCustody::ScopedClaimed
            | scoped_custody::LaunchCustody::Legacy => false,
        };
        if !process_absent || state.terminal.is_some() {
            return Err(ForkWorkspaceAdmissionError {
                detail: "workspace transfer requires a live actor with no possible native process"
                    .into(),
            });
        }
        let mut binding = self.binding.lock();
        let lease = binding
            .as_mut()
            .ok_or_else(|| ForkWorkspaceAdmissionError {
                detail: "workspace custody was already transferred".into(),
            })?;
        self.bindings
            .lock()
            .transfer(
                lease,
                &WorktreePrincipal::exact_actor(
                    &self.runtime,
                    successor.id.0,
                    successor.incarnation.0,
                ),
                current_time_ms(),
            )
            .map_err(|error| ForkWorkspaceAdmissionError {
                detail: error.to_string(),
            })?;
        Ok(Arc::new(Self {
            runtime: self.runtime.clone(),
            bindings: self.bindings.clone(),
            binding: Mutex::new(binding.take()),
            actor: successor,
            state: Arc::new(Mutex::new(scoped_custody::CustodyState::default())),
            workspace: self.workspace.clone(),
            inheritance_notice: self.inheritance_notice.clone(),
        }))
    }
    fn actor_stopped(&self, terminal: &exomonad_actor::ActorTerminal) {
        // Observation is monotonic: duplicate notifications cannot replace the
        // first exact terminal or confuse "not completed" with "still active".
        self.state
            .lock()
            .terminal
            .get_or_insert_with(|| terminal.clone());
    }
    fn process_may_exist(&self) {
        self.state.lock().launch = scoped_custody::LaunchCustody::Legacy;
    }
}

impl Drop for ActorWorkspaceCustody {
    fn drop(&mut self) {
        let state = self.state.lock();
        if matches!(
            state.launch,
            scoped_custody::LaunchCustody::Legacy | scoped_custody::LaunchCustody::ScopedClaimed
        ) {
            tracing::error!(actor = ?self.actor, "retaining worktree custody: process or host cleanup is unconfirmed");
            return;
        }
        if let Some(binding) = self.binding.get_mut().take() {
            let result = if state
                .terminal
                .as_ref()
                .is_some_and(|terminal| terminal.kind == ActorExitKind::Completed)
            {
                binding.complete(&mut self.bindings.lock())
            } else {
                binding.release(&mut self.bindings.lock())
            };
            if let Err(error) = result {
                tracing::error!(actor = ?self.actor, %error, "worktree custody release failed");
            }
        }
    }
}

impl ActorForkWorkspaceAdmission {
    fn recover_workspace(
        &self,
        predecessor: ActorRef,
        successor: ActorRef,
        worktree: &str,
        role: exomonad_actor::ActorRole,
    ) -> Result<Arc<dyn exomonad_actor::ForkWorkspaceCustody>, ForkWorkspaceAdmissionError> {
        if !WorktreeId::is_path_safe(worktree) {
            return Err(ForkWorkspaceAdmissionError {
                detail: "invalid recovery worktree id".into(),
            });
        }
        let worktree = WorktreeId::from_raw(worktree);
        if self
            .manager
            .lookup(&worktree)
            .map_err(|error| ForkWorkspaceAdmissionError {
                detail: error.to_string(),
            })?
            .is_none()
        {
            return Err(ForkWorkspaceAdmissionError {
                detail: "recovery worktree is not registered".into(),
            });
        }
        let predecessor_principal = WorktreePrincipal::exact_actor(
            &self.runtime,
            predecessor.id.0,
            predecessor.incarnation.0,
        );
        let successor_principal =
            WorktreePrincipal::exact_actor(&self.runtime, successor.id.0, successor.incarnation.0);
        let binding = self
            .bindings
            .lock()
            .recover_active(
                &worktree,
                &predecessor_principal,
                &successor_principal,
                current_time_ms(),
            )
            .map_err(|error| ForkWorkspaceAdmissionError {
                detail: error.to_string(),
            })?;
        self.authority
            .install_grant(successor.into(), worktree_grant(role));
        Ok(Arc::new(ActorWorkspaceCustody {
            runtime: self.runtime.clone(),
            bindings: self.bindings.clone(),
            binding: Mutex::new(Some(binding)),
            actor: successor,
            state: Arc::new(Mutex::new(scoped_custody::CustodyState::default())),
            workspace: None,
            inheritance_notice: None,
        }))
    }

    fn bind_workspace(
        &self,
        actor: ActorRef,
        worktree: &str,
        workspace: Option<Arc<PreparedWorkspace>>,
        inheritance_notice: Option<String>,
    ) -> Result<Arc<dyn exomonad_actor::ForkWorkspaceCustody>, ForkWorkspaceAdmissionError> {
        if !WorktreeId::is_path_safe(worktree) {
            return Err(ForkWorkspaceAdmissionError {
                detail: "invalid custody worktree id".into(),
            });
        }
        if self
            .manager
            .lookup(&WorktreeId::from_raw(worktree))
            .map_err(|error| ForkWorkspaceAdmissionError {
                detail: error.to_string(),
            })?
            .is_none()
        {
            return Err(ForkWorkspaceAdmissionError {
                detail: "custody worktree is not registered".into(),
            });
        }
        let principal =
            WorktreePrincipal::exact_actor(&self.runtime, actor.id.0, actor.incarnation.0);
        let binding = self
            .bindings
            .lock()
            .bind(
                &WorktreeId::from_raw(worktree),
                &principal,
                current_time_ms(),
            )
            .map_err(|error| ForkWorkspaceAdmissionError {
                detail: error.to_string(),
            })?;
        Ok(Arc::new(ActorWorkspaceCustody {
            runtime: self.runtime.clone(),
            bindings: self.bindings.clone(),
            binding: Mutex::new(Some(binding)),
            actor,
            state: Arc::new(Mutex::new(scoped_custody::CustodyState::default())),
            workspace,
            inheritance_notice,
        }))
    }
}

impl ForkWorkspaceAdmission for ActorForkWorkspaceAdmission {
    fn install_custody(
        &self,
        actor: ActorRef,
        worktree: &str,
        role: exomonad_actor::ActorRole,
    ) -> Result<Arc<dyn exomonad_actor::ForkWorkspaceCustody>, ForkWorkspaceAdmissionError> {
        let custody = self.bind_workspace(actor, worktree, None, None)?;
        // Custody alone lets the actor inspect its tree; the grant is what
        // lets a coding-role holder merge into it. Interactive actors are
        // granted again at `PolicyInstalled` with the same value.
        self.authority
            .install_grant(actor.into(), worktree_grant(role));
        Ok(custody)
    }

    fn admit(
        &self,
        owner: ActorRef,
        actor_path: String,
        seed: ForkWorkspaceSeed,
        policy: exomonad_actor::ForkWorkspacePolicy,
    ) -> exomonad_actor::ForkWorkspaceAdmissionFuture<'_> {
        let worktrees = self.worktrees.clone();
        let custody = self.clone();
        Box::pin(async move {
            let authorized = tidepool_runtime::spawn_blocking_in_span(move || {
                let (spec, dirty_policy) = match seed {
                    ForkWorkspaceSeed::Explicit(spec) => {
                        let dirty_policy = spec.spec_dirty_policy;
                        (Some(spec), dirty_policy)
                    }
                    ForkWorkspaceSeed::CurrentCheckout(dirty_policy) => (None, dirty_policy),
                };
                worktrees
                    .lock()
                    .authorize_fork_workspace(owner.into(), actor_path, spec, dirty_policy)
                    // The worktree's own sentence, not a struct dump: "source
                    // repository is dirty: … commit or stash first, or call
                    // allowDirtySnapshot" is exactly what the forking actor
                    // needs, and Debug throws the remedy away.
                    .map_err(|error| ForkWorkspaceAdmissionError {
                        detail: tidepool_handlers::render_worktree_error(&error),
                    })
            })
            .await
            .map_err(|error| ForkWorkspaceAdmissionError {
                detail: format!("workspace preparation task failed: {error}"),
            })??;
            let (handle, workspace, notice) = match &custody.native {
                Some(native) if native.layout.is_some() => {
                    let prepared = native
                        .prepare_workspace(owner, authorized, policy)
                        .await
                        .map_err(|error| ForkWorkspaceAdmissionError {
                            detail: error.to_string(),
                        })?;
                    (prepared.handle, Some(prepared.workspace), prepared.notice)
                }
                _ => {
                    let handle =
                        tidepool_runtime::spawn_blocking_in_span(move || authorized.materialize())
                            .await
                            .map_err(|error| ForkWorkspaceAdmissionError {
                                detail: format!("workspace preparation task failed: {error}"),
                            })?
                            .map_err(|error| ForkWorkspaceAdmissionError {
                                detail: tidepool_handlers::render_worktree_error(&error),
                            })?;
                    (handle, None, None)
                }
            };
            let worktree = handle.handle_receipt.tree_id.raw.clone();
            Ok(exomonad_actor::PreparedForkWorkspace::new(
                handle,
                move |actor| custody.bind_workspace(actor, &worktree, workspace, notice),
            ))
        })
    }
}

fn fork_workspace_admission(
    worktrees: WorktreeManager,
    authority: ActorWorktreeAuthority,
    bindings: Arc<Mutex<BindingTable>>,
    runtime: String,
    native: Option<NativeForkAdmission>,
) -> Arc<ActorForkWorkspaceAdmission> {
    Arc::new(ActorForkWorkspaceAdmission {
        bindings,
        runtime,
        native,
        manager: worktrees.clone(),
        authority: authority.clone(),
        worktrees: Arc::new(Mutex::new(ActorWorktreeHandler::new(
            WorktreeHandler::from_manager(worktrees),
            authority,
        ))),
    })
}

#[derive(Clone)]
pub struct ActorHostConfig {
    pub systemd_slice: Option<exomonad_node::systemd_slice::SystemdSlice>,
    pub source_exclude: Vec<String>,
    pub command_resources: Option<Arc<exomonad_node::command_resources::CommandResourceClient>>,
    /// This Exomonad installation provides the internal namespace-entry executable.
    pub exomonad_executable: PathBuf,
    pub workspace_inputs: Option<crate::exomonad::workspace::FrozenWorkspace>,
    pub workspace: PathBuf,
    pub haskell_root: PathBuf,
    pub run_root: PathBuf,
    pub root_binding_path: PathBuf,
    pub interactive_agent: InteractiveAgentInstallation,
    pub tmux_session: String,
    pub model: String,
    pub effort: ReasoningEffort,
    pub research_policy: exomonad_actor::ResearchPolicy,
    pub root_launch_mode: InteractiveLaunchMode,
    pub pane_environment: std::collections::BTreeMap<String, String>,
    /// Answers actors' `Jev` requests; `None` uses the TypeSafe client with
    /// the key from `TYPESAFE_API_KEY` or the secrets directory.
    pub jev: Option<exomonad_actor::JevBackendHandle>,
}

const PROCESS_RECOVERY_RECORD: &str = "process-recovery.json";

fn hosted_operation_journal(run_root: &Path, actor: exomonad_actor::ActorId) -> PathBuf {
    run_root
        .join("hosted-operations")
        .join(format!("{}.v1.jsonl", actor.0))
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ProcessRecoveryRecord {
    version: u32,
    launch_id: String,
    recovery_secret: String,
    supervisor_socket: PathBuf,
    socket_root: PathBuf,
    retired: bool,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ProcessRecoveryCheckpoint {
    version: u32,
    launch_id: String,
    observation: exomonad_node::ProcessSupervisorObservation,
    operation_pending: bool,
    error: Option<String>,
}

pub(crate) struct PredecessorRecovery {
    pub(crate) stopped: usize,
    pub(crate) unavailable: Vec<String>,
}

impl PredecessorRecovery {
    pub(crate) fn root_available(&self) -> bool {
        !self.unavailable.iter().any(|actor| actor.starts_with("1-"))
    }
}

/// Stop each predecessor whose exact supervisor identity remains provable.
/// Unverifiable children remain unavailable without preventing independent
/// actors from recovering. Composition separately rejects an unverifiable root.
pub(crate) fn stop_predecessor_processes(
    run_root: &Path,
) -> Result<PredecessorRecovery, std::io::Error> {
    let mut report = PredecessorRecovery {
        stopped: 0,
        unavailable: Vec::new(),
    };
    for actor in std::fs::read_dir(run_root)? {
        let actor = actor?;
        if !actor.file_type()?.is_dir() {
            continue;
        }
        let actor_name = actor.file_name().to_string_lossy().into_owned();
        let mut components = actor_name.split('-');
        let actor_directory = components
            .next()
            .is_some_and(|part| part.parse::<u64>().is_ok())
            && components
                .next()
                .is_some_and(|part| part.parse::<u64>().is_ok())
            && components.next().is_none();
        if !actor_directory {
            continue;
        }
        let record_path = actor.path().join(PROCESS_RECOVERY_RECORD);
        let bytes = match std::fs::read(&record_path) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                report.unavailable.push(actor_name);
                continue;
            }
            Err(error) => return Err(error),
        };
        let mut record: ProcessRecoveryRecord = match serde_json::from_slice(&bytes) {
            Ok(record) => record,
            Err(error) => {
                tracing::warn!(actor = %actor_name, %error, "corrupt predecessor evidence");
                report.unavailable.push(actor_name);
                continue;
            }
        };
        if record.version != 1 {
            report.unavailable.push(actor_name);
            continue;
        }
        if record.retired {
            continue;
        }
        let terminal = (|| -> Result<_, std::io::Error> {
            if record.supervisor_socket.exists() {
                let (mut recovery, _) = exomonad_node::ProcessSupervisorRecovery::recover(
                    record.supervisor_socket.clone(),
                    record.launch_id.clone(),
                    record.recovery_secret.clone(),
                    PROCESS_OPERATION_TIMEOUT,
                )
                .map_err(std::io::Error::other)?;
                let observation = recovery
                    .stop(PROCESS_OPERATION_TIMEOUT)
                    .map_err(std::io::Error::other)?;
                recovery
                    .finalize(PROCESS_OPERATION_TIMEOUT)
                    .map_err(std::io::Error::other)?;
                Ok(observation)
            } else {
                let checkpoint_path = record
                    .supervisor_socket
                    .parent()
                    .ok_or_else(|| std::io::Error::other("supervisor socket has no parent"))?
                    .join(exomonad_node::PROCESS_SUPERVISOR_CHECKPOINT);
                let checkpoint: ProcessRecoveryCheckpoint =
                    serde_json::from_slice(&std::fs::read(&checkpoint_path).map_err(|error| {
                        std::io::Error::other(format!(
                            "predecessor process evidence unavailable at {}: {error}",
                            checkpoint_path.display()
                        ))
                    })?)
                    .map_err(std::io::Error::other)?;
                if checkpoint.version != exomonad_node::PROCESS_SUPERVISOR_VERSION
                    || checkpoint.launch_id != record.launch_id
                    || checkpoint.operation_pending
                    || checkpoint.error.is_some()
                {
                    return Err(std::io::Error::other(format!(
                        "predecessor process evidence is unresolved at {}",
                        checkpoint_path.display()
                    )));
                }
                Ok(checkpoint.observation)
            }
        })();
        let terminal = match terminal {
            Ok(terminal) => terminal,
            Err(error) => {
                tracing::warn!(actor = %actor_name, %error, "predecessor actor remains unavailable");
                report.unavailable.push(actor_name);
                continue;
            }
        };
        if !matches!(
            terminal,
            exomonad_node::ProcessSupervisorObservation::ProcessStopped
                | exomonad_node::ProcessSupervisorObservation::NotSpawned
        ) {
            report.unavailable.push(actor_name);
            continue;
        }
        record.retired = true;
        tidepool_atomic_write::write_durable(
            &record_path,
            &serde_json::to_vec_pretty(&record).map_err(std::io::Error::other)?,
        )
        .map_err(std::io::Error::from)?;
        std::fs::remove_dir_all(&record.socket_root)?;
        report.stopped += 1;
        // Process evidence is enough to prove that replacing the root is safe,
        // but it does not reconstruct a child actor's lost Haskell state,
        // lineage, or mailbox. Keep that child visible as unavailable until an
        // actor-owned durable record can restore those identities.
        if !actor_name.starts_with("1-") {
            report.unavailable.push(actor_name);
        }
    }
    Ok(report)
}

fn recovery_role(
    durable: &exomonad_actor::DurableActorAdmission,
    research_policy: exomonad_actor::ResearchPolicy,
) -> Option<exomonad_actor::EffectiveRole> {
    let role = match durable.role.as_str() {
        "research" => exomonad_actor::EffectiveRole::research(),
        "coding" => exomonad_actor::EffectiveRole::coding(),
        "scaffolding" => {
            exomonad_actor::EffectiveRole::scaffolding(exomonad_actor::DescendantBudget {
                maximum_depth: durable.descendant_depth,
                maximum_active_children: durable.descendant_active_children,
            })
        }
        "integration" => exomonad_actor::EffectiveRole::integration(),
        // A second root or an inherited role carries authority that cannot be
        // reconstructed from the compact durable role name alone.
        "root" | "inherited" => return None,
        _ => return None,
    };
    Some(role.with_research_policy(research_policy))
}

fn operator_effective_role(
    research_policy: exomonad_actor::ResearchPolicy,
) -> exomonad_actor::EffectiveRole {
    exomonad_actor::EffectiveRole::root().with_research_policy(research_policy)
}

fn predecessor_process_was_retired(run_root: &Path, actor: ActorRef) -> bool {
    let path = run_root
        .join(format!("{}-{}", actor.id.0, actor.incarnation.0))
        .join(PROCESS_RECOVERY_RECORD);
    std::fs::read(path)
        .ok()
        .and_then(|bytes| serde_json::from_slice::<ProcessRecoveryRecord>(&bytes).ok())
        .is_some_and(|record| record.version == 1 && record.retired)
}

fn next_actor_incarnation(actor: ActorRef) -> Result<ActorRef, Box<dyn std::error::Error>> {
    Ok(ActorRef {
        id: actor.id,
        incarnation: exomonad_actor::Incarnation(
            actor
                .incarnation
                .0
                .checked_add(1)
                .ok_or_else(|| runtime_error("actor incarnation space exhausted"))?,
        ),
    })
}

fn durable_root_identity(
    records: &[exomonad_actor::DurableActorRecord],
    accepted_source: Option<&str>,
) -> Result<Option<(ActorRef, ActorRef)>, Box<dyn std::error::Error>> {
    records
        .iter()
        .filter(|record| {
            record.admission.role == "root"
                && record.admission.creator.is_none()
                && record.admission.supervisor_parent.is_none()
                && record.admission.context_parent.is_none()
        })
        .max_by_key(|record| record.admission.actor.incarnation)
        .filter(|record| {
            record.terminal.is_none()
                && record.application.as_ref().is_some_and(|application| {
                    application.conversation.is_some()
                        && application.accepted_source.as_deref() == accepted_source
                })
        })
        .map(|record| {
            next_actor_incarnation(record.admission.actor)
                .map(|successor| (record.admission.actor, successor))
        })
        .transpose()
}

fn contains_durable_root_admission(records: &[exomonad_actor::DurableActorRecord]) -> bool {
    records.iter().any(|record| {
        record.admission.role == "root"
            && record.admission.creator.is_none()
            && record.admission.supervisor_parent.is_none()
            && record.admission.context_parent.is_none()
    })
}

fn latest_recoverable_actor_records(
    records: &[exomonad_actor::DurableActorRecord],
    root: ActorRef,
) -> Vec<exomonad_actor::DurableActorRecord> {
    let mut latest = BTreeMap::new();
    for record in records
        .iter()
        .filter(|record| record.admission.actor.id != root.id)
    {
        latest
            .entry(record.admission.actor.id)
            .and_modify(|current: &mut &exomonad_actor::DurableActorRecord| {
                if record.admission.actor.incarnation > current.admission.actor.incarnation {
                    *current = record;
                }
            })
            .or_insert(record);
    }
    latest
        .into_values()
        .filter(|record| {
            record.terminal.is_none()
                && record
                    .application
                    .as_ref()
                    .is_some_and(|application| application.conversation.is_some())
        })
        .cloned()
        .collect()
}

#[allow(
    clippy::too_many_arguments,
    reason = "heterogeneous recovery inputs (forest, paths, actor identity, durable records, compiled program, policy, admission); no natural grouping"
)]
async fn recover_prior_actors(
    forest: &Arc<ResidentForest<ExomonadHandlerStack, CapturedOutput>>,
    run_root: &Path,
    root: ActorRef,
    incarnation: exomonad_actor::Incarnation,
    records: &[exomonad_actor::DurableActorRecord],
    program: Arc<tidepool_runtime::session::CompiledTurn>,
    research_policy: exomonad_actor::ResearchPolicy,
    worktree_admission: &ActorForkWorkspaceAdmission,
    accepted_source: Option<&str>,
) -> BTreeMap<ActorRef, (ActorRef, QueueReadyThread)> {
    if incarnation == exomonad_actor::Incarnation::FIRST {
        return BTreeMap::new();
    }
    let mut pending = latest_recoverable_actor_records(records, root);
    let mut recovered = BTreeMap::new();
    let mut recovered_ids = std::collections::BTreeSet::from([root.id]);
    loop {
        let mut progressed = false;
        let mut remaining = Vec::new();
        for record in pending {
            let durable = &record.admission;
            let parents_ready = [
                durable.creator,
                durable.supervisor_parent,
                durable.context_parent,
            ]
            .into_iter()
            .flatten()
            .all(|parent| recovered_ids.contains(&parent.id));
            if !parents_ready {
                remaining.push(record);
                continue;
            }
            let Some(application) = &record.application else {
                continue;
            };
            let Some(expected_conversation) = application.conversation.as_deref() else {
                continue;
            };
            if application.accepted_source.as_deref() != accepted_source {
                tracing::warn!(actor = %durable.actor,
                    recorded_source = ?application.accepted_source,
                    current_source = ?accepted_source,
                    "durable actor accepted-source identity cannot be verified");
                continue;
            }
            let Some(role) = recovery_role(durable, research_policy) else {
                tracing::warn!(actor = %durable.actor, role = %durable.role,
                    "durable actor role cannot be reconstructed");
                continue;
            };
            if durable.launch_worktrees.len() > 1
                || durable.source_layer.iter().any(|path| !path.exists())
                || !predecessor_process_was_retired(run_root, durable.actor)
            {
                tracing::warn!(actor = %durable.actor,
                    "durable actor resources are not independently recoverable");
                continue;
            }
            if let Err(error) = crate::host_dynamic_tools::validate_operation_recovery(
                hosted_operation_journal(run_root, durable.actor.id),
            ) {
                tracing::warn!(actor = %durable.actor, %error,
                    "durable actor operation ownership cannot be verified");
                continue;
            }
            let thread = match read_interactive_binding(&application.binding_path).await {
                Ok(thread) if thread.id().0 == expected_conversation => thread,
                Ok(_) => {
                    tracing::warn!(actor = %durable.actor,
                        "durable actor conversation binding changed identity");
                    continue;
                }
                Err(error) => {
                    tracing::warn!(actor = %durable.actor, %error,
                        "durable actor conversation binding is unavailable");
                    continue;
                }
            };
            let successor = match next_actor_incarnation(durable.actor) {
                Ok(successor) => successor,
                Err(error) => {
                    tracing::warn!(actor = %durable.actor, %error,
                        "durable actor incarnation cannot advance");
                    continue;
                }
            };
            let custody = match durable.launch_worktrees.as_slice() {
                [] => None,
                [worktree] => match worktree_admission.recover_workspace(
                    durable.actor,
                    successor,
                    worktree,
                    role.role(),
                ) {
                    Ok(custody) => Some(custody),
                    Err(error) => {
                        tracing::warn!(actor = %durable.actor, detail = %error.detail,
                            "durable actor worktree custody could not be recovered");
                        continue;
                    }
                },
                _ => {
                    tracing::warn!(actor = %durable.actor,
                        "durable actor names more than one launch worktree");
                    continue;
                }
            };
            let (actor, task) = match forest
                .recover_durable_program_root(durable, role, program.clone(), custody)
                .await
            {
                Ok(actor) => actor,
                Err(error) => {
                    tracing::warn!(actor = %durable.actor, %error,
                        "durable actor program could not be reconstructed");
                    continue;
                }
            };
            drop(task);
            let binding = run_root
                .join(format!(
                    "{}-{}",
                    actor.identity().id.0,
                    actor.identity().incarnation.0
                ))
                .join("binding.json");
            if let Err(error) = copy_interactive_binding(&binding, &thread).await {
                tracing::warn!(actor = %durable.actor, %error,
                    "recovered actor binding could not be republished");
                if let Err(shutdown_error) = actor
                    .shutdown(ActorTerminal {
                        kind: ActorExitKind::Failed,
                        summary: "recovered conversation binding could not be republished".into(),
                    })
                    .await
                {
                    tracing::warn!(actor = %durable.actor, error = %shutdown_error,
                        "recovered actor could not be shut down after a republish failure");
                }
                continue;
            }
            recovered_ids.insert(actor.identity().id);
            recovered.insert(actor.identity(), (durable.actor, thread));
            progressed = true;
        }
        if remaining.is_empty() || !progressed {
            for record in remaining {
                tracing::warn!(actor = %record.admission.actor,
                    "durable actor lineage has an unavailable predecessor");
            }
            break;
        }
        pending = remaining;
    }
    recovered
}

impl ActorHostConfig {
    /// Whether this run's captured workspace supplies the Jev authoring
    /// surface. Jev is pinned source a project opts into through
    /// `[haskell.flake_sources]`, not part of the Tidepool library, so both
    /// the workbench that offers `J` and the instructions that describe it
    /// read this one answer.
    fn jev_surface(&self) -> prompt_catalog::JevSurface {
        if self
            .workspace_inputs
            .as_ref()
            .is_some_and(|inputs| inputs.provides_module("Jev.Operators"))
        {
            prompt_catalog::JevSurface::Installed
        } else {
            prompt_catalog::JevSurface::Absent
        }
    }
}

/// The TypeSafe client as the forest's `Jev` backend.
struct HostJev(tidepool_handlers::JevClient);

impl exomonad_actor::JevBackend for HostJev {
    fn ask(
        &self,
        request: String,
    ) -> futures_util::future::BoxFuture<'_, Result<String, exomonad_actor::JevCallFailure>> {
        use exomonad_actor::JevCallFailure as Failure;
        use tidepool_handlers::JevFailure;
        Box::pin(async move {
            let body: serde_json::Value = serde_json::from_str(&request)
                .map_err(|error| Failure::Malformed(format!("request is not JSON: {error}")))?;
            match self.0.ask(body).await {
                Ok(response) => Ok(response.to_string()),
                Err(failure) => Err(match failure {
                    JevFailure::Unconfigured => Failure::Unconfigured,
                    JevFailure::CallCap => Failure::CallCap,
                    JevFailure::Transport(detail) => Failure::Transport(detail),
                    JevFailure::Timeout => Failure::Timeout,
                    JevFailure::Http { status, body } => Failure::Http(i64::from(status), body),
                    JevFailure::BodyLimit => Failure::BodyLimit,
                    JevFailure::Malformed(detail) => Failure::Malformed(detail),
                }),
            }
        })
    }
}

fn jev_backend(config: &ActorHostConfig) -> exomonad_actor::JevBackendHandle {
    if let Some(backend) = &config.jev {
        return Arc::clone(backend);
    }
    match tidepool_handlers::JevClient::new(tidepool_handlers::JevConfig::default()) {
        Ok(client) => {
            if !client.configured() {
                tracing::info!("jev: no TYPESAFE_API_KEY; Jev requests answer JevUnconfigured");
            }
            Arc::new(HostJev(client))
        }
        Err(error) => {
            tracing::warn!(%error, "jev client unavailable; Jev requests answer JevUnconfigured");
            exomonad_actor::unconfigured_jev()
        }
    }
}

fn worker_launch_resolver(config: &ActorHostConfig) -> exomonad_actor::WorkerLaunchResolver {
    let config = config.clone();
    let base = FrozenBasePrompt::selected_body(
        config
            .workspace_inputs
            .as_ref()
            .and_then(|inputs| inputs.prompts.get("core"))
            .map(String::as_str),
        config.jev_surface(),
    );
    let fingerprint = blake3::hash(base.as_bytes()).to_hex().to_string();
    Arc::new(move |request| resolve_worker_launch(&config, request, &fingerprint))
}

fn resolve_worker_launch(
    config: &ActorHostConfig,
    request: &exomonad_actor::WorkerLaunchRequest,
    base_fingerprint: &str,
) -> Result<exomonad_actor::WorkerLaunchPreview, String> {
    let mut instructions = developer_instructions_selected(
        &request.role,
        &InteractiveLaunchMode::Fresh,
        config.workspace_inputs.as_ref(),
        request.instructions.as_deref(),
    );
    append_inheritance_authority(&mut instructions);
    let model = match &request.model {
        Some(exomonad_actor::Model::Alias(alias)) => Some(
            config
                .workspace_inputs
                .as_ref()
                .and_then(|workspace| workspace.models.get(alias))
                .cloned()
                .ok_or_else(|| format!("unknown frozen workspace model alias: {alias}"))?,
        ),
        Some(exomonad_actor::Model::Literal(model)) => Some(model.clone()),
        None => None,
    };
    Ok(exomonad_actor::WorkerLaunchPreview {
        model: model.or_else(|| {
            (request.context == exomonad_actor::ForkContext::SelectedContext)
                .then(|| config.model.clone())
        }),
        effort: request.effort.unwrap_or(match config.effort {
            ReasoningEffort::Low => exomonad_actor::ForkEffort::Low,
            ReasoningEffort::Medium => exomonad_actor::ForkEffort::Medium,
            ReasoningEffort::High => exomonad_actor::ForkEffort::High,
        }),
        instructions,
        base_fingerprint: base_fingerprint.into(),
        workspace_identity: config
            .workspace_inputs
            .as_ref()
            .map(|inputs| inputs.identity().into()),
        modules: config
            .workspace_inputs
            .as_ref()
            .map(|inputs| inputs.import_modules().map(str::to_owned).collect())
            .unwrap_or_default(),
    })
}

fn append_inheritance_authority(instructions: &mut String) {
    instructions.push_str("\nInherited parent bindings do not grant parent authority; the runtime policy above governs this actor.\n");
}

fn launch_effort(
    mode: &InteractiveLaunchMode,
    default: ReasoningEffort,
    requested: Option<exomonad_actor::ForkEffort>,
) -> ReasoningEffort {
    match requested {
        Some(exomonad_actor::ForkEffort::Low) => ReasoningEffort::Low,
        Some(exomonad_actor::ForkEffort::Medium) => ReasoningEffort::Medium,
        Some(exomonad_actor::ForkEffort::High) => ReasoningEffort::High,
        None if matches!(mode, InteractiveLaunchMode::Fork { .. }) => ReasoningEffort::Low,
        None => default,
    }
}

#[derive(Debug, Clone)]
pub enum ActorHostReadiness {
    /// Coordination failed; the native application may still be alive.
    CoordinationFailed { root: ActorRef, error: String },
    /// The root pane is selected, but its queue-ready session binding has not
    /// yet been published.
    AwaitingBinding { root: ActorRef },
    /// The v2 host-tools session callback proved that the exact surrounding
    /// thread is durably addressable by native lifecycle commands.
    Ready {
        root: ActorRef,
        thread: QueueReadyThread,
    },
    /// One predecessor conversation was independently verified and its
    /// replacement native application was admitted for the same logical actor.
    ActorRecovered {
        predecessor: ActorRef,
        actor: ActorRef,
    },
    /// Durable evidence was insufficient to reconstruct one actor. This is
    /// reported even when the actor had no native-process directory.
    ActorUnavailable {
        predecessor: ActorRef,
        reason: String,
    },
}

struct InteractiveDeployment {
    /// Retain the view independently of the bootstrap and native process lifetimes.
    active_workspace: Arc<ActiveWorkspace>,
    supervisor: Option<ActorRef>,
    notified_provider_failures: std::collections::BTreeSet<(String, String)>,
    actor: ActorRef,
    local_actor: LocalActorRef,
    pane: TmuxPaneId,
    workspace: PathBuf,
    inbox: Arc<ActorInbox>,
    notification_inbox_key: String,
    /// Exact durable producer scope used for native input deduplication. This
    /// binds the run/inbox owner and actor incarnation; it is not a display ID.
    input_producer: InputProducerId,
    update_reconciliations: Arc<Mutex<BTreeMap<String, PendingUpdateReconciliation>>>,
    connection: InteractiveConnection,
    service: hosted_retirement::HostedOwner,
    socket_directory: SocketDirectory,
    process_recovery_record: PathBuf,
    worktree_custody: Option<Arc<dyn exomonad_actor::ForkWorkspaceCustody>>,
    failure_reported: bool,
    last_activation_sequence: u64,
    thread: Option<QueueReadyThread>,
    fork_gate: Option<exomonad_actor::ForkGroupGate>,
    runtime_observation: exomonad_actor::ActorRuntimeObservationHandle,
    fork_parent_thread: Option<BackendThreadId>,
}

#[derive(Clone)]
struct PendingUpdateReconciliation {
    inbox: Arc<ActorInbox>,
    sequence: u64,
    context: DeliveryProvenance,
    reconciler: exomonad_actor::RequestUpdateReconciler,
}

impl PendingUpdateReconciliation {
    fn retain_unconfirmed(&self, detail: String) {
        let exact_receipt = matches!(
            self.inbox.observe_receipt(self.sequence),
            Ok(exomonad_node::ReceiptLookup::Retained(ref evidence))
                if evidence.context == self.context
        );
        if !exact_receipt {
            tracing::warn!(
                sequence = self.sequence,
                "request-update reconciliation retained without matching inbox receipt"
            );
            return;
        }
        if let Err(error) = self
            .reconciler
            .reconcile(exomonad_actor::LateUpdateEvidence::Unconfirmed(detail))
        {
            tracing::warn!(
                sequence = self.sequence,
                %error,
                "request-update reconciliation could not retain unconfirmed evidence"
            );
        }
    }
}

enum InteractiveConnection {
    // Pane, inbox, tool listener, and cleanup are already owned in this state.
    AwaitingBinding,
    Bound {
        delivery_shutdown: oneshot::Sender<()>,
        delivery: tokio::task::JoinHandle<()>,
    },
}

struct InteractiveBindingRequest {
    control: crate::host_dynamic_tools::HostToolControl,
    path: PathBuf,
    expected: Option<BackendThreadId>,
}

struct LaunchedInteractiveApplication {
    deployment: InteractiveDeployment,
    binding: InteractiveBindingRequest,
}

struct OwnerNotification {
    owner: ActorRef,
    inbox: Arc<ActorInbox>,
    event: DurableActorEvent,
}

type ActorInbox = DurableInbox<DurableActorEvent, DeliveryProvenance>;
type WatchRetentionCheck = Arc<dyn Fn(ActorRef, exomonad_actor::WatchId) -> bool + Send + Sync>;
/// Whether the owner has already observed a watch (via `ObserveWatchWith` or
/// `pollWatch`) settled at or after the given `occurred_at_unix_ms`. Backed
/// by `ResidentForest::watch_observed_since`.
type WatchObservationCheck =
    Arc<dyn Fn(ActorRef, exomonad_actor::WatchId, u64) -> bool + Send + Sync>;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
enum DeliveryProvenance {
    Notification {
        sender: ActorRef,
        target: ActorRef,
    },
    RequestUpdate {
        owner: ActorRef,
        target: ActorRef,
        request: exomonad_actor::RequestId,
        update: u64,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
enum DurableActorEvent {
    Typed(TypedActorEvent),
    Text(String),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "camelCase")]
enum TypedActorEvent {
    ProviderTurnFailed {
        #[serde(default)]
        revision: u64,
        actor: ActorRef,
        thread: String,
        turn: String,
        failure: exomonad_agent::ProviderFailure,
    },
    SessionReady {
        sequence: u64,
        request: exomonad_actor::RequestId,
        input_type: String,
        message: String,
    },
    WatchChanged {
        #[serde(flatten)]
        notification: exomonad_actor::WatchNotification,
    },
    SettlementChanged {
        #[serde(flatten)]
        notification: exomonad_actor::SettlementNotification,
    },
    RequestCancellation {
        #[serde(flatten)]
        notification: exomonad_actor::RequestCancellationNotification,
    },
    RequestUpdate {
        request: exomonad_actor::RequestId,
        update: u64,
        message: String,
    },
    CleanupFinished {
        receipt: InteractiveCleanupReceipt,
    },
    ChildExited,
}

impl DurableActorEvent {
    fn session(activation: &exomonad_actor::ResidentActivation) -> Self {
        Self::Typed(TypedActorEvent::SessionReady {
            sequence: activation.id.sequence(),
            request: activation.request,
            input_type: activation.input_type.clone(),
            message: activation.message.clone(),
        })
    }

    /// `reader_launched_at` is the launch time of the actor this text is being
    /// delivered to, not of whatever actor the event is about. The label has to
    /// say so: it read "since actor launch" beside a child's settlement, and a
    /// live parent was told its child had taken five and a half minutes when the
    /// child had lived seventy seconds. The figure was the parent's own age.
    fn render(&self, reader_launched_at: Option<i64>) -> String {
        let elapsed = |occurred: u64| match reader_launched_at
            .and_then(|start| i64::try_from(occurred).ok()?.checked_sub(start))
            .filter(|elapsed| *elapsed >= 0)
        {
            Some(ms) => format!(
                "+{}m{:02}s into your session",
                ms / 60_000,
                (ms / 1000) % 60
            ),
            None => "elapsed time unavailable".to_owned(),
        };
        match self {
            Self::Typed(TypedActorEvent::ProviderTurnFailed { actor, thread, turn, failure, .. }) => format!(
                "actor {}@{} provider turn {turn:?} in thread {thread:?} failed: {failure:?}. The actor request remains pending. Inspect status before deciding whether to retire or recover it; further steering does not repair rejected history.",
                actor.id.0, actor.incarnation.0,
            ),
            Self::Typed(TypedActorEvent::SessionReady { message, .. }) => message.clone(),
            Self::Text(message) => message.clone(),
            // A watch names no single child: `register_watch_groups` can
            // fold several requests' targets under one watch, and a `Ready`
            // transition carries no request (or target) at all, so there is
            // no child identity to render here even when a settlement on
            // the same request would have one.
            Self::Typed(TypedActorEvent::WatchChanged { notification })
                if matches!(notification.transition, exomonad_actor::WatchTransition::RouteFailed { .. }) => format!(
                "route {} failed: {:?} ({}). Recover owned handles with `listRoutes`, then inspect with `pollRoute`. Earlier effects may have completed; do not replay the callback blindly.",
                notification.watch.0,
                notification.current,
                elapsed(notification.occurred_at_unix_ms),
            ),
            Self::Typed(TypedActorEvent::WatchChanged { notification }) => format!(
                "watch {} {:?}: {:?} → {:?} ({}). This records the transition when it was observed; cleanup may since have forgotten the watch.",
                notification.watch.0,
                notification.label,
                notification.previous,
                notification.current,
                elapsed(notification.occurred_at_unix_ms),
            ),
            Self::Typed(TypedActorEvent::SettlementChanged { notification }) => {
                let identity = settlement_identity_line(notification);
                match &notification.reply_preview {
                    Some(preview) => format!(
                        "{identity}request {} {:?} settled {:?} ({}).\nReply:\n{preview}\n\nRead the full value with `pollResponse` only if you need more than this preview; settlement is not integration.",
                        notification.request.0,
                        notification.label,
                        notification.transition,
                        elapsed(notification.occurred_at_unix_ms),
                    ),
                    None => format!(
                        "{identity}request {} {:?} settled {:?} ({}). Inspect its retained `Response` with `pollResponse`; settlement is not integration.",
                        notification.request.0,
                        notification.label,
                        notification.transition,
                        elapsed(notification.occurred_at_unix_ms),
                    ),
                }
            }
            Self::Typed(TypedActorEvent::RequestCancellation { notification }) => format!(
                "request {} {:?} has cancellation pending ({:?}; {}). Inspect `sessionReply` with `pollReply`; acknowledge it with `acknowledgeCancellation sessionReply` when the active work is safely quiescent.",
                notification.request.0,
                notification.label,
                notification.reason,
                elapsed(notification.occurred_at_unix_ms),
            ),
            Self::Typed(TypedActorEvent::RequestUpdate { message, .. }) => message.clone(),
            Self::Typed(TypedActorEvent::CleanupFinished { receipt }) => receipt.render(),
            Self::Typed(TypedActorEvent::ChildExited) => CHILD_LIFECYCLE_NOTICE.into(),
        }
    }
}

/// The settled child's identity, as a first line ahead of the settlement
/// body: its full `exomonad/<path>` actor path (the same string the tmux
/// window shows), and the exact commit it was seeded from when it was
/// launched from a fork workspace. Either half may be absent (an unforked
/// target has no path; a target not launched from a fork workspace has no
/// recorded revision), and the line is empty when there is nothing to say.
///
/// The engine has no notion of a re-fork attempt of the same label — a
/// relaunch under the same path is a distinct actor identity, not a
/// numbered retry of this one — so no attempt number is rendered here.
fn settlement_identity_line(notification: &exomonad_actor::SettlementNotification) -> String {
    match (&notification.target_path, &notification.target_revision) {
        (Some(path), Some(revision)) => format!("{path} (seeded from {revision})\n"),
        (Some(path), None) => format!("{path}\n"),
        (None, _) => String::new(),
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
enum CleanupComponent {
    Process,
    Pane,
    ToolService,
    Delivery,
    Socket,
    WorktreeBinding,
    BuildResource,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "camelCase")]
enum CleanupComponentOutcome {
    Completed,
    Forced,
    Failed { detail: String },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct CleanupComponentReceipt {
    component: CleanupComponent,
    outcome: CleanupComponentOutcome,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct InteractiveCleanupReceipt {
    actor: ActorRef,
    components: Vec<CleanupComponentReceipt>,
}

impl InteractiveCleanupReceipt {
    fn degraded(&self) -> bool {
        self.components
            .iter()
            .any(|component| !matches!(component.outcome, CleanupComponentOutcome::Completed))
    }

    /// Components that did not settle, each named with its reason.
    fn retained(&self) -> Vec<String> {
        self.components
            .iter()
            .filter_map(|component| match &component.outcome {
                CleanupComponentOutcome::Failed { detail } => {
                    Some(format!("{:?}: {detail}", component.component))
                }
                CleanupComponentOutcome::Forced => Some(format!(
                    "{:?}: forcibly stopped before graceful settlement",
                    component.component
                )),
                CleanupComponentOutcome::Completed => None,
            })
            .collect()
    }

    /// The answer a waiting supervisor receives for `stopAgent`/cleanup.
    fn release(&self) -> exomonad_actor::ResourceRelease {
        let retained = self.retained();
        if retained.is_empty() {
            exomonad_actor::ResourceRelease::Released
        } else {
            exomonad_actor::ResourceRelease::Retained(retained.join("; "))
        }
    }

    fn render(&self) -> String {
        let actor = format!("{}@{}", self.actor.id.0, self.actor.incarnation.0);
        let retained = self.retained();
        if retained.is_empty() {
            format!("Actor {actor} is stopped and its resources are released.")
        } else {
            format!(
                "Actor {actor} is stopped. Resources still retained: {}. Nothing is deleted: worktrees, branches and commits stay available. The host and sibling actors are unaffected.",
                retained.join("; ")
            )
        }
    }
}

struct InteractiveApplicationOwner {
    supervisor: Option<ActorRef>,
    creator_workspace: Option<BoundWorkspace>,
    cancel: Option<oneshot::Sender<NativeRetirement>>,
    native_retirement: NativeRetirement,
    pane: Arc<Mutex<Option<TmuxPaneId>>>,
    fork_gate: Option<exomonad_actor::ForkGroupGate>,
    custody: Option<Arc<dyn exomonad_actor::ForkWorkspaceCustody>>,
    scoped_retention: Option<scoped_custody::ScopedHostRetention>,
    hosted: hosted_retirement::HostedSlot,
    launch: HostLaunchState,
    pending_activations: Vec<exomonad_actor::ResidentActivation>,
    terminal: Option<ActorTerminal>,
    retirement: Arc<Mutex<Option<InteractiveCleanupReceipt>>>,
}

/// Coordination failure does not authorize terminating the native conversation.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
enum NativeRetirement {
    #[default]
    Preserve,
    Terminate,
}

#[derive(Clone)]
enum HostLaunchState {
    Pending,
    Published,
    Abandoned,
    Failed(String),
}

impl HostLaunchState {
    /// Phrase describing why the host cannot yet (or ever) hand off a
    /// published application for this actor, for surfacing to a caller whose
    /// delivery landed on an admitted actor with no provider running.
    fn provider_not_started_phase(&self) -> String {
        match self {
            HostLaunchState::Pending => "launching".to_string(),
            HostLaunchState::Failed(reason) => format!("launch failed: {reason}"),
            HostLaunchState::Abandoned => "launch abandoned".to_string(),
            HostLaunchState::Published => "provider retired".to_string(),
        }
    }
}

type InteractiveOwners = Arc<Mutex<HashMap<ActorRef, InteractiveApplicationOwner>>>;

#[derive(Clone)]
struct BoundWorkspace {
    workspace: Arc<ActiveWorkspace>,
    thread: QueueReadyThread,
}

#[derive(Clone)]
struct NativeForkAdmission {
    owners: InteractiveOwners,
    backend: Arc<dyn InteractiveAgentBackend>,
    layout: Option<WorkspaceLayout>,
}

fn native_tool_policy(
    native_tools: exomonad_actor::NativeToolClass,
) -> InteractiveNativeToolPolicy {
    match native_tools {
        exomonad_actor::NativeToolClass::InspectionOnly => {
            InteractiveNativeToolPolicy::InspectionOnly
        }
        exomonad_actor::NativeToolClass::Coding
        | exomonad_actor::NativeToolClass::Integration
        | exomonad_actor::NativeToolClass::Inherited => InteractiveNativeToolPolicy::Standard,
    }
}

/// Read an actor's OWN conversation, or report that it has none.
///
/// The actor identity arrives from the executing turn, so the lookup can only
/// find the caller's own binding. An actor with no bound application — an
/// operator proxy, a context whose application has not started, a retired one
/// — is `Unbound`; no other actor's conversation, the root's included, stands
/// in for it.
fn conversation_reader(
    owners: InteractiveOwners,
    backend: Arc<dyn InteractiveAgentBackend>,
) -> exomonad_actor::ConversationReader {
    Arc::new(move |actor, count| {
        let bound = owners
            .lock()
            .get(&actor)
            .filter(|owner| owner.terminal.is_none())
            .and_then(|owner| owner.creator_workspace.as_ref())
            .map(|workspace| workspace.thread.clone());
        let backend = Arc::clone(&backend);
        Box::pin(async move {
            let Some(thread) = bound else {
                return Err(exomonad_actor::ConversationUnavailable::Unbound);
            };
            match backend.conversation(&thread, count).await {
                Ok(Some(turns)) => Ok(turns),
                Ok(None) => Err(exomonad_actor::ConversationUnavailable::Unreadable(
                    "this conversation keeps no readable durable record".into(),
                )),
                Err(error) => Err(exomonad_actor::ConversationUnavailable::Unreadable(
                    error.to_string(),
                )),
            }
        })
    })
}

impl NativeForkAdmission {
    async fn build_snapshot(
        &self,
        creator: ActorRef,
        native_tools: exomonad_actor::NativeToolClass,
    ) -> Option<OverlaySnapshot> {
        if native_tool_policy(native_tools) == InteractiveNativeToolPolicy::InspectionOnly {
            return None;
        }
        self.owners
            .lock()
            .get(&creator)
            .filter(|owner| owner.terminal.is_none())
            .and_then(|owner| owner.creator_workspace.as_ref())
            .and_then(|owner| owner.workspace.build.as_ref())
            .and_then(SharedOverlayResource::latest_snapshot)
    }
}

impl InteractiveApplicationOwner {
    fn reserve_scope(
        &mut self,
        workspace: ActorWorkspaceRequest<'_>,
        actor: ActorRef,
    ) -> Result<Arc<Mutex<scoped_custody::ScopedProcessSlot>>, scoped_custody::ScopedClaimError>
    {
        if self.scoped_retention.is_some() {
            return Err(scoped_custody::ScopedClaimError::AlreadyClaimed);
        }
        let retention = match (workspace, self.custody.as_ref()) {
            (_, Some(custody)) => scoped_custody::reserve(custody.clone(), actor)?,
            (ActorWorkspaceRequest::SourceCheckout, None) => {
                scoped_custody::reserve_source_checkout()
            }
            (ActorWorkspaceRequest::Worktree(_), None) => {
                return Err(scoped_custody::ScopedClaimError::MissingLease);
            }
        };
        let slot = retention.slot.clone();
        self.scoped_retention = Some(retention);
        Ok(slot)
    }

    fn cancel(&mut self) {
        self.creator_workspace = None;
        if let Some(gate) = &self.fork_gate {
            // best-effort: the fork group may already be resolved (ready or
            // failed) by a concurrent path; there is nothing more to do here.
            gate.mark_failed().ok();
        }
        if let Some(cancel) = self.cancel.take() {
            // best-effort: the receiver may already have been dropped if the
            // cancellation race resolved on the other side first.
            cancel.send(self.native_retirement).ok();
        }
    }

    fn retired(&mut self, terminal: ActorTerminal) {
        self.native_retirement = match terminal.kind {
            ActorExitKind::Failed => NativeRetirement::Preserve,
            ActorExitKind::Completed | ActorExitKind::Cancelled => NativeRetirement::Terminate,
        };
        if let Some(custody) = &self.custody {
            custody.actor_stopped(&terminal);
        }
        self.terminal.get_or_insert(terminal);
        self.cancel();
    }
}

/// Returned to run's real host caller, not formatted into a resource-free error.
/// The host retains this error through shutdown. Dropping it at process exit is
/// not settlement, nor continuity of process handles across host death.
pub(crate) struct RetainedInteractiveFleet {
    owners: InteractiveOwners,
    unfinished: Option<tokio::task::JoinHandle<Result<(), String>>>,
    failures: Vec<Box<dyn std::error::Error>>,
}
impl fmt::Debug for RetainedInteractiveFleet {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("RetainedInteractiveFleet")
            .field("actors", &self.owners.lock().keys().collect::<Vec<_>>())
            .field("unfinished", &self.unfinished.is_some())
            .field("failures", &self.failures)
            .finish()
    }
}
impl fmt::Display for RetainedInteractiveFleet {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for error in &self.failures {
            write!(f, "{error}; ")?;
        }
        write!(f, "addressable actor resources retained: host work/process settlement remains unsupported")
    }
}
impl std::error::Error for RetainedInteractiveFleet {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        self.failures.first().map(|error| error.as_ref())
    }
}

#[derive(Debug, thiserror::Error)]
pub(crate) enum RetainedHostedError {
    #[error("exact actor has no retained hosted service")]
    NoHostedActor,
}

/// Host-only operations. These never select a launch mode, release the command
/// gate, establish host-work quiescence, or settle workspace custody.
#[allow(dead_code)] // Available to the crate's host error consumer; not a model API.
pub(crate) enum RetainedProcessOperation {
    Observe,
    #[cfg(test)]
    Pin,
    Stop,
}

#[derive(Debug)]
#[allow(dead_code)]
pub(crate) enum RetainedProcessState {
    Reserved,
    #[cfg(test)]
    Spawning,
    #[cfg(test)]
    NotSpawned(String),
    Blocked,
    Pinned,
    Released,
    ReleaseUnconfirmed,
    Stopping,
    ProcessStopped(Option<exomonad_node::ServiceScopeCleanup>),
}

#[derive(Debug)]
#[allow(dead_code)]
pub(crate) struct RetainedProcessObservation {
    pub(crate) actor: ActorRef,
    pub(crate) actor_terminal: Option<ActorTerminal>,
    pub(crate) process: RetainedProcessState,
}

#[derive(Debug, thiserror::Error)]
pub(crate) enum RetainedProcessError {
    #[error("exact actor has no retained scoped process")]
    NoScopedActor,
    #[error("retained process observation deadline elapsed")]
    Deadline,
    #[error(transparent)]
    Scope(#[from] exomonad_node::ServiceScopeError),
    #[error("retained process supervisor: {0}")]
    Supervisor(String),
}

#[allow(dead_code)]
impl RetainedInteractiveFleet {
    /// Recover the actual resource-bearing error after the host's ordinary
    /// Box<dyn Error> propagation. Crate visibility lets exomonad own a subsequent
    /// recovery policy without exposing this mechanism to authored programs.
    pub(crate) fn from_error<'a>(
        error: &'a mut (dyn std::error::Error + 'static),
    ) -> Option<&'a mut Self> {
        error.downcast_mut::<Self>()
    }

    /// Continue the exact stored seal/shutdown/service operations after waiter
    /// loss. Abort is an explicit host decision; no native-success branch exists.
    pub(crate) async fn recover_hosted(
        &self,
        actor: ActorRef,
        boundary: CompletionBoundary,
        timeout: Duration,
    ) -> Result<HostedObservation, RetainedHostedError> {
        let owner = {
            let rows = self.owners.lock();
            rows.get(&actor)
                .and_then(|row| row.hosted.lock().clone())
                .ok_or(RetainedHostedError::NoHostedActor)?
        };
        Ok(hosted_retirement::observe(&owner, boundary, timeout).await)
    }

    /// Blocking, deadline-bounded host operation: call outside an actor turn.
    /// Exact identity includes incarnation. Even ProcessStopped is status only;
    /// the row remains owned by this carrier after every result or error.
    pub(crate) fn recover_process(
        &self,
        actor: ActorRef,
        operation: RetainedProcessOperation,
        deadline: std::time::Instant,
    ) -> Result<RetainedProcessObservation, RetainedProcessError> {
        if std::time::Instant::now() >= deadline {
            return Err(RetainedProcessError::Deadline);
        }
        let rows = self
            .owners
            .try_lock_until(deadline)
            .ok_or(RetainedProcessError::Deadline)?;
        let slot = rows
            .get(&actor)
            .and_then(|row| row.scoped_retention.as_ref())
            .map(|retention| retention.slot.clone())
            .ok_or(RetainedProcessError::NoScopedActor)?;
        let actor_terminal = rows.get(&actor).and_then(|row| row.terminal.clone());
        drop(rows);
        let map_observation = |observation| match observation {
            scoped_custody::ScopedProcessObservation::Reserved => RetainedProcessState::Reserved,
            scoped_custody::ScopedProcessObservation::Blocked => RetainedProcessState::Blocked,
            scoped_custody::ScopedProcessObservation::Pinned => RetainedProcessState::Pinned,
            scoped_custody::ScopedProcessObservation::Released => RetainedProcessState::Released,
            scoped_custody::ScopedProcessObservation::ReleaseUnconfirmed => {
                RetainedProcessState::ReleaseUnconfirmed
            }
            scoped_custody::ScopedProcessObservation::Stopping => RetainedProcessState::Stopping,
            scoped_custody::ScopedProcessObservation::ProcessStopped => {
                RetainedProcessState::ProcessStopped(None)
            }
        };
        let process = match operation {
            RetainedProcessOperation::Observe => map_observation(
                scoped_custody::observe_slot(&slot, deadline)
                    .map_err(|error| RetainedProcessError::Supervisor(error.to_string()))?,
            ),
            RetainedProcessOperation::Stop => {
                #[cfg(test)]
                {
                    let mut direct = slot
                        .try_lock_until(deadline)
                        .ok_or(RetainedProcessError::Deadline)?;
                    if let scoped_custody::ScopedProcessSlot::Owned(scope) = &mut *direct {
                        let status = scope.terminate_and_wait(deadline)?;
                        RetainedProcessState::ProcessStopped(Some(status))
                    } else {
                        drop(direct);
                        map_observation(
                            scoped_custody::stop_supervisor_slot(&slot, deadline).map_err(
                                |error| RetainedProcessError::Supervisor(error.to_string()),
                            )?,
                        )
                    }
                }
                #[cfg(not(test))]
                {
                    map_observation(
                        scoped_custody::stop_supervisor_slot(&slot, deadline)
                            .map_err(|error| RetainedProcessError::Supervisor(error.to_string()))?,
                    )
                }
            }
            #[cfg(test)]
            RetainedProcessOperation::Pin => {
                let mut slot = slot
                    .try_lock_until(deadline)
                    .ok_or(RetainedProcessError::Deadline)?;
                match &mut *slot {
                    scoped_custody::ScopedProcessSlot::Owned(scope) => {
                        scope.pin_init(deadline)?;
                        RetainedProcessState::Pinned
                    }
                    _ => return Err(exomonad_node::ServiceScopeError::WrongPhase.into()),
                }
            }
        };
        Ok(RetainedProcessObservation {
            actor,
            actor_terminal,
            process,
        })
    }
}

fn handoff_application_owners(
    owners: InteractiveOwners,
    task: tokio::task::JoinHandle<Result<(), String>>,
    cleanup: Result<(), Box<dyn std::error::Error>>,
    run_result: Result<(), Box<dyn std::error::Error>>,
) -> Result<(), Box<dyn std::error::Error>> {
    let retained = owners.lock().values().any(|owner| {
        owner.scoped_retention.is_some() || owner.custody.is_some() || owner.hosted.lock().is_some()
    });
    let unfinished = !task.is_finished();
    if retained || unfinished {
        return Err(Box::new(RetainedInteractiveFleet {
            owners,
            unfinished: unfinished.then_some(task),
            // Preserve the errors themselves: they may own resources too.
            failures: cleanup.err().into_iter().chain(run_result.err()).collect(),
        }));
    }
    cleanup?;
    run_result
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RootRunDisposition {
    Complete,
    Recover,
}

#[derive(Debug, Clone, Copy)]
enum InteractiveOperation {
    BindWorktree,
    PrepareRuntime,
    BindToolHost,
    BuildCommand,
    ServeToolHost,
    LaunchProcess,
    DiscoverBinding,
}

impl fmt::Display for InteractiveOperation {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let name = match self {
            Self::BindWorktree => "bind actor worktree",
            Self::PrepareRuntime => "prepare runtime",
            Self::BindToolHost => "bind tool host",
            Self::BuildCommand => "build agent command",
            Self::ServeToolHost => "serve host dynamic tools",
            Self::LaunchProcess => "launch agent process",
            Self::DiscoverBinding => "discover conversation binding",
        };
        formatter.write_str(name)
    }
}

impl InteractiveOperation {
    fn failure_class(self) -> ExternalApplicationFailureClass {
        match self {
            Self::BindWorktree => ExternalApplicationFailureClass::WorktreeBinding,
            Self::BuildCommand => ExternalApplicationFailureClass::CommandConstruction,
            Self::LaunchProcess => ExternalApplicationFailureClass::ProcessLaunch,
            Self::BindToolHost
            | Self::ServeToolHost
            | Self::DiscoverBinding
            | Self::PrepareRuntime => ExternalApplicationFailureClass::ToolHostStartup,
        }
    }
}

#[derive(Debug, thiserror::Error)]
#[error("actor {actor:?} failed to {operation}: {detail}")]
struct InteractiveApplicationError {
    actor: ActorRef,
    operation: InteractiveOperation,
    detail: String,
}

async fn apply_application_failure(
    actor: LocalActorRef,
    failure: ExternalApplicationFailure,
) -> Result<(), String> {
    let identity = actor.identity();
    tracing::error!(actor = ?identity, class = ?failure.class, detail = %failure.detail,
        "interactive application failed");
    match actor.report_external_failure(failure).await {
        Ok(ExternalFailureDisposition::Applied | ExternalFailureDisposition::AlreadyTerminal) => {
            Ok(())
        }
        Ok(ExternalFailureDisposition::UnknownOrStale) => Err(format!(
            "actor registry rejected exact deployed actor {identity:?} as unknown or stale"
        )),
        Err(error) => Err(format!(
            "actor registry could not record application failure for {identity:?}: {error}"
        )),
    }
}

fn application_error(
    actor: ActorRef,
    operation: InteractiveOperation,
    error: impl fmt::Display,
) -> InteractiveApplicationError {
    InteractiveApplicationError {
        actor,
        operation,
        detail: error.to_string(),
    }
}

struct InteractiveFleet {
    root: LocalActorRef,
    config: ActorHostConfig,
    run_root: PathBuf,
    tmux: TmuxSession,
    backend: Arc<dyn InteractiveAgentBackend>,
    worktrees: WorktreeManager,
    bindings: Arc<Mutex<BindingTable>>,
    /// Readiness events are best-effort notifications: a dropped receiver
    /// means the caller stopped observing startup, not a delivery bug, so
    /// every `readiness.send(..)` below discards the `SendError` with `.ok()`.
    readiness: mpsc::UnboundedSender<ActorHostReadiness>,
    worktree_authority: ActorWorktreeAuthority,
    watch_retention: WatchRetentionCheck,
    watch_observation: WatchObservationCheck,
    /// `None` when the run has no frozen workspace to compare against, in
    /// which case source drift is never observed (see
    /// `run_delivery_pump`'s usage poll).
    source_layers: Option<Arc<crate::exomonad::source::ExomonadSourceReload>>,
    actor_recovery: Arc<exomonad_actor::ActorRecoveryJournal>,
    recovered_threads: Arc<BTreeMap<ActorRef, (ActorRef, QueueReadyThread)>>,
    recovered_root_predecessor: Option<ActorRef>,
}

#[derive(Clone)]
struct InteractiveLaunchContext {
    base_prompt: FrozenBasePrompt,
    root: ActorRef,
    config: ActorHostConfig,
    run_root: PathBuf,
    tmux: TmuxSession,
    backend: Arc<dyn InteractiveAgentBackend>,
    worktrees: WorktreeManager,
    bindings: Arc<Mutex<BindingTable>>,
    actor_recovery: Arc<exomonad_actor::ActorRecoveryJournal>,
    recovered_threads: Arc<BTreeMap<ActorRef, (ActorRef, QueueReadyThread)>>,
}

fn active_source_identity(
    run_root: &Path,
    has_workspace_source: bool,
) -> Result<Option<String>, Box<dyn std::error::Error>> {
    if !has_workspace_source {
        return Ok(None);
    }
    Ok(Some(
        crate::exomonad::source::SourceLayer::new(run_root)
            .read_active()?
            .ok_or("root compilation did not publish its accepted source revision")?
            .identity,
    ))
}

pub(crate) async fn run(
    mut config: ActorHostConfig,
    readiness: mpsc::UnboundedSender<ActorHostReadiness>,
    host_incarnation: HostIncarnationLease,
) -> Result<(), Box<dyn std::error::Error>> {
    let run_root = config.run_root.clone();
    std::fs::create_dir_all(&run_root)?;
    let workspace = config.workspace.clone();
    let (worktrees, bindings) =
        tidepool_runtime::spawn_blocking_in_span(move || actor_worktree_resources(&workspace))
            .await??;
    let bindings = Arc::new(Mutex::new(bindings));
    let worktree_authority =
        ActorWorktreeAuthority::new(runtime_namespace(&run_root), Arc::clone(&bindings));
    let tmux = TmuxSession::new(&config.tmux_session)?;
    if !tmux.exists().await? {
        return Err(runtime_error(format!(
            "Exomonad tmux session {:?} does not exist",
            config.tmux_session
        )));
    }
    let backend = native_interactive_backend(config.interactive_agent.clone());
    let application_owners: InteractiveOwners = Arc::new(Mutex::new(HashMap::new()));
    let source_layers = source_service(&config, &run_root, worktrees.clone());
    let actor_recovery_path = run_root.join("actor-lifecycle.v2.jsonl");
    let actor_recovery = if host_incarnation.incarnation() == exomonad_actor::Incarnation::FIRST {
        exomonad_actor::ActorRecoveryJournal::open(actor_recovery_path)
    } else {
        exomonad_actor::ActorRecoveryJournal::open_existing(actor_recovery_path)
    }?;
    let prior_actor_records = actor_recovery.records();
    let (source, root, program) = compile_root(
        &config,
        &run_root,
        worktrees.clone(),
        worktree_authority.clone(),
        source_layers.as_ref(),
        host_incarnation.incarnation(),
    )?;
    let accepted_source = active_source_identity(&run_root, config.workspace_inputs.is_some())?;
    let (descriptor, machine, outcome) = root.into_parts();
    let worktree_admission = fork_workspace_admission(
        worktrees.clone(),
        worktree_authority.clone(),
        bindings.clone(),
        runtime_namespace(&run_root),
        Some(NativeForkAdmission {
            owners: application_owners.clone(),
            backend: backend.clone(),
            layout: Some(WorkspaceLayout {
                run_namespace: runtime_namespace(&run_root),
                source_root: config.workspace.clone(),
                source_exclude: config.source_exclude.clone(),
                root_imports: Arc::default(),
                worktrees: worktrees.clone(),
                backend: backend.clone(),
                base_prompt: FrozenBasePrompt::materialize_selected(
                    &run_root,
                    config
                        .workspace_inputs
                        .as_ref()
                        .and_then(|inputs| inputs.prompts.get("core"))
                        .map(String::as_str),
                    config.jev_surface(),
                )?,
            }),
        }),
    );
    let (forest, deployments) = ResidentForest::new_with_launch_resolver(
        source,
        descriptor.placement().session,
        machine,
        Some(worktree_admission.clone()),
        host_incarnation.incarnation(),
        Some(worker_launch_resolver(&config)),
    );
    let mut forest = forest
        .with_usage_pointers(exomonad_actor::UsagePointerTable::discover(
            &config.workspace,
        )?)
        .with_recovery_journal(actor_recovery.clone())
        .with_conversation_reader(conversation_reader(
            application_owners.clone(),
            backend.clone(),
        ));
    forest.set_jev_backend(jev_backend(&config));
    if let Some(layers) = &source_layers {
        forest.set_source_layers(layers.clone());
    }
    forest.track_resource_release();
    let forest = Arc::new(forest);
    let recovered_root = durable_root_identity(&prior_actor_records, accepted_source.as_deref())?;
    if contains_durable_root_admission(&prior_actor_records) && recovered_root.is_none() {
        return Err(runtime_error(
            "host recovery cannot adopt a root without complete durable actor, source, and conversation evidence",
        ));
    }
    let recovered_root_predecessor = recovered_root.map(|(predecessor, _)| predecessor);
    if let Some(predecessor) = recovered_root_predecessor {
        crate::host_dynamic_tools::validate_operation_recovery(hosted_operation_journal(
            &run_root,
            predecessor.id,
        ))
        .map_err(|error| {
            runtime_error(format!(
                "root actor {predecessor} cannot be recovered without hosted-operation evidence: {error}"
            ))
        })?;
    }
    forest
        .fence_recovery_identities(
            prior_actor_records
                .iter()
                .filter(|record| record.terminal.is_none())
                .map(|record| record.admission.actor.id)
                .chain(recovered_root.map(|(_, actor)| actor.id)),
        )
        .map_err(runtime_error)?;
    let (mut root_actor, mut root_task) = match recovered_root {
        Some((_, identity)) => {
            forest
                .admit_root_with_identity(descriptor, outcome, identity)
                .await?
        }
        None => forest.admit_root(descriptor, outcome).await?,
    };
    let recovered_threads = Arc::new(
        recover_prior_actors(
            &forest,
            &run_root,
            root_actor.identity(),
            host_incarnation.incarnation(),
            &prior_actor_records,
            program.clone(),
            config.research_policy,
            &worktree_admission,
            accepted_source.as_deref(),
        )
        .await,
    );
    let recovered_predecessors = recovered_threads
        .values()
        .map(|(predecessor, _)| *predecessor)
        .collect::<std::collections::BTreeSet<_>>();
    let unavailable_records = prior_actor_records
        .iter()
        .filter(|record| {
            record.terminal.is_none()
                && record.admission.actor.id != root_actor.identity().id
                && !recovered_predecessors.contains(&record.admission.actor)
        })
        .map(|record| record.admission.actor)
        .collect::<Vec<_>>();
    for predecessor in &unavailable_records {
        readiness
            .send(ActorHostReadiness::ActorUnavailable {
                predecessor: *predecessor,
                reason: "durable actor resources or replayable launch state could not be verified"
                    .into(),
            })
            .ok();
    }
    if host_incarnation.incarnation() != exomonad_actor::Incarnation::FIRST {
        let notice_path = run_root.join("host-recovery-notice.txt");
        let mut notice = std::fs::read_to_string(&notice_path).unwrap_or_default();
        let recovered = recovered_threads
            .iter()
            .map(|(actor, (predecessor, _))| format!("{predecessor}->{actor}"))
            .collect::<Vec<_>>();
        let unavailable = unavailable_records
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>();
        notice.push_str(&format!(
            " Durable actor reconstruction admitted: {}. Durable actors still unavailable: {}.",
            if recovered.is_empty() {
                "none".into()
            } else {
                recovered.join(", ")
            },
            if unavailable.is_empty() {
                "none".into()
            } else {
                unavailable.join(", ")
            },
        ));
        tidepool_atomic_write::write_durable(&notice_path, notice.as_bytes())?;
    }
    worktree_authority.install_grant(root_actor.identity().into(), ActorWorktreeGrant::Repository);
    // The run's own layer belongs to the actor that owns the run, named here,
    // before it runs anything. Every other actor is bound as it is admitted.
    if let Some(layers) = &source_layers {
        layers.bind_run(root_actor.identity().into());
    }
    let provision_forest = forest.clone();
    let provision_authority = worktree_authority.clone();
    let provision_source = source_layers.clone();
    let operator_role = operator_effective_role(config.research_policy);
    let operator_socket = run_root.join("operator").join("operator.sock");
    if operator_socket.exists() {
        std::fs::remove_file(&operator_socket)?;
    }
    let inspection_forest = forest.clone();
    let operator = crate::operator::OperatorService::bind(
        operator_socket.clone(),
        Arc::new(move || {
            let forest = provision_forest.clone();
            let role = operator_role.clone();
            let authority = provision_authority.clone();
            let source = provision_source.clone();
            Box::pin(async move {
                let actor = forest
                    .new_workbench("operator".into(), role)
                    .await
                    .map_err(|e| e.to_string())?;
                authority.install_grant(
                    actor.identity().into(),
                    ActorWorktreeGrant::RepositoryReadOnly,
                );
                // The operator workbench holds the run itself, not a checkout:
                // it reads and republishes the run's own layer.
                if let Some(layers) = &source {
                    layers.bind_run(actor.identity().into());
                }
                Ok(actor)
            })
        }),
        Arc::new(move |requester| inspection_forest.inspect_graph(requester)),
    )
    .await;
    let operator = match operator {
        Ok(operator) => operator,
        Err(error) => {
            forest.shutdown().await;
            return Err(Box::new(error));
        }
    };
    tracing::info!(socket = %operator_socket.display(), "operator control and attachment ready");
    let (shutdown, shutdown_rx) = watch::channel(None);
    let (root_config, root_config_rx) = watch::channel(config.clone());
    let watch_forest = Arc::clone(&forest);
    let watch_retention: WatchRetentionCheck =
        Arc::new(move |owner, watch| watch_forest.retains_watch(owner, watch));
    let watch_observation_forest = Arc::clone(&forest);
    let watch_observation: WatchObservationCheck = Arc::new(move |owner, watch, occurred_at| {
        watch_observation_forest.watch_observed_since(owner, watch, occurred_at)
    });
    let mut applications_task = tokio::spawn(run_interactive_applications(
        deployments,
        application_owners.clone(),
        InteractiveFleet {
            root: root_actor.clone(),
            config: config.clone(),
            run_root: run_root.clone(),
            tmux: tmux.clone(),
            backend,
            worktrees,
            bindings,
            readiness: readiness.clone(),
            worktree_authority: worktree_authority.clone(),
            watch_retention,
            watch_observation,
            source_layers,
            actor_recovery: actor_recovery.clone(),
            recovered_threads,
            recovered_root_predecessor,
        },
        shutdown_rx,
        root_config_rx,
    ));
    let mut recovery = 0_u64;
    let mut applications_finished = false;
    let mut root_active = true;
    let result: Result<(), Box<dyn std::error::Error>> = async {
        loop {
            tokio::select! {
                signal = operator_shutdown() => { signal?; break Ok(()); }
                result = &mut applications_task => {
                    applications_finished = true;
                    break result.map_err(join_error)?.map_err(runtime_error);
                }
                result = &mut root_task, if root_active => {
                    result.map_err(join_error)?;
                    let terminal = root_actor.terminal().get().ok_or_else(|| runtime_error("root stopped without terminal"))?;
                    if terminal.kind == ActorExitKind::Failed {
                        let pane = application_owners.lock().get(&root_actor.identity())
                            .and_then(|owner| owner.pane.lock().clone());
                        if let Err(error) = confirm_native_exit(&tmux, pane.as_ref()).await {
                            readiness.send(ActorHostReadiness::CoordinationFailed {
                                root: root_actor.identity(),
                                error: terminal.summary.clone(),
                            }).ok();
                            if root_never_bound(&config.root_binding_path) {
                                // The root never reached a queue-ready binding in
                                // this run, so there is no live conversation this
                                // host could be retained to preserve. Staying up
                                // only holds the incarnation lease and every
                                // worktree binding lock a fresh run of this
                                // workspace needs — exit instead of lingering.
                                tracing::error!(%error, failure = %terminal.summary, "root coordination stopped before its conversation was ever bound; exiting instead of retaining an unbindable session");
                                break Err(runtime_error(format!(
                                    "Exomonad root failed before its conversation was ever bound and native execution is unconfirmed ({error}): {}",
                                    terminal.summary
                                )));
                            }
                            tracing::error!(%error, failure = %terminal.summary, "root coordination stopped; native execution unconfirmed, retaining session without automatic conversation resume");
                            root_active = false;
                            continue;
                        }
                    }
                    let resident_state = forest.resident_session_state();
                    match prepare_root_recovery(
                        &mut config,
                        root_actor.identity(),
                        terminal,
                        resident_state,
                        &mut recovery,
                    ).await {
                        Ok(RootRunDisposition::Recover) => {}
                        Ok(RootRunDisposition::Complete) => {
                            root_active = false;
                            continue;
                        }
                        Err(error) => {
                            readiness.send(ActorHostReadiness::CoordinationFailed {
                                root: root_actor.identity(),
                                error: error.to_string(),
                            }).ok();
                            if root_never_bound(&config.root_binding_path) {
                                // Same reasoning as the unconfirmed-native-exit
                                // case above: nothing was ever bound in this run,
                                // so there is nothing worth staying up for. Exit
                                // and release every lock a fresh run needs.
                                tracing::error!(%error, "model root recovery unavailable and no conversation was ever bound; exiting instead of retaining an unbindable session");
                                break Err(error);
                            }
                            tracing::error!(%error, "model root recovery unavailable; operator forest remains attached");
                            root_active = false;
                            continue;
                        }
                    }
                    root_config.send_replace(config.clone());
                    (root_actor, root_task) = forest.recover_program_root(root_actor.identity(), "exomonad-root".into(),
                        exomonad_actor::EffectiveRole::root().with_research_policy(config.research_policy), program.clone())
                        .await.map_err(|e| runtime_error(e.to_string()))?;
                    worktree_authority.install_grant(root_actor.identity().into(), ActorWorktreeGrant::Repository);
                }
            }
        }
    }.await;
    shutdown.send_replace(Some(if result.is_ok() {
        NativeRetirement::Terminate
    } else {
        NativeRetirement::Preserve
    }));
    operator.shutdown().await;
    forest.shutdown().await;
    let cleanup = if !applications_finished {
        await_applications(&mut applications_task, APPLICATION_SHUTDOWN_TIMEOUT).await
    } else {
        Ok(())
    };
    handoff_application_owners(application_owners, applications_task, cleanup, result)
}

/// True when this run's root has never reached a queue-ready binding —
/// `root_binding_path` is written the moment the interactive application's
/// session callback certifies queue readiness, so its absence is proof, not
/// inference. A root that fails before that point has no live conversation a
/// lingering host could be retained to preserve, so a coordination failure at
/// that point must end the host rather than hold its locks indefinitely.
fn root_never_bound(root_binding_path: &Path) -> bool {
    !root_binding_path.exists()
}

async fn confirm_native_exit(tmux: &TmuxSession, pane: Option<&TmuxPaneId>) -> Result<(), String> {
    let pane = pane.ok_or("native launch outcome is unknown")?;
    match tmux.pane_status(pane).await {
        Ok(Some(status)) if status.dead => Ok(()),
        Ok(_) => Err("native pane is live or its exit is unconfirmed".into()),
        Err(error) => Err(format!("cannot confirm native pane exit: {error}")),
    }
}

/// Classify an intentional completion or prepare an abnormal root for a fresh
/// incarnation attached to the retained conversation.
async fn prepare_root_recovery(
    config: &mut ActorHostConfig,
    actor: ActorRef,
    terminal: ActorTerminal,
    resident_state: ResidentSessionState,
    recovery: &mut u64,
) -> Result<RootRunDisposition, Box<dyn std::error::Error>> {
    let Some((launch_mode, thread)) =
        root_recovery_launch_mode(&config.root_binding_path, &terminal, resident_state).await?
    else {
        return Ok(RootRunDisposition::Complete);
    };
    *recovery = (*recovery).saturating_add(1);
    tracing::warn!(
        ?actor,
        kind = ?terminal.kind,
        summary = %terminal.summary,
        ?resident_state,
        recovery = *recovery,
        thread = %thread.id().0,
        "Exomonad root stopped abnormally; recreating a fresh root incarnation"
    );
    config.root_launch_mode = launch_mode;
    Ok(RootRunDisposition::Recover)
}

async fn root_recovery_launch_mode(
    binding_path: &Path,
    terminal: &ActorTerminal,
    resident_state: ResidentSessionState,
) -> Result<Option<(InteractiveLaunchMode, QueueReadyThread)>, Box<dyn std::error::Error>> {
    if terminal.kind != ActorExitKind::Failed {
        return Ok(None);
    }
    match resident_state {
        ResidentSessionState::Uninitialized | ResidentSessionState::Reusable => {}
        ResidentSessionState::Running => {
            return Err(runtime_error(
                "Exomonad root failed while its resident session remains running; automatic recovery cannot overtake the admitted operation",
            ));
        }
        ResidentSessionState::Unavailable => {
            return Err(runtime_error(
                "Exomonad root failed with an unavailable resident machine; automatic recovery cannot recreate live values or grants",
            ));
        }
        ResidentSessionState::Gone => {
            return Err(runtime_error(
                "Exomonad root failed after its resident session was retired; automatic recovery requires the original session incarnation",
            ));
        }
    }
    let thread = read_interactive_binding(binding_path)
        .await
        .map_err(|error| {
            runtime_error(format!(
                "Exomonad root {terminal:?} and its conversation cannot be resumed: {error}"
            ))
        })?;
    Ok(Some((
        InteractiveLaunchMode::Resume(thread.id().clone()),
        thread,
    )))
}

async fn await_applications(
    task: &mut tokio::task::JoinHandle<Result<(), String>>,
    timeout: Duration,
) -> Result<(), Box<dyn std::error::Error>> {
    match tokio::time::timeout(timeout, &mut *task).await {
        Ok(result) => result.map_err(join_error)?.map_err(runtime_error),
        Err(_) => {
            // The caller transfers the unfinished task AND its addressable
            // owner map into the host result; timeout is not cancellation proof.
            Err(runtime_error(format!(
                "interactive fleet did not stop within {timeout:?}"
            )))
        }
    }
}

fn actor_worktree_resources(
    workspace: &Path,
) -> Result<(WorktreeManager, BindingTable), exomonad_worktree::WorktreeError> {
    let root = actor_worktree_storage_root(workspace);
    actor_worktree_resources_at(&root, workspace)
}

fn actor_worktree_storage_root(workspace: &Path) -> PathBuf {
    let project = blake3::hash(workspace.as_os_str().as_encoded_bytes())
        .to_hex()
        .to_string();
    tidepool_toolchain::paths::cache_dir()
        .join("exomonad")
        .join("actor-worktrees")
        .join(project)
}

fn actor_worktree_resources_at(
    root: &Path,
    workspace: &Path,
) -> Result<(WorktreeManager, BindingTable), exomonad_worktree::WorktreeError> {
    let git = GitCli::new();
    // The root actor writes its own runtime state (journal, logs) directly
    // into `workspace` — it is not admitted through `prepare()` the way a
    // fork's checkout is, so nothing else on this path installs the
    // exclusion that keeps that state from registering as a dirty source.
    // This is the one place every caller that builds a `WorktreeManager`
    // over a source repository passes through, so it is where the exclusion
    // belongs rather than in each caller (a launcher, a scaffold, a test
    // harness) remembering to call it separately. `ensure_exomonad_local_exclude`
    // is idempotent, so a caller upstream that already installed it (real
    // `exomonad` launches do, via `exomonad.rs`) pays only a no-op write check.
    git.ensure_exomonad_local_exclude(workspace)?;
    let registry = WorktreeRegistry::open(root.join("registry"))?;
    let worktree_root = root.join("worktrees");
    // The root's own allocation directory exists before ANY launch: a mount
    // boundary canonicalizes each writable root it is given, and the root's
    // namespace is fixed at launch, so a directory created later would be
    // unreachable to the process that needs to build in it.
    for directory in [
        &worktree_root,
        &worktree_root.join(WorktreeManager::ROOT_ALLOCATION_DIR),
    ] {
        std::fs::create_dir_all(directory).map_err(|error| {
            exomonad_worktree::WorktreeError::StorageFailure {
                path: directory.clone(),
                detail: error.to_string(),
            }
        })?;
    }
    Ok((
        WorktreeManager::new(git, registry, worktree_root, workspace),
        BindingTable::open_with_timeout(root.join("bindings"), Duration::from_secs(10))?,
    ))
}

/// The run id, as the durable principal and resource namespace for ONE host run.
///
/// `run_root` is `.../exomonad/runs/<run id>`, so its file name IS the run id the
/// host loop was launched with. Every principal spelled with this namespace
/// (see [`exomonad_worktree::AgentRef::exact_actor`]) is therefore unreachable
/// from any other run — which matters because the binding table is durable and
/// per-project while actor identities restart from zero in each run, and a
/// degraded teardown retains its `Active` row rather than manufacturing
/// cleanup evidence.
fn runtime_namespace(run_root: &Path) -> String {
    run_root
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("unknown-runtime")
        .to_owned()
}

fn input_producer_id(
    run_root: &Path,
    actor: ActorRef,
    inbox_key: &str,
) -> Result<InputProducerId, exomonad_agent::interactive::InputEnvelopeError> {
    InputProducerId::new(format!(
        "{}\0{}\0{}\0{}",
        run_root.to_string_lossy(),
        inbox_key,
        actor.id.0,
        actor.incarnation.0
    ))
}

pub(crate) fn exomonad_effect_declarations() -> Vec<tidepool_mcp::EffectDecl> {
    vec![
        tidepool_mcp::agent_session_decl(),
        tidepool_mcp::agent_tools_decl(),
        tidepool_mcp::actor_decl(),
        tidepool_mcp::actor_context_decl(),
        tidepool_mcp::agent_control_decl(),
        tidepool_mcp::notifications_decl(),
        tidepool_mcp::jev_decl(),
        tidepool_mcp::commands_decl(),
        tidepool_mcp::agent_inspection_decl(),
        tidepool_mcp::agent_launch_decl(),
        tidepool_mcp::forks_decl(),
        tidepool_mcp::actor_kernel_decl(),
        tidepool_mcp::actor_local_decl(),
        tidepool_mcp::reflect_decl(),
        tidepool_mcp::source_decl(),
        tidepool_mcp::sleep_decl(),
        tidepool_mcp::fs_read_decl(),
        tidepool_mcp::worktree_decl(),
        tidepool_mcp::bound_worktree_decl(),
        tidepool_mcp::worktree_registry_decl(),
        tidepool_mcp::worktree_allocation_decl(),
        tidepool_mcp::worktree_integration_decl(),
        tidepool_mcp::event_decl(),
        tidepool_mcp::journal_decl(),
    ]
}

struct CompiledExomonadDriver {
    preamble: String,
    include: Vec<PathBuf>,
    compiled: tidepool_runtime::session::CompiledTurn,
}

/// Compile the exact selected driver and imports without launching an actor.
/// Initialization uses this before replacing a live swarm; admission uses the
/// same compiler path and the toolchain owner's content-addressed cache.
pub(crate) fn validate_workspace_program(
    inputs: &crate::exomonad::workspace::FrozenWorkspace,
    run_root: &Path,
) -> Result<(), Box<dyn std::error::Error>> {
    compile_driver(
        &crate::haskell_sources::ensure_exomonad_haskell()?,
        Some(inputs),
        run_root,
        None,
    )?;
    Ok(())
}

/// Compile the driver against a CANDIDATE source revision. This is the whole
/// reload check: GHC's own module graph, rooted at the driver and every
/// configured workspace module, decides whether the candidate's
/// reverse-dependency closure typechecks. Nothing is published unless it does.
///
/// `replaces_run_layer` says which layer is being reloaded. The run's own
/// reload stands in for the run's layer, exactly as publishing would. A
/// checkout's reload goes in FRONT of it instead, because that is where the
/// checkout's layer sits in its own actor's include list, and the run's layer
/// must stay on the path beneath it either way.
pub(crate) fn typecheck_candidate_revision(
    inputs: &crate::exomonad::workspace::FrozenWorkspace,
    run_root: &Path,
    haskell_root: &Path,
    candidate: &[PathBuf],
    replaces_run_layer: bool,
    extra_modules: &[String],
) -> Result<(), Box<dyn std::error::Error>> {
    // A scratch session root, never the live swarm's. The check compiles a
    // driver turn, and a turn writes generated modules under its session root;
    // pointing that at the running session would let a rejected candidate
    // disturb the very graph this check exists to protect.
    let scratch = run_root
        .join("reload-checks")
        .join(uuid::Uuid::new_v4().to_string());
    std::fs::create_dir_all(&scratch)?;
    let checked = compile_driver(
        haskell_root,
        Some(inputs),
        &scratch,
        Some(CandidateSources {
            include: candidate,
            replaces_run_layer,
            extra_modules,
        }),
    );
    // best-effort: cleanup of a scratch check directory; a leftover directory
    // does not affect correctness, only disk usage.
    std::fs::remove_dir_all(&scratch).ok();
    checked?;
    Ok(())
}

/// The run's source service: the run's own layer, one layer per checkout that
/// carries source, and which actor reaches which.
///
/// A run without a frozen workspace has no declared source roots, so there is
/// nothing a reload could honestly read; that case has no service at all and
/// every verb answers `SourceUnavailable`.
pub(crate) fn source_service(
    config: &ActorHostConfig,
    run_root: &Path,
    worktrees: WorktreeManager,
) -> Option<Arc<crate::exomonad::source::ExomonadSourceReload>> {
    let inputs = config.workspace_inputs.as_ref()?;
    Some(Arc::new(
        crate::exomonad::source::ExomonadSourceReload::new(
            inputs.clone(),
            config.workspace.clone(),
            run_root.to_path_buf(),
            config.haskell_root.clone(),
        )
        .with_worktrees(worktrees),
    ))
}

fn source_handler(
    service: Option<&Arc<crate::exomonad::source::ExomonadSourceReload>>,
) -> tidepool_handlers::SourceHandler {
    match service {
        Some(service) => tidepool_handlers::SourceHandler::new(service.clone()),
        None => tidepool_handlers::SourceHandler::unavailable(),
    }
}

/// The candidate source layer a driver compile reads, when it is not the one
/// the run has published.
struct CandidateSources<'a> {
    include: &'a [PathBuf],
    /// Does the candidate stand in for the run's own layer, or sit in front of
    /// it? A run reload replaces it; a checkout reload adds its own layer and
    /// leaves the run's exactly where it is.
    replaces_run_layer: bool,
    extra_modules: &'a [String],
}

/// The preamble and include list read by one driver compile.
struct DriverSources {
    preamble: String,
    include: Vec<PathBuf>,
}

fn driver_sources(
    haskell_root: &Path,
    inputs: Option<&crate::exomonad::workspace::FrozenWorkspace>,
    run_root: &Path,
    candidate: Option<CandidateSources<'_>>,
) -> Result<DriverSources, Box<dyn std::error::Error>> {
    let declarations = exomonad_effect_declarations();
    let effects = tidepool_mcp::ensure_effects_module(&declarations)?;
    let mut include = effects.include_paths().to_vec();
    include.push(haskell_root.to_path_buf());
    include.push(crate::haskell_sources::ensure_embedded_stdlib()?);
    let mut preamble = insert_preamble_imports(
        &tidepool_mcp::build_preamble_with_companions_hiding(
            &declarations,
            false,
            tidepool_mcp::CompanionImports::Omit,
            EXOMONAD_REPLACED_EFFECT_NAMES,
        ),
        DRIVER_MODULE,
    );
    if let Some(inputs) = inputs {
        // The live source layer goes AHEAD of the run's frozen capture, so a
        // reloaded module shadows the copy the run started from. The frozen
        // capture stays on the path beneath it as the verified floor.
        let layer = crate::exomonad::source::SourceLayer::new(run_root);
        let roots = inputs.captured_source_roots().len();
        if let Some(candidate) = &candidate {
            include.extend(candidate.include.iter().cloned());
        }
        if candidate
            .as_ref()
            .is_none_or(|candidate| !candidate.replaces_run_layer)
        {
            layer.ensure_active(inputs)?;
            include.extend(layer.include_paths(roots));
        }
        include.extend(inputs.include.iter().cloned());
        for module in inputs.import_modules() {
            preamble = insert_preamble_imports(&preamble, module);
        }
        for module in candidate.iter().flat_map(|c| c.extra_modules.iter()) {
            preamble = insert_preamble_imports(&preamble, module);
        }
        // What an actor installs at startup is compiled here too, so a broken
        // spec fails `exomonad check` and `exomonad init` instead of the first actor
        // to start. None of these is in `[haskell] modules`: the spec module is
        // found by convention, and the two keys name a value, not an import.
        // They are imported qualified because they only need to typecheck.
        let named = [inputs.spec.as_deref(), inputs.tools.as_deref()]
            .into_iter()
            .flatten()
            .filter_map(|entry| entry.rsplit_once('.').map(|(module, _)| module.to_owned()));
        let conventional = inputs
            .provides_module("AgentSpec")
            .then(|| "AgentSpec".to_owned());
        let installed: std::collections::BTreeSet<String> =
            conventional.into_iter().chain(named).collect();
        for module in installed {
            if inputs.provides_module(&module) {
                preamble = insert_preamble_imports(&preamble, &format!("qualified {module}"));
            }
        }
    }
    Ok(DriverSources { preamble, include })
}

fn compile_driver(
    haskell_root: &Path,
    inputs: Option<&crate::exomonad::workspace::FrozenWorkspace>,
    run_root: &Path,
    candidate: Option<CandidateSources<'_>>,
) -> Result<CompiledExomonadDriver, Box<dyn std::error::Error>> {
    let DriverSources { preamble, include } =
        driver_sources(haskell_root, inputs, run_root, candidate)?;
    let templates = resident_workbench_templates(&preamble, DRIVER_EFFECTS, "");
    let include_refs: Vec<_> = include.iter().map(PathBuf::as_path).collect();
    let session_root = run_root.join("haskell-session");
    std::fs::create_dir_all(&session_root)?;
    let compiled = match run_turn(HaskellTurnRequest {
        session_id: None,
        turn_text: DRIVER_ENTRY,
        templates: &templates,
        include: &include_refs,
        session_root: &session_root,
        inject_modules: &[],
        gen: 1,
        verdict: None,
        target: None,
        // The driver is the session's first turn, so there is nothing
        // retained to link against yet.
        retained_imports: &[],
    })
    .map_err(render_root_compile_failure)?
    {
        TurnResult::Expr { compiled, .. } => compiled,
        other => {
            return Err(runtime_error(format!(
                "root interactive driver is not an expression: {other:?}"
            )))
        }
    };

    Ok(CompiledExomonadDriver {
        preamble,
        include,
        compiled,
    })
}

fn compile_root(
    config: &ActorHostConfig,
    run_root: &Path,
    worktrees: WorktreeManager,
    worktree_authority: ActorWorktreeAuthority,
    source: Option<&Arc<crate::exomonad::source::ExomonadSourceReload>>,
    host_incarnation: exomonad_actor::Incarnation,
) -> Result<
    (
        ActorWorkbenchSource,
        ExomonadRoot,
        Arc<tidepool_runtime::session::CompiledTurn>,
    ),
    Box<dyn std::error::Error>,
> {
    let CompiledExomonadDriver {
        preamble,
        include,
        compiled,
    } = compile_driver(
        &config.haskell_root,
        config.workspace_inputs.as_ref(),
        run_root,
        None,
    )?;
    let declarations = exomonad_effect_declarations();
    let session_root = run_root.join("haskell-session");
    let session = tidepool_runtime::session::fresh_session_id();
    let mut module_env = tidepool_mcp::session_decl_module_env_hiding(
        &declarations,
        false,
        tidepool_mcp::CompanionImports::Omit,
        EXOMONAD_REPLACED_EFFECT_NAMES,
    );
    if let Some(inputs) = &config.workspace_inputs {
        module_env.imports.extend(inputs.imports());
    }
    let mut library = SessionLib::open(session, &session_root, module_env)?
        .with_validation_include(include.clone());
    let recovery_report =
        library.attach_recovery_manifest(config.run_root.join("root-declarations.json"))?;
    tracing::info!(
        source_session = ?recovery_report.source_session,
        successor_session = recovery_report.successor_session,
        replayed = recovery_report.replayed.len(),
        lost = recovery_report.lost.len(),
        "attached Exomonad root declaration recovery manifest"
    );
    let event_registry =
        WorktreeRegistry::open(actor_worktree_storage_root(&config.workspace).join("registry"))?;
    let event_journal = EventJournal::open(run_root.join("repo-events.jsonl"))?;
    let event_handler = RepoEventHandler::with_registry_namespace(
        WorktreeMonitor::new(GitCli::new(), event_journal),
        event_registry,
        EventConfig::default(),
        runtime_namespace(run_root),
    );
    let worktree_handler =
        ActorWorktreeHandler::new(WorktreeHandler::from_manager(worktrees), worktree_authority);
    let run_id = run_root
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| runtime_error("run root has no UTF-8 run identifier"))?;
    let journal_path = crate::exomonad::exomonad_journal_path(&config.workspace, run_id);
    if let Some(parent) = journal_path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let journal = if host_incarnation == exomonad_actor::Incarnation::FIRST {
        tidepool_handlers::JournalHandler::new(tidepool_handlers::SegmentPath::create_exclusive(
            journal_path,
        )?)?
    } else {
        tidepool_handlers::JournalHandler::resuming(
            tidepool_handlers::SegmentPath::open_existing(journal_path)?,
            host_incarnation.0,
        )?
    };
    let mut machine = ResidentSession::unbootstrapped(
        hlist![
            source_handler(source),
            journal,
            event_handler,
            ActorBoundWorktreeHandler::new(worktree_handler.clone()),
            ActorWorktreeRegistryHandler::new(worktree_handler.clone()),
            ActorWorktreeAllocationHandler::new(worktree_handler.clone()),
            ActorWorktreeIntegrationHandler::new(worktree_handler.clone()),
            worktree_handler,
        ],
        CapturedOutput::new(),
        DEFAULT_NURSERY_SIZE,
        Some(library),
    );
    machine.set_effect_execution(
        EffectRunPolicy::HandleOrSuspend,
        LivePayloadPolicy::HASKELL_EFFECT_VALUE,
    );
    let lexical_scope = machine.mint_isolated_scope();
    machine.set_actor_execution(
        tidepool_runtime::session::SessionRunContext {
            lexical_scope,
            resource_scope: tidepool_codegen::suspension::RealmId::fresh(),
            ..tidepool_runtime::session::SessionRunContext::ROOT
        },
        EffectRunPolicy::HandleOrSuspend,
        LivePayloadPolicy::HASKELL_EFFECT_VALUE,
    )?;
    let outcome = machine.run_with_sites("exomonad_root_driver", compiled.code())?;
    let resource_scope = match &outcome {
        tidepool_runtime::session::ResidentOutcome::Suspended { hole, .. } => machine
            .parked_realm(hole)
            .ok_or_else(|| runtime_error("root driver suspension had no owning realm"))?,
        _ => {
            return Err(runtime_error(
                "root driver completed before attaching its permanent application",
            ))
        }
    };
    let descriptor = ActorDescriptor::new(
        "exomonad-root",
        ActorPlacement {
            session,
            resource_scope,
            lexical_scope,
        },
    )
    // Profiles classify resident Haskell rows, not the native Codex sandbox.
    // The root allocates worktrees and may attenuate children to ReadOnly.
    .with_profile(ActorEffectProfile::ReadWrite)
    .with_effective_role(
        exomonad_actor::EffectiveRole::root().with_research_policy(config.research_policy),
    );
    // A run that does not supply `Jev.Operators` gets a workbench without `J`,
    // rather than a compile failure over a module nothing on its search path
    // defines. The same answer tells the agent so in its instructions.
    let jev = config.jev_surface() == prompt_catalog::JevSurface::Installed;
    let mut workbench = ActorWorkbenchSource::new(preamble, include)
        .with_imports(WORKBENCH_SURFACE_MODULE)
        .with_imports("qualified Tidepool.Actor.Record as R")
        .with_imports("qualified Tidepool.Command as Cmd");
    if jev {
        workbench = workbench
            .with_imports("qualified Jev.Operators as J")
            .with_imports("Jev.Operators (Packet ((:=), (:&)))")
            .with_imports("qualified Jev.Core")
            .with_imports("qualified Jev.Core.Contract")
            .with_imports("qualified Jev.Core.Schema")
            .with_imports("qualified Jev.Core.Json");
    }
    Ok((
        workbench
            .with_imports("Tidepool.Command (bash, withMemory, Memory(..))")
            .with_imports("qualified Tidepool.Actor as Actor")
            .with_default_quasiquoters()
            .with_tools(
                config
                    .workspace_inputs
                    .as_ref()
                    .and_then(|inputs| inputs.tools.as_deref())
                    .unwrap_or("Tidepool.Tools.tools"),
            )
            // Rule two of spec discovery. Rule one is a file in an actor's own
            // checkout and belongs to no run-wide value; this key is how a
            // workspace names a spec for actors that have no checkout.
            .with_spec_if(
                config
                    .workspace_inputs
                    .as_ref()
                    .and_then(|inputs| inputs.spec.as_deref()),
            )
            .with_workspace_modules(
                config
                    .workspace_inputs
                    .as_ref()
                    .map(|inputs| inputs.modules.clone())
                    .unwrap_or_default(),
            ),
        ResidentActorRoot::new(descriptor, machine, outcome),
        Arc::new(compiled),
    ))
}

fn render_root_compile_failure(
    failure: tidepool_runtime::session::TurnFailure,
) -> Box<dyn std::error::Error> {
    let source = failure.attempted_source.as_deref().unwrap_or_default();
    let detail = match &failure.error {
        tidepool_runtime::CompileError::Diagnostics(diagnostics)
        | tidepool_runtime::CompileError::WorkerFailure(diagnostics) => {
            tidepool_toolchain::diag::render_diagnostics(
                diagnostics,
                &tidepool_toolchain::diag::RenderOpts {
                    anchor: "Expr.hs",
                    label: "<exomonad-driver>",
                    user_lines: None,
                    line_offset: 0,
                    col_indent: 0,
                    drop_foreign_gen_warnings_except: None,
                    source,
                },
            )
        }
        _ => failure.to_string(),
    };
    runtime_error(format!(
        "root interactive driver compilation failed:\n{detail}"
    ))
}

fn spawn_undeployed_hosted_retirement(
    retirements: &mut JoinSet<InteractiveCleanupReceipt>,
    actor: ActorRef,
    owners: &InteractiveOwners,
    tmux: &TmuxSession,
) {
    let retained = {
        let rows = owners.lock();
        rows.get(&actor).map(|row| {
            (
                row.hosted.lock().clone(),
                row.pane.lock().clone(),
                row.scoped_retention
                    .as_ref()
                    .map(|retention| retention.slot.clone()),
                row.retirement.clone(),
                row.native_retirement,
            )
        })
    };
    let Some((service, pane, scope, receipt_slot, native_retirement)) = retained else {
        return;
    };
    let tmux = tmux.clone();
    retirements.spawn(async move {
        let http = match service {
            Some(mut service) => stop_retired_tool_service(actor, &mut service).await,
            None => CleanupComponentOutcome::Completed,
        };
        let process = retire_scoped_process(scope, native_retirement)
            .await
            .unwrap_or_else(|| CleanupComponentOutcome::Failed {
                detail: "undeployed launch has no exact native process row".into(),
            });
        let pane = match pane {
            None => CleanupComponentOutcome::Completed,
            Some(pane)
                if native_retirement == NativeRetirement::Preserve
                    || matches!(process, CleanupComponentOutcome::Completed) =>
            {
                retire_pane_artifact(&tmux, &pane, native_retirement).await
            }
            Some(_) => CleanupComponentOutcome::Failed {
                detail: "pane retained because exact process termination is unconfirmed".into(),
            },
        };
        let receipt = InteractiveCleanupReceipt {
            actor,
            components: vec![
                CleanupComponentReceipt {
                    component: CleanupComponent::ToolService,
                    outcome: http,
                },
                CleanupComponentReceipt {
                    component: CleanupComponent::Process,
                    outcome: process,
                },
                CleanupComponentReceipt {
                    component: CleanupComponent::Pane,
                    outcome: pane,
                },
            ],
        };
        receipt_slot.lock().get_or_insert_with(|| receipt.clone());
        receipt
    });
}

fn spawn_owned_retirement(
    retirements: &mut JoinSet<InteractiveCleanupReceipt>,
    deployment: InteractiveDeployment,
    tmux: TmuxSession,
    owners: &InteractiveOwners,
) {
    let (scope, receipt_slot, native_retirement) = {
        let rows = owners.lock();
        #[allow(clippy::expect_used, reason = "exact deployment retention row")]
        let row = rows
            .get(&deployment.actor)
            .expect("exact deployment retention row");
        (
            row.scoped_retention
                .as_ref()
                .map(|retention| retention.slot.clone()),
            row.retirement.clone(),
            row.native_retirement,
        )
    };
    // Only slots cross the task boundary; neither owns a back-reference to its
    // map row. Namespace cleanup is status only and does not discharge host work.
    retirements.spawn(async move {
        hosted_retirement::settle_input_seal(&deployment.service, APPLICATION_TASK_GRACE_TIMEOUT)
            .await;
        let process = retire_scoped_process(scope, native_retirement).await;
        let receipt =
            retire_interactive_application_guarded(deployment, &tmux, native_retirement, process)
                .await;
        receipt_slot.lock().get_or_insert_with(|| receipt.clone());
        receipt
    });
}

/// Consume the exact retained slot without moving it out of the lifecycle row.
/// The returned outcome is reporting evidence only: the slot still owns the
/// cleanup receipt and the caller must account for hosted work and leases.
async fn retire_scoped_process(
    scope: Option<Arc<Mutex<scoped_custody::ScopedProcessSlot>>>,
    native_retirement: NativeRetirement,
) -> Option<CleanupComponentOutcome> {
    let scope = scope?;
    // The slot only leaves `Reserved` once `stage_supervisor` runs, immediately
    // before the tmux native-process submission. A row still `Reserved` at
    // retirement (admission timeout, workspace preparation failure, or any
    // other error raised before that point) is certain to have never spawned
    // a native process, so there is nothing to preserve or stop either way.
    if matches!(*scope.lock(), scoped_custody::ScopedProcessSlot::Reserved) {
        return Some(CleanupComponentOutcome::Completed);
    }
    if native_retirement == NativeRetirement::Preserve {
        return Some(CleanupComponentOutcome::Failed {
            detail: "native process intentionally preserved; exact scope remains retained".into(),
        });
    }
    let stopped = tidepool_runtime::spawn_blocking_in_span(move || {
        scoped_custody::stop_retained_slot(
            &scope,
            std::time::Instant::now() + APPLICATION_SHUTDOWN_TIMEOUT,
        )
    })
    .await;
    Some(match stopped {
        Ok(Ok(_)) => CleanupComponentOutcome::Completed,
        Ok(Err(error)) => CleanupComponentOutcome::Failed {
            detail: format!("exact scoped process cleanup remains unconfirmed: {error}"),
        },
        Err(error) => CleanupComponentOutcome::Failed {
            detail: format!("scoped process cleanup task failed: {error}"),
        },
    })
}

async fn retain_input_custody_and_bind(
    owner: &hosted_retirement::HostedOwner,
    backend: Arc<dyn InteractiveAgentBackend>,
    thread: &QueueReadyThread,
    producer: &InputProducerId,
) -> Result<(), String> {
    hosted_retirement::begin_input_seal(
        owner,
        Arc::clone(&backend),
        thread.clone(),
        producer.clone(),
    )
    .await
    .map_err(|error| format!("could not retain native input custody: {error}"))?;
    if !thread.supports_active_input() {
        return Ok(());
    }
    match backend.bind_input(thread).await {
        Ok(exomonad_agent::InputAdmission::Admitted) => Ok(()),
        Ok(outcome) => Err(format!("native input bind returned {outcome:?}")),
        Err(error) => Err(format!("could not bind native input control: {error}")),
    }
}

async fn run_interactive_applications(
    mut lifecycle: mpsc::Receiver<LocalResidentDeployment>,
    application_owners: InteractiveOwners,
    fleet: InteractiveFleet,
    shutdown: watch::Receiver<Option<NativeRetirement>>,
    mut root_config: watch::Receiver<ActorHostConfig>,
) -> Result<(), String> {
    let InteractiveFleet {
        root,
        config,
        run_root,
        tmux,
        backend,
        worktrees,
        bindings,
        readiness,
        worktree_authority,
        watch_retention,
        watch_observation,
        source_layers,
        actor_recovery,
        recovered_threads,
        recovered_root_predecessor,
    } = fleet;
    let base_prompt = FrozenBasePrompt::materialize_selected(
        &run_root,
        config
            .workspace_inputs
            .as_ref()
            .and_then(|inputs| inputs.prompts.get("core"))
            .map(String::as_str),
        config.jev_surface(),
    )
    .map_err(|error| format!("cannot prepare Exomonad base prompt: {error}"))?;
    let mut root_identity = root.identity();
    let mut launch_context = InteractiveLaunchContext {
        base_prompt,
        root: root_identity,
        config,
        run_root,
        tmux: tmux.clone(),
        backend: Arc::clone(&backend),
        worktrees: worktrees.clone(),
        bindings: Arc::clone(&bindings),
        actor_recovery,
        recovered_threads: Arc::clone(&recovered_threads),
    };
    let mut deployments: Vec<InteractiveDeployment> = Vec::new();
    let mut launches = JoinSet::new();
    let mut binding_discoveries = JoinSet::new();
    let mut retirements = JoinSet::new();
    // Supervisors waiting for a stopped actor's release receipt. Served from
    // the receipt slot when it already exists, else when retirement joins.
    let mut release_waiters: HashMap<ActorRef, Vec<Arc<exomonad_actor::ReleaseAwait>>> =
        HashMap::new();
    let mut notifications = JoinSet::new();
    let mut publication_retries = JoinSet::new();
    let mut process_observations = JoinSet::new();
    let mut health = tokio::time::interval(Duration::from_secs(1));
    let failure = loop {
        tokio::select! {
            biased;
            _ = wait_for_shutdown(shutdown.clone()) => break None,
            changed = root_config.changed() => {
                if changed.is_ok() { launch_context.config = root_config.borrow_and_update().clone(); }
            }
            _ = health.tick() => {
                if process_observations.is_empty() {
                    let rows = application_owners.lock();
                    for deployment in deployments.iter().filter(|deployment| {
                        !deployment.failure_reported
                            && deployment.local_actor.terminal().get().is_none()
                    }) {
                        let Some(slot) = rows
                            .get(&deployment.actor)
                            .and_then(|row| row.scoped_retention.as_ref())
                            .map(|retention| retention.slot.clone())
                        else {
                            continue;
                        };
                        let actor = deployment.actor;
                        process_observations.spawn_blocking(move || {
                            let observed = scoped_custody::observe_slot(
                                &slot,
                                std::time::Instant::now() + Duration::from_millis(250),
                            );
                            (actor, observed)
                        });
                    }
                }
                for deployment in &deployments {
                    if let Some(thread) = &deployment.thread {
                        let workspace = deployment.active_workspace.clone();
                        if let Ok(mut publication) = workspace.publication.clone().try_lock_owned() {
                            if publication.is_pending() {
                                let backend = backend.clone();
                                let owner = BoundWorkspace { workspace, thread: thread.clone() };
                                let actor = deployment.actor;
                                publication_retries.spawn(async move {
                                    if let Err(error) = owner.settle_publication(&mut publication, backend.as_ref()).await {
                                        tracing::debug!(?actor, %error, "workspace publication recovery remains pending");
                                    }
                                });
                            }
                        }
                    }
                }
                for index in 0..deployments.len() {
                    let deployment = &deployments[index];
                    let snapshot = deployment.runtime_observation.snapshot();
                    if snapshot.provider_observation_stale { continue; }
                    for turn in snapshot.provider_failures {
                    let deployment = &deployments[index];
                    let exomonad_agent::ProviderTurnState::Failed(failure) = turn.state else { continue; };
                    let key = (turn.thread.clone(), turn.turn.clone());
                    if deployment.notified_provider_failures.contains(&key) { continue; }
                    let actor = deployment.actor;
                    if let Some(supervisor) = deployment.supervisor {
                        let Some(owner) = deployments.iter().find(|app| app.actor == supervisor) else { continue; };
                        let event = DurableActorEvent::Typed(TypedActorEvent::ProviderTurnFailed {
                            revision: turn.revision as u64,
                            actor, thread: turn.thread, turn: turn.turn, failure,
                        });
                        if let Err(error) = publish_inbox_event(Arc::clone(&owner.inbox), event).await {
                            tracing::warn!(?actor, %error, "provider failure notice pending publication");
                            // Preserve source order: a later watermark must not suppress
                            // this failed publication on retry.
                            break;
                        }
                    } else {
                        // Root failures are operator-visible, never self-injected retries.
                        tracing::error!(?actor, ?failure, turn = %turn.turn, "root provider turn needs attention");
                    }
                    deployments[index].notified_provider_failures.insert(key);
                    }
                }
                if let Some(index) = deployments.iter().position(|deployment| {
                    !deployment.failure_reported
                        && deployment.local_actor.terminal().get().is_none()
                        && (hosted_retirement::service_finished(&deployment.service)
                            || matches!(
                                &deployment.connection,
                                InteractiveConnection::Bound { delivery, .. }
                                    if delivery.is_finished()
                            ))
                }) {
                    let local_actor = deployments[index].local_actor.clone();
                    deployments[index].failure_reported = true;
                    let detail = "interactive application exited before actor settlement".to_string();
                    if let Err(error) = apply_application_failure(
                        local_actor,
                        ExternalApplicationFailure {
                            class: ExternalApplicationFailureClass::UnexpectedExit,
                            detail,
                        }
                    ).await {
                        break Some(error);
                    }
                }
            }
            Some(observed) = process_observations.join_next(), if !process_observations.is_empty() => {
                let Ok((actor, Ok(scoped_custody::ScopedProcessObservation::ProcessStopped))) = observed else {
                    continue;
                };
                let Some(index) = deployments.iter().position(|deployment| {
                    deployment.actor == actor
                        && !deployment.failure_reported
                        && deployment.local_actor.terminal().get().is_none()
                }) else {
                    continue;
                };
                let local_actor = deployments[index].local_actor.clone();
                deployments[index].failure_reported = true;
                if let Err(error) = apply_application_failure(
                    local_actor,
                    ExternalApplicationFailure {
                        class: ExternalApplicationFailureClass::UnexpectedExit,
                        detail: "supervised native process exited before actor settlement".into(),
                    },
                )
                .await
                {
                    break Some(error);
                }
            }
            event = lifecycle.recv() => {
                let Some(event) = event else { break None };
                match event {
                    LocalResidentDeployment::PolicyInstalled(installation) => {
                        if installation.creator.is_none() {
                            launch_context.config = root_config.borrow_and_update().clone();
                            root_identity = installation.actor.identity();
                            launch_context.root = root_identity;
                        }
                        worktree_authority.install_grant(
                            installation.actor.identity().into(),
                            worktree_grant(installation.effective_role.role()),
                        );
                        let fork_parent_thread = match (
                            recovered_threads.contains_key(&installation.actor.identity()),
                            installation.context_parent,
                        ) {
                            (true, _) => None,
                            (false, None) => None,
                            (false, Some(parent)) => {
                                let Some(thread) = deployments
                                    .iter()
                                    .find(|deployment| deployment.actor == parent)
                                    .and_then(|deployment| deployment.thread.clone())
                                else {
                                    break Some(format!("context-fork parent {parent:?} has no queue-ready conversation"));
                                };
                                Some(thread.id().clone())
                            }
                        };
                        let workspace_prepared = installation.worktree_custody.as_ref()
                            .and_then(|custody| (custody.as_ref() as &dyn std::any::Any)
                                .downcast_ref::<ActorWorkspaceCustody>())
                            .is_some_and(|custody| custody.workspace.is_some());
                        let native_admission = NativeForkAdmission {
                            owners: application_owners.clone(), backend: backend.clone(), layout: None,
                        };
                        let context = launch_context.clone();
                        let actor = installation.actor.identity();
                        let (cancel, cancelled) = oneshot::channel();
                        let mut owners = application_owners.lock();
                        if owners.contains_key(&actor) {
                            break Some(format!("duplicate application owner for {actor:?}"));
                        }
                        let hosted_slot = Arc::new(Mutex::new(None));
                        let pane_slot = Arc::new(Mutex::new(None));
                        let mut owner = InteractiveApplicationOwner {
                            supervisor: installation.supervisor_parent,
                            creator_workspace: None,
                            cancel: Some(cancel),
                            native_retirement: NativeRetirement::Preserve,
                            pane: pane_slot.clone(),
                            fork_gate: installation.fork_gate.clone(),
                            custody: installation.worktree_custody.clone(),
                            scoped_retention: None,
                            hosted: hosted_slot.clone(),
                            launch: HostLaunchState::Pending,
                            pending_activations: Vec::new(),
                            terminal: None,
                            retirement: Arc::new(Mutex::new(None)),
                        };
                        let workspace = match actor_workspace_request(
                            actor == root_identity, &installation.launch_worktrees,
                        ) {
                            Ok(workspace) => workspace,
                            Err(error) => break Some(error),
                        };
                        let scope_slot = match owner.reserve_scope(workspace, actor) {
                            Ok(slot) => slot,
                            Err(error) => break Some(format!(
                                "actor {actor:?} process-scope reservation failed: {error}"
                            )),
                        };
                        owners.insert(actor, owner);
                        drop(owners);
                        let retention = InteractiveLaunchRetention {
                            hosted: hosted_slot,
                            pane: pane_slot,
                            process: scope_slot,
                        };
                        tracing::info!(
                            actor = ?actor,
                            worktree = matches!(workspace, ActorWorkspaceRequest::Worktree(_)),
                            "actor launch started"
                        );
                        installation
                            .runtime_observation
                            .publish_launch_pending("preparing the workspace");
                        launches.spawn(async move {
                            let local_actor = installation.actor.clone();
                            let launch_observation = installation.runtime_observation.clone();
                            let result = AssertUnwindSafe(async {
                                let build_snapshot = if workspace_prepared { None } else {
                                    match installation.creator {
                                        Some(creator) => native_admission.build_snapshot(creator, installation.effective_role.native_tools()).await,
                                        None => None,
                                    }
                                };
                                launch_interactive_application(
                                    *installation,
                                    context,
                                    cancelled,
                                    InteractiveInheritance { thread: fork_parent_thread, build_snapshot },
                                    retention,
                                ).await
                            })
                            .catch_unwind()
                            .await
                            .unwrap_or_else(|_| {
                                Err(application_error(
                                    actor,
                                    InteractiveOperation::PrepareRuntime,
                                    "interactive launch task panicked",
                                ))
                            });
                            if let Err(error) = &result {
                                launch_observation
                                    .publish_launch_pending(format!("launch failed: {}", error.detail));
                            }
                            (local_actor, result)
                        });
                    }
                    LocalResidentDeployment::SessionReady { activation } => {
                        let actor = activation.id.actor();
                        let Some(application) = deployments.iter_mut().find(|app| app.actor == actor) else {
                            let mut owners = application_owners.lock();
                            if let Some(owner) = owners.get_mut(&actor) {
                                if owner.terminal.is_some() { continue; }
                                match owner.launch {
                                    HostLaunchState::Pending => {
                                        owner.pending_activations.push(activation);
                                        continue;
                                    }
                                    HostLaunchState::Failed(_) | HostLaunchState::Abandoned => continue,
                                    HostLaunchState::Published => {}
                                }
                            }
                            break Some(format!("resident actor {actor:?} requested a session activation without a deployed application"));
                        };
                        if let Err(error) = deliver_session_activation(application, activation).await {
                            break Some(error);
                        }
                    }

                    LocalResidentDeployment::Retired { actor, terminal } => {
                        worktree_authority.remove_grant(actor.into());
                        if let Some(resources) = &launch_context.config.command_resources {
                            let producer = format!("{}-{}", actor.id.0, actor.incarnation.0);
                            if let Err(error) = resources.seal_producer(&producer).await {
                                tracing::warn!(?actor, %error, "command resource producer retirement remains unconfirmed");
                            }
                        }
                        if let Some(owner) = application_owners.lock().get_mut(&actor) {
                            owner.retired(terminal);
                        }
                        if let Some(index) = deployments.iter().position(|app| app.actor == actor) {
                            let deployment = deployments.swap_remove(index);
                            spawn_owned_retirement(&mut retirements, deployment, tmux.clone(), &application_owners);
                        } else {
                            spawn_undeployed_hosted_retirement(
                                &mut retirements,
                                actor,
                                &application_owners,
                                &tmux,
                            );
                        }
                    }
                    LocalResidentDeployment::ReleaseAwait(request) => {
                        // `Retired` precedes this on the same channel, so an
                        // owner row either already holds its receipt or has a
                        // retirement in flight. An actor without a row never
                        // held interactive resources.
                        let actor = request.actor;
                        let settled = match application_owners.lock().get(&actor) {
                            None => Some(exomonad_actor::ResourceRelease::Released),
                            Some(owner) => owner.retirement.lock().as_ref().map(InteractiveCleanupReceipt::release),
                        };
                        match settled {
                            Some(release) => { request.answer(release); }
                            None => release_waiters.entry(actor).or_default().push(request),
                        }
                    }
                    LocalResidentDeployment::CommandBackend(request) => {
                        // An actor with an agent process of its own runs its
                        // commands inside that process's sandbox. One without
                        // — a record actor started from a notebook, or an
                        // operator workbench — has no sandbox to run in, so it
                        // runs here instead, in whatever worktree it holds
                        // custody of. Borrowing an ancestor's sandbox, which
                        // is what this did before, put the command somewhere
                        // the actor's own worktree is mounted read-only.
                        let backend = launch_context.config.command_resources.clone()
                            .ok_or_else(|| tidepool_bridge_effects::CommandError::CommandUnavailable("this run has no command resource authority".into()))
                            .and_then(|resources| {
                                match deployments.iter().find(|app| app.actor == request.owner).and_then(|app| app.thread.clone()) {
                                    Some(thread) => Ok(Arc::new(commands::NativeCommandBackend::new(
                                        launch_context.backend.clone(), thread, resources, request.owner,
                                    )) as Arc<dyn exomonad_actor::command_jobs::CommandBackend>),
                                    None => {
                                        let bubblewrap = resolve_scope_bubblewrap(
                                            &launch_context.config.pane_environment,
                                        )
                                        .map_err(|error| {
                                            tidepool_bridge_effects::CommandError::CommandUnavailable(
                                                format!("cannot resolve bubblewrap for resident commands: {error}"),
                                            )
                                        })?;
                                        Ok(Arc::new(commands::HostCommandBackend::new(
                                            resources,
                                            request.owner,
                                            resident_command_roots(
                                                &worktree_authority,
                                                &launch_context.worktrees,
                                                &launch_context.config.workspace,
                                                request.owner,
                                            ),
                                            bubblewrap,
                                        ))
                                            as Arc<dyn exomonad_actor::command_jobs::CommandBackend>)
                                    }
                                }
                            });
                        request.supply(backend);
                    }
                    LocalResidentDeployment::NotificationSend(command) => {
                        let target = command.target();
                        let Some(application) = deployments.iter().find(|app| app.actor == target) else {
                            command.rejected(exomonad_actor::NotificationError::Unavailable);
                            continue;
                        };
                        if !application.thread.as_ref().is_some_and(QueueReadyThread::supports_active_input) {
                            command.rejected(exomonad_actor::NotificationError::Unavailable);
                            continue;
                        }
                        let inbox = Arc::clone(&application.inbox);
                        let key = application.notification_inbox_key.clone();
                        notifications.spawn(async move {
                            let result = tidepool_runtime::spawn_blocking_in_span(move || {
                                admit_notification(&command, key, &inbox);
                            }).await.map_err(|error| error.to_string());
                            (target, result)
                        });
                    }
                    LocalResidentDeployment::NotificationPoll(command) => {
                        let result = deployments.iter()
                            .find(|application| application.actor == command.receipt().target())
                            .ok_or(exomonad_actor::NotificationError::Unavailable)
                            .and_then(|application| observe_notification_receipt(
                                &command, application.actor,
                                &application.notification_inbox_key, &application.inbox,
                            ));
                        command.observed(result);
                    }
                    LocalResidentDeployment::RequestUpdate { delivery } => {
                        let target = delivery.target();
                        let Some(application) = deployments.iter().find(|app| app.actor == target) else {
                            if let Some(presentation) = delivery.begin() {
                                let detail = match application_owners.lock().get(&target) {
                                    // The host admitted this actor (it holds a
                                    // launch-lifecycle row) but has not published
                                    // an application for it yet, if ever.
                                    Some(owner) => format!(
                                        "actor {}@{} admitted; provider not started ({})",
                                        target.id.0,
                                        target.incarnation.0,
                                        owner.launch.provider_not_started_phase()
                                    ),
                                    None => "target application unavailable".into(),
                                };
                                presentation.not_presented(detail);
                            }
                            continue;
                        };
                        if application.thread.is_none() {
                            if let Some(presentation) = delivery.begin() {
                                presentation.not_presented("target conversation is not bound".into());
                            }
                            continue;
                        }
                        let update = delivery.id();
                        let context = DeliveryProvenance::RequestUpdate {
                            owner: delivery.owner(),
                            target,
                            request: update.request,
                            update: update.sequence,
                        };
                        let message = delivery.message().to_owned();
                        let published = application.inbox.publish_tracked(
                            DurableActorEvent::Typed(TypedActorEvent::RequestUpdate {
                                request: update.request,
                                update: update.sequence,
                                message: message.clone(),
                            }),
                            context.clone(),
                        );
                        let envelope = match published {
                            Ok(envelope) => envelope,
                            Err(error) => {
                                if let Some(presentation) = delivery.begin() {
                                    presentation.not_presented(format!("durable update publication failed: {error}"));
                                }
                                continue;
                            }
                        };
                        let Some(sequence) = std::num::NonZeroU64::new(envelope.sequence) else {
                            if let Some(presentation) = delivery.begin() {
                                presentation.not_presented("durable inbox allocated zero sequence".into());
                            }
                            continue;
                        };
                        let operation_id = InputOperationId {
                            producer: application.input_producer.clone(),
                            sequence,
                        };
                        let correlation = exomonad_actor::RequestUpdateCorrelation {
                            producer: operation_id.producer.as_str().to_owned(),
                            sequence,
                        };
                        let reconciler = match delivery.bind_correlation(correlation) {
                            Ok(reconciler) => reconciler,
                            Err(error) => {
                                if let Err(reject_error) = application
                                    .inbox
                                    .confirm_rejected(envelope.sequence, &context)
                                {
                                    tracing::warn!(
                                        sequence = envelope.sequence,
                                        error = %reject_error,
                                        "cannot confirm rejection of an unreconcilable update"
                                    );
                                }
                                if let Some(presentation) = delivery.begin() {
                                    presentation.not_presented(format!(
                                        "update correlation failed: {error}"
                                    ));
                                }
                                continue;
                            }
                        };
                        let Some(presentation) = delivery.begin() else {
                            if let Err(reject_error) = application
                                .inbox
                                .confirm_rejected(envelope.sequence, &context)
                            {
                                tracing::warn!(
                                    sequence = envelope.sequence,
                                    error = %reject_error,
                                    "cannot confirm rejection of an unpresented update"
                                );
                            }
                            continue;
                        };
                        application.update_reconciliations.lock().insert(
                            operation_id.native_key(),
                            PendingUpdateReconciliation {
                                inbox: Arc::clone(&application.inbox),
                                sequence: envelope.sequence,
                                context,
                                reconciler,
                            },
                        );
                        presentation.unconfirmed(
                            "request update is durably queued for ordered native delivery".into(),
                        );
                    }
                    LocalResidentDeployment::ChildExited { notice } => {
                        if let Some(notification) = prepare_owner_notification(&notice, &deployments) {
                            notifications.spawn(publish_owner_notification(notification));
                        }
                    }
                    LocalResidentDeployment::WatchChanged { notification } => {
                        let Some(application) = deployments
                            .iter()
                            .find(|app| app.actor == notification.owner)
                        else {
                            continue;
                        };
                        notifications.spawn(publish_inbox_event_for(
                            notification.owner,
                            Arc::clone(&application.inbox),
                            DurableActorEvent::Typed(TypedActorEvent::WatchChanged {
                                notification,
                            }),
                        ));
                    }
                    LocalResidentDeployment::SettlementChanged { notification } => {
                        let Some(application) = deployments
                            .iter()
                            .find(|app| app.actor == notification.owner)
                        else {
                            continue;
                        };
                        notifications.spawn(publish_inbox_event_for(
                            notification.owner,
                            Arc::clone(&application.inbox),
                            DurableActorEvent::Typed(TypedActorEvent::SettlementChanged {
                                notification,
                            }),
                        ));
                    }
                    LocalResidentDeployment::RequestCancellation { notification } => {
                        let Some(application) = deployments
                            .iter()
                            .find(|app| app.actor == notification.target)
                        else {
                            continue;
                        };
                        notifications.spawn(publish_inbox_event_for(
                            notification.target,
                            Arc::clone(&application.inbox),
                            DurableActorEvent::Typed(TypedActorEvent::RequestCancellation {
                                notification,
                            }),
                        ));
                    }
                }
            }
            recovered = publication_retries.join_next(), if !publication_retries.is_empty() => {
                if let Some(Err(error)) = recovered {
                    tracing::warn!(%error, "build publication recovery task interrupted; durable state retained");
                }
            }
            launched = launches.join_next(), if !launches.is_empty() => {
                match launched {
                    Some(Ok((local_actor, Ok(Some(launched))))) => {
                        let actor = local_actor.identity();
                        let (already_retired, pending_activations) = {
                            let mut owners = application_owners.lock();
                            #[allow(clippy::expect_used, reason = "registered launch owner")]
                            let owner = owners.get_mut(&actor).expect("registered launch owner");
                            owner.launch = HostLaunchState::Published;
                            (owner.terminal.is_some(), std::mem::take(&mut owner.pending_activations))
                        };
                        let mut deployment = launched.deployment;
                        if already_retired {
                            spawn_owned_retirement(&mut retirements, deployment, tmux.clone(), &application_owners);
                            continue;
                        }
                        if let Some((predecessor, _)) = recovered_threads.get(&actor) {
                            readiness.send(ActorHostReadiness::ActorRecovered {
                                predecessor: *predecessor,
                                actor,
                            }).ok();
                        }
                        if actor == root_identity {
                            if let Some(predecessor) = recovered_root_predecessor {
                                readiness.send(ActorHostReadiness::ActorRecovered {
                                    predecessor,
                                    actor,
                                }).ok();
                            }
                            readiness.send(ActorHostReadiness::AwaitingBinding { root: root_identity }).ok();
                        }
                        let pane = deployment.pane.clone();
                        let fork_gate = deployment.fork_gate.clone();
                        let runtime_observation = deployment.runtime_observation.clone();
                        let fork_parent_thread = deployment.fork_parent_thread.clone();
                        let tmux = tmux.clone();
                        binding_discoveries.spawn(async move {
                            let result = AssertUnwindSafe(discover_interactive_binding(
                                actor,
                                launched.binding,
                                &tmux,
                                &pane,
                            ))
                            .catch_unwind()
                            .await
                            .unwrap_or_else(|_| {
                                Err(application_error(
                                    actor,
                                    InteractiveOperation::DiscoverBinding,
                                    "interactive binding task panicked",
                                ))
                            });
                            if let Ok(thread) = &result {
                                runtime_observation.publish_provider_binding(
                                    fork_parent_thread.map(|thread| thread.0),
                                    thread.id().0.clone(),
                                );
                            }
                            let result = match (result, fork_gate) {
                                (Ok(thread), Some(gate)) => {
                                    match gate.mark_ready() {
                                        Err(error) => Err(application_error(
                                            actor,
                                            InteractiveOperation::DiscoverBinding,
                                            error,
                                        )),
                                        Ok(()) => match gate.wait_committed().await {
                                            Ok(()) => Ok(thread),
                                            Err(error) => Err(application_error(
                                                actor,
                                                InteractiveOperation::DiscoverBinding,
                                                error,
                                            )),
                                        },
                                    }
                                }
                                (result, None) => result,
                                (Err(error), Some(_)) => Err(error),
                            };
                            (actor, result)
                        });
                        let mut activation_error = None;
                        for activation in pending_activations {
                            if let Err(error) = deliver_session_activation(&mut deployment, activation).await {
                                activation_error = Some(error);
                                break;
                            }
                        }
                        deployments.push(deployment);
                        if let Some(error) = activation_error { break Some(error); }
                    }
                    Some(Ok((local_actor, Ok(None)))) => {
                        let actor = local_actor.identity();
                        if let Some(owner) = application_owners.lock().get_mut(&actor) {
                            owner.launch = HostLaunchState::Abandoned;
                            owner.cancel();
                        }
                    }
                    Some(Ok((local_actor, Err(error)))) => {
                        let actor = local_actor.identity();
                        if let Some(owner) = application_owners.lock().get_mut(&actor) {
                            owner.launch = HostLaunchState::Failed(error.detail.clone());
                            owner.cancel();
                        }
                        if let Err(error) = apply_application_failure(
                            local_actor,
                            ExternalApplicationFailure {
                                class: error.operation.failure_class(),
                                detail: error.detail,
                            }
                        ).await {
                            break Some(error);
                        }
                    }
                    Some(Err(error)) => break Some(format!("interactive launch task: {error}")),
                    None => {}
                }
            }
            discovered = binding_discoveries.join_next(), if !binding_discoveries.is_empty() => {
                match discovered {
                    Some(Ok((actor, Ok(thread)))) => {
                        let Some(deployment) = deployments.iter_mut().find(|app| app.actor == actor) else {
                            continue;
                        };
                        if !matches!(deployment.connection, InteractiveConnection::AwaitingBinding) {
                            break Some(format!("interactive application {actor:?} published more than one conversation binding"));
                        }
                        if let Err(error) = launch_context.actor_recovery.bind_application(
                            actor,
                            thread.id().0.clone(),
                        ) {
                            break Some(format!(
                                "interactive application {actor:?} binding could not be journalled: {error}"
                            ));
                        }
                        if let Err(error) = retain_input_custody_and_bind(
                            &deployment.service,
                            Arc::clone(&backend),
                            &thread,
                            &deployment.input_producer,
                        )
                        .await
                        {
                            break Some(format!(
                                "interactive application {actor:?} {error}"
                            ));
                        }
                        let (delivery_shutdown, stop_delivery) = oneshot::channel();
                        let delivery = tokio::spawn(run_delivery_pump(
                            actor,
                            Arc::clone(&deployment.inbox),
                            thread.clone(),
                            Arc::clone(&backend),
                            deployment.input_producer.clone(),
                            Arc::clone(&deployment.update_reconciliations),
                            deployment.workspace.clone(),
                            deployment.runtime_observation.clone(),
                            Arc::clone(&watch_retention),
                            Arc::clone(&watch_observation),
                            source_layers.clone(),
                            worktrees.clone(),
                            stop_delivery,
                        ));
                        tracing::info!(
                            ?actor,
                            input_producer = deployment.input_producer.as_str(),
                            "installed run-scoped native input producer"
                        );
                        deployment.connection = InteractiveConnection::Bound {
                            delivery_shutdown,
                            delivery,
                        };
                        deployment.thread = Some(thread.clone());
                        if let Some(owner) = application_owners.lock().get_mut(&actor) {
                            owner.creator_workspace = Some(BoundWorkspace {
                                workspace: deployment.active_workspace.clone(), thread: thread.clone(),
                            });
                        }
                        if actor == root_identity {
                            readiness.send(ActorHostReadiness::Ready {
                                root: root_identity,
                                thread,
                            }).ok();
                        }
                    }
                    Some(Ok((actor, Err(error)))) => {
                        let Some(deployment) = deployments.iter_mut().find(|app| app.actor == actor) else {
                            continue;
                        };
                        if let Some(gate) = &deployment.fork_gate {
                            // best-effort: the fork group may already be resolved
                            // by a concurrent path; nothing more to do here.
                            gate.mark_failed().ok();
                        }
                        deployment.failure_reported = true;
                        let Some(local_actor) = deployments
                            .iter()
                            .find(|application| application.actor == actor)
                            .map(|application| application.local_actor.clone())
                        else {
                            break Some(format!("lost exact local actor for failed application {actor:?}"));
                        };
                        if let Err(error) = apply_application_failure(
                            local_actor,
                            ExternalApplicationFailure {
                                class: error.operation.failure_class(),
                                detail: error.detail,
                            }
                        ).await {
                            break Some(error);
                        }
                    }
                    Some(Err(error)) => break Some(format!("interactive binding task: {error}")),
                    None => {}
                }
            }
            retired = retirements.join_next(), if !retirements.is_empty() => {
                match retired {
                    Some(Ok(receipt)) => {
                        let supervisor = application_owners.lock().get_mut(&receipt.actor).and_then(|owner| {
                            owner.retirement.lock().get_or_insert_with(|| receipt.clone());
                            owner.supervisor
                        });
                        let degraded = receipt.degraded();
                        if degraded {
                            tracing::warn!(actor = ?receipt.actor, components = ?receipt.components, "interactive application cleanup degraded");
                        } else {
                            tracing::info!(actor = ?receipt.actor, "interactive application retired");
                        }
                        // A supervisor that received the release in its stop
                        // receipt needs no notice. One that stopped waiting
                        // (its reply dropped) is told either way, so a
                        // `StoppedReleasing` receipt always gets its ending.
                        let waiters = release_waiters.remove(&receipt.actor).unwrap_or_default();
                        let waited = !waiters.is_empty();
                        let mut answered = false;
                        for waiter in waiters {
                            answered |= waiter.answer(receipt.release());
                        }
                        let notify = if waited { !answered } else { degraded };
                        if notify {
                            if let Some(supervisor) = supervisor {
                                if let Some(application) = deployments.iter().find(|app| app.actor == supervisor) {
                                    notifications.spawn(publish_inbox_event_for(
                                        supervisor,
                                        Arc::clone(&application.inbox),
                                        DurableActorEvent::Typed(TypedActorEvent::CleanupFinished { receipt }),
                                    ));
                                }
                            }
                        }
                    }
                    Some(Err(error)) => {
                        tracing::warn!(%error, "interactive retirement task join failed after cleanup isolation");
                    }
                    None => {}
                }
            }
            notified = notifications.join_next(), if !notifications.is_empty() => {
                match notified {
                    Some(Ok((_actor, Ok(())))) => {}
                    Some(Ok((actor, Err(error)))) => {
                        tracing::warn!(?actor, %error, "actor notification delivery degraded");
                        let Some(local_actor) = deployments
                            .iter()
                            .find(|application| application.actor == actor)
                            .map(|application| application.local_actor.clone())
                        else {
                            continue;
                        };
                        if let Err(error) = apply_application_failure(
                            local_actor,
                            ExternalApplicationFailure {
                                class: ExternalApplicationFailureClass::ToolHostStartup,
                                detail: error,
                            },
                        )
                        .await
                        {
                            break Some(error);
                        }
                    }
                    Some(Err(error)) => {
                        tracing::warn!(%error, "actor notification task join failed");
                    }
                    None => {}
                }
            }
        }
    };

    let native_retirement = if failure.is_some() {
        NativeRetirement::Preserve
    } else {
        shutdown.borrow().unwrap_or_default()
    };
    for owner in application_owners.lock().values_mut() {
        owner.native_retirement = native_retirement;
        owner.cancel();
    }
    let launch_cleanup =
        drain_launches_for_shutdown(&mut launches, APPLICATION_SHUTDOWN_TIMEOUT).await;
    deployments.extend(
        launch_cleanup
            .completed
            .into_iter()
            .map(|launched| launched.deployment),
    );
    let launch_failure = if launch_cleanup.failures.is_empty() {
        None
    } else {
        Some(
            launch_cleanup
                .failures
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>()
                .join("; "),
        )
    };
    let publication_cleanup = tokio::time::timeout(APPLICATION_SHUTDOWN_TIMEOUT, async {
        let mut failure = None;
        while let Some(result) = publication_retries.join_next().await {
            if let Err(error) = result {
                failure.get_or_insert_with(|| format!("build publication recovery task: {error}"));
            }
        }
        failure
    })
    .await
    .unwrap_or_else(|_| {
        publication_retries.abort_all();
        Some("build publication recovery timed out; durable state and storage retained".into())
    });
    binding_discoveries.abort_all();
    while binding_discoveries.join_next().await.is_some() {}
    let undeployed = application_owners
        .lock()
        .keys()
        .copied()
        .filter(|actor| {
            !deployments
                .iter()
                .any(|deployment| deployment.actor == *actor)
        })
        .collect::<Vec<_>>();
    for actor in undeployed {
        spawn_undeployed_hosted_retirement(&mut retirements, actor, &application_owners, &tmux);
    }
    for deployment in deployments {
        spawn_owned_retirement(
            &mut retirements,
            deployment,
            tmux.clone(),
            &application_owners,
        );
    }
    let notification_cleanup = tokio::time::timeout(APPLICATION_SHUTDOWN_TIMEOUT, async {
        let mut failure = None;
        while let Some(result) = notifications.join_next().await {
            let result = result
                .map_err(|error| format!("owner notification task: {error}"))
                .and_then(|(_actor, result)| result);
            if let Err(error) = result {
                failure.get_or_insert(error);
            }
        }
        failure
    })
    .await
    .unwrap_or_else(|_| Some("owner notification cleanup timed out".into()));
    let cleanup_failure = tokio::time::timeout(APPLICATION_SHUTDOWN_TIMEOUT, async {
        let mut failure = None;
        while let Some(result) = retirements.join_next().await {
            if let Ok(receipt) = &result {
                if let Some(owner) = application_owners.lock().get_mut(&receipt.actor) {
                    owner.retirement.lock().get_or_insert_with(|| receipt.clone());
                }
            }
            match result {
                Ok(receipt) if receipt.degraded() && receipt.actor == root_identity => {
                    failure.get_or_insert_with(|| receipt.render());
                }
                Ok(receipt) if receipt.degraded() => {
                    tracing::warn!(actor = ?receipt.actor, components = ?receipt.components, "child cleanup degraded during host shutdown");
                }
                Ok(_) => {}
                Err(error) => {
                    failure.get_or_insert_with(|| format!("interactive retirement task: {error}"));
                }
            }
        }
        failure
    })
    .await
    .unwrap_or_else(|_| Some("interactive application cleanup timed out".into()));
    let cleanup_failures = [
        launch_failure,
        publication_cleanup,
        notification_cleanup,
        cleanup_failure,
    ]
    .into_iter()
    .flatten()
    .collect::<Vec<_>>();
    let cleanup_failure = if cleanup_failures.is_empty() {
        None
    } else {
        Some(cleanup_failures.join("; "))
    };
    match (failure, cleanup_failure) {
        (Some(error), Some(cleanup)) => Err(format!("{error}; cleanup: {cleanup}")),
        (Some(error), None) | (None, Some(error)) => Err(error),
        (None, None) => Ok(()),
    }
}

struct LaunchShutdown<T> {
    completed: Vec<T>,
    failures: Vec<LaunchShutdownFailure>,
}

#[derive(Debug, thiserror::Error)]
enum LaunchShutdownFailure {
    #[error("interactive launch cleanup failed: {0}")]
    Launch(InteractiveApplicationError),
    #[error("interactive launch join failed; cleanup unconfirmed: {0}")]
    Join(tokio::task::JoinError),
    #[error("interactive launch cleanup timed out with {pending} unsettled tasks; abort requested, cleanup unconfirmed")]
    TimedOut { pending: usize },
}

impl<T> LaunchShutdown<T> {
    fn record<A>(
        &mut self,
        result: Result<(A, Result<Option<T>, InteractiveApplicationError>), tokio::task::JoinError>,
    ) {
        match result {
            Ok((_, Ok(Some(completed)))) => self.completed.push(completed),
            Ok((_, Ok(None))) => {}
            Ok((_, Err(error))) => self.failures.push(LaunchShutdownFailure::Launch(error)),
            Err(error) => self.failures.push(LaunchShutdownFailure::Join(error)),
        }
    }
}

/// Keep observed completed deployments outside timeout-owned futures so they can
/// still be retired if a later launch fails or cannot finish. Aborting a task is
/// not proof that its process, hosted work or resource cleanup completed.
async fn drain_launches_for_shutdown<A: Send + 'static, T: Send + 'static>(
    launches: &mut JoinSet<(A, Result<Option<T>, InteractiveApplicationError>)>,
    grace: Duration,
) -> LaunchShutdown<T> {
    let mut outcome = LaunchShutdown {
        completed: Vec::new(),
        failures: Vec::new(),
    };
    let deadline = tokio::time::Instant::now() + grace;
    while !launches.is_empty() {
        match tokio::time::timeout_at(deadline, launches.join_next()).await {
            Ok(Some(result)) => outcome.record(result),
            Ok(None) => break,
            Err(_) => {
                outcome.failures.push(LaunchShutdownFailure::TimedOut {
                    pending: launches.len(),
                });
                launches.abort_all();
                // Preserve results already ready at the cutoff. Do not wait
                // indefinitely for cancellation or label it cleanup success.
                while let Some(result) = launches.try_join_next() {
                    outcome.record(result);
                }
                break;
            }
        }
    }
    outcome
}

struct InteractiveInheritance {
    thread: Option<BackendThreadId>,
    build_snapshot: Option<OverlaySnapshot>,
}

struct InteractiveLaunchRetention {
    hosted: hosted_retirement::HostedSlot,
    pane: Arc<Mutex<Option<TmuxPaneId>>>,
    process: Arc<Mutex<scoped_custody::ScopedProcessSlot>>,
}

async fn launch_interactive_application(
    installation: LocalResidentInstallation,
    context: InteractiveLaunchContext,
    cancelled: oneshot::Receiver<NativeRetirement>,
    inherited: InteractiveInheritance,
    retention: InteractiveLaunchRetention,
) -> Result<Option<LaunchedInteractiveApplication>, InteractiveApplicationError> {
    let worktree = prepare_actor_worktree(&installation, &context)?;
    launch_prepared_interactive_application(
        installation,
        context,
        worktree,
        cancelled,
        inherited,
        retention,
    )
    .await
}

fn prepare_actor_worktree(
    installation: &LocalResidentInstallation,
    context: &InteractiveLaunchContext,
) -> Result<Option<WorktreeHandle>, InteractiveApplicationError> {
    let actor = installation.actor.identity();
    let raw_id =
        match actor_workspace_request(actor == context.root, &installation.launch_worktrees)
            .map_err(|detail| {
                application_error(actor, InteractiveOperation::BindWorktree, detail)
            })? {
            ActorWorkspaceRequest::SourceCheckout => return Ok(None),
            ActorWorkspaceRequest::Worktree(raw_id) => raw_id,
        };
    if !WorktreeId::is_path_safe(raw_id) {
        return Err(application_error(
            actor,
            InteractiveOperation::BindWorktree,
            "the worktree recipe carried an invalid durable id",
        ));
    }
    let id = WorktreeId::from_raw(raw_id);
    let handle = context
        .worktrees
        .lookup(&id)
        .map_err(|error| application_error(actor, InteractiveOperation::BindWorktree, error))?
        .ok_or_else(|| {
            application_error(
                actor,
                InteractiveOperation::BindWorktree,
                format!("worktree {id} is not registered"),
            )
        })?;
    let principal = WorktreePrincipal::exact_actor(
        &runtime_namespace(&context.run_root),
        actor.id.0,
        actor.incarnation.0,
    );
    if installation.worktree_custody.is_none()
        || context
            .bindings
            .lock()
            .current(handle.id())
            .is_none_or(|binding| binding.agent() != &principal)
    {
        return Err(application_error(
            actor,
            InteractiveOperation::BindWorktree,
            "exact pre-bootstrap worktree custody is absent",
        ));
    }
    Ok(Some(handle))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ActorWorkspaceRequest<'a> {
    /// The actor may inspect the source checkout. Only the root receives write
    /// authority for it; an ordinary actor without a worktree remains a useful
    /// orchestration or review actor rather than acquiring ambient code-write
    /// authority by accident.
    SourceCheckout,
    Worktree(&'a str),
}

fn actor_workspace_request<'a>(
    root: bool,
    launch_worktrees: &'a [String],
) -> Result<ActorWorkspaceRequest<'a>, String> {
    match (root, launch_worktrees) {
        (_, []) => Ok(ActorWorkspaceRequest::SourceCheckout),
        (false, [worktree]) => Ok(ActorWorkspaceRequest::Worktree(worktree)),
        (true, _) => Err("the root application may not carry a child worktree recipe".into()),
        (false, worktrees) => Err(format!(
            "an interactive actor may carry at most one worktree recipe, received {}",
            worktrees.len()
        )),
    }
}

fn current_time_ms() -> i64 {
    let millis = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis();
    i64::try_from(millis).unwrap_or(i64::MAX)
}

async fn launch_prepared_interactive_application(
    installation: LocalResidentInstallation,
    context: InteractiveLaunchContext,
    worktree: Option<WorktreeHandle>,
    mut cancelled: oneshot::Receiver<NativeRetirement>,
    inherited: InteractiveInheritance,
    retention: InteractiveLaunchRetention,
) -> Result<Option<LaunchedInteractiveApplication>, InteractiveApplicationError> {
    let InteractiveInheritance {
        thread: fork_parent_thread,
        build_snapshot,
    } = inherited;
    let InteractiveLaunchRetention {
        hosted: hosted_slot,
        pane: pane_slot,
        process: scope_slot,
    } = retention;
    let InteractiveLaunchContext {
        base_prompt,
        root,
        config,
        run_root,
        tmux,
        backend,
        worktrees,
        bindings: _,
        actor_recovery,
        recovered_threads,
    } = context;
    let actor = installation.actor;
    let fork_gate = installation.fork_gate.clone();
    let runtime_observation = installation.runtime_observation.clone();
    let actor_identity = actor.identity();
    let _resource_start = match &config.command_resources {
        Some(owner) => {
            tracing::info!(actor = ?actor_identity, "actor waiting for resource admission");
            runtime_observation.publish_launch_pending("waiting for memory admission");
            let admitted = Some(tokio::select! {
                result = owner.admit_actor() => result.map_err(|error| application_error(actor_identity, InteractiveOperation::LaunchProcess, error.to_string()))?,
                _ = &mut cancelled => return Ok(None),
            });
            tracing::info!(actor = ?actor_identity, "actor resource admission granted");
            runtime_observation.publish_launch_pending("starting the provider");
            admitted
        }
        None => None,
    };
    let workspace = worktree.as_ref().map_or_else(
        || config.workspace.clone(),
        |handle| handle.cwd().to_path_buf(),
    );
    let actor_root = run_root.join(format!(
        "{}-{}",
        actor_identity.id.0, actor_identity.incarnation.0
    ));
    std::fs::create_dir_all(&actor_root).map_err(|error| {
        application_error(actor_identity, InteractiveOperation::PrepareRuntime, error)
    })?;
    let agent_workspace = PathBuf::from(ACTOR_PROJECT_ROOT);
    if cancelled.try_recv().is_ok() {
        return Ok(None);
    }
    let prepared_workspace = installation
        .worktree_custody
        .as_ref()
        .and_then(|custody| {
            (custody.as_ref() as &dyn std::any::Any).downcast_ref::<ActorWorkspaceCustody>()
        })
        .and_then(|custody| custody.workspace.clone());
    let prepared_workspace = match prepared_workspace {
        Some(prepared) => prepared,
        None => {
            let layout = WorkspaceLayout {
                run_namespace: runtime_namespace(&run_root),
                source_root: config.workspace.clone(),
                source_exclude: config.source_exclude.clone(),
                root_imports: Arc::default(),
                worktrees: worktrees.clone(),
                base_prompt: base_prompt.clone(),
                backend: backend.clone(),
            };
            let host_path = workspace.clone();
            let id = worktree.as_ref().map(|tree| tree.id().clone());
            let key = id
                .as_ref()
                .map(|id| id.as_str().to_owned())
                .unwrap_or_else(|| {
                    format!(
                        "actor-{}-{}",
                        actor_identity.id.0, actor_identity.incarnation.0
                    )
                });
            let policy = exomonad_actor::ForkWorkspacePolicy {
                native_tools: installation.effective_role.native_tools(),
                workspace: installation.effective_role.workspace(),
            };
            tidepool_runtime::spawn_blocking_in_span(move || {
                layout.prepare(
                    host_path,
                    id,
                    &key,
                    actor_identity == root,
                    policy,
                    None,
                    build_snapshot,
                )
            })
            .await
            .map_err(|error| {
                application_error(actor_identity, InteractiveOperation::PrepareRuntime, error)
            })?
            .map_err(|error| {
                application_error(actor_identity, InteractiveOperation::PrepareRuntime, error)
            })?
        }
    };
    let workspace_view = prepared_workspace.view.clone();
    let build_output = prepared_workspace
        .build
        .as_ref()
        .map(|_| agent_workspace.join(ACTOR_BUILD_TARGET));
    let run_socket_id = run_root
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("run");
    let socket_root = std::env::temp_dir().join(format!(
        "tidepool-{}-{}-{}",
        &run_socket_id[..run_socket_id.len().min(8)],
        actor_identity.id.0,
        actor_identity.incarnation.0
    ));
    let (mut socket_directory, listener, inbox) = prepare_socket_inbox(
        actor_identity,
        socket_root,
        actor_root.join("inbox.jsonl"),
        actor_root.join("inbox.cursor"),
    )?;
    let endpoint = socket_directory.path().join("host-tools.sock");
    let binding_path = if actor_identity == root {
        config.root_binding_path.clone()
    } else {
        actor_root.join("binding.json")
    };
    let accepted_source = active_source_identity(&run_root, config.workspace_inputs.is_some())
        .map_err(|error| {
            application_error(actor_identity, InteractiveOperation::PrepareRuntime, error)
        })?;
    actor_recovery
        .prepare_application(actor_identity, binding_path.clone(), accepted_source)
        .map_err(|error| {
            application_error(actor_identity, InteractiveOperation::PrepareRuntime, error)
        })?;
    let launch_mode = if let Some((_, thread)) = recovered_threads.get(&actor_identity) {
        InteractiveLaunchMode::Resume(thread.id().clone())
    } else if let Some(parent) = fork_parent_thread.clone() {
        let boundary = installation
            .fork_boundary
            .as_ref()
            .filter(|boundary| boundary.thread_id == parent.0 && !boundary.call_id.is_empty())
            .ok_or_else(|| {
                application_error(
                    actor_identity,
                    InteractiveOperation::BuildCommand,
                    "context fork has no matching parent hosted-call boundary",
                )
            })?;
        InteractiveLaunchMode::Fork {
            parent,
            after_call: boundary.call_id.clone(),
        }
    } else if actor_identity == root {
        config.root_launch_mode.clone()
    } else {
        InteractiveLaunchMode::Fresh
    };
    let expected_resume = match &launch_mode {
        InteractiveLaunchMode::Resume(thread) => Some(thread.clone()),
        InteractiveLaunchMode::Fresh | InteractiveLaunchMode::Fork { .. } => None,
    };
    runtime_observation.publish_cache_boundary(match &launch_mode {
        InteractiveLaunchMode::Fresh => exomonad_actor::CacheBoundaryReason::Fresh,
        InteractiveLaunchMode::Fork { .. } => exomonad_actor::CacheBoundaryReason::ForkedPrefix,
        InteractiveLaunchMode::Resume(_) => exomonad_actor::CacheBoundaryReason::ReattachedThread,
    });
    let workspace_observation = exomonad_actor::ActorWorkspaceObservation {
        workspace_path: agent_workspace.clone(),
        host_storage_path: workspace.clone(),
        worktree_id: installation.launch_worktrees.first().cloned(),
        expected_branch: worktree
            .as_ref()
            .map(|tree| tree.branch().as_str().to_owned()),
    };
    runtime_observation.publish_workspace(workspace_observation);
    runtime_observation.publish_launch_role(installation.effective_role.clone(), current_time_ms());
    let resolved_worker = installation
        .creator
        .map(|_| {
            resolve_worker_launch(
                &config,
                &exomonad_actor::WorkerLaunchRequest {
                    role: installation.effective_role.clone(),
                    model: installation.model.clone(),
                    effort: installation.fork_effort,
                    context: if matches!(launch_mode, InteractiveLaunchMode::Fork { .. }) {
                        exomonad_actor::ForkContext::InheritedContext
                    } else {
                        exomonad_actor::ForkContext::SelectedContext
                    },
                    instructions: installation.instructions.clone(),
                },
                blake3::hash(base_prompt.body().as_bytes())
                    .to_hex()
                    .as_ref(),
            )
        })
        .transpose()
        .map_err(|detail| {
            application_error(actor_identity, InteractiveOperation::BuildCommand, detail)
        })?;
    let developer_instructions = if let Some(resolved) = &resolved_worker {
        resolved.instructions.clone()
    } else {
        let mut instructions = developer_instructions_selected(
            &installation.effective_role,
            &launch_mode,
            config.workspace_inputs.as_ref(),
            installation.instructions.as_deref(),
        );
        append_inheritance_authority(&mut instructions);
        instructions
    };
    let mut developer_instructions = developer_instructions;
    if let Some(notice) = installation
        .worktree_custody
        .as_ref()
        .and_then(|custody| {
            (custody.as_ref() as &dyn std::any::Any).downcast_ref::<ActorWorkspaceCustody>()
        })
        .and_then(|custody| custody.inheritance_notice.as_ref())
    {
        developer_instructions.push('\n');
        developer_instructions.push_str(notice);
    }
    let developer_instructions =
        orient_launch_instructions(&developer_instructions, &runtime_observation.snapshot());
    runtime_observation.publish_prompt_profile(
        installation.effective_role.prompt_profile(),
        PromptId::CATALOG_VERSION,
        PromptId::composed_fingerprint(
            base_prompt.body(),
            &developer_instructions,
            &exomonad_actor::exomonad_hosted_prompt_fingerprint(),
        ),
    );
    let (model, effort) = if let Some(resolved) = resolved_worker {
        (
            resolved.model,
            launch_effort(&launch_mode, ReasoningEffort::Low, Some(resolved.effort)),
        )
    } else {
        (
            installation
                .model
                .as_ref()
                .map(|model| model.value().to_owned())
                .or_else(|| {
                    (!matches!(launch_mode, InteractiveLaunchMode::Fork { .. }))
                        .then(|| config.model.clone())
                }),
            launch_effort(&launch_mode, config.effort, installation.fork_effort),
        )
    };
    let recovery_notice = matches!(launch_mode, InteractiveLaunchMode::Resume(_))
        .then(|| std::fs::read_to_string(config.run_root.join("host-recovery-notice.txt")).ok())
        .flatten();
    let spec = InteractiveAgentSpec {
        shell_tools: exomonad_agent::InteractiveShellTools::Hosted,
        mode: launch_mode,
        // Exomonad owns continuation on every node. Keep the native tool surface
        // identical across roots and forks, without inheriting native goals.
        goal_policy: exomonad_agent::InteractiveGoalPolicy::Disabled,
        model,
        effort: Some(effort),
        developer_instructions,
        base_instructions_file: base_prompt.file().to_path_buf(),
        initial_prompt: recovery_notice.or_else(|| installation.initial_user_message.clone()),
        native_sandbox: InteractiveNativeSandbox::HostMountBoundary,
        host_tools_socket: endpoint.clone(),
    };
    let command = backend.render(&spec).map_err(|error| {
        application_error(actor_identity, InteractiveOperation::BuildCommand, error)
    })?;
    runtime_observation.publish_backend_provenance(
        config
            .interactive_agent
            .executable()
            .to_string_lossy()
            .into_owned(),
        config.interactive_agent.version().into(),
        spec.model.clone(),
        spec.effort.map(|effort| format!("{effort:?}")),
    );
    let command = ProcessInvocation {
        program: command.program,
        args: command.args,
    };
    // Accepted hosted work may outlive listener cancellation. Retention starts
    // before either hosted submission or native process submission can occur.
    socket_directory.work_may_exist();
    let service = hosted_retirement::start_with_resources(
        &hosted_slot,
        actor.clone(),
        installation
            .policy
            .tools()
            .iter()
            .filter(|tool| tool.name() != exomonad_actor::HASKELL_TOOL)
            .cloned()
            .collect(),
        binding_path.clone(),
        expected_resume.clone(),
        listener,
        config.command_resources.clone().map(|r| {
            (
                r,
                format!("{}-{}", actor_identity.id.0, actor_identity.incarnation.0),
            )
        }),
        hosted_operation_journal(&run_root, actor_identity.id),
    )
    .map_err(|error| {
        application_error(actor_identity, InteractiveOperation::ServeToolHost, error)
    })?;
    let retirement_service = service.clone();
    let launch_result = async {
    if cancelled.try_recv().is_ok() {
        return Err(socket_launch_failure(
            actor_identity,
            InteractiveOperation::LaunchProcess,
            "launch cancelled after hosted work submission",
            socket_directory,
        ));
    }
    let mut launch_environment = actor_launch_environment(
        config.pane_environment.clone(),
        actor_identity == root,
        build_output.as_deref(),
    );
    // Shell commands must resolve the same verified installation as delivery.
    // An inherited PATH or override can name a different rollout protocol.
    launch_environment.set.insert(
        "EXOMONAD_INTERACTIVE_CODEX_BIN".into(),
        config
            .interactive_agent
            .executable()
            .to_string_lossy()
            .into_owned(),
    );
    if let Some(owner) = &config.command_resources {
        let actor_key = format!("{}-{}", actor_identity.id.0, actor_identity.incarnation.0);
        let directory = owner.actor_directory(&actor_key).await.map_err(|error| {
            application_error(
                actor_identity,
                InteractiveOperation::LaunchProcess,
                error.to_string(),
            )
        })?;
        launch_environment.set.insert(
            "CODEX_COMMAND_RESOURCE_SOCKET".into(),
            endpoint.to_string_lossy().into_owned(),
        );
        launch_environment.set.insert(
            "CODEX_COMMAND_WRITER_CGROUP".into(),
            directory.to_string_lossy().into_owned(),
        );
    }
    let supervisor_directory = socket_directory.path().join("process-supervisor");
    std::fs::create_dir(&supervisor_directory).map_err(|error| {
        application_error(
            actor_identity,
            InteractiveOperation::PrepareRuntime,
            format!("cannot reserve private process supervisor directory: {error}"),
        )
    })?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(
            &supervisor_directory,
            std::fs::Permissions::from_mode(0o700),
        )
        .map_err(|error| {
            application_error(
                actor_identity,
                InteractiveOperation::PrepareRuntime,
                format!("cannot protect process supervisor directory: {error}"),
            )
        })?;
    }
    let supervisor_directory = std::fs::canonicalize(&supervisor_directory).map_err(|error| {
        application_error(actor_identity, InteractiveOperation::PrepareRuntime, error)
    })?;
    let launch_id = format!(
        "actor-{}-{}-{}",
        actor_identity.id.0,
        actor_identity.incarnation.0,
        uuid::Uuid::new_v4().simple()
    );
    let pairing_secret = fresh_process_supervisor_secret();
    let recovery_secret = fresh_process_supervisor_secret();
    let bubblewrap = resolve_scope_bubblewrap(&launch_environment.set).map_err(|error| {
        application_error(actor_identity, InteractiveOperation::PrepareRuntime, error)
    })?;
    let boundary = ProcessMountBoundary::new(&workspace, [workspace.clone()], [workspace.clone()])
        .map_err(|error| {
            application_error(actor_identity, InteractiveOperation::PrepareRuntime, error)
        })?;
    let mut manifest = ProcessSupervisorManifest::new(
        launch_id.clone(),
        pairing_secret.clone(),
        recovery_secret.clone(),
        supervisor_directory,
        bubblewrap,
        boundary,
        command,
        ServiceEnvironment {
            set: launch_environment.set.clone(),
            unset: launch_environment.unset.clone(),
        },
    )
    .map_err(|error| {
        application_error(actor_identity, InteractiveOperation::PrepareRuntime, error)
    })?;
    let supervisor_socket = manifest.socket_path();
    manifest.retained_view = Some(exomonad_node::RetainedProcessView {
        entry: workspace_view.entry().map_err(|error| {
            application_error(actor_identity, InteractiveOperation::BuildCommand, error)
        })?,
        directory: agent_workspace.clone(),
    });
    let manifest_path = manifest.write_new().map_err(|error| {
        application_error(actor_identity, InteractiveOperation::PrepareRuntime, error)
    })?;
    tidepool_atomic_write::write_durable(
        &actor_root.join(PROCESS_RECOVERY_RECORD),
        &serde_json::to_vec_pretty(&ProcessRecoveryRecord {
            version: 1,
            launch_id: launch_id.clone(),
            recovery_secret: recovery_secret.clone(),
            supervisor_socket: supervisor_socket.clone(),
            socket_root: socket_directory.path().to_path_buf(),
            retired: false,
        })
        .map_err(|error| {
            application_error(
                actor_identity,
                InteractiveOperation::PrepareRuntime,
                error,
            )
        })?,
    )
    .map_err(|error| {
        application_error(
            actor_identity,
            InteractiveOperation::PrepareRuntime,
            error,
        )
    })?;
    scoped_custody::stage_supervisor(
        &scope_slot,
        scoped_custody::SupervisorRecoveryKey::new(
            supervisor_socket.clone(),
            launch_id.clone(),
            recovery_secret,
        ),
    )
    .map_err(|error| {
        application_error(actor_identity, InteractiveOperation::PrepareRuntime, error)
    })?;
    let supervisor_command = exomonad_node::ProcessInvocation {
        program: config.exomonad_executable.to_string_lossy().into_owned(),
        args: vec![
            "process-supervisor".into(),
            "--manifest".into(),
            manifest_path.to_string_lossy().into_owned(),
        ],
    };
    let supervisor_command = match &config.systemd_slice {
        Some(slice) => {
            slice.scope(slice.verified_command(&config.exomonad_executable, supervisor_command))
        }
        None => supervisor_command,
    };
    let pane = match tokio::time::timeout(
        PROCESS_OPERATION_TIMEOUT,
        tmux.spawn_window(&TmuxLaunch {
            // The full actor path, so a tmux window reads the same as the
            // path in logs, activation messages and sibling rosters.
            window_name: format!(
                "{} [{}@{}]",
                installation.label, actor_identity.id.0, actor_identity.incarnation.0
            ),
            cwd: workspace.clone(),
            program: supervisor_command.program,
            args: supervisor_command.args,
            environment: launch_environment.set,
            unset_environment: launch_environment.unset,
        }),
    )
    .await
    {
        Ok(Ok(pane)) => pane,
        Ok(Err(error)) => {
            return Err(socket_launch_failure(
                actor_identity,
                InteractiveOperation::LaunchProcess,
                error,
                socket_directory,
            ));
        }
        Err(_) => {
            return Err(socket_launch_failure(
                actor_identity,
                InteractiveOperation::LaunchProcess,
                format!("tmux launch exceeded {PROCESS_OPERATION_TIMEOUT:?}"),
                socket_directory,
            ));
        }
    };

    *pane_slot.lock() = Some(pane.clone());
    let activation_slot = scope_slot.clone();
    let activation_socket = supervisor_socket.clone();
    let activation_launch = launch_id.clone();
    let activation_cancelled = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let activation_cancelled_worker = activation_cancelled.clone();
    // Cancellation and release share one linearization point. If cancellation
    // acquires it first, the worker cannot submit release; if release acquires
    // it first, later cancellation is retirement of an already committed
    // launch rather than a pre-release loss.
    let release_gate = Arc::new(std::sync::Mutex::new(()));
    let release_gate_worker = release_gate.clone();
    let activation_workspace = prepared_workspace.clone();
    let activation_worktrees = worktrees.clone();
    let mut activation_task = tidepool_runtime::spawn_blocking_in_span(move || {
        let deadline = std::time::Instant::now() + PROCESS_OPERATION_TIMEOUT;
        while !activation_socket.exists() {
            if std::time::Instant::now() >= deadline {
                return Err(scoped_custody::ScopedProcessError::WrongPhase);
            }
            #[allow(
                clippy::disallowed_methods,
                reason = "dedicated blocking-pool thread (spawn_blocking_in_span), not async context"
            )]
            std::thread::sleep(Duration::from_millis(10));
        }
        let remaining = deadline
            .checked_duration_since(std::time::Instant::now())
            .ok_or(scoped_custody::ScopedProcessError::WrongPhase)?;
        let (client, observation) = ProcessSupervisorClient::pair(
            activation_socket,
            activation_launch,
            pairing_secret,
            remaining,
        )?;
        scoped_custody::install_supervisor(&activation_slot, client, observation)?;
        if activation_cancelled_worker.load(std::sync::atomic::Ordering::Acquire) {
            return Err(scoped_custody::ScopedProcessError::WrongPhase);
        }
        if scoped_custody::prepare_supervisor_slot(&activation_slot, deadline)?
            != scoped_custody::ScopedProcessObservation::Blocked
        {
            return Err(scoped_custody::ScopedProcessError::WrongPhase);
        }
        if activation_cancelled_worker.load(std::sync::atomic::Ordering::Acquire) {
            return Err(scoped_custody::ScopedProcessError::WrongPhase);
        }
        if scoped_custody::pin_supervisor_slot(&activation_slot, deadline)?
            != scoped_custody::ScopedProcessObservation::Pinned
        {
            return Err(scoped_custody::ScopedProcessError::WrongPhase);
        }
        if activation_cancelled_worker.load(std::sync::atomic::Ordering::Acquire) {
            return Err(scoped_custody::ScopedProcessError::WrongPhase);
        }
        let _release = release_gate_worker
            .lock()
            .map_err(|_| scoped_custody::ScopedProcessError::WrongPhase)?;
        if activation_cancelled_worker.load(std::sync::atomic::Ordering::Acquire) {
            return Err(scoped_custody::ScopedProcessError::WrongPhase);
        }
        let view = scoped_custody::supervisor_workspace(&activation_slot, deadline)?;
        let active = activation_workspace.activate(&activation_worktrees, view)?;
        match scoped_custody::release_supervisor_slot(&activation_slot, deadline)? {
            scoped_custody::ScopedProcessObservation::Released => Ok(active),
            // A committed but unconfirmed release is never retried. Retain the
            // row for explicit recovery rather than publishing readiness.
            scoped_custody::ScopedProcessObservation::ReleaseUnconfirmed => {
                Err(scoped_custody::ScopedProcessError::WrongPhase)
            }
            _ => Err(scoped_custody::ScopedProcessError::WrongPhase),
        }
    });
    let (activation, activation_retirement) = tokio::select! {
        result = &mut activation_task => (result, None),
        retirement = &mut cancelled => {
            let _release = release_gate.lock().map_err(|_| {
                application_error(
                    actor_identity,
                    InteractiveOperation::LaunchProcess,
                    "process supervisor release gate poisoned",
                )
            })?;
            activation_cancelled.store(true, std::sync::atomic::Ordering::Release);
            let requested = retirement.unwrap_or(NativeRetirement::Preserve);
            drop(_release);
            let activation = activation_task.await;
            (activation, Some(requested))
        }
    };
    if let Some(native_retirement) = activation_retirement {
        let process = retire_scoped_process(Some(scope_slot.clone()), native_retirement).await;
        if matches!(process, Some(CleanupComponentOutcome::Completed)) {
            if let CleanupComponentOutcome::Failed { detail } =
                retire_pane_artifact(&tmux, &pane, native_retirement).await
            {
                tracing::warn!(actor = ?actor_identity, %detail, "cannot retire actor pane after cancelled launch");
            }
        }
        return Err(socket_launch_failure(
            actor_identity,
            InteractiveOperation::LaunchProcess,
            "launch cancelled during exact process activation",
            socket_directory,
        ));
    }
    let active_workspace = activation
        .map_err(|error| {
            application_error(
                actor_identity,
                InteractiveOperation::LaunchProcess,
                format!("process supervisor activation task failed: {error}"),
            )
        })?
        .map_err(|error| {
            application_error(actor_identity, InteractiveOperation::LaunchProcess, error)
        })?;
    if let Err(error) = tmux.retain_pane_on_exit(&pane).await {
        tracing::warn!(actor = ?actor_identity, %error, "cannot retain actor pane for exit diagnosis; application remains active");
    }

    if actor_identity == root {
        if let Err(error) = tmux.select_window_for_pane(&pane).await {
            tracing::warn!(actor = ?actor_identity, %error, "cannot select root window; application remains active");
        }
    }
    if let Ok(native_retirement) = cancelled.try_recv() {
        let process = retire_scoped_process(Some(scope_slot.clone()), native_retirement).await;
        if matches!(process, Some(CleanupComponentOutcome::Completed)) {
            if let CleanupComponentOutcome::Failed { detail } =
                retire_pane_artifact(&tmux, &pane, native_retirement).await
            {
                tracing::warn!(actor = ?actor_identity, %detail, "cannot retire actor pane after cancelled launch");
            }
        }
        return Err(socket_launch_failure(
            actor_identity,
            InteractiveOperation::LaunchProcess,
            "launch cancelled after native submission",
            socket_directory,
        ));
    }
    tracing::info!(
        actor = ?actor_identity,
        pane = pane.as_str(),
        worktree = worktree
            .as_ref()
            .map(|handle| handle.id().as_str())
            .unwrap_or("source"),
        "interactive application launched"
    );
    let notification_inbox_key = format!(
        "{}:{}:{}",
        runtime_namespace(&run_root),
        actor_identity.id.0,
        actor_identity.incarnation.0
    );
    let input_producer = input_producer_id(&run_root, actor_identity, &notification_inbox_key)
        .map_err(|error| {
            application_error(actor_identity, InteractiveOperation::PrepareRuntime, error)
        })?;
    let binding_control = service.lock().await.control.clone();
    Ok(Some(LaunchedInteractiveApplication {
        deployment: InteractiveDeployment {
            active_workspace,
            supervisor: installation.supervisor_parent,
            notified_provider_failures: Default::default(),
            actor: actor_identity,
            local_actor: actor,
            pane,
            workspace,
            inbox,
            notification_inbox_key,
            input_producer,
            update_reconciliations: Arc::new(Mutex::new(BTreeMap::new())),
            connection: InteractiveConnection::AwaitingBinding,
            service,
            socket_directory,
            process_recovery_record: actor_root.join(PROCESS_RECOVERY_RECORD),
            worktree_custody: installation.worktree_custody.clone(),
            failure_reported: false,
            last_activation_sequence: 0,
            thread: None,
            fork_gate,
            runtime_observation,
            fork_parent_thread,
        },
        binding: InteractiveBindingRequest {
            control: binding_control,
            path: binding_path,
            expected: expected_resume,
        },
    }))
    }
    .await;
    if launch_result.is_err() {
        // best-effort: the launch already failed and `launch_result` below is
        // what's returned; these settle bookkeeping for the retirement
        // service so custody isn't left retained, but a failure here has no
        // separate action to take.
        hosted_retirement::confirm_no_input_producer(&retirement_service)
            .await
            .ok();
        drop(
            hosted_retirement::observe(
                &retirement_service,
                hosted_retirement::CompletionBoundary::AbortForShutdown,
                APPLICATION_TASK_GRACE_TIMEOUT,
            )
            .await,
        );
    }
    launch_result
}

/// Acquire exclusive path custody before the first fallible preparation step.
fn prepare_socket_inbox(
    actor: ActorRef,
    socket_root: PathBuf,
    rows: PathBuf,
    cursor: PathBuf,
) -> Result<(SocketDirectory, UnixListener, Arc<ActorInbox>), InteractiveApplicationError> {
    let socket = SocketDirectory::create(socket_root)
        .map_err(|error| application_error(actor, InteractiveOperation::PrepareRuntime, error))?;
    let prepared: Result<_, InteractiveApplicationError> = (|| {
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(socket.path(), std::fs::Permissions::from_mode(0o700))
                .map_err(|error| {
                    application_error(actor, InteractiveOperation::PrepareRuntime, error)
                })?;
        }
        let listener = UnixListener::bind(socket.path().join("host-tools.sock"))
            .map_err(|error| application_error(actor, InteractiveOperation::BindToolHost, error))?;
        let inbox = ActorInbox::open(rows, cursor).map_err(|error| {
            application_error(actor, InteractiveOperation::PrepareRuntime, error)
        })?;
        Ok((listener, Arc::new(inbox)))
    })();
    match prepared {
        Ok((listener, inbox)) => Ok((socket, listener, inbox)),
        Err(error) => Err(socket_launch_failure(
            actor,
            error.operation,
            error.detail,
            socket,
        )),
    }
}

fn socket_cleanup_outcome(socket: SocketDirectory) -> CleanupComponentOutcome {
    match socket.release() {
        Ok(()) => CleanupComponentOutcome::Completed,
        Err(error) => CleanupComponentOutcome::Failed {
            detail: error.to_string(),
        },
    }
}

fn socket_launch_failure(
    actor: ActorRef,
    operation: InteractiveOperation,
    cause: impl fmt::Display,
    socket: SocketDirectory,
) -> InteractiveApplicationError {
    let detail = match socket_cleanup_outcome(socket) {
        CleanupComponentOutcome::Failed { detail } => {
            format!("{cause}; socket cleanup failed: {detail}")
        }
        _ => cause.to_string(),
    };
    application_error(actor, operation, detail)
}

async fn deliver_session_activation(
    application: &mut InteractiveDeployment,
    activation: exomonad_actor::ResidentActivation,
) -> Result<(), String> {
    let actor = activation.id.actor();
    if accepts_activation(application, &activation) {
        let sequence = activation.id.sequence();
        if let Err(error) = publish_inbox_event(
            Arc::clone(&application.inbox),
            DurableActorEvent::session(&activation),
        )
        .await
        {
            application.failure_reported = true;
            let local_actor = application.local_actor.clone();
            tracing::warn!(?actor, %error, "actor activation delivery degraded");
            apply_application_failure(
                local_actor,
                ExternalApplicationFailure {
                    class: ExternalApplicationFailureClass::ToolHostStartup,
                    detail: error,
                },
            )
            .await?;
            return Ok(());
        }
        application
            .runtime_observation
            .publish_request_activation(activation.request, sequence);
        application.last_activation_sequence = sequence;
    }
    Ok(())
}

fn accepts_activation(
    deployment: &InteractiveDeployment,
    activation: &exomonad_actor::ResidentActivation,
) -> bool {
    accepts_activation_id(
        deployment.actor,
        deployment.last_activation_sequence,
        activation.id.actor(),
        activation.id.sequence(),
    )
}

fn accepts_activation_id(
    actor: ActorRef,
    last_sequence: u64,
    candidate_actor: ActorRef,
    candidate_sequence: u64,
) -> bool {
    candidate_actor == actor && candidate_sequence > last_sequence
}

/// Toolchain processes selected for the source checkout are not valid in a
/// child worktree. Each worker resolves tools from its own sources instead.
fn workspace_local_toolchain_pins(is_root: bool) -> BTreeSet<String> {
    if is_root {
        return BTreeSet::new();
    }

    [
        "TIDEPOOL_EXTRACT",
        "TIDEPOOL_EXTRACT_WORKER",
        "TIDEPOOL_EXTRACT_DAEMON_SOCKET",
    ]
    .into_iter()
    .map(str::to_owned)
    .collect()
}

struct ActorLaunchEnvironment {
    set: BTreeMap<String, String>,
    unset: BTreeSet<String>,
}

/// Compose the host environment and actor-local credentials before handing
/// them to tmux. A worker's explicit unsets win over values captured from the
/// root process; handing the same name to both tmux channels is ambiguous and
/// rejected by the deployment adapter.
fn actor_launch_environment(
    mut inherited: BTreeMap<String, String>,
    is_root: bool,
    build_output: Option<&Path>,
) -> ActorLaunchEnvironment {
    inherited.insert("CODEX_WORKSPACE_SNAPSHOTS".into(), "1".into());
    if let Some(build_output) = build_output {
        inherited.insert(
            "CARGO_TARGET_DIR".into(),
            build_output.to_string_lossy().into_owned(),
        );
        // A host compiler-cache daemon resolves the shared visible pathname in
        // its own mount namespace. Empty overrides also disable Cargo config
        // wrappers, keeping compiler execution inside this actor's workspace.
        inherited.insert("RUSTC_WRAPPER".into(), String::new());
        inherited.insert("RUSTC_WORKSPACE_WRAPPER".into(), String::new());
    }
    let unset = workspace_local_toolchain_pins(is_root);
    inherited.retain(|name, _| !unset.contains(name));
    ActorLaunchEnvironment {
        set: inherited,
        unset,
    }
}

fn fresh_process_supervisor_secret() -> String {
    format!(
        "{}{}",
        uuid::Uuid::new_v4().simple(),
        uuid::Uuid::new_v4().simple()
    )
}

/// Resolve the same bubblewrap selected by the already-frozen pane PATH. The
/// absolute result is written into the immutable launch manifest so the helper
/// never repeats executable selection after the row has been reserved.
fn resolve_scope_bubblewrap(
    environment: &BTreeMap<String, String>,
) -> Result<PathBuf, std::io::Error> {
    use std::os::unix::fs::PermissionsExt;

    let path = environment
        .get("PATH")
        .map(std::ffi::OsString::from)
        .or_else(|| std::env::var_os("PATH"))
        .ok_or_else(|| std::io::Error::new(std::io::ErrorKind::NotFound, "PATH is unset"))?;
    for directory in std::env::split_paths(&path) {
        let candidate = directory.join(BUBBLEWRAP_PROGRAM);
        let Ok(metadata) = candidate.metadata() else {
            continue;
        };
        if metadata.is_file() && metadata.permissions().mode() & 0o111 != 0 {
            return std::fs::canonicalize(candidate);
        }
    }
    Err(std::io::Error::new(
        std::io::ErrorKind::NotFound,
        format!("{BUBBLEWRAP_PROGRAM} is not executable on the frozen pane PATH"),
    ))
}

fn orient_launch_instructions(
    message: &str,
    observation: &exomonad_actor::ActorRuntimeObservation,
) -> String {
    match observation.launch_orientation() {
        Some(orientation) => format!("{message}\n\n{orientation}"),
        None => message.to_owned(),
    }
}

fn admit_notification(command: &exomonad_actor::NotificationSend, key: String, inbox: &ActorInbox) {
    match inbox.publish_tracked(
        DurableActorEvent::Text(command.message().to_owned()),
        DeliveryProvenance::Notification {
            sender: command.owner(),
            target: command.target(),
        },
    ) {
        Ok(row) => command.admitted(key, row.sequence),
        Err(error @ exomonad_node::InboxError::UncertainWrite { .. }) => {
            command.rejected(exomonad_actor::NotificationError::Unconfirmed(
                error.to_string(),
            ));
        }
        Err(error) => command.rejected(exomonad_actor::NotificationError::StorageFailure(
            error.to_string(),
        )),
    }
}

fn observe_notification_receipt(
    command: &exomonad_actor::NotificationPoll,
    target: ActorRef,
    inbox_key: &str,
    inbox: &ActorInbox,
) -> Result<exomonad_actor::NotificationState, exomonad_actor::NotificationError> {
    use exomonad_actor::{NotificationError, NotificationState};
    use exomonad_node::{DeliveryPhase, ReceiptLookup};
    let receipt = command.receipt();
    if receipt.owner() != command.owner() {
        return Err(NotificationError::Unauthorized);
    }
    if receipt.target() != target || receipt.inbox() != inbox_key {
        return Err(NotificationError::InvalidReceipt);
    }
    match inbox
        .observe_receipt(receipt.sequence())
        .map_err(|error| NotificationError::StorageFailure(error.to_string()))?
    {
        ReceiptLookup::Unavailable => Err(NotificationError::Unavailable),
        ReceiptLookup::Retained(evidence) => {
            if evidence.context
                != (DeliveryProvenance::Notification {
                    sender: command.owner(),
                    target,
                })
            {
                return Err(NotificationError::Unauthorized);
            }
            Ok(match evidence.phase {
                DeliveryPhase::Accepted => NotificationState::Accepted,
                DeliveryPhase::Presented => NotificationState::Presented,
                DeliveryPhase::InFlight
                | DeliveryPhase::Submitted
                | DeliveryPhase::Withdrawn
                | DeliveryPhase::Rejected
                | DeliveryPhase::Unconfirmed
                | DeliveryPhase::Compacted => NotificationState::Unconfirmed,
            })
        }
    }
}

#[cfg(test)]
#[allow(clippy::too_many_arguments)]
async fn deliver_pending(
    actor: ActorRef,
    inbox: &Arc<ActorInbox>,
    thread: &QueueReadyThread,
    backend: &dyn InteractiveAgentBackend,
    producer: &InputProducerId,
    reconciliations: &Mutex<BTreeMap<String, PendingUpdateReconciliation>>,
    workspace: &Path,
    runtime_observation: &exomonad_actor::ActorRuntimeObservationHandle,
) -> Result<(), String> {
    deliver_pending_checked(
        actor,
        inbox,
        thread,
        backend,
        producer,
        reconciliations,
        workspace,
        runtime_observation,
        &|_, _| true,
        &|_, _, _| false,
    )
    .await
}

#[allow(clippy::too_many_arguments)]
async fn deliver_pending_checked(
    actor: ActorRef,
    inbox: &Arc<ActorInbox>,
    thread: &QueueReadyThread,
    backend: &dyn InteractiveAgentBackend,
    producer: &InputProducerId,
    reconciliations: &Mutex<BTreeMap<String, PendingUpdateReconciliation>>,
    workspace: &Path,
    runtime_observation: &exomonad_actor::ActorRuntimeObservationHandle,
    watch_retained: &(dyn Fn(ActorRef, exomonad_actor::WatchId) -> bool + Send + Sync),
    watch_observed_since: &(dyn Fn(ActorRef, exomonad_actor::WatchId, u64) -> bool + Send + Sync),
) -> Result<(), String> {
    let cwd = workspace.to_string_lossy();
    let pending_inbox = Arc::clone(inbox);
    let pending =
        tidepool_runtime::spawn_blocking_in_span(move || pending_inbox.legacy_pending_prefix())
            .await
            .map_err(|error| format!("inbox reader task: {error}"))?
            .map_err(|error| error.to_string())?;
    let Some(last) = pending.last() else {
        // The front row is tracked (a native request update/notification).
        // `deliver_tracked_message` below only ever advances that exact
        // sequence, one at a time; if it is stuck (`Submitted`/`Unconfirmed`
        // and not resolving), any settlement/watch notice queued behind it
        // would otherwise never reach the model. Surface those out of band
        // before attempting the stuck row itself.
        deliver_out_of_order_notices(actor, inbox, thread, backend, &cwd, runtime_observation).await?;
        return deliver_tracked_message(
            actor,
            inbox,
            thread,
            backend,
            producer,
            reconciliations,
            runtime_observation,
        )
        .await;
    };
    let inbox_sequence = last.sequence;
    let already_surfaced = {
        let surfaced_inbox = Arc::clone(inbox);
        tidepool_runtime::spawn_blocking_in_span(move || surfaced_inbox.surfaced_out_of_order())
            .await
            .map_err(|error| format!("inbox reader task: {error}"))?
    };
    let mut out_of_order_delivered = Vec::new();
    let mut suppressed_watches = Vec::new();
    let mut stale_watches = Vec::new();
    let pending = pending
        .into_iter()
        .filter(|message| {
            if already_surfaced.contains(&message.sequence) {
                out_of_order_delivered.push(message.sequence);
                return false;
            }
            let DurableActorEvent::Typed(TypedActorEvent::WatchChanged { notification }) =
                &message.payload
            else {
                return true;
            };
            if !watch_retained(notification.owner, notification.watch) {
                suppressed_watches.push((message.sequence, notification.owner, notification.watch));
                return false;
            }
            // Queued while the owner was mid-turn, this notice can describe a
            // transition the owner already picked up by polling the same
            // watch in the meantime (`ObserveWatchWith`/`pollWatch`).
            // Re-announcing it would prompt over state the owner already has.
            if watch_observed_since(
                notification.owner,
                notification.watch,
                notification.occurred_at_unix_ms,
            ) {
                stale_watches.push((message.sequence, notification.owner, notification.watch));
                return false;
            }
            true
        })
        .collect::<Vec<_>>();
    if pending.is_empty() {
        let ack_inbox = Arc::clone(inbox);
        tidepool_runtime::spawn_blocking_in_span(move || ack_inbox.acknowledge(inbox_sequence))
            .await
            .map_err(|error| format!("actor inbox acknowledgement task: {error}"))?
            .map_err(|error| error.to_string())?;
        tracing::debug!(
            actor = ?actor,
            inbox_sequence,
            suppressed_watches = ?suppressed_watches,
            "suppressed queued watch notices whose handles were forgotten"
        );
        if !stale_watches.is_empty() {
            tracing::info!(
                actor = ?actor,
                inbox_sequence,
                stale_watches = ?stale_watches,
                "acknowledged queued watch notices the owner had already observed settled"
            );
        }
        if !out_of_order_delivered.is_empty() {
            tracing::info!(
                actor = ?actor,
                inbox_sequence,
                out_of_order_delivered = ?out_of_order_delivered,
                "acknowledged notices already pushed out of order past a stuck delivery"
            );
        }
        return Ok(());
    }
    let inbox_watermark = inbox.watermark();
    let activation = pending
        .iter()
        .rev()
        .find_map(|message| match &message.payload {
            DurableActorEvent::Typed(TypedActorEvent::SessionReady {
                sequence, request, ..
            }) => Some((*request, *sequence)),
            _ => None,
        });
    let event_sequences = pending
        .iter()
        .filter(|message| {
            !matches!(
                message.payload,
                DurableActorEvent::Typed(TypedActorEvent::SessionReady { .. })
            )
        })
        .map(|message| message.sequence)
        .collect::<Vec<_>>();
    let rendered = pending
        .iter()
        .map(|message| {
            message
                .payload
                .render(runtime_observation.snapshot().launched_at_unix_ms)
        })
        .collect::<Vec<_>>()
        .join("\n\n");
    backend
        .push(&cwd, thread, &rendered)
        .await
        .map_err(|error| error.to_string())?;
    if let Some((request, sequence)) = activation {
        runtime_observation.publish_request_activation(request, sequence);
    } else {
        runtime_observation.publish_event_activation(event_sequences, inbox_watermark);
    }
    let ack_inbox = Arc::clone(inbox);
    tidepool_runtime::spawn_blocking_in_span(move || ack_inbox.acknowledge(inbox_sequence))
        .await
        .map_err(|error| format!("inbox acknowledgement task: {error}"))?
        .map_err(|error| error.to_string())?;
    tracing::info!(
        actor = ?actor,
        inbox_sequence,
        inbox_watermark,
        event_count = pending.len(),
        "actor activation batch delivered"
    );
    if !suppressed_watches.is_empty() {
        tracing::debug!(
            actor = ?actor,
            inbox_sequence,
            suppressed_watches = ?suppressed_watches,
            "suppressed queued watch notices whose handles were forgotten"
        );
    }
    if !stale_watches.is_empty() {
        tracing::info!(
            actor = ?actor,
            inbox_sequence,
            stale_watches = ?stale_watches,
            "acknowledged queued watch notices the owner had already observed settled"
        );
    }
    if !out_of_order_delivered.is_empty() {
        tracing::info!(
            actor = ?actor,
            inbox_sequence,
            out_of_order_delivered = ?out_of_order_delivered,
            "acknowledged notices already pushed out of order past a stuck delivery"
        );
    }
    Ok(())
}

/// Untracked (settlement/watch/cancellation) notices queued anywhere behind a
/// stuck tracked row, pushed to the backend out of order and marked so the
/// ordinary batch path above does not render them again once the barrier
/// clears. See `ActorInbox::legacy_notices_beyond_barrier`.
async fn deliver_out_of_order_notices(
    actor: ActorRef,
    inbox: &Arc<ActorInbox>,
    thread: &QueueReadyThread,
    backend: &dyn InteractiveAgentBackend,
    cwd: &str,
    runtime_observation: &exomonad_actor::ActorRuntimeObservationHandle,
) -> Result<(), String> {
    let beyond_inbox = Arc::clone(inbox);
    let beyond =
        tidepool_runtime::spawn_blocking_in_span(move || beyond_inbox.legacy_notices_beyond_barrier())
            .await
            .map_err(|error| format!("inbox reader task: {error}"))?
            .map_err(|error| error.to_string())?;
    if beyond.is_empty() {
        return Ok(());
    }
    let rendered = beyond
        .iter()
        .map(|message| {
            message
                .payload
                .render(runtime_observation.snapshot().launched_at_unix_ms)
        })
        .collect::<Vec<_>>()
        .join("\n\n");
    backend
        .push(cwd, thread, &rendered)
        .await
        .map_err(|error| error.to_string())?;
    let sequences = beyond.iter().map(|message| message.sequence).collect::<Vec<_>>();
    inbox.mark_surfaced_out_of_order(sequences.iter().copied());
    tracing::info!(
        actor = ?actor,
        sequences = ?sequences,
        "delivered settlement/watch notice queued behind a stuck native delivery"
    );
    Ok(())
}

/// The existing inbox pump is the sole sender for tracked actor messages.
/// Its durable attempt fence prevents a cancelled or uncertain submission from
/// being sent again, and prevents later rows from overtaking it.
async fn deliver_tracked_message(
    actor: ActorRef,
    inbox: &ActorInbox,
    thread: &QueueReadyThread,
    backend: &dyn InteractiveAgentBackend,
    producer: &InputProducerId,
    reconciliations: &Mutex<BTreeMap<String, PendingUpdateReconciliation>>,
    observation: &exomonad_actor::ActorRuntimeObservationHandle,
) -> Result<(), String> {
    use exomonad_node::{DeliveryPhase, ReceiptLookup};
    let sequence = inbox
        .cursor()
        .checked_add(1)
        .ok_or("inbox sequence exhausted")?;
    let ReceiptLookup::Retained(evidence) =
        inbox.observe_receipt(sequence).map_err(|e| e.to_string())?
    else {
        return Ok(());
    };
    let target = match &evidence.context {
        DeliveryProvenance::Notification { target, .. }
        | DeliveryProvenance::RequestUpdate { target, .. } => *target,
    };
    if target != actor {
        return Err(format!(
            "message {sequence} targets another actor incarnation"
        ));
    }
    let native_sequence =
        std::num::NonZeroU64::new(sequence).ok_or("durable inbox allocated zero sequence")?;
    let operation_id = InputOperationId {
        producer: producer.clone(),
        sequence: native_sequence,
    };
    let native_key = operation_id.native_key();
    if evidence.phase == DeliveryPhase::Compacted {
        finish_update_reconciliation(
            reconciliations,
            &native_key,
            exomonad_actor::LateUpdateEvidence::Compacted(
                "native input evidence is compacted; resubmission remains fenced".into(),
            ),
        )?;
        return Err(format!(
            "message {sequence} native operation {native_key} retains terminal compacted evidence; later delivery is fenced"
        ));
    }
    if matches!(evidence.context, DeliveryProvenance::RequestUpdate { .. })
        && !reconciliations.lock().contains_key(&native_key)
    {
        return Err(format!(
            "request update {sequence} awaits its exact actor correlation"
        ));
    }
    let (purpose, correlation) = match &evidence.context {
        DeliveryProvenance::Notification { .. } => (
            InputPurpose::Notification,
            Some(format!("notification-{sequence}")),
        ),
        DeliveryProvenance::RequestUpdate {
            request, update, ..
        } => (
            InputPurpose::RequestUpdate,
            Some(format!("request-{}-update-{update}", request.0)),
        ),
    };
    let envelope = inbox
        .front_pending()
        .map_err(|e| e.to_string())?
        .ok_or_else(|| format!("tracked receipt {sequence} has no pending row"))?;
    if envelope.sequence != sequence || envelope.receipt_context.as_ref() != Some(&evidence.context)
    {
        return Err(format!(
            "tracked receipt {sequence} does not match the durable front row"
        ));
    }
    let operation = InteractiveInputEnvelope::new(
        operation_id,
        purpose,
        InteractiveInputMode::StartOrSteer,
        InteractiveInputTarget {
            conversation: thread.id().clone(),
            actor: format!("{}@{}", actor.id.0, actor.incarnation.0),
            correlation,
        },
        envelope
            .payload
            .render(observation.snapshot().launched_at_unix_ms)
            .into_bytes(),
    )
    .map_err(|error| error.to_string())?;

    let outcome = match evidence.phase {
        DeliveryPhase::Accepted => {
            let attempt = inbox
                .begin_tracked_delivery(sequence)
                .map_err(|e| e.to_string())?;
            match backend.submit_input(thread, &operation).await {
                Ok(outcome) => {
                    let persisted = if matches!(
                        outcome,
                        exomonad_agent::InputAdmission::Unknown
                            | exomonad_agent::InputAdmission::Compacted
                    ) {
                        attempt.unconfirmed()
                    } else {
                        attempt.submitted()
                    };
                    persisted.map_err(|e| e.to_string())?;
                    outcome
                }
                Err(exomonad_agent::InteractiveInputError::NotSubmitted(error)) => {
                    attempt.not_submitted().map_err(|e| e.to_string())?;
                    return Err(error.to_string());
                }
                Err(exomonad_agent::InteractiveInputError::Unconfirmed(error)) => {
                    attempt.unconfirmed().map_err(|e| e.to_string())?;
                    retain_update_unconfirmed(reconciliations, &native_key, error.to_string());
                    return Err(error.to_string());
                }
            }
        }
        DeliveryPhase::Submitted | DeliveryPhase::Unconfirmed => backend
            .query_input(thread, operation.id())
            .await
            .map_err(|error| error.to_string())?,
        phase => {
            return Err(format!(
                "message {sequence} retains unexpected delivery phase {phase:?}"
            ));
        }
    };

    match outcome {
        exomonad_agent::InputAdmission::Presented => {
            inbox
                .confirm_presented_exact(sequence, &evidence.context)
                .map_err(|e| e.to_string())?;
            finish_update_reconciliation(
                reconciliations,
                &native_key,
                exomonad_actor::LateUpdateEvidence::Presented,
            )?;
            observation.publish_event_activation(vec![sequence], inbox.watermark());
            Ok(())
        }
        exomonad_agent::InputAdmission::Withdrawn => {
            inbox
                .confirm_withdrawn(sequence, &evidence.context)
                .map_err(|e| e.to_string())?;
            finish_update_reconciliation(
                reconciliations,
                &native_key,
                exomonad_actor::LateUpdateEvidence::NotPresented(
                    "native input was withdrawn before presentation".into(),
                ),
            )?;
            Ok(())
        }
        exomonad_agent::InputAdmission::Rejected => {
            inbox
                .confirm_rejected(sequence, &evidence.context)
                .map_err(|e| e.to_string())?;
            finish_update_reconciliation(
                reconciliations,
                &native_key,
                exomonad_actor::LateUpdateEvidence::NotPresented(
                    "native input was rejected before presentation".into(),
                ),
            )?;
            Ok(())
        }
        exomonad_agent::InputAdmission::Compacted => {
            inbox
                .confirm_compacted_exact(sequence, &evidence.context)
                .map_err(|e| e.to_string())?;
            let detail = format!(
                "message {sequence} native operation {native_key} has compacted input evidence; resubmission remains fenced"
            );
            finish_update_reconciliation(
                reconciliations,
                &native_key,
                exomonad_actor::LateUpdateEvidence::Compacted(detail.clone()),
            )?;
            Err(detail)
        }
        exomonad_agent::InputAdmission::NotSubmitted
        | exomonad_agent::InputAdmission::Admitted
        | exomonad_agent::InputAdmission::Dispatching
        | exomonad_agent::InputAdmission::Unknown => {
            let phase = match inbox.observe_receipt(sequence) {
                Ok(ReceiptLookup::Retained(receipt)) => format!("{:?}", receipt.phase),
                Ok(ReceiptLookup::Unavailable) => "unavailable".to_owned(),
                Err(error) => format!("unreadable ({error})"),
            };
            Err(format!(
                "message {sequence} native operation {native_key} remains pending in durable phase {phase}; latest provider input state: {outcome:?}"
            ))
        }
    }
}

fn retain_update_unconfirmed(
    reconciliations: &Mutex<BTreeMap<String, PendingUpdateReconciliation>>,
    native_key: &str,
    detail: String,
) {
    if let Some(pending) = reconciliations.lock().get(native_key) {
        pending.retain_unconfirmed(detail);
    }
}

fn finish_update_reconciliation(
    reconciliations: &Mutex<BTreeMap<String, PendingUpdateReconciliation>>,
    native_key: &str,
    evidence: exomonad_actor::LateUpdateEvidence,
) -> Result<(), String> {
    let pending = reconciliations.lock().get(native_key).cloned();
    let Some(pending) = pending else {
        return Ok(());
    };
    pending
        .reconciler
        .reconcile(evidence)
        .map_err(|error| error.to_string())?;
    reconciliations.lock().remove(native_key);
    Ok(())
}

#[allow(clippy::too_many_arguments)]
async fn run_delivery_pump(
    actor: ActorRef,
    inbox: Arc<ActorInbox>,
    thread: QueueReadyThread,
    backend: Arc<dyn InteractiveAgentBackend>,
    producer: InputProducerId,
    reconciliations: Arc<Mutex<BTreeMap<String, PendingUpdateReconciliation>>>,
    workspace: PathBuf,
    runtime_observation: exomonad_actor::ActorRuntimeObservationHandle,
    watch_retained: WatchRetentionCheck,
    watch_observed_since: WatchObservationCheck,
    source_layers: Option<Arc<crate::exomonad::source::ExomonadSourceReload>>,
    worktrees: WorktreeManager,
    mut shutdown: oneshot::Receiver<()>,
) {
    let mut health = tokio::time::interval(Duration::from_secs(1));
    let mut usage_poll = tokio::time::interval(Duration::from_secs(10));
    let mut last_error = None;
    loop {
        tokio::select! {
            _ = &mut shutdown => return,
            _ = health.tick() => {
                let result = deliver_pending_checked(
                    actor,
                    &inbox,
                    &thread,
                    backend.as_ref(),
                    &producer,
                    &reconciliations,
                    &workspace,
                    &runtime_observation,
                    watch_retained.as_ref(),
                    watch_observed_since.as_ref(),
                ).await;
                match result {
                    Ok(()) => {
                        if last_error.take().is_some() {
                            tracing::info!(actor = ?actor, "actor inbox delivery recovered");
                        }
                    }
                    Err(error) => {
                        if last_error.as_deref() != Some(error.as_str()) {
                            tracing::warn!(actor = ?actor, %error, "actor inbox delivery remains pending");
                        }
                        last_error = Some(error);
                    }
                }
            }
            _ = usage_poll.tick() => {
                match backend.observe(&thread).await {
                    Ok(Some(observation)) => runtime_observation.publish_provider_observation(observation),
                    Ok(None) => runtime_observation.mark_provider_observation_stale(),
                    Err(error) => {
                        runtime_observation.mark_provider_observation_stale();
                        tracing::debug!(actor = ?actor, %error, "provider observation unavailable");
                    }
                }
                poll_source_drift(actor, &runtime_observation, source_layers.as_ref(), &worktrees).await;
            }
        }
    }
}

/// Observe source drift for `actor` on the same 10-second cadence
/// `usage_poll` already pays for provider observation, rather than a new
/// timer: reading a source layer's disk revision or a checkout's dirty
/// files is real filesystem and Git work, and the status view this feeds
/// (`ResidentKernelBehavior::live_status_text`) must stay cheap on every
/// call.
///
/// Each of the three rows is observed independently, and a row this actor
/// has nothing to observe for (no source service on the run, no assigned
/// worktree) is simply left unpublished rather than published as clean —
/// `ActorRuntimeObservation::source_drift` distinguishes "not observed" from
/// "checked, identical" for exactly this reason. All filesystem and Git work
/// runs on a blocking thread; nothing here runs on the async executor.
async fn poll_source_drift(
    actor: ActorRef,
    runtime_observation: &exomonad_actor::ActorRuntimeObservationHandle,
    source_layers: Option<&Arc<crate::exomonad::source::ExomonadSourceReload>>,
    worktrees: &WorktreeManager,
) {
    let worktree_id = runtime_observation
        .snapshot()
        .workspace
        .and_then(|workspace| workspace.worktree_id);
    let source_layers = source_layers.cloned();
    let worktrees = worktrees.clone();
    let caller = tidepool_repr::PrincipalId::from(actor);
    let (layer, frozen, checkout) = tidepool_runtime::spawn_blocking_in_span(move || {
        let layer = source_layers.as_ref().and_then(|layers| {
            layers
                .drift(caller)
                .inspect_err(|error| {
                    tracing::debug!(?actor, ?error, "source layer drift unavailable");
                })
                .ok()
        });
        let frozen = source_layers.as_ref().and_then(|layers| {
            layers
                .frozen_drift()
                .inspect_err(|error| {
                    tracing::debug!(?actor, %error, "frozen workspace drift unavailable");
                })
                .ok()
        });
        let checkout = worktree_id.and_then(|id| {
            checkout_git_drift(&worktrees, &id)
                .inspect_err(|error| {
                    tracing::debug!(?actor, worktree = %id, %error, "checkout drift unavailable");
                })
                .ok()
        });
        (layer, frozen, checkout)
    })
    .await
    .unwrap_or_default();
    if let Some(layer) = layer {
        runtime_observation.publish_source_layer_drift(layer);
    }
    if let Some(frozen) = frozen {
        runtime_observation.publish_frozen_source_drift(frozen);
    }
    if let Some(checkout) = checkout {
        runtime_observation.publish_checkout_git_drift(checkout);
    }
}

/// The checkout's Git head and dirty files, via [`GitCli`] — the sole
/// sanctioned way to invoke git in this repository. There is no recorded
/// build revision for the running binary to compare `head` against; see
/// `exomonad_actor::CheckoutGitDrift`.
fn checkout_git_drift(
    worktrees: &WorktreeManager,
    worktree_id: &str,
) -> std::result::Result<exomonad_actor::CheckoutGitDrift, String> {
    let handle = worktrees
        .lookup(&WorktreeId::from_raw(worktree_id))
        .map_err(|error| error.to_string())?
        .ok_or_else(|| format!("worktree {worktree_id:?} is not allocated"))?;
    let git = worktrees.git();
    let head = git
        .try_run(handle.cwd(), &["rev-parse", "HEAD"])
        .map_err(|error| error.to_string())?
        .trimmed()
        .to_owned();
    let dirty = exomonad_worktree::git::inspect::dirty_summary(git, handle.cwd())
        .map_err(|error| error.to_string())?;
    let mut dirty_files: Vec<String> = dirty
        .staged
        .into_iter()
        .chain(dirty.unstaged)
        .chain(dirty.untracked)
        .collect();
    dirty_files.sort();
    dirty_files.dedup();
    Ok(exomonad_actor::CheckoutGitDrift { head, dirty_files })
}

fn prepare_owner_notification(
    notice: &exomonad_actor::ChildExitNotice,
    deployments: &[InteractiveDeployment],
) -> Option<OwnerNotification> {
    if notice
        .child
        .terminal()
        .retirement_acknowledged_by(notice.owner)
    {
        return None;
    }
    let owner_application = deployments.iter().find(|app| app.actor == notice.owner)?;
    Some(OwnerNotification {
        owner: notice.owner,
        inbox: Arc::clone(&owner_application.inbox),
        event: DurableActorEvent::Typed(TypedActorEvent::ChildExited),
    })
}

async fn publish_owner_notification(
    notification: OwnerNotification,
) -> (ActorRef, Result<(), String>) {
    publish_inbox_event_for(notification.owner, notification.inbox, notification.event).await
}

async fn publish_inbox_event_for(
    actor: ActorRef,
    inbox: Arc<ActorInbox>,
    event: DurableActorEvent,
) -> (ActorRef, Result<(), String>) {
    (actor, publish_inbox_event(inbox, event).await)
}

async fn publish_inbox_event(
    inbox: Arc<ActorInbox>,
    event: DurableActorEvent,
) -> Result<(), String> {
    tidepool_runtime::spawn_blocking_in_span(move || {
        if let DurableActorEvent::Typed(TypedActorEvent::ProviderTurnFailed {
            actor,
            thread,
            revision,
            ..
        }) = &event
        {
            inbox
                .publish_latest(
                    format!(
                        "provider-failure:{}@{}:{thread}",
                        actor.id.0, actor.incarnation.0
                    ),
                    *revision,
                    event,
                )
                .map(|_| ())
        } else {
            inbox.publish(event).map(|_| ())
        }
    })
    .await
    .map_err(|error| format!("actor inbox publisher task: {error}"))?
    .map_err(|error| error.to_string())?;
    Ok(())
}

async fn retire_interactive_application_guarded(
    deployment: InteractiveDeployment,
    tmux: &TmuxSession,
    native_retirement: NativeRetirement,
    scoped_process: Option<CleanupComponentOutcome>,
) -> InteractiveCleanupReceipt {
    let actor = deployment.actor;
    match AssertUnwindSafe(retire_interactive_application(
        deployment,
        tmux,
        native_retirement,
        scoped_process,
    ))
    .catch_unwind()
    .await
    {
        Ok(receipt) => receipt,
        Err(_) => panicked_cleanup_receipt(actor),
    }
}

/// A lost retirement task cannot identify which owner panicked or which later
/// owners it never reached. Preserve every cleanup domain as unknown rather
/// than mislabelling one component and silently omitting the rest.
fn panicked_cleanup_receipt(actor: ActorRef) -> InteractiveCleanupReceipt {
    InteractiveCleanupReceipt {
        actor,
        components: [
            CleanupComponent::Process,
            CleanupComponent::Pane,
            CleanupComponent::ToolService,
            CleanupComponent::Delivery,
            CleanupComponent::Socket,
            CleanupComponent::BuildResource,
            CleanupComponent::WorktreeBinding,
        ]
        .into_iter()
        .map(|component| CleanupComponentReceipt {
            component,
            outcome: CleanupComponentOutcome::Failed {
                detail: "cleanup task panicked; component completion is unknown".into(),
            },
        })
        .collect(),
    }
}

async fn retire_interactive_application(
    mut deployment: InteractiveDeployment,
    tmux: &TmuxSession,
    native_retirement: NativeRetirement,
    scoped_process: Option<CleanupComponentOutcome>,
) -> InteractiveCleanupReceipt {
    let actor = deployment.actor;
    let exact_process_stopped = matches!(
        scoped_process.as_ref(),
        Some(CleanupComponentOutcome::Completed)
    );
    let mut components = Vec::with_capacity(6);
    let mut delivery = match deployment.connection {
        InteractiveConnection::AwaitingBinding => None,
        InteractiveConnection::Bound {
            delivery_shutdown,
            delivery,
            ..
        } => {
            // best-effort: the delivery task's receiver may already have
            // ended if the task exited before shutdown was requested.
            delivery_shutdown.send(()).ok();
            Some(delivery)
        }
    };
    if let Some(process) = scoped_process {
        let pane = if native_retirement == NativeRetirement::Preserve
            || matches!(process, CleanupComponentOutcome::Completed)
        {
            retire_pane_artifact(tmux, &deployment.pane, native_retirement).await
        } else {
            CleanupComponentOutcome::Failed {
                detail: "pane retained because exact process termination is unconfirmed".into(),
            }
        };
        components.push(CleanupComponentReceipt {
            component: CleanupComponent::Process,
            outcome: process,
        });
        components.push(CleanupComponentReceipt {
            component: CleanupComponent::Pane,
            outcome: pane,
        });
    } else {
        components.push(CleanupComponentReceipt {
            component: CleanupComponent::Process,
            outcome: retire_native_pane(tmux, &deployment.pane, native_retirement).await,
        });
    }
    let (service_outcome, delivery_outcome) = tokio::join!(
        stop_retired_tool_service(deployment.actor, &mut deployment.service),
        async {
            if let Some(delivery) = delivery.as_mut() {
                stop_retired_delivery(deployment.actor, delivery, APPLICATION_TASK_GRACE_TIMEOUT)
                    .await
            } else {
                CleanupComponentOutcome::Completed
            }
        },
    );
    components.push(CleanupComponentReceipt {
        component: CleanupComponent::ToolService,
        outcome: service_outcome,
    });
    components.push(CleanupComponentReceipt {
        component: CleanupComponent::Delivery,
        outcome: delivery_outcome,
    });
    let retirement_record_error = exact_process_stopped
        .then(|| mark_process_recovery_retired(&deployment.process_recovery_record).err())
        .flatten();
    let retirement_recorded = exact_process_stopped && retirement_record_error.is_none();
    let quiescent = retirement_recorded
        && components
            .iter()
            .filter(|component| {
                matches!(
                    component.component,
                    CleanupComponent::ToolService | CleanupComponent::Delivery
                )
            })
            .all(|component| matches!(component.outcome, CleanupComponentOutcome::Completed));
    let mut socket_directory = deployment.socket_directory;
    if quiescent {
        socket_directory.work_settled();
    }
    let socket_outcome = retirement_record_error.map_or_else(
        || socket_cleanup_outcome(socket_directory),
        |error| CleanupComponentOutcome::Failed {
            detail: format!("process retirement evidence remains retained: {error}"),
        },
    );
    components.push(CleanupComponentReceipt {
        component: CleanupComponent::Socket,
        outcome: socket_outcome,
    });
    let build_outcome = if quiescent {
        match deployment
            .active_workspace
            .retire(&deployment.active_workspace.view)
            .await
        {
            Ok(()) => CleanupComponentOutcome::Completed,
            Err(error) => CleanupComponentOutcome::Failed {
                detail: error.to_string(),
            },
        }
    } else {
        CleanupComponentOutcome::Failed {
            detail: "workspace retained: exact process and hosted work must both settle".into(),
        }
    };
    let workspace_retired = matches!(build_outcome, CleanupComponentOutcome::Completed);
    components.push(CleanupComponentReceipt {
        component: CleanupComponent::BuildResource,
        outcome: build_outcome,
    });
    let binding_outcome = if let Some(custody) = deployment.worktree_custody.take() {
        if workspace_retired {
            if let Some(custody) =
                (custody.as_ref() as &dyn std::any::Any).downcast_ref::<ActorWorkspaceCustody>()
            {
                let completed = {
                    let mut state = custody.state.lock();
                    state.launch = scoped_custody::LaunchCustody::Unclaimed;
                    state
                        .terminal
                        .as_ref()
                        .is_some_and(|exit| exit.kind == ActorExitKind::Completed)
                };
                let binding = custody.binding.lock().take();
                match binding
                    .map(|binding| {
                        if completed {
                            binding.complete(&mut custody.bindings.lock())
                        } else {
                            binding.release(&mut custody.bindings.lock())
                        }
                    })
                    .transpose()
                {
                    Ok(_) => CleanupComponentOutcome::Completed,
                    Err(error) => CleanupComponentOutcome::Failed {
                        detail: error.to_string(),
                    },
                }
            } else {
                CleanupComponentOutcome::Failed {
                    detail: "unknown workspace custody owner".into(),
                }
            }
        } else {
            CleanupComponentOutcome::Failed {
                detail: "custody retained: the workspace view was not retired (see BuildResource)"
                    .into(),
            }
        }
    } else {
        CleanupComponentOutcome::Completed
    };
    components.push(CleanupComponentReceipt {
        component: CleanupComponent::WorktreeBinding,
        outcome: binding_outcome,
    });
    InteractiveCleanupReceipt { actor, components }
}

fn mark_process_recovery_retired(path: &Path) -> std::io::Result<()> {
    let mut record: ProcessRecoveryRecord =
        serde_json::from_slice(&std::fs::read(path)?).map_err(std::io::Error::other)?;
    if record.version != 1 {
        return Err(std::io::Error::other(
            "unsupported process recovery record version",
        ));
    }
    record.retired = true;
    tidepool_atomic_write::write_durable(
        path,
        &serde_json::to_vec_pretty(&record).map_err(std::io::Error::other)?,
    )
    .map_err(std::io::Error::from)
}

/// Account for exact resident cleanup before draining the original HTTP task.
/// Namespace/native and external-handler domains remain independently unknown.
async fn stop_retired_tool_service(
    actor: ActorRef,
    service: &mut hosted_retirement::HostedOwner,
) -> CleanupComponentOutcome {
    match hosted_retirement::observe(service,
        hosted_retirement::CompletionBoundary::AbortForShutdown,
        APPLICATION_TASK_GRACE_TIMEOUT).await {
        hosted_retirement::HostedObservation::Observed {
            input_seal,
            seal: hosted_retirement::SealObservation::Confirmed(seal),
            resident: hosted_retirement::ResidentObservation::Accounted(cleanup),
            http: hosted_retirement::HttpObservation::Drained,
        } if input_seal.confirms_retirement()
            && seal.actor() == actor
            && cleanup.actor() == actor
            && cleanup.is_confirmed() => CleanupComponentOutcome::Completed,
        hosted_retirement::HostedObservation::Observed {
            input_seal,
            seal: hosted_retirement::SealObservation::TerminalPath,
            resident: hosted_retirement::ResidentObservation::Accounted(cleanup),
            http: hosted_retirement::HttpObservation::Drained,
        } if input_seal.confirms_retirement()
            && cleanup.actor() == actor
            && cleanup.is_confirmed() => CleanupComponentOutcome::Completed,
        observation => CleanupComponentOutcome::Failed {
            detail: format!("resident/HTTP cleanup retained: {observation:?}; native/external cleanup is not established"),
        },
    }
}

async fn stop_retired_delivery(
    actor: ActorRef,
    delivery: &mut tokio::task::JoinHandle<()>,
    grace: Duration,
) -> CleanupComponentOutcome {
    match tokio::time::timeout(grace, &mut *delivery).await {
        Ok(Ok(())) => CleanupComponentOutcome::Completed,
        Ok(Err(error)) => {
            tracing::warn!(actor = ?actor, %error, "retired actor inbox task failed");
            CleanupComponentOutcome::Failed {
                detail: error.to_string(),
            }
        }
        Err(_) => {
            tracing::debug!(actor = ?actor, "forcing retired actor inbox task to stop");
            delivery.abort();
            // best-effort: the task was just aborted; the join outcome is
            // already reported above as `Forced`.
            delivery.await.ok();
            CleanupComponentOutcome::Forced
        }
    }
}

async fn retire_native_pane(
    tmux: &TmuxSession,
    pane: &TmuxPaneId,
    disposition: NativeRetirement,
) -> CleanupComponentOutcome {
    let detail = match disposition {
        NativeRetirement::Preserve => {
            "native TUI preserved; hosted coordination unavailable; process custody retained".into()
        }
        NativeRetirement::Terminate => match tmux.kill_pane(pane).await {
            Ok(()) => {
                "pane cleanup requested; exact process termination remains unconfirmed".into()
            }
            Err(error) => error.to_string(),
        },
    };
    CleanupComponentOutcome::Failed { detail }
}

/// Once an exact scope owner accounted for the process, tmux is only a UI
/// artifact. Its disappearance cannot strengthen the process observation.
async fn retire_pane_artifact(
    tmux: &TmuxSession,
    pane: &TmuxPaneId,
    disposition: NativeRetirement,
) -> CleanupComponentOutcome {
    match disposition {
        NativeRetirement::Preserve => CleanupComponentOutcome::Completed,
        NativeRetirement::Terminate => match tmux.kill_pane(pane).await {
            Ok(()) => CleanupComponentOutcome::Completed,
            Err(error) => CleanupComponentOutcome::Failed {
                detail: error.to_string(),
            },
        },
    }
}

async fn discover_interactive_binding(
    actor: ActorRef,
    request: InteractiveBindingRequest,
    tmux: &TmuxSession,
    pane: &TmuxPaneId,
) -> Result<QueueReadyThread, InteractiveApplicationError> {
    let mut binding_poll = tokio::time::interval(Duration::from_millis(100));
    let mut pane_health = tokio::time::interval(Duration::from_secs(2));
    loop {
        tokio::select! {
            _ = binding_poll.tick() => {
                // A retained binding belongs to the previous launch until this
                // exact tool host has accepted the new TUI's session callback.
                if !request.control.session_attached() {
                    continue;
                }
                if let Ok(thread) = read_interactive_binding(&request.path).await {
                    if let Some(expected) = &request.expected {
                        if expected != thread.id() {
                            return Err(application_error(
                                actor,
                                InteractiveOperation::DiscoverBinding,
                                format!(
                                    "resume published thread {} instead of retained thread {}",
                                    thread.id().0, expected.0
                                ),
                            ));
                        }
                    }
                    let binding = request.control.challenged_binding();
                    if thread.supports_active_input() && binding.is_none() {
                        continue;
                    }
                    return Ok(thread.with_challenged_session_binding(binding));
                }
            }
            _ = pane_health.tick() => {
                let status = tmux.pane_status(pane).await.map_err(|error| {
                    application_error(actor, InteractiveOperation::DiscoverBinding, error)
                })?;
                if status.as_ref().is_none_or(|status| status.dead) {
                    let exit_status = status.and_then(|status| status.exit_status);
                    let output = tmux
                        .capture_pane(pane, 120)
                        .await
                        .unwrap_or_else(|error| format!("<pane output unavailable: {error}>"));
                    let detail = if output.is_empty() {
                        format!(
                            "interactive application exited before conversation binding; exit_status={exit_status:?}"
                        )
                    } else {
                        format!(
                            "interactive application exited before conversation binding; exit_status={exit_status:?}; pane_output={output:?}"
                        )
                    };
                    tracing::error!(?actor, ?exit_status, pane_output = %output, "interactive application exited before binding");
                    return Err(application_error(
                        actor,
                        InteractiveOperation::DiscoverBinding,
                        detail,
                    ));
                }
            }
        }
    }
}

async fn wait_for_shutdown(mut shutdown: watch::Receiver<Option<NativeRetirement>>) {
    if shutdown.borrow().is_some() {
        return;
    }
    while shutdown.changed().await.is_ok() {
        if shutdown.borrow().is_some() {
            return;
        }
    }
}

async fn operator_shutdown() -> Result<(), std::io::Error> {
    #[cfg(unix)]
    {
        use tokio::signal::unix::{signal, SignalKind};

        let mut terminate = signal(SignalKind::terminate())?;
        let mut hangup = signal(SignalKind::hangup())?;
        tokio::select! {
            result = tokio::signal::ctrl_c() => result,
            _ = terminate.recv() => Ok(()),
            _ = hangup.recv() => Ok(()),
        }
    }
    #[cfg(not(unix))]
    {
        tokio::signal::ctrl_c().await
    }
}

#[cfg(test)]
fn developer_instructions(
    effective_role: &exomonad_actor::EffectiveRole,
    mode: &InteractiveLaunchMode,
) -> String {
    developer_instructions_selected(effective_role, mode, None, None)
}

fn developer_instructions_selected(
    effective_role: &exomonad_actor::EffectiveRole,
    mode: &InteractiveLaunchMode,
    inputs: Option<&crate::exomonad::workspace::FrozenWorkspace>,
    instructions: Option<&str>,
) -> String {
    if let Some(body) = instructions {
        return append_effective_role(body.to_owned(), effective_role);
    }
    let role = effective_role.role();
    let key = match role {
        exomonad_actor::ActorRole::Root => "root",
        exomonad_actor::ActorRole::Research => "research",
        exomonad_actor::ActorRole::Coding | exomonad_actor::ActorRole::Inherited => "coding",
        exomonad_actor::ActorRole::Scaffolding => "scaffolding",
        exomonad_actor::ActorRole::Integration => "integration",
    };
    if let Some(body) = inputs.and_then(|inputs| inputs.prompts.get(key)) {
        let mut body = body.clone();
        if role == exomonad_actor::ActorRole::Root
            && matches!(mode, InteractiveLaunchMode::Resume(_))
        {
            body.push_str(PromptId::RecreatedRoot.body());
        }
        return append_effective_role(body, effective_role);
    }
    if role == exomonad_actor::ActorRole::Root {
        let mut instructions = PromptId::ExomonadRoot.body().to_string();
        if matches!(mode, InteractiveLaunchMode::Resume(_)) {
            instructions.push_str(PromptId::RecreatedRoot.body());
        }
        append_effective_role(instructions, effective_role)
    } else {
        let instructions = match role {
            exomonad_actor::ActorRole::Research => PromptId::ReadonlyAgent.body().into(),
            exomonad_actor::ActorRole::Coding | exomonad_actor::ActorRole::Inherited => {
                PromptId::WorktreeAgent.body().into()
            }
            exomonad_actor::ActorRole::Scaffolding => PromptId::ScaffoldingAgent.body().into(),
            exomonad_actor::ActorRole::Integration => PromptId::IntegrationAgent.body().into(),
            exomonad_actor::ActorRole::Root => unreachable!("root handled above"),
        };
        append_effective_role(instructions, effective_role)
    }
}

fn append_effective_role(mut instructions: String, role: &exomonad_actor::EffectiveRole) -> String {
    let descendants = role.descendants();
    instructions.push_str(&format!(
        "\n\nRuntime policy ({}): role={:?}; effects={}; native_tools={:?}; workspace={:?}; descendant_depth={}; active_children={}. These are the effective runtime facts; effect membership alone is not authority.\n",
        role.prompt_profile(),
        role.role(),
        role.haskell_effects_type(),
        role.native_tools(),
        role.workspace(),
        descendants.maximum_depth,
        exomonad_actor::render_child_budget(descendants.maximum_active_children),
    ));
    instructions
}

fn worktree_grant(role: exomonad_actor::ActorRole) -> ActorWorktreeGrant {
    match role {
        exomonad_actor::ActorRole::Root => ActorWorktreeGrant::Repository,
        exomonad_actor::ActorRole::Coding | exomonad_actor::ActorRole::Scaffolding => {
            ActorWorktreeGrant::Bound {
                enumerate: false,
                allocate: true,
                integrate: true,
            }
        }
        exomonad_actor::ActorRole::Integration => ActorWorktreeGrant::Bound {
            enumerate: false,
            allocate: false,
            integrate: true,
        },
        exomonad_actor::ActorRole::Research | exomonad_actor::ActorRole::Inherited => {
            ActorWorktreeGrant::default()
        }
    }
}

/// Where a command raised by an actor with no agent process of its own runs,
/// and what it may write while it runs there.
///
/// Custody is exclusive, so an actor's bound worktree is unambiguous and is
/// the only checkout it is entitled to write in. It is granted alongside the
/// repository's shared git directory, because publishing a checked revision
/// means moving a ref and a linked worktree keeps its refs there; an actor
/// holding no custody — an operator workbench — gets neither, and has to
/// allocate a worktree before it can change anything. This is the same
/// boundary [`workspace::prepare`] builds for an agent process, applied to a
/// resident actor's single command; [`writable_repository_roots`] is the same
/// decision for the process case.
///
/// The boundary itself is built per command rather than once, because it
/// carries the working directory: a command that names its own directory has
/// to be wrapped in a boundary rooted there, or the wrapper's `--chdir` puts
/// it somewhere else than it asked for.
fn resident_command_roots(
    authority: &ActorWorktreeAuthority,
    worktrees: &WorktreeManager,
    source: &Path,
    actor: ActorRef,
) -> ResidentCommandRoots {
    let custody = authority
        .bound_worktree(actor.into())
        .and_then(|id| worktrees.registry().get(&id).ok().flatten())
        .map(|receipt| receipt.cwd);
    let mut writable = Vec::new();
    if let Some(worktree) = &custody {
        writable.push(worktree.clone());
        // A linked worktree's refs and objects live in the source
        // repository's git directory, so a publication is a write there.
        writable.push(source.join(".git"));
    }
    ResidentCommandRoots {
        directory: custody.clone().unwrap_or_else(|| source.to_owned()),
        protected: vec![
            source.to_owned(),
            worktrees.managed_root().to_owned(),
            worktrees.root_allocations().managed_root().to_owned(),
        ],
        writable,
        custody: custody.is_some(),
    }
}

/// The roots a resident actor's commands are confined to, and the directory
/// they run in when they do not name one.
#[derive(Clone)]
struct ResidentCommandRoots {
    directory: PathBuf,
    protected: Vec<PathBuf>,
    writable: Vec<PathBuf>,
    /// Whether this actor holds a worktree. False means nothing in the
    /// repository is writable to it.
    custody: bool,
}

/// The writable filesystem roots one actor's mount boundary grants.
///
/// `root_worktrees` is the directory the ROOT's own allocations materialize in
/// (`WorktreeManager::root_allocations`). The root has to be able to BUILD in a
/// worktree it allocated for itself — running the project's check script in an
/// integration worktree is ordinary root work — and the mount namespace is
/// fixed at launch, so that directory is granted up front. Children's worktrees
/// stay under the managed root, which is read-only to everyone including the
/// root: the root reads a child's work through the shared Git namespace and
/// typed observation, never by writing in the child's checkout.
fn writable_repository_roots(
    root: bool,
    workspace_access: exomonad_actor::WorkspaceAccess,
    source: &Path,
    worker_worktree: Option<&Path>,
    git_common_dir: &Path,
    root_worktrees: Option<&Path>,
) -> Vec<PathBuf> {
    let mut writable = if root {
        // Integration advances the source HEAD; child coding happens only in
        // the exact linked worktree granted to that child.
        let mut writable = vec![source.to_path_buf()];
        writable.extend(root_worktrees.map(Path::to_path_buf));
        writable
    } else if workspace_access == exomonad_actor::WorkspaceAccess::WritableBound {
        worker_worktree.map(Path::to_path_buf).into_iter().collect()
    } else {
        Vec::new()
    };
    if root || workspace_access == exomonad_actor::WorkspaceAccess::WritableBound {
        // Writable linked worktrees intentionally share objects, refs, config,
        // and per-worktree administrative state. Inspection-only actors must
        // observe the same metadata without being able to mutate it.
        writable.push(git_common_dir.to_path_buf());
    }
    writable
}

fn join_error(error: tokio::task::JoinError) -> Box<dyn std::error::Error> {
    runtime_error(format!("actor host task failed: {error}"))
}

fn runtime_error(message: impl Into<String>) -> Box<dyn std::error::Error> {
    Box::new(std::io::Error::other(message.into()))
}
