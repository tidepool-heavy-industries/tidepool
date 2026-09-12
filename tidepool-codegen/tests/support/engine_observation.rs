//! Private M0 vocabulary for independent engine observations.
//!
//! This stays beside its concrete consumer (`engine_review.rs`). It deliberately
//! does not define the future prepared-program schema or ABI.

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EngineOutcome<Success, CompileFailure, RuntimeFailure, HarnessFailure> {
    Success(Success),
    LanguageRejected(CompileFailure),
    CompilerFailure(CompileFailure),
    RuntimeError(RuntimeFailure),
    Cancelled(RuntimeFailure),
    IntegrityFailure(RuntimeFailure),
    OperationalFailure(RuntimeFailure),
    UnsupportedInput(CompileFailure),
    Timeout,
    NativeFault { signal: i32 },
    HarnessError(HarnessFailure),
}

impl<Success, CompileFailure, RuntimeFailure, HarnessFailure>
    EngineOutcome<Success, CompileFailure, RuntimeFailure, HarnessFailure>
{
    pub fn map_success<Mapped>(
        self,
        map: impl FnOnce(Success) -> Mapped,
    ) -> EngineOutcome<Mapped, CompileFailure, RuntimeFailure, HarnessFailure> {
        match self {
            Self::Success(value) => EngineOutcome::Success(map(value)),
            Self::LanguageRejected(error) => EngineOutcome::LanguageRejected(error),
            Self::CompilerFailure(error) => EngineOutcome::CompilerFailure(error),
            Self::RuntimeError(error) => EngineOutcome::RuntimeError(error),
            Self::Cancelled(error) => EngineOutcome::Cancelled(error),
            Self::IntegrityFailure(error) => EngineOutcome::IntegrityFailure(error),
            Self::OperationalFailure(error) => EngineOutcome::OperationalFailure(error),
            Self::UnsupportedInput(error) => EngineOutcome::UnsupportedInput(error),
            Self::Timeout => EngineOutcome::Timeout,
            Self::NativeFault { signal } => EngineOutcome::NativeFault { signal },
            Self::HarnessError(error) => EngineOutcome::HarnessError(error),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SupportCategory {
    Expression,
    RightHandSide,
    Binding,
    Alternative,
    UpdatePolicy,
    RuntimeRepresentation,
    Primitive,
    ResidentValue,
    Effect,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SupportStatus {
    Verified,
    Unverified,
    Unsupported,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SupportItem {
    pub category: SupportCategory,
    pub name: &'static str,
    pub status: SupportStatus,
    pub evidence: &'static str,
}
