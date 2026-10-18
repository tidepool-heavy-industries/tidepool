//! Native Exomonad host composition.
//!
//! The host owns resident Haskell scheduling and exact actor lifecycle.
//! Harness owns provider conversations; shared Rust owners supervise commands.

#[cfg(test)]
mod agent_spec_tests;
#[cfg(test)]
mod embedded_agent_spec_tests;
#[cfg(test)]
mod hosted_test_context;

#[cfg(test)]
pub(crate) use crate::transport_test_support::ResidentToolEndpointTestExt;

mod cell_context;
mod cell_model;
pub(crate) mod run_auth;

#[cfg(test)]
mod command_test_support;
mod commands;
mod context_wire;
mod effect_vocabulary;
pub(crate) use effect_vocabulary::exomonad_effect_declarations;
mod application_supervisor;
#[cfg(test)]
mod context_transaction_acceptance_tests;
#[cfg(test)]
mod startup_context_acceptance_tests;
#[cfg(test)]
mod startup_persona_acceptance_tests;

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
mod embedded_checkpoint_release_tests;
#[cfg(test)]
mod embedded_command_tests;
mod embedded_context;
mod embedded_harness;
#[cfg(test)]
mod embedded_idle_retirement_tests;
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
#[cfg(test)]
mod embedded_reflect_tests;
mod embedded_service;
mod form_output;
mod operation_observation;
#[cfg(test)]
mod scaffold_admission_tests;

#[cfg(test)]
use delivery::embedded_notification_operation_id;

#[cfg(test)]
use delivery::{admit_notification, observe_notification_receipt};

use delivery::{
    observe_embedded_notification, schedule_embedded_notification_drain,
    schedule_embedded_notification_send,
};

#[cfg(test)]
mod embedded_shutdown_tests;
mod host_incarnation;

#[cfg(test)]
mod lookup_availability_tests;
#[cfg(test)]
mod m1_host_tests;

#[cfg(test)]
mod native_prefix_publication_tests;

#[cfg(test)]
mod deepseek_offline_acceptance;
mod overlay_resource;
#[cfg(test)]
mod packaged_catalog_tests;
#[cfg(test)]
mod prepared_display_tests;
#[cfg(test)]
mod prepared_runtime_acceptance;
#[cfg(test)]
mod scripted_recursive_acceptance;
#[cfg(test)]
mod scripted_three_actor_performance;
pub(crate) use overlay_resource::valid_artifact_path;

mod workspace;
#[cfg(test)]
mod workspace_admission_tests;
pub mod workspace_cleanup;

pub(crate) use workspace::initialize_helper_draft;
use workspace::{ActiveWorkspace, PreparedWorkspace};
#[cfg(test)]
mod fresh_child_tests;
mod model_free;
mod prompt_catalog;
mod prompt_composition;
mod provider_attachment;
pub(crate) mod recipe_checks;

mod root_declaration_recovery;

#[cfg(test)]
mod background_command_example_tests;
#[cfg(test)]
mod call_timing_tests;
#[cfg(test)]
mod cell_compile_cost_tests;
#[cfg(test)]
mod coherent_reload_acceptance_tests;
#[cfg(test)]
pub(crate) mod command_jobs_tests;
#[cfg(test)]
mod custody_tests;
#[cfg(test)]
mod hosted_tools_tests;
#[cfg(test)]
mod invocation_lifetime_tests;
#[cfg(test)]
mod jev_recording_reload_tests;
#[cfg(test)]
mod jev_tests;
#[cfg(test)]
mod observation_budget_tests;
#[cfg(test)]
mod persona_file_tools_acceptance_tests;
#[cfg(test)]
mod researcher_acceptance_tests;
#[cfg(test)]
mod source_reload_tests;
#[cfg(test)]
mod terminal_transfer_tests;
#[cfg(test)]
pub(crate) mod test_campaign;
#[cfg(test)]
mod tests;

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::fmt;
use std::panic::AssertUnwindSafe;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use exomonad_actor::{
    ActorDescriptor, ActorExitKind, ActorPlacement, ActorRef, ActorTerminal, ActorWorkbenchSource,
    ExternalApplicationFailure, ExternalApplicationFailureClass, ExternalFailureDisposition,
    LocalActorRef, LocalResidentDeployment, LocalResidentInstallation, ResidentActorRoot,
    ResidentForest, WorkspaceAdmission, WorkspaceAdmissionError,
};

use exomonad_actor::Reasoning;
use frunk::{hlist, HCons, HNil};
use futures_util::FutureExt;
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};

use exomonad_node::DurableInbox;

use exomonad_node::{ProcessMountBoundary, BUBBLEWRAP_PROGRAM};

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
    insert_preamble_imports, resident_workbench_templates, run_turn, ResidentSession, SessionLib,
    TurnRequest as HaskellTurnRequest, TurnResult,
};
use tidepool_runtime::DEFAULT_NURSERY_SIZE;

use tokio::sync::{mpsc, oneshot, watch};
use tokio::task::JoinSet;
use tracing::Instrument;

use self::application_supervisor::run_interactive_applications;
use self::embedded_projection::LifecyclePublisher;
pub(crate) use self::host_incarnation::HostIncarnationLease;

use self::overlay_resource::{
    ArtifactInspection, OverlayResourceLease, OverlaySnapshot, SharedOverlayResource,
};
use self::prompt_catalog::{FrozenBasePrompt, PromptId};

/// Every interactive actor sees its own repository at this path. Bubblewrap
/// mount namespaces make the shared name safe across concurrent actors.
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

const APPLICATION_SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(15);

const PROCESS_OPERATION_TIMEOUT: Duration = Duration::from_secs(30);

type ExomonadApplicationStack = HCons<
    tidepool_handlers::WebHandler,
    HCons<tidepool_handlers::FsReadHandler, HCons<tidepool_handlers::FsWriteHandler, HNil>>,
>;

type ExomonadHandlerStack = HCons<
    tidepool_handlers::JournalHandler,
    HCons<
        RepoEventHandler,
        HCons<
            ActorBoundWorktreeHandler,
            HCons<
                ActorWorktreeRegistryHandler,
                HCons<
                    ActorWorktreeAllocationHandler,
                    HCons<
                        ActorWorktreeIntegrationHandler,
                        HCons<ActorWorktreeHandler, ExomonadApplicationStack>,
                    >,
                >,
            >,
        >,
    >,
>;
type ExomonadRoot = ResidentActorRoot<ExomonadHandlerStack, CapturedOutput>;

fn host_handlers(
    workspace_root: &Path,
    journal: tidepool_handlers::JournalHandler,
    events: RepoEventHandler,
    worktrees: ActorWorktreeHandler,
    web: tidepool_handlers::WebHandler,
) -> ExomonadHandlerStack {
    hlist![
        journal,
        events,
        ActorBoundWorktreeHandler::new(worktrees.clone()),
        ActorWorktreeRegistryHandler::new(worktrees.clone()),
        ActorWorktreeAllocationHandler::new(worktrees.clone()),
        ActorWorktreeIntegrationHandler::new(worktrees.clone()),
        worktrees,
        web,
        tidepool_handlers::FsReadHandler::new(workspace_root.to_owned()),
        tidepool_handlers::FsWriteHandler::new(workspace_root.to_owned()),
    ]
}

fn host_context_support() -> Vec<exomonad_tool::ToolEffectKey> {
    vec![exomonad_tool::ToolEffectKey::ContextReadWrite]
}

fn with_host_interpreters(
    mut forest: ResidentForest<ExomonadHandlerStack, CapturedOutput>,
    source_layers: Option<exomonad_actor::ActorSourceLayerResolver>,
    config: &ActorHostConfig,
    runtime: &embedded_harness::EmbeddedHarnessRuntime,
    recovery: Arc<exomonad_actor::ActorRecoveryJournal>,
    models: Arc<dyn exomonad_actor::CellModelFactory>,
) -> Result<ResidentForest<ExomonadHandlerStack, CapturedOutput>, Box<dyn std::error::Error>> {
    if let Some(layers) = source_layers {
        forest.set_source_layers(layers);
    }
    runtime
        .configure_context_models(config)
        .map_err(runtime_error)?;
    let mut forest = forest
        .with_conversation_reader(embedded_reflect::run_conversation_reader(
            runtime.store(),
            recovery.clone(),
        ))
        .with_result_inspection_reader(embedded_reflect::result_reader(
            runtime.store(),
            recovery.clone(),
        ))
        .with_operation_observation_reader(operation_observation::model_reader(
            runtime.store(),
            runtime.scheduler(),
            recovery,
        ));
    forest.set_jev_backend(jev_backend(config));
    Ok(forest
        .with_cell_model_factory(models)
        .with_form_host(form_output::host(
            runtime.store(),
            runtime.run_identity().to_owned(),
            runtime.output_control_handle(),
        )))
}

#[derive(Clone)]
struct ActorWorkspaceAdmission {
    worktrees: Arc<Mutex<ActorWorktreeHandler>>,
    authority: ActorWorktreeAuthority,
    manager: WorktreeManager,
    bindings: Arc<Mutex<BindingTable>>,
    runtime: String,
}

#[derive(Default)]
struct WorkspaceCustodyState {
    process_possible: bool,
    terminal: Option<ActorTerminal>,
}

struct ActorWorkspaceCustody {
    runtime: String,
    bindings: Arc<Mutex<BindingTable>>,
    binding: Mutex<Option<ActiveBinding>>,
    actor: ActorRef,
    state: Mutex<WorkspaceCustodyState>,
    workspace: Option<Arc<PreparedWorkspace>>,
    inheritance_notice: Option<String>,
}

