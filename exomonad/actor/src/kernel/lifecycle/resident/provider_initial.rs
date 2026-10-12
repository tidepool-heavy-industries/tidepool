//! One exact provider admission spans native grant, retained publication and settlement.
use super::*;
use tidepool_runtime::session::ProviderTransition;

pub(super) struct Request {
    pub expected: Arc<WorkbenchPublicOwner>,
    pub operation: Operation,
    pub reply: RpcReplyPort<Result<InitialContextObservation, String>>,
}

pub(super) enum Operation {
    Observe,
    Acknowledge {
        operation: tidepool_runtime::session::WorkbenchExecutionId,
        publication: crate::InitialContextPublication,
    },
}

enum Effect {
    Observe(InitialContextObservation),
    Acknowledge {
        operation: tidepool_runtime::session::WorkbenchExecutionId,
        publication: crate::InitialContextPublication,
        projected: InitialContextObservation,
    },
}

pub(super) struct Pending {
    reply: RpcReplyPort<Result<InitialContextObservation, String>>,
    stage: Stage,
}

enum Stage {
    Beginning(Effect),
    Journal { ticket: Option<ProviderTransition> },
    Settling(Result<InitialContextObservation, String>),
}

pub(super) enum Completion {
    Begun(Result<Option<ProviderTransition>, String>),
    Journal(Result<InitialContextObservation, JournalFailure>),
    Settled(Result<(), String>),
    Failed(String),
}

pub(super) enum JournalFailure {
    Refused(String),
    Uncertain(String),
}

pub(super) fn begin(
    state: &mut LifecycleState,
    _sequence: u64,
    request: Request,
) -> Option<(super::Pending, BoxFuture<'static, super::Completion>)> {
    let resident = state
        .resident
        .as_ref()
        .expect("configured resident request");
    let current = &resident.current;
    let refuse = |detail: &str, request: Request| {
        let _ = request.reply.send(Err(detail.into()));
    };
    if state.refusal().is_some()
        || resident.successor.is_some()
        || current
            .startup
            .is_some_and(|startup| startup != RootStartupState::Activated)
        || current
            .public_owner
            .ready()
            .is_none_or(|owner| !Arc::ptr_eq(owner, &request.expected))
    {
        refuse(
            "provider initial context differs from its live exact admission",
            request,
        );
        return None;
    }
    let Some(observed) = current.initial_context.clone() else {
        refuse("initial context has not been admitted", request);
        return None;
    };
    let Request {
        expected,
        operation,
        reply,
    } = request;
    let effect = match operation {
        Operation::Observe => Effect::Observe(observed),
        Operation::Acknowledge {
            operation,
            publication,
        } => {
            if observed
                .identity
                .as_ref()
                .is_none_or(|identity| identity.operation != operation)
            {
                let _ = reply.send(Err(
                    "initial context publication has another operation identity".into(),
                ));
                return None;
            }
            let mut projected = Some(observed);
            if let Err(error) = crate::initial_context::apply_event(
                &mut projected,
                InitialContextEvent::Publish {
                    publication: publication.clone(),
                },
            ) {
                let _ = reply.send(Err(error.to_string()));
                return None;
            }
            Effect::Acknowledge {
                operation,
                publication,
                projected: projected.unwrap(),
            }
        }
    };
    let native = expected.native_readiness();
    Some((
        super::Pending::ProviderInitial(Pending {
            reply,
            stage: Stage::Beginning(effect),
        }),
        Box::pin(async move {
            super::Completion::ProviderInitial(Completion::Begun(match native {
                Some(native) => native
                    .begin_provider_transition()
                    .await
                    .map(Some)
                    .map_err(|error| error.to_string()),
                None => Ok(None),
            }))
        }),
    ))
}

