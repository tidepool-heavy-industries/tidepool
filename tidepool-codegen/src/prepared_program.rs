//! Connected prepared entry ownership and native lowering.
//!
//! Compiled addresses/descriptors are program-owned; heap pointers and top
//! tables are invocation-owned. No invocation pointer is embedded in code.

use crate::entry_abi::EntryAbi;
mod addresses;
mod capabilities;
mod failures;
mod fingerprint;
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
    Architecture, Endianness, GlobalId, LinkedProgram, ResultContract, RuntimeRep, Signature,
    TargetDescriptor, ValueId,
};
use tidepool_repr::DataConId;

mod adapter;
mod admission;
mod apply;
mod emit;
mod image;
#[cfg(test)]
mod invocation;
mod machine;
pub(crate) mod md5_kernel;
mod no_success;
#[cfg(test)]
mod no_success_tests;
mod observe;
pub(crate) mod resolve;
mod roots;
pub use observe::ObservationFailure;
mod interner;
pub use interner::DescriptorInterner;
mod run;
pub use machine::{
    ImportBindings, PreparedCallOptions, PreparedHandle, PreparedInput, PreparedMachine,
    PreparedMachineOptions, PreparedOuter, PreparedResult, PreparedResultBatch, ProgramId,
};
pub use run::{ExecutionError, ImportShapeFact, RunOptions, RunResult};
#[cfg(test)]
mod apply_tests;
mod arrays;
mod byte_arrays;
#[cfg(test)]
mod bytes_tests;
mod data_tag;
#[cfg(test)]
mod double_to_int_tests;
mod entry;
mod fallible;
mod floating;
mod forcing;
mod formatting;
mod plan;
mod primitives;
#[cfg(test)]
mod retention_tests;
mod safepoint;
#[cfg(test)]
mod settlement_tests;
mod static_bytes;
mod text_search;
mod wide_words;
pub use admission::{admit_prepared, admit_program, supports_operation};

/// Absolute machine-wide top-table slot at which one compiled program's own
/// top slots begin. Generated code addresses every top through
/// [`crate::layout::VMCTX_PREPARED_TOPS_OFFSET`] using an immediate baked in
/// at compile time (`slot * size_of::<usize>()`), so a program's base must be
/// fixed before [`CompiledProgram::compile`] runs; nothing later can rebase
/// already-emitted code. [`PreparedMachine::install_program`] validates that
/// a program was compiled against the base it actually claims.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd, Hash)]
pub struct TopSlotBase(pub u32);

