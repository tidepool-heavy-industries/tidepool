//! Capture of one public Haskell `startActor` suspension.
//!
//! The child entry is an existentially row-typed Haskell closure. Rust never
//! decodes that row: it claims the request's field-1 live payload and later
//! starts it through `ResidentSession::run_rooted_entry`, the same entry
//! primitive used by green threads.

use std::collections::BTreeSet;

use tidepool_bridge::{BridgeError, FromCore};
use tidepool_codegen::suspension::RealmId;
use tidepool_effect::dispatch::DispatchEffect;
use tidepool_eval::Value;
use tidepool_model::{DynModelProvider, StreamSink};
use tidepool_repr::DataConTable;
use tidepool_runtime::session::{
    MaterializedFacade, OutputSink, ResidentHole, ResidentOutcome, ResidentSession, RootCustody,
};

use crate::generated::actor::ActorReq;
use crate::resident_workbench::ResidentActorStartupStep;
use crate::{
    ActorDescriptor, ActorExitKind, ActorRegistry, ActorRegistryError, ActorTerminal,
    ResidentActorRunner, ResidentActorWorkbenchError, ResidentCompletionError,
    ResidentCompletionExecutor, ResidentLifecycleError, StartInitiator, TurnLease,
};

/// One parked parent continuation paired with exclusive custody of its child
/// entry. Compiler provenance travels with the rooted entry itself.
pub struct ResidentActorStart {
    descriptor: ActorDescriptor,
    parent_hole: ResidentHole,
    entry: RootCustody,
}

