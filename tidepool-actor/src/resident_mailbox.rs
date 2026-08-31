//! Resident execution of typed actor messaging and exact lifecycle waits.
//!
//! Routing and linear reply ownership remain in [`crate::ActorRegistry`].
//! This adapter moves opaque rooted Haskell values through the installed
//! rank-N handler and parks/resumes exact wait obligations. It owns neither a
//! second mailbox nor a second exit-value store.

use tidepool_codegen::suspension::RealmId;
use tidepool_effect::dispatch::DispatchEffect;
use tidepool_runtime::session::{OutputSink, ResidentOutcome};

use crate::mailbox::{InstalledActorState, ResidentOutbound};
use crate::{
    ActorExitKind, ActorRef, ActorRegistry, ActorRegistryError, ActorTerminal, CallFailure,
    CallStatus, CallTicket, MailboxDelivery, MailboxFailure, MailboxValue, ResidentActorRunner,
    ResidentActorWorkbenchError, TurnLease,
};

#[derive(Debug, thiserror::Error)]
pub enum ResidentMailboxError {
    #[error(transparent)]
    Registry(#[from] ActorRegistryError),
    #[error(transparent)]
    Mailbox(#[from] MailboxFailure),
    #[error(transparent)]
    Workbench(#[from] ResidentActorWorkbenchError),
    #[error(transparent)]
    Wait(#[from] crate::ActorWaitError),
    #[error("mailbox {0} had no request value")]
    MissingRequest(&'static str),
    #[error("mailbox handler continued after its private settlement protocol")]
    HandlerDidNotComplete,
    #[error("synchronous actor call failed: {0:?}")]
    CallFailed(CallFailure),
}

pub enum OutboundSettlement {
    Continued {
        turn: TurnLease,
        outcome: ResidentOutcome,
    },
    Pending(ResidentCall),
}

pub struct ResidentCall {
    caller: ActorRef,
    context: crate::ActorSessionContext,
    continuation: tidepool_runtime::session::ResidentHole,
    ticket: CallTicket,
}

pub enum ResidentCallPoll {
    Pending(ResidentCall),
    Continued {
        turn: TurnLease,
        outcome: ResidentOutcome,
    },
}

pub struct ResidentWait {
    waiter: ActorRef,
    context: crate::ActorSessionContext,
    continuation: tidepool_runtime::session::ResidentHole,
    wait: crate::ActorWait,
}

pub enum ResidentWaitPoll {
    Pending(ResidentWait),
    Continued {
        turn: TurnLease,
        outcome: ResidentOutcome,
    },
}

pub struct ResidentActorMailbox<H, O> {
    registry: ActorRegistry,
    runner: ResidentActorRunner<H, O>,
}

impl<H, O> ResidentActorMailbox<H, O> {
    #[must_use]
    pub fn new(registry: ActorRegistry, runner: ResidentActorRunner<H, O>) -> Self {
        Self { registry, runner }
    }
}

impl<H, O> ResidentActorMailbox<H, O>
where
    H: DispatchEffect<O> + Send + 'static,
    O: OutputSink + Sync + 'static,
{
    async fn fail_actor(&self, actor: ActorRef, summary: String) {
        if let Ok(context) = self.registry.session_context(actor) {
            let realm = context.placement.resource_scope;
            let _ = self.runner.close_realm(context, realm).await;
        }
        let _ = self.registry.finish(
            actor,
            ActorTerminal {
                kind: ActorExitKind::Failed,
                summary,
            },
        );
    }

    /// Submit one already-suspended public `call` or `cast`. Cast resumes the
    /// same admitted Haskell turn after truthful mailbox acceptance. Call
    /// releases that turn and returns the exact parked obligation.
    pub async fn submit_outbound(
        &self,
        turn: TurnLease,
        outcome: ResidentOutcome,
    ) -> Result<OutboundSettlement, ResidentMailboxError> {
        let caller = turn.actor();
        let context = turn.session_context();
        let result: Result<OutboundSettlement, ResidentMailboxError> = async {
            match self
                .runner
                .capture_outbound(context.clone(), outcome)
                .await?
            {
                ResidentOutbound::Cast {
                    target,
                    continuation,
                    request,
                } => {
                    self.registry.cast(caller, target, request)?;
                    let outcome = self.runner.resume_unit(context, continuation).await?;
                    Ok(OutboundSettlement::Continued { turn, outcome })
                }
                ResidentOutbound::Call {
                    target,
                    continuation,
                    request,
                } => {
                    let ticket = self.registry.call(caller, target, request)?;
                    drop(turn);
                    Ok(OutboundSettlement::Pending(ResidentCall {
                        caller,
                        context,
                        continuation,
                        ticket,
                    }))
                }
            }
        }
        .await;
        if let Err(error) = &result {
            self.fail_actor(caller, error.to_string()).await;
        }
        result
    }

    /// Poll one exact synchronous call. Pending preserves the obligation;
    /// reply reacquires the caller's serialized Haskell turn before resuming
    /// its continuation with the opaque live result.
    pub async fn poll_call(
        &self,
        mut pending: ResidentCall,
    ) -> Result<ResidentCallPoll, ResidentMailboxError> {
        let caller = pending.caller;
        let result: Result<ResidentCallPoll, ResidentMailboxError> = async {
            match pending.ticket.poll()? {
                CallStatus::Pending => Ok(ResidentCallPoll::Pending(pending)),
                CallStatus::Failed(failure) => Err(ResidentMailboxError::CallFailed(failure)),
                CallStatus::Reply(reply) => {
                    let turn = self
                        .registry
                        .begin_turn(caller, crate::ActorTurnKind::Haskell)?;
                    let outcome = self
                        .runner
                        .resume_live(pending.context, pending.continuation, reply.into_custody())
                        .await?;
                    Ok(ResidentCallPoll::Continued { turn, outcome })
                }
            }
        }
        .await;
        if let Err(error) = &result {
            self.fail_actor(caller, error.to_string()).await;
        }
        result
    }

    /// Park one exact-incarnation `awaitExit` after releasing the current
    /// Haskell turn. The retained result remains in the Haskell `ActorRef`;
    /// this ticket carries only lifecycle observation and the continuation.
    pub async fn submit_wait(
        &self,
        turn: TurnLease,
        outcome: ResidentOutcome,
    ) -> Result<ResidentWait, ResidentMailboxError> {
        let waiter = turn.actor();
        let context = turn.session_context();
        let result: Result<ResidentWait, ResidentMailboxError> = async {
            let request = self.runner.capture_wait(context.clone(), outcome).await?;
            drop(turn);
            let wait = crate::ActorWait::register_target(&self.registry, waiter, request.target)?;
            Ok(ResidentWait {
                waiter,
                context,
                continuation: request.continuation,
                wait,
            })
        }
        .await;
        if let Err(error) = &result {
            self.fail_actor(waiter, error.to_string()).await;
        }
        result
    }

    /// Poll a retained exact wait. Target completion is an ordinary typed
    /// result: reacquire the waiter turn and resume with terminal metadata so
    /// Haskell can read the shared exit cell.
    pub async fn poll_wait(
        &self,
        mut pending: ResidentWait,
    ) -> Result<ResidentWaitPoll, ResidentMailboxError> {
        let terminal = match pending.wait.poll() {
            Ok(None) => return Ok(ResidentWaitPoll::Pending(pending)),
            Ok(Some(terminal)) => terminal,
            Err(error) => {
                self.fail_actor(pending.waiter, error.to_string()).await;
                return Err(error.into());
            }
        };
        let result: Result<ResidentWaitPoll, ResidentMailboxError> = async {
            let turn = self
                .registry
                .begin_turn(pending.waiter, crate::ActorTurnKind::Haskell)?;
            let outcome = self
                .runner
                .resume_terminal(pending.context, pending.continuation, terminal)
                .await?;
            Ok(ResidentWaitPoll::Continued { turn, outcome })
        }
        .await;
        if let Err(error) = &result {
            self.fail_actor(pending.waiter, error.to_string()).await;
        }
        result
    }

    /// Handle at most one accepted message. `false` means the mailbox was
    /// empty; `true` means a call or cast ran to its next installed receive or
    /// completed the actor.
    pub async fn dispatch_one(&self, actor: ActorRef) -> Result<bool, ResidentMailboxError> {
        let Some(delivery) = self.registry.dequeue(actor)? else {
            return Ok(false);
        };
        if let Err(error) = self.dispatch_admitted(actor, delivery).await {
            self.fail_actor(actor, error.to_string()).await;
            return Err(error);
        }
        Ok(true)
    }

    async fn dispatch_admitted(
        &self,
        actor: ActorRef,
        mut delivery: MailboxDelivery,
    ) -> Result<(), ResidentMailboxError> {
        let receiver = self.registry.take_receiver(actor)?;
        let context = self.registry.session_context(actor)?;
        let actor_realm = context.placement.resource_scope;
        let handler_realm = RealmId::fresh();
        let request = match &mut delivery {
            MailboxDelivery::Call(call) => call
                .take_value()
                .ok_or(ResidentMailboxError::MissingRequest("call"))?,
            MailboxDelivery::Cast(cast) => cast
                .take_value()
                .ok_or(ResidentMailboxError::MissingRequest("cast"))?,
        };
        let outcome = self
            .runner
            .run_mailbox_handler(
                context.clone(),
                receiver.handler,
                request.into_custody(),
                handler_realm,
            )
            .await?;
        let reply = self
            .runner
            .capture_kernel_value(
                context.clone(),
                outcome,
                "ActorReplyWith",
                receiver.site,
                handler_realm,
                actor_realm,
            )
            .await?;
        let reply_continuation = reply.continuation;
        let reply = match &mut delivery {
            MailboxDelivery::Call(_) => {
                Some(MailboxValue::new(context.placement.session, reply.value))
            }
            MailboxDelivery::Cast(_) => {
                drop(reply.value);
                None
            }
        };
        let outcome = self
            .runner
            .resume_unit(context.clone(), reply_continuation)
            .await?;
        let next = self
            .runner
            .capture_kernel_value(
                context.clone(),
                outcome,
                "ActorContinueWith",
                receiver.site,
                handler_realm,
                actor_realm,
            )
            .await?;
        let handler_done = self
            .runner
            .resume_unit(context.clone(), next.continuation)
            .await?;
        if !matches!(handler_done, ResidentOutcome::Completed { .. }) {
            return Err(ResidentMailboxError::HandlerDidNotComplete);
        }
        self.runner
            .close_realm(context.clone(), handler_realm)
            .await?;

        let program = self
            .runner
            .resume_live(context.clone(), receiver.continuation, next.value)
            .await?;
        let next = match program {
            ResidentOutcome::Completed { .. } => InstalledActorState::Completed(ActorTerminal {
                kind: ActorExitKind::Completed,
                summary: "completed".into(),
            }),
            suspended => {
                let receiver = self
                    .runner
                    .capture_receiver(context, suspended, actor_realm)
                    .await?;
                InstalledActorState::Receiving(receiver)
            }
        };
        self.registry
            .settle_resident_delivery(actor, &mut delivery, reply, next)?;
        Ok(())
    }
}
