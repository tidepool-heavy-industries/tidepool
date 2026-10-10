//! Connected prepared entry ownership and native lowering.
//!
//! Compiled addresses/descriptors are program-owned; heap pointers and top
//! tables are invocation-owned. No invocation pointer is embedded in code.

use crate::entry_abi::EntryAbi;
mod addresses;
mod capabilities;
mod facts;
mod failures;
mod fingerprint;
pub use facts::DefinitionFacts;
mod lifetime;
#[cfg(test)]
mod lifetime_tests;
use crate::pipeline::{CodegenPipeline, PipelineError};
use cranelift_codegen::ir::{self, types, AbiParam, InstBuilder, Value as SsaValue};
use cranelift_frontend::FunctionBuilder;
use cranelift_module::{FuncId, Module};
use std::collections::BTreeMap;
use std::sync::Arc;
use tidepool_heap::execution_descriptor::ObjectDescriptor;
use tidepool_heap::static_region::{StaticImage, StaticImageError};
use tidepool_repr::execution_schema::{
    Architecture, CertifiedGroup, DefinitionsView, Endianness, GlobalId, LinkedProgram,
    PreparedProgram, ResultContract, RuntimeRep, Signature, TargetDescriptor, ValueId,
};
use tidepool_repr::DataConId;

mod adapter;
mod admission;
mod apply;
#[cfg(test)]
mod caller_result_tests;
pub(crate) mod compile_phases;
mod emit;
mod image;
mod image_registry;
mod package_literals;
pub use package_literals::{PackageLiteral, SourceLiteral};
mod instance;
#[cfg(test)]
mod invocation;
pub use image_registry::ImageRegistry;
mod machine;
pub(crate) mod md5_kernel;
mod no_success;
mod session_var_id;
pub use session_var_id::session_var_id;
mod evacuation;
#[cfg(test)]
mod no_success_tests;
mod observe;
pub(crate) mod resolve;
mod roots;
pub use evacuation::{ImportedParcel, Parcel, ParcelImage, ParcelImports};
pub use observe::{AddressOrigin, ObservationFailure};
mod interner;
pub use interner::DescriptorInterner;
pub(crate) use interner::ExternalDescriptors;
mod answer;
mod construction;
mod run;
pub use crate::resource_ledger::{PreparedFrameEvidence, PreparedReplyEvidence};
pub use answer::{AnswerBuildError, MAX_ANSWER_DEPTH};

struct ActiveIntrinsicScope<'a> {
    machine: &'a crate::machine_state::MachineState,
}

impl<'a> ActiveIntrinsicScope<'a> {
    fn new(
        machine: &'a crate::machine_state::MachineState,
        program: &'a CompiledProgram,
        statics: &'a tidepool_heap::static_region::StaticRegionCatalog,
        registry: &'a BTreeMap<usize, DescriptorMetadata>,
    ) -> Result<Self, ExecutionError> {
        if !machine.install_active_intrinsic_program(program, statics, registry) {
            return Err(ExecutionError::Invariant(
                "a nested managed intrinsic operation is already active",
            ));
        }
        Ok(Self { machine })
    }
}

impl Drop for ActiveIntrinsicScope<'_> {
    fn drop(&mut self) {
        self.machine.clear_active_intrinsic_program();
    }
}
pub use machine::{
    BatchImport, BatchInstallReceipt, BatchLeaseRequest, BatchProgram, GroupInstanceId,
    ImportBindings, ManagedBuilder, ManagedField, ManagedNode, ParkRequest, PreparedCallOptions,
    PreparedCompileSnapshot, PreparedHandle, PreparedInput, PreparedMachine,
    PreparedMachineOptions, PreparedOuter, PreparedResult, PreparedResultBatch, ProgramId,
    Quiescent, ResidencyCounts, RetirementReceipt,
};
pub use run::{
    BatchImportContractMismatch, BatchImportSelection, ExecutionError, ImportShapeFact, RunOptions,
    RunResult,
};
#[cfg(test)]
mod apply_tests;
mod arrays;
mod byte_arrays;
#[cfg(test)]
mod bytes_tests;
mod data_tag;
mod demand;
pub use demand::{
    DemandError, DemandedImage, GroupInventory, InheritedSourceDemand, PendingGroupInventory,
    PendingScopedSourceDemand, PendingSealedDemand, ScopedCertifiedGroup, ScopedDemandedImage,
    ScopedInheritedSourceDemand, ScopedSourceBinder, ScopedSourceGroupDemand, SealedDemand,
    SourceBinder, SourceDomainSelection, SourceGroupOutline, SourceInstanceAttachment,
    SourceInstanceDomain, SourceInstanceLease,
};
#[cfg(test)]
mod double_to_int_tests;
mod entry;
mod fallible;
mod floating;
mod forcing;
#[cfg(test)]
mod foreign_apply_tests;
mod formatting;
mod json;
mod plan;
mod primitives;
#[cfg(test)]
mod retention_tests;
mod safepoint;
#[cfg(test)]
mod settlement_tests;
pub(crate) mod static_bytes;
mod text_search;
mod time;
mod wide_words;
pub use admission::{admit_prepared, admit_program, supports_operation};

/// The native boundary that refused one declared or referenced global.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GlobalRefusalPhase {
    NonReferenceRepresentation,
    StaticManagedCapture,
    StaticAddressCapture,
    MissingCaptureDeclaration,
}

/// Bounded facts for one native global refusal; no execution graph or interface
/// artifact is copied. A certified source image also names its original owner.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
#[error("closed execution cannot admit global {id:?} at {phase:?}; declaration={declaration:?}; signature={entry_signature:?}; source_group={source_group:?}")]
pub struct GlobalRefusal {
    pub id: GlobalId,
    pub phase: GlobalRefusalPhase,
    pub declaration: Option<tidepool_repr::execution_schema::GlobalDecl>,
    pub entry_signature: Option<Signature>,
    pub source_group: Option<(tidepool_repr::execution_schema::CachedHomeOwner, u32)>,
}

fn unsupported_global(
    definitions: &DefinitionsView<'_>,
    id: GlobalId,
    phase: GlobalRefusalPhase,
) -> Unsupported {
    let declaration = definitions.globals().get(id.0 as usize).cloned();
    let entry_signature = declaration
        .as_ref()
        .and_then(|declaration| declaration.entry_signature)
        .and_then(|signature| definitions.signatures().get(signature.0 as usize))
        .cloned();
    Unsupported::Global(Box::new(GlobalRefusal {
        id,
        phase,
        declaration,
        entry_signature,
        source_group: None,
    }))
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum Unsupported {
    #[error("closed execution target {0:?} does not match the pinned native host profile")]
    Target(TargetDescriptor),
    #[error(transparent)]
    Global(Box<GlobalRefusal>),
    #[error("thunk {0:?} is outside this execution checkpoint")]
    Thunk(ValueId),
    #[error("thunk {0:?} requires a zero-argument lifted-reference result signature")]
    ThunkSignature(ValueId),
    #[error("static object contains a managed edge to heap top {0:?}")]
    StaticHeapEdge(ValueId),
    #[error("unsupported expression at node {node} in binding {binding:?}")]
    Expression { binding: ValueId, node: usize },
    #[error(
        "unsupported operation {identity:?} with signature {signature:?} at node {node} in binding {binding:?}"
    )]
    Operation {
        binding: ValueId,
        node: usize,
        identity: tidepool_repr::execution_schema::OperationIdentity,
        signature: Signature,
    },
    #[error("entry {0:?} cannot accept managed host arguments")]
    HostArguments(ValueId),
}

