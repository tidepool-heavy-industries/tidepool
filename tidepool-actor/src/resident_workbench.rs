//! Actor binding for the shared resident Haskell workbench.
//!
//! This adapter owns no machine and holds no checkout between calls. Each
//! fenced block checks out the actor's registered resident session, installs
//! the exact actor context, compiles and runs one segment on the blocking
//! pool, then restores the machine before the actor loop continues.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use tidepool_bridge::{BridgeError, FromCore, ToCore};
use tidepool_codegen::suspension::RealmId;
use tidepool_effect::dispatch::{request_constructor, DispatchEffect};
use tidepool_eval::Value;
use tidepool_repr::DataConTable;
use tidepool_runtime::session::registry::{CheckoutError, SessionRegistry};
use tidepool_runtime::session::{
    classify_workbench_item, insert_preamble_imports, render_turn_compile_error,
    resident_workbench_templates, run_inspections, run_turn, GhciInputKind, InspectionQuery,
    InspectionRequest, MetaCommandLine, OutputSink, ParsedBlock, ResidentError, ResidentHole,
    ResidentOutcome, ResidentSession, RootCustody, SourceImports, TurnClassification, TurnKind,
    TurnRequest, TurnResult, WorkbenchDiscovery, WorkbenchItem,
};
use tidepool_runtime::{classify_compile, classify_session, CompileError, FailureClass};

use crate::mailbox::{InstalledReceiver, KernelValue, ResidentOutbound, ResidentWaitRequest};
use crate::request_effect::{
    CancellationAcknowledgement, RepliesReq, ReplyAttempt, ReplyPoll, RequestCancellation,
    RequestReservation, RequestSubmission, ResponseAbandonment, ResponseForget, ResponsePoll,
    WatchForget, WatchPoll, WatchRegistration, WatchesReq,
};
use crate::{ActorCompileViewError, ResponseExpectation};

impl ResponseExpectation {
    fn request_preamble(
        &self,
        preamble: &str,
        request: crate::RequestId,
        effects_alias: &str,
    ) -> String {
        let mut preamble = insert_preamble_imports(
            preamble,
            "qualified Tidepool.Agent.Reply.Internal as TidepoolReplies",
        );
        preamble = insert_preamble_imports(&preamble, "qualified Data.Void as TidepoolVoid");
        preamble = insert_preamble_imports(&preamble, "qualified Prelude as TidepoolPrelude");
        preamble.push_str(&format!(
            "\nsessionReply :: TidepoolReplies.Reply ({0})\nsessionReply = TidepoolReplies.Reply (TidepoolReplies.RequestId {1})\n{2}\nrespond = TidepoolReplies.reply sessionReply\n",
            self.expected_type(),
            request.0,
            self.respond_signature(effects_alias),
        ));
        if let Some(progress_type) = &self.progress_type {
            preamble.push_str(&format!(
                "\nreportProgress :: ({progress_type}) -> Eff {effects_alias} ()\nreportProgress value = do\n  outcome <- TidepoolReplies.reportRequestProgress (TidepoolReplies.ProgressSink (TidepoolReplies.RequestId {})) value\n  case outcome of\n    Left failure -> TidepoolPrelude.error (TidepoolPrelude.show failure)\n    Right () -> pure ()\n", request.0,
            ));
        }
        preamble
    }
}

/// Trusted source environment supplied by actor deployment. The canonical
/// `ActorEffects` alias itself lives in the imported Haskell facade; Rust does
/// not reflect or authorize its row entries.
#[derive(Clone)]
pub struct ActorWorkbenchSource {
    preamble: Arc<str>,
    base_include: Arc<[PathBuf]>,
    default_browse_module: Option<Arc<str>>,
    workbench_imports: SourceImports,
}

/// One prepared import environment for evaluation and inspection. Name
/// resolution comes from the exact lexical view before either path builds
/// a compiler request.
struct WorkbenchCompilation {
    preamble: String,
    imports: String,
    include: Vec<PathBuf>,
    injected: Vec<String>,
}

impl ActorWorkbenchSource {
    fn prepare(&self, scope: &crate::ActorCompileView) -> WorkbenchCompilation {
        WorkbenchCompilation {
            preamble: insert_preamble_imports(
                &scope.shadow_preamble(&self.preamble),
                "qualified Tidepool.Inspection as TidepoolInspection",
            ),
            imports: scope.turn_imports(),
            include: scope.include_paths(&self.base_include),
            injected: scope.injected_module_names(),
        }
    }

    #[must_use]
    pub fn new(preamble: impl Into<Arc<str>>, base_include: Vec<PathBuf>) -> Self {
        Self {
            preamble: preamble.into(),
            base_include: base_include.into(),
            default_browse_module: None,
            workbench_imports: SourceImports::new(),
        }
    }

    #[must_use]
    pub fn with_default_browse_module(mut self, module: impl Into<Arc<str>>) -> Self {
        let module = module.into();
        self.workbench_imports.extend_text(module.as_ref());
        self.default_browse_module = Some(module);
        self
    }

    /// Preload the small quasiquoter vocabulary promised by a hosted
    /// workbench. Deployment selects this policy; the generic actor engine
    /// does not inspect input text or depend on an MCP-specific parser.
    #[must_use]
    pub fn with_default_quasiquoters(mut self) -> Self {
        self.workbench_imports
            .extend_text("Tidepool.QQ (fmt, j, patch, uri)");
        self
    }

    /// Additional shared imports must enter declaration compilation as well as
    /// expression templates, so bindings keep the same vocabulary after a fork.
    #[must_use]
    pub fn with_imports(mut self, imports: &str) -> Self {
        self.workbench_imports.extend_text(imports);
        self
    }
}

/// Shared-machine registry shape used by actors. String holes are only the
/// registry's checkout index; obligation-carrying `ResidentHole` values stay
/// inside each running segment.
pub type ActorMachineRegistry<H, O> = SessionRegistry<ResidentSession<H, O>, String>;

/// Shared checkout boundary for every actor machine entry path. Fenced
/// fragments and installed actor programs differ above this layer, but use
/// exactly the same admission and settlement mechanism. Every checkout
/// installs the supplied actor context before invoking its operation, so a
/// child cannot leak its lexical scope, resource realm, or principal into the
/// next parent or sibling entry.
struct ResidentMachineAccess<H, O> {
    machines: Arc<ActorMachineRegistry<H, O>>,
    source: ActorWorkbenchSource,
}

impl<H, O> ResidentMachineAccess<H, O> {
    fn new(machines: Arc<ActorMachineRegistry<H, O>>, source: ActorWorkbenchSource) -> Self {
        Self { machines, source }
    }
}

/// Concrete resident workbench for one typed agent-session obligation.
pub struct ResidentActorWorkbench<H, O> {
    access: ResidentMachineAccess<H, O>,
    response: Option<ResponseExpectation>,
    request: Option<crate::RequestId>,
    type_modules: Arc<[String]>,
    json_input: Option<serde_json::Value>,
}

/// Live execution state for one workbench item that suspended on an actor
/// effect. The continuation and any value it binds remain owned by the
/// actor's resource scope; this value only carries the item-local rendering
/// state needed while the actor interpreter settles nominal effects.
pub(crate) struct ResidentWorkbenchFragment {
    display: WorkbenchDisplay,
    output: Vec<String>,
    presented: Vec<String>,
    warnings: Vec<String>,
}

impl ResidentWorkbenchFragment {
    pub(crate) fn present_command(
        &mut self,
        job: String,
        presentation: tidepool_bridge_effects::CommandPresentation,
        remaining: &mut usize,
    ) -> String {
        if !self.presented.contains(&job) {
            self.presented.push(job);
        }
        if let tidepool_bridge_effects::CommandPresentation::CommandVisible(text) = presentation {
            let text = crate::workbench_display::bounded_output(&text, *remaining);
            *remaining = remaining.saturating_sub(text.len() + 1);
            text
        } else {
            String::new()
        }
    }
}

enum WorkbenchDisplay {
    Binding(Vec<String>),
    Opaque,
    Observation {
        name: String,
        source: ActorWorkbenchSource,
        type_modules: Vec<String>,
    },
}

#[derive(Clone, Copy)]
struct RequestWorkbenchScope<'a> {
    response: Option<&'a ResponseExpectation>,
    request: Option<crate::RequestId>,
    type_modules: &'a [String],
}

pub(crate) enum ResidentWorkbenchStep {
    Committed {
        output: String,
        warnings: Vec<String>,
        installed_bindings: Vec<String>,
    },
    Rejected(String),
    CommandBackgrounded {
        job: String,
        binding: String,
        reason: CommandObservationStop,
    },
    Running {
        fragment: ResidentWorkbenchFragment,
        outcome: Box<ResidentOutcome>,
    },
    Replied {
        request: crate::RequestId,
        result: RootCustody,
    },
    CancellationAcknowledged {
        request: crate::RequestId,
    },
}

/// The one machine-entry component for installed actor program segments.
/// It shares checkout/context installation with the fenced workbench; startup
/// and later mailbox scheduling therefore cannot grow a second dispatcher.
pub struct ResidentActorRunner<H, O> {
    access: ResidentMachineAccess<H, O>,
}

impl<H, O> Clone for ResidentActorRunner<H, O> {
    fn clone(&self) -> Self {
        Self {
            access: ResidentMachineAccess::new(
                Arc::clone(&self.access.machines),
                self.access.source.clone(),
            ),
        }
    }
}

/// A private readiness continuation validated while its actor is still
/// unpublished. Construction proves both the nominal request and owning
/// resource realm; consuming it is the only way the runner enters the
/// installed program.
pub(crate) struct ResidentActorReadiness {
    hole: ResidentHole,
}

pub(crate) struct ResidentActorShutdown {
    continuation: ResidentHole,
    hook: RootCustody,
}

impl ResidentActorShutdown {
    pub(crate) fn into_parts(self) -> (ResidentHole, RootCustody) {
        (self.continuation, self.hook)
    }
}

/// Suspensions admitted while installing the trusted actor entry.
pub(crate) enum ResidentActorStartupStep {
    InstallSource {
        continuation: ResidentHole,
        source: crate::request::sources::SourceBinding,
    },
    InstallShutdown(ResidentActorShutdown),
    Attach(ResidentAgentAttachment),
    Ready(ResidentActorReadiness),
}

pub(crate) struct ResidentAgentAttachment {
    pub(crate) continuation: ResidentHole,
    pub(crate) initial_user_message: Option<String>,
}

pub(crate) struct AgentRosterProjection {
    pub(crate) received: (u64, u64),
    pub(crate) requests: (Vec<crate::RequestId>, Vec<crate::RequestId>),
    pub(crate) actor: crate::ActorRef,
    pub(crate) descriptor: crate::ActorDescriptor,
    pub(crate) bound_worktree: Option<String>,
    pub(crate) terminal: Option<crate::ActorTerminal>,
    pub(crate) runtime: crate::ActorRuntimeObservation,
}

pub(crate) struct AgentInspectionBoundary {
    pub(crate) target: crate::ActorRef,
    pub(crate) continuation: ResidentHole,
}

pub(crate) enum AgentForgetProjection {
    Forgotten,
    Running,
    Retained {
        requests: Vec<crate::RequestId>,
        watches: Vec<crate::WatchId>,
    },
    Unavailable,
}

#[derive(Clone)]
pub(crate) enum AgentStopProjection {
    StoppedNow,
    AlreadyStopped,
    Unavailable,
    Unauthorized,
    Failed(String),
}

#[derive(Clone)]
pub(crate) struct CleanupActorProjection {
    pub actor: crate::ActorRef,
    pub label: String,
    pub terminal: bool,
    pub revision: u64,
}

#[derive(Clone)]
pub(crate) struct CleanupPlanProjection {
    pub group: crate::ForkGroupId,
    pub actors: Vec<CleanupActorProjection>,
    pub pending_responses: Vec<crate::RequestId>,
    pub pending_watches: Vec<crate::WatchId>,
    pub refusal: Option<String>,
}

pub(crate) enum CleanupStepProjection {
    ForgotResponses(Vec<crate::RequestId>),
    ForgotWatches(Vec<crate::WatchId>),
    StoppedActor(crate::ActorRef, AgentStopProjection),
    ForgotActor(crate::ActorRef),
    ActorRetained {
        actor: crate::ActorRef,
        requests: Vec<crate::RequestId>,
        watches: Vec<crate::WatchId>,
    },
    GroupRetired(crate::ForkGroupId),
    Blocked(String),
    StalePlan,
}

pub(crate) struct CleanupReceiptProjection {
    pub plan: CleanupPlanProjection,
    pub steps: Vec<CleanupStepProjection>,
    pub complete: bool,
}

fn usage_observation_value(
    table: &DataConTable,
    sample: Option<&crate::ProviderUsageSample>,
) -> Result<Value, ResidentActorWorkbenchError> {
    let value = sample
        .map(|sample| {
            actor_context_constructor(
                table,
                "ProviderUsageObservation",
                vec![
                    sample.observation_id.to_value(table)?,
                    sample.source_timestamp.to_value(table)?,
                    sample.cached_input_tokens.to_value(table)?,
                    sample.uncached_input_tokens.to_value(table)?,
                ],
            )
        })
        .transpose()?;
    Ok(value.to_value(table)?)
}

fn usage_summary_value(
    table: &DataConTable,
    summary: Option<&tidepool_model::ProviderUsageSummary>,
) -> Result<Value, ResidentActorWorkbenchError> {
    use tidepool_model::{ProviderUsageCompleteness, ProviderUsageScope};
    let value = summary
        .map(|summary| {
            let scope = match &summary.scope {
                ProviderUsageScope::Thread(thread) => {
                    actor_context_constructor(table, "UsageThread", vec![thread.to_value(table)?])?
                }
                ProviderUsageScope::Turn { thread, turn } => actor_context_constructor(
                    table,
                    "UsageTurn",
                    vec![thread.to_value(table)?, turn.to_value(table)?],
                )?,
            };
            actor_context_constructor(
                table,
                "ProviderUsageSummary",
                vec![
                    scope,
                    actor_context_constructor(
                        table,
                        match summary.completeness {
                            ProviderUsageCompleteness::Partial => "UsagePartial",
                            ProviderUsageCompleteness::Complete => "UsageComplete",
                        },
                        Vec::new(),
                    )?,
                    summary.observations.to_value(table)?,
                    summary.usage.cached_input_tokens.to_value(table)?,
                    (summary.usage.input_tokens - summary.usage.cached_input_tokens)
                        .to_value(table)?,
                    summary.usage.output_tokens.to_value(table)?,
                    summary.usage.reasoning_output_tokens.to_value(table)?,
                    summary.usage.total_tokens.to_value(table)?,
                ],
            )
        })
        .transpose()?;
    Ok(value.to_value(table)?)
}

fn agent_roster_value(
    table: &DataConTable,
    entry: AgentRosterProjection,
) -> Result<Value, ResidentActorWorkbenchError> {
    let (health_name, health_fields) =
        match entry.runtime.provider_turn.as_ref().map(|turn| &turn.state) {
            None => ("ProviderUnknown", vec![]),
            Some(tidepool_model::ProviderTurnState::Active) => ("ProviderActive", vec![]),
            Some(tidepool_model::ProviderTurnState::Succeeded) => ("ProviderSucceeded", vec![]),
            Some(tidepool_model::ProviderTurnState::Interrupted) => ("ProviderInterrupted", vec![]),
            Some(tidepool_model::ProviderTurnState::Failed(failure)) => {
                let (name, fields) = match failure {
                    tidepool_model::ProviderFailure::RequestRejected => ("RequestRejected", vec![]),
                    tidepool_model::ProviderFailure::TransportFailed => ("TransportFailed", vec![]),
                    tidepool_model::ProviderFailure::Other(detail) => {
                        ("OtherProviderFailure", vec![detail.to_value(table)?])
                    }
                };
                (
                    "ProviderFailed",
                    vec![actor_context_constructor(table, name, fields)?],
                )
            }
        };
    let health = actor_context_constructor(table, health_name, health_fields)?;
    let disposition = if entry.terminal.is_none() {
        Some(actor_context_constructor(
            table,
            entry
                .runtime
                .disposition(!entry.requests.0.is_empty() || !entry.requests.1.is_empty())
                .constructor_name(),
            vec![],
        )?)
    } else {
        None
    };
    let usage = entry.runtime.latest_provider_usage();
    let supervisor = entry.descriptor.supervisor_parent();
    let creator = entry.descriptor.creator();
    let context_parent = entry.descriptor.context_parent();
    let cache_boundary = usage
        .map(|sample| {
            actor_context_constructor(
                table,
                match sample.cache_boundary {
                    crate::CacheBoundaryReason::Fresh => "CacheFresh",
                    crate::CacheBoundaryReason::ForkedPrefix => "CacheForkedPrefix",
                    crate::CacheBoundaryReason::ReattachedThread => "CacheReattachedThread",
                    crate::CacheBoundaryReason::ProviderUnknown => "CacheProviderUnknown",
                },
                Vec::new(),
            )
        })
        .transpose()?;
    let state = match entry.terminal {
        None => actor_context_constructor(table, "RosterRunning", Vec::new())?,
        Some(terminal) => match terminal.kind {
            crate::ActorExitKind::Completed => {
                actor_context_constructor(table, "RosterStopped", Vec::new())?
            }
            crate::ActorExitKind::Failed => actor_context_constructor(
                table,
                "RosterFailed",
                vec![terminal.summary.to_value(table)?],
            )?,
            crate::ActorExitKind::Cancelled => actor_context_constructor(
                table,
                "RosterCancelled",
                vec![terminal.summary.to_value(table)?],
            )?,
        },
    };
    let role = match entry.descriptor.effective_role().role() {
        crate::ActorRole::Root => "ContextRoot",
        crate::ActorRole::Research => "ContextResearch",
        crate::ActorRole::Coding => "ContextCoding",
        crate::ActorRole::Scaffolding => "ContextScaffolding",
        crate::ActorRole::Integration => "ContextIntegration",
        crate::ActorRole::Inherited => "ContextInherited",
    };
    let workbench_posture = match &entry.runtime.workbench_posture {
        crate::ActorWorkbenchPosture::Idle => {
            actor_context_constructor(table, "WorkbenchIdle", Vec::new())?
        }
        crate::ActorWorkbenchPosture::RunningUnit {
            input_unit_index,
            total,
        } => actor_context_constructor(
            table,
            "WorkbenchRunningUnit",
            vec![
                workbench_int(*input_unit_index)?.to_value(table)?,
                workbench_int(*total)?.to_value(table)?,
            ],
        )?,
        crate::ActorWorkbenchPosture::AwaitingEffect {
            input_unit_index,
            total,
            effect,
        } => actor_context_constructor(
            table,
            "WorkbenchAwaitingEffect",
            vec![
                workbench_int(*input_unit_index)?.to_value(table)?,
                workbench_int(*total)?.to_value(table)?,
                effect.to_value(table)?,
            ],
        )?,
        crate::ActorWorkbenchPosture::TerminalTransfer { transfer } => {
            let transfer = actor_context_constructor(
                table,
                match transfer {
                    crate::ActorWorkbenchTransfer::Reply => "WorkbenchReplyTransfer",
                    crate::ActorWorkbenchTransfer::CancellationAcknowledgement => {
                        "WorkbenchCancellationTransfer"
                    }
                },
                Vec::new(),
            )?;
            actor_context_constructor(table, "WorkbenchTerminalTransfer", vec![transfer])?
        }
        crate::ActorWorkbenchPosture::Failed => {
            actor_context_constructor(table, "WorkbenchFailed", Vec::new())?
        }
    };
    Ok(actor_context_constructor(
        table,
        "AgentRosterEntry",
        vec![
            actor_int(entry.actor.id.0)?.to_value(table)?,
            actor_int(entry.actor.incarnation.0)?.to_value(table)?,
            entry.descriptor.label().to_owned().to_value(table)?,
            entry
                .runtime
                .requested_model
                .as_deref()
                .or(entry.descriptor.model())
                .map(str::to_owned)
                .to_value(table)?,
            entry.runtime.confirmed_model.to_value(table)?,
            actor_int(entry.received.0)?.to_value(table)?,
            actor_int(entry.received.1)?.to_value(table)?,
            entry
                .runtime
                .compactions
                .map(actor_int)
                .transpose()?
                .to_value(table)?,
            creator
                .map(|actor| actor_int(actor.id.0))
                .transpose()?
                .to_value(table)?,
            creator
                .map(|actor| actor_int(actor.incarnation.0))
                .transpose()?
                .to_value(table)?,
            supervisor
                .map(|actor| actor_int(actor.id.0))
                .transpose()?
                .to_value(table)?,
            supervisor
                .map(|actor| actor_int(actor.incarnation.0))
                .transpose()?
                .to_value(table)?,
            context_parent
                .map(|actor| actor_int(actor.id.0))
                .transpose()?
                .to_value(table)?,
            context_parent
                .map(|actor| actor_int(actor.incarnation.0))
                .transpose()?
                .to_value(table)?,
            state,
            health,
            entry
                .runtime
                .provider_turn
                .as_ref()
                .map(|turn| turn.turn.clone())
                .to_value(table)?,
            entry.runtime.provider_observation_stale.to_value(table)?,
            disposition.to_value(table)?,
            entry
                .requests
                .0
                .iter()
                .map(|id| actor_int(id.0))
                .collect::<Result<Vec<_>, _>>()?
                .to_value(table)?,
            entry
                .requests
                .1
                .iter()
                .map(|id| actor_int(id.0))
                .collect::<Result<Vec<_>, _>>()?
                .to_value(table)?,
            actor_context_constructor(table, role, Vec::new())?,
            entry.bound_worktree.to_value(table)?,
            entry
                .descriptor
                .fork_group()
                .map(|group| actor_int(group.0))
                .transpose()?
                .to_value(table)?,
            actor_int(entry.descriptor.placement().lexical_scope.0)?.to_value(table)?,
            entry.runtime.provider_thread.to_value(table)?,
            entry.runtime.provider_parent_thread.to_value(table)?,
            usage_observation_value(table, entry.runtime.first_provider_usage.as_ref())?,
            usage_observation_value(table, usage)?,
            usage_summary_value(table, entry.runtime.provider_usage_summary.as_ref())?,
            usage_summary_value(table, entry.runtime.latest_turn_usage_summary.as_ref())?,
            cache_boundary.to_value(table)?,
            actor_int(entry.runtime.event_watermark)?.to_value(table)?,
            workbench_posture,
        ],
    )?)
}

