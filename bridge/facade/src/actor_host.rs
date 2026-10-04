//! Composition root for the first actor-native interactive swarm.
//!
//! The daemon owns resident Haskell scheduling and exact actor lifecycle. One
//! stock interactive agent is attached to each installed Haskell tool policy;
//! tmux is process ownership and observability, never message transport.

#[cfg(all(test, feature = "codex-compat"))]
mod agent_spec_tests;
#[cfg(test)]
mod embedded_agent_spec_tests;

#[cfg(test)]
pub(crate) use crate::transport_test_support::ResidentToolEndpointTestExt;
#[cfg(all(test, feature = "codex-compat"))]
mod background_command_example_tests;
#[cfg(all(test, feature = "codex-compat"))]
mod call_timing_tests;
#[cfg(all(test, feature = "codex-compat"))]
mod cell_compile_cost_tests;
mod cell_context;
mod cell_model;
#[cfg(all(test, feature = "codex-compat"))]
pub(crate) mod command_jobs_tests;
#[cfg(test)]
mod command_test_support;
mod commands;
mod context_wire;
mod effect_vocabulary;
pub(crate) use effect_vocabulary::exomonad_effect_declarations;
mod application_supervisor;
#[cfg(test)]
mod context_transaction_acceptance_tests;
#[cfg(all(test, feature = "codex-compat"))]
mod custody_tests;
mod delivery;
mod display_output;
#[cfg(test)]
mod documentation_tests;
#[cfg(test)]
mod embedded_captured_unfold_tests;
#[cfg(test)]
mod embedded_checkpoint_children_survive_later_failure_tests;
#[cfg(test)]
mod embedded_checkpoint_children_tests;
#[cfg(test)]
mod embedded_command_tests;
mod embedded_context;
mod embedded_harness;
#[cfg(test)]
mod embedded_notification_tests;
#[cfg(test)]
mod embedded_pending_compaction_tests;
mod embedded_policy;
mod embedded_projection;
mod embedded_recovery;
#[cfg(test)]
mod embedded_recovery_tests;
mod embedded_reflect;
mod embedded_service;
#[cfg(test)]
mod scaffold_admission_tests;
#[cfg(all(test, feature = "codex-compat"))]
use delivery::deliver_pending;
#[cfg(test)]
use delivery::embedded_notification_operation_id;
#[cfg(feature = "codex-compat")]
use delivery::run_delivery_pump;
#[cfg(any(test, feature = "codex-compat"))]
use delivery::{admit_notification, observe_notification_receipt};
#[cfg(all(test, feature = "codex-compat"))]
use delivery::{
    deliver_pending_checked, observe_inbound_delivery, remind_turn_ended_without_respond,
};
use delivery::{
    observe_embedded_notification, schedule_embedded_notification_drain,
    schedule_embedded_notification_send,
};
#[cfg(all(test, feature = "codex-compat"))]
use delivery::{
    run_periodic_observation, supervise_delivery, turn_end_reminder, until_shutdown,
    ActorObservation, WithoutEvidence, POSSIBLY_SEEN_PREFIX, PROVIDER_POLL_INTERVAL,
    REDELIVERED_PREFIX, WITHDRAW_WITHOUT_EVIDENCE_AFTER,
};
#[cfg(test)]
mod embedded_shutdown_tests;
mod host_incarnation;
#[cfg(feature = "codex-compat")]
mod hosted_retirement;
#[cfg(all(test, feature = "codex-compat"))]
mod hosted_tools_tests;
#[cfg(all(test, feature = "codex-compat"))]
mod invocation_lifetime_tests;
#[cfg(all(test, feature = "codex-compat"))]
mod jev_tests;
#[cfg(test)]
mod lookup_availability_tests;
#[cfg(test)]
mod m1_host_tests;
#[cfg(feature = "codex-compat")]
mod native_launch;
#[cfg(test)]
mod native_prefix_publication_tests;
#[cfg(all(test, feature = "codex-compat"))]
mod observation_budget_tests;
mod overlay_resource;
#[cfg(test)]
mod packaged_catalog_tests;
#[cfg(test)]
mod prepared_display_tests;
pub(crate) use overlay_resource::valid_artifact_path;
#[cfg(all(test, feature = "codex-compat"))]
mod source_reload_tests;
#[cfg(all(test, feature = "codex-compat"))]
#[path = "host_dynamic_tools/tui_resource_tests.rs"]
mod tui_resource_tests;
#[cfg(all(test, feature = "codex-compat"))]
#[path = "host_dynamic_tools/tui_sleep_tests.rs"]
mod tui_sleep_tests;
mod workspace;
pub mod workspace_cleanup;
mod workspace_publication;
#[cfg(feature = "codex-compat")]
use workspace::WorkspaceLayout;
pub(crate) use workspace::{copy_helper_draft, initialize_helper_draft};
use workspace::{ActiveWorkspace, PreparedWorkspace};
#[cfg(test)]
mod fresh_child_tests;
mod model_free;
mod prompt_catalog;
mod provider_attachment;
pub(crate) mod recipe_checks;
#[cfg(all(test, feature = "codex-compat"))]
mod research_policy_tests;
#[cfg(all(test, feature = "codex-compat"))]
mod resource_tests;
mod root_declaration_recovery;
mod scoped_custody;
#[cfg(feature = "codex-compat")]
mod socket_directory;
#[cfg(test)]
mod test_campaign;
#[cfg(all(test, feature = "codex-compat"))]
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
#[cfg(feature = "codex-compat")]
use exomonad_agent::{
    copy_interactive_binding, native_interactive_backend, read_interactive_binding,
};
#[cfg(feature = "codex-compat")]
use exomonad_agent::{
    BackendThreadId, InputOperationId, InputPurpose, InteractiveAgentBackend,
    InteractiveInputEnvelope, InteractiveInputMode, InteractiveInputTarget,
};
#[cfg(feature = "codex-compat")]
use exomonad_agent::{
    InteractiveAgentSpec, InteractiveNativeSandbox, InteractiveNativeToolPolicy,
    InteractivePolicyMount,
};
use exomonad_agent::{InteractiveLaunchMode, QueueReadyThread, ReasoningEffort};
use frunk::{hlist, HCons, HNil};
use futures_util::FutureExt;
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};

use exomonad_node::DurableInbox;
#[cfg(feature = "codex-compat")]
use exomonad_node::{
    ProcessInvocation, ProcessSupervisorClient, ProcessSupervisorManifest, ServiceEnvironment,
    TmuxLaunch,
};
use exomonad_node::{ProcessMountBoundary, TmuxPaneId, TmuxSession, BUBBLEWRAP_PROGRAM};
#[cfg(feature = "codex-compat")]
use exomonad_worktree::WorktreeHandle;
use exomonad_worktree::{
    ActiveBinding, AgentRef as WorktreePrincipal, BindingTable, EventJournal, GitCli, WorktreeId,
    WorktreeManager, WorktreeMonitor, WorktreeRegistry,
};
use tidepool_effect::{EffectRunPolicy, LivePayloadPolicy};
use tidepool_handlers::{
    ActorBoundWorktreeHandler, ActorWorktreeAllocationHandler, ActorWorktreeAuthority,
    ActorWorktreeGrant, ActorWorktreeHandler, ActorWorktreeIntegrationHandler,
    ActorWorktreeRegistryHandler, EventConfig, InertObservationSource, RepoEventHandler,
    WorktreeHandler,
};
use tidepool_mcp::CapturedOutput;
use tidepool_runtime::session::{
    insert_preamble_imports, resident_workbench_templates, run_turn, ResidentSession,
    ResidentSessionState, SessionLib, TurnRequest as HaskellTurnRequest, TurnResult,
};
use tidepool_runtime::DEFAULT_NURSERY_SIZE;
#[cfg(feature = "codex-compat")]
use tokio::net::UnixListener;
use tokio::sync::{mpsc, oneshot, watch};
use tokio::task::JoinSet;

use self::application_supervisor::run_interactive_applications;
use self::embedded_projection::LifecyclePublisher;
pub(crate) use self::host_incarnation::HostIncarnationLease;
#[cfg(feature = "codex-compat")]
use self::native_launch::launch_prepared_interactive_application;
use self::overlay_resource::{
    ArtifactInspection, OverlayResourceLease, OverlaySnapshot, SharedOverlayResource,
};
use self::prompt_catalog::{FrozenBasePrompt, PromptId};
#[cfg(feature = "codex-compat")]
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
#[cfg(feature = "codex-compat")]
const CHILD_LIFECYCLE_NOTICE: &str = "A child actor changed lifecycle state.";
const APPLICATION_SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(15);
#[cfg(feature = "codex-compat")]
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
    #[cfg(feature = "codex-compat")]
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
    #[cfg(feature = "codex-compat")]
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
        // The sole caller has already verified durable predecessor retirement.
        // A live Mounted checkout must be sealed from its exact descriptor
        // before lookup may materialize ordinary host working files.
        self.manager
            .seal_orphaned_mounted_view(&worktree)
            .map_err(|error| ForkWorkspaceAdmissionError {
                detail: error.to_string(),
            })?;
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
        _policy: exomonad_actor::ForkWorkspacePolicy,
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
            #[cfg(feature = "codex-compat")]
            let (handle, workspace, notice) = match &custody.native {
                Some(native) if native.layout.is_some() => {
                    let prepared = native
                        .prepare_workspace(owner, authorized, _policy)
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
            #[cfg(not(feature = "codex-compat"))]
            let (handle, workspace, notice) = {
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
    #[cfg(feature = "codex-compat")] native: Option<NativeForkAdmission>,
) -> Arc<ActorForkWorkspaceAdmission> {
    Arc::new(ActorForkWorkspaceAdmission {
        bindings,
        runtime,
        #[cfg(feature = "codex-compat")]
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
    pub source_import: crate::exomonad::SourceImportPolicy,
    pub command_resources: Option<Arc<exomonad_node::command_resources::CommandResourceClient>>,
    /// This Exomonad installation provides the internal namespace-entry executable.
    pub exomonad_executable: PathBuf,
    pub workspace_inputs: Option<crate::exomonad::workspace::FrozenWorkspace>,
    pub workspace: PathBuf,
    pub haskell_root: PathBuf,
    pub run_root: PathBuf,
    pub root_binding_path: PathBuf,
    pub backend: crate::exomonad::HostBackendOptions,
    pub embedded: Option<crate::exomonad::EmbeddedLaunchConfig>,
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
// The local forest admits its root first, before any child actor identities.
const ROOT_ACTOR_ID: exomonad_actor::ActorId = exomonad_actor::ActorId(1);

#[cfg(feature = "codex-compat")]
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
    unavailable: Vec<UnavailablePredecessor>,
}

struct UnavailablePredecessor {
    actor: ActorRef,
    directory: String,
}

impl PredecessorRecovery {
    pub(crate) fn root_available(&self) -> bool {
        !self
            .unavailable
            .iter()
            .any(|entry| entry.actor.id == ROOT_ACTOR_ID)
    }

    pub(crate) fn unavailable_names(&self) -> Vec<String> {
        self.unavailable
            .iter()
            .map(|entry| entry.directory.clone())
            .collect()
    }

    fn mark_unavailable(&mut self, actor: ActorRef, directory: &str) {
        self.unavailable.push(UnavailablePredecessor {
            actor,
            directory: directory.to_owned(),
        });
    }
}

fn actor_ref_from_directory(name: &str) -> Option<ActorRef> {
    let (id, incarnation) = name.split_once('-')?;
    Some(ActorRef {
        id: exomonad_actor::ActorId(id.parse().ok()?),
        incarnation: exomonad_actor::Incarnation(incarnation.parse().ok()?),
    })
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
        let Some(actor_ref) = actor_ref_from_directory(&actor_name) else {
            continue;
        };
        let record_path = actor.path().join(PROCESS_RECOVERY_RECORD);
        let bytes = match std::fs::read(&record_path) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                report.mark_unavailable(actor_ref, &actor_name);
                continue;
            }
            Err(error) => return Err(error),
        };
        let mut record: ProcessRecoveryRecord = match serde_json::from_slice(&bytes) {
            Ok(record) => record,
            Err(error) => {
                tracing::warn!(actor = %actor_name, %error, "corrupt predecessor evidence");
                report.mark_unavailable(actor_ref, &actor_name);
                continue;
            }
        };
        if record.version != 1 {
            report.mark_unavailable(actor_ref, &actor_name);
            continue;
        }
        if record.retired {
            // The durable marker is written before deleting this directory.
            // A previous host can die between those two operations, so a
            // retired record also authorizes retrying only the idempotent
            // socket cleanup. It does not authorize another process stop.
            remove_predecessor_socket_root(&record.socket_root)?;
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
                report.mark_unavailable(actor_ref, &actor_name);
                continue;
            }
        };
        if !matches!(
            terminal,
            exomonad_node::ProcessSupervisorObservation::ProcessStopped
                | exomonad_node::ProcessSupervisorObservation::NotSpawned
        ) {
            report.mark_unavailable(actor_ref, &actor_name);
            continue;
        }
        record.retired = true;
        tidepool_atomic_write::write_durable(
            &record_path,
            &serde_json::to_vec_pretty(&record).map_err(std::io::Error::other)?,
        )
        .map_err(std::io::Error::from)?;
        remove_predecessor_socket_root(&record.socket_root)?;
        report.stopped += 1;
        // Process evidence is enough to prove that replacing the root is safe,
        // but it does not reconstruct a child actor's lost Haskell state,
        // lineage, or mailbox. Keep that child visible as unavailable until an
        // actor-owned durable record can restore those identities.
        if actor_ref.id != ROOT_ACTOR_ID {
            report.mark_unavailable(actor_ref, &actor_name);
        }
    }
    Ok(report)
}

