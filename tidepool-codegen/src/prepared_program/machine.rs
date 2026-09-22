//! Persistent ownership for every prepared compiled program installed on one
//! shared heap, each program addressing it through its own root block.
//!
//! `PreparedMachine` owns exactly one `MachineState`, one `VMContext`, one
//! nursery (inside that `MachineState`'s `GcState`), and one `OldSpace`.
//! Every installed program shares all of it. An `InstalledProgram` keeps only
//! its code custody (`CompiledProgram`/`ProgramCustody`) -- its top-level
//! roots live in its own root block (a fixed-address `RootWords`, see
//! `roots.rs`, that the `CompiledProgram` owns and its generated code embeds
//! the address of; there is no machine-wide top table), and its contribution
//! to the shared heap (pinned descriptor layouts, instantiated static image)
//! is folded into the machine-wide `descriptors`/`descriptor_registry`/
//! `statics` sets and the shared `MachineState`'s descriptor space at install
//! time, not kept per-program.
//!
//! This is Path A from the design pass (see `plans/` "S2 restated"): a
//! mediated copy-at-import-time alternative was rejected because a closure
//! applied across programs runs the PRODUCER's code with the CONSUMER's
//! frame live, so the consumer's collector needs the producer's descriptors,
//! statics and stack maps regardless -- the unions this module builds are
//! required either way, and only sharing one heap gives identity (rung 2:
//! "reads the same persistent heap", not a re-imported copy).
//!
//! Every heap-managed value is addressed through the one shared
//! nursery/`OldSpace`, so a value handle produced by one program's code is,
//! structurally, just as usable as an argument to another program's entry as
//! a handle produced by that program itself: there is no "owner" to check. A
//! handle from a genuinely different `PreparedMachine` is rejected because
//! `ValueHandle`s are process-unique (`suspension::ValueHandle::fresh`) and
//! this machine's own `RootHandleLedger` simply never saw it minted --
//! ledger emptiness is what rejects a foreign handle.
//!
//! Install order for the second and later program (the first program's
//! install additionally creates the shared `MachineState`/nursery/`vmctx`/
//! `OldSpace`, since nothing exists yet): instantiate the program's own
//! statics; write its non-heap-top root-block words; extend the shared
//! descriptor space (layouts + static region) and
//! the shared stack-map chain; run a real collection reserving room for this
//! program's heap tops (`collect_on`, safe because no generated frames are
//! live between installs); initialize this program's heap tops into the
//! now-live shared nursery at the current `vmctx.alloc_ptr` (not nursery
//! start -- the earlier program's objects are already there) and advance the
//! cursor; register each as a persistent root; publish. Any failure before
//! publish leaves every already-installed program's roots, root blocks and
//! running state untouched (T3).

use super::roots::{OldSpaceScope, RootWords};
use super::run::{
    heap_top_extent, initialize_heap_tops, register_result_roots, runtime_error,
    runtime_error_for_status, runtime_error_from_machine,
    runtime_error_from_machine_or_observation, runtime_error_without_machine, try_root_words,
    try_words,
};
use super::safepoint::NativeStackBounds;
use super::{
    CompiledProgram, DescriptorMetadata, ExecutionError, ImportShapeFact, RunResult, Unsupported,
};
use crate::context::VMContext;
use crate::descriptor_bridge::{DescriptorMarshalError, DescriptorValue};
use crate::host_fns::{prepared_gc_trigger, RuntimeError};
use crate::machine::CancelHandle;
use crate::machine_state::{MachineDisposition, MachineFailure, MachineState};
use crate::old_space::OldSpace;
use crate::prepared_control::CallStatus;
use crate::resource_ledger::{
    ContinuationFrame, HandleClass, PreparedFrameEvidence, ResourceLedger,
};
use crate::suspension::{ContinuationId, RealmId, ValueHandle};
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::rc::Rc;
use std::sync::atomic::AtomicBool;
use std::sync::Arc;
use tidepool_bridge::HaskellValue;
use tidepool_effect::{EffectRunPolicy, LivePayloadPolicy};
use tidepool_heap::execution_descriptor::ObjectDescriptor;
use tidepool_heap::static_region::StaticRegion;
use tidepool_repr::execution_schema::{ResultContract, RuntimeRep, SymbolIdentity, ValueId};
use tidepool_repr::{DataConId, PrincipalId};

/// A compiled program and its custody. Deliberately !Send: code custody, its
/// VM context, and every live heap root stay on the thread that enters
/// generated code. A later resident owner may stow the whole machine under
/// its existing single-owner protocol; it must not split these fields into
/// independent registries.
enum ProgramCustody<'code> {
    Borrowed(&'code CompiledProgram),
    Owned(Rc<CompiledProgram>),
}

impl ProgramCustody<'_> {
    fn get(&self) -> &CompiledProgram {
        match self {
            Self::Borrowed(program) => program,
            Self::Owned(program) => program,
        }
    }
}

/// Identifies one program installed on a [`PreparedMachine`]. Returned by
/// [`PreparedMachine::new`] and [`PreparedMachine::install_program`]; opaque
/// outside this module so only a machine that actually installed a program
/// can mint the id that later selects it. Ids are issued monotonically per
/// machine and never reused, so a retired program's id stays a typed
/// `UnknownProgram` rather than aliasing a later install.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd, Hash)]
pub struct ProgramId(u32);

/// One installed program's code custody, root block included (the block is
/// a field of the `CompiledProgram`), plus what retirement needs to undo
/// its install: its static region, the descriptor headers only it owns (its
/// thunk, function, PAP and external layouts; interned constructors are
/// shared and ownerless), and its call/enter rows.
struct InstalledProgram<'code> {
    program: ProgramCustody<'code>,
    statics: Arc<StaticRegion>,
    owned_headers: Vec<usize>,
}

/// Proof that the machine is at a quiescent point: no generated frame is
/// live, no temporary root is registered, no observation borrows old space,
/// the heap exists and the machine is reusable. Minted only by
/// [`PreparedMachine::quiesce`] and consumed by the operations that may
/// move or free what running code could otherwise be using. The allocation
/// trigger has no `&mut PreparedMachine` and can never mint one.
#[must_use]
pub struct Quiescent(());

/// What one major collection retired, for the runtime to drain
/// synchronously: it removes the retired programs' facts and leases, and
/// it reads the counts as residency evidence.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RetirementReceipt {
    /// Programs retired, in id order.
    pub programs: Vec<ProgramId>,
    /// Root-block words freed with them.
    pub block_words: usize,
    /// Unreachable programs whose roots were detached but whose metadata a
    /// surviving nursery object still names: garbage the generational
    /// collectors cannot drop in one pass (an old-to-young remembered edge
    /// from dead old space, or a cycle across generations). They stay
    /// installed, rootless, and are retried on the next major collection.
    pub deferred: Vec<ProgramId>,
    /// Prepared old-space bytes in use after the collection: the live
    /// descriptor objects, compacted into one arena. Retiring programs'
    /// blocks are detached before compaction, so their old objects leave in
    /// the same collection.
    pub old_bytes: usize,
    /// Prepared old-space bytes this collection's compaction reclaimed.
    pub compacted_bytes: usize,
}

/// Machine residency counters, each reported separately so a leak in one
/// class cannot hide behind another.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ResidencyCounts {
    pub programs: usize,
    pub block_words: usize,
    pub persistent_roots: usize,
    pub handles: usize,
    /// Installed-program tops retained as machine-lifetime export roots
    /// ([`PreparedMachine::retain_export_top`]), counted apart from
    /// `handles` so neither class hides the other's growth.
    pub code_exports: usize,
    pub parked: usize,
    pub stack_map_links: usize,
    pub static_regions: usize,
    pub descriptor_rows: usize,
    pub callable_rows: usize,
    pub enter_rows: usize,
}

pub struct PreparedMachine<'code> {
    programs: BTreeMap<ProgramId, InstalledProgram<'code>>,
    /// The next id [`Self::install`] mints; never decremented.
    next_program: u32,
    /// The next incremental managed-construction owner. Nodes carry this
    /// identity so a completed node from an ended builder cannot alias the
    /// same root index in a later builder.
    next_builder: u64,
    /// Programs a caller holds live regardless of reachability
    /// ([`Self::pin`]): the install-to-bind gap, and explicit retention.
    pins: BTreeSet<ProgramId>,
    nursery_bytes: usize,
    /// HaskellValue handles, parked continuations AND realm-scoped cancellation
    /// flags for this machine, shared exactly as `PreparedMachine` shares
    /// its own [`ResourceLedger`]. A parked frame ([`Self::park`]) owns its
    /// continuation's root slot as a stowed root until [`Self::take_parked`]
    /// or [`Self::close_realm`]; the rooting receipt
    /// (`stowed_roots_count() == parked_continuations`) holds at every
    /// quiescent point, as on the Core machine.
    handles: ResourceLedger,
    /// The one heap shared by every installed program -- see the module doc.
    machine: Rc<MachineState>,
    vmctx: VMContext,
    old_space: Box<OldSpace>,
    /// Every installed program's instantiated static image, kept alive for
    /// the machine's lifetime. Unioned into `machine`'s descriptor space at
    /// install time (`extend_prepared_descriptors`); observation reads this
    /// slice directly (see `descriptor_region`/`observe.rs`) rather than one
    /// program's own region, so a cross-program static field resolves
    /// through whichever region actually admits it (T4).
    statics: Vec<Arc<StaticRegion>>,
    /// Union of every installed program's pinned descriptor layouts, passed
    /// to `retain_prepared`/`promote_prepared` so a promoted value's
    /// transitive graph is covered no matter which program produced the
    /// objects it reaches.
    descriptors: Vec<Arc<ObjectDescriptor>>,
    /// One descriptor per constructor identity across every installed
    /// program ([`super::DescriptorInterner`]): later programs compile
    /// against it through [`Self::compile_for_install`], so their `Case`,
    /// enter and observation recognise cells an earlier program built.
    interner: super::DescriptorInterner,
    /// Union of every installed program's descriptor registry (constructor
    /// identity/field-representation metadata for non-forcing observation).
    descriptor_registry: BTreeMap<usize, DescriptorMetadata>,
    /// Every installed program's owned descriptor headers, inverted for
    /// [`Self::mark_live_programs`]: which program a live object's header
    /// belongs to. [`Self::install`] inserts a program's
    /// `InstalledProgram::owned_headers`; [`Self::retire`] removes them.
    /// Interned/shared headers are never inserted (no single owner), same as
    /// `owned_headers` itself.
    header_owners: HashMap<usize, ProgramId>,
    /// Lifetime totals of the Cranelift work this machine's installs caused:
    /// functions `define_function` accepted, and the machine-code bytes they
    /// occupy. Never decremented, including by retirement -- they answer
    /// "how much code generation did this machine pay for", not "how much
    /// code is live now" (which is `residency()`'s job). A session that
    /// reuses an earlier unit's code shows a small delta per install; one
    /// that regenerates the same reachable program shows the same large
    /// delta every time.
    compiled_functions: u64,
    compiled_code_bytes: u64,
    /// Each installed program's static region paired with its owner, in
    /// install order, for [`Self::mark_live_programs`]'s
    /// `observation_heap_and_starts` call: a `Traced::Static { region }` hit is an
    /// index into this same list, so it names the owning program directly.
    /// [`Self::install`] pushes; [`Self::retire`] removes the retired
    /// program's entry.
    region_owners: Vec<(ProgramId, Arc<StaticRegion>)>,
}

/// Immutable capacity selected when a prepared machine is created. Root
/// storage needs no capacity here: every program brings its own block.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PreparedMachineOptions {
    pub nursery_bytes: usize,
}

/// Per-entry behavior that does not alter the resident machine's capacity.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PreparedCallOptions {
    pub observation_budget: usize,
    pub collect_before_observation: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PreparedHandle {
    raw: ValueHandle,
    rep: RuntimeRep,
}

/// One completed object owned by an active [`ManagedBuilder`]. The index is
/// meaningful only to that builder and never escapes as a heap address.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ManagedNode {
    node: super::construction::ConstructionNode,
}

/// One logical constructor field for incremental managed construction.
#[derive(Clone, Copy, Debug)]
pub enum ManagedField {
    /// Reusable child reference. Use this for a DAG edge whose source remains
    /// available to later fields or parents.
    Node(ManagedNode),
    /// Transfer a completed tree child into this parent. The child remains
    /// rooted through capacity collection and publication, then becomes
    /// invalid and stops contributing a temporary collector root.
    Consume(ManagedNode),
    Scalar([u8; 16]),
    Handle(PreparedHandle),
}

/// Incremental descriptor-authenticated construction under fixed temporary
/// roots. Each completed object is rooted before another collection can run.
pub struct ManagedBuilder<'machine, 'code> {
    machine: &'machine mut PreparedMachine<'code>,
    core: super::construction::ConstructionCore,
}

impl Drop for ManagedBuilder<'_, '_> {
    fn drop(&mut self) {
        self.core.release(&self.machine.machine);
    }
}

impl<'machine, 'code> ManagedBuilder<'machine, 'code> {
    fn new(machine: &'machine mut PreparedMachine<'code>) -> Result<Self, ExecutionError> {
        machine.ensure_handle_access()?;
        machine
            .handles
            .try_reserve_handles(1)
            .map_err(|_| runtime_error(&machine.machine, RuntimeError::HeapOverflow))?;
        if unsafe { machine.machine.prepared_old_space() }.is_some() {
            return Err(runtime_error(&machine.machine, RuntimeError::BadPointer));
        }
        let owner = machine.next_builder;
        machine.next_builder = machine
            .next_builder
            .checked_add(1)
            .ok_or_else(|| runtime_error(&machine.machine, RuntimeError::HeapOverflow))?;
        let core = super::construction::ConstructionCore::new(&machine.machine, owner);
        Ok(Self { machine, core })
    }

    pub fn bytes(&mut self, bytes: &[u8]) -> Result<ManagedNode, ExecutionError> {
        let descriptor = Arc::clone(
            &self
                .machine
                .interner
                .shared_externals()
                .ok_or(ExecutionError::Invariant(
                    "managed builder: no shared byte-array descriptor",
                ))?
                .bytes_array,
        );
        let machine = &self.machine.machine;
        let old_space = &self.machine.old_space;
        let node = self
            .core
            .bytes(
                machine,
                &mut self.machine.vmctx,
                &descriptor,
                bytes,
                |machine, vmctx, extent| collect_on(machine, vmctx, old_space, extent),
            )
            .map_err(|error| match error {
                super::construction::ConstructionError::Runtime(cause) => {
                    runtime_error(machine, cause)
                }
                super::construction::ConstructionError::Operation(error) => error,
                super::construction::ConstructionError::TooLarge(extent) => {
                    super::answer::AnswerBuildError::TooLarge(extent).into()
                }
                super::construction::ConstructionError::Marshal(error) => {
                    super::answer::AnswerBuildError::Wrapper(error).into()
                }
                super::construction::ConstructionError::Storage(error) => {
                    super::answer::AnswerBuildError::Storage(error).into()
                }
            })?;
        Ok(ManagedNode { node })
    }

    pub fn constructor(
        &mut self,
        host_id: DataConId,
        fields: &[ManagedField],
    ) -> Result<ManagedNode, ExecutionError> {
        let descriptor = Arc::clone(
            self.machine
                .interner
                .by_host(host_id)
                .map(|(_, descriptor)| descriptor)
                .ok_or(super::answer::AnswerBuildError::UnknownConstructor(host_id))?,
        );
        let expected = descriptor.payload().logical_to_stored().len();
        if fields.len() != expected {
            return Err(super::answer::AnswerBuildError::FieldCount {
                host_id,
                expected,
                actual: fields.len(),
            }
            .into());
        }
        if descriptor.allocation_alignment() as usize > 8 {
            return Err(super::answer::AnswerBuildError::Alignment {
                host_id,
                required: descriptor.allocation_alignment(),
            }
            .into());
        }
        let machine = &self.machine.machine;
        let old_space = &self.machine.old_space;
        let handles = &self.machine.handles;
        let mut consumed = Vec::new();
        consumed
            .try_reserve_exact(fields.len())
            .map_err(|_| runtime_error(machine, RuntimeError::HeapOverflow))?;
        for field in fields {
            if let ManagedField::Consume(node) = field {
                if consumed.contains(&node.node) {
                    return Err(runtime_error(machine, RuntimeError::BadPointer));
                }
                self.core.word(node.node).map_err(|error| match error {
                    super::construction::NodeAccessError::Foreign => {
                        super::answer::AnswerBuildError::ForeignNode.into()
                    }
                    super::construction::NodeAccessError::Invalid => {
                        runtime_error(machine, RuntimeError::BadPointer)
                    }
                })?;
                consumed.push(node.node);
            }
        }
        let node = self
            .core
            .constructor(
                machine,
                &mut self.machine.vmctx,
                &descriptor,
                fields.len(),
                &consumed,
                |machine, vmctx, extent| collect_on(machine, vmctx, old_space, extent),
                |core, values| {
                    for (index, field) in fields.iter().enumerate() {
                        values[index] = match field {
                            ManagedField::Node(node) | ManagedField::Consume(node) => core
                                .word(node.node)
                                .map(|word| DescriptorValue::Managed(word as *mut u8))
                                .map_err(|error| match error {
                                    super::construction::NodeAccessError::Foreign => {
                                        super::answer::AnswerBuildError::ForeignNode.into()
                                    }
                                    super::construction::NodeAccessError::Invalid => {
                                        runtime_error(machine, RuntimeError::BadPointer)
                                    }
                                })?,
                            ManagedField::Scalar(bits) => DescriptorValue::Bits(*bits),
                            ManagedField::Handle(handle) => handles
                                .handle(handle.raw)
                                .map(|entry| unsafe { entry.slot.current() } as usize)
                                .filter(|word| *word != 0)
                                .map(|word| DescriptorValue::Managed(word as *mut u8))
                                .ok_or_else(|| {
                                    ExecutionError::from(
                                        super::answer::AnswerBuildError::UnknownHandle,
                                    )
                                })?,
                        };
                    }
                    Ok(())
                },
            )
            .map_err(|error| match error {
                super::construction::ConstructionError::Runtime(cause) => {
                    runtime_error(machine, cause)
                }
                super::construction::ConstructionError::Operation(error) => error,
                super::construction::ConstructionError::TooLarge(extent) => {
                    super::answer::AnswerBuildError::TooLarge(extent).into()
                }
                super::construction::ConstructionError::Marshal(error) => {
                    super::answer::AnswerBuildError::Field {
                        host_id,
                        index: match &error {
                            DescriptorMarshalError::Representation { index, .. }
                            | DescriptorMarshalError::ManagedReference { index } => *index,
                            _ => 0,
                        },
                        error,
                    }
                    .into()
                }
                super::construction::ConstructionError::Storage(error) => {
                    super::answer::AnswerBuildError::Storage(error).into()
                }
            })?;
        Ok(ManagedNode { node })
    }

    #[cfg(test)]
    fn temporary_root_metrics(&self) -> (usize, usize) {
        self.core.root_metrics()
    }

    pub fn finish(
        self,
        realm: RealmId,
        root: ManagedNode,
    ) -> Result<PreparedHandle, ExecutionError> {
        let source = self.core.slot(root.node).map_err(|error| match error {
            super::construction::NodeAccessError::Foreign => {
                super::answer::AnswerBuildError::ForeignNode.into()
            }
            super::construction::NodeAccessError::Invalid => {
                runtime_error(&self.machine.machine, RuntimeError::BadPointer)
            }
        })?;
        let source = source.cast::<*mut u8>();
        unsafe {
            self.machine
                .machine
                .install_prepared_old_space(&self.machine.old_space)
        };
        let retained = unsafe {
            self.machine.old_space.retain_prepared(
                &self.machine.machine,
                &mut self.machine.vmctx,
                &[source],
                &self.machine.descriptors,
            )
        };
        self.machine.machine.clear_prepared_old_space();
        let mut retained = retained.map_err(|cause| runtime_error(&self.machine.machine, cause))?;
        let Some(root) = retained.pop().filter(|_| retained.is_empty()) else {
            for root in retained {
                self.machine.machine.deregister_persistent_root(root.addr());
            }
            return Err(runtime_error(
                &self.machine.machine,
                RuntimeError::BadPointer,
            ));
        };
        let raw = self
            .machine
            .handles
            .insert_handle(root, realm, RuntimeRep::LiftedRef);
        Ok(PreparedHandle {
            raw,
            rep: RuntimeRep::LiftedRef,
        })
    }
}

impl PreparedHandle {
    /// The representation this handle was retained with; what an importing
    /// program's declaration must match.
    #[must_use]
    pub fn rep(&self) -> RuntimeRep {
        self.rep
    }

    /// This handle's opaque cross-engine identity, the same `ValueHandle`
    /// a `suspension::ResumeInput::Handle`/`FramedHandle` delivery carries. A
    /// caller that already holds a `PreparedHandle` (a session binding's own
    /// linking handle, a run result) uses this to deliver it by handle
    /// through the ordinary session resume path, rather than reaching into
    /// the machine a second time.
    #[must_use]
    pub fn raw(&self) -> ValueHandle {
        self.raw
    }
}

/// Caller-owned import resolution for [`PreparedMachine::install_program`]:
/// one live [`PreparedHandle`], retained by this same machine, per declared
/// import identity. A declared import whose identity is absent here is
/// [`ExecutionError::UnknownPreparedHandle`], never a silently-skipped slot.
pub type ImportBindings = BTreeMap<SymbolIdentity, PreparedHandle>;

/// [`PreparedMachine::park`]'s policy/evidence bundle -- grouped so the
/// method stays under clippy's argument-count lint alongside the two
/// identity/root parameters (`continuation`, `realm`) and the payload root
/// `park` itself never derives.
#[derive(Clone, Copy)]
pub struct ParkRequest {
    pub principal: PrincipalId,
    pub effect_policy: EffectRunPolicy,
    pub live_payload: LivePayloadPolicy,
    pub evidence: PreparedFrameEvidence,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PreparedInput {
    Scalar(u64),
    Managed(PreparedHandle),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PreparedResult {
    Void,
    Scalar(u64),
    Managed(PreparedHandle),
}

#[derive(Debug)]
pub struct PreparedResultBatch {
    pub values: Vec<PreparedResult>,
    pub collections: u64,
}

/// One constructor layer read without evaluating any field.
///
/// Managed fields are retained as fresh handles. Callable fields remain
/// opaque: inspection never enters them or invokes generated code.
#[derive(Debug)]
pub enum PreparedOuter {
    Constructor {
        identity: DataConId,
        fields: Vec<PreparedResult>,
    },
}

struct CancelScope<'a>(&'a MachineState);

impl Drop for CancelScope<'_> {
    fn drop(&mut self) {
        self.0.clear_cancel_flag();
    }
}

struct TemporaryRoots<'a> {
    machine: &'a MachineState,
    mark: usize,
}

impl Drop for TemporaryRoots<'_> {
    fn drop(&mut self) {
        self.machine.truncate_rust_roots(self.mark);
    }
}

/// Candidate registrations are unpublished until install returns successfully.
/// Undo only registration state: a completed collection may have moved live
/// objects, whose updated roots and nursery cursor must survive rollback.
/// `block` is the candidate program's root block, which the caller keeps
/// alive until this transaction has dropped: every root registered into it
/// is deregistered here before the block can be freed.
struct InstallTransaction<'a, 'code> {
    machine: &'a mut PreparedMachine<'code>,
    block: (*mut u64, usize),
    owners: Option<tidepool_heap::gc::raw::OwnersMark>,
    stack_maps: usize,
    committed: bool,
}

impl Drop for InstallTransaction<'_, '_> {
    fn drop(&mut self) {
        if !self.committed {
            let (block, words) = self.block;
            let roots: Vec<*mut *mut u8> = (0..words)
                .map(|slot| {
                    // SAFETY: the caller holds the candidate `CompiledProgram`
                    // (and so its block) until this transaction has dropped.
                    unsafe { block.add(slot) }.cast::<*mut u8>()
                })
                .collect();
            self.machine.machine.deregister_persistent_roots(&roots);
            for root in roots {
                unsafe { root.write(std::ptr::null_mut()) };
            }
            while self.machine.machine.stack_map_link_count() > self.stack_maps {
                self.machine.machine.pop_stack_map_registry();
            }
            if let Some(owners) = self.owners.take() {
                self.machine
                    .machine
                    .finish_prepared_descriptor_owners(owners, false);
            } else {
                self.machine.machine.clear_gc_state();
            }
        } else if let Some(owners) = self.owners.take() {
            self.machine
                .machine
                .finish_prepared_descriptor_owners(owners, true);
        }
        self.machine.machine.end_prepared_call();
    }
}

impl PreparedMachine<'static> {
    /// Create a machine and install `program` as its first program, retaining
    /// its mutable heap, static image, top-table range, descriptor registry
    /// and compiled code until this owner drops (or a later
    /// [`PreparedMachine::install_program`] adds another program alongside
    /// it). Single-program callers use the returned [`ProgramId`] with every
    /// `run_entry*` call.
    pub fn new(
        program: CompiledProgram,
        options: PreparedMachineOptions,
    ) -> Result<(Self, ProgramId), ExecutionError> {
        let mut machine = Self::empty(options)?;
        let id = machine.install(
            ProgramCustody::Owned(Rc::new(program)),
            &ImportBindings::new(),
        )?;
        Ok((machine, id))
    }
}

impl<'code> PreparedMachine<'code> {
    /// Temporary compatibility owner for the direct compiled-program API.
    /// Runtime persistence always uses [`PreparedMachine::new`], whose code
    /// custody is owned rather than borrowed.
    pub(crate) fn from_borrowed(
        program: &'code CompiledProgram,
        options: PreparedMachineOptions,
    ) -> Result<(Self, ProgramId), ExecutionError> {
        let mut machine = Self::empty(options)?;
        let id = machine.install(ProgramCustody::Borrowed(program), &ImportBindings::new())?;
        Ok((machine, id))
    }

    /// A machine with no program: nursery, GC state and `vmctx` come into
    /// being with the first install, whichever program that is.
    pub fn empty(options: PreparedMachineOptions) -> Result<Self, ExecutionError> {
        Ok(Self {
            programs: BTreeMap::new(),
            next_program: 0,
            next_builder: 0,
            pins: BTreeSet::new(),
            nursery_bytes: options.nursery_bytes,
            handles: ResourceLedger::default(),
            // No heap exists until the first program installs
            // (`MachineState::new` is plain ambient state -- it owns no GC
            // region yet -- and `VMContext::new` merely stores raw pointers,
            // so a null placeholder is sound until `install` fills it in).
            machine: Rc::new(MachineState::new()),
            vmctx: VMContext::new(std::ptr::null_mut(), std::ptr::null()),
            old_space: Box::new(OldSpace::new()),
            statics: Vec::new(),
            descriptors: Vec::new(),
            descriptor_registry: BTreeMap::new(),
            interner: super::DescriptorInterner::default(),
            header_owners: HashMap::new(),
            region_owners: Vec::new(),
            compiled_functions: 0,
            compiled_code_bytes: 0,
        })
    }

    /// Lifetime Cranelift functions compiled for this machine's installs.
    /// See the `compiled_functions` field doc for how to read a delta.
    #[must_use]
    pub fn compiled_functions(&self) -> u64 {
        self.compiled_functions
    }

    /// Lifetime machine-code bytes generated for this machine's installs.
    #[must_use]
    pub fn compiled_code_bytes(&self) -> u64 {
        self.compiled_code_bytes
    }

    /// Compile a program to install next on this machine: against this
    /// machine's descriptor interner, so every constructor identity an
    /// installed program already declared resolves to the same descriptor
    /// address the existing cells carry. Each compile owns its root block, so
    /// any number of compiles may be outstanding; every one of them compiles
    /// through a clone of this same machine's interner (an overlay sharing
    /// the machine's tables, so the clone copies nothing already interned),
    /// so at install time
    /// [`super::interner::DescriptorInterner::check_absorb`] finds each declaration
    /// already agreeing with what it staged and keeps the existing `Arc`
    /// rather than minting a second one -- a fresh compile's own externals
    /// are likewise absorbed, or, for a genuinely divergent descriptor,
    /// refused as [`ExecutionError::ForeignExternals`], never accepted by a
    /// slot reservation.
    pub fn compile_for_install(
        &mut self,
        linked: &tidepool_repr::execution_schema::LinkedProgram,
    ) -> Result<CompiledProgram, super::CompileError> {
        let mut staged = self.interner.clone();
        // A cheap `Arc::clone` snapshot of the machine's current permanent
        // literal pool: content it already carries resolves to that same
        // address here, so `Case`, enter and observation recognise a
        // literal an earlier program interned. Any content this program
        // newly needs is minted into its own returned `bytes` view (never
        // mutating this snapshot or the machine's live pool directly);
        // `install` folds it into the live pool once this program actually
        // installs (see `Self::install_staged`).
        let existing_bytes = self.machine.interned_bytes();
        CompiledProgram::compile_with(linked, &mut staged, &existing_bytes)
    }

    /// Install one more program on this machine. Its tops live in its own
    /// root block, so no machine-wide capacity is claimed. A transaction
    /// removes candidate roots and metadata on failure. Existing programs
    /// retain their live state, including root updates from any completed
    /// collection.
    /// `imports` resolves every one of `program`'s declared globals by
    /// identity, one live [`PreparedHandle`] retained by THIS machine per
    /// import (an identity absent here is [`ExecutionError::UnknownPreparedHandle`]).
    /// Every import is verified -- handle known, representation matches, and
    /// (when the declaration requires it) the referenced value already
    /// resolves to a settled, evaluated constructor -- before anything is
    /// written; a single bad import leaves the table, the roots and every
    /// already-installed program exactly as before, same as a capacity or
    /// heap-reserve failure (see [`Self::install`]'s "reserve, verify all,
    /// then publish" ordering).
    pub fn install_program(
        &mut self,
        program: CompiledProgram,
        imports: ImportBindings,
    ) -> Result<ProgramId, ExecutionError> {
        self.install(ProgramCustody::Owned(Rc::new(program)), &imports)
    }

    fn install(
        &mut self,
        program: ProgramCustody<'code>,
        imports: &ImportBindings,
    ) -> Result<ProgramId, ExecutionError> {
        self.machine
            .begin_prepared_call()
            .map_err(ExecutionError::Runtime)?;
        let owners = self.machine.mark_prepared_descriptor_owners();
        let stack_maps = self.machine.stack_map_link_count();
        let block = {
            let block = &program.get().root_block;
            (block.as_mut_ptr(), block.len())
        };
        let mut transaction = InstallTransaction {
            machine: self,
            block,
            owners,
            stack_maps,
            committed: false,
        };
        let staged = transaction.machine.install_staged(program.get(), imports);
        transaction.committed = staged.is_ok();
        // Rollback (if any) deregisters the candidate block's roots while
        // `program` still owns the block.
        drop(transaction);
        let statics = staged?;
        let compiled = program.get();
        // Shared descriptors (interned constructors, external wrappers) have
        // no owner; retiring this program leaves them.
        let shared: HashSet<usize> = compiled
            .interned_constructors
            .iter()
            .map(|(_, descriptor)| descriptor.initial_header_word())
            .chain(compiled.externals.headers())
            .collect();
        let owned_headers: Vec<usize> = compiled
            .descriptors
            .iter()
            .map(|descriptor| descriptor.initial_header_word())
            .filter(|header| !shared.contains(header))
            .collect();
        self.compiled_functions += compiled.pipeline.functions_defined();
        self.compiled_code_bytes += compiled.pipeline.code_bytes();
        let id = ProgramId(self.next_program);
        self.next_program += 1;
        self.header_owners
            .extend(owned_headers.iter().map(|&header| (header, id)));
        self.region_owners.push((id, Arc::clone(&statics)));
        self.programs.insert(
            id,
            InstalledProgram {
                program,
                statics,
                owned_headers,
            },
        );
        Ok(id)
    }

    fn install_staged(
        &mut self,
        compiled: &CompiledProgram,
        imports: &ImportBindings,
    ) -> Result<Arc<StaticRegion>, ExecutionError> {
        // A constructor identity this machine already shares must be
        // declared identically, with the same descriptor, by the incoming
        // program; otherwise nothing is absorbed and nothing else happens.
        // Validate only: the machine's interner is extended at the very end,
        // after every fallible step, so a refused install leaves it as is.
        self.interner
            .check_absorb(&compiled.interned_constructors)
            .map_err(|conflict| match conflict {
                super::interner::AbsorbConflict::Identity { existing, incoming } => {
                    ExecutionError::DescriptorShape {
                        identity: Box::new(incoming.identity.clone()),
                        existing_field_reps: existing.field_reps.clone(),
                        incoming_field_reps: incoming.field_reps.clone(),
                    }
                }
                super::interner::AbsorbConflict::HostId {
                    host_id,
                    identity,
                    existing,
                } => ExecutionError::HostIdConflict {
                    host_id,
                    identity,
                    existing,
                },
                super::interner::AbsorbConflict::Externals => ExecutionError::ForeignExternals,
            })?;
        self.interner
            .check_externals(&compiled.externals)
            .map_err(|_| ExecutionError::ForeignExternals)?;

        // Reserve (capacity/contiguity, above) then verify EVERY declared
        // import before any other install side effect -- no statics
        // instantiated, no descriptor/stack-map union extended, no heap
        // touched. A bad import must look, from every already-installed
        // program's perspective, exactly like an install that never
        // happened. `link_program` already proved identity/signature/
        // generation agreement for each declaration; only the runtime shape
        // of the actual handle this caller supplied remains to check here.
        // The handle's CURRENT pointer is deliberately NOT cached here: the
        // second-or-later-program branch below runs its own `collect_on` to
        // reserve heap-top room, which can relocate this very object (it is
        // already reachable, and therefore a legitimate root, through
        // whichever program produced it) -- publishing must re-read each
        // handle's slot fresh, after every GC-triggering step, or the
        // published cell would carry a pointer stale by exactly that
        // collection.
        let mut resolved_imports: Vec<(usize, ValueHandle)> =
            Vec::with_capacity(compiled.import_slots.len());
        // Pass 1, no heap access: every declared import must name a handle
        // this machine retains, of the declared representation, with a
        // live pointer. On an EMPTY machine (first program) the ledger has
        // no handles, so a program with any import is refused here as
        // `UnknownPreparedHandle` -- before the evaluatedness pass below
        // could ask for an observation heap that does not exist yet (which
        // would report `BadPointer` and latch the machine Unavailable for
        // what is a caller error).
        let mut evaluated_checks: Vec<(usize, &super::plan::ImportSlot)> = Vec::new();
        for slot in &compiled.import_slots {
            let handle = imports
                .get(&slot.identity)
                .copied()
                .ok_or(ExecutionError::UnknownPreparedHandle)?;
            if handle.rep != slot.rep {
                return Err(ExecutionError::ImportShape {
                    identity: Box::new(slot.identity.clone()),
                    expected: ImportShapeFact::Representation(slot.rep),
                    found: ImportShapeFact::Representation(handle.rep),
                });
            }
            let entry = self
                .handles
                .handle(handle.raw)
                .ok_or(ExecutionError::UnknownPreparedHandle)?;
            let pointer = unsafe { entry.slot.current() } as usize;
            if pointer == 0 {
                return Err(ExecutionError::UnknownPreparedHandle);
            }
            if slot.required_evaluated {
                evaluated_checks.push((pointer, slot));
            }
            resolved_imports.push((slot.slot, handle.raw));
        }
        // Pass 2, one observation heap built only if some verified import
        // actually needs the evaluatedness check -- most installs need no
        // heap read at all.
        if !evaluated_checks.is_empty() {
            let heap = self.observation_heap()?;
            for (pointer, slot) in evaluated_checks {
                if !heap.resolves_to_whnf_value(pointer)? {
                    return Err(ExecutionError::ImportShape {
                        identity: Box::new(slot.identity.clone()),
                        expected: ImportShapeFact::Evaluated(true),
                        found: ImportShapeFact::Evaluated(false),
                    });
                }
            }
        }

        let statics = Arc::new(compiled.statics.instantiate()?);
        let block = &compiled.root_block;
        let heap_tops: HashSet<_> = compiled.heap_top_specs.iter().map(|spec| spec.id).collect();
        for (&id, &slot) in &compiled.top_slots {
            if heap_tops.contains(&id) {
                continue;
            }
            let value = statics
                .entry(id)
                .or_else(|| {
                    compiled
                        .byte_tops
                        .get(&id)
                        .map(|bytes| bytes.as_ptr() as usize)
                })
                .ok_or(ExecutionError::MissingEntry(id))?;
            block.write(slot, value as u64)?;
        }

        let heap_reserve = heap_top_extent(&compiled.heap_top_specs)?;
        // The heap, not the program table, says whether this is the first
        // install: retirement can empty the table while the heap (handles,
        // old space, nursery) lives on.
        if self.machine.gc_active_range().is_none() {
            // First program: nothing exists yet. Create the shared nursery
            // and GcState from scratch, exactly as a single-program machine
            // always did; every later program extends this same heap instead
            // (the branch below).
            let nursery = try_words(
                self.nursery_bytes
                    .max(heap_reserve)
                    .div_ceil(std::mem::size_of::<u64>()),
            )?;
            self.machine
                .set_stack_map_registry(&compiled.pipeline.stack_maps);
            if let Err(error) = self.machine.install_prepared_buffer_with_static_region(
                nursery,
                compiled.descriptors.clone(),
                Some(Arc::clone(&statics)),
            ) {
                return Err(runtime_error(&self.machine, error));
            }
            let (start, size) = match self.machine.gc_active_range() {
                Some(range) => range,
                None => {
                    // `install_prepared_buffer_with_static_region` just
                    // returned `Ok`, which unconditionally sets this
                    // machine's `GcState` before returning -- reaching here
                    // means that postcondition did not hold, not that
                    // anything already on the heap is untrustworthy.
                    return Err(ExecutionError::Invariant(
                        "install: heap buffer installed with no active range",
                    ));
                }
            };
            // Publish every verified import before this program's own heap
            // tops initialize: a heap top's `ValueRef::Global` field can now
            // only resolve by reading the import's word in the program's
            // root block (see `run::write_atoms`'s `Global` arm), so it must
            // already hold the import's live pointer by the time
            // `initialize_heap_tops` runs. `resolved_imports` is always
            // empty on this branch (the first program on a machine is
            // always installed with no imports -- see `PreparedMachine::new`/
            // `from_borrowed`), so this loop is a defensive no-op here, kept
            // symmetric with the second-program branch below.
            self.publish_imports(&resolved_imports, block)?;
            let heap_used = match initialize_heap_tops(
                start,
                size,
                &compiled.heap_top_specs,
                &compiled.top_slots,
                block,
                &statics,
                &compiled.byte_tops,
                &compiled.bytes,
                &compiled.import_slots,
            ) {
                Ok(heap_used) => heap_used,
                Err(cause) => {
                    return Err(runtime_error(&self.machine, cause));
                }
            };
            self.vmctx.alloc_ptr = unsafe { start.add(heap_used) };
            self.vmctx.alloc_limit = unsafe { start.add(size) };
            self.vmctx.machine_state = Rc::as_ptr(&self.machine).cast_mut();
        } else {
            // Second-and-later program: the shared heap is already live,
            // possibly holding an earlier program's persistent objects. Union
            // this program's layouts/static image into the same descriptor
            // space and stack-map chain (never swap), then reserve room in
            // the LIVE nursery via a real collection -- sound because no
            // generated frames are live between installs -- before writing
            // this program's own heap tops at the current cursor (not
            // nursery start).
            self.machine
                .push_stack_map_registry(&compiled.pipeline.stack_maps);
            if let Err(error) = self
                .machine
                .extend_prepared_descriptors(compiled.descriptors.clone(), Arc::clone(&statics))
            {
                return Err(runtime_error(&self.machine, error));
            }
            // Publish every verified import HERE, before `collect_on`: each
            // published slot is registered as a persistent root below, so
            // the collection just after this can (and, for a cross-program
            // import, generally will) relocate the object it points at --
            // registering the root first is exactly what lets a persistent
            // root survive a collection at all (the same mechanism this
            // program's OWN heap tops rely on once THEY are registered,
            // just below). The published pointer is each handle's CURRENT
            // one (re-read here, not the value observed during the earlier
            // verification pass above `statics.instantiate()` -- nothing
            // between that verification and here can move it, but nothing
            // guarantees that stays true indefinitely, so this still reads
            // fresh rather than trusting a stale local). Once `collect_on`
            // runs, the registered root's slot is updated in place to the
            // post-collection address, so `initialize_heap_tops` below (which
            // resolves a `ValueRef::Global` field by reading this same slot,
            // see `run::write_atoms`) always observes the correct address --
            // it no longer needs imports published AFTER collection, because
            // it is no longer imports' own relocation this ordering protects
            // against: it is a heap top's *own* pointer to the import, which
            // does not exist yet until `initialize_heap_tops` writes it, so
            // there is nothing of this program's to go stale.
            self.publish_imports(&resolved_imports, block)?;
            collect_on(
                &self.machine,
                &mut self.vmctx,
                &self.old_space,
                heap_reserve,
            )?;
            let (start, size) = match self.machine.gc_active_range() {
                Some(range) => range,
                None => {
                    return Err(runtime_error(&self.machine, RuntimeError::BadPointer));
                }
            };
            let cursor = match (self.vmctx.alloc_ptr as usize)
                .checked_sub(start as usize)
                .filter(|cursor| *cursor <= size)
            {
                Some(cursor) => cursor,
                None => {
                    return Err(runtime_error(&self.machine, RuntimeError::BadPointer));
                }
            };
            let remaining = size - cursor;
            let heap_used = match initialize_heap_tops(
                self.vmctx.alloc_ptr,
                remaining,
                &compiled.heap_top_specs,
                &compiled.top_slots,
                block,
                &statics,
                &compiled.byte_tops,
                &compiled.bytes,
                &compiled.import_slots,
            ) {
                Ok(heap_used) => heap_used,
                Err(cause) => {
                    return Err(runtime_error(&self.machine, cause));
                }
            };
            self.vmctx.alloc_ptr = unsafe { self.vmctx.alloc_ptr.add(heap_used) };
        }

        // Heap tops persist with the machine. They must not share the
        // run-scoped registry that a call frame truncates on native unwind.
        for spec in &compiled.heap_top_specs {
            if let Some(root) = compiled
                .top_slots
                .get(&spec.id)
                .and_then(|&slot| block.slot_address(slot))
            {
                self.machine.register_persistent_root(root);
            }
        }

        // Every verified import was already published above (before this
        // program's own heap tops initialized -- see the per-branch comments
        // above), so there is nothing left to publish here.

        self.statics.push(Arc::clone(&statics));
        // Interned constructor layouts are shared by every program that
        // declares them and are never retired, so union by header identity:
        // a plain extend would grow this list by each install's shared
        // layouts forever, and promotion and compaction walk all of it.
        // `descriptor_registry` is keyed by exactly the headers of
        // `descriptors` (every program registers each descriptor it lists,
        // and retirement removes the same owned set from both), so it is the
        // membership index; consult it before this program's rows join it.
        debug_assert_eq!(self.descriptors.len(), self.descriptor_registry.len());
        let registry = &self.descriptor_registry;
        self.descriptors.extend(
            compiled
                .descriptors
                .iter()
                .filter(|descriptor| !registry.contains_key(&descriptor.initial_header_word()))
                .cloned(),
        );
        self.descriptor_registry.extend(
            compiled
                .descriptor_registry
                .iter()
                .map(|(&k, v)| (k, v.clone())),
        );

        self.machine.register_prepared_constructors(
            compiled
                .descriptor_registry
                .iter()
                .filter_map(|(&header, metadata)| match &metadata.meaning {
                    super::DescriptorMeaning::Constructor(observation) => {
                        Some((header, observation.identity))
                    }
                    _ => None,
                }),
        );
        // Last step: register this program's cross-program call/enter
        // resolution entries. Nothing after this point can fail, so no
        // rollback path needs to touch these registrations.
        self.machine.register_prepared_entries(
            compiled.callables.iter().map(|c| {
                (
                    c.header,
                    c.signature.clone(),
                    compiled.pipeline.get_function_ptr(c.function),
                )
            }),
            compiled
                .thunk_entries
                .iter()
                .map(|&(header, function)| (header, compiled.pipeline.get_function_ptr(function))),
        );

        self.interner.commit_absorb(&compiled.interned_constructors);
        self.interner.commit_externals(&compiled.externals);
        // Fold this program's own literal view into the one permanent
        // machine-wide pool. A compile against this same machine's
        // `interned_bytes()` snapshot (`compile_for_install`) already
        // reused every address the machine already had, so this is a cheap
        // no-op union for content already shared; a standalone
        // `CompiledProgram::compile` (no machine involved) contributes its
        // own freshly minted addresses here for the first time. Placed
        // last, alongside the other commits above, because nothing here can
        // fail -- but the choice is not load-bearing: every entry absorbed
        // is an `Arc<[u8]>` this `CompiledProgram` already owns, so even a
        // failed install earlier in this function could not have left a
        // dangling address, only (harmlessly) an uninstalled program's
        // literals never reaching the pool at all.
        self.machine.absorb_interned_bytes(&compiled.bytes);
        Ok(statics)
    }

