//! `ToHaskell` projections for resident-workbench answers.
//!
//! These wrapper types and their `ToHaskell`/`ToHaskellSealed` impls form the
//! serialization boundary between resident-actor Rust state and the Haskell
//! constructors the compiled program observes. They carry no independent
//! behavior; each `visit` walks the owning Rust value into the matching
//! `Tidepool.Effects.Core` (or effect-specific) constructor.

use super::*;

pub(crate) fn visit_named(
    table: &DataConTable,
    visitor: &mut dyn HaskellVisitor,
    module: &str,
    name: &str,
    fields: impl FnOnce(&mut dyn HaskellVisitor) -> Result<(), BridgeError>,
) -> Result<(), BridgeError> {
    let qualified = format!("{module}.{name}");
    let constructor = table
        .get_by_qualified_name(&qualified)
        .ok_or_else(|| BridgeError::UnknownDataConName(qualified.clone()))?;
    let arity = table
        .get(constructor)
        .map(|data_con| data_con.rep_arity as usize)
        .ok_or(BridgeError::UnknownDataConName(qualified))?;
    visitor.begin_constructor(constructor, arity)?;
    fields(visitor)?;
    visitor.end_constructor()
}

pub(crate) fn actor_haskell_int(value: u64, label: &str) -> Result<i64, BridgeError> {
    i64::try_from(value)
        .map_err(|_| BridgeError::UnsupportedType(format!("{label} exceeds Haskell Int")))
}

pub(super) fn visit_core(
    table: &DataConTable,
    visitor: &mut dyn HaskellVisitor,
    name: &str,
    fields: impl FnOnce(&mut dyn HaskellVisitor) -> Result<(), BridgeError>,
) -> Result<(), BridgeError> {
    visit_named(table, visitor, "Tidepool.Effects.Core", name, fields)
}

/// One actor's `AgentRosterState`, derived from its retained exit terminal.
/// Shared by `AgentRosterEntry` (the `observeAgent` roster) and
/// `PendingProgress` (a still-pending response or watch observation) so both
/// project the same lifecycle evidence instead of each deriving their own.
pub(crate) fn visit_agent_roster_state(
    table: &DataConTable,
    visitor: &mut dyn HaskellVisitor,
    terminal: Option<&crate::ActorTerminal>,
) -> Result<(), BridgeError> {
    match terminal {
        None => visit_core(table, visitor, "RosterRunning", |_| Ok(())),
        Some(t) => match t.kind {
            crate::ActorExitKind::Completed => {
                visit_core(table, visitor, "RosterStopped", |_| Ok(()))
            }
            crate::ActorExitKind::Failed => visit_core(table, visitor, "RosterFailed", |v| {
                t.summary.visit(table, v)
            }),
            crate::ActorExitKind::Cancelled => visit_core(table, visitor, "RosterCancelled", |v| {
                t.summary.visit(table, v)
            }),
        },
    }
}

/// One actor's `ProviderHealth`, derived from its latest observed provider
/// turn. Shared by `AgentRosterEntry` and `PendingProgress`; see
/// `visit_agent_roster_state`.
pub(crate) fn visit_provider_health(
    table: &DataConTable,
    visitor: &mut dyn HaskellVisitor,
    provider_turn: Option<&exomonad_model::ProviderTurnObservation>,
) -> Result<(), BridgeError> {
    match provider_turn.map(|t| &t.state) {
        None => visit_core(table, visitor, "ProviderUnknown", |_| Ok(())),
        Some(exomonad_model::ProviderTurnState::Active) => {
            visit_core(table, visitor, "ProviderActive", |_| Ok(()))
        }
        Some(exomonad_model::ProviderTurnState::Succeeded) => {
            visit_core(table, visitor, "ProviderSucceeded", |_| Ok(()))
        }
        Some(exomonad_model::ProviderTurnState::Interrupted) => {
            visit_core(table, visitor, "ProviderInterrupted", |_| Ok(()))
        }
        Some(exomonad_model::ProviderTurnState::Failed(f)) => {
            visit_core(table, visitor, "ProviderFailed", |v| match f {
                exomonad_model::ProviderFailure::RequestRejected => {
                    visit_core(table, v, "RequestRejected", |_| Ok(()))
                }
                exomonad_model::ProviderFailure::TransportFailed => {
                    visit_core(table, v, "TransportFailed", |_| Ok(()))
                }
                exomonad_model::ProviderFailure::Other(d) => {
                    visit_core(table, v, "OtherProviderFailure", |v| d.visit(table, v))
                }
            })
        }
    }
}

