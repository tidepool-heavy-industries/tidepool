//! Runtime custody for validated prepared-STG execution artifacts.
//!
//! Here `prepared` refers to the GHC prepared-STG handoff. It is distinct from
//! cell preparation in `workbench.rs` and `resident_workbench.rs`.
//!
//! Parsing, linking, compiled-owner construction, execution, cancellation, disposition,
//! and retained-program reuse cross this boundary in that order. The legacy
//! `CoreExpr` machine is not a fallback for any operation in this module.

use std::collections::BTreeMap;

use tidepool_bridge::Value;
use tidepool_codegen::binding_table::{BindingEntry, BindingTable, BoundValue};
use tidepool_codegen::machine_state::MachineFailure;
use tidepool_codegen::prepared_program::{
    CompileError, CompiledProgram, ExecutionError, ImportBindings, PreparedCallOptions,
    PreparedHandle, PreparedInput, PreparedMachine, PreparedMachineOptions,
    PreparedOuter as CodegenPreparedOuter, PreparedResult, PreparedResultBatch, ProgramId,
    RunOptions, TopSlotBase,
};
use tidepool_codegen::scope::ScopeId;
// Re-exported: callers of this module's realm-scoped cancellation API
// (`open_realm`/`cancel_handle`/`close_realm`) need both types without a
// separate `tidepool_codegen` dependency of their own.
pub use tidepool_codegen::jit_machine::CancelHandle;
pub use tidepool_codegen::jit_machine::MachineDisposition;
pub use tidepool_codegen::suspension::RealmId;
use tidepool_repr::execution_schema::{
    link_program, parse_program, DecodeLimits, Group, HeapRhs, ImportedValue, LinkError,
    LinkedProgram, MachineImports, ParseError, PreparedProgram, ProgramRequirements, Signature,
    SymbolIdentity, ValueId,
};
use tidepool_repr::{
    BindingName, Generation, MonotonicIdIssuer, SessionModule, SessionVarId, VarId,
};

use tidepool_effect::{EffectRunPolicy, LivePayloadPolicy};

use super::resident::SessionRunContext;

/// Machine-wide top-table capacity a session machine reserves up front:
/// every later `install` claims its tops and import slots from this fixed
/// range, and registered root addresses must never move, so it is sized for
/// a whole session rather than one program. Exhaustion is the typed
/// `ExecutionError::TopTableExhausted`, never a reallocation.
const SESSION_TOP_SLOTS: usize = 4096;

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
    #[error(
        "a managed argument's handle is not live under realm {realm:?}: it was minted under a \
         different runtime resource scope (or already released)"
    )]
    CrossRealmArgument { realm: RealmId },
}

