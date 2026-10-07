//! Lexical delimiters retain callback and parent continuation custody while
//! borrowing the existing invocation resource owner and native workbench lane.
use super::*;
use tidepool_bridge_effects::{CleanupError, ResourceScopeId, ScopeFailure};

pub(super) struct ScopeFrame {
    pub continuation: ResidentHole,
    pub work: Arc<InvocationWork>,
    pub realm: RealmId,
}

pub(super) type ScopeStatus = (Result<(), ScopeFailure>, Result<(), CleanupError>);

pub(super) fn finish_scope<H, O>(
    environment: ResidentEnvironment<H, O>,
    kernel: KernelContext,
    context: ActorSessionContext,
    frame: ScopeFrame,
    body: Result<(), ScopeFailure>,
) -> futures_util::future::BoxFuture<'static, Result<ResidentOutcome, ResidentActorWorkbenchError>>
where
    H: DispatchEffect<O> + Send + 'static,
    O: OutputSink + Sync + 'static,
{
    Box::pin(async move {
        frame.work.close();
        let cleanup = frame.work.cleanup(&environment, &kernel).await;
        let cleanup = cleanup.uncertainty().map_or(Ok(()), |detail| {
            Err(CleanupError::ScopeCleanupUnconfirmed(detail))
        });
        // Parent continuation owns the result cell before body roots retire.
        let status: ScopeStatus = (body, cleanup);
        environment
            .runner
            .resume_value(context, frame.continuation, status)
            .await
    })
}