pub(super) struct RouteStateAnswer(
    pub(super) Result<crate::request::routes::RouteState, crate::ReplyError>,
);

impl tidepool_bridge::sealed::ToHaskellSealed for RouteStateAnswer {}
impl ToHaskell for RouteStateAnswer {
    fn visit(
        &self,
        table: &DataConTable,
        visitor: &mut dyn HaskellVisitor,
    ) -> Result<(), BridgeError> {
        use crate::request::routes::RouteState;
        match &self.0 {
            Ok(RouteState::Waiting) => visit_named(
                table,
                visitor,
                "Tidepool.Agent.Watch.Internal",
                "RouteWaiting",
                |_| Ok(()),
            ),
            Ok(RouteState::Running) => visit_named(
                table,
                visitor,
                "Tidepool.Agent.Watch.Internal",
                "RouteRunning",
                |_| Ok(()),
            ),
            Ok(RouteState::Completed) => visit_named(
                table,
                visitor,
                "Tidepool.Agent.Watch.Internal",
                "RouteCompleted",
                |_| Ok(()),
            ),
            Ok(RouteState::Failed(error)) => visit_named(
                table,
                visitor,
                "Tidepool.Agent.Watch.Internal",
                "RouteFailed",
                |visitor| error.visit(table, visitor),
            ),
            Err(error) => visit_named(
                table,
                visitor,
                "Tidepool.Agent.Watch.Internal",
                "RouteRejected",
                |visitor| error.visit(table, visitor),
            ),
        }
    }
}

pub(super) struct CallStatus(pub(super) Option<String>);

impl tidepool_bridge::sealed::ToHaskellSealed for CallStatus {}
impl ToHaskell for CallStatus {
    fn visit(
        &self,
        table: &DataConTable,
        visitor: &mut dyn HaskellVisitor,
    ) -> Result<(), BridgeError> {
        match &self.0 {
            Some(summary) => visit_core(table, visitor, "ActorCallFailed", |visitor| {
                summary.visit(table, visitor)
            }),
            None => visit_core(table, visitor, "ActorCallSucceeded", |_| Ok(())),
        }
    }
}

pub(super) enum ProgressAnswer {
    Pending,
    Closed,
    Rejected(crate::ReplyError),
}

pub(super) enum LifecycleAnswer {
    Live,
    Paused(String),
    Exited(crate::ActorTerminal),
}

impl tidepool_bridge::sealed::ToHaskellSealed for LifecycleAnswer {}
impl ToHaskell for LifecycleAnswer {
    fn visit(
        &self,
        table: &DataConTable,
        visitor: &mut dyn HaskellVisitor,
    ) -> Result<(), BridgeError> {
        match self {
            Self::Live => visit_core(table, visitor, "ActorLive", |_| Ok(())),
            Self::Paused(detail) => visit_core(table, visitor, "ActorPaused", |visitor| {
                detail.visit(table, visitor)
            }),
            Self::Exited(terminal) => visit_core(
                table,
                visitor,
                match terminal.kind {
                    crate::ActorExitKind::Completed => "ActorFinished",
                    crate::ActorExitKind::Failed => "ActorFailed",
                    crate::ActorExitKind::Cancelled => "ActorCancelled",
                },
                |visitor| terminal.summary.visit(table, visitor),
            ),
        }
    }
}

impl tidepool_bridge::sealed::ToHaskellSealed for ProgressAnswer {}
impl ToHaskell for ProgressAnswer {
    fn visit(
        &self,
        table: &DataConTable,
        visitor: &mut dyn HaskellVisitor,
    ) -> Result<(), BridgeError> {
        match self {
            Self::Pending => visit_named(
                table,
                visitor,
                "Tidepool.Agent.Reply.Internal",
                "ProgressPending",
                |_| Ok(()),
            ),
            Self::Closed => visit_named(
                table,
                visitor,
                "Tidepool.Agent.Reply.Internal",
                "ProgressClosed",
                |_| Ok(()),
            ),
            Self::Rejected(error) => visit_named(
                table,
                visitor,
                "Tidepool.Agent.Reply.Internal",
                "ProgressRejected",
                |visitor| error.visit(table, visitor),
            ),
        }
    }
}