fn agent_stop_value(
    table: &DataConTable,
    outcome: AgentStopProjection,
) -> Result<Value, ResidentActorWorkbenchError> {
    let (name, fields) = match outcome {
        AgentStopProjection::StoppedNow => ("AgentStoppedNow", Vec::new()),
        AgentStopProjection::AlreadyStopped => ("AgentStopAlreadyStopped", Vec::new()),
        AgentStopProjection::Unavailable => ("AgentStopUnavailable", Vec::new()),
        AgentStopProjection::Unauthorized => ("AgentStopUnauthorized", Vec::new()),
        AgentStopProjection::Failed(detail) => ("AgentStopFailed", vec![detail.to_value(table)?]),
    };
    Ok(actor_context_constructor(table, name, fields)?)
}

fn cleanup_plan_value(
    table: &DataConTable,
    plan: &CleanupPlanProjection,
) -> Result<Value, ResidentActorWorkbenchError> {
    let actors = plan
        .actors
        .iter()
        .map(|actor| -> Result<Value, ResidentActorWorkbenchError> {
            let state = actor_context_constructor(
                table,
                if actor.terminal {
                    "CleanupActorTerminal"
                } else {
                    "CleanupActorRunning"
                },
                Vec::new(),
            )?;
            Ok(actor_context_constructor(
                table,
                "CleanupActorPlan",
                vec![
                    actor_int(actor.actor.id.0)?.to_value(table)?,
                    actor_int(actor.actor.incarnation.0)?.to_value(table)?,
                    actor.label.clone().to_value(table)?,
                    state,
                    actor_int(actor.revision)?.to_value(table)?,
                ],
            )?)
        })
        .collect::<Result<Vec<_>, _>>()?
        .to_value(table)?;
    let responses = plan
        .pending_responses
        .iter()
        .map(|request| actor_int(request.0))
        .collect::<Result<Vec<_>, _>>()?
        .to_value(table)?;
    let watches = plan
        .pending_watches
        .iter()
        .map(|watch| actor_int(watch.0))
        .collect::<Result<Vec<_>, _>>()?
        .to_value(table)?;
    Ok(actor_context_constructor(
        table,
        "CleanupPlan",
        vec![
            actor_int(plan.group.0)?.to_value(table)?,
            actors,
            responses,
            watches,
            plan.refusal.clone().to_value(table)?,
        ],
    )?)
}

fn cleanup_step_value(
    table: &DataConTable,
    step: CleanupStepProjection,
) -> Result<Value, ResidentActorWorkbenchError> {
    let ints = |values: Vec<u64>| -> Result<Value, ResidentActorWorkbenchError> {
        values
            .into_iter()
            .map(actor_int)
            .collect::<Result<Vec<_>, _>>()?
            .to_value(table)
            .map_err(ResidentActorWorkbenchError::Bridge)
    };
    let (name, fields) = match step {
        CleanupStepProjection::ForgotResponses(requests) => (
            "CleanupForgotResponses",
            vec![ints(
                requests.into_iter().map(|request| request.0).collect(),
            )?],
        ),
        CleanupStepProjection::ForgotWatches(watches) => (
            "CleanupForgotWatches",
            vec![ints(watches.into_iter().map(|watch| watch.0).collect())?],
        ),
        CleanupStepProjection::StoppedActor(actor, outcome) => (
            "CleanupStoppedActor",
            vec![
                actor_int(actor.id.0)?.to_value(table)?,
                actor_int(actor.incarnation.0)?.to_value(table)?,
                agent_stop_value(table, outcome)?,
            ],
        ),
        CleanupStepProjection::ForgotActor(actor) => (
            "CleanupForgotActor",
            vec![
                actor_int(actor.id.0)?.to_value(table)?,
                actor_int(actor.incarnation.0)?.to_value(table)?,
            ],
        ),
        CleanupStepProjection::ActorRetained {
            actor,
            requests,
            watches,
        } => (
            "CleanupActorRetained",
            vec![
                actor_int(actor.id.0)?.to_value(table)?,
                actor_int(actor.incarnation.0)?.to_value(table)?,
                ints(requests.into_iter().map(|request| request.0).collect())?,
                ints(watches.into_iter().map(|watch| watch.0).collect())?,
            ],
        ),
        CleanupStepProjection::GroupRetired(group) => (
            "CleanupGroupRetired",
            vec![actor_int(group.0)?.to_value(table)?],
        ),
        CleanupStepProjection::Blocked(detail) => ("CleanupBlocked", vec![detail.to_value(table)?]),
        CleanupStepProjection::StalePlan => ("CleanupStalePlan", Vec::new()),
    };
    Ok(actor_context_constructor(table, name, fields)?)
}

/// One fully captured boundary reached by an installed actor program.
/// Variants own every linear runtime value needed to service that boundary;
/// downstream orchestration never re-decodes the suspended request.
#[allow(
    clippy::large_enum_variant,
    reason = "boundaries deliberately retain linear runtime custody without a second allocation layer"
)]
pub(crate) enum ResidentActorBoundary {
    Command {
        continuation: ResidentHole,
        request: crate::generated::commands::CommandsReq,
    },
    Replace {
        target: crate::ActorRef,
        candidate: crate::ResidentActorStart,
    },
    Drain {
        continuation: ResidentHole,
        target: crate::ActorRef,
    },
    NotificationSend {
        continuation: ResidentHole,
        target: crate::ActorRef,
        message: String,
    },
    NotificationPoll {
        continuation: ResidentHole,
        receipt: crate::notification::NotificationReceiptWire,
    },
    Completed,
    ActorContext(ResidentHole),
    ActorLocalContext(ResidentHole),
    ForkGroup(ForkGroupBoundary),
    Start(crate::ResidentActorStart),
    Outbound(ResidentOutbound),
    Wait(ResidentWaitRequest),
    Poll(crate::wait::ResidentPollRequest),
    Receive(InstalledReceiver),
    Checkpoint {
        continuation: ResidentHole,
        site: u64,
        value: RootCustody,
    },
    ToolAwait(crate::resident_tools::ResidentToolAwait),
    ToolReply(crate::resident_tools::ResidentToolReply),
    AgentSession(crate::ResidentInteractiveSession),
    AgentAttachment(ResidentAgentAttachment),
    AgentInspect(AgentInspectionBoundary),
    AgentList(ResidentHole),
    AgentShareObservation {
        continuation: ResidentHole,
        recipient: crate::ActorRef,
        scope: crate::ActorRef,
    },
    AgentGroupList {
        continuation: ResidentHole,
        group: crate::ForkGroupId,
    },
    AgentForget(AgentInspectionBoundary),
    AgentStop(AgentInspectionBoundary),
    CleanupPlan {
        continuation: ResidentHole,
        group: crate::ForkGroupId,
    },
    CleanupExecute {
        continuation: ResidentHole,
        group: crate::ForkGroupId,
        inspected: Vec<(crate::ActorRef, u64)>,
    },
    RequestReservation(RequestReservation),
    RequestSubmission(RequestSubmission),
    ReplyAttempt(ReplyAttempt),
    ResponsePoll(ResponsePoll),
    ProgressPublication {
        continuation: ResidentHole,
        request: crate::RequestId,
        value: RootCustody,
    },
    ProgressPoll(ResponsePoll),
    RequestUpdate {
        continuation: ResidentHole,
        request: crate::RequestId,
        message: String,
    },
    RequestUpdatePoll {
        continuation: ResidentHole,
        update: crate::RequestUpdateId,
    },
    WatchProgressPoll {
        continuation: ResidentHole,
        watch: crate::WatchId,
        request: crate::RequestId,
        after: u64,
    },
    RequestCancellation(RequestCancellation),
    ResponseAbandonment(ResponseAbandonment),
    ResponseForget(ResponseForget),
    ReplyPoll(ReplyPoll),
    CancellationAcknowledgement(CancellationAcknowledgement),
    WatchRegistration(WatchRegistration),
    RouteRegistration {
        registration: WatchRegistration,
        entry: RootCustody,
    },
    RoutePoll(WatchPoll),
    RouteList(ResidentHole),
    WatchPoll(WatchPoll),
    WatchForget(WatchForget),
}

pub(crate) enum ForkGroupBoundary {
    Preview {
        continuation: ResidentHole,
        role: crate::ActorLaunchRoleWire,
        effect_keys: Vec<crate::ActorEffectKeyWire>,
        budget: Option<(i64, i64)>,
        model: Option<String>,
        effort: Option<crate::ForkEffort>,
        context: crate::ForkContext,
        instructions: Option<String>,
        lifetime: crate::WorkerLifetime,
    },
    Begin {
        continuation: ResidentHole,
        relative: bool,
        group: String,
        branches: Vec<String>,
    },
    Commit {
        continuation: ResidentHole,
        group: crate::ForkGroupId,
    },
    Abort {
        continuation: ResidentHole,
        group: crate::ForkGroupId,
    },
    Cleanup {
        continuation: ResidentHole,
        group: crate::ForkGroupId,
    },
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum ResidentKernelBoundary {
    Reply,
    Continue,
}

impl ResidentKernelBoundary {
    fn operation(self) -> &'static str {
        match self {
            Self::Reply => "reply",
            Self::Continue => "continue",
        }
    }
}

impl ResidentActorBoundary {
    pub(crate) fn operation(&self) -> &'static str {
        match self {
            Self::Completed => "program completion",
            Self::Command { .. } => "command job",
            Self::NotificationSend { .. } => "notify",
            Self::NotificationPoll { .. } => "pollNotification",
            Self::ActorContext(_) => "actorContext",
            Self::ActorLocalContext(_) => "actor local context",
            Self::ForkGroup(ForkGroupBoundary::Preview { .. }) => "preview context-fork policy",
            Self::ForkGroup(ForkGroupBoundary::Begin { .. }) => "begin context-fork group",
            Self::ForkGroup(ForkGroupBoundary::Commit { .. }) => "commit context-fork group",
            Self::ForkGroup(ForkGroupBoundary::Abort { .. }) => "abort context-fork group",
            Self::ForkGroup(ForkGroupBoundary::Cleanup { .. }) => "cleanup context-fork group",
            Self::Start(_) => "startActor",
            Self::Replace { .. } => "replaceActor",
            Self::Outbound(ResidentOutbound::Call { .. }) => "call",
            Self::Outbound(ResidentOutbound::TryCall { .. }) => "tryCall",
            Self::Outbound(ResidentOutbound::Cast { .. }) => "cast",
            Self::Drain { .. } => "drainActor",
            Self::Wait(_) => "awaitExit",
            Self::Poll(_) => "pollExit",
            Self::Receive(_) => "receive",
            Self::Checkpoint { .. } => "state checkpoint",
            Self::ToolAwait(_) => "agent tool await",
            Self::ToolReply(_) => "agent tool reply",
            Self::AgentSession(_) => "agent session",
            Self::AgentAttachment(_) => "agent attachment",
            Self::AgentInspect(_) => "observeAgent",
            Self::AgentList(_) => "listAgents",
            Self::AgentShareObservation { .. } => "shareObservation",
            Self::AgentGroupList { .. } => "observeForkGroup",
            Self::AgentForget(_) => "forgetAgent",
            Self::AgentStop(_) => "stopAgent",
            Self::CleanupPlan { .. } => "planCleanup",
            Self::CleanupExecute { .. } => "executeCleanup",
            Self::RequestReservation(_) => "request",
            Self::RequestSubmission(_) => "request",
            Self::ReplyAttempt(_) => "reply",
            Self::ResponsePoll(_) => "pollResponse",
            Self::ProgressPublication { .. } => "reportProgress",
            Self::ProgressPoll(_) => "pollProgress",
            Self::RequestUpdate { .. } => "updateRequest",
            Self::RequestUpdatePoll { .. } => "pollRequestUpdate",
            Self::WatchProgressPoll { .. } => "pollWatch progress",
            Self::RequestCancellation(_) => "cancelRequest",
            Self::ResponseAbandonment(_) => "abandonResponse",
            Self::ResponseForget(_) => "forgetResponse",
            Self::ReplyPoll(_) => "pollReply",
            Self::CancellationAcknowledgement(_) => "acknowledgeCancellation",
            Self::WatchRegistration(_) => "watch",
            Self::RouteRegistration { .. } => "route",
            Self::RoutePoll(_) => "pollRoute",
            Self::RouteList(_) => "listRoutes",
            Self::WatchPoll(_) => "pollWatch",
            Self::WatchForget(_) => "forgetWatch",
        }
    }
}

#[derive(Clone, Copy)]
enum BoundaryCapture {
    Execution,
    Replacement,
}

/// The one nominal roster for requests interpreted at actor execution
/// boundaries. Generated request enums own constructor recognition and field
/// shape; this sum owns orchestration routing.
enum ResidentRequest {
    Commands(crate::generated::commands::CommandsReq),
    Notifications(crate::generated::notifications::NotificationsReq),
    Actor(crate::generated::actor::ActorReq),
    ActorContext(crate::generated::actor_context::ActorContextReq),
    AgentControl(crate::generated::agent_control::AgentControlReq),
    AgentInspection(crate::generated::agent_inspection::AgentInspectionReq),
    AgentLaunch(crate::generated::agent_launch::AgentLaunchReq),
    Forks(crate::generated::forks::ForksReq),
    ActorKernel(crate::generated::actor_kernel::ActorKernelReq),
    ActorLocal(crate::generated::actor_local::ActorLocalReq),
    AgentTools(crate::generated::agent_tools::AgentToolsReq),
    AgentSession(crate::generated::agent_session::AgentSessionReq),
    Replies(RepliesReq),
    Watches(WatchesReq),
}

impl ResidentRequest {
    fn decode(request: &Value, table: &DataConTable) -> Result<Self, ResidentActorWorkbenchError> {
        macro_rules! try_member {
            ($variant:path, $request:ty) => {
                match <$request as FromCore>::from_value(request, table) {
                    Ok(decoded) => return Ok($variant(decoded)),
                    Err(BridgeError::UnknownDataCon(_)) => {}
                    Err(source) => {
                        return Err(ResidentActorWorkbenchError::RequestDecode {
                            constructor: request_constructor(request, table),
                            source,
                        });
                    }
                }
            };
        }

        try_member!(
            Self::Notifications,
            crate::generated::notifications::NotificationsReq
        );
        try_member!(Self::Commands, crate::generated::commands::CommandsReq);
        try_member!(Self::Actor, crate::generated::actor::ActorReq);
        try_member!(
            Self::ActorContext,
            crate::generated::actor_context::ActorContextReq
        );
        try_member!(
            Self::AgentControl,
            crate::generated::agent_control::AgentControlReq
        );
        try_member!(
            Self::AgentInspection,
            crate::generated::agent_inspection::AgentInspectionReq
        );
        try_member!(
            Self::AgentLaunch,
            crate::generated::agent_launch::AgentLaunchReq
        );
        try_member!(Self::Forks, crate::generated::forks::ForksReq);
        try_member!(
            Self::ActorKernel,
            crate::generated::actor_kernel::ActorKernelReq
        );
        try_member!(
            Self::ActorLocal,
            crate::generated::actor_local::ActorLocalReq
        );
        try_member!(
            Self::AgentTools,
            crate::generated::agent_tools::AgentToolsReq
        );
        try_member!(
            Self::AgentSession,
            crate::generated::agent_session::AgentSessionReq
        );
        try_member!(Self::Replies, RepliesReq);
        try_member!(Self::Watches, WatchesReq);

        Err(ResidentActorWorkbenchError::UnsupportedRequest {
            constructor: request_constructor(request, table),
        })
    }

    fn operation(&self) -> &'static str {
        match self {
            Self::Commands(_) => "command job",
            Self::Notifications(crate::generated::notifications::NotificationsReq::NotifyWith(
                ..,
            )) => "notify",
            Self::Notifications(
                crate::generated::notifications::NotificationsReq::PollNotificationWith(..),
            ) => "pollNotification",
            Self::Actor(crate::generated::actor::ActorReq::ActorBeginForkGroupWith(..)) => {
                "begin context-fork group"
            }
            Self::ActorContext(
                crate::generated::actor_context::ActorContextReq::ActorContextWith,
            ) => "actorContext",
            Self::AgentControl(
                crate::generated::agent_control::AgentControlReq::AgentControlStopWith(..),
            ) => "stopAgent",
            Self::AgentInspection(
                crate::generated::agent_inspection::AgentInspectionReq::AgentInspectCleanupWith(..),
            ) => "planCleanup",
            Self::AgentControl(
                crate::generated::agent_control::AgentControlReq::AgentControlExecuteCleanupWith(
                    ..,
                ),
            ) => "executeCleanup",
            Self::AgentInspection(
                crate::generated::agent_inspection::AgentInspectionReq::AgentInspectWith(..),
            ) => "observeAgent",
            Self::AgentInspection(
                crate::generated::agent_inspection::AgentInspectionReq::AgentListWith,
            ) => "listAgents",
            Self::AgentInspection(
                crate::generated::agent_inspection::AgentInspectionReq::AgentShareObservationWith(
                    ..,
                ),
            ) => "shareObservation",
            Self::AgentInspection(
                crate::generated::agent_inspection::AgentInspectionReq::AgentGroupListWith(..),
            ) => "observeForkGroup",
            Self::AgentInspection(
                crate::generated::agent_inspection::AgentInspectionReq::AgentForgetWith(..),
            ) => "forgetAgent",
            Self::AgentLaunch(crate::generated::agent_launch::AgentLaunchReq::AgentLaunchWith(
                ..,
            )) => "startAgent",
            Self::Forks(crate::generated::forks::ForksReq::ForksBeginWith(..)) => {
                "begin context-fork group"
            }
            Self::Forks(crate::generated::forks::ForksReq::ForksStartWith(..)) => "context fork",
            Self::Forks(crate::generated::forks::ForksReq::ForksPreviewWith(..)) => {
                "preview context-fork policy"
            }
            Self::Forks(crate::generated::forks::ForksReq::ForksCommitWith(..)) => {
                "commit context-fork group"
            }
            Self::Forks(crate::generated::forks::ForksReq::ForksAbortWith(..)) => {
                "abort context-fork group"
            }
            Self::Forks(crate::generated::forks::ForksReq::ForksCleanupWith(..)) => {
                "cleanup context-fork group"
            }
            Self::Actor(crate::generated::actor::ActorReq::ActorStartWith(..)) => "startActor",
            Self::Actor(crate::generated::actor::ActorReq::ActorForkWith(..)) => "context fork",
            Self::Actor(crate::generated::actor::ActorReq::ActorCommitForkGroupWith(..)) => {
                "commit context-fork group"
            }
            Self::Actor(crate::generated::actor::ActorReq::ActorAbortForkGroupWith(..)) => {
                "abort context-fork group"
            }
            Self::Actor(crate::generated::actor::ActorReq::ActorWaitWith(..)) => "awaitExit",
            Self::Actor(crate::generated::actor::ActorReq::ActorPollWith(..)) => "pollExit",
            Self::Actor(crate::generated::actor::ActorReq::ActorCallWith(..)) => "call",
            Self::Actor(crate::generated::actor::ActorReq::ActorTryCallWith(..)) => "tryCall",
            Self::Actor(crate::generated::actor::ActorReq::ActorCastWith(..)) => "cast",
            Self::Actor(crate::generated::actor::ActorReq::ActorDrainWith(..)) => "drainActor",
            Self::Actor(crate::generated::actor::ActorReq::ActorReplaceWith(..)) => "replaceActor",
            Self::ActorKernel(
                crate::generated::actor_kernel::ActorKernelReq::ActorInstallShutdownWith(..),
            ) => "installShutdown",
            Self::ActorKernel(
                crate::generated::actor_kernel::ActorKernelReq::ActorInstallProgressSourceWith(..),
            ) => "install progress source",
            Self::ActorKernel(
                crate::generated::actor_kernel::ActorKernelReq::ActorInstallSettlementSourceWith(
                    ..,
                ),
            ) => "install settlement source",
            Self::ActorKernel(
                crate::generated::actor_kernel::ActorKernelReq::ActorInstallCommandSourceWith(..),
            ) => "command source",
            Self::ActorKernel(
                crate::generated::actor_kernel::ActorKernelReq::ActorInstallLifecycleSourceWith(..),
            ) => "install lifecycle source",
            Self::ActorKernel(
                crate::generated::actor_kernel::ActorKernelReq::ActorSourceInputWith,
            ) => "source input",
            Self::ActorKernel(crate::generated::actor_kernel::ActorKernelReq::ActorReadyWith) => {
                "ready"
            }
            Self::ActorKernel(crate::generated::actor_kernel::ActorKernelReq::ActorReplyWith(
                ..,
            )) => "reply",
            Self::ActorKernel(
                crate::generated::actor_kernel::ActorKernelReq::ActorContinueWith(..),
            ) => "continue",
            Self::ActorLocal(
                crate::generated::actor_local::ActorLocalReq::ActorLocalContextWith,
            ) => "actor local context",
            Self::ActorLocal(crate::generated::actor_local::ActorLocalReq::ActorReceiveWith(
                ..,
            )) => "receive",
            Self::ActorLocal(
                crate::generated::actor_local::ActorLocalReq::ActorCheckpointWith(..),
            ) => "state checkpoint",
            Self::AgentTools(
                crate::generated::agent_tools::AgentToolsReq::AgentToolsAwaitWith(..),
            ) => "agent tool await",
            Self::AgentTools(
                crate::generated::agent_tools::AgentToolsReq::AgentToolsReplyWith(..),
            ) => "agent tool reply",
            Self::AgentSession(
                crate::generated::agent_session::AgentSessionReq::AgentSessionWith(..),
            ) => "agent session",
            Self::AgentSession(
                crate::generated::agent_session::AgentSessionReq::AgentAttachWith(..),
            ) => "agent attachment",
            Self::Replies(RepliesReq::ReserveRequestWith(..)) => "request reservation",
            Self::Replies(RepliesReq::SubmitRequestWith(..)) => "request submission",
            Self::Replies(RepliesReq::AttemptReplyWith(..)) => "attemptReply",
            Self::Replies(RepliesReq::PublishProgressWith(..)) => "reportProgress",
            Self::Replies(RepliesReq::ObserveProgressWith(..)) => "pollProgress",
            Self::Replies(RepliesReq::UpdateRequestWith(..)) => "updateRequest",
            Self::Replies(RepliesReq::ObserveRequestUpdateWith(..)) => "pollRequestUpdate",
            Self::Replies(RepliesReq::ReplyWith(..)) => "reply",
            Self::Replies(RepliesReq::ObserveResponseWith(..)) => "pollResponse",
            Self::Replies(RepliesReq::CancelRequestWith(..)) => "cancelRequest",
            Self::Replies(RepliesReq::AbandonResponseWith(..)) => "abandonResponse",
            Self::Replies(RepliesReq::ForgetResponseWith(..)) => "forgetResponse",
            Self::Replies(RepliesReq::ObserveReplyWith(..)) => "pollReply",
            Self::Replies(RepliesReq::AttemptAcknowledgeCancellationWith(..)) => {
                "attemptAcknowledgeCancellation"
            }
            Self::Replies(RepliesReq::AcknowledgeCancellationWith(..)) => "acknowledgeCancellation",
            Self::Watches(WatchesReq::RegisterWatchWith(..)) => "watch",
            Self::Watches(WatchesReq::RegisterWatchGroupsWith(..)) => "watch",
            Self::Watches(WatchesReq::RegisterRouteWith(..)) => "route",
            Self::Watches(WatchesReq::RegisterRouteGroupsWith(..)) => "route",
            Self::Watches(WatchesReq::ObserveRouteWith(..)) => "pollRoute",
            Self::Watches(WatchesReq::ListRoutesWith) => "listRoutes",
            Self::Watches(WatchesReq::ObserveWatchWith(..)) => "pollWatch",
            Self::Watches(WatchesReq::ObserveWatchProgressWith(..)) => "pollWatch progress",
            Self::Watches(WatchesReq::ForgetWatchWith(..)) => "forgetWatch",
        }
    }
}

