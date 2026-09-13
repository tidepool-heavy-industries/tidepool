//! Runtime custody for validated prepared-STG execution artifacts.
//!
//! Here `prepared` refers to the GHC prepared-STG handoff. It is distinct from
//! cell preparation in `workbench.rs` and `resident_workbench.rs`.
//!
//! Parsing, linking, compiled-owner construction, execution, cancellation, disposition,
//! and retained-program reuse cross this boundary in that order. The legacy
//! `CoreExpr` machine is not a fallback for any operation in this module.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use tidepool_bridge::Value;
use tidepool_codegen::jit_machine::MachineDisposition;
use tidepool_codegen::machine_state::MachineFailure;
use tidepool_codegen::prepared_program::{
    CompileError, CompiledProgram, ExecutionError, RunOptions,
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
    #[error("prepared compilation rejected: {0}")]
    Compile(CompileError),
    #[error("prepared execution failed: {0}")]
    Run(ExecutionError),
    #[error("prepared runtime is unavailable after an integrity failure")]
    Unavailable(MachineFailure),
}

impl PreparedRuntimeError {
    #[must_use]
    pub fn kind(&self) -> PreparedFailureKind {
        match self {
            Self::Parse(_) | Self::Link(_) => PreparedFailureKind::Rejected,
            Self::Cancelled => PreparedFailureKind::Cancelled,
            Self::Unavailable(_) => PreparedFailureKind::Integrity,
            Self::Compile(_) => PreparedFailureKind::Rejected,
            Self::Run(error) => match error {
                ExecutionError::MissingEntry(_)
                | ExecutionError::Unsupported(_)
                | ExecutionError::Arguments { .. } => PreparedFailureKind::Rejected,
                ExecutionError::Runtime(failure) => {
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
                ExecutionError::Observation(_) | ExecutionError::Static(_) => {
                    PreparedFailureKind::Language
                }
            },
        }
    }
}

#[derive(Debug)]
pub struct PreparedRunResult {
    pub values: Vec<Value>,
    pub collections: u64,
}

/// A linked program and its lazily compiled owner retained across entries with
/// a monotonic reuse decision.
pub struct PreparedRuntime {
    linked: LinkedProgram,
    compiled: Option<CompiledProgram>,
    terminal: Option<MachineFailure>,
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
            compiled: None,
            terminal: None,
        })
    }

    #[must_use]
    pub fn disposition(&self) -> MachineDisposition {
        self.terminal
            .as_ref()
            .map_or(MachineDisposition::Reusable, |failure| failure.disposition)
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
        if let Some(failure) = &self.terminal {
            return Err(PreparedRuntimeError::Unavailable(failure.clone()));
        }
        if cancel.is_cancelled() {
            return Err(PreparedRuntimeError::Cancelled);
        }
        if self.compiled.is_none() {
            self.compiled = Some(
                CompiledProgram::compile(&self.linked).map_err(PreparedRuntimeError::Compile)?,
            );
        }
        if cancel.is_cancelled() {
            return Err(PreparedRuntimeError::Cancelled);
        }
        let entry = binding.unwrap_or_else(|| self.linked.prepared().entry());
        let result = self
            .compiled
            .as_ref()
            .expect("compiled program installed above")
            .run_entry(
                entry,
                arguments,
                &RunOptions {
                    collect_before_observation: collect,
                    ..RunOptions::default()
                },
                Arc::clone(&cancel.0),
            )
            .map_err(|error| self.classify_execution(error))?;
        if cancel.is_cancelled() {
            return Err(PreparedRuntimeError::Cancelled);
        }
        Ok(PreparedRunResult {
            values: result.values,
            collections: result.collections,
        })
    }

    fn classify_execution(&mut self, error: ExecutionError) -> PreparedRuntimeError {
        let terminal = terminal_failure(&error);
        let error = PreparedRuntimeError::Run(error);
        if self.terminal.is_none() {
            if let Some(failure) = terminal {
                self.terminal = Some(failure);
            }
        }
        error
    }
}