impl tidepool_bridge::sealed::ToHaskellSealed for AgentForgetProjection {}
impl ToHaskell for AgentForgetProjection {
    fn visit(
        &self,
        table: &DataConTable,
        visitor: &mut dyn HaskellVisitor,
    ) -> Result<(), BridgeError> {
        match self {
            Self::Forgotten => visit_core(table, visitor, "AgentForgotten", |_| Ok(())),
            Self::Running => visit_core(table, visitor, "AgentForgetRunning", |_| Ok(())),
            Self::Unavailable => visit_core(table, visitor, "AgentForgetUnavailable", |_| Ok(())),
            Self::OutputPending { displays } => {
                visit_core(table, visitor, "AgentForgetOutputPending", |visitor| {
                    actor_haskell_int(*displays as u64, "pending display count")?
                        .visit(table, visitor)
                })
            }
            Self::Retained { requests, watches } => {
                visit_core(table, visitor, "AgentForgetRetained", |visitor| {
                    requests
                        .iter()
                        .map(|request| actor_haskell_int(request.0, "request id"))
                        .collect::<Result<Vec<_>, _>>()?
                        .visit(table, visitor)?;
                    watches
                        .iter()
                        .map(|watch| actor_haskell_int(watch.0, "watch id"))
                        .collect::<Result<Vec<_>, _>>()?
                        .visit(table, visitor)
                })
            }
        }
    }
}

impl tidepool_bridge::sealed::ToHaskellSealed for AgentStopProjection {}
impl ToHaskell for AgentStopProjection {
    fn visit(
        &self,
        table: &DataConTable,
        visitor: &mut dyn HaskellVisitor,
    ) -> Result<(), BridgeError> {
        match self {
            Self::StoppedNow => visit_core(table, visitor, "AgentStoppedNow", |_| Ok(())),
            Self::StoppedRetaining(detail) => {
                visit_core(table, visitor, "AgentStoppedRetaining", |visitor| {
                    detail.visit(table, visitor)
                })
            }
            Self::StoppedReleasing => {
                visit_core(table, visitor, "AgentStoppedReleasing", |_| Ok(()))
            }
            Self::AlreadyStopped => {
                visit_core(table, visitor, "AgentStopAlreadyStopped", |_| Ok(()))
            }
            Self::Unavailable => visit_core(table, visitor, "AgentStopUnavailable", |_| Ok(())),
            Self::Unauthorized => visit_core(table, visitor, "AgentStopUnauthorized", |_| Ok(())),
            Self::Failed(detail) => visit_core(table, visitor, "AgentStopFailed", |visitor| {
                detail.visit(table, visitor)
            }),
        }
    }
}

fn visit_usage_observation(
    table: &DataConTable,
    visitor: &mut dyn HaskellVisitor,
    sample: Option<&crate::ProviderUsageSample>,
) -> Result<(), BridgeError> {
    match sample {
        None => Option::<i64>::None.visit(table, visitor),
        Some(sample) => visit_named(table, visitor, "GHC.Maybe", "Just", |visitor| {
            visit_core(table, visitor, "ProviderUsageObservation", |visitor| {
                sample.observation_id.visit(table, visitor)?;
                sample.source_timestamp.visit(table, visitor)?;
                sample.cached_input_tokens.visit(table, visitor)?;
                sample.uncached_input_tokens.visit(table, visitor)
            })
        }),
    }
}