    // --- residency: pins, quiescence, major collection, retirement --------

    /// Hold `program` live until [`Self::unpin`], whatever reaches it. This
    /// is the host's facility: closing the install-to-first-binding gap is
    /// its responsibility once it calls this, not something the runtime
    /// does on a caller's behalf today -- no runtime caller pins yet, so
    /// that integration is still pending. A test pins what it means to keep.
    pub fn pin(&mut self, program: ProgramId) -> Result<(), ExecutionError> {
        if !self.programs.contains_key(&program) {
            return Err(ExecutionError::UnknownProgram(program));
        }
        self.pins.insert(program);
        Ok(())
    }

    /// Release a pin. `false` if it was not held.
    pub fn unpin(&mut self, program: ProgramId) -> bool {
        self.pins.remove(&program)
    }

    /// Prove the machine is quiescent (see [`Quiescent`]). A typed refusal
    /// otherwise: a major collection or retirement must never run while
    /// generated frames, temporary roots or an observation borrow could be
    /// looking at what it moves or frees.
    pub fn quiesce(&self) -> Result<Quiescent, ExecutionError> {
        if self.machine.disposition() != MachineDisposition::Reusable {
            return Err(self.machine.last_failure().map_or(
                ExecutionError::Invariant(
                    "quiesce: machine is not Reusable but no failure was recorded",
                ),
                ExecutionError::Runtime,
            ));
        }
        if self.machine.rust_roots_len() != 0
            || unsafe { self.machine.prepared_old_space() }.is_some()
            || self.machine.gc_active_range().is_none()
        {
            return Err(ExecutionError::NotQuiescent);
        }
        Ok(Quiescent(()))
    }

    /// The residency counters at this quiescent point.
    #[must_use]
    pub fn residency(&self) -> ResidencyCounts {
        let (callable_rows, enter_rows) = self.machine.prepared_entry_rows();
        ResidencyCounts {
            programs: self.programs.len(),
            block_words: self
                .programs
                .values()
                .map(|installed| installed.program.get().root_block.len())
                .sum(),
            persistent_roots: self.machine.persistent_roots_count(),
            handles: self.handle_count(),
            code_exports: self.handles.counts().code_exports,
            parked: self.parked_count(),
            stack_map_links: self.machine.stack_map_link_count(),
            static_regions: self.statics.len(),
            descriptor_rows: self.descriptor_registry.len(),
            callable_rows,
            enter_rows,
        }
    }

    /// Live prepared old-space bytes right now -- the same figure
    /// [`Self::collect_major`] reads into `RetirementReceipt::old_bytes`,
    /// but callable at any point rather than only right after a completed
    /// collection. `residency().block_words` only grows with installs (root
    /// blocks are never shrunk in place), so it cannot see promotion that
    /// happens between installs; this reads the old space's own live byte
    /// count instead.
    #[must_use]
    pub fn old_bytes_live(&self) -> usize {
        self.old_space.prepared_bytes_used()
    }

    /// Mark the live programs and retire the rest.
    ///
    /// A program is live when the mark reaches it from a handle, a parked
    /// frame (its evidence owner and runner included), a pin, or a live
    /// program's root block: through a live object whose header is a
    /// descriptor the program owns, or a reference into its static image
    /// (lifetime contract decision 1). The mark is non-moving and forces
    /// nothing; ordinary collection runs first so it sees a compact nursery.
    /// A program's own literal-bytes storage is no longer a liveness edge
    /// (edge (c)): every literal is interned into the one permanent
    /// machine-wide pool at install (see
    /// `crate::machine_state::MachineState::absorb_interned_bytes`), so a
    /// program with no other root is retired exactly like one with none.
    ///
    /// Retirement is split around the collectors (decision 7): every
    /// unreachable program's block roots are detached first, so one ordinary
    /// collection drops the nursery objects only those blocks reached, and
    /// compaction then drops their old objects in the same pass. A second
    /// ordinary collection drops nursery objects that only a now-compacted
    /// dead old object remembered. Every step that can fail runs while every
    /// descriptor is still installed. A retiring program a surviving nursery
    /// object still names is deferred, never released: the next ordinary
    /// collection validates every nursery start against the installed
    /// descriptor space. The remaining programs' metadata is then released;
    /// the receipt is what the runtime drains.
    pub fn collect_major(&mut self, token: Quiescent) -> Result<RetirementReceipt, ExecutionError> {
        drop(token);
        // Re-check: the token proves the caller went through the gate, and
        // nothing may have changed between the two calls.
        let still_quiescent = self.quiesce()?;
        collect_on(&self.machine, &mut self.vmctx, &self.old_space, 0)?;
        let live = self.mark_live_programs(false)?;
        let retiring: BTreeSet<ProgramId> = self
            .programs
            .keys()
            .copied()
            .filter(|id| !live.contains(id))
            .collect();
        let mut receipt = RetirementReceipt::default();
        // 1. Detach, then collect: the nursery keeps nothing only a
        //    retiring block reached.
        for id in &retiring {
            self.detach_roots(*id)?;
        }
        if !retiring.is_empty() {
            collect_on(&self.machine, &mut self.vmctx, &self.old_space, 0)?;
        }
        // Compaction commits before any metadata is released: every
        // descriptor a nursery or old object may carry is still installed.
        receipt.compacted_bytes = self
            .compact_old_space(&still_quiescent, &retiring)?
            .reclaimed_bytes;
        let mut releasing = retiring;
        if !releasing.is_empty() {
            collect_on(&self.machine, &mut self.vmctx, &self.old_space, 0)?;
            let named_by_nursery = self.mark_live_programs(true)?;
            releasing.retain(|id| {
                let deferred = named_by_nursery.contains(id);
                if deferred {
                    receipt.deferred.push(*id);
                }
                !deferred
            });
            // `release_metadata` fails only on these, so check them once
            // before the first descriptor leaves.
            self.machine
                .check_prepared_descriptor_space()
                .map_err(|cause| runtime_error(&self.machine, cause))?;
        }
        // 2-8. Nothing below can fail.
        for id in releasing {
            receipt.block_words += self.release_metadata(id);
            receipt.programs.push(id);
        }
        receipt.old_bytes = self.old_space.prepared_bytes_used();
        self.assert_rooting_receipt();
        Ok(receipt)
    }

    /// Compact the prepared descriptor arenas into one arena holding exactly
    /// the objects reachable from the machine's roots, every non-`retiring`
    /// installed program's root block, and the live nursery. Only a major
    /// collection calls this, after ordinary collection, under its token.
    fn compact_old_space(
        &mut self,
        _quiescent: &Quiescent,
        retiring: &BTreeSet<ProgramId>,
    ) -> Result<crate::old_space::PreparedCompactionStats, ExecutionError> {
        let mut block_roots = Vec::new();
        for (id, installed) in &self.programs {
            if retiring.contains(id) {
                continue;
            }
            let block = &installed.program.get().root_block;
            block_roots.extend(
                installed
                    .reference_slots()
                    .filter_map(|slot| block.slot_address(slot)),
            );
        }
        // SAFETY: the token proves no generated frame, temporary root or
        // observation borrow exists; machine, vmctx and old space are this
        // machine's own, installed for the whole call.
        unsafe {
            self.old_space.compact_prepared(
                &self.machine,
                &mut self.vmctx,
                &block_roots,
                &self.descriptors,
            )
        }
        .map_err(|cause| runtime_error(&self.machine, cause))
    }

    /// The set of live programs, by a worklist mark over handles, frames,
    /// pins and live programs' root blocks. With `from_nursery`, every
    /// nursery object is a seed too: the result then also names each
    /// program a surviving nursery object reaches.
    ///
    /// `header_owners` and `region_owners` are machine fields maintained
    /// incrementally by [`Self::install`]/[`Self::retire`], not rebuilt here:
    /// a collection touches only the programs whose liveness this worklist
    /// actually needs to snapshot, not every installed program's bookkeeping.
    fn mark_live_programs(
        &self,
        from_nursery: bool,
    ) -> Result<BTreeSet<ProgramId>, ExecutionError> {
        let regions: Vec<Arc<StaticRegion>> = self
            .region_owners
            .iter()
            .map(|(_, region)| Arc::clone(region))
            .collect();
        let (heap, nursery_base, nursery_starts) = self.observation_heap_and_starts(&regions)?;

        let mut live: BTreeSet<ProgramId> = self.pins.iter().copied().collect();
        // One worklist of program ids still needing their root block
        // snapshotted into `work`, seeded with the programs live from the
        // start (pins); a program discovered live later by the trace below
        // is pushed here exactly once (`live.insert` guards it), never
        // re-snapshotted.
        let mut program_work: Vec<ProgramId> = live.iter().copied().collect();
        let mut work: Vec<usize> = self
            .handles
            .handle_slots()
            .map(|slot| unsafe { slot.current() } as usize)
            .collect();
        if from_nursery {
            for (index, bits) in nursery_starts.iter().enumerate() {
                let mut bits = *bits;
                while bits != 0 {
                    let word = index * 64 + bits.trailing_zeros() as usize;
                    work.push(nursery_base + word * std::mem::size_of::<u64>());
                    bits &= bits - 1;
                }
            }
        }
        for (root, evidence) in self.handles.frame_roots() {
            work.push(unsafe { root.read() } as usize);
            if let Some(evidence) = evidence {
                live.insert(evidence.owner);
                live.insert(evidence.runner);
            }
        }
        let mut visited: HashSet<usize> = HashSet::new();
        loop {
            if let Some(id) = program_work.pop() {
                if let Some(installed) = self.programs.get(&id) {
                    let block = installed.program.get().root_block.snapshot();
                    work.extend(installed.reference_slots().map(|slot| block[slot] as usize));
                }
                continue;
            }
            let Some(word) = work.pop() else {
                break;
            };
            if word == 0 || !visited.insert(tidepool_heap::managed_reference::untag(word)) {
                continue;
            }
            let reached = match heap.trace_step(word)? {
                super::observe::Traced::Static { region } => {
                    Some(self.region_owners.get(region).map(|(id, _)| *id).ok_or(
                        ExecutionError::Invariant(
                            "mark_live_programs: a static hit named a region index outside the \
                             observation heap's own region list",
                        ),
                    )?)
                }
                super::observe::Traced::Object { header, children } => {
                    work.extend(children);
                    self.header_owners.get(&header).copied()
                }
            };
            if let Some(program) = reached {
                if live.insert(program) {
                    program_work.push(program);
                }
            }
        }
        Ok(live)
    }

    /// Retirement step 1 for one unreachable program: deregister and zero
    /// its block roots and forget remembered slots inside its block and
    /// statics. The program stays installed; its objects become garbage for
    /// the next collection. Idempotent, so a deferred program detaches again.
    fn detach_roots(&self, id: ProgramId) -> Result<(), ExecutionError> {
        let installed = self
            .programs
            .get(&id)
            .ok_or(ExecutionError::UnknownProgram(id))?;
        let block = &installed.program.get().root_block;
        let mut roots = Vec::with_capacity(block.len());
        for slot in 0..block.len() {
            if let Some(root) = block.slot_address(slot) {
                roots.push(root);
            }
        }
        self.machine.deregister_persistent_roots(&roots);
        for root in roots {
            unsafe { root.write(std::ptr::null_mut()) };
        }
        let block_start = block.as_mut_ptr().cast::<u8>().cast_const();
        self.machine.forget_remembered_range(block_start, unsafe {
            block_start.add(block.len() * std::mem::size_of::<u64>())
        });
        let range = installed.statics.address_range();
        self.machine
            .forget_remembered_range(range.start as *const u8, range.end as *const u8);
        Ok(())
    }

    /// Retirement steps 2-8 for one detached program no heap object names:
    /// release its rows, descriptors, stack maps, statics and code; returns
    /// the root-block words freed. Infallible: the caller checked the
    /// descriptor space is installed, and an unknown id releases nothing.
    fn release_metadata(&mut self, id: ProgramId) -> usize {
        let Some(installed) = self.programs.remove(&id) else {
            return 0;
        };
        self.pins.remove(&id);
        let compiled = installed.program.get();
        let block = &compiled.root_block;
        // 2. Call and enter rows.
        self.machine
            .retire_prepared_entries(&compiled.dispatch_owned_headers);
        // 3. Descriptor rows and the descriptor space: only what this program
        //    owned; interned constructors stay shared.
        let owned: HashSet<usize> = installed.owned_headers.iter().copied().collect();
        self.descriptors
            .retain(|descriptor| !owned.contains(&descriptor.initial_header_word()));
        for header in &owned {
            self.descriptor_registry.remove(header);
            self.header_owners.remove(header);
        }
        self.machine
            .retire_prepared_constructors(&installed.owned_headers);
        self.machine
            .retire_prepared_descriptors(&installed.owned_headers, &installed.statics);
        // 4. Stack maps, by identity.
        self.machine
            .remove_stack_map_registry(&compiled.pipeline.stack_maps);
        // 5. Static region. This program's own literal bytes are NOT
        //    removed from the machine's permanent pool: they are interned
        //    forever (see `MachineState::absorb_interned_bytes`), since
        //    another still-installed program's code may share the same
        //    address and, unlike a static region, an interned literal
        //    cannot be proven ownerless by header identity alone.
        self.statics
            .retain(|region| !Arc::ptr_eq(region, &installed.statics));
        self.region_owners.retain(|(owner, _)| *owner != id);
        // 6-8. The block, the receipt entry and the code go with `installed`.
        block.len()
    }

    /// Publish each verified import into the candidate's root block as a
    /// persistent root before collection or heap-top initialization. The
    /// install transaction deregisters and zeros the whole block if any
    /// subsequent step fails.
    fn publish_imports(
        &self,
        resolved_imports: &[(usize, ValueHandle)],
        block: &RootWords,
    ) -> Result<(), ExecutionError> {
        for &(slot, raw) in resolved_imports {
            let pointer = self
                .handles
                .handle(raw)
                .map(|entry| unsafe { entry.slot.current() } as u64)
                .ok_or(ExecutionError::UnknownPreparedHandle)?;
            block.write(slot, pointer)?;
            let root = block
                .slot_address(slot)
                .ok_or(ExecutionError::UnknownPreparedHandle)?;
            self.machine.register_persistent_root(root);
        }
        Ok(())
    }

    #[must_use]
    pub fn disposition(&self) -> MachineDisposition {
        self.machine.disposition()
    }

    /// The machine latch: the first integrity failure, if any. A reusable
    /// failure (cancellation, a language error, an unresolved cross-program
    /// callee) is reported by the call that hit it and never latches here,
    /// so this is `None` on a reusable machine whatever its calls did.
    #[must_use]
    pub fn failure(&self) -> Option<MachineFailure> {
        self.machine.last_failure()
    }

    /// Number of value handles this machine currently retains. Diagnostic
    /// surface only, mirroring `JitMachine::value_handle_count`: it exists so
    /// a caller can confirm every retained `PreparedHandle` was released.
    #[must_use]
    pub fn handle_count(&self) -> usize {
        self.handles.counts().value_handles
    }

    /// Release one retained managed result. Unknown or foreign values do not
    /// expose a slot and therefore cannot affect a later entry. A handle from
    /// a genuinely different `PreparedMachine` is rejected here: `ValueHandle`
    /// ids are process-unique, so this machine's ledger simply never saw it
    /// minted (see the module doc).
    pub fn release(&mut self, handle: PreparedHandle) -> bool {
        let Some(entry) = self.handles.take_handle(handle.raw) else {
            return false;
        };
        self.machine.deregister_persistent_root(entry.slot.addr());
        true
    }

    /// [`Self::release`], but by the bare cross-engine [`ValueHandle`] id
    /// rather than a rep-carrying [`PreparedHandle`] -- `take_handle` never
    /// needed the rep (see [`Self::release`]'s body), so a caller holding
    /// only the raw id (a dropped [`crate::suspension::RootCustody`]'s
    /// deferred cleanup, the one case that crosses the runtime session
    /// boundary without its `PreparedHandle` wrapper) can still release it.
    /// Mirrors `PreparedMachine::discard_handle` minus that machine's
    /// immediate `collect_retired_storage` call: the prepared route reclaims
    /// retired storage only at its own quiescent boundary
    /// (`PreparedEngine::quiesce_and_collect`), never inline on release.
    pub fn discard_handle(&mut self, handle: ValueHandle) -> bool {
        let Some(entry) = self.handles.take_handle(handle) else {
            return false;
        };
        self.machine.deregister_persistent_root(entry.slot.addr());
        true
    }

    /// Move a live handle to another runtime resource scope; the root and the
    /// handle id are unchanged. `false` for a handle this machine never
    /// minted or already released.
    pub fn rehome_handle(&mut self, handle: ValueHandle, owner: RealmId) -> bool {
        self.handles.rehome_handle(handle, owner)
    }

    /// Obtain a clone-able cancellation handle scoped to ONE runtime
    /// resource scope, lazily minting that scope's flag on first request.
    /// Cancelling this handle aborts only runs/calls made with `realm` --
    /// a sibling realm's run on the same machine is unaffected, because
    /// [`Self::run_entry`]/[`Self::run_entry_retained`] install the ACTIVE
    /// call's flag (via [`ResourceLedger::cancel_flag`]) into the shared
    /// `MachineState`, not a machine-wide flag.
    ///
    /// A cancelled realm's flag is NOT auto-cleared after the cancelled run
    /// completes -- same discipline as `PreparedMachine`'s own
    /// [`CancelHandle`] (whose own doc says "call `reset` between runs if
    /// you intend to reuse"): the caller decides when a realm is done
    /// retrying and calls [`CancelHandle::reset`] explicitly.
    pub fn realm_cancel_handle(&mut self, realm: RealmId) -> CancelHandle {
        CancelHandle::from_flag(self.handles.cancel_flag(realm))
    }

    /// SCOPE EXIT: close `realm`, releasing every parked frame and value
    /// handle it owns. Mirrors `PreparedMachine::close_realm`'s contract:
    ///
    /// - every frame parked under `realm` ([`Self::park`]) is removed and
    ///   its continuation's stowed root deregistered;
    /// - every [`PreparedHandle`] minted under `realm` (by
    ///   [`Self::inspect_outer`] or a call's retained results) has its
    ///   persistent-root registration deregistered (the slot cell stays
    ///   with `OldSpace` for the machine's life; the VALUE it pinned becomes
    ///   collectable once nothing else reaches it);
    /// - the realm's cancel flag entry is dropped;
    /// - sibling realms and their frames/handles are untouched;
    /// - `RealmId::ROOT`-tagged handles ([`Self::retain_top`]'s session-level
    ///   bindings) are never affected by any `close_realm` call -- they are
    ///   not part of any realm a caller can close this way.
    ///
    /// Returns `(frames_dropped, handles_released)`. Closing a realm that
    /// owns nothing is a no-op `(0, 0)` -- idempotent by construction, so a
    /// retirement path that can race a wholesale teardown stays safe.
    pub fn close_realm(&mut self, realm: RealmId) -> (usize, usize) {
        if realm == RealmId::ROOT {
            return (0, 0);
        }
        let closed = self.handles.close_realm(realm);
        let frames_dropped = closed.frames.len();
        let handles_released = closed.handles.len();
        for mut frame in closed.frames {
            self.machine.deregister_stowed_root(frame.cell.addr());
            if let Some(root) = frame.live_payload_root.take() {
                self.machine.deregister_persistent_root(root.addr());
            }
        }
        for entry in closed.handles {
            self.machine.deregister_persistent_root(entry.slot.addr());
        }
        self.assert_rooting_receipt();
        (frames_dropped, handles_released)
    }

    /// The rooting receipt: every parked continuation is a registered stowed
    /// root for its whole parked lifetime, so the two counts agree at every
    /// quiescent point. `debug_assert` as on the Core machine: a violation is
    /// a soundness bug worth a debug crash, not a release-mode cost.
    fn assert_rooting_receipt(&self) {
        debug_assert_eq!(
            self.machine.stowed_roots_count(),
            self.handles.counts().parked_continuations,
            "rooting receipt violated: stowed_roots_count() must equal the parked \
             continuation count at every quiescent point"
        );
    }

    // --- parked continuations ---------------------------------------------

    /// Park a retained continuation under `realm`. The handle leaves the
    /// handle ledger and its root slot moves from the persistent-root list to
    /// the stowed-root list: the frame now owns the value's liveness, and
    /// only [`Self::take_parked`] or [`Self::close_realm`] ends it. The
    /// handle must be live under `realm`; a foreign, released or other-realm
    /// handle is [`ExecutionError::UnknownPreparedHandle`] with nothing
    /// changed. The minted [`ContinuationId`] is the only token that names
    /// the frame afterwards; no [`PreparedHandle`] ever is.
    ///
    /// `live_payload_root` is the frame's already-tenured live payload, if
    /// any -- callers compute it (`PreparedEngine::tenure_live_payload`
    /// mirrors `PreparedMachine`'s own pre-park tenure) since minting it
    /// needs a handle inspection this method has no reason to also know how
    /// to do; `park` only stows what it is given.
    pub fn park(
        &mut self,
        continuation: PreparedHandle,
        realm: RealmId,
        live_payload_root: Option<crate::old_space::RootSlot>,
        request: ParkRequest,
    ) -> Result<ContinuationId, ExecutionError> {
        let ParkRequest { evidence, .. } = request;
        self.ensure_handle_access()?;
        let owned_here = self
            .handles
            .handle(continuation.raw)
            .is_some_and(|entry| entry.realm == realm);
        if !owned_here {
            return Err(ExecutionError::UnknownPreparedHandle);
        }
        let entry = self
            .handles
            .take_handle(continuation.raw)
            .ok_or(ExecutionError::UnknownPreparedHandle)?;
        let slot = entry.slot;
        // Nothing allocates between these two registrations, so the value is
        // rooted throughout: it leaves one list and enters the other before
        // any collection can run.
        self.machine.deregister_persistent_root(slot.addr());
        self.machine.register_stowed_root(slot.addr());
        let id = self.handles.park(ContinuationFrame {
            cell: slot,
            realm,
            live_payload_root,
            evidence,
        });
        self.assert_rooting_receipt();
        Ok(id)
    }

    /// Peek at a parked frame without consuming it: its realm and the
    /// evidence a resume validates against. `None` for an unknown id.
    #[must_use]
    pub fn parked(&self, id: ContinuationId) -> Option<(RealmId, &PreparedFrameEvidence)> {
        let frame = self.handles.continuation(id)?;
        Some((frame.realm, &frame.evidence))
    }

    /// The runtime resource scope owning the frame parked under `id`, if any.
    #[must_use]
    pub fn parked_realm(&self, id: ContinuationId) -> Option<RealmId> {
        self.handles.continuation(id).map(|frame| frame.realm)
    }

    /// The ids currently parked, ascending.
    #[must_use]
    pub fn parked_ids(&self) -> Vec<ContinuationId> {
        self.handles.parked_ids()
    }

    /// Number of frames currently parked on this machine.
    #[must_use]
    pub fn parked_count(&self) -> usize {
        self.handles.counts().parked_continuations
    }

    /// Number of stowed roots registered on this machine: the rooting
    /// receipt's other half, equal to [`Self::parked_count`] at quiescence.
    #[must_use]
    pub fn stowed_roots_count(&self) -> usize {
        self.machine.stowed_roots_count()
    }

    /// Consume the frame parked under `id`: its continuation returns to the
    /// handle ledger under the frame's realm (the root slot moves back to the
    /// persistent-root list) so it can be passed to the runner's resume entry
    /// as a `Managed` argument, alongside the frame's evidence. Every failure
    /// leaves the frame parked and rooted. The frame's realm cancel flag is
    /// the same flag the resumed call installs, since both come from this
    /// ledger.
    pub fn take_parked(
        &mut self,
        id: ContinuationId,
    ) -> Result<(PreparedHandle, PreparedFrameEvidence), ExecutionError> {
        self.ensure_handle_access()?;
        let frame = self
            .handles
            .continuation(id)
            .ok_or(ExecutionError::UnknownContinuation(id))?;
        let (evidence, slot, realm) = (frame.evidence, frame.cell, frame.realm);
        let mut frame = self
            .handles
            .take_continuation(id)
            .ok_or(ExecutionError::UnknownContinuation(id))?;
        self.machine.deregister_stowed_root(slot.addr());
        self.machine.register_persistent_root(slot.addr());
        if let Some(root) = frame.live_payload_root.take() {
            // A live payload not claimed before resume is no longer
            // externally reachable, as on Core.
            self.machine.deregister_persistent_root(root.addr());
        }
        let raw = self
            .handles
            .insert_handle(slot, realm, evidence.continuation_rep);
        self.assert_rooting_receipt();
        Ok((
            PreparedHandle {
                raw,
                rep: evidence.continuation_rep,
            },
            evidence,
        ))
    }

    /// Take the frame parked under `id`'s stowed live payload root and mint
    /// a [`ValueHandle`] over it under the frame's realm, mirroring
    /// `PreparedMachine::handle_from_live_payload` on the Core route. The
    /// frame itself stays parked and rooted -- only the payload's root moves
    /// from the frame's own stash into the handle ledger, which now owns its
    /// liveness (it was already a persistent root; [`Self::park`] never
    /// touches `live_payload_root`, so this is the same registration
    /// `take_parked`'s untaken-payload branch would otherwise release at
    /// resume). `None` when `id` is not parked or its live payload was
    /// already taken.
    pub fn take_live_payload_handle(
        &mut self,
        id: ContinuationId,
    ) -> Result<Option<ValueHandle>, ExecutionError> {
        self.take_live_payload_handle_owned_by(id, None)
    }

    /// [`Self::take_live_payload_handle`] with the minted handle owned by
    /// `owner` instead of the frame's own realm (`None`): a payload that must
    /// outlive the parked frame's realm, such as a green thread's completion
    /// value owned by the session's realm, mirroring Core's
    /// `mint_handle_from_root(slot, realm)`.
    pub fn take_live_payload_handle_owned_by(
        &mut self,
        id: ContinuationId,
        owner: Option<RealmId>,
    ) -> Result<Option<ValueHandle>, ExecutionError> {
        self.ensure_handle_access()?;
        let Some(frame) = self.handles.continuation_mut(id) else {
            return Ok(None);
        };
        let realm = owner.unwrap_or(frame.realm);
        let Some(root) = frame.live_payload_root.take() else {
            return Ok(None);
        };
        let handle = self
            .handles
            .insert_handle(root, realm, RuntimeRep::LiftedRef);
        self.assert_rooting_receipt();
        Ok(Some(handle))
    }

    fn ensure_handle_access(&self) -> Result<(), ExecutionError> {
        if self.machine.disposition() == MachineDisposition::Unavailable {
            return Err(ExecutionError::Runtime(
                self.machine.last_failure().unwrap_or(MachineFailure {
                    cause: RuntimeError::BadPointer,
                    disposition: MachineDisposition::Unavailable,
                }),
            ));
        }
        Ok(())
    }

    /// Inspect one constructor layer of a retained value without forcing it.
    ///
    /// Every managed field receives its own persistent root before the
    /// descriptor reader releases the active nursery borrow. The source
    /// handle remains owned by this machine and can be inspected again.
    pub fn inspect_outer(
        &mut self,
        handle: PreparedHandle,
        realm: RealmId,
    ) -> Result<PreparedOuter, ExecutionError> {
        self.ensure_handle_access()?;
        let source = self
            .handles
            .handle(handle.raw)
            .filter(|entry| entry.realm == realm)
            .map(|entry| entry.slot)
            .ok_or(ExecutionError::UnknownPreparedHandle)?;
        let word = unsafe { source.current() } as usize;
        if word == 0 {
            return Err(ExecutionError::UnknownPreparedHandle);
        }
        // The object this handle roots may belong to ANY installed program --
        // that is the whole point of a shared heap -- so inspection always
        // reads through the machine-wide descriptor registry/static union,
        // never one program's own metadata.
        let (identity, fields) = self.inspect_constructor(super::observe::ObservationSeed {
            word,
            rep: handle.rep,
        })?;
        let words = RootWords::new(fields.len())?;
        let mut managed = Vec::new();
        let mut output = Vec::new();
        managed
            .try_reserve_exact(fields.len())
            .map_err(|_| runtime_error(&self.machine, RuntimeError::HeapOverflow))?;
        output
            .try_reserve_exact(fields.len())
            .map_err(|_| runtime_error(&self.machine, RuntimeError::HeapOverflow))?;
        for (field_index, field) in fields.iter().copied().enumerate() {
            words.write(field_index, field.word as u64)?;
            match field.rep {
                RuntimeRep::LiftedRef | RuntimeRep::UnliftedRef => {
                    managed.push((field_index, field.rep));
                    output.push(PreparedResult::Void);
                }
                RuntimeRep::Void => {
                    return Err(ExecutionError::Observation(
                        super::ObservationFailure::Integrity(
                            tidepool_heap::execution_descriptor::DescriptorTraceError::InvalidRange,
                        ),
                    ))
                }
                RuntimeRep::Address => {
                    return Err(
                        super::ObservationFailure::Representation(RuntimeRep::Address).into(),
                    )
                }
                RuntimeRep::Int(_) | RuntimeRep::Word(_) | RuntimeRep::Float(_) => {
                    output.push(PreparedResult::Scalar(field.word as u64));
                }
            }
        }
        self.handles
            .try_reserve_handles(managed.len())
            .map_err(|_| runtime_error(&self.machine, RuntimeError::HeapOverflow))?;
        let mut selected = Vec::new();
        selected
            .try_reserve_exact(managed.len())
            .map_err(|_| runtime_error(&self.machine, RuntimeError::HeapOverflow))?;
        for &(field_index, _) in &managed {
            selected.push(unsafe { words.as_mut_ptr().add(field_index).cast::<*mut u8>() });
        }
        let mark = self.machine.rust_roots_len();
        for &(field_index, _) in &managed {
            let slot = unsafe { words.as_mut_ptr().add(field_index).cast::<*mut u8>() };
            self.machine.register_rust_root(slot);
        }
        let _roots = TemporaryRoots {
            machine: &self.machine,
            mark,
        };
        if !managed.is_empty() {
            if unsafe { self.machine.prepared_old_space() }.is_some() {
                return Err(runtime_error(&self.machine, RuntimeError::BadPointer));
            }
            unsafe { self.machine.install_prepared_old_space(&self.old_space) };
            let retained = unsafe {
                self.old_space.retain_prepared(
                    &self.machine,
                    &mut self.vmctx,
                    &selected,
                    &self.descriptors,
                )
            };
            self.machine.clear_prepared_old_space();
            let roots = retained.map_err(|cause| runtime_error(&self.machine, cause))?;
            if roots.len() != managed.len() {
                for root in roots {
                    self.machine.deregister_persistent_root(root.addr());
                }
                return Err(runtime_error(&self.machine, RuntimeError::BadPointer));
            }
            for ((field_index, rep), root) in managed.into_iter().zip(roots) {
                let raw = self.handles.insert_handle(root, realm, rep);
                output[field_index] = PreparedResult::Managed(PreparedHandle { raw, rep });
            }
        }
        Ok(PreparedOuter::Constructor {
            identity,
            fields: output,
        })
    }

    /// Read one constructor layer through the machine-wide descriptor/static
    /// union and the shared nursery/old-space -- the object may have been
    /// produced by any installed program.
    fn inspect_constructor(
        &self,
        seed: super::observe::ObservationSeed,
    ) -> Result<(DataConId, Vec<super::observe::ObservationSeed>), ExecutionError> {
        let heap = self.observation_heap()?;
        heap.inspect_constructor(seed).map_err(ExecutionError::from)
    }