impl<H, O> ResidentKernelBehavior<H, O>
where
    H: DispatchEffect<O> + Send + 'static,
    O: OutputSink + Sync + 'static,
{
    pub(super) fn retained_scope_roots(&self) -> Vec<Arc<InvocationWork>> {
        self.workbench_executions.lock().invocation_work()
    }

    pub(super) fn resolve_resource_owner(
        &self,
        context: &ActorSessionContext,
        effect_owner: &CurrentEffectOwner<'_>,
        lifetime: crate::WorkerLifetime,
    ) -> Result<Option<Arc<InvocationWork>>, ResidentActorWorkbenchError> {
        match lifetime {
            crate::WorkerLifetime::ActorOwned | crate::WorkerLifetime::RunOwned => Ok(None),
            crate::WorkerLifetime::InvocationOwned => {
                effect_owner.invocation_work().map(Some).ok_or_else(|| {
                    ResidentActorWorkbenchError::ActorProtocol(
                        "InvocationOwned requires an admitted invocation".into(),
                    )
                })
            }
            crate::WorkerLifetime::InScope(ResourceScopeId::ScopeToken(token)) => self
                .retained_scope_roots()
                .into_iter()
                .find_map(|root| root.find_scope(context.actor, token).ok())
                .map(Some)
                .ok_or_else(|| {
                    ResidentActorWorkbenchError::ActorProtocol(
                        "scope is unavailable, closed, or belongs to another actor incarnation"
                            .into(),
                    )
                }),
        }
    }

    pub(super) fn request_resource_owner(
        &self,
        context: &ActorSessionContext,
        effect_owner: &CurrentEffectOwner<'_>,
        lifetime: crate::WorkerLifetime,
    ) -> Result<Option<Arc<InvocationWork>>, crate::ReplyError> {
        match lifetime {
            crate::WorkerLifetime::ActorOwned | crate::WorkerLifetime::RunOwned => Ok(None),
            crate::WorkerLifetime::InvocationOwned => effect_owner
                .invocation_work()
                .map(Some)
                .ok_or(crate::ReplyError::Unauthorized),
            crate::WorkerLifetime::InScope(ResourceScopeId::ScopeToken(token)) => {
                fn retained(root: Arc<InvocationWork>, token: i64) -> Option<Arc<InvocationWork>> {
                    if root.scope_token() == Some(token) {
                        return Some(root);
                    }
                    root.scopes()
                        .into_iter()
                        .find_map(|child| retained(child, token))
                }
                let scope = self
                    .retained_scope_roots()
                    .into_iter()
                    .filter(|root| root.is_owned_by(context.actor))
                    .find_map(|root| retained(root, token))
                    .ok_or(crate::ReplyError::Unauthorized)?;
                scope
                    .with_admission(|| ())
                    .map_err(|_| crate::ReplyError::CancellationRequested)?;
                Ok(Some(scope))
            }
        }
    }

    pub(super) fn retain_request_resource(
        &self,
        context: &ActorSessionContext,
        effect_owner: &CurrentEffectOwner<'_>,
        request: crate::RequestId,
        lifetime: crate::WorkerLifetime,
    ) -> Result<(), crate::ReplyError> {
        use crate::request::ResourceCleanupOwner;
        let old = self
            .environment
            .requests
            .request_cleanup_owner(context.actor, request)?;
        let destination = self.request_resource_owner(context, effect_owner, lifetime)?;
        let new = match lifetime {
            crate::WorkerLifetime::ActorOwned => ResourceCleanupOwner::Actor,
            crate::WorkerLifetime::RunOwned => ResourceCleanupOwner::Run,
            _ => destination
                .as_ref()
                .expect("bounded retention owner")
                .resource_cleanup_owner(),
        };
        fn find(
            root: Arc<InvocationWork>,
            old: &ResourceCleanupOwner,
        ) -> Option<Arc<InvocationWork>> {
            if &root.resource_cleanup_owner() == old {
                return Some(root);
            }
            root.scopes().into_iter().find_map(|scope| find(scope, old))
        }
        let source = match old {
            ResourceCleanupOwner::Actor | ResourceCleanupOwner::Run => None,
            _ => Some(
                self.retained_scope_roots()
                    .into_iter()
                    .find_map(|root| find(root, &old))
                    .ok_or(crate::ReplyError::CancellationRequested)?,
            ),
        };
        let transfer = || {
            self.environment.requests.transfer_request_cleanup_owner(
                context.actor,
                request,
                &old,
                new,
            )
        };
        match (source, destination) {
            (Some(source), Some(destination)) => {
                source.with_transfer_admission(&destination, transfer)
            }
            (Some(source), None) => source.with_admission(transfer),
            (None, Some(destination)) => destination.with_admission(transfer),
            (None, None) => Ok(transfer()),
        }
        .map_err(|_| crate::ReplyError::CancellationRequested)?
    }

    pub(super) fn register_resource_scope(
        &mut self,
        context: &ActorSessionContext,
        effect_owner: &CurrentEffectOwner<'_>,
    ) -> Result<(Arc<InvocationWork>, RealmId), String> {
        let parent = match effect_owner {
            CurrentEffectOwner::Scoped { scope, .. } => scope.clone(),
            _ => effect_owner.invocation_work().unwrap_or_else(|| {
                self.workbench_executions
                    .lock()
                    .actor_scope_root(context.actor)
            }),
        };
        let work = parent.new_scope()?;
        let realm = RealmId::fresh();
        work.register_scope_realm(context.clone(), realm)?;
        Ok((work, realm))
    }

    pub(super) async fn run_resource_scope(
        &mut self,
        kernel: &KernelContext,
        context: &ActorSessionContext,
        effect_owner: CurrentEffectOwner<'_>,
        ancestry: &crate::CallAncestry,
        continuation: ResidentHole,
        callback: RootCustody,
    ) -> Result<ResidentOutcome, ResidentActorWorkbenchError> {
        let (work, realm) = match self.register_resource_scope(context, &effect_owner) {
            Ok(registered) => registered,
            Err(detail) => {
                return self
                    .environment
                    .runner
                    .resume_value(
                        context.clone(),
                        continuation,
                        (
                            Err::<(), _>(ScopeFailure::ScopeRejected(detail)),
                            Ok::<(), CleanupError>(()),
                        ),
                    )
                    .await
            }
        };
        let frame = ScopeFrame {
            continuation,
            work,
            realm,
        };
        let token = frame.work.scope_token().expect("registered scope identity");
        let scoped_owner = CurrentEffectOwner::Scoped {
            base: Box::new(effect_owner),
            scope: frame.work.clone(),
        };
        let mut next = self
            .environment
            .runner
            .run_owned_scope_callback(
                context.clone(),
                callback,
                frame.realm,
                token,
                frame.work.clone(),
            )
            .await;
        let body = loop {
            let boundary = match next {
                Ok(outcome) => {
                    self.environment
                        .runner
                        .capture_boundary(context.clone(), outcome, frame.realm)
                        .await
                }
                Err(error) => break Err(ScopeFailure::ScopeEvaluationFailed(error.to_string())),
            };
            match boundary {
                Ok(ResidentActorBoundary::ScopeDone { token: done, .. }) if done == token => {
                    break Ok(())
                }
                Ok(ResidentActorBoundary::ScopeDone { .. }) => {
                    break Err(ScopeFailure::ScopeEvaluationFailed(
                        "scope completion belongs to a different delimiter".into(),
                    ))
                }
                Ok(ResidentActorBoundary::Completed) => {
                    break Err(ScopeFailure::ScopeEvaluationFailed(
                        "scope body completed without its result marker".into(),
                    ))
                }
                Ok(boundary) => {
                    next = Box::pin(self.resolve_effect(
                        kernel,
                        context,
                        scoped_owner.clone(),
                        ancestry,
                        boundary,
                    ))
                    .await
                }
                Err(error) => break Err(ScopeFailure::ScopeEvaluationFailed(error.to_string())),
            }
        };
        finish_scope(
            self.environment.clone(),
            kernel.clone(),
            context.clone(),
            frame,
            body,
        )
        .await
    }

    pub(super) fn prepare_scope_finish(
        &self,
        kernel: &KernelContext,
        context: &ActorSessionContext,
        current: &mut WorkbenchFragmentExecution,
        body: Result<(), ScopeFailure>,
    ) -> Result<(), ResidentActorWorkbenchError> {
        let frame = current.scopes.pop().ok_or_else(|| {
            ResidentActorWorkbenchError::ActorProtocol("scope stack is empty".into())
        })?;
        current.native_start = Some(owned_workbench::WorkbenchFragmentRequest::ScopeResume {
            fragment: current
                .fragment
                .take()
                .expect("scope retains parent fragment"),
            operation: finish_scope(
                self.environment.clone(),
                kernel.clone(),
                context.clone(),
                frame,
                body,
            ),
        });
        Ok(())
    }
}