impl PreparedRuntimeError {
    #[must_use]
    pub fn kind(&self) -> PreparedFailureKind {
        match self {
            Self::Parse(_)
            | Self::Link(_)
            | Self::UnknownBinding(_)
            | Self::GenerationNotStarted
            | Self::BindingLeased { .. }
            | Self::CrossRealmArgument { .. } => PreparedFailureKind::Rejected,
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
                | ExecutionError::DescriptorShape { .. }
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

/// The identity of one [`PreparedValue`] under a runtime resource scope,
/// for a caller that must name a retained value without holding it (e.g. a
/// [`crate::session::registry::SessionRegistry`] hole). Unlike
/// [`PreparedValue`] this is `Clone + Copy + PartialEq + Debug` and carries
/// no ownership: it does not keep the handle's root alive, and holding one
/// past a [`PreparedRuntime::release`] or [`PreparedRuntime::close_realm`]
/// simply makes [`PreparedRuntime::parked_realm`] answer `None`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PreparedHole {
    pub realm: RealmId,
    pub k: PreparedHandle,
}

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

/// What closing a realm actually released, from [`PreparedRuntime::close_realm_report`].
/// `frames` is always `0`: this engine never parks a continuation (see
/// [`PreparedMachine::close_realm`]'s doc), so the field exists only to mirror
/// the JIT machine's own scope-retirement receipt shape.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RealmRetirement {
    pub frames: usize,
    pub handles_released: usize,
    pub leases_released: usize,
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
    /// Session-var ids leased by each realm's `install_prepared_in` calls,
    /// released together (`self.bindings.release_leases`) when that realm
    /// closes. The import slot a lease guards is the installing program's
    /// own persistent root — the lease exists to protect binding-table
    /// identity/generation from a later mismatched re-`bind_top`/install,
    /// not to keep the underlying value alive — so releasing every lease a
    /// realm holds at `close_realm` is safe even though the machine may
    /// still (independently) retain the value elsewhere.
    realm_leases: BTreeMap<RealmId, Vec<SessionVarId>>,
    /// Ambient actor mount context, set once by `Self::set_actor_execution`
    /// and otherwise unused: this engine has no effect handlers of its own
    /// yet, so `EffectRunPolicy`/`LivePayloadPolicy` are stored for a later
    /// wave rather than acted on.
    actor_execution: Option<(SessionRunContext, EffectRunPolicy, LivePayloadPolicy)>,
}

// SAFETY: `PreparedRuntime` is Send under the same stowed-XOR-running
// discipline as `PersistentSession`/`JitEffectMachine`
// (`tidepool-codegen/src/jit_machine.rs`'s `unsafe impl Send for
// JitEffectMachine`, and this crate's `persistent.rs` threading note above
// `PersistentSession`'s machine lifecycle section): exactly one thread ever
// touches a `PreparedRuntime` at a time, because `SessionRegistry` only ever
// hands it to one checkout, which either runs on the caller's own thread or
// is moved wholesale onto a blocking thread and back (`Checkout::into_parts`)
// -- it is never split, aliased, or driven from two threads at once.
// Field by field:
//  - `pending: Option<LinkedProgram>` -- plain owned data (parsed/linked
//    program tree), no thread affinity.
//  - `machine: Option<(PreparedMachine<'static>, ProgramId)>` -- the only
//    field that is not already auto-`Send`. `PreparedMachine`'s non-`Send`
//    fields are `Rc<MachineState>` and (inside its installed programs)
//    `Rc<CompiledProgram>`. `MachineState` itself already carries `unsafe
//    impl Send` (`tidepool-codegen/src/machine_state.rs`) for the identical
//    reason: it is touched by exactly one thread at a time. An `Rc`'s own
//    non-`Send`-ness is about un-synchronized refcount mutation from two
//    threads concurrently, not about its pointee being thread-affine data;
//    under the single-owner discipline above, no second thread ever holds a
//    clone of either `Rc` while this one moves, so no concurrent refcount
//    access can occur. The raw code pointers, vmctx, and heap buffers
//    reachable through `PreparedMachine` are process-global address space
//    (`tidepool-codegen/CLAUDE.md`'s "JIT allocation" section), valid from
//    any thread, exactly like `JitEffectMachine`'s own code/heap.
//  - `programs: BTreeMap<ProgramId, LinkedProgram>` -- plain owned data.
//  - `bindings: BindingTable` -- already carries its own `unsafe impl Send`
//    (`tidepool-codegen/src/binding_table.rs`) for its `RootSlot(*mut *mut
//    u8)` cells, which are owned by the machine's `OldSpace` and therefore
//    live under the very same single-owner discipline as `machine` above:
//    `BindingTable` never deregisters a GC root itself (that is
//    `JitEffectMachine`/`PreparedMachine`'s job), so it makes no unsynchronized
//    access to the pointee either.
//  - `val_gen: Generation`, `binding_ids: MonotonicIdIssuer` -- plain data.
//  - `realm_leases: BTreeMap<RealmId, Vec<SessionVarId>>` -- plain data.
//  - `actor_execution: Option<(SessionRunContext, EffectRunPolicy,
//    LivePayloadPolicy)>` -- plain data (ids and policy enums).
unsafe impl Send for PreparedRuntime {}

static_assertions::assert_impl_all!(PreparedRuntime: Send);

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
            realm_leases: BTreeMap::new(),
            actor_execution: None,
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
        self.install_in(artifact, requirements, limits, imports, RealmId::ROOT)
    }

    /// [`Self::install`] whose imports' leases are released together when
    /// `realm` closes, instead of being held for the machine's whole life.
    pub fn install_in(
        &mut self,
        artifact: &[u8],
        requirements: &ProgramRequirements,
        limits: DecodeLimits,
        imports: &[(SymbolIdentity, SessionVarId)],
        realm: RealmId,
    ) -> Result<ProgramId, PreparedRuntimeError> {
        let prepared = parse_program(artifact, requirements, limits)?;
        self.install_prepared_in(prepared, imports, realm)
    }

    /// [`Self::install`] for an already-decoded program.
    pub fn install_prepared(
        &mut self,
        prepared: PreparedProgram,
        imports: &[(SymbolIdentity, SessionVarId)],
    ) -> Result<ProgramId, PreparedRuntimeError> {
        self.install_prepared_in(prepared, imports, RealmId::ROOT)
    }

    /// [`Self::install_in`] for an already-decoded program.
    pub fn install_prepared_in(
        &mut self,
        prepared: PreparedProgram,
        imports: &[(SymbolIdentity, SessionVarId)],
        realm: RealmId,
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
        let compiled = machine
            .compile_for_install(&linked)
            .map_err(PreparedRuntimeError::Compile)?;
        let program = machine
            .install_program(compiled, bindings)
            .map_err(Self::classify_execution)?;
        self.bindings
            .acquire_leases(imports.iter().map(|(_, id)| *id));
        self.realm_leases
            .entry(realm)
            .or_default()
            .extend(imports.iter().map(|(_, id)| *id));
        self.programs.insert(program, linked);
        Ok(program)
    }

    /// Install one live session turn's projected artifact and bind the name
    /// it introduces at the runtime's current generation. `imports` pairs
    /// each retained-generation identity the turn's module declared with the
    /// session binding that satisfies it. This is [`Self::install_prepared`]
    /// followed by [`Self::bind_top`] against the freshly installed
    /// program's own entry — the link+install+bind sequence
    /// [`super::prepared_turn::SessionTurns::run`] drives after it has
    /// projected `prepared` through `ExtractCmd`'s `--target` mode.
    pub fn turn(
        &mut self,
        prepared: PreparedProgram,
        imports: &[(SymbolIdentity, SessionVarId)],
        introduces: &str,
    ) -> Result<SessionVarId, PreparedRuntimeError> {
        let program = self.install_prepared(prepared, imports)?;
        let entry = self
            .programs
            .get(&program)
            .map(|linked| linked.prepared().entry())
            .ok_or(PreparedRuntimeError::Run(ExecutionError::UnknownProgram(
                program,
            )))?;
        self.bind_top(program, entry, introduces)
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

    /// Mint a fresh realm id for a new cancellation scope. A realm id is a
    /// free-standing identity (`RealmId::fresh`); it needs no machine and
    /// nothing is registered under it until the first call or inspection
    /// made with it.
    #[must_use]
    pub fn open_realm(&self) -> RealmId {
        RealmId::fresh()
    }

    /// Obtain a clone-able cancellation handle scoped to `realm`, lazily
    /// minting that realm's flag on first request. Cancelling it aborts
    /// only calls made with `realm`; sibling realms are unaffected.
    pub fn cancel_handle(&mut self, realm: RealmId) -> Result<CancelHandle, PreparedRuntimeError> {
        self.ensure_machine()?;
        Ok(self.machine_mut()?.realm_cancel_handle(realm))
    }

    /// SCOPE EXIT: close `realm`, releasing every value handle it owns.
    /// `(0, 0)` if no machine has been installed yet (nothing to close).
    /// See [`PreparedMachine::close_realm`] for the exact contract. Kept for
    /// existing callers; [`Self::close_realm_report`] additionally reports
    /// leases released.
    pub fn close_realm(&mut self, realm: RealmId) -> (usize, usize) {
        let report = self.close_realm_report(realm);
        (report.frames, report.handles_released)
    }

    /// [`Self::close_realm`], also releasing every lease
    /// [`Self::install_prepared_in`] acquired under `realm`
    /// (`self.bindings.release_leases`) and reporting the full receipt. The
    /// leased import slot is the installing program's own persistent root —
    /// the lease protects binding-table identity/generation, not the
    /// value's liveness — so releasing it at realm close never drops a value
    /// out from under a still-running program.
    pub fn close_realm_report(&mut self, realm: RealmId) -> RealmRetirement {
        let (frames, handles_released) = self
            .machine
            .as_mut()
            .map_or((0, 0), |(machine, _)| machine.close_realm(realm));
        let leases = self.realm_leases.remove(&realm).unwrap_or_default();
        let leases_released = leases.len();
        self.bindings.release_leases(leases);
        RealmRetirement {
            frames,
            handles_released,
            leases_released,
        }
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
        realm: RealmId,
    ) -> Result<PreparedRunResult, PreparedRuntimeError> {
        let program = self.ensure_machine()?;
        let entry = self.entry_of(program, binding)?;
        self.run_entry_with_completion_hook(program, entry, arguments, collect, realm, || {})
    }

    /// [`Self::run_entry`] for any installed program.
    pub fn run_entry_in(
        &mut self,
        program: ProgramId,
        entry: ValueId,
        arguments: &[u64],
        collect: bool,
        realm: RealmId,
    ) -> Result<PreparedRunResult, PreparedRuntimeError> {
        self.run_entry_with_completion_hook(program, entry, arguments, collect, realm, || {})
    }

    /// Execute with scalar or borrowed retained arguments and retain managed
    /// results under this runtime's machine owner. Runs the session's first
    /// program (`None` selects its declared entry).
    pub fn run_entry_retained(
        &mut self,
        binding: Option<ValueId>,
        arguments: &[PreparedArgument<'_>],
        collect: bool,
        realm: RealmId,
    ) -> Result<PreparedRetainedResult, PreparedRuntimeError> {
        let program = self.ensure_machine()?;
        let entry = self.entry_of(program, binding)?;
        self.run_entry_retained_in(program, entry, arguments, collect, realm)
    }

    /// [`Self::run_entry_retained`] for any installed program.
    pub fn run_entry_retained_in(
        &mut self,
        program: ProgramId,
        entry: ValueId,
        arguments: &[PreparedArgument<'_>],
        collect: bool,
        realm: RealmId,
    ) -> Result<PreparedRetainedResult, PreparedRuntimeError> {
        self.ensure_available()?;
        self.ensure_machine()?;
        let machine = self.machine_ref()?;
        for argument in arguments {
            if let PreparedArgument::Managed(value) = argument {
                if machine.handle_realm(value.0) != Some(realm) {
                    return Err(PreparedRuntimeError::CrossRealmArgument { realm });
                }
            }
        }
        if self
            .machine_mut()?
            .realm_cancel_handle(realm)
            .is_cancelled()
        {
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
        let machine = self.machine_mut()?;
        if machine.realm_cancel_handle(realm).is_cancelled() {
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
                realm,
            )
            .map_err(Self::classify_execution)?;
        Ok(self.retain_result(result))
    }

    /// Inspect one retained constructor layer without evaluating its fields.
    /// This never installs a machine for a fabricated value.
    pub fn inspect_outer(
        &mut self,
        value: &PreparedValue,
        realm: RealmId,
    ) -> Result<PreparedOuter, PreparedRuntimeError> {
        self.ensure_available()?;
        let (machine, _) = self.machine.as_mut().ok_or(PreparedRuntimeError::Run(
            ExecutionError::UnknownPreparedHandle,
        ))?;
        let outer = machine
            .inspect_outer(value.0, realm)
            .map_err(Self::classify_execution)?;
        Ok(self.outer_result(outer))
    }

    /// Consume one retained value's runtime wrapper and release its root.
    /// Releasing an already-closed or foreign value is a no-op.
    pub fn release(&mut self, value: PreparedValue) -> bool {
        self.machine
            .as_mut()
            .is_some_and(|(machine, _)| machine.release(value.0))
    }

    /// This runtime's [`PreparedHole`] for `value` under `realm`. Does not
    /// consume or alter `value`'s liveness; call [`Self::release`] once the
    /// value itself is no longer needed.
    #[must_use]
    pub fn hole_for(&self, value: &PreparedValue, realm: RealmId) -> PreparedHole {
        PreparedHole { realm, k: value.0 }
    }

    /// `Some(hole.realm)` iff `hole`'s handle is still live in this
    /// runtime's machine under that realm; `None` once released (by
    /// [`Self::release`], [`Self::release_binding`], or [`Self::close_realm`])
    /// or if it was never minted under this realm to begin with.
    ///
    /// Answered from `PreparedMachine::handle_realm`, the machine's own
    /// `ResourceLedger` query -- the single owner of the handle-to-realm
    /// fact, not a mirror kept in step with it.
    #[must_use]
    pub fn parked_realm(&self, hole: &PreparedHole) -> Option<RealmId> {
        (self.machine_ref().ok()?.handle_realm(hole.k) == Some(hole.realm)).then_some(hole.realm)
    }

    /// Set the ambient actor mount context for this runtime. See the
    /// `actor_execution` field doc: this engine does not yet act on
    /// `effect_policy`/`live_payload`, but stores them for parity with
    /// [`ActorRunTarget::install_actor_execution`]'s other implementers.
    pub fn set_actor_execution(
        &mut self,
        context: SessionRunContext,
        effect_policy: EffectRunPolicy,
        live_payload: LivePayloadPolicy,
    ) {
        self.actor_execution = Some((context, effect_policy, live_payload));
    }

    fn run_entry_with_completion_hook(
        &mut self,
        program: ProgramId,
        entry: ValueId,
        arguments: &[u64],
        collect: bool,
        realm: RealmId,
        after_lower_success: impl FnOnce(),
    ) -> Result<PreparedRunResult, PreparedRuntimeError> {
        self.ensure_available()?;
        self.ensure_machine()?;
        if self
            .machine_mut()?
            .realm_cancel_handle(realm)
            .is_cancelled()
        {
            return Err(PreparedRuntimeError::Cancelled);
        }
        let options = PreparedCallOptions {
            observation_budget: RunOptions::default().observation_budget,
            collect_before_observation: collect,
        };
        let machine = self.machine_mut()?;
        if machine.realm_cancel_handle(realm).is_cancelled() {
            return Err(PreparedRuntimeError::Cancelled);
        }
        let result = machine
            .run_entry(program, entry, arguments, options, realm)
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

    fn retain_result(&self, result: PreparedResultBatch) -> PreparedRetainedResult {
        PreparedRetainedResult {
            values: result.values.into_iter().map(Self::value_result).collect(),
            collections: result.collections,
        }
    }

    fn outer_result(&self, outer: CodegenPreparedOuter) -> PreparedOuter {
        match outer {
            CodegenPreparedOuter::Constructor { identity, fields } => PreparedOuter::Constructor {
                identity,
                fields: fields.into_iter().map(Self::value_result).collect(),
            },
        }
    }

    /// Convert one codegen result. The machine's own `ResourceLedger`
    /// already records a newly minted managed handle's realm at the point it
    /// is minted (`retain_top`/`run_entry_retained`/`inspect_outer`), so
    /// there is no bookkeeping to do here.
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

/// Run a closed, non-retained entry once against a freshly parsed and linked
/// artifact, discarding the machine afterward. `cancel` is an
/// externally-owned flag (not a realm): this is a one-shot helper with no
/// session to scope a realm against, so a caller may pre-cancel before this
/// function even constructs a machine (e.g. a request already cancelled
/// before compilation started), or flip it mid-call from another thread --
/// the same contract [`tidepool_codegen::prepared_program::CompiledProgram::run_entry`]
/// preserves for the same reason.
pub fn run_prepared_once(
    artifact: &[u8],
    requirements: &ProgramRequirements,
    limits: DecodeLimits,
    imports: MachineImports,
    cancel: std::sync::Arc<std::sync::atomic::AtomicBool>,
) -> Result<PreparedRunResult, PreparedRuntimeError> {
    if cancel.load(std::sync::atomic::Ordering::Acquire) {
        return Err(PreparedRuntimeError::Cancelled);
    }
    let mut runtime = PreparedRuntime::from_artifact(artifact, requirements, limits, imports)?;
    let program = runtime.ensure_machine()?;
    let entry = runtime.entry_of(program, None)?;
    let machine = runtime.machine_mut()?;
    let result = machine
        .run_entry_with_raw_cancel(
            program,
            entry,
            &[],
            PreparedCallOptions {
                observation_budget: RunOptions::default().observation_budget,
                collect_before_observation: true,
            },
            cancel,
        )
        .map_err(PreparedRuntime::classify_execution)?;
    Ok(PreparedRunResult {
        values: result.values,
        collections: result.collections,
    })
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

        let realm = runtime.open_realm();
        runtime.cancel_handle(realm).unwrap().cancel();
        let replayed = runtime.run_entry(None, &[], false, realm).unwrap_err();
        assert!(matches!(replayed, PreparedRuntimeError::Cancelled));
    }

    #[test]
    fn cancellation_after_compiled_success_does_not_veto_completion() {
        let mut runtime = m3_runtime();
        let realm = runtime.open_realm();

        let program = runtime.first_program().expect("first program installs");
        let entry = runtime
            .entry_of(program, None)
            .expect("first program has an entry");
        let cancel = runtime.cancel_handle(realm).unwrap();
        let result =
            runtime.run_entry_with_completion_hook(program, entry, &[], false, realm, || {
                cancel.cancel();
            });

        assert!(result.is_ok());
        assert!(runtime.cancel_handle(realm).unwrap().is_cancelled());

        let next_realm = runtime.open_realm();
        runtime
            .run_entry(None, &[], false, next_realm)
            .expect("cancellation published after completion must not poison reuse");
    }

    #[test]
    fn prepared_machine_reuses_one_heap_across_settled_entries() {
        let mut runtime = m3_runtime();
        let first = runtime
            .run_entry(None, &[], true, RealmId::ROOT)
            .expect("first prepared entry settles");
        let second = runtime
            .run_entry(None, &[], true, RealmId::ROOT)
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
        runtime
            .run_entry(None, &[], false, RealmId::ROOT)
            .expect("first prepared entry installs the machine");

        let cancelled_realm = runtime.open_realm();
        runtime.cancel_handle(cancelled_realm).unwrap().cancel();
        assert!(matches!(
            runtime.run_entry(None, &[], false, cancelled_realm),
            Err(PreparedRuntimeError::Cancelled)
        ));

        runtime
            .run_entry(None, &[], true, RealmId::ROOT)
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
        // Force the CAF once so it is an evaluated (updated) constructor.
        runtime
            .run_entry(None, &[], true, RealmId::ROOT)
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
            .run_entry_retained_in(consumer, ValueId(0), &[], true, RealmId::ROOT)
            .expect("consumer reads its import through the slot");
        let mut values = read.values;
        let PreparedValueResult::Managed(value) = values.remove(0) else {
            panic!("consumer must return the imported managed value");
        };
        let PreparedOuter::Constructor { identity, fields } = runtime
            .inspect_outer(&value, RealmId::ROOT)
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
        runtime
            .run_entry(None, &[], true, RealmId::ROOT)
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

    // ---- A4: Send, realm-owned leases, cross-realm refusal, holes --------

    /// An entry taking one managed `LiftedRef` argument and returning it
    /// unchanged -- used to exercise a managed argument crossing (or
    /// failing to cross) a realm boundary, which the zero-argument fixtures
    /// above (`producer_program`, `consumer_program`) cannot do.
    fn identity_program() -> PreparedProgram {
        let mut wire = testing::wire_program();
        wire.signatures[0] = Signature {
            arguments: vec![RuntimeRep::LiftedRef],
            results: ResultContract::Returns(vec![RuntimeRep::LiftedRef]),
        };
        wire.expressions.nodes[0] =
            ExprFrame::Return(vec![Atom::Ref(ValueRef::Local(ValueId(50)))]);
        let Group::NonRecursive(top) = &mut wire.bindings[0] else {
            unreachable!()
        };
        top.binding.rhs = HeapRhs::Function {
            signature: SignatureId(0),
            parameters: vec![ValueId(50)],
            captures: vec![],
            body: 0,
        };
        testing::prepare(wire).expect("identity fixture")
    }

    #[test]
    fn prepared_runtime_moves_across_a_thread_boundary_with_a_live_machine() {
        let mut runtime = m3_runtime();
        runtime
            .run_entry(None, &[], true, RealmId::ROOT)
            .expect("first entry runs on the constructing thread");

        let mut runtime = std::thread::spawn(move || {
            runtime
                .run_entry(None, &[], true, RealmId::ROOT)
                .expect("second entry runs on a different thread");
            runtime
        })
        .join()
        .expect("PreparedRuntime crosses the thread boundary intact");

        runtime
            .run_entry(None, &[], true, RealmId::ROOT)
            .expect("runtime carries a still-usable machine back on the original thread");
    }

    #[test]
    fn leases_acquired_under_a_realm_are_released_when_it_closes() {
        let (mut runtime, first) = session();
        let id = runtime
            .bind_top(first, ValueId(0), "producer")
            .expect("producer top binds");
        let realm = runtime.open_realm();
        runtime
            .install_prepared_in(
                consumer_program(false, Some(1)),
                &[(producer_identity(), id)],
                realm,
            )
            .expect("consumer installs under a realm-scoped lease");
        assert_eq!(runtime.bindings().lease_count(id), 1);

        let report = runtime.close_realm_report(realm);
        assert_eq!(
            report.leases_released, 1,
            "the realm's one lease is released"
        );
        assert_eq!(runtime.bindings().lease_count(id), 0);

        runtime
            .release_binding(id)
            .expect("the binding releases once its only lease is gone");
    }

    #[test]
    fn managed_argument_from_another_realm_is_rejected_before_any_machine_call() {
        let (mut runtime, _first) = session();
        let realm_a = runtime.open_realm();
        let produced = runtime
            .run_entry_retained(None, &[], true, realm_a)
            .expect("producer entry runs and retains its result under realm_a");
        let PreparedValueResult::Managed(value) = produced
            .values
            .into_iter()
            .next()
            .expect("the producer entry returns one value")
        else {
            panic!("producer's entry returns a managed value");
        };

        // The producer's own entry takes no arguments; a second program
        // whose entry actually accepts one managed `LiftedRef` is needed to
        // exercise passing `value` as an argument at all.
        let identity = runtime
            .install_prepared_in(identity_program(), &[], realm_a)
            .expect("identity program installs alongside the producer");

        let realm_b = runtime.open_realm();
        let error = match runtime.run_entry_retained_in(
            identity,
            ValueId(0),
            &[PreparedArgument::Managed(&value)],
            false,
            realm_b,
        ) {
            Err(error) => error,
            Ok(_) => {
                panic!("a handle minted under realm_a must not run as an argument under realm_b")
            }
        };
        assert!(matches!(
            error,
            PreparedRuntimeError::CrossRealmArgument { realm } if realm == realm_b
        ));
        assert_eq!(error.kind(), PreparedFailureKind::Rejected);

        // The same handle is still accepted back under its own realm.
        runtime
            .run_entry_retained_in(
                identity,
                ValueId(0),
                &[PreparedArgument::Managed(&value)],
                false,
                realm_a,
            )
            .expect("the refused call left the machine and the handle usable");
    }

    #[test]
    fn parked_realm_reports_liveness_and_clears_on_release() {
        let (mut runtime, _first) = session();
        let realm = runtime.open_realm();
        let produced = runtime
            .run_entry_retained(None, &[], true, realm)
            .expect("producer entry runs and retains its result");
        let PreparedValueResult::Managed(value) = produced
            .values
            .into_iter()
            .next()
            .expect("the producer entry returns one value")
        else {
            panic!("producer's entry returns a managed value");
        };

        let hole = runtime.hole_for(&value, realm);
        assert_eq!(runtime.parked_realm(&hole), Some(realm));

        assert!(runtime.release(value));
        assert_eq!(
            runtime.parked_realm(&hole),
            None,
            "a released handle's hole is no longer live under any realm"
        );
    }
}
