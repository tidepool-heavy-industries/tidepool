//! Captured boundaries for one resident Haskell MCP policy.
//!
//! These values are deliberately transport-neutral. The actor runtime owns
//! the suspended Haskell continuation; `tidepool-mcp` projects the immutable
//! declarations and supplies invocations above this boundary.

use std::sync::Arc;

use tidepool_effect::dispatch::DispatchEffect;
use tidepool_runtime::session::{OutputSink, ResidentHole};

use crate::{
    ActorExitKind, ActorRef, ActorRegistry, ActorTerminal, ActorTurnKind, ResidentActorLifecycle,
    ResidentActorRunner,
};

/// A policy waiting for its next invocation.
pub(crate) struct ResidentMcpAwait {
    pub(crate) continuation: ResidentHole,
    pub(crate) declarations: Vec<tidepool_tool::ToolDeclaration>,
    pub(crate) synopsis: String,
}

/// A completed invocation waiting for Rust to acknowledge its result.
pub(crate) struct ResidentMcpReply {
    pub(crate) continuation: ResidentHole,
    pub(crate) result: serde_json::Value,
}

#[derive(Debug, thiserror::Error)]
pub enum ResidentMcpError {
    #[error(transparent)]
    Registry(#[from] crate::ActorRegistryError),
    #[error(transparent)]
    Workbench(#[from] crate::ResidentActorWorkbenchError),
    #[error("resident MCP invocation reached `{0}` instead of a reply")]
    ExpectedReply(&'static str),
    #[error("resident MCP reply resumed to `{0}` instead of awaiting the next invocation")]
    ExpectedAwait(&'static str),
    #[error("resident MCP policy changed its declarations while installed")]
    DeclarationDrift,
    #[error("resident MCP policy is unavailable after an earlier failed invocation")]
    Unavailable,
}

pub struct ResidentMcpPolicy<H, O> {
    actor: ActorRef,
    registry: ActorRegistry,
    runner: ResidentActorRunner<H, O>,
    lifecycle: Arc<ResidentActorLifecycle<H, O>>,
    declarations: Arc<[tidepool_tool::ToolDeclaration]>,
    instructions: Option<String>,
    awaiting: tokio::sync::Mutex<Option<ResidentMcpAwait>>,
}

impl<H, O> ResidentMcpPolicy<H, O>
where
    H: DispatchEffect<O> + Send + 'static,
    O: OutputSink + Sync + 'static,
{
    #[must_use]
    pub fn declarations(&self) -> &[tidepool_tool::ToolDeclaration] {
        &self.declarations
    }

    #[must_use]
    pub fn instructions(&self) -> Option<&str> {
        self.instructions.as_deref()
    }

    pub async fn dispatch(
        &self,
        name: String,
        arguments: serde_json::Value,
    ) -> Result<serde_json::Value, ResidentMcpError> {
        let result = self.dispatch_inner(name, arguments).await;
        if let Err(error) = &result {
            let _ = self
                .lifecycle
                .force_terminate(
                    self.actor,
                    ActorTerminal {
                        kind: ActorExitKind::Failed,
                        summary: error.to_string(),
                    },
                )
                .await;
        }
        result
    }

    async fn dispatch_inner(
        &self,
        name: String,
        arguments: serde_json::Value,
    ) -> Result<serde_json::Value, ResidentMcpError> {
        let mut slot = self.awaiting.lock().await;
        let awaiting = slot.take().ok_or(ResidentMcpError::Unavailable)?;
        let expected_declarations = awaiting.declarations.clone();
        let context = self.registry.session_context(self.actor)?;
        let _turn = self
            .registry
            .begin_turn(self.actor, ActorTurnKind::Haskell)?;
        let outcome = self
            .runner
            .resume_mcp_invocation(context.clone(), awaiting.continuation, name, arguments)
            .await?;
        let reply = match self
            .runner
            .capture_boundary(context.clone(), outcome, context.placement.resource_scope)
            .await?
        {
            crate::resident_workbench::ResidentActorBoundary::McpReply(reply) => reply,
            other => return Err(ResidentMcpError::ExpectedReply(other.operation())),
        };
        let result = reply.result;
        let outcome = self
            .runner
            .resume_unit(context.clone(), reply.continuation)
            .await?;
        let next = match self
            .runner
            .capture_boundary(context.clone(), outcome, context.placement.resource_scope)
            .await?
        {
            crate::resident_workbench::ResidentActorBoundary::McpAwait(next) => next,
            other => return Err(ResidentMcpError::ExpectedAwait(other.operation())),
        };
        if next.declarations != expected_declarations {
            return Err(ResidentMcpError::DeclarationDrift);
        }
        *slot = Some(next);
        Ok(result)
    }
}

pub(crate) fn install_resident_mcp<H, O>(
    actor: ActorRef,
    registry: ActorRegistry,
    runner: ResidentActorRunner<H, O>,
    lifecycle: Arc<ResidentActorLifecycle<H, O>>,
    awaiting: ResidentMcpAwait,
) -> ResidentMcpPolicy<H, O>
where
    H: DispatchEffect<O> + Send + 'static,
    O: OutputSink + Sync + 'static,
{
    let declarations = awaiting.declarations.clone().into();
    let instructions = (!awaiting.synopsis.is_empty()).then(|| awaiting.synopsis.clone());
    ResidentMcpPolicy {
        actor,
        registry,
        runner,
        lifecycle,
        declarations,
        instructions,
        awaiting: tokio::sync::Mutex::new(Some(awaiting)),
    }
}
