//! Actor binding for the shared resident Haskell workbench.
//!
//! This adapter owns no machine and holds no checkout between calls. Each
//! fenced block checks out the actor's registered resident session, installs
//! the exact actor context, compiles and runs one segment on the blocking
//! pool, then restores the machine before the provider loop continues.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use tidepool_codegen::suspension::RealmId;
use tidepool_effect::dispatch::{request_constructor, DispatchEffect};
use tidepool_repr::DataConTable;
use tidepool_runtime::session::registry::{CheckoutError, SessionRegistry};
use tidepool_runtime::session::{
    insert_preamble_imports, resident_workbench_templates, run_turn, BlockExecution, OutputSink,
    ParsedBlock, ResidentError, ResidentHole, ResidentOutcome, ResidentSession, RootCustody,
    SessionRunContext, TurnRequest, TurnResult,
};
use tidepool_runtime::{classify_compile, classify_session, CompileError, FailureClass};

use crate::{
    ActorCompileViewError, ActorRegistryError, AdmittedAgentSession, AgentBlockStop, AgentWorkbench,
};

const MACHINE_WAIT: Duration = Duration::from_secs(30);

/// Trusted source environment supplied by an actor runtime profile. The
/// canonical `AgentEffects` alias itself lives in the imported Haskell facade;
/// Rust does not reflect or authorize its row entries.
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

/// Concrete resident workbench for one typed agent-session obligation.
pub struct ResidentActorWorkbench<H, O> {
    machines: Arc<ActorMachineRegistry<H, O>>,
    source: ActorWorkbenchSource,
    expected_type: String,
    type_modules: Arc<[String]>,
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
            machines,
            source,
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
    #[error("could not mount the typed deliberation input: {0}")]
    InputMount(String),
}

impl<H, O> ResidentActorWorkbench<H, O>
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
        let checkout = self
            .machines
            .checkout_wait(
                context.placement.session,
                tidepool_runtime::session::registry::CheckoutRequest::Run,
                MACHINE_WAIT,
            )
            .await
            .map_err(ResidentActorWorkbenchError::Checkout)?;
        let (mut session, receipt) = checkout.into_parts();
        let source = self.source.clone();

        let task = tokio::task::spawn_blocking(move || {
            let outcome = operation(&mut session, &context, &source);
            let holes = session
                .parked_holes()
                .into_iter()
                .map(str::to_string)
                .collect();
            (session, holes, outcome)
        })
        .await;

        match task {
            Ok((session, holes, outcome)) => {
                self.machines.settle_suspended(receipt, session, holes);
                outcome
            }
            Err(error) => {
                self.machines.settle_retire(receipt);
                Err(ResidentActorWorkbenchError::Join(error))
            }
        }
    }

    /// Mount the authoritative input for the current deliberation under the
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
        self.with_machine(
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
                    "AgentEffects",
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

    pub(crate) async fn resume_deliberation(
        &self,
        context: crate::ActorSessionContext,
        hole: ResidentHole,
        answer: RootCustody,
    ) -> Result<ResidentOutcome, ResidentActorWorkbenchError> {
        self.with_machine(context, move |session, context, _source| {
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
        self.with_machine(
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
        Err(error) if classify_compile(&error).class == FailureClass::UserHaskell => {
            Ok(CompiledBlock::Rejected(classify_compile(&error).message))
        }
        Err(error) => Err(ResidentActorWorkbenchError::Compile(error)),
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
    let effect_stack = format!("(Complete ({expected_type}) ': AgentEffects)");
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
        TurnResult::Decl { binders, .. } => {
            match session.define_scoped_in(context.placement.lexical_scope, &[&declaration_source])
            {
                Ok(generation) => Ok(BlockExecution::Committed(format!(
                    "defined {} at generation {}",
                    if binders.is_empty() {
                        "declaration".to_string()
                    } else {
                        binders.join(", ")
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
            let outcome = session.run_bind(
                "actor_workbench_bind",
                &compiled.expr,
                &compiled.table,
                &bound[0],
                generation,
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
            let outcome = session.run("actor_workbench_expr", &compiled.expr, &compiled.table);
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
            let constructor = request_constructor(&request, table);
            if constructor.rsplit('.').next() == Some("CompleteWith") {
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
            Ok(BlockExecution::Stopped(AgentBlockStop::Rejected(format!(
                "fragment suspended on `{constructor}`, which the current actor interpreter could not settle{output}"
            ))))
        }
    }
}
