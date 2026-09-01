//! Actor binding for the shared resident Haskell workbench.
//!
//! This adapter owns no machine and holds no checkout between calls. Each
//! fenced block checks out the actor's registered resident session, installs
//! the exact actor context, compiles and runs one segment on the blocking
//! pool, then restores the machine before the provider loop continues.

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
    insert_preamble_imports, resident_workbench_templates, run_turn, BlockExecution, OutputSink,
    ParsedBlock, ResidentError, ResidentHole, ResidentOutcome, ResidentSession, RootCustody,
    SessionRunContext, TurnRequest, TurnResult,
};
use tidepool_runtime::{classify_compile, classify_session, CompileError, FailureClass};

use crate::mailbox::{InstalledReceiver, KernelValue, ResidentOutbound, ResidentWaitRequest};
use crate::{
    ActorCompileViewError, ActorRegistryError, AdmittedAgentSession, AgentBlockStop, AgentWorkbench,
};

const MACHINE_WAIT: Duration = Duration::from_secs(30);

/// Trusted source environment supplied by actor deployment. The canonical
/// `ActorEffects` alias itself lives in the imported Haskell facade; Rust does
/// not reflect or authorize its row entries.
#[derive(Clone)]
pub struct ActorWorkbenchSource {
    preamble: Arc<str>,
    base_include: Arc<[PathBuf]>,
}

impl ActorWorkbenchSource {
    #[must_use]
    pub fn new(preamble: impl Into<Arc<str>>, base_include: Vec<PathBuf>) -> Self {
        let preamble = preamble.into();
        Self {
            preamble: insert_preamble_imports(&preamble, "Tidepool.Deliberation").into(),
            base_include: base_include.into(),
        }
    }
}

/// Shared-machine registry shape used by actors. String holes are only the
/// registry's checkout index; obligation-carrying `ResidentHole` values stay
/// inside each running segment.
pub type ActorMachineRegistry<H, O> = SessionRegistry<ResidentSession<H, O>, String>;

/// Shared checkout boundary for every actor machine entry path. Fenced
/// fragments and installed actor programs differ above this layer, but use
/// exactly the same admission and settlement mechanism.
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
    expected_type: String,
    type_modules: Arc<[String]>,
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
    Deliberate(crate::ResidentCompletion),
    Ready(ResidentActorReadiness),
}

/// One fully captured boundary reached by an installed actor program.
/// Variants own every linear runtime value needed to service that boundary;
/// downstream orchestration never re-decodes the suspended request.
pub(crate) enum ResidentActorBoundary {
    Completed,
    Deliberate(crate::ResidentCompletion),
    Start(crate::ResidentActorStart),
    Outbound(ResidentOutbound),
    Wait(ResidentWaitRequest),
    Poll(crate::wait::ResidentPollRequest),
    Receive(InstalledReceiver),
    McpAwait(crate::resident_mcp::ResidentMcpAwait),
    McpReply(crate::resident_mcp::ResidentMcpReply),
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
            Self::Deliberate(_) => "deliberate",
            Self::Start(_) => "startActor",
            Self::Outbound(ResidentOutbound::Call { .. }) => "call",
            Self::Outbound(ResidentOutbound::Cast { .. }) => "cast",
            Self::Wait(_) => "awaitExit",
            Self::Poll(_) => "pollExit",
            Self::Receive(_) => "receive",
            Self::McpAwait(_) => "actor MCP await",
            Self::McpReply(_) => "actor MCP reply",
        }
    }
}

#[derive(tidepool_bridge_derive::FromCore)]
#[allow(dead_code)]
enum CompleteReq {
    #[core(module = "Tidepool.Deliberation")]
    CompleteWith(i64, Value),
}