pub(super) fn visit_usage_summary(
    table: &DataConTable,
    visitor: &mut dyn HaskellVisitor,
    summary: Option<&exomonad_model::ProviderUsageSummary>,
) -> Result<(), BridgeError> {
    use exomonad_model::{ProviderUsageCompleteness, ProviderUsageScope};
    match summary {
        None => Option::<i64>::None.visit(table, visitor),
        Some(summary) => {
            if summary.usage.cached_input_tokens > summary.usage.input_tokens {
                return Err(BridgeError::UnsupportedType(
                    "cached input tokens exceed input tokens".into(),
                ));
            }
            let uncached = summary
                .usage
                .input_tokens
                .checked_sub(summary.usage.cached_input_tokens)
                .ok_or_else(|| {
                    BridgeError::UnsupportedType("cached input tokens exceed input tokens".into())
                })?;
            visit_named(table, visitor, "GHC.Maybe", "Just", |visitor| {
                visit_core(table, visitor, "ProviderUsageSummary", |visitor| {
                    match &summary.scope {
                        ProviderUsageScope::Thread(thread) => {
                            visit_core(table, visitor, "UsageThread", |visitor| {
                                thread.visit(table, visitor)
                            })?
                        }
                        ProviderUsageScope::Turn { thread, turn } => {
                            visit_core(table, visitor, "UsageTurn", |visitor| {
                                thread.visit(table, visitor)?;
                                turn.visit(table, visitor)
                            })?
                        }
                    }
                    visit_core(
                        table,
                        visitor,
                        match summary.completeness {
                            ProviderUsageCompleteness::Partial => "UsagePartial",
                            ProviderUsageCompleteness::Complete => "UsageComplete",
                        },
                        |_| Ok(()),
                    )?;
                    summary.observations.visit(table, visitor)?;
                    summary.usage.cached_input_tokens.visit(table, visitor)?;
                    uncached.visit(table, visitor)?;
                    summary.usage.output_tokens.visit(table, visitor)?;
                    summary
                        .usage
                        .reasoning_output_tokens
                        .visit(table, visitor)?;
                    summary.usage.total_tokens.visit(table, visitor)
                })
            })
        }
    }
}

impl tidepool_bridge::sealed::ToHaskellSealed for AgentRosterProjection {}
impl ToHaskell for AgentRosterProjection {
    fn visit(
        &self,
        table: &DataConTable,
        visitor: &mut dyn HaskellVisitor,
    ) -> Result<(), BridgeError> {
        visit_core(table, visitor, "AgentRosterEntry", |visitor| {
            actor_haskell_int(self.actor.id.0, "actor id")?.visit(table, visitor)?;
            actor_haskell_int(self.actor.incarnation.0, "actor incarnation")?
                .visit(table, visitor)?;
            self.descriptor.label().to_owned().visit(table, visitor)?;
            self.runtime
                .requested_model
                .as_deref()
                .or(self.descriptor.model_name())
                .map(str::to_owned)
                .visit(table, visitor)?;
            self.runtime.confirmed_model.visit(table, visitor)?;
            actor_haskell_int(self.received.0, "received count")?.visit(table, visitor)?;
            actor_haskell_int(self.received.1, "received count")?.visit(table, visitor)?;
            self.runtime
                .compactions
                .map(|x| actor_haskell_int(x, "compaction count"))
                .transpose()?
                .visit(table, visitor)?;
            self.descriptor
                .creator()
                .map(|x| actor_haskell_int(x.id.0, "creator id"))
                .transpose()?
                .visit(table, visitor)?;
            self.descriptor
                .creator()
                .map(|x| actor_haskell_int(x.incarnation.0, "creator incarnation"))
                .transpose()?
                .visit(table, visitor)?;
            self.descriptor
                .supervisor_parent()
                .map(|x| actor_haskell_int(x.id.0, "supervisor id"))
                .transpose()?
                .visit(table, visitor)?;
            self.descriptor
                .supervisor_parent()
                .map(|x| actor_haskell_int(x.incarnation.0, "supervisor incarnation"))
                .transpose()?
                .visit(table, visitor)?;
            self.descriptor
                .context_parent()
                .map(|x| actor_haskell_int(x.id.0, "context parent id"))
                .transpose()?
                .visit(table, visitor)?;
            self.descriptor
                .context_parent()
                .map(|x| actor_haskell_int(x.incarnation.0, "context parent incarnation"))
                .transpose()?
                .visit(table, visitor)?;
            visit_agent_roster_state(table, visitor, self.terminal.as_ref())?;
            visit_provider_health(table, visitor, self.runtime.provider_turn.as_ref())?;
            self.runtime
                .provider_turn
                .as_ref()
                .map(|turn| turn.turn.clone())
                .visit(table, visitor)?;
            self.runtime
                .provider_observation_stale
                .visit(table, visitor)?;
            match self.terminal {
                None => visit_named(table, visitor, "GHC.Maybe", "Just", |v| {
                    visit_core(
                        table,
                        v,
                        self.runtime
                            .disposition(!self.requests.0.is_empty() || !self.requests.1.is_empty())
                            .constructor_name(),
                        |_| Ok(()),
                    )
                })?,
                Some(_) => Option::<i64>::None.visit(table, visitor)?,
            }
            self.requests
                .0
                .iter()
                .map(|x| actor_haskell_int(x.0, "request id"))
                .collect::<Result<Vec<_>, _>>()?
                .visit(table, visitor)?;
            self.requests
                .1
                .iter()
                .map(|x| actor_haskell_int(x.0, "request id"))
                .collect::<Result<Vec<_>, _>>()?
                .visit(table, visitor)?;
            self.bound_worktree.visit(table, visitor)?;
            actor_haskell_int(self.descriptor.placement().lexical_scope.0, "lexical scope")?
                .visit(table, visitor)?;
            self.runtime.provider_thread.visit(table, visitor)?;
            self.runtime.provider_parent_thread.visit(table, visitor)?;
            visit_usage_observation(table, visitor, self.runtime.first_provider_usage.as_ref())?;
            visit_usage_observation(table, visitor, self.runtime.latest_provider_usage())?;
            visit_usage_summary(table, visitor, self.runtime.provider_usage_summary.as_ref())?;
            visit_usage_summary(
                table,
                visitor,
                self.runtime.latest_turn_usage_summary.as_ref(),
            )?;
            match self.runtime.latest_provider_usage() {
                None => Option::<i64>::None.visit(table, visitor)?,
                Some(s) => visit_named(table, visitor, "GHC.Maybe", "Just", |v| {
                    visit_core(
                        table,
                        v,
                        match s.cache_boundary {
                            crate::CacheBoundaryReason::Fresh => "CacheFresh",
                            crate::CacheBoundaryReason::ForkedPrefix => "CacheForkedPrefix",
                            crate::CacheBoundaryReason::ReattachedThread => "CacheReattachedThread",
                            crate::CacheBoundaryReason::ProviderUnknown => "CacheProviderUnknown",
                        },
                        |_| Ok(()),
                    )
                })?,
            }
            actor_haskell_int(self.runtime.event_watermark, "event watermark")?
                .visit(table, visitor)?;
            visit_workbench_posture(table, visitor, &self.runtime.workbench_posture)?;
            self.runtime.launched_at_unix_ms.visit(table, visitor)
        })
    }
}