#[derive(Debug, thiserror::Error)]
pub enum ActorStartCaptureError {
    #[error(transparent)]
    Decode(#[from] BridgeError),
    #[error("actor start decoder received a non-start request")]
    UnexpectedRequest,
    #[error("actor start suspended without its child entry live payload")]
    MissingEntry,
    #[error(transparent)]
    ExactExports(#[from] tidepool_runtime::session::ExactExportError),
    #[error(transparent)]
    Facade(#[from] tidepool_runtime::session::ExactFacadeError),
    #[error("actor start has no live declaration plane")]
    NoCompileView,
}

impl ResidentActorStart {
    /// Decode and claim a newly suspended start request while the resident
    /// machine is checked out. The entry root is born in the unpublished
    /// child's realm so parent cleanup cannot revoke a successfully accepted
    /// child computation.
    pub fn capture<H, O>(
        session: &mut ResidentSession<H, O>,
        parent_hole: ResidentHole,
        request: &Value,
        table: &DataConTable,
        session_id: tidepool_repr::SessionId,
    ) -> Result<Self, ActorStartCaptureError>
    where
        H: DispatchEffect<O> + Send,
        O: OutputSink + Sync,
    {
        let ActorReq::ActorStartWith(label, _entry_projection) =
            ActorReq::from_value(request, table)?
        else {
            return Err(ActorStartCaptureError::UnexpectedRequest);
        };
        let child_realm = RealmId::fresh();
        let entry = session
            .live_payload_handle_owned_by(parent_hole.cont_id(), child_realm)
            .ok_or(ActorStartCaptureError::MissingEntry)?;
        let facade = materialize_entry_facade(session, &entry)?;
        let lexical_scope = session.mint_isolated_scope();
        let descriptor = ActorDescriptor::new(
            label.clone(),
            ["ActorKernel", "Actor", "ActorLocal", "Deliberate"],
            crate::ActorPlacement {
                session: session_id,
                resource_scope: child_realm,
                lexical_scope,
            },
        )
        .with_source_imports(crate::ActorSourceImports::from_exact_facades([&facade]));
        Ok(Self {
            descriptor,
            parent_hole,
            entry,
        })
    }

    /// Consume the capture into the exact parent obligation and child entry.
    pub fn into_parts(self) -> (ActorDescriptor, ResidentHole, RootCustody) {
        (self.descriptor, self.parent_hole, self.entry)
    }
}

fn materialize_entry_facade<H, O>(
    session: &ResidentSession<H, O>,
    entry: &RootCustody,
) -> Result<MaterializedFacade, ActorStartCaptureError>
where
    H: DispatchEffect<O> + Send,
    O: OutputSink + Sync,
{
    let mut heads = BTreeSet::new();
    for site in entry.provenance().sites() {
        for head in site
            .heads
            .iter()
            .chain(site.inputs.iter().flat_map(|input| input.heads.iter()))
        {
            if head.module.starts_with("Tidepool.Session.Lib.G") {
                heads.insert(head.name.clone());
            }
        }
    }
    let scope = session.run_context().lexical_scope;
    let names: Vec<_> = heads.iter().map(String::as_str).collect();
    let surface = session.exact_exports_in(scope, &names)?;
    let view = session
        .compile_view_in(scope)
        .ok_or(ActorStartCaptureError::NoCompileView)?;
    Ok(surface.materialize(&view)?)
}

#[derive(Debug, thiserror::Error)]
pub enum ResidentActorStartError {
    #[error(transparent)]
    Registry(#[from] ActorRegistryError),
    #[error(transparent)]
    Workbench(#[from] ResidentActorWorkbenchError),
    #[error(transparent)]
    Completion(#[from] ResidentCompletionError),
    #[error(transparent)]
    Lifecycle(#[from] ResidentLifecycleError),
}

/// Runtime-owned prompted startup. This is orchestration over the permanent
/// registry, resident runner, and result-session executor—not another
/// actor execution mechanism.
pub struct ResidentActorStarter<H, O> {
    registry: ActorRegistry,
    runner: ResidentActorRunner<H, O>,
    completions: ResidentCompletionExecutor<H, O>,
    lifecycle: crate::ResidentActorLifecycle<H, O>,
}

impl<H, O> ResidentActorStarter<H, O> {
    #[must_use]
    pub fn new(
        registry: ActorRegistry,
        runner: ResidentActorRunner<H, O>,
        completions: ResidentCompletionExecutor<H, O>,
    ) -> Self {
        let lifecycle = crate::ResidentActorLifecycle::new(registry.clone(), runner.clone());
        Self {
            registry,
            runner,
            completions,
            lifecycle,
        }
    }
}

impl<H, O> ResidentActorStarter<H, O>
where
    H: DispatchEffect<O> + Send + 'static,
    O: OutputSink + Sync + 'static,
{
    async fn abort_starting(
        &self,
        starting: crate::StartingActor,
        context: crate::ActorSessionContext,
        terminal: ActorTerminal,
    ) -> Result<(), ResidentActorStartError> {
        let realm = context.placement.resource_scope;
        let abort = self.registry.abort_start(starting, terminal);
        let cleanup = self.runner.close_realm(context, realm).await;
        abort?;
        cleanup?;
        Ok(())
    }

    pub async fn start(
        &self,
        parent_turn: TurnLease,
        provider: &dyn DynModelProvider,
        start: ResidentActorStart,
        sink: Option<StreamSink>,
    ) -> Result<(TurnLease, crate::ActorRef, ResidentOutcome), ResidentActorStartError> {
        let owner = parent_turn.actor();
        let parent_context = parent_turn.session_context();
        let (descriptor, parent_hole, entry) = start.into_parts();
        let child_realm = descriptor.placement().resource_scope;
        let starting =
            self.registry
                .begin_start(Some(owner), descriptor, StartInitiator::Policy)?;
        let child_session = self.registry.startup_agent_session(&starting)?;
        let child_context = self.registry.session_context(starting.actor())?;

        let startup_result = async {
            let mut admitted = None;
            let mut outcome = self
                .runner
                .run_rooted_entry(child_context.clone(), entry, child_realm)
                .await?;
            let readiness = loop {
                match self
                    .runner
                    .capture_startup_step(child_context.clone(), outcome, child_realm)
                    .await?
                {
                    ResidentActorStartupStep::Deliberate(completion) => {
                        let admitted = match admitted.as_mut() {
                            Some(admitted) => admitted,
                            None => admitted
                                .insert(child_session.begin_startup_agent_session(&starting)?),
                        };
                        outcome = self
                            .completions
                            .resolve_admitted(admitted, provider, completion, sink.clone())
                            .await?;
                    }
                    ResidentActorStartupStep::Ready(readiness) => break readiness,
                }
            };
            drop(admitted);
            Ok::<_, ResidentActorStartError>(readiness)
        }
        .await;

        let readiness = match startup_result {
            Ok(readiness) => readiness,
            Err(error) => {
                self.abort_starting(
                    starting,
                    child_context,
                    ActorTerminal {
                        kind: ActorExitKind::Failed,
                        summary: error.to_string(),
                    },
                )
                .await?;
                return Err(error);
            }
        };

        let child = match self
            .runner
            .resume_readiness(child_context.clone(), readiness)
            .await
        {
            Ok(child) => child,
            Err(error) => {
                self.abort_starting(
                    starting,
                    child_context,
                    ActorTerminal {
                        kind: ActorExitKind::Failed,
                        summary: error.to_string(),
                    },
                )
                .await?;
                return Err(error.into());
            }
        };
        let completed = matches!(child, ResidentOutcome::Completed { .. });
        if !completed {
            let receiver = match self
                .runner
                .capture_receiver(child_context.clone(), child, child_realm)
                .await
            {
                Ok(receiver) => receiver,
                Err(error) => {
                    self.abort_starting(
                        starting,
                        child_context,
                        ActorTerminal {
                            kind: ActorExitKind::Failed,
                            summary: error.to_string(),
                        },
                    )
                    .await?;
                    return Err(error.into());
                }
            };
            if let Err(error) = self.registry.install_starting_receiver(&starting, receiver) {
                self.abort_starting(
                    starting,
                    child_context,
                    ActorTerminal {
                        kind: ActorExitKind::Failed,
                        summary: error.to_string(),
                    },
                )
                .await?;
                return Err(error.into());
            }
        }
        let actor = match self.registry.publish_ready(starting) {
            Ok(actor) => actor,
            Err(error) => {
                let _ = self.runner.close_realm(child_context, child_realm).await;
                return Err(error.into());
            }
        };
        if completed {
            self.lifecycle
                .force_terminate(
                    actor,
                    ActorTerminal {
                        kind: ActorExitKind::Completed,
                        summary: "completed".into(),
                    },
                )
                .await?;
        }
        let parent = match self
            .runner
            .resume_starting_parent(parent_context, parent_hole, actor)
            .await
        {
            Ok(parent) => parent,
            Err(error) => {
                let _ = self
                    .lifecycle
                    .force_terminate(
                        actor,
                        ActorTerminal {
                            kind: ActorExitKind::Failed,
                            summary: format!(
                                "starter could not publish the child reference: {error}"
                            ),
                        },
                    )
                    .await;
                return Err(error.into());
            }
        };
        Ok((parent_turn, actor, parent))
    }
}