impl<H, O> ResidentActorRunner<H, O> {
    #[must_use]
    pub fn new(machines: Arc<ActorMachineRegistry<H, O>>, source: ActorWorkbenchSource) -> Self {
        Self {
            access: ResidentMachineAccess::new(machines, source),
        }
    }

    pub(crate) fn workbench(
        &self,
        response: ResponseExpectation,
        request: crate::RequestId,
        type_modules: Vec<String>,
    ) -> ResidentActorWorkbench<H, O> {
        ResidentActorWorkbench::new(
            Arc::clone(&self.access.machines),
            self.access.source.clone(),
            Some(response),
            Some(request),
            type_modules,
        )
    }

    pub(crate) fn application_workbench(&self) -> ResidentActorWorkbench<H, O> {
        ResidentActorWorkbench::new(
            Arc::clone(&self.access.machines),
            self.access.source.clone(),
            None,
            None,
            Vec::new(),
        )
    }
}

impl<H, O> ResidentActorWorkbench<H, O> {
    #[must_use]
    pub fn new(
        machines: Arc<ActorMachineRegistry<H, O>>,
        source: ActorWorkbenchSource,
        response: Option<ResponseExpectation>,
        request: Option<crate::RequestId>,
        type_modules: Vec<String>,
    ) -> Self {
        Self {
            access: ResidentMachineAccess::new(machines, source),
            response,
            request,
            type_modules: type_modules.into(),
            json_input: None,
        }
    }

    #[must_use]
    pub(crate) fn with_json_input(mut self, input: Option<serde_json::Value>) -> Self {
        self.json_input = input;
        self
    }
}

#[derive(Debug, thiserror::Error)]
pub enum CommandObservationStop {
    #[error("command is still running after 30 seconds")]
    Deadline,
    #[error("command completed, but output observation failed: {0:?}")]
    OutputUnavailable(tidepool_bridge_effects::CommandError),
}

#[derive(Debug, thiserror::Error)]
pub enum ResidentActorWorkbenchError {
    #[error("command {job} retained: {reason}")]
    CommandObservationStopped {
        job: String,
        reason: CommandObservationStop,
    },
    #[error(transparent)]
    CompileView(#[from] ActorCompileViewError),
    #[error("resident machine checkout failed: {0}")]
    Checkout(CheckoutError<String>),
    #[error("resident workbench compiler failed: {0}")]
    Compile(CompileError),
    #[error("resident workbench compiler infrastructure failed:\n{0}")]
    CompileInfrastructure(String),
    #[error("resident workbench execution failed: {0}")]
    Resident(ResidentError),
    #[error("resident workbench task panicked or was cancelled: {0}")]
    Join(tokio::task::JoinError),
    #[error("could not mount the typed completion input: {0}")]
    InputMount(String),
    #[error("could not inspect the saved value: {0}")]
    Inspection(String),
    #[error("actor protocol violation: {0}")]
    ActorProtocol(String),
    #[error("unsupported resident actor request `{constructor}`")]
    UnsupportedRequest { constructor: String },
    #[error("could not decode resident actor request `{constructor}`: {source}")]
    RequestDecode {
        constructor: String,
        source: BridgeError,
    },
    #[error("could not bridge an actor protocol value: {0}")]
    Bridge(#[from] tidepool_bridge::BridgeError),
    #[error(transparent)]
    InteractiveSessionCapture(#[from] crate::InteractiveSessionCaptureError),
    #[error(transparent)]
    StartCapture(#[from] crate::ActorStartCaptureError),
    #[error(transparent)]
    WaitCapture(#[from] crate::ActorWaitError),
    #[error("invalid agent tool declaration set: {0}")]
    ToolDeclarations(serde_json::Error),
}

impl<H, O> ResidentMachineAccess<H, O>
where
    H: DispatchEffect<O> + Send + 'static,
    O: OutputSink + Sync + 'static,
{
    async fn with_machine<ResultValue>(
        &self,
        context: crate::ActorSessionContext,
        operation: impl FnOnce(
                &mut ResidentSession<H, O>,
                &crate::ActorSessionContext,
                &ActorWorkbenchSource,
            ) -> Result<ResultValue, ResidentActorWorkbenchError>
            + Send
            + 'static,
    ) -> Result<ResultValue, ResidentActorWorkbenchError>
    where
        ResultValue: Send + 'static,
    {
        self.with_machine_wait(context, None, operation).await
    }

    async fn with_machine_wait<ResultValue>(
        &self,
        context: crate::ActorSessionContext,
        max_wait: Option<Duration>,
        operation: impl FnOnce(
                &mut ResidentSession<H, O>,
                &crate::ActorSessionContext,
                &ActorWorkbenchSource,
            ) -> Result<ResultValue, ResidentActorWorkbenchError>
            + Send
            + 'static,
    ) -> Result<ResultValue, ResidentActorWorkbenchError>
    where
        ResultValue: Send + 'static,
    {
        self.with_host_machine(
            context.placement.session,
            max_wait,
            move |session, source| {
                session
                    .set_actor_execution(
                        context.run_context(),
                        context.effect_policy,
                        context.live_payload,
                    )
                    .map_err(ResidentActorWorkbenchError::Resident)?;
                operation(session, &context, source)
            },
        )
        .await
    }

    async fn with_host_machine<T: Send + 'static>(
        &self,
        session_id: tidepool_repr::SessionId,
        max_wait: Option<Duration>,
        operation: impl FnOnce(
                &mut ResidentSession<H, O>,
                &ActorWorkbenchSource,
            ) -> Result<T, ResidentActorWorkbenchError>
            + Send
            + 'static,
    ) -> Result<T, ResidentActorWorkbenchError> {
        let request = tidepool_runtime::session::registry::CheckoutRequest::Run;
        let admission_started = std::time::Instant::now();
        let checkout = match max_wait {
            Some(limit) => {
                self.machines
                    .checkout_wait(session_id, request, limit)
                    .await
            }
            None => self.machines.checkout_queued(session_id, request).await,
        }
        .map_err(ResidentActorWorkbenchError::Checkout)?;
        tracing::debug!(session = ?session_id,
            waited_ms = admission_started.elapsed().as_millis(),
            "resident machine checkout admitted");
        let (mut session, receipt) = checkout.into_parts();
        let source = self.source.clone();
        let machines = Arc::clone(&self.machines);

        let task = tokio::task::spawn_blocking(move || {
            // The blocking task owns the machine and its linear checkout
            // receipt together. Its async caller may be cooperatively
            // cancelled while this closure is running; settlement must not
            // depend on that caller continuing to poll the JoinHandle.
            let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                operation(&mut session, &source)
            }));
            match outcome {
                Ok(outcome) => {
                    if session.compilation_failed() {
                        machines.settle_retire(receipt);
                        return outcome;
                    }

                    let holes = session
                        .parked_holes()
                        .into_iter()
                        .map(str::to_string)
                        .collect();
                    machines.settle_suspended(receipt, session, holes);
                    outcome
                }
                Err(payload) => {
                    machines.settle_retire(receipt);
                    std::panic::resume_unwind(payload);
                }
            }
        })
        .await;

        match task {
            Ok(outcome) => outcome,
            Err(error) => Err(ResidentActorWorkbenchError::Join(error)),
        }
    }
}

impl<H, O> ResidentActorWorkbench<H, O>
where
    H: DispatchEffect<O> + Send + 'static,
    O: OutputSink + Sync + 'static,
{
    pub(crate) async fn mount_named_input(
        &self,
        context: crate::ActorSessionContext,
        name: &'static str,
        input_type: impl Into<String>,
        input: RootCustody,
    ) -> Result<(), ResidentActorWorkbenchError> {
        let input_type = input_type.into();
        let type_modules = Arc::clone(&self.type_modules);
        self.access
            .with_machine(context, move |session, context, source| {
                let block = ParsedBlock {
                    ordinal: 1,
                    total: 1,
                    source: format!("{name} <- pure (undefined :: ({input_type}))"),
                };
                let compiled = match compile_block(
                    session,
                    context,
                    source,
                    &context.haskell_effects_alias,
                    &type_modules,
                    &block,
                )? {
                    CompiledBlock::Ready(compiled) => compiled,
                    CompiledBlock::Rejected(diagnostic) => {
                        return Err(ResidentActorWorkbenchError::InputMount(diagnostic));
                    }
                };
                let ReadyBlock {
                    result, generation, ..
                } = *compiled;
                let TurnResult::Bind {
                    bound,
                    compiled: expression,
                    ..
                } = result
                else {
                    return Err(ResidentActorWorkbenchError::InputMount(
                        "the internal goal-input source was not classified as a binding".into(),
                    ));
                };
                let [binder] = bound.as_slice() else {
                    return Err(ResidentActorWorkbenchError::InputMount(format!(
                        "the internal goal-input binding produced {} binders",
                        bound.len()
                    )));
                };
                session
                    .mount_compiled_binding_in(
                        context.placement.lexical_scope,
                        binder,
                        generation,
                        &expression.table,
                        input,
                    )
                    .map_err(ResidentActorWorkbenchError::Resident)
            })
            .await
    }

    pub(crate) async fn activation_preview(
        &self,
        context: crate::ActorSessionContext,
        reply_type: String,
        reply_declaration: Option<String>,
    ) -> (String, String) {
        let mut source = self.access.source.clone();
        if let (Some(response), Some(request)) = (&self.response, self.request) {
            source.preamble = response
                .request_preamble(&source.preamble, request, &context.haskell_effects_alias)
                .into();
        }
        let type_modules = self.type_modules.clone();
        let input = match self
            .access
            .with_machine(context, move |session, context, _| {
                render_observation(
                    session, context, &source, &type_modules, "sessionInput",
                    ObservationPurpose::Assignment,
                )
            })
            .await
        {
            Ok((text, omitted)) => bounded_activation_text(
                text, ACTIVATION_INPUT_LIMIT, omitted, "inspectFull sessionInput",
            ),
            Err(_) => "<input rendering unavailable; use `:type sessionInput` and select or apply the value; `inspectFull sessionInput` requires Show>".into(),
        };
        let reply = reply_declaration.unwrap_or_else(|| {
            format!("{reply_type} (no declaration captured at the request site)")
        });
        (
            input,
            bounded_activation_text(reply, 4 * 1024, false, &format!(":info {reply_type}")),
        )
    }

    /// Compile and begin one actor-local workbench item. Declarations commit
    /// immediately; executable items retain their fragment realm so the host
    /// can route any actor effects through the ordinary actor driver.
    pub(crate) async fn begin_item(
        &self,
        context: crate::ActorSessionContext,
        block: ParsedBlock,
        kind: GhciInputKind,
    ) -> Result<ResidentWorkbenchStep, ResidentActorWorkbenchError> {
        let response = self.response.clone();
        let request = self.request;
        let type_modules = Arc::clone(&self.type_modules);
        let mut turn_source = self.access.source.clone();
        turn_source.preamble = format!(
            "{}{}",
            turn_source.preamble,
            tidepool_runtime::session::workbench_input_binding(self.json_input.as_ref())
        )
        .into();
        self.access
            .with_machine(context, move |session, context, _| {
                begin_fragment(
                    session,
                    context,
                    &turn_source,
                    RequestWorkbenchScope {
                        response: response.as_ref(),
                        request,
                        type_modules: &type_modules,
                    },
                    block,
                    kind,
                )
            })
            .await
    }

    /// Install a trusted job reference after stopping a foreground computation.
    pub(crate) async fn bind_background_job(
        &self,
        context: crate::ActorSessionContext,
        job: String,
        reason: CommandObservationStop,
    ) -> Result<ResidentWorkbenchStep, ResidentActorWorkbenchError> {
        self.access.with_machine(context, move |session, context, source| {
            let scope = context.placement.lexical_scope;
            let names: Vec<_> = session.workbench_bindings_in(scope).into_iter().map(|binding| binding.name).collect();
            let trusted_imports = SourceImports::from_specs([
                "qualified Tidepool.Command.Types as ShoalCommandBinding",
                "qualified Data.Text as ShoalCommandText",
            ]);
            let literal = tidepool_runtime::session::escape_workbench_haskell_string(&job);
            let declaration_for = |binding: &str| format!(
                "{binding} :: ShoalCommandBinding.Job\n{binding} = ShoalCommandBinding.Job (ShoalCommandText.pack \"{literal}\")"
            );
            for name in &names {
                if name.strip_prefix("job").is_some_and(|suffix| !suffix.is_empty() && suffix.bytes().all(|byte| byte.is_ascii_digit()))
                    && session.workbench_declaration_matches_in(scope, name, &declaration_for(name), &trusted_imports)
                {
                    return Ok(ResidentWorkbenchStep::CommandBackgrounded { job, binding: name.clone(), reason });
                }
            }
            let mut index = session.val_gen().0;
            let binding = loop {
                let name = format!("job{index}");
                if !names.contains(&name) { break name; }
                index += 1;
            };
            let mut imports = source.workbench_imports.clone();
            imports.extend(&trusted_imports);
            let declaration = declaration_for(&binding);
            session.define_scoped_with_imports_in(scope, &[&declaration], &imports)
                .map_err(|error| ResidentActorWorkbenchError::InputMount(format!(
                    "command {job} remains owned, but its automatic binding failed: {error}"
                )))?;
            Ok(ResidentWorkbenchStep::CommandBackgrounded { job, binding, reason })
        }).await
    }

    pub(crate) fn inspection_query(
        &self,
        source: &str,
        kind: GhciInputKind,
    ) -> Result<Option<InspectionQuery>, String> {
        inspection_query(&self.access.source, source, kind)
    }

    pub(crate) async fn inspect_items(
        &self,
        context: crate::ActorSessionContext,
        queries: Vec<InspectionQuery>,
    ) -> Result<Vec<Result<String, String>>, ResidentActorWorkbenchError> {
        let type_modules = Arc::clone(&self.type_modules);
        let mut turn_source = self.access.source.clone();
        let preamble = match (&self.response, self.request) {
            (Some(response), Some(request)) => response.request_preamble(
                &turn_source.preamble,
                request,
                &context.haskell_effects_alias,
            ),
            (None, None) => turn_source.preamble.to_string(),
            _ => unreachable!("request workbench scope is constructed atomically"),
        };
        turn_source.preamble = format!(
            "{}{}",
            preamble,
            tidepool_runtime::session::workbench_input_binding(self.json_input.as_ref())
        )
        .into();
        let snapshot_source = turn_source.clone();
        let compile_view = self
            .access
            .with_machine(context, move |session, context, _| {
                actor_compile_view(session, context, &snapshot_source, &type_modules)
            })
            .await?;
        // Inspection consumes immutable compiler inputs and never touches live
        // Haskell values. Other actors may use the machine while GHC answers.
        tokio::task::spawn_blocking(move || {
            inspect_compile_view(&compile_view, &turn_source, &queries)
        })
        .await
        .map_err(ResidentActorWorkbenchError::Join)?
    }

    /// Settle a resumed fragment outcome. Nominal suspensions retain
    /// the same realm and return to the host for nominal actor dispatch.
    pub(crate) async fn settle_item(
        &self,
        context: crate::ActorSessionContext,
        fragment: ResidentWorkbenchFragment,
        outcome: ResidentOutcome,
    ) -> Result<ResidentWorkbenchStep, ResidentActorWorkbenchError> {
        self.access
            .with_machine(context, move |session, context, _| {
                settle_fragment(session, context, fragment, outcome)
            })
            .await
    }
}

fn begin_fragment<H, O>(
    session: &mut ResidentSession<H, O>,
    context: &crate::ActorSessionContext,
    source: &ActorWorkbenchSource,
    scope: RequestWorkbenchScope<'_>,
    block: ParsedBlock,
    kind: GhciInputKind,
) -> Result<ResidentWorkbenchStep, ResidentActorWorkbenchError>
where
    H: DispatchEffect<O> + Send,
    O: OutputSink + Sync,
{
    let mut source = source.clone();
    source.preamble = match (scope.response, scope.request) {
        (Some(response), Some(request)) => {
            response.request_preamble(&source.preamble, request, &context.haskell_effects_alias)
        }
        (None, None) => source.preamble.to_string(),
        _ => unreachable!("request workbench scope is constructed atomically"),
    }
    .into();
    if kind == GhciInputKind::Command {
        return match run_discovery(session, context, &source, scope.type_modules, &block)? {
            Ok(output) => Ok(ResidentWorkbenchStep::Committed {
                output,
                warnings: Vec::new(),
                installed_bindings: Vec::new(),
            }),
            Err(diagnostic) => Ok(ResidentWorkbenchStep::Rejected(diagnostic)),
        };
    }
    let compiled = match compile_block(
        session,
        context,
        &source,
        &context.haskell_effects_alias,
        scope.type_modules,
        &block,
    )? {
        CompiledBlock::Ready(compiled) => compiled,
        CompiledBlock::Rejected(diagnostic) => {
            return Ok(ResidentWorkbenchStep::Rejected(diagnostic));
        }
    };
    let ReadyBlock {
        result,
        generation,
        declaration_source,
        declaration_imports,
        observation,
    } = *compiled;
    match result {
        TurnResult::Decl(receipt) => {
            let result = match session.define_scoped_with_imports_in(
                context.placement.lexical_scope,
                &[&declaration_source],
                &declaration_imports,
            ) {
                Ok(generation) => ResidentWorkbenchStep::Committed {
                    output: format!(
                        "defined {} at generation {}",
                        if receipt.binders.is_empty() {
                            "declaration".to_string()
                        } else {
                            receipt.binders.join(", ")
                        },
                        generation.0
                    ),
                    warnings: Vec::new(),
                    installed_bindings: receipt.binders.clone(),
                },
                Err(tidepool_runtime::session::SessionError::ValidationFailed(failure)) => {
                    ResidentWorkbenchStep::Rejected(failure.render_for_input(
                        &format!("<input unit {}>", block.ordinal),
                        &block.source,
                    ))
                }
                Err(error) if classify_session(&error).class == FailureClass::UserHaskell => {
                    ResidentWorkbenchStep::Rejected(classify_session(&error).message)
                }
                Err(error) => {
                    return Err(ResidentActorWorkbenchError::Resident(
                        ResidentError::Session(error),
                    ))
                }
            };
            Ok(result)
        }
        TurnResult::Bind {
            bound,
            compiled,
            variant,
            ..
        } => {
            let warnings = compiled.warnings.warnings.clone();
            let names = bound
                .iter()
                .map(|binder| binder.name.clone())
                .collect::<Vec<_>>();
            let outcome = match bound.as_slice() {
                [] => session.run_with_sites(
                    "actor_interactive_discard_bind",
                    &compiled.expr,
                    &compiled.table,
                    &compiled.asks,
                ),
                [binder] if observation.is_some() => session.run_observation_with_sites(
                    &compiled.expr,
                    &compiled.table,
                    binder,
                    generation,
                    &compiled.asks,
                    variant == 0,
                ),
                [binder] => session.run_bind_with_sites(
                    "actor_interactive_bind",
                    &compiled.expr,
                    &compiled.table,
                    binder,
                    generation,
                    &compiled.asks,
                ),
                binders => session.run_projected_bind_with_sites(
                    "actor_interactive_pattern_bind",
                    &compiled.expr,
                    &compiled.table,
                    binders,
                    generation,
                    &compiled.asks,
                ),
            };
            let display = if let Some(name) = observation {
                WorkbenchDisplay::Observation {
                    name,
                    source: source.clone(),
                    type_modules: scope.type_modules.to_vec(),
                }
            } else if names.is_empty() {
                WorkbenchDisplay::Opaque
            } else {
                WorkbenchDisplay::Binding(names)
            };
            start_fragment_settlement(session, context, block.ordinal, display, warnings, outcome)
        }
        TurnResult::Expr { .. } => Err(ResidentActorWorkbenchError::ActorProtocol(
            "workbench expression compiled without its observation binding".into(),
        )),
    }
}

fn start_fragment_settlement<H, O>(
    session: &mut ResidentSession<H, O>,
    context: &crate::ActorSessionContext,
    input_ordinal: usize,
    display: WorkbenchDisplay,
    warnings: Vec<String>,
    outcome: Result<ResidentOutcome, ResidentError>,
) -> Result<ResidentWorkbenchStep, ResidentActorWorkbenchError>
where
    H: DispatchEffect<O> + Send,
    O: OutputSink + Sync,
{
    match outcome {
        Ok(outcome) => settle_fragment(
            session,
            context,
            ResidentWorkbenchFragment {
                display,
                output: Vec::new(),
                presented: Vec::new(),
                warnings,
            },
            outcome,
        ),
        Err(ResidentError::Run(error)) => Ok(ResidentWorkbenchStep::Rejected(
            render_runtime_rejection(input_ordinal, &error),
        )),
        Err(error) => Err(ResidentActorWorkbenchError::Resident(error)),
    }
}

fn render_runtime_rejection(
    input_ordinal: usize,
    error: &tidepool_runtime::RuntimeError,
) -> String {
    use tidepool_codegen::{jit_machine::JitError, yield_type::YieldError};
    let detail = match error {
        tidepool_runtime::RuntimeError::Jit(JitError::Yield(YieldError::Runtime(cause))) => {
            cause.to_string()
        }
        _ => error.to_string(),
    };
    format!("<input unit {input_ordinal}>: runtime error: {detail}")
}

fn settle_fragment<H, O>(
    session: &mut ResidentSession<H, O>,
    context: &crate::ActorSessionContext,
    mut fragment: ResidentWorkbenchFragment,
    outcome: ResidentOutcome,
) -> Result<ResidentWorkbenchStep, ResidentActorWorkbenchError>
where
    H: DispatchEffect<O> + Send,
    O: OutputSink + Sync,
{
    match outcome {
        ResidentOutcome::Completed { output, .. } => {
            fragment.output.extend(output);
            let installed_bindings = match &fragment.display {
                WorkbenchDisplay::Binding(names) => names.clone(),
                WorkbenchDisplay::Observation { name, .. } => vec![name.clone()],
                WorkbenchDisplay::Opaque => Vec::new(),
            };
            let receipt = match fragment.display {
                WorkbenchDisplay::Binding(names) => format!("[bound {}]", names.join(", ")),
                WorkbenchDisplay::Opaque => "<opaque value>".into(),
                WorkbenchDisplay::Observation {
                    name,
                    source,
                    type_modules,
                } => {
                    let preview = render_observation(
                        session,
                        context,
                        &source,
                        &type_modules,
                        &format!("{name} ()"),
                        ObservationPurpose::Inspection(fragment.presented.clone()),
                    );
                    let (text, omitted) = match preview {
                        Ok(result) => result,
                        Err(error) => (format!("Display failed: {error}\nValue remains bound as {name} (). Inspect a smaller field or projection; execution was not repeated."), false),
                    };
                    let mut text = crate::workbench_display::layout(&text);
                    if omitted {
                        text.push_str(&format!("\nDisplay shortened; captured value retained. Saved: {name} ()\nExpand: inspectFull ({name} ())\nKept among this actor's latest 8 automatic observations; bind explicitly to retain longer."));
                    }
                    text
                }
            };
            let mut transcript = fragment.output.join("\n");
            if !transcript.is_empty() && !receipt.is_empty() {
                transcript.push('\n');
            }
            transcript.push_str(&receipt);
            Ok(ResidentWorkbenchStep::Committed {
                output: transcript,
                warnings: fragment.warnings,
                installed_bindings,
            })
        }
        ResidentOutcome::BindingsCommitted { output } => {
            let bound_name = match &fragment.display {
                WorkbenchDisplay::Binding(names) => Some(names.join(", ")),
                WorkbenchDisplay::Opaque | WorkbenchDisplay::Observation { .. } => None,
            };
            let receipt = projected_binding_receipt(bound_name.as_deref(), &output)?;
            let installed_bindings = match fragment.display {
                WorkbenchDisplay::Binding(names) => names,
                WorkbenchDisplay::Opaque | WorkbenchDisplay::Observation { .. } => Vec::new(),
            };
            fragment.output.push(receipt);
            Ok(ResidentWorkbenchStep::Committed {
                output: fragment.output.join("\n"),
                warnings: fragment.warnings,
                installed_bindings,
            })
        }
        ResidentOutcome::Suspended {
            output,
            hole,
            request,
        } => {
            fragment.output.extend(output);
            let _ = ResidentRequest::decode(&request, session.data_con_table())?;
            Ok(ResidentWorkbenchStep::Running {
                fragment,
                outcome: Box::new(ResidentOutcome::Suspended {
                    output: Vec::new(),
                    hole,
                    request,
                }),
            })
        }
    }
}

#[cfg(test)]
mod activation_preview_tests {
    use super::bounded_activation_text;

