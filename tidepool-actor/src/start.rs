//! Capture of one public Haskell `startActor` suspension.
//!
//! The child entry is an existentially row-typed Haskell closure. Rust never
//! decodes that row: it claims the request's field-1 live payload and later
//! starts it through `ResidentSession::run_rooted_entry`, the same entry
//! primitive used by green threads.

use std::collections::BTreeSet;
use std::future::Future;
use std::panic::AssertUnwindSafe;
use std::sync::Arc;

use futures_util::FutureExt;
use tidepool_bridge::{BridgeError, FromCore};
use tidepool_codegen::suspension::RealmId;
use tidepool_effect::dispatch::DispatchEffect;
use tidepool_eval::Value;
use tidepool_model::{DynModelProvider, StreamSink};
use tidepool_repr::{DataConTable, Generation, SessionModule};
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
    #[error(
        "actor export `{head}` drifted from rooted definition module `{expected}` to `{actual}`"
    )]
    ShadowDrift {
        head: String,
        expected: String,
        actual: String,
    },
    #[error("actor export `{head}` has more than one rooted nominal incarnation: {modules:?}")]
    AmbiguousIncarnation { head: String, modules: Vec<String> },
    #[error("actor start has no live declaration plane")]
    NoCompileView,
    #[error("actor start carried unknown effect profile {0}")]
    UnknownProfile(i64),
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
        let ActorReq::ActorStartWith(label, _entry_projection, profile, explicit_exports) =
            ActorReq::from_value(request, table)?
        else {
            return Err(ActorStartCaptureError::UnexpectedRequest);
        };
        let child_realm = RealmId::fresh();
        let entry = session
            .live_payload_handle_owned_by(parent_hole.cont_id(), child_realm)
            .ok_or(ActorStartCaptureError::MissingEntry)?;
        let profile = match profile {
            0 => crate::ActorEffectProfile::ReadWrite,
            1 => crate::ActorEffectProfile::ReadOnly,
            other => return Err(ActorStartCaptureError::UnknownProfile(other)),
        };
        let facade = materialize_entry_facade(session, &entry, &explicit_exports)?;
        let lexical_scope = session.mint_isolated_scope();
        let descriptor = ActorDescriptor::new(
            label.clone(),
            profile.effect_names().iter().copied(),
            crate::ActorPlacement {
                session: session_id,
                resource_scope: child_realm,
                lexical_scope,
            },
        )
        .with_profile(profile)
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
    explicit_exports: &[String],
) -> Result<MaterializedFacade, ActorStartCaptureError>
where
    H: DispatchEffect<O> + Send,
    O: OutputSink + Sync,
{
    let heads = facade_heads(entry.provenance(), explicit_exports);
    let scope = session.run_context().lexical_scope;
    validate_head_incarnations(session, scope, entry.provenance(), &heads)?;
    let names: Vec<_> = heads.iter().map(String::as_str).collect();
    let surface = session.exact_exports_in(scope, &names)?;
    let view = session
        .compile_view_in(scope)
        .ok_or(ActorStartCaptureError::NoCompileView)?;
    Ok(surface.materialize(&view)?)
}