#[derive(Debug, thiserror::Error)]
pub enum CompileError {
    #[error("application demand has no declared dispatcher: {0:?}")]
    MissingDemand(Signature),
    #[error("checked keepAlive call lacks dispatcher for {0:?}")]
    MissingKeepAliveDispatcher(Signature),
    #[error(transparent)]
    Unsupported(#[from] Unsupported),
    #[error(transparent)]
    Pipeline(#[from] PipelineError),
    #[error(transparent)]
    Static(#[from] StaticImageError),
    #[error(transparent)]
    Abi(#[from] crate::entry_abi::AbiError),
    #[error(transparent)]
    Layout(#[from] tidepool_repr::execution_schema::LayoutError),
    #[error(transparent)]
    Descriptor(#[from] tidepool_heap::execution_descriptor::DescriptorConstructionError),
    #[error("checked program lacks representation for {0:?}")]
    MissingRepresentation(ValueId),
    #[error("literal import owner count {owners} differs from declared globals {globals}")]
    LiteralImportCount { globals: usize, owners: usize },
    #[error("immutable package literal differs from the certified global {0:?}")]
    PackageLiteralContract(Box<tidepool_repr::execution_schema::SymbolIdentity>),
    #[error("immutable source literal differs from the certified global {0:?}")]
    SourceLiteralContract(Box<SourceBinder>),
    #[error("JSON operation was admitted without a program JSON layout")]
    MissingJsonLayout,
    #[error("JSON layout names constructor {0:?}, which this program does not declare")]
    UnknownJsonConstructor(tidepool_repr::execution_schema::ConstructorId),
    #[error("the program's root block could not be allocated")]
    RootBlock,
    #[error("constructor host id {host_id:?} already names {existing:?}, not {identity:?}")]
    HostIdConflict {
        host_id: tidepool_repr::DataConId,
        identity: Box<tidepool_repr::execution_schema::SymbolIdentity>,
        existing: Box<tidepool_repr::execution_schema::SymbolIdentity>,
    },
    #[error(
        "constructor {identity:?} is declared differently from the descriptor already interned \
         for it: interned field representations {existing_field_reps:?} (arity \
         {existing_arity}), incoming {incoming_field_reps:?} (arity {incoming_arity})",
        existing_arity = existing_field_reps.len(),
        incoming_arity = incoming_field_reps.len(),
    )]
    DescriptorShape {
        identity: Box<tidepool_repr::execution_schema::SymbolIdentity>,
        existing_field_reps: Vec<RuntimeRep>,
        incoming_field_reps: Vec<RuntimeRep>,
    },
}

pub(crate) struct CompiledEntry {
    #[cfg(test)]
    pub function: FuncId,
    pub adapter: FuncId,
    pub abi: EntryAbi,
}

#[derive(Clone)]
pub(crate) struct ConstructorObservation {
    pub identity: DataConId,
    pub fields: Vec<RuntimeRep>,
}

/// The program-owned descriptor index supplies observation identity. Generated
/// entry dispatch uses the same descriptors, whose code remains pinned by the
/// compiled pipeline and is never exposed as a Rust function pointer.
/// Constructor metadata is authoritative independent of family-relative tags.
#[derive(Clone)]
pub(crate) struct DescriptorMetadata {
    pub descriptor: Arc<ObjectDescriptor>,
    pub meaning: DescriptorMeaning,
}

/// A machine-wide union of every installed program's registry -- see
/// `PreparedMachine`'s `descriptor_registry` field -- clones each entry
/// (cheap: an `Arc` clone plus small owned metadata) rather than borrowing,
/// since it must outlive any one program's own registry.
#[derive(Clone)]
pub(crate) enum DescriptorMeaning {
    External,
    Constructor(ConstructorObservation),
    Callable {
        #[cfg(test)]
        binding: ValueId,
    },
    Pap,
}

/// A prepared case matched none of its alternatives. `scrutinee` is the
/// tagged reference of an algebraic case (0 for any other case kind); `owner`
/// names the compiled value. A scrutinee whose header is a known constructor
/// is an intact object the case did not expect: the dispatch read it and
/// wrote nothing, so the call fails as a reusable `CaseMiss` and unwinding
/// restores thunk headers as for any language failure. Anything else is an
/// integrity failure. Returns the resulting entry status.
unsafe extern "C" fn prepared_case_trap(
    vmctx: *mut crate::context::VMContext,
    scrutinee: u64,
    owner: u64,
) -> i32 {
    let machine = unsafe { crate::machine_state::machine_state(vmctx) };
    let object = scrutinee & !7;
    let constructor = if object < crate::host_fns::MIN_VALID_ADDR {
        None
    } else {
        // SAFETY: generated code passes a nonzero scrutinee only for an
        // algebraic case, whose value is a managed reference in this call.
        let header = unsafe { std::ptr::read(object as *const usize) };
        machine.prepared_constructor_at(header)
    };
    machine.set_first_cause(match constructor {
        Some(constructor) => crate::host_fns::RuntimeError::CaseMiss { constructor, owner },
        None => crate::host_fns::RuntimeError::CaseTrap,
    });
    machine.prepared_call_status() as i32
}

unsafe extern "C" fn prepared_bad_state(vmctx: *mut crate::context::VMContext) -> i32 {
    let machine = unsafe { crate::machine_state::machine_state(vmctx) };
    machine.set_first_cause(crate::host_fns::RuntimeError::BadThunkState(0));
    machine.prepared_call_status() as i32
}

unsafe extern "C" fn prepared_blackhole(vmctx: *mut crate::context::VMContext) -> i32 {
    let machine = unsafe { crate::machine_state::machine_state(vmctx) };
    machine.set_first_cause(crate::host_fns::RuntimeError::BlackHole);
    machine.prepared_call_status() as i32
}

/// Non-mutating application probe. `demand` points to a boxed signature
/// owned by the calling CompiledProgram, which outlives its native frames.
/// Neither a hit nor a miss changes the machine's failure state.
unsafe extern "C" fn prepared_resolve_call(
    vmctx: *mut crate::context::VMContext,
    header: u64,
    demand: *const Signature,
    logical_cursor: u64,
    plan_out: *mut u64,
) -> u64 {
    let machine = unsafe { crate::machine_state::machine_state(vmctx) };
    let masked = (header as usize) & !7;
    let Some(resolution) =
        machine.resolve_prepared_application(masked, unsafe { &*demand }, logical_cursor as usize)
    else {
        return 0;
    };
    let kind = match resolution.continuation {
        crate::machine_state::PreparedCallContinuation::Return => apply::RESOLUTION_RETURN,
        crate::machine_state::PreparedCallContinuation::Terminal => apply::RESOLUTION_TERMINAL,
        crate::machine_state::PreparedCallContinuation::Apply => apply::RESOLUTION_APPLY,
    };
    unsafe {
        plan_out.write(resolution.logical_consumed as u64);
        plan_out.add(1).write(resolution.physical_consumed as u64);
        plan_out.add(2).write(kind);
        plan_out.add(3).write(resolution.environment as u64);
    }
    resolution.code as u64
}

/// Report an exhausted application search exactly once. `object` is the
/// untagged callee whose header the dispatcher just read.
unsafe extern "C" fn prepared_unresolved_call(
    vmctx: *mut crate::context::VMContext,
    object: u64,
) -> i32 {
    let machine = unsafe { crate::machine_state::machine_state(vmctx) };
    let header = unsafe { (object as *const usize).read() } & !7;
    // A live object of this machine is a reusable miss (incompatible
    // demand); anything else does not name an object at all.
    let known = machine.owns_prepared_entry(header)
        || unsafe { machine.prepared_constructor_tag(object as usize) }.is_ok();
    let cause = if known {
        crate::host_fns::RuntimeError::UnresolvedCallee
    } else {
        crate::host_fns::RuntimeError::BadThunkState(0)
    };
    machine.set_first_cause(cause);
    machine.prepared_call_status() as i32
}

/// `prepared_resolve_enter`'s answer for an object that is already a value:
/// the caller returns the reference unchanged. Never a code address.
pub(crate) const ENTER_EVALUATED: u64 = 1;

/// Resolve how to enter an untagged object `entry.rs`'s thunk chain does not
/// know. Foreign thunks resolve to their owner's `prepared_enter`; functions,
/// PAPs, and constructors are already values and are recognized through the
/// descriptor space without a generated header enumeration.
unsafe extern "C" fn prepared_resolve_enter(
    vmctx: *mut crate::context::VMContext,
    object: u64,
    environment_out: *mut u64,
) -> u64 {
    let machine = unsafe { crate::machine_state::machine_state(vmctx) };
    let header = unsafe { (object as *const usize).read() } & !7;
    if let Some((code, environment)) = machine.resolve_prepared_enter(header) {
        unsafe { environment_out.write(environment as u64) };
        return code as u64;
    }
    if matches!(
        unsafe { machine.prepared_object_kind(object as usize) },
        Ok(tidepool_heap::execution_descriptor::ObjectKind::Function
            | tidepool_heap::execution_descriptor::ObjectKind::Pap
            | tidepool_heap::execution_descriptor::ObjectKind::Constructor)
    ) {
        return ENTER_EVALUATED;
    }
    machine.set_first_cause(crate::host_fns::RuntimeError::BadThunkState(0));
    0
}

/// Status-only failure return for a site whose cause was already recorded
/// by the host fn that decided it (`prepared_resolve_call`/`_enter`).
/// Recording a second cause there would be wrong twice over: the first
/// cause is what the caller must see, and a `BadThunkState` after an
/// `UnresolvedCallee` would latch the machine Unavailable for a failure
/// that touched nothing.
unsafe extern "C" fn prepared_recorded_failure(vmctx: *mut crate::context::VMContext) -> i32 {
    let machine = unsafe { crate::machine_state::machine_state(vmctx) };
    machine.prepared_call_status() as i32
}

static NEXT_IMAGE_INSTANCE: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);

/// Pins generated entries, descriptors and immutable images together. Each run
/// owns its mutable heap; materialization may force values before releasing it.
pub struct CompiledProgram {
    image_instance: u64,
    definition_facts: Arc<DefinitionFacts>,
    pub(crate) pipeline: CodegenPipeline,
    pub(crate) entries: BTreeMap<ValueId, CompiledEntry>,
    pub(crate) descriptors: Vec<Arc<ObjectDescriptor>>,
    /// Owns the immutable role binding embedded by this image's JSON intrinsics.
    _json_layout: Option<Box<json::BoundJsonLayout>>,
    pub(crate) descriptor_registry: BTreeMap<usize, DescriptorMetadata>,
    pub(crate) descriptor_slots: BTreeMap<usize, usize>,
    pub(crate) statics: StaticImage,
    /// Block-local slot of every top.
    pub(crate) top_slots: BTreeMap<ValueId, usize>,
    /// Immutable source identity and runtime contract for each top. Batch
    /// installation uses this to authenticate exact inter-group imports
    /// before allocating any mutable installation state.
    pub(crate) top_exports: BTreeMap<ValueId, TopExport>,
    /// Original owner of a worker-certified group image. A target/ordinary
    /// prepared image has no source certificate and cannot mint source leases.
    pub(crate) certified_source: Option<(tidepool_repr::execution_schema::CachedHomeOwner, u32)>,
    /// Only an independent original Bytes group can produce a source literal.
    pub(crate) source_literal_producer: Option<(ValueId, package_literals::SourceLiteralOwner)>,
    /// Admitted imports' slots -- see [`plan::ImportSlot`]. Indexed by
    /// `GlobalId`, occupying the block range right after `top_slots`.
    pub(crate) import_slots: Vec<plan::ImportSlot>,
    /// Block length (one word per top and import slot). Each installation
    /// allocates its own block; only reference slots are collector roots.
    pub(crate) root_words: usize,
    /// This program's constructor declarations with the descriptors they
    /// compiled against, so an installing machine can absorb them into its
    /// [`DescriptorInterner`] and later programs share them.
    pub(crate) interned_constructors: Vec<(
        tidepool_repr::execution_schema::ConstructorDecl,
        Arc<ObjectDescriptor>,
    )>,
    pub(crate) byte_tops: BTreeMap<ValueId, Arc<[u8]>>,
    /// Own every address embedded in generated code, including scalar literals
    /// with no top-level Bytes binding. Keys are logical bytes; values are the
    /// exact allocations whose addresses the emitter used.
    pub(crate) bytes: Arc<static_bytes::PinnedBytes>,
    /// The process-wide external wrapper descriptors this program compiled
    /// against (also in `descriptors`); every program in the process mints
    /// the same three from `ExternalDescriptors::shared`, so any installing
    /// machine's cache already agrees with what it carries.
    pub(crate) externals: ExternalDescriptors,
    pub(crate) heap_top_specs: Vec<plan::HeapTopSpec>,
    /// Platform C-ABI adapter `(vmctx, result_out, managed_ref) -> status`.
    /// The target is generated code which calls Tail `prepared_enter`.
    pub(crate) force_adapter: FuncId,
    /// Every function this program exports as a cross-program call target.
    /// `PreparedMachine::install` registers these in the machine's
    /// resolution table, which `apply::emit_dispatchers`' fallback queries
    /// through `prepared_resolve_call`.
    pub(crate) callables: Vec<resolve::CallableExport>,
    /// Pins every demand address embedded in the generated resolver calls.
    _dispatchers: apply::Dispatchers,
    /// Exact per-thunk entry state machines, keyed by descriptor header.
    pub(crate) thunk_entries: Vec<(usize, FuncId)>,
    /// Set exactly once, by whichever install (on whichever machine, and
    /// therefore possibly whichever thread) is first to install this image:
    /// see [`crate::prepared_program::machine::PreparedMachine::install`]'s
    /// use of [`Self::charge_codegen_once`]. An `ImageRegistry` hit and every
    /// later `install_shared` of the same `Arc<CompiledProgram>` sees this
    /// already `true` and charges nothing.
    codegen_charged: std::sync::atomic::AtomicBool,
}

#[derive(Clone)]
pub(crate) struct TopExport {
    pub identity: tidepool_repr::execution_schema::SymbolIdentity,
    pub rep: RuntimeRep,
    pub entry_signature: Option<Signature>,
    pub evaluated: bool,
}

// SAFETY: every field above is written only inside `compile_with`, which
// completes and hands back an owned `CompiledProgram` before any install
// can see it; nothing an install or a later `run_entry*` does mutates a
// field of this struct in place. What each field's post-compile life looks
// like:
//   - `pipeline` (`CodegenPipeline`, holding `OwnedJitModule`/`JITModule`):
//     `finalize()` runs inside `compile_with`, before `Self { .. }` is
//     built. After that, every accessor this crate calls on it
//     (`get_function_ptr`, `native_frame_maximum`, `functions_defined`,
//     `code_bytes`, `stack_maps`) takes `&self` and reads already-finalized
//     tables (`compiled_functions: SecondaryMap`, plain counters). Cranelift
//     JIT's `JITModule` itself holds a `RefCell<HashMap<..>>` symbol-lookup
//     cache, but that cache is populated only from `get_address`, which
//     `finalize_definitions` alone calls (during `compile_with`, under
//     `&mut self`) -- no post-compile accessor this crate uses reaches it,
//     so the `RefCell` is never borrowed again once `compile_with` returns.
//     Its `memory: Box<dyn JITMemoryProvider + Send>` is likewise touched
//     only by `finalize`/`free_memory`, the latter running from `Drop`,
//     which -- like any `Arc<CompiledProgram>` drop -- happens on whichever
//     thread releases the last reference, never concurrently with another
//     reference's use. `lookup_symbols`/`declarations`/`compiled_functions`/
//     `compiled_data_objects`/`code_ranges` are likewise read-only once
//     `finalize_definitions` returns: no accessor this crate calls after
//     `compile_with` mutates them, and `get_finalized_function`'s raw code
//     pointer is a stable address into memory this program's `Drop` alone
//     frees.
//   - `definition_facts`, `descriptors`, `_json_layout`, `descriptor_registry`, `statics`, `top_slots`,
//     `import_slots`, `interned_constructors`, `byte_tops`, `externals`,
//     `heap_top_specs`, `callables`, `thunk_entries`:
//     plain owned data (`Vec`/`BTreeMap`/`Arc<..>` of `Send + Sync` content,
//     no `Cell`/`RefCell`/raw pointer), read-only after construction.
//   - `bytes` (`Arc<static_bytes::PinnedBytes>`): content-addressed,
//     append-only by construction; this program's own `Arc` is never
//     mutated after `compile_with` returns (only a machine's OWN pool,
//     a different value, is later extended by absorption).
//   - `root_words`, `force_adapter` (`FuncId`): `Copy` plain
//     data.
//   - `codegen_charged`: an `AtomicBool`, synchronized by construction.
// No field is a raw pointer, `Cell`, or non-atomic interior-mutability cell
// reachable from more than one thread after `compile_with` returns.
unsafe impl Send for CompiledProgram {}
unsafe impl Sync for CompiledProgram {}

static_assertions::assert_impl_all!(CompiledProgram: Send, Sync);

impl CompiledProgram {
    /// Process-local observation identity; never an image key or authority.
    pub fn image_instance_id(&self) -> u64 {
        self.image_instance
    }

    /// Shared immutable evidence for every installation of this image.
    pub fn definition_facts(&self) -> &Arc<DefinitionFacts> {
        &self.definition_facts
    }

    /// Collector roots in an installation's block, classified by the admitted
    /// runtime representation. Byte tops and Address imports contain literal
    /// pool addresses, whose lifetime belongs to the machine's permanent pool.
    fn reference_slots(&self) -> impl Iterator<Item = usize> + '_ {
        self.top_slots
            .iter()
            .filter_map(|(id, slot)| {
                matches!(
                    self.top_exports[id].rep,
                    RuntimeRep::LiftedRef | RuntimeRep::UnliftedRef
                )
                .then_some(*slot)
            })
            .chain(self.import_slots.iter().filter_map(|slot| {
                matches!(slot.rep, RuntimeRep::LiftedRef | RuntimeRep::UnliftedRef)
                    .then_some(slot.slot)
            }))
    }

    /// Compile with a fresh descriptor interner: a standalone program, or the
    /// first program of a machine (whose descriptors the machine absorbs at
    /// install). Later programs on a machine compile through
    /// `PreparedMachine::compile_for_install` so they share descriptors.
    pub fn compile(linked: &LinkedProgram) -> Result<Self, CompileError> {
        Self::compile_with(
            linked,
            &mut DescriptorInterner::default(),
            &std::sync::Arc::new(static_bytes::PinnedBytes::empty()),
        )
    }

    /// Compile one worker-certified, entry-free recursive group. Its source
    /// owner and import provenance were sealed before native compilation;
    /// machine-local import handles are supplied only when the image installs.
    pub fn compile_certified_group(group: &CertifiedGroup) -> Result<Self, CompileError> {
        Self::compile_certified_group_with_literals(
            group,
            &package_literals::GroupPackageLiterals::default(),
        )
    }

    fn compile_certified_group_with_literals(
        group: &CertifiedGroup,
        literals: &package_literals::GroupPackageLiterals,
    ) -> Result<Self, CompileError> {
        let mut image = Self::compile_definitions_with_literals(
            group.definitions(),
            &mut DescriptorInterner::default(),
            &Arc::new(static_bytes::PinnedBytes::empty()),
            literals,
        )
        .map_err(|mut error| {
            if let CompileError::Unsupported(Unsupported::Global(refusal)) = &mut error {
                refusal.source_group = Some((group.owner().clone(), group.original_ordinal()));
            }
            error
        })?;
        image.certified_source = Some((group.owner().clone(), group.original_ordinal()));
        image.source_literal_producer = package_literals::SourceLiteralOwner::from_group(group);
        Ok(image)
    }

    /// Compile a target's validated definitions off machine checkout. Its
    /// exact global owners and live handles are checked when the target and
    /// its reachable source groups install as one batch.
    pub fn compile_prepared_definitions(prepared: &PreparedProgram) -> Result<Self, CompileError> {
        Self::compile_definitions(
            prepared.definitions(),
            &mut DescriptorInterner::default(),
            &Arc::new(static_bytes::PinnedBytes::empty()),
        )
    }

    /// Specialize target imports only with storage from actual compiled original
    /// singleton Bytes groups. The target's resolved owners bind each literal's
    /// original binder and version; machine batch admission rechecks its producer.
    pub fn compile_prepared_with_source_literals(
        prepared: &PreparedProgram,
        owners: &[tidepool_repr::execution_schema::ImportOwner],
        sources: &BTreeMap<SourceBinder, SourceLiteral>,
        registry: &ImageRegistry,
    ) -> Result<Arc<Self>, CompileError> {
        let literals = package_literals::GroupPackageLiterals::select_definitions(
            &prepared.definitions(),
            owners,
            &BTreeMap::new(),
            sources,
        )?;
        registry.get_or_compile_literal_prepared(prepared, &literals, || {
            Self::compile_definitions_with_literals(
                prepared.definitions(),
                &mut DescriptorInterner::default(),
                &Arc::new(static_bytes::PinnedBytes::empty()),
                &literals,
            )
            .map(Arc::new)
        })
    }

    /// [`Self::compile`] against `interner`: constructor identities already
    /// interned reuse their descriptor, so this program's `Case`, enter and
    /// observation recognise objects an earlier program built. Likewise
    /// against `existing_bytes` (see [`crate::machine_state::MachineState::interned_bytes`]
    /// for a machine's live pool, or a fresh [`static_bytes::PinnedBytes::empty`]
    /// for a standalone compile): literal content it already carries
    /// resolves to the SAME address here, so `Case`, enter and observation
    /// -- and every `Addr#` host primitive -- recognise a literal an
    /// earlier program interned. This program's own newly minted literals
    /// join `existing_bytes` in the returned [`CompiledProgram::bytes`];
    /// the installing machine folds them into its permanent pool (see
    /// `PreparedMachine::install`).
    pub(crate) fn compile_with(
        linked: &LinkedProgram,
        interner: &mut DescriptorInterner,
        existing_bytes: &std::sync::Arc<static_bytes::PinnedBytes>,
    ) -> Result<Self, CompileError> {
        Self::compile_definitions(linked.prepared().definitions(), interner, existing_bytes)
    }

    fn compile_definitions(
        definitions: DefinitionsView<'_>,
        interner: &mut DescriptorInterner,
        existing_bytes: &Arc<static_bytes::PinnedBytes>,
    ) -> Result<Self, CompileError> {
        Self::compile_definitions_with_literals(
            definitions,
            interner,
            existing_bytes,
            &package_literals::GroupPackageLiterals::default(),
        )
    }

    fn compile_definitions_with_literals(
        definitions: DefinitionsView<'_>,
        interner: &mut DescriptorInterner,
        existing_bytes: &Arc<static_bytes::PinnedBytes>,
        literals: &package_literals::GroupPackageLiterals,
    ) -> Result<Self, CompileError> {
        let target = &definitions.envelope().target;
        let host_matches = cfg!(all(target_os = "linux", target_arch = "x86_64"))
            && target.architecture == Architecture::X86_64;
        if !host_matches
            || target.endianness != Endianness::Little
            || target.pointer_width != 64
            || target.word_width != 64
            || !matches!(target.abi.as_str(), "sysv64" | "system-v")
        {
            return Err(Unsupported::Target(target.clone()).into());
        }
        let mut clock = compile_phases::PhaseClock::start();
        let mut phases = compile_phases::CompilePhases::default();
        let mut native_metrics = compile_phases::NativeMetrics::new(&definitions);
        admission::admit_definitions_with_literals(&definitions, literals)?;
        phases.admit = clock.lap();
        use crate::entry_abi::{EnvironmentMode, NativeAbiProfile};
        use cranelift_codegen::isa::CallConv;
        use cranelift_module::Linkage;
        use tidepool_repr::execution_schema::{HeapRhs, RuntimeRep};
        let plan = plan::ProgramPlan::new(definitions, interner, existing_bytes, literals)?;
        let profile = NativeAbiProfile::new(plan.program.envelope().target.clone(), 0)?;
        phases.plan = clock.lap();
        let statics = image::build_static_image(&plan)?;
        phases.static_image = clock.lap();
        let mut pipeline = CodegenPipeline::new(
            &[
                (
                    "prepared_gc_trigger",
                    crate::host_fns::prepared_gc_trigger as *const u8,
                ),
                ("write_barrier", crate::host_fns::write_barrier as *const u8),
                ("prepared_poll", safepoint::prepared_poll_at as *const u8),
                (
                    "prepared_stack_overflow",
                    safepoint::prepared_stack_overflow as *const u8,
                ),
                ("prepared_case_trap", prepared_case_trap as *const u8),
                ("prepared_bad_state", prepared_bad_state as *const u8),
                ("prepared_blackhole", prepared_blackhole as *const u8),
                ("prepared_resolve_call", prepared_resolve_call as *const u8),
                (
                    "prepared_unresolved_call",
                    prepared_unresolved_call as *const u8,
                ),
                (
                    "prepared_resolve_enter",
                    prepared_resolve_enter as *const u8,
                ),
                (
                    "prepared_recorded_failure",
                    prepared_recorded_failure as *const u8,
                ),
                ("prepared_raise", no_success::raise as *const u8),
                ("prepared_keep_alive", lifetime::keep_alive as *const u8),
                (
                    "prepared_byte_array_contents",
                    byte_arrays::prepared_byte_array_contents as *const u8,
                ),
                (
                    "prepared_wired_in_error",
                    failures::prepared_wired_in_error as *const u8,
                ),
                (
                    "prepared_unsupported_capability",
                    capabilities::unsupported as *const u8,
                ),
                (
                    "prepared_new_boxed",
                    arrays::prepared_new_boxed as *const u8,
                ),
                (
                    "prepared_read_boxed",
                    arrays::prepared_read_boxed as *const u8,
                ),
                (
                    "prepared_write_boxed",
                    arrays::prepared_write_boxed as *const u8,
                ),
                (
                    "prepared_sizeof_boxed",
                    arrays::prepared_sizeof_boxed as *const u8,
                ),
                (
                    "prepared_freeze_boxed",
                    arrays::prepared_freeze_boxed as *const u8,
                ),
                (
                    "prepared_shrink_boxed",
                    arrays::prepared_shrink_boxed as *const u8,
                ),
                (
                    "prepared_copy_boxed",
                    arrays::prepared_copy_boxed as *const u8,
                ),
                (
                    "prepared_cas_boxed",
                    arrays::prepared_cas_boxed as *const u8,
                ),
                (
                    "prepared_new_bytes",
                    byte_arrays::prepared_new_bytes as *const u8,
                ),
                (
                    "prepared_new_aligned_bytes",
                    byte_arrays::prepared_new_aligned_bytes as *const u8,
                ),
                (
                    "prepared_resize_bytes",
                    byte_arrays::prepared_resize_bytes as *const u8,
                ),
                (
                    "prepared_freeze_bytes",
                    byte_arrays::prepared_freeze_bytes as *const u8,
                ),
                (
                    "prepared_sizeof_bytes",
                    byte_arrays::prepared_sizeof_bytes as *const u8,
                ),
                (
                    "prepared_shrink_bytes",
                    byte_arrays::prepared_shrink_bytes as *const u8,
                ),
                (
                    "prepared_copy_bytes",
                    byte_arrays::prepared_copy_bytes as *const u8,
                ),
                (
                    "prepared_copy_mutable_bytes",
                    byte_arrays::prepared_copy_mutable_bytes as *const u8,
                ),
                (
                    "prepared_set_bytes",
                    byte_arrays::prepared_set_bytes as *const u8,
                ),
                (
                    "prepared_compare_bytes",
                    byte_arrays::prepared_compare_bytes as *const u8,
                ),
                (
                    "prepared_read_word8_bytes",
                    byte_arrays::prepared_read_word8_bytes as *const u8,
                ),
                (
                    "prepared_read_int_bytes",
                    byte_arrays::prepared_read_int_bytes as *const u8,
                ),
                (
                    "prepared_write_word8_bytes",
                    byte_arrays::prepared_write_word8_bytes as *const u8,
                ),
                (
                    "prepared_write_int_bytes",
                    byte_arrays::prepared_write_int_bytes as *const u8,
                ),
                (
                    "prepared_render_double_bytes",
                    formatting::prepared_render_double_bytes as *const u8,
                ),
                (
                    "prepared_render_double_prec_bytes",
                    formatting::prepared_render_double_prec_bytes as *const u8,
                ),
                (
                    time::PARSE_ISO8601_HOST,
                    time::prepared_parse_iso8601 as *const u8,
                ),
                (
                    json::PARSE_JSON_HOST,
                    json::prepared_parse_json as *const u8,
                ),
                (
                    json::ENCODE_JSON_HOST,
                    json::prepared_encode_json as *const u8,
                ),
                (
                    "prepared_no_success_returned",
                    no_success::unexpected_success as *const u8,
                ),
                (
                    "prepared_primitive_failure",
                    fallible::prepared_primitive_failure as *const u8,
                ),
                (
                    "prepared_quot_rem_word2",
                    wide_words::prepared_quot_rem_word2 as *const u8,
                ),
                (
                    "prepared_data_to_tag_small",
                    data_tag::prepared_data_to_tag_small as *const u8,
                ),
                (
                    floating::DECODE_DOUBLE_INT64_HOST,
                    floating::prepared_decode_double_int64 as *const u8,
                ),
                (
                    floating::DECODE_FLOAT_INT_HOST,
                    floating::prepared_decode_float_int as *const u8,
                ),
                (
                    floating::LIBM_HOST,
                    floating::prepared_float_libm as *const u8,
                ),
                (
                    floating::ENCODE_DOUBLE_INT_HOST,
                    floating::prepared_encode_double_int as *const u8,
                ),
                (
                    floating::ENCODE_DOUBLE_WORD_HOST,
                    floating::prepared_encode_double_word as *const u8,
                ),
                (
                    "prepared_index_char",
                    static_bytes::prepared_index_char as *const u8,
                ),
                (
                    "prepared_c_string_len",
                    static_bytes::prepared_c_string_len as *const u8,
                ),
                (
                    "prepared_copy_addr_to_byte_array",
                    static_bytes::prepared_copy_addr_to_byte_array as *const u8,
                ),
            ]
            .into_iter()
            .chain(addresses::host_functions())
            .chain(fingerprint::host_functions())
            .chain(text_search::host_functions())
            .collect::<Vec<_>>(),
        )?;
        phases.pipeline_init = clock.lap();
        #[cfg(test)]
        {
            pipeline.emitted_ir = Some(BTreeMap::new());
        }
        let mut prepared_gc_signature = ir::Signature::new(pipeline.isa.default_call_conv());
        prepared_gc_signature.params.push(AbiParam::new(types::I64));
        prepared_gc_signature.params.push(AbiParam::new(types::I64));
        prepared_gc_signature
            .returns
            .push(AbiParam::new(types::I32));
        let prepared_gc = pipeline
            .module
            .declare_function(
                "prepared_gc_trigger",
                Linkage::Import,
                &prepared_gc_signature,
            )
            .map_err(|error| PipelineError::Declaration(error.to_string()))?;
        let mut prepared_status_signature = ir::Signature::new(pipeline.isa.default_call_conv());
        prepared_status_signature
            .params
            .push(AbiParam::new(types::I64));
        prepared_status_signature
            .returns
            .push(AbiParam::new(types::I32));
        let mut prepared_poll_signature = prepared_status_signature.clone();
        prepared_poll_signature
            .params
            .push(AbiParam::new(types::I32));
        let prepared_poll = pipeline
            .module
            .declare_function("prepared_poll", Linkage::Import, &prepared_poll_signature)
            .map_err(|error| PipelineError::Declaration(error.to_string()))?;
        let prepared_stack_overflow = pipeline
            .module
            .declare_function(
                "prepared_stack_overflow",
                Linkage::Import,
                &prepared_status_signature,
            )
            .map_err(|error| PipelineError::Declaration(error.to_string()))?;
        let mut case_trap_signature = ir::Signature::new(pipeline.isa.default_call_conv());
        case_trap_signature.params.push(AbiParam::new(types::I64));
        case_trap_signature.params.push(AbiParam::new(types::I64));
        case_trap_signature.params.push(AbiParam::new(types::I64));
        case_trap_signature.returns.push(AbiParam::new(types::I32));
        let case_trap = pipeline
            .module
            .declare_function("prepared_case_trap", Linkage::Import, &case_trap_signature)
            .map_err(|error| PipelineError::Declaration(error.to_string()))?;
        let prepared_bad_state = pipeline
            .module
            .declare_function(
                "prepared_bad_state",
                Linkage::Import,
                &prepared_status_signature,
            )
            .map_err(|error| PipelineError::Declaration(error.to_string()))?;
        let prepared_blackhole = pipeline
            .module
            .declare_function(
                "prepared_blackhole",
                Linkage::Import,
                &prepared_status_signature,
            )
            .map_err(|error| PipelineError::Declaration(error.to_string()))?;
        let mut prepared_resolve_call_signature =
            ir::Signature::new(pipeline.isa.default_call_conv());
        prepared_resolve_call_signature
            .params
            .push(AbiParam::new(types::I64));
        prepared_resolve_call_signature
            .params
            .push(AbiParam::new(types::I64));
        prepared_resolve_call_signature
            .params
            .push(AbiParam::new(types::I64));
        prepared_resolve_call_signature
            .params
            .push(AbiParam::new(types::I64));
        prepared_resolve_call_signature
            .params
            .push(AbiParam::new(types::I64));
        prepared_resolve_call_signature
            .returns
            .push(AbiParam::new(types::I64));
        // `entry::emit_prepared_enter` calls `prepared_resolve_enter`
        // (declared below); `apply::emit_dispatchers`' fallback call site
        // calls this one.
        let prepared_resolve_call = pipeline
            .module
            .declare_function(
                "prepared_resolve_call",
                Linkage::Import,
                &prepared_resolve_call_signature,
            )
            .map_err(|error| PipelineError::Declaration(error.to_string()))?;
        let mut prepared_resolve_enter_signature =
            ir::Signature::new(pipeline.isa.default_call_conv());
        prepared_resolve_enter_signature
            .params
            .push(AbiParam::new(types::I64));
        prepared_resolve_enter_signature
            .params
            .push(AbiParam::new(types::I64));
        prepared_resolve_enter_signature
            .params
            .push(AbiParam::new(types::I64));
        prepared_resolve_enter_signature
            .returns
            .push(AbiParam::new(types::I64));
        let prepared_resolve_enter = pipeline
            .module
            .declare_function(
                "prepared_resolve_enter",
                Linkage::Import,
                &prepared_resolve_enter_signature,
            )
            .map_err(|error| PipelineError::Declaration(error.to_string()))?;
        let prepared_recorded_failure = pipeline
            .module
            .declare_function(
                "prepared_recorded_failure",
                Linkage::Import,
                &prepared_status_signature,
            )
            .map_err(|error| PipelineError::Declaration(error.to_string()))?;
        let mut unresolved_signature = prepared_status_signature.clone();
        unresolved_signature.params.push(AbiParam::new(types::I64));
        let prepared_unresolved_call = pipeline
            .module
            .declare_function(
                "prepared_unresolved_call",
                Linkage::Import,
                &unresolved_signature,
            )
            .map_err(|error| PipelineError::Declaration(error.to_string()))?;
        let mut write_barrier_signature = ir::Signature::new(pipeline.isa.default_call_conv());
        write_barrier_signature
            .params
            .push(AbiParam::new(types::I64));
        write_barrier_signature
            .params
            .push(AbiParam::new(types::I64));
        let write_barrier = pipeline
            .module
            .declare_function("write_barrier", Linkage::Import, &write_barrier_signature)
            .map_err(|error| PipelineError::Declaration(error.to_string()))?;
        let mut signatures = BTreeMap::new();
        for (&id, function) in &plan.functions {
            signatures.insert(id, function.signature.clone());
        }
        for (&id, thunk) in &plan.thunks {
            signatures.insert(id, thunk.signature.clone());
        }
        for (&id, binding) in &plan.top_bindings {
            signatures.entry(id).or_insert_with(|| Signature {
                arguments: vec![],
                results: ResultContract::Returns(vec![match &binding.rhs {
                    HeapRhs::Bytes(_) => RuntimeRep::Address,
                    HeapRhs::Constructor { constructor, .. } => {
                        plan.program.constructors()[constructor.0 as usize].result_rep
                    }
                    _ => RuntimeRep::LiftedRef,
                }]),
            });
        }
        let mut thunk_bodies = BTreeMap::new();
        let thunk_native_signature = entry::signature();
        for (&id, thunk) in &plan.thunks {
            let body_abi =
                EntryAbi::lower_internal(&profile, thunk.signature, EnvironmentMode::Captured)?;
            let body_signature =
                body_abi.cranelift_signature(&profile, cranelift_codegen::isa::CallConv::Tail)?;
            let body = pipeline.declare_function_with_signature(
                &format!("prepared_thunk_body_{}", id.0),
                Linkage::Local,
                &body_signature,
            )?;
            thunk_bodies.insert(id, body);
        }
        let prepared_enter = pipeline.declare_function_with_signature(
            "prepared_enter",
            Linkage::Local,
            &thunk_native_signature,
        )?;
        // The call map names the callable view of each binding. Thunk bodies
        // are deliberately kept separate: Enter sites must route through the
        // state machine, while its body call uses the private body ID.
        let mut functions = BTreeMap::new();
        let mut abis = BTreeMap::new();
        let result_instances = plan::result_instances(&plan.program);
        for (&id, signature) in &signatures {
            let results = if signature.results.is_caller_result() {
                result_instances.iter().cloned().collect::<Vec<_>>()
            } else {
                vec![signature.results.clone()]
            };
            for (instance, results) in results.into_iter().enumerate() {
                let concrete = Signature {
                    arguments: signature.arguments.clone(),
                    results: results.clone(),
                };
                let abi = EntryAbi::lower_internal(&profile, &concrete, EnvironmentMode::Captured)?;
                let function = if plan.thunks.contains_key(&id) {
                    prepared_enter
                } else {
                    let native = abi.cranelift_signature(&profile, CallConv::Tail)?;
                    pipeline.declare_function_with_signature(
                        &format!("prepared_entry_{}_{}", id.0, instance),
                        Linkage::Local,
                        &native,
                    )?
                };
                functions
                    .entry(id)
                    .or_insert_with(BTreeMap::new)
                    .insert(results.clone(), function);
                abis.insert((id, results), abi);
            }
        }
        let dispatchers = apply::declare_dispatchers(&plan, &profile, &mut pipeline)?;
        phases.declare = clock.lap();
        let mut category_start = compile_phases::NativeCounts::read(&pipeline);
        let mut callables = apply::emit_dispatchers(
            &plan,
            &dispatchers,
            &functions,
            &profile,
            prepared_gc,
            prepared_poll,
            prepared_stack_overflow,
            prepared_enter,
            prepared_bad_state,
            prepared_resolve_call,
            prepared_unresolved_call,
            &mut pipeline,
        )?;
        for (index, callable) in callables.iter_mut().enumerate() {
            callable.function = adapter::emit_dynamic_adapter(
                &mut pipeline,
                &format!("prepared_dynamic_adapter_{index}"),
                callable.function,
                &callable.signature,
                &profile,
            )?;
        }
        native_metrics.category("dispatchers", category_start, &pipeline);
        phases.emit_dispatchers = clock.lap();
        category_start = compile_phases::NativeCounts::read(&pipeline);
        // Every function address has been declared, including recursive peers.
        for (&id, instances) in &functions {
            if plan.thunks.contains_key(&id) {
                continue;
            }
            for (results, &output) in instances {
                let definition_start = compile_phases::NativeCounts::read(&pipeline);
                emit::emit_function(
                    &plan,
                    id,
                    output,
                    results,
                    &functions,
                    &dispatchers,
                    prepared_gc,
                    prepared_poll,
                    prepared_stack_overflow,
                    prepared_enter,
                    case_trap,
                    &mut pipeline,
                )?;
                native_metrics.definition("function", id, definition_start, &pipeline);
            }
        }
        native_metrics.category("functions", category_start, &pipeline);
        phases.emit_functions = clock.lap();
        category_start = compile_phases::NativeCounts::read(&pipeline);
        for (&id, &body) in &thunk_bodies {
            let definition_start = compile_phases::NativeCounts::read(&pipeline);
            emit::emit_thunk_body(
                &plan,
                id,
                body,
                &functions,
                &dispatchers,
                prepared_gc,
                prepared_poll,
                prepared_stack_overflow,
                prepared_enter,
                case_trap,
                &mut pipeline,
            )?;
            native_metrics.definition("thunk", id, definition_start, &pipeline);
        }
        native_metrics.category("thunks", category_start, &pipeline);
        phases.emit_thunks = clock.lap();
        category_start = compile_phases::NativeCounts::read(&pipeline);
        let thunk_metadata = plan
            .thunks
            .iter()
            .map(|(&id, thunk)| entry::ThunkEntry {
                descriptor: Arc::clone(&thunk.descriptor),
                descriptor_slot: plan.descriptor_slots[&thunk.descriptor.initial_header_word()],
                body: thunk_bodies[&id],
                policy: thunk.policy,
                results: thunk.signature.results.clone(),
            })
            .collect::<Vec<_>>();
        let thunk_entries = thunk_metadata
            .iter()
            .enumerate()
            .map(|(index, thunk)| {
                pipeline
                    .declare_function_with_signature(
                        &format!("prepared_thunk_enter_{index}"),
                        Linkage::Local,
                        &entry::signature(),
                    )
                    .map(|function| (thunk.descriptor.initial_header_word(), function))
            })
            .collect::<Result<Vec<_>, _>>()?;
        // Evaluated objects are classified by the machine's descriptor space;
        // only thunk state machines require owner-specific generated code.
        let enter_evaluated = Vec::new();
        entry::emit_prepared_enter(
            &mut pipeline,
            prepared_enter,
            prepared_enter,
            &[],
            &enter_evaluated,
            prepared_poll,
            prepared_stack_overflow,
            prepared_bad_state,
            prepared_blackhole,
            prepared_resolve_enter,
            prepared_recorded_failure,
            write_barrier,
        )?;
        for (thunk, &(_, function)) in thunk_metadata.iter().zip(&thunk_entries) {
            entry::emit_prepared_enter(
                &mut pipeline,
                function,
                prepared_enter,
                std::slice::from_ref(thunk),
                &enter_evaluated,
                prepared_poll,
                prepared_stack_overflow,
                prepared_bad_state,
                prepared_blackhole,
                prepared_resolve_enter,
                prepared_recorded_failure,
                write_barrier,
            )?;
        }
        native_metrics.category("enter", category_start, &pipeline);
        phases.emit_enter = clock.lap();
        category_start = compile_phases::NativeCounts::read(&pipeline);
        let force_adapter =
            adapter::emit_force_adapter(&mut pipeline, "prepared_force_adapter", prepared_enter)?;
        let mut entries = BTreeMap::new();
        for (&id, &slot) in &plan.top_slots {
            let signature = &signatures[&id];
            if signature.results.is_caller_result() {
                continue;
            }
            let key = (id, signature.results.clone());
            let abi = abis[&key].clone();
            if slot >= plan.root_words {
                return Err(CompileError::MissingRepresentation(id));
            }
            let adapter = adapter::emit_adapter(
                &mut pipeline,
                &format!("prepared_adapter_{}", id.0),
                if plan.thunks.contains_key(&id) {
                    prepared_enter
                } else {
                    functions[&id][&signature.results]
                },
                &abi,
                slot,
            )?;
            entries.insert(
                id,
                CompiledEntry {
                    #[cfg(test)]
                    function: functions[&id][&signature.results],
                    adapter,
                    abi,
                },
            );
        }
        native_metrics.category("adapters", category_start, &pipeline);
        phases.emit_adapters = clock.lap();
        pipeline.finalize()?;
        phases.finalize = clock.lap();
        let mut descriptors = plan.constructors.clone();
        descriptors.push(Arc::clone(&plan.boxed_array));
        descriptors.push(Arc::clone(&plan.mut_var));
        descriptors.push(Arc::clone(&plan.bytes_array));
        descriptors.extend(
            plan.functions
                .values()
                .map(|function| function.descriptor.clone()),
        );
        descriptors.extend(
            plan.thunks
                .values()
                .map(|thunk| Arc::clone(&thunk.descriptor)),
        );
        descriptors.extend(
            plan.pap_layouts
                .values()
                .map(|pap| Arc::clone(&pap.descriptor)),
        );
        let mut descriptor_registry = BTreeMap::new();
        descriptor_registry.insert(
            plan.mut_var.initial_header_word(),
            DescriptorMetadata {
                descriptor: Arc::clone(&plan.mut_var),
                meaning: DescriptorMeaning::External,
            },
        );
        descriptor_registry.insert(
            plan.boxed_array.initial_header_word(),
            DescriptorMetadata {
                descriptor: Arc::clone(&plan.boxed_array),
                meaning: DescriptorMeaning::External,
            },
        );
        descriptor_registry.insert(
            plan.bytes_array.initial_header_word(),
            DescriptorMetadata {
                descriptor: Arc::clone(&plan.bytes_array),
                meaning: DescriptorMeaning::External,
            },
        );
        for (declaration, descriptor) in plan.program.constructors().iter().zip(&plan.constructors)
        {
            descriptor_registry.insert(
                descriptor.initial_header_word(),
                DescriptorMetadata {
                    descriptor: Arc::clone(descriptor),
                    meaning: DescriptorMeaning::Constructor(ConstructorObservation {
                        identity: declaration.host_id,
                        fields: declaration.field_reps.clone(),
                    }),
                },
            );
        }
        for (&_id, function) in &plan.functions {
            descriptor_registry.insert(
                function.descriptor.initial_header_word(),
                DescriptorMetadata {
                    descriptor: Arc::clone(&function.descriptor),
                    meaning: DescriptorMeaning::Callable {
                        #[cfg(test)]
                        binding: _id,
                    },
                },
            );
        }
        for (&_id, thunk) in &plan.thunks {
            descriptor_registry.insert(
                thunk.descriptor.initial_header_word(),
                DescriptorMetadata {
                    descriptor: Arc::clone(&thunk.descriptor),
                    meaning: DescriptorMeaning::Callable {
                        #[cfg(test)]
                        binding: _id,
                    },
                },
            );
        }
        for pap in plan.pap_layouts.values() {
            descriptor_registry.insert(
                pap.descriptor.initial_header_word(),
                DescriptorMetadata {
                    descriptor: Arc::clone(&pap.descriptor),
                    meaning: DescriptorMeaning::Pap,
                },
            );
        }
        let byte_tops = plan
            .top_bindings
            .iter()
            .filter_map(|(&id, binding)| match &binding.rhs {
                HeapRhs::Bytes(bytes) => Some((id, bytes)),
                _ => None,
            })
            .map(|(id, bytes)| {
                plan.bytes
                    .get(bytes)
                    .cloned()
                    .map(|storage| (id, storage))
                    .ok_or(CompileError::MissingRepresentation(id))
            })
            .collect::<Result<BTreeMap<_, _>, _>>()?;
        phases.descriptors = clock.lap();
        native_metrics.report();
        let image_instance = NEXT_IMAGE_INSTANCE.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        compile_phases::record(
            image_instance,
            &phases,
            &compile_phases::CompileScale {
                plan_functions: plan.functions.len(),
                plan_thunks: plan.thunks.len(),
                tops: plan.top_slots.len(),
                constructors: plan.program.constructors().len(),
                imports: plan.import_slots.len(),
                functions_defined: pipeline.functions_defined(),
                blocks_emitted: pipeline.blocks_emitted(),
                code_bytes: pipeline.code_bytes(),
            },
        );
        Ok(Self {
            image_instance,
            definition_facts: Arc::new(DefinitionFacts::new(plan.program)),
            pipeline,
            entries,
            descriptors,
            _json_layout: plan.json_layout,
            descriptor_registry,
            descriptor_slots: plan.descriptor_slots,
            statics,
            top_slots: plan.top_slots,
            top_exports: plan
                .program
                .bindings()
                .iter()
                .flat_map(|group| match group {
                    tidepool_repr::execution_schema::Group::NonRecursive(top) => {
                        std::slice::from_ref(top)
                    }
                    tidepool_repr::execution_schema::Group::Recursive(tops) => tops.as_slice(),
                })
                .map(|top| {
                    (
                        top.binding.id,
                        TopExport {
                            identity: top.identity.clone(),
                            rep: plan.value_reps[&top.binding.id],
                            entry_signature: match &top.binding.rhs {
                                HeapRhs::Function { signature, .. }
                                | HeapRhs::Thunk { signature, .. } => {
                                    Some(plan.program.signatures()[signature.0 as usize].clone())
                                }
                                HeapRhs::Constructor { .. } | HeapRhs::Bytes(_) => None,
                            },
                            evaluated: !matches!(top.binding.rhs, HeapRhs::Thunk { .. }),
                        },
                    )
                })
                .collect(),
            certified_source: None,
            source_literal_producer: None,
            import_slots: plan.import_slots,
            root_words: plan.root_words,
            interned_constructors: plan.interned_constructors,
            byte_tops,
            bytes: plan.bytes,
            externals: plan.externals,
            heap_top_specs: plan.heap_top_specs,
            force_adapter,
            callables,
            _dispatchers: dispatchers,
            thunk_entries,
            codegen_charged: std::sync::atomic::AtomicBool::new(false),
        })
    }

    pub(crate) fn prepared_force_adapter(&self) -> FuncId {
        self.force_adapter
    }

    /// `true` exactly once across this image's lifetime, for whichever
    /// install (any machine, any thread) is first to call it: the caller
    /// that pays for the codegen this compile already did, in its own
    /// `compiled_functions`/`compiled_code_bytes` counters. Every later
    /// install of the same `Arc<CompiledProgram>` -- a registry hit, or a
    /// direct `install_shared` of an `Arc` another machine already
    /// installed -- sees `false` and adds nothing: the image itself, not
    /// the machine, is what remembers it was already paid for.
    pub(crate) fn charge_codegen_once(&self) -> bool {
        let already_charged = self
            .codegen_charged
            .swap(true, std::sync::atomic::Ordering::AcqRel);
        !already_charged
    }

    /// The words of this program's root block: its tops plus every admitted
    /// import's slot. Accounting class 3 for one installed program.
    #[must_use]
    pub fn root_block_words(&self) -> usize {
        self.root_words
    }
}

/// Lower a saturated, statically resolved call. Payload SSA values become
/// observable only in the success block and are explicitly marked for GC.
pub(crate) fn emit_direct_call(
    builder: &mut FunctionBuilder<'_>,
    pipeline: &mut CodegenPipeline,
    vmctx: SsaValue,
    callee: ir::FuncRef,
    arguments: &[SsaValue],
    results: &ResultContract,
) -> Result<Option<Vec<SsaValue>>, CompileError> {
    let call = builder.ins().call(callee, arguments);
    let returned = builder.inst_results(call).to_vec();
    emit_call_results(builder, pipeline, vmctx, &returned, results)
}

/// Shared status, terminal-return and result-rooting contract for direct and
/// resolved indirect calls. No caller may consume a payload before this check.
pub(crate) fn emit_call_results(
    builder: &mut FunctionBuilder<'_>,
    pipeline: &mut CodegenPipeline,
    vmctx: SsaValue,
    returned: &[SsaValue],
    results: &ResultContract,
) -> Result<Option<Vec<SsaValue>>, CompileError> {
    let success = builder.create_block();
    let failure = builder.create_block();
    let ok = builder
        .ins()
        .icmp_imm(ir::condcodes::IntCC::Equal, returned[0], 0);
    builder.ins().brif(ok, success, &[], failure, &[]);
    builder.switch_to_block(failure);
    builder.seal_block(failure);
    crate::alloc::emit_prepared_failure_return(builder, returned[0]);
    builder.switch_to_block(success);
    builder.seal_block(success);
    let ResultContract::Returns(results) = results else {
        no_success::emit_terminal(
            builder,
            pipeline,
            vmctx,
            no_success::TerminalCause::UnexpectedSuccess,
        )?;
        return Ok(None);
    };
    let payload = returned[1..].to_vec();
    for (&value, rep) in payload
        .iter()
        .zip(results.iter().filter(|rep| **rep != RuntimeRep::Void))
    {
        if matches!(rep, RuntimeRep::LiftedRef | RuntimeRep::UnliftedRef) {
            builder.declare_value_needs_stack_map(value);
        }
    }
    Ok(Some(payload))
}

#[cfg(test)]
mod entry_tests;
#[cfg(test)]
mod freer_boundary_tests;
#[cfg(test)]
mod tests;

impl Drop for CompiledProgram {
    fn drop(&mut self) {
        tracing::info!(target: "tidepool_codegen::image_lifetime", image_instance = self.image_instance, process_id = std::process::id(), outcome = "released", "native image lifetime");
    }
}