    #[test]
    fn preview_bounds_preserve_unicode_and_mark_omission() {
        assert_eq!(
            bounded_activation_text("".into(), 4, false, "inspectFull sessionInput"),
            ""
        );
        assert_eq!(
            bounded_activation_text("a\nλ".into(), 4, false, "inspectFull sessionInput"),
            "a\nλ"
        );
        assert_eq!(
            bounded_activation_text("aλz".into(), 2, true, "inspectFull sessionInput"),
            "a\n<additional detail omitted; expand with `inspectFull sessionInput`>"
        );
        assert_eq!(
            bounded_activation_text("data R".into(), 4, false, ":info R"),
            "data\n<additional detail omitted; expand with `:info R`>"
        );
    }
}

// Bound the demanded character prefix as well as the rendered UTF-8 bytes.
// The final byte clipping can shorten a multibyte prefix further.
const ACTIVATION_INPUT_LIMIT: usize = 16 * 1024;

fn bounded_activation_text(
    mut text: String,
    maximum: usize,
    mut omitted: bool,
    expansion: &str,
) -> String {
    if text.len() > maximum {
        let mut end = maximum;
        while !text.is_char_boundary(end) {
            end -= 1;
        }
        text.truncate(end);
        omitted = true;
    }
    if omitted {
        text.push_str(&format!(
            "\n<additional detail omitted; expand with `{expansion}`>"
        ));
    }
    text
}

enum ObservationPurpose {
    Inspection(Vec<String>),
    Assignment,
}

fn render_observation<H, O>(
    session: &mut ResidentSession<H, O>,
    context: &crate::ActorSessionContext,
    source: &ActorWorkbenchSource,
    type_modules: &[String],
    expression: &str,
    purpose: ObservationPurpose,
) -> Result<(String, bool), ResidentActorWorkbenchError>
where
    H: DispatchEffect<O> + Send,
    O: OutputSink + Sync,
{
    use tidepool_runtime::session::{
        assemble_expression_module, ExpressionLift, TemplateSelector, TurnTemplate,
    };
    let view = actor_compile_view(session, context, source, type_modules)?;
    let prepared = source.prepare(&view);
    let preamble = insert_preamble_imports(&prepared.preamble, &prepared.imports);
    let (renderer, opaque) = match purpose {
        ObservationPurpose::Inspection(keys) => {
            let keys = keys.iter().map(|key| format!("T.pack \"{}\"", tidepool_runtime::session::escape_workbench_haskell_string(key))).collect::<Vec<_>>().join(",");
            (format!("workbenchDisplayWithout [{keys}]"), "(T.pack \"<opaque value>\", True)")
        },
        ObservationPurpose::Assignment => (
            format!("workbenchActivationDisplay {ACTIVATION_INPUT_LIMIT}"),
            "(T.pack \"<opaque value>\\nUse the input type to select fields or apply sessionInput; full printing requires Show.\", False)",
        ),
    };
    let render = format!("TidepoolInspection.{renderer} ({expression})");
    let templates: Vec<_> = [render.as_str(), opaque]
        .into_iter()
        .map(|expression| TurnTemplate {
            kind: TemplateSelector::Expr,
            source: assemble_expression_module(
                &preamble,
                "__result",
                &context.haskell_effects_alias,
                expression,
                ExpressionLift::Pure,
            ),
        })
        .collect();
    let include: Vec<_> = prepared.include.iter().map(PathBuf::as_path).collect();
    let result = run_turn(TurnRequest {
        turn_text: expression,
        templates: &templates,
        include: &include,
        session_root: view.session_root(),
        inject_modules: &prepared.injected,
        gen: view.next_value_generation().0,
        verdict: Some(TurnClassification {
            kind: TurnKind::Expr,
            binders: Vec::new(),
            items: Vec::new(),
        }),
        target: None,
    })
    .map_err(|failure| {
        ResidentActorWorkbenchError::Inspection(render_turn_compile_error(
            &failure.error,
            failure.attempted_source.as_deref(),
            expression,
            "<inspection>",
        ))
    })?;
    let TurnResult::Expr { compiled, .. } = result else {
        return Err(ResidentActorWorkbenchError::Inspection(
            "preview did not compile as an expression".into(),
        ));
    };
    match session
        .run_inspection_with_sites(&compiled.expr, &compiled.table, &compiled.asks)
        .map_err(ResidentActorWorkbenchError::Resident)?
    {
        ResidentOutcome::Completed { result, .. } => {
            <(String, bool)>::from_value(result.value(), result.table())
                .map_err(|error| ResidentActorWorkbenchError::Inspection(error.to_string()))
        }
        _ => Err(ResidentActorWorkbenchError::Inspection(
            "pure preview unexpectedly suspended".into(),
        )),
    }
}

impl<H, O> ResidentActorRunner<H, O>
where
    H: DispatchEffect<O> + Send + 'static,
    O: OutputSink + Sync + 'static,
{
    pub(crate) async fn retire_fork_scopes(
        &self,
        context: crate::ActorSessionContext,
        scopes: Vec<tidepool_codegen::scope::ScopeId>,
    ) -> Result<(), ResidentActorWorkbenchError> {
        self.access
            .with_machine(context, move |session, _, _| {
                for scope in scopes {
                    session.retire_scope(scope);
                }
                Ok(())
            })
            .await
    }

    pub(crate) async fn finalize_fork_scopes(
        &self,
        context: crate::ActorSessionContext,
        previous: Vec<(tidepool_codegen::scope::ScopeId, crate::ForkContext)>,
    ) -> Result<Vec<tidepool_codegen::scope::ScopeId>, ResidentActorWorkbenchError> {
        self.access
            .with_machine(context, move |session, context, _| {
                let mut scopes = Vec::with_capacity(previous.len());
                for (old, ancestry) in previous {
                    if ancestry == crate::ForkContext::SelectedContext {
                        scopes.push(old);
                        continue;
                    }
                    let scope = session
                        .mint_scope(context.placement.lexical_scope)
                        .ok_or_else(|| {
                            ResidentActorWorkbenchError::ActorProtocol(
                                "fork parent scope was retired".into(),
                            )
                        })?;
                    session.retire_scope(old);
                    scopes.push(scope);
                }
                Ok(scopes)
            })
            .await
    }

    pub(crate) async fn capture_boundary(
        &self,
        context: crate::ActorSessionContext,
        outcome: ResidentOutcome,
        actor_realm: RealmId,
    ) -> Result<ResidentActorBoundary, ResidentActorWorkbenchError> {
        self.capture_boundary_mode(context, outcome, actor_realm, BoundaryCapture::Execution)
            .await
    }

    pub(crate) async fn capture_replacement_boundary(
        &self,
        context: crate::ActorSessionContext,
        outcome: ResidentOutcome,
        actor_realm: RealmId,
    ) -> Result<ResidentActorBoundary, ResidentActorWorkbenchError> {
        self.capture_boundary_mode(context, outcome, actor_realm, BoundaryCapture::Replacement)
            .await
    }

    async fn capture_boundary_mode(
        &self,
        context: crate::ActorSessionContext,
        outcome: ResidentOutcome,
        actor_realm: RealmId,
        mode: BoundaryCapture,
    ) -> Result<ResidentActorBoundary, ResidentActorWorkbenchError> {
        let (hole, request) = match outcome {
            ResidentOutcome::Completed { .. } | ResidentOutcome::BindingsCommitted { .. } => {
                return Ok(ResidentActorBoundary::Completed);
            }
            ResidentOutcome::Suspended { hole, request, .. } => (hole, request),
        };

        self.access
            .with_machine(context, move |session, context, _| {
                let decoded = ResidentRequest::decode(&request, session.data_con_table())?;
                if matches!(mode, BoundaryCapture::Replacement)
                    && !matches!(&decoded, ResidentRequest::ActorLocal(
                        crate::generated::actor_local::ActorLocalReq::ActorCheckpointWith(..)
                        | crate::generated::actor_local::ActorLocalReq::ActorReceiveWith(..)
                    ))
                {
                    return Err(ResidentActorWorkbenchError::ActorProtocol(format!(
                        "replacement staging cannot execute `{}`", decoded.operation()
                    )));
                }
                match decoded {
                    ResidentRequest::ActorContext(
                        crate::generated::actor_context::ActorContextReq::ActorContextWith,
                    ) => Ok(ResidentActorBoundary::ActorContext(hole)),
                    ResidentRequest::AgentLaunch(
                        crate::generated::agent_launch::AgentLaunchReq::AgentLaunchWith(
                            label,
                            _,
                            role,
                            profile,
                            worktrees,
                        ),
                    ) => crate::ResidentActorStart::capture_decoded(
                        session,
                        hole,
                        crate::start::ActorStartRequest {
                            label, role, profile, launch_worktrees: worktrees,
                            fork_group: None, fork_workspace: None, effect_keys: None,
                            fork_effort: None, fork_budget: None, model: None, instructions: None, context: crate::ForkContext::SelectedContext,
                            lifetime: crate::WorkerLifetime::ParentOwned,
                            session_id: context.placement.session, parent_actor: context.actor,
                        },
                    )
                    .map(ResidentActorBoundary::Start)
                    .map_err(ResidentActorWorkbenchError::StartCapture),
                    ResidentRequest::Forks(crate::generated::forks::ForksReq::ForksStartWith(
                        label,
                        _,
                        group,
                        role,
                        profile,
                        worktrees,
                        worktree_spec,
                        bound_dirty_policy,
                        effect_keys,
                        effort,
                        budget,
                        model, fork_context, instructions, lifetime,
                    )) => {
                        let group = u64::try_from(group).map_err(|_| {
                            ResidentActorWorkbenchError::ActorProtocol(format!(
                                "invalid fork group id {group}"
                            ))
                        })?;
                        crate::ResidentActorStart::capture_decoded(
                            session,
                            hole,
                            crate::start::ActorStartRequest {
                                label, role, profile, launch_worktrees: worktrees,
                                fork_group: Some(crate::ForkGroupId(group)),
                                fork_workspace: Some(match worktree_spec {
                                    Some(spec) => crate::ForkWorkspaceSeed::Explicit(spec),
                                    None => crate::ForkWorkspaceSeed::BoundHead(bound_dirty_policy),
                                }),
                                effect_keys: Some(effect_keys), fork_effort: effort, fork_budget: budget, model, instructions, context: fork_context, lifetime,
                                session_id: context.placement.session, parent_actor: context.actor,
                            },
                        )
                        .map(ResidentActorBoundary::Start)
                        .map_err(ResidentActorWorkbenchError::StartCapture)
                    }
                    ResidentRequest::Forks(crate::generated::forks::ForksReq::ForksPreviewWith(role, effect_keys, budget, model, effort, context, instructions, lifetime)) => Ok(ResidentActorBoundary::ForkGroup(ForkGroupBoundary::Preview { continuation: hole, role, effect_keys, budget, model, effort, context, instructions, lifetime })),
                    ResidentRequest::Forks(crate::generated::forks::ForksReq::ForksBeginWith(
                        relative,
                        group,
                        branches,
                    )) => Ok(ResidentActorBoundary::ForkGroup(ForkGroupBoundary::Begin {
                        continuation: hole,
                        relative,
                        group,
                        branches,
                    })),
                    ResidentRequest::Forks(crate::generated::forks::ForksReq::ForksCommitWith(
                        group,
                    )) => Ok(ResidentActorBoundary::ForkGroup(
                        ForkGroupBoundary::Commit {
                            continuation: hole,
                            group: crate::ForkGroupId(u64::try_from(group).map_err(|_| {
                                ResidentActorWorkbenchError::ActorProtocol(format!(
                                    "invalid fork group id {group}"
                                ))
                            })?),
                        },
                    )),
                    ResidentRequest::Forks(crate::generated::forks::ForksReq::ForksAbortWith(
                        group,
                    )) => Ok(ResidentActorBoundary::ForkGroup(ForkGroupBoundary::Abort {
                        continuation: hole,
                        group: crate::ForkGroupId(u64::try_from(group).map_err(|_| {
                            ResidentActorWorkbenchError::ActorProtocol(format!(
                                "invalid fork group id {group}"
                            ))
                        })?),
                    })),
                    ResidentRequest::Forks(
                        crate::generated::forks::ForksReq::ForksCleanupWith(group),
                    ) => Ok(ResidentActorBoundary::ForkGroup(
                        ForkGroupBoundary::Cleanup {
                            continuation: hole,
                            group: crate::ForkGroupId(u64::try_from(group).map_err(|_| {
                                ResidentActorWorkbenchError::ActorProtocol(format!(
                                    "invalid fork group id {group}"
                                ))
                            })?),
                        },
                    )),
                    ResidentRequest::Notifications(crate::generated::notifications::NotificationsReq::NotifyWith(target, message)) => {
                        Ok(ResidentActorBoundary::NotificationSend { continuation: hole, target: crate::wait::decode_address(target.0, target.1)?, message })
                    }
                    ResidentRequest::Notifications(crate::generated::notifications::NotificationsReq::PollNotificationWith(receipt)) => {
                        Ok(ResidentActorBoundary::NotificationPoll { continuation: hole, receipt })
                    }
                    ResidentRequest::AgentInspection(
                        crate::generated::agent_inspection::AgentInspectionReq::AgentInspectWith(
                            target,
                        ),
                    ) => Ok(ResidentActorBoundary::AgentInspect(
                        AgentInspectionBoundary {
                            target: crate::wait::decode_address(target.0, target.1)?,
                            continuation: hole,
                        },
                    )),
                    ResidentRequest::AgentInspection(
                        crate::generated::agent_inspection::AgentInspectionReq::AgentListWith,
                    ) => Ok(ResidentActorBoundary::AgentList(hole)),
                    ResidentRequest::AgentInspection(crate::generated::agent_inspection::AgentInspectionReq::AgentShareObservationWith(recipient, scope)) => Ok(ResidentActorBoundary::AgentShareObservation {
                        continuation: hole,
                        recipient: crate::wait::decode_address(recipient.0, recipient.1)?,
                        scope: crate::wait::decode_address(scope.0, scope.1)?,
                    }),
                    ResidentRequest::AgentInspection(
                        crate::generated::agent_inspection::AgentInspectionReq::AgentGroupListWith(group),
                    ) => Ok(ResidentActorBoundary::AgentGroupList {
                        continuation: hole,
                        group: crate::ForkGroupId(u64::try_from(group).map_err(|_| {
                            ResidentActorWorkbenchError::ActorProtocol(format!("invalid fork group id {group}"))
                        })?),
                    }),
                    ResidentRequest::AgentInspection(
                        crate::generated::agent_inspection::AgentInspectionReq::AgentForgetWith(
                            target,
                        ),
                    ) => Ok(ResidentActorBoundary::AgentForget(
                        AgentInspectionBoundary {
                            target: crate::wait::decode_address(target.0, target.1)?,
                            continuation: hole,
                        },
                    )),
                    ResidentRequest::Commands(request) => Ok(ResidentActorBoundary::Command { continuation: hole, request }),
                    ResidentRequest::AgentControl(
                        crate::generated::agent_control::AgentControlReq::AgentControlStopWith(
                            target,
                        ),
                    ) => Ok(ResidentActorBoundary::AgentStop(AgentInspectionBoundary {
                        target: crate::wait::decode_address(target.0, target.1)?,
                        continuation: hole,
                    })),
                    ResidentRequest::AgentInspection(
                        crate::generated::agent_inspection::AgentInspectionReq::AgentInspectCleanupWith(
                            group,
                        ),
                    ) => Ok(ResidentActorBoundary::CleanupPlan {
                        continuation: hole,
                        group: crate::ForkGroupId(u64::try_from(group).map_err(|_| {
                            ResidentActorWorkbenchError::ActorProtocol(format!(
                                "invalid cleanup group id {group}"
                            ))
                        })?),
                    }),
                    ResidentRequest::AgentControl(
                        crate::generated::agent_control::AgentControlReq::AgentControlExecuteCleanupWith(
                            group, inspected,
                        ),
                    ) => Ok(ResidentActorBoundary::CleanupExecute {
                        continuation: hole,
                        inspected: inspected.into_iter().map(|(id, incarnation, revision)| {
                            Ok((crate::wait::decode_address(id, incarnation)?, u64::try_from(revision).map_err(|_| {
                                ResidentActorWorkbenchError::ActorProtocol("invalid cleanup revision".into())
                            })?))
                        }).collect::<Result<_, ResidentActorWorkbenchError>>()?,
                        group: crate::ForkGroupId(u64::try_from(group).map_err(|_| {
                            ResidentActorWorkbenchError::ActorProtocol(format!(
                                "invalid cleanup group id {group}"
                            ))
                        })?),
                    }),
                    ResidentRequest::Actor(
                        crate::generated::actor::ActorReq::ActorStartWith(..)
                        | crate::generated::actor::ActorReq::ActorForkWith(..),
                    ) => {
                        let table = session.data_con_table().clone();
                        crate::ResidentActorStart::capture(
                            session,
                            hole,
                            &request,
                            &table,
                            context.placement.session,
                            context.actor,
                        )
                        .map(ResidentActorBoundary::Start)
                        .map_err(ResidentActorWorkbenchError::StartCapture)
                    }
                    ResidentRequest::Actor(
                        crate::generated::actor::ActorReq::ActorBeginForkGroupWith(
                            relative,
                            group,
                            branches,
                        ),
                    ) => Ok(ResidentActorBoundary::ForkGroup(ForkGroupBoundary::Begin {
                        continuation: hole,
                        relative,
                        group,
                        branches,
                    })),
                    ResidentRequest::Actor(
                        crate::generated::actor::ActorReq::ActorCommitForkGroupWith(group),
                    ) => Ok(ResidentActorBoundary::ForkGroup(
                        ForkGroupBoundary::Commit {
                            continuation: hole,
                            group: crate::ForkGroupId(u64::try_from(group).map_err(|_| {
                                ResidentActorWorkbenchError::ActorProtocol(format!(
                                    "invalid fork group id {group}"
                                ))
                            })?),
                        },
                    )),
                    ResidentRequest::Actor(
                        crate::generated::actor::ActorReq::ActorAbortForkGroupWith(group),
                    ) => Ok(ResidentActorBoundary::ForkGroup(ForkGroupBoundary::Abort {
                        continuation: hole,
                        group: crate::ForkGroupId(u64::try_from(group).map_err(|_| {
                            ResidentActorWorkbenchError::ActorProtocol(format!(
                                "invalid fork group id {group}"
                            ))
                        })?),
                    })),
                    ResidentRequest::Actor(crate::generated::actor::ActorReq::ActorCallWith(
                        target,
                        _,
                    )) => capture_outbound_boundary(
                        session,
                        context,
                        hole,
                        target,
                        OutboundKind::Call,
                        actor_realm,
                    ),
                    ResidentRequest::Actor(
                        crate::generated::actor::ActorReq::ActorTryCallWith(target, _),
                    ) => capture_outbound_boundary(
                        session,
                        context,
                        hole,
                        target,
                        OutboundKind::TryCall,
                        actor_realm,
                    ),
                    ResidentRequest::Actor(crate::generated::actor::ActorReq::ActorCastWith(
                        target,
                        _,
                    )) => capture_outbound_boundary(
                        session,
                        context,
                        hole,
                        target,
                        OutboundKind::Cast,
                        actor_realm,
                    ),
                    ResidentRequest::Actor(crate::generated::actor::ActorReq::ActorReplaceWith(target, ..)) => {
                        let target = crate::wait::decode_address(target.0, target.1)?;
                        let table = session.data_con_table().clone();
                        crate::ResidentActorStart::capture(
                            session,
                            hole,
                            &request,
                            &table,
                            context.placement.session,
                            context.actor,
                        )
                        .map(|candidate| ResidentActorBoundary::Replace { target, candidate })
                        .map_err(ResidentActorWorkbenchError::StartCapture)
                    }
                    ResidentRequest::Actor(crate::generated::actor::ActorReq::ActorDrainWith(target)) => {
                        Ok(ResidentActorBoundary::Drain {
                            continuation: hole,
                            target: crate::wait::decode_address(target.0, target.1)?,
                        })
                    }
                    ResidentRequest::Actor(crate::generated::actor::ActorReq::ActorWaitWith(
                        ..,
                    )) => {
                        let target =
                            crate::wait::decode_wait_target(&request, session.data_con_table())?;
                        Ok(ResidentActorBoundary::Wait(ResidentWaitRequest {
                            target,
                            continuation: hole,
                        }))
                    }
                    ResidentRequest::Actor(crate::generated::actor::ActorReq::ActorPollWith(
                        ..,
                    )) => {
                        let target =
                            crate::wait::decode_poll_target(&request, session.data_con_table())?;
                        Ok(ResidentActorBoundary::Poll(
                            crate::wait::ResidentPollRequest {
                                target,
                                continuation: hole,
                            },
                        ))
                    }
                    ResidentRequest::ActorLocal(crate::generated::actor_local::ActorLocalReq::ActorLocalContextWith) =>
                        Ok(ResidentActorBoundary::ActorLocalContext(hole)),
                    ResidentRequest::ActorLocal(
                        crate::generated::actor_local::ActorLocalReq::ActorReceiveWith(site, _),
                    ) => capture_receiver_boundary(session, hole, site, actor_realm),
                    ResidentRequest::ActorLocal(
                        crate::generated::actor_local::ActorLocalReq::ActorCheckpointWith(site, _),
                    ) => {
                        let site = u64::try_from(site).map_err(|_| {
                            ResidentActorWorkbenchError::ActorProtocol("negative checkpoint site".into())
                        })?;
                        if session.parked_realm(&hole) != Some(actor_realm) {
                            return Err(ResidentActorWorkbenchError::ActorProtocol(
                                "checkpoint escaped its actor program".into(),
                            ));
                        }
                        let value = session
                            .live_payload_handle_owned_by(hole.cont_id(), actor_realm)
                            .ok_or_else(|| ResidentActorWorkbenchError::ActorProtocol(
                                "checkpoint carried no state".into(),
                            ))?;
                        Ok(ResidentActorBoundary::Checkpoint {
                            continuation: hole,
                            site,
                            value,
                        })
                    }
                    ResidentRequest::AgentTools(
                        crate::generated::agent_tools::AgentToolsReq::AgentToolsAwaitWith(
                            declarations,
                            synopsis,
                            initial_user_message,
                        ),
                    ) => {
                        let declarations = tidepool_runtime::value_to_json(
                            &declarations,
                            session.data_con_table(),
                            0,
                        );
                        let declarations = serde_json::from_value(declarations)
                            .map_err(ResidentActorWorkbenchError::ToolDeclarations)?;
                        Ok(ResidentActorBoundary::ToolAwait(
                            crate::resident_tools::ResidentToolAwait {
                                continuation: hole,
                                declarations,
                                synopsis,
                                initial_user_message,
                            },
                        ))
                    }
                    ResidentRequest::AgentTools(
                        crate::generated::agent_tools::AgentToolsReq::AgentToolsReplyWith(result),
                    ) => Ok(ResidentActorBoundary::ToolReply(
                        crate::resident_tools::ResidentToolReply {
                            continuation: hole,
                            result: tidepool_runtime::value_to_json(
                                &result,
                                session.data_con_table(),
                                0,
                            ),
                        },
                    )),
                    ResidentRequest::AgentSession(
                        crate::generated::agent_session::AgentSessionReq::AgentSessionWith(..),
                    ) => {
                        let table = session.data_con_table().clone();
                        crate::ResidentInteractiveSession::capture(
                            session,
                            hole,
                            &request,
                            &table,
                            actor_realm,
                        )
                        .map(ResidentActorBoundary::AgentSession)
                        .map_err(ResidentActorWorkbenchError::InteractiveSessionCapture)
                    }
                    ResidentRequest::Replies(RepliesReq::ReserveRequestWith(label, address)) => Ok(
                        ResidentActorBoundary::RequestReservation(RequestReservation {
                            continuation: hole,
                            target: crate::wait::decode_address(address.0, address.1)?,
                            label,
                        }),
                    ),
                    ResidentRequest::Replies(RepliesReq::SubmitRequestWith(
                        request_id,
                        _,
                        address,
                        deadline,
                    )) => {
                        let custody = session
                            .live_payload_handle_owned_by(hole.cont_id(), actor_realm)
                            .ok_or_else(|| {
                                ResidentActorWorkbenchError::ActorProtocol(
                                    "request submission suspended without its live payload".into(),
                                )
                            })?;
                        Ok(ResidentActorBoundary::RequestSubmission(
                            RequestSubmission {
                                continuation: hole,
                                request: crate::request_effect::request_id(request_id)?,
                                target: crate::wait::decode_address(address.0, address.1)?,
                                message: crate::MailboxValue::new(
                                    context.placement.session,
                                    custody,
                                ),
                                deadline: deadline
                                    .map(crate::request_effect::RequestDuration::checked)
                                    .transpose()
                                    .map_err(ResidentActorWorkbenchError::ActorProtocol)?,
                            },
                        ))
                    }
                    ResidentRequest::Replies(RepliesReq::AttemptReplyWith(request_id, _)) => {
                        let result = session
                            .live_payload_handle_owned_by(hole.cont_id(), actor_realm)
                            .ok_or_else(|| {
                                ResidentActorWorkbenchError::ActorProtocol(
                                    "reply suspended without its live result".into(),
                                )
                            })?;
                        Ok(ResidentActorBoundary::ReplyAttempt(ReplyAttempt {
                            continuation: hole,
                            request: crate::request_effect::request_id(request_id)?,
                            result,
                            recoverable: true,
                        }))
                    }
                    ResidentRequest::Replies(RepliesReq::ReplyWith(request_id, _)) => {
                        let result = session
                            .live_payload_handle_owned_by(hole.cont_id(), actor_realm)
                            .ok_or_else(|| {
                                ResidentActorWorkbenchError::ActorProtocol(
                                    "reply suspended without its live result".into(),
                                )
                            })?;
                        Ok(ResidentActorBoundary::ReplyAttempt(ReplyAttempt {
                            continuation: hole,
                            request: crate::request_effect::request_id(request_id)?,
                            result,
                            recoverable: false,
                        }))
                    }
                    ResidentRequest::Replies(RepliesReq::ObserveResponseWith(request_id)) => {
                        Ok(ResidentActorBoundary::ResponsePoll(ResponsePoll {
                            continuation: hole,
                            request: crate::request_effect::request_id(request_id)?,
                        }))
                    }
                    ResidentRequest::Replies(RepliesReq::PublishProgressWith(request_id, _)) => {
                        // Request/watch custody can outlive the publishing actor.
                        // Arc<RootCustody> releases this shared-machine root when
                        // the last registry or watch snapshot stops retaining it.
                        let value = session.live_payload_handle_owned_by(hole.cont_id(), RealmId::ROOT)
                            .ok_or_else(|| ResidentActorWorkbenchError::ActorProtocol("progress publication has no live payload".into()))?;
                        Ok(ResidentActorBoundary::ProgressPublication {
                            continuation: hole, request: crate::request_effect::request_id(request_id)?, value,
                        })
                    }
                    ResidentRequest::Replies(RepliesReq::ObserveProgressWith(request_id)) => {
                        Ok(ResidentActorBoundary::ProgressPoll(ResponsePoll {
                            continuation: hole, request: crate::request_effect::request_id(request_id)?,
                        }))
                    }
                    ResidentRequest::Replies(RepliesReq::UpdateRequestWith(request, message)) => {
                        Ok(ResidentActorBoundary::RequestUpdate { continuation: hole,
                            request: crate::request_effect::request_id(request)?, message })
                    }
                    ResidentRequest::Replies(RepliesReq::ObserveRequestUpdateWith(request, sequence)) => {
                        Ok(ResidentActorBoundary::RequestUpdatePoll { continuation: hole,
                            update: crate::RequestUpdateId { request: crate::request_effect::request_id(request)?,
                                sequence: u64::try_from(sequence).map_err(|_| ResidentActorWorkbenchError::ActorProtocol("invalid update sequence".into()))? } })
                    }
                    ResidentRequest::Replies(RepliesReq::CancelRequestWith(request_id)) => Ok(
                        ResidentActorBoundary::RequestCancellation(RequestCancellation {
                            continuation: hole,
                            request: crate::request_effect::request_id(request_id)?,
                        }),
                    ),
                    ResidentRequest::Replies(RepliesReq::AbandonResponseWith(request_id)) => Ok(
                        ResidentActorBoundary::ResponseAbandonment(ResponseAbandonment {
                            continuation: hole,
                            request: crate::request_effect::request_id(request_id)?,
                        }),
                    ),
                    ResidentRequest::Replies(RepliesReq::ForgetResponseWith(request_id)) => {
                        Ok(ResidentActorBoundary::ResponseForget(ResponseForget {
                            continuation: hole,
                            request: crate::request_effect::request_id(request_id)?,
                        }))
                    }
                    ResidentRequest::Replies(RepliesReq::ObserveReplyWith(request_id)) => {
                        Ok(ResidentActorBoundary::ReplyPoll(ReplyPoll {
                            continuation: hole,
                            request: crate::request_effect::request_id(request_id)?,
                        }))
                    }
                    ResidentRequest::Replies(RepliesReq::AttemptAcknowledgeCancellationWith(
                        request_id,
                    )) => Ok(ResidentActorBoundary::CancellationAcknowledgement(
                        CancellationAcknowledgement {
                            continuation: hole,
                            request: crate::request_effect::request_id(request_id)?,
                            recoverable: true,
                        },
                    )),
                    ResidentRequest::Replies(RepliesReq::AcknowledgeCancellationWith(
                        request_id,
                    )) => Ok(ResidentActorBoundary::CancellationAcknowledgement(
                        CancellationAcknowledgement {
                            continuation: hole,
                            request: crate::request_effect::request_id(request_id)?,
                            recoverable: false,
                        },
                    )),
                    ResidentRequest::Watches(WatchesReq::RegisterRouteWith(label, callback, dependencies)) => {
                        drop(callback); // Custody is claimed from the suspension, not the decoded value.
                        let entry = session.live_payload_handle_owned_by(hole.cont_id(), context.placement.resource_scope)
                            .ok_or_else(|| ResidentActorWorkbenchError::ActorProtocol("route has no retained callback".into()))?;
                        let dependencies = dependencies.into_iter().map(|dependency| {
                            Ok(vec![crate::request_effect::AwaitDependency::checked(dependency)?])
                        }).collect::<Result<Vec<Vec<_>>, tidepool_bridge::BridgeError>>()?;
                        Ok(ResidentActorBoundary::RouteRegistration {
                            registration: WatchRegistration { continuation: hole, label, dependencies }, entry,
                        })
                    }
                    ResidentRequest::Watches(WatchesReq::RegisterRouteGroupsWith(label, callback, groups)) => {
                        drop(callback); // Custody is claimed from the suspension, not the decoded value.
                        let entry = session.live_payload_handle_owned_by(hole.cont_id(), context.placement.resource_scope)
                            .ok_or_else(|| ResidentActorWorkbenchError::ActorProtocol("route has no retained callback".into()))?;
                        let dependencies = groups.into_iter().map(|dependencies| dependencies.into_iter().map(crate::request_effect::AwaitDependency::checked).collect()).collect::<Result<Vec<Vec<_>>, _>>()?;
                        Ok(ResidentActorBoundary::RouteRegistration {
                            registration: WatchRegistration { continuation: hole, label, dependencies }, entry,
                        })
                    }
                    ResidentRequest::Watches(WatchesReq::ListRoutesWith) => Ok(ResidentActorBoundary::RouteList(hole)),
                    ResidentRequest::Watches(WatchesReq::ObserveRouteWith(id)) => Ok(ResidentActorBoundary::RoutePoll(WatchPoll {
                        continuation: hole, watch: crate::request_effect::watch_id(id)?,
                    })),
                    ResidentRequest::Watches(WatchesReq::RegisterWatchWith(
                        label,
                        dependencies,
                    )) => {
                        let dependencies = dependencies
                            .into_iter()
                            .map(|dependency| {
                                Ok(vec![crate::request_effect::AwaitDependency::checked(dependency)?])
                            })
                            .collect::<Result<Vec<Vec<_>>, tidepool_bridge::BridgeError>>()?;
                        Ok(ResidentActorBoundary::WatchRegistration(
                            WatchRegistration {
                                continuation: hole,
                                dependencies,
                                label,
                            },
                        ))
                    }
                    ResidentRequest::Watches(WatchesReq::RegisterWatchGroupsWith(label, groups)) => {
                        let dependencies = groups
                            .into_iter()
                            .map(|dependencies| dependencies.into_iter().map(crate::request_effect::AwaitDependency::checked).collect())
                            .collect::<Result<Vec<Vec<_>>, _>>()?;
                        Ok(ResidentActorBoundary::WatchRegistration(
                            WatchRegistration { continuation: hole, dependencies, label },
                        ))
                    }
                    ResidentRequest::Watches(WatchesReq::ObserveWatchWith(watch_id)) => {
                        Ok(ResidentActorBoundary::WatchPoll(WatchPoll {
                            continuation: hole,
                            watch: crate::request_effect::watch_id(watch_id)?,
                        }))
                    }
                    ResidentRequest::Watches(WatchesReq::ObserveWatchProgressWith(watch, request, after)) => {
                        Ok(ResidentActorBoundary::WatchProgressPoll {
                            continuation: hole,
                            watch: crate::request_effect::watch_id(watch)?,
                            request: crate::request_effect::request_id(request)?,
                            after: u64::try_from(after).map_err(|_| ResidentActorWorkbenchError::ActorProtocol("negative progress cursor".into()))?,
                        })
                    }
                    ResidentRequest::Watches(WatchesReq::ForgetWatchWith(watch_id)) => {
                        Ok(ResidentActorBoundary::WatchForget(WatchForget {
                            continuation: hole,
                            watch: crate::request_effect::watch_id(watch_id)?,
                        }))
                    }
                    ResidentRequest::AgentSession(
                        crate::generated::agent_session::AgentSessionReq::AgentAttachWith(
                            initial_user_message,
                        ),
                    ) => Ok(ResidentActorBoundary::AgentAttachment(
                        ResidentAgentAttachment {
                            continuation: hole,
                            initial_user_message,
                        },
                    )),
                    invalid => Err(ResidentActorWorkbenchError::ActorProtocol(format!(
                        "installed actor program suspended on phase-invalid `{}`",
                        invalid.operation()
                    ))),
                }
            })
            .await
    }

    /// Claim and seal the live child entry carried by one public `startActor`
    /// suspension. The same checked-out operation derives its exact source
    /// facade, mints an isolated lexical scope, and rehomes the entry into the
    /// unpublished child's fresh resource realm.
    pub async fn capture_start(
        &self,
        context: crate::ActorSessionContext,
        outcome: ResidentOutcome,
    ) -> Result<crate::ResidentActorStart, ResidentActorWorkbenchError> {
        let actor_realm = context.placement.resource_scope;
        let boundary = self.capture_boundary(context, outcome, actor_realm).await?;
        match boundary {
            ResidentActorBoundary::Start(start) => Ok(start),
            other => Err(unexpected_boundary("startActor", &other)),
        }
    }

    pub async fn run_rooted_entry(
        &self,
        context: crate::ActorSessionContext,
        entry: RootCustody,
        realm: RealmId,
    ) -> Result<ResidentOutcome, ResidentActorWorkbenchError> {
        self.access
            .with_machine(context, move |session, _context, _| {
                session
                    .run_rooted_entry("actor_program", entry, 0, realm, None)
                    .map_err(ResidentActorWorkbenchError::Resident)
            })
            .await
    }

    pub(crate) async fn capture_startup_step(
        &self,
        context: crate::ActorSessionContext,
        outcome: ResidentOutcome,
        actor_realm: RealmId,
    ) -> Result<ResidentActorStartupStep, ResidentActorWorkbenchError> {
        let ResidentOutcome::Suspended { hole, request, .. } = outcome else {
            return Err(ResidentActorWorkbenchError::ActorProtocol(
                "actor startup completed without reaching readiness".into(),
            ));
        };
        self.access
            .with_machine(context, move |session, _, _| {
                let request_kind = ResidentRequest::decode(&request, session.data_con_table())?;
                use crate::request::sources::{SourceTarget, RequestSourceKind};
                let source = match &request_kind {
                    ResidentRequest::ActorKernel(crate::generated::actor_kernel::ActorKernelReq::ActorInstallProgressSourceWith(request, _)) => Some(SourceTarget::Request(source_request_id(*request)?, RequestSourceKind::Progress)),
                    ResidentRequest::ActorKernel(crate::generated::actor_kernel::ActorKernelReq::ActorInstallSettlementSourceWith(request, _)) => Some(SourceTarget::Request(source_request_id(*request)?, RequestSourceKind::Settlement)),
                    ResidentRequest::ActorKernel(crate::generated::actor_kernel::ActorKernelReq::ActorInstallCommandSourceWith(job, _)) => Some(SourceTarget::Command(uuid::Uuid::parse_str(job).map_err(|_| ResidentActorWorkbenchError::ActorProtocol("invalid command job handle".into()))?.as_u128())),
                    ResidentRequest::ActorKernel(crate::generated::actor_kernel::ActorKernelReq::ActorInstallLifecycleSourceWith((id, incarnation), _)) => Some(SourceTarget::Lifecycle(crate::ActorRef {
                        id: crate::ActorId(u64::try_from(*id).map_err(|_| ResidentActorWorkbenchError::ActorProtocol("invalid lifecycle actor id".into()))?),
                        incarnation: crate::Incarnation(u64::try_from(*incarnation).map_err(|_| ResidentActorWorkbenchError::ActorProtocol("invalid lifecycle incarnation".into()))?),
                    })),
                    _ => None,
                };
                if let Some(target) = source {
                    if session.parked_realm(&hole) != Some(actor_realm) {
                        return Err(ResidentActorWorkbenchError::ActorProtocol("source installation crossed actor realm".into()));
                    }
                    let entry = session.live_payload_handle_owned_by(hole.cont_id(), actor_realm).ok_or_else(|| ResidentActorWorkbenchError::ActorProtocol("source has no live mapping closure".into()))?;
                    return Ok(ResidentActorStartupStep::InstallSource {
                        continuation: hole,
                        source: crate::request::sources::SourceBinding { target, entry: Arc::new(entry) },
                    });
                }
                match request_kind {
                    ResidentRequest::ActorKernel(
                        crate::generated::actor_kernel::ActorKernelReq::ActorInstallShutdownWith(
                            ..,
                        ),
                    ) if session.parked_realm(&hole) == Some(actor_realm) => {
                        let hook = session
                            .live_payload_handle_owned_by(hole.cont_id(), actor_realm)
                            .ok_or_else(|| {
                                ResidentActorWorkbenchError::ActorProtocol(
                                    "shutdown registration carried no live hook".into(),
                                )
                            })?;
                        Ok(ResidentActorStartupStep::InstallShutdown(
                            ResidentActorShutdown {
                                continuation: hole,
                                hook,
                            },
                        ))
                    }
                    ResidentRequest::ActorKernel(
                        crate::generated::actor_kernel::ActorKernelReq::ActorReadyWith,
                    ) if session.parked_realm(&hole) == Some(actor_realm) => {
                        Ok(ResidentActorStartupStep::Ready(ResidentActorReadiness {
                            hole,
                        }))
                    }
                    ResidentRequest::AgentSession(
                        crate::generated::agent_session::AgentSessionReq::AgentAttachWith(
                            initial_user_message,
                        ),
                    ) if session.parked_realm(&hole) == Some(actor_realm) => {
                        Ok(ResidentActorStartupStep::Attach(ResidentAgentAttachment {
                            continuation: hole,
                            initial_user_message,
                        }))
                    }
                    unsupported => Err(ResidentActorWorkbenchError::ActorProtocol(format!(
                        "actor initialization suspended on unsupported `{}` in {actor_realm:?}",
                        unsupported.operation()
                    ))),
                }
            })
            .await
    }

    pub(crate) async fn run_shutdown(
        &self,
        context: crate::ActorSessionContext,
        hook: RootCustody,
        realm: RealmId,
        reason: crate::ActorExitKind,
        admission_timeout: Duration,
    ) -> Result<(), ResidentActorWorkbenchError> {
        let argument = match reason {
            crate::ActorExitKind::Completed => 0,
            crate::ActorExitKind::Failed => 1,
            crate::ActorExitKind::Cancelled => 2,
        };
        self.access
            .with_machine_wait(
                context,
                Some(admission_timeout),
                move |session, _context, _| match session
                    .run_rooted_entry("actor_shutdown", hook, argument, realm, None)
                    .map_err(ResidentActorWorkbenchError::Resident)?
                {
                    ResidentOutcome::Completed { .. } => Ok(()),
                    ResidentOutcome::BindingsCommitted { .. } => {
                        Err(ResidentActorWorkbenchError::ActorProtocol(
                            "shutdown completed as an impossible projected binding".into(),
                        ))
                    }
                    ResidentOutcome::Suspended { request, .. } => {
                        let request = ResidentRequest::decode(&request, session.data_con_table())?;
                        Err(ResidentActorWorkbenchError::ActorProtocol(format!(
                            "shutdown suspended on disallowed `{}`",
                            request.operation()
                        )))
                    }
                },
            )
            .await
    }

    pub(crate) async fn resume_readiness(
        &self,
        context: crate::ActorSessionContext,
        readiness: ResidentActorReadiness,
    ) -> Result<ResidentOutcome, ResidentActorWorkbenchError> {
        self.access
            .with_machine(context, move |session, _, _| {
                let answer = ().to_value(session.data_con_table())?;
                session
                    .resume(readiness.hole, answer)
                    .map_err(ResidentActorWorkbenchError::Resident)
            })
            .await
    }

    pub(crate) async fn run_rooted_application(
        &self,
        context: crate::ActorSessionContext,
        handler: RootCustody,
        request: Arc<RootCustody>,
        handler_realm: RealmId,
    ) -> Result<ResidentOutcome, ResidentActorWorkbenchError> {
        self.access
            .with_machine(context, move |session, _context, _| {
                session
                    .run_rooted_application(
                        "actor_application",
                        &handler,
                        &request,
                        handler_realm,
                        None,
                    )
                    .map_err(ResidentActorWorkbenchError::Resident)
            })
            .await
    }

    pub(crate) async fn map_source(
        &self,
        context: crate::ActorSessionContext,
        entry: Arc<RootCustody>,
        event: crate::request::sources::SourceEvent,
    ) -> Result<crate::MailboxValue, ResidentActorWorkbenchError> {
        let realm = RealmId::fresh();
        let hole =
            self.access
                .with_machine(context.clone(), move |session, _, _| {
                    let outcome = session
                        .run_rooted_entry_borrowed("actor_source", &entry, 0, realm, None)
                        .map_err(ResidentActorWorkbenchError::Resident)?;
                    let ResidentOutcome::Suspended { hole, request, .. } = outcome else {
                        return Err(ResidentActorWorkbenchError::ActorProtocol(
                            "source mapper did not request its input".into(),
                        ));
                    };
                    if session.parked_realm(&hole) != Some(realm)
                        || !matches!(
                        ResidentRequest::decode(&request, session.data_con_table())?,
                        ResidentRequest::ActorKernel(
                            crate::generated::actor_kernel::ActorKernelReq::ActorSourceInputWith
                        )
                    ) {
                        return Err(ResidentActorWorkbenchError::ActorProtocol(
                            "source mapper crossed an unexpected boundary".into(),
                        ));
                    }
                    Ok(hole)
                })
                .await?;
        use crate::request::sources::SourceEvent;
        let outcome = match event {
            SourceEvent::Command(event) => self.resume_value(context.clone(), hole, event).await?,
            SourceEvent::Lifecycle(event) => {
                self.access
                    .with_machine(context.clone(), move |session, _, _| {
                        let table = session.data_con_table();
                        let (name, fields) = match event {
                            crate::ActorLifecycle::Live => ("ActorLive", vec![]),
                            crate::ActorLifecycle::Paused(detail) => {
                                ("ActorPaused", vec![detail.to_value(table)?])
                            }
                            crate::ActorLifecycle::Exited(terminal) => {
                                let name = match terminal.kind {
                                    crate::ActorExitKind::Completed => "ActorFinished",
                                    crate::ActorExitKind::Failed => "ActorFailed",
                                    crate::ActorExitKind::Cancelled => "ActorCancelled",
                                };
                                (name, vec![terminal.summary.to_value(table)?])
                            }
                        };
                        let value =
                            qualified_constructor(table, "Tidepool.Actor.Source", name, fields)?;
                        session
                            .resume(hole, value)
                            .map_err(ResidentActorWorkbenchError::Resident)
                    })
                    .await?
            }
            SourceEvent::Progress(snapshot) => {
                self.resume_progress_observation(context.clone(), hole, Ok((Some(snapshot), false)))
                    .await?
            }
            SourceEvent::ProgressClosed => {
                self.resume_progress_observation(context.clone(), hole, Ok((None, true)))
                    .await?
            }
            SourceEvent::Settled(result) => {
                self.resume_response_observation(
                    context.clone(),
                    hole,
                    Ok(match result {
                        Ok(()) => crate::ResponseObservation::Ready,
                        Err(failure) => crate::ResponseObservation::Unavailable(failure),
                    }),
                )
                .await?
            }
        };
        let message = self
            .capture_kernel_value(
                context.clone(),
                outcome,
                ResidentKernelBoundary::Reply,
                0,
                realm,
                context.placement.resource_scope,
            )
            .await?;
        let finished = self
            .resume_unit(context.clone(), message.continuation)
            .await?;
        if !matches!(finished, ResidentOutcome::Completed { .. }) {
            return Err(ResidentActorWorkbenchError::ActorProtocol(
                "source mapper continued after returning its message".into(),
            ));
        }
        self.close_realm(context.clone(), realm).await?;
        Ok(crate::MailboxValue::new(
            context.placement.session,
            message.value,
        ))
    }

    pub(crate) async fn capture_kernel_value(
        &self,
        context: crate::ActorSessionContext,
        outcome: ResidentOutcome,
        expected: ResidentKernelBoundary,
        expected_site: u64,
        handler_realm: RealmId,
        actor_realm: RealmId,
    ) -> Result<KernelValue, ResidentActorWorkbenchError> {
        let ResidentOutcome::Suspended { hole, request, .. } = outcome else {
            return Err(ResidentActorWorkbenchError::ActorProtocol(format!(
                "mailbox handler completed before `{}`",
                expected.operation()
            )));
        };
        self.access
            .with_machine(context, move |session, _, _| {
                if session.parked_realm(&hole) != Some(handler_realm) {
                    return Err(ResidentActorWorkbenchError::ActorProtocol(format!(
                        "`{}` escaped mailbox handler realm {handler_realm:?}",
                        expected.operation()
                    )));
                }
                let decoded = crate::generated::actor_kernel::ActorKernelReq::from_value(
                    &request,
                    session.data_con_table(),
                )?;
                let (actual, site) = match decoded {
                    crate::generated::actor_kernel::ActorKernelReq::ActorInstallShutdownWith(
                        ..,
                    ) => {
                        return Err(ResidentActorWorkbenchError::ActorProtocol(format!(
                            "expected `{}`, got `installShutdown`",
                            expected.operation()
                        )));
                    }
                    crate::generated::actor_kernel::ActorKernelReq::ActorReplyWith(site, _) => {
                        (ResidentKernelBoundary::Reply, site)
                    }
                    crate::generated::actor_kernel::ActorKernelReq::ActorContinueWith(site, _) => {
                        (ResidentKernelBoundary::Continue, site)
                    }
                    crate::generated::actor_kernel::ActorKernelReq::ActorReadyWith => {
                        return Err(ResidentActorWorkbenchError::ActorProtocol(format!(
                            "expected `{}`, got `ready`",
                            expected.operation()
                        )));
                    }
                    crate::generated::actor_kernel::ActorKernelReq::ActorInstallProgressSourceWith(..)
                    | crate::generated::actor_kernel::ActorKernelReq::ActorInstallSettlementSourceWith(..)
                    | crate::generated::actor_kernel::ActorKernelReq::ActorInstallLifecycleSourceWith(..)
                    | crate::generated::actor_kernel::ActorKernelReq::ActorInstallCommandSourceWith(..)
                    | crate::generated::actor_kernel::ActorKernelReq::ActorSourceInputWith => {
                        return Err(ResidentActorWorkbenchError::ActorProtocol("source boundary escaped its mapping or installation".into()));
                    }
                };
                let site = u64::try_from(site).map_err(|_| {
                    ResidentActorWorkbenchError::ActorProtocol(format!(
                        "`{}` carried invalid site id {site}",
                        actual.operation()
                    ))
                })?;
                if actual != expected || site != expected_site {
                    return Err(ResidentActorWorkbenchError::ActorProtocol(format!(
                        "expected `{}` at site {expected_site}, got `{}` at site {site}",
                        expected.operation(),
                        actual.operation()
                    )));
                }
                let value = session
                    .live_payload_handle_owned_by(hole.cont_id(), actor_realm)
                    .ok_or_else(|| {
                        ResidentActorWorkbenchError::ActorProtocol(format!(
                            "`{}` suspended without its live value",
                            actual.operation()
                        ))
                    })?;
                Ok(KernelValue {
                    continuation: hole,
                    value,
                })
            })
            .await
    }

    pub(crate) async fn kernel_boundary(
        &self,
        context: crate::ActorSessionContext,
        outcome: &ResidentOutcome,
    ) -> Result<Option<(ResidentKernelBoundary, u64)>, ResidentActorWorkbenchError> {
        let ResidentOutcome::Suspended { request, .. } = outcome else {
            return Ok(None);
        };
        let request = request.clone();
        self.access
            .with_machine(context, move |session, _, _| {
                let decoded = match crate::generated::actor_kernel::ActorKernelReq::from_value(
                    &request,
                    session.data_con_table(),
                ) {
                    Ok(decoded) => decoded,
                    Err(BridgeError::UnknownDataCon(_)) => return Ok(None),
                    Err(source) => return Err(source.into()),
                };
                let (kind, site) = match decoded {
                    crate::generated::actor_kernel::ActorKernelReq::ActorReplyWith(site, _) => {
                        (ResidentKernelBoundary::Reply, site)
                    }
                    crate::generated::actor_kernel::ActorKernelReq::ActorContinueWith(site, _) => {
                        (ResidentKernelBoundary::Continue, site)
                    }
                    crate::generated::actor_kernel::ActorKernelReq::ActorInstallShutdownWith(
                        ..,
                    )
                    | crate::generated::actor_kernel::ActorKernelReq::ActorReadyWith
                    | crate::generated::actor_kernel::ActorKernelReq::ActorInstallProgressSourceWith(..)
                    | crate::generated::actor_kernel::ActorKernelReq::ActorInstallSettlementSourceWith(..)
                    | crate::generated::actor_kernel::ActorKernelReq::ActorInstallLifecycleSourceWith(..)
                    | crate::generated::actor_kernel::ActorKernelReq::ActorInstallCommandSourceWith(..)
                    | crate::generated::actor_kernel::ActorKernelReq::ActorSourceInputWith => {
                        return Ok(None)
                    }
                };
                let site = u64::try_from(site).map_err(|_| {
                    ResidentActorWorkbenchError::ActorProtocol(format!(
                        "`{}` carried invalid site id {site}",
                        kind.operation()
                    ))
                })?;
                Ok(Some((kind, site)))
            })
            .await
    }

    pub(crate) async fn resume_unit(
        &self,
        context: crate::ActorSessionContext,
        hole: ResidentHole,
    ) -> Result<ResidentOutcome, ResidentActorWorkbenchError> {
        self.access
            .with_machine(context, move |session, _, _| {
                let answer = ().to_value(session.data_con_table())?;
                session
                    .resume(hole, answer)
                    .map_err(ResidentActorWorkbenchError::Resident)
            })
            .await
    }

    pub(crate) async fn resume_int(
        &self,
        context: crate::ActorSessionContext,
        hole: ResidentHole,
        value: u64,
    ) -> Result<ResidentOutcome, ResidentActorWorkbenchError> {
        self.access
            .with_machine(context, move |session, _, _| {
                let value = i64::try_from(value).map_err(|_| {
                    ResidentActorWorkbenchError::ActorProtocol(
                        "runtime identity exceeds Haskell Int".into(),
                    )
                })?;
                let answer = value.to_value(session.data_con_table())?;
                session
                    .resume(hole, answer)
                    .map_err(ResidentActorWorkbenchError::Resident)
            })
            .await
    }

    pub(crate) async fn resume_actor_context(
        &self,
        context: crate::ActorSessionContext,
        hole: ResidentHole,
        descriptor: crate::ActorDescriptor,
        bound_worktree: Option<String>,
        runtime: crate::ActorRuntimeObservation,
    ) -> Result<ResidentOutcome, ResidentActorWorkbenchError> {
        self.access
            .with_machine(context, move |session, context, _| {
                let table = session.data_con_table();
                let role = match descriptor.effective_role().role() {
                    crate::ActorRole::Root => "ContextRoot",
                    crate::ActorRole::Research => "ContextResearch",
                    crate::ActorRole::Coding => "ContextCoding",
                    crate::ActorRole::Scaffolding => "ContextScaffolding",
                    crate::ActorRole::Integration => "ContextIntegration",
                    crate::ActorRole::Inherited => "ContextInherited",
                };
                let native_tools = match descriptor.effective_role().native_tools() {
                    crate::NativeToolClass::InspectionOnly => "NativeInspectionOnly",
                    crate::NativeToolClass::Coding => "NativeCoding",
                    crate::NativeToolClass::Integration => "NativeIntegration",
                    crate::NativeToolClass::Inherited => "NativeInherited",
                };
                let workspace = match descriptor.effective_role().workspace() {
                    crate::WorkspaceAccess::None => "WorkspaceNone",
                    crate::WorkspaceAccess::InspectOnly => "WorkspaceInspectOnly",
                    crate::WorkspaceAccess::WritableBound => "WorkspaceWritableBound",
                };
                let parent = descriptor.context_parent();
                let descendants = descriptor.effective_role().descendants();
                let usage = runtime.latest_provider_usage();
                let activation_kind = match &runtime.activation_kind {
                    crate::ActorActivationKind::RootStarted => {
                        actor_context_constructor(table, "ActivationRootStarted", Vec::new())?
                    }
                    crate::ActorActivationKind::RequestActivated {
                        request,
                        activation_sequence,
                    } => actor_context_constructor(
                        table,
                        "ActivationRequest",
                        vec![
                            actor_int(request.0)?.to_value(table)?,
                            actor_int(*activation_sequence)?.to_value(table)?,
                        ],
                    )?,
                    crate::ActorActivationKind::EventsActivated { inbox_sequences } => {
                        actor_context_constructor(
                            table,
                            "ActivationEvents",
                            vec![inbox_sequences
                                .iter()
                                .copied()
                                .map(actor_int)
                                .collect::<Result<Vec<_>, _>>()?
                                .to_value(table)?],
                        )?
                    }
                };
                let fields = vec![
                    actor_int(context.actor.id.0)?.to_value(table)?,
                    actor_int(context.actor.incarnation.0)?.to_value(table)?,
                    parent
                        .map(|actor| actor_int(actor.id.0))
                        .transpose()?
                        .to_value(table)?,
                    parent
                        .map(|actor| actor_int(actor.incarnation.0))
                        .transpose()?
                        .to_value(table)?,
                    descriptor.label().to_owned().to_value(table)?,
                    actor_context_constructor(table, role, Vec::new())?,
                    descriptor
                        .effective_role()
                        .haskell_effects_type()
                        .to_value(table)?,
                    actor_context_constructor(table, native_tools, Vec::new())?,
                    actor_context_constructor(table, workspace, Vec::new())?,
                    bound_worktree.to_value(table)?,
                    descriptor
                        .fork_group()
                        .map(|group| actor_int(group.0))
                        .transpose()?
                        .to_value(table)?,
                    actor_int(context.placement.lexical_scope.0)?.to_value(table)?,
                    activation_kind,
                    actor_int(runtime.event_watermark)?.to_value(table)?,
                    runtime.provider_thread.to_value(table)?,
                    runtime.provider_parent_thread.to_value(table)?,
                    usage_observation_value(table, runtime.first_provider_usage.as_ref())?,
                    usage_observation_value(table, usage)?,
                    usage_summary_value(table, runtime.provider_usage_summary.as_ref())?,
                    usage_summary_value(table, runtime.latest_turn_usage_summary.as_ref())?,
                    i64::from(descendants.maximum_depth).to_value(table)?,
                    descendants
                        .maximum_active_children
                        .map(i64::from)
                        .to_value(table)?,
                    descriptor
                        .effective_role()
                        .prompt_profile()
                        .to_owned()
                        .to_value(table)?,
                ];
                let answer = actor_context_constructor(table, "ActorContextInfo", fields)?;
                session
                    .resume(hole, answer)
                    .map_err(ResidentActorWorkbenchError::Resident)
            })
            .await
    }

    pub(crate) async fn resume_agent_roster(
        &self,
        context: crate::ActorSessionContext,
        hole: ResidentHole,
        roster: Vec<AgentRosterProjection>,
    ) -> Result<ResidentOutcome, ResidentActorWorkbenchError> {
        self.access
            .with_machine(context, move |session, _, _| {
                let table = session.data_con_table();
                let mut entries = Vec::with_capacity(roster.len());
                for entry in roster {
                    entries.push(agent_roster_value(table, entry)?);
                }
                let answer = core_list(table, entries)?;
                session
                    .resume(hole, answer)
                    .map_err(ResidentActorWorkbenchError::Resident)
            })
            .await
    }

    pub(crate) async fn resume_group_roster(
        &self,
        context: crate::ActorSessionContext,
        hole: ResidentHole,
        roster: Option<Vec<AgentRosterProjection>>,
    ) -> Result<ResidentOutcome, ResidentActorWorkbenchError> {
        self.access
            .with_machine(context, move |session, _, _| {
                let table = session.data_con_table();
                let answer = roster
                    .map(|roster| {
                        let entries = roster
                            .into_iter()
                            .map(|entry| agent_roster_value(table, entry))
                            .collect::<Result<Vec<_>, _>>()?;
                        Ok::<_, ResidentActorWorkbenchError>(core_list(table, entries)?)
                    })
                    .transpose()?
                    .to_value(table)?;
                session
                    .resume(hole, answer)
                    .map_err(ResidentActorWorkbenchError::Resident)
            })
            .await
    }

    pub(crate) async fn resume_agent_observation(
        &self,
        context: crate::ActorSessionContext,
        hole: ResidentHole,
        observation: Option<AgentRosterProjection>,
    ) -> Result<ResidentOutcome, ResidentActorWorkbenchError> {
        self.access
            .with_machine(context, move |session, _, _| {
                let table = session.data_con_table();
                let answer = observation
                    .map(|entry| agent_roster_value(table, entry))
                    .transpose()?
                    .to_value(table)?;
                session
                    .resume(hole, answer)
                    .map_err(ResidentActorWorkbenchError::Resident)
            })
            .await
    }

    pub(crate) async fn resume_agent_forget(
        &self,
        context: crate::ActorSessionContext,
        hole: ResidentHole,
        outcome: AgentForgetProjection,
    ) -> Result<ResidentOutcome, ResidentActorWorkbenchError> {
        self.access
            .with_machine(context, move |session, _, _| {
                let table = session.data_con_table();
                let (name, fields) = match outcome {
                    AgentForgetProjection::Forgotten => ("AgentForgotten", Vec::new()),
                    AgentForgetProjection::Running => ("AgentForgetRunning", Vec::new()),
                    AgentForgetProjection::Unavailable => ("AgentForgetUnavailable", Vec::new()),
                    AgentForgetProjection::Retained { requests, watches } => (
                        "AgentForgetRetained",
                        vec![
                            requests
                                .into_iter()
                                .map(|request| actor_int(request.0))
                                .collect::<Result<Vec<_>, _>>()?
                                .to_value(table)?,
                            watches
                                .into_iter()
                                .map(|watch| actor_int(watch.0))
                                .collect::<Result<Vec<_>, _>>()?
                                .to_value(table)?,
                        ],
                    ),
                };
                let answer = actor_context_constructor(table, name, fields)?;
                session
                    .resume(hole, answer)
                    .map_err(ResidentActorWorkbenchError::Resident)
            })
            .await
    }

    /// End only the suspended computation; the independent command job remains owned.
    pub(crate) async fn stop_command_observation(
        &self,
        context: crate::ActorSessionContext,
        hole: ResidentHole,
        job: String,
        reason: CommandObservationStop,
    ) -> Result<ResidentOutcome, ResidentActorWorkbenchError> {
        self.access
            .with_machine(context, move |session, _, _| {
                let stopped = session.abort(
                    hole.cont_id(),
                    "foreground command observation ended".into(),
                );
                if session.parked_holes().contains(&hole.cont_id()) {
                    return match stopped {
                        Err(error) => Err(ResidentActorWorkbenchError::Resident(error)),
                        Ok(_) => Err(ResidentActorWorkbenchError::ActorProtocol(
                            "command observation continuation remained parked after abort".into(),
                        )),
                    };
                }
                match stopped {
                    Err(ResidentError::Run(tidepool_runtime::RuntimeError::Jit(
                        tidepool_runtime::JitError::Effect(
                            tidepool_effect::error::EffectError::Handler(_),
                        ),
                    ))) => {
                        Err(ResidentActorWorkbenchError::CommandObservationStopped { job, reason })
                    }
                    Err(error) => Err(ResidentActorWorkbenchError::Resident(error)),
                    Ok(_) => Err(ResidentActorWorkbenchError::ActorProtocol(
                        "aborted command observation unexpectedly completed".into(),
                    )),
                }
            })
            .await
    }

    pub(crate) async fn resume_value<T: ToCore + Send + 'static>(
        &self,
        context: crate::ActorSessionContext,
        hole: ResidentHole,
        outcome: T,
    ) -> Result<ResidentOutcome, ResidentActorWorkbenchError> {
        self.access
            .with_machine(context, move |session, _, _| {
                let answer = outcome.to_value(session.data_con_table())?;
                session
                    .resume(hole, answer)
                    .map_err(ResidentActorWorkbenchError::Resident)
            })
            .await
    }

