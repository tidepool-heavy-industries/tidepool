//! Runtime custody for validated prepared-STG execution artifacts.
//!
//! Here `prepared` refers to the GHC prepared-STG handoff. It is distinct from
//! cell preparation in `workbench.rs` and `resident_workbench.rs`.
//!
//! Parsing, linking, compiled-owner construction, execution, cancellation, disposition,
//! and retained-program reuse cross this boundary in that order. The legacy
//! `CoreExpr` machine is not a fallback for any operation in this module.

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use tidepool_bridge::Value;
use tidepool_codegen::binding_table::{BindingEntry, BindingTable, BoundValue};
use tidepool_codegen::jit_machine::MachineDisposition;
use tidepool_codegen::machine_state::MachineFailure;
use tidepool_codegen::prepared_program::{
    CompileError, CompiledProgram, ExecutionError, ImportBindings, PreparedCallOptions,
    PreparedHandle, PreparedInput, PreparedMachine, PreparedMachineOptions,
    PreparedOuter as CodegenPreparedOuter, PreparedResult, PreparedResultBatch, ProgramId,
    RunOptions, TopSlotBase,
};
use tidepool_codegen::scope::ScopeId;
use tidepool_repr::execution_schema::{
    link_program, parse_program, DecodeLimits, Group, HeapRhs, ImportedValue, LinkError,
    LinkedProgram, MachineImports, ParseError, PreparedProgram, ProgramRequirements, Signature,
    SymbolIdentity, ValueId,
};
use tidepool_repr::{
    BindingName, Generation, MonotonicIdIssuer, SessionModule, SessionVarId, VarId,
};

/// Machine-wide top-table capacity a session machine reserves up front:
/// every later `install` claims its tops and import slots from this fixed
/// range, and registered root addresses must never move, so it is sized for
/// a whole session rather than one program. Exhaustion is the typed
/// `ExecutionError::TopTableExhausted`, never a reallocation.
const SESSION_TOP_SLOTS: usize = 4096;

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
    #[error("session binding {0:?} is not a live prepared binding")]
    UnknownBinding(SessionVarId),
    #[error("no value generation has been started: advance or set the session generation before binding")]
    GenerationNotStarted,
    #[error(
        "session binding {id:?} is leased by {leases} installed program(s) and cannot be released"
    )]
    BindingLeased { id: SessionVarId, leases: usize },
}

