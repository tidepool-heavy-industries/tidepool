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

const MACHINE_WAIT: Duration = Duration::from_secs(30);

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
        preamble.push_str(&format!(
            "\nsessionReply :: TidepoolReplies.Reply ({0})\nsessionReply = TidepoolReplies.Reply (TidepoolReplies.RequestId {1})\nrespond :: ({0}) -> Eff {2} TidepoolVoid.Void\nrespond = TidepoolReplies.reply sessionReply\n",
            self.expected_type(),
            request.0,
            effects_alias,
        ));
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
    conditional_imports: Arc<[ConditionalWorkbenchImport]>,
}

#[derive(Clone)]
struct ConditionalWorkbenchImport {
    triggers: Arc<[Arc<str>]>,
    import: Arc<str>,
}

impl ActorWorkbenchSource {
    #[must_use]
    pub fn new(preamble: impl Into<Arc<str>>, base_include: Vec<PathBuf>) -> Self {
        Self {
            preamble: preamble.into(),
            base_include: base_include.into(),
            default_browse_module: None,
            workbench_imports: SourceImports::new(),
            conditional_imports: Arc::new([]),
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
        self.conditional_imports = Arc::new([ConditionalWorkbenchImport {
            triggers: ["[fmt|", "[j|", "[patch|", "[uri|"]
                .into_iter()
                .map(Arc::from)
                .collect(),
            import: Arc::from("Tidepool.QQ (fmt, j, patch, uri)"),
        }]);
        self
    }

    fn imports_for(&self, input: &str) -> SourceImports {
        let mut imports = SourceImports::new();
        for conditional in self.conditional_imports.iter() {
            if conditional
                .triggers
                .iter()
                .any(|trigger| input.contains(trigger.as_ref()))
            {
                imports.extend_text(conditional.import.as_ref());
            }
        }
        imports
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
    warnings: Vec<String>,
}

enum WorkbenchDisplay {
    Binding(Vec<String>),
    Haskell,
    Opaque,
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

/// The only suspensions the trusted V0 initialization driver settles.
/// Other nominal effects will join this classifier when their actor-local
/// interpreters land; they must never be mistaken for readiness.
pub(crate) enum ResidentActorStartupStep {
    InstallShutdown(ResidentActorShutdown),
    Attach(ResidentAgentAttachment),
    Ready(ResidentActorReadiness),
}

pub(crate) struct ResidentAgentAttachment {
    pub(crate) continuation: ResidentHole,
    pub(crate) initial_user_message: Option<String>,
}

pub(crate) struct AgentRosterProjection {
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
}

pub(crate) struct CleanupReceiptProjection {
    pub plan: CleanupPlanProjection,
    pub steps: Vec<CleanupStepProjection>,
    pub complete: bool,
}

fn agent_roster_value(
    table: &DataConTable,
    entry: AgentRosterProjection,
) -> Result<Value, ResidentActorWorkbenchError> {
    let usage = entry.runtime.latest_provider_usage();
    let supervisor = entry.descriptor.supervisor_parent();
    let context_parent = entry.descriptor.context_parent();
    let usage_scope = usage
        .map(|sample| match sample.scope {
            crate::ProviderUsageScope::LastProviderResponse => {
                actor_context_constructor(table, "UsageLastProviderResponse", Vec::new())
            }
        })
        .transpose()?;
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
    Ok(actor_context_constructor(
        table,
        "AgentRosterEntry",
        vec![
            actor_int(entry.actor.id.0)?.to_value(table)?,
            actor_int(entry.actor.incarnation.0)?.to_value(table)?,
            entry.descriptor.label().to_owned().to_value(table)?,
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
            usage
                .map(|sample| sample.cached_input_tokens)
                .to_value(table)?,
            usage
                .map(|sample| sample.uncached_input_tokens)
                .to_value(table)?,
            usage
                .and_then(|sample| sample.activation_sequence)
                .map(actor_int)
                .transpose()?
                .to_value(table)?,
            usage
                .map(|sample| actor_int(sample.observed_at_unix_ms))
                .transpose()?
                .to_value(table)?,
            usage_scope.to_value(table)?,
            cache_boundary.to_value(table)?,
            actor_int(entry.runtime.event_watermark)?.to_value(table)?,
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
    Completed,
    ActorContext(ResidentHole),
    ForkGroup(ForkGroupBoundary),
    Start(crate::ResidentActorStart),
    Outbound(ResidentOutbound),
    Wait(ResidentWaitRequest),
    Poll(crate::wait::ResidentPollRequest),
    Receive(InstalledReceiver),
    ToolAwait(crate::resident_tools::ResidentToolAwait),
    ToolReply(crate::resident_tools::ResidentToolReply),
    AgentSession(crate::ResidentInteractiveSession),
    AgentAttachment(ResidentAgentAttachment),
    AgentInspect(AgentInspectionBoundary),
    AgentList(ResidentHole),
    AgentForget(AgentInspectionBoundary),
    AgentStop(AgentInspectionBoundary),
    CleanupPlan {
        continuation: ResidentHole,
        group: crate::ForkGroupId,
    },
    CleanupExecute {
        continuation: ResidentHole,
        group: crate::ForkGroupId,
    },
    RequestReservation(RequestReservation),
    RequestSubmission(RequestSubmission),
    ReplyAttempt(ReplyAttempt),
    ResponsePoll(ResponsePoll),
    RequestCancellation(RequestCancellation),
    ResponseAbandonment(ResponseAbandonment),
    ResponseForget(ResponseForget),
    ReplyPoll(ReplyPoll),
    CancellationAcknowledgement(CancellationAcknowledgement),
    WatchRegistration(WatchRegistration),
    WatchPoll(WatchPoll),
    WatchForget(WatchForget),
}

pub(crate) enum ForkGroupBoundary {
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
    pub(crate) fn commits_with_workbench_unit(&self) -> bool {
        matches!(self, Self::ForkGroup(ForkGroupBoundary::Commit { .. }))
    }

    pub(crate) fn operation(&self) -> &'static str {
        match self {
            Self::Completed => "program completion",
            Self::ActorContext(_) => "actorContext",
            Self::ForkGroup(ForkGroupBoundary::Begin { .. }) => "begin context-fork group",
            Self::ForkGroup(ForkGroupBoundary::Commit { .. }) => "commit context-fork group",
            Self::ForkGroup(ForkGroupBoundary::Abort { .. }) => "abort context-fork group",
            Self::ForkGroup(ForkGroupBoundary::Cleanup { .. }) => "cleanup context-fork group",
            Self::Start(_) => "startActor",
            Self::Outbound(ResidentOutbound::Call { .. }) => "call",
            Self::Outbound(ResidentOutbound::TryCall { .. }) => "tryCall",
            Self::Outbound(ResidentOutbound::Cast { .. }) => "cast",
            Self::Wait(_) => "awaitExit",
            Self::Poll(_) => "pollExit",
            Self::Receive(_) => "receive",
            Self::ToolAwait(_) => "agent tool await",
            Self::ToolReply(_) => "agent tool reply",
            Self::AgentSession(_) => "agent session",
            Self::AgentAttachment(_) => "agent attachment",
            Self::AgentInspect(_) => "observeAgent",
            Self::AgentList(_) => "listAgents",
            Self::AgentForget(_) => "forgetAgent",
            Self::AgentStop(_) => "stopAgent",
            Self::CleanupPlan { .. } => "planCleanup",
            Self::CleanupExecute { .. } => "executeCleanup",
            Self::RequestReservation(_) => "request",
            Self::RequestSubmission(_) => "request",
            Self::ReplyAttempt(_) => "reply",
            Self::ResponsePoll(_) => "pollResponse",
            Self::RequestCancellation(_) => "cancelRequest",
            Self::ResponseAbandonment(_) => "abandonResponse",
            Self::ResponseForget(_) => "forgetResponse",
            Self::ReplyPoll(_) => "pollReply",
            Self::CancellationAcknowledgement(_) => "acknowledgeCancellation",
            Self::WatchRegistration(_) => "watch",
            Self::WatchPoll(_) => "pollWatch",
            Self::WatchForget(_) => "forgetWatch",
        }
    }
}

/// The one nominal roster for requests interpreted at actor execution
/// boundaries. Generated request enums own constructor recognition and field
/// shape; this sum owns orchestration routing.
enum ResidentRequest {
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
            Self::Actor(crate::generated::actor::ActorReq::ActorBeginForkGroupWith(..)) => {
                "begin context-fork group"
            }
            Self::ActorContext(
                crate::generated::actor_context::ActorContextReq::ActorContextWith,
            ) => "actorContext",
            Self::AgentControl(
                crate::generated::agent_control::AgentControlReq::AgentControlStopWith(..),
            ) => "stopAgent",
            Self::AgentControl(
                crate::generated::agent_control::AgentControlReq::AgentControlPlanCleanupWith(..),
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
                crate::generated::agent_inspection::AgentInspectionReq::AgentForgetWith(..),
            ) => "forgetAgent",
            Self::AgentLaunch(crate::generated::agent_launch::AgentLaunchReq::AgentLaunchWith(
                ..,
            )) => "startAgent",
            Self::Forks(crate::generated::forks::ForksReq::ForksBeginWith(..)) => {
                "begin context-fork group"
            }
            Self::Forks(crate::generated::forks::ForksReq::ForksStartWith(..)) => "context fork",
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
            Self::ActorKernel(
                crate::generated::actor_kernel::ActorKernelReq::ActorInstallShutdownWith(..),
            ) => "installShutdown",
            Self::ActorKernel(crate::generated::actor_kernel::ActorKernelReq::ActorReadyWith) => {
                "ready"
            }
            Self::ActorKernel(crate::generated::actor_kernel::ActorKernelReq::ActorReplyWith(
                ..,
            )) => "reply",
            Self::ActorKernel(
                crate::generated::actor_kernel::ActorKernelReq::ActorContinueWith(..),
            ) => "continue",
            Self::ActorLocal(crate::generated::actor_local::ActorLocalReq::ActorReceiveWith(
                ..,
            )) => "receive",
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
            Self::Watches(WatchesReq::ObserveWatchWith(..)) => "pollWatch",
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
pub enum ResidentActorWorkbenchError {
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
        self.with_machine_wait(context, MACHINE_WAIT, operation)
            .await
    }

    async fn with_machine_wait<ResultValue>(
        &self,
        context: crate::ActorSessionContext,
        max_wait: Duration,
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
        let checkout = self
            .machines
            .checkout_wait(
                context.placement.session,
                tidepool_runtime::session::registry::CheckoutRequest::Run,
                max_wait,
            )
            .await
            .map_err(ResidentActorWorkbenchError::Checkout)?;
        let (mut session, receipt) = checkout.into_parts();
        let source = self.source.clone();
        let machines = Arc::clone(&self.machines);

        let task = tokio::task::spawn_blocking(move || {
            // The blocking task owns the machine and its linear checkout
            // receipt together. Its async caller may be cooperatively
            // cancelled while this closure is running; settlement must not
            // depend on that caller continuing to poll the JoinHandle.
            let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                tracing::debug!(
                    actor_id = context.actor.id.0,
                    incarnation = context.actor.incarnation.0,
                    session = ?context.placement.session,
                    resource_scope = ?context.placement.resource_scope,
                    lexical_scope = ?context.placement.lexical_scope,
                    "entering resident actor machine"
                );
                session
                    .set_actor_execution(
                        context.run_context(),
                        context.effect_policy,
                        context.live_payload,
                    )
                    .map_err(ResidentActorWorkbenchError::Resident)?;
                operation(&mut session, &context, &source)
            }));
            match outcome {
                Ok(outcome) => {
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

    pub(crate) async fn resume_request(
        &self,
        context: crate::ActorSessionContext,
        hole: ResidentHole,
        answer: RootCustody,
    ) -> Result<ResidentOutcome, ResidentActorWorkbenchError> {
        self.access
            .with_machine(context, move |session, _context, _source| {
                session
                    .resume_handle(hole, answer)
                    .map_err(ResidentActorWorkbenchError::Resident)
            })
            .await
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
        self.access
            .with_machine(context, move |session, context, _| {
                inspect_actor_batch(session, context, &turn_source, &type_modules, &queries)
            })
            .await
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
            bound, compiled, ..
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
            let display = if names.is_empty() {
                WorkbenchDisplay::Opaque
            } else {
                WorkbenchDisplay::Binding(names)
            };
            start_fragment_settlement(session, context, display, warnings, outcome)
        }
        TurnResult::Expr {
            variant, compiled, ..
        } => {
            let warnings = compiled.warnings.warnings.clone();
            let outcome = session.run_with_sites(
                "actor_interactive_expr",
                &compiled.expr,
                &compiled.table,
                &compiled.asks,
            );
            let display = if variant < 2 {
                WorkbenchDisplay::Haskell
            } else {
                WorkbenchDisplay::Opaque
            };
            start_fragment_settlement(session, context, display, warnings, outcome)
        }
    }
}

fn start_fragment_settlement<H, O>(
    session: &mut ResidentSession<H, O>,
    context: &crate::ActorSessionContext,
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
                warnings,
            },
            outcome,
        ),
        Err(ResidentError::Run(error)) => Ok(ResidentWorkbenchStep::Rejected(error.to_string())),
        Err(error) => Err(ResidentActorWorkbenchError::Resident(error)),
    }
}

fn settle_fragment<H, O>(
    session: &mut ResidentSession<H, O>,
    _context: &crate::ActorSessionContext,
    mut fragment: ResidentWorkbenchFragment,
    outcome: ResidentOutcome,
) -> Result<ResidentWorkbenchStep, ResidentActorWorkbenchError>
where
    H: DispatchEffect<O> + Send,
    O: OutputSink + Sync,
{
    match outcome {
        ResidentOutcome::Completed { output, result } => {
            fragment.output.extend(output);
            let installed_bindings = match &fragment.display {
                WorkbenchDisplay::Binding(names) => names.clone(),
                WorkbenchDisplay::Haskell | WorkbenchDisplay::Opaque => Vec::new(),
            };
            let receipt = match fragment.display {
                WorkbenchDisplay::Binding(names) => format!("[bound {}]", names.join(", ")),
                WorkbenchDisplay::Haskell => {
                    haskell_display(&result).unwrap_or_else(|| "<rendering unavailable>".into())
                }
                WorkbenchDisplay::Opaque => "<opaque value>".into(),
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
                WorkbenchDisplay::Haskell | WorkbenchDisplay::Opaque => None,
            };
            let receipt = projected_binding_receipt(bound_name.as_deref(), &output)?;
            let installed_bindings = match fragment.display {
                WorkbenchDisplay::Binding(names) => names,
                WorkbenchDisplay::Haskell | WorkbenchDisplay::Opaque => Vec::new(),
            };
            Ok(ResidentWorkbenchStep::Committed {
                output: receipt,
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

fn haskell_display(result: &tidepool_runtime::EvalResult) -> Option<String> {
    let Value::Con(constructor, fields) = result.value() else {
        return None;
    };
    if result.table().name_of(*constructor) != Some("(,)") {
        return None;
    }
    let [_, rendered] = fields.as_slice() else {
        return None;
    };
    tidepool_runtime::value_to_json(rendered, result.table(), 0)
        .as_str()
        .map(str::to_owned)
}

impl<H, O> ResidentActorRunner<H, O>
where
    H: DispatchEffect<O> + Send + 'static,
    O: OutputSink + Sync + 'static,
{
    pub(crate) async fn capture_boundary(
        &self,
        context: crate::ActorSessionContext,
        outcome: ResidentOutcome,
        actor_realm: RealmId,
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
                        label,
                        role,
                        profile,
                        worktrees,
                        None,
                        None,
                        None,
                        context.placement.session,
                        context.actor,
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
                    )) => {
                        let group = u64::try_from(group).map_err(|_| {
                            ResidentActorWorkbenchError::ActorProtocol(format!(
                                "invalid fork group id {group}"
                            ))
                        })?;
                        crate::ResidentActorStart::capture_decoded(
                            session,
                            hole,
                            label,
                            role,
                            profile,
                            worktrees,
                            Some(crate::ForkGroupId(group)),
                            Some(match worktree_spec {
                                Some(spec) => crate::ForkWorkspaceSeed::Explicit(spec),
                                None => crate::ForkWorkspaceSeed::BoundHead(bound_dirty_policy),
                            }),
                            Some(effect_keys),
                            context.placement.session,
                            context.actor,
                        )
                        .map(ResidentActorBoundary::Start)
                        .map_err(ResidentActorWorkbenchError::StartCapture)
                    }
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
                    ResidentRequest::AgentControl(
                        crate::generated::agent_control::AgentControlReq::AgentControlStopWith(
                            target,
                        ),
                    ) => Ok(ResidentActorBoundary::AgentStop(AgentInspectionBoundary {
                        target: crate::wait::decode_address(target.0, target.1)?,
                        continuation: hole,
                    })),
                    ResidentRequest::AgentControl(
                        crate::generated::agent_control::AgentControlReq::AgentControlPlanCleanupWith(
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
                            group,
                        ),
                    ) => Ok(ResidentActorBoundary::CleanupExecute {
                        continuation: hole,
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
                    ResidentRequest::ActorLocal(
                        crate::generated::actor_local::ActorLocalReq::ActorReceiveWith(site, _),
                    ) => capture_receiver_boundary(session, hole, site, actor_realm),
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
                                    .map(crate::request_effect::RequestDeadlineWire::checked)
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
                    ResidentRequest::Watches(WatchesReq::RegisterWatchWith(
                        label,
                        dependencies,
                    )) => {
                        let dependencies = dependencies
                            .into_iter()
                            .map(|(request, allow_failure)| {
                                crate::request_effect::request_id(request)
                                    .map(|request| (request, allow_failure))
                            })
                            .collect::<Result<Vec<_>, _>>()?;
                        Ok(ResidentActorBoundary::WatchRegistration(
                            WatchRegistration {
                                continuation: hole,
                                dependencies,
                                label,
                            },
                        ))
                    }
                    ResidentRequest::Watches(WatchesReq::ObserveWatchWith(watch_id)) => {
                        Ok(ResidentActorBoundary::WatchPoll(WatchPoll {
                            continuation: hole,
                            watch: crate::request_effect::watch_id(watch_id)?,
                        }))
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
                admission_timeout,
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

    pub(crate) async fn run_mailbox_handler(
        &self,
        context: crate::ActorSessionContext,
        handler: RootCustody,
        request: RootCustody,
        handler_realm: RealmId,
    ) -> Result<ResidentOutcome, ResidentActorWorkbenchError> {
        self.access
            .with_machine(context, move |session, _context, _| {
                session
                    .run_rooted_application(
                        "actor_mailbox_handler",
                        handler,
                        request,
                        handler_realm,
                        None,
                    )
                    .map_err(ResidentActorWorkbenchError::Resident)
            })
            .await
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
                    | crate::generated::actor_kernel::ActorKernelReq::ActorReadyWith => {
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
                    usage
                        .map(|sample| sample.cached_input_tokens)
                        .to_value(table)?,
                    usage
                        .map(|sample| sample.uncached_input_tokens)
                        .to_value(table)?,
                    i64::from(descendants.maximum_depth).to_value(table)?,
                    i64::from(descendants.maximum_active_children).to_value(table)?,
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

fn actor_int(value: u64) -> Result<i64, ResidentActorWorkbenchError> {
    i64::try_from(value).map_err(|_| {
        ResidentActorWorkbenchError::ActorProtocol(
            "runtime actor identity exceeds Haskell Int".into(),
        )
    })
}

fn actor_context_constructor(
    table: &DataConTable,
    name: &str,
    fields: Vec<Value>,
) -> Result<Value, tidepool_bridge::BridgeError> {
    let qualified = format!("Tidepool.Effects.Core.{name}");
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
    let compile_view = actor_compile_view(session, context, source, type_modules)?
        .with_workbench_imports(&source.imports_for(&block.source));
    let templates =
        resident_workbench_templates(&source.preamble, effect_stack, &compile_view.turn_imports());
    let include = compile_view.include_paths(&source.base_include);
    let include_refs: Vec<_> = include.iter().map(PathBuf::as_path).collect();
    let injected = compile_view.injected_module_names();
    let verdict = match classify_workbench_item(&block.source) {
        Ok(WorkbenchItem::Declaration(_)) => Some(TurnClassification {
            kind: TurnKind::Decl,
            binders: Vec::new(),
            items: Vec::new(),
        }),
        Ok(WorkbenchItem::Haskell(_) | WorkbenchItem::Command(_)) => None,
        Err(diagnostic) => return Ok(CompiledBlock::Rejected(diagnostic)),
    };
    tracing::debug!(
        actor_id = context.actor.id.0,
        incarnation = context.actor.incarnation.0,
        lexical_scope = ?context.placement.lexical_scope,
        generation = compile_view.next_value_generation().0,
        imports = %compile_view.turn_imports(),
        injected = ?injected,
        source = %block.source,
        "compiling resident actor workbench item"
    );
    let request = TurnRequest {
        turn_text: &block.source,
        templates: &templates,
        include: &include_refs,
        session_root: compile_view.session_root(),
        inject_modules: &injected,
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
            "unknown actor workbench command `:{}` (supported: :status, :type/:t, :info/:i, :browse, :browse!, :bindings/:b, :recovery, :show imports, :doc)",
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
    let includes = compile_view.include_paths(&source.base_include);
    let include_refs = includes.iter().map(PathBuf::as_path).collect::<Vec<_>>();
    let injected = compile_view.injected_module_names();
    let imports = compile_view.turn_imports();
    match run_inspections(InspectionRequest {
        preamble: &source.preamble,
        imports: &imports,
        include: &include_refs,
        session_root: compile_view.session_root(),
        inject_modules: &injected,
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
