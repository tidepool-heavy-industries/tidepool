//! Retaining an actor moves cleanup custody through the existing invocation
//! membership and kernel supervision owners; its creation authority is unchanged.
use super::*;

#[derive(Debug, Clone, Copy, PartialEq, Eq, tidepool_bridge_derive::ToHaskell)]
#[haskell(module = "Tidepool.Effects.Core")]
pub(super) enum AgentRetentionError {
    AgentRetainUnavailable,
    AgentRetainUnauthorized,
    AgentRetainOwnerUnavailable,
    AgentRetainOwnerClosed,
}

impl<H, O> ResidentKernelBehavior<H, O>
where
    H: DispatchEffect<O> + Send + 'static,
    O: OutputSink + Sync + 'static,
{
    pub(super) fn retain_agent_resource(
        &self,
        kernel: &KernelContext,
        context: &ActorSessionContext,
        effect_owner: &CurrentEffectOwner<'_>,
        target: ActorRef,
        lifetime: crate::WorkerLifetime,
    ) -> Result<(), AgentRetentionError> {
        use AgentRetentionError::*;
        let child = kernel
            .resolve(target)
            .filter(|child| child.terminal().get().is_none())
            .ok_or(AgentRetainUnavailable)?;
        {
            let records = self.environment.actors.lock();
            let record = records.get(&target).ok_or(AgentRetainUnavailable)?;
            if target == context.actor
                || record
                    .descriptor
                    .supervisor_parent()
                    .or(record.descriptor.creator())
                    != Some(context.actor)
            {
                return Err(AgentRetainUnauthorized);
            }
        }
        let destination = match lifetime {
            crate::WorkerLifetime::InvocationOwned => effect_owner
                .invocation_work()
                .map(Some)
                .ok_or(AgentRetainOwnerUnavailable)?,
            crate::WorkerLifetime::InScope(
                tidepool_bridge_effects::ResourceScopeId::ScopeToken(token),
            ) => {
                fn retained(root: Arc<InvocationWork>, token: i64) -> Option<Arc<InvocationWork>> {
                    if root.scope_token() == Some(token) {
                        return Some(root);
                    }
                    root.scopes()
                        .into_iter()
                        .find_map(|scope| retained(scope, token))
                }
                let owner = self
                    .retained_scope_roots()
                    .into_iter()
                    .filter(|root| root.is_owned_by(context.actor))
                    .find_map(|root| retained(root, token))
                    .ok_or(AgentRetainOwnerUnavailable)?;
                owner
                    .with_admission(|| ())
                    .map_err(|_| AgentRetainOwnerClosed)?;
                Some(owner)
            }
            crate::WorkerLifetime::ActorOwned | crate::WorkerLifetime::RunOwned => None,
        };
        fn source(root: Arc<InvocationWork>, target: ActorRef) -> Option<Arc<InvocationWork>> {
            if root.owns_worker(target) {
                return Some(root);
            }
            root.scopes()
                .into_iter()
                .find_map(|scope| source(scope, target))
        }
        let previous = self
            .retained_scope_roots()
            .into_iter()
            .filter(|root| root.is_owned_by(context.actor))
            .find_map(|root| source(root, target));
        let destination_run = lifetime == crate::WorkerLifetime::RunOwned;
        let transferred = kernel
            .with_worker_lifetime_transfer(&child, destination_run, || {
                match (previous, destination) {
                    (Some(source), Some(destination)) => {
                        source.transfer_worker_to_owner(&destination, context.actor, target)
                    }
                    (Some(source), None) => source.release_worker_cleanup(context.actor, target),
                    (None, Some(destination)) => {
                        destination.adopt_worker(context.actor, child.clone())
                    }
                    (None, None) => Ok(()),
                }
            })
            .map_err(|error| match error {
                crate::local_actor::LifetimeTransferError::Closed => AgentRetainOwnerClosed,
                crate::local_actor::LifetimeTransferError::Unauthorized => AgentRetainUnauthorized,
                crate::local_actor::LifetimeTransferError::Unavailable => AgentRetainUnavailable,
            })?;
        transferred?;
        if let Some(record) = self.environment.actors.lock().get_mut(&target) {
            record.scheduler_root = destination_run;
            record.descriptor = record
                .descriptor
                .clone()
                .with_supervisor_parent((!destination_run).then_some(context.actor));
        }
        Ok(())
    }
}