impl TopSlotBase {
    /// The base every single-program caller (`PreparedMachine::new`,
    /// `CompiledProgram::run_entry`) compiles against.
    pub const ZERO: TopSlotBase = TopSlotBase(0);
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum Unsupported {
    #[error("closed execution target {0:?} does not match the pinned native host profile")]
    Target(TargetDescriptor),
    #[error("closed execution cannot admit global {0:?}")]
    Global(GlobalId),
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
    #[error("constructor {identity:?} is declared differently from the descriptor already interned for it")]
    DescriptorShape {
        identity: Box<tidepool_repr::execution_schema::SymbolIdentity>,
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

/// Prepared descriptor mismatches are compiler-contract failures. The JIT can
/// only report them through a host boundary; the machine owns first-cause
/// precedence and turns the recorded cause into the entry status.
unsafe extern "C" fn prepared_case_trap(vmctx: *mut crate::context::VMContext) {
    let machine = unsafe { crate::machine_state::machine_state(vmctx) };
    machine.set_first_cause(crate::host_fns::RuntimeError::CaseTrap);
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

/// Resolve a foreign callee for `apply::emit_dispatchers`' terminal
/// fallback call site: `header` is the callee object's masked
/// header word, `fingerprint` is the CALLER's expectation of that
/// callee's signature shape (see `resolve::signature_fingerprint`).
/// Returns the code pointer on a fingerprint-matching hit, 0 on any
/// miss -- a fingerprint mismatch is treated exactly like "nothing
/// registered", never a silent wrong-ABI jump.
unsafe extern "C" fn prepared_resolve_call(
    vmctx: *mut crate::context::VMContext,
    header: u64,
    fingerprint: u64,
) -> u64 {
    let machine = unsafe { crate::machine_state::machine_state(vmctx) };
    let masked = (header as usize) & !7;
    match machine.resolve_prepared_call(masked) {
        Some(entry) if entry.fingerprint == fingerprint => entry.code as u64,
        _ => {
            // A header some installed program can enter names a real
            // callable (a function with another arity/signature, or a PAP):
            // the application is unservable here, the heap is untouched,
            // the machine stays reusable. Any other header is not a
            // callable object at all.
            if machine.owns_prepared_entry(masked) {
                machine.set_first_cause(crate::host_fns::RuntimeError::UnresolvedCallee);
            } else {
                machine.set_first_cause(crate::host_fns::RuntimeError::BadThunkState(0));
            }
            0
        }
    }
}

/// Resolve the owning program's `prepared_enter` for a foreign
/// thunk/function/PAP header, so `entry.rs`'s per-program enter chain
/// can fall back to it. Returns 0 on a miss: the enter map is the union
/// of every installed program's enterable headers, so a miss means no
/// program owns the object -- an integrity failure, not an unresolved
/// callee.
unsafe extern "C" fn prepared_resolve_enter(
    vmctx: *mut crate::context::VMContext,
    header: u64,
) -> u64 {
    let machine = unsafe { crate::machine_state::machine_state(vmctx) };
    let masked = (header as usize) & !7;
    match machine.resolve_prepared_enter(masked) {
        Some(code) => code as u64,
        None => {
            machine.set_first_cause(crate::host_fns::RuntimeError::BadThunkState(0));
            0
        }
    }
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

/// Pins generated entries, descriptors and immutable images together. Each run
/// owns its mutable heap; materialization may force values before releasing it.
pub struct CompiledProgram {
    pub(crate) pipeline: CodegenPipeline,
    pub(crate) entries: BTreeMap<ValueId, CompiledEntry>,
    pub(crate) descriptors: Vec<Arc<ObjectDescriptor>>,
    pub(crate) descriptor_registry: BTreeMap<usize, DescriptorMetadata>,
    pub(crate) statics: StaticImage,
    pub(crate) top_slots: BTreeMap<ValueId, usize>,
    /// Admitted imports' slots -- see [`plan::ImportSlot`]. Indexed by
    /// `GlobalId`, occupying the machine-wide range right after `top_slots`.
    pub(crate) import_slots: Vec<plan::ImportSlot>,
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
    pub(crate) heap_top_specs: Vec<plan::HeapTopSpec>,
    /// Platform C-ABI adapter `(vmctx, result_out, managed_ref) -> status`.
    /// The target is generated code which calls Tail `prepared_enter`.
    pub(crate) force_adapter: FuncId,
    /// Every function this program exports as a cross-program call target.
    /// `PreparedMachine::install` registers these in the machine's
    /// resolution table, which `apply::emit_dispatchers`' fallback queries
    /// through `prepared_resolve_call`.
    pub(crate) callables: Vec<resolve::CallableExport>,
    /// This program's own `prepared_enter` FuncId. `PreparedMachine::install`
    /// registers it as the owner for every header in `enter_owned_headers`.
    pub(crate) enter: FuncId,
    /// Every thunk/function/PAP descriptor header THIS program's own
    /// `prepared_enter` (the `enter` field above) knows how to force --
    /// i.e. the union of `plan.thunks`' and the evaluated-chain descriptors'
    /// header words, mirroring what `entry::emit_prepared_enter`'s
    /// `thunks`/`evaluated` parameters already cover for this program.
    /// `PreparedMachine::install` registers these against `enter` so
    /// `prepared_resolve_enter` can dispatch foreign headers to their
    /// owning program.
    pub(crate) enter_owned_headers: Vec<usize>,
}

impl CompiledProgram {
    /// Compile with a fresh descriptor interner: a standalone program, or the
    /// first program of a machine (whose descriptors the machine absorbs at
    /// install). Later programs on a machine compile through
    /// `PreparedMachine::compile_for_install` so they share descriptors.
    pub fn compile(linked: &LinkedProgram, base: TopSlotBase) -> Result<Self, CompileError> {
        Self::compile_with(linked, base, &mut DescriptorInterner::default())
    }

    /// [`Self::compile`] against `interner`: constructor identities already
    /// interned reuse their descriptor, so this program's `Case`, enter and
    /// observation recognise objects an earlier program built.
    pub fn compile_with(
        linked: &LinkedProgram,
        base: TopSlotBase,
        interner: &mut DescriptorInterner,
    ) -> Result<Self, CompileError> {
        let target = &linked.prepared().envelope().target;
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
        admit_program(linked)?;
        use crate::entry_abi::{EnvironmentMode, NativeAbiProfile};
        use cranelift_codegen::isa::CallConv;
        use cranelift_module::Linkage;
        use tidepool_repr::execution_schema::{HeapRhs, RuntimeRep};
        let plan = plan::ProgramPlan::new(linked.prepared(), base, interner)?;
        let profile = NativeAbiProfile::new(plan.program.envelope().target.clone(), 0)?;
        let statics = image::build_static_image(&plan)?;
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
        for (&id, signature) in &signatures {
            let abi = EntryAbi::lower_internal(&profile, signature, EnvironmentMode::Captured)?;
            if plan.thunks.contains_key(&id) {
                abis.insert(id, abi);
                continue;
            }
            let native = abi.cranelift_signature(&profile, CallConv::Tail)?;
            let function = pipeline.declare_function_with_signature(
                &format!("prepared_entry_{}", id.0),
                Linkage::Local,
                &native,
            )?;
            functions.insert(id, function);
            abis.insert(id, abi);
        }
        for &id in plan.thunks.keys() {
            functions.insert(id, prepared_enter);
        }
        let dispatchers = apply::declare_dispatchers(&plan, &profile, &mut pipeline)?;
        apply::emit_dispatchers(
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
            prepared_recorded_failure,
            &mut pipeline,
        )?;
        // Every function address has been declared, including recursive peers.
        for &id in functions.keys() {
            if plan.thunks.contains_key(&id) {
                continue;
            }
            emit::emit_function(
                &plan,
                id,
                &functions,
                &dispatchers,
                prepared_gc,
                prepared_poll,
                prepared_stack_overflow,
                prepared_enter,
                case_trap,
                &mut pipeline,
            )?;
        }
        for (&id, &body) in &thunk_bodies {
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
        }
        let thunk_entries = plan
            .thunks
            .iter()
            .map(|(&id, thunk)| entry::ThunkEntry {
                descriptor: Arc::clone(&thunk.descriptor),
                body: thunk_bodies[&id],
                policy: thunk.policy,
                results: thunk.signature.results.clone(),
            })
            .collect::<Vec<_>>();
        let mut enter_evaluated = plan.constructors.clone();
        enter_evaluated.extend(
            plan.functions
                .values()
                .map(|function| Arc::clone(&function.descriptor)),
        );
        enter_evaluated.extend(
            plan.pap_layouts
                .values()
                .map(|pap| Arc::clone(&pap.descriptor)),
        );
        entry::emit_prepared_enter(
            &mut pipeline,
            prepared_enter,
            &thunk_entries,
            &enter_evaluated,
            prepared_poll,
            prepared_stack_overflow,
            prepared_bad_state,
            prepared_blackhole,
            prepared_resolve_enter,
            prepared_recorded_failure,
            write_barrier,
        )?;
        let force_adapter =
            adapter::emit_force_adapter(&mut pipeline, "prepared_force_adapter", prepared_enter)?;
        let mut entries = BTreeMap::new();
        for (&id, &slot) in &plan.top_slots {
            let abi = abis[&id].clone();
            let adapter = adapter::emit_adapter(
                &mut pipeline,
                &format!("prepared_adapter_{}", id.0),
                if plan.thunks.contains_key(&id) {
                    prepared_enter
                } else {
                    functions[&id]
                },
                &abi,
                slot,
            )?;
            entries.insert(
                id,
                CompiledEntry {
                    #[cfg(test)]
                    function: functions[&id],
                    adapter,
                    abi,
                },
            );
        }
        pipeline.finalize()?;
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
        let callables = plan
            .functions
            .iter()
            .filter_map(|(id, function)| {
                functions
                    .get(id)
                    .map(|&function_id| resolve::CallableExport {
                        header: function.descriptor.initial_header_word(),
                        function: function_id,
                        fingerprint: resolve::signature_fingerprint(function.signature),
                    })
            })
            .collect::<Vec<_>>();
        let enter_owned_headers = thunk_entries
            .iter()
            .map(|thunk_entry| thunk_entry.descriptor.initial_header_word())
            .chain(
                enter_evaluated
                    .iter()
                    .map(|descriptor| descriptor.initial_header_word()),
            )
            .collect::<Vec<_>>();
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
        Ok(Self {
            pipeline,
            entries,
            descriptors,
            descriptor_registry,
            statics,
            top_slots: plan.top_slots,
            import_slots: plan.import_slots,
            interned_constructors: plan.interned_constructors,
            byte_tops,
            bytes: plan.bytes,
            heap_top_specs: plan.heap_top_specs,
            force_adapter,
            callables,
            enter: prepared_enter,
            enter_owned_headers,
        })
    }

    pub(crate) fn prepared_force_adapter(&self) -> FuncId {
        self.force_adapter
    }

    /// The number of machine-wide top-table slots this program claims when
    /// installed -- its own tops plus every admitted import's slot. A caller
    /// sizing a fresh [`PreparedMachine`]'s [`PreparedMachineOptions::top_slots`]
    /// for exactly one program reads this after compiling, before installing.
    #[must_use]
    pub fn top_slot_count(&self) -> usize {
        self.top_slots.len() + self.import_slots.len()
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