    pub(crate) async fn resume_agent_stop(
        &self,
        context: crate::ActorSessionContext,
        hole: ResidentHole,
        outcome: AgentStopProjection,
    ) -> Result<ResidentOutcome, ResidentActorWorkbenchError> {
        self.access
            .with_machine(context, move |session, _, _| {
                let table = session.data_con_table();
                let answer = agent_stop_value(table, outcome)?;
                session
                    .resume(hole, answer)
                    .map_err(ResidentActorWorkbenchError::Resident)
            })
            .await
    }

    pub(crate) async fn resume_cleanup_plan(
        &self,
        context: crate::ActorSessionContext,
        hole: ResidentHole,
        plan: CleanupPlanProjection,
    ) -> Result<ResidentOutcome, ResidentActorWorkbenchError> {
        self.access
            .with_machine(context, move |session, _, _| {
                let answer = cleanup_plan_value(session.data_con_table(), &plan)?;
                session
                    .resume(hole, answer)
                    .map_err(ResidentActorWorkbenchError::Resident)
            })
            .await
    }

    pub(crate) async fn resume_cleanup_receipt(
        &self,
        context: crate::ActorSessionContext,
        hole: ResidentHole,
        receipt: CleanupReceiptProjection,
    ) -> Result<ResidentOutcome, ResidentActorWorkbenchError> {
        self.access
            .with_machine(context, move |session, _, _| {
                let table = session.data_con_table();
                let plan = cleanup_plan_value(table, &receipt.plan)?;
                let steps = receipt
                    .steps
                    .into_iter()
                    .map(|step| cleanup_step_value(table, step))
                    .collect::<Result<Vec<_>, _>>()?
                    .to_value(table)?;
                let answer = actor_context_constructor(
                    table,
                    "CleanupReceipt",
                    vec![plan, steps, receipt.complete.to_value(table)?],
                )?;
                session
                    .resume(hole, answer)
                    .map_err(ResidentActorWorkbenchError::Resident)
            })
            .await
    }

