#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(i64)]
pub enum CallStatus {
    Success = 0,
    LanguageFailure = 1,
    IntegrityFailure = 2,
    Cancelled = 3,
}

/// Prepared safepoint identities are shared by emission and deterministic
/// settlement tests. They describe where cancellation is sampled, not its
/// cause or the machine's reusable/terminal disposition.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u32)]
pub(crate) enum PreparedSafepoint {
    Allocation = 0,
    FunctionEntry = 1,
    Backedge = 2,
    ThunkEntry = 3,
    ThunkCommit = 4,
}

impl PreparedSafepoint {
    pub(crate) fn from_raw(raw: u32) -> Option<Self> {
        match raw {
            0 => Some(Self::Allocation),
            1 => Some(Self::FunctionEntry),
            2 => Some(Self::Backedge),
            3 => Some(Self::ThunkEntry),
            4 => Some(Self::ThunkCommit),
            _ => None,
        }
    }
}

impl CallStatus {
    pub fn from_raw(raw: i64) -> Result<Self, ControlError> {
        match raw {
            0 => Ok(Self::Success),
            1 => Ok(Self::LanguageFailure),
            2 => Ok(Self::IntegrityFailure),
            3 => Ok(Self::Cancelled),
            other => Err(ControlError::UnknownStatus(other)),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
pub enum ControlError {
    #[error("unknown native call status {0}")]
    UnknownStatus(i64),
}
