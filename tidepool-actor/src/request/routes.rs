//! Watch-owned Haskell continuations. Readiness enqueues work on the exact
//! existing actor; the request registry retains status and root custody.

use super::{
    ReplyError, RequestRegistry, WatchId, WatchNotification, WatchStateProjection, WatchTransition,
};
use crate::{ActorRef, LocalActorRef};
use tidepool_runtime::session::RootCustody;

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
}

impl WatchRoute {
    pub(crate) fn new(owner: LocalActorRef, entry: RootCustody) -> Self {
        Self {
            owner,
            entry: Some(entry),
            state: RouteState::Waiting,
        }
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
    pub(crate) fn take_route(&self, owner: ActorRef, watch: WatchId) -> Option<RootCustody> {
        let mut state = self.state.lock();
        let record = state.watches.get_mut(&watch)?;
        if record.owner != owner {
            return None;
        }
        let route = record.route.as_mut()?;
        if route.state != RouteState::Running {
            return None;
        }
        route.entry.take()
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
        owner: ActorRef,
        watch: WatchId,
    ) -> Result<RouteState, ReplyError> {
        let state = self.state.lock();
        let record = state.watches.get(&watch).ok_or(ReplyError::Stale)?;
        if record.owner != owner {
            return Err(super::identity_error(record.owner, owner));
        }
        record
            .route
            .as_ref()
            .map(|route| route.state.clone())
            .ok_or(ReplyError::Stale)
    }
}