    pub(crate) async fn resume_fork_group(
        &self,
        context: crate::ActorSessionContext,
        hole: ResidentHole,
        group: crate::ForkGroupId,
        group_path: String,
        paths: Vec<String>,
    ) -> Result<ResidentOutcome, ResidentActorWorkbenchError> {
        self.access
            .with_machine(context, move |session, _, _| {
                let group = i64::try_from(group.0).map_err(|_| {
                    ResidentActorWorkbenchError::ActorProtocol(
                        "fork group identity exceeds Haskell Int".into(),
                    )
                })?;
                let answer = Ok::<_, String>((group, group_path, paths))
                    .to_value(session.data_con_table())?;
                session
                    .resume(hole, answer)
                    .map_err(ResidentActorWorkbenchError::Resident)
            })
            .await
    }

    pub(crate) async fn resume_reply_rejection(
        &self,
        context: crate::ActorSessionContext,
        hole: ResidentHole,
        error: crate::ReplyError,
    ) -> Result<ResidentOutcome, ResidentActorWorkbenchError> {
        self.access
            .with_machine(context, move |session, _, _| {
                let answer =
                    crate::request_effect::rejected_reply_value(error, session.data_con_table())?;
                session
                    .resume(hole, answer)
                    .map_err(ResidentActorWorkbenchError::Resident)
            })
            .await
    }

