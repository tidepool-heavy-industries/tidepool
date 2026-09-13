//! Runtime custody for validated prepared-STG execution artifacts.
//!
//! Here `prepared` refers to the GHC prepared-STG handoff. It is distinct from
//! cell preparation in `workbench.rs` and `resident_workbench.rs`.
//!
//! Parsing, linking, native compilation, execution, cancellation, disposition,
//! and retained-program reuse cross this boundary in that order. The legacy
//! `CoreExpr` machine is not a fallback for any operation in this module.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use tidepool_codegen::jit_machine::MachineDisposition;
use tidepool_codegen::prepared_control::{CallStatus, ControlError};
use tidepool_codegen::prepared_native::{
    CollectionEvidence, NativeConstructor, PreparedNativeError, PreparedNativeProgram,
};
use tidepool_repr::execution_schema::{
    link_program, parse_program, DecodeLimits, LinkError, LinkedProgram, MachineImports,
    ParseError, ProgramRequirements, ValueId,
};

#[derive(Clone, Debug, Default)]
pub struct PreparedCancelHandle(Arc<AtomicBool>);

impl PreparedCancelHandle {
    pub fn cancel(&self) {
        self.0.store(true, Ordering::Release);
    }

    #[must_use]
    pub fn is_cancelled(&self) -> bool {
        self.0.load(Ordering::Acquire)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PreparedFailureKind {
    Rejected,
    Language,
    Cancelled,
    Integrity,
}

#[derive(Debug, thiserror::Error)]
pub enum PreparedRuntimeError {
    #[error(transparent)]
    Parse(#[from] ParseError),
    #[error(transparent)]
    Link(#[from] LinkError),
    #[error("prepared execution cancelled")]
    Cancelled,
    #[error("prepared native compilation rejected: {0}")]
    NativeCompile(PreparedNativeError),
    #[error("prepared native execution failed: {0}")]
    NativeRun(PreparedNativeError),
    #[error("prepared runtime is unavailable after an integrity failure")]
    Unavailable,
}

impl PreparedRuntimeError {
    #[must_use]
    pub fn kind(&self) -> PreparedFailureKind {
        match self {
            Self::Parse(_) | Self::Link(_) => PreparedFailureKind::Rejected,
            Self::Cancelled => PreparedFailureKind::Cancelled,
            Self::Unavailable => PreparedFailureKind::Integrity,
            Self::NativeCompile(_) => PreparedFailureKind::Rejected,
            Self::NativeRun(error) => match error {
                PreparedNativeError::MissingBinding(_)
                | PreparedNativeError::Unsupported(_)
                | PreparedNativeError::Constructor(_)
                | PreparedNativeError::Arguments { .. } => PreparedFailureKind::Rejected,
                PreparedNativeError::Pipeline(_) => PreparedFailureKind::Rejected,
                PreparedNativeError::Runtime(failure) => {
                    if failure.disposition == MachineDisposition::Unavailable {
                        PreparedFailureKind::Integrity
                    } else if matches!(
                        failure.cause,
                        tidepool_codegen::host_fns::RuntimeError::Cancelled
                    ) {
                        PreparedFailureKind::Cancelled
                    } else {
                        PreparedFailureKind::Language
                    }
                }
                PreparedNativeError::Control(ControlError::CallFailed(
                    CallStatus::LanguageFailure,
                )) => PreparedFailureKind::Language,
                PreparedNativeError::Control(ControlError::CallFailed(CallStatus::Cancelled)) => {
                    PreparedFailureKind::Cancelled
                }
                PreparedNativeError::ResultArea
                | PreparedNativeError::Descriptor(_)
                | PreparedNativeError::Control(_) => PreparedFailureKind::Integrity,
            },
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PreparedRunResult {
    pub value: NativeConstructor,
    pub collection: Option<CollectionEvidence>,
}

/// A linked program retained across entries with a monotonic reuse decision.
pub struct PreparedRuntime {
    linked: LinkedProgram,
    disposition: MachineDisposition,
}

impl PreparedRuntime {
    pub fn from_artifact(
        artifact: &[u8],
        requirements: &ProgramRequirements,
        limits: DecodeLimits,
        imports: MachineImports,
    ) -> Result<Self, PreparedRuntimeError> {
        let prepared = parse_program(artifact, requirements, limits)?;
        let linked = link_program(prepared, &imports)?;
        Ok(Self {
            linked,
            disposition: MachineDisposition::Reusable,
        })
    }

    #[must_use]
    pub fn disposition(&self) -> MachineDisposition {
        self.disposition
    }

    #[must_use]
    pub fn new_cancel_handle(&self) -> PreparedCancelHandle {
        PreparedCancelHandle::default()
    }

    pub fn run_entry(
        &mut self,
        binding: Option<ValueId>,
        arguments: &[u64],
        collect: bool,
        cancel: &PreparedCancelHandle,
    ) -> Result<PreparedRunResult, PreparedRuntimeError> {
        if self.disposition == MachineDisposition::Unavailable {
            return Err(PreparedRuntimeError::Unavailable);
        }
        if cancel.is_cancelled() {
            return Err(PreparedRuntimeError::Cancelled);
        }
        let native = match binding {
            Some(binding) => PreparedNativeProgram::compile_binding(&self.linked, binding),
            None => PreparedNativeProgram::compile(&self.linked),
        }
        .map_err(PreparedRuntimeError::NativeCompile)?;
        if cancel.is_cancelled() {
            return Err(PreparedRuntimeError::Cancelled);
        }
        let result = if collect {
            let evidence = native
                .execute_after_collection(arguments)
                .map_err(|error| self.classify_execution(error))?;
            PreparedRunResult {
                value: evidence.result.clone(),
                collection: Some(evidence),
            }
        } else {
            PreparedRunResult {
                value: native
                    .execute_with_arguments(arguments)
                    .map_err(|error| self.classify_execution(error))?,
                collection: None,
            }
        };
        if cancel.is_cancelled() {
            return Err(PreparedRuntimeError::Cancelled);
        }
        Ok(result)
    }

    fn classify_execution(&mut self, error: PreparedNativeError) -> PreparedRuntimeError {
        let error = PreparedRuntimeError::NativeRun(error);
        if error.kind() == PreparedFailureKind::Integrity {
            self.disposition = MachineDisposition::Unavailable;
        }
        error
    }
}

pub fn run_prepared_once(
    artifact: &[u8],
    requirements: &ProgramRequirements,
    limits: DecodeLimits,
    imports: MachineImports,
    cancel: &PreparedCancelHandle,
) -> Result<PreparedRunResult, PreparedRuntimeError> {
    if cancel.is_cancelled() {
        return Err(PreparedRuntimeError::Cancelled);
    }
    let mut runtime = PreparedRuntime::from_artifact(artifact, requirements, limits, imports)?;
    runtime.run_entry(None, &[], true, cancel)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tidepool_codegen::host_fns::RuntimeError;
    use tidepool_codegen::machine_state::MachineFailure;

    #[test]
    fn native_failure_retains_disposition_separately_from_first_cause() {
        let error = PreparedRuntimeError::NativeRun(PreparedNativeError::Runtime(MachineFailure {
            cause: RuntimeError::Cancelled,
            disposition: MachineDisposition::Unavailable,
        }));
        assert_eq!(error.kind(), PreparedFailureKind::Integrity);
    }

    #[test]
    fn native_language_and_cancellation_are_not_integrity_failures() {
        for (cause, expected) in [
            (RuntimeError::Cancelled, PreparedFailureKind::Cancelled),
            (RuntimeError::HeapOverflow, PreparedFailureKind::Language),
            (RuntimeError::DivisionByZero, PreparedFailureKind::Language),
        ] {
            let error =
                PreparedRuntimeError::NativeRun(PreparedNativeError::Runtime(MachineFailure {
                    cause,
                    disposition: MachineDisposition::Reusable,
                }));
            assert_eq!(error.kind(), expected);
        }
    }
}