impl PreparedRuntimeError {
    #[must_use]
    pub fn kind(&self) -> PreparedFailureKind {
        match self {
            Self::Parse(_)
            | Self::Link(_)
            | Self::UnknownBinding(_)
            | Self::GenerationNotStarted
            | Self::BindingLeased { .. } => PreparedFailureKind::Rejected,
            Self::Cancelled => PreparedFailureKind::Cancelled,
            Self::Unavailable(_) => PreparedFailureKind::Integrity,
            Self::Compile(_) => PreparedFailureKind::Rejected,
            Self::Run(error) => match error {
                ExecutionError::MissingEntry(_)
                | ExecutionError::Unsupported(_)
                | ExecutionError::Arguments { .. }
                | ExecutionError::ArgumentRepresentation { .. }
                | ExecutionError::UnknownPreparedHandle
                | ExecutionError::ImportShape { .. }
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

/// One prepared session: a lazily installed machine shared by every program
/// installed into it, the session's retained bindings, and the one
/// generation counter later programs link against.
///
/// The first program is linked at construction and installed on first use;
/// later programs are installed through [`Self::install`] against bindings
/// made by [`Self::bind_top`]. Root registration stays with the machine
/// ([`PreparedMachine::release`] is the only deregistration path); the
/// [`BindingTable`] is the name/generation/lease ledger, reused from the Core
/// session rather than duplicated.
pub struct PreparedRuntime {
    /// The first program, until the machine exists and takes custody of it.
    pending: Option<LinkedProgram>,
    /// Set together: the machine and the id of the first program it was
    /// created with (the program `run_entry` addresses by default).
    machine: Option<(PreparedMachine<'static>, ProgramId)>,
    /// Every installed program, for entry defaults and export lookup.
    programs: BTreeMap<ProgramId, LinkedProgram>,
    bindings: BindingTable,
    /// The session's single value generation counter. A generation is a
    /// turn: every binding made before the next `advance_generation` shares
    /// it (as every binder of one Core turn shares its `Val.G<g>` module),
    /// and an importer's `required_generation` is checked against the
    /// generation the named binding was made at. `Generation(0)` is the
    /// empty session; binding at it is refused.
    val_gen: Generation,
    binding_ids: MonotonicIdIssuer,
}

impl PreparedRuntime {
    pub fn from_artifact(
        artifact: &[u8],
        requirements: &ProgramRequirements,
        limits: DecodeLimits,
        imports: MachineImports,
    ) -> Result<Self, PreparedRuntimeError> {
        let prepared = parse_program(artifact, requirements, limits)?;
        Self::from_prepared(prepared, imports)
    }

    /// Start a session from an already-decoded program (the artifact-bytes
    /// path above parses and then comes here).
    pub fn from_prepared(
        prepared: PreparedProgram,
        imports: MachineImports,
    ) -> Result<Self, PreparedRuntimeError> {
        let linked = link_program(prepared, &imports)?;
        Ok(Self {
            pending: Some(linked),
            machine: None,
            programs: BTreeMap::new(),
            bindings: BindingTable::new(),
            val_gen: Generation::default(),
            binding_ids: MonotonicIdIssuer::starting_at("prepared-binding", 1),
        })
    }

    /// The id of the session's first program, installing the machine if it
    /// has not been yet.
    pub fn first_program(&mut self) -> Result<ProgramId, PreparedRuntimeError> {
        self.ensure_machine()
    }

    /// The session's retained bindings (read-only; mutation goes through
    /// [`Self::bind_top`], [`Self::install`] and [`Self::release_binding`]).
    #[must_use]
    pub fn bindings(&self) -> &BindingTable {
        &self.bindings
    }

    /// The current value generation (`Generation(0)` before any turn).
    #[must_use]
    pub fn val_gen(&self) -> Generation {
        self.val_gen
    }

    /// Set the current value generation, e.g. to the generation a caller's
    /// projection was told to retain against. Generations only ever move
    /// forward: a value at or below the current one is refused.
    pub fn set_val_gen(&mut self, generation: Generation) -> Result<(), PreparedRuntimeError> {
        if generation <= self.val_gen {
            return Err(PreparedRuntimeError::GenerationNotStarted);
        }
        self.val_gen = generation;
        Ok(())
    }

    /// Start the next turn's generation and return it.
    pub fn advance_generation(&mut self) -> Generation {
        self.val_gen = self.val_gen.next();
        self.val_gen
    }

    /// Retain one of an installed program's top-level bindings under `name`
    /// at the session's current value generation, without running it. The
    /// returned id is what a later [`Self::install`] names an import by.
    /// Refused at `Generation(0)`: advance or set the generation first.
    pub fn bind_top(
        &mut self,
        program: ProgramId,
        value: ValueId,
        name: &str,
    ) -> Result<SessionVarId, PreparedRuntimeError> {
        if self.val_gen == Generation::default() {
            return Err(PreparedRuntimeError::GenerationNotStarted);
        }
        self.ensure_machine()?;
        let machine = self.machine_mut()?;
        let handle = machine
            .retain_top(program, value)
            .map_err(Self::classify_execution)?;
        let root = machine
            .handle_root(handle)
            .ok_or(PreparedRuntimeError::Run(
                ExecutionError::UnknownPreparedHandle,
            ))?;
        let id = SessionVarId::from_var(VarId(self.binding_ids.next_raw()));
        let entry = BindingEntry {
            name: BindingName(name.to_string()),
            id,
            module: SessionModule::val(self.val_gen),
            value: BoundValue::Prepared {
                root,
                handle,
                origin: Some((program, value)),
            },
            type_display: None,
            defining_expr: None,
            scope: ScopeId::ROOT,
        };
        Ok(self.bindings.bind(entry))
    }

    /// Install a later program that imports session bindings by identity.
    /// `imports` pairs each identity the artifact declares with the binding
    /// that satisfies it. The artifact is linked against those bindings'
    /// live shape (representation, settledness, exporting signature,
    /// generation) BEFORE anything is compiled or installed, so a stale
    /// generation (`LinkError::ImportContract`) or an undeclared identity
    /// (`LinkError::MissingImport`) has no machine side effect. On success
    /// every named binding is leased for the program's lifetime.
    pub fn install(
        &mut self,
        artifact: &[u8],
        requirements: &ProgramRequirements,
        limits: DecodeLimits,
        imports: &[(SymbolIdentity, SessionVarId)],
    ) -> Result<ProgramId, PreparedRuntimeError> {
        let prepared = parse_program(artifact, requirements, limits)?;
        self.install_prepared(prepared, imports)
    }

    /// [`Self::install`] for an already-decoded program.
    pub fn install_prepared(
        &mut self,
        prepared: PreparedProgram,
        imports: &[(SymbolIdentity, SessionVarId)],
    ) -> Result<ProgramId, PreparedRuntimeError> {
        self.ensure_machine()?;
        let mut values = MachineImports::default();
        let mut bindings = ImportBindings::new();
        for (identity, id) in imports {
            let entry = self
                .bindings
                .get(*id)
                .ok_or(PreparedRuntimeError::UnknownBinding(*id))?;
            let BoundValue::Prepared { handle, origin, .. } = entry.value else {
                return Err(PreparedRuntimeError::UnknownBinding(*id));
            };
            let generation = entry.module.gen().0;
            let entry_signature = origin.and_then(|origin| self.export_signature(origin));
            let evaluated = self
                .machine_ref()?
                .handle_is_evaluated(handle)
                .map_err(Self::classify_execution)?;
            values.values.insert(
                identity.clone(),
                ImportedValue {
                    identity: identity.clone(),
                    rep: handle.rep(),
                    entry_signature,
                    evaluated,
                    generation,
                },
            );
            bindings.insert(identity.clone(), handle);
        }
        let linked = link_program(prepared, &values)?;
        let machine = self.machine_mut()?;
        let compiled = CompiledProgram::compile(&linked, machine.next_top_slot_base())
            .map_err(PreparedRuntimeError::Compile)?;
        let program = machine
            .install_program(compiled, bindings)
            .map_err(Self::classify_execution)?;
        self.bindings
            .acquire_leases(imports.iter().map(|(_, id)| *id));
        self.programs.insert(program, linked);
        Ok(program)
    }

    /// Release a session binding's root. Refused, with the lease count,
    /// while any installed program imports it; leases are held for the
    /// importing program's lifetime, which this wave ends only with the
    /// machine.
    pub fn release_binding(&mut self, id: SessionVarId) -> Result<(), PreparedRuntimeError> {
        let leases = self.bindings.lease_count(id);
        if leases > 0 {
            return Err(PreparedRuntimeError::BindingLeased { id, leases });
        }
        let entry = self
            .bindings
            .remove_live(id)
            .ok_or(PreparedRuntimeError::UnknownBinding(id))?;
        let BoundValue::Prepared { handle, .. } = entry.value else {
            return Err(PreparedRuntimeError::UnknownBinding(id));
        };
        if !self.machine_mut()?.release(handle) {
            return Err(PreparedRuntimeError::Run(
                ExecutionError::UnknownPreparedHandle,
            ));
        }
        Ok(())
    }

    /// The signature a bound top exports, from its producing program's own
    /// declaration: a function's or thunk's entry signature, none for a
    /// constructor or byte top. What an importer's `entry_signature`
    /// declaration must equal at link time.
    fn export_signature(&self, (program, value): (ProgramId, ValueId)) -> Option<Signature> {
        let prepared = self.programs.get(&program)?.prepared();
        let binding = prepared
            .bindings()
            .iter()
            .flat_map(|group| match group {
                Group::NonRecursive(top) => std::slice::from_ref(top),
                Group::Recursive(tops) => tops.as_slice(),
            })
            .find(|top| top.binding.id == value)?;
        let signature = match &binding.binding.rhs {
            HeapRhs::Function { signature, .. } | HeapRhs::Thunk { signature, .. } => *signature,
            HeapRhs::Constructor { .. } | HeapRhs::Bytes(_) => return None,
        };
        prepared.signatures().get(signature.0 as usize).cloned()
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
            .map_or(0, |(machine, _)| machine.handle_count())
    }

    /// Run an entry of the session's first program (`None` selects that
    /// program's declared entry).
    pub fn run_entry(
        &mut self,
        binding: Option<ValueId>,
        arguments: &[u64],
        collect: bool,
        cancel: &PreparedCancelHandle,
    ) -> Result<PreparedRunResult, PreparedRuntimeError> {
        let program = self.ensure_machine()?;
        let entry = self.entry_of(program, binding)?;
        self.run_entry_with_completion_hook(program, entry, arguments, collect, cancel, || {})
    }

    /// [`Self::run_entry`] for any installed program.
    pub fn run_entry_in(
        &mut self,
        program: ProgramId,
        entry: ValueId,
        arguments: &[u64],
        collect: bool,
        cancel: &PreparedCancelHandle,
    ) -> Result<PreparedRunResult, PreparedRuntimeError> {
        self.run_entry_with_completion_hook(program, entry, arguments, collect, cancel, || {})
    }

    /// Execute with scalar or borrowed retained arguments and retain managed
    /// results under this runtime's machine owner. Runs the session's first
    /// program (`None` selects its declared entry).
    pub fn run_entry_retained(
        &mut self,
        binding: Option<ValueId>,
        arguments: &[PreparedArgument<'_>],
        collect: bool,
        cancel: &PreparedCancelHandle,
    ) -> Result<PreparedRetainedResult, PreparedRuntimeError> {
        let program = self.ensure_machine()?;
        let entry = self.entry_of(program, binding)?;
        self.run_entry_retained_in(program, entry, arguments, collect, cancel)
    }

    /// [`Self::run_entry_retained`] for any installed program.
    pub fn run_entry_retained_in(
        &mut self,
        program: ProgramId,
        entry: ValueId,
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
        self.ensure_machine()?;
        let machine = self.machine_mut()?;
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
        program: ProgramId,
        entry: ValueId,
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
        self.ensure_machine()?;
        let machine = self.machine_mut()?;
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

    /// Install the machine with the first program if that has not happened
    /// yet; returns the first program's id either way.
    fn ensure_machine(&mut self) -> Result<ProgramId, PreparedRuntimeError> {
        self.ensure_available()?;
        if let Some((_, program)) = &self.machine {
            return Ok(*program);
        }
        let linked = self.pending.take().ok_or(PreparedRuntimeError::Run(
            ExecutionError::UnknownProgram(ProgramId::FIRST),
        ))?;
        let compiled = match CompiledProgram::compile(&linked, TopSlotBase::ZERO) {
            Ok(compiled) => compiled,
            Err(error) => {
                self.pending = Some(linked);
                return Err(PreparedRuntimeError::Compile(error));
            }
        };
        let top_slots = compiled.top_slot_count().max(SESSION_TOP_SLOTS);
        let installed = match PreparedMachine::new(
            compiled,
            PreparedMachineOptions {
                nursery_bytes: RunOptions::default().nursery_bytes,
                top_slots,
            },
        ) {
            Ok(installed) => installed,
            Err(error) => {
                self.pending = Some(linked);
                return Err(Self::classify_execution(error));
            }
        };
        let program = installed.1;
        self.machine = Some(installed);
        self.programs.insert(program, linked);
        Ok(program)
    }

    fn machine_mut(&mut self) -> Result<&mut PreparedMachine<'static>, PreparedRuntimeError> {
        self.machine
            .as_mut()
            .map(|(machine, _)| machine)
            .ok_or(PreparedRuntimeError::Run(ExecutionError::UnknownProgram(
                ProgramId::FIRST,
            )))
    }

    fn machine_ref(&self) -> Result<&PreparedMachine<'static>, PreparedRuntimeError> {
        self.machine
            .as_ref()
            .map(|(machine, _)| machine)
            .ok_or(PreparedRuntimeError::Run(ExecutionError::UnknownProgram(
                ProgramId::FIRST,
            )))
    }

    /// `binding`, or the program's declared entry when `None`.
    fn entry_of(
        &self,
        program: ProgramId,
        binding: Option<ValueId>,
    ) -> Result<ValueId, PreparedRuntimeError> {
        if let Some(entry) = binding {
            return Ok(entry);
        }
        self.programs
            .get(&program)
            .map(|linked| linked.prepared().entry())
            .ok_or(PreparedRuntimeError::Run(ExecutionError::UnknownProgram(
                program,
            )))
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

        let program = runtime.first_program().expect("first program installs");
        let entry = runtime
            .entry_of(program, None)
            .expect("first program has an entry");
        let result =
            runtime.run_entry_with_completion_hook(program, entry, &[], false, &cancel, || {
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

    // ---- S4: session custody -- bind, import by generation, leases --------

    use tidepool_repr::execution_schema::{
        testing, Atom, CheckedLayout, ConstructorDecl, ConstructorId, ExprFrame, FieldLayout,
        GlobalDecl, GlobalId, Group, HeapRhs, ResultContract, RuntimeRep, ScalarLiteral, Signature,
        SignatureId, UpdatePolicy, ValueRef,
    };

    fn producer_identity() -> SymbolIdentity {
        testing::identity("S4Session", "producer")
    }

    /// A memoized CAF returning `Field(99)`: unevaluated until first run,
    /// then an updated indirection to an evaluated constructor.
    fn producer_program() -> PreparedProgram {
        let mut wire = testing::wire_program();
        wire.signatures[0].results = ResultContract::Returns(vec![RuntimeRep::LiftedRef]);
        wire.constructors.push(ConstructorDecl {
            identity: testing::identity("S4Session", "Field"),
            family: testing::identity("S4Session", "Field"),
            host_id: tidepool_repr::DataConId(980),
            result_rep: RuntimeRep::LiftedRef,
            tag: 1,
            family_size: 1,
            field_reps: vec![RuntimeRep::Int(64)],
            strict_fields: vec![true],
            layout: CheckedLayout {
                fields: vec![FieldLayout {
                    rep: RuntimeRep::Int(64),
                    offset: 0,
                }],
                alignment: 8,
                payload_size: 8,
                root_mask: vec![false],
            },
        });
        wire.expressions.nodes[0] = ExprFrame::Construct {
            constructor: ConstructorId(0),
            fields: vec![Atom::Scalar(ScalarLiteral::Int {
                bits: 64,
                bytes: 99_i64.to_be_bytes().to_vec(),
            })],
        };
        let Group::NonRecursive(top) = &mut wire.bindings[0] else {
            unreachable!()
        };
        top.identity = producer_identity();
        top.binding.rhs = HeapRhs::Thunk {
            signature: SignatureId(0),
            update: UpdatePolicy::Memoize,
            captures: vec![],
            body: 0,
        };
        testing::prepare(wire).expect("producer fixture")
    }

    /// A program whose only entry returns its one imported global. The
    /// declaration carries the producer top's own `[] -> LiftedRef` entry
    /// signature, so linking also exercises the exported-signature check.
    fn consumer_program(
        required_evaluated: bool,
        required_generation: Option<u64>,
    ) -> PreparedProgram {
        let mut wire = testing::wire_program();
        wire.signatures[0] = Signature {
            arguments: vec![],
            results: ResultContract::Returns(vec![RuntimeRep::LiftedRef]),
        };
        wire.globals = vec![GlobalDecl {
            identity: producer_identity(),
            rep: RuntimeRep::LiftedRef,
            entry_signature: Some(SignatureId(0)),
            required_evaluated,
            required_generation,
        }];
        wire.expressions.nodes[0] =
            ExprFrame::Return(vec![Atom::Ref(ValueRef::Global(GlobalId(0)))]);
        let Group::NonRecursive(top) = &mut wire.bindings[0] else {
            unreachable!()
        };
        top.binding.rhs = HeapRhs::Function {
            signature: SignatureId(0),
            parameters: vec![],
            captures: vec![],
            body: 0,
        };
        testing::prepare(wire).expect("consumer fixture")
    }

    fn session() -> (PreparedRuntime, ProgramId) {
        let mut runtime =
            PreparedRuntime::from_prepared(producer_program(), MachineImports::default())
                .expect("producer links closed");
        let first = runtime.first_program().expect("first program installs");
        runtime
            .set_val_gen(Generation(1))
            .expect("the first turn starts at generation 1");
        (runtime, first)
    }

    #[test]
    fn bind_install_run_reads_the_bound_top_by_generation() {
        let (mut runtime, first) = session();
        let cancel = runtime.new_cancel_handle();
        // Force the CAF once so it is an evaluated (updated) constructor.
        runtime
            .run_entry(None, &[], true, &cancel)
            .expect("producer entry runs");
        let id = runtime
            .bind_top(first, ValueId(0), "producer")
            .expect("producer top binds");
        assert_eq!(
            runtime.bindings().get(id).map(|entry| entry.module.gen()),
            Some(Generation(1)),
            "a binding is made at the session's current generation"
        );
        let consumer = runtime
            .install_prepared(
                consumer_program(true, Some(1)),
                &[(producer_identity(), id)],
            )
            .expect("consumer links against generation 1 and installs");
        assert_ne!(consumer, first);
        assert_eq!(runtime.bindings().lease_count(id), 1);

        let read = runtime
            .run_entry_retained_in(consumer, ValueId(0), &[], true, &cancel)
            .expect("consumer reads its import through the slot");
        let mut values = read.values;
        let PreparedValueResult::Managed(value) = values.remove(0) else {
            panic!("consumer must return the imported managed value");
        };
        let PreparedOuter::Constructor { identity, fields } = runtime
            .inspect_outer(&value)
            .expect("imported value inspects through the shared machine");
        assert_eq!(identity, tidepool_repr::DataConId(980));
        assert!(matches!(
            fields.as_slice(),
            [PreparedValueResult::Scalar(99)]
        ));
        assert!(runtime.release(value));
        assert_eq!(runtime.disposition(), MachineDisposition::Reusable);
    }

    #[test]
    fn stale_generation_is_refused_by_link_before_any_install_side_effect() {
        let (mut runtime, first) = session();
        let id = runtime
            .bind_top(first, ValueId(0), "producer")
            .expect("producer top binds at generation 1");
        let handles_before = runtime.retained_handle_count();
        let error = runtime
            .install_prepared(
                consumer_program(false, Some(7)),
                &[(producer_identity(), id)],
            )
            .expect_err("a consumer linked against generation 7 must not install");
        assert!(
            matches!(&error, PreparedRuntimeError::Link(link) if matches!(**link, LinkError::ImportContract(_))),
            "expected ImportContract, got {error:?}"
        );
        assert_eq!(error.kind(), PreparedFailureKind::Rejected);
        assert_eq!(runtime.retained_handle_count(), handles_before);
        assert_eq!(
            runtime.bindings().lease_count(id),
            0,
            "a refused link leases nothing"
        );
        // The same session still installs a correctly-linked consumer.
        runtime
            .install_prepared(
                consumer_program(false, Some(1)),
                &[(producer_identity(), id)],
            )
            .expect("the refused link left the machine installable");
    }

    #[test]
    fn missing_import_is_refused_by_link() {
        let (mut runtime, _first) = session();
        let error = runtime
            .install_prepared(consumer_program(false, None), &[])
            .expect_err("a declared global with no binding must not install");
        assert!(
            matches!(&error, PreparedRuntimeError::Link(link) if matches!(**link, LinkError::MissingImport(_))),
            "expected MissingImport, got {error:?}"
        );
    }

    #[test]
    fn required_evaluated_is_checked_against_the_live_value() {
        let (mut runtime, first) = session();
        let id = runtime
            .bind_top(first, ValueId(0), "producer")
            .expect("the unforced CAF binds");
        let error = runtime
            .install_prepared(
                consumer_program(true, Some(1)),
                &[(producer_identity(), id)],
            )
            .expect_err("an unforced thunk does not satisfy required_evaluated");
        assert!(
            matches!(&error, PreparedRuntimeError::Link(link) if matches!(**link, LinkError::ImportContract(_))),
            "expected ImportContract, got {error:?}"
        );
        let cancel = runtime.new_cancel_handle();
        runtime
            .run_entry(None, &[], true, &cancel)
            .expect("forcing the CAF updates the bound top in place");
        runtime
            .install_prepared(
                consumer_program(true, Some(1)),
                &[(producer_identity(), id)],
            )
            .expect("the same binding now satisfies required_evaluated");
    }

    #[test]
    fn release_refuses_a_leased_binding_and_releases_an_unleased_one() {
        let (mut runtime, first) = session();
        let leased = runtime
            .bind_top(first, ValueId(0), "leased")
            .expect("binds");
        runtime.advance_generation();
        let free = runtime
            .bind_top(first, ValueId(0), "free")
            .expect("binds again at the next generation");
        assert_eq!(
            runtime.bindings().get(free).map(|entry| entry.module.gen()),
            Some(Generation(2))
        );
        runtime
            .install_prepared(
                consumer_program(false, Some(1)),
                &[(producer_identity(), leased)],
            )
            .expect("consumer installs against the leased binding");
        let error = runtime
            .release_binding(leased)
            .expect_err("a leased binding must not release");
        assert!(matches!(
            error,
            PreparedRuntimeError::BindingLeased { id, leases: 1 } if id == leased
        ));
        let handles_before = runtime.retained_handle_count();
        runtime
            .release_binding(free)
            .expect("an unleased binding releases");
        assert_eq!(runtime.retained_handle_count(), handles_before - 1);
        assert!(matches!(
            runtime.release_binding(free),
            Err(PreparedRuntimeError::UnknownBinding(id)) if id == free
        ));
        assert_eq!(runtime.disposition(), MachineDisposition::Reusable);
    }
}