fn visit_workbench_posture(
    table: &DataConTable,
    visitor: &mut dyn HaskellVisitor,
    posture: &crate::ActorWorkbenchPosture,
) -> Result<(), BridgeError> {
    match posture {
        crate::ActorWorkbenchPosture::Idle => {
            visit_core(table, visitor, "WorkbenchIdle", |_| Ok(()))
        }
        crate::ActorWorkbenchPosture::RunningUnit {
            input_unit_index,
            total,
        } => visit_core(table, visitor, "WorkbenchRunningUnit", |v| {
            i64::try_from(*input_unit_index)
                .map_err(|_| {
                    BridgeError::UnsupportedType(
                        "workbench input-unit coordinate exceeds Haskell Int".into(),
                    )
                })?
                .visit(table, v)?;
            i64::try_from(*total)
                .map_err(|_| {
                    BridgeError::UnsupportedType(
                        "workbench input-unit coordinate exceeds Haskell Int".into(),
                    )
                })?
                .visit(table, v)
        }),
        crate::ActorWorkbenchPosture::AwaitingEffect {
            input_unit_index,
            total,
            effect,
        } => visit_core(table, visitor, "WorkbenchAwaitingEffect", |v| {
            i64::try_from(*input_unit_index)
                .map_err(|_| {
                    BridgeError::UnsupportedType(
                        "workbench input-unit coordinate exceeds Haskell Int".into(),
                    )
                })?
                .visit(table, v)?;
            i64::try_from(*total)
                .map_err(|_| {
                    BridgeError::UnsupportedType(
                        "workbench input-unit coordinate exceeds Haskell Int".into(),
                    )
                })?
                .visit(table, v)?;
            effect.visit(table, v)
        }),
        crate::ActorWorkbenchPosture::TerminalTransfer { transfer } => {
            visit_core(table, visitor, "WorkbenchTerminalTransfer", |v| {
                visit_core(
                    table,
                    v,
                    match transfer {
                        crate::ActorWorkbenchTransfer::Reply => "WorkbenchReplyTransfer",
                        crate::ActorWorkbenchTransfer::CancellationAcknowledgement => {
                            "WorkbenchCancellationTransfer"
                        }
                    },
                    |_| Ok(()),
                )
            })
        }
        crate::ActorWorkbenchPosture::Failed => {
            visit_core(table, visitor, "WorkbenchFailed", |_| Ok(()))
        }
    }
}

