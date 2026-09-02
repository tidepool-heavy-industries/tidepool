//! Lifecycle facts reported by an actor's attached native application.
//!
//! The local actor kernel consumes these values; deployment adapters merely
//! observe processes and report what happened.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExternalApplicationFailureClass {
    WorktreeBinding,
    CommandConstruction,
    ProcessLaunch,
    ProxyStartup,
    UnexpectedExit,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExternalApplicationFailure {
    pub class: ExternalApplicationFailureClass,
    pub detail: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExternalFailureDisposition {
    Applied,
    AlreadyTerminal,
    UnknownOrStale,
}