    pub(crate) async fn resume_response_observation(
        &self,
        context: crate::ActorSessionContext,
        hole: ResidentHole,
        observation: Result<crate::ResponseObservation, crate::ReplyError>,
    ) -> Result<ResidentOutcome, ResidentActorWorkbenchError> {
        self.access
            .with_machine(context, move |session, _, _| {
                let answer = crate::request_effect::response_observation_value(
                    observation,
                    session.data_con_table(),
                )?;
                session
                    .resume(hole, answer)
                    .map_err(ResidentActorWorkbenchError::Resident)
            })
            .await
    }

    pub(crate) async fn resume_request_update<T: ToCore + Send + 'static>(
        &self,
        context: crate::ActorSessionContext,
        hole: ResidentHole,
        outcome: Result<T, crate::ReplyError>,
    ) -> Result<ResidentOutcome, ResidentActorWorkbenchError> {
        self.access
            .with_machine(context, move |session, _, _| {
                let table = session.data_con_table();
                let answer = match outcome {
                    Ok(value) => {
                        let right =
                            tidepool_bridge::get_resilient(table, "Right", 1).ok_or_else(|| {
                                tidepool_bridge::BridgeError::UnknownDataConName("Right".into())
                            })?;
                        Value::Con(right, vec![value.to_value(table)?])
                    }
                    Err(error) => crate::request_effect::rejected_reply_value(error, table)?,
                };
                session
                    .resume(hole, answer)
                    .map_err(ResidentActorWorkbenchError::Resident)
            })
            .await
    }

    pub(crate) async fn resume_progress_publication(
        &self,
        context: crate::ActorSessionContext,
        hole: ResidentHole,
        outcome: Result<u64, crate::ReplyError>,
    ) -> Result<ResidentOutcome, ResidentActorWorkbenchError> {
        self.access
            .with_machine(context, move |session, _, _| {
                let table = session.data_con_table();
                let answer = match outcome {
                    Ok(_) => {
                        let right =
                            tidepool_bridge::get_resilient(table, "Right", 1).ok_or_else(|| {
                                tidepool_bridge::BridgeError::UnknownDataConName("Right".into())
                            })?;
                        Value::Con(right, vec![().to_value(table)?])
                    }
                    Err(error) => crate::request_effect::rejected_reply_value(error, table)?,
                };
                session
                    .resume(hole, answer)
                    .map_err(ResidentActorWorkbenchError::Resident)
            })
            .await
    }

    pub(crate) async fn resume_progress_observation(
        &self,
        context: crate::ActorSessionContext,
        hole: ResidentHole,
        observation: Result<(Option<crate::request::ProgressSnapshot>, bool), crate::ReplyError>,
    ) -> Result<ResidentOutcome, ResidentActorWorkbenchError> {
        self.access
            .with_machine(context, move |session, _, _| {
                let table = session.data_con_table();
                let module = "Tidepool.Agent.Reply.Internal";
                let answer = match observation {
                    Ok((Some(snapshot), _)) => {
                        let name = format!("{module}.ProgressUpdate");
                        let constructor = table
                            .get_by_qualified_name(&name)
                            .ok_or(tidepool_bridge::BridgeError::UnknownDataConName(name))?;
                        let revision = i64::try_from(snapshot.revision).map_err(|_| {
                            ResidentActorWorkbenchError::ActorProtocol(
                                "progress revision exceeds Haskell Int".into(),
                            )
                        })?;
                        let prefix = vec![revision.to_value(table)?];
                        return session
                            .resume_framed_custody(hole, &snapshot.value, constructor, prefix)
                            .map_err(ResidentActorWorkbenchError::Resident);
                    }
                    Ok((None, closed)) => crate::request_effect::constructor(
                        table,
                        module,
                        if closed {
                            "ProgressClosed"
                        } else {
                            "ProgressPending"
                        },
                        vec![],
                    )?,
                    Err(error) => crate::request_effect::constructor(
                        table,
                        module,
                        "ProgressRejected",
                        vec![crate::request_effect::reply_error_value(error, table)?],
                    )?,
                };
                session
                    .resume(hole, answer)
                    .map_err(ResidentActorWorkbenchError::Resident)
            })
            .await
    }

    pub(crate) async fn resume_cancel_request(
        &self,
        context: crate::ActorSessionContext,
        hole: ResidentHole,
        outcome: Result<crate::CancelRequestOutcome, crate::ReplyError>,
    ) -> Result<ResidentOutcome, ResidentActorWorkbenchError> {
        self.access
            .with_machine(context, move |session, _, _| {
                let answer =
                    crate::request_effect::cancel_request_value(outcome, session.data_con_table())?;
                session
                    .resume(hole, answer)
                    .map_err(ResidentActorWorkbenchError::Resident)
            })
            .await
    }

    pub(crate) async fn resume_abandonment(
        &self,
        context: crate::ActorSessionContext,
        hole: ResidentHole,
        outcome: Result<crate::AbandonResponseOutcome, crate::ReplyError>,
    ) -> Result<ResidentOutcome, ResidentActorWorkbenchError> {
        self.access
            .with_machine(context, move |session, _, _| {
                let answer = crate::request_effect::abandon_response_value(
                    outcome,
                    session.data_con_table(),
                )?;
                session
                    .resume(hole, answer)
                    .map_err(ResidentActorWorkbenchError::Resident)
            })
            .await
    }

    pub(crate) async fn resume_response_forget(
        &self,
        context: crate::ActorSessionContext,
        hole: ResidentHole,
        outcome: Result<crate::ForgetResponseOutcome, crate::ReplyError>,
    ) -> Result<ResidentOutcome, ResidentActorWorkbenchError> {
        self.access
            .with_machine(context, move |session, _, _| {
                let answer = crate::request_effect::forget_response_value(
                    outcome,
                    session.data_con_table(),
                )?;
                session
                    .resume(hole, answer)
                    .map_err(ResidentActorWorkbenchError::Resident)
            })
            .await
    }

    pub(crate) async fn resume_reply_observation(
        &self,
        context: crate::ActorSessionContext,
        hole: ResidentHole,
        observation: Result<crate::ReplyObservation, crate::ReplyError>,
    ) -> Result<ResidentOutcome, ResidentActorWorkbenchError> {
        self.access
            .with_machine(context, move |session, _, _| {
                let answer = crate::request_effect::reply_observation_value(
                    observation,
                    session.data_con_table(),
                )?;
                session
                    .resume(hole, answer)
                    .map_err(ResidentActorWorkbenchError::Resident)
            })
            .await
    }

    pub(crate) async fn abandon_cast_handler(
        &self,
        context: crate::ActorSessionContext,
        receiver_continuation: ResidentHole,
        handler_realm: RealmId,
    ) -> Result<ResidentOutcome, ResidentActorWorkbenchError> {
        self.access
            .with_machine(context, move |session, _, _| {
                let keep_receiving = true.to_value(session.data_con_table())?;
                let outcome = session
                    .resume(receiver_continuation, keep_receiving)
                    .map_err(ResidentActorWorkbenchError::Resident)?;
                let _ = session.close_realm(handler_realm);
                Ok(outcome)
            })
            .await
    }

    pub(crate) async fn resume_watch_observation(
        &self,
        context: crate::ActorSessionContext,
        hole: ResidentHole,
        observation: Result<crate::WatchObservation, crate::ReplyError>,
    ) -> Result<ResidentOutcome, ResidentActorWorkbenchError> {
        self.access
            .with_machine(context, move |session, _, _| {
                let answer = crate::request_effect::watch_observation_value(
                    observation,
                    session.data_con_table(),
                )?;
                session
                    .resume(hole, answer)
                    .map_err(ResidentActorWorkbenchError::Resident)
            })
            .await
    }

    pub(crate) async fn resume_route_state(
        &self,
        context: crate::ActorSessionContext,
        hole: ResidentHole,
        observation: Result<crate::request::routes::RouteState, crate::ReplyError>,
    ) -> Result<ResidentOutcome, ResidentActorWorkbenchError> {
        self.access
            .with_machine(context, move |session, _, _| {
                use crate::request::routes::RouteState;
                let table = session.data_con_table();
                let (name, fields) = match observation {
                    Ok(RouteState::Waiting) => ("RouteWaiting", vec![]),
                    Ok(RouteState::Running) => ("RouteRunning", vec![]),
                    Ok(RouteState::Completed) => ("RouteCompleted", vec![]),
                    Ok(RouteState::Failed(error)) => ("RouteFailed", vec![error.to_value(table)?]),
                    Err(error) => (
                        "RouteRejected",
                        vec![crate::request_effect::reply_error_value(error, table)?],
                    ),
                };
                let answer = crate::request_effect::constructor(
                    session.data_con_table(),
                    "Tidepool.Agent.Watch.Internal",
                    name,
                    fields,
                )?;
                session
                    .resume(hole, answer)
                    .map_err(ResidentActorWorkbenchError::Resident)
            })
            .await
    }

    pub(crate) async fn run_route_entry(
        &self,
        context: crate::ActorSessionContext,
        entry: RootCustody,
        watch: crate::WatchId,
    ) -> Result<ResidentOutcome, ResidentActorWorkbenchError> {
        self.access
            .with_machine(context, move |session, context, _| {
                let argument = i64::try_from(watch.0).map_err(|_| {
                    ResidentActorWorkbenchError::ActorProtocol(
                        "route ID exceeds Haskell Int".into(),
                    )
                })?;
                session
                    .run_rooted_entry(
                        "watch_route",
                        entry,
                        argument,
                        context.placement.resource_scope,
                        None,
                    )
                    .map_err(ResidentActorWorkbenchError::Resident)
            })
            .await
    }

    pub(crate) async fn resume_watch_forget(
        &self,
        context: crate::ActorSessionContext,
        hole: ResidentHole,
        outcome: Result<crate::ForgetWatchOutcome, crate::ReplyError>,
    ) -> Result<ResidentOutcome, ResidentActorWorkbenchError> {
        self.access
            .with_machine(context, move |session, _, _| {
                let answer =
                    crate::request_effect::forget_watch_value(outcome, session.data_con_table())?;
                session
                    .resume(hole, answer)
                    .map_err(ResidentActorWorkbenchError::Resident)
            })
            .await
    }

    pub(crate) async fn resume_tool_invocation(
        &self,
        context: crate::ActorSessionContext,
        hole: ResidentHole,
        name: String,
        arguments: serde_json::Value,
    ) -> Result<ResidentOutcome, ResidentActorWorkbenchError> {
        self.access
            .with_machine(context, move |session, _, _| {
                let answer = (name, arguments).to_value(session.data_con_table())?;
                session
                    .resume(hole, answer)
                    .map_err(ResidentActorWorkbenchError::Resident)
            })
            .await
    }

    pub(crate) async fn resume_live(
        &self,
        context: crate::ActorSessionContext,
        hole: ResidentHole,
        value: RootCustody,
    ) -> Result<ResidentOutcome, ResidentActorWorkbenchError> {
        self.access
            .with_machine(context, move |session, _, _| {
                session
                    .resume_handle(hole, value)
                    .map_err(ResidentActorWorkbenchError::Resident)
            })
            .await
    }

    pub(crate) async fn resume_terminal(
        &self,
        context: crate::ActorSessionContext,
        hole: ResidentHole,
        terminal: crate::ActorTerminal,
    ) -> Result<ResidentOutcome, ResidentActorWorkbenchError> {
        self.access
            .with_machine(context, move |session, _, _| {
                let answer = crate::actor_terminal_value(&terminal, session.data_con_table())?;
                session
                    .resume(hole, answer)
                    .map_err(ResidentActorWorkbenchError::Resident)
            })
            .await
    }

    pub(crate) async fn resume_call_status(
        &self,
        context: crate::ActorSessionContext,
        hole: ResidentHole,
        failure: Option<String>,
    ) -> Result<ResidentOutcome, ResidentActorWorkbenchError> {
        self.access
            .with_machine(context, move |session, _, _| {
                let table = session.data_con_table();
                let (name, fields) = match failure {
                    Some(summary) => (
                        "Tidepool.Effects.Core.ActorCallFailed",
                        vec![summary.to_value(table)?],
                    ),
                    None => ("Tidepool.Effects.Core.ActorCallSucceeded", Vec::new()),
                };
                let constructor = table.get_by_qualified_name(name).ok_or_else(|| {
                    tidepool_bridge::BridgeError::UnknownDataConName(name.to_owned())
                })?;
                session
                    .resume(hole, Value::Con(constructor, fields))
                    .map_err(ResidentActorWorkbenchError::Resident)
            })
            .await
    }

    pub(crate) async fn resume_optional_terminal(
        &self,
        context: crate::ActorSessionContext,
        hole: ResidentHole,
        terminal: Option<crate::ActorTerminal>,
    ) -> Result<ResidentOutcome, ResidentActorWorkbenchError> {
        self.access
            .with_machine(context, move |session, _, _| {
                let table = session.data_con_table();
                let answer = match terminal {
                    Some(terminal) => {
                        let just =
                            tidepool_bridge::get_resilient(table, "Just", 1).ok_or_else(|| {
                                tidepool_bridge::BridgeError::UnknownDataConName("Just".into())
                            })?;
                        Value::Con(just, vec![crate::actor_terminal_value(&terminal, table)?])
                    }
                    None => {
                        let nothing = tidepool_bridge::get_resilient(table, "Nothing", 0)
                            .ok_or_else(|| {
                                tidepool_bridge::BridgeError::UnknownDataConName("Nothing".into())
                            })?;
                        Value::Con(nothing, Vec::new())
                    }
                };
                session
                    .resume(hole, answer)
                    .map_err(ResidentActorWorkbenchError::Resident)
            })
            .await
    }

    pub(crate) async fn rehome_mailbox_value(
        &self,
        context: crate::ActorSessionContext,
        value: crate::MailboxValue,
        owner: RealmId,
    ) -> Result<crate::MailboxValue, ResidentActorWorkbenchError> {
        let session_id = value.session();
        let custody = value.into_custody();
        self.access
            .with_machine(context, move |session, _, _| {
                let custody = session
                    .rehome_custody(custody, owner)
                    .map_err(ResidentActorWorkbenchError::Resident)?;
                Ok(crate::MailboxValue::new(session_id, custody))
            })
            .await
    }

    pub(crate) async fn prepare_root_program(
        &self,
        session_id: tidepool_repr::SessionId,
        compiled: Arc<tidepool_runtime::session::CompiledTurn>,
    ) -> Result<(crate::ActorPlacement, ResidentOutcome), ResidentActorWorkbenchError> {
        self.access
            .with_host_machine(session_id, None, move |session, _| {
                let placement = crate::ActorPlacement {
                    session: session_id,
                    resource_scope: RealmId::fresh(),
                    lexical_scope: session.mint_isolated_scope(),
                };
                let prepared = (|| {
                    session
                        .set_actor_execution(
                            tidepool_runtime::session::SessionRunContext {
                                resource_scope: placement.resource_scope,
                                lexical_scope: placement.lexical_scope,
                                ..tidepool_runtime::session::SessionRunContext::ROOT
                            },
                            tidepool_effect::EffectRunPolicy::HandleOrSuspend,
                            tidepool_effect::LivePayloadPolicy::HASKELL_EFFECT_VALUE,
                        )
                        .map_err(ResidentActorWorkbenchError::Resident)?;
                    let outcome = session
                        .run_with_sites(
                            "forest-root",
                            &compiled.expr,
                            &compiled.table,
                            &compiled.asks,
                        )
                        .map_err(ResidentActorWorkbenchError::Resident)?;
                    Ok::<_, ResidentActorWorkbenchError>(outcome)
                })();
                let outcome = match prepared {
                    Ok(outcome) => outcome,
                    Err(error) => {
                        session.close_realm(placement.resource_scope);
                        session.retire_scope(placement.lexical_scope);
                        return Err(error);
                    }
                };
                Ok((placement, outcome))
            })
            .await
    }

    pub(crate) async fn retire_root_placement(
        &self,
        placement: crate::ActorPlacement,
    ) -> Result<(), ResidentActorWorkbenchError> {
        self.access
            .with_host_machine(placement.session, None, move |session, _| {
                session.close_realm(placement.resource_scope);
                session.retire_scope(placement.lexical_scope);
                Ok(())
            })
            .await
    }

    pub(crate) async fn provision_root_scope(
        &self,
        session_id: tidepool_repr::SessionId,
    ) -> Result<crate::ActorPlacement, ResidentActorWorkbenchError> {
        self.access
            .with_host_machine(session_id, None, move |session, _| {
                Ok(crate::ActorPlacement {
                    session: session_id,
                    resource_scope: RealmId::fresh(),
                    lexical_scope: session.mint_isolated_scope(),
                })
            })
            .await
    }

    pub(crate) async fn close_realm(
        &self,
        context: crate::ActorSessionContext,
        realm: RealmId,
    ) -> Result<(), ResidentActorWorkbenchError> {
        self.access
            .with_machine(context, move |session, _, _| {
                let _ = session.close_realm(realm);
                Ok(())
            })
            .await
    }

    pub async fn resume_starting_parent(
        &self,
        context: crate::ActorSessionContext,
        hole: ResidentHole,
        actor: crate::ActorRef,
        allocated_label: String,
    ) -> Result<ResidentOutcome, ResidentActorWorkbenchError> {
        self.access
            .with_machine(context, move |session, _, _| {
                let answer = (
                    actor.id.0 as i64,
                    actor.incarnation.0 as i64,
                    allocated_label,
                )
                    .to_value(session.data_con_table())?;
                session
                    .resume(hole, answer)
                    .map_err(ResidentActorWorkbenchError::Resident)
            })
            .await
    }

    pub async fn resume_fork_starting_parent(
        &self,
        context: crate::ActorSessionContext,
        hole: ResidentHole,
        actor: crate::ActorRef,
        allocated_label: String,
        worktree: tidepool_bridge_effects::WtWorktreeHandle,
    ) -> Result<ResidentOutcome, ResidentActorWorkbenchError> {
        self.access
            .with_machine(context, move |session, _, _| {
                let answer = Ok::<_, String>((
                    (
                        actor.id.0 as i64,
                        actor.incarnation.0 as i64,
                        allocated_label,
                    ),
                    worktree,
                ))
                .to_value(session.data_con_table())?;
                session
                    .resume(hole, answer)
                    .map_err(ResidentActorWorkbenchError::Resident)
            })
            .await
    }

    pub(crate) async fn resume_fork_failure(
        &self,
        context: crate::ActorSessionContext,
        hole: ResidentHole,
        detail: String,
    ) -> Result<ResidentOutcome, ResidentActorWorkbenchError> {
        self.access
            .with_machine(context, move |session, _, _| {
                let answer = Err::<(), _>(detail).to_value(session.data_con_table())?;
                session
                    .resume(hole, answer)
                    .map_err(ResidentActorWorkbenchError::Resident)
            })
            .await
    }

    pub(crate) async fn resume_fork_unit(
        &self,
        context: crate::ActorSessionContext,
        hole: ResidentHole,
    ) -> Result<ResidentOutcome, ResidentActorWorkbenchError> {
        self.access
            .with_machine(context, move |session, _, _| {
                let answer = Ok::<(), String>(()).to_value(session.data_con_table())?;
                session
                    .resume(hole, answer)
                    .map_err(ResidentActorWorkbenchError::Resident)
            })
            .await
    }

    pub(crate) async fn resume_fork_cleanup(
        &self,
        context: crate::ActorSessionContext,
        hole: ResidentHole,
        outcome: Result<crate::ForkGroupCleanupOutcome, crate::ForkGroupError>,
    ) -> Result<ResidentOutcome, ResidentActorWorkbenchError> {
        self.access
            .with_machine(context, move |session, _, _| {
                let table = session.data_con_table();
                let (name, fields) = match outcome {
                    Ok(crate::ForkGroupCleanupOutcome::Cleaned) => ("ForkGroupCleaned", Vec::new()),
                    Ok(crate::ForkGroupCleanupOutcome::Active(actors)) => (
                        "ForkGroupStillActive",
                        vec![actors
                            .into_iter()
                            .map(|actor| {
                                Ok((actor_int(actor.id.0)?, actor_int(actor.incarnation.0)?))
                            })
                            .collect::<Result<Vec<_>, ResidentActorWorkbenchError>>()?
                            .to_value(table)?],
                    ),
                    Err(error) => (
                        "ForkGroupCleanupRejected",
                        vec![error.to_string().to_value(table)?],
                    ),
                };
                let answer = actor_context_constructor(table, name, fields)?;
                session
                    .resume(hole, answer)
                    .map_err(ResidentActorWorkbenchError::Resident)
            })
            .await
    }
}

fn source_request_id(value: i64) -> Result<crate::RequestId, ResidentActorWorkbenchError> {
    u64::try_from(value)
        .map(crate::RequestId)
        .map_err(|_| ResidentActorWorkbenchError::ActorProtocol("invalid source request".into()))
}

fn actor_int(value: u64) -> Result<i64, ResidentActorWorkbenchError> {
    i64::try_from(value).map_err(|_| {
        ResidentActorWorkbenchError::ActorProtocol(
            "runtime actor identity exceeds Haskell Int".into(),
        )
    })
}

fn workbench_int(value: usize) -> Result<i64, ResidentActorWorkbenchError> {
    i64::try_from(value).map_err(|_| {
        ResidentActorWorkbenchError::ActorProtocol(
            "workbench input-unit coordinate exceeds Haskell Int".into(),
        )
    })
}

fn actor_context_constructor(
    table: &DataConTable,
    name: &str,
    fields: Vec<Value>,
) -> Result<Value, tidepool_bridge::BridgeError> {
    qualified_constructor(table, "Tidepool.Effects.Core", name, fields)
}

fn qualified_constructor(
    table: &DataConTable,
    module: &str,
    name: &str,
    fields: Vec<Value>,
) -> Result<Value, tidepool_bridge::BridgeError> {
    let qualified = format!("{module}.{name}");
    let constructor = table
        .get_by_qualified_name(&qualified)
        .ok_or(tidepool_bridge::BridgeError::UnknownDataConName(qualified))?;
    Ok(Value::Con(constructor, fields))
}