pub(super) fn complete(
    state: &mut LifecycleState,
    address: &RactorRef<LifecycleCommand>,
    sequence: u64,
    outcome: Completion,
) {
    let Some(resident) = state.resident.as_mut() else {
        return;
    };
    let matches = resident.pending.as_ref().is_some_and(|(issued, pending)| {
        *issued == sequence && matches!(pending, super::Pending::ProviderInitial(pending) if
            matches!((&pending.stage, &outcome),
                (Stage::Beginning(_), Completion::Begun(_)) |
                (Stage::Journal { .. }, Completion::Journal(_)) |
                (Stage::Settling(_), Completion::Settled(_))) || matches!(outcome, Completion::Failed(_)))
    });
    if !matches {
        return;
    }
    let (_, super::Pending::ProviderInitial(pending)) = resident.pending.take().unwrap() else {
        unreachable!()
    };
    state.tasks.remove(&sequence);
    let Pending { reply, stage } = pending;
    match (stage, outcome) {
        (_, Completion::Failed(error)) => uncertain(state, reply, error),
        (Stage::Beginning(_), Completion::Begun(Err(error))) => {
            // No native grant and no local mutation occurred.
            let _ = reply.send(Err(error));
        }
        (Stage::Beginning(effect), Completion::Begun(Ok(ticket))) => match effect {
            Effect::Observe(observed) => {
                let result = if state.is_closed() {
                    Err("actor closed before provider initial-context observation".into())
                } else {
                    Ok(observed)
                };
                settle(state, address, sequence, reply, ticket, result);
            }
            Effect::Acknowledge {
                operation,
                publication,
                projected,
            } => {
                let resident = state.resident.as_mut().unwrap();
                let journal = resident.journal.clone();
                let actor = state.identity;
                resident.pending = Some((
                    sequence,
                    super::Pending::ProviderInitial(Pending {
                        reply,
                        stage: Stage::Journal { ticket },
                    }),
                ));
                super::spawn(
                    state,
                    address,
                    sequence,
                    Box::pin(async move {
                        let result = async {
                        if let Some(journal) = journal {
                            match journal.initial_context_publication_matches(actor, &operation, &publication.conversation).await {
                                Ok(true) => {},
                                Ok(false) => return Err(JournalFailure::Refused("initial context publication has another conversation association".into())),
                                Err(error) => return Err(JournalFailure::Uncertain(error.to_string())),
                            }
                            journal.record_initial_context(actor, InitialContextEvent::Publish { publication }).await
                                .map_err(|error| JournalFailure::Uncertain(error.to_string()))
                        } else { Ok(projected) }
                    }.await;
                        super::Completion::ProviderInitial(Completion::Journal(result))
                    }),
                );
            }
        },
        (Stage::Journal { ticket }, Completion::Journal(result)) => match result {
            Ok(observed) => {
                // Install the retained projection while the native ticket still excludes transfer.
                // Closing is preserved; admitted settlement cannot resurrect readiness.
                state.resident.as_mut().unwrap().current.initial_context = Some(observed.clone());
                state.publish();
                settle(state, address, sequence, reply, ticket, Ok(observed));
            }
            Err(JournalFailure::Refused(error)) => {
                settle(state, address, sequence, reply, ticket, Err(error))
            }
            Err(JournalFailure::Uncertain(error)) => {
                drop(ticket); // Unknown mutation abandons and fences, never releases success.
                uncertain(state, reply, error);
            }
        },
        (Stage::Settling(result), Completion::Settled(outcome)) => match outcome {
            Ok(()) => {
                let _ = reply.send(result);
            }
            Err(error) => uncertain(state, reply, error),
        },
        _ => unreachable!("matched provider initial-context stage"),
    }
}

fn settle(
    state: &mut LifecycleState,
    address: &RactorRef<LifecycleCommand>,
    sequence: u64,
    reply: RpcReplyPort<Result<InitialContextObservation, String>>,
    ticket: Option<ProviderTransition>,
    result: Result<InitialContextObservation, String>,
) {
    let committed = result.is_ok();
    state.resident.as_mut().unwrap().pending = Some((
        sequence,
        super::Pending::ProviderInitial(Pending {
            reply,
            stage: Stage::Settling(result),
        }),
    ));
    super::spawn(
        state,
        address,
        sequence,
        Box::pin(async move {
            super::Completion::ProviderInitial(Completion::Settled(match ticket {
                Some(ticket) if committed => {
                    ticket.complete().await.map_err(|error| error.to_string())
                }
                Some(ticket) => ticket.refuse().await.map_err(|error| error.to_string()),
                None => Ok(()),
            }))
        }),
    );
}

fn uncertain(
    state: &mut LifecycleState,
    reply: RpcReplyPort<Result<InitialContextObservation, String>>,
    error: String,
) {
    state.resident.as_mut().unwrap().current.settlement =
        ResidentSettlement::NeedsAttention(error.clone());
    state.native_settlement = super::super::NativeSettlement::NeedsAttention(error.clone());
    let _ = reply.send(Err(error));
}