    /// Build a non-forcing view over the machine-wide nursery/static/old-space
    /// union as it stands right now -- the shared construction behind
    /// [`Self::inspect_constructor`] and [`Self::install`]'s import
    /// re-verification. Requires a live heap (at least one program already
    /// installed); [`Self::install`] only reaches this after a declared
    /// import's handle has resolved, which itself requires a live heap.
    fn observation_heap(&self) -> Result<super::observe::ObservationHeap<'_>, ExecutionError> {
        self.observation_heap_and_starts(&self.statics)
            .map(|(heap, _, _)| heap)
    }

    /// [`Self::observation_heap`] admitting exactly `statics` (in that
    /// order), so a caller that maps static hits back to programs supplies
    /// the regions in the order it indexes them. Also returns the nursery
    /// base address and its exact-start bitmap (bit `i` is the word at
    /// `base + 8 * i`).
    fn observation_heap_and_starts<'s>(
        &'s self,
        statics: &'s [Arc<StaticRegion>],
    ) -> Result<(super::observe::ObservationHeap<'s>, usize, Vec<u64>), ExecutionError> {
        let (start, size) = self
            .machine
            .gc_active_range()
            .ok_or_else(|| runtime_error(&self.machine, RuntimeError::BadPointer))?;
        let cursor = (self.vmctx.alloc_ptr as usize)
            .checked_sub(start as usize)
            .filter(|cursor| *cursor <= size && *cursor % std::mem::size_of::<u64>() == 0)
            .ok_or_else(|| runtime_error(&self.machine, RuntimeError::BadPointer))?;
        let nursery = unsafe {
            std::slice::from_raw_parts(start.cast::<u64>(), cursor / std::mem::size_of::<u64>())
        };
        let mut starts = Vec::new();
        let mut scanned_words = 0;
        super::observe::append_exact_starts(
            nursery,
            &self.descriptor_registry,
            &mut starts,
            &mut scanned_words,
        )?;
        let heap = super::observe::ObservationHeap::new_with_registry_and_starts(
            nursery,
            statics,
            &self.descriptor_registry,
            &starts,
            Some(&*self.old_space),
            &self.machine,
        )
        .map_err(ExecutionError::from)?;
        Ok((heap, start as usize, starts))
    }

    #[cfg(test)]
    pub(crate) fn persistent_roots_count(&self, id: ProgramId) -> usize {
        let Some(program) = self.programs.get(&id) else {
            return 0;
        };
        // A program-scoped count: the machine's registered persistent roots
        // that fall inside this program's own root block (its top words; the
        // import words follow them in the same block). Proves the actual
        // registration happened (not merely that metadata implies it should
        // have) -- see `two_closed_programs_share_one_machine_across_a_forced_collection`.
        let compiled = program.program.get();
        let block = compiled.root_block.as_mut_ptr() as usize;
        let range_end = block + compiled.top_slots.len() * std::mem::size_of::<u64>();
        let mut roots = Vec::new();
        self.machine.extend_persistent_roots(&mut roots);
        roots
            .into_iter()
            .filter(|&slot| {
                let address = slot as usize;
                address >= block && address < range_end
            })
            .count()
    }

    /// Whether `identity`'s import slot on program `id` is itself a
    /// registered persistent root right now -- the direct root-accounting
    /// proof (`tidepool-codegen/CLAUDE.md` "Root accounting") that
    /// `install_program` actually registered it, distinct from and stronger
    /// than any GC-survival inference: an already-`retain_prepared`d value's
    /// OWN root slot is never relocated by a minor collection (old space is
    /// compacted only on an explicit major pass this machine never runs), so
    /// a skipped registration for the import slot specifically would not be
    /// exposed by the underlying object moving -- it is exposed here, and by
    /// the deregistration-count check callers can build from it.
    #[cfg(test)]
    pub(crate) fn import_slot_is_registered_root(
        &self,
        id: ProgramId,
        identity: &tidepool_repr::execution_schema::SymbolIdentity,
    ) -> bool {
        let Some(program) = self.programs.get(&id) else {
            return false;
        };
        let compiled = program.program.get();
        let Some(address) = compiled
            .import_slots
            .iter()
            .find(|candidate| &candidate.identity == identity)
            .and_then(|candidate| compiled.root_block.slot_address(candidate.slot))
        else {
            return false;
        };
        let mut roots = Vec::new();
        self.machine.extend_persistent_roots(&mut roots);
        roots.into_iter().any(|root| root == address)
    }

    /// The program's top words (its block without the import words).
    #[cfg(test)]
    pub(crate) fn top_words(&self, id: ProgramId) -> Vec<u64> {
        let Some(program) = self.programs.get(&id) else {
            return Vec::new();
        };
        let compiled = program.program.get();
        let mut snapshot = compiled.root_block.snapshot();
        snapshot.truncate(compiled.top_slots.len());
        snapshot
    }

    /// The live heap pointer a retained handle's root slot currently holds
    /// (not the slot's own bookkeeping address). Two handles that read the
    /// SAME value here -- across a collection, and regardless of which
    /// installed program produced either handle -- point at the identical
    /// heap object: identity, not a copy. `None` for an unknown handle.
    #[cfg(test)]
    pub(crate) fn handle_current_pointer(&self, handle: PreparedHandle) -> Option<usize> {
        self.handles
            .handle(handle.raw)
            .map(|entry| unsafe { entry.slot.current() } as usize)
    }

    /// Retain one of an installed program's own top-level bindings as a
    /// handle, without running anything: the value a later program can
    /// import by identity. `value` must name a heap or static top of
    /// `program` (a raw byte top has no managed representation and is
    /// `Unsupported::HostArguments`-class refused as `MissingEntry`); the
    /// handle roots the top's current object through its own persistent
    /// slot, exactly as a retained entry result does, so the top table's
    /// own slot and this handle stay two roots to one object.
    pub fn retain_top(
        &mut self,
        id: ProgramId,
        value: ValueId,
    ) -> Result<PreparedHandle, ExecutionError> {
        self.retain_top_as(id, value, HandleClass::Value)
    }

    /// [`Self::retain_top`] for a machine-lifetime export root: the same
    /// handle, accounted as [`HandleClass::CodeExport`] and therefore
    /// outside `residency().handles` and outside every scope closure. Use it
    /// for a top a later program will import rather than compile its own
    /// copy of; the root is what keeps the defining program, and so its
    /// code, alive for those importers.
    pub fn retain_export_top(
        &mut self,
        id: ProgramId,
        value: ValueId,
    ) -> Result<PreparedHandle, ExecutionError> {
        self.retain_top_as(id, value, HandleClass::CodeExport)
    }

    fn retain_top_as(
        &mut self,
        id: ProgramId,
        value: ValueId,
        class: HandleClass,
    ) -> Result<PreparedHandle, ExecutionError> {
        self.ensure_handle_access()?;
        let compiled = self
            .programs
            .get(&id)
            .ok_or(ExecutionError::UnknownProgram(id))?
            .program
            .get();
        if compiled.byte_tops.contains_key(&value) {
            return Err(ExecutionError::MissingEntry(value));
        }
        let slot = *compiled
            .top_slots
            .get(&value)
            .ok_or(ExecutionError::MissingEntry(value))?;
        let word = compiled
            .root_block
            .read(slot)
            .map_err(|cause| runtime_error(&self.machine, cause))?;
        if word == 0 {
            return Err(ExecutionError::MissingEntry(value));
        }
        let words = RootWords::new(1)?;
        words.write(0, word)?;
        let source = words.as_mut_ptr().cast::<*mut u8>();
        let mark = self.machine.rust_roots_len();
        self.machine.register_rust_root(source);
        let _roots = TemporaryRoots {
            machine: &self.machine,
            mark,
        };
        self.handles
            .try_reserve_handles(1)
            .map_err(|_| runtime_error(&self.machine, RuntimeError::HeapOverflow))?;
        if unsafe { self.machine.prepared_old_space() }.is_some() {
            return Err(runtime_error(&self.machine, RuntimeError::BadPointer));
        }
        unsafe { self.machine.install_prepared_old_space(&self.old_space) };
        let retained = unsafe {
            self.old_space.retain_prepared(
                &self.machine,
                &mut self.vmctx,
                &[source],
                &self.descriptors,
            )
        };
        self.machine.clear_prepared_old_space();
        let mut roots = retained.map_err(|cause| runtime_error(&self.machine, cause))?;
        let Some(root) = roots.pop().filter(|_| roots.is_empty()) else {
            for root in roots {
                self.machine.deregister_persistent_root(root.addr());
            }
            return Err(runtime_error(&self.machine, RuntimeError::BadPointer));
        };
        let raw = match class {
            HandleClass::Value => {
                self.handles
                    .insert_handle(root, RealmId::ROOT, RuntimeRep::LiftedRef)
            }
            HandleClass::CodeExport => self
                .handles
                .insert_export_handle(root, RuntimeRep::LiftedRef),
        };
        Ok(PreparedHandle {
            raw,
            rep: RuntimeRep::LiftedRef,
        })
    }

    /// Start one incremental managed construction operation. Dropping the
    /// builder publishes no handle and releases all temporary root ownership.
    pub fn managed_builder(&mut self) -> Result<ManagedBuilder<'_, 'code>, ExecutionError> {
        ManagedBuilder::new(self)
    }

    /// The persistent root slot behind a retained handle, for an owner that
    /// records roots by slot (the session `BindingTable`). The slot stays
    /// registered until [`Self::release`] takes the handle; a caller holding
    /// the slot must therefore refuse to release the handle first.
    #[must_use]
    pub fn handle_root(&self, handle: PreparedHandle) -> Option<crate::old_space::RootSlot> {
        self.handles.handle(handle.raw).map(|entry| entry.slot)
    }

    /// [`Self::handle_root`] by the bare cross-engine [`ValueHandle`] id --
    /// a caller holding only a runtime session `RootCustody`'s raw id (the
    /// runtime session's `run_rooted_entry`, which crosses the eval-thread
    /// boundary as the bare id; see [`Self::discard_handle`]'s doc for the
    /// same shape). `None` for an unknown or already-released handle,
    /// exactly as `handle_root`.
    #[must_use]
    pub fn handle_slot(&self, handle: ValueHandle) -> Option<crate::old_space::RootSlot> {
        self.handles.handle(handle).map(|entry| entry.slot)
    }

    /// Adopt a handle's rooted slot into another ownership discipline: the
    /// handle is removed from this machine's ledger atomically and its
    /// persistent-root registration remains active. Mirrors
    /// `PreparedMachine::take_handle_root`; the caller must install the
    /// returned slot in a root-owning structure (a parked frame's own
    /// `live_payload_root` stash, at present -- see
    /// `PreparedEngine::tenure_live_payload`). Unknown or already-released
    /// handles return `Ok(None)`.
    pub fn take_handle_root(
        &mut self,
        handle: PreparedHandle,
    ) -> Result<Option<crate::old_space::RootSlot>, ExecutionError> {
        self.ensure_handle_access()?;
        Ok(self.handles.take_handle(handle.raw).map(|entry| entry.slot))
    }

    /// Move a retained handle into the machine's own ROOT scope, so closing
    /// the realm it was minted under no longer releases it. The session value
    /// plane calls this when it binds a run's result: from then on the
    /// binding owns the value's lifetime and ends it through [`Self::release`].
    /// `false` for an unknown or already released handle.
    pub fn adopt_handle(&mut self, handle: PreparedHandle) -> bool {
        self.handles.rehome_handle(handle.raw, RealmId::ROOT)
    }

    /// Every persistent root this machine has registered, whatever program
    /// owns it -- the ledger a session's scope-retirement receipt is checked
    /// against, as `PreparedMachine::persistent_roots_count` is for Core.
    #[must_use]
    pub fn total_persistent_roots(&self) -> usize {
        self.machine.persistent_roots_count()
    }

    /// Materialize a retained value as a bridge `HaskellValue`, forcing its lazy
    /// fields through program `id`'s force adapter under the value's own
    /// realm cancel flag. The handle stays retained: forcing may evaluate and
    /// move the graph it roots, and the handle's root slot follows the move.
    /// `budget` bounds observed nodes and copied payload bytes together, and
    /// exhausting it fails the whole observation.
    pub fn observe_handle(
        &mut self,
        id: ProgramId,
        handle: PreparedHandle,
        budget: usize,
    ) -> Result<HaskellValue, ExecutionError> {
        self.observe_handle_with(
            id,
            handle,
            budget,
            crate::observation::BudgetPolicy::Complete,
        )
    }

    /// [`Self::observe_handle`], but an exhausted budget CUTS instead of
    /// failing: the walk stops where the budget ran out and
    /// [`crate::observation::oversize_cut`] stands in for each subtree it did
    /// not read. What comes back is a SELECTION of the value, never the whole
    /// of it, and [`crate::observation::contains_oversize_sentinel`] is how a
    /// caller tells the two apart. Every other observation failure — an
    /// unauthenticated address, a descriptor integrity error, an unobservable
    /// object kind — still fails, because each of those says something is
    /// actually wrong rather than merely large. The handle stays retained
    /// either way, so the value the cut omitted remains reachable.
    pub fn observe_handle_bounded(
        &mut self,
        id: ProgramId,
        handle: PreparedHandle,
        budget: usize,
    ) -> Result<HaskellValue, ExecutionError> {
        self.observe_handle_with(
            id,
            handle,
            budget,
            crate::observation::BudgetPolicy::Bounded,
        )
    }

    fn observe_handle_with(
        &mut self,
        id: ProgramId,
        handle: PreparedHandle,
        budget: usize,
        policy: crate::observation::BudgetPolicy,
    ) -> Result<HaskellValue, ExecutionError> {
        self.ensure_handle_access()?;
        let (word, realm) = self
            .handles
            .handle(handle.raw)
            .map(|entry| (unsafe { entry.slot.current() } as usize, entry.realm))
            .filter(|(word, _)| *word != 0)
            .ok_or(ExecutionError::UnknownPreparedHandle)?;
        let cancel = self.handles.cancel_flag(realm);
        let program = self
            .programs
            .get(&id)
            .ok_or(ExecutionError::UnknownProgram(id))?
            .program
            .get();
        let max_native_frame = program.pipeline.native_frame_maximum();
        let reserve = max_native_frame
            .checked_mul(2)
            .ok_or_else(|| runtime_error_without_machine(RuntimeError::StackOverflow))?;
        let bounds = NativeStackBounds::current().map_err(runtime_error_without_machine)?;
        bounds
            .ensure_current_frame_reserve(reserve)
            .map_err(runtime_error_without_machine)?;
        self.vmctx.prepared_stack_limit = bounds
            .limit_with_frame_reserve(max_native_frame)
            .map_err(runtime_error_without_machine)?;
        self.machine
            .begin_prepared_call()
            .map_err(ExecutionError::Runtime)?;
        self.machine.set_cancel_flag(cancel);
        let observed = {
            let _cancel = CancelScope(&self.machine);
            let _scope = OldSpaceScope::new(&self.machine, &self.old_space)?;
            super::forcing::observe_results(
                &self.machine,
                program,
                &mut self.vmctx,
                &self.statics,
                &self.descriptor_registry,
                &self.old_space,
                &[super::observe::ObservationSeed {
                    word,
                    rep: handle.rep,
                }],
                budget,
                policy,
            )
        };
        self.machine.end_prepared_call();
        let mut values = match observed {
            Ok(values) => values,
            Err(ExecutionError::Observation(error @ super::ObservationFailure::Integrity(_))) => {
                self.machine.set_first_cause(RuntimeError::BadPointer);
                return Err(runtime_error_from_machine_or_observation(
                    &self.machine,
                    error,
                ));
            }
            Err(error) => return Err(error),
        };
        values
            .pop()
            .ok_or_else(|| runtime_error(&self.machine, RuntimeError::BadPointer))
    }

    /// The runtime resource scope `handle` is currently live under, or
    /// `None` for an unknown, foreign or released handle. Answered from the
    /// machine's own `ResourceLedger`, the single owner of that fact.
    #[must_use]
    pub fn handle_realm(&self, handle: PreparedHandle) -> Option<RealmId> {
        self.handles.handle(handle.raw).map(|entry| entry.realm)
    }

    /// Look up a bare `ValueHandle` (as a `Handle`/`FramedHandle` resume
    /// delivers it) as this machine's own [`PreparedHandle`], when it is
    /// live in this machine's ledger. Every handle this engine mints roots a
    /// lifted reference, so `rep` is always `RuntimeRep::LiftedRef` — the
    /// only check possible on this route, matching Core's own lack of a
    /// deeper type check for a bare handle delivery. No realm check: a
    /// handle is meant to move between parked continuations across resource
    /// scopes.
    #[must_use]
    pub fn prepared_handle_of(&self, raw: ValueHandle) -> Option<PreparedHandle> {
        let entry = self.handles.handle(raw)?;
        Some(PreparedHandle {
            raw,
            rep: entry.rep,
        })
    }

    /// The program whose descriptor owns the object `handle` currently
    /// refers to (following any settled thunk indirection), for a rooted
    /// apply's hosting-program choice
    /// (`tidepool-runtime::session::prepared::PreparedEngine::hosting_program`).
    /// `None` for an unknown/released handle, a null slot, a reference into a
    /// static region whose owner is not in [`Self::region_owners`] (should
    /// not happen: every installed program's own region is pushed there), or
    /// any representation this machine cannot trace. Mirrors
    /// [`Self::mark_live_programs`]'s single mark step, but for one handle
    /// rather than a whole-heap worklist.
    #[must_use]
    pub fn owner_of_handle(&self, handle: PreparedHandle) -> Option<ProgramId> {
        let word = self
            .handles
            .handle(handle.raw)
            .map(|entry| unsafe { entry.slot.current() } as usize)?;
        if word == 0 {
            return None;
        }
        let regions: Vec<Arc<StaticRegion>> = self
            .region_owners
            .iter()
            .map(|(_, region)| Arc::clone(region))
            .collect();
        let (heap, _, _) = self.observation_heap_and_starts(&regions).ok()?;
        match heap.trace_step(word).ok()? {
            super::observe::Traced::Object { header, .. } => {
                self.header_owners.get(&header).copied()
            }
            super::observe::Traced::Static { region } => {
                self.region_owners.get(region).map(|(id, _)| *id)
            }
        }
    }

    /// Test-only: fail the `occurrence`th poll of `point` in the next call(s)
    /// with `cause`, through the same status path as real cancellation. The
    /// injection is consumed when it fires; an unfired one stays armed.
    #[cfg(test)]
    pub(super) fn fail_prepared_at(
        &self,
        point: crate::prepared_control::PreparedSafepoint,
        occurrence: usize,
        cause: crate::host_fns::RuntimeError,
    ) {
        self.machine.fail_prepared_at(point, occurrence, cause);
    }

    /// Whether a retained handle's value is already in weak head normal form
    /// (a constructor, function or PAP, following any settled thunk
    /// indirection), read without forcing -- the fact an importing program's
    /// `required_evaluated` declaration is checked against.
    pub fn handle_is_evaluated(&self, handle: PreparedHandle) -> Result<bool, ExecutionError> {
        let word = self
            .handles
            .handle(handle.raw)
            .map(|entry| unsafe { entry.slot.current() } as usize)
            .filter(|word| *word != 0)
            .ok_or(ExecutionError::UnknownPreparedHandle)?;
        let heap = self.observation_heap()?;
        heap.resolves_to_whnf_value(word)
            .map_err(ExecutionError::from)
    }

    /// Execute with representation-checked values and retain every managed
    /// result before its temporary adapter storage can disappear. A managed
    /// argument may be a handle produced by ANY installed program on this
    /// machine -- they share one heap, so there is no "wrong owner" to
    /// reject (see the module doc); a handle from a different
    /// `PreparedMachine` is still rejected, by ledger emptiness.
    pub fn run_entry_retained(
        &mut self,
        id: ProgramId,
        entry: ValueId,
        arguments: &[PreparedInput],
        options: PreparedCallOptions,
        realm: RealmId,
    ) -> Result<PreparedResultBatch, ExecutionError> {
        let cancel = self.handles.cancel_flag(realm);
        let program = self
            .programs
            .get(&id)
            .ok_or(ExecutionError::UnknownProgram(id))?;
        let result = program.run_entry_retained(
            entry,
            arguments,
            options,
            cancel,
            realm,
            &self.machine,
            &mut self.vmctx,
            &mut self.old_space,
            &self.descriptors,
            &mut self.handles,
            &self.statics,
            &self.descriptor_registry,
        );
        // The call's outcome is in `result`; nothing of it outlives the call.
        self.machine.end_prepared_call();
        result
    }

    /// Execute one scalar-only entry on the retained machine.
    pub fn run_entry(
        &mut self,
        id: ProgramId,
        entry: ValueId,
        arguments: &[u64],
        options: PreparedCallOptions,
        realm: RealmId,
    ) -> Result<RunResult, ExecutionError> {
        let cancel = self.handles.cancel_flag(realm);
        self.run_entry_with_raw_cancel(id, entry, arguments, options, cancel)
    }

    /// [`Self::run_entry`]'s primitive, taking an externally-owned cancel
    /// flag directly instead of minting one from a realm. `pub` (not
    /// crate-internal): both [`CompiledProgram::run_entry`] (`run.rs`)'s
    /// one-shot ephemeral-machine convenience API and callers in other
    /// crates that construct and control their own `Arc<AtomicBool>`
    /// directly (e.g. from a watchdog thread, or to pre-cancel a call before
    /// any realm/machine exists) use this instead of a realm -- that
    /// contract predates realm-scoped cancellation and is out of C1's scope
    /// to migrate (dozens of existing test call sites).
    pub fn run_entry_with_raw_cancel(
        &mut self,
        id: ProgramId,
        entry: ValueId,
        arguments: &[u64],
        options: PreparedCallOptions,
        cancel: Arc<AtomicBool>,
    ) -> Result<RunResult, ExecutionError> {
        let program = self
            .programs
            .get(&id)
            .ok_or(ExecutionError::UnknownProgram(id))?;
        let result = program.run_entry(
            entry,
            arguments,
            options,
            cancel,
            &self.machine,
            &mut self.vmctx,
            &self.old_space,
            &self.statics,
            &self.descriptor_registry,
        );
        self.machine.end_prepared_call();
        result
    }
}

fn collect_on(
    machine: &MachineState,
    vmctx: &mut VMContext,
    old_space: &OldSpace,
    reserve: usize,
) -> Result<(), ExecutionError> {
    let _scope = OldSpaceScope::new(machine, old_space)?;
    let raw = unsafe { prepared_gc_trigger(vmctx, reserve) };
    let status = CallStatus::from_raw(i64::from(raw))
        .map_err(|_| runtime_error(machine, RuntimeError::BadPointer))?;
    if status != CallStatus::Success || machine.prepared_call_status() != CallStatus::Success {
        return Err(runtime_error_for_status(machine, status));
    }
    Ok(())
}

impl<'code> InstalledProgram<'code> {
    /// The root-block slots that may hold a managed or static reference:
    /// every slot but a byte top's, whose word is a raw literal-pool address
    /// (its liveness is edge (c), not a heap edge).
    fn reference_slots(&self) -> impl Iterator<Item = usize> + '_ {
        let compiled = self.program.get();
        let byte_slots: HashSet<usize> = compiled
            .byte_tops
            .keys()
            .filter_map(|top| compiled.top_slots.get(top).copied())
            .collect();
        (0..compiled.root_block.len()).filter(move |slot| !byte_slots.contains(slot))
    }

    #[expect(
        clippy::too_many_arguments,
        reason = "retained entry execution independently borrows this program's own compiled entry alongside the machine-wide shared heap (machine, vmctx, old space, descriptor union) and the shared handle ledger"
    )]
    fn run_entry_retained(
        &self,
        entry: ValueId,
        arguments: &[PreparedInput],
        options: PreparedCallOptions,
        cancel: Arc<AtomicBool>,
        realm: RealmId,
        machine: &MachineState,
        vmctx: &mut VMContext,
        old_space: &mut OldSpace,
        descriptors: &[Arc<ObjectDescriptor>],
        handles: &mut ResourceLedger,
        statics: &[Arc<StaticRegion>],
        descriptor_registry: &BTreeMap<usize, DescriptorMetadata>,
    ) -> Result<PreparedResultBatch, ExecutionError> {
        let (adapter, reps, result_contract, result_layout) = {
            let compiled = self
                .program
                .get()
                .entries
                .get(&entry)
                .ok_or(ExecutionError::MissingEntry(entry))?;
            (
                compiled.adapter,
                compiled.abi.physical_arguments().to_vec(),
                compiled.abi.semantic_results().clone(),
                compiled.abi.result_layout().clone(),
            )
        };
        if arguments.len() != reps.len() {
            return Err(ExecutionError::Arguments {
                expected: reps.len(),
                actual: arguments.len(),
            });
        }
        let argument_area = RootWords::new(arguments.len())?;
        let mut managed_arguments = Vec::new();
        for (argument_index, (argument, expected)) in arguments.iter().zip(&reps).enumerate() {
            let word = match (argument, expected) {
                (PreparedInput::Scalar(word), actual)
                    if !matches!(actual, RuntimeRep::LiftedRef | RuntimeRep::UnliftedRef) =>
                {
                    *word
                }
                (PreparedInput::Managed(handle), actual)
                    if *actual == handle.rep
                        && matches!(actual, RuntimeRep::LiftedRef | RuntimeRep::UnliftedRef) =>
                {
                    // Every installed program shares this machine's ledger:
                    // a handle produced by ANY of them is a legitimate
                    // argument here, so long as it is still live in this
                    // machine's ledger (see the module doc).
                    let entry = handles
                        .handle(handle.raw)
                        .ok_or(ExecutionError::UnknownPreparedHandle)?;
                    let word = unsafe { entry.slot.current() } as usize as u64;
                    if word == 0 {
                        return Err(ExecutionError::UnknownPreparedHandle);
                    }
                    managed_arguments.push(argument_index);
                    word
                }
                (PreparedInput::Managed(handle), actual) => {
                    return Err(ExecutionError::ArgumentRepresentation {
                        index: argument_index,
                        expected: *actual,
                        actual: handle.rep,
                    });
                }
                (PreparedInput::Scalar(_), actual) => {
                    return Err(ExecutionError::ArgumentRepresentation {
                        index: argument_index,
                        expected: *actual,
                        actual: RuntimeRep::Word(64),
                    });
                }
            };
            argument_area.write(argument_index, word)?;
        }
        let argument_mark = machine.rust_roots_len();
        for argument_index in managed_arguments {
            let slot = unsafe {
                argument_area
                    .as_mut_ptr()
                    .add(argument_index)
                    .cast::<*mut u8>()
            };
            machine.register_rust_root(slot);
        }
        let _arguments = TemporaryRoots {
            machine,
            mark: argument_mark,
        };
        let max_native_frame = self.program.get().pipeline.native_frame_maximum();
        let reserve = max_native_frame
            .checked_mul(2)
            .ok_or_else(|| runtime_error_without_machine(RuntimeError::StackOverflow))?;
        let bounds = NativeStackBounds::current().map_err(runtime_error_without_machine)?;
        bounds
            .ensure_current_frame_reserve(reserve)
            .map_err(runtime_error_without_machine)?;
        vmctx.prepared_stack_limit = bounds
            .limit_with_frame_reserve(max_native_frame)
            .map_err(runtime_error_without_machine)?;
        machine
            .begin_prepared_call()
            .map_err(ExecutionError::Runtime)?;
        machine.set_cancel_flag(cancel);
        let _cancel = CancelScope(machine);
        let result_words =
            (result_layout.payload_size() as usize).div_ceil(std::mem::size_of::<u64>());
        let results = try_root_words(result_words.max(1))?;
        let collections_before = machine.gc_generation();
        let pointer = self.program.get().pipeline.get_function_ptr(adapter);
        let raw = {
            let _intrinsic = super::ActiveIntrinsicScope::new(
                machine,
                self.program.get(),
                statics,
                descriptor_registry,
            )?;
            let _scope = OldSpaceScope::new(machine, old_space)?;
            unsafe {
                let adapter: extern "C" fn(*mut VMContext, *mut u64, *const u64) -> i32 =
                    std::mem::transmute(pointer);
                adapter(vmctx, results.as_mut_ptr(), argument_area.as_mut_ptr())
            }
        };
        let status = CallStatus::from_raw(i64::from(raw))
            .map_err(|_| runtime_error(machine, RuntimeError::BadPointer))?;
        if status != CallStatus::Success || machine.prepared_call_status() != CallStatus::Success {
            super::forcing::describe_raised_exception(
                machine,
                self.program.get(),
                vmctx,
                statics,
                descriptor_registry,
                old_space,
            );
            return Err(runtime_error_for_status(machine, status));
        }
        let result_reps = result_contract
            .returned_reps()
            .ok_or_else(|| runtime_error(machine, RuntimeError::NoSuccessReturned))?;
        let mark = machine.rust_roots_len();
        register_result_roots(machine, &results, &result_layout);
        let _results = TemporaryRoots { machine, mark };
        if options.collect_before_observation {
            collect_on(machine, vmctx, old_space, 0)?;
        }
        let mut slots = Vec::new();
        let mut output = Vec::new();
        for (logical, rep) in result_reps.iter().copied().enumerate() {
            let Some(stored) = result_layout
                .logical_to_stored()
                .get(logical)
                .copied()
                .flatten()
            else {
                output.push(PreparedResult::Void);
                continue;
            };
            let field = &result_layout.fields()[stored as usize];
            let address = unsafe {
                results
                    .as_mut_ptr()
                    .cast::<u8>()
                    .add(field.offset() as usize)
            };
            if matches!(rep, RuntimeRep::LiftedRef | RuntimeRep::UnliftedRef) {
                slots.push(address.cast::<*mut u8>());
                output.push(PreparedResult::Void);
            } else {
                let mut bytes = [0_u8; 8];
                unsafe {
                    std::ptr::copy_nonoverlapping(
                        address,
                        bytes.as_mut_ptr(),
                        field.size() as usize,
                    )
                };
                output.push(PreparedResult::Scalar(u64::from_ne_bytes(bytes)));
            }
        }
        if !slots.is_empty() {
            if unsafe { machine.prepared_old_space() }.is_some() {
                return Err(runtime_error(machine, RuntimeError::BadPointer));
            }
            // Promotion mutates OldSpace, so its admission pointer is scoped
            // manually rather than held through an immutable Rust borrow.
            unsafe { machine.install_prepared_old_space(old_space) };
            let retained =
                unsafe { old_space.retain_prepared(machine, vmctx, &slots, descriptors) };
            machine.clear_prepared_old_space();
            let roots = retained.map_err(|cause| runtime_error(machine, cause))?;
            if roots.len() != slots.len() {
                for root in roots {
                    machine.deregister_persistent_root(root.addr());
                }
                return Err(runtime_error(machine, RuntimeError::BadPointer));
            }
            handles
                .try_reserve_handles(roots.len())
                .map_err(|_| runtime_error(machine, RuntimeError::HeapOverflow))?;
            let managed =
                result_reps.iter().copied().enumerate().filter(|(_, rep)| {
                    matches!(rep, RuntimeRep::LiftedRef | RuntimeRep::UnliftedRef)
                });
            for ((logical, rep), root) in managed.zip(roots) {
                let raw = handles.insert_handle(root, realm, rep);
                output[logical] = PreparedResult::Managed(PreparedHandle { raw, rep });
            }
        }
        Ok(PreparedResultBatch {
            values: output,
            collections: machine.gc_generation().saturating_sub(collections_before),
        })
    }

    /// Execute one scalar-only entry on the shared machine.
    #[expect(
        clippy::too_many_arguments,
        reason = "entry execution independently borrows this program's own compiled entry alongside the machine-wide shared heap (machine, vmctx, old space, static/descriptor-registry unions)"
    )]
    fn run_entry(
        &self,
        entry: ValueId,
        arguments: &[u64],
        options: PreparedCallOptions,
        cancel: Arc<AtomicBool>,
        machine: &MachineState,
        vmctx: &mut VMContext,
        old_space: &OldSpace,
        statics: &[Arc<StaticRegion>],
        descriptor_registry: &BTreeMap<usize, DescriptorMetadata>,
    ) -> Result<RunResult, ExecutionError> {
        let (adapter, expected_arguments, has_managed_arguments, result_contract, result_layout) = {
            let compiled = self
                .program
                .get()
                .entries
                .get(&entry)
                .ok_or(ExecutionError::MissingEntry(entry))?;
            (
                compiled.adapter,
                compiled.abi.physical_arguments().len(),
                compiled
                    .abi
                    .semantic_arguments()
                    .iter()
                    .any(|rep| matches!(rep, RuntimeRep::LiftedRef | RuntimeRep::UnliftedRef)),
                compiled.abi.semantic_results().clone(),
                compiled.abi.result_layout().clone(),
            )
        };
        if has_managed_arguments {
            return Err(ExecutionError::Unsupported(Unsupported::HostArguments(
                entry,
            )));
        }
        if arguments.len() != expected_arguments {
            return Err(ExecutionError::Arguments {
                expected: expected_arguments,
                actual: arguments.len(),
            });
        }
        let max_native_frame = self.program.get().pipeline.native_frame_maximum();
        let native_frame_reserve = max_native_frame
            .checked_mul(2)
            .ok_or_else(|| runtime_error_without_machine(RuntimeError::StackOverflow))?;
        let bounds = NativeStackBounds::current().map_err(runtime_error_without_machine)?;
        bounds
            .ensure_current_frame_reserve(native_frame_reserve)
            .map_err(runtime_error_without_machine)?;
        vmctx.prepared_stack_limit = bounds
            .limit_with_frame_reserve(max_native_frame)
            .map_err(runtime_error_without_machine)?;

        machine
            .begin_prepared_call()
            .map_err(ExecutionError::Runtime)?;
        machine.set_cancel_flag(cancel);
        let _cancel = CancelScope(machine);
        let mut argument_area = try_words(arguments.len())?;
        argument_area.copy_from_slice(arguments);
        let result_words =
            (result_layout.payload_size() as usize).div_ceil(std::mem::size_of::<u64>());
        let results = try_root_words(result_words.max(1))?;
        let collections_before = machine.gc_generation();
        let pointer = self.program.get().pipeline.get_function_ptr(adapter);
        let raw_status = {
            let _intrinsic = super::ActiveIntrinsicScope::new(
                machine,
                self.program.get(),
                statics,
                descriptor_registry,
            )?;
            let _scope = OldSpaceScope::new(machine, old_space)?;
            unsafe {
                let adapter: extern "C" fn(*mut VMContext, *mut u64, *const u64) -> i32 =
                    std::mem::transmute(pointer);
                adapter(vmctx, results.as_mut_ptr(), argument_area.as_ptr())
            }
        };
        let status = match CallStatus::from_raw(i64::from(raw_status)) {
            Ok(status) => status,
            Err(_) => {
                machine.set_first_cause(RuntimeError::BadPointer);
                return Err(runtime_error_from_machine(machine));
            }
        };
        if status == CallStatus::IntegrityFailure {
            machine.set_first_cause(RuntimeError::BadPointer);
        }
        if status != CallStatus::Success || machine.prepared_call_status() != CallStatus::Success {
            super::forcing::describe_raised_exception(
                machine,
                self.program.get(),
                vmctx,
                statics,
                descriptor_registry,
                old_space,
            );
            return Err(runtime_error_for_status(machine, status));
        }
        if result_contract == ResultContract::NoSuccess {
            return Err(runtime_error(machine, RuntimeError::NoSuccessReturned));
        }

        let root_mark = machine.rust_roots_len();
        register_result_roots(machine, &results, &result_layout);
        let _roots = TemporaryRoots {
            machine,
            mark: root_mark,
        };
        if options.collect_before_observation {
            collect_on(machine, vmctx, old_space, 0)?;
        }
        let result_reps = result_contract
            .returned_reps()
            .ok_or_else(|| runtime_error(machine, RuntimeError::NoSuccessReturned))?;
        let _scope = OldSpaceScope::new(machine, old_space)?;
        let result_words = results.snapshot();
        let seeds =
            match super::observe::snapshot_results(&result_words, result_reps, &result_layout) {
                Ok(seeds) => seeds,
                Err(error @ super::ObservationFailure::Integrity(_)) => {
                    machine.set_first_cause(RuntimeError::BadPointer);
                    return Err(runtime_error_from_machine_or_observation(machine, error));
                }
                Err(error) => return Err(error.into()),
            };
        let values = match super::forcing::observe_results(
            machine,
            self.program.get(),
            vmctx,
            statics,
            descriptor_registry,
            old_space,
            &seeds,
            options.observation_budget,
            crate::observation::BudgetPolicy::Complete,
        ) {
            Ok(values) => values,
            Err(ExecutionError::Observation(error @ super::ObservationFailure::Integrity(_))) => {
                machine.set_first_cause(RuntimeError::BadPointer);
                return Err(runtime_error_from_machine_or_observation(machine, error));
            }
            Err(error) => return Err(error),
        };
        Ok(RunResult {
            values,
            collections: machine.gc_generation().saturating_sub(collections_before),
        })
    }
}

