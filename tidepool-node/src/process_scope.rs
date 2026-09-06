//! Opt-in namespace lifetime ownership for native actor services.
//!
//! A direct monitor wait is not namespace drain. Only the checked namespace
//! init witness may establish drain, and cleanup requires both observations.
//! This scaffold intentionally rejects spawning until identity and gate
//! ownership are implemented and reviewed; legacy mount wrapping is unchanged.

use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;
use std::process::ExitStatus;
use std::time::Instant;

use super::{ProcessInvocation, ProcessMountBoundary};

#[derive(Debug, thiserror::Error)]
pub enum ServiceScopeError {
    #[error("service scope executable must be absolute")]
    ExecutableNotAbsolute,
    #[error("service scope is unsupported: {0}")]
    Unsupported(&'static str),
    #[error("service process was not spawned: {0}")]
    NotSpawned(#[source] std::io::Error),
    #[error("service init identity could not be established: {0}")]
    IdentityUnconfirmed(String),
    #[error("service scope operation is invalid in its current phase")]
    WrongPhase,
    #[error("service scope cleanup is unconfirmed: {0}")]
    CleanupUnconfirmed(String),
    #[error("service scope io: {0}")]
    Io(#[from] std::io::Error),
}

/// Host-resolved environment changes. No model/provider policy is decided here.
#[derive(Debug, Default)]
pub struct ServiceEnvironment {
    pub set: BTreeMap<String, String>,
    pub unset: BTreeSet<String>,
}

/// Validated mount intent, with no process or externally submitted command yet.
pub struct PreparedServiceScope {
    boundary: ProcessMountBoundary,
    bubblewrap: PathBuf,
    command: ProcessInvocation,
}

impl PreparedServiceScope {
    pub(super) fn new(
        boundary: ProcessMountBoundary,
        bubblewrap: PathBuf,
        command: ProcessInvocation,
    ) -> Result<Self, ServiceScopeError> {
        if !bubblewrap.is_absolute() {
            return Err(ServiceScopeError::ExecutableNotAbsolute);
        }
        Ok(Self {
            boundary,
            bubblewrap,
            command,
        })
    }

    /// Create a blocked launch. Errors here mean no process was submitted.
    /// Pinning and release are separate operations on the retained owner.
    pub fn spawn(
        self,
        _environment: ServiceEnvironment,
        _output: std::fs::File,
    ) -> Result<ServiceScope, ServiceScopeError> {
        let Self {
            boundary,
            bubblewrap,
            command,
        } = self;
        let _unsubmitted = (boundary, bubblewrap, command);
        Err(ServiceScopeError::Unsupported(
            "namespace identity/gate implementation pending",
        ))
    }
}

/// Noncloneable process owner. Operations borrow it so failure cannot consume
/// the only monitor/init/gate custody. Constructors are private to spawn().
pub struct ServiceScope {
    _private: (),
}

impl ServiceScope {
    pub fn pin_init(&mut self, _deadline: Instant) -> Result<(), ServiceScopeError> {
        Err(ServiceScopeError::Unsupported(
            "namespace identity implementation pending",
        ))
    }

    pub fn release_command(&mut self) -> Result<(), ServiceScopeError> {
        Err(ServiceScopeError::WrongPhase)
    }

    pub fn terminate_and_wait(
        &mut self,
        _deadline: Instant,
    ) -> Result<ServiceScopeCleanup, ServiceScopeError> {
        Err(ServiceScopeError::CleanupUnconfirmed(
            "no implemented lifetime witness".into(),
        ))
    }
}

/// Both exact namespace drain and direct monitor wait. This is not a receipt
/// for remote work, host-side Haskell tasks, or arbitrary external effects.
#[derive(Debug)]
pub struct ServiceScopeCleanup {
    monitor_status: ExitStatus,
}

impl ServiceScopeCleanup {
    pub fn monitor_status(&self) -> ExitStatus {
        self.monitor_status
    }
}