/// The one nominal roster for requests interpreted at actor execution
/// boundaries. Generated request enums own constructor recognition and field
/// shape; this sum owns orchestration routing.
enum ResidentRequest {
    Actor(crate::generated::actor::ActorReq),
    ActorKernel(crate::generated::actor_kernel::ActorKernelReq),
    ActorLocal(crate::generated::actor_local::ActorLocalReq),
    ActorMcp(crate::generated::actor_mcp::ActorMcpReq),
    Deliberate(crate::generated::deliberate::DeliberateReq),
    Complete(CompleteReq),
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
            Self::ActorKernel,
            crate::generated::actor_kernel::ActorKernelReq
        );
        try_member!(
            Self::ActorLocal,
            crate::generated::actor_local::ActorLocalReq
        );
        try_member!(Self::ActorMcp, crate::generated::actor_mcp::ActorMcpReq);
        try_member!(
            Self::Deliberate,
            crate::generated::deliberate::DeliberateReq
        );
        try_member!(Self::Complete, CompleteReq);

        Err(ResidentActorWorkbenchError::UnsupportedRequest {
            constructor: request_constructor(request, table),
        })
    }

    fn operation(&self) -> &'static str {
        match self {
            Self::Actor(crate::generated::actor::ActorReq::ActorStartWith(..)) => "startActor",
            Self::Actor(crate::generated::actor::ActorReq::ActorWaitWith(..)) => "awaitExit",
            Self::Actor(crate::generated::actor::ActorReq::ActorPollWith(..)) => "pollExit",
            Self::Actor(crate::generated::actor::ActorReq::ActorCallWith(..)) => "call",
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
            Self::ActorMcp(crate::generated::actor_mcp::ActorMcpReq::ActorMcpAwaitWith(..)) => {
                "actor MCP await"
            }
            Self::ActorMcp(crate::generated::actor_mcp::ActorMcpReq::ActorMcpReplyWith(..)) => {
                "actor MCP reply"
            }
            Self::Deliberate(crate::generated::deliberate::DeliberateReq::DeliberateWith(..)) => {
                "deliberate"
            }
            Self::Complete(CompleteReq::CompleteWith(..)) => "complete",
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
}