impl Drop for PreparedMachine<'_> {
    fn drop(&mut self) {
        // Clear every registry and root before `self.programs` (and their
        // compiled pipelines, whose stack maps the chain points into) drop.
        // Parked frames first, so the "registered from park until take, and
        // no longer" invariant holds on every drop path.
        for frame in self.handles.drain_continuations() {
            self.machine.deregister_stowed_root(frame.cell.addr());
        }
        self.machine.clear_prepared_old_space();
        self.machine.clear_rust_roots();
        for (start, end) in self.machine.old_space_arena_ranges() {
            self.machine.retire_old_space_arena(start, end);
        }
        self.machine.free_session_heap();
        self.machine.clear_stack_map_registry();
        self.machine.clear_cancel_flag();
        self.machine.clear_prepared_entries();
        self.vmctx.machine_state = std::ptr::null_mut();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::host_fns::RuntimeError;
    use crate::machine_state::MachineDisposition;
    use crate::prepared_program::entry_tests::caf_program;
    use crate::prepared_program::{
        ExecutionError, PreparedCallOptions, PreparedMachineOptions, RunOptions,
    };
    use tidepool_repr::execution_schema::{
        link_program, parse_program, testing, Alternative, AlternativePattern, Architecture, Atom,
        CaseKind, CheckedLayout, ConstructorDecl, ConstructorId, DecodeLimits, Endianness,
        ExprFrame, FieldLayout, GlobalDecl, GlobalId, Group, HeapBinding, HeapRhs, ImportedValue,
        MachineImports, ProgramRequirements, ResultContract, RuntimeRep, ScalarLiteral, Signature,
        SignatureId, SymbolIdentity, TargetDescriptor, TopBinding, UpdatePolicy, ValueId, ValueRef,
        EXECUTION_ABI_VERSION, SCHEMA_VERSION,
    };

    fn machine() -> (PreparedMachine<'static>, ProgramId) {
        PreparedMachine::new(
            caf_program(0, false, UpdatePolicy::Memoize),
            PreparedMachineOptions {
                nursery_bytes: RunOptions::default().nursery_bytes,
            },
        )
        .expect("prepared machine")
    }

    fn language_failure_program() -> CompiledProgram {
        let mut wire = super::super::no_success_tests::raised_caf();
        wire.signatures.push(Signature {
            arguments: vec![],
            results: ResultContract::Returns(vec![RuntimeRep::Int(64)]),
        });
        wire.signatures.push(Signature {
            arguments: vec![RuntimeRep::LiftedRef],
            results: ResultContract::Returns(vec![RuntimeRep::Int(64)]),
        });
        wire.expressions
            .nodes
            .push(ExprFrame::Return(vec![Atom::Scalar(ScalarLiteral::Int {
                bits: 64,
                bytes: 7_i64.to_be_bytes().to_vec(),
            })]));
        wire.bindings.push(Group::NonRecursive(TopBinding {
            identity: testing::identity("PreparedMachine", "success"),
            binding: HeapBinding {
                id: ValueId(2),
                rhs: HeapRhs::Function {
                    signature: SignatureId(2),
                    parameters: vec![],
                    captures: vec![],
                    body: 1,
                },
            },
        }));
        wire.expressions
            .nodes
            .push(ExprFrame::Return(vec![Atom::Scalar(ScalarLiteral::Int {
                bits: 64,
                bytes: 7_i64.to_be_bytes().to_vec(),
            })]));
        wire.bindings.push(Group::NonRecursive(TopBinding {
            identity: testing::identity("PreparedMachine", "managed"),
            binding: HeapBinding {
                id: ValueId(3),
                rhs: HeapRhs::Function {
                    signature: SignatureId(3),
                    parameters: vec![ValueId(77)],
                    captures: vec![],
                    body: 2,
                },
            },
        }));
        let prepared = testing::prepare(wire).expect("language failure fixture");
        let linked =
            tidepool_repr::execution_schema::link_program(prepared, &MachineImports::default())
                .expect("language failure fixture links");
        CompiledProgram::compile(&linked).expect("language failure fixture compiles")
    }

    fn managed_roundtrip_program() -> CompiledProgram {
        let mut wire = testing::wire_program();
        wire.signatures[0].results = ResultContract::Returns(vec![RuntimeRep::LiftedRef]);
        wire.signatures.push(Signature {
            arguments: vec![RuntimeRep::LiftedRef],
            results: ResultContract::Returns(vec![RuntimeRep::LiftedRef]),
        });
        wire.constructors.push(ConstructorDecl {
            identity: testing::identity("PreparedMachine", "Unit"),
            family: testing::identity("PreparedMachine", "Unit"),
            host_id: tidepool_repr::DataConId(990),
            result_rep: RuntimeRep::LiftedRef,
            tag: 1,
            family_size: 1,
            field_reps: vec![],
            strict_fields: vec![],
            layout: CheckedLayout {
                fields: vec![],
                alignment: 1,
                payload_size: 0,
                root_mask: vec![],
            },
        });
        wire.expressions.nodes[0] = ExprFrame::Construct {
            constructor: ConstructorId(0),
            fields: vec![],
        };
        wire.expressions
            .nodes
            .push(ExprFrame::Return(vec![Atom::Ref(ValueRef::Local(
                ValueId(99),
            ))]));
        let mut body = 1;
        for id in 0..32 {
            wire.expressions.nodes.push(ExprFrame::Let {
                bindings: Group::NonRecursive(HeapBinding {
                    id: ValueId(100 + id),
                    rhs: HeapRhs::Constructor {
                        constructor: ConstructorId(0),
                        fields: vec![],
                    },
                }),
                body,
            });
            body = wire.expressions.nodes.len() - 1;
        }
        wire.bindings = vec![
            Group::NonRecursive(TopBinding {
                identity: testing::identity("PreparedMachine", "producer"),
                binding: HeapBinding {
                    id: ValueId(0),
                    rhs: HeapRhs::Thunk {
                        signature: SignatureId(0),
                        update: UpdatePolicy::Memoize,
                        captures: vec![],
                        body: 0,
                    },
                },
            }),
            Group::NonRecursive(TopBinding {
                identity: testing::identity("PreparedMachine", "consumer"),
                binding: HeapBinding {
                    id: ValueId(1),
                    rhs: HeapRhs::Function {
                        signature: SignatureId(1),
                        parameters: vec![ValueId(99)],
                        captures: vec![],
                        body,
                    },
                },
            }),
        ];
        let prepared = testing::prepare(wire).expect("roundtrip fixture");
        let linked =
            tidepool_repr::execution_schema::link_program(prepared, &MachineImports::default())
                .expect("roundtrip links");
        CompiledProgram::compile(&linked).expect("roundtrip compiles")
    }

    fn outer_with_function_field_program() -> CompiledProgram {
        let mut wire = testing::wire_program();
        wire.signatures[0].results = ResultContract::Returns(vec![RuntimeRep::LiftedRef]);
        wire.signatures.push(Signature {
            arguments: vec![],
            results: ResultContract::Returns(vec![RuntimeRep::LiftedRef]),
        });
        wire.constructors.push(ConstructorDecl {
            identity: testing::identity("PreparedOuter", "Envelope"),
            family: testing::identity("PreparedOuter", "Envelope"),
            host_id: tidepool_repr::DataConId(991),
            result_rep: RuntimeRep::LiftedRef,
            tag: 1,
            family_size: 1,
            field_reps: vec![RuntimeRep::LiftedRef, RuntimeRep::LiftedRef],
            strict_fields: vec![false, false],
            layout: CheckedLayout {
                fields: vec![
                    FieldLayout {
                        rep: RuntimeRep::LiftedRef,
                        offset: 0,
                    },
                    FieldLayout {
                        rep: RuntimeRep::LiftedRef,
                        offset: 8,
                    },
                ],
                alignment: 8,
                payload_size: 16,
                root_mask: vec![true, true],
            },
        });
        wire.constructors.push(ConstructorDecl {
            identity: testing::identity("PreparedOuter", "Unit"),
            family: testing::identity("PreparedOuter", "Unit"),
            host_id: tidepool_repr::DataConId(992),
            result_rep: RuntimeRep::LiftedRef,
            tag: 1,
            family_size: 1,
            field_reps: vec![],
            strict_fields: vec![],
            layout: CheckedLayout {
                fields: vec![],
                alignment: 1,
                payload_size: 0,
                root_mask: vec![],
            },
        });
        wire.expressions.nodes = vec![
            ExprFrame::Construct {
                constructor: ConstructorId(0),
                fields: vec![
                    Atom::Ref(ValueRef::Local(ValueId(1))),
                    Atom::Ref(ValueRef::Local(ValueId(2))),
                ],
            },
            ExprFrame::Construct {
                constructor: ConstructorId(1),
                fields: vec![],
            },
            ExprFrame::Construct {
                constructor: ConstructorId(1),
                fields: vec![],
            },
        ];
        wire.bindings = vec![
            Group::NonRecursive(TopBinding {
                identity: testing::identity("PreparedOuter", "producer"),
                binding: HeapBinding {
                    id: ValueId(0),
                    rhs: HeapRhs::Thunk {
                        signature: SignatureId(0),
                        update: UpdatePolicy::Memoize,
                        captures: vec![],
                        body: 0,
                    },
                },
            }),
            Group::NonRecursive(TopBinding {
                identity: testing::identity("PreparedOuter", "continuation"),
                binding: HeapBinding {
                    id: ValueId(1),
                    rhs: HeapRhs::Function {
                        signature: SignatureId(1),
                        parameters: vec![],
                        captures: vec![],
                        body: 1,
                    },
                },
            }),
            Group::NonRecursive(TopBinding {
                identity: testing::identity("PreparedOuter", "unforced"),
                binding: HeapBinding {
                    id: ValueId(2),
                    rhs: HeapRhs::Thunk {
                        signature: SignatureId(0),
                        update: UpdatePolicy::SingleEntry,
                        captures: vec![],
                        body: 2,
                    },
                },
            }),
        ];
        let prepared = testing::prepare(wire).expect("prepared outer fixture");
        let linked =
            tidepool_repr::execution_schema::link_program(prepared, &MachineImports::default())
                .expect("prepared outer fixture links");
        CompiledProgram::compile(&linked).expect("prepared outer fixture compiles")
    }

    fn freer_retention_program() -> (CompiledProgram, ValueId, DataConId) {
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
        let prepared = parse_program(
            include_bytes!("../../../haskell/test-prepared-stg/fixtures/freer-retention.cbor"),
            &requirements,
            DecodeLimits::default(),
        )
        .expect("FreerRetention artifact parses");
        let entry = prepared.entry();
        let effect = prepared
            .constructors()
            .iter()
            .find(|constructor| constructor.identity.occurrence == "E")
            .expect("FreerRetention artifact includes the real freer E constructor")
            .host_id;
        let linked = link_program(prepared, &MachineImports::default())
            .expect("FreerRetention artifact links");
        (
            CompiledProgram::compile(&linked).expect("FreerRetention artifact compiles"),
            entry,
            effect,
        )
    }

    /// Minimal closed CAF program returning a distinct nullary constructor,
    /// compiled against an explicit base so two of these can install side by
    /// side on one machine.
    fn base_program(host_id: u64) -> tidepool_repr::execution_schema::LinkedProgram {
        let mut wire = testing::wire_program();
        wire.signatures[0].results = ResultContract::Returns(vec![RuntimeRep::LiftedRef]);
        // One identity per host id: two programs on one machine may not
        // declare the same constructor identity differently.
        let unit = format!("Unit{host_id}");
        wire.constructors.push(ConstructorDecl {
            identity: testing::identity("MachineMulti", &unit),
            family: testing::identity("MachineMulti", &unit),
            host_id: tidepool_repr::DataConId(host_id),
            result_rep: RuntimeRep::LiftedRef,
            tag: 1,
            family_size: 1,
            field_reps: vec![],
            strict_fields: vec![],
            layout: CheckedLayout {
                fields: vec![],
                alignment: 1,
                payload_size: 0,
                root_mask: vec![],
            },
        });
        wire.expressions.nodes[0] = ExprFrame::Construct {
            constructor: ConstructorId(0),
            fields: vec![],
        };
        let Group::NonRecursive(top) = &mut wire.bindings[0] else {
            unreachable!()
        };
        top.binding.rhs = HeapRhs::Thunk {
            signature: SignatureId(0),
            update: UpdatePolicy::Memoize,
            captures: vec![],
            body: 0,
        };
        let prepared = testing::prepare(wire).expect("base_program fixture");
        link_program(prepared, &MachineImports::default()).expect("base_program fixture links")
    }

    #[test]
    fn cancellation_is_recoverable_before_a_following_entry() {
        let (mut machine, program) = machine();
        let realm = RealmId::fresh();
        machine.realm_cancel_handle(realm).cancel();

        let error = machine
            .run_entry(
                program,
                ValueId(0),
                &[],
                PreparedCallOptions {
                    observation_budget: RunOptions::default().observation_budget,
                    collect_before_observation: false,
                },
                realm,
            )
            .expect_err("cancelled entry must not publish a result");
        assert!(matches!(
            error,
            ExecutionError::Runtime(failure)
                if failure.cause == RuntimeError::Cancelled
                    && failure.disposition == MachineDisposition::Reusable
        ));
        assert_eq!(machine.disposition(), MachineDisposition::Reusable);

        let result = machine
            .run_entry(
                program,
                ValueId(0),
                &[],
                PreparedCallOptions {
                    observation_budget: RunOptions::default().observation_budget,
                    collect_before_observation: false,
                },
                RealmId::ROOT,
            )
            .expect("a settled cancellation must leave the machine reusable");
        assert_eq!(result.values.len(), 1);
        assert_eq!(machine.disposition(), MachineDisposition::Reusable);
    }

    #[test]
    fn observation_failure_is_recoverable_before_a_following_entry() {
        let (mut machine, program) = machine();
        let error = machine
            .run_entry(
                program,
                ValueId(0),
                &[],
                PreparedCallOptions {
                    observation_budget: 0,
                    collect_before_observation: false,
                },
                RealmId::ROOT,
            )
            .expect_err("bounded observation must reject a constructor at zero budget");
        assert!(matches!(
            error,
            ExecutionError::Observation(super::super::ObservationFailure::BudgetExceeded {
                limit: 0
            })
        ));
        assert_eq!(machine.disposition(), MachineDisposition::Reusable);

        let result = machine
            .run_entry(
                program,
                ValueId(0),
                &[],
                PreparedCallOptions {
                    observation_budget: RunOptions::default().observation_budget,
                    collect_before_observation: false,
                },
                RealmId::ROOT,
            )
            .expect("an observation failure must not poison the prepared machine");
        assert_eq!(result.values.len(), 1);
    }

    #[test]
    fn language_failure_is_recoverable_before_a_following_entry() {
        let (mut machine, program) = PreparedMachine::new(
            language_failure_program(),
            PreparedMachineOptions {
                nursery_bytes: RunOptions::default().nursery_bytes,
            },
        )
        .expect("prepared machine");
        let error = machine
            .run_entry(
                program,
                ValueId(0),
                &[],
                PreparedCallOptions {
                    observation_budget: RunOptions::default().observation_budget,
                    collect_before_observation: false,
                },
                RealmId::ROOT,
            )
            .expect_err("raised entry must report a language failure");
        assert!(matches!(
            error,
            ExecutionError::Runtime(failure)
                if failure.cause == RuntimeError::RaisedException
                    && failure.disposition == MachineDisposition::Reusable
        ));
        assert_eq!(machine.disposition(), MachineDisposition::Reusable);

        let result = machine
            .run_entry(
                program,
                ValueId(2),
                &[],
                PreparedCallOptions {
                    observation_budget: RunOptions::default().observation_budget,
                    collect_before_observation: false,
                },
                RealmId::ROOT,
            )
            .expect("a language failure must not poison the prepared machine");
        assert!(matches!(
            result.values.as_slice(),
            [tidepool_bridge::HaskellValue::Lit(tidepool_repr::Literal::LitInt(value))]
                if *value == 7
        ));
    }

    #[test]
    fn persistent_roots_survive_collection_between_successive_entries() {
        let (mut machine, program) = PreparedMachine::new(
            caf_program(
                0,
                false,
                tidepool_repr::execution_schema::UpdatePolicy::Memoize,
            ),
            PreparedMachineOptions {
                nursery_bytes: RunOptions::default().nursery_bytes,
            },
        )
        .expect("prepared machine");
        let initial_tops = machine.top_words(program);
        let persistent_roots = machine.persistent_roots_count(program);
        assert_eq!(persistent_roots, initial_tops.len());
        assert!(initial_tops.iter().all(|word| *word != 0));
        let first = machine
            .run_entry(
                program,
                ValueId(0),
                &[],
                PreparedCallOptions {
                    observation_budget: RunOptions::default().observation_budget,
                    collect_before_observation: false,
                },
                RealmId::ROOT,
            )
            .expect("first entry");
        let second = machine
            .run_entry(
                program,
                ValueId(0),
                &[],
                PreparedCallOptions {
                    observation_budget: RunOptions::default().observation_budget,
                    collect_before_observation: true,
                },
                RealmId::ROOT,
            )
            .expect("entry after collection");

        assert_eq!(second.collections, 1);
        assert!(matches!(
            first.values.as_slice(),
            [tidepool_bridge::HaskellValue::Con(id, fields)]
                if *id == tidepool_repr::DataConId(900) && fields.is_empty()
        ));
        assert!(matches!(
            second.values.as_slice(),
            [tidepool_bridge::HaskellValue::Con(id, fields)]
                if *id == tidepool_repr::DataConId(900) && fields.is_empty()
        ));
        assert_eq!(machine.persistent_roots_count(program), persistent_roots);
        assert!(machine.top_words(program).iter().all(|word| *word != 0));
        assert_eq!(machine.disposition(), MachineDisposition::Reusable);
    }

    #[test]
    fn machine_drop_clears_registered_roots_before_storage_drops() {
        let (machine, _program) = machine();
        let state = Rc::clone(&machine.machine);
        assert!(state.persistent_roots_count() > 0);
        drop(machine);
        assert_eq!(state.persistent_roots_count(), 0);
        assert_eq!(state.rust_roots_len(), 0);
        assert!(state.old_space_arena_ranges().is_empty());
    }

    #[test]
    fn retained_managed_result_survives_collection_and_releases() {
        let (mut machine, program) = machine();
        let batch = machine
            .run_entry_retained(
                program,
                ValueId(0),
                &[],
                PreparedCallOptions {
                    observation_budget: RunOptions::default().observation_budget,
                    collect_before_observation: true,
                },
                RealmId::ROOT,
            )
            .expect("managed result is retained before frame cleanup");
        assert!(batch.collections >= 1);
        let [PreparedResult::Managed(handle)] = batch.values.as_slice() else {
            panic!("CAF must return one retained managed value");
        };
        assert!(machine.release(*handle));
        assert!(!machine.release(*handle));
    }

    #[test]
    fn managed_inputs_reject_foreign_and_rep_mismatches_before_entry() {
        let (mut source, source_program) = machine();
        let batch = source
            .run_entry_retained(
                source_program,
                ValueId(0),
                &[],
                PreparedCallOptions {
                    observation_budget: RunOptions::default().observation_budget,
                    collect_before_observation: true,
                },
                RealmId::ROOT,
            )
            .expect("source handle");
        let [PreparedResult::Managed(handle)] = batch.values.as_slice() else {
            panic!("source result must be managed");
        };
        let (mut target, target_program) = PreparedMachine::new(
            language_failure_program(),
            PreparedMachineOptions {
                nursery_bytes: RunOptions::default().nursery_bytes,
            },
        )
        .expect("target machine");
        let options = PreparedCallOptions {
            observation_budget: RunOptions::default().observation_budget,
            collect_before_observation: false,
        };
        assert!(matches!(
            target.run_entry_retained(
                target_program,
                ValueId(3),
                &[PreparedInput::Managed(*handle)],
                options,
                RealmId::ROOT,
            ),
            Err(ExecutionError::UnknownPreparedHandle)
        ));
        let wrong_rep = PreparedHandle {
            raw: handle.raw,
            rep: RuntimeRep::UnliftedRef,
        };
        assert!(matches!(
            target.run_entry_retained(
                target_program,
                ValueId(3),
                &[PreparedInput::Managed(wrong_rep)],
                options,
                RealmId::ROOT,
            ),
            Err(ExecutionError::ArgumentRepresentation { .. })
        ));
    }

    #[test]
    fn managed_input_stays_rooted_through_collection_in_the_callee() {
        let (mut machine, program) = PreparedMachine::new(
            managed_roundtrip_program(),
            PreparedMachineOptions { nursery_bytes: 64 },
        )
        .expect("roundtrip machine");
        let options = PreparedCallOptions {
            observation_budget: RunOptions::default().observation_budget,
            collect_before_observation: false,
        };
        let producer = machine
            .run_entry_retained(program, ValueId(0), &[], options, RealmId::ROOT)
            .expect("producer result");
        let [PreparedResult::Managed(handle)] = producer.values.as_slice() else {
            panic!("producer must retain its constructor");
        };
        let consumer = machine
            .run_entry_retained(
                program,
                ValueId(1),
                &[PreparedInput::Managed(*handle)],
                options,
                RealmId::ROOT,
            )
            .expect("managed argument survives generated allocation");
        assert!(consumer.collections >= 1);
        let [PreparedResult::Managed(returned)] = consumer.values.as_slice() else {
            panic!("consumer must retain the returned managed argument");
        };
        assert!(machine.release(*handle));
        assert!(machine.release(*returned));
    }

    #[test]
    fn outer_inspection_retains_callable_fields_without_forcing_them() {
        let options = PreparedMachineOptions { nursery_bytes: 128 };
        let (mut machine, program) =
            PreparedMachine::new(outer_with_function_field_program(), options)
                .expect("prepared outer machine");
        let call = PreparedCallOptions {
            observation_budget: 0,
            collect_before_observation: true,
        };
        let produced = machine
            .run_entry_retained(program, ValueId(0), &[], call, RealmId::ROOT)
            .expect("retained outer result");
        assert!(produced.collections >= 1);
        let [PreparedResult::Managed(outer)] = produced.values.as_slice() else {
            panic!("producer must return one managed outer result");
        };
        let PreparedOuter::Constructor { identity, fields } = machine
            .inspect_outer(*outer, RealmId::ROOT)
            .expect("outer inspection must not force its callable field");
        assert_eq!(identity, tidepool_repr::DataConId(991));
        let [PreparedResult::Managed(continuation), PreparedResult::Managed(unforced)] =
            fields.as_slice()
        else {
            panic!("outer inspection must retain callable and thunk fields");
        };
        assert!(matches!(
            machine.inspect_outer(*continuation, RealmId::ROOT),
            Err(ExecutionError::Observation(
                super::super::ObservationFailure::Unobservable(
                    tidepool_heap::execution_descriptor::ObjectKind::Function
                )
            ))
        ));
        assert!(matches!(
            machine.inspect_outer(*unforced, RealmId::ROOT),
            Err(ExecutionError::Observation(
                super::super::ObservationFailure::Unobservable(
                    tidepool_heap::execution_descriptor::ObjectKind::Thunk
                )
            ))
        ));

        let PreparedOuter::Constructor { fields, .. } = machine
            .inspect_outer(*outer, RealmId::ROOT)
            .expect("source handle remains live for repeated inspection");
        let [PreparedResult::Managed(second_continuation), PreparedResult::Managed(second_unforced)] =
            fields.as_slice()
        else {
            panic!("repeated inspection must retain fresh child handles");
        };

        let (mut foreign, _foreign_program) =
            PreparedMachine::new(outer_with_function_field_program(), options)
                .expect("foreign prepared machine");
        assert!(matches!(
            foreign.inspect_outer(*outer, RealmId::ROOT),
            Err(ExecutionError::UnknownPreparedHandle)
        ));
        assert!(machine.release(*outer));
        assert!(matches!(
            machine.inspect_outer(*outer, RealmId::ROOT),
            Err(ExecutionError::UnknownPreparedHandle)
        ));
        assert!(machine.release(*continuation));
        assert!(machine.release(*unforced));
        assert!(machine.release(*second_continuation));
        assert!(machine.release(*second_unforced));
    }

    #[test]
    fn outer_inspection_refuses_an_unavailable_machine_before_handle_lookup() {
        let (mut machine, program) = machine();
        let batch = machine
            .run_entry_retained(
                program,
                ValueId(0),
                &[],
                PreparedCallOptions {
                    observation_budget: 0,
                    collect_before_observation: false,
                },
                RealmId::ROOT,
            )
            .expect("retained source handle");
        let [PreparedResult::Managed(handle)] = batch.values.as_slice() else {
            panic!("CAF must return one managed value");
        };
        machine.machine.set_first_cause(RuntimeError::BadPointer);
        assert!(matches!(
            machine.inspect_outer(*handle, RealmId::ROOT),
            Err(ExecutionError::Runtime(failure))
                if failure.cause == RuntimeError::BadPointer
                    && failure.disposition == MachineDisposition::Unavailable
        ));
    }

    #[test]
    fn real_freer_request_retains_its_continuation_across_another_collection() {
        let (program, entry, effect) = freer_retention_program();
        let (mut machine, program_id) = PreparedMachine::new(
            program,
            PreparedMachineOptions {
                nursery_bytes: 4096,
            },
        )
        .expect("FreerRetention machine");
        let call = PreparedCallOptions {
            observation_budget: 0,
            collect_before_observation: false,
        };
        let first = machine
            .run_entry_retained(program_id, entry, &[], call, RealmId::ROOT)
            .expect("real freer request returns a retained outer value");
        let [PreparedResult::Managed(outer)] = first.values.as_slice() else {
            panic!("FreerRetention entry must return one managed E request");
        };
        let second = machine
            .run_entry_retained(
                program_id,
                entry,
                &[],
                PreparedCallOptions {
                    collect_before_observation: true,
                    ..call
                },
                RealmId::ROOT,
            )
            .expect("second real freer request collects without losing the first");
        assert!(second.collections >= 1);
        let [PreparedResult::Managed(second_outer)] = second.values.as_slice() else {
            panic!("second FreerRetention request must also be managed");
        };
        let PreparedOuter::Constructor { identity, fields } = machine
            .inspect_outer(*outer, RealmId::ROOT)
            .expect("first E request remains rooted after the later collection");
        assert_eq!(
            identity, effect,
            "descriptor metadata, not tag, identifies E"
        );
        let continuation = match fields.last() {
            Some(PreparedResult::Managed(handle)) => *handle,
            _ => panic!("the real E continuation field must remain an opaque managed handle"),
        };
        let children: Vec<_> = fields
            .iter()
            .filter_map(|field| match field {
                PreparedResult::Managed(handle) => Some(*handle),
                PreparedResult::Void | PreparedResult::Scalar(_) => None,
            })
            .collect();
        assert!(machine.release(*outer));
        assert!(children.contains(&continuation));
        for child in children {
            assert!(machine.release(child));
        }
        assert!(machine.release(*second_outer));
    }

    #[test]
    fn two_closed_programs_share_one_machine_across_a_forced_collection() {
        let (mut machine, program_a) = PreparedMachine::new(
            CompiledProgram::compile(&base_program(950)).expect("A compiles"),
            PreparedMachineOptions {
                nursery_bytes: RunOptions::default().nursery_bytes,
            },
        )
        .expect("first program installs");
        let compiled_b = machine
            .compile_for_install(&base_program(951))
            .expect("B compiles against the machine interner");
        let program_b = machine
            .install_program(compiled_b, ImportBindings::new())
            .expect("second program installs alongside the first, on the same machine");

        let options = PreparedCallOptions {
            observation_budget: RunOptions::default().observation_budget,
            collect_before_observation: false,
        };
        let before_a = machine
            .run_entry(program_a, ValueId(0), &[], options, RealmId::ROOT)
            .expect("program A entry before collection");
        let before_b = machine
            .run_entry(program_b, ValueId(0), &[], options, RealmId::ROOT)
            .expect("program B entry before collection");
        assert!(matches!(
            before_a.values.as_slice(),
            [tidepool_bridge::HaskellValue::Con(id, fields)]
                if *id == tidepool_repr::DataConId(950) && fields.is_empty()
        ));
        assert!(matches!(
            before_b.values.as_slice(),
            [tidepool_bridge::HaskellValue::Con(id, fields)]
                if *id == tidepool_repr::DataConId(951) && fields.is_empty()
        ));

        let a_roots_before = machine.persistent_roots_count(program_a);
        let a_tops_before = machine.top_words(program_a);
        assert_eq!(a_roots_before, 1);
        assert_eq!(a_tops_before.len(), 1);
        assert_ne!(a_tops_before[0], 0);
        let b_roots_before = machine.persistent_roots_count(program_b);
        let b_tops = machine.top_words(program_b);
        assert_eq!(b_roots_before, 1);
        assert_eq!(b_tops.len(), 1);
        assert_ne!(b_tops[0], 0);
        // Disjoint, non-overlapping slot ranges: B's own slot can never alias
        // A's, so neither program's generated code can observe the other's
        // table cell.
        assert_ne!(a_tops_before[0], b_tops[0]);

        let collect = PreparedCallOptions {
            collect_before_observation: true,
            ..options
        };
        let after_a = machine
            .run_entry(program_a, ValueId(0), &[], collect, RealmId::ROOT)
            .expect("program A entry after a forced collection");
        let after_b = machine
            .run_entry(program_b, ValueId(0), &[], collect, RealmId::ROOT)
            .expect("program B entry after a forced collection");
        assert!(after_a.collections >= 1);
        assert!(after_b.collections >= 1);
        assert!(matches!(
            after_a.values.as_slice(),
            [tidepool_bridge::HaskellValue::Con(id, fields)]
                if *id == tidepool_repr::DataConId(950) && fields.is_empty()
        ));
        assert!(matches!(
            after_b.values.as_slice(),
            [tidepool_bridge::HaskellValue::Con(id, fields)]
                if *id == tidepool_repr::DataConId(951) && fields.is_empty()
        ));

        // Installing B, and a moving collection driven from either program,
        // never deregistered either program's already-claimed root: same
        // persistent-root count as observed right after each program's own
        // install, and each slot still resolves to a live object. A copying
        // collector relocates the object and rewrites the table cell's
        // *contents* in place -- the cell's own storage address (not checked
        // here) is what must never move, guaranteed by `RootWords` never
        // reallocating after creation -- so the cell's *content* changing is
        // exactly the positive evidence that this invocation's collection
        // really walked and updated this program's root, for BOTH programs,
        // not merely the first one installed. This is the assertion that a
        // silently-skipped `register_persistent_root` for any
        // second-or-later installed program would fail: without a live
        // persistent root, a copying collection has nothing to update in
        // place, so the table cell would keep its pre-collection value even
        // though the bytes it points to (now-abandoned from-space) are no
        // longer valid -- and a values-only assertion after that collection
        // could still coincidentally read back correct data before that
        // stale memory is overwritten by later allocation.
        assert_eq!(machine.persistent_roots_count(program_a), a_roots_before);
        assert_ne!(machine.top_words(program_a)[0], 0);
        assert_ne!(machine.top_words(program_a)[0], a_tops_before[0]);
        assert_eq!(machine.persistent_roots_count(program_b), b_roots_before);
        assert_ne!(machine.top_words(program_b)[0], 0);
        assert_ne!(machine.top_words(program_b)[0], b_tops[0]);
        assert_eq!(machine.disposition(), MachineDisposition::Reusable);
    }

    // ---- S2 acceptance: T1, T2, T3 pass (T2 closed by X2, see its doc -----
    // ---- comment above); T4 is still not attempted -------------------------
    //
    // T4 ("a variant of T2") needs a genuinely static, `StaticImage`-embedded
    // (not nursery-allocated) object reached through a cross-program CLOSURE
    // CALL, plus its own new-fixture cost. The machine-wide `static_regions`
    // union (`tidepool-heap/src/gc/raw.rs`) is exercised by every
    // `PreparedMachine` install and directly by
    // `s2b_a_static_object_is_retained_through_b_via_the_shared_static_region_set`;
    // only the specific proof that a cross-program *closure result* takes
    // that already-stable path is not pinned by a test here.

    /// A constructor with one scalar field, as a zero-argument CAF -- used as
    /// program A's producer for T1 (cross-program managed argument/result).
    fn field_constructor_program(host_id: u64) -> CompiledProgram {
        let mut wire = testing::wire_program();
        wire.signatures[0].results = ResultContract::Returns(vec![RuntimeRep::LiftedRef]);
        wire.constructors.push(ConstructorDecl {
            identity: testing::identity("MachineImports", "Field"),
            family: testing::identity("MachineImports", "Field"),
            host_id: tidepool_repr::DataConId(host_id),
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
                bytes: 42_i64.to_be_bytes().to_vec(),
            })],
        };
        let Group::NonRecursive(top) = &mut wire.bindings[0] else {
            unreachable!()
        };
        top.binding.rhs = HeapRhs::Thunk {
            signature: SignatureId(0),
            update: UpdatePolicy::Memoize,
            captures: vec![],
            body: 0,
        };
        let prepared = testing::prepare(wire).expect("field_constructor_program fixture");
        let linked = link_program(prepared, &MachineImports::default())
            .expect("field_constructor_program fixture links");
        CompiledProgram::compile(&linked).expect("field_constructor_program fixture compiles")
    }

    /// A one-argument entry `consume :: LiftedRef -> LiftedRef` that
    /// allocates 32 throwaway constructors (forcing a collection in a tiny
    /// nursery) before returning its own managed argument unchanged -- used
    /// as program B for T1.
    fn managed_argument_consumer_program() -> tidepool_repr::execution_schema::LinkedProgram {
        let mut wire = testing::wire_program();
        wire.signatures[0] = Signature {
            arguments: vec![RuntimeRep::LiftedRef],
            results: ResultContract::Returns(vec![RuntimeRep::LiftedRef]),
        };
        wire.constructors.push(ConstructorDecl {
            identity: testing::identity("MachineImports", "Filler"),
            family: testing::identity("MachineImports", "Filler"),
            host_id: tidepool_repr::DataConId(971),
            result_rep: RuntimeRep::LiftedRef,
            tag: 1,
            family_size: 1,
            field_reps: vec![],
            strict_fields: vec![],
            layout: CheckedLayout {
                fields: vec![],
                alignment: 1,
                payload_size: 0,
                root_mask: vec![],
            },
        });
        wire.expressions.nodes[0] =
            ExprFrame::Return(vec![Atom::Ref(ValueRef::Local(ValueId(99)))]);
        let mut body = 0;
        for id in 0..32 {
            wire.expressions.nodes.push(ExprFrame::Let {
                bindings: Group::NonRecursive(HeapBinding {
                    id: ValueId(100 + id),
                    rhs: HeapRhs::Constructor {
                        constructor: ConstructorId(0),
                        fields: vec![],
                    },
                }),
                body,
            });
            body = wire.expressions.nodes.len() - 1;
        }
        let Group::NonRecursive(top) = &mut wire.bindings[0] else {
            unreachable!()
        };
        top.binding.rhs = HeapRhs::Function {
            signature: SignatureId(0),
            parameters: vec![ValueId(99)],
            captures: vec![],
            body,
        };
        let prepared = testing::prepare(wire).expect("managed_argument_consumer_program fixture");
        link_program(prepared, &MachineImports::default())
            .expect("managed_argument_consumer_program fixture links")
    }

    #[test]
    fn t1_managed_argument_crosses_installed_programs_with_identity_preserved() {
        let (mut machine, program_a) = PreparedMachine::new(
            field_constructor_program(980),
            PreparedMachineOptions { nursery_bytes: 64 },
        )
        .expect("A installs");
        let program_b = install_linked(
            &mut machine,
            &managed_argument_consumer_program(),
            ImportBindings::new(),
        )
        .expect("B (64-byte-class nursery pressure via 32 allocations) installs alongside A");

        let call = PreparedCallOptions {
            observation_budget: RunOptions::default().observation_budget,
            collect_before_observation: false,
        };
        let produced = machine
            .run_entry_retained(program_a, ValueId(0), &[], call, RealmId::ROOT)
            .expect("A produces a retained constructor with a field");
        let [PreparedResult::Managed(handle)] = produced.values.as_slice() else {
            panic!("A must return one managed constructor");
        };
        let original_pointer = machine
            .handle_current_pointer(*handle)
            .expect("A's handle is live");

        let consumed = machine
            .run_entry_retained(
                program_b,
                ValueId(0),
                &[PreparedInput::Managed(*handle)],
                call,
                RealmId::ROOT,
            )
            .expect("B accepts A's handle as a managed argument");
        assert!(
            consumed.collections >= 1,
            "B's 32 throwaway allocations in a tiny nursery must force at least one collection"
        );
        let [PreparedResult::Managed(returned)] = consumed.values.as_slice() else {
            panic!("B must retain the returned argument");
        };

        let PreparedOuter::Constructor { identity, .. } = machine
            .inspect_outer(*returned, RealmId::ROOT)
            .expect("the returned handle inspects through the machine-wide union");
        assert_eq!(
            identity,
            tidepool_repr::DataConId(980),
            "the value crossing programs keeps A's constructor identity"
        );
        let returned_pointer = machine
            .handle_current_pointer(*returned)
            .expect("B's returned handle is live");
        assert_eq!(
            original_pointer, returned_pointer,
            "identity, not a copy: B returned the SAME heap object A produced"
        );

        // Repeat after a forced collection driven from A's own entry: an
        // already-stable (old-space) pointer must not move, and both
        // handles must still agree.
        let after = machine
            .run_entry(
                program_a,
                ValueId(0),
                &[],
                PreparedCallOptions {
                    collect_before_observation: true,
                    ..call
                },
                RealmId::ROOT,
            )
            .expect("A's own entry still runs and forces a collection");
        assert!(after.collections >= 1);
        assert_eq!(
            machine.handle_current_pointer(*handle),
            machine.handle_current_pointer(*returned)
        );
        assert_eq!(
            machine.handle_current_pointer(*handle),
            Some(original_pointer)
        );

        assert!(machine.release(*handle));
        assert!(machine.release(*returned));
        assert_eq!(machine.handle_count(), 0);
    }

    #[test]
    fn t3_install_failure_after_registries_extend_leaves_every_installed_program_functional() {
        // A tiny forced ceiling makes ANY nonzero second-program heap-top
        // reserve exceed it, deterministically, regardless of exact object
        // byte sizes -- see `collect_prepared`'s `reserve > ceiling` check
        // (tidepool-codegen/src/host_fns/gc.rs), which this override targets
        // without needing a multi-gigabyte test program.
        crate::host_fns::set_max_heap_bytes_for_test(8);
        let (mut machine, program_a) = PreparedMachine::new(
            first(&base_program(990)),
            PreparedMachineOptions {
                nursery_bytes: RunOptions::default().nursery_bytes,
            },
        )
        .expect("A installs");
        let call = PreparedCallOptions {
            observation_budget: RunOptions::default().observation_budget,
            collect_before_observation: false,
        };
        machine
            .run_entry(program_a, ValueId(0), &[], call, RealmId::ROOT)
            .expect("A runs once before B's install is attempted");
        let a_roots_before = machine.persistent_roots_count(program_a);
        let total_roots_before = machine.total_persistent_roots();
        let candidate = machine
            .compile_for_install(&base_program(991))
            .expect("B compiles for install");
        let candidate_descriptor = Arc::downgrade(&candidate.interned_constructors[0].1);
        let error = machine
            .install_program(candidate, ImportBindings::new())
            .expect_err("B's heap-top reserve cannot fit under the forced test ceiling, even after a collection");
        crate::host_fns::clear_max_heap_bytes_override();

        assert!(matches!(
            error,
            ExecutionError::Runtime(failure)
                if failure.disposition == MachineDisposition::Reusable
        ));
        assert_eq!(machine.disposition(), MachineDisposition::Reusable);
        assert!(
            candidate_descriptor.upgrade().is_none(),
            "failed install must release candidate descriptor ownership"
        );
        assert_eq!(machine.persistent_roots_count(program_a), a_roots_before);
        // The candidate's block roots were deregistered with the rollback,
        // before the block itself dropped with the candidate.
        assert_eq!(
            machine.total_persistent_roots(),
            total_roots_before,
            "a rejected install leaves no root registered"
        );
        let handle = machine
            .retain_top(program_a, ValueId(0))
            .expect("failed install leaves no pending collection failure");
        assert_eq!(machine.close_realm(RealmId::ROOT), (0, 0));
        assert!(machine.handle_root(handle).is_some());
        install_linked(&mut machine, &base_program(992), ImportBindings::new())
            .expect("a new install succeeds without running an entry first");
        assert!(machine.release(handle));

        let result = machine
            .run_entry(
                program_a,
                ValueId(0),
                &[],
                PreparedCallOptions {
                    collect_before_observation: true,
                    ..call
                },
                RealmId::ROOT,
            )
            .expect(
                "A still runs correctly with collect_before_observation after the rejected install",
            );
        assert!(matches!(
            result.values.as_slice(),
            [tidepool_bridge::HaskellValue::Con(id, fields)]
                if *id == tidepool_repr::DataConId(990) && fields.is_empty()
        ));
    }

    /// Program A for T2: a memoized CAF `producer` returning
    /// `Envelope(f, unforced)`, where `f` is a zero-argument closure
    /// (reachable only through the constructor field, never forced by
    /// inspection) whose body -- once actually CALLED -- allocates 32
    /// throwaway constructors before constructing and returning a fresh,
    /// distinctly-tagged `MakeResult` value. Mirrors
    /// `outer_with_function_field_program`'s shape.
    fn closure_producer_program() -> CompiledProgram {
        let mut wire = testing::wire_program();
        wire.signatures[0].results = ResultContract::Returns(vec![RuntimeRep::LiftedRef]);
        wire.signatures.push(Signature {
            arguments: vec![],
            results: ResultContract::Returns(vec![RuntimeRep::LiftedRef]),
        });
        wire.constructors.push(ConstructorDecl {
            identity: testing::identity("MachineClosure", "Envelope"),
            family: testing::identity("MachineClosure", "Envelope"),
            host_id: tidepool_repr::DataConId(981),
            result_rep: RuntimeRep::LiftedRef,
            tag: 1,
            family_size: 1,
            field_reps: vec![RuntimeRep::LiftedRef, RuntimeRep::LiftedRef],
            strict_fields: vec![false, false],
            layout: CheckedLayout {
                fields: vec![
                    FieldLayout {
                        rep: RuntimeRep::LiftedRef,
                        offset: 0,
                    },
                    FieldLayout {
                        rep: RuntimeRep::LiftedRef,
                        offset: 8,
                    },
                ],
                alignment: 8,
                payload_size: 16,
                root_mask: vec![true, true],
            },
        });
        wire.constructors.push(ConstructorDecl {
            identity: testing::identity("MachineClosure", "Unit"),
            family: testing::identity("MachineClosure", "Unit"),
            host_id: tidepool_repr::DataConId(982),
            result_rep: RuntimeRep::LiftedRef,
            tag: 1,
            family_size: 1,
            field_reps: vec![],
            strict_fields: vec![],
            layout: CheckedLayout {
                fields: vec![],
                alignment: 1,
                payload_size: 0,
                root_mask: vec![],
            },
        });
        wire.constructors.push(ConstructorDecl {
            identity: testing::identity("MachineClosure", "MakeResult"),
            family: testing::identity("MachineClosure", "MakeResult"),
            host_id: tidepool_repr::DataConId(983),
            result_rep: RuntimeRep::LiftedRef,
            tag: 1,
            family_size: 1,
            field_reps: vec![],
            strict_fields: vec![],
            layout: CheckedLayout {
                fields: vec![],
                alignment: 1,
                payload_size: 0,
                root_mask: vec![],
            },
        });
        // node 0: Envelope(f, unforced) -- producer's body.
        // node 1: MakeResult -- the seed f's let-chain wraps.
        // node 2: Unit -- unforced's body.
        wire.expressions.nodes = vec![
            ExprFrame::Construct {
                constructor: ConstructorId(0),
                fields: vec![
                    Atom::Ref(ValueRef::Local(ValueId(1))),
                    Atom::Ref(ValueRef::Local(ValueId(2))),
                ],
            },
            ExprFrame::Construct {
                constructor: ConstructorId(2),
                fields: vec![],
            },
            ExprFrame::Construct {
                constructor: ConstructorId(1),
                fields: vec![],
            },
        ];
        let mut f_body = 1;
        for id in 0..32 {
            wire.expressions.nodes.push(ExprFrame::Let {
                bindings: Group::NonRecursive(HeapBinding {
                    id: ValueId(200 + id),
                    rhs: HeapRhs::Constructor {
                        constructor: ConstructorId(1),
                        fields: vec![],
                    },
                }),
                body: f_body,
            });
            f_body = wire.expressions.nodes.len() - 1;
        }
        wire.bindings = vec![
            Group::NonRecursive(TopBinding {
                identity: testing::identity("MachineClosure", "producer"),
                binding: HeapBinding {
                    id: ValueId(0),
                    rhs: HeapRhs::Thunk {
                        signature: SignatureId(0),
                        update: UpdatePolicy::Memoize,
                        captures: vec![],
                        body: 0,
                    },
                },
            }),
            Group::NonRecursive(TopBinding {
                identity: testing::identity("MachineClosure", "f"),
                binding: HeapBinding {
                    id: ValueId(1),
                    rhs: HeapRhs::Function {
                        signature: SignatureId(1),
                        parameters: vec![],
                        captures: vec![],
                        body: f_body,
                    },
                },
            }),
            Group::NonRecursive(TopBinding {
                identity: testing::identity("MachineClosure", "unforced"),
                binding: HeapBinding {
                    id: ValueId(2),
                    rhs: HeapRhs::Thunk {
                        signature: SignatureId(0),
                        update: UpdatePolicy::SingleEntry,
                        captures: vec![],
                        body: 2,
                    },
                },
            }),
        ];
        let prepared = testing::prepare(wire).expect("closure_producer_program fixture");
        let linked = link_program(prepared, &MachineImports::default())
            .expect("closure_producer_program fixture links");
        CompiledProgram::compile(&linked).expect("closure_producer_program fixture compiles")
    }

    /// Program B for T2: a one-argument entry `caller :: LiftedRef ->
    /// LiftedRef` that calls its argument as a zero-argument closure through
    /// `emit_exact_call`'s generic dispatcher (a `Local` callee, never a
    /// same-program top reference) and returns the call's own result in tail
    /// position (no separate `Return` needed -- `ExprFrame::Call` at body
    /// position already finishes the entry).
    fn closure_caller_program() -> tidepool_repr::execution_schema::LinkedProgram {
        let mut wire = testing::wire_program();
        wire.signatures[0] = Signature {
            arguments: vec![RuntimeRep::LiftedRef],
            results: ResultContract::Returns(vec![RuntimeRep::LiftedRef]),
        };
        wire.signatures.push(Signature {
            arguments: vec![],
            results: ResultContract::Returns(vec![RuntimeRep::LiftedRef]),
        });
        wire.expressions.nodes[0] = ExprFrame::Call {
            callee: Atom::Ref(ValueRef::Local(ValueId(50))),
            signature: SignatureId(1),
            arguments: vec![],
        };
        let Group::NonRecursive(top) = &mut wire.bindings[0] else {
            unreachable!()
        };
        top.binding.rhs = HeapRhs::Function {
            signature: SignatureId(0),
            parameters: vec![ValueId(50)],
            captures: vec![],
            body: 0,
        };
        let prepared = testing::prepare(wire).expect("closure_caller_program fixture");
        link_program(prepared, &MachineImports::default())
            .expect("closure_caller_program fixture links")
    }

    /// HISTORY: at S2 this test was a pinned `#[ignore]`d FINDING, not a
    /// regression from this card. `emit_exact_call`'s dispatcher
    /// (`apply.rs::emit_dispatchers`, the terminal fallback) matched the
    /// entered callee's header word against ONLY the COMPILING program's own
    /// `plan.functions`/`plan.pap_layouts` -- a closed, closed-world table
    /// baked in at that program's own codegen time. A closure produced by a
    /// DIFFERENT installed program had a descriptor that program never
    /// declared, so the match loop found nothing and fell through to
    /// `emit_bad_state` -> `BadThunkState(0)`, Unavailable. S2's shared
    /// `DescriptorSpace` fixed GC-time recognition (the collector walking an
    /// object by tag) -- a different concern from compile-time call-target
    /// recognition (which native function to jump to), which this test needed
    /// but S2 alone did not provide.
    ///
    /// X2 closes it: `apply.rs::emit_dispatchers`' terminal fallback now
    /// calls the host fn `prepared_resolve_call(vmctx, header, demand)`,
    /// and `PreparedMachine::install` publishes machine-wide dispatch records via
    /// `MachineState::register_prepared_entries` as its LAST step, so a
    /// foreign callee this machine actually owns resolves and its code runs
    /// with the caller's frame live. Only a genuine signature mismatch or an
    /// unowned header still misses (see
    /// `x2_foreign_callee_with_mismatching_signature_is_a_typed_reusable_failure`).
    #[test]
    fn t2_closure_crosses_programs_and_collects_inside_the_producing_program() {
        let (mut machine, program_a) = PreparedMachine::new(
            closure_producer_program(),
            PreparedMachineOptions { nursery_bytes: 128 },
        )
        .expect("A installs");
        let program_b = install_linked(
            &mut machine,
            &closure_caller_program(),
            ImportBindings::new(),
        )
        .expect(
            "B installs alongside A, extending the shared descriptor space and stack-map chain",
        );

        let call = PreparedCallOptions {
            observation_budget: 0,
            collect_before_observation: true,
        };
        let produced = machine
            .run_entry_retained(program_a, ValueId(0), &[], call, RealmId::ROOT)
            .expect("A produces its Envelope(f, unforced)");
        let [PreparedResult::Managed(outer)] = produced.values.as_slice() else {
            panic!("A's producer must return one managed Envelope");
        };
        let PreparedOuter::Constructor { identity, fields } = machine
            .inspect_outer(*outer, RealmId::ROOT)
            .expect("A's outer inspects without forcing its callable field");
        assert_eq!(identity, tidepool_repr::DataConId(981));
        let [PreparedResult::Managed(f), PreparedResult::Managed(unforced)] = fields.as_slice()
        else {
            panic!("Envelope's fields must both remain retained, opaque managed handles");
        };

        let called = machine
            .run_entry_retained(
                program_b,
                ValueId(0),
                &[PreparedInput::Managed(*f)],
                call,
                RealmId::ROOT,
            )
            .expect(
                "B calls A's closure through the shared dispatcher; A's code runs with B's frame live",
            );
        assert!(
            called.collections >= 1,
            "f's 32 allocations, running as B's callee, must force a collection that walks BOTH \
             B's calling frame and A's executing frame -- this is the proof that the stack-map \
             chain (not just the first program's registry) is consulted"
        );
        let [PreparedResult::Managed(result)] = called.values.as_slice() else {
            panic!("the call's result must be retained as one managed value");
        };
        let PreparedOuter::Constructor {
            identity: result_identity,
            ..
        } = machine
            .inspect_outer(*result, RealmId::ROOT)
            .expect("the call's result inspects through the machine-wide descriptor union");
        assert_eq!(
            result_identity,
            tidepool_repr::DataConId(983),
            "the returned value is A's fresh MakeResult constructor"
        );

        assert!(machine.release(*outer));
        assert!(machine.release(*f));
        assert!(machine.release(*unforced));
        assert!(machine.release(*result));
        assert_eq!(machine.handle_count(), 0);
    }

    /// Program B' for the X2 signature-mismatch test: like
    /// [`closure_caller_program`] but applies its first argument TO its
    /// second through a one-argument call site. A's `f` is a zero-argument
    /// function, so the call-site signature (argument reps and result
    /// contract) can never match A's exported one.
    fn closure_miscaller_program() -> tidepool_repr::execution_schema::LinkedProgram {
        let mut wire = testing::wire_program();
        wire.signatures[0] = Signature {
            arguments: vec![RuntimeRep::LiftedRef, RuntimeRep::LiftedRef],
            results: ResultContract::Returns(vec![RuntimeRep::LiftedRef]),
        };
        wire.signatures.push(Signature {
            arguments: vec![RuntimeRep::LiftedRef],
            results: ResultContract::Returns(vec![RuntimeRep::LiftedRef]),
        });
        wire.expressions.nodes[0] = ExprFrame::Call {
            callee: Atom::Ref(ValueRef::Local(ValueId(50))),
            signature: SignatureId(1),
            arguments: vec![Atom::Ref(ValueRef::Local(ValueId(51)))],
        };
        let Group::NonRecursive(top) = &mut wire.bindings[0] else {
            unreachable!()
        };
        top.binding.rhs = HeapRhs::Function {
            signature: SignatureId(0),
            parameters: vec![ValueId(50), ValueId(51)],
            captures: vec![],
            body: 0,
        };
        let prepared = testing::prepare(wire).expect("closure_miscaller_program fixture");
        link_program(prepared, &MachineImports::default())
            .expect("closure_miscaller_program fixture links")
    }

    /// X2: a foreign callee the machine knows but cannot serve at this call
    /// site (here: an arity/signature mismatch) is an ordinary, reusable
    /// `UnresolvedCallee`. The
    /// decision is made before any code of A runs and the heap is untouched,
    /// so the machine latch stays clear and the next entry succeeds. Pins the
    /// two halves of that contract that were previously wrong: the miss block
    /// must not record a second (`BadThunkState`) cause, and a reusable cause
    /// must never latch.
    #[test]
    fn x2_foreign_callee_with_mismatching_signature_is_a_typed_reusable_failure() {
        let (mut machine, program_a) = PreparedMachine::new(
            closure_producer_program(),
            PreparedMachineOptions {
                nursery_bytes: 4096,
            },
        )
        .expect("A installs");
        let program_b = install_linked(
            &mut machine,
            &closure_miscaller_program(),
            ImportBindings::new(),
        )
        .expect("B' installs alongside A");
        let call = PreparedCallOptions {
            observation_budget: 0,
            collect_before_observation: true,
        };
        let produced = machine
            .run_entry_retained(program_a, ValueId(0), &[], call, RealmId::ROOT)
            .expect("A produces its Envelope(f, unforced)");
        let [PreparedResult::Managed(outer)] = produced.values.as_slice() else {
            panic!("A's producer must return one managed Envelope");
        };
        let PreparedOuter::Constructor { fields, .. } = machine
            .inspect_outer(*outer, RealmId::ROOT)
            .expect("A's outer inspects");
        let [PreparedResult::Managed(f), PreparedResult::Managed(unforced)] = fields.as_slice()
        else {
            panic!("Envelope's fields must both be managed handles");
        };

        let error = machine
            .run_entry_retained(
                program_b,
                ValueId(0),
                &[
                    PreparedInput::Managed(*f),
                    PreparedInput::Managed(*unforced),
                ],
                call,
                RealmId::ROOT,
            )
            .expect_err(
                "a one-argument application of a zero-argument foreign closure cannot resolve",
            );
        let ExecutionError::Runtime(failure) = error else {
            panic!("the failure is the machine-reported runtime cause, got {error:?}");
        };
        assert_eq!(failure.cause, RuntimeError::UnresolvedCallee);
        assert_eq!(failure.disposition, MachineDisposition::Reusable);
        assert_eq!(machine.disposition(), MachineDisposition::Reusable);
        assert_eq!(
            machine.failure(),
            None,
            "a reusable cause is the call's outcome, never the machine latch"
        );

        // The very next observation and entry see a clean machine.
        let PreparedOuter::Constructor {
            identity,
            fields: fields_again,
        } = machine
            .inspect_outer(*outer, RealmId::ROOT)
            .expect("observation after a reusable failure reports the value, not a stale cause");
        assert_eq!(identity, tidepool_repr::DataConId(981));
        for field in fields_again {
            let PreparedResult::Managed(handle) = field else {
                panic!("Envelope's fields are managed");
            };
            assert!(machine.release(handle));
        }
        let again = machine
            .run_entry_retained(program_a, ValueId(0), &[], call, RealmId::ROOT)
            .expect("A's entry runs again on the still-reusable machine");
        let [PreparedResult::Managed(outer_again)] = again.values.as_slice() else {
            panic!("A's producer must return one managed Envelope");
        };

        assert!(machine.release(*outer));
        assert!(machine.release(*f));
        assert!(machine.release(*unforced));
        assert!(machine.release(*outer_again));
        assert_eq!(machine.handle_count(), 0);
    }

    /// A for the G0 scalar-return test: a zero-argument FUNCTION (not a
    /// thunk) top returning the raw `Int(64)` 7, retained as a value so B
    /// can apply it as a dynamic callee.
    fn scalar_returning_function_program() -> CompiledProgram {
        let mut wire = testing::wire_program();
        wire.expressions.nodes[0] = ExprFrame::Return(vec![Atom::Scalar(ScalarLiteral::Int {
            bits: 64,
            bytes: 7_i64.to_be_bytes().to_vec(),
        })]);
        let prepared = testing::prepare(wire).expect("scalar function fixture");
        let linked = link_program(prepared, &MachineImports::default())
            .expect("scalar function fixture links");
        CompiledProgram::compile(&linked).expect("scalar function fixture compiles")
    }

    /// B for the G0 scalar-return test: entry `(LiftedRef) -> Int(64)`
    /// applying its argument as `() -> Int(64)`. B declares NO function of
    /// the demanded shape, so before G0 admission refused the whole program
    /// (`admission_admits_a_dynamic_callee_without_a_locally_shaped_function`
    /// pins the admission half; this fixture proves the runtime half).
    fn scalar_dynamic_caller_program() -> tidepool_repr::execution_schema::LinkedProgram {
        let mut wire = testing::wire_program();
        wire.signatures[0] = Signature {
            arguments: vec![RuntimeRep::LiftedRef],
            results: ResultContract::Returns(vec![RuntimeRep::Int(64)]),
        };
        wire.signatures.push(Signature {
            arguments: vec![],
            results: ResultContract::Returns(vec![RuntimeRep::Int(64)]),
        });
        wire.expressions.nodes[0] = ExprFrame::Call {
            callee: Atom::Ref(ValueRef::Local(ValueId(50))),
            signature: SignatureId(1),
            arguments: vec![],
        };
        let Group::NonRecursive(top) = &mut wire.bindings[0] else {
            unreachable!()
        };
        top.binding.rhs = HeapRhs::Function {
            signature: SignatureId(0),
            parameters: vec![ValueId(50)],
            captures: vec![],
            body: 0,
        };
        let prepared = testing::prepare(wire).expect("scalar dynamic caller fixture");
        link_program(prepared, &MachineImports::default())
            .expect("scalar dynamic caller fixture links")
    }

    /// G0: a dynamic callee whose demanded shape matches NO local function
    /// is admitted and served through the machine-wide resolver -- the
    /// scalar-returning shape stage F recorded as an unexplained
    /// `Unsupported`, now explained (closed-world admission) and closed.
    #[test]
    fn g0_dynamic_scalar_returning_call_resolves_a_foreign_function() {
        let (mut machine, program_a) = PreparedMachine::new(
            scalar_returning_function_program(),
            PreparedMachineOptions {
                nursery_bytes: 4096,
            },
        )
        .expect("A installs");
        let function = machine
            .retain_top(program_a, ValueId(0))
            .expect("A's function top is retained as a value");
        let program_b = install_linked(
            &mut machine,
            &scalar_dynamic_caller_program(),
            ImportBindings::new(),
        )
        .expect("B installs: its dynamic call is admitted without a locally shaped function");
        let call = PreparedCallOptions {
            observation_budget: 0,
            collect_before_observation: false,
        };
        let called = machine
            .run_entry_retained(
                program_b,
                ValueId(0),
                &[PreparedInput::Managed(function)],
                call,
                RealmId::ROOT,
            )
            .expect("B applies A's function through the resolver and gets its scalar back");
        assert_eq!(called.values.as_slice(), &[PreparedResult::Scalar(7)]);
        assert_eq!(machine.disposition(), MachineDisposition::Reusable);
        assert!(machine.release(function));
        assert_eq!(machine.handle_count(), 0);
    }

    fn thunk_to_closure_identity() -> SymbolIdentity {
        testing::identity("ThunkToClosure", "makeClosure")
    }

    /// A for the forced-environment test: a memoized CAF whose body builds a
    /// `Ready` constructor `y` and returns a closure `f = \() -> y` that
    /// CAPTURES `y`. Imported unforced, so a caller's dispatcher must enter
    /// it before applying the function it evaluates to -- and must apply
    /// that function with its OWN environment, not the thunk's.
    fn thunk_to_closure_program() -> CompiledProgram {
        let mut wire = testing::wire_program();
        wire.signatures[0].results = ResultContract::Returns(vec![RuntimeRep::LiftedRef]);
        wire.constructors.push(ConstructorDecl {
            identity: testing::identity("ThunkToClosure", "Ready"),
            family: testing::identity("ThunkToClosure", "Ready"),
            host_id: tidepool_repr::DataConId(1_301),
            result_rep: RuntimeRep::LiftedRef,
            tag: 1,
            family_size: 1,
            field_reps: vec![],
            strict_fields: vec![],
            layout: CheckedLayout {
                fields: vec![],
                alignment: 1,
                payload_size: 0,
                root_mask: vec![],
            },
        });
        wire.expressions.nodes = vec![
            // node 0: f's body returns its captured `y`.
            ExprFrame::Return(vec![Atom::Ref(ValueRef::Local(ValueId(301)))]),
            // node 1: the let body returns `f`.
            ExprFrame::Return(vec![Atom::Ref(ValueRef::Local(ValueId(302)))]),
            // node 2: let f = \() -> y (captures y) in node 1.
            ExprFrame::Let {
                bindings: Group::NonRecursive(HeapBinding {
                    id: ValueId(302),
                    rhs: HeapRhs::Function {
                        signature: SignatureId(0),
                        parameters: vec![],
                        captures: vec![ValueRef::Local(ValueId(301))],
                        body: 0,
                    },
                }),
                body: 1,
            },
            // node 3: let y = Ready in node 2.
            ExprFrame::Let {
                bindings: Group::NonRecursive(HeapBinding {
                    id: ValueId(301),
                    rhs: HeapRhs::Constructor {
                        constructor: ConstructorId(0),
                        fields: vec![],
                    },
                }),
                body: 2,
            },
        ];
        let Group::NonRecursive(top) = &mut wire.bindings[0] else {
            unreachable!()
        };
        top.identity = thunk_to_closure_identity();
        top.binding.rhs = HeapRhs::Thunk {
            signature: SignatureId(0),
            update: UpdatePolicy::Memoize,
            captures: vec![],
            body: 3,
        };
        let prepared = testing::prepare(wire).expect("thunk-to-closure fixture");
        let linked = link_program(prepared, &MachineImports::default())
            .expect("thunk-to-closure fixture links");
        CompiledProgram::compile(&linked).expect("thunk-to-closure fixture compiles")
    }

    /// B for the forced-environment test: its entry calls the imported,
    /// still-unforced global DIRECTLY (`ValueRef::Global` callee) at
    /// `() -> LiftedRef`. B declares no function of that shape, so the call
    /// is served entirely by the dispatcher's enter-then-resolve fallback.
    fn direct_import_caller_program(
        identity: SymbolIdentity,
    ) -> tidepool_repr::execution_schema::LinkedProgram {
        let mut wire = testing::wire_program();
        wire.signatures[0] = Signature {
            arguments: vec![],
            results: ResultContract::Returns(vec![RuntimeRep::LiftedRef]),
        };
        wire.globals = vec![GlobalDecl {
            identity: identity.clone(),
            rep: RuntimeRep::LiftedRef,
            entry_signature: None,
            required_evaluated: false,
            required_generation: None,
        }];
        wire.expressions.nodes[0] = ExprFrame::Call {
            callee: Atom::Ref(ValueRef::Global(GlobalId(0))),
            signature: SignatureId(0),
            arguments: vec![],
        };
        let prepared = testing::prepare(wire).expect("direct import caller fixture");
        let mut imports = MachineImports::default();
        imports.values.insert(
            identity.clone(),
            ImportedValue {
                identity,
                rep: RuntimeRep::LiftedRef,
                entry_signature: None,
                evaluated: false,
                generation: 0,
            },
        );
        link_program(prepared, &imports).expect("direct import caller fixture links")
    }

    /// The foreign-resolution fallback applies the ENTERED callee. B calls
    /// A's unforced CAF directly; the dispatcher enters it (running A's body
    /// through `prepared_resolve_enter`), gets the closure `f`, resolves
    /// `f`'s code machine-wide, and must pass `f` -- not the original thunk --
    /// as the environment `f` reads its captured `y` from. Passing the
    /// thunk made `f` load its capture out of the thunk's own payload.
    #[test]
    fn foreign_resolution_applies_the_entered_closure_not_the_thunk() {
        let (mut machine, program_a) = PreparedMachine::new(
            thunk_to_closure_program(),
            PreparedMachineOptions {
                nursery_bytes: 4096,
            },
        )
        .expect("A installs");
        let thunk = machine
            .retain_top(program_a, ValueId(0))
            .expect("A's unforced CAF is retained without running it");
        let mut imports = ImportBindings::new();
        imports.insert(thunk_to_closure_identity(), thunk);
        let program_b = install_linked(
            &mut machine,
            &direct_import_caller_program(thunk_to_closure_identity()),
            imports,
        )
        .expect("B installs, importing A's unforced CAF");
        let call = PreparedCallOptions {
            observation_budget: RunOptions::default().observation_budget,
            collect_before_observation: true,
        };
        let called = machine
            .run_entry_retained(program_b, ValueId(0), &[], call, RealmId::ROOT)
            .expect("B's direct call enters A's CAF and applies the closure it evaluates to");
        let [PreparedResult::Managed(result)] = called.values.as_slice() else {
            panic!("the call returns one managed value");
        };
        let PreparedOuter::Constructor { identity, .. } = machine
            .inspect_outer(*result, RealmId::ROOT)
            .expect("the closure's captured constructor inspects");
        assert_eq!(
            identity,
            tidepool_repr::DataConId(1_301),
            "the closure returned its own captured Ready, read from its own environment"
        );
        assert_eq!(machine.disposition(), MachineDisposition::Reusable);
        assert!(machine.release(*result));
        assert!(machine.release(thunk));
        assert_eq!(machine.handle_count(), 0);
    }

    fn x2b_thunk_producer_identity() -> SymbolIdentity {
        testing::identity("X2ThunkForce", "producer")
    }

    /// Program A for X2b: a memoized CAF `producer :: () -> LiftedRef` whose
    /// body allocates 32 throwaway `Filler` constructors (forcing a
    /// collection under a tiny nursery) before constructing and returning a
    /// fresh `Ready` value. This top is retained via
    /// [`PreparedMachine::retain_top`] WITHOUT running it, so the handle B
    /// imports really is still an unforced thunk.
    fn x2b_thunk_producer_program() -> CompiledProgram {
        let mut wire = testing::wire_program();
        wire.signatures[0].results = ResultContract::Returns(vec![RuntimeRep::LiftedRef]);
        wire.constructors.push(ConstructorDecl {
            identity: testing::identity("X2ThunkForce", "Filler"),
            family: testing::identity("X2ThunkForce", "Filler"),
            host_id: tidepool_repr::DataConId(994),
            result_rep: RuntimeRep::LiftedRef,
            tag: 1,
            family_size: 1,
            field_reps: vec![],
            strict_fields: vec![],
            layout: CheckedLayout {
                fields: vec![],
                alignment: 1,
                payload_size: 0,
                root_mask: vec![],
            },
        });
        wire.constructors.push(ConstructorDecl {
            identity: testing::identity("X2ThunkForce", "Ready"),
            family: testing::identity("X2ThunkForce", "Ready"),
            host_id: tidepool_repr::DataConId(995),
            result_rep: RuntimeRep::LiftedRef,
            tag: 1,
            family_size: 1,
            field_reps: vec![],
            strict_fields: vec![],
            layout: CheckedLayout {
                fields: vec![],
                alignment: 1,
                payload_size: 0,
                root_mask: vec![],
            },
        });
        wire.expressions.nodes[0] = ExprFrame::Construct {
            constructor: ConstructorId(1),
            fields: vec![],
        };
        let mut body = 0;
        for id in 0..32 {
            wire.expressions.nodes.push(ExprFrame::Let {
                bindings: Group::NonRecursive(HeapBinding {
                    id: ValueId(200 + id),
                    rhs: HeapRhs::Constructor {
                        constructor: ConstructorId(0),
                        fields: vec![],
                    },
                }),
                body,
            });
            body = wire.expressions.nodes.len() - 1;
        }
        let Group::NonRecursive(top) = &mut wire.bindings[0] else {
            unreachable!()
        };
        top.identity = x2b_thunk_producer_identity();
        top.binding.rhs = HeapRhs::Thunk {
            signature: SignatureId(0),
            update: UpdatePolicy::Memoize,
            captures: vec![],
            body,
        };
        let prepared = testing::prepare(wire).expect("x2b thunk producer fixture");
        let linked = link_program(prepared, &MachineImports::default())
            .expect("x2b thunk producer fixture links");
        CompiledProgram::compile(&linked).expect("x2b thunk producer fixture compiles")
    }

    /// Program B for X2b: a zero-argument entry that does nothing but
    /// `Enter` the imported (still-unforced) global -- forcing it through
    /// this program's own generated `prepared_enter` state machine, which
    /// recognizes nothing of its own and falls back to
    /// `prepared_resolve_enter` (X2's cross-program enter resolution).
    fn x2b_enter_consumer_program(
        identity: SymbolIdentity,
    ) -> tidepool_repr::execution_schema::LinkedProgram {
        let mut wire = testing::wire_program();
        wire.signatures[0] = Signature {
            arguments: vec![],
            results: ResultContract::Returns(vec![RuntimeRep::LiftedRef]),
        };
        wire.signatures.push(Signature {
            arguments: vec![],
            results: ResultContract::Returns(vec![RuntimeRep::LiftedRef]),
        });
        wire.globals = vec![GlobalDecl {
            identity: identity.clone(),
            rep: RuntimeRep::LiftedRef,
            entry_signature: None,
            required_evaluated: false,
            required_generation: None,
        }];
        wire.expressions.nodes[0] = ExprFrame::Enter {
            callee: Atom::Ref(ValueRef::Global(GlobalId(0))),
            signature: SignatureId(1),
        };
        let Group::NonRecursive(top) = &mut wire.bindings[0] else {
            unreachable!()
        };
        top.binding.rhs = HeapRhs::Function {
            signature: SignatureId(0),
            parameters: vec![],
            captures: vec![],
            body: 0,
        };
        let prepared = testing::prepare(wire).expect("x2b enter consumer fixture");
        let mut imports = MachineImports::default();
        imports.values.insert(
            identity.clone(),
            ImportedValue {
                identity,
                rep: RuntimeRep::LiftedRef,
                entry_signature: None,
                evaluated: false,
                generation: 0,
            },
        );
        link_program(prepared, &imports).expect("x2b enter consumer fixture links")
    }

    /// X2b: forcing a foreign, still-UNEVALUATED thunk import through a
    /// generated `Enter` reaches the owning program's own entry code via
    /// `prepared_resolve_enter` (the enter-side half of X2, mirroring
    /// `prepared_resolve_call` on the call side). A's thunk is imported
    /// with `required_evaluated: false` (install never forces it -- the
    /// handle comes from `retain_top`, which reads the top-table slot
    /// without running anything), so B's `Enter` genuinely forces A's body,
    /// with B's frame live, while A allocates under a shared tiny nursery.
    /// A's own later entry then observes the SAME settled (memoized) cell.
    #[test]
    fn x2_b_forces_a_thunk_import_through_the_owning_programs_enter() {
        let (mut machine, program_a) = PreparedMachine::new(
            x2b_thunk_producer_program(),
            PreparedMachineOptions { nursery_bytes: 128 },
        )
        .expect("A installs");

        let handle_a = machine
            .retain_top(program_a, ValueId(0))
            .expect("A's own unforced thunk top can be retained without running anything");
        let mut imports = ImportBindings::new();
        imports.insert(x2b_thunk_producer_identity(), handle_a);
        let program_b = install_linked(
            &mut machine,
            &x2b_enter_consumer_program(x2b_thunk_producer_identity()),
            imports,
        )
        .expect(
            "B installs, importing A's still-unforced thunk as a non-evaluated-required global",
        );

        let call = PreparedCallOptions {
            observation_budget: RunOptions::default().observation_budget,
            collect_before_observation: false,
        };
        let called = machine
            .run_entry_retained(program_b, ValueId(0), &[], call, RealmId::ROOT)
            .expect(
                "B's generated Enter forces A's foreign thunk through prepared_enter's \
                 cross-program resolve fallback -- A's own code runs, with B's frame live, \
                 until the thunk settles",
            );
        assert!(
            called.collections >= 1,
            "A's 32 allocations, forced as B's entered callee, must force a collection under \
             the tiny shared nursery while B's calling frame is live"
        );
        let [PreparedResult::Managed(result)] = called.values.as_slice() else {
            panic!("the forced import's result must be retained as one managed value");
        };
        let PreparedOuter::Constructor {
            identity: result_identity,
            ..
        } = machine
            .inspect_outer(*result, RealmId::ROOT)
            .expect("the forced result inspects through the machine-wide descriptor union");
        assert_eq!(
            result_identity,
            tidepool_repr::DataConId(995),
            "B observes A's freshly constructed Ready value"
        );

        // A's own entry, run directly, now sees the SAME memoized value -- no
        // fresh allocation, no second forcing, just the settled cell B's
        // cross-program Enter updated in place.
        let after_a = machine
            .run_entry_retained(program_a, ValueId(0), &[], call, RealmId::ROOT)
            .expect("A's own entry reads its now-memoized thunk");
        let [PreparedResult::Managed(via_a)] = after_a.values.as_slice() else {
            panic!("A must return one managed constructor");
        };
        assert_eq!(
            machine.handle_current_pointer(*result),
            machine.handle_current_pointer(*via_a),
            "identity: A's own re-entry resolves to the exact object B's cross-program Enter \
             forced"
        );

        assert!(machine.release(handle_a));
        assert!(machine.release(*result));
        assert!(machine.release(*via_a));
        assert_eq!(machine.handle_count(), 0);
        assert_eq!(machine.disposition(), MachineDisposition::Reusable);
    }

    // ---- S3: global lowering, per-global admission, import bindings ------
    //
    // CORRECTION carried from the plan card: cross-program CLOSURE
    // invocation has no mechanism this wave (S2's T2 finding). Test (2)
    // below imports a closure, holds it live and never enters or calls it.
    //
    // A second, independent finding surfaced while building test (1):
    // `CaseKind::Algebraic` dispatch (`emit.rs::emit_case_dispatch`,
    // `emit_algebraic_dispatch`) matches a scrutinee's header word against
    // only the COMPILING program's own `plan.constructors` descriptor
    // addresses (`emit.rs` ~1699-1728, "Checked algebraic dispatch uses full
    // descriptor identity, not a family-relative low-bit tag") -- the exact
    // same per-program-baked-table shape as the apply dispatcher S2's T2
    // hit, just for `Case` instead of `Call`. A foreign program's
    // constructor object can never match a locally-declared descriptor's
    // address, so a genuine generated `Case` over an imported constructor
    // always falls through to the integrity trap, confirmed empirically
    // below (`s3_finding_generated_case_cannot_recognize_a_foreign_constructor`,
    // pinned `#[ignore]`d). Test (1) is therefore adapted to the same
    // achievable shape as test (2): B reads the import through
    // `PreparedMachine::inspect_outer` (the host-boundary, non-forcing path
    // through the SAME machine-wide descriptor union a real `Case` would
    // need), never through a generated `Case`. This is still real,
    // meaningful coverage: it proves the import slot, its independent
    // persistent root, and the shared descriptor/static union all resolve a
    // cross-program CONSTRUCTOR correctly across collections on both sides
    // -- the property the reviewer's root-registration mutation targets.

    fn s3_field_producer_identity() -> SymbolIdentity {
        testing::identity("S3Import", "producer")
    }

    /// A's producer for S3 tests (1) and (3): a memoized CAF returning
    /// `Field(99)`, one strict `Int(64)` field.
    fn s3_field_producer_program() -> CompiledProgram {
        let mut wire = testing::wire_program();
        wire.signatures[0].results = ResultContract::Returns(vec![RuntimeRep::LiftedRef]);
        wire.constructors.push(ConstructorDecl {
            identity: testing::identity("S3Import", "Field"),
            family: testing::identity("S3Import", "Field"),
            host_id: tidepool_repr::DataConId(960),
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
        top.identity = s3_field_producer_identity();
        top.binding.rhs = HeapRhs::Thunk {
            signature: SignatureId(0),
            update: UpdatePolicy::Memoize,
            captures: vec![],
            body: 0,
        };
        let prepared = testing::prepare(wire).expect("s3 field producer fixture");
        let linked = link_program(prepared, &MachineImports::default())
            .expect("s3 field producer fixture links");
        CompiledProgram::compile(&linked).expect("s3 field producer fixture compiles")
    }

    /// B: declares one global of `identity`/`rep`/`required_evaluated` and
    /// its entry does nothing but read and return it -- shared by S3 tests
    /// (1) and (3). Linking is checked against a `MachineImports` snapshot
    /// that mirrors the declaration exactly (identity/rep/evaluatedness
    /// agreement is `link_program`'s job, proven once here; the machine-level
    /// `install_program` re-verification under test is a separate, later
    /// check against the REAL live handle).
    fn s3_import_consumer_program(
        identity: SymbolIdentity,
        rep: RuntimeRep,
        required_evaluated: bool,
    ) -> tidepool_repr::execution_schema::LinkedProgram {
        let mut wire = testing::wire_program();
        wire.signatures[0] = Signature {
            arguments: vec![],
            results: ResultContract::Returns(vec![rep]),
        };
        wire.globals = vec![GlobalDecl {
            identity: identity.clone(),
            rep,
            entry_signature: None,
            required_evaluated,
            required_generation: None,
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
        let prepared = testing::prepare(wire).expect("s3 import consumer fixture");
        let mut imports = MachineImports::default();
        imports.values.insert(
            identity.clone(),
            ImportedValue {
                identity,
                rep,
                entry_signature: None,
                evaluated: required_evaluated,
                generation: 0,
            },
        );
        link_program(prepared, &imports).expect("s3 import consumer fixture links")
    }

    #[test]
    fn s3_test1_imported_constructor_field_reads_correctly_before_and_after_collections_both_sides()
    {
        let (mut machine, program_a) = PreparedMachine::new(
            s3_field_producer_program(),
            PreparedMachineOptions {
                nursery_bytes: RunOptions::default().nursery_bytes,
            },
        )
        .expect("A installs");
        let call = PreparedCallOptions {
            observation_budget: RunOptions::default().observation_budget,
            collect_before_observation: false,
        };
        let produced = machine
            .run_entry_retained(program_a, ValueId(0), &[], call, RealmId::ROOT)
            .expect("A produces its retained Field(99)");
        let [PreparedResult::Managed(handle_a)] = produced.values.as_slice() else {
            panic!("A must return one managed constructor");
        };
        let mut imports = ImportBindings::new();
        imports.insert(s3_field_producer_identity(), *handle_a);
        let program_b = install_linked(
            &mut machine,
            &s3_import_consumer_program(s3_field_producer_identity(), RuntimeRep::LiftedRef, true),
            imports,
        )
        .expect("B installs, importing A's Field as a required-evaluated global");

        let before = machine
            .run_entry_retained(program_b, ValueId(0), &[], call, RealmId::ROOT)
            .expect("B reads its import slot and returns the same handle");
        let [PreparedResult::Managed(handle_b_before)] = before.values.as_slice() else {
            panic!("B must retain the imported handle");
        };
        assert_eq!(
            machine.handle_current_pointer(*handle_a),
            machine.handle_current_pointer(*handle_b_before),
            "identity, not a copy: B's import slot resolves to A's own object"
        );
        assert!(
            machine.import_slot_is_registered_root(program_b, &s3_field_producer_identity()),
            "B's import slot must be registered as its own independent persistent root"
        );
        let PreparedOuter::Constructor {
            identity: field_identity,
            fields: field_fields,
        } = machine
            .inspect_outer(*handle_b_before, RealmId::ROOT)
            .expect("B's imported handle inspects through the machine-wide descriptor union");
        assert_eq!(field_identity, tidepool_repr::DataConId(960));
        let [PreparedResult::Scalar(field_value)] = field_fields.as_slice() else {
            panic!("Field must have exactly one scalar field");
        };
        assert_eq!(*field_value, 99);

        let before_pointer = machine
            .handle_current_pointer(*handle_b_before)
            .expect("B's handle is live");

        // Force real collections on both sides -- A's own entry (already
        // memoized, so this is a harness-driven collection, not one A's own
        // code triggers) and then B's -- and re-read the import through B's
        // OWN generated code again. `handle_a`/`handle_b_before` were minted
        // through `retain_prepared`, which promotes into old space; old
        // space here is compacted only on an explicit major pass this
        // machine never runs, so the object's address is expected to stay
        // put (an equality check, matching `t1_managed_argument_crosses_
        // installed_programs_with_identity_preserved`'s own established
        // pattern for a retained value) -- the mutation this proves against
        // is caught directly by `import_slot_is_registered_root` above and
        // again below, not by relocation.
        let collect = PreparedCallOptions {
            collect_before_observation: true,
            ..call
        };
        let after_a = machine
            .run_entry_retained(program_a, ValueId(0), &[], collect, RealmId::ROOT)
            .expect("A's own entry still runs and forces a collection");
        let [PreparedResult::Managed(handle_a_after)] = after_a.values.as_slice() else {
            panic!("A must still return one managed constructor");
        };
        let after = machine
            .run_entry_retained(program_b, ValueId(0), &[], collect, RealmId::ROOT)
            .expect("B's own entry still runs and forces a collection");
        let [PreparedResult::Managed(handle_b_after)] = after.values.as_slice() else {
            panic!("B must still retain the imported handle");
        };
        let after_pointer = machine
            .handle_current_pointer(*handle_b_after)
            .expect("B's post-collection handle is live");
        assert_eq!(
            before_pointer, after_pointer,
            "a retained value's own root slot is stable across a minor collection"
        );
        assert_eq!(
            machine.handle_current_pointer(*handle_a_after),
            Some(after_pointer),
            "B's import slot still resolves to A's own object after both collections"
        );
        assert!(
            machine.import_slot_is_registered_root(program_b, &s3_field_producer_identity()),
            "B's import slot root registration survives a forced collection on both sides"
        );
        let PreparedOuter::Constructor {
            identity: field_identity_after,
            fields: field_fields_after,
        } = machine
            .inspect_outer(*handle_b_after, RealmId::ROOT)
            .expect("B's re-read handle still classifies correctly after collection");
        assert_eq!(field_identity_after, tidepool_repr::DataConId(960));
        let [PreparedResult::Scalar(field_value_after)] = field_fields_after.as_slice() else {
            panic!("Field must still have exactly one scalar field");
        };
        assert_eq!(*field_value_after, 99);

        assert!(machine.release(*handle_a));
        assert!(machine.release(*handle_b_before));
        assert!(machine.release(*handle_a_after));
        assert!(machine.release(*handle_b_after));
        assert_eq!(machine.handle_count(), 0);
        assert_eq!(machine.disposition(), MachineDisposition::Reusable);
    }

    /// Pinned FINDING, not an acceptance test: a genuine generated `Case`
    /// dispatch (`CaseKind::Algebraic`) can never recognize a constructor
    /// produced by a DIFFERENT installed program, because
    /// `emit_algebraic_dispatch` matches the scrutinee's header word against
    /// the COMPILING program's own `plan.constructors` descriptor addresses
    /// only (`emit.rs`'s `emit_case_dispatch`/`emit_algebraic_dispatch`,
    /// doc comment at ~1674: "Checked algebraic dispatch uses full
    /// descriptor identity, not a family-relative low-bit tag"). B below
    /// declares its OWN, separately-allocated copy of the SAME logical
    /// `Field` constructor (same tag/family/layout) and cases on A's
    /// imported value; A's object's header holds A's descriptor's address,
    /// which never equals B's descriptor's address, so the dispatch always
    /// falls through to the integrity trap. This is why test (1) above
    /// reads the import through `inspect_outer` instead.
    /// X1: a generated `Case` in B over a constructor A built. B is compiled
    /// through `compile_for_install`, so its `Field` descriptor IS A's (one
    /// interned descriptor per constructor identity) and algebraic dispatch
    /// matches A's cell. Before interning this exact program trapped
    /// (`CaseTrap`, machine `Unavailable`) -- the S3 finding.
    #[test]
    fn x1_generated_case_reads_a_foreign_constructor_through_the_interned_descriptor() {
        let (mut machine, program_a) = PreparedMachine::new(
            s3_field_producer_program(),
            PreparedMachineOptions {
                nursery_bytes: RunOptions::default().nursery_bytes,
            },
        )
        .expect("A installs");
        let call = PreparedCallOptions {
            observation_budget: RunOptions::default().observation_budget,
            collect_before_observation: false,
        };
        let produced = machine
            .run_entry_retained(program_a, ValueId(0), &[], call, RealmId::ROOT)
            .expect("A produces its retained Field(99)");
        let [PreparedResult::Managed(handle_a)] = produced.values.as_slice() else {
            panic!("A must return one managed constructor");
        };
        let mut imports = ImportBindings::new();
        imports.insert(s3_field_producer_identity(), *handle_a);

        // B's own case-dispatching consumer: declares the SAME logical
        // `Field` constructor as A (same tag/family/layout, but a distinct
        // Arc<ObjectDescriptor> allocation) and cases on the import.
        let mut wire = testing::wire_program();
        wire.signatures[0] = Signature {
            arguments: vec![],
            results: ResultContract::Returns(vec![RuntimeRep::Int(64)]),
        };
        wire.globals = vec![GlobalDecl {
            identity: s3_field_producer_identity(),
            rep: RuntimeRep::LiftedRef,
            entry_signature: None,
            required_evaluated: true,
            required_generation: None,
        }];
        wire.constructors.push(ConstructorDecl {
            identity: testing::identity("S3Import", "Field"),
            family: testing::identity("S3Import", "Field"),
            host_id: tidepool_repr::DataConId(960),
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
        wire.expressions.nodes = vec![
            ExprFrame::Return(vec![Atom::Ref(ValueRef::Global(GlobalId(0)))]),
            ExprFrame::Return(vec![Atom::Ref(ValueRef::Local(ValueId(50)))]),
            ExprFrame::Case {
                scrutinee: 0,
                binder: ValueId(49),
                scrutinee_results: ResultContract::Returns(vec![RuntimeRep::LiftedRef]),
                kind: CaseKind::Algebraic(testing::identity("S3Import", "Field")),
                alternatives: vec![Alternative {
                    pattern: AlternativePattern::Constructor(ConstructorId(0)),
                    binders: vec![ValueId(50)],
                    body: 1,
                }],
            },
        ];
        let Group::NonRecursive(top) = &mut wire.bindings[0] else {
            unreachable!()
        };
        top.binding.rhs = HeapRhs::Function {
            signature: SignatureId(0),
            parameters: vec![],
            captures: vec![],
            body: 2,
        };
        let prepared = testing::prepare(wire).expect("case-dispatch consumer fixture");
        let mut machine_imports = MachineImports::default();
        machine_imports.values.insert(
            s3_field_producer_identity(),
            ImportedValue {
                identity: s3_field_producer_identity(),
                rep: RuntimeRep::LiftedRef,
                entry_signature: None,
                evaluated: true,
                generation: 0,
            },
        );
        let linked =
            link_program(prepared, &machine_imports).expect("case-dispatch consumer fixture links");
        let compiled = machine
            .compile_for_install(&linked)
            .expect("case-dispatch consumer fixture compiles against the machine's interner");
        let program_b = machine
            .install_program(compiled, imports)
            .expect("B installs, importing A's Field");

        let result = machine
            .run_entry(program_b, ValueId(0), &[], call, RealmId::ROOT)
            .expect("B's generated Case recognises A's Field cell through the shared descriptor");
        assert!(matches!(
            result.values.as_slice(),
            [tidepool_bridge::HaskellValue::Lit(
                tidepool_repr::Literal::LitInt(99)
            )]
        ));
        assert_eq!(machine.disposition(), MachineDisposition::Reusable);
        assert!(machine.release(*handle_a));
    }

    /// A case that has no alternative for an intact constructor fails its own
    /// call with a reusable `CaseMiss`; the machine keeps running programs.
    #[test]
    fn case_miss_on_an_intact_constructor_leaves_the_machine_reusable() {
        let (mut machine, program_a) = PreparedMachine::new(
            s3_field_producer_program(),
            PreparedMachineOptions {
                nursery_bytes: RunOptions::default().nursery_bytes,
            },
        )
        .expect("A installs");
        let call = PreparedCallOptions {
            observation_budget: RunOptions::default().observation_budget,
            collect_before_observation: false,
        };
        let produced = machine
            .run_entry_retained(program_a, ValueId(0), &[], call, RealmId::ROOT)
            .expect("A produces its retained Field(99)");
        let [PreparedResult::Managed(handle_a)] = produced.values.as_slice() else {
            panic!("A must return one managed constructor");
        };
        let mut imports = ImportBindings::new();
        imports.insert(s3_field_producer_identity(), *handle_a);

        let mut wire = testing::wire_program();
        wire.signatures[0] = Signature {
            arguments: vec![],
            results: ResultContract::Returns(vec![RuntimeRep::Int(64)]),
        };
        wire.globals = vec![GlobalDecl {
            identity: s3_field_producer_identity(),
            rep: RuntimeRep::LiftedRef,
            entry_signature: None,
            required_evaluated: true,
            required_generation: None,
        }];
        wire.expressions.nodes = vec![
            ExprFrame::Return(vec![Atom::Ref(ValueRef::Global(GlobalId(0)))]),
            ExprFrame::Case {
                scrutinee: 0,
                binder: ValueId(49),
                scrutinee_results: ResultContract::Returns(vec![RuntimeRep::LiftedRef]),
                kind: CaseKind::Algebraic(testing::identity("S3Import", "Field")),
                alternatives: vec![],
            },
        ];
        let Group::NonRecursive(top) = &mut wire.bindings[0] else {
            unreachable!()
        };
        top.binding.rhs = HeapRhs::Function {
            signature: SignatureId(0),
            parameters: vec![],
            captures: vec![],
            body: 1,
        };
        let prepared = testing::prepare(wire).expect("case-miss consumer fixture");
        let mut machine_imports = MachineImports::default();
        machine_imports.values.insert(
            s3_field_producer_identity(),
            ImportedValue {
                identity: s3_field_producer_identity(),
                rep: RuntimeRep::LiftedRef,
                entry_signature: None,
                evaluated: true,
                generation: 0,
            },
        );
        let linked =
            link_program(prepared, &machine_imports).expect("case-miss consumer fixture links");
        let compiled = machine
            .compile_for_install(&linked)
            .expect("case-miss consumer fixture compiles");
        let program_b = machine
            .install_program(compiled, imports)
            .expect("B installs, importing A's Field");

        let error = match machine.run_entry(program_b, ValueId(0), &[], call, RealmId::ROOT) {
            Err(error) => error,
            Ok(_) => panic!("a case with no alternative for Field cannot succeed"),
        };
        match error {
            ExecutionError::Runtime(failure) => {
                assert!(
                    matches!(
                        failure.cause,
                        RuntimeError::CaseMiss {
                            constructor: tidepool_repr::DataConId(960),
                            ..
                        }
                    ),
                    "{failure:?}"
                );
                assert_eq!(failure.disposition, MachineDisposition::Reusable);
            }
            other => panic!("expected a runtime case miss, got {other:?}"),
        }
        assert_eq!(machine.disposition(), MachineDisposition::Reusable);
        machine
            .run_entry_retained(program_a, ValueId(0), &[], call, RealmId::ROOT)
            .expect("the machine still runs programs after a case miss");
        assert!(machine.release(*handle_a));
    }

    /// X1: the same identity declared differently by a later program is a
    /// typed refusal at compile, not a silently aliased descriptor.
    #[test]
    fn x1_conflicting_constructor_declaration_is_a_typed_compile_error() {
        let (mut machine, _program_a) = PreparedMachine::new(
            s3_field_producer_program(),
            PreparedMachineOptions {
                nursery_bytes: RunOptions::default().nursery_bytes,
            },
        )
        .expect("A installs");
        // Same identity `S3Import.Field`, but declared with no fields.
        let mut wire = testing::wire_program();
        wire.signatures[0].results = ResultContract::Returns(vec![RuntimeRep::LiftedRef]);
        wire.constructors.push(ConstructorDecl {
            identity: testing::identity("S3Import", "Field"),
            family: testing::identity("S3Import", "Field"),
            host_id: tidepool_repr::DataConId(960),
            result_rep: RuntimeRep::LiftedRef,
            tag: 1,
            family_size: 1,
            field_reps: vec![],
            strict_fields: vec![],
            layout: CheckedLayout {
                fields: vec![],
                alignment: 1,
                payload_size: 0,
                root_mask: vec![],
            },
        });
        wire.expressions.nodes[0] = ExprFrame::Construct {
            constructor: ConstructorId(0),
            fields: vec![],
        };
        let Group::NonRecursive(top) = &mut wire.bindings[0] else {
            unreachable!()
        };
        top.binding.rhs = HeapRhs::Thunk {
            signature: SignatureId(0),
            update: UpdatePolicy::Memoize,
            captures: vec![],
            body: 0,
        };
        let prepared = testing::prepare(wire).expect("conflicting fixture");
        let linked =
            link_program(prepared, &MachineImports::default()).expect("conflicting fixture links");
        match machine.compile_for_install(&linked) {
            Err(super::super::CompileError::DescriptorShape { identity, .. })
                if *identity == testing::identity("S3Import", "Field") => {}
            Err(other) => panic!("expected DescriptorShape, got {other:?}"),
            Ok(_) => panic!("a differently-declared Field must not compile against A's interner"),
        }
        assert_eq!(machine.disposition(), MachineDisposition::Reusable);
    }

    /// Two compiles may be outstanding at once: each owns its root block, so
    /// both install, as distinct programs. (A compile that minted a
    /// descriptor another install has since minted differently is still
    /// refused, by the interner's identity check at install, never by a slot
    /// reservation.)
    #[test]
    fn two_outstanding_compiles_both_install_on_their_own_root_blocks() {
        let (mut machine, _program_a) = PreparedMachine::new(
            s3_field_producer_program(),
            PreparedMachineOptions {
                nursery_bytes: RunOptions::default().nursery_bytes,
            },
        )
        .expect("A installs");
        let linked = || {
            let mut wire = testing::wire_program();
            wire.signatures[0].results = ResultContract::Returns(vec![RuntimeRep::LiftedRef]);
            wire.constructors.push(ConstructorDecl {
                identity: testing::identity("StaleReservation", "Unit"),
                family: testing::identity("StaleReservation", "Unit"),
                host_id: tidepool_repr::DataConId(961),
                result_rep: RuntimeRep::LiftedRef,
                tag: 1,
                family_size: 1,
                field_reps: vec![],
                strict_fields: vec![],
                layout: CheckedLayout {
                    fields: vec![],
                    alignment: 1,
                    payload_size: 0,
                    root_mask: vec![],
                },
            });
            wire.expressions.nodes[0] = ExprFrame::Construct {
                constructor: ConstructorId(0),
                fields: vec![],
            };
            let Group::NonRecursive(top) = &mut wire.bindings[0] else {
                unreachable!()
            };
            top.binding.rhs = HeapRhs::Thunk {
                signature: SignatureId(0),
                update: UpdatePolicy::Memoize,
                captures: vec![],
                body: 0,
            };
            let prepared = testing::prepare(wire).expect("reservation fixture");
            link_program(prepared, &MachineImports::default()).expect("reservation fixture links")
        };
        let first = machine
            .compile_for_install(&linked())
            .expect("first outstanding compile");
        let second = machine
            .compile_for_install(&linked())
            .expect("second outstanding compile");
        let first = machine
            .install_program(first, ImportBindings::new())
            .expect("the first outstanding compile installs");
        let second = machine
            .install_program(second, ImportBindings::new())
            .expect("the second outstanding compile installs on its own root block");
        assert_ne!(first, second, "every install mints a fresh program id");
        assert_eq!(machine.disposition(), MachineDisposition::Reusable);
    }

    fn s3_closure_producer_identity() -> SymbolIdentity {
        testing::identity("S3ImportClosure", "producer")
    }

    /// A's producer for S3 test (2): a memoized CAF that returns `f`, a
    /// distinct zero-argument top-level closure, directly (no wrapping
    /// constructor). `f`'s own body is never reached by this test (S2's T2
    /// finding: no cross-program apply primitive exists this wave) so its
    /// exact contents do not matter; it stays trivial.
    fn s3_closure_producer_program() -> CompiledProgram {
        let mut wire = testing::wire_program();
        wire.signatures[0].results = ResultContract::Returns(vec![RuntimeRep::LiftedRef]);
        wire.signatures.push(Signature {
            arguments: vec![],
            results: ResultContract::Returns(vec![RuntimeRep::Int(64)]),
        });
        wire.expressions.nodes[0] = ExprFrame::Return(vec![Atom::Ref(ValueRef::Local(ValueId(1)))]);
        wire.expressions
            .nodes
            .push(ExprFrame::Return(vec![Atom::Scalar(ScalarLiteral::Int {
                bits: 64,
                bytes: 7_i64.to_be_bytes().to_vec(),
            })]));
        let Group::NonRecursive(top) = &mut wire.bindings[0] else {
            unreachable!()
        };
        top.identity = s3_closure_producer_identity();
        top.binding.rhs = HeapRhs::Thunk {
            signature: SignatureId(0),
            update: UpdatePolicy::Memoize,
            captures: vec![],
            body: 0,
        };
        wire.bindings.push(Group::NonRecursive(TopBinding {
            identity: testing::identity("S3ImportClosure", "f"),
            binding: HeapBinding {
                id: ValueId(1),
                rhs: HeapRhs::Function {
                    signature: SignatureId(1),
                    parameters: vec![],
                    captures: vec![],
                    body: 1,
                },
            },
        }));
        let prepared = testing::prepare(wire).expect("s3 closure producer fixture");
        let linked = link_program(prepared, &MachineImports::default())
            .expect("s3 closure producer fixture links");
        CompiledProgram::compile(&linked).expect("s3 closure producer fixture compiles")
    }

    /// B for S3 test (2): imports `identity` as a global it never enters or
    /// calls, allocating 32 throwaway constructors under a tiny nursery
    /// (forcing a real collection FROM WITHIN this same call, before the
    /// final read of the import slot) before reading and returning it.
    fn s3_closure_import_holder_program(
        identity: SymbolIdentity,
    ) -> tidepool_repr::execution_schema::LinkedProgram {
        let mut wire = testing::wire_program();
        wire.signatures[0] = Signature {
            arguments: vec![],
            results: ResultContract::Returns(vec![RuntimeRep::LiftedRef]),
        };
        wire.globals = vec![GlobalDecl {
            identity: identity.clone(),
            rep: RuntimeRep::LiftedRef,
            entry_signature: None,
            required_evaluated: false,
            required_generation: None,
        }];
        wire.constructors.push(ConstructorDecl {
            identity: testing::identity("S3ImportClosure", "Filler"),
            family: testing::identity("S3ImportClosure", "Filler"),
            host_id: tidepool_repr::DataConId(970),
            result_rep: RuntimeRep::LiftedRef,
            tag: 1,
            family_size: 1,
            field_reps: vec![],
            strict_fields: vec![],
            layout: CheckedLayout {
                fields: vec![],
                alignment: 1,
                payload_size: 0,
                root_mask: vec![],
            },
        });
        wire.expressions.nodes[0] =
            ExprFrame::Return(vec![Atom::Ref(ValueRef::Global(GlobalId(0)))]);
        let mut body = 0;
        for id in 0..32 {
            wire.expressions.nodes.push(ExprFrame::Let {
                bindings: Group::NonRecursive(HeapBinding {
                    id: ValueId(100 + id),
                    rhs: HeapRhs::Constructor {
                        constructor: ConstructorId(0),
                        fields: vec![],
                    },
                }),
                body,
            });
            body = wire.expressions.nodes.len() - 1;
        }
        let Group::NonRecursive(top) = &mut wire.bindings[0] else {
            unreachable!()
        };
        top.binding.rhs = HeapRhs::Function {
            signature: SignatureId(0),
            parameters: vec![],
            captures: vec![],
            body,
        };
        let prepared = testing::prepare(wire).expect("s3 closure import holder fixture");
        let mut imports = MachineImports::default();
        imports.values.insert(
            identity.clone(),
            ImportedValue {
                identity,
                rep: RuntimeRep::LiftedRef,
                entry_signature: None,
                evaluated: false,
                generation: 0,
            },
        );
        link_program(prepared, &imports).expect("s3 closure import holder fixture links")
    }

    /// S3 test (2): A produces a closure (function-shaped object, not data).
    /// B imports it as a global and holds it live -- reads/stores the
    /// handle -- but NEVER enters or calls it (S2's T2 finding: no
    /// cross-program apply primitive exists this wave). B allocates enough
    /// on its own side, under a tiny nursery, to force a collection FROM
    /// WITHIN its own generated code before it finishes reading the import;
    /// a further collection is forced on A's side too. The imported
    /// closure's slot still resolves to valid, correctly-relocated memory on
    /// both sides, and its descriptor/header still classifies as Callable
    /// (not Constructor) through the machine-wide descriptor union.
    #[test]
    fn s3_test2_imported_closure_held_live_never_entered_survives_collections_both_sides() {
        let (mut machine, program_a) = PreparedMachine::new(
            s3_closure_producer_program(),
            PreparedMachineOptions { nursery_bytes: 64 },
        )
        .expect("A installs");
        let call = PreparedCallOptions {
            observation_budget: RunOptions::default().observation_budget,
            collect_before_observation: false,
        };
        let produced = machine
            .run_entry_retained(program_a, ValueId(0), &[], call, RealmId::ROOT)
            .expect("A produces its retained closure f");
        let [PreparedResult::Managed(handle_f)] = produced.values.as_slice() else {
            panic!("A must return one managed closure");
        };
        let mut imports = ImportBindings::new();
        imports.insert(s3_closure_producer_identity(), *handle_f);
        let program_b = install_linked(
            &mut machine,
            &s3_closure_import_holder_program(s3_closure_producer_identity()),
            imports,
        )
        .expect("B installs, importing A's closure as a non-evaluated-required global");

        let before = machine
            .run_entry_retained(
                program_b,
                ValueId(0),
                &[],
                call,
                RealmId::ROOT,
            )
            .expect("B's own 32 allocations under a tiny nursery force a collection, then it reads the import");
        assert!(
            before.collections >= 1,
            "B's 32 throwaway allocations in a tiny nursery must force at least one collection \
             BEFORE B's own generated code finally reads the import slot"
        );
        let [PreparedResult::Managed(handle_f_via_b_before)] = before.values.as_slice() else {
            panic!("B must retain the imported handle");
        };
        assert_eq!(
            machine.handle_current_pointer(*handle_f),
            machine.handle_current_pointer(*handle_f_via_b_before),
            "identity, not a copy: B's import slot resolves to A's own closure object"
        );
        assert!(
            machine.import_slot_is_registered_root(program_b, &s3_closure_producer_identity()),
            "B's import slot must be registered as its own independent persistent root"
        );
        match machine.inspect_outer(*handle_f_via_b_before, RealmId::ROOT) {
            Err(ExecutionError::Observation(super::super::ObservationFailure::Unobservable(kind))) => {
                assert_eq!(
                    kind,
                    tidepool_heap::execution_descriptor::ObjectKind::Function,
                    "the imported value must still classify as a callable Function, never entered"
                );
            }
            other => panic!(
                "inspecting a callable-shaped import as a constructor must be the typed \
                 Unobservable(Function) refusal, never a value and never a different failure: {other:?}"
            ),
        }

        let before_pointer = machine
            .handle_current_pointer(*handle_f_via_b_before)
            .expect("B's handle is live");

        let collect = PreparedCallOptions {
            collect_before_observation: true,
            ..call
        };
        let after_a = machine
            .run_entry_retained(program_a, ValueId(0), &[], collect, RealmId::ROOT)
            .expect("A's own entry still runs and forces a further collection");
        let [PreparedResult::Managed(handle_f_after)] = after_a.values.as_slice() else {
            panic!("A must still return one managed closure");
        };
        let after = machine
            .run_entry_retained(program_b, ValueId(0), &[], collect, RealmId::ROOT)
            .expect("B's own entry still runs, allocates, and forces another collection");
        assert!(after.collections >= 1);
        let [PreparedResult::Managed(handle_f_via_b_after)] = after.values.as_slice() else {
            panic!("B must still retain the imported handle");
        };
        let after_pointer = machine
            .handle_current_pointer(*handle_f_via_b_after)
            .expect("B's post-collection handle is live");
        assert_eq!(
            before_pointer, after_pointer,
            "a retained value's own root slot is stable across a minor collection"
        );
        assert_eq!(
            machine.handle_current_pointer(*handle_f_after),
            Some(after_pointer),
            "B's import slot still resolves to A's own closure after both collections"
        );
        assert!(
            machine.import_slot_is_registered_root(program_b, &s3_closure_producer_identity()),
            "B's import slot root registration survives a forced collection on both sides"
        );
        match machine.inspect_outer(*handle_f_via_b_after, RealmId::ROOT) {
            Err(ExecutionError::Observation(super::super::ObservationFailure::Unobservable(kind))) => {
                assert_eq!(kind, tidepool_heap::execution_descriptor::ObjectKind::Function);
            }
            other => panic!(
                "the re-read handle must still classify as Callable after collection, not: {other:?}"
            ),
        }

        assert!(machine.release(*handle_f));
        assert!(machine.release(*handle_f_via_b_before));
        assert!(machine.release(*handle_f_after));
        assert!(machine.release(*handle_f_via_b_after));
        assert_eq!(machine.handle_count(), 0);
        assert_eq!(machine.disposition(), MachineDisposition::Reusable);
    }

    /// A function-typed import satisfies `required_evaluated`. The projection
    /// declares a re-entrant function global as evaluated
    /// (`ExecutionProjection.hs`, `importedEntry`: `LFReEntrant` -> `True`),
    /// so a retained function binding imported by a later program must
    /// install; only an unforced thunk fails the check (test 3 below).
    #[test]
    fn s3_test2b_imported_function_satisfies_required_evaluated() {
        let (mut machine, program_a) = PreparedMachine::new(
            s3_closure_producer_program(),
            PreparedMachineOptions {
                nursery_bytes: RunOptions::default().nursery_bytes,
            },
        )
        .expect("A installs");
        let call = PreparedCallOptions {
            observation_budget: RunOptions::default().observation_budget,
            collect_before_observation: false,
        };
        let produced = machine
            .run_entry_retained(program_a, ValueId(0), &[], call, RealmId::ROOT)
            .expect("A produces its retained closure f");
        let [PreparedResult::Managed(handle_f)] = produced.values.as_slice() else {
            panic!("A must return one managed closure");
        };
        let mut imports = ImportBindings::new();
        imports.insert(s3_closure_producer_identity(), *handle_f);
        let program_b = install_linked(
            &mut machine,
            &s3_import_consumer_program(
                s3_closure_producer_identity(),
                RuntimeRep::LiftedRef,
                true,
            ),
            imports,
        )
        .expect("a function object is in WHNF and satisfies required_evaluated");
        let read = machine
            .run_entry_retained(program_b, ValueId(0), &[], call, RealmId::ROOT)
            .expect("B reads its function import");
        let [PreparedResult::Managed(handle_f_via_b)] = read.values.as_slice() else {
            panic!("B must retain the imported function handle");
        };
        assert_eq!(
            machine.handle_current_pointer(*handle_f),
            machine.handle_current_pointer(*handle_f_via_b),
            "identity: B's import slot resolves to A's own function object"
        );
        assert!(machine.release(*handle_f));
        assert!(machine.release(*handle_f_via_b));
        assert_eq!(machine.handle_count(), 0);
        assert_eq!(machine.disposition(), MachineDisposition::Reusable);
    }

    /// `observe_handle` (the forcing bridge behind the session's `observe`
    /// call, and the request-payload read `park_suspension` runs while
    /// parking a suspension) does NOT refuse a bare function object the way
    /// `inspect_outer`/`inspect_constructor` do -- it bridges it as the
    /// reserved closure sentinel, matching Core's own tolerant bridge
    /// (`tidepool-codegen/CLAUDE.md` "HaskellValue handles and scope closure"). This
    /// is the settled-payload shape the self-harness's outer effect requests
    /// hit: `withHandler`/green-thread/`after` payloads carry a closure, and
    /// parking must observe successfully rather than erroring
    /// "cannot observe Function without forcing or applying it".
    #[test]
    fn observe_handle_bridges_a_bare_function_as_the_closure_sentinel() {
        let (mut machine, program_a) = PreparedMachine::new(
            s3_closure_producer_program(),
            PreparedMachineOptions {
                nursery_bytes: RunOptions::default().nursery_bytes,
            },
        )
        .expect("A installs");
        let call = PreparedCallOptions {
            observation_budget: RunOptions::default().observation_budget,
            collect_before_observation: false,
        };
        let produced = machine
            .run_entry_retained(program_a, ValueId(0), &[], call, RealmId::ROOT)
            .expect("A produces its retained closure f");
        let [PreparedResult::Managed(handle_f)] = produced.values.as_slice() else {
            panic!("A must return one managed closure");
        };
        // Confirm the object really is a bare, un-wrapped callable: peeling
        // it as a constructor layer still gets the typed refusal (unchanged
        // behavior for `inspect_outer`/`inspect_constructor`).
        assert!(matches!(
            machine.inspect_outer(*handle_f, RealmId::ROOT),
            Err(ExecutionError::Observation(
                super::super::ObservationFailure::Unobservable(
                    tidepool_heap::execution_descriptor::ObjectKind::Function
                )
            ))
        ));
        let observed = machine
            .observe_handle(
                program_a,
                *handle_f,
                RunOptions::default().observation_budget,
            )
            .expect("a bare function observes to the closure sentinel, not an error");
        assert!(matches!(
            observed,
            HaskellValue::Con(id, ref fields)
                if id == crate::observation::CLOSURE_SENTINEL && fields.is_empty()
        ));
        assert!(machine.release(*handle_f));
        assert_eq!(machine.handle_count(), 0);
        assert_eq!(machine.disposition(), MachineDisposition::Reusable);
    }

    // ---- S3 test (3): rep mismatch, evaluatedness mismatch, unknown handle:
    // typed errors, machine Reusable, no slot claimed. ---------------------

    #[test]
    fn s3_test3_rep_mismatch_is_a_typed_import_shape_error_no_slot_claimed() {
        let (mut machine, program_a) = PreparedMachine::new(
            s3_field_producer_program(),
            PreparedMachineOptions {
                nursery_bytes: RunOptions::default().nursery_bytes,
            },
        )
        .expect("A installs");
        let call = PreparedCallOptions {
            observation_budget: RunOptions::default().observation_budget,
            collect_before_observation: false,
        };
        let produced = machine
            .run_entry_retained(program_a, ValueId(0), &[], call, RealmId::ROOT)
            .expect("A produces its retained Field(99)");
        let [PreparedResult::Managed(handle_a)] = produced.values.as_slice() else {
            panic!("A must return one managed constructor");
        };
        let mut imports = ImportBindings::new();
        imports.insert(s3_field_producer_identity(), *handle_a);
        let error = install_linked(
            &mut machine,
            &s3_import_consumer_program(
                s3_field_producer_identity(),
                RuntimeRep::UnliftedRef,
                false,
            ),
            imports,
        )
        .expect_err("a declared UnliftedRef import must not accept a LiftedRef handle");
        assert!(
            matches!(
                error,
                ExecutionError::ImportShape {
                    expected: ImportShapeFact::Representation(RuntimeRep::UnliftedRef),
                    found: ImportShapeFact::Representation(RuntimeRep::LiftedRef),
                    ..
                }
            ),
            "expected a typed rep-mismatch ImportShape error, got {error:?}"
        );
        assert_eq!(machine.disposition(), MachineDisposition::Reusable);

        let result = machine
            .run_entry(program_a, ValueId(0), &[], call, RealmId::ROOT)
            .expect("A still runs correctly after the rejected install");
        assert!(matches!(
            result.values.as_slice(),
            [tidepool_bridge::HaskellValue::Con(id, fields)]
                if *id == tidepool_repr::DataConId(960)
                    && matches!(
                        fields.as_slice(),
                        [tidepool_bridge::HaskellValue::Lit(tidepool_repr::Literal::LitInt(99))]
                    )
        ));
        assert!(machine.release(*handle_a));
    }

    #[test]
    fn s3_test3_evaluatedness_mismatch_is_a_typed_import_shape_error_no_slot_claimed() {
        let (mut machine, program_a) = PreparedMachine::new(
            outer_with_function_field_program(),
            PreparedMachineOptions {
                nursery_bytes: RunOptions::default().nursery_bytes,
            },
        )
        .expect("A installs");
        let call = PreparedCallOptions {
            observation_budget: RunOptions::default().observation_budget,
            collect_before_observation: false,
        };
        let produced = machine
            .run_entry_retained(program_a, ValueId(0), &[], call, RealmId::ROOT)
            .expect("A produces its Envelope(f, unforced)");
        let [PreparedResult::Managed(outer)] = produced.values.as_slice() else {
            panic!("A's producer must return one managed Envelope");
        };
        let PreparedOuter::Constructor { fields, .. } = machine
            .inspect_outer(*outer, RealmId::ROOT)
            .expect("A's outer inspects without forcing its unforced field");
        let [PreparedResult::Managed(f), PreparedResult::Managed(unforced)] = fields.as_slice()
        else {
            panic!("Envelope's fields must both remain retained, opaque managed handles");
        };
        let identity = testing::identity("S3ImportMismatch", "unforced");
        let mut imports = ImportBindings::new();
        imports.insert(identity.clone(), *unforced);
        let error = install_linked(
            &mut machine,
            &s3_import_consumer_program(identity, RuntimeRep::LiftedRef, true),
            imports,
        )
        .expect_err("an unforced, never-entered thunk must not satisfy required_evaluated");
        assert!(
            matches!(
                error,
                ExecutionError::ImportShape {
                    expected: ImportShapeFact::Evaluated(true),
                    found: ImportShapeFact::Evaluated(false),
                    ..
                }
            ),
            "expected a typed evaluatedness-mismatch ImportShape error, got {error:?}"
        );
        assert_eq!(machine.disposition(), MachineDisposition::Reusable);

        assert!(machine.release(*outer));
        assert!(machine.release(*f));
        assert!(machine.release(*unforced));
    }

    #[test]
    fn s3_test3_unknown_handle_is_typed_no_slot_claimed() {
        let (mut machine, _program_a) = PreparedMachine::new(
            s3_field_producer_program(),
            PreparedMachineOptions {
                nursery_bytes: RunOptions::default().nursery_bytes,
            },
        )
        .expect("A installs");
        let error = install_linked(
            &mut machine,
            &s3_import_consumer_program(s3_field_producer_identity(), RuntimeRep::LiftedRef, false),
            ImportBindings::new(),
        )
        .expect_err("a declared import with no supplied handle must be refused");
        assert!(
            matches!(error, ExecutionError::UnknownPreparedHandle),
            "expected the typed UnknownPreparedHandle error, got {error:?}"
        );
        assert_eq!(machine.disposition(), MachineDisposition::Reusable);
    }

    // ---- S3b: import-holding heap tops; the default-only Case shortcut ---

    /// B for S3b test (1): a top-level `HeapRhs::Constructor` binding
    /// `Pair(import, 7)` -- `image.rs::heap_top_partition` classifies this
    /// as a heap top purely because its first field is
    /// `Atom::Ref(ValueRef::Global(..))`, so it is materialized fresh at
    /// install time (never through generated code) and its import field must
    /// already resolve when `initialize_heap_tops` runs. The entry (this
    /// same top) simply returns it.
    fn s3b_pair_import_holder_program(
        identity: SymbolIdentity,
    ) -> tidepool_repr::execution_schema::LinkedProgram {
        let mut wire = testing::wire_program();
        wire.globals = vec![GlobalDecl {
            identity: identity.clone(),
            rep: RuntimeRep::LiftedRef,
            entry_signature: None,
            required_evaluated: true,
            required_generation: None,
        }];
        wire.constructors.push(ConstructorDecl {
            identity: testing::identity("S3bImportTop", "Pair"),
            family: testing::identity("S3bImportTop", "Pair"),
            host_id: tidepool_repr::DataConId(965),
            result_rep: RuntimeRep::LiftedRef,
            tag: 1,
            family_size: 1,
            field_reps: vec![RuntimeRep::LiftedRef, RuntimeRep::Int(64)],
            strict_fields: vec![true, true],
            layout: CheckedLayout {
                fields: vec![
                    FieldLayout {
                        rep: RuntimeRep::LiftedRef,
                        offset: 0,
                    },
                    FieldLayout {
                        rep: RuntimeRep::Int(64),
                        offset: 8,
                    },
                ],
                alignment: 8,
                payload_size: 16,
                root_mask: vec![true, false],
            },
        });
        let Group::NonRecursive(top) = &mut wire.bindings[0] else {
            unreachable!()
        };
        top.binding.rhs = HeapRhs::Constructor {
            constructor: ConstructorId(0),
            fields: vec![
                Atom::Ref(ValueRef::Global(GlobalId(0))),
                Atom::Scalar(ScalarLiteral::Int {
                    bits: 64,
                    bytes: 7_i64.to_be_bytes().to_vec(),
                }),
            ],
        };
        // A bare Constructor top has no body index into the expression
        // arena (`wire_program()`'s default node was the now-replaced
        // Function rhs's body); leaving it in place fails validation with
        // an unreachable-node error (see
        // `execution_schema::validation::top_metadata_errors_precede_earlier_rhs_errors`
        // for the same pattern).
        wire.expressions.nodes.clear();
        let prepared = testing::prepare(wire).expect("s3b pair import holder fixture");
        let mut imports = MachineImports::default();
        imports.values.insert(
            identity.clone(),
            ImportedValue {
                identity,
                rep: RuntimeRep::LiftedRef,
                entry_signature: None,
                evaluated: true,
                generation: 0,
            },
        );
        link_program(prepared, &imports).expect("s3b pair import holder fixture links")
    }

    /// S3b test (1): a top-level constructor binding -- not a `Thunk` or
    /// `Function`, a bare heap top materialized only at install time -- whose
    /// own field IS the cross-program import. Proves `install`'s ordering
    /// contract: `publish_imports` runs before `initialize_heap_tops` reads
    /// the import slot to resolve this field (`run.rs::write_atoms`'s
    /// `Global` arm), on both the first-program and later-program install
    /// branches. Correct field resolution survives forced collections on
    /// both sides.
    #[test]
    fn s3b_import_holding_top_is_a_heap_top_published_after_import_slots() {
        let (mut machine, program_a) = PreparedMachine::new(
            s3_field_producer_program(),
            PreparedMachineOptions {
                nursery_bytes: RunOptions::default().nursery_bytes,
            },
        )
        .expect("A installs");
        let call = PreparedCallOptions {
            observation_budget: RunOptions::default().observation_budget,
            collect_before_observation: false,
        };
        let produced = machine
            .run_entry_retained(program_a, ValueId(0), &[], call, RealmId::ROOT)
            .expect("A produces its retained Field(99)");
        let [PreparedResult::Managed(handle_a)] = produced.values.as_slice() else {
            panic!("A must return one managed constructor");
        };
        let mut imports = ImportBindings::new();
        imports.insert(s3_field_producer_identity(), *handle_a);
        let program_b = install_linked(
            &mut machine,
            &s3b_pair_import_holder_program(s3_field_producer_identity()),
            imports,
        )
        .expect(
            "B installs: its import-holding top-level Pair is a heap top resolved at \
             install time, after the import slot is already published",
        );

        let pair = machine
            .run_entry_retained(program_b, ValueId(0), &[], call, RealmId::ROOT)
            .expect("B's entry returns its pre-materialized Pair top");
        let [PreparedResult::Managed(handle_pair)] = pair.values.as_slice() else {
            panic!("B must return one managed constructor");
        };
        let PreparedOuter::Constructor {
            identity: pair_identity,
            fields: pair_fields,
        } = machine
            .inspect_outer(*handle_pair, RealmId::ROOT)
            .expect("B's Pair inspects through the machine-wide descriptor union");
        assert_eq!(pair_identity, tidepool_repr::DataConId(965));
        let [PreparedResult::Managed(handle_field), PreparedResult::Scalar(local_field)] =
            pair_fields.as_slice()
        else {
            panic!("Pair must have one managed field and one scalar field");
        };
        assert_eq!(*local_field, 7);
        assert_eq!(
            machine.handle_current_pointer(*handle_a),
            machine.handle_current_pointer(*handle_field),
            "identity: Pair's own field resolves to A's own object, not a copy"
        );

        // Force real collections on both sides and re-read: the Pair top's
        // import field must still resolve to A's (possibly relocated)
        // object.
        let collect = PreparedCallOptions {
            collect_before_observation: true,
            ..call
        };
        let after_a = machine
            .run_entry_retained(program_a, ValueId(0), &[], collect, RealmId::ROOT)
            .expect("A's own entry still runs and forces a collection");
        let [PreparedResult::Managed(handle_a_after)] = after_a.values.as_slice() else {
            panic!("A must still return one managed constructor");
        };
        let pair_after = machine
            .run_entry_retained(program_b, ValueId(0), &[], collect, RealmId::ROOT)
            .expect("B's entry still returns its Pair top and forces a collection");
        let [PreparedResult::Managed(handle_pair_after)] = pair_after.values.as_slice() else {
            panic!("B must still return one managed constructor");
        };
        let PreparedOuter::Constructor {
            fields: pair_fields_after,
            ..
        } = machine
            .inspect_outer(*handle_pair_after, RealmId::ROOT)
            .expect("B's re-read Pair still inspects correctly after collection");
        let [PreparedResult::Managed(handle_field_after), PreparedResult::Scalar(local_field_after)] =
            pair_fields_after.as_slice()
        else {
            panic!("Pair must still have one managed field and one scalar field");
        };
        assert_eq!(*local_field_after, 7);
        assert_eq!(
            machine.handle_current_pointer(*handle_a_after),
            machine.handle_current_pointer(*handle_field_after),
            "Pair's field still resolves to A's own (possibly relocated) object after \
             collections on both sides"
        );

        assert!(machine.release(*handle_a));
        assert!(machine.release(*handle_field));
        assert!(machine.release(*handle_pair));
        assert!(machine.release(*handle_a_after));
        assert!(machine.release(*handle_field_after));
        assert!(machine.release(*handle_pair_after));
        assert_eq!(machine.handle_count(), 0);
        assert_eq!(machine.disposition(), MachineDisposition::Reusable);
    }

    /// B for S3b test (2): compiled STANDALONE (`CompiledProgram::compile`,
    /// never `compile_for_install`) -- it shares no constructor descriptor
    /// with A at all, only the import. Its entry does a default-only
    /// algebraic `Case` on the imported, required-evaluated constructor and
    /// returns a LOCAL scalar from the default branch.
    fn s3b_default_only_case_consumer_program(
        identity: SymbolIdentity,
    ) -> tidepool_repr::execution_schema::LinkedProgram {
        let mut wire = testing::wire_program();
        wire.signatures[0] = Signature {
            arguments: vec![],
            results: ResultContract::Returns(vec![RuntimeRep::Int(64)]),
        };
        wire.globals = vec![GlobalDecl {
            identity: identity.clone(),
            rep: RuntimeRep::LiftedRef,
            entry_signature: None,
            required_evaluated: true,
            required_generation: None,
        }];
        wire.expressions.nodes = vec![
            ExprFrame::Return(vec![Atom::Ref(ValueRef::Global(GlobalId(0)))]),
            ExprFrame::Return(vec![Atom::Scalar(ScalarLiteral::Int {
                bits: 64,
                bytes: 123_i64.to_be_bytes().to_vec(),
            })]),
            ExprFrame::Case {
                scrutinee: 0,
                binder: ValueId(49),
                scrutinee_results: ResultContract::Returns(vec![RuntimeRep::LiftedRef]),
                kind: CaseKind::Algebraic(testing::identity("S3ImportStandalone", "Field")),
                alternatives: vec![Alternative {
                    pattern: AlternativePattern::Default,
                    binders: vec![],
                    body: 1,
                }],
            },
        ];
        let Group::NonRecursive(top) = &mut wire.bindings[0] else {
            unreachable!()
        };
        top.binding.rhs = HeapRhs::Function {
            signature: SignatureId(0),
            parameters: vec![],
            captures: vec![],
            body: 2,
        };
        let prepared = testing::prepare(wire).expect("s3b default-only case consumer fixture");
        let mut imports = MachineImports::default();
        imports.values.insert(
            identity.clone(),
            ImportedValue {
                identity,
                rep: RuntimeRep::LiftedRef,
                entry_signature: None,
                evaluated: true,
                generation: 0,
            },
        );
        link_program(prepared, &imports).expect("s3b default-only case consumer fixture links")
    }

    /// S3b test (2): `emit_algebraic_dispatch`'s default-only shortcut (no
    /// named alternatives means there is nothing for a header comparison to
    /// rule out) lets a genuinely generated `Case` dispatch on a foreign
    /// import even when the compiling program shares NO interned descriptor
    /// with the producer -- B here is compiled standalone, unlike
    /// [`x1_generated_case_reads_a_foreign_constructor_through_the_interned_descriptor`],
    /// which needs `compile_for_install` because it names a real
    /// alternative.
    #[test]
    fn s3b_default_only_case_on_an_import_skips_dispatch() {
        let (mut machine, program_a) = PreparedMachine::new(
            s3_field_producer_program(),
            PreparedMachineOptions {
                nursery_bytes: RunOptions::default().nursery_bytes,
            },
        )
        .expect("A installs");
        let call = PreparedCallOptions {
            observation_budget: RunOptions::default().observation_budget,
            collect_before_observation: false,
        };
        let produced = machine
            .run_entry_retained(program_a, ValueId(0), &[], call, RealmId::ROOT)
            .expect("A produces its retained Field(99)");
        let [PreparedResult::Managed(handle_a)] = produced.values.as_slice() else {
            panic!("A must return one managed constructor");
        };
        let mut imports = ImportBindings::new();
        imports.insert(s3_field_producer_identity(), *handle_a);
        let program_b = install_linked(
            &mut machine,
            &s3b_default_only_case_consumer_program(s3_field_producer_identity()),
            imports,
        )
        .expect(
            "B installs standalone -- it shares no constructor descriptor with A, only \
             the default-only case shortcut lets its generated Case dispatch on the import",
        );

        let result = machine
            .run_entry(program_b, ValueId(0), &[], call, RealmId::ROOT)
            .expect(
                "B's default-only algebraic Case never compares the scrutinee's header \
                 against B's own (empty) descriptor family, so it reaches the default branch \
                 without a foreign-constructor trap",
            );
        assert!(matches!(
            result.values.as_slice(),
            [tidepool_bridge::HaskellValue::Lit(
                tidepool_repr::Literal::LitInt(123)
            )]
        ));
        assert_eq!(machine.disposition(), MachineDisposition::Reusable);
        assert!(machine.release(*handle_a));
        assert_eq!(machine.handle_count(), 0);
    }

    // ---- S2b: GC residuals -------------------------------------------
    //
    // T1's managed-argument crossing (`t1_managed_argument_crosses_...`)
    // forces collections while B is live, but the value that must survive
    // (the argument) is threaded through `argument_area`'s explicit
    // `register_rust_root` calls -- a root source independent of native
    // frame walking entirely. So it proves the collector correctly moves
    // and updates a *registered root*, but not that the stack-map CHAIN
    // (`MachineState::stack_map_registries`, `gc/frame_walker.rs`) is
    // consulted correctly for a SECOND installed program's own live
    // native frames: a mutation truncating the chain to only the
    // first-installed program's registry passes every existing test
    // (confirmed empirically). `s2b_second_program_native_frame` below closes that gap: a
    // cons-list built entirely by nested `Let`s within ONE native call,
    // where each cell is a bare Cranelift-tracked local (never wrapped in
    // an explicit root) that must stay live and correctly relocatable
    // across the NEXT cell's allocation -- exactly the case a
    // stack-map-chain bug corrupts.

    /// A `List` family: `Nil` (tag 1, no fields) and `Cons { tail: LiftedRef }`
    /// (tag 2, one field). `field_reps`/`layout` deliberately carry only the
    /// recursive link -- no `head` -- so the fixture stays minimal while
    /// still giving every non-base cell one managed field a collector must
    /// trace and relocate.
    fn push_list_constructors(
        wire: &mut tidepool_repr::execution_schema::WireProgram,
        family: &str,
    ) {
        wire.constructors.push(ConstructorDecl {
            identity: testing::identity(family, "Nil"),
            family: testing::identity(family, "List"),
            host_id: tidepool_repr::DataConId(0), // overwritten per call site below
            result_rep: RuntimeRep::LiftedRef,
            tag: 1,
            family_size: 2,
            field_reps: vec![],
            strict_fields: vec![],
            layout: CheckedLayout {
                fields: vec![],
                alignment: 1,
                payload_size: 0,
                root_mask: vec![],
            },
        });
        wire.constructors.push(ConstructorDecl {
            identity: testing::identity(family, "Cons"),
            family: testing::identity(family, "List"),
            host_id: tidepool_repr::DataConId(0), // overwritten per call site below
            result_rep: RuntimeRep::LiftedRef,
            tag: 2,
            family_size: 2,
            field_reps: vec![RuntimeRep::LiftedRef],
            strict_fields: vec![false],
            layout: CheckedLayout {
                fields: vec![FieldLayout {
                    rep: RuntimeRep::LiftedRef,
                    offset: 0,
                }],
                alignment: 8,
                payload_size: 8,
                root_mask: vec![true],
            },
        });
    }

    /// B's own entry: build a `length`-long `Cons` chain entirely within
    /// one native call via nested `Let`s (no Rust-registered root touches
    /// any cell but the function's own final return value), then return
    /// it. Each `Cons(tail)` cell's `tail` field is a bare Cranelift local
    /// live across the NEXT cell's own allocation -- the property a
    /// truncated stack-map chain corrupts. `family` distinguishes this
    /// program's constructor identities from a second installed program's
    /// (two programs may not declare one identity differently).
    fn stack_map_chain_list_program(
        family: &str,
        nil_host_id: u64,
        cons_host_id: u64,
        length: u32,
    ) -> tidepool_repr::execution_schema::LinkedProgram {
        let mut wire = testing::wire_program();
        wire.signatures[0] = Signature {
            arguments: vec![],
            results: ResultContract::Returns(vec![RuntimeRep::LiftedRef]),
        };
        push_list_constructors(&mut wire, family);
        wire.constructors[0].host_id = tidepool_repr::DataConId(nil_host_id);
        wire.constructors[1].host_id = tidepool_repr::DataConId(cons_host_id);

        let base_local = ValueId(200);
        wire.expressions.nodes[0] = ExprFrame::Return(vec![Atom::Ref(ValueRef::Local(base_local))]);
        let mut body = 0;
        // Loop-iteration i is allocated LAST-to-FIRST as i goes 0..length
        // (each new Let wraps the previous as its own body, and a Let
        // allocates its own binding before entering its body -- see the
        // module's other Let-chain fixtures). So i == length - 1 (the
        // LAST pushed, OUTERMOST Let) is allocated FIRST and is the base
        // case (Nil); every other i is Cons(Local(id for i + 1)), the
        // cell allocated immediately before it.
        for i in 0..length {
            let id = ValueId(200 + i);
            let (constructor, fields) = if i == length - 1 {
                (ConstructorId(0), vec![])
            } else {
                let next_id = ValueId(200 + i + 1);
                (ConstructorId(1), vec![Atom::Ref(ValueRef::Local(next_id))])
            };
            wire.expressions.nodes.push(ExprFrame::Let {
                bindings: Group::NonRecursive(HeapBinding {
                    id,
                    rhs: HeapRhs::Constructor {
                        constructor,
                        fields,
                    },
                }),
                body,
            });
            body = wire.expressions.nodes.len() - 1;
        }

        let Group::NonRecursive(top) = &mut wire.bindings[0] else {
            unreachable!()
        };
        top.binding.rhs = HeapRhs::Function {
            signature: SignatureId(0),
            parameters: vec![],
            captures: vec![],
            body,
        };
        let prepared = testing::prepare(wire).expect("stack_map_chain_list_program fixture");
        link_program(prepared, &MachineImports::default())
            .expect("stack_map_chain_list_program fixture links")
    }

    /// The length-`n` `Cons` chain `program` returns, read back through
    /// `inspect_outer` (non-forcing, host-boundary observation -- a
    /// separate correctness signal from the collector's own internal
    /// tracing, confirming the chain a GC-surviving collector produced is
    /// actually the right shape and length, not merely non-crashing).
    fn read_list_length(
        machine: &mut PreparedMachine<'static>,
        cons_host_id: tidepool_repr::DataConId,
        nil_host_id: tidepool_repr::DataConId,
        mut handle: PreparedHandle,
    ) -> u32 {
        let mut length = 0;
        loop {
            let PreparedOuter::Constructor { identity, fields } = machine
                .inspect_outer(handle, RealmId::ROOT)
                .expect("list cell inspects");
            assert!(machine.release(handle));
            if identity == nil_host_id {
                assert!(fields.is_empty());
                return length;
            }
            assert_eq!(identity, cons_host_id);
            let [PreparedResult::Managed(tail)] = fields.as_slice() else {
                panic!("Cons must have exactly one managed field");
            };
            length += 1;
            handle = *tail;
        }
    }

    /// Test-only: force from-space poisoning for the duration of one S2b
    /// mutation-check test, following `gc_fault_recovery.rs`'s
    /// `DiagnosticOverrides` pattern. Poisoning fills a retired from-space
    /// buffer with an unmistakable tag after every collection, so a missed
    /// stack root (e.g. from a truncated stack-map chain) reads back as a
    /// deterministic failure instead of sometimes-correct garbage.
    struct PoisonGuard;

    impl PoisonGuard {
        fn enabled() -> Self {
            crate::host_fns::set_gc_poison(true);
            Self
        }
    }

    impl Drop for PoisonGuard {
        fn drop(&mut self) {
            crate::host_fns::clear_gc_poison_override();
        }
    }

    /// S2b test 1: a collection triggered from WITHIN a second installed
    /// program's own live native call, tracing a value that is a bare
    /// Cranelift local (not a registered Rust root). A's own tiny CAF
    /// installs first (so its stack-map registry occupies the chain's
    /// first slot); B, installed second under a small nursery, builds a
    /// 40-cell chain that cannot fit without at least one mid-call
    /// collection, and each cell but the last is live only as a Cranelift
    /// local across the next cell's own allocation.
    #[test]
    fn s2b_second_program_native_frame_is_walked_through_the_stack_map_chain() {
        let _poison_guard = PoisonGuard::enabled();
        // 40 Cons cells are 640 bytes of payload+header: under a 256-byte
        // nursery the chain CANNOT be built without collecting while B's
        // own frame, holding the previous cell as a bare local, is live.
        // (Under the 4096-byte default it fit entirely, and the one
        // collection the test used to observe was result retention after B
        // had returned -- a window in which a truncated chain is harmless.)
        let (mut machine, program_a) = PreparedMachine::new(
            first(&base_program(995)),
            PreparedMachineOptions { nursery_bytes: 256 },
        )
        .expect("A installs first, occupying the chain's first stack-map slot");
        let program_b = install_linked(
            &mut machine,
            &stack_map_chain_list_program("S2bChain", 920, 921, 40),
            ImportBindings::new(),
        )
        .expect("B installs second, extending the shared stack-map chain");

        let call_bridged = PreparedCallOptions {
            observation_budget: RunOptions::default().observation_budget,
            collect_before_observation: false,
        };
        let call_retained = PreparedCallOptions {
            observation_budget: 0,
            collect_before_observation: false,
        };
        let a_result = machine
            .run_entry(program_a, ValueId(0), &[], call_bridged, RealmId::ROOT)
            .expect("A's own entry still runs correctly alongside B");
        assert!(matches!(
            a_result.values.as_slice(),
            [tidepool_bridge::HaskellValue::Con(id, fields)]
                if *id == tidepool_repr::DataConId(995) && fields.is_empty()
        ));

        let b_result = machine
            .run_entry_retained(program_b, ValueId(0), &[], call_retained, RealmId::ROOT)
            .expect(
                "B builds its 40-cell chain across at least one mid-call collection; if the \
                 stack-map chain only resolved A's registry, B's own live locals at that \
                 collection would be missed and the run would corrupt or crash rather than \
                 return cleanly",
            );
        assert!(
            b_result.collections >= 2,
            "640 bytes of Cons cells in a 256-byte nursery must force at least two \
             collections DURING B's own native call, before the one result retention \
             adds (collect_before_observation is false here). Mutation-checked: with \
             `MachineState::stack_map_registries` truncated to A's registry this call \
             fails with IncompletePromotion(InvalidManagedPointer), the dangling tail \
             a missed root left pointing into the poisoned retired semispace"
        );
        let [PreparedResult::Managed(list)] = b_result.values.as_slice() else {
            panic!("B must return one managed list head");
        };
        let length = read_list_length(
            &mut machine,
            tidepool_repr::DataConId(921),
            tidepool_repr::DataConId(920),
            *list,
        );
        assert_eq!(
            length, 39,
            "the full 39-Cons/1-Nil chain must read back intact after collection(s) \
             during its own construction"
        );
        assert_eq!(machine.handle_count(), 0);
        assert_eq!(machine.disposition(), MachineDisposition::Reusable);
    }

    /// S2b test 2: retention of one of A's genuinely STATIC (not nursery)
    /// objects through B, exercising the machine-wide static-region SET
    /// (`observe.rs`'s `self.statics` union, `machine.rs`'s
    /// `prepared_static_reference` promotion check) rather than a single
    /// program's own region. `field_constructor_program`'s CAF is
    /// memoized: A's own repeated entry calls make its result an
    /// old-space-stable value the SECOND time it is read, which
    /// `promote_prepared`'s already-stable fast path (`admit`/
    /// `prepared_static_reference`) must recognize through the union, not
    /// only A's own region, when B is the one holding the handle.
    #[test]
    fn s2b_a_static_object_is_retained_through_b_via_the_shared_static_region_set() {
        let _poison_guard = PoisonGuard::enabled();
        let (mut machine, program_a) = PreparedMachine::new(
            field_constructor_program(985),
            PreparedMachineOptions {
                nursery_bytes: RunOptions::default().nursery_bytes,
            },
        )
        .expect("A installs first, occupying the chain's first static-region slot");
        let program_b = install_linked(
            &mut machine,
            &managed_argument_consumer_program(),
            ImportBindings::new(),
        )
        .expect("B installs second, extending the shared static-region set");

        let call = PreparedCallOptions {
            observation_budget: 0,
            collect_before_observation: false,
        };
        let produced = machine
            .run_entry_retained(program_a, ValueId(0), &[], call, RealmId::ROOT)
            .expect("A produces its retained constructor");
        let [PreparedResult::Managed(handle)] = produced.values.as_slice() else {
            panic!("A must return one managed constructor");
        };

        // B accepts A's already-retained (old-space-stable) handle as a
        // managed argument; its own 32 throwaway allocations force a
        // collection whose promotion path must recognize the argument as
        // already stable through the machine-wide static/old-space union,
        // not corrupt or duplicate it.
        let consumed = machine
            .run_entry_retained(
                program_b,
                ValueId(0),
                &[PreparedInput::Managed(*handle)],
                call,
                RealmId::ROOT,
            )
            .expect("B accepts A's already-stable handle and collects during its own call");
        let [PreparedResult::Managed(returned)] = consumed.values.as_slice() else {
            panic!("B must retain the returned argument");
        };
        let PreparedOuter::Constructor { identity, .. } = machine
            .inspect_outer(*returned, RealmId::ROOT)
            .expect("the returned handle inspects through the machine-wide union");
        assert_eq!(identity, tidepool_repr::DataConId(985));
        assert_eq!(
            machine.handle_current_pointer(*handle),
            machine.handle_current_pointer(*returned),
            "identity: B returned the SAME already-stable object A produced"
        );
        assert!(machine.release(*handle));
        assert!(machine.release(*returned));
        assert_eq!(machine.handle_count(), 0);
        assert_eq!(machine.disposition(), MachineDisposition::Reusable);
    }

    /// A parked frame owns its continuation's root for the whole park: the
    /// handle leaves the ledger and the persistent-root class, the value
    /// survives a forced collection while parked, `take_parked` hands it back
    /// as a realm-owned handle that observes to the same value, a taken id
    /// is unknown afterwards, and closing the realm drops a parked frame and
    /// its stowed root. The rooting receipt holds at every step.
    #[test]
    fn a_parked_frame_roots_its_continuation_until_taken_or_its_realm_closes() {
        let (mut machine, program) = machine();
        let realm = RealmId::fresh();
        let call = PreparedCallOptions {
            observation_budget: 100,
            collect_before_observation: true,
        };
        let evidence = PreparedFrameEvidence {
            owner: program,
            site: 7,
            runner: program,
            resume_entry: ValueId(0),
            continuation_rep: RuntimeRep::LiftedRef,
        };

        let batch = machine
            .run_entry_retained(program, ValueId(0), &[], call, realm)
            .expect("the CAF entry runs and retains its constructor");
        let [PreparedResult::Managed(continuation)] = batch.values.as_slice() else {
            panic!("the CAF entry returns one managed value");
        };
        let continuation = *continuation;
        let handles_before = machine.handle_count();
        let roots_before = machine.total_persistent_roots();

        // A handle from another realm is refused with nothing changed.
        let other = RealmId::fresh();
        assert!(matches!(
            machine.park(
                continuation,
                other,
                None,
                ParkRequest {
                    principal: PrincipalId::SYSTEM,
                    effect_policy: EffectRunPolicy::SuspendAll,
                    live_payload: LivePayloadPolicy::None,
                    evidence,
                },
            ),
            Err(ExecutionError::UnknownPreparedHandle)
        ));
        assert_eq!(machine.parked_count(), 0);
        assert_eq!(machine.handle_count(), handles_before);

        let id = machine
            .park(
                continuation,
                realm,
                None,
                ParkRequest {
                    principal: PrincipalId::SYSTEM,
                    effect_policy: EffectRunPolicy::SuspendAll,
                    live_payload: LivePayloadPolicy::None,
                    evidence,
                },
            )
            .expect("a realm-owned handle parks");
        assert_eq!(machine.parked_count(), 1);
        assert_eq!(machine.handle_count(), handles_before - 1);
        assert_eq!(machine.total_persistent_roots(), roots_before - 1);
        assert_eq!(machine.parked_realm(id), Some(realm));
        assert_eq!(machine.parked(id).map(|(_, e)| *e), Some(evidence));
        assert!(
            !machine.release(continuation),
            "the parked continuation is no longer a handle"
        );

        // A collection while parked: the frame is the value's only root.
        machine
            .run_entry(program, ValueId(0), &[], call, realm)
            .expect("an unrelated call collects while the frame is parked");
        assert_eq!(machine.parked_count(), 1);

        let (taken, taken_evidence) = machine.take_parked(id).expect("the frame is taken once");
        assert_eq!(taken_evidence, evidence);
        assert_eq!(machine.parked_count(), 0);
        assert_eq!(machine.handle_count(), handles_before);
        assert_eq!(machine.total_persistent_roots(), roots_before);
        assert_eq!(machine.handle_realm(taken), Some(realm));
        assert!(matches!(
            machine.observe_handle(program, taken, 100),
            Ok(HaskellValue::Con(id, ref fields)) if id == DataConId(900) && fields.is_empty()
        ));
        assert!(matches!(
            machine.take_parked(id),
            Err(ExecutionError::UnknownContinuation(taken_id)) if taken_id == id
        ));

        let again = machine
            .park(
                taken,
                realm,
                None,
                ParkRequest {
                    principal: PrincipalId::SYSTEM,
                    effect_policy: EffectRunPolicy::SuspendAll,
                    live_payload: LivePayloadPolicy::None,
                    evidence,
                },
            )
            .expect("the taken handle parks again");
        assert_eq!(machine.close_realm(realm), (1, 0));
        assert_eq!(machine.parked_count(), 0);
        assert!(machine.parked_realm(again).is_none());
        assert_eq!(machine.handle_count(), handles_before - 1);
        assert_eq!(machine.disposition(), MachineDisposition::Reusable);
    }

    /// Managed construction resolves constructors through the interner and
    /// publishes only a completed root. Unknown constructors and wrong arity
    /// leave the allocation cursor, handle ledger and root counts untouched.
    #[test]
    fn managed_construction_builds_through_the_interner_or_leaves_the_heap_untouched() {
        use crate::prepared_program::AnswerBuildError;
        let (mut machine, program) = machine();
        let realm = RealmId::fresh();
        let handles_before = machine.handle_count();
        let roots_before = machine.total_persistent_roots();
        let cursor_before = machine.vmctx.alloc_ptr;

        {
            let mut builder = machine.managed_builder().expect("managed builder");
            assert!(matches!(
                builder.constructor(DataConId(4242), &[]),
                Err(ExecutionError::Answer(
                    AnswerBuildError::UnknownConstructor(DataConId(4242))
                ))
            ));
            assert!(matches!(
                builder.constructor(DataConId(900), &[ManagedField::Scalar([0; 16])]),
                Err(ExecutionError::Answer(AnswerBuildError::FieldCount {
                    expected: 0,
                    actual: 1,
                    ..
                }))
            ));
        }
        assert_eq!(machine.vmctx.alloc_ptr, cursor_before);
        assert_eq!(machine.handle_count(), handles_before);
        assert_eq!(machine.total_persistent_roots(), roots_before);

        let mut builder = machine.managed_builder().expect("managed builder");
        let unit = builder
            .constructor(DataConId(900), &[])
            .expect("the CAF program's Unit constructor builds");
        let handle = builder.finish(realm, unit).expect("Unit is published");
        assert_eq!(machine.handle_realm(handle), Some(realm));
        assert_eq!(machine.handle_count(), handles_before + 1);
        assert!(matches!(
            machine.observe_handle(program, handle, 100),
            Ok(HaskellValue::Con(id, ref fields)) if id == DataConId(900) && fields.is_empty()
        ));
        // The built value survives a collection like any retained value.
        machine
            .run_entry(
                program,
                ValueId(0),
                &[],
                PreparedCallOptions {
                    observation_budget: 100,
                    collect_before_observation: true,
                },
                realm,
            )
            .expect("an unrelated call collects");
        assert!(matches!(
            machine.observe_handle(program, handle, 100),
            Ok(HaskellValue::Con(id, ref fields)) if id == DataConId(900) && fields.is_empty()
        ));
        assert!(machine.release(handle));
        assert_eq!(machine.handle_count(), handles_before);
        assert_eq!(machine.disposition(), MachineDisposition::Reusable);
    }

    /// A managed node is meaningful only to the builder that minted it. A
    /// later builder can reuse the same root-vector index, so accepting an
    /// earlier node would silently publish or embed the wrong object.
    #[test]
    fn a_managed_node_cannot_cross_builder_ownership() {
        use crate::prepared_program::AnswerBuildError;

        let (mut machine, _) = machine();
        let stale = {
            let mut builder = machine.managed_builder().expect("first builder");
            builder
                .constructor(DataConId(900), &[])
                .expect("first builder constructs Unit")
        };

        let mut builder = machine.managed_builder().expect("second builder");
        let fresh = builder
            .constructor(DataConId(900), &[])
            .expect("second builder constructs Unit at the same local index");
        assert_ne!(stale, fresh);
        assert!(matches!(
            builder.finish(RealmId::ROOT, stale),
            Err(ExecutionError::Answer(AnswerBuildError::ForeignNode))
        ));

        let mut builder = machine.managed_builder().expect("third builder");
        let unit = builder
            .constructor(DataConId(900), &[])
            .expect("third builder constructs Unit");
        let handle = builder
            .finish(RealmId::ROOT, unit)
            .expect("the rejected cross-builder node leaves the machine reusable");
        assert!(machine.release(handle));
        assert_eq!(machine.disposition(), MachineDisposition::Reusable);
    }

    #[test]
    fn active_intrinsic_program_refuses_nesting_and_clears_on_scope_exit() {
        let (machine, program) = machine();
        let compiled = machine.programs[&program].program.get();
        {
            let _active = crate::prepared_program::ActiveIntrinsicScope::new(
                &machine.machine,
                compiled,
                &machine.statics,
                &machine.descriptor_registry,
            )
            .expect("first intrinsic operation owns the invocation scope");
            assert!(matches!(
                crate::prepared_program::ActiveIntrinsicScope::new(
                    &machine.machine,
                    compiled,
                    &machine.statics,
                    &machine.descriptor_registry,
                ),
                Err(ExecutionError::Invariant(
                    "a nested managed intrinsic operation is already active"
                ))
            ));
            assert!(unsafe { machine.machine.active_intrinsic_program() }.is_some());
        }
        assert!(unsafe { machine.machine.active_intrinsic_program() }.is_none());
        crate::prepared_program::ActiveIntrinsicScope::new(
            &machine.machine,
            compiled,
            &machine.statics,
            &machine.descriptor_registry,
        )
        .expect("scope cleanup permits later intrinsic construction");
    }

    /// A constructor with a scalar field observes the field back as its
    /// declared representation. Supplying a node for that scalar field is a
    /// typed reusable failure and publishes no partial result.
    #[test]
    fn managed_scalar_fields_build_and_a_bad_field_remains_reusable() {
        use crate::prepared_program::AnswerBuildError;
        let (mut machine, program) = PreparedMachine::new(
            field_constructor_program(910),
            PreparedMachineOptions {
                nursery_bytes: RunOptions::default().nursery_bytes,
            },
        )
        .expect("prepared machine");
        let realm = RealmId::fresh();
        let handles_before = machine.handle_count();

        let mut bits = [0u8; 16];
        bits[..8].copy_from_slice(&42_i64.to_ne_bytes());
        let mut builder = machine.managed_builder().expect("managed builder");
        let good = builder
            .constructor(DataConId(910), &[ManagedField::Scalar(bits)])
            .expect("the Field constructor builds with its Int(64) field");
        let handle = builder.finish(realm, good).expect("Field is published");
        assert_eq!(machine.handle_realm(handle), Some(realm));
        assert_eq!(machine.handle_count(), handles_before + 1);
        assert!(matches!(
            machine.observe_handle(program, handle, 100),
            Ok(HaskellValue::Con(id, ref fields))
                if id == DataConId(910)
                    && matches!(
                        fields.as_slice(),
                        [HaskellValue::Lit(tidepool_repr::Literal::LitInt(42))]
                    )
        ));

        let cursor_after_good = machine.vmctx.alloc_ptr;
        let handles_after_good = machine.handle_count();
        let roots_after_good = machine.total_persistent_roots();

        let mut builder = machine.managed_builder().expect("managed builder");
        let inner = builder
            .constructor(DataConId(910), &[ManagedField::Scalar(bits)])
            .expect("inner Field builds");
        assert!(matches!(
            builder.constructor(DataConId(910), &[ManagedField::Node(inner)]),
            Err(ExecutionError::Answer(AnswerBuildError::Field {
                host_id: DataConId(910),
                index: 0,
                ..
            }))
        ));
        drop(builder);
        assert!(machine.vmctx.alloc_ptr > cursor_after_good);
        assert_eq!(machine.handle_count(), handles_after_good);
        assert_eq!(machine.total_persistent_roots(), roots_after_good);

        assert!(machine.release(handle));
        assert_eq!(machine.handle_count(), handles_before);
        assert_eq!(machine.disposition(), MachineDisposition::Reusable);
    }

    /// A program declaring a `Text`-shaped constructor (`ByteArray#`, `Int#`
    /// offset, `Int#` length) beside a nullary `Unit` its CAF returns, so a
    /// managed construction can exercise the byte-backed leaf without the program ever
    /// constructing one itself.
    fn text_shaped_program(unit_id: u64, text_id: u64) -> CompiledProgram {
        let mut wire = testing::wire_program();
        wire.signatures[0].results = ResultContract::Returns(vec![RuntimeRep::LiftedRef]);
        wire.constructors.push(ConstructorDecl {
            identity: testing::identity("MachineImports", "Unit"),
            family: testing::identity("MachineImports", "Unit"),
            host_id: tidepool_repr::DataConId(unit_id),
            result_rep: RuntimeRep::LiftedRef,
            tag: 1,
            family_size: 1,
            field_reps: vec![],
            strict_fields: vec![],
            layout: CheckedLayout {
                fields: vec![],
                alignment: 1,
                payload_size: 0,
                root_mask: vec![],
            },
        });
        wire.constructors.push(ConstructorDecl {
            identity: testing::identity("Data.Text.Internal", "Text"),
            family: testing::identity("Data.Text.Internal", "Text"),
            host_id: tidepool_repr::DataConId(text_id),
            result_rep: RuntimeRep::LiftedRef,
            tag: 1,
            family_size: 1,
            field_reps: vec![
                RuntimeRep::UnliftedRef,
                RuntimeRep::Int(64),
                RuntimeRep::Int(64),
            ],
            strict_fields: vec![true, true, true],
            layout: CheckedLayout {
                fields: vec![
                    FieldLayout {
                        rep: RuntimeRep::UnliftedRef,
                        offset: 0,
                    },
                    FieldLayout {
                        rep: RuntimeRep::Int(64),
                        offset: 8,
                    },
                    FieldLayout {
                        rep: RuntimeRep::Int(64),
                        offset: 16,
                    },
                ],
                alignment: 8,
                payload_size: 24,
                root_mask: vec![true, false, false],
            },
        });
        wire.expressions.nodes[0] = ExprFrame::Construct {
            constructor: ConstructorId(0),
            fields: vec![],
        };
        let Group::NonRecursive(top) = &mut wire.bindings[0] else {
            unreachable!()
        };
        top.binding.rhs = HeapRhs::Thunk {
            signature: SignatureId(0),
            update: UpdatePolicy::Memoize,
            captures: vec![],
            body: 0,
        };
        let prepared = testing::prepare(wire).expect("text_shaped_program fixture");
        let linked = link_program(prepared, &MachineImports::default())
            .expect("text_shaped_program fixture links");
        CompiledProgram::compile(&linked).expect("text_shaped_program fixture compiles")
    }

    /// A byte-backed managed value allocates its payload only after wrapper
    /// capacity is available and observes back as the bytes it was given. If
    /// a later field fails, the initialized wrapper is unreachable and the
    /// next collection reclaims its payload; the machine remains reusable.
    #[test]
    fn byte_backed_managed_construction_builds_and_failure_is_collectable() {
        use crate::prepared_program::AnswerBuildError;
        let (mut machine, program) = PreparedMachine::new(
            text_shaped_program(920, 921),
            PreparedMachineOptions {
                nursery_bytes: RunOptions::default().nursery_bytes,
            },
        )
        .expect("prepared machine");
        let realm = RealmId::fresh();
        let handles_before = machine.handle_count();
        let roots_before = machine.total_persistent_roots();
        let ledger_before = machine.machine.external_storage_stats();
        let cursor_before = machine.vmctx.alloc_ptr;
        let text = "héllo, wörld".as_bytes().to_vec();
        let int_bits = |value: i64| {
            let mut bits = [0u8; 16];
            bits[..8].copy_from_slice(&value.to_ne_bytes());
            bits
        };
        // A failing later field leaves the initialized byte wrapper
        // unreachable. It publishes no result root; normal collection owns
        // reclamation rather than pretending to roll the nursery cursor back.
        let mut builder = machine.managed_builder().expect("managed builder");
        let bytes = builder.bytes(&text).expect("byte wrapper builds");
        let unit = builder
            .constructor(DataConId(920), &[])
            .expect("Unit builds");
        assert!(matches!(
            builder.constructor(
                DataConId(921),
                &[
                    ManagedField::Node(bytes),
                    ManagedField::Scalar(int_bits(0)),
                    ManagedField::Node(unit),
                ],
            ),
            Err(ExecutionError::Answer(AnswerBuildError::Field {
                host_id: DataConId(921),
                index: 2,
                ..
            }))
        ));
        drop(builder);
        assert_eq!(
            machine.machine.external_storage_stats().live_objects,
            ledger_before.live_objects + 1
        );
        assert!(machine.vmctx.alloc_ptr > cursor_before);
        assert_eq!(machine.handle_count(), handles_before);
        assert_eq!(machine.total_persistent_roots(), roots_before);

        machine
            .run_entry(
                program,
                ValueId(0),
                &[],
                PreparedCallOptions {
                    observation_budget: 100,
                    collect_before_observation: true,
                },
                realm,
            )
            .expect("the machine remains usable and collects the refused wrapper");
        assert_eq!(
            machine.machine.external_storage_stats().live_objects,
            ledger_before.live_objects
        );

        let mut builder = machine.managed_builder().expect("managed builder");
        let bytes = builder.bytes(&text).expect("byte wrapper builds");
        let root = builder
            .constructor(
                DataConId(921),
                &[
                    ManagedField::Node(bytes),
                    ManagedField::Scalar(int_bits(0)),
                    ManagedField::Scalar(int_bits(text.len() as i64)),
                ],
            )
            .expect("the Text-shaped constructor builds over its byte array");
        let handle = builder.finish(realm, root).expect("Text is published");
        assert_eq!(
            machine.machine.external_storage_stats().live_objects,
            ledger_before.live_objects + 1
        );
        assert_eq!(machine.handle_count(), handles_before + 1);
        let observed = |machine: &mut PreparedMachine<'_>| {
            matches!(
                machine.observe_handle(program, handle, 1_000),
                Ok(HaskellValue::Con(id, ref fields))
                    if id == DataConId(921)
                        && matches!(
                            fields.as_slice(),
                            [
                                HaskellValue::Lit(tidepool_repr::Literal::LitByteArray(bytes)),
                                HaskellValue::Lit(tidepool_repr::Literal::LitInt(0)),
                                HaskellValue::Lit(tidepool_repr::Literal::LitInt(len)),
                            ] if *bytes == text && *len == text.len() as i64
                        )
            )
        };
        assert!(
            observed(&mut machine),
            "the built Text observes as its bytes"
        );
        machine
            .run_entry(
                program,
                ValueId(0),
                &[],
                PreparedCallOptions {
                    observation_budget: 100,
                    collect_before_observation: true,
                },
                realm,
            )
            .expect("an unrelated call collects");
        assert!(
            observed(&mut machine),
            "the built Text survives a collection"
        );

        assert!(machine.release(handle));
        assert_eq!(machine.handle_count(), handles_before);
        assert_eq!(machine.disposition(), MachineDisposition::Reusable);
    }

    /// Compile a fixture as the first program of a fresh machine
    /// (`PreparedMachine::new` adopts its externals into the machine-wide
    /// interner).
    fn first(linked: &tidepool_repr::execution_schema::LinkedProgram) -> CompiledProgram {
        CompiledProgram::compile(linked).expect("first program compiles")
    }

    /// Compile a fixture against an existing machine's interner and install
    /// it as a later program. Every second-and-later install must go
    /// through `compile_for_install` so its externals are the machine's.
    fn install_linked(
        machine: &mut PreparedMachine<'_>,
        linked: &tidepool_repr::execution_schema::LinkedProgram,
        imports: ImportBindings,
    ) -> Result<ProgramId, ExecutionError> {
        let compiled = machine
            .compile_for_install(linked)
            .expect("program compiles for install");
        machine.install_program(compiled, imports)
    }

    /// The same program `machine()` installs, as a linked program a later
    /// install compiles against the machine's interner: a memoized CAF whose
    /// top is a thunk (an owned header) returning the shared `Unit`.
    fn unit_thunk_linked() -> tidepool_repr::execution_schema::LinkedProgram {
        let mut wire = testing::wire_program();
        wire.signatures[0].results = ResultContract::Returns(vec![RuntimeRep::LiftedRef]);
        wire.constructors.push(ConstructorDecl {
            identity: testing::identity("W5", "Unit"),
            family: testing::identity("W5", "Unit"),
            host_id: tidepool_repr::DataConId(900),
            result_rep: RuntimeRep::LiftedRef,
            tag: 1,
            family_size: 1,
            field_reps: vec![],
            strict_fields: vec![],
            layout: CheckedLayout {
                fields: vec![],
                alignment: 1,
                payload_size: 0,
                root_mask: vec![],
            },
        });
        wire.expressions.nodes[0] = ExprFrame::Construct {
            constructor: ConstructorId(0),
            fields: vec![],
        };
        let Group::NonRecursive(top) = &mut wire.bindings[0] else {
            unreachable!()
        };
        top.binding.rhs = HeapRhs::Thunk {
            signature: SignatureId(0),
            update: UpdatePolicy::Memoize,
            captures: vec![],
            body: 0,
        };
        link_program(
            testing::prepare(wire).expect("unit thunk fixture"),
            &MachineImports::default(),
        )
        .expect("unit thunk fixture links")
    }

    /// Bounded residency (lifetime contract, first slice): install a program
    /// per iteration, retain its top (a thunk: an owned header, so the handle
    /// keeps the program live), run it, release the previous iteration's
    /// handle, retain one host-built heap value in place of the previous
    /// one, quiesce and collect. Exactly the previous program retires each
    /// time, every residency counter is flat, and the compacted prepared
    /// old-space bytes are flat after one warm-up cycle, so repeated
    /// turns far beyond any fixed slot budget leave a bounded machine. The
    /// surviving handle still observes at the end.
    #[test]
    fn repeated_installs_retire_and_keep_residency_flat() {
        // 2,000 cycles run in the ordinary suite; the contract's 10,000 is
        // the same loop with `TIDEPOOL_RESIDENCY_ITERATIONS=10000`.
        let iterations = std::env::var("TIDEPOOL_RESIDENCY_ITERATIONS")
            .ok()
            .and_then(|value| value.parse().ok())
            .unwrap_or(2_000_usize);
        let (mut machine, first) = machine();
        let call = PreparedCallOptions {
            observation_budget: 100,
            collect_before_observation: false,
        };
        let mut previous = machine
            .retain_top(first, ValueId(0))
            .expect("retain the first top");
        let mut latest = first;
        // Prepared old-space bytes join the flat check: each collection
        // compacts the descriptor arenas down to the live objects, so the
        // figure is the same after warm-up as every other counter.
        let mut baseline: Option<(ResidencyCounts, usize)> = None;
        let mut old_bytes: Option<usize> = None;
        let mut answer_handle: Option<PreparedHandle> = None;
        for iteration in 0..iterations {
            let compiled = machine
                .compile_for_install(&unit_thunk_linked())
                .expect("the shared Unit compiles against the machine interner");
            let program = machine
                .install_program(compiled, ImportBindings::new())
                .expect("install");
            let handle = machine
                .retain_top(program, ValueId(0))
                .expect("retain the new top");
            machine
                .run_entry(program, ValueId(0), &[], call, RealmId::ROOT)
                .expect("the new program runs");
            assert!(machine.release(previous));
            previous = handle;
            // One heap value retained per turn, the previous turn's released:
            // without compaction its promotion arena would accumulate.
            let mut builder = machine.managed_builder().expect("managed builder");
            let unit = builder
                .constructor(DataConId(900), &[])
                .expect("a Unit value builds");
            let answer = builder
                .finish(RealmId::ROOT, unit)
                .expect("the Unit value is retained");
            if let Some(stale) = answer_handle.replace(answer) {
                assert!(machine.release(stale));
            }
            let token = machine.quiesce().expect("quiescent between calls");
            let receipt = machine.collect_major(token).expect("major collection");
            assert_eq!(
                receipt.programs,
                vec![latest],
                "iteration {iteration}: exactly the released program retires"
            );
            assert!(machine.old_space.prepared_arenas.len() <= 1);
            latest = program;
            let counts = machine.residency();
            assert_eq!(counts.programs, 1);
            match baseline {
                None => baseline = Some((counts, machine.descriptors.len())),
                Some(baseline) => assert_eq!(
                    (counts, machine.descriptors.len()),
                    baseline,
                    "iteration {iteration}"
                ),
            }
            // Retiring blocks are detached before compaction, so a retiring
            // program's objects leave in the same collection: bytes are flat
            // from the first cycle on.
            match old_bytes {
                None => {
                    assert!(receipt.old_bytes > 0, "the latest answer stays retained");
                    old_bytes = Some(receipt.old_bytes);
                }
                Some(old_bytes) => {
                    assert_eq!(receipt.old_bytes, old_bytes, "iteration {iteration}");
                }
            }
            assert!(receipt.deferred.is_empty(), "iteration {iteration}");
        }
        assert!(matches!(
            machine.observe_handle(latest, previous, 100),
            Ok(HaskellValue::Con(id, ref fields)) if id == DataConId(900) && fields.is_empty()
        ));
        assert!(machine.release(previous));
        assert!(machine.release(answer_handle.expect("the loop ran")));
        assert_eq!(machine.disposition(), MachineDisposition::Reusable);
    }

    /// Retirement drops a program's nursery objects before its descriptors
    /// (contract decision 7): forcing A's CAF leaves its closure `f` -- a
    /// header A owns -- in the nursery, reachable only through A's root
    /// block. With no handle, frame or pin, a major collection retires A;
    /// the next ordinary collection validates every nursery allocation start
    /// against the installed descriptor space, so an `f` left behind would
    /// be an unknown descriptor and latch the machine.
    #[test]
    fn a_retired_programs_nursery_objects_leave_before_its_descriptors() {
        let (mut machine, program_a) = PreparedMachine::new(
            thunk_to_closure_program(),
            PreparedMachineOptions {
                nursery_bytes: RunOptions::default().nursery_bytes,
            },
        )
        .expect("A installs");
        let program_b = install_linked(&mut machine, &unit_thunk_linked(), ImportBindings::new())
            .expect("B installs");
        machine.pin(program_b).expect("B is installed");
        let quiet = PreparedCallOptions {
            observation_budget: RunOptions::default().observation_budget,
            collect_before_observation: false,
        };
        // A closure observes to the reserved closure sentinel, not a data
        // `HaskellValue`, so this `run_entry` (a non-retained observe: no handle is
        // minted for its result) leaves NOTHING rooting `f` once the call
        // returns -- the same orphaned-nursery-object setup the refusal used
        // to produce, now reached via a successful sentinel observation
        // instead of an `ObservationFailure`.
        assert!(matches!(
            machine.run_entry(program_a, ValueId(0), &[], quiet, RealmId::ROOT),
            Ok(ref result) if matches!(
                result.values.as_slice(),
                [HaskellValue::Con(id, fields)]
                    if *id == crate::observation::CLOSURE_SENTINEL && fields.is_empty()
            )
        ));
        assert_eq!(machine.disposition(), MachineDisposition::Reusable);
        assert_eq!(machine.handle_count(), 0);

        let token = machine.quiesce().expect("quiescent");
        let receipt = machine.collect_major(token).expect("major collection");
        assert_eq!(receipt.programs, vec![program_a], "{receipt:?}");
        assert!(receipt.deferred.is_empty(), "{receipt:?}");

        let collecting = PreparedCallOptions {
            collect_before_observation: true,
            ..quiet
        };
        machine
            .run_entry(program_b, ValueId(0), &[], collecting, RealmId::ROOT)
            .expect("an ordinary collection after A's retirement succeeds");
        assert_eq!(machine.disposition(), MachineDisposition::Reusable);
        let token = machine.quiesce().expect("still quiescent and reusable");
        let receipt = machine
            .collect_major(token)
            .expect("second major collection");
        assert!(receipt.programs.is_empty(), "B is pinned: {receipt:?}");
        assert_eq!(machine.disposition(), MachineDisposition::Reusable);
    }

    /// A program whose only top is a byte literal: its root block holds a
    /// raw address into its literal pool, not a managed reference.
    fn byte_top_program() -> CompiledProgram {
        let mut wire = testing::wire_program();
        wire.signatures[0].results = ResultContract::Returns(vec![RuntimeRep::Address]);
        wire.expressions.nodes[0] = ExprFrame::Return(vec![Atom::Ref(ValueRef::Local(ValueId(1)))]);
        wire.bindings.push(Group::NonRecursive(TopBinding {
            identity: testing::identity("MachineBytes", "bytes"),
            binding: HeapBinding {
                id: ValueId(1),
                rhs: HeapRhs::Bytes(b"residency".to_vec()),
            },
        }));
        let linked = link_program(
            testing::prepare(wire).expect("byte-top fixture"),
            &MachineImports::default(),
        )
        .expect("byte-top fixture links");
        let program = CompiledProgram::compile(&linked).expect("byte-top fixture compiles");
        assert!(!program.byte_tops.is_empty());
        program
    }

    /// Turns on a machine that ends each one with zero handles: a
    /// byte-pinned program's block (a raw literal address among its words)
    /// is marked every collection without being mistaken for a managed
    /// reference, and a turn that retires every other program -- the table
    /// then holding only the byte-pinned one, or nothing at all -- leaves a
    /// heap the next install extends rather than recreates.
    #[test]
    fn zero_handle_turns_collect_and_reinstall() {
        let call = PreparedCallOptions {
            observation_budget: 100,
            collect_before_observation: false,
        };
        let turn = |machine: &mut PreparedMachine<'static>, program, label: &str| {
            let batch = machine
                .run_entry_retained(program, ValueId(0), &[], call, RealmId::ROOT)
                .unwrap_or_else(|error| panic!("{label}: runs: {error:?}"));
            for value in batch.values {
                if let PreparedResult::Managed(handle) = value {
                    assert!(machine.release(handle));
                }
            }
            assert_eq!(machine.handle_count(), 0);
            let token = machine.quiesce().expect("quiescent");
            let receipt = machine
                .collect_major(token)
                .unwrap_or_else(|error| panic!("{label}: major collection: {error:?}"));
            assert_eq!(receipt.programs, vec![program], "{label}");
            assert!(receipt.deferred.is_empty(), "{label}");
        };

        // Every program retires: the table empties under a live heap.
        let (mut machine, first) = machine();
        turn(&mut machine, first, "first, alone");
        for index in 0..3 {
            let program = install_linked(&mut machine, &unit_thunk_linked(), ImportBindings::new())
                .expect("installs into the emptied table");
            turn(&mut machine, program, &format!("empty table, turn {index}"));
        }
        assert_eq!(machine.residency().programs, 0);
    }

    /// A program whose only top is a byte literal retires by ordinary
    /// reachability, exactly like any other program with no live root: its
    /// own literal storage is no longer a liveness edge (see
    /// `PreparedMachine::mark_live_programs`'s doc). The literal content
    /// itself stays resolvable through the machine's one permanent pool for
    /// the rest of the machine's life, regardless of whether the program
    /// that first interned it is still installed.
    #[test]
    fn byte_top_program_retires_and_its_literal_stays_resolvable() {
        let program = byte_top_program();
        let address = program
            .byte_tops
            .values()
            .next()
            .expect("byte_top_program has one byte top")
            .as_ptr() as usize;
        let (mut machine, id) = PreparedMachine::new(
            program,
            PreparedMachineOptions {
                nursery_bytes: RunOptions::default().nursery_bytes,
            },
        )
        .expect("byte-top program installs");
        let token = machine.quiesce().expect("quiescent after install");
        let receipt = machine.collect_major(token).expect("major collection");
        assert_eq!(
            receipt.programs,
            vec![id],
            "nothing references the byte-top program's own top, so it retires"
        );
        assert!(machine.programs.is_empty());
        assert_eq!(
            machine
                .machine
                .resolve_literal_bytes(|pool| pool.read_range(address, 9).map(<[u8]>::to_vec)),
            Some(b"residency".to_vec()),
            "the literal stays resolvable through the permanent pool after retirement"
        );
    }

    /// Two programs that each declare the same literal content compile
    /// against the same address (`compile_for_install` resolves every
    /// literal against the machine's live pool); retiring the first leaves
    /// the second's reads of that same address valid.
    #[test]
    fn shared_literal_content_gets_one_address_across_programs() {
        let (mut machine, first) = PreparedMachine::new(
            byte_top_program(),
            PreparedMachineOptions {
                nursery_bytes: RunOptions::default().nursery_bytes,
            },
        )
        .expect("first byte-top program installs");
        let first_address = machine
            .programs
            .get(&first)
            .expect("just installed")
            .program
            .get()
            .byte_tops
            .values()
            .next()
            .expect("byte_top_program has one byte top")
            .as_ptr() as usize;

        let linked = link_program(
            testing::prepare({
                let mut wire = testing::wire_program();
                wire.signatures[0].results = ResultContract::Returns(vec![RuntimeRep::Address]);
                wire.expressions.nodes[0] =
                    ExprFrame::Return(vec![Atom::Ref(ValueRef::Local(ValueId(1)))]);
                wire.bindings.push(Group::NonRecursive(TopBinding {
                    identity: testing::identity("MachineBytesTwo", "bytes"),
                    binding: HeapBinding {
                        id: ValueId(1),
                        rhs: HeapRhs::Bytes(b"residency".to_vec()),
                    },
                }));
                wire
            })
            .expect("second byte-top fixture"),
            &MachineImports::default(),
        )
        .expect("second byte-top fixture links");
        let compiled = machine
            .compile_for_install(&linked)
            .expect("second byte-top program compiles against the machine pool");
        let second_address = compiled
            .byte_tops
            .values()
            .next()
            .expect("second byte_top program has one byte top")
            .as_ptr() as usize;
        assert_eq!(
            first_address, second_address,
            "identical literal content resolves to the same interned address"
        );
        let second = machine
            .install_program(compiled, ImportBindings::new())
            .expect("second byte-top program installs");
        // Pin the second program so it survives the collection below: its
        // own top is a raw literal address, not a heap reference, so (like
        // the first program) nothing else marks it live -- the pin isolates
        // this test to the one fact under test, that retiring `first` does
        // not disturb `second`'s interned address.
        machine.pin(second).expect("pin second");

        // Retiring the first program leaves the second's own reads of the
        // same address valid: the pool is permanent, not owned per-program.
        let token = machine.quiesce().expect("quiescent");
        let receipt = machine.collect_major(token).expect("major collection");
        assert_eq!(receipt.programs, vec![first]);
        assert!(machine.programs.contains_key(&second));
        assert_eq!(
            machine.machine.resolve_literal_bytes(|pool| pool
                .read_range(second_address, 9)
                .map(<[u8]>::to_vec)),
            Some(b"residency".to_vec())
        );
    }

    /// A program declaring a nullary `Unit` (its CAF's result) and a
    /// one-field `Box a`, so managed construction can build a nested value graph.
    fn boxed_shape_program(unit_id: u64, box_id: u64) -> CompiledProgram {
        let mut wire = testing::wire_program();
        wire.signatures[0].results = ResultContract::Returns(vec![RuntimeRep::LiftedRef]);
        wire.constructors.push(ConstructorDecl {
            identity: testing::identity("MachineCompaction", "Unit"),
            family: testing::identity("MachineCompaction", "Unit"),
            host_id: DataConId(unit_id),
            result_rep: RuntimeRep::LiftedRef,
            tag: 1,
            family_size: 1,
            field_reps: vec![],
            strict_fields: vec![],
            layout: CheckedLayout {
                fields: vec![],
                alignment: 1,
                payload_size: 0,
                root_mask: vec![],
            },
        });
        wire.constructors.push(ConstructorDecl {
            identity: testing::identity("MachineCompaction", "Box"),
            family: testing::identity("MachineCompaction", "Box"),
            host_id: DataConId(box_id),
            result_rep: RuntimeRep::LiftedRef,
            tag: 1,
            family_size: 1,
            field_reps: vec![RuntimeRep::LiftedRef],
            strict_fields: vec![false],
            layout: CheckedLayout {
                fields: vec![FieldLayout {
                    rep: RuntimeRep::LiftedRef,
                    offset: 0,
                }],
                alignment: 8,
                payload_size: 8,
                root_mask: vec![true],
            },
        });
        wire.expressions.nodes[0] = ExprFrame::Construct {
            constructor: ConstructorId(0),
            fields: vec![],
        };
        let Group::NonRecursive(top) = &mut wire.bindings[0] else {
            unreachable!()
        };
        top.binding.rhs = HeapRhs::Thunk {
            signature: SignatureId(0),
            update: UpdatePolicy::Memoize,
            captures: vec![],
            body: 0,
        };
        let prepared = testing::prepare(wire).expect("boxed_shape_program fixture");
        let linked = link_program(prepared, &MachineImports::default())
            .expect("boxed_shape_program fixture links");
        CompiledProgram::compile(&linked).expect("boxed_shape_program fixture compiles")
    }

    fn build_boxed(
        machine: &mut PreparedMachine<'_>,
        realm: RealmId,
        depth: usize,
        unit_id: u64,
        box_id: u64,
    ) -> Result<PreparedHandle, ExecutionError> {
        let mut builder = machine.managed_builder()?;
        let mut node = builder.constructor(DataConId(unit_id), &[])?;
        for _ in 0..depth {
            node = builder.constructor(DataConId(box_id), &[ManagedField::Consume(node)])?;
        }
        builder.finish(realm, node)
    }

    #[test]
    fn consuming_tree_fields_keep_only_the_live_frontier_rooted() {
        let (mut machine, _) = PreparedMachine::new(
            boxed_shape_program(930, 931),
            PreparedMachineOptions { nursery_bytes: 256 },
        )
        .expect("prepared machine");
        let mut builder = machine.managed_builder().expect("managed builder");
        let mut node = builder.constructor(DataConId(930), &[]).expect("leaf");
        for _ in 0..200 {
            node = builder
                .constructor(DataConId(931), &[ManagedField::Consume(node)])
                .expect("parent");
        }
        let (chunks, active) = builder.temporary_root_metrics();
        assert_eq!(active, 1, "consumed children leave the active root set");
        assert_eq!(chunks, 1, "one stable chunk serves the whole tree");
        drop(builder);
        assert_eq!(machine.machine.rust_roots_len(), 0);
    }

    #[test]
    fn duplicate_consuming_field_rejects_before_parent_publication() {
        let (mut machine, _) = PreparedMachine::new(
            boxed_shape_program(930, 931),
            PreparedMachineOptions { nursery_bytes: 256 },
        )
        .expect("prepared machine");
        let mut builder = machine.managed_builder().expect("managed builder");
        let child = builder.constructor(DataConId(930), &[]).expect("leaf");
        let roots_before = builder.temporary_root_metrics();
        assert!(builder
            .constructor(
                DataConId(931),
                &[ManagedField::Consume(child), ManagedField::Consume(child)],
            )
            .is_err());
        assert_eq!(builder.temporary_root_metrics(), roots_before);
        assert!(
            builder.core.word(child.node).is_ok(),
            "failed parent preserves child"
        );
    }

    /// The nesting depth of an observed `boxed` value, `None` for any other
    /// shape.
    fn boxed_depth(value: &HaskellValue, unit_id: u64, box_id: u64) -> Option<usize> {
        match value {
            HaskellValue::Con(id, fields) if *id == DataConId(unit_id) && fields.is_empty() => {
                Some(0)
            }
            HaskellValue::Con(id, fields) if *id == DataConId(box_id) => match fields.as_slice() {
                [inner] => boxed_depth(inner, unit_id, box_id).map(|depth| depth + 1),
                _ => None,
            },
            _ => None,
        }
    }

    /// Incremental managed construction roots each completed child before it
    /// asks the collector for the next object's space. The complete graph is
    /// deliberately much larger than this nursery; the old contiguous-span
    /// builder refused it as `TooLarge`.
    #[test]
    fn a_managed_value_larger_than_the_nursery_builds_across_collections() {
        let (mut machine, program) = PreparedMachine::new(
            boxed_shape_program(930, 931),
            PreparedMachineOptions { nursery_bytes: 256 },
        )
        .expect("prepared machine");
        let realm = RealmId::fresh();
        let handle = build_boxed(&mut machine, realm, 100, 930, 931)
            .expect("incremental construction spans nursery collections");
        let observed = machine
            .observe_handle(program, handle, 1_000)
            .expect("the incrementally built answer observes");
        assert_eq!(boxed_depth(&observed, 930, 931), Some(100));
        assert!(machine.release(handle));
        assert_eq!(machine.disposition(), MachineDisposition::Reusable);
    }

    #[test]
    fn a_stale_handle_field_is_rejected_without_publishing_a_root() {
        use crate::prepared_program::AnswerBuildError;

        let (mut machine, _) = PreparedMachine::new(
            boxed_shape_program(930, 931),
            PreparedMachineOptions {
                nursery_bytes: RunOptions::default().nursery_bytes,
            },
        )
        .expect("prepared machine");
        let stale = build_boxed(&mut machine, RealmId::ROOT, 0, 930, 931).expect("Unit builds");
        assert!(machine.release(stale));
        let handles_before = machine.handle_count();

        let mut builder = machine.managed_builder().expect("managed builder");
        assert!(matches!(
            builder.constructor(DataConId(931), &[ManagedField::Handle(stale)]),
            Err(ExecutionError::Answer(AnswerBuildError::UnknownHandle))
        ));
        drop(builder);
        assert_eq!(machine.handle_count(), handles_before);
        assert_eq!(machine.disposition(), MachineDisposition::Reusable);
    }

    /// `old_bytes_live` reads live promoted bytes without needing a major
    /// collection first: `ManagedBuilder::finish` retains its result in old
    /// space, so the accessor grows between two builds with no
    /// `collect_major` in between -- the property `PreparedEngine`'s
    /// amortization trigger (`tidepool-runtime/src/session/prepared.rs`,
    /// `major_collection_due`) depends on to see promotion between installs,
    /// not only at collection boundaries. It also matches the exact figure
    /// `collect_major` later reports as `RetirementReceipt::old_bytes` for
    /// the same still-reachable graph.
    #[test]
    fn old_bytes_live_grows_with_each_retained_build_before_any_collection() {
        let (mut machine, program) = PreparedMachine::new(
            boxed_shape_program(930, 931),
            PreparedMachineOptions {
                nursery_bytes: RunOptions::default().nursery_bytes,
            },
        )
        .expect("prepared machine");
        machine.pin(program).expect("installed");
        assert_eq!(machine.old_bytes_live(), 0, "nothing retained yet");

        let realm = RealmId::fresh();
        let small = build_boxed(&mut machine, realm, 1, 930, 931).expect("small graph builds");
        let after_small = machine.old_bytes_live();
        assert!(after_small > 0, "the retained graph is live old-space");
        assert_eq!(after_small, machine.old_space.prepared_bytes_used());

        let large = build_boxed(&mut machine, realm, 5, 930, 931).expect("larger graph builds");
        let after_large = machine.old_bytes_live();
        assert!(
            after_large > after_small,
            "a second retained build grows the live figure with no collection yet: \
             {after_small} then {after_large}"
        );

        let token = machine.quiesce().expect("quiescent");
        let receipt = machine.collect_major(token).expect("major collection");
        assert_eq!(
            receipt.old_bytes,
            machine.old_bytes_live(),
            "the live accessor agrees with the collection's own reported figure"
        );
        assert!(machine.release(small));
        assert!(machine.release(large));
    }

    /// Slice 1c: a value graph reached from two handles (the outer value and
    /// a handle to its field) compacts to ONE copy. The released graph
    /// beside it is reclaimed, `old_bytes` is exactly the surviving graph's
    /// extent, both handles observe their values, and the field handle still
    /// aliases the outer value's field.
    #[test]
    fn a_value_graph_shared_by_two_handles_compacts_to_one_copy() {
        let (mut machine, program) = PreparedMachine::new(
            boxed_shape_program(930, 931),
            PreparedMachineOptions {
                nursery_bytes: RunOptions::default().nursery_bytes,
            },
        )
        .expect("prepared machine");
        // Interned constructors own no program; the pin keeps the program
        // the observations below force through.
        machine.pin(program).expect("installed");
        let realm = RealmId::fresh();
        let outer = build_boxed(&mut machine, realm, 3, 930, 931).expect("the shared graph builds");
        let one_copy = machine.old_space.prepared_bytes_used();
        assert!(one_copy > 0, "the built graph is retained in old space");
        let garbage =
            build_boxed(&mut machine, realm, 2, 930, 931).expect("the discarded graph builds");
        assert!(machine.release(garbage));
        let PreparedOuter::Constructor { fields, .. } = machine
            .inspect_outer(outer, realm)
            .expect("the outer box inspects");
        let [PreparedResult::Managed(inner)] = fields.as_slice() else {
            panic!("a box has one managed field");
        };
        let inner = *inner;
        let arenas_before = machine.old_space.prepared_arenas.len();
        assert!(arenas_before >= 2, "one arena per retention promotion");
        let bytes_before = machine.old_space.prepared_bytes_used();
        assert!(bytes_before > one_copy);

        let token = machine.quiesce().expect("quiescent");
        let receipt = machine.collect_major(token).expect("major collection");
        assert!(
            receipt.programs.is_empty(),
            "the program is pinned: {receipt:?}"
        );
        assert_eq!(receipt.old_bytes, one_copy);
        assert_eq!(receipt.compacted_bytes, bytes_before - one_copy);
        assert_eq!(machine.old_space.prepared_arenas.len(), 1);
        let arena = machine.old_space.prepared_arenas[0].allocation_range();
        let address = |machine: &PreparedMachine<'_>, handle| {
            tidepool_heap::managed_reference::untag(unsafe {
                machine.handle_root(handle).expect("live handle").current()
            } as usize)
        };
        assert!(arena.contains(&address(&machine, outer)));
        assert!(arena.contains(&address(&machine, inner)));

        // The field handle still names the outer value's own field object.
        let PreparedOuter::Constructor { fields, .. } = machine
            .inspect_outer(outer, realm)
            .expect("the compacted outer box inspects");
        let [PreparedResult::Managed(field_again)] = fields.as_slice() else {
            panic!("a box has one managed field");
        };
        assert_eq!(
            address(&machine, *field_again),
            address(&machine, inner),
            "sharing survives: one copy, two handles"
        );
        assert!(machine.release(*field_again));
        let observed = machine
            .observe_handle(program, outer, 100)
            .expect("outer observes");
        assert_eq!(boxed_depth(&observed, 930, 931), Some(3), "{observed:?}");
        let observed = machine
            .observe_handle(program, inner, 100)
            .expect("inner observes");
        assert_eq!(boxed_depth(&observed, 930, 931), Some(2), "{observed:?}");

        // Nothing new is garbage: a second collection is a fixed point.
        let token = machine.quiesce().expect("quiescent");
        let receipt = machine
            .collect_major(token)
            .expect("second major collection");
        assert_eq!((receipt.old_bytes, receipt.compacted_bytes), (one_copy, 0));
        assert!(machine.release(outer));
        assert!(machine.release(inner));
        let token = machine.quiesce().expect("quiescent");
        let receipt = machine
            .collect_major(token)
            .expect("third major collection");
        assert_eq!((receipt.old_bytes, receipt.compacted_bytes), (0, one_copy));
        assert!(machine.old_space.prepared_arenas.is_empty());
        assert_eq!(machine.disposition(), MachineDisposition::Reusable);
    }

    /// Slice 1c: a parked frame's continuation -- a heap closure capturing a
    /// constructor -- is moved by compaction (its stowed cell is a
    /// compaction root, its captured environment an edge of the copy), and
    /// the taken continuation is then entered by generated code at its new
    /// address and returns its captured value from the moved environment.
    #[test]
    fn a_parked_continuation_survives_compaction_and_resumes() {
        let (mut machine, program_a) = PreparedMachine::new(
            thunk_to_closure_program(),
            PreparedMachineOptions {
                nursery_bytes: 4096,
            },
        )
        .expect("A installs");
        let program_b = install_linked(
            &mut machine,
            &closure_caller_program(),
            ImportBindings::new(),
        )
        .expect("B installs");
        let realm = RealmId::fresh();
        let call = PreparedCallOptions {
            observation_budget: 0,
            collect_before_observation: false,
        };
        let produced = machine
            .run_entry_retained(program_a, ValueId(0), &[], call, realm)
            .expect("A evaluates its CAF to the closure f");
        let [PreparedResult::Managed(f)] = produced.values.as_slice() else {
            panic!("A's CAF returns one managed closure");
        };
        let f = *f;
        // A released host value is old-space garbage for compaction.
        let mut builder = machine.managed_builder().expect("managed builder");
        let ready = builder
            .constructor(DataConId(1_301), &[])
            .expect("a Ready value builds");
        let garbage = builder
            .finish(realm, ready)
            .expect("the Ready value is retained");
        assert!(machine.release(garbage));
        let address = |machine: &PreparedMachine<'_>, handle| {
            tidepool_heap::managed_reference::untag(unsafe {
                machine.handle_root(handle).expect("live handle").current()
            } as usize)
        };
        let before = address(&machine, f);
        let ranges_before: Vec<_> = machine
            .old_space
            .prepared_arenas
            .iter()
            .map(|arena| arena.allocation_range())
            .collect();
        assert!(
            ranges_before.iter().any(|range| range.contains(&before)),
            "f is a retained heap closure in old space"
        );
        let bytes_before = machine.old_space.prepared_bytes_used();

        let evidence = PreparedFrameEvidence {
            owner: program_a,
            site: 1,
            runner: program_b,
            resume_entry: ValueId(0),
            continuation_rep: RuntimeRep::LiftedRef,
        };
        let id = machine
            .park(
                f,
                realm,
                None,
                ParkRequest {
                    principal: PrincipalId::SYSTEM,
                    effect_policy: EffectRunPolicy::SuspendAll,
                    live_payload: LivePayloadPolicy::None,
                    evidence,
                },
            )
            .expect("f parks");

        let token = machine
            .quiesce()
            .expect("parked frames do not block quiescence");
        let receipt = machine.collect_major(token).expect("major collection");
        assert!(
            receipt.programs.is_empty(),
            "the frame keeps A and B: {receipt:?}"
        );
        assert!(
            receipt.compacted_bytes > 0,
            "the released value is reclaimed"
        );
        assert_eq!(receipt.old_bytes, bytes_before - receipt.compacted_bytes);
        assert_eq!(machine.old_space.prepared_arenas.len(), 1);
        let arena = machine.old_space.prepared_arenas[0].allocation_range();
        assert!(ranges_before.iter().all(|range| *range != arena));
        assert_eq!(machine.parked_count(), 1);

        let (taken, taken_evidence) = machine.take_parked(id).expect("the frame is taken");
        assert_eq!(taken_evidence, evidence);
        let moved = address(&machine, taken);
        assert_ne!(moved, before);
        assert!(
            arena.contains(&moved),
            "the continuation lives in the compacted arena"
        );
        let resumed = machine
            .run_entry_retained(
                taken_evidence.runner,
                taken_evidence.resume_entry,
                &[PreparedInput::Managed(taken)],
                call,
                realm,
            )
            .expect("the moved continuation is entered");
        let [PreparedResult::Managed(value)] = resumed.values.as_slice() else {
            panic!("the resumed call retains one managed result");
        };
        assert!(
            arena.contains(&address(&machine, *value)),
            "the captured Ready was read from the moved environment"
        );
        let PreparedOuter::Constructor { identity, .. } = machine
            .inspect_outer(*value, realm)
            .expect("the resumed result inspects");
        assert_eq!(identity, DataConId(1_301));
        assert!(machine.release(*value));
        assert!(machine.release(taken));
        assert_eq!(machine.handle_count(), 0);
        assert_eq!(machine.disposition(), MachineDisposition::Reusable);
    }

    /// Liveness edges (a), (b) and (g): a consumer whose root block imports a
    /// producer's static closure keeps the producer installed while the
    /// consumer is pinned, even after every handle to the producer is
    /// released; unpinning the consumer retires both. A pinned program is
    /// never retired, and the gate refuses a collection while a temporary
    /// root is registered.
    #[test]
    fn a_static_import_keeps_its_producer_live_until_the_consumer_retires() {
        let (mut machine, program_a) = PreparedMachine::new(
            s3_closure_producer_program(),
            PreparedMachineOptions {
                nursery_bytes: RunOptions::default().nursery_bytes,
            },
        )
        .expect("A installs");
        let call = PreparedCallOptions {
            observation_budget: RunOptions::default().observation_budget,
            collect_before_observation: false,
        };
        let produced = machine
            .run_entry_retained(program_a, ValueId(0), &[], call, RealmId::ROOT)
            .expect("A produces its retained closure f");
        let [PreparedResult::Managed(handle_f)] = produced.values.as_slice() else {
            panic!("A must return one managed closure");
        };
        let mut imports = ImportBindings::new();
        imports.insert(s3_closure_producer_identity(), *handle_f);
        let program_b = install_linked(
            &mut machine,
            &s3_import_consumer_program(
                s3_closure_producer_identity(),
                RuntimeRep::LiftedRef,
                true,
            ),
            imports,
        )
        .expect("B installs against A's closure");
        machine.pin(program_b).expect("B is installed");
        assert!(machine.release(*handle_f));

        // B is pinned; its block imports A's static closure, so A stays.
        let token = machine.quiesce().expect("quiescent");
        let receipt = machine.collect_major(token).expect("major collection");
        assert!(receipt.programs.is_empty(), "nothing retires: {receipt:?}");
        assert_eq!(machine.residency().programs, 2);
        machine
            .run_entry_retained(program_b, ValueId(0), &[], call, RealmId::ROOT)
            .expect("B still reads its import after the collection")
            .values
            .into_iter()
            .for_each(|value| {
                if let PreparedResult::Managed(handle) = value {
                    assert!(machine.release(handle));
                }
            });

        // Unpinned, B is unreachable, and with it A.
        assert!(machine.unpin(program_b));
        let token = machine.quiesce().expect("quiescent");
        let receipt = machine.collect_major(token).expect("major collection");
        assert_eq!(receipt.programs, vec![program_a, program_b]);
        let counts = machine.residency();
        assert_eq!(counts.programs, 0);
        assert_eq!(counts.block_words, 0);
        assert_eq!(counts.persistent_roots, 0);
        assert_eq!(counts.stack_map_links, 0);
        assert_eq!(counts.static_regions, 0);
        assert_eq!((counts.callable_rows, counts.enter_rows), (0, 0));
        assert!(matches!(
            machine.run_entry(program_a, ValueId(0), &[], call, RealmId::ROOT),
            Err(ExecutionError::UnknownProgram(id)) if id == program_a
        ));
        assert_eq!(machine.disposition(), MachineDisposition::Reusable);
    }
}
