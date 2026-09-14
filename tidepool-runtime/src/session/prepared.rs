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
    CompileError, CompiledProgram, ExecutionError, PreparedCallOptions, PreparedHandle,
    PreparedInput, PreparedMachine, PreparedMachineOptions, PreparedOuter as CodegenPreparedOuter,
    PreparedResult, PreparedResultBatch, ProgramId, RunOptions, TopSlotBase,
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
    Link(Box<LinkError>),
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
                | ExecutionError::Arguments { .. }
                | ExecutionError::ArgumentRepresentation { .. }
                | ExecutionError::UnknownPreparedHandle
                | ExecutionError::UnknownProgram(_)
                | ExecutionError::TopTableExhausted { .. }
                | ExecutionError::TopSlotBaseMismatch { .. } => PreparedFailureKind::Rejected,
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

impl From<LinkError> for PreparedRuntimeError {
    fn from(error: LinkError) -> Self {
        Self::Link(Box::new(error))
    }
}

#[derive(Debug)]
pub struct PreparedRunResult {
    pub values: Vec<Value>,
    pub collections: u64,
}

/// An opaque value retained by one [`PreparedRuntime`].
///
/// The value is intentionally linear at the runtime boundary: pass and
/// inspect it by borrow, then consume it with [`PreparedRuntime::release`].
/// Its codegen root never escapes this wrapper.
pub struct PreparedValue(PreparedHandle);

pub enum PreparedArgument<'a> {
    Scalar(u64),
    Managed(&'a PreparedValue),
}

pub enum PreparedValueResult {
    Void,
    Scalar(u64),
    Managed(PreparedValue),
}

pub struct PreparedRetainedResult {
    pub values: Vec<PreparedValueResult>,
    pub collections: u64,
}

/// One constructor layer of a retained value, read without forcing children.
pub enum PreparedOuter {
    Constructor {
        identity: tidepool_repr::DataConId,
        fields: Vec<PreparedValueResult>,
    },
}