fn validate_head_incarnations<H, O>(
    session: &ResidentSession<H, O>,
    scope: tidepool_codegen::scope::ScopeId,
    provenance: &tidepool_runtime::session::ProgramProvenance,
    selected: &BTreeSet<String>,
) -> Result<(), ActorStartCaptureError>
where
    H: DispatchEffect<O> + Send,
    O: OutputSink + Sync,
{
    let mut rooted: std::collections::BTreeMap<String, BTreeSet<String>> =
        std::collections::BTreeMap::new();
    for site in provenance.sites() {
        for head in site
            .heads
            .iter()
            .chain(site.inputs.iter().flat_map(|input| input.heads.iter()))
            .filter(|head| head.module.starts_with("Tidepool.Session.Lib.G"))
        {
            if selected.contains(&head.name) {
                rooted
                    .entry(head.name.clone())
                    .or_default()
                    .insert(head.module.clone());
            }
        }
    }

    let visible: std::collections::BTreeMap<_, _> =
        session.current_decl_heads_in(scope).into_iter().collect();
    for (head, modules) in rooted {
        if modules.len() != 1 {
            return Err(ActorStartCaptureError::AmbiguousIncarnation {
                head,
                modules: modules.into_iter().collect(),
            });
        }
        let Some(expected) = modules.into_iter().next() else {
            unreachable!("the rooted module count was validated above");
        };
        let Some(generation) = visible.get(&head) else {
            continue;
        };
        let actual = SessionModule::lib(Generation(*generation)).module_name();
        if actual != expected {
            return Err(ActorStartCaptureError::ShadowDrift {
                head,
                expected,
                actual,
            });
        }
    }
    Ok(())
}