impl exomonad_actor::WorkspaceCustody for ActorWorkspaceCustody {
    fn transfer_to(
        &self,
        successor: ActorRef,
    ) -> Result<Arc<dyn exomonad_actor::WorkspaceCustody>, WorkspaceAdmissionError> {
        let state = self.state.lock();
        if state.process_possible || state.terminal.is_some() {
            return Err(WorkspaceAdmissionError {
                detail: "workspace transfer requires a live actor with no possible native process"
                    .into(),
            });
        }
        let mut binding = self.binding.lock();
        let lease = binding.as_mut().ok_or_else(|| WorkspaceAdmissionError {
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
            .map_err(|error| WorkspaceAdmissionError {
                detail: error.to_string(),
            })?;
        Ok(Arc::new(Self {
            runtime: self.runtime.clone(),
            bindings: self.bindings.clone(),
            binding: Mutex::new(binding.take()),
            actor: successor,
            state: Mutex::new(WorkspaceCustodyState::default()),
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
        self.state.lock().process_possible = true;
    }
}

impl Drop for ActorWorkspaceCustody {
    fn drop(&mut self) {
        let state = self.state.lock();
        if state.process_possible {
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

impl ActorWorkspaceAdmission {
    fn bind_workspace(
        &self,
        actor: ActorRef,
        worktree: &str,
        access: exomonad_worktree::WorkspaceAccess,
        workspace: Option<Arc<PreparedWorkspace>>,
        inheritance_notice: Option<String>,
    ) -> Result<Arc<dyn exomonad_actor::WorkspaceCustody>, WorkspaceAdmissionError> {
        if !WorktreeId::is_path_safe(worktree) {
            return Err(WorkspaceAdmissionError {
                detail: "invalid custody worktree id".into(),
            });
        }
        if self
            .manager
            .lookup(&WorktreeId::from_raw(worktree))
            .map_err(|error| WorkspaceAdmissionError {
                detail: error.to_string(),
            })?
            .is_none()
        {
            return Err(WorkspaceAdmissionError {
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
                access,
                current_time_ms(),
            )
            .map_err(|error| WorkspaceAdmissionError {
                detail: error.to_string(),
            })?;
        Ok(Arc::new(ActorWorkspaceCustody {
            runtime: self.runtime.clone(),
            bindings: self.bindings.clone(),
            binding: Mutex::new(Some(binding)),
            actor,
            state: Mutex::new(WorkspaceCustodyState::default()),
            workspace,
            inheritance_notice,
        }))
    }
}

impl WorkspaceAdmission for ActorWorkspaceAdmission {
    fn prepare(
        &self,
        owner: ActorRef,
        selection: exomonad_actor::WorkspaceSelection,
        requested: Option<exomonad_actor::WorkspaceAccess>,
    ) -> exomonad_actor::WorkspaceAdmissionFuture<'_> {
        let custody = self.clone();
        Box::pin(async move {
            let preparation = custody.clone();
            let (handle, access) = tidepool_runtime::spawn_blocking_in_span(move || {
                let authority = &preparation.authority;
                let (tree, available) = match selection {
                    exomonad_actor::WorkspaceSelection::SameDirectory => {
                        if let Some(tree) = authority.bound_worktree(owner.into()) {
                            let access = authority
                                .workspace_access(owner.into(), &tree)
                                .ok_or_else(|| WorkspaceAdmissionError {
                                    detail: "caller workspace membership is unavailable".into(),
                                })?;
                            (tree, access)
                        } else {
                            let available = match authority.grant(owner.into()) {
                                ActorWorktreeGrant::Repository => {
                                    exomonad_worktree::WorkspaceAccess::ReadWrite
                                }
                                ActorWorktreeGrant::RepositoryReadOnly => {
                                    exomonad_worktree::WorkspaceAccess::ReadOnly
                                }
                                _ => {
                                    return Err(WorkspaceAdmissionError {
                                        detail: "SameDir requires an attached caller workspace"
                                            .into(),
                                    })
                                }
                            };
                            (
                                preparation
                                    .manager
                                    .register_source_checkout()
                                    .map_err(workspace_admission_error)?
                                    .id()
                                    .clone(),
                                available,
                            )
                        }
                    }
                    exomonad_actor::WorkspaceSelection::ExistingDirectory(handle) => authority
                        .workspace_capability(&handle.raw)
                        .map_err(workspace_admission_error)?,
                    exomonad_actor::WorkspaceSelection::ForkDirectory(seed) => {
                        let seed = match seed {
                            exomonad_actor::WorkspaceSeedWire::CurrentCheckout => {
                                let tree =
                                    if let Some(tree) = authority.bound_worktree(owner.into()) {
                                        tree
                                    } else if matches!(
                                        authority.grant(owner.into()),
                                        ActorWorktreeGrant::Repository
                                            | ActorWorktreeGrant::RepositoryReadOnly
                                    ) {
                                        preparation
                                            .manager
                                            .register_source_checkout()
                                            .map_err(workspace_admission_error)?
                                            .id()
                                            .clone()
                                    } else {
                                        return Err(WorkspaceAdmissionError {
                                            detail: "currentCheckout requires an attached backing"
                                                .into(),
                                        });
                                    };
                                tidepool_bridge_effects::WtWorktreeSource::SourceWorktree(
                                    tidepool_bridge_effects::WtWorktreeId {
                                        raw: tree.as_str().into(),
                                    },
                                )
                            }
                            exomonad_actor::WorkspaceSeedWire::CommittedSource(source) => source,
                        };
                        let authorized = preparation
                            .worktrees
                            .lock()
                            .authorize_committed_fork(owner.into(), seed)
                            .map_err(|error| WorkspaceAdmissionError {
                                detail: tidepool_handlers::render_worktree_error(&error),
                            })?;
                        let handle = authorized.materialize_committed().map_err(|error| {
                            WorkspaceAdmissionError {
                                detail: tidepool_handlers::render_worktree_error(&error),
                            }
                        })?;
                        return Ok((
                            handle,
                            requested.unwrap_or(exomonad_worktree::WorkspaceAccess::ReadWrite),
                        ));
                    }
                };
                let access = requested.unwrap_or(available);
                if !available.permits(access) {
                    return Err(WorkspaceAdmissionError {
                        detail: "workspace attachment cannot widen read-only access".into(),
                    });
                }
                let handle = preparation
                    .manager
                    .lookup(&tree)
                    .map_err(workspace_admission_error)?
                    .map(|handle| tidepool_handlers::handlers::worktree::handle_to_wire(&handle))
                    .ok_or_else(|| WorkspaceAdmissionError {
                        detail: "workspace backing is not registered".into(),
                    })?;
                Ok((handle, access))
            })
            .await
            .map_err(|error| WorkspaceAdmissionError {
                detail: format!("workspace preparation task failed: {error}"),
            })??;
            let tree = handle.handle_receipt.tree_id.raw.clone();
            Ok(exomonad_actor::PreparedWorkspaceAttachment::new(
                handle,
                move |actor| custody.bind_workspace(actor, &tree, access, None, None),
            ))
        })
    }
}

fn workspace_admission_error(error: exomonad_worktree::WorktreeError) -> WorkspaceAdmissionError {
    WorkspaceAdmissionError {
        detail: error.to_string(),
    }
}

fn fork_workspace_admission(
    worktrees: WorktreeManager,
    authority: ActorWorktreeAuthority,
    bindings: Arc<Mutex<BindingTable>>,
    runtime: String,
) -> Arc<ActorWorkspaceAdmission> {
    Arc::new(ActorWorkspaceAdmission {
        bindings,
        runtime,

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
    pub run_id: String,
    pub systemd_slice: Option<exomonad_node::systemd_slice::SystemdSlice>,
    pub source_exclude: Vec<String>,
    pub source_import: crate::exomonad::SourceImportPolicy,
    pub command_resources: Option<Arc<exomonad_node::command_resources::CommandResourceClient>>,
    /// This Exomonad installation provides the internal namespace-entry executable.
    pub exomonad_executable: PathBuf,
    pub workspace_inputs: Option<crate::exomonad::workspace::FrozenWorkspace>,
    pub workspace: PathBuf,
    pub haskell_root: PathBuf,
    pub run_directory: tidepool_atomic_write::DirectoryAnchor,
    pub root_binding_path: PathBuf,

    pub embedded: Option<crate::exomonad::EmbeddedLaunchConfig>,
    /// An embedding may supply an already admitted transport at startup.
    /// CLI launches construct the official transport from frozen settings.
    pub(crate) inference_auth: Option<run_auth::RunAuth>,
    pub tmux_session: String,
    pub model: exomonad_actor::Model,
    pub reasoning: Reasoning,
    pub pane_environment: std::collections::BTreeMap<String, String>,
    /// Answers actors' `Jev` requests; `None` uses the TypeSafe client with
    /// the key from `TYPESAFE_API_KEY` or the secrets directory.
    pub jev: Option<exomonad_actor::JevBackendHandle>,
}

fn admitted_capabilities(
    workspace: Option<&crate::exomonad::workspace::FrozenWorkspace>,
) -> Result<exomonad_actor::ActorCapabilities, String> {
    workspace
        .and_then(|workspace| workspace.model_catalog.as_ref())
        .map(|catalog| {
            catalog.root_capabilities(
                exomonad_actor::ActorCapabilities::default()
                    .effect_keys()
                    .to_vec(),
            )
        })
        .transpose()
        .map(|capabilities| capabilities.unwrap_or_default())
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

fn needs_host_recovery(record: &exomonad_actor::DurableActorRecord) -> bool {
    match record.terminal.as_ref().map(|terminal| terminal.kind) {
        None | Some(ActorExitKind::Interrupted) => true,
        Some(ActorExitKind::Completed | ActorExitKind::Cancelled | ActorExitKind::Failed) => false,
    }
}

fn durable_root_identity(
    records: &[exomonad_actor::DurableActorRecord],
    accepted_source: Option<&str>,
) -> Result<Option<(ActorRef, ActorRef)>, Box<dyn std::error::Error>> {
    latest_durable_root_application(records)?
        .filter(|record| {
            record.application.as_ref().is_some_and(|application| {
                application.accepted_source.as_deref() == accepted_source
                    && needs_host_recovery(record)
                    && ((record.startup.is_some() && application.conversation.is_none())
                        || application.conversation.is_some())
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
        record.admission.creator.is_none()
            && record.admission.supervisor_parent.is_none()
            && record.admission.context_parent.is_none()
            && record.admission.actor_path.as_deref() == Some("root")
    })
}

#[allow(
    clippy::too_many_arguments,
    reason = "heterogeneous recovery inputs (forest, paths, actor identity, durable records, compiled program, policy, admission); no natural grouping"
)]

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
                .map_err(|error| Failure::JevMalformed(format!("request is not JSON: {error}")))?;
            match self.0.ask(body).await {
                Ok(response) => Ok(response.to_string()),
                Err(failure) => Err(match failure {
                    JevFailure::Unconfigured => Failure::JevUnconfigured,
                    JevFailure::CallCap => Failure::JevCallCap,
                    JevFailure::Transport(detail) => Failure::JevTransport(detail),
                    JevFailure::Timeout => Failure::JevTimeout,
                    JevFailure::Http { status, body } => Failure::JevHttp(i64::from(status), body),
                    JevFailure::CircuitOpen {
                        status,
                        retry_after_ms,
                    } => Failure::JevCircuitOpen(
                        i64::from(status),
                        i64::try_from(retry_after_ms).unwrap_or(i64::MAX),
                    ),
                    JevFailure::ClientSetup(detail) => Failure::JevClientSetup(detail),
                    JevFailure::BodyLimit => Failure::JevBodyLimit,
                    JevFailure::Malformed(detail) => Failure::JevMalformed(detail),
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
            tracing::warn!(%error, "jev client setup failed; Jev requests retain the setup failure");
            exomonad_actor::failed_jev_client_setup(error.to_string())
        }
    }
}

fn reasoning_effort(reasoning: &exomonad_actor::Reasoning) -> harness::model::Effort {
    use exomonad_actor::{ArgonEffort, DeepSeekReasoning as DS, OpenAIReasoning as OAI, Reasoning};
    match reasoning {
        Reasoning::DeepSeekReasoning(DS::DeepSeekNone) => harness::model::Effort::None,
        Reasoning::DeepSeekReasoning(DS::DeepSeekLow)
        | Reasoning::OpenAIReasoning(OAI::OpenAILow)
        | Reasoning::ArgonReasoning(ArgonEffort::EffortLow) => harness::model::Effort::Low,
        Reasoning::DeepSeekReasoning(DS::DeepSeekHigh)
        | Reasoning::OpenAIReasoning(OAI::OpenAIHigh)
        | Reasoning::ArgonReasoning(ArgonEffort::EffortHigh) => harness::model::Effort::High,
        Reasoning::DeepSeekReasoning(DS::DeepSeekMax) => harness::model::Effort::Max,
        Reasoning::OpenAIReasoning(OAI::OpenAIMedium)
        | Reasoning::ArgonReasoning(ArgonEffort::EffortMedium) => harness::model::Effort::Medium,
    }
}

fn reasoning_from_effort(
    backend: exomonad_actor::Backend,
    effort: harness::model::Effort,
) -> Result<exomonad_actor::Reasoning, String> {
    use exomonad_actor::{Backend, DeepSeekReasoning as DS, OpenAIReasoning as OAI, Reasoning};
    use harness::model::Effort;
    match (backend, effort) {
        (Backend::DeepSeek, Effort::None) => Ok(Reasoning::DeepSeekReasoning(DS::DeepSeekNone)),
        (Backend::DeepSeek, Effort::Low) => Ok(Reasoning::DeepSeekReasoning(DS::DeepSeekLow)),
        (Backend::DeepSeek, Effort::High) => Ok(Reasoning::DeepSeekReasoning(DS::DeepSeekHigh)),
        (Backend::DeepSeek, Effort::Max) => Ok(Reasoning::DeepSeekReasoning(DS::DeepSeekMax)),
        (Backend::OpenAIPlan, Effort::Low) => Ok(Reasoning::OpenAIReasoning(OAI::OpenAILow)),
        (Backend::OpenAIPlan, Effort::Medium) => Ok(Reasoning::OpenAIReasoning(OAI::OpenAIMedium)),
        (Backend::OpenAIPlan, Effort::High) => Ok(Reasoning::OpenAIReasoning(OAI::OpenAIHigh)),
        (Backend::GeminiInteractions, Effort::Low) => Ok(Reasoning::ArgonReasoning(
            exomonad_actor::ArgonEffort::EffortLow,
        )),
        (Backend::GeminiInteractions, Effort::Medium) => Ok(Reasoning::ArgonReasoning(
            exomonad_actor::ArgonEffort::EffortMedium,
        )),
        (Backend::GeminiInteractions, Effort::High) => Ok(Reasoning::ArgonReasoning(
            exomonad_actor::ArgonEffort::EffortHigh,
        )),
        _ => Err("retained reasoning is unsupported by the run backend".into()),
    }
}

fn append_inheritance_authority(instructions: &mut String) {
    instructions.push_str("\nInherited parent bindings do not grant parent authority; the runtime policy above governs this actor.\n");
}

#[derive(Debug, Clone)]
pub enum ActorHostReadiness {
    /// Current actor availability is independent of browser listener readiness.
    ActorsObserved {
        actors: watch::Receiver<Vec<harness::server::HostActorProjection>>,
    },
    /// Coordination failed; the native application may still be alive.
    CoordinationFailed { root: ActorRef, error: String },
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
fn prepare_embedded_browser_command(
    operation: harness::embedding::ClientOperationId,
    command: &harness::server::HostCommand,
    store: &Arc<harness::store::Store>,
    run: &str,
    nodes: &[exomonad_actor::ActorGraphNode],
    live: &BTreeSet<ActorRef>,
    projection: &embedded_projection::EmbeddedProjection,
    conversation_for: impl Fn(ActorRef) -> Option<Arc<harness::embedding::Conversation>>,
    lifecycle: &embedded_projection::LifecycleSender,
    accounts: Option<(
        &harness::transport::auth::chatgpt::RunAccountAuth,
        &harness::server::ServerControl,
        &Arc<HostIncarnationLease>,
    )>,
) -> Result<EmbeddedBrowserCommandDispatch, String> {
    use harness::server::CommandReceiptOutcome;

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
        return Ok(EmbeddedBrowserCommandDispatch::Settled(Some(receipt)));
    }
    let Some(_) = store
        .claim_embedded_command(command_run, operation)
        .map_err(|error| error.to_string())?
    else {
        // Another delivery has already claimed this operation. In particular,
        // an uncertain control cannot be replayed against a later round.
        return Ok(EmbeddedBrowserCommandDispatch::Settled(None));
    };
    let settle = |outcome| {
        store
            .settle_embedded_command(command_run, operation, outcome)
            .map(|receipt| EmbeddedBrowserCommandDispatch::Settled(Some(receipt)))
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
        .is_some_and(|node| node.terminal().is_none())
        && live.contains(&actor);
    if !is_live {
        return refuse("target actor is no longer live".into());
    }
    if matches!(command, harness::server::HostCommand::SelectAccount { .. }) {
        if !nodes
            .iter()
            .any(|node| node.actor == actor && node.creator.is_none())
        {
            return refuse("account selection must target the current root actor".into());
        }
        let Some((auth, control, owner)) = accounts else {
            return refuse("saved accounts are unavailable".into());
        };
        return Ok(EmbeddedBrowserCommandDispatch::SelectAccount {
            operation,
            auth: auth.clone(),
            control: control.clone(),
            owner: owner.clone(),
        });
    }
    let Some(conversation) = conversation_for(actor) else {
        return refuse("target has no live model conversation".into());
    };

    Ok(EmbeddedBrowserCommandDispatch::Admitted {
        operation,
        command: command.clone(),
        store: store.clone(),
        actor,
        conversation,
        lifecycle: lifecycle.clone(),
    })
}

enum EmbeddedBrowserCommandDispatch {
    SelectAccount {
        operation: harness::embedding::ClientOperationId,
        auth: harness::transport::auth::chatgpt::RunAccountAuth,
        control: harness::server::ServerControl,
        owner: Arc<HostIncarnationLease>,
    },
    Settled(Option<harness::server::CommandReceipt>),
    Admitted {
        operation: harness::embedding::ClientOperationId,
        command: harness::server::HostCommand,
        store: Arc<harness::store::Store>,
        actor: ActorRef,
        conversation: Arc<harness::embedding::Conversation>,
        lifecycle: embedded_projection::LifecycleSender,
    },
}

impl EmbeddedBrowserCommandDispatch {
    async fn complete(self) -> Result<Option<harness::server::CommandReceipt>, String> {
        use harness::{embedding::HostControlError, server::CommandReceiptOutcome};

        let (operation, command, store, actor, conversation, lifecycle) = match self {
            Self::Settled(receipt) => return Ok(receipt),
            Self::SelectAccount {
                operation,
                auth,
                control,
                owner,
            } => {
                let receipt = tokio::task::spawn_blocking(move || {
                    // Blocking work survives async cancellation; retain the exact
                    // host lease until its atomic Store transition has settled.
                    let _owner = owner;
                    auth.select_account(operation, |selection| {
                        control.publish(
                            harness::server::browser_contract::StateEvent::AccountSelectionUpsert(
                                selection,
                            ),
                        );
                    })
                    .map_err(|error| error.to_string())
                })
                .await
                .map_err(|error| error.to_string())??;
                return Ok(Some(receipt));
            }
            Self::Admitted {
                operation,
                command,
                store,
                actor,
                conversation,
                lifecycle,
            } => (operation, command, store, actor, conversation, lifecycle),
        };
        let target = command.target().clone();
        let command_run = &command.target().run;
        let settle = |outcome| {
            store
                .settle_embedded_command(command_run, operation, outcome)
                .map(Some)
                .map_err(|error| error.to_string())
        };
        let refuse = |reason: String| {
            settle(CommandReceiptOutcome::Refused {
                target: Some(target.clone()),
                reason,
            })
        };

        let atomic_receipt = match &command {
            harness::server::HostCommand::Input { text, .. } => {
                Some(conversation.command_input(operation, text).await)
            }
            harness::server::HostCommand::Recover { .. } => {
                Some(conversation.command_recover(operation).await)
            }
            _ => None,
        };
        if let Some(result) = atomic_receipt {
            return match result {
                Ok(receipt) => Ok(Some(receipt)),
                Err(error) => {
                    // Observation cannot turn a committed input or recovery
                    // authorization into a refusal after its atomic transition.
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
        let control = match &command {
            harness::server::HostCommand::Interrupt { expected_round, .. } => {
                harness::embedding::HostControl::Interrupt {
                    expected_round: *expected_round,
                }
            }
            harness::server::HostCommand::Retire { .. } => harness::embedding::HostControl::Retire,
            harness::server::HostCommand::Input { .. }
            | harness::server::HostCommand::Recover { .. }
            | harness::server::HostCommand::SelectAccount { .. } => {
                unreachable!("atomic commands return their retained receipt")
            }
        };
        match conversation.control(control).await {
            Ok(_) => {
                let control = match &command {
                    harness::server::HostCommand::Retire { .. } => {
                        lifecycle.publish(actor, harness::server::HostActorLifecycle::Retiring);
                        harness::server::CommandControl::Retire
                    }
                    harness::server::HostCommand::Interrupt { .. } => {
                        harness::server::CommandControl::Interrupt
                    }
                    harness::server::HostCommand::Input { .. }
                    | harness::server::HostCommand::Recover { .. }
                    | harness::server::HostCommand::SelectAccount { .. } => {
                        unreachable!("atomic commands return their retained receipt")
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
}

#[cfg(test)]
#[allow(clippy::too_many_arguments)]
async fn dispatch_embedded_browser_command(
    operation: harness::embedding::ClientOperationId,
    command: &harness::server::HostCommand,
    store: &Arc<harness::store::Store>,
    run: &str,
    nodes: &[exomonad_actor::ActorGraphNode],
    live: &BTreeSet<ActorRef>,
    projection: &embedded_projection::EmbeddedProjection,
    conversation_for: impl Fn(ActorRef) -> Option<Arc<harness::embedding::Conversation>>,
    lifecycle: &embedded_projection::LifecycleSender,
) -> Result<Option<harness::server::CommandReceipt>, String> {
    prepare_embedded_browser_command(
        operation,
        command,
        store,
        run,
        nodes,
        live,
        projection,
        conversation_for,
        lifecycle,
        None,
    )?
    .complete()
    .await
}

type BrowserCommandTasks = JoinSet<Result<Option<harness::server::CommandReceipt>, String>>;

#[allow(
    clippy::too_many_arguments,
    reason = "drain through the existing Store and actor authority owners"
)]
fn drain_embedded_browser_commands(
    store: &Arc<harness::store::Store>,
    control: &harness::server::ServerControl,
    run: &str,
    nodes: &[exomonad_actor::ActorGraphNode],
    live: &BTreeSet<ActorRef>,
    projection: &embedded_projection::EmbeddedProjection,
    conversations: &HashMap<ActorRef, embedded_harness::EmbeddedActorBinding>,
    lifecycle: &embedded_projection::LifecycleSender,
    tasks: &mut BrowserCommandTasks,
    account_auth: Option<(
        &harness::transport::auth::chatgpt::RunAccountAuth,
        &Arc<HostIncarnationLease>,
    )>,
) -> Result<(), String> {
    // The channel is a wake hint. Store rows survive the persist/enqueue gap
    // and are also drained by the existing host health tick.
    for record in store
        .queued_embedded_commands(run)
        .map_err(|error| error.to_string())?
    {
        if let Some(binding) = conversations
            .values()
            .find(|binding| binding.identity() == record.command.target())
        {
            if binding.is_pending()
                && (matches!(
                    record.command,
                    harness::server::HostCommand::Input { .. }
                        | harness::server::HostCommand::Recover { .. }
                ) || binding.control_conversation().is_none())
            {
                continue;
            }
        }
        let dispatch = prepare_embedded_browser_command(
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
                    .and_then(|binding| match record.command {
                        harness::server::HostCommand::Input { .. }
                        | harness::server::HostCommand::Recover { .. } => binding.conversation(),
                        _ => binding.control_conversation(),
                    })
            },
            lifecycle,
            account_auth.map(|(auth, owner)| (auth, control, owner)),
        )?;
        match dispatch {
            EmbeddedBrowserCommandDispatch::Settled(Some(receipt)) => {
                control.publish_command_receipt(receipt);
            }
            EmbeddedBrowserCommandDispatch::Settled(None) => {}
            admitted @ (EmbeddedBrowserCommandDispatch::Admitted { .. }
            | EmbeddedBrowserCommandDispatch::SelectAccount { .. }) => {
                // The sole supervisor must keep receiving native release events
                // while this exact conversation awaits its retirement receipt.
                tasks.spawn(admitted.complete());
            }
        }
    }
    Ok(())
}

#[derive(Clone)]
struct BoundWorkspace {
    workspace: Arc<ActiveWorkspace>,
}

type ActorInbox = DurableInbox<DurableActorEvent, DeliveryProvenance>;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
enum DeliveryProvenance {
    Notification { sender: ActorRef, target: ActorRef },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
enum DurableActorEvent {
    Text(String),
}

struct InteractiveApplicationOwner {
    creator_workspace: Option<BoundWorkspace>,
    embedded: EmbeddedApplicationState,
    terminal: Option<ActorTerminal>,
}

/// Per-actor Engine custody, liveness, and retained conversation state.
/// Projection state remains in `EmbeddedProjection`; this state owns the
/// resources and facts needed to move one actor through its host lifecycle.
struct EmbeddedApplicationState {
    task_id: Option<tokio::task::Id>,
    cancellation: Option<watch::Sender<bool>>,
    live: bool,
    conversation: Option<embedded_harness::EmbeddedActorBinding>,
    cleanup_failure: Option<embedded_service::EmbeddedDriverError>,
}

impl EmbeddedApplicationState {
    fn new() -> Self {
        Self {
            task_id: None,
            cancellation: None,
            live: false,
            conversation: None,
            cleanup_failure: None,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
enum ApplicationShutdown {
    #[default]
    Running,
    Draining,
    ForestSettled,
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
        .map(|owner| update(&mut owner.embedded))
}

fn update_embedded_state(
    owners: &InteractiveOwners,
    actor: ActorRef,
    update: impl FnOnce(&mut EmbeddedApplicationState),
) {
    if let Some(owner) = owners.lock().get_mut(&actor) {
        update(&mut owner.embedded);
    }
}

fn embedded_is_live(owners: &InteractiveOwners, actor: ActorRef) -> bool {
    owners
        .lock()
        .get(&actor)
        .is_some_and(|owner| owner.embedded.live)
}

fn embedded_binding(
    owners: &InteractiveOwners,
    actor: ActorRef,
) -> Option<embedded_harness::EmbeddedActorBinding> {
    owners
        .lock()
        .get(&actor)
        .and_then(|owner| owner.embedded.conversation.clone())
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
                .conversation
                .clone()
                .map(|binding| (*actor, binding))
        })
        .collect()
}

fn embedded_live_actors(owners: &InteractiveOwners) -> BTreeSet<ActorRef> {
    owners
        .lock()
        .iter()
        .filter_map(|(actor, owner)| owner.embedded.live.then_some(*actor))
        .collect()
}

fn embedded_actor_for_task(owners: &InteractiveOwners, task: tokio::task::Id) -> Option<ActorRef> {
    owners
        .lock()
        .iter()
        .find_map(|(actor, owner)| (owner.embedded.task_id == Some(task)).then_some(*actor))
}

impl InteractiveApplicationOwner {
    fn embedded() -> Self {
        Self {
            creator_workspace: None,
            embedded: EmbeddedApplicationState::new(),
            terminal: None,
        }
    }

    fn retired(&mut self, terminal: ActorTerminal) {
        if self.terminal.is_some() {
            return;
        }
        self.terminal.get_or_insert(terminal);
        self.creator_workspace = None;
    }
}

/// Returned to run's real host caller, not formatted into a resource-free error.
/// The host retains this error through shutdown. Dropping it at process exit is
/// not settlement of the unfinished supervisor.
pub(crate) struct RetainedInteractiveFleet {
    owners: InteractiveOwners,
    task: tokio::task::JoinHandle<Result<(), String>>,
    failures: Vec<Box<dyn std::error::Error>>,
}
impl fmt::Debug for RetainedInteractiveFleet {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("RetainedInteractiveFleet")
            .field("actors", &self.owners.lock().keys().collect::<Vec<_>>())
            .field("unfinished", &!self.task.is_finished())
            .field("failures", &self.failures)
            .finish()
    }
}
impl fmt::Display for RetainedInteractiveFleet {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for error in &self.failures {
            write!(f, "{error}; ")?;
        }
        write!(f, "actor host task remains unfinished")
    }
}
impl std::error::Error for RetainedInteractiveFleet {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        self.failures.first().map(|error| error.as_ref())
    }
}

struct ApplicationRunCleanupError {
    run: Box<dyn std::error::Error>,
    cleanup: Box<dyn std::error::Error>,
}

impl fmt::Display for ApplicationRunCleanupError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "application run failed: {}; application cleanup failed: {}",
            self.run, self.cleanup
        )
    }
}

impl fmt::Debug for ApplicationRunCleanupError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ApplicationRunCleanupError")
            .field("run", &self.run)
            .field("cleanup", &self.cleanup)
            .finish()
    }
}

impl std::error::Error for ApplicationRunCleanupError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(self.run.as_ref())
    }
}

fn application_run_cleanup_error(
    cleanup: Result<(), Box<dyn std::error::Error>>,
    run: Result<(), Box<dyn std::error::Error>>,
) -> Result<(), Box<dyn std::error::Error>> {
    match (run, cleanup) {
        (Ok(()), Ok(())) => Ok(()),
        (Ok(()), Err(cleanup)) => Err(cleanup),
        (Err(run), Ok(())) => Err(run),
        (Err(run), Err(cleanup)) => Err(Box::new(ApplicationRunCleanupError { run, cleanup })),
    }
}

fn handoff_application_owners(
    owners: InteractiveOwners,
    task: tokio::task::JoinHandle<Result<(), String>>,
    cleanup: Result<(), Box<dyn std::error::Error>>,
    run_result: Result<(), Box<dyn std::error::Error>>,
) -> Result<(), Box<dyn std::error::Error>> {
    if !task.is_finished() {
        let failures = application_run_cleanup_error(cleanup, run_result)
            .err()
            .into_iter()
            .collect();
        return Err(Box::new(RetainedInteractiveFleet {
            owners,
            task,
            failures,
        }));
    }
    application_run_cleanup_error(cleanup, run_result)
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

struct InteractiveFleet {
    provider_forest: Arc<ResidentForest<ExomonadHandlerStack, CapturedOutput>>,
    recovery_store: Arc<tidepool_runtime::session::RunRecoveryStore>,
    root: LocalActorRef,
    config: ActorHostConfig,
    run_root: PathBuf,
    worktrees: WorktreeManager,

    /// Readiness events are best-effort notifications: a dropped receiver
    /// means the caller stopped observing startup, not a delivery bug, so
    /// every `readiness.send(..)` below discards the `SendError` with `.ok()`.
    readiness: mpsc::UnboundedSender<ActorHostReadiness>,
    worktree_authority: ActorWorktreeAuthority,

    host_graph: Arc<
        dyn Fn() -> futures_util::future::BoxFuture<
                'static,
                Result<Vec<exomonad_actor::ActorGraphNode>, exomonad_actor::DirectoryError>,
            > + Send
            + Sync,
    >,
    #[cfg(test)]
    test_observer: Option<hosted_test_context::HostTestObserver>,
}

#[derive(Clone)]
struct InteractiveLaunchContext {
    base_prompt: FrozenBasePrompt,
    root: ActorRef,
    config: ActorHostConfig,
    run_root: PathBuf,

    worktrees: WorktreeManager,
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
    #[error("root startup lost its retained public recovery domain at {path}")]
    MissingIssuedRecoveryStore { path: PathBuf },
    #[error("initial context for {actor:?} requires reconciliation after host interruption")]
    InterruptedInitialContext { actor: ActorRef },
}

/// Issue the initially empty journals before publishing durable launch admission.
/// A host only reopens these owners; it never recreates missing history.
pub(crate) async fn admit_run_journals(
    directory: &tidepool_atomic_write::DirectoryAnchor,
    workspace: &Path,
    run_id: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    let journal = exomonad_actor::ActorRecoveryJournal::open(
        directory,
        "actor-lifecycle.v2.jsonl",
        Arc::new(directory.clone()),
    )
    .await?;
    let mut status = journal.owner_status();
    drop(journal);
    tokio::time::timeout(APPLICATION_SHUTDOWN_TIMEOUT, async {
        loop {
            match status.borrow_and_update().clone() {
                exomonad_actor::JournalOwnerStatus::Stopped {
                    clean: true,
                    fenced: false,
                } => return Ok(()),
                exomonad_actor::JournalOwnerStatus::Stopped { .. } => {
                    return Err(runtime_error("initial actor journal did not close cleanly"))
                }
                _ => {}
            }
            status.changed().await.map_err(|_| {
                runtime_error("initial actor journal owner closed without settlement")
            })?;
        }
    })
    .await
    .map_err(|_| runtime_error("initial actor journal close timed out"))??;
    let actor_path = directory.path().join("actor-lifecycle.v2.jsonl");
    std::fs::File::open(&actor_path)?.sync_all()?;
    tidepool_atomic_write::sync_parent_directory(&actor_path)?;
    let path = crate::exomonad::exomonad_journal_path(workspace, run_id);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    drop(tidepool_handlers::JournalHandler::new(
        tidepool_handlers::SegmentPath::create_exclusive(path.clone())?,
    )?);
    std::fs::File::open(&path)?.sync_all()?;
    tidepool_atomic_write::sync_parent_directory(&path)?;
    Ok(())
}

async fn await_host_lease_release(
    released: &mut watch::Receiver<host_incarnation::HostLeaseRelease>,
    budget: Duration,
) -> Result<(), String> {
    tokio::time::timeout(budget, async {
        loop {
            match released.borrow_and_update().clone() {
                host_incarnation::HostLeaseRelease::Released => return Ok(()),
                host_incarnation::HostLeaseRelease::Unconfirmed(detail) => return Err(detail),
                host_incarnation::HostLeaseRelease::Pending => {}
            }
            released.changed().await.map_err(|_| {
                "host lease release observer closed without release acknowledgment".to_owned()
            })?;
        }
    })
    .await
    .map_err(|_| "physical host lease release remains unconfirmed after shutdown".to_owned())?
}

async fn acknowledge_completed_run_release(
    outcome: Result<(), Box<dyn std::error::Error>>,
    mut released: watch::Receiver<host_incarnation::HostLeaseRelease>,
) -> Result<(), Box<dyn std::error::Error>> {
    // Resource-bearing failures retain their original owner and error type.
    // Only successful completion may promise physical run release.
    outcome?;
    await_host_lease_release(&mut released, APPLICATION_SHUTDOWN_TIMEOUT)
        .await
        .map_err(runtime_error)
}

pub(crate) async fn run(
    config: ActorHostConfig,
    readiness: mpsc::UnboundedSender<ActorHostReadiness>,
    host_incarnation: HostIncarnationLease,
) -> Result<(), Box<dyn std::error::Error>> {
    let released = host_incarnation.release_observation();
    let outcome = run_owned(
        config,
        readiness,
        host_incarnation,
        #[cfg(test)]
        None,
        #[cfg(test)]
        None,
    )
    .await;
    acknowledge_completed_run_release(outcome, released).await
}

#[cfg(test)]
pub(crate) use hosted_test_context::HostTestHooks;

#[cfg(test)]
pub(crate) async fn run_with_test_hooks(
    config: ActorHostConfig,
    readiness: mpsc::UnboundedSender<ActorHostReadiness>,
    host_incarnation: HostIncarnationLease,
    hooks: HostTestHooks,
) -> Result<(), Box<dyn std::error::Error>> {
    let released = host_incarnation.release_observation();
    let outcome = run_owned(config, readiness, host_incarnation, None, Some(hooks)).await;
    acknowledge_completed_run_release(outcome, released).await
}

#[cfg(test)]
async fn run_with_test_transport(
    config: ActorHostConfig,
    readiness: mpsc::UnboundedSender<ActorHostReadiness>,
    host_incarnation: HostIncarnationLease,
    transport: Arc<dyn harness::engine::ResponsesTransport>,
) -> Result<(), Box<dyn std::error::Error>> {
    let released = host_incarnation.release_observation();
    let outcome = run_owned(config, readiness, host_incarnation, Some(transport), None).await;
    acknowledge_completed_run_release(outcome, released).await
}

#[tracing::instrument(target = "tidepool::actor_host::startup", name = "host_run", skip_all, fields(run_root = %config.run_directory.path().display()))]
async fn run_owned(
    config: ActorHostConfig,
    readiness: mpsc::UnboundedSender<ActorHostReadiness>,
    host_incarnation: HostIncarnationLease,
    #[cfg(test)] test_transport: Option<Arc<dyn harness::engine::ResponsesTransport>>,
    #[cfg(test)] mut test_hooks: Option<hosted_test_context::HostTestHooks>,
) -> Result<(), Box<dyn std::error::Error>> {
    let settings = config
        .embedded
        .as_ref()
        .ok_or_else(|| runtime_error("embedded host requires [launch.embedded]"))?;
    if config.model.backend() != settings.provider
        || config.reasoning.backend() != settings.provider
        || config
            .inference_auth
            .as_ref()
            .is_some_and(|auth| auth.backend() != settings.provider)
    {
        return Err(runtime_error(
            "model, reasoning and transport must select one run backend",
        ));
    }
    if config.model.value().trim().is_empty() {
        return Err(runtime_error("run model must be nonempty"));
    }
    if let Some(auth) = &config.inference_auth {
        auth.validate_for(settings).map_err(runtime_error)?;
        display_output::require_unopened_run_store(config.run_directory.path())
            .map_err(runtime_error)?;
    }
    let (host_incarnation, boot_registration) = host_incarnation.into_boot_registration();
    let run_root = config.run_directory.path().to_path_buf();
    if !host_incarnation.owns_run(&run_root)? {
        return Err(runtime_error(
            "host incarnation belongs to another run directory",
        ));
    }
    config.run_directory.create_dir_all("")?;
    let workspace = config.workspace.clone();
    let resource_run_directory = config.run_directory.clone();
    #[cfg(test)]
    let owner_admission = test_hooks
        .as_ref()
        .map(|hooks| Arc::clone(&hooks.owner_admission));
    let (worktrees, bindings, worktree_directory) =
        tidepool_runtime::spawn_blocking_in_span(move || {
            tracing::info_span!(target: "tidepool::actor_host::startup", "worktree_resources")
                .in_scope(|| {
                    actor_worktree_resources(&workspace, &resource_run_directory, || {
                        #[cfg(test)]
                        if let Some(admission) = owner_admission {
                            *admission.lock() = hosted_test_context::HostOwnerAdmission::Admitted;
                        }
                    })
                })
        })
        .await??;
    let bindings = Arc::new(Mutex::new(bindings));
    let worktree_authority =
        ActorWorktreeAuthority::new(runtime_namespace(&run_root), Arc::clone(&bindings));

    let application_owners: InteractiveOwners = Arc::new(Mutex::new(HashMap::new()));
    let source_layers = source_service(
        &config,
        &run_root,
        worktrees.clone(),
        crate::exomonad::source::SourceRootOwner::Host(Arc::clone(&host_incarnation)),
    )
    .map_err(|error| crate::exomonad::boot_stage_refusal("accepted source", error))?;
    let (actor_recovery, recovery_boot) = exomonad_actor::ActorRecoveryJournal::open_for_boot(
        &config.run_directory,
        "actor-lifecycle.v2.jsonl",
        true,
        Box::new(boot_registration),
    )
    .await
    .map_err(|error| crate::exomonad::boot_stage_refusal("actor journal", error))?;
    let startup_records = embedded_recovery::observe_boot(&actor_recovery, &run_root)
        .await
        .map_err(|error| crate::exomonad::boot_stage_refusal("startup prerequisites", error))?;
    let run_journal_mode = JournalOpenMode::Resume;
    let embedded_service = embedded_service::EmbeddedService::prepare_owned(
        &run_root,
        settings,
        Arc::clone(&host_incarnation),
        config.inference_auth.clone(),
        config
            .workspace_inputs
            .as_ref()
            .and_then(|workspace| workspace.model_catalog.as_ref()),
    )
    .instrument(
        tracing::info_span!(target: "tidepool::actor_host::startup", "embedded_service_prepare"),
    )
    .await
    .map_err(runtime_error)?;
    #[cfg(test)]
    let mut embedded_service = embedded_service;
    #[cfg(test)]
    if let Some(transport) = test_transport {
        embedded_service.set_test_transport(transport);
    }
    #[cfg(test)]
    if let Some(hooks) = &mut test_hooks {
        if let Some(factory) = hooks.transport.take() {
            embedded_service.set_test_transport(factory(&embedded_service.runtime, &config));
        }
    }
    let mut embedded_startup = embedded_recovery::EmbeddedStartupRecovery {
        lease: Arc::clone(&host_incarnation),
        journal: Arc::clone(&actor_recovery),
        store: embedded_service.runtime.store(),
        root_binding_path: config.root_binding_path.clone(),
        manifest: None,
    };
    let startup_observation = embedded_startup.observe_manifest(startup_records);
    let web = tidepool_handlers::WebHandler::default();
    #[cfg(test)]
    let web = test_hooks
        .as_mut()
        .and_then(|hooks| hooks.web_transport.take())
        .map(tidepool_handlers::WebHandler::with_transport)
        .unwrap_or(web);
    let CompiledRoot {
        source,
        root,
        program,
        child_session_factory,
        image_registry,
        recovery_store,
    } = run_compiler_preparation(|settlement| {
        compile_root(
            &config,
            &config.run_directory,
            &worktree_directory,
            worktrees.clone(),
            worktree_authority.clone(),
            web,
            source_layers.as_ref(),
            Arc::clone(&host_incarnation),
            run_journal_mode,
            Some(startup_observation),
            settlement,
        )
    })?;
    let prior_actor_records = actor_recovery.records().await?;
    let accepted_source = active_source_identity(&run_root, config.workspace_inputs.is_some())?;
    let (descriptor, machine, entry) = root.into_parts();
    let prepared_validation = config
        .workspace_inputs
        .as_ref()
        .filter(|inputs| inputs.prepared_toolset_coverage().is_some())
        .map(|inputs| {
            let mut supported = host_context_support();
            supported.extend(
                exomonad_tool::InstalledEffectSupport::installed_effect_support(machine.handlers()),
            );
            (inputs, source.clone(), supported)
        });
    let exomonad_actor::ResidentRootEntry::Startup(entry) = entry else {
        return Err(runtime_error(
            "root startup requires its installed executable entry",
        ));
    };
    let bootstrap_identity = entry
        .compile_input_identity()
        .ok_or_else(|| {
            runtime_error(
                "durable root bootstrap requires replay-eligible compiler input continuity",
            )
        })?
        .to_owned();
    let worktree_admission = fork_workspace_admission(
        worktrees.clone(),
        worktree_authority.clone(),
        bindings.clone(),
        runtime_namespace(&run_root),
    );
    let (forest, deployments) = ResidentForest::new(
        source,
        descriptor.placement().session,
        machine,
        Some(worktree_admission.clone()),
        host_incarnation.incarnation(),
    )
    .await?;
    let forest = forest
        .with_usage_pointers(exomonad_actor::UsagePointerTable::discover(
            &config.workspace,
        )?)
        .with_recovery_journal(actor_recovery.clone())
        .with_child_session_factory(child_session_factory)
        .with_handler_effect_support(
            exomonad_tool::InstalledEffectSupport::installed_effect_support,
        )
        .with_image_registry(image_registry);
    forest
        .register_recovery_boot(recovery_boot)
        .await
        .map_err(|error| runtime_error(error.to_string()))?;
    // Production hosts retain the launching machine. Dedicated acceptance
    // campaigns select the existing capability with the owner-produced root.
    #[cfg(test)]
    let forest = if test_hooks.as_ref().is_some_and(|hooks| {
        hooks.child_machines == hosted_test_context::HostedChildMachines::Dedicated
    }) {
        forest.with_child_bootstrap_program(Arc::clone(&program))
    } else {
        forest
    };
    let _ = &program;
    let interpreted_source_layers = source_layers
        .clone()
        .map(|layers| layers as exomonad_actor::ActorSourceLayerResolver);
    #[cfg(test)]
    let interpreted_source_layers = interpreted_source_layers.map(|layers| {
        test_hooks
            .as_ref()
            .and_then(|hooks| hooks.source_layers.as_ref())
            .map_or_else(|| layers.clone(), |transform| transform(layers.clone()))
    });
    let forest = with_host_interpreters(
        forest,
        interpreted_source_layers,
        &config,
        &embedded_service.runtime,
        actor_recovery.clone(),
        cell_model::admitted_factory(&embedded_service, settings, &config),
    )?;
    if let Some((inputs, workbench, mut supported)) = prepared_validation {
        for key in forest.installed_intrinsic_effect_support() {
            if !supported.contains(&key) {
                supported.push(key);
            }
        }
        let layers = source_layers.as_ref().ok_or_else(|| {
            runtime_error("prepared toolset validation requires its admitted source owner")
        })?;
        let frozen = layers.prepared_toolset_layer()?;
        inputs.validate_prepared_toolset_recipes(&workbench, &frozen, &supported)?;
    }
    forest.track_resource_release();
    let forest = Arc::new(forest);
    let recovered_root = durable_root_identity(&prior_actor_records, accepted_source.as_deref())
        .map_err(|error| crate::exomonad::boot_stage_refusal("root identity", error))?;
    if contains_durable_root_admission(&prior_actor_records) && recovered_root.is_none() {
        return Err(crate::exomonad::boot_stage_refusal("root identity", runtime_error(
            "host recovery cannot adopt a root without complete durable actor, source, and conversation evidence",
        )));
    }

    forest
        .fence_recovery_identities(
            prior_actor_records
                .iter()
                .filter(|record| needs_host_recovery(record))
                .map(|record| record.admission.actor.id)
                .chain(recovered_root.map(|(_, actor)| actor.id)),
        )
        .await?;
    let mut startup_intent = embedded_startup
        .intent(
            &run_root,
            recovered_root
                .map(|(_, identity)| identity)
                .unwrap_or(ActorRef::first(exomonad_actor::ActorId(0))),
            accepted_source.clone(),
            bootstrap_identity.clone(),
        )
        .await
        .map_err(|error| crate::exomonad::boot_stage_refusal("startup ownership", error))?;
    let (root_actor, mut root_task, startup_release) = async {
        Ok::<_, Box<dyn std::error::Error>>(match recovered_root {
            Some((_, identity)) => {
                forest
                    .admit_pending_root_with_identity(
                        descriptor,
                        entry,
                        identity,
                        startup_intent.clone(),
                    )
                    .await?
            }
            None => {
                let mut intent = startup_intent.clone();
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
        })
    }
    .instrument(tracing::info_span!(target: "tidepool::actor_host::startup", "root_admission"))
    .await?;
    startup_intent.conversation = embedded_recovery::conversation(
        &embedded_recovery::host_identity(&run_root, "/root", root_actor.identity()),
    );
    #[cfg(test)]
    embedded_recovery_tests::startup_checkpoint("admitted");
    if let Err(error) = actor_recovery
        .prepare_application_with_intent(
            root_actor.identity(),
            config.root_binding_path.clone(),
            accepted_source.clone(),
            Some(embedded_recovery::conversation(
                &embedded_recovery::host_identity(&run_root, "/root", root_actor.identity()),
            )),
        )
        .await
    {
        forest.shutdown().await;
        return Err(runtime_error(format!(
            "root application ownership could not be journalled before declaration recovery: {error}"
        )));
    }
    let declaration_recovery = async {
        let intent = &startup_intent;
        let release = &startup_release;
        let service = &embedded_service;
        let placement = forest
            .root_recovery_placement(root_actor.identity())
            .await
            .map_err(|error| runtime_error(error.to_string()))?;
        let claim = if let Some(pin) = intent.manifest.as_ref() {
            forest
                .begin_successor_operation(root_actor.identity(), &pin.public_owner.owner)
                .await
                .map_err(|error| runtime_error(error.to_string()))?
        } else {
            forest
                .begin_root_binding_operation(root_actor.identity())
                .await
                .map_err(|error| runtime_error(error.to_string()))?
        };
        let admission = if let Some(predecessor) = intent.manifest_predecessor {
            Some(
                actor_recovery
                    .certify_root_successor(
                        predecessor,
                        placement,
                        accepted_source.as_deref(),
                        &config.root_binding_path,
                        &claim,
                    )
                    .await?,
            )
        } else {
            None
        };
        let manifest = match (intent.manifest_predecessor, admission.as_ref()) {
            (Some(_), Some(admission)) => {
                let pin = intent.manifest.as_ref().ok_or_else(|| {
                    runtime_error("root manifest predecessor has no exact selected owner pin")
                })?;
                forest
                    .transfer_recovered_root_public_owner(
                        root_actor.identity(),
                        &pin.public_owner.owner,
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
            tidepool_runtime::session::PublicManifestCommit::Refused { error }
            | tidepool_runtime::session::PublicManifestCommit::RecoveryRequired { error } => {
                return Err(runtime_error(error.to_string()));
            }
            outcome => {
                return Err(runtime_error(format!(
                    "root startup manifest did not become durable: {outcome:?}"
                )))
            }
        }
        #[cfg(test)]
        embedded_recovery_tests::startup_checkpoint("manifest");
        let installed = forest
            .committed_public_readiness(&claim)
            .await
            .map_err(|error| runtime_error(error.to_string()))?;
        let origin = match admission {
            Some(admission) => embedded_recovery::EmbeddedBindingOrigin::Successor(admission),
            None => embedded_recovery::EmbeddedBindingOrigin::Fresh,
        };
        claim
            .finish_binding(
                installed,
                Arc::new(embedded_recovery::EmbeddedRootBinding {
                    lease: Arc::clone(&host_incarnation),
                    journal: Arc::clone(&actor_recovery),
                    store: service.runtime.store(),
                    run_root: run_root.clone(),
                    intent: intent.clone(),
                    origin,
                }),
            )
            .await
            .map_err(runtime_error)?;
        #[cfg(test)]
        embedded_recovery_tests::startup_checkpoint("bound");
        forest
            .release_root_startup(release)
            .await
            .map_err(|error| runtime_error(error.to_string()))?;
        #[cfg(test)]
        embedded_recovery_tests::startup_checkpoint("released");
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
        Ok::<(), Box<dyn std::error::Error>>(())
    }
    .instrument(
        tracing::info_span!(target: "tidepool::actor_host::startup", "root_declaration_recovery"),
    )
    .await;
    if let Err(error) = declaration_recovery {
        let summary = format!("root declaration recovery remains unavailable: {error}");
        let cleanup = tokio::time::timeout(
            APPLICATION_SHUTDOWN_TIMEOUT,
            root_actor.shutdown(ActorTerminal {
                kind: ActorExitKind::Failed,
                summary: summary.clone(),
                diagnostic: None,
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

    let unavailable_records = prior_actor_records
        .iter()
        .filter(|record| {
            needs_host_recovery(record) && record.admission.actor.id != root_actor.identity().id
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
        let unavailable = unavailable_records
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>();
        notice.push_str(&format!(
            " Durable actor reconstruction admitted: {}. Durable actors still unavailable: {}.",
            "none",
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
    let operator_role = admitted_capabilities(config.workspace_inputs.as_ref())
        .map_err(runtime_error)?
        .with_conversation_permission(None);
    let operator_socket = run_root.join("operator").join("operator.sock");
    if operator_socket.exists() {
        std::fs::remove_file(&operator_socket)?;
    }
    let inspection_forest = forest.clone();
    let artifact_owners = application_owners.clone();
    let artifact_run_id = runtime_namespace(&run_root);
    let operator = crate::operator::OperatorService::bind(
        operator_socket.clone(),
        Some(embedded_service.control.input_bridge()),
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
        Arc::new(move |requester| {
            let forest = Arc::clone(&inspection_forest);
            Box::pin(async move { forest.inspect_graph(requester).await })
        }),
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
    let (shutdown, shutdown_rx) = watch::channel(ApplicationShutdown::Running);
    let (coordination_failed, mut coordination_failure) = oneshot::channel();
    let (_root_config, root_config_rx) = watch::channel(config.clone());

    let host_graph_forest = Arc::clone(&forest);
    let host_graph = Arc::new(move || {
        let forest = Arc::clone(&host_graph_forest);
        Box::pin(async move { forest.inspect_host_graph().await })
            as futures_util::future::BoxFuture<'static, _>
    });
    tracing::info!(target: "tidepool::actor_host::startup", actor = %root_actor.identity(), "production assembly ready");
    #[cfg(test)]
    let test_observer = test_hooks.as_ref().map(|hooks| hooks.observer.clone());
    #[cfg(test)]
    if let Some(hooks) = &mut test_hooks {
        if let Some(assembled) = hooks.assembled.take() {
            let _ = assembled.send(hosted_test_context::HostedActorContext {
                config: config.clone(),
                service_address: embedded_service.address,
                actor: root_actor.clone(),
                forest: Arc::clone(&forest),
                runtime: Arc::clone(&embedded_service.runtime),
                recovery_store: Arc::clone(&recovery_store),
                journal_status: actor_recovery.owner_status(),
                observer: hooks.observer.clone(),
                owners: Arc::clone(&application_owners),
            });
        }
    }
    #[cfg(test)]
    let shutdown_observer = test_observer.clone();
    let run_store = embedded_service.runtime.store();
    let mut applications_task = tokio::spawn(run_interactive_applications(
        deployments,
        application_owners.clone(),
        InteractiveFleet {
            provider_forest: Arc::clone(&forest),
            recovery_store: Arc::clone(&recovery_store),
            root: root_actor.clone(),
            config: config.clone(),
            run_root: run_root.clone(),
            worktrees,

            readiness: readiness.clone(),
            worktree_authority: worktree_authority.clone(),

            host_graph,
            #[cfg(test)]
            test_observer,
        },
        shutdown_rx,
        coordination_failed,
        root_config_rx,
        embedded_service,
    ));
    let mut applications_finished = false;
    let mut root_active = true;
    let test_stop = async {
        #[cfg(test)]
        hosted_test_context::test_stop(&mut test_hooks).await;
        #[cfg(not(test))]
        std::future::pending::<()>().await;
    };
    tokio::pin!(test_stop);
    let mut host_stopped = false;
    let result: Result<(), Box<dyn std::error::Error>> = async {
        loop {
            tokio::select! {
                biased;
                failure = recovery_store.wait_recovery_required() => {
                    break Err(runtime_error(format!("native run store requires authenticated recovery: {failure}")));
                }
                failure = run_store.wait_recovery_required() => {
                    break Err(runtime_error(format!("embedded run store requires authenticated recovery: {failure}")));
                }
                failure = &mut coordination_failure => {
                    break Err(runtime_error(failure.unwrap_or_else(|_| "interactive application coordination owner stopped".into())));
                }
                signal = operator_shutdown() => { signal?; host_stopped = true; break Ok(()); }
                _ = &mut test_stop => { host_stopped = true; break Ok(()); },
                result = &mut applications_task => {
                    applications_finished = true;
                    break result.map_err(join_error)?.map_err(runtime_error);
                }
                result = &mut root_task, if root_active => {
                    result.map_err(join_error)?;
                    root_actor.terminal().get().ok_or_else(|| runtime_error("root stopped without terminal"))?;
                    root_active = false;

                }
            }
        }
    }.await;
    shutdown.send_replace(ApplicationShutdown::Draining);
    operator.shutdown().await;
    let forest_shutdown = if host_stopped {
        forest.interrupt_for_host_stop().await
    } else {
        forest.shutdown().await
    };
    #[cfg(test)]
    if let Some(observer) = &shutdown_observer {
        observer.forest_shutdown(&forest_shutdown);
    }
    shutdown.send_replace(ApplicationShutdown::ForestSettled);
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
#[cfg(test)]
fn native_exit_required(conversation: Option<&exomonad_actor::ApplicationConversation>) -> bool {
    !matches!(
        conversation,
        Some(exomonad_actor::ApplicationConversation::Embedded { .. })
    )
}

#[cfg(test)]
fn root_never_bound(
    application: Option<&exomonad_actor::DurableActorApplication>,
    root_binding_path: &Path,
) -> bool {
    match application.and_then(|application| application.conversation.as_ref()) {
        Some(exomonad_actor::ApplicationConversation::Embedded { .. }) => false,
        _ => !root_binding_path.exists(),
    }
}

/// Classify an intentional completion or prepare an abnormal root for a fresh
/// incarnation attached to the retained conversation.

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
    run_directory: &tidepool_atomic_write::DirectoryAnchor,
    admit_runtime_owners: impl FnOnce(),
) -> Result<
    (
        WorktreeManager,
        BindingTable,
        tidepool_atomic_write::DirectoryAnchor,
    ),
    exomonad_worktree::WorktreeError,
> {
    let git = admitted_workspace_git(workspace)?;
    // The hosted-runtime domain begins before any worktree registry, service,
    // actor or task can be constructed, including partially failed construction.
    // Diagnostic directories and the RAII incarnation lease precede this domain;
    // no runtime admission is a storage-reclamation or daemon-cleanup receipt.
    admit_runtime_owners();
    let root = actor_worktree_storage_root(workspace, run_directory.path())?;
    let family = root.parent().and_then(Path::parent).ok_or_else(|| {
        exomonad_worktree::WorktreeError::StorageFailure {
            path: root.clone(),
            detail: "managed worktree root has no Exomonad family".into(),
        }
    })?;
    let relative = root
        .strip_prefix(family)
        .map_err(|error| exomonad_worktree::WorktreeError::StorageFailure {
            path: root.clone(),
            detail: error.to_string(),
        })?
        .to_path_buf();
    let recorded_family = run_directory
        .path()
        .parent()
        .filter(|runs| runs.file_name().is_some_and(|name| name == "runs"))
        .and_then(Path::parent)
        .filter(|root| root.file_name().is_some_and(|name| name == "exomonad"));
    let family = if recorded_family.is_some() {
        // Recorded run families were established by their launch owner.
        tidepool_atomic_write::DirectoryAnchor::open_existing(family)
    } else {
        crate::exomonad::durable_state_directory()
            .and_then(|state| state.child("exomonad").map_err(std::io::Error::from))
            .map_err(|error| tidepool_atomic_write::WriteError {
                path: family.to_path_buf(),
                source: error,
            })
    }
    .map_err(|error| exomonad_worktree::WorktreeError::StorageFailure {
        path: error.path,
        detail: error.source.to_string(),
    })?;
    let directory = family.child(&relative).map_err(|error| {
        exomonad_worktree::WorktreeError::StorageFailure {
            path: error.path,
            detail: error.source.to_string(),
        }
    })?;
    let (worktrees, bindings) = actor_worktree_resources_with_git(&directory, workspace, git)?;
    Ok((worktrees, bindings, directory))
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
    directory: &tidepool_atomic_write::DirectoryAnchor,
    workspace: &Path,
) -> Result<(WorktreeManager, BindingTable), exomonad_worktree::WorktreeError> {
    let git = admitted_workspace_git(workspace)?;
    actor_worktree_resources_with_git(directory, workspace, git)
}

fn admitted_workspace_git(workspace: &Path) -> Result<GitCli, exomonad_worktree::WorktreeError> {
    let git = GitCli::new();
    // Every WorktreeManager over a source repository admits Git ownership and
    // its local runtime exclusions before constructing registry resources.
    git.ensure_exomonad_local_exclude(workspace)?;
    Ok(git)
}

fn actor_worktree_resources_with_git(
    directory: &tidepool_atomic_write::DirectoryAnchor,
    workspace: &Path,
    git: GitCli,
) -> Result<(WorktreeManager, BindingTable), exomonad_worktree::WorktreeError> {
    let root = directory.path();
    let registry = WorktreeRegistry::open(directory, "registry")?;
    let worktree_root = root.join("worktrees");
    // The root's own allocation directory exists before ANY launch: a mount
    // boundary canonicalizes each writable root it is given, and the root's
    // namespace is fixed at launch, so a directory created later would be
    // unreachable to the process that needs to build in it.
    for relative in [
        PathBuf::from("worktrees"),
        Path::new("worktrees").join(WorktreeManager::ROOT_ALLOCATION_DIR),
    ] {
        directory.create_dir_all(&relative).map_err(|error| {
            exomonad_worktree::WorktreeError::StorageFailure {
                path: error.path,
                detail: error.source.to_string(),
            }
        })?;
    }
    Ok((
        WorktreeManager::new(git, registry, worktree_root, workspace),
        BindingTable::open_with_timeout(directory, "bindings", Duration::from_secs(10))?,
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

struct CompiledExomonadDriver {
    preamble: String,
    include: Vec<PathBuf>,
    toolset_support: Vec<PathBuf>,
    compiled: Arc<tidepool_runtime::session::CompiledTurn>,
    prepared: Arc<tidepool_runtime::session::PreparedSourceEntry>,
}

/// Explicit preparation boundary used before invocation admission.
pub(crate) fn run_compiler_preparation<T: 'static>(
    action: impl FnOnce(
        &mut dyn FnMut(tidepool_runtime::CompilerTransactionClose),
    ) -> Result<T, Box<dyn std::error::Error>>,
) -> Result<T, Box<dyn std::error::Error>> {
    let mut owner = exomonad_actor::CompilerPreparationOwner::new();
    let outcome = owner.run(action);
    if !outcome.cleanup.observation().is_confirmed() {
        return Err(Box::new(PreparationCleanupUnconfirmed { outcome }));
    }
    outcome.action?
}

struct PreparationCleanupUnconfirmed<T> {
    outcome: exomonad_actor::CompilerPreparationOutcome<
        Result<Result<T, Box<dyn std::error::Error>>, exomonad_actor::ResidentActorWorkbenchError>,
    >,
}
impl<T> fmt::Debug for PreparationCleanupUnconfirmed<T> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("PreparationCleanupUnconfirmed")
            .field("cleanup", &self.outcome.cleanup.observation())
            .finish()
    }
}
impl<T> fmt::Display for PreparationCleanupUnconfirmed<T> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "compiler preparation cleanup is unconfirmed: {:?}",
            self.outcome.cleanup.observation()
        )
    }
}
impl<T> std::error::Error for PreparationCleanupUnconfirmed<T> {}

/// Invoke a source check under the exact current actor cleanup authority.
pub(crate) fn typecheck_candidate_revision_owned(
    inputs: &crate::exomonad::workspace::FrozenWorkspace,
    run_root: &Path,
    haskell_root: &Path,
    candidate_paths: &[PathBuf],
    replaces_run_layer: bool,
    also_check: &[String],
) -> Result<(), Box<dyn std::error::Error>> {
    let owner = exomonad_actor::ActorCompilerCloseOwner::current()?;
    let outcome = owner.run(|settlement| {
        typecheck_candidate_revision(
            inputs,
            run_root,
            haskell_root,
            candidate_paths,
            replaces_run_layer,
            also_check,
            settlement,
        )
    })?;
    if matches!(
        &outcome.close,
        tidepool_runtime::CompilerTransactionClose::Unconfirmed(_)
    ) {
        return Err(Box::new(ActorCompilerCheckUnconfirmed { outcome }));
    }
    outcome.action
}
struct ActorCompilerCheckUnconfirmed {
    outcome: tidepool_runtime::CompilerTransactionOutcome<Result<(), Box<dyn std::error::Error>>>,
}
impl fmt::Debug for ActorCompilerCheckUnconfirmed {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ActorCompilerCheckUnconfirmed")
            .field("close", &self.outcome.close)
            .finish()
    }
}
impl fmt::Display for ActorCompilerCheckUnconfirmed {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "source check compiler close is unconfirmed: {:?}",
            self.outcome.close
        )
    }
}
impl std::error::Error for ActorCompilerCheckUnconfirmed {}

/// Typecheck the frozen workspace without producing or discarding native code.
pub(crate) fn validate_workspace_program(
    inputs: &crate::exomonad::workspace::FrozenWorkspace,
    run_root: &Path,
    settlement: &mut dyn FnMut(tidepool_runtime::CompilerTransactionClose),
) -> Result<(), Box<dyn std::error::Error>> {
    typecheck_candidate_revision(
        inputs,
        run_root,
        &inputs.runtime_actors(),
        &[],
        false,
        &[],
        settlement,
    )
}

/// Typecheck the selected spec with the catalog-issued root effect list. GHC expands aliases and checks the selected entry's actual
/// type, so this also covers requirements that are not spelled in its
/// signature. This is an authored-row check; runtime handler availability is
/// resolved later when an actor's workbench is installed.
pub(crate) fn spec_effect_preflight(
    workspace: &crate::exomonad::workspace::FrozenWorkspace,
    run_root: &Path,
    settlement: &mut dyn FnMut(tidepool_runtime::CompilerTransactionClose),
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
        workbench_preamble: mut preamble,
        include,
        ..
    } = driver_sources(
        workspace.runtime_actors().as_path(),
        Some(workspace),
        run_root,
        None,
    )?;
    preamble = insert_preamble_imports(&preamble, "qualified Tidepool.Effects.Core");
    preamble = insert_preamble_imports(&preamble, "qualified Tidepool.Agent.Contract");
    preamble = insert_preamble_imports(&preamble, "Tidepool.Actors.Exomonad");
    preamble = insert_preamble_imports(&preamble, &format!("qualified {module}"));
    let mut failures = Vec::new();
    {
        let capabilities = admitted_capabilities(Some(workspace)).map_err(runtime_error)?;
        let installation =
            exomonad_actor::agent_spec::installation_expression(entry, capabilities.effect_keys());
        let dispatcher_effects = installation.dispatcher_effect_row();
        let templates = resident_workbench_templates(
            &preamble,
            dispatcher_effects.expression(),
            &dispatcher_effects
                .source_imports(&tidepool_runtime::session::SourceImports::default())
                .template_text(),
        );
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
            settlement,
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
                    "root installer cannot install spec {entry}:\n{detail}"
                ));
            }
            Err(error) => {
                return Err(runtime_error(format!(
                    "could not establish whether the root can install spec {entry}: {error}"
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
    settlement: &mut dyn FnMut(tidepool_runtime::CompilerTransactionClose),
) -> Result<(), Box<dyn std::error::Error>> {
    let DriverSources {
        workbench_preamble: mut preamble,
        include,
        ..
    } = driver_sources(
        haskell_root,
        Some(inputs),
        run_root,
        Some(CandidateSources {
            include: candidate,
            replaces_run_layer,
            extra_modules,
        }),
    )?;
    // Workspace validation checks selected policy modules independently of
    // their exposure in the public notebook's configured module imports.
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
        settlement,
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

/// Fixed bootstrap code and the full actor workbench share declared sources.
struct DriverSources {
    bootstrap_preamble: String,
    workbench_preamble: String,
    include: Vec<PathBuf>,
    toolset_support: Vec<PathBuf>,
}

enum DriverCompilePurpose {
    Bootstrap,
}

fn driver_sources(
    haskell_root: &Path,
    inputs: Option<&crate::exomonad::workspace::FrozenWorkspace>,
    run_root: &Path,
    candidate: Option<CandidateSources<'_>>,
) -> Result<DriverSources, Box<dyn std::error::Error>> {
    let declarations = exomonad_effect_declarations();
    let effects = tidepool_mcp::ensure_effects_module(&declarations)?;
    let orchestration = match inputs {
        Some(inputs) => {
            let retained = inputs.runtime_orchestration();
            if tidepool_toolchain::cache::source_root_manifest(&retained)?
                != tidepool_toolchain::cache::source_root_manifest(&effects.orchestration)?
            {
                return Err("frozen orchestration source differs from the host composition".into());
            }
            retained
        }
        None => effects.orchestration.clone(),
    };
    let deployment_roots = match inputs {
        Some(inputs) => inputs.runtime_catalog_roots(),
        None => tidepool_toolchain::toolchain::configured_module_source_selection()?
            .map(|selection| selection.include_roots()),
    };
    let mut include = if let Some(mut roots) = deployment_roots {
        if roots.first() != Some(&effects.core) {
            return Err("frozen stable effect source selection changed".into());
        }
        roots.push(orchestration);
        roots
    } else {
        let mut roots = vec![effects.core.clone(), orchestration];
        roots.push(
            inputs
                .map(|inputs| inputs.runtime_actors())
                .unwrap_or_else(|| haskell_root.to_path_buf()),
        );
        roots.push(match inputs {
            Some(inputs) => inputs.runtime_stdlib(),
            None => tidepool::haskell_sources::ensure_embedded_stdlib()?,
        });
        roots
    };
    let mut preamble = insert_preamble_imports(
        &tidepool_mcp::build_notebook_preamble_with_companions_hiding(
            &declarations,
            false,
            tidepool_mcp::CompanionImports::Omit,
            EXOMONAD_REPLACED_EFFECT_NAMES,
        ),
        DRIVER_MODULE,
    );
    let bootstrap_preamble = preamble.clone();
    let toolset_support = include.clone();
    if let Some(inputs) = inputs {
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
        // A live revision is the complete selected workspace source. Keeping
        // the startup capture underneath it would resurrect deleted modules.
        include.push(inputs.workspace_resources());
        for module in inputs.import_modules() {
            preamble = insert_preamble_imports(&preamble, module);
        }
        for module in candidate.iter().flat_map(|c| c.extra_modules.iter()) {
            preamble = insert_preamble_imports(&preamble, module);
        }
    }
    Ok(DriverSources {
        bootstrap_preamble,
        workbench_preamble: preamble,
        include,
        toolset_support,
    })
}

#[tracing::instrument(
    target = "tidepool::actor_host::startup",
    name = "compile_driver",
    skip_all
)]
fn compile_driver(
    haskell_root: &Path,
    inputs: Option<&crate::exomonad::workspace::FrozenWorkspace>,
    run_root: &Path,
    candidate: Option<CandidateSources<'_>>,
    purpose: DriverCompilePurpose,
    settlement: &mut dyn FnMut(tidepool_runtime::CompilerTransactionClose),
) -> Result<CompiledExomonadDriver, Box<dyn std::error::Error>> {
    let DriverSources {
        bootstrap_preamble,
        workbench_preamble,
        include,
        toolset_support,
    } = driver_sources(haskell_root, inputs, run_root, candidate)?;
    let DriverCompilePurpose::Bootstrap = purpose;
    let preamble = &bootstrap_preamble;
    let templates = resident_workbench_templates(preamble, DRIVER_EFFECTS, "");
    let include_refs: Vec<_> = include.iter().map(PathBuf::as_path).collect();
    let session_root = run_root.join("haskell-session");
    std::fs::create_dir_all(&session_root)?;
    let compiled = if let Some(path) = std::env::var_os("TIDEPOOL_PREPARED_ROOT_ENTRY") {
        let configuration =
            tidepool_toolchain::toolchain::CompilerDeploymentConfiguration::from_env()?;
        let tidepool_toolchain::toolchain::CompilerDeploymentConfiguration::Configured(authority) =
            configuration
        else {
            return Err(runtime_error(
                "prepared root entry requires configured compiler deployment authority",
            ));
        };
        let selection = tidepool_toolchain::toolchain::configured_module_source_selection()?
            .ok_or_else(|| {
                runtime_error("prepared root entry requires the retained native source selection")
            })?;
        let entry = tidepool_toolchain::artifacts::load_production_entry(
            &PathBuf::from(path),
            &authority,
            &selection,
        )?;
        tidepool_runtime::session::CompiledTurn::from_production_entry(&entry)?
    } else {
        match run_turn(
            HaskellTurnRequest {
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
            },
            settlement,
        )
        .map_err(render_root_compile_failure)?
        {
            TurnResult::Expr { compiled, .. } => compiled,
            other => {
                return Err(runtime_error(format!(
                    "root interactive driver is not an expression: {other:?}"
                )))
            }
        }
    };
    let compiled = Arc::new(compiled);
    let registry = inputs
        .and_then(|inputs| inputs.prepared_toolset.as_ref())
        .map(exomonad_actor::PreparedSourceToolset::image_registry)
        .unwrap_or_else(|| Arc::new(tidepool_runtime::session::ImageRegistry::new()));
    let prepared = Arc::new(tidepool_runtime::session::PreparedSourceEntry::prepare(
        Arc::clone(&compiled),
        registry,
    )?);

    Ok(CompiledExomonadDriver {
        preamble: workbench_preamble,
        include,
        toolset_support,
        compiled,
        prepared,
    })
}

/// [`compile_root`]'s output: the workbench source template, the root's retained
/// executable entry, its compiled driver turn (reused as-is by a factory-built
/// child machine — see the composite facade test), and the composition
/// root's one [`exomonad_actor::ChildSessionFactory`].
struct CompiledRoot {
    source: ActorWorkbenchSource,
    root: ExomonadRoot,
    program: Arc<tidepool_runtime::session::CompiledTurn>,
    child_session_factory:
        exomonad_actor::ChildSessionFactory<ExomonadHandlerStack, CapturedOutput>,
    image_registry: Arc<tidepool_runtime::session::ImageRegistry>,
    recovery_store: Arc<tidepool_runtime::session::RunRecoveryStore>,
}

#[tracing::instrument(
    target = "tidepool::actor_host::startup",
    name = "compile_root",
    skip_all,
    fields(run_root = %run_directory.path().display(), actor_path = %root_declaration_recovery::root_path())
)]
fn compile_root(
    config: &ActorHostConfig,
    run_directory: &tidepool_atomic_write::DirectoryAnchor,
    worktree_directory: &tidepool_atomic_write::DirectoryAnchor,
    worktrees: WorktreeManager,
    worktree_authority: ActorWorktreeAuthority,
    web: tidepool_handlers::WebHandler,
    source: Option<&Arc<crate::exomonad::source::ExomonadSourceReload>>,
    host_incarnation: Arc<HostIncarnationLease>,
    run_journal_mode: JournalOpenMode,
    embedded_startup: Option<embedded_recovery::ObservedStartupRecovery<'_>>,
    settlement: &mut dyn FnMut(tidepool_runtime::CompilerTransactionClose),
) -> Result<CompiledRoot, Box<dyn std::error::Error>> {
    let state_root = config.workspace.join(".exomonad/state");
    std::fs::create_dir_all(&state_root)?;
    let child_workspace_root = config.workspace.clone();
    let run_root = run_directory.path();
    let CompiledExomonadDriver {
        preamble,
        include,
        toolset_support,
        compiled,
        prepared,
    } = compile_driver(
        &config.haskell_root,
        config.workspace_inputs.as_ref(),
        run_root,
        None,
        DriverCompilePurpose::Bootstrap,
        settlement,
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
    let recovery_store = root_declaration_recovery::attach(
        &mut library,
        run_root,
        Arc::clone(&host_incarnation),
        settlement,
    )?;
    if let Some(startup) = embedded_startup {
        startup
            .validate_manifest(
                &library,
                run_root,
                active_source_identity(run_root, config.workspace_inputs.is_some())?.as_deref(),
            )
            .map_err(|error| crate::exomonad::boot_stage_refusal("startup manifest", error))?;
    }
    let event_registry = WorktreeRegistry::open(worktree_directory, "registry")?;
    let event_journal = EventJournal::open(run_directory, "repo-events.jsonl")?;
    let event_handler = RepoEventHandler::with_registry_namespace(
        WorktreeMonitor::new(GitCli::new(), event_journal),
        event_registry,
        EventConfig::default(),
        runtime_namespace(run_root),
    );
    let worktree_handler =
        ActorWorktreeHandler::new(WorktreeHandler::from_manager(worktrees), worktree_authority);
    let journal_path = crate::exomonad::exomonad_journal_path(&config.workspace, &config.run_id);
    if let Some(parent) = journal_path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let journal = if run_journal_mode == JournalOpenMode::Create {
        tidepool_handlers::JournalHandler::new(tidepool_handlers::SegmentPath::create_exclusive(
            journal_path,
        )?)?
    } else {
        tidepool_handlers::JournalHandler::resuming(
            tidepool_handlers::SegmentPath::open_existing(journal_path)
                .map_err(|error| crate::exomonad::boot_stage_refusal("run journal", error))?,
            host_incarnation.incarnation().0,
        )
        .map_err(|error| crate::exomonad::boot_stage_refusal("run journal header", error))?
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
    let child_journal = journal.clone();
    let child_worktree_handler = worktree_handler.clone();
    let child_web = web.clone();
    let child_run_root = run_root.to_path_buf();
    let child_run_lease = Arc::clone(&host_incarnation);
    let child_recovery_store = Arc::clone(&recovery_store);
    // This run's one shared image cache: installed on the forest
    // (`ResidentForest::with_image_registry`) so it is applied to every
    // session's engine, root and child alike, including each session's
    // bootstrap install (`PersistentSession::set_image_registry` holds it
    // for the first turn).
    let image_registry = Arc::clone(prepared.image_registry());
    let display_owners = Arc::new(tidepool_runtime::session::ActivationDisplayOwners::default());
    let child_display_owners = Arc::clone(&display_owners);
    let child_prepared_driver = Arc::clone(&prepared);
    let child_session_factory: exomonad_actor::ChildSessionFactory<
        ExomonadHandlerStack,
        CapturedOutput,
    > = Arc::new(move |child_session_id, source_layer, settlement| {
        // The source owner retains native images between fresh child installs;
        // the registry itself continues to hold only weak references.
        let _native_driver = &child_prepared_driver;
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
            Arc::clone(&child_recovery_store),
            Arc::clone(&child_run_lease),
            settlement,
        )
        .map_err(|error| format!("child session declaration recovery: {error}"))?;
        let child_event_handler =
            RepoEventHandler::with_source(Box::new(InertObservationSource), EventConfig::default());
        let child_worktree_handler = child_worktree_handler.clone();
        let mut machine = ResidentSession::unbootstrapped(
            host_handlers(
                &child_workspace_root,
                child_journal.clone(),
                child_event_handler,
                child_worktree_handler,
                child_web.clone(),
            ),
            CapturedOutput::new(),
            DEFAULT_NURSERY_SIZE,
            Some(library),
        );
        machine.set_activation_display_owners(Arc::clone(&child_display_owners));
        machine.set_catalog_selection(
            child_workspace_inputs
                .as_ref()
                .map(|inputs| inputs.catalog_selection.clone())
                .unwrap_or_default(),
        );
        Ok(Box::new(machine))
    });
    let mut machine = ResidentSession::unbootstrapped(
        host_handlers(
            &config.workspace,
            journal,
            event_handler,
            worktree_handler,
            web,
        ),
        CapturedOutput::new(),
        DEFAULT_NURSERY_SIZE,
        Some(library),
    );
    machine.set_activation_display_owners(display_owners);
    machine.set_catalog_selection(
        config
            .workspace_inputs
            .as_ref()
            .map(|inputs| inputs.catalog_selection.clone())
            .unwrap_or_default(),
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
    let entry =
        tracing::info_span!(target: "tidepool::actor_host::startup", "prepare_startup_entry")
            .in_scope(|| machine.prepare_startup_entry(compiled.code()))?;
    machine.seal_recovery_initialization_scope(lexical_scope)?;
    let capabilities =
        admitted_capabilities(config.workspace_inputs.as_ref()).map_err(runtime_error)?;
    let root_model = capabilities
        .conversation_permission()
        .map(|permission| permission.model().model().clone())
        .unwrap_or_else(|| config.model.clone());
    if capabilities
        .conversation_permission()
        .is_some_and(|permission| !permission.permits(permission.model(), config.reasoning))
    {
        return Err(runtime_error(
            "root reasoning effort differs from its admitted conversation permission",
        ));
    }
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
    .with_model(Some(root_model))
    .with_reasoning(Some(config.reasoning))
    .with_capabilities(capabilities);
    if let Some(layers) = source {
        descriptor = descriptor.with_source_layer(
            exomonad_actor::ActorSourceLayers::layer_include_for(layers.as_ref(), "run")
                .map_err(std::io::Error::other)?,
        );
    }
    descriptor = prompt_composition::install(descriptor, config, source.cloned());
    descriptor
        .validate_conversation_selection()
        .map_err(runtime_error)?;
    let source = host_workbench_source(config, preamble, include, toolset_support)?;
    Ok(CompiledRoot {
        source,
        root: ResidentActorRoot::pending(descriptor, machine, entry),
        program: compiled,
        child_session_factory,
        image_registry,
        recovery_store,
    })
}

fn host_workbench_source(
    config: &ActorHostConfig,
    preamble: String,
    include: Vec<PathBuf>,
    toolset_support: Vec<PathBuf>,
) -> Result<ActorWorkbenchSource, Box<dyn std::error::Error>> {
    // A run that does not supply `Jev.Operators` gets a workbench without `J`,
    // rather than a compile failure over a module nothing on its search path
    // defines. The same answer tells the agent so in its instructions.
    let jev = config.jev_surface() == prompt_catalog::JevSurface::Installed;
    let mut workbench = ActorWorkbenchSource::new(preamble, include)
        .with_prepared_toolset(
            config
                .workspace_inputs
                .as_ref()
                .and_then(|inputs| inputs.prepared_toolset.clone()),
        )
        .with_toolset_support_roots(toolset_support)
        .with_installed_effect_support(host_context_support())
        .with_tool_surface_validator(Arc::new(embedded_policy::validate_tools))
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
    Ok(workbench
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
        ))
}

/// Prepare the selected rows with the live host's actual handler and native
/// interpreter composition. The temporary Store and idle machine admit no
/// actors; completed entries retain only the independent source deployment.
#[tracing::instrument(
    target = "tidepool::actor_host::startup",
    name = "workspace_toolsets_prepare",
    skip_all,
    fields(workspace = %workspace.display(), deployment = %directory.path().display())
)]
pub(crate) async fn prepare_workspace_toolsets(
    workspace: &Path,
    directory: &tidepool_atomic_write::DirectoryAnchor,
    inputs: crate::exomonad::workspace::FrozenWorkspace,
    source: Arc<crate::exomonad::source::ExomonadSourceReload>,
) -> Result<
    (
        Vec<crate::exomonad::workspace::PreparedToolsetCoverage>,
        exomonad_actor::PreparedSourceToolset,
    ),
    Box<dyn std::error::Error>,
> {
    let authored = inputs.config()?;
    let requested_effects = admitted_capabilities(Some(&inputs))
        .map_err(runtime_error)?
        .effect_keys()
        .to_vec();
    let settings = authored
        .launch
        .embedded
        .as_ref()
        .ok_or("native preparation requires [launch.embedded]")?
        .clone();
    settings.validate()?;
    let temporary = Arc::new(tempfile::tempdir()?);
    let runtime_directory =
        tidepool_atomic_write::DirectoryAnchor::open_existing(temporary.path())?;
    let config = ActorHostConfig {
        run_id: "preparation".into(),
        systemd_slice: Some(authored.launch.systemd_slice),
        source_exclude: authored.launch.source_exclude,
        source_import: authored.launch.source,
        command_resources: None,
        exomonad_executable: std::env::current_exe()?,
        workspace_inputs: Some(inputs.clone()),
        workspace: workspace.to_owned(),
        haskell_root: inputs.runtime_actors(),
        run_directory: runtime_directory.clone(),
        root_binding_path: runtime_directory.path().join("root-binding.json"),
        embedded: Some(settings.clone()),
        inference_auth: None,
        tmux_session: String::new(),
        model: authored.defaults.model,
        reasoning: authored.defaults.reasoning,
        pane_environment: Default::default(),
        jev: None,
    };
    let DriverSources {
        workbench_preamble,
        include,
        toolset_support,
        ..
    } = driver_sources(&config.haskell_root, Some(&inputs), directory.path(), None)?;
    let workbench = host_workbench_source(&config, workbench_preamble, include, toolset_support)?;
    let (worktrees, bindings) = actor_worktree_resources_at(&runtime_directory, workspace)?;
    let authority = ActorWorktreeAuthority::new(
        runtime_namespace(runtime_directory.path()),
        Arc::new(Mutex::new(bindings)),
    );
    let events = RepoEventHandler::with_registry_namespace(
        WorktreeMonitor::new(
            GitCli::new(),
            EventJournal::open(&runtime_directory, "repo-events.jsonl")?,
        ),
        WorktreeRegistry::open(&runtime_directory, "registry")?,
        EventConfig::default(),
        runtime_namespace(runtime_directory.path()),
    );
    let handlers = host_handlers(
        runtime_directory.path(),
        tidepool_handlers::JournalHandler::new(tidepool_handlers::SegmentPath::create_exclusive(
            runtime_directory.path().join("journal.jsonl"),
        )?)?,
        events,
        ActorWorktreeHandler::new(WorktreeHandler::from_manager(worktrees), authority),
        tidepool_handlers::WebHandler::default(),
    );
    let mut supported = host_context_support();
    supported.extend(exomonad_tool::InstalledEffectSupport::installed_effect_support(&handlers));
    let runtime = embedded_harness::EmbeddedHarnessRuntime::open(
        runtime_directory.path(),
        settings.concurrent_jobs,
    )?;
    runtime.configure_inference(run_auth::RunAuth::from_settings(
        &settings,
        runtime.store(),
        runtime.run_identity().to_owned(),
    )?)?;
    let recovery = exomonad_actor::ActorRecoveryJournal::open(
        &runtime_directory,
        "actor-lifecycle.v2.jsonl",
        temporary.clone(),
    )
    .await?;
    runtime
        .configure_context_models(&config)
        .map_err(runtime_error)?;
    let source_layers: exomonad_actor::ActorSourceLayerResolver = source.clone();
    let jev = jev_backend(&config);
    let models = cell_model::admitted_runtime_factory(&runtime, &settings, &config);
    let conversation = embedded_reflect::run_conversation_reader(runtime.store(), recovery);
    for key in exomonad_actor::installed_intrinsic_effect_support(
        Some(&source_layers),
        &jev,
        Some(&models),
        Some(&conversation),
    ) {
        if !supported.contains(&key) {
            supported.push(key);
        }
    }
    let frozen = exomonad_actor::ActorSourceLayers::freeze_toolset_layer(
        source.as_ref(),
        tidepool_repr::PrincipalId::SYSTEM,
    )
    .map_err(std::io::Error::other)?;
    let registry = Arc::new(tidepool_runtime::session::ImageRegistry::new());
    // All explicitly requested rows are required work. Runtime proactive
    // warming continues to use Preparation urgency after its root is ready.
    let mut compiler_owner = exomonad_actor::CompilerPreparationOwner::new();
    let outcome = compiler_owner
        .scope_with_client(|client| async move {
            let workbench = workbench.bind_preparation(client);
            let ready = workbench
                .prepare_source_toolset(
                    tidepool_toolchain::artifacts::CompileWorkload::Foreground,
                    frozen.clone(),
                    &requested_effects,
                    &supported,
                    Arc::clone(&registry),
                )
                .await?;
            let program = ready.selection().ok_or_else(|| {
                exomonad_actor::ResidentActorWorkbenchError::ActorProtocol(
                    "source preparation produced no retained original selection".into(),
                )
            })?;
            let selected = vec![crate::exomonad::workspace::PreparedToolsetCoverage {
                requested_effects,
                effective_effects: ready.effects().to_vec(),
                program,
            }];
            Ok::<_, exomonad_actor::ResidentActorWorkbenchError>((selected, ready))
        })
        .await;
    if !outcome.cleanup.observation().is_confirmed() {
        return Err(Box::new(SourcePreparationCleanupUnconfirmed {
            action: outcome.action,
            cleanup: outcome.cleanup,
        }));
    }
    Ok(outcome.action?)
}

struct SourcePreparationCleanupUnconfirmed {
    action: Result<
        (
            Vec<crate::exomonad::workspace::PreparedToolsetCoverage>,
            exomonad_actor::PreparedSourceToolset,
        ),
        exomonad_actor::ResidentActorWorkbenchError,
    >,
    cleanup: exomonad_actor::CompilerPreparationCleanup,
}

impl fmt::Debug for SourcePreparationCleanupUnconfirmed {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("SourcePreparationCleanupUnconfirmed")
            .field("action", &self.action)
            .field("cleanup", &self.cleanup.observation())
            .finish()
    }
}

impl fmt::Display for SourcePreparationCleanupUnconfirmed {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "source preparation compiler cleanup is unconfirmed: {:?}",
            self.cleanup.observation()
        )?;
        if let Err(error) = &self.action {
            write!(formatter, "; {error}")?;
        }
        Ok(())
    }
}

impl std::error::Error for SourcePreparationCleanupUnconfirmed {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        self.action.as_ref().err().map(|error| error as _)
    }
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
    Box::new(RootCompilationFailure { failure, detail })
}

#[derive(Debug)]
struct RootCompilationFailure {
    failure: tidepool_runtime::session::TurnFailure,
    detail: String,
}
impl fmt::Display for RootCompilationFailure {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "root interactive driver compilation failed:\n{}",
            self.detail
        )
    }
}
impl std::error::Error for RootCompilationFailure {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(&self.failure)
    }
}

fn embedded_resource_release(
    error: Option<&embedded_service::EmbeddedDriverError>,
) -> exomonad_actor::ResourceRelease {
    match error.filter(|error| error.cleanup_unconfirmed()) {
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
    let owners = owners.lock();
    let Some(owner) = owners.get(&actor) else {
        return Some(exomonad_actor::ResourceRelease::Released);
    };
    if owner.embedded.live {
        return None;
    }
    Some(embedded_resource_release(
        owner.embedded.cleanup_failure.as_ref(),
    ))
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

#[derive(Debug, Default, PartialEq, Eq)]
struct EmbeddedShutdownOutcome {
    driver_failure: Option<String>,
    cleanup_failure: Option<String>,
}

async fn drain_embedded_shutdown<L: Send + 'static>(
    tasks: &mut JoinSet<(
        ActorRef,
        L,
        Result<(), embedded_service::EmbeddedDriverError>,
    )>,
    grace: Duration,
    application_owners: &InteractiveOwners,
    release_waiters: &mut HashMap<ActorRef, Vec<Arc<exomonad_actor::ReleaseAwait>>>,
) -> EmbeddedShutdownOutcome {
    let mut result = EmbeddedShutdownOutcome::default();
    match tokio::time::timeout(grace, async {
        while let Some(joined) = tasks.join_next_with_id().await {
            match joined {
                Ok((task_id, (actor, _local_actor, outcome))) => {
                    match application_supervisor::settle_embedded_completion(
                        actor, task_id, outcome, application_owners, release_waiters,
                    ) {
                        Ok(application_supervisor::EmbeddedTaskCompletion::Completed) => {}
                        Ok(application_supervisor::EmbeddedTaskCompletion::ExecutionFailed(detail))
                        | Ok(application_supervisor::EmbeddedTaskCompletion::RecoveryRequired(detail)) => {
                            tracing::warn!(?actor, %detail, "embedded Engine stopped with an error during host shutdown");
                            result.driver_failure.get_or_insert_with(|| format!("embedded Engine {actor:?}: {detail}"));
                        }
                        Ok(application_supervisor::EmbeddedTaskCompletion::CleanupFailed(detail)) => {
                            result.cleanup_failure.get_or_insert_with(|| format!("embedded Engine {actor:?}: {detail}"));
                        }
                        Err(error) => { result.cleanup_failure.get_or_insert(error); }
                    }
                }
                Err(error) => {
                    let actor = embedded_actor_for_task(application_owners, error.id());
                    result.cleanup_failure.get_or_insert_with(|| match actor {
                        Some(actor) => format!("embedded Engine task for {actor:?}: {error}"),
                        None => format!("unattributed embedded Engine task: {error}"),
                    });
                }
            }
        }
    }).await {
        Ok(()) => result,
        Err(_) => {
            tasks.abort_all();
            let timeout = "embedded Engine cleanup timed out; abort requested, cleanup unconfirmed";
            result.cleanup_failure = Some(match result.cleanup_failure {
                Some(failure) => format!("{failure}; {timeout}"),
                None => timeout.into(),
            });
            result
        }
    }
}

fn current_time_ms() -> i64 {
    let millis = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis();
    i64::try_from(millis).unwrap_or(i64::MAX)
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

    host_resources: Option<Arc<exomonad_node::command_resources::CommandResourceClient>>,
) {
    let backend = (|| {
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
            .map(|effect| match effect {
                exomonad_tool::ToolEffectKey::Actor(actor) => actor.haskell_row_entry(),
                effect => effect.haskell_name(),
            })
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
    run_directory: &tidepool_atomic_write::DirectoryAnchor,
    actor: ActorRef,
    actor_path: harness::model::AgentPath,
) -> Result<embedded_harness::EmbeddedActorBinding, String> {
    let run_root = run_directory.path();
    let actor_root = run_directory
        .child(format!("{}-{}", actor.id.0, actor.incarnation.0))
        .map_err(|error| error.to_string())?;
    let inbox = ActorInbox::open(
        &actor_root,
        "embedded-notifications.jsonl",
        "embedded-notifications.cursor",
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
    Ok(embedded_harness::EmbeddedActorBinding::pending(
        identity,
        Arc::new(inbox),
        inbox_key,
    ))
}

async fn wait_for_shutdown(mut shutdown: watch::Receiver<ApplicationShutdown>) {
    if *shutdown.borrow() != ApplicationShutdown::Running {
        return;
    }
    while shutdown.changed().await.is_ok() {
        if *shutdown.borrow() != ApplicationShutdown::Running {
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

fn developer_instructions_selected(
    capabilities: &exomonad_actor::ActorCapabilities,
    inputs: Option<&crate::exomonad::workspace::FrozenWorkspace>,
    instructions: Option<&str>,
) -> String {
    let body = instructions
        .or_else(|| inputs.and_then(|inputs| inputs.prompts.get("agent").map(String::as_str)))
        .unwrap_or_else(|| PromptId::Agent.body());
    append_capabilities(body.to_owned(), capabilities)
}

fn append_capabilities(
    mut instructions: String,
    capabilities: &exomonad_actor::ActorCapabilities,
) -> String {
    let descendants = capabilities.descendants();
    instructions.push_str(&format!(
        "\n\nAvailable effects: {}; descendant_depth={}; active_children={}. Concrete resource grants authorize resource access independently of effect membership.\n",
        capabilities.haskell_effects_type(),
        descendants.maximum_depth,
        exomonad_actor::render_child_budget(descendants.maximum_active_children),
    ));
    instructions
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
    let bound = authority.bound_worktree(actor.into());
    let custody = bound
        .as_ref()
        .map(|id| {
            worktrees.lookup(id).and_then(|handle| {
                handle.ok_or_else(|| {
                    exomonad_worktree::WorktreeError::WorktreeNotRegistered(id.clone())
                })
            })
        })
        .transpose()?
        .map(|handle| handle.cwd().to_owned());
    let access = bound
        .as_ref()
        .and_then(|id| authority.workspace_access(actor.into(), id))
        .unwrap_or(exomonad_worktree::WorkspaceAccess::ReadOnly);
    let git_common_dir = exomonad_worktree::git::inspect::git_common_dir(worktrees.git(), source)?;
    let root_allocations = worktrees.root_allocations();
    let grant = authority.grant(actor.into());
    let root = grant == ActorWorktreeGrant::Repository;
    let writable = writable_repository_roots(
        root,
        access,
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
    /// Whether this actor holds its exact workspace attachment.
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
    workspace_access: exomonad_worktree::WorkspaceAccess,
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
    } else if workspace_access == exomonad_worktree::WorkspaceAccess::ReadWrite {
        worker_worktree.map(Path::to_path_buf).into_iter().collect()
    } else {
        Vec::new()
    };
    if root || workspace_access == exomonad_worktree::WorkspaceAccess::ReadWrite {
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
