#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(i64)]
pub enum CallStatus {
    Success = 0,
    LanguageFailure = 1,
    IntegrityFailure = 2,
    Cancelled = 3,
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