fn facade_heads(
    provenance: &tidepool_runtime::session::ProgramProvenance,
    explicit_exports: &[String],
) -> BTreeSet<String> {
    let mut heads: BTreeSet<_> = explicit_exports.iter().cloned().collect();
    for site in provenance.sites() {
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
    heads
}

#[cfg(test)]
mod tests {
    use super::facade_heads;

    #[test]
    fn explicit_facade_heads_are_deduplicated_and_sorted() {
        let heads = facade_heads(
            &tidepool_runtime::session::ProgramProvenance::default(),
            &["Policy".into(), "helper".into(), "Policy".into()],
        );
        assert_eq!(heads.into_iter().collect::<Vec<_>>(), ["Policy", "helper"]);
    }
}

#[derive(Debug, thiserror::Error)]
pub enum ResidentActorStartError {
    #[error("actor startup was cancelled")]
    Cancelled,
    #[error("actor initialization panicked")]
    InitializationPanicked,
    #[error("actor startup panicked while resuming its parent")]
    ParentResumePanicked,
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
    lifecycle: Arc<crate::ResidentActorLifecycle<H, O>>,
}

/// Exclusive custody of an actor between registry allocation and readiness
/// publication. Cancellable or unwind-catching phase futures borrow this
/// owner, so they cannot discard the token needed for authoritative cleanup.
struct UnpublishedResidentActor {
    starting: crate::StartingActor,
    context: crate::ActorSessionContext,
    realm: RealmId,
}

impl UnpublishedResidentActor {
    async fn abort<H, O>(
        self,
        lifecycle: &crate::ResidentActorLifecycle<H, O>,
        terminal: ActorTerminal,
    ) -> Result<(), ResidentLifecycleError>
    where
        H: DispatchEffect<O> + Send + 'static,
        O: OutputSink + Sync + 'static,
    {
        lifecycle.abort_starting(self.starting, terminal).await
    }

    async fn publish<H, O>(
        mut self,
        registry: &ActorRegistry,
        lifecycle: &crate::ResidentActorLifecycle<H, O>,
    ) -> Result<crate::ActorRef, ResidentActorStartError>
    where
        H: DispatchEffect<O> + Send + 'static,
        O: OutputSink + Sync + 'static,
    {
        match registry.publish_ready_borrowed(&mut self.starting) {
            Ok(actor) => Ok(actor),
            Err(error) => {
                let terminal = failed_terminal(error.to_string());
                self.abort(lifecycle, terminal).await?;
                Err(error.into())
            }
        }
    }
}

impl<H, O> ResidentActorStarter<H, O> {
    #[must_use]
    pub fn new(
        lifecycle: Arc<crate::ResidentActorLifecycle<H, O>>,
        completions: ResidentCompletionExecutor<H, O>,
    ) -> Self {
        let registry = lifecycle.registry();
        let runner = lifecycle.runner();
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
        unpublished: UnpublishedResidentActor,
        terminal: ActorTerminal,
    ) -> Result<(), ResidentActorStartError> {
        unpublished.abort(&self.lifecycle, terminal).await?;
        Ok(())
    }

    pub async fn start(
        &self,
        parent_turn: TurnLease,
        provider: &dyn DynModelProvider,
        start: ResidentActorStart,
        sink: Option<StreamSink>,
    ) -> Result<(TurnLease, crate::ActorRef, ResidentOutcome), ResidentActorStartError> {
        self.start_until_cancelled(parent_turn, provider, start, sink, std::future::pending())
            .await
    }

    /// Run startup under a cooperative cancellation signal. The signal may
    /// stop ordinary startup work, but terminal publication, shutdown hooks,
    /// and realm cleanup always run to completion before this returns.
    pub(crate) async fn start_until_cancelled<C>(
        &self,
        parent_turn: TurnLease,
        provider: &dyn DynModelProvider,
        start: ResidentActorStart,
        sink: Option<StreamSink>,
        mut cancelled: C,
    ) -> Result<(TurnLease, crate::ActorRef, ResidentOutcome), ResidentActorStartError>
    where
        C: Future<Output = ()> + Unpin,
    {
        let owner = parent_turn.actor();
        let parent_context = parent_turn.session_context();
        let (descriptor, parent_hole, entry) = start.into_parts();
        let child_realm = descriptor.placement().resource_scope;
        let starting =
            self.registry
                .begin_start(Some(owner), descriptor, StartInitiator::Policy)?;
        let unpublished = UnpublishedResidentActor {
            context: self.registry.session_context(starting.actor())?,
            starting,
            realm: child_realm,
        };
        let child_session = self.registry.startup_agent_session(&unpublished.starting)?;

        let startup_result = await_startup_phase(
            &mut cancelled,
            AssertUnwindSafe(async {
                let mut admitted = None;
                let mut outcome = self
                    .runner
                    .run_rooted_entry(unpublished.context.clone(), entry, unpublished.realm)
                    .await?;
                let readiness = loop {
                    match self
                        .runner
                        .capture_startup_step(
                            unpublished.context.clone(),
                            outcome,
                            unpublished.realm,
                        )
                        .await?
                    {
                        ResidentActorStartupStep::InstallShutdown(shutdown) => {
                            let (continuation, hook) = shutdown.into_parts();
                            self.registry
                                .install_starting_shutdown(&unpublished.starting, hook)?;
                            outcome = self
                                .runner
                                .resume_unit(unpublished.context.clone(), continuation)
                                .await?;
                        }
                        ResidentActorStartupStep::Deliberate(completion) => {
                            let admitted = match admitted.as_mut() {
                                Some(admitted) => admitted,
                                None => admitted.insert(
                                    child_session
                                        .begin_startup_agent_session(&unpublished.starting)?,
                                ),
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
                let child = self
                    .runner
                    .resume_readiness(unpublished.context.clone(), readiness)
                    .await?;
                let completed = matches!(child, ResidentOutcome::Completed { .. });
                if !completed {
                    let receiver = self
                        .runner
                        .capture_receiver(unpublished.context.clone(), child, unpublished.realm)
                        .await?;
                    self.registry
                        .install_starting_receiver(&unpublished.starting, receiver)?;
                }
                Ok::<_, ResidentActorStartError>(completed)
            })
            .catch_unwind(),
        )
        .await;

        let completed = match startup_result {
            StartupPhaseOutcome::Completed(Ok(completed)) => completed,
            StartupPhaseOutcome::Completed(Err(error)) => {
                self.abort_starting(
                    unpublished,
                    ActorTerminal {
                        kind: ActorExitKind::Failed,
                        summary: error.to_string(),
                    },
                )
                .await?;
                return Err(error);
            }
            StartupPhaseOutcome::Panicked => {
                self.abort_starting(
                    unpublished,
                    failed_terminal(ResidentActorStartError::InitializationPanicked.to_string()),
                )
                .await?;
                return Err(ResidentActorStartError::InitializationPanicked);
            }
            StartupPhaseOutcome::Cancelled => {
                self.abort_starting(unpublished, cancelled_terminal())
                    .await?;
                return Err(ResidentActorStartError::Cancelled);
            }
        };
        let actor = unpublished.publish(&self.registry, &self.lifecycle).await?;
        if completed {
            let cleanup = self
                .lifecycle
                .force_terminate(
                    actor,
                    ActorTerminal {
                        kind: ActorExitKind::Completed,
                        summary: "completed".into(),
                    },
                )
                .await;
            match cleanup {
                Ok(()) | Err(ResidentLifecycleError::Shutdown { .. }) => {}
                Err(error) => return Err(error.into()),
            }
        }
        let parent = match await_startup_phase(
            &mut cancelled,
            AssertUnwindSafe(self.runner.resume_starting_parent(
                parent_context,
                parent_hole,
                actor,
            ))
            .catch_unwind(),
        )
        .await
        {
            StartupPhaseOutcome::Completed(Ok(parent)) => parent,
            StartupPhaseOutcome::Completed(Err(error)) => {
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
            StartupPhaseOutcome::Panicked => {
                let _ = self
                    .lifecycle
                    .force_terminate(
                        actor,
                        failed_terminal(ResidentActorStartError::ParentResumePanicked.to_string()),
                    )
                    .await;
                return Err(ResidentActorStartError::ParentResumePanicked);
            }
            StartupPhaseOutcome::Cancelled => {
                let _ = self
                    .lifecycle
                    .force_terminate(actor, cancelled_terminal())
                    .await;
                return Err(ResidentActorStartError::Cancelled);
            }
        };
        Ok((parent_turn, actor, parent))
    }
}

#[derive(Debug, PartialEq, Eq)]
enum StartupPhaseOutcome<T> {
    Completed(T),
    Cancelled,
    Panicked,
}

async fn await_startup_phase<T, Panic>(
    cancelled: &mut (impl Future<Output = ()> + Unpin),
    work: impl Future<Output = Result<T, Panic>>,
) -> StartupPhaseOutcome<T> {
    tokio::select! {
        biased;
        () = cancelled => StartupPhaseOutcome::Cancelled,
        result = work => match result {
            Ok(value) => StartupPhaseOutcome::Completed(value),
            Err(_) => StartupPhaseOutcome::Panicked,
        },
    }
}

fn cancelled_terminal() -> ActorTerminal {
    ActorTerminal {
        kind: ActorExitKind::Cancelled,
        summary: "startup cancelled before settlement".into(),
    }
}

fn failed_terminal(summary: String) -> ActorTerminal {
    ActorTerminal {
        kind: ActorExitKind::Failed,
        summary,
    }
}

#[cfg(test)]
mod cancellation_tests {
    use futures_util::FutureExt;

    #[tokio::test]
    async fn cancellation_wins_an_unpublished_startup_tie() {
        let mut cancelled = std::future::ready(());
        assert_eq!(
            super::await_startup_phase(&mut cancelled, async { Ok::<_, ()>(41) }).await,
            super::StartupPhaseOutcome::Cancelled
        );
    }

    #[tokio::test]
    async fn ordinary_startup_progresses_without_cancellation() {
        let mut cancelled = std::future::pending();
        assert_eq!(
            super::await_startup_phase(&mut cancelled, async { Ok::<_, ()>(41) }).await,
            super::StartupPhaseOutcome::Completed(41)
        );
    }

    #[tokio::test]
    async fn panic_is_a_typed_startup_phase_outcome() {
        let mut cancelled = std::future::pending();
        assert_eq!(
            super::await_startup_phase(
                &mut cancelled,
                std::panic::AssertUnwindSafe(async { panic!("injected startup panic") })
                    .catch_unwind(),
            )
            .await,
            super::StartupPhaseOutcome::<()>::Panicked
        );
    }
}
