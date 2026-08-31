//! Capture of one public Haskell `startActor` suspension.
//!
//! The child entry is an existentially row-typed Haskell closure. Rust never
//! decodes that row: it claims the request's field-1 live payload and later
//! starts it through `ResidentSession::run_rooted_entry`, the same entry
//! primitive used by green threads.

use tidepool_bridge::{BridgeError, FromCore};
use tidepool_codegen::suspension::RealmId;
use tidepool_effect::dispatch::DispatchEffect;
use tidepool_eval::Value;
use tidepool_model::{DynModelProvider, StreamSink};
use tidepool_repr::DataConTable;
use tidepool_runtime::session::{
    OutputSink, ResidentHole, ResidentOutcome, ResidentSession, RootCustody,
};

use crate::generated::actor::ActorReq;
use crate::{
    ActorDescriptor, ActorExitKind, ActorRegistry, ActorRegistryError, ActorTerminal,
    ResidentActorRunner, ResidentActorWorkbenchError, ResidentDeliberationError,
    ResidentDeliberationExecutor, StartInitiator, TurnLease,
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ActorStartRequest {
    pub label: String,
    pub promotion: String,
}

/// One parked parent continuation paired with exclusive custody of its child
/// entry. Compiler provenance travels with the rooted entry itself.
pub struct ResidentActorStart {
    request: ActorStartRequest,
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
        child_realm: RealmId,
    ) -> Result<Self, ActorStartCaptureError>
    where
        H: DispatchEffect<O> + Send,
        O: OutputSink + Sync,
    {
        let ActorReq::ActorStartWith(label, _entry_projection, promotion) =
            ActorReq::from_value(request, table)?
        else {
            return Err(ActorStartCaptureError::UnexpectedRequest);
        };
        let entry = session
            .live_payload_handle_owned_by(parent_hole.cont_id(), child_realm)
            .ok_or(ActorStartCaptureError::MissingEntry)?;
        Ok(Self {
            request: ActorStartRequest { label, promotion },
            parent_hole,
            entry,
        })
    }

    #[must_use]
    pub fn request(&self) -> &ActorStartRequest {
        &self.request
    }

    /// Consume the capture into the exact parent obligation and child entry.
    pub fn into_parts(self) -> (ResidentHole, RootCustody) {
        (self.parent_hole, self.entry)
    }
}

#[derive(Debug, thiserror::Error)]
pub enum ResidentActorStartError {
    #[error(transparent)]
    Registry(#[from] ActorRegistryError),
    #[error(transparent)]
    Workbench(#[from] ResidentActorWorkbenchError),
    #[error(transparent)]
    Deliberation(#[from] ResidentDeliberationError),
    #[error("actor start carried an unknown or stale promotion receipt")]
    InvalidPromotion,
    #[error("actor start label {request:?} does not match descriptor label {descriptor:?}")]
    DescriptorLabel { request: String, descriptor: String },
}

/// Runtime-owned prompted startup. This is orchestration over the permanent
/// registry, resident runner, and typed-deliberation executor—not another
/// actor execution mechanism.
pub struct ResidentActorStarter<H, O> {
    registry: ActorRegistry,
    runner: ResidentActorRunner<H, O>,
    deliberations: ResidentDeliberationExecutor<H, O>,
}

impl<H, O> ResidentActorStarter<H, O> {
    #[must_use]
    pub fn new(
        registry: ActorRegistry,
        runner: ResidentActorRunner<H, O>,
        deliberations: ResidentDeliberationExecutor<H, O>,
    ) -> Self {
        Self {
            registry,
            runner,
            deliberations,
        }
    }
}

impl<H, O> ResidentActorStarter<H, O>
where
    H: DispatchEffect<O> + Send + 'static,
    O: OutputSink + Sync + 'static,
{
    pub async fn start(
        &self,
        parent_turn: TurnLease,
        descriptor: ActorDescriptor,
        provider: &dyn DynModelProvider,
        start: ResidentActorStart,
        sink: Option<StreamSink>,
    ) -> Result<(TurnLease, crate::ActorRef, ResidentOutcome), ResidentActorStartError> {
        let owner = parent_turn.actor();
        let parent_context = parent_turn.session_context();
        let child_realm = descriptor.placement().resource_scope;
        if descriptor.label() != start.request().label {
            return Err(ResidentActorStartError::DescriptorLabel {
                request: start.request().label.clone(),
                descriptor: descriptor.label().to_owned(),
            });
        }
        if !descriptor
            .source_imports()
            .contains_receipt(&start.request().promotion)
        {
            return Err(ResidentActorStartError::InvalidPromotion);
        }
        let starting =
            self.registry
                .begin_start(Some(owner), descriptor, StartInitiator::Policy)?;
        let child_session = self.registry.startup_agent_session(&starting)?;
        let child_context = self.registry.session_context(starting.actor())?;
        let (parent_hole, entry) = start.into_parts();

        let startup_result = async {
            let mut admitted = child_session.begin_startup_agent_session(&starting)?;
            let first = self
                .runner
                .run_rooted_entry(child_context.clone(), entry, child_realm)
                .await?;
            let deliberation = self
                .runner
                .capture_deliberation(child_context.clone(), first, child_realm)
                .await?;
            let ready = self
                .deliberations
                .resolve_admitted(&mut admitted, provider, deliberation, sink)
                .await?;
            drop(admitted);
            Ok::<_, ResidentActorStartError>(ready)
        }
        .await;

        let readiness = match startup_result {
            Ok(ready) => match self
                .runner
                .capture_readiness(child_context.clone(), ready, child_realm)
                .await
            {
                Ok(readiness) => readiness,
                Err(error) => {
                    self.registry.abort_start(
                        starting,
                        ActorTerminal {
                            kind: ActorExitKind::Failed,
                            summary: error.to_string(),
                        },
                    )?;
                    return Err(error.into());
                }
            },
            Err(error) => {
                self.registry.abort_start(
                    starting,
                    ActorTerminal {
                        kind: ActorExitKind::Failed,
                        summary: error.to_string(),
                    },
                )?;
                return Err(error);
            }
        };

        let actor = self.registry.publish_ready(starting)?;
        let child = match self.runner.resume_readiness(child_context, readiness).await {
            Ok(child) => child,
            Err(error) => {
                self.registry.finish(
                    actor,
                    ActorTerminal {
                        kind: ActorExitKind::Failed,
                        summary: error.to_string(),
                    },
                )?;
                return Err(error.into());
            }
        };
        if matches!(child, ResidentOutcome::Completed { .. }) {
            self.registry.finish(
                actor,
                ActorTerminal {
                    kind: ActorExitKind::Completed,
                    summary: "completed".into(),
                },
            )?;
        }
        let parent = self
            .runner
            .resume_starting_parent(parent_context, parent_hole, actor)
            .await?;
        Ok((parent_turn, actor, parent))
    }
}