pub(super) struct ActorContextProjection {
    pub(super) context: crate::ActorSessionContext,
    pub(super) descriptor: crate::ActorDescriptor,
    pub(super) bound_worktree: Option<String>,
    pub(super) runtime: crate::ActorRuntimeObservation,
}

impl tidepool_bridge::sealed::ToHaskellSealed for ActorContextProjection {}
impl ToHaskell for ActorContextProjection {
    fn visit(
        &self,
        table: &DataConTable,
        visitor: &mut dyn HaskellVisitor,
    ) -> Result<(), BridgeError> {
        visit_core(table, visitor, "ActorContextInfo", |v| {
            actor_haskell_int(self.context.actor.id.0, "actor id")?.visit(table, v)?;
            actor_haskell_int(self.context.actor.incarnation.0, "actor incarnation")?
                .visit(table, v)?;
            self.descriptor
                .supervisor_parent()
                .map(|x| actor_haskell_int(x.id.0, "supervisor id"))
                .transpose()?
                .visit(table, v)?;
            self.descriptor
                .supervisor_parent()
                .map(|x| actor_haskell_int(x.incarnation.0, "supervisor incarnation"))
                .transpose()?
                .visit(table, v)?;
            self.descriptor.label().to_owned().visit(table, v)?;
            self.descriptor
                .capabilities()
                .haskell_effects_type()
                .visit(table, v)?;
            self.bound_worktree.visit(table, v)?;
            actor_haskell_int(self.context.placement.lexical_scope.0, "lexical scope")?
                .visit(table, v)?;
            match &self.runtime.activation_kind {
                crate::ActorActivationKind::RootStarted => {
                    visit_core(table, v, "ActivationRootStarted", |_| Ok(()))?
                }
                crate::ActorActivationKind::RequestActivated {
                    request,
                    activation_sequence,
                } => visit_core(table, v, "ActivationRequest", |v| {
                    actor_haskell_int(request.0, "request id")?.visit(table, v)?;
                    actor_haskell_int(*activation_sequence, "activation sequence")?.visit(table, v)
                })?,
                crate::ActorActivationKind::EventsActivated { inbox_sequences } => {
                    visit_core(table, v, "ActivationEvents", |v| {
                        inbox_sequences
                            .iter()
                            .copied()
                            .map(|x| actor_haskell_int(x, "inbox sequence"))
                            .collect::<Result<Vec<_>, _>>()?
                            .visit(table, v)
                    })?
                }
            }
            actor_haskell_int(self.runtime.event_watermark, "event watermark")?.visit(table, v)?;
            self.runtime.provider_thread.visit(table, v)?;
            self.runtime.provider_parent_thread.visit(table, v)?;
            visit_usage_observation(table, v, self.runtime.first_provider_usage.as_ref())?;
            visit_usage_observation(table, v, self.runtime.latest_provider_usage())?;
            visit_usage_summary(table, v, self.runtime.provider_usage_summary.as_ref())?;
            visit_usage_summary(table, v, self.runtime.latest_turn_usage_summary.as_ref())?;
            i64::from(self.descriptor.capabilities().descendants().maximum_depth)
                .visit(table, v)?;
            self.descriptor
                .capabilities()
                .descendants()
                .maximum_active_children
                .map(i64::from)
                .visit(table, v)
        })
    }
}

impl tidepool_bridge::sealed::ToHaskellSealed for SpecReplacementError {}
impl ToHaskell for SpecReplacementError {
    fn visit(
        &self,
        table: &DataConTable,
        visitor: &mut dyn HaskellVisitor,
    ) -> Result<(), BridgeError> {
        let name = match self {
            Self::Unavailable => "SpecReplacementUnavailable",
            Self::Unauthorized => "SpecReplacementUnauthorized",
            Self::SurfaceChanged => "SpecReplacementSurfaceChanged",
            Self::Failed(_) => "SpecReplacementFailed",
        };
        visit_core(table, visitor, name, |visitor| match self {
            Self::Failed(detail) => detail.visit(table, visitor),
            _ => Ok(()),
        })
    }
}