fn core_list(
    table: &DataConTable,
    values: Vec<Value>,
) -> Result<Value, tidepool_bridge::BridgeError> {
    let nil = tidepool_bridge::get_resilient(table, "[]", 0)
        .ok_or_else(|| tidepool_bridge::BridgeError::UnknownDataConName("[]".into()))?;
    let cons = tidepool_bridge::get_resilient(table, ":", 2)
        .ok_or_else(|| tidepool_bridge::BridgeError::UnknownDataConName(":".into()))?;
    Ok(values
        .into_iter()
        .rev()
        .fold(Value::Con(nil, Vec::new()), |tail, head| {
            Value::Con(cons, vec![head, tail])
        }))
}

#[derive(Clone, Copy)]
enum OutboundKind {
    Call,
    TryCall,
    Cast,
}

fn capture_outbound_boundary<H, O>(
    session: &mut ResidentSession<H, O>,
    context: &crate::ActorSessionContext,
    hole: ResidentHole,
    target: (i64, i64),
    kind: OutboundKind,
    actor_realm: RealmId,
) -> Result<ResidentActorBoundary, ResidentActorWorkbenchError>
where
    H: DispatchEffect<O> + Send,
    O: OutputSink + Sync,
{
    let (id, incarnation) = target;
    let id = u64::try_from(id).map_err(|_| {
        ResidentActorWorkbenchError::ActorProtocol(format!(
            "actor request carried invalid actor id {id}"
        ))
    })?;
    let incarnation = u64::try_from(incarnation).map_err(|_| {
        ResidentActorWorkbenchError::ActorProtocol(format!(
            "actor request carried invalid incarnation {incarnation}"
        ))
    })?;
    let target = crate::ActorRef {
        id: crate::ActorId(id),
        incarnation: crate::Incarnation(incarnation),
    };
    let custody = session
        .live_payload_handle_owned_by(hole.cont_id(), actor_realm)
        .ok_or_else(|| {
            ResidentActorWorkbenchError::ActorProtocol(
                "actor call or cast suspended without its request".into(),
            )
        })?;
    let request = crate::MailboxValue::new(context.placement.session, custody);
    Ok(ResidentActorBoundary::Outbound(match kind {
        OutboundKind::Call => ResidentOutbound::Call {
            target,
            continuation: hole,
            request,
        },
        OutboundKind::TryCall => ResidentOutbound::TryCall {
            target,
            continuation: hole,
            request,
        },
        OutboundKind::Cast => ResidentOutbound::Cast {
            target,
            continuation: hole,
            request,
        },
    }))
}

fn capture_receiver_boundary<H, O>(
    session: &mut ResidentSession<H, O>,
    hole: ResidentHole,
    site: i64,
    actor_realm: RealmId,
) -> Result<ResidentActorBoundary, ResidentActorWorkbenchError>
where
    H: DispatchEffect<O> + Send,
    O: OutputSink + Sync,
{
    let site = u64::try_from(site).map_err(|_| {
        ResidentActorWorkbenchError::ActorProtocol(format!(
            "actor receive carried invalid site id {site}"
        ))
    })?;
    if session.parked_realm(&hole) != Some(actor_realm) {
        return Err(ResidentActorWorkbenchError::ActorProtocol(format!(
            "actor receive escaped its owning realm {actor_realm:?}"
        )));
    }
    let handler = session
        .live_payload_handle_owned_by(hole.cont_id(), actor_realm)
        .ok_or_else(|| {
            ResidentActorWorkbenchError::ActorProtocol(
                "actor receive suspended without its handler".into(),
            )
        })?;
    Ok(ResidentActorBoundary::Receive(InstalledReceiver {
        site,
        continuation: hole,
        handler,
    }))
}

fn unexpected_boundary(
    expected: &str,
    actual: &ResidentActorBoundary,
) -> ResidentActorWorkbenchError {
    ResidentActorWorkbenchError::ActorProtocol(format!(
        "expected `{expected}`, reached `{}`",
        actual.operation()
    ))
}

struct ReadyBlock {
    result: TurnResult,
    generation: tidepool_repr::Generation,
    declaration_source: String,
    declaration_imports: SourceImports,
    observation: Option<String>,
}

enum CompiledBlock {
    Ready(Box<ReadyBlock>),
    Rejected(String),
}

fn actor_compile_view<H, O>(
    session: &ResidentSession<H, O>,
    context: &crate::ActorSessionContext,
    source: &ActorWorkbenchSource,
    type_modules: &[String],
) -> Result<crate::ActorCompileView, ResidentActorWorkbenchError>
where
    H: DispatchEffect<O> + Send,
    O: OutputSink + Sync,
{
    let session_view = session
        .compile_view_in(context.placement.lexical_scope)
        .ok_or_else(|| {
            ResidentActorWorkbenchError::Resident(ResidentError::Session(
                tidepool_runtime::session::SessionError::DeadScope(context.placement.lexical_scope),
            ))
        })?;
    Ok(context
        .compile_view(session_view)?
        .with_workbench_imports(&source.workbench_imports)
        .with_type_modules(type_modules))
}

fn compile_block<H, O>(
    session: &ResidentSession<H, O>,
    context: &crate::ActorSessionContext,
    source: &ActorWorkbenchSource,
    effect_stack: &str,
    type_modules: &[String],
    block: &ParsedBlock,
) -> Result<CompiledBlock, ResidentActorWorkbenchError>
where
    H: DispatchEffect<O> + Send,
    O: OutputSink + Sync,
{
    let compile_view = actor_compile_view(session, context, source, type_modules)?;
    let prepared = source.prepare(&compile_view);
    let mut templates =
        resident_workbench_templates(&prepared.preamble, effect_stack, &prepared.imports);
    let include_refs: Vec<_> = prepared.include.iter().map(PathBuf::as_path).collect();
    let mut verdict = match classify_workbench_item(&block.source) {
        Ok(WorkbenchItem::Declaration(_)) => Some(TurnClassification {
            kind: TurnKind::Decl,
            binders: Vec::new(),
            items: Vec::new(),
        }),
        Ok(WorkbenchItem::Haskell(_) | WorkbenchItem::Command(_)) => None,
        Err(diagnostic) => return Ok(CompiledBlock::Rejected(diagnostic)),
    };
    if verdict.is_none() {
        verdict = tidepool_runtime::session::classify_block(&[&block.source])
            .map_err(|error| ResidentActorWorkbenchError::CompileInfrastructure(error.to_string()))?
            .into_iter()
            .next();
    }
    let observation = if verdict
        .as_ref()
        .is_some_and(|verdict| verdict.kind == TurnKind::Expr)
    {
        let mut name = format!("observation{}", compile_view.next_value_generation().0);
        let visible = session.workbench_bindings_in(context.placement.lexical_scope);
        while visible.iter().any(|binding| binding.name == name) {
            name.push('_');
        }
        let preamble = insert_preamble_imports(&prepared.preamble, &prepared.imports);
        templates = [
            tidepool_runtime::session::ExpressionLift::Effectful,
            tidepool_runtime::session::ExpressionLift::Pure,
        ]
        .into_iter()
        .map(|lift| tidepool_runtime::session::TurnTemplate {
            kind: tidepool_runtime::session::TemplateSelector::Bind,
            source: tidepool_runtime::session::turn::assemble_observation_module(
                &preamble,
                "__result",
                effect_stack,
                "{{TURN}}",
                lift,
            ),
        })
        .collect();
        verdict = Some(TurnClassification {
            kind: TurnKind::Bind,
            binders: vec![name.clone()],
            items: Vec::new(),
        });
        Some(name)
    } else {
        None
    };
    tracing::debug!(
        actor_id = context.actor.id.0,
        incarnation = context.actor.incarnation.0,
        lexical_scope = ?context.placement.lexical_scope,
        generation = compile_view.next_value_generation().0,
        imports = %compile_view.turn_imports(),
        injected = ?prepared.injected,
        source = %block.source,
        "compiling resident actor workbench item"
    );
    let request = TurnRequest {
        turn_text: &block.source,
        templates: &templates,
        include: &include_refs,
        session_root: compile_view.session_root(),
        inject_modules: &prepared.injected,
        gen: compile_view.next_value_generation().0,
        verdict,
        target: None,
    };
    match run_turn(request) {
        Ok(result) => Ok(CompiledBlock::Ready(Box::new(ReadyBlock {
            result,
            generation: compile_view.next_value_generation(),
            declaration_source: block.source.clone(),
            declaration_imports: compile_view.workbench_imports(),
            observation,
        }))),
        Err(failure) if classify_compile(&failure.error).class == FailureClass::UserHaskell => {
            let label = format!("<input unit {}>", block.ordinal);
            Ok(CompiledBlock::Rejected(render_turn_compile_error(
                &failure.error,
                failure.attempted_source.as_deref(),
                &block.source,
                &label,
            )))
        }
        Err(failure) => Err(ResidentActorWorkbenchError::CompileInfrastructure(
            classify_compile(&failure.error).message,
        )),
    }
}

fn run_discovery<H, O>(
    session: &ResidentSession<H, O>,
    context: &crate::ActorSessionContext,
    source: &ActorWorkbenchSource,
    type_modules: &[String],
    block: &ParsedBlock,
) -> Result<Result<String, String>, ResidentActorWorkbenchError>
where
    H: DispatchEffect<O> + Send,
    O: OutputSink + Sync,
{
    let line = match MetaCommandLine::parse(&block.source) {
        Ok(line) => line,
        Err(diagnostic) => return Ok(Err(diagnostic)),
    };
    let command = match line.discovery() {
        Ok(Some(command)) => command,
        Ok(None) => {
            return Ok(Err(format!(
            "unknown actor workbench command `:{}` (supported: :status, :status!, :lineage, :trace, :type/:t, :info/:i, :browse, :browse!, :bindings/:b, :recovery, :show imports, :doc)",
            line.name
        )))
        }
        Err(diagnostic) => return Ok(Err(diagnostic)),
    };
    match command {
        WorkbenchDiscovery::ShowImports => {
            let imports = actor_compile_view(session, context, source, &[])?
                .workbench_imports()
                .source_lines();
            Ok(Ok(if imports.is_empty() {
                "no persistent imports".into()
            } else {
                imports.join("\n")
            }))
        }
        WorkbenchDiscovery::Bindings => {
            let bindings = session.workbench_bindings_in(context.placement.lexical_scope);
            let queries = bindings
                .iter()
                .filter_map(|binding| binding.type_query())
                .map(|expression| InspectionQuery::TypeOf(expression.to_owned()))
                .collect::<Vec<_>>();
            let inspected = if queries.is_empty() {
                Vec::new()
            } else {
                inspect_actor_batch(session, context, source, type_modules, &queries)?
            };
            let mut inspected = inspected.into_iter();
            let lines = bindings
                .into_iter()
                .map(|binding| {
                    let needs_inspection = binding.type_query().is_some();
                    let kind = binding.kind.label();
                    let rendered = match binding.type_display {
                        Some(type_display) => format!("{} :: {type_display}", binding.name),
                        None if needs_inspection => inspected
                            .next()
                            .and_then(Result::ok)
                            .unwrap_or_else(|| format!("{} :: <type unavailable>", binding.name)),
                        None => format!("{} :: <type unavailable>", binding.name),
                    };
                    format!("{rendered} [{kind}]")
                })
                .collect::<Vec<_>>();
            Ok(Ok(if lines.is_empty() {
                "no persistent bindings".into()
            } else {
                lines.join("\n")
            }))
        }
        WorkbenchDiscovery::Recovery => {
            let Some(report) = session.declaration_recovery_report() else {
                return Ok(Ok("declaration recovery is not configured".into()));
            };
            let warning = session
                .recovery_manifest_warning()
                .map(|warning| format!("; manifest_warning={warning}"))
                .unwrap_or_default();
            let mut lines = vec![format!(
                "declaration recovery: source_session={:?}; successor_session={}; replayed={}; lost={}{}",
                report.source_session,
                report.successor_session,
                report.replayed.len(),
                report.lost.len(),
                warning,
            )];
            lines.extend(report.replayed.iter().map(|item| {
                format!(
                    "replayed session {} generation {} -> {} source_hash={}",
                    item.origin_session,
                    item.source_generation,
                    item.successor_generation,
                    item.source_hash
                )
            }));
            lines.extend(report.lost.iter().map(|item| {
                format!(
                    "lost session {} generation {} source_hash={} reason={}",
                    item.origin_session, item.source_generation, item.source_hash, item.reason
                )
            }));
            Ok(Ok(lines.join("\n")))
        }
        WorkbenchDiscovery::Doc(topic) => Ok(crate::prompt_catalog::workbench_doc(&topic)
            .map(str::trim)
            .map(str::to_owned)),
        WorkbenchDiscovery::Info(name) => inspect_actor(
            session,
            context,
            source,
            type_modules,
            InspectionQuery::Info(name),
        ),
        WorkbenchDiscovery::Type(expression) => inspect_actor(
            session,
            context,
            source,
            type_modules,
            InspectionQuery::TypeOf(expression),
        ),
        WorkbenchDiscovery::Browse { module, expanded } => {
            let module = match module
                .or_else(|| source.default_browse_module.as_deref().map(str::to_owned))
            {
                Some(module) => module,
                None => return Ok(Err(":browse has no configured actor API module".into())),
            };
            inspect_actor(
                session,
                context,
                source,
                type_modules,
                InspectionQuery::Browse { module, expanded },
            )
        }
    }
}

fn inspection_query(
    source: &ActorWorkbenchSource,
    raw: &str,
    kind: GhciInputKind,
) -> Result<Option<InspectionQuery>, String> {
    if kind != GhciInputKind::Command {
        return Ok(None);
    }
    let line = MetaCommandLine::parse(raw)?;
    match line.discovery()? {
        Some(WorkbenchDiscovery::Type(expression)) => Ok(Some(InspectionQuery::TypeOf(expression))),
        Some(WorkbenchDiscovery::Info(name)) => Ok(Some(InspectionQuery::Info(name))),
        Some(WorkbenchDiscovery::Browse { module, expanded }) => {
            let module = module
                .or_else(|| source.default_browse_module.as_deref().map(str::to_owned))
                .ok_or_else(|| ":browse has no configured actor API module".to_string())?;
            Ok(Some(InspectionQuery::Browse { module, expanded }))
        }
        Some(
            WorkbenchDiscovery::Bindings
            | WorkbenchDiscovery::Recovery
            | WorkbenchDiscovery::ShowImports
            | WorkbenchDiscovery::Doc(_),
        )
        | None => Ok(None),
    }
}

fn inspect_actor<H, O>(
    session: &ResidentSession<H, O>,
    context: &crate::ActorSessionContext,
    source: &ActorWorkbenchSource,
    type_modules: &[String],
    query: InspectionQuery,
) -> Result<Result<String, String>, ResidentActorWorkbenchError>
where
    H: DispatchEffect<O> + Send,
    O: OutputSink + Sync,
{
    let mut results = inspect_actor_batch(session, context, source, type_modules, &[query])?;
    Ok(results
        .pop()
        .unwrap_or_else(|| Err("inspection returned no result".into())))
}

fn inspect_actor_batch<H, O>(
    session: &ResidentSession<H, O>,
    context: &crate::ActorSessionContext,
    source: &ActorWorkbenchSource,
    type_modules: &[String],
    queries: &[InspectionQuery],
) -> Result<Vec<Result<String, String>>, ResidentActorWorkbenchError>
where
    H: DispatchEffect<O> + Send,
    O: OutputSink + Sync,
{
    let compile_view = actor_compile_view(session, context, source, type_modules)?;
    inspect_compile_view(&compile_view, source, queries)
}

fn inspect_compile_view(
    compile_view: &crate::ActorCompileView,
    source: &ActorWorkbenchSource,
    queries: &[InspectionQuery],
) -> Result<Vec<Result<String, String>>, ResidentActorWorkbenchError> {
    let prepared = source.prepare(compile_view);
    let include_refs = prepared
        .include
        .iter()
        .map(PathBuf::as_path)
        .collect::<Vec<_>>();
    match run_inspections(InspectionRequest {
        preamble: &prepared.preamble,
        imports: &prepared.imports,
        include: &include_refs,
        session_root: compile_view.session_root(),
        inject_modules: &prepared.injected,
        queries,
    }) {
        Ok(results) if results.len() == queries.len() => Ok(results
            .into_iter()
            .map(|result| match result {
                tidepool_runtime::session::InspectionResult::NotFound { .. }
                | tidepool_runtime::session::InspectionResult::ModuleNotFound { .. }
                | tidepool_runtime::session::InspectionResult::Rejected { .. } => {
                    Err(result.render())
                }
                _ => Ok(result.render()),
            })
            .collect()),
        Ok(results) => Err(ResidentActorWorkbenchError::CompileInfrastructure(format!(
            "inspection returned {} results for {} queries",
            results.len(),
            queries.len()
        ))),
        Err(error) if classify_compile(&error).class == FailureClass::UserHaskell => {
            Ok(vec![Err(classify_compile(&error).message)])
        }
        Err(error) => Err(ResidentActorWorkbenchError::Compile(error)),
    }
}

fn projected_binding_receipt(
    bound_name: Option<&str>,
    output: &[String],
) -> Result<String, ResidentActorWorkbenchError> {
    let name = bound_name.ok_or_else(|| {
        ResidentActorWorkbenchError::ActorProtocol(
            "projected binding completed without binder metadata".into(),
        )
    })?;
    let mut receipt = format!("bound `{name}`");
    if !output.is_empty() {
        receipt.push_str("\n\nOutput:\n");
        receipt.push_str(&output.join("\n"));
    }
    Ok(receipt)
}

#[cfg(test)]
mod request_tests {
    use super::*;

    #[tokio::test]
    async fn incomplete_compilation_retires_only_its_registered_machine() {
        use tidepool_repr::{CoreFrame, Literal, PrimOpKind, SessionId, TreeBuilder, VarId};
        let machines = Arc::new(SessionRegistry::new());
        for id in [SessionId(1), SessionId(2)] {
            let mut b = TreeBuilder::new();
            b.push(CoreFrame::Lit(Literal::LitInt(42)));
            let session = ResidentSession::bootstrap(
                &b.build(),
                DataConTable::new(),
                frunk::HNil,
                tidepool_mcp::CapturedOutput::new(),
                Vec::new(),
                64 * 1024,
                None,
            )
            .unwrap();
            machines.insert_idle(id, session);
        }
        let access =
            ResidentMachineAccess::new(machines.clone(), ActorWorkbenchSource::new("", Vec::new()));
        let failed = access
            .with_host_machine(SessionId(1), None, |session, _| {
                let mut b = TreeBuilder::new();
                let invalid = b.push(CoreFrame::PrimOp {
                    op: PrimOpKind::SeqOp,
                    args: vec![],
                });
                b.push(CoreFrame::Lam {
                    binder: VarId(1),
                    body: invalid,
                });
                session
                    .run("invalid_job", &b.build(), &DataConTable::new())
                    .map(|_| ())
                    .map_err(ResidentActorWorkbenchError::Resident)
            })
            .await;
        assert!(matches!(
            failed,
            Err(ResidentActorWorkbenchError::Resident(
                ResidentError::AddFunction(_)
            ))
        ));
        assert!(matches!(
            machines.checkout_run(SessionId(1)),
            Err(CheckoutError::Unknown(SessionId(1)))
        ));
        let sibling = access
            .with_host_machine(SessionId(2), None, |session, _| {
                assert!(!session.compilation_failed());
                Ok(42)
            })
            .await
            .unwrap();
        assert_eq!(sibling, 42);
        assert!(machines.checkout_run(SessionId(2)).is_ok());
    }

    #[test]
    fn runtime_rejection_identifies_input_and_preserves_pattern_cause() {
        use tidepool_codegen::{
            host_fns::RuntimeError, jit_machine::JitError, yield_type::YieldError,
        };
        let error = tidepool_runtime::RuntimeError::Jit(JitError::Yield(YieldError::Runtime(
            RuntimeError::PatternMatchFailure("Expr.hs:35:7-55|Just x".into()),
        )));
        assert_eq!(
            render_runtime_rejection(4, &error),
            "<input unit 4>: runtime error: pattern match failure: Expr.hs:35:7-55|Just x"
        );
        let error =
            tidepool_runtime::RuntimeError::Jit(JitError::InvalidSuspensionState("missing"));
        assert_eq!(
            render_runtime_rejection(2, &error),
            "<input unit 2>: runtime error: invalid suspension state: missing"
        );
    }

    #[test]
    fn matched_request_with_invalid_deadline_is_not_skipped_by_dispatch() {
        use tidepool_repr::{DataCon, DataConId};
        let mut table = tidepool_testing::gen::datacon_table::standard_datacon_table();
        let submit = DataConId(100);
        table.insert(DataCon {
            id: submit,
            name: "SubmitRequestWith".into(),
            tag: 1,
            rep_arity: 4,
            field_bangs: Vec::new(),
            qualified_name: Some("Tidepool.Agent.Reply.Internal.SubmitRequestWith".into()),
            type_name: "Replies".into(),
        });
        let request = Value::Con(
            submit,
            vec![
                1_i64.to_value(&table).unwrap(),
                Value::Con(DataConId(999), vec![]),
                (2_i64, 1_i64).to_value(&table).unwrap(),
                Some(Value::Con(DataConId(998), vec![]))
                    .to_value(&table)
                    .unwrap(),
            ],
        );
        assert!(matches!(
            ResidentRequest::decode(&request, &table),
            Err(ResidentActorWorkbenchError::RequestDecode {
                source: BridgeError::FieldDecode { field: 4, .. },
                ..
            })
        ));
    }

    #[test]
    fn bare_browse_resolves_the_actor_incarnations_configured_api_module() {
        let source = ActorWorkbenchSource::new("module Expr where\n", Vec::new())
            .with_default_browse_module("Tidepool.Actors.Shoal");
        assert_eq!(
            source.workbench_imports.source_lines(),
            ["import Tidepool.Actors.Shoal"]
        );
        assert_eq!(
            inspection_query(&source, ":browse", GhciInputKind::Command).unwrap(),
            Some(InspectionQuery::Browse {
                module: "Tidepool.Actors.Shoal".into(),
                expanded: false,
            })
        );
        assert_eq!(
            inspection_query(&source, ":browse!", GhciInputKind::Command).unwrap(),
            Some(InspectionQuery::Browse {
                module: "Tidepool.Actors.Shoal".into(),
                expanded: true,
            })
        );
        assert_eq!(
            inspection_query(&source, ":browse", GhciInputKind::Code).unwrap(),
            None
        );
    }
}
