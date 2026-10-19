//! Watch-owned Haskell continuations. Readiness enqueues work on the exact
//! existing actor; the request registry retains status and the root handle.

use super::sources::CallbackExecution;
use super::{
    ReplyError, RequestRegistry, WatchId, WatchNotification, WatchStateProjection, WatchTransition,
};
use crate::resident_workbench::generation_admission::GenerationAdmissionError;
use crate::{ActorRef, LocalActorRef};
use tidepool_runtime::session::RootCustody;

#[derive(Debug)]
pub(crate) enum RouteRegistrationError {
    Source(GenerationAdmissionError),
    Watch(ReplyError),
}

/// The registry issues this affine entry only when activation wins its gate.
pub(crate) struct RouteEntry {
    entry: RootCustody,
    execution: CallbackExecution,
}

impl RouteEntry {
    pub(crate) fn execution(&self) -> &CallbackExecution {
        &self.execution
    }

    pub(crate) fn into_parts(self) -> (RootCustody, CallbackExecution) {
        (self.entry, self.execution)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum RouteState {
    Waiting,
    Running,
    Completed,
    Failed(String),
}

pub(crate) struct WatchRoute {
    owner: LocalActorRef,
    entry: Option<RootCustody>,
    state: RouteState,
    execution: CallbackExecution,
}

impl WatchRoute {
    pub(super) fn transfer_owner(&mut self, successor: LocalActorRef, watch: WatchId) {
        self.owner = successor;
        // A queued old notice cannot run after its owner retires. Requeue the
        // still-owned entry; take_route settles duplicate notices without
        // invoking the handler twice.
        if self.state == RouteState::Running && self.entry.is_some() {
            if let Err(error) = self
                .owner
                .address()
                .send_message(crate::KernelMessage::RouteReady { watch })
            {
                self.state =
                    RouteState::Failed(format!("replacement route owner is unavailable: {error}"));
            }
        }
    }
    pub(crate) fn new(
        owner: LocalActorRef,
        entry: RootCustody,
        execution: CallbackExecution,
    ) -> Self {
        Self {
            owner,
            entry: Some(entry),
            state: RouteState::Waiting,
            execution,
        }
    }

    pub(super) fn execution(&self) -> &CallbackExecution {
        &self.execution
    }

    pub(super) fn schedule(&mut self, watch: WatchId) {
        if self.state != RouteState::Waiting {
            return;
        }
        self.state = RouteState::Running;
        if let Err(error) = self
            .owner
            .address()
            .send_message(crate::KernelMessage::RouteReady { watch })
        {
            self.entry = None;
            self.state = RouteState::Failed(format!("route owner is unavailable: {error}"));
        }
    }

    pub(super) fn is_active(&self) -> bool {
        matches!(self.state, RouteState::Waiting | RouteState::Running)
    }

    pub(super) fn retire(&mut self) {
        if self.is_active() {
            self.entry = None;
            self.state = RouteState::Failed("route owner stopped".into());
        }
    }
}

impl RequestRegistry {
    pub(crate) fn register_watch_route(
        &self,
        owner: ActorRef,
        label: String,
        plan: super::readiness::Plan<super::ReadinessDependency>,
        route: WatchRoute,
    ) -> Result<(WatchId, Vec<WatchNotification>), RouteRegistrationError> {
        if route.owner.identity() != owner {
            return Err(RouteRegistrationError::Watch(ReplyError::Unauthorized));
        }
        let execution = route.execution().clone();
        execution
            .with_activation_admission(|| {
                self.register_watch_plan_owned(owner, label, plan, Some(route), false)
            })
            .map_err(RouteRegistrationError::Source)?
            .map_err(RouteRegistrationError::Watch)
    }

    pub(crate) fn take_route(
        &self,
        owner: ActorRef,
        watch: WatchId,
    ) -> Result<Option<RouteEntry>, GenerationAdmissionError> {
        let execution = {
            let state = self.state.lock();
            let Some(route) = state
                .watches
                .get(&watch)
                .filter(|record| record.owner == owner)
                .and_then(|record| record.route.as_ref())
                .filter(|route| route.state == RouteState::Running && route.entry.is_some())
            else {
                return Ok(None);
            };
            route.execution.clone()
        };
        let admitted = execution.with_activation_admission(|| {
            let mut state = self.state.lock();
            let route = state
                .watches
                .get_mut(&watch)
                .filter(|record| record.owner == owner)
                .and_then(|record| record.route.as_mut())
                .filter(|route| route.state == RouteState::Running)?;
            route.entry.take().map(|entry| RouteEntry {
                entry,
                execution: route.execution.clone(),
            })
        });
        match admitted {
            Ok(entry) => Ok(entry),
            Err(error) => {
                // A duplicate notice may lose to an already admitted callback.
                // Only consume an entry that is still waiting to start; never
                // fail or reactivate work whose root was already taken.
                let mut state = self.state.lock();
                let Some(route) = state
                    .watches
                    .get_mut(&watch)
                    .filter(|record| record.owner == owner)
                    .and_then(|record| record.route.as_mut())
                    .filter(|route| route.state == RouteState::Running && route.entry.is_some())
                else {
                    return Ok(None);
                };
                drop(route.entry.take());
                Err(error)
            }
        }
    }

    pub(crate) fn finish_route(
        &self,
        owner: ActorRef,
        watch: WatchId,
        outcome: Result<(), String>,
    ) -> Option<WatchNotification> {
        let mut state = self.state.lock();
        let record = state
            .watches
            .get_mut(&watch)
            .filter(|record| record.owner == owner)?;
        let route = record
            .route
            .as_mut()
            .filter(|route| route.state == RouteState::Running)?;
        let detail = match outcome {
            Ok(()) => {
                route.state = RouteState::Completed;
                return None;
            }
            Err(error) => {
                route.state = RouteState::Failed(error.clone());
                error
            }
        };
        let label = record.label.clone();
        let sequence = super::next_event_sequence(&mut state, owner);
        Some(WatchNotification {
            owner,
            watch,
            label,
            previous: WatchStateProjection::RouteRunning,
            current: WatchStateProjection::RouteFailed {
                detail: detail.clone(),
            },
            transition: WatchTransition::RouteFailed { detail },
            occurred_at_unix_ms: super::unix_time_ms(),
            sequence,
            watermark: sequence,
        })
    }

    pub(crate) fn list_routes(&self, owner: ActorRef) -> Vec<WatchId> {
        let state = self.state.lock();
        let mut routes = state
            .watches
            .iter()
            .filter(|(_, record)| record.owner == owner && record.route.is_some())
            .map(|(id, _)| *id)
            .collect::<Vec<_>>();
        routes.sort_by_key(|id| id.0);
        routes
    }

    pub(crate) fn observe_route(
        &self,
        _owner: ActorRef,
        watch: WatchId,
    ) -> Result<RouteState, ReplyError> {
        let state = self.state.lock();
        let record = state.watches.get(&watch).ok_or(ReplyError::Stale)?;
        record
            .route
            .as_ref()
            .map(|route| route.state.clone())
            .ok_or(ReplyError::Stale)
    }
}
