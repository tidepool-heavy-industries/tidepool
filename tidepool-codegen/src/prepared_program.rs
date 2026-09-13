//! Connected prepared entry ownership and native lowering.
//!
//! Compiled addresses/descriptors are program-owned; heap pointers and top
//! tables are invocation-owned. No invocation pointer is embedded in code.

use crate::entry_abi::EntryAbi;
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
mod invocation;
mod no_success;
#[cfg(test)]
mod no_success_tests;
mod observe;
pub use observe::ObservationFailure;
mod run;
pub use run::{ExecutionError, RunOptions, RunResult};
#[cfg(test)]
mod apply_tests;
mod arrays;
mod byte_arrays;
#[cfg(test)]
mod bytes_tests;
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
pub use admission::{admit_prepared, admit_program};

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
    #[error("entry {0:?} cannot accept managed host arguments")]
    HostArguments(ValueId),
}

#[derive(Debug, thiserror::Error)]
pub enum CompileError {
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
}

pub(crate) struct CompiledEntry {
    pub function: FuncId,
    pub adapter: FuncId,
    pub signature: Signature,
    pub abi: EntryAbi,
}

pub(crate) struct ConstructorObservation {
    pub identity: DataConId,
    pub fields: Vec<RuntimeRep>,
}

/// One program-owned descriptor index supplies both entry and observation.
/// Callable addresses are finalized Tail entries and remain pinned by pipeline;
/// they are never callable as Rust function pointers. Constructor metadata is
/// the authoritative observation identity, independent of family-relative tags.
pub(crate) struct DescriptorMetadata {
    pub descriptor: Arc<ObjectDescriptor>,
    pub meaning: DescriptorMeaning,
}

pub(crate) enum DescriptorMeaning {
    External,
    Constructor(ConstructorObservation),
    Callable {
        binding: ValueId,
        address: *const u8,
        signature: Signature,
        update: Option<tidepool_repr::execution_schema::UpdatePolicy>,
    },
    Pap {
        function: ValueId,
        pending: usize,
        signature: Signature,
    },
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

/// Pins generated entries, descriptors and immutable images together. Each run
/// owns its mutable heap; materialization may force values before releasing it.
pub struct CompiledProgram {
    pub(crate) pipeline: CodegenPipeline,
    pub(crate) entries: BTreeMap<ValueId, CompiledEntry>,
    pub(crate) descriptors: Vec<Arc<ObjectDescriptor>>,
    pub(crate) descriptor_registry: BTreeMap<usize, DescriptorMetadata>,
    pub(crate) statics: StaticImage,
    pub(crate) top_slots: BTreeMap<ValueId, usize>,
    pub(crate) byte_tops: BTreeMap<ValueId, Arc<[u8]>>,
    /// Own every address embedded in generated code, including scalar literals
    /// with no top-level Bytes binding. Keys are logical bytes; values are the
    /// exact allocations whose addresses the emitter used.
    pub(crate) bytes: Arc<static_bytes::PinnedBytes>,
    pub(crate) heap_top_specs: Vec<plan::HeapTopSpec>,
    /// Platform C-ABI adapter `(vmctx, result_out, managed_ref) -> status`.
    /// The target is generated code which calls Tail `prepared_enter`.
    pub(crate) force_adapter: FuncId,
}

impl CompiledProgram {
    pub fn compile(linked: &LinkedProgram) -> Result<Self, CompileError> {
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
        let plan = plan::ProgramPlan::new(linked.prepared())?;
        let profile = NativeAbiProfile::new(plan.program.envelope().target.clone(), 0)?;
        let statics = image::build_static_image(&plan)?;
        let mut pipeline = CodegenPipeline::new(&[
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
            ("prepared_raise", no_success::raise as *const u8),
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
                "prepared_cas_boxed",
                arrays::prepared_cas_boxed as *const u8,
            ),
            (
                "prepared_new_bytes",
                byte_arrays::prepared_new_bytes as *const u8,
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
        ])?;
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
                EntryAbi::lower_internal(&profile, &thunk.signature, EnvironmentMode::Captured)?;
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
        for (&id, _) in &plan.thunks {
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
                    function: functions[&id],
                    adapter,
                    signature: signatures[&id].clone(),
                    abi,
                },
            );
        }
        pipeline.finalize()?;
        let mut descriptors = plan.constructors.clone();
        descriptors.push(Arc::clone(&plan.boxed_array));
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
        for (&id, function) in &plan.functions {
            descriptor_registry.insert(
                function.descriptor.initial_header_word(),
                DescriptorMetadata {
                    descriptor: Arc::clone(&function.descriptor),
                    meaning: DescriptorMeaning::Callable {
                        binding: id,
                        address: pipeline.get_function_ptr(functions[&id]),
                        signature: function.signature.clone(),
                        update: None,
                    },
                },
            );
        }
        for (&id, thunk) in &plan.thunks {
            descriptor_registry.insert(
                thunk.descriptor.initial_header_word(),
                DescriptorMetadata {
                    descriptor: Arc::clone(&thunk.descriptor),
                    meaning: DescriptorMeaning::Callable {
                        binding: id,
                        address: pipeline.get_function_ptr(prepared_enter),
                        signature: thunk.signature.clone(),
                        update: Some(thunk.policy),
                    },
                },
            );
        }
        for (&(function, pending), pap) in &plan.pap_layouts {
            let signature = plan
                .functions
                .get(&function)
                .map(|entry| Signature {
                    arguments: entry.signature.arguments[pending..].to_vec(),
                    results: entry.signature.results.clone(),
                })
                .ok_or(CompileError::MissingRepresentation(function))?;
            descriptor_registry.insert(
                pap.descriptor.initial_header_word(),
                DescriptorMetadata {
                    descriptor: Arc::clone(&pap.descriptor),
                    meaning: DescriptorMeaning::Pap {
                        function,
                        pending,
                        signature,
                    },
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
        Ok(Self {
            pipeline,
            entries,
            descriptors,
            descriptor_registry,
            statics,
            top_slots: plan.top_slots,
            byte_tops,
            bytes: plan.bytes,
            heap_top_specs: plan.heap_top_specs,
            force_adapter,
        })
    }

    pub(crate) fn prepared_force_adapter(&self) -> FuncId {
        self.force_adapter
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
mod tests;