fn terminal_failure(error: &ExecutionError) -> Option<MachineFailure> {
    let ExecutionError::Runtime(failure) = error else {
        return None;
    };
    (failure.disposition == MachineDisposition::Unavailable).then(|| failure.clone())
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
    use tidepool_repr::execution_schema::{
        Architecture, Endianness, ImportedValue, TargetDescriptor, EXECUTION_ABI_VERSION,
        SCHEMA_VERSION,
    };

    fn m3_runtime() -> PreparedRuntime {
        const ARTIFACT: &[u8] =
            include_bytes!("../../../haskell/test-prepared-stg/fixtures/m3-vertical.cbor");
        let requirements = ProgramRequirements {
            schema_version: SCHEMA_VERSION,
            projection_profile: "ghc-9.12-prepared-stg".into(),
            toolchain: "ghc-9.12.2".into(),
            execution_abi_version: EXECUTION_ABI_VERSION,
            target: TargetDescriptor {
                architecture: Architecture::X86_64,
                endianness: Endianness::Little,
                pointer_width: 64,
                word_width: 64,
                abi: "sysv64".into(),
                features: vec![],
            },
        };
        let prepared = parse_program(ARTIFACT, &requirements, DecodeLimits::default()).unwrap();
        let imports = MachineImports {
            values: prepared
                .globals()
                .iter()
                .map(|global| {
                    let value = ImportedValue {
                        identity: global.identity.clone(),
                        rep: global.rep,
                        entry_signature: global
                            .entry_signature
                            .map(|id| prepared.signatures()[id.0 as usize].clone()),
                        dead_end: global.dead_end,
                        evaluated: global.required_evaluated,
                        generation: global.required_generation.unwrap_or(0),
                    };
                    (value.identity.clone(), value)
                })
                .collect(),
        };
        PreparedRuntime::from_artifact(ARTIFACT, &requirements, DecodeLimits::default(), imports)
            .unwrap()
    }

    #[test]
    fn compiled_failure_retains_disposition_separately_from_first_cause() {
        let failure = MachineFailure {
            cause: RuntimeError::Cancelled,
            disposition: MachineDisposition::Unavailable,
        };
        let error = PreparedRuntimeError::Unavailable(failure.clone());
        assert_eq!(error.kind(), PreparedFailureKind::Integrity);
        assert!(matches!(
            error,
            PreparedRuntimeError::Unavailable(retained) if retained == failure
        ));
        assert_eq!(
            terminal_failure(&ExecutionError::Runtime(failure.clone())),
            Some(failure)
        );
    }

    #[test]
    fn terminal_failure_is_replayed_before_cancellation() {
        let mut runtime = m3_runtime();
        let failure = MachineFailure {
            cause: RuntimeError::Cancelled,
            disposition: MachineDisposition::Unavailable,
        };
        let reported = runtime.classify_execution(ExecutionError::Runtime(failure.clone()));
        assert!(matches!(
            reported,
            PreparedRuntimeError::Run(ExecutionError::Runtime(retained))
                if retained == failure
        ));

        let cancel = runtime.new_cancel_handle();
        cancel.cancel();
        let replayed = runtime.run_entry(None, &[], false, &cancel).unwrap_err();
        assert!(matches!(
            replayed,
            PreparedRuntimeError::Unavailable(retained) if retained == failure
        ));
    }

    #[test]
    fn compiled_language_and_cancellation_are_not_integrity_failures() {
        for (cause, expected) in [
            (RuntimeError::Cancelled, PreparedFailureKind::Cancelled),
            (RuntimeError::HeapOverflow, PreparedFailureKind::Language),
            (RuntimeError::DivisionByZero, PreparedFailureKind::Language),
        ] {
            let error = PreparedRuntimeError::Run(ExecutionError::Runtime(MachineFailure {
                cause,
                disposition: MachineDisposition::Reusable,
            }));
            assert_eq!(error.kind(), expected);
        }
    }
}