impl<H, O> ResidentActorWorkbench<H, O> {
    #[must_use]
    pub fn new(
        machines: Arc<ActorMachineRegistry<H, O>>,
        source: ActorWorkbenchSource,
        expected_type: impl Into<String>,
        type_modules: Vec<String>,
    ) -> Self {
        Self {
            access: ResidentMachineAccess::new(machines, source),
            expected_type: expected_type.into(),
            type_modules: type_modules.into(),
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum ResidentActorWorkbenchError {
    #[error(transparent)]
    Registry(#[from] ActorRegistryError),
    #[error(transparent)]
    CompileView(#[from] ActorCompileViewError),
    #[error("resident machine checkout failed: {0}")]
    Checkout(CheckoutError<String>),
    #[error("resident workbench compiler failed: {0}")]
    Compile(CompileError),
    #[error("resident workbench execution failed: {0}")]
    Resident(ResidentError),
    #[error("resident workbench task panicked or was cancelled: {0}")]
    Join(tokio::task::JoinError),
    #[error("typed completion suspended without a live payload")]
    MissingCompletionPayload,
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
    CompletionCapture(#[from] crate::CompletionCaptureError),
    #[error(transparent)]
    StartCapture(#[from] crate::ActorStartCaptureError),
    #[error(transparent)]
    WaitCapture(#[from] crate::ActorWaitError),
    #[error("invalid actor MCP declaration set: {0}")]
    McpDeclarations(serde_json::Error),
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
    /// Mount the authoritative input for the current completion under the
    /// one stable workbench name `goalInput`.
    ///
    /// GHC compiles the binding identity and thin interface, but its
    /// `undefined` expression is deliberately never run. The resident mount
    /// transfers the already-existing live value directly into that binding.
    pub async fn mount_goal_input(
        &self,
        admitted: &AdmittedAgentSession,
        input_type: impl Into<String>,
        input: RootCustody,
    ) -> Result<(), ResidentActorWorkbenchError> {
        let input_type = input_type.into();
        let type_modules = Arc::clone(&self.type_modules);
        self.access
            .with_machine(
                admitted.session_context(),
                move |session, context, source| {
                    session
                        .set_actor_execution(
                            context.run_context(),
                            context.effect_policy,
                            context.live_payload,
                        )
                        .map_err(ResidentActorWorkbenchError::Resident)?;
                    let block = ParsedBlock {
                        ordinal: 1,
                        total: 1,
                        source: format!("goalInput <- pure (undefined :: ({input_type}))"),
                    };
                    let compiled = match compile_block(
                        session,
                        context,
                        source,
                        "ActorEffects",
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
                },
            )
            .await
    }

    pub(crate) async fn resume_completion(
        &self,
        context: crate::ActorSessionContext,
        hole: ResidentHole,
        answer: RootCustody,
    ) -> Result<ResidentOutcome, ResidentActorWorkbenchError> {
        self.access
            .with_machine(context, move |session, context, _source| {
                session
                    .set_actor_execution(
                        context.run_context(),
                        context.effect_policy,
                        context.live_payload,
                    )
                    .map_err(ResidentActorWorkbenchError::Resident)?;
                session
                    .resume_handle(hole, answer)
                    .map_err(ResidentActorWorkbenchError::Resident)
            })
            .await
    }
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
            ResidentOutcome::Completed { .. } => return Ok(ResidentActorBoundary::Completed),
            ResidentOutcome::Suspended { hole, request, .. } => (hole, request),
        };

        self.access
            .with_machine(context, move |session, context, _| {
                session
                    .set_actor_execution(
                        context.run_context(),
                        context.effect_policy,
                        context.live_payload,
                    )
                    .map_err(ResidentActorWorkbenchError::Resident)?;
                let decoded = ResidentRequest::decode(&request, session.data_con_table())?;
                match decoded {
                    ResidentRequest::Actor(crate::generated::actor::ActorReq::ActorStartWith(
                        ..,
                    )) => {
                        let table = session.data_con_table().clone();
                        crate::ResidentActorStart::capture(
                            session,
                            hole,
                            &request,
                            &table,
                            context.placement.session,
                        )
                        .map(ResidentActorBoundary::Start)
                        .map_err(ResidentActorWorkbenchError::StartCapture)
                    }
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
                            crate::ActorWait::decode_target(&request, session.data_con_table())?;
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
                    ResidentRequest::ActorMcp(
                        crate::generated::actor_mcp::ActorMcpReq::ActorMcpAwaitWith(
                            declarations,
                            synopsis,
                        ),
                    ) => {
                        let declarations = tidepool_runtime::value_to_json(
                            &declarations,
                            session.data_con_table(),
                            0,
                        );
                        let declarations = serde_json::from_value(declarations)
                            .map_err(ResidentActorWorkbenchError::McpDeclarations)?;
                        Ok(ResidentActorBoundary::McpAwait(
                            crate::resident_mcp::ResidentMcpAwait {
                                continuation: hole,
                                declarations,
                                synopsis,
                            },
                        ))
                    }
                    ResidentRequest::ActorMcp(
                        crate::generated::actor_mcp::ActorMcpReq::ActorMcpReplyWith(result),
                    ) => Ok(ResidentActorBoundary::McpReply(
                        crate::resident_mcp::ResidentMcpReply {
                            continuation: hole,
                            result: tidepool_runtime::value_to_json(
                                &result,
                                session.data_con_table(),
                                0,
                            ),
                        },
                    )),
                    ResidentRequest::Deliberate(
                        crate::generated::deliberate::DeliberateReq::DeliberateWith(..),
                    ) => {
                        let table = session.data_con_table().clone();
                        crate::ResidentCompletion::capture(
                            session,
                            hole,
                            &request,
                            &table,
                            actor_realm,
                        )
                        .map(ResidentActorBoundary::Deliberate)
                        .map_err(ResidentActorWorkbenchError::CompletionCapture)
                    }
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
            .with_machine(context, move |session, context, _| {
                session
                    .set_actor_execution(
                        context.run_context(),
                        context.effect_policy,
                        context.live_payload,
                    )
                    .map_err(ResidentActorWorkbenchError::Resident)?;
                session
                    .run_rooted_entry("actor_program", entry, 0, realm, None)
                    .map_err(ResidentActorWorkbenchError::Resident)
            })
            .await
    }

    pub async fn capture_completion(
        &self,
        context: crate::ActorSessionContext,
        outcome: ResidentOutcome,
        actor_realm: RealmId,
    ) -> Result<crate::ResidentCompletion, ResidentActorWorkbenchError> {
        let boundary = self.capture_boundary(context, outcome, actor_realm).await?;
        match boundary {
            ResidentActorBoundary::Deliberate(completion) => Ok(completion),
            other => Err(unexpected_boundary("deliberate", &other)),
        }
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
                    ResidentRequest::Deliberate(
                        crate::generated::deliberate::DeliberateReq::DeliberateWith(..),
                    ) => {
                        let table = session.data_con_table().clone();
                        let completion = crate::ResidentCompletion::capture(
                            session,
                            hole,
                            &request,
                            &table,
                            actor_realm,
                        )?;
                        Ok(ResidentActorStartupStep::Deliberate(completion))
                    }
                    ResidentRequest::ActorKernel(
                        crate::generated::actor_kernel::ActorKernelReq::ActorReadyWith,
                    ) if session.parked_realm(&hole) == Some(actor_realm) => {
                        Ok(ResidentActorStartupStep::Ready(ResidentActorReadiness {
                            hole,
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
            .with_machine_wait(context, admission_timeout, move |session, context, _| {
                session
                    .set_actor_execution(
                        context.run_context(),
                        context.effect_policy,
                        context.live_payload,
                    )
                    .map_err(ResidentActorWorkbenchError::Resident)?;
                match session
                    .run_rooted_entry("actor_shutdown", hook, argument, realm, None)
                    .map_err(ResidentActorWorkbenchError::Resident)?
                {
                    ResidentOutcome::Completed { .. } => Ok(()),
                    ResidentOutcome::Suspended { request, .. } => {
                        let request = ResidentRequest::decode(&request, session.data_con_table())?;
                        Err(ResidentActorWorkbenchError::ActorProtocol(format!(
                            "shutdown suspended on disallowed `{}`",
                            request.operation()
                        )))
                    }
                }
            })
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

    pub(crate) async fn capture_receiver(
        &self,
        context: crate::ActorSessionContext,
        outcome: ResidentOutcome,
        actor_realm: RealmId,
    ) -> Result<InstalledReceiver, ResidentActorWorkbenchError> {
        let boundary = self.capture_boundary(context, outcome, actor_realm).await?;
        match boundary {
            ResidentActorBoundary::Receive(receiver) => Ok(receiver),
            other => Err(unexpected_boundary("receive", &other)),
        }
    }

    pub(crate) async fn run_mailbox_handler(
        &self,
        context: crate::ActorSessionContext,
        handler: RootCustody,
        request: RootCustody,
        handler_realm: RealmId,
    ) -> Result<ResidentOutcome, ResidentActorWorkbenchError> {
        self.access
            .with_machine(context, move |session, context, _| {
                session
                    .set_actor_execution(
                        context.run_context(),
                        context.effect_policy,
                        context.live_payload,
                    )
                    .map_err(ResidentActorWorkbenchError::Resident)?;
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

    pub(crate) async fn capture_outbound(
        &self,
        context: crate::ActorSessionContext,
        outcome: ResidentOutcome,
    ) -> Result<ResidentOutbound, ResidentActorWorkbenchError> {
        let actor_realm = context.placement.resource_scope;
        let boundary = self.capture_boundary(context, outcome, actor_realm).await?;
        match boundary {
            ResidentActorBoundary::Outbound(outbound) => Ok(outbound),
            other => Err(unexpected_boundary("call or cast", &other)),
        }
    }

    pub(crate) async fn capture_wait(
        &self,
        context: crate::ActorSessionContext,
        outcome: ResidentOutcome,
    ) -> Result<ResidentWaitRequest, ResidentActorWorkbenchError> {
        let actor_realm = context.placement.resource_scope;
        let boundary = self.capture_boundary(context, outcome, actor_realm).await?;
        match boundary {
            ResidentActorBoundary::Wait(wait) => Ok(wait),
            other => Err(unexpected_boundary("awaitExit", &other)),
        }
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

    pub(crate) async fn resume_mcp_invocation(
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
    ) -> Result<ResidentOutcome, ResidentActorWorkbenchError> {
        self.access
            .with_machine(context, move |session, _, _| {
                let answer = (actor.id.0 as i64, actor.incarnation.0 as i64)
                    .to_value(session.data_con_table())?;
                session
                    .resume(hole, answer)
                    .map_err(ResidentActorWorkbenchError::Resident)
            })
            .await
    }
}

#[derive(Clone, Copy)]
enum OutboundKind {
    Call,
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

impl<H, O> AgentWorkbench for ResidentActorWorkbench<H, O>
where
    H: DispatchEffect<O> + Send + 'static,
    O: OutputSink + Sync + 'static,
{
    type Completion = RootCustody;
    type Error = ResidentActorWorkbenchError;

    async fn execute(
        &mut self,
        admitted: &AdmittedAgentSession,
        block: ParsedBlock,
    ) -> Result<BlockExecution<String, AgentBlockStop<RootCustody>>, Self::Error> {
        let expected_type = self.expected_type.clone();
        let type_modules = Arc::clone(&self.type_modules);
        self.access
            .with_machine(
                admitted.session_context(),
                move |session, context, source| {
                    execute_checked_out(
                        session,
                        context,
                        source,
                        &expected_type,
                        &type_modules,
                        block,
                    )
                },
            )
            .await
    }
}

struct ReadyBlock {
    result: TurnResult,
    generation: tidepool_repr::Generation,
    declaration_source: String,
}

enum CompiledBlock {
    Ready(Box<ReadyBlock>),
    Rejected(String),
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
    let session_view = session
        .compile_view_in(context.placement.lexical_scope)
        .ok_or_else(|| {
            ResidentActorWorkbenchError::Resident(ResidentError::Session(
                tidepool_runtime::session::SessionError::DeadScope(context.placement.lexical_scope),
            ))
        })?;
    let compile_view = context
        .compile_view(session_view)?
        .with_type_modules(type_modules);
    let templates =
        resident_workbench_templates(&source.preamble, effect_stack, &compile_view.turn_imports());
    let include = compile_view.include_paths(&source.base_include);
    let include_refs: Vec<_> = include.iter().map(PathBuf::as_path).collect();
    let injected = compile_view.injected_module_names();
    let request = TurnRequest {
        turn_text: &block.source,
        templates: &templates,
        include: &include_refs,
        session_root: compile_view.session_root(),
        inject_modules: &injected,
        gen: compile_view.next_value_generation().0,
        verdict: None,
        target: None,
    };
    match run_turn(request) {
        Ok(result) => Ok(CompiledBlock::Ready(Box::new(ReadyBlock {
            result,
            generation: compile_view.next_value_generation(),
            declaration_source: compile_view.declaration_source(&block.source),
        }))),
        Err(failure) if classify_compile(&failure.error).class == FailureClass::UserHaskell => Ok(
            CompiledBlock::Rejected(classify_compile(&failure.error).message),
        ),
        Err(failure) => Err(ResidentActorWorkbenchError::Compile(failure.error)),
    }
}

fn execute_checked_out<H, O>(
    session: &mut ResidentSession<H, O>,
    context: &crate::ActorSessionContext,
    source: &ActorWorkbenchSource,
    expected_type: &str,
    type_modules: &[String],
    block: ParsedBlock,
) -> Result<BlockExecution<String, AgentBlockStop<RootCustody>>, ResidentActorWorkbenchError>
where
    H: DispatchEffect<O> + Send,
    O: OutputSink + Sync,
{
    let fragment_realm = RealmId::fresh();
    let actor_context = context.run_context();
    session
        .set_actor_execution(
            SessionRunContext::new(
                fragment_realm,
                actor_context.lexical_scope,
                actor_context.principal,
            ),
            context.effect_policy,
            context.live_payload,
        )
        .map_err(ResidentActorWorkbenchError::Resident)?;
    let outcome = execute_fragment(session, context, source, expected_type, type_modules, block);
    session.close_realm(fragment_realm);
    session
        .set_actor_execution(actor_context, context.effect_policy, context.live_payload)
        .map_err(ResidentActorWorkbenchError::Resident)?;
    outcome
}

fn execute_fragment<H, O>(
    session: &mut ResidentSession<H, O>,
    context: &crate::ActorSessionContext,
    source: &ActorWorkbenchSource,
    expected_type: &str,
    type_modules: &[String],
    block: ParsedBlock,
) -> Result<BlockExecution<String, AgentBlockStop<RootCustody>>, ResidentActorWorkbenchError>
where
    H: DispatchEffect<O> + Send,
    O: OutputSink + Sync,
{
    let effect_stack = format!("(Complete ({expected_type}) ': ActorEffects)");
    let compiled = match compile_block(
        session,
        context,
        source,
        &effect_stack,
        type_modules,
        &block,
    )? {
        CompiledBlock::Ready(compiled) => compiled,
        CompiledBlock::Rejected(diagnostic) => {
            return Ok(BlockExecution::Stopped(AgentBlockStop::Rejected(
                diagnostic,
            )));
        }
    };
    let ReadyBlock {
        result,
        generation,
        declaration_source,
    } = *compiled;
    match result {
        TurnResult::Decl(receipt) => {
            match session.define_scoped_in(context.placement.lexical_scope, &[&declaration_source])
            {
                Ok(generation) => Ok(BlockExecution::Committed(format!(
                    "defined {} at generation {}",
                    if receipt.binders.is_empty() {
                        "declaration".to_string()
                    } else {
                        receipt.binders.join(", ")
                    },
                    generation.0
                ))),
                Err(error) if classify_session(&error).class == FailureClass::UserHaskell => {
                    Ok(BlockExecution::Stopped(AgentBlockStop::Rejected(
                        classify_session(&error).message,
                    )))
                }
                Err(error) => Err(ResidentActorWorkbenchError::Resident(
                    ResidentError::Session(error),
                )),
            }
        }
        TurnResult::Bind {
            bound, compiled, ..
        } => {
            if bound.len() != 1 {
                return Ok(BlockExecution::Stopped(AgentBlockStop::Rejected(
                    "the resident actor workbench currently materializes one binding per statement; bind the tuple to one name or split this into separate blocks"
                        .into(),
                )));
            }
            let outcome = session.run_bind_with_sites(
                "actor_workbench_bind",
                &compiled.expr,
                &compiled.table,
                &bound[0],
                generation,
                &compiled.asks,
            );
            settle_run(
                session,
                context,
                &compiled.table,
                outcome,
                Some(&bound[0].name),
            )
        }
        TurnResult::Expr { compiled, .. } => {
            let outcome = session.run_with_sites(
                "actor_workbench_expr",
                &compiled.expr,
                &compiled.table,
                &compiled.asks,
            );
            settle_run(session, context, &compiled.table, outcome, None)
        }
    }
}

fn settle_run<H, O>(
    session: &mut ResidentSession<H, O>,
    context: &crate::ActorSessionContext,
    table: &DataConTable,
    outcome: Result<ResidentOutcome, ResidentError>,
    bound_name: Option<&str>,
) -> Result<BlockExecution<String, AgentBlockStop<RootCustody>>, ResidentActorWorkbenchError>
where
    H: DispatchEffect<O> + Send,
    O: OutputSink + Sync,
{
    let outcome = match outcome {
        Ok(outcome) => outcome,
        Err(ResidentError::Run(error)) => {
            return Ok(BlockExecution::Stopped(AgentBlockStop::Rejected(
                error.to_string(),
            )));
        }
        Err(error) => {
            return Err(ResidentActorWorkbenchError::Resident(error));
        }
    };
    match outcome {
        ResidentOutcome::Completed { output, result } => {
            let mut receipt = match bound_name {
                Some(name) => format!("bound `{name}`"),
                None => result.to_string_pretty(),
            };
            if !output.is_empty() {
                receipt.push_str("\n\nOutput:\n");
                receipt.push_str(&output.join("\n"));
            }
            Ok(BlockExecution::Committed(receipt))
        }
        ResidentOutcome::Suspended {
            output,
            hole,
            request,
        } => {
            let request_kind = ResidentRequest::decode(&request, table);
            if matches!(&request_kind, Ok(ResidentRequest::Complete(_))) {
                let completion = session
                    .live_payload_handle_owned_by(hole.cont_id(), context.placement.resource_scope)
                    .ok_or(ResidentActorWorkbenchError::MissingCompletionPayload)?;
                return Ok(BlockExecution::Stopped(AgentBlockStop::Completed(
                    completion,
                )));
            }
            let output = if output.is_empty() {
                String::new()
            } else {
                format!("\n\nOutput before suspension:\n{}", output.join("\n"))
            };
            let operation = match request_kind {
                Ok(request) => request.operation().to_string(),
                Err(error) => error.to_string(),
            };
            Ok(BlockExecution::Stopped(AgentBlockStop::Rejected(format!(
                "fragment suspended on `{}`, which the current actor interpreter could not settle{output}",
                operation
            ))))
        }
    }
}

#[cfg(test)]
mod request_tests {
    use super::*;
    use tidepool_repr::{DataCon, DataConId, Literal};

    #[test]
    fn resident_roster_decodes_complete_nominally() {
        let mut table = DataConTable::new();
        table.insert(data_con(
            1,
            "CompleteWith",
            "Tidepool.Deliberation.CompleteWith",
            2,
        ));
        let request = Value::Con(
            DataConId(1),
            vec![
                Value::Lit(Literal::LitInt(0)),
                Value::Lit(Literal::LitInt(42)),
            ],
        );

        let decoded = ResidentRequest::decode(&request, &table).expect("decode CompleteWith");
        assert_eq!(decoded.operation(), "complete");
    }

    #[test]
    fn same_spelled_complete_from_another_module_is_not_completion() {
        let mut table = DataConTable::new();
        table.insert(data_con(1, "CompleteWith", "User.CompleteWith", 2));
        let request = Value::Con(
            DataConId(1),
            vec![
                Value::Lit(Literal::LitInt(0)),
                Value::Lit(Literal::LitInt(42)),
            ],
        );

        assert!(matches!(
            ResidentRequest::decode(&request, &table),
            Err(ResidentActorWorkbenchError::UnsupportedRequest { constructor })
                if constructor == "User.CompleteWith"
        ));
    }

    #[test]
    fn malformed_known_request_is_a_decode_error_not_an_unknown_operation() {
        let mut table = DataConTable::new();
        table.insert(data_con(
            1,
            "CompleteWith",
            "Tidepool.Deliberation.CompleteWith",
            2,
        ));
        let request = Value::Con(
            DataConId(1),
            vec![
                Value::Lit(Literal::LitString(b"not an Int".to_vec())),
                Value::Lit(Literal::LitInt(42)),
            ],
        );

        assert!(matches!(
            ResidentRequest::decode(&request, &table),
            Err(ResidentActorWorkbenchError::RequestDecode { constructor, .. })
                if constructor == "Tidepool.Deliberation.CompleteWith"
        ));
    }

    fn data_con(id: u64, name: &str, qualified_name: &str, rep_arity: u32) -> DataCon {
        DataCon {
            id: DataConId(id),
            name: name.to_string(),
            tag: 1,
            rep_arity,
            field_bangs: Vec::new(),
            qualified_name: Some(qualified_name.to_string()),
            type_name: "Request".to_string(),
        }
    }
}