fn remove_predecessor_socket_root(path: &Path) -> Result<(), std::io::Error> {
    match std::fs::remove_dir_all(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
    }
}

#[cfg(feature = "codex-compat")]
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

#[cfg(feature = "codex-compat")]
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

fn latest_durable_root_application(
    records: &[exomonad_actor::DurableActorRecord],
) -> Result<Option<&exomonad_actor::DurableActorRecord>, Box<dyn std::error::Error>> {
    let startup_records: Vec<_> = records
        .iter()
        .filter(|record| record.startup.is_some())
        .collect();
    if !startup_records.is_empty() {
        let predecessors: std::collections::BTreeSet<_> = startup_records
            .iter()
            .filter_map(|record| {
                record
                    .startup
                    .as_ref()
                    .and_then(|intent| intent.predecessor)
            })
            .collect();
        let heads: Vec<_> = startup_records
            .iter()
            .copied()
            .filter(|record| !predecessors.contains(&record.admission.actor))
            .collect();
        if heads.len() != 1 {
            return Err(runtime_error(
                "root startup chain has no unique unsucceeded head",
            ));
        }
        let head = heads[0];
        let mut cursor = Some(head.admission.actor);
        let mut seen = std::collections::BTreeSet::new();
        while let Some(actor) = cursor {
            if !seen.insert(actor) {
                return Err(runtime_error("root startup chain contains a cycle"));
            }
            let record = startup_records
                .iter()
                .find(|record| record.admission.actor == actor)
                .ok_or_else(|| runtime_error("root startup predecessor is absent"))?;
            cursor = record
                .startup
                .as_ref()
                .and_then(|intent| intent.predecessor);
        }
        if seen.len() != startup_records.len() {
            return Err(runtime_error(
                "root startup chain has disconnected admissions",
            ));
        }
        return Ok(Some(head));
    }
    if records.iter().any(|record| {
        record.admission.actor_path.as_deref() == Some("root") && record.application.is_some()
    }) {
        return Err(StartupRecoveryRefusal::MissingRootStartupIntent.into());
    }
    Ok(None)
}

// Owner selection stops at the first bound application, which may have run
// effects. Earlier authority can never be revived across that boundary.
fn root_startup_chain(
    records: &[exomonad_actor::DurableActorRecord],
) -> Result<Vec<&exomonad_actor::DurableActorRecord>, Box<dyn std::error::Error>> {
    let mut chain = Vec::new();
    let mut cursor = latest_durable_root_application(records)?;
    while let Some(record) = cursor {
        chain.push(record);
        if record
            .application
            .as_ref()
            .is_some_and(|application| application.conversation.is_some())
        {
            break;
        }
        cursor = record
            .startup
            .as_ref()
            .and_then(|intent| intent.predecessor)
            .and_then(|previous| {
                records
                    .iter()
                    .find(|record| record.admission.actor == previous)
            });
    }
    Ok(chain)
}

