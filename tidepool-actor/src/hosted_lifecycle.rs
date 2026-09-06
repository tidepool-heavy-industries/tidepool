//! Exact actor-owned admission and cleanup observations. Neither observation
//! certifies native processes, HTTP requests, or arbitrary external handlers.
use crate::{ActorRef, ActorTerminal};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HostedWorkSeal {
    pub(crate) actor: ActorRef,
}
impl HostedWorkSeal {
    pub fn actor(&self) -> ActorRef {
        self.actor
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CleanupComponentOutcome {
    Confirmed,
    Unsupported,
    Unconfirmed(String),
}

/// Only lifecycle owners construct this evidence after closing completion
/// admission and accounting for the resulting children.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResidentCleanupOutcome {
    pub(crate) actor: ActorRef,
    pub(crate) hook: CleanupComponentOutcome,
    pub(crate) realm: CleanupComponentOutcome,
    pub(crate) children: CleanupComponentOutcome,
}
impl ResidentCleanupOutcome {
    pub fn actor(&self) -> ActorRef {
        self.actor
    }
    pub fn hook(&self) -> &CleanupComponentOutcome {
        &self.hook
    }
    pub fn realm(&self) -> &CleanupComponentOutcome {
        &self.realm
    }
    pub fn children(&self) -> &CleanupComponentOutcome {
        &self.children
    }
    pub fn is_confirmed(&self) -> bool {
        [&self.hook, &self.realm, &self.children]
            .into_iter()
            .all(|v| matches!(v, CleanupComponentOutcome::Confirmed))
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResidentShutdown {
    pub terminal: ActorTerminal,
    pub cleanup: ResidentCleanupOutcome,
}