/// A linked program and its lazily compiled owner retained across entries with
/// a monotonic reuse decision.
pub struct PreparedRuntime {
    linked: LinkedProgram,
    /// Set together: a machine is installed with exactly one program (this
    /// runtime is still single-program), so its id lives alongside it rather
    /// than as a second, independently-optional field.
    machine: Option<(PreparedMachine<'static>, ProgramId)>,
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
            machine: None,
        })
    }

    #[must_use]
    pub fn disposition(&self) -> MachineDisposition {
        self.machine
            .as_ref()
            .map_or(MachineDisposition::Reusable, |(machine, _)| {
                machine.disposition()
            })
    }

    #[must_use]
    pub fn new_cancel_handle(&self) -> PreparedCancelHandle {
        PreparedCancelHandle::default()
    }

    /// Number of `PreparedValue`s this runtime's machine currently retains.
    /// Diagnostic surface for confirming a caller released every value it
    /// produced (e.g. through a resume loop); zero before any machine is
    /// installed.
    #[must_use]
    pub fn retained_handle_count(&self) -> usize {
        self.machine
            .as_ref()
            .map_or(0, PreparedMachine::handle_count)
    }

    pub fn run_entry(
        &mut self,
        binding: Option<ValueId>,
        arguments: &[u64],
        collect: bool,
        cancel: &PreparedCancelHandle,
    ) -> Result<PreparedRunResult, PreparedRuntimeError> {
        self.run_entry_with_completion_hook(binding, arguments, collect, cancel, || {})
    }

    /// Execute with scalar or borrowed retained arguments and retain managed
    /// results under this runtime's machine owner.
    pub fn run_entry_retained(
        &mut self,
        binding: Option<ValueId>,
        arguments: &[PreparedArgument<'_>],
        collect: bool,
        cancel: &PreparedCancelHandle,
    ) -> Result<PreparedRetainedResult, PreparedRuntimeError> {
        self.ensure_available()?;
        if cancel.is_cancelled() {
            return Err(PreparedRuntimeError::Cancelled);
        }
        let mut lowered = Vec::new();
        lowered.try_reserve_exact(arguments.len()).map_err(|_| {
            PreparedRuntimeError::Run(ExecutionError::Runtime(MachineFailure {
                cause: tidepool_codegen::host_fns::RuntimeError::HeapOverflow,
                disposition: MachineDisposition::Reusable,
            }))
        })?;
        for argument in arguments {
            lowered.push(match argument {
                PreparedArgument::Scalar(word) => PreparedInput::Scalar(*word),
                PreparedArgument::Managed(value) => PreparedInput::Managed(value.0),
            });
        }
        let entry = binding.unwrap_or_else(|| self.linked.prepared().entry());
        let (machine, program) = self.ensure_machine()?;
        if cancel.is_cancelled() {
            return Err(PreparedRuntimeError::Cancelled);
        }
        let result = machine
            .run_entry_retained(
                program,
                entry,
                &lowered,
                PreparedCallOptions {
                    observation_budget: 0,
                    collect_before_observation: collect,
                },
                Arc::clone(&cancel.0),
            )
            .map_err(Self::classify_execution)?;
        Ok(Self::retain_result(result))
    }

    /// Inspect one retained constructor layer without evaluating its fields.
    /// This never installs a machine for a fabricated value.
    pub fn inspect_outer(
        &mut self,
        value: &PreparedValue,
    ) -> Result<PreparedOuter, PreparedRuntimeError> {
        self.ensure_available()?;
        let (machine, _) = self.machine.as_mut().ok_or(PreparedRuntimeError::Run(
            ExecutionError::UnknownPreparedHandle,
        ))?;
        let outer = machine
            .inspect_outer(value.0)
            .map_err(Self::classify_execution)?;
        Ok(Self::outer_result(outer))
    }

    /// Consume one retained value's runtime wrapper and release its root.
    /// Releasing an already-closed or foreign value is a no-op.
    pub fn release(&mut self, value: PreparedValue) -> bool {
        self.machine
            .as_mut()
            .is_some_and(|(machine, _)| machine.release(value.0))
    }

    fn run_entry_with_completion_hook(
        &mut self,
        binding: Option<ValueId>,
        arguments: &[u64],
        collect: bool,
        cancel: &PreparedCancelHandle,
        after_lower_success: impl FnOnce(),
    ) -> Result<PreparedRunResult, PreparedRuntimeError> {
        self.ensure_available()?;
        if cancel.is_cancelled() {
            return Err(PreparedRuntimeError::Cancelled);
        }
        let options = PreparedCallOptions {
            observation_budget: RunOptions::default().observation_budget,
            collect_before_observation: collect,
        };
        let entry = binding.unwrap_or_else(|| self.linked.prepared().entry());
        let (machine, program) = self.ensure_machine()?;
        if cancel.is_cancelled() {
            return Err(PreparedRuntimeError::Cancelled);
        }
        let result = machine
            .run_entry(program, entry, arguments, options, Arc::clone(&cancel.0))
            .map_err(Self::classify_execution)?;
        // Lower success is the completion point. Cancellation published after
        // it may affect a later entry, but cannot rewrite this result.
        after_lower_success();
        Ok(PreparedRunResult {
            values: result.values,
            collections: result.collections,
        })
    }

    fn ensure_available(&self) -> Result<(), PreparedRuntimeError> {
        if let Some((machine, _)) = &self.machine {
            if machine.disposition() == MachineDisposition::Unavailable {
                return Err(PreparedRuntimeError::Unavailable(
                    machine.failure().unwrap_or(MachineFailure {
                        cause: tidepool_codegen::host_fns::RuntimeError::BadPointer,
                        disposition: MachineDisposition::Unavailable,
                    }),
                ));
            }
        }
        Ok(())
    }

    fn ensure_machine(
        &mut self,
    ) -> Result<(&mut PreparedMachine<'static>, ProgramId), PreparedRuntimeError> {
        self.ensure_available()?;
        if self.machine.is_none() {
            let compiled = CompiledProgram::compile(&self.linked, TopSlotBase::ZERO)
                .map_err(PreparedRuntimeError::Compile)?;
            let top_slots = compiled.top_slot_count();
            let installed = PreparedMachine::new(
                compiled,
                PreparedMachineOptions {
                    nursery_bytes: RunOptions::default().nursery_bytes,
                    top_slots,
                },
            )
            .map_err(Self::classify_execution)?;
            self.machine = Some(installed);
        }
        match self.machine.as_mut() {
            Some((machine, program)) => Ok((machine, *program)),
            None => unreachable!("prepared machine installed above"),
        }
    }

    fn retain_result(result: PreparedResultBatch) -> PreparedRetainedResult {
        PreparedRetainedResult {
            values: result.values.into_iter().map(Self::value_result).collect(),
            collections: result.collections,
        }
    }

    fn outer_result(outer: CodegenPreparedOuter) -> PreparedOuter {
        match outer {
            CodegenPreparedOuter::Constructor { identity, fields } => PreparedOuter::Constructor {
                identity,
                fields: fields.into_iter().map(Self::value_result).collect(),
            },
        }
    }

    fn value_result(result: PreparedResult) -> PreparedValueResult {
        match result {
            PreparedResult::Void => PreparedValueResult::Void,
            PreparedResult::Scalar(word) => PreparedValueResult::Scalar(word),
            PreparedResult::Managed(handle) => PreparedValueResult::Managed(PreparedValue(handle)),
        }
    }

    fn classify_execution(error: ExecutionError) -> PreparedRuntimeError {
        PreparedRuntimeError::Run(error)
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
    use tidepool_repr::execution_schema::{
        Architecture, Endianness, ImportedValue, TargetDescriptor, EXECUTION_ABI_VERSION,
        SCHEMA_VERSION,
    };

    fn head(major: u8, length: usize) -> Vec<u8> {
        assert!(length < 24);
        vec![(major << 5) | length as u8]
    }

    fn array(values: impl IntoIterator<Item = Vec<u8>>) -> Vec<u8> {
        let values: Vec<_> = values.into_iter().collect();
        let mut result = head(4, values.len());
        for value in values {
            result.extend(value);
        }
        result
    }

    fn uint(value: u64) -> Vec<u8> {
        if value <= 23 {
            vec![value as u8]
        } else if value <= u8::MAX as u64 {
            vec![0x18, value as u8]
        } else if value <= u16::MAX as u64 {
            let mut result = vec![0x19];
            result.extend((value as u16).to_be_bytes());
            result
        } else if value <= u32::MAX as u64 {
            let mut result = vec![0x1a];
            result.extend((value as u32).to_be_bytes());
            result
        } else {
            let mut result = vec![0x1b];
            result.extend(value.to_be_bytes());
            result
        }
    }

    fn text(value: &str) -> Vec<u8> {
        let mut result = head(3, value.len());
        result.extend(value.as_bytes());
        result
    }

    fn rep_lifted() -> Vec<u8> {
        array([uint(1)])
    }

    fn symbol(namespace: &str, module: &str, occurrence: &str) -> Vec<u8> {
        array([
            text("fixture"),
            text(module),
            text(namespace),
            text(occurrence),
            array([uint(0)]),
        ])
    }

    fn terminal_fixture() -> Vec<u8> {
        let constructor = array([
            symbol("value", "PreparedStrict", "Box"),
            symbol("type", "PreparedStrict", "BoxFamily"),
            array([]),
            array([]),
            array([array([]), uint(1), uint(0), array([])]),
            rep_lifted(),
            uint(1),
            uint(1),
            uint(100),
        ]);
        let expression = array([uint(4), uint(0), array([])]);
        let function = array([uint(0), uint(0), array([]), array([]), uint(0)]);
        let top = array([
            symbol("value", "PreparedStrict", "entry"),
            array([uint(0), function]),
        ]);
        let binding_group = array([uint(0), top]);
        array([
            text("TPSTG"),
            uint(SCHEMA_VERSION),
            text("ghc-9.12-prepared-stg"),
            text("ghc-9.12.2"),
            uint(EXECUTION_ABI_VERSION),
            array([
                uint(0),
                uint(0),
                uint(64),
                uint(64),
                text("sysv64"),
                array([]),
            ]),
            array([array([array([]), array([uint(0), array([rep_lifted()])])])]),
            array([]),
            array([constructor]),
            array([]),
            array([expression]),
            array([binding_group]),
            uint(0),
        ])
    }

    fn m3_runtime() -> PreparedRuntime {
        let artifact = terminal_fixture();
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
        let prepared = parse_program(&artifact, &requirements, DecodeLimits::default()).unwrap();
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
                        evaluated: global.required_evaluated,
                        generation: global.required_generation.unwrap_or(0),
                    };
                    (value.identity.clone(), value)
                })
                .collect(),
        };
        PreparedRuntime::from_artifact(&artifact, &requirements, DecodeLimits::default(), imports)
            .unwrap()
    }

    #[test]
    fn integrity_failure_is_typed_independently_from_its_cause() {
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
    }

    #[test]
    fn uninstalled_failure_does_not_create_a_second_terminal_owner() {
        let mut runtime = m3_runtime();
        let failure = MachineFailure {
            cause: RuntimeError::Cancelled,
            disposition: MachineDisposition::Unavailable,
        };
        let reported =
            PreparedRuntime::classify_execution(ExecutionError::Runtime(failure.clone()));
        assert!(matches!(
            reported,
            PreparedRuntimeError::Run(ExecutionError::Runtime(retained))
                if retained == failure
        ));

        let cancel = runtime.new_cancel_handle();
        cancel.cancel();
        let replayed = runtime.run_entry(None, &[], false, &cancel).unwrap_err();
        assert!(matches!(replayed, PreparedRuntimeError::Cancelled));
    }

    #[test]
    fn cancellation_after_compiled_success_does_not_veto_completion() {
        let mut runtime = m3_runtime();
        let cancel = runtime.new_cancel_handle();

        let result = runtime.run_entry_with_completion_hook(None, &[], false, &cancel, || {
            cancel.cancel();
        });

        assert!(result.is_ok());
        assert!(cancel.is_cancelled());

        let next_cancel = runtime.new_cancel_handle();
        runtime
            .run_entry(None, &[], false, &next_cancel)
            .expect("cancellation published after completion must not poison reuse");
    }

    #[test]
    fn prepared_machine_reuses_one_heap_across_settled_entries() {
        let mut runtime = m3_runtime();
        let first_cancel = runtime.new_cancel_handle();
        let first = runtime
            .run_entry(None, &[], true, &first_cancel)
            .expect("first prepared entry settles");
        let second_cancel = runtime.new_cancel_handle();
        let second = runtime
            .run_entry(None, &[], true, &second_cancel)
            .expect("second prepared entry reuses the machine");

        assert_eq!(
            format!("{:?}", first.values),
            format!("{:?}", second.values)
        );
        assert_eq!(runtime.disposition(), MachineDisposition::Reusable);
    }

    #[test]
    fn cancelled_admission_does_not_poison_the_retained_machine() {
        let mut runtime = m3_runtime();
        let first_cancel = runtime.new_cancel_handle();
        runtime
            .run_entry(None, &[], false, &first_cancel)
            .expect("first prepared entry installs the machine");

        let cancelled = runtime.new_cancel_handle();
        cancelled.cancel();
        assert!(matches!(
            runtime.run_entry(None, &[], false, &cancelled),
            Err(PreparedRuntimeError::Cancelled)
        ));

        let retry_cancel = runtime.new_cancel_handle();
        runtime
            .run_entry(None, &[], true, &retry_cancel)
            .expect("cancelled admission leaves machine reusable");
        assert_eq!(runtime.disposition(), MachineDisposition::Reusable);
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