fn durable_root_identity(
    records: &[exomonad_actor::DurableActorRecord],
    accepted_source: Option<&str>,
) -> Result<Option<(ActorRef, ActorRef)>, Box<dyn std::error::Error>> {
    latest_durable_root_application(records)?
        .filter(|record| {
            record.application.as_ref().is_some_and(|application| {
                application.accepted_source.as_deref() == accepted_source
                    && ((record.startup.is_some() && application.conversation.is_none())
                        || (record.terminal.is_none() && application.conversation.is_some()))
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

#[cfg(feature = "codex-compat")]
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
#[cfg(feature = "codex-compat")]
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
            let Some(expected_conversation) = application
                .conversation
                .as_ref()
                .and_then(exomonad_actor::ApplicationConversation::codex_thread)
            else {
                continue;
            };
            if application.accepted_source.as_deref() != accepted_source {
                tracing::warn!(actor = %durable.actor,
                    recorded_source = ?application.accepted_source,
                    current_source = ?accepted_source,
                    "durable actor accepted-source identity cannot be verified");
                continue;
            }
            if has_legacy_checkout_source_layer(run_root, &durable.source_layer) {
                tracing::warn!(actor = %durable.actor,
                    "legacy checkout tooling layer cannot be adopted by the coherent run-source host");
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

#[cfg(feature = "codex-compat")]
fn has_legacy_checkout_source_layer(run_root: &Path, source_layer: &[PathBuf]) -> bool {
    let legacy = run_root.join("workspace/checkouts");
    source_layer.iter().any(|path| path.starts_with(&legacy))
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
                    // The actor's Haskell error surface treats provider
                    // refusal as HTTP; preserve that contract for fast fails.
                    JevFailure::CircuitOpen {
                        status,
                        retry_after_ms,
                    } => Failure::Http(
                        i64::from(status),
                        format!("Jev circuit open; retry after {retry_after_ms} ms"),
                    ),
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

/// Resolve a requested `Model` (an alias into the frozen workspace's model
/// table, or an already-literal provider model name) against the host
/// config. Shared by the real launch preview below and by recipe checks'
/// `RecipeActivation`, so both report the same resolved model string instead
/// of one of them echoing the alias back unresolved.
pub(crate) fn resolve_model(
    config: &ActorHostConfig,
    model: &exomonad_actor::Model,
) -> std::result::Result<String, String> {
    match model {
        exomonad_actor::Model::Alias(alias) => config
            .workspace_inputs
            .as_ref()
            .and_then(|workspace| workspace.models.get(alias))
            .cloned()
            .ok_or_else(|| format!("unknown frozen workspace model alias: {alias}")),
        exomonad_actor::Model::Literal(model) => Ok(model.clone()),
    }
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
    let model = request
        .model
        .as_ref()
        .map(|model| resolve_model(config, model))
        .transpose()?;
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
    /// The embedded browser listener is bound and its root conversation is
    /// attached to the admitted resident actor.
    EmbeddedReady {
        root: ActorRef,
        address: std::net::SocketAddr,
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

#[cfg(feature = "codex-compat")]
struct InteractiveDeployment {
    _provider_attachment: provider_attachment::ProviderAttachment,
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
    checkpoint: Option<exomonad_actor::CheckpointLease>,
    runtime_observation: exomonad_actor::ActorRuntimeObservationHandle,
    fork_parent_thread: Option<BackendThreadId>,
}

fn embedded_root_attachment_error(
    actor: ActorRef,
    is_root: bool,
    has_checkpoint: bool,
    context_parent: Option<ActorRef>,
) -> Option<String> {
    if has_checkpoint {
        return Some(format!(
            "embedded actor {actor:?} has a retained checkpoint attachment that M1 cannot restore"
        ));
    }
    if context_parent.is_some() {
        return Some(format!(
            "embedded actor {actor:?} inherits context from {context_parent:?}; M1 cannot restore inherited claims"
        ));
    }
    if !is_root {
        return Some(format!(
            "embedded child actor {actor:?} has no explicit fresh-launch attachment in M1"
        ));
    }
    None
}

#[allow(
    clippy::too_many_arguments,
    reason = "compose the existing Store and actor authority owners"
)]
async fn dispatch_embedded_browser_command(
    operation: harness::embedding::ClientOperationId,
    command: &harness::server::HostCommand,
    store: &harness::store::Store,
    run: &str,
    nodes: &[exomonad_actor::ActorGraphNode],
    live: &BTreeSet<ActorRef>,
    projection: &embedded_projection::EmbeddedProjection,
    conversation_for: impl Fn(ActorRef) -> Option<Arc<harness::embedding::Conversation>>,
    lifecycle: &embedded_projection::LifecycleSender,
) -> Result<Option<harness::server::CommandReceipt>, String> {
    use harness::{embedding::HostControlError, server::CommandReceiptOutcome};

    let command_run = &command.target().run;
    let retained = store
        .embedded_command(command_run, operation)
        .map_err(|error| error.to_string())?
        .ok_or_else(|| "browser command was not durably queued".to_owned())?;
    if retained.command != *command {
        return Err("browser operation identity has conflicting command contents".into());
    }
    // Retained observations outlive actor admission and projection retention.
    // Retry never consults a retired actor or sends another input wake.
    if let Some(receipt) = retained.receipt {
        return Ok(Some(receipt));
    }
    let Some(_) = store
        .claim_embedded_command(command_run, operation)
        .map_err(|error| error.to_string())?
    else {
        // Another delivery has already claimed this operation. In particular,
        // an uncertain control cannot be replayed against a later round.
        return Ok(None);
    };
    let settle = |outcome| {
        store
            .settle_embedded_command(command_run, operation, outcome)
            .map(Some)
            .map_err(|error| error.to_string())
    };
    let target = command.target().clone();
    let refuse = |reason: String| {
        settle(CommandReceiptOutcome::Refused {
            target: Some(target.clone()),
            reason,
        })
    };
    if target.run != run {
        return refuse("target belongs to a different host run".into());
    }
    let Some(actor) = projection.resolve_identity(run, &target, nodes) else {
        return refuse("target actor identity is not present in this host run".into());
    };
    let is_live = nodes
        .iter()
        .find(|node| node.actor == actor)
        .is_some_and(|node| node.terminal.is_none())
        && live.contains(&actor);
    if !is_live {
        return refuse("target actor is no longer live".into());
    }
    let Some(conversation) = conversation_for(actor) else {
        return refuse("target has no live model conversation".into());
    };

    let control = match command {
        harness::server::HostCommand::Input { text, .. } => {
            return match conversation.command_input(operation, text).await {
                Ok(receipt) => Ok(Some(receipt)),
                Err(error) => {
                    // A Store failure after the atomic admission commit must
                    // not turn a real envelope into a refused operation.
                    if let Some(receipt) = store
                        .embedded_command(command_run, operation)
                        .map_err(|error| error.to_string())?
                        .and_then(|record| record.receipt)
                    {
                        Ok(Some(receipt))
                    } else {
                        settle(CommandReceiptOutcome::Unconfirmed {
                            target,
                            reason: error.to_string(),
                        })
                    }
                }
            };
        }
        harness::server::HostCommand::Interrupt { expected_round, .. } => {
            harness::embedding::HostControl::Interrupt {
                expected_round: *expected_round,
            }
        }
        harness::server::HostCommand::Retire { .. } => harness::embedding::HostControl::Retire,
    };
    match conversation.control(control).await {
        Ok(_) => {
            let control = match command {
                harness::server::HostCommand::Retire { .. } => {
                    lifecycle.publish(actor, harness::server::HostActorLifecycle::Retiring);
                    harness::server::CommandControl::Retire
                }
                harness::server::HostCommand::Interrupt { .. } => {
                    harness::server::CommandControl::Interrupt
                }
                harness::server::HostCommand::Input { .. } => {
                    unreachable!("input returns its atomic receipt")
                }
            };
            settle(CommandReceiptOutcome::ControlRequested { target, control })
        }
        Err(HostControlError::Refused(reason)) => refuse(reason),
        Err(HostControlError::Unconfirmed(reason)) => {
            settle(CommandReceiptOutcome::Unconfirmed { target, reason })
        }
    }
}

#[allow(
    clippy::too_many_arguments,
    reason = "drain through the existing Store and actor authority owners"
)]
async fn drain_embedded_browser_commands(
    store: &harness::store::Store,
    control: &harness::server::ServerControl,
    run: &str,
    nodes: &[exomonad_actor::ActorGraphNode],
    live: &BTreeSet<ActorRef>,
    projection: &embedded_projection::EmbeddedProjection,
    conversations: &HashMap<ActorRef, embedded_harness::EmbeddedActorBinding>,
    lifecycle: &embedded_projection::LifecycleSender,
) -> Result<(), String> {
    // The channel is a wake hint. Store rows survive the persist/enqueue gap
    // and are also drained by the existing host health tick.
    for record in store
        .queued_embedded_commands(run)
        .map_err(|error| error.to_string())?
    {
        if let Some(receipt) = dispatch_embedded_browser_command(
            record.operation_id,
            &record.command,
            store,
            run,
            nodes,
            live,
            projection,
            |actor| {
                conversations
                    .get(&actor)
                    .filter(|binding| binding.is_live())
                    .and_then(|binding| binding.conversation())
            },
            lifecycle,
        )
        .await?
        {
            control.publish_command_receipt(receipt);
        }
    }
    Ok(())
}

#[derive(Clone)]
#[cfg(feature = "codex-compat")]
struct PendingUpdateReconciliation {
    inbox: Arc<ActorInbox>,
    sequence: u64,
    context: DeliveryProvenance,
    reconciler: exomonad_actor::RequestUpdateReconciler,
}

#[cfg(feature = "codex-compat")]
impl PendingUpdateReconciliation {
    #[cfg(feature = "codex-compat")]
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

#[cfg(feature = "codex-compat")]
enum InteractiveConnection {
    // Pane, inbox, tool listener, and cleanup are already owned in this state.
    AwaitingBinding,
    Bound {
        delivery_shutdown: oneshot::Sender<()>,
        delivery: tokio::task::JoinHandle<()>,
    },
}

#[cfg(feature = "codex-compat")]
struct InteractiveBindingRequest {
    control: crate::host_dynamic_tools::HostToolControl,
    path: PathBuf,
    expected: Option<BackendThreadId>,
}

#[cfg(feature = "codex-compat")]
struct LaunchedInteractiveApplication {
    deployment: InteractiveDeployment,
    binding: InteractiveBindingRequest,
}

#[cfg(feature = "codex-compat")]
struct OwnerNotification {
    owner: ActorRef,
    inbox: Arc<ActorInbox>,
    event: DurableActorEvent,
}

type ActorInbox = DurableInbox<DurableActorEvent, DeliveryProvenance>;
#[cfg(feature = "codex-compat")]
type WatchRetentionCheck = Arc<dyn Fn(ActorRef, exomonad_actor::WatchId) -> bool + Send + Sync>;
/// Whether the owner has already observed a watch (via `ObserveWatchWith` or
/// `pollWatch`) settled at or after the given `occurred_at_unix_ms`. Backed
/// by `ResidentForest::watch_observed_since`.
#[cfg(feature = "codex-compat")]
type WatchObservationCheck =
    Arc<dyn Fn(ActorRef, exomonad_actor::WatchId, u64) -> bool + Send + Sync>;
/// The request presented to an actor that it has not begun replying to.
/// Backed by `ResidentForest::open_request_without_reply`.
#[cfg(feature = "codex-compat")]
type OpenRequestCheck = Arc<dyn Fn(ActorRef) -> Option<exomonad_actor::RequestId> + Send + Sync>;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
enum DeliveryProvenance {
    Notification {
        sender: ActorRef,
        target: ActorRef,
    },
    #[cfg(feature = "codex-compat")]
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
    #[cfg(feature = "codex-compat")]
    Typed(TypedActorEvent),
    Text(String),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "camelCase")]
#[cfg(feature = "codex-compat")]
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
    #[cfg(feature = "codex-compat")]
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
    #[cfg(feature = "codex-compat")]
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
            Self::Typed(TypedActorEvent::SettlementChanged { notification })
                if notification.command_job.is_some() =>
            {
                let job = notification.command_job.as_deref().unwrap_or_default();
                match (&notification.transition, &notification.reply_preview) {
                    (exomonad_actor::SettlementTransition::Ready, Some(report)) => format!(
                        "job {job} finished ({}).\n{report}",
                        elapsed(notification.occurred_at_unix_ms),
                    ),
                    (transition, _) => format!(
                        "job {job} no longer reports its completion to you: {} ({}). Its retained output stays readable with read_output session_id={job}; nothing reruns.",
                        command_settlement_loss(transition),
                        elapsed(notification.occurred_at_unix_ms),
                    ),
                }
            }
            Self::Typed(TypedActorEvent::SettlementChanged { notification }) => {
                let identity = settlement_identity_line(notification);
                match &notification.reply_preview {
                    Some(preview) => format!(
                        "{identity}request {} {:?} settled {:?} ({}).\nReply:\n{preview}\n\nSettlement is not integration.",
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

/// Why a command job's settlement ended without its report, in words.
#[cfg(feature = "codex-compat")]
fn command_settlement_loss(transition: &exomonad_actor::SettlementTransition) -> &'static str {
    use exomonad_actor::{ResponseFailure, SettlementTransition};
    match transition {
        SettlementTransition::Ready => "its report was not retained",
        SettlementTransition::Unavailable(ResponseFailure::RequesterStopped) => {
            "the actor that owned it stopped"
        }
        SettlementTransition::Unavailable(ResponseFailure::Released) => {
            "its settlement was released"
        }
        SettlementTransition::Unavailable(_) => "its settlement became unavailable",
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
#[cfg(feature = "codex-compat")]
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
    #[cfg(feature = "codex-compat")]
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

    #[cfg(feature = "codex-compat")]
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
    #[cfg(feature = "codex-compat")]
    supervisor: Option<ActorRef>,
    creator_workspace: Option<BoundWorkspace>,
    cancel: Option<oneshot::Sender<NativeRetirement>>,
    native_retirement: NativeRetirement,
    pane: Arc<Mutex<Option<TmuxPaneId>>>,
    fork_gate: Option<exomonad_actor::ForkGroupGate>,
    custody: Option<Arc<dyn exomonad_actor::ForkWorkspaceCustody>>,
    scoped_retention: Option<scoped_custody::ScopedHostRetention>,
    #[cfg(feature = "codex-compat")]
    hosted: hosted_retirement::HostedSlot,
    embedded_policy: Option<Arc<embedded_policy::EmbeddedPolicyInstallation>>,
    #[cfg(feature = "codex-compat")]
    launch: HostLaunchState,
    #[cfg(feature = "codex-compat")]
    pending_activations: Vec<exomonad_actor::ResidentActivation>,
    embedded: Option<EmbeddedApplicationState>,
    terminal: Option<ActorTerminal>,
    retirement: Arc<Mutex<Option<InteractiveCleanupReceipt>>>,
}

/// Per-actor Engine custody, liveness, and retained conversation state.
/// Projection state remains in `EmbeddedProjection`; this state owns the
/// resources and facts needed to move one actor through its host lifecycle.
struct EmbeddedApplicationState {
    task_id: Option<tokio::task::Id>,
    cancellation: Option<watch::Sender<bool>>,
    live: bool,
    conversation: Option<embedded_harness::EmbeddedActorBinding>,
    pending_activations: Option<Vec<exomonad_actor::ResidentActivation>>,
    cleanup_failure: Option<embedded_service::EmbeddedDriverError>,
}

impl EmbeddedApplicationState {
    fn new() -> Self {
        Self {
            task_id: None,
            cancellation: None,
            live: false,
            conversation: None,
            pending_activations: None,
            cleanup_failure: None,
        }
    }
}

/// Coordination failure does not authorize terminating the native conversation.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
enum NativeRetirement {
    #[default]
    Preserve,
    Terminate,
}

#[derive(Clone)]
#[cfg(feature = "codex-compat")]
enum HostLaunchState {
    Pending,
    Published,
    Abandoned,
    Failed(String),
}

#[cfg(feature = "codex-compat")]
impl HostLaunchState {
    /// Phrase describing why the host cannot yet (or ever) hand off a
    /// published application for this actor, for surfacing to a caller whose
    /// delivery landed on an admitted actor with no provider running.
    #[cfg(feature = "codex-compat")]
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

fn with_embedded_state<R>(
    owners: &InteractiveOwners,
    actor: ActorRef,
    update: impl FnOnce(&mut EmbeddedApplicationState) -> R,
) -> Option<R> {
    owners
        .lock()
        .get_mut(&actor)
        .and_then(|owner| owner.embedded.as_mut())
        .map(update)
}

fn update_embedded_state(
    owners: &InteractiveOwners,
    actor: ActorRef,
    update: impl FnOnce(&mut EmbeddedApplicationState),
) {
    if let Some(state) = owners
        .lock()
        .get_mut(&actor)
        .and_then(|owner| owner.embedded.as_mut())
    {
        update(state);
    }
}

fn embedded_is_live(owners: &InteractiveOwners, actor: ActorRef) -> bool {
    owners
        .lock()
        .get(&actor)
        .and_then(|owner| owner.embedded.as_ref())
        .is_some_and(|embedded| embedded.live)
}

fn embedded_binding(
    owners: &InteractiveOwners,
    actor: ActorRef,
) -> Option<embedded_harness::EmbeddedActorBinding> {
    owners
        .lock()
        .get(&actor)
        .and_then(|owner| owner.embedded.as_ref())
        .and_then(|embedded| embedded.conversation.clone())
}

fn embedded_bindings(
    owners: &InteractiveOwners,
) -> HashMap<ActorRef, embedded_harness::EmbeddedActorBinding> {
    owners
        .lock()
        .iter()
        .filter_map(|(actor, owner)| {
            owner
                .embedded
                .as_ref()
                .and_then(|embedded| embedded.conversation.clone())
                .map(|binding| (*actor, binding))
        })
        .collect()
}

fn embedded_live_actors(owners: &InteractiveOwners) -> BTreeSet<ActorRef> {
    owners
        .lock()
        .iter()
        .filter_map(|(actor, owner)| {
            owner
                .embedded
                .as_ref()
                .is_some_and(|embedded| embedded.live)
                .then_some(*actor)
        })
        .collect()
}

fn embedded_actor_for_task(owners: &InteractiveOwners, task: tokio::task::Id) -> Option<ActorRef> {
    owners.lock().iter().find_map(|(actor, owner)| {
        owner
            .embedded
            .as_ref()
            .filter(|embedded| embedded.task_id == Some(task))
            .map(|_| *actor)
    })
}

#[derive(Clone)]
struct BoundWorkspace {
    workspace: Arc<ActiveWorkspace>,
    thread: QueueReadyThread,
}

#[derive(Clone)]
#[cfg(feature = "codex-compat")]
struct NativeForkAdmission {
    owners: InteractiveOwners,
    backend: Arc<dyn InteractiveAgentBackend>,
    layout: Option<WorkspaceLayout>,
}

#[cfg(feature = "codex-compat")]
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
#[cfg(feature = "codex-compat")]
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

#[cfg(feature = "codex-compat")]
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
    fn should_retire_undeployed_native(&self, has_deployment: bool) -> bool {
        self.embedded.is_none() && !has_deployment
    }

    fn embedded() -> Self {
        Self {
            #[cfg(feature = "codex-compat")]
            supervisor: None,
            creator_workspace: None,
            cancel: None,
            native_retirement: NativeRetirement::Preserve,
            pane: Arc::new(Mutex::new(None)),
            fork_gate: None,
            custody: None,
            scoped_retention: None,
            #[cfg(feature = "codex-compat")]
            hosted: Arc::new(Mutex::new(None)),
            embedded_policy: None,
            #[cfg(feature = "codex-compat")]
            launch: HostLaunchState::Published,
            #[cfg(feature = "codex-compat")]
            pending_activations: Vec::new(),
            embedded: Some(EmbeddedApplicationState::new()),
            terminal: None,
            retirement: Arc::new(Mutex::new(None)),
        }
    }

    #[cfg(feature = "codex-compat")]
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
        drop(self.embedded_policy.take());
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

/// Host-only operations. These never select a launch mode, release the command
/// gate, establish host-work quiescence, or settle workspace custody.
#[cfg(test)]
pub(crate) enum RetainedProcessOperation {
    Observe,
    #[cfg(test)]
    Pin,
    Stop,
}

#[cfg(test)]
#[derive(Debug)]
pub(crate) enum RetainedProcessState {
    Reserved,
    Blocked,
    Pinned,
    Released,
    ReleaseUnconfirmed,
    Stopping,
    #[allow(
        dead_code,
        reason = "carried to keep the cleanup handle's own drop-time effects \
                  alive on the observation; tests only match the variant"
    )]
    ProcessStopped(Option<exomonad_node::ServiceScopeCleanup>),
}

#[cfg(test)]
#[derive(Debug)]
pub(crate) struct RetainedProcessObservation {
    pub(crate) actor: ActorRef,
    pub(crate) actor_terminal: Option<ActorTerminal>,
    pub(crate) process: RetainedProcessState,
}

#[cfg(test)]
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

#[cfg(test)]
impl RetainedInteractiveFleet {
    /// Recover the actual resource-bearing error after the host's ordinary
    /// Box<dyn Error> propagation. Crate visibility lets exomonad own a subsequent
    /// recovery policy without exposing this mechanism to authored programs.
    pub(crate) fn from_error<'a>(
        error: &'a mut (dyn std::error::Error + 'static),
    ) -> Option<&'a mut Self> {
        error.downcast_mut::<Self>()
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
        owner.scoped_retention.is_some() || owner.custody.is_some() || {
            #[cfg(feature = "codex-compat")]
            {
                owner.hosted.lock().is_some()
            }
            #[cfg(not(feature = "codex-compat"))]
            {
                false
            }
        }
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

#[cfg(feature = "codex-compat")]
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

#[cfg(feature = "codex-compat")]
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

#[cfg(feature = "codex-compat")]
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

#[cfg(feature = "codex-compat")]
#[derive(Debug, thiserror::Error)]
#[error("actor {actor:?} failed to {operation}: {detail}")]
struct InteractiveApplicationError {
    actor: ActorRef,
    operation: InteractiveOperation,
    detail: String,
    disposition: LaunchDisposition,
}

#[cfg(feature = "codex-compat")]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LaunchDisposition {
    Failed,
    Cancelled,
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

#[cfg(feature = "codex-compat")]
fn application_error(
    actor: ActorRef,
    operation: InteractiveOperation,
    error: impl fmt::Display,
) -> InteractiveApplicationError {
    InteractiveApplicationError {
        actor,
        operation,
        detail: error.to_string(),
        disposition: LaunchDisposition::Failed,
    }
}

struct InteractiveFleet {
    provider_forest: Arc<ResidentForest<ExomonadHandlerStack, CapturedOutput>>,
    root: LocalActorRef,
    config: ActorHostConfig,
    run_root: PathBuf,
    output_store: Arc<harness::store::Store>,
    #[cfg(feature = "codex-compat")]
    tmux: TmuxSession,
    #[cfg(feature = "codex-compat")]
    backend: HostRuntimeMode,
    worktrees: WorktreeManager,
    #[cfg(feature = "codex-compat")]
    bindings: Arc<Mutex<BindingTable>>,
    /// Readiness events are best-effort notifications: a dropped receiver
    /// means the caller stopped observing startup, not a delivery bug, so
    /// every `readiness.send(..)` below discards the `SendError` with `.ok()`.
    readiness: mpsc::UnboundedSender<ActorHostReadiness>,
    worktree_authority: ActorWorktreeAuthority,
    #[cfg(feature = "codex-compat")]
    watch_retention: WatchRetentionCheck,
    #[cfg(feature = "codex-compat")]
    watch_observation: WatchObservationCheck,
    #[cfg(feature = "codex-compat")]
    open_request: OpenRequestCheck,
    /// `None` when the run has no frozen workspace to compare against, in
    /// which case source drift is never observed (see
    /// `run_delivery_pump`'s source observation loop).
    #[cfg(feature = "codex-compat")]
    source_layers: Option<Arc<crate::exomonad::source::ExomonadSourceReload>>,
    #[cfg(feature = "codex-compat")]
    actor_recovery: Arc<exomonad_actor::ActorRecoveryJournal>,
    #[cfg(feature = "codex-compat")]
    recovered_threads: Arc<BTreeMap<ActorRef, (ActorRef, QueueReadyThread)>>,
    #[cfg(feature = "codex-compat")]
    recovered_root_predecessor: Option<ActorRef>,
    host_graph: Arc<dyn Fn() -> Vec<exomonad_actor::ActorGraphNode> + Send + Sync>,
}

#[derive(Clone)]
struct InteractiveLaunchContext {
    base_prompt: FrozenBasePrompt,
    root: ActorRef,
    config: ActorHostConfig,
    run_root: PathBuf,
    #[cfg(feature = "codex-compat")]
    tmux: TmuxSession,
    #[cfg(feature = "codex-compat")]
    backend: HostRuntimeMode,
    worktrees: WorktreeManager,
    #[cfg(feature = "codex-compat")]
    source_layers: Option<Arc<crate::exomonad::source::ExomonadSourceReload>>,
    #[cfg(feature = "codex-compat")]
    bindings: Arc<Mutex<BindingTable>>,
    #[cfg(feature = "codex-compat")]
    actor_recovery: Arc<exomonad_actor::ActorRecoveryJournal>,
    #[cfg(feature = "codex-compat")]
    recovered_threads: Arc<BTreeMap<ActorRef, (ActorRef, QueueReadyThread)>>,
}

#[derive(Clone)]
enum HostRuntimeMode {
    #[cfg(feature = "codex-compat")]
    Codex(Arc<dyn InteractiveAgentBackend>),
    Embedded,
}

impl HostRuntimeMode {
    #[cfg(feature = "codex-compat")]
    fn codex_backend(&self) -> Option<&Arc<dyn InteractiveAgentBackend>> {
        match self {
            #[cfg(feature = "codex-compat")]
            Self::Codex(backend) => Some(backend),
            Self::Embedded => None,
        }
    }
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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum JournalOpenMode {
    Create,
    Resume,
}

#[derive(Debug, thiserror::Error)]
enum StartupRecoveryRefusal {
    #[error(
        "root application lacks its required atomic startup intent; retained state is unavailable"
    )]
    MissingRootStartupIntent,
    #[cfg(feature = "codex-compat")]
    #[error("Codex root recovery lacks the required atomic startup intent; retained state is unavailable")]
    CodexMissingStartupIntent,
}

// The host incarnation fences identities even when a predecessor failed before
// reaching the actor host. Its number alone cannot prove that a journal exists.
fn actor_journal_mode(
    incarnation: exomonad_actor::Incarnation,
    root_binding: &Path,
    actor_journal: &Path,
    run_journal: &Path,
) -> std::io::Result<JournalOpenMode> {
    if incarnation == exomonad_actor::Incarnation::FIRST {
        return Ok(JournalOpenMode::Create);
    }
    if actor_journal.try_exists()? || root_binding.try_exists()? || run_journal.try_exists()? {
        Ok(JournalOpenMode::Resume)
    } else {
        Ok(JournalOpenMode::Create)
    }
}

// The actor journal is written before root compilation opens the run journal.
// A restart between those steps may create the latter only if no actor was
// admitted and no root conversation was bound.
fn run_journal_mode(
    incarnation: exomonad_actor::Incarnation,
    root_binding: &Path,
    run_journal: &Path,
    no_prior_actors: bool,
) -> std::io::Result<JournalOpenMode> {
    if incarnation == exomonad_actor::Incarnation::FIRST {
        return Ok(JournalOpenMode::Create);
    }
    if run_journal.try_exists()? || root_binding.try_exists()? || !no_prior_actors {
        Ok(JournalOpenMode::Resume)
    } else {
        Ok(JournalOpenMode::Create)
    }
}

pub(crate) async fn run(
    config: ActorHostConfig,
    readiness: mpsc::UnboundedSender<ActorHostReadiness>,
    host_incarnation: HostIncarnationLease,
) -> Result<(), Box<dyn std::error::Error>> {
    run_owned(
        config,
        readiness,
        host_incarnation,
        #[cfg(test)]
        None,
    )
    .await
}

#[cfg(test)]
async fn run_with_test_transport(
    config: ActorHostConfig,
    readiness: mpsc::UnboundedSender<ActorHostReadiness>,
    host_incarnation: HostIncarnationLease,
    transport: Arc<dyn harness::engine::ResponsesTransport>,
) -> Result<(), Box<dyn std::error::Error>> {
    run_owned(config, readiness, host_incarnation, Some(transport)).await
}

async fn run_owned(
    mut config: ActorHostConfig,
    readiness: mpsc::UnboundedSender<ActorHostReadiness>,
    host_incarnation: HostIncarnationLease,
    #[cfg(test)] test_transport: Option<Arc<dyn harness::engine::ResponsesTransport>>,
) -> Result<(), Box<dyn std::error::Error>> {
    let host_incarnation = Arc::new(host_incarnation);
    let run_root = config.run_root.clone();
    std::fs::create_dir_all(&run_root)?;
    let workspace = config.workspace.clone();
    let resource_run_root = run_root.clone();
    let (worktrees, bindings) = tidepool_runtime::spawn_blocking_in_span(move || {
        actor_worktree_resources(&workspace, &resource_run_root)
    })
    .await??;
    let bindings = Arc::new(Mutex::new(bindings));
    let worktree_authority =
        ActorWorktreeAuthority::new(runtime_namespace(&run_root), Arc::clone(&bindings));
    let tmux = TmuxSession::new(&config.tmux_session)?;
    if config.backend.kind() != crate::exomonad::ExomonadBackend::Embedded && !tmux.exists().await?
    {
        return Err(runtime_error(format!(
            "Exomonad tmux session {:?} does not exist",
            config.tmux_session
        )));
    }
    #[cfg(feature = "codex-compat")]
    let runtime_backend = match &config.backend {
        #[cfg(feature = "codex-compat")]
        crate::exomonad::HostBackendOptions::Codex(installation) => {
            HostRuntimeMode::Codex(native_interactive_backend(installation.clone()))
        }
        crate::exomonad::HostBackendOptions::Embedded => HostRuntimeMode::Embedded,
    };
    let application_owners: InteractiveOwners = Arc::new(Mutex::new(HashMap::new()));
    let source_layers = source_service(
        &config,
        &run_root,
        worktrees.clone(),
        crate::exomonad::source::SourceRootOwner::Host(Arc::clone(&host_incarnation)),
    )?;
    let actor_recovery_path = run_root.join("actor-lifecycle.v2.jsonl");
    let run_id = run_root
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| runtime_error("run root has no UTF-8 run identifier"))?;
    let run_journal_path = crate::exomonad::exomonad_journal_path(&config.workspace, run_id);
    let actor_journal_mode = actor_journal_mode(
        host_incarnation.incarnation(),
        &config.root_binding_path,
        &actor_recovery_path,
        &run_journal_path,
    )?;
    let actor_recovery = if actor_journal_mode == JournalOpenMode::Create {
        exomonad_actor::ActorRecoveryJournal::open(actor_recovery_path)
    } else {
        exomonad_actor::ActorRecoveryJournal::open_existing(actor_recovery_path)
    }?;
    let prior_actor_records = actor_recovery.records();
    #[cfg(feature = "codex-compat")]
    if config.backend.kind() == crate::exomonad::ExomonadBackend::Codex
        && contains_durable_root_admission(&prior_actor_records)
    {
        return Err(StartupRecoveryRefusal::CodexMissingStartupIntent.into());
    }
    let run_journal_mode = run_journal_mode(
        host_incarnation.incarnation(),
        &config.root_binding_path,
        &run_journal_path,
        prior_actor_records.is_empty(),
    )?;
    let embedded_service = if config.backend.kind() == crate::exomonad::ExomonadBackend::Embedded {
        let settings = config
            .embedded
            .as_ref()
            .ok_or_else(|| runtime_error("embedded backend requires [launch.embedded]"))?;
        Some(
            embedded_service::EmbeddedService::prepare_owned(
                &run_root,
                settings,
                Arc::clone(&host_incarnation),
            )
            .await
            .map_err(runtime_error)?,
        )
    } else {
        None
    };
    let output_store = match embedded_service.as_ref() {
        Some(service) => service.runtime.store(),
        None => display_output::open_run_store(&run_root).map_err(runtime_error)?,
    };
    #[cfg(test)]
    let mut embedded_service = embedded_service;
    #[cfg(test)]
    if let (Some(service), Some(transport)) = (&mut embedded_service, test_transport) {
        service.set_test_transport(transport);
    }
    let mut embedded_startup =
        embedded_service
            .as_ref()
            .map(|service| embedded_recovery::EmbeddedStartupRecovery {
                lease: Arc::clone(&host_incarnation),
                journal: Arc::clone(&actor_recovery),
                store: service.runtime.store(),
                root_binding_path: config.root_binding_path.clone(),
                manifest: None,
            });
    let (source, root, program, child_session_factory, image_registry) = compile_root(
        &config,
        &run_root,
        worktrees.clone(),
        worktree_authority.clone(),
        source_layers.as_ref(),
        Arc::clone(&host_incarnation),
        run_journal_mode,
        embedded_startup.as_mut(),
    )?;
    let prior_actor_records = actor_recovery.records();
    let accepted_source = active_source_identity(&run_root, config.workspace_inputs.is_some())?;
    let (descriptor, mut machine, entry) = root.into_parts();
    let exomonad_actor::ResidentRootEntry::Startup(entry) = entry else {
        return Err(runtime_error(
            "root startup requires its installed executable entry",
        ));
    };
    let bootstrap_identity = entry.compile_input_identity().to_owned();
    let outcome = if embedded_service.is_some() {
        exomonad_actor::ResidentRootEntry::Startup(entry)
    } else {
        exomonad_actor::ResidentRootEntry::Prepared(machine.run_startup_entry(entry)?)
    };
    #[cfg(feature = "codex-compat")]
    let native_fork_admission = match &runtime_backend {
        #[cfg(feature = "codex-compat")]
        HostRuntimeMode::Codex(backend) => Some(NativeForkAdmission {
            owners: application_owners.clone(),
            backend: backend.clone(),
            layout: Some(WorkspaceLayout {
                run_namespace: runtime_namespace(&run_root),
                source_root: config.workspace.clone(),
                source_exclude: config.source_exclude.clone(),
                source_import: config.source_import,
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
        HostRuntimeMode::Embedded => None,
    };
    let worktree_admission = fork_workspace_admission(
        worktrees.clone(),
        worktree_authority.clone(),
        bindings.clone(),
        runtime_namespace(&run_root),
        #[cfg(feature = "codex-compat")]
        native_fork_admission,
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
        .with_child_session_factory(child_session_factory)
        .with_handler_effect_support(tidepool_mcp::InstalledEffectSupport::installed_effect_support)
        .with_image_registry(image_registry);
    #[cfg(feature = "codex-compat")]
    if let HostRuntimeMode::Codex(backend) = &runtime_backend {
        forest = forest.with_conversation_reader(conversation_reader(
            application_owners.clone(),
            backend.clone(),
        ));
    }
    if let Some(service) = &embedded_service {
        forest = forest.with_conversation_reader(embedded_reflect::run_conversation_reader(
            service.runtime.store(),
            actor_recovery.clone(),
        ));
    }
    // No child bootstrap program: every launch stays on its launching
    // session, as before per-actor machines.
    let _ = &program;
    forest.set_jev_backend(jev_backend(&config));
    if let Some(layers) = &source_layers {
        forest.set_source_layers(layers.clone());
    }
    if let (Some(service), Some(settings)) = (&embedded_service, &config.embedded) {
        service
            .runtime
            .configure_context_models(&config)
            .map_err(runtime_error)?;
        forest = forest
            .with_cell_model_factory(cell_model::admitted_factory(service, settings, &config));
    }
    forest.track_resource_release();
    let forest = Arc::new(forest);
    let recovered_root = durable_root_identity(&prior_actor_records, accepted_source.as_deref())?;
    if contains_durable_root_admission(&prior_actor_records) && recovered_root.is_none() {
        return Err(runtime_error(
            "host recovery cannot adopt a root without complete durable actor, source, and conversation evidence",
        ));
    }
    #[cfg(feature = "codex-compat")]
    let recovered_root_predecessor = recovered_root.map(|(predecessor, _)| predecessor);
    #[cfg(feature = "codex-compat")]
    if let (true, Some(predecessor)) = (
        matches!(
            &config.backend,
            crate::exomonad::HostBackendOptions::Codex(_)
        ),
        recovered_root_predecessor,
    ) {
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
    let mut startup_intent = embedded_startup
        .as_ref()
        .map(|startup| {
            startup.intent(
                &run_root,
                recovered_root
                    .map(|(_, identity)| identity)
                    .unwrap_or(ActorRef::first(exomonad_actor::ActorId(0))),
                accepted_source.clone(),
                bootstrap_identity.clone(),
            )
        })
        .transpose()?;
    let (mut root_actor, mut root_task, startup_release) = if let Some(intent) = &startup_intent {
        let exomonad_actor::ResidentRootEntry::Startup(entry) = outcome else {
            return Err(runtime_error(
                "embedded root startup lost its executable entry",
            ));
        };
        let (actor, task, release) = match recovered_root {
            Some((_, identity)) => {
                forest
                    .admit_pending_root_with_identity(descriptor, entry, identity, intent.clone())
                    .await?
            }
            None => {
                let mut intent = intent.clone();
                let run_root = run_root.clone();
                forest
                    .admit_pending_root(descriptor, entry, move |identity| {
                        intent.conversation = embedded_recovery::conversation(
                            &embedded_recovery::host_identity(&run_root, "/root", identity),
                        );
                        intent
                    })
                    .await?
            }
        };
        startup_intent
            .as_mut()
            .expect("pending intent")
            .conversation = embedded_recovery::conversation(&embedded_recovery::host_identity(
            &run_root,
            "/root",
            actor.identity(),
        ));
        (actor, task, Some(release))
    } else {
        let exomonad_actor::ResidentRootEntry::Prepared(outcome) = outcome else {
            return Err(runtime_error(
                "standalone root startup has no prepared outcome",
            ));
        };
        let (actor, task) = match recovered_root {
            Some((_, identity)) => {
                forest
                    .admit_root_with_identity(descriptor, outcome, identity)
                    .await?
            }
            None => forest.admit_root(descriptor, outcome).await?,
        };
        (actor, task, None)
    };
    #[cfg(test)]
    embedded_recovery_tests::startup_checkpoint("admitted");
    if let Err(error) = actor_recovery.prepare_application_with_intent(
        root_actor.identity(),
        config.root_binding_path.clone(),
        accepted_source.clone(),
        embedded_service.as_ref().map(|_| {
            embedded_recovery::conversation(&embedded_recovery::host_identity(
                &run_root,
                "/root",
                root_actor.identity(),
            ))
        }),
    ) {
        forest.shutdown().await;
        return Err(runtime_error(format!(
            "root application ownership could not be journalled before declaration recovery: {error}"
        )));
    }
    let declaration_recovery = async {
        if let (Some(intent), Some(release), Some(service)) =
            (&startup_intent, &startup_release, &embedded_service)
        {
            let placement = forest
                .root_recovery_placement(root_actor.identity())
                .map_err(|error| runtime_error(error.to_string()))?;
            let admission = intent
                .manifest_predecessor
                .map(|predecessor| {
                    actor_recovery.certify_root_successor(
                        predecessor,
                        placement,
                        accepted_source.as_deref(),
                        &config.root_binding_path,
                    )
                })
                .transpose()?;
            let manifest = match (intent.manifest_predecessor, admission.as_ref()) {
                (Some(predecessor), Some(admission)) => {
                    let owner = tidepool_runtime::session::RecoveryPublicOwner::new(
                        &root_declaration_recovery::root_path(),
                        predecessor.incarnation.0,
                    )
                    .ok_or_else(|| runtime_error("root manifest predecessor has no incarnation"))?;
                    forest
                        .transfer_recovered_root_public_owner(
                            root_actor.identity(),
                            &owner,
                            root_declaration_recovery::successor_authority(
                                Arc::clone(&host_incarnation),
                                Arc::clone(admission),
                            ),
                        )
                        .await
                        .map_err(|error| runtime_error(error.to_string()))?
                }
                _ => forest
                    .bind_durable_root_public_owner(root_actor.identity())
                    .await
                    .map_err(|error| runtime_error(error.to_string()))?,
            };
            match manifest {
                tidepool_runtime::session::PublicManifestCommit::Durable => {}
                tidepool_runtime::session::PublicManifestCommit::PublishedDurabilityUnconfirmed { .. } => {
                    forest
                        .confirm_durable_root_public_owner(root_actor.identity())
                        .await
                        .map_err(|error| runtime_error(error.to_string()))?;
                }
                outcome => return Err(runtime_error(format!(
                    "root startup manifest did not become durable: {outcome:?}"
                ))),
            }
            #[cfg(test)]
            embedded_recovery_tests::startup_checkpoint("manifest");
            let identity = embedded_recovery::host_identity(&run_root, "/root", root_actor.identity());
            let store = service.runtime.store();
            #[cfg(test)]
            let _uncertain_reader = embedded_recovery_tests::uncertain_store_reader();
            if let Some(predecessor) = &intent.store_predecessor {
                let predecessor = embedded_recovery::identity_from_conversation(predecessor)
                    .map_err(runtime_error)?;
                let admission = admission
                    .ok_or_else(|| runtime_error("Store successor lacks exact manifest admission"))?;
                let authority = embedded_recovery::EmbeddedBindingSuccessorAuthority {
                    lease: Arc::clone(&host_incarnation),
                    run_root: run_root.clone(),
                    admission,
                };
                store.transfer_embedded_binding(&predecessor, &identity, &authority)?;
            } else {
                let authority = embedded_recovery::EmbeddedBindingInitialAuthority {
                    lease: Arc::clone(&host_incarnation),
                    journal: Arc::clone(&actor_recovery),
                    run_root: run_root.clone(),
                    actor: root_actor.identity(),
                    intent: intent.clone(),
                };
                store.bind_initial_embedded_binding(&identity, None, &authority)?;
            }
            if !store.embedded_binding_matches(&identity)? {
                return Err(runtime_error("root startup Store binding readback differs after durable commit"));
            }
            #[cfg(test)]
            embedded_recovery_tests::startup_checkpoint("store");
            actor_recovery.bind_application_conversation(
                root_actor.identity(),
                intent.conversation.clone(),
            )?;
            #[cfg(test)]
            embedded_recovery_tests::startup_checkpoint("bound");
            forest
                .release_root_startup(release)
                .map_err(|error| runtime_error(error.to_string()))?;
            #[cfg(test)]
            embedded_recovery_tests::startup_checkpoint("released");
        } else {
            match forest
                .bind_durable_root_public_owner(root_actor.identity())
                .await
                .map_err(|error| runtime_error(error.to_string()))?
            {
                tidepool_runtime::session::PublicManifestCommit::Durable => {}
                outcome => return Err(runtime_error(format!(
                    "initial root declaration ownership did not become durable: {outcome:?}"
                ))),
            }
        }
        if let Some(service) = &embedded_service {
            service
                .runtime
                .configure_application_recovery(Arc::new(
                    embedded_recovery::EmbeddedApplicationRecovery {
                        lease: Arc::clone(&host_incarnation),
                        journal: Arc::clone(&actor_recovery),
                        run_root: run_root.clone(),
                        root: root_actor.identity(),
                        root_binding_path: config.root_binding_path.clone(),
                        accepted_source: accepted_source.clone(),
                        recovered_root: recovered_root.is_some(),
                    },
                ))
                .map_err(runtime_error)?;
        }
        Ok::<(), Box<dyn std::error::Error>>(())
    }
    .await;
    if let Err(error) = declaration_recovery {
        let summary = format!("root declaration recovery remains unavailable: {error}");
        let cleanup = tokio::time::timeout(
            APPLICATION_SHUTDOWN_TIMEOUT,
            root_actor.shutdown(ActorTerminal {
                kind: ActorExitKind::Failed,
                summary: summary.clone(),
            }),
        )
        .await;
        if !matches!(cleanup, Ok(Ok(_))) {
            tracing::warn!(actor = %root_actor.identity(), ?cleanup,
                "root declaration recovery cleanup remains unconfirmed");
        }
        forest.shutdown().await;
        return Err(runtime_error(summary));
    }
    #[cfg(feature = "codex-compat")]
    let recovered_threads = Arc::new(match &runtime_backend {
        HostRuntimeMode::Codex(_) => {
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
            .await
        }
        HostRuntimeMode::Embedded => BTreeMap::new(),
    });
    #[cfg(not(feature = "codex-compat"))]
    let recovered_threads = Arc::new(BTreeMap::<ActorRef, (ActorRef, QueueReadyThread)>::new());
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
    let artifact_owners = application_owners.clone();
    let artifact_run_id = runtime_namespace(&run_root);
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
        Arc::new(move |actor: ActorRef, relative_path: PathBuf| {
            let owners = artifact_owners.clone();
            let run_id = artifact_run_id.clone();
            Box::pin(async move {
                let logical_path = Path::new(ACTOR_PROJECT_ROOT)
                    .join(ACTOR_BUILD_TARGET)
                    .join(&relative_path)
                    .display()
                    .to_string();
                // Clone the exact resource while holding the owner map briefly;
                // its publication lock may wait for a rotation.
                let (build, retired) = {
                    let owners = owners.lock();
                    match owners.get(&actor) {
                        Some(owner) => (
                            owner
                                .creator_workspace
                                .as_ref()
                                .and_then(|bound| bound.workspace.build.clone()),
                            owner.terminal.is_some(),
                        ),
                        None => (None, false),
                    }
                };
                let availability = match build {
                    Some(build) => match build.inspect_artifact(&relative_path).await {
                        ArtifactInspection::Present(layers) => {
                            crate::run_map::ArtifactAvailability::BackingEntries {
                                entries: layers
                                    .into_iter()
                                    .map(|layer| crate::run_map::ArtifactLayer {
                                        order: layer.order,
                                        host_path: layer.path.display().to_string(),
                                        current_upper: layer.current_upper,
                                    })
                                    .collect(),
                                visible_source: crate::run_map::Evidence::Unknown {
                                    reason: "physical backing paths do not prove merged overlay visibility".into(),
                                },
                            }
                        }
                        ArtifactInspection::Missing => {
                            crate::run_map::ArtifactAvailability::Missing
                        }
                        ArtifactInspection::Retired => {
                            crate::run_map::ArtifactAvailability::Retired
                        }
                        ArtifactInspection::RefusedPath => {
                            crate::run_map::ArtifactAvailability::RefusedPath
                        }
                        ArtifactInspection::Unknown(reason) => {
                            crate::run_map::ArtifactAvailability::Unknown { reason }
                        }
                    },
                    None if retired => crate::run_map::ArtifactAvailability::Retired,
                    None => crate::run_map::ArtifactAvailability::Unknown {
                        reason: "No active build overlay owner for this exact actor".into(),
                    },
                };
                crate::run_map::ArtifactProvenance {
                    run_id,
                    actor,
                    logical_path,
                    availability,
                }
            })
        }),
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
    #[cfg(feature = "codex-compat")]
    let watch_forest = Arc::clone(&forest);
    let host_graph_forest = Arc::clone(&forest);
    let host_graph = Arc::new(move || host_graph_forest.inspect_host_graph());
    #[cfg(feature = "codex-compat")]
    let watch_retention: WatchRetentionCheck =
        Arc::new(move |owner, watch| watch_forest.retains_watch(owner, watch));
    #[cfg(feature = "codex-compat")]
    let watch_observation_forest = Arc::clone(&forest);
    #[cfg(feature = "codex-compat")]
    let watch_observation: WatchObservationCheck = Arc::new(move |owner, watch, occurred_at| {
        watch_observation_forest.watch_observed_since(owner, watch, occurred_at)
    });
    #[cfg(feature = "codex-compat")]
    let open_request_forest = Arc::clone(&forest);
    #[cfg(feature = "codex-compat")]
    let open_request: OpenRequestCheck =
        Arc::new(move |actor| open_request_forest.open_request_without_reply(actor));
    let mut applications_task = tokio::spawn(run_interactive_applications(
        deployments,
        application_owners.clone(),
        InteractiveFleet {
            provider_forest: Arc::clone(&forest),
            root: root_actor.clone(),
            config: config.clone(),
            run_root: run_root.clone(),
            output_store,
            #[cfg(feature = "codex-compat")]
            tmux: tmux.clone(),
            #[cfg(feature = "codex-compat")]
            backend: runtime_backend,
            worktrees,
            #[cfg(feature = "codex-compat")]
            bindings,
            readiness: readiness.clone(),
            worktree_authority: worktree_authority.clone(),
            #[cfg(feature = "codex-compat")]
            watch_retention,
            #[cfg(feature = "codex-compat")]
            watch_observation,
            #[cfg(feature = "codex-compat")]
            open_request,
            #[cfg(feature = "codex-compat")]
            source_layers,
            #[cfg(feature = "codex-compat")]
            actor_recovery: actor_recovery.clone(),
            #[cfg(feature = "codex-compat")]
            recovered_threads,
            #[cfg(feature = "codex-compat")]
            recovered_root_predecessor,
            host_graph,
        },
        shutdown_rx,
        root_config_rx,
        embedded_service,
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
                    let application = actor_recovery.records().into_iter()
                        .find(|record| record.admission.actor == root_actor.identity())
                        .and_then(|record| record.application);
                    let conversation = application.as_ref().and_then(|application|
                        application.conversation.as_ref().or(application.intended_conversation.as_ref()));
                    if terminal.kind == ActorExitKind::Failed && native_exit_required(conversation) {
                        let pane = application_owners.lock().get(&root_actor.identity())
                            .and_then(|owner| owner.pane.lock().clone());
                        if let Err(error) = confirm_native_exit(&tmux, pane.as_ref()).await {
                            readiness.send(ActorHostReadiness::CoordinationFailed {
                                root: root_actor.identity(),
                                error: terminal.summary.clone(),
                            }).ok();
                            if root_never_bound(application.as_ref(), &config.root_binding_path) {
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
                            if root_never_bound(application.as_ref(), &config.root_binding_path) {
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
    let forest_shutdown = forest.shutdown().await;
    let cleanup = if !applications_finished {
        await_applications(&mut applications_task, APPLICATION_SHUTDOWN_TIMEOUT).await
    } else {
        Ok(())
    };
    let unconfirmed = forest_shutdown
        .into_iter()
        .filter(|outcome| !outcome.is_confirmed())
        .collect::<Vec<_>>();
    let cleanup = match (cleanup, unconfirmed.is_empty()) {
        (cleanup, true) => cleanup,
        (Ok(()), false) => Err(runtime_error(format!(
            "resident forest cleanup unconfirmed: {unconfirmed:?}"
        ))),
        (Err(error), false) => Err(runtime_error(format!(
            "{error}; resident forest cleanup unconfirmed: {unconfirmed:?}"
        ))),
    };
    handoff_application_owners(application_owners, applications_task, cleanup, result)
}

/// An embedded application has no independently running native process.
/// Unknown ownership retains the native exit guard until its exit is proven.
fn native_exit_required(conversation: Option<&exomonad_actor::ApplicationConversation>) -> bool {
    !matches!(
        conversation,
        Some(exomonad_actor::ApplicationConversation::Embedded { .. })
    )
}

fn root_never_bound(
    application: Option<&exomonad_actor::DurableActorApplication>,
    root_binding_path: &Path,
) -> bool {
    match application.and_then(|application| application.conversation.as_ref()) {
        Some(exomonad_actor::ApplicationConversation::Embedded { .. }) => false,
        _ => !root_binding_path.exists(),
    }
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
    #[cfg(not(feature = "codex-compat"))]
    {
        let _ = (config, actor, terminal, resident_state, recovery);
        return Ok(RootRunDisposition::Complete);
    }
    #[cfg(feature = "codex-compat")]
    if matches!(
        &config.backend,
        crate::exomonad::HostBackendOptions::Embedded
    ) {
        return Ok(RootRunDisposition::Complete);
    }
    #[cfg(feature = "codex-compat")]
    {
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
}

#[cfg(feature = "codex-compat")]
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
    run_root: &Path,
) -> Result<(WorktreeManager, BindingTable), exomonad_worktree::WorktreeError> {
    let root = actor_worktree_storage_root(workspace, run_root)?;
    actor_worktree_resources_at(&root, workspace)
}

pub(crate) fn ensure_actor_workspace_available(
    workspace: &Path,
) -> Result<(), exomonad_worktree::WorktreeError> {
    let state = tidepool_toolchain::paths::state_dir().map_err(|error| {
        exomonad_worktree::WorktreeError::StorageFailure {
            path: workspace.to_path_buf(),
            detail: error.to_string(),
        }
    })?;
    let bindings =
        actor_worktree_storage_root_in(&state.join("exomonad"), workspace).join("bindings");
    if BindingTable::has_live_owner(&bindings)? {
        return Err(exomonad_worktree::WorktreeError::StorageFailure {
            path: bindings,
            detail: "another Exomonad host owns this workspace; stop its session and wait for shutdown before launching again".into(),
        });
    }
    let legacy = actor_worktree_storage_root_in(
        &tidepool_toolchain::paths::cache_dir().join("exomonad"),
        workspace,
    );
    if legacy != bindings.parent().unwrap() && legacy_has_meaningful_state(&legacy)? {
        return Err(exomonad_worktree::WorktreeError::StorageFailure {
            path: legacy,
            detail: "legacy managed worktrees remain in the cache; inspect and retire them or recover the old host using its recorded --run-root before starting a new state-root run; no data was moved".into(),
        });
    }
    if let Some(run_root) = legacy_active_run_for_workspace(workspace)? {
        return Err(exomonad_worktree::WorktreeError::StorageFailure {
            path: run_root,
            detail: "a legacy cache-root run still records this workspace as active; recover or stop that run using its explicit --run-root before starting a state-root run".into(),
        });
    }
    Ok(())
}

fn legacy_active_run_for_workspace(
    workspace: &Path,
) -> Result<Option<PathBuf>, exomonad_worktree::WorktreeError> {
    let runs = tidepool_toolchain::paths::cache_dir().join("exomonad/runs");
    active_run_for_workspace_in(&runs, workspace)
}

fn active_run_for_workspace_in(
    runs: &Path,
    workspace: &Path,
) -> Result<Option<PathBuf>, exomonad_worktree::WorktreeError> {
    let entries = match std::fs::read_dir(runs) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => {
            return Err(exomonad_worktree::WorktreeError::StorageFailure {
                path: runs.to_path_buf(),
                detail: error.to_string(),
            })
        }
    };
    for entry in entries {
        let entry = entry.map_err(|error| exomonad_worktree::WorktreeError::StorageFailure {
            path: runs.to_path_buf(),
            detail: error.to_string(),
        })?;
        let run_root = entry.path();
        let bytes = match std::fs::read(run_root.join("status.json")) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => {
                return Err(exomonad_worktree::WorktreeError::StorageFailure {
                    path: run_root,
                    detail: error.to_string(),
                })
            }
        };
        let status = crate::exomonad::decode_run_status(&bytes).map_err(|error| {
            exomonad_worktree::WorktreeError::StorageFailure {
                path: run_root.join("status.json"),
                detail: error.to_string(),
            }
        })?;
        if status.workspace.as_path() == workspace
            && !matches!(
                status.phase,
                crate::exomonad::RunPhase::Exited | crate::exomonad::RunPhase::Failed { .. }
            )
        {
            return Ok(Some(run_root));
        }
    }
    Ok(None)
}

fn actor_worktree_storage_root(
    workspace: &Path,
    run_root: &Path,
) -> Result<PathBuf, exomonad_worktree::WorktreeError> {
    let family = run_root
        .parent()
        .filter(|runs| runs.file_name().is_some_and(|name| name == "runs"))
        .and_then(Path::parent)
        .filter(|exomonad| exomonad.file_name().is_some_and(|name| name == "exomonad"));
    let exomonad = match family {
        Some(family) => family.to_path_buf(),
        None => tidepool_toolchain::paths::state_dir()
            .map_err(|error| exomonad_worktree::WorktreeError::StorageFailure {
                path: run_root.to_path_buf(),
                detail: error.to_string(),
            })?
            .join("exomonad"),
    };
    Ok(actor_worktree_storage_root_in(&exomonad, workspace))
}

fn actor_worktree_storage_root_in(exomonad: &Path, workspace: &Path) -> PathBuf {
    let project = blake3::hash(workspace.as_os_str().as_encoded_bytes())
        .to_hex()
        .to_string();
    exomonad.join("actor-worktrees").join(project)
}

fn legacy_has_meaningful_state(root: &Path) -> Result<bool, exomonad_worktree::WorktreeError> {
    let mut pending = vec![root.to_path_buf()];
    while let Some(directory) = pending.pop() {
        let entries = match std::fs::read_dir(&directory) {
            Ok(entries) => entries,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => {
                return Err(exomonad_worktree::WorktreeError::StorageFailure {
                    path: directory,
                    detail: error.to_string(),
                })
            }
        };
        for entry in entries {
            let entry =
                entry.map_err(|error| exomonad_worktree::WorktreeError::StorageFailure {
                    path: directory.clone(),
                    detail: error.to_string(),
                })?;
            let kind = entry.file_type().map_err(|error| {
                exomonad_worktree::WorktreeError::StorageFailure {
                    path: entry.path(),
                    detail: error.to_string(),
                }
            })?;
            if kind.is_dir() {
                pending.push(entry.path());
            } else if entry.file_name() != ".owner.lock" {
                return Ok(true);
            }
        }
    }
    Ok(false)
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

/// Builds the exact producer identity `input_control` and the host-input
/// queue key rows against. The only owner of this format; a reader matching
/// queue rows (`run_map`'s review) must call this rather than re-deriving it.
pub(crate) fn input_producer_id(
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
    compile_driver(inputs.runtime_actors(), Some(inputs), run_root, None)?;
    Ok(())
}

/// One configured launchable role: the label diagnostics name it by, and the
/// constructor for its effect row.
type LaunchableRole = (&'static str, fn() -> exomonad_actor::EffectiveRole);

/// Every role `exomonad check --workspace` can launch a child into, paired
/// with the label its diagnostics name it by. Root is not among them: it is
/// never admitted as a child, so no role row can omit anything from it.
const LAUNCHABLE_ROLES: &[LaunchableRole] = &[
    ("research", exomonad_actor::EffectiveRole::research),
    ("coding", exomonad_actor::EffectiveRole::coding),
    ("scaffolding", scaffolding_default),
    ("integration", exomonad_actor::EffectiveRole::integration),
];

fn scaffolding_default() -> exomonad_actor::EffectiveRole {
    // A scaffolding role's effect row does not depend on the descendant
    // budget passed here; any budget answers the same row.
    exomonad_actor::EffectiveRole::scaffolding(exomonad_actor::DescendantBudget {
        maximum_depth: 0,
        maximum_active_children: Some(0),
    })
}

/// Typecheck the exact selected spec installation for each static launchable
/// child role row. GHC expands aliases and checks the selected entry's actual
/// type, so this also covers requirements that are not spelled in its
/// signature. This is an authored-row check; runtime handler availability is
/// resolved later when an actor's workbench is installed.
pub(crate) fn spec_effect_preflight(
    workspace: &crate::exomonad::workspace::FrozenWorkspace,
    run_root: &Path,
) -> Result<Vec<String>, Box<dyn std::error::Error>> {
    let roots = workspace.captured_source_roots().to_vec();
    let resolved = exomonad_actor::agent_spec::resolve(roots, workspace.spec.as_deref());
    let Some(entry) = resolved.entry.as_deref() else {
        return Ok(Vec::new());
    };
    let Some((module, _)) = entry.rsplit_once('.') else {
        return Err(runtime_error(format!(
            "selected agent spec entry {entry} is not qualified by a module"
        )));
    };
    let DriverSources {
        mut preamble,
        include,
    } = driver_sources(workspace.runtime_actors(), Some(workspace), run_root, None)?;
    preamble = insert_preamble_imports(&preamble, "qualified Tidepool.Effects.Core");
    preamble = insert_preamble_imports(&preamble, "qualified Tidepool.Agent.Contract");
    preamble = insert_preamble_imports(&preamble, &format!("qualified {module}"));
    let mut failures = Vec::new();
    for (label, role) in LAUNCHABLE_ROLES {
        let installation =
            exomonad_actor::agent_spec::installation_expression(entry, role().effect_keys());
        let dispatcher_effects = format!(
            "(Tidepool.Effects.Core.AgentTools ': Tidepool.Effects.Core.ContextReadWrite ': {})",
            installation.effect_row
        );
        let templates = resident_workbench_templates(&preamble, &dispatcher_effects, "");
        let template = templates
            .iter()
            .find(|template| {
                template.kind == tidepool_runtime::session::TemplateSelector::BindDiscard
            })
            .ok_or_else(|| runtime_error("resident workbench has no discarded-bind template"))?;
        let source = tidepool_runtime::session::render_template(
            &template.source,
            &installation.expression,
            &[],
        );
        match tidepool_toolchain::artifacts::check_source(
            &tidepool_toolchain::artifacts::SourceCheckRequest {
                source: &source,
                include: &include,
                fallback_module_name: "Expr",
            },
        ) {
            Ok(()) => {}
            Err(tidepool_toolchain::CompileError::Diagnostics(diagnostics)) => {
                let detail = tidepool_toolchain::diag::render_diagnostics(
                    &diagnostics,
                    &tidepool_toolchain::diag::RenderOpts {
                        anchor: "Expr.hs",
                        label: "<agent-spec-installation>",
                        user_lines: None,
                        line_offset: 0,
                        col_indent: 0,
                        drop_foreign_gen_warnings_except: None,
                        source: &source,
                    },
                );
                failures.push(format!(
                    "role {label} cannot install spec {entry}:\n{detail}"
                ));
            }
            Err(error) => {
                return Err(runtime_error(format!(
                    "could not establish whether role {label} can install spec {entry}: {error}"
                )));
            }
        }
    }
    Ok(failures)
}

/// Typecheck the driver against a CANDIDATE source revision. This is the whole
/// reload check: GHC's own module graph, rooted at the driver and every
/// configured workspace module, decides whether the candidate's
/// reverse-dependency closure typechecks. Nothing is published unless it does.
///
/// `replaces_run_layer` says which layer is being reloaded. The run's own
/// reload stands in for the run's layer, exactly as publishing would. A
/// helper candidate sits in front of the run layer, as it does for actor cells.
pub(crate) fn typecheck_candidate_revision(
    inputs: &crate::exomonad::workspace::FrozenWorkspace,
    run_root: &Path,
    haskell_root: &Path,
    candidate: &[PathBuf],
    replaces_run_layer: bool,
    extra_modules: &[String],
) -> Result<(), Box<dyn std::error::Error>> {
    let DriverSources { preamble, include } = driver_sources(
        haskell_root,
        Some(inputs),
        run_root,
        Some(CandidateSources {
            include: candidate,
            replaces_run_layer,
            extra_modules,
        }),
    )?;
    // rootDriver has the fixed type Eff RootEffects (), so its ordinary
    // driver turn uses the first effectful expression template. Keep that
    // exact module and graph while requesting only GHC typechecking.
    let templates = resident_workbench_templates(&preamble, DRIVER_EFFECTS, "");
    let template = templates
        .iter()
        .find(|template| template.kind == tidepool_runtime::session::TemplateSelector::Expr)
        .ok_or_else(|| runtime_error("resident driver has no expression template"))?;
    let source = tidepool_runtime::session::render_template(&template.source, DRIVER_ENTRY, &[]);
    let checked = tidepool_toolchain::artifacts::check_source(
        &tidepool_toolchain::artifacts::SourceCheckRequest {
            source: &source,
            include: &include,
            fallback_module_name: "Expr",
        },
    );
    checked.map_err(|error| {
        render_root_compile_failure(tidepool_runtime::session::TurnFailure {
            error,
            attempted_source: Some(source),
        })
    })?;
    Ok(())
}

/// The run's source service and branch-local helper layers. Only the run
/// owner can publish authored tooling; workers read the same run graph.
///
/// A run without a frozen workspace has no declared source roots, so there is
/// nothing a reload could honestly read; that case has no service at all and
/// every verb answers `SourceUnavailable`.
pub(crate) fn source_service(
    config: &ActorHostConfig,
    run_root: &Path,
    worktrees: WorktreeManager,
    owner: crate::exomonad::source::SourceRootOwner,
) -> Result<Option<Arc<crate::exomonad::source::ExomonadSourceReload>>, Box<dyn std::error::Error>>
{
    let Some(inputs) = config.workspace_inputs.as_ref() else {
        return Ok(None);
    };
    Ok(Some(Arc::new(
        crate::exomonad::source::ExomonadSourceReload::new_owned(
            inputs.clone(),
            config.workspace.clone(),
            run_root.to_path_buf(),
            config.haskell_root.clone(),
            owner,
        )?
        .with_helper_root(
            worktrees
                .managed_root()
                .join(".resources")
                .join(runtime_namespace(run_root))
                .join("helpers"),
        ),
    )))
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
    /// it? A run reload replaces it; a helper reload adds its private layer
    /// and leaves the run's exactly where it is.
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
    include.push(
        inputs
            .map(|inputs| inputs.runtime_actors().to_path_buf())
            .unwrap_or_else(|| haskell_root.to_path_buf()),
    );
    include.push(match inputs {
        Some(inputs) => inputs.runtime_stdlib().to_path_buf(),
        None => crate::haskell_sources::ensure_embedded_stdlib()?,
    });
    let mut preamble = insert_preamble_imports(
        &tidepool_mcp::build_notebook_preamble_with_companions_hiding(
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
        // found by convention, and the configured key names a value, not an import.
        // They are imported qualified because they only need to typecheck.
        let named = inputs
            .spec
            .as_deref()
            .into_iter()
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
        exact_context: None,
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

/// [`compile_root`]'s output: the workbench source template, the root's retained
/// executable entry, its compiled driver turn (reused as-is by a factory-built
/// child machine — see the composite facade test), and the composition
/// root's one [`exomonad_actor::ChildSessionFactory`].
type CompiledRoot = (
    ActorWorkbenchSource,
    ExomonadRoot,
    Arc<tidepool_runtime::session::CompiledTurn>,
    exomonad_actor::ChildSessionFactory<ExomonadHandlerStack, CapturedOutput>,
    Arc<tidepool_runtime::session::ImageRegistry>,
);

fn compile_root(
    config: &ActorHostConfig,
    run_root: &Path,
    worktrees: WorktreeManager,
    worktree_authority: ActorWorktreeAuthority,
    source: Option<&Arc<crate::exomonad::source::ExomonadSourceReload>>,
    host_incarnation: Arc<HostIncarnationLease>,
    run_journal_mode: JournalOpenMode,
    embedded_startup: Option<&mut embedded_recovery::EmbeddedStartupRecovery>,
) -> Result<CompiledRoot, Box<dyn std::error::Error>> {
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
    root_declaration_recovery::attach(&mut library, run_root, Arc::clone(&host_incarnation))?;
    if let Some(startup) = embedded_startup {
        startup.observe_manifest(
            &library,
            run_root,
            active_source_identity(run_root, config.workspace_inputs.is_some())?.as_deref(),
        )?;
    }
    let event_registry = WorktreeRegistry::open(
        actor_worktree_storage_root(&config.workspace, run_root)?.join("registry"),
    )?;
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
    let journal = if run_journal_mode == JournalOpenMode::Create {
        tidepool_handlers::JournalHandler::new(tidepool_handlers::SegmentPath::create_exclusive(
            journal_path,
        )?)?
    } else {
        tidepool_handlers::JournalHandler::resuming(
            tidepool_handlers::SegmentPath::open_existing(journal_path)?,
            host_incarnation.incarnation().0,
        )?
    };
    // The composition root's `ChildSessionFactory`: builds a fresh, idle
    // `ResidentSession` with the root's workspace handlers for a
    // `SelectedContext` launch that owns its own machine.
    //
    // `RepoEventHandler` cannot be shared or cloned across two live
    // machines (per-heap mailboxes/subscription registry, and its
    // `EventJournal` enforces one lifetime-owned writer), so each child gets
    // its own, built over `InertObservationSource` -- the fixed handler
    // hlist type still needs a slot here, but an eligible launch's resolved
    // effect row never dispatches to it (see
    // `bridge/handlers/src/handlers/event.rs`).
    let child_workspace_inputs = config.workspace_inputs.clone();
    let child_include = include.clone();
    let child_source_service = source.cloned();
    let child_journal = journal.clone();
    let child_worktree_handler = worktree_handler.clone();
    let child_run_root = run_root.to_path_buf();
    let child_run_lease = Arc::clone(&host_incarnation);
    // This run's one shared image cache: installed on the forest
    // (`ResidentForest::with_image_registry`) so it is applied to every
    // session's engine, root and child alike, including each session's
    // bootstrap install (`PersistentSession::set_image_registry` holds it
    // for the first turn).
    let image_registry = Arc::new(tidepool_runtime::session::ImageRegistry::new());
    let child_session_factory: exomonad_actor::ChildSessionFactory<
        ExomonadHandlerStack,
        CapturedOutput,
    > = Arc::new(move |child_session_id, source_layer| {
        let child_declarations = exomonad_effect_declarations();
        let mut module_env = tidepool_mcp::session_decl_module_env_hiding(
            &child_declarations,
            false,
            tidepool_mcp::CompanionImports::Omit,
            EXOMONAD_REPLACED_EFFECT_NAMES,
        );
        if let Some(inputs) = &child_workspace_inputs {
            module_env.imports.extend(inputs.imports());
        }
        let session_root = child_run_root
            .join("haskell-session-children")
            .join(child_session_id.0.to_string());
        let mut validation_include = source_layer.to_vec();
        validation_include.extend(child_include.iter().cloned());
        let mut library = SessionLib::open(child_session_id, &session_root, module_env)
            .map_err(|error| format!("child session declaration plane: {error}"))?
            .with_validation_include(validation_include);
        root_declaration_recovery::attach_child(
            &mut library,
            &child_run_root,
            Arc::clone(&child_run_lease),
        )
        .map_err(|error| format!("child session declaration recovery: {error}"))?;
        let child_event_handler =
            RepoEventHandler::with_source(Box::new(InertObservationSource), EventConfig::default());
        let child_worktree_handler = child_worktree_handler.clone();
        let machine = ResidentSession::unbootstrapped(
            hlist![
                source_handler(child_source_service.as_ref()),
                child_journal.clone(),
                child_event_handler,
                ActorBoundWorktreeHandler::new(child_worktree_handler.clone()),
                ActorWorktreeRegistryHandler::new(child_worktree_handler.clone()),
                ActorWorktreeAllocationHandler::new(child_worktree_handler.clone()),
                ActorWorktreeIntegrationHandler::new(child_worktree_handler.clone()),
                child_worktree_handler,
            ],
            CapturedOutput::new(),
            DEFAULT_NURSERY_SIZE,
            Some(library),
        );
        Ok(Box::new(machine))
    });
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
    let resource_scope = tidepool_codegen::suspension::RealmId::fresh();
    machine.set_actor_execution(
        tidepool_runtime::session::SessionRunContext {
            lexical_scope,
            resource_scope,
            ..tidepool_runtime::session::SessionRunContext::ROOT
        },
        EffectRunPolicy::HandleOrSuspend,
        LivePayloadPolicy::HASKELL_EFFECT_VALUE,
    )?;
    // The root's own bootstrap install goes through the run's registry, so
    // every child session bootstraps with this same driver image rather
    // than a second compile of it.
    machine.set_image_registry(Arc::clone(&image_registry));
    let entry = machine.prepare_startup_entry(compiled.code())?;
    machine.seal_recovery_initialization_scope(lexical_scope)?;
    let mut descriptor = ActorDescriptor::new(
        "exomonad-root",
        ActorPlacement {
            session,
            resource_scope,
            lexical_scope,
        },
    )
    .with_actor_path(root_declaration_recovery::root_path())
    .with_persistence_policy(exomonad_actor::ActorPersistencePolicy::Durable)
    // Profiles classify resident Haskell rows, not the native Codex sandbox.
    // The root allocates worktrees and may attenuate children to ReadOnly.
    .with_profile(ActorEffectProfile::ReadWrite)
    .with_effective_role(
        exomonad_actor::EffectiveRole::root().with_research_policy(config.research_policy),
    );
    if let Some(layers) = source {
        descriptor = descriptor.with_source_layer(
            exomonad_actor::ActorSourceLayers::layer_include(layers.as_ref(), &[])
                .map_err(std::io::Error::other)?,
        );
    }
    // A run that does not supply `Jev.Operators` gets a workbench without `J`,
    // rather than a compile failure over a module nothing on its search path
    // defines. The same answer tells the agent so in its instructions.
    let jev = config.jev_surface() == prompt_catalog::JevSurface::Installed;
    let context_support = if matches!(
        config.backend,
        crate::exomonad::HostBackendOptions::Embedded
    ) {
        vec![exomonad_tool::ToolEffectKey::ContextReadWrite]
    } else {
        Vec::new()
    };
    let mut workbench = ActorWorkbenchSource::new(preamble, include)
        .with_installed_effect_support(context_support)
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
        ResidentActorRoot::pending(descriptor, machine, entry),
        Arc::new(compiled),
        child_session_factory,
        image_registry,
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

#[cfg(feature = "codex-compat")]
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

#[cfg(feature = "codex-compat")]
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
#[cfg(feature = "codex-compat")]
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

#[cfg(feature = "codex-compat")]
async fn retain_input_custody_and_bind(
    owner: &hosted_retirement::HostedOwner,
    backend: Arc<dyn InteractiveAgentBackend>,
    thread: &QueueReadyThread,
    producer: &InputProducerId,
    provider_attachment: &provider_attachment::ProviderAttachment,
) -> Result<(), String> {
    hosted_retirement::begin_input_seal(
        owner,
        Arc::clone(&backend),
        thread.clone(),
        producer.clone(),
    )
    .await
    .map_err(|error| format!("could not retain native input custody: {error}"))?;
    provider_attachment
        .validate()
        .map_err(|error| format!("native provider attachment is unavailable: {error}"))?;
    if !thread.supports_active_input() {
        return Ok(());
    }
    match backend.bind_input(thread).await {
        Ok(exomonad_agent::InputAdmission::Admitted) => Ok(()),
        Ok(outcome) => Err(format!("native input bind returned {outcome:?}")),
        Err(error) => Err(format!("could not bind native input control: {error}")),
    }
}

fn embedded_resource_release(
    error: Option<&embedded_service::EmbeddedDriverError>,
) -> exomonad_actor::ResourceRelease {
    match error.filter(|error| error.cleanup_failed()) {
        Some(error) => exomonad_actor::ResourceRelease::Retained(format!(
            "embedded Engine cleanup unconfirmed: {error}"
        )),
        None => exomonad_actor::ResourceRelease::Released,
    }
}

fn observed_resource_release(
    actor: ActorRef,
    owners: &InteractiveOwners,
) -> Option<exomonad_actor::ResourceRelease> {
    let retirement = {
        let owners = owners.lock();
        let Some(owner) = owners.get(&actor) else {
            return Some(exomonad_actor::ResourceRelease::Released);
        };
        if let Some(embedded) = owner.embedded.as_ref() {
            if embedded.live {
                return None;
            }
            if let Some(error) = embedded.cleanup_failure.as_ref() {
                return Some(embedded_resource_release(Some(error)));
            }
            return Some(exomonad_actor::ResourceRelease::Released);
        }
        Arc::clone(&owner.retirement)
    };
    let receipt = retirement.lock();
    let release = receipt.as_ref().map(InteractiveCleanupReceipt::release);
    release
}

/// Fence admission and preserve release waits already queued when shutdown wins.
fn close_release_observations(
    lifecycle: &mut mpsc::Receiver<LocalResidentDeployment>,
    waiters: &mut HashMap<ActorRef, Vec<Arc<exomonad_actor::ReleaseAwait>>>,
    mut observed: impl FnMut(ActorRef) -> Option<exomonad_actor::ResourceRelease>,
) {
    lifecycle.close();
    while let Ok(event) = lifecycle.try_recv() {
        if let LocalResidentDeployment::ReleaseAwait(request) = event {
            match observed(request.actor) {
                Some(release) => {
                    request.answer(release);
                }
                None => waiters.entry(request.actor).or_default().push(request),
            }
        }
    }
}

fn retain_unsettled_release_waiters(
    waiters: &mut HashMap<ActorRef, Vec<Arc<exomonad_actor::ReleaseAwait>>>,
) {
    for (_, requests) in waiters.drain() {
        for request in requests {
            request.answer(exomonad_actor::ResourceRelease::Retained(
                "host shutdown ended without confirming this actor's resource release".into(),
            ));
        }
    }
}

fn answer_release_waiters(
    waiters: &mut HashMap<ActorRef, Vec<Arc<exomonad_actor::ReleaseAwait>>>,
    actor: ActorRef,
    release: exomonad_actor::ResourceRelease,
) {
    for waiter in waiters.remove(&actor).unwrap_or_default() {
        waiter.answer(release.clone());
    }
}

async fn drain_embedded_shutdown<A: fmt::Debug + Send + 'static, L: Send + 'static>(
    tasks: &mut JoinSet<(A, L, Result<(), embedded_service::EmbeddedDriverError>)>,
    grace: Duration,
    mut actor_for_task: impl FnMut(tokio::task::Id) -> Option<ActorRef>,
    mut released: impl FnMut(A, exomonad_actor::ResourceRelease),
) -> Option<String> {
    let mut failure = None;
    match tokio::time::timeout(grace, async {
        while let Some(result) = tasks.join_next_with_id().await {
            match result {
                Ok((_task_id, (actor, _local_actor, outcome))) => {
                    let release = embedded_resource_release(outcome.as_ref().err());
                    if let Err(error) = outcome {
                        tracing::warn!(?actor, %error, "embedded Engine stopped with an error during host shutdown");
                        failure.get_or_insert_with(|| format!("embedded Engine {actor:?}: {error}"));
                    }
                    released(actor, release);
                }
                Err(error) => {
                    let actor = actor_for_task(error.id());
                    failure.get_or_insert_with(|| match actor {
                        Some(actor) => format!("embedded Engine task for {actor:?}: {error}"),
                        None => format!("unattributed embedded Engine task: {error}"),
                    });
                }
            }
        }
    }).await {
        Ok(()) => failure,
        Err(_) => {
            tasks.abort_all();
            let timeout = "embedded Engine cleanup timed out; abort requested, cleanup unconfirmed";
            Some(match failure {
                Some(failure) => format!("{failure}; {timeout}"),
                None => timeout.into(),
            })
        }
    }
}

#[cfg(feature = "codex-compat")]
struct LaunchShutdown<T> {
    completed: Vec<T>,
    failures: Vec<LaunchShutdownFailure>,
}

#[derive(Debug, thiserror::Error)]
#[cfg(feature = "codex-compat")]
enum LaunchShutdownFailure {
    #[error("interactive launch cleanup failed: {0}")]
    Launch(InteractiveApplicationError),
    #[error("interactive launch join failed; cleanup unconfirmed: {0}")]
    Join(tokio::task::JoinError),
    #[error("interactive launch cleanup timed out with {pending} unsettled tasks; abort requested, cleanup unconfirmed")]
    TimedOut { pending: usize },
}

#[cfg(feature = "codex-compat")]
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
#[cfg(feature = "codex-compat")]
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

#[cfg(feature = "codex-compat")]
struct InteractiveInheritance {
    thread: Option<BackendThreadId>,
    build_snapshot: Option<OverlaySnapshot>,
}

#[cfg(feature = "codex-compat")]
struct InteractiveLaunchRetention {
    hosted: hosted_retirement::HostedSlot,
    pane: Arc<Mutex<Option<TmuxPaneId>>>,
    process: Arc<Mutex<scoped_custody::ScopedProcessSlot>>,
}

#[cfg(feature = "codex-compat")]
async fn launch_interactive_application(
    installation: LocalResidentInstallation,
    context: InteractiveLaunchContext,
    cancelled: oneshot::Receiver<NativeRetirement>,
    inherited: InteractiveInheritance,
    retention: InteractiveLaunchRetention,
    provider_attachment: provider_attachment::ProviderAttachment,
) -> Result<Option<LaunchedInteractiveApplication>, InteractiveApplicationError> {
    provider_attachment.validate().map_err(|error| {
        application_error(
            installation.actor.identity(),
            InteractiveOperation::PrepareRuntime,
            error,
        )
    })?;
    let worktree = prepare_actor_worktree(&installation, &context)?;
    launch_prepared_interactive_application(
        installation,
        context,
        worktree,
        cancelled,
        inherited,
        retention,
        provider_attachment,
    )
    .await
}

#[cfg(feature = "codex-compat")]
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

#[cfg(feature = "codex-compat")]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg(feature = "codex-compat")]
enum ActorWorkspaceRequest<'a> {
    /// The actor may inspect the source checkout. Only the root receives write
    /// authority for it; an ordinary actor without a worktree remains a useful
    /// orchestration or review actor rather than acquiring ambient code-write
    /// authority by accident.
    SourceCheckout,
    Worktree(&'a str),
}

#[cfg(feature = "codex-compat")]
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

#[cfg(feature = "codex-compat")]
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

#[cfg(feature = "codex-compat")]
fn socket_cleanup_outcome(socket: SocketDirectory) -> CleanupComponentOutcome {
    match socket.release() {
        Ok(()) => CleanupComponentOutcome::Completed,
        Err(error) => CleanupComponentOutcome::Failed {
            detail: error.to_string(),
        },
    }
}

#[cfg(feature = "codex-compat")]
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

#[cfg(feature = "codex-compat")]
fn socket_launch_cancelled(
    actor: ActorRef,
    cause: impl fmt::Display,
    socket: SocketDirectory,
) -> InteractiveApplicationError {
    let mut error =
        socket_launch_failure(actor, InteractiveOperation::LaunchProcess, cause, socket);
    error.disposition = LaunchDisposition::Cancelled;
    error
}

#[cfg(feature = "codex-compat")]
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

#[cfg(feature = "codex-compat")]
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

#[cfg(feature = "codex-compat")]
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
#[cfg(feature = "codex-compat")]
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

#[cfg(feature = "codex-compat")]
struct ActorLaunchEnvironment {
    set: BTreeMap<String, String>,
    unset: BTreeSet<String>,
}

/// Compose the host environment and actor-local credentials before handing
/// them to tmux. A worker's explicit unsets win over values captured from the
/// root process; handing the same name to both tmux channels is ambiguous and
/// rejected by the deployment adapter.
#[cfg(feature = "codex-compat")]
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

#[cfg(feature = "codex-compat")]
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

fn supply_resident_command_backend(
    request: Arc<exomonad_actor::command_jobs::CommandBackendRequest>,
    config: &ActorHostConfig,
    authority: &ActorWorktreeAuthority,
    worktrees: &WorktreeManager,
    actor: ActorRef,
    #[cfg(feature = "codex-compat")] resource_client: Option<
        Arc<exomonad_node::command_resources::CommandResourceClient>,
    >,
    host_resources: Option<Arc<exomonad_node::command_resources::CommandResourceClient>>,
    #[cfg(feature = "codex-compat")] thread: Option<QueueReadyThread>,
    #[cfg(feature = "codex-compat")] native: Option<Arc<dyn InteractiveAgentBackend>>,
) {
    let backend = (|| {
        #[cfg(feature = "codex-compat")]
        if let Some(thread) = thread {
            let resources = resource_client.as_ref().cloned().ok_or_else(|| {
                tidepool_bridge_effects::CommandError::CommandUnavailable(
                    "this run has no command resource authority".into(),
                )
            })?;
            let Some(native) = native else {
                return Err(tidepool_bridge_effects::CommandError::CommandUnavailable(
                    "native command backend is unavailable for the embedded provider".into(),
                ));
            };
            return Ok(Arc::new(commands::NativeCommandBackend::new(
                native, thread, resources, actor,
            ))
                as Arc<dyn exomonad_actor::command_jobs::CommandBackend>);
        }

        let resources = host_resources.ok_or_else(|| {
            tidepool_bridge_effects::CommandError::CommandUnavailable(
                "this run has no command resource authority".into(),
            )
        })?;
        let bubblewrap = resolve_scope_bubblewrap(&config.pane_environment).map_err(|error| {
            tidepool_bridge_effects::CommandError::CommandUnavailable(format!(
                "cannot resolve bubblewrap for resident commands: {error}"
            ))
        })?;
        Ok(Arc::new(commands::HostCommandBackend::new(
            resources,
            actor,
            resident_command_roots(authority, worktrees, &config.workspace, actor).map_err(
                |error| {
                    tidepool_bridge_effects::CommandError::CommandUnavailable(format!(
                        "cannot resolve resident command authority: {error}"
                    ))
                },
            )?,
            bubblewrap,
        ))
            as Arc<dyn exomonad_actor::command_jobs::CommandBackend>)
    })();
    request.supply(backend);
}

fn orient_launch_instructions(
    message: &str,
    observation: &exomonad_actor::ActorRuntimeObservation,
    tools: &[exomonad_tool::HostedTool],
) -> String {
    let mut instructions = match observation.launch_orientation() {
        Some(orientation) => format!("{message}\n\n{orientation}"),
        None => message.to_owned(),
    };
    let names = tools.iter().map(|tool| tool.name()).collect::<Vec<_>>();
    instructions.push_str(&format!(
        "\n\nDeclared tools: {}.",
        if names.is_empty() {
            "(none)".to_owned()
        } else {
            names.join(", ")
        },
    ));
    for tool in tools {
        if tool.implementation() != exomonad_tool::ToolImplementation::HaskellCell {
            continue;
        }
        let effects = tool
            .effect_keys()
            .iter()
            .map(|effect| effect.haskell_name())
            .collect::<Vec<_>>();
        instructions.push_str(&format!(
            "\nNotebook {} effects: {}.",
            tool.name(),
            effects.join(", "),
        ));
        if tool
            .effect_keys()
            .contains(&exomonad_tool::ToolEffectKey::Actor(
                exomonad_tool::ActorEffectKey::Lookup,
            ))
        {
            instructions.push_str(&format!(
                "\nDiscovery in {}: LookupApi.lookupRaw (LookupApi.lookupRequest [\"doc topics\"]).",
                tool.name(),
            ));
        }
    }
    instructions
}

fn open_embedded_actor_binding(
    run_root: &Path,
    actor: ActorRef,
    actor_path: harness::model::AgentPath,
    conversation: Option<Arc<harness::embedding::Conversation>>,
) -> Result<embedded_harness::EmbeddedActorBinding, String> {
    let actor_root = run_root.join(format!("{}-{}", actor.id.0, actor.incarnation.0));
    std::fs::create_dir_all(&actor_root).map_err(|error| error.to_string())?;
    let inbox = ActorInbox::open(
        actor_root.join("embedded-notifications.jsonl"),
        actor_root.join("embedded-notifications.cursor"),
    )
    .map_err(|error| error.to_string())?;
    let inbox_key = format!(
        "{}:{}:{}:embedded-notifications",
        runtime_namespace(run_root),
        actor.id.0,
        actor.incarnation.0,
    );
    let identity = harness::embedding::HostIdentity {
        run: runtime_namespace(run_root),
        actor: actor_path,
        incarnation: actor.incarnation.0.to_string(),
    };
    Ok(embedded_harness::EmbeddedActorBinding::new(
        identity,
        Arc::new(inbox),
        inbox_key,
        conversation,
    ))
}

/// The checkout's Git head and dirty files, via [`GitCli`] — the sole
/// sanctioned way to invoke git in this repository. There is no recorded
/// build revision for the running binary to compare `head` against; see
/// `exomonad_actor::CheckoutGitDrift`.
#[cfg(feature = "codex-compat")]
fn checkout_git_drift(
    worktrees: &WorktreeManager,
    worktree_id: &str,
) -> std::result::Result<exomonad_actor::CheckoutGitDrift, String> {
    let handle = worktrees
        .lookup(&WorktreeId::from_raw(worktree_id))
        .map_err(|error| error.to_string())?
        .ok_or_else(|| format!("worktree {worktree_id:?} is not allocated"))?;
    git_drift_at(worktrees.git(), handle.cwd())
}

/// A checkout's Git head and dirty files at `cwd`.
#[cfg(feature = "codex-compat")]
fn git_drift_at(
    git: &GitCli,
    cwd: &std::path::Path,
) -> std::result::Result<exomonad_actor::CheckoutGitDrift, String> {
    let head = git
        .try_run(cwd, &["rev-parse", "HEAD"])
        .map_err(|error| error.to_string())?
        .trimmed()
        .to_owned();
    let dirty = exomonad_worktree::git::inspect::dirty_summary(git, cwd)
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

#[cfg(feature = "codex-compat")]
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

#[cfg(feature = "codex-compat")]
async fn publish_owner_notification(
    notification: OwnerNotification,
) -> (ActorRef, Result<(), String>) {
    publish_inbox_event_for(notification.owner, notification.inbox, notification.event).await
}

#[cfg(feature = "codex-compat")]
async fn publish_inbox_event_for(
    actor: ActorRef,
    inbox: Arc<ActorInbox>,
    event: DurableActorEvent,
) -> (ActorRef, Result<(), String>) {
    (actor, publish_inbox_event(inbox, event).await)
}

#[cfg(feature = "codex-compat")]
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

#[cfg(feature = "codex-compat")]
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
#[cfg(feature = "codex-compat")]
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

#[cfg(feature = "codex-compat")]
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

#[cfg(feature = "codex-compat")]
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
#[cfg(feature = "codex-compat")]
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

#[cfg(feature = "codex-compat")]
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

#[cfg(feature = "codex-compat")]
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
#[cfg(feature = "codex-compat")]
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

#[cfg(feature = "codex-compat")]
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

/// Resolve command mounts from the exact actor's worktree grant and custody.
/// Repository authority writes the source, root allocations, and shared Git
/// metadata. A bound actor writes only its own checkout and that metadata;
/// inspection-only actors get no writable repository roots. Child checkouts
/// remain protected even when the root can write its own allocations.
fn resident_command_roots(
    authority: &ActorWorktreeAuthority,
    worktrees: &WorktreeManager,
    source: &Path,
    actor: ActorRef,
) -> Result<ResidentCommandRoots, exomonad_worktree::WorktreeError> {
    let custody = authority
        .bound_worktree(actor.into())
        .and_then(|id| worktrees.registry().get(&id).ok().flatten())
        .map(|receipt| receipt.cwd);
    let git_common_dir = exomonad_worktree::git::inspect::git_common_dir(worktrees.git(), source)?;
    let root_allocations = worktrees.root_allocations();
    let grant = authority.grant(actor.into());
    let root = grant == ActorWorktreeGrant::Repository;
    let writable = writable_repository_roots(
        root,
        if custody.is_some() && grant != ActorWorktreeGrant::RepositoryReadOnly {
            exomonad_actor::WorkspaceAccess::WritableBound
        } else {
            exomonad_actor::WorkspaceAccess::InspectOnly
        },
        source,
        custody.as_deref(),
        &git_common_dir,
        Some(root_allocations.managed_root()),
    );
    Ok(ResidentCommandRoots {
        directory: custody.clone().unwrap_or_else(|| source.to_owned()),
        protected: vec![
            source.to_owned(),
            worktrees.managed_root().to_owned(),
            root_allocations.managed_root().to_owned(),
            git_common_dir,
        ],
        writable,
        custody: custody.is_some(),
    })
}

/// The roots a resident actor's commands are confined to, and the directory
/// they run in when they do not name one.
#[derive(Clone)]
struct ResidentCommandRoots {
    directory: PathBuf,
    protected: Vec<PathBuf>,
    writable: Vec<PathBuf>,
    /// Whether this actor holds an exclusive worktree binding.
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
