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
    Architecture, Endianness, GlobalId, LinkedProgram, RuntimeRep, Signature, TargetDescriptor,
    ValueId,
};
use tidepool_repr::DataConId;

mod adapter;
mod admission;
mod emit;
mod image;
mod observe;
pub use observe::ObservationFailure;
mod run;
pub use run::{ExecutionError, RunOptions, RunResult};
mod plan;
pub use admission::{admit_prepared, admit_program};

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum Unsupported {
    #[error("closed execution target {0:?} does not match the pinned native host profile")]
    Target(TargetDescriptor),
    #[error("closed execution cannot admit global {0:?}")]
    Global(GlobalId),
    #[error("thunk {0:?} is outside this execution checkpoint")]
    Thunk(ValueId),
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

/// Prepared descriptor mismatches are compiler-contract failures. The JIT can
/// only report them through a host boundary; the machine owns first-cause
/// precedence and turns the recorded cause into the entry status.
unsafe extern "C" fn prepared_case_trap(vmctx: *mut crate::context::VMContext) {
    let machine = unsafe { crate::machine_state::machine_state(vmctx) };
    machine.set_first_cause(crate::host_fns::RuntimeError::CaseTrap);
}

/// Shared slow inspection for generated, provenance-checked references. It does
/// not force, allocate, collect, or replace the reference in this strict phase.
unsafe extern "C" fn prepared_enter_slow(
    vmctx: *mut crate::context::VMContext,
    reference: *const usize,
) -> i32 {
    let machine = unsafe { crate::machine_state::machine_state(vmctx) };
    if machine.prepared_call_status() == crate::prepared_control::CallStatus::Success {
        if let Err(cause) = unsafe { machine.inspect_prepared_entry(reference) } {
            machine.set_first_cause(cause);
        }
    }
    machine.prepared_call_status() as i32
}

#[cfg(test)]
mod slow_entry_tests {
    use super::*;
    use crate::context::VMContext;
    use crate::host_fns::RuntimeError;
    use crate::machine_state::MachineState;
    use crate::prepared_control::CallStatus;
    use tidepool_heap::execution_descriptor::ObjectDescriptor;
    use tidepool_repr::execution_schema::{StorageLayout, TargetDescriptor};

    unsafe extern "C" fn no_gc(_: *mut VMContext) {}

    fn target() -> TargetDescriptor {
        TargetDescriptor {
            architecture: Architecture::X86_64,
            endianness: Endianness::Little,
            pointer_width: 64,
            word_width: 64,
            abi: "sysv64".into(),
            features: Vec::new(),
        }
    }

    fn vmctx(machine: &MachineState) -> VMContext {
        let mut vmctx = VMContext::new(std::ptr::null_mut(), std::ptr::null(), no_gc);
        vmctx.machine_state = (machine as *const MachineState).cast_mut();
        vmctx
    }

    #[test]
    fn prepared_enter_slow_rejects_null_and_preserves_first_cause() {
        let machine = MachineState::new();
        let mut vmctx = vmctx(&machine);
        assert_eq!(
            unsafe { prepared_enter_slow(&mut vmctx, std::ptr::null()) },
            CallStatus::IntegrityFailure as i32
        );
        assert_eq!(machine.take_runtime_error(), Some(RuntimeError::BadPointer));
    }

    #[test]
    fn prepared_enter_slow_rejects_unknown_headers() {
        let machine = MachineState::new();
        let descriptor = Arc::new(
            ObjectDescriptor::constructor(
                1,
                StorageLayout::for_reps(&target(), &[]).unwrap(),
                None,
            )
            .unwrap(),
        );
        machine
            .install_prepared_buffer_with_static_region(vec![0; 4], vec![descriptor], None)
            .unwrap();
        let (start, _) = machine.gc_active_range().unwrap();
        unsafe { start.cast::<usize>().write(0x1000) };
        let mut vmctx = vmctx(&machine);
        assert_eq!(
            unsafe { prepared_enter_slow(&mut vmctx, start.cast()) },
            CallStatus::IntegrityFailure as i32
        );
        assert_eq!(machine.take_runtime_error(), Some(RuntimeError::BadPointer));
    }

    #[test]
    fn prepared_enter_slow_accepts_live_constructor_headers() {
        let machine = MachineState::new();
        let descriptor = Arc::new(
            ObjectDescriptor::constructor(
                1,
                StorageLayout::for_reps(&target(), &[]).unwrap(),
                None,
            )
            .unwrap(),
        );
        let header = descriptor.initial_header_word();
        machine
            .install_prepared_buffer_with_static_region(vec![0; 4], vec![descriptor], None)
            .unwrap();
        let (start, _) = machine.gc_active_range().unwrap();
        unsafe { start.cast::<usize>().write(header) };
        let mut vmctx = vmctx(&machine);
        assert_eq!(
            unsafe { prepared_enter_slow(&mut vmctx, start.cast()) },
            CallStatus::Success as i32
        );
        assert_eq!(machine.take_runtime_error(), None);
    }

    #[test]
    fn prepared_enter_slow_does_not_overwrite_a_terminal_cause() {
        let machine = MachineState::new();
        machine.set_first_cause(RuntimeError::Cancelled);
        let mut vmctx = vmctx(&machine);
        assert_eq!(
            unsafe { prepared_enter_slow(&mut vmctx, std::ptr::null()) },
            CallStatus::Cancelled as i32
        );
        assert_eq!(machine.take_runtime_error(), Some(RuntimeError::Cancelled));
    }
}

pub struct CompiledProgram {
    pub(crate) pipeline: CodegenPipeline,
    pub(crate) entries: BTreeMap<ValueId, CompiledEntry>,
    pub(crate) descriptors: Vec<Arc<ObjectDescriptor>>,
    pub(crate) constructors: BTreeMap<usize, ConstructorObservation>,
    pub(crate) statics: StaticImage,
    pub(crate) bytes: BTreeMap<Vec<u8>, Arc<[u8]>>,
    pub(crate) top_slots: BTreeMap<ValueId, usize>,
    pub(crate) byte_tops: BTreeMap<ValueId, Arc<[u8]>>,
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
            ("prepared_enter_slow", prepared_enter_slow as *const u8),
            ("prepared_case_trap", prepared_case_trap as *const u8),
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
        let mut prepared_enter_slow_signature =
            ir::Signature::new(pipeline.isa.default_call_conv());
        prepared_enter_slow_signature
            .params
            .push(AbiParam::new(types::I64));
        prepared_enter_slow_signature
            .params
            .push(AbiParam::new(types::I64));
        prepared_enter_slow_signature
            .returns
            .push(AbiParam::new(types::I32));
        let prepared_enter_slow = pipeline
            .module
            .declare_function(
                "prepared_enter_slow",
                Linkage::Import,
                &prepared_enter_slow_signature,
            )
            .map_err(|error| PipelineError::Declaration(error.to_string()))?;
        let mut case_trap_signature = ir::Signature::new(pipeline.isa.default_call_conv());
        case_trap_signature.params.push(AbiParam::new(types::I64));
        let case_trap = pipeline
            .module
            .declare_function("prepared_case_trap", Linkage::Import, &case_trap_signature)
            .map_err(|error| PipelineError::Declaration(error.to_string()))?;
        let mut signatures = BTreeMap::new();
        for (&id, function) in &plan.functions {
            signatures.insert(id, function.signature.clone());
        }
        for (&id, binding) in &plan.top_bindings {
            signatures.entry(id).or_insert_with(|| Signature {
                arguments: vec![],
                results: vec![match &binding.rhs {
                    HeapRhs::Bytes(_) => RuntimeRep::Address,
                    HeapRhs::Constructor { constructor, .. } => {
                        plan.program.constructors()[constructor.0 as usize].result_rep
                    }
                    _ => RuntimeRep::LiftedRef,
                }],
            });
        }
        let mut functions = BTreeMap::new();
        let mut abis = BTreeMap::new();
        for (&id, signature) in &signatures {
            let abi = EntryAbi::lower_internal(&profile, signature, EnvironmentMode::Captured)?;
            let native = abi.cranelift_signature(&profile, CallConv::Tail)?;
            let function = pipeline.declare_function_with_signature(
                &format!("prepared_entry_{}", id.0),
                Linkage::Local,
                &native,
            )?;
            functions.insert(id, function);
            abis.insert(id, abi);
        }
        // Every function address has been declared, including recursive peers.
        for &id in functions.keys() {
            emit::emit_function(
                &plan,
                id,
                &functions,
                prepared_gc,
                prepared_enter_slow,
                case_trap,
                &mut pipeline,
            )?;
        }
        let mut entries = BTreeMap::new();
        for (&id, &slot) in &plan.top_slots {
            let abi = abis[&id].clone();
            let adapter = adapter::emit_adapter(
                &mut pipeline,
                &format!("prepared_adapter_{}", id.0),
                functions[&id],
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
        descriptors.extend(
            plan.functions
                .values()
                .map(|function| function.descriptor.clone()),
        );
        let constructors = plan
            .program
            .constructors()
            .iter()
            .zip(&plan.constructors)
            .map(|(declaration, descriptor)| {
                (
                    descriptor.initial_header_word(),
                    ConstructorObservation {
                        identity: declaration.host_id,
                        fields: declaration.field_reps.clone(),
                    },
                )
            })
            .collect();
        let byte_tops = plan
            .top_bindings
            .iter()
            .filter_map(|(&id, binding)| match &binding.rhs {
                HeapRhs::Bytes(bytes) => Some((id, plan.bytes[bytes].clone())),
                _ => None,
            })
            .collect();
        Ok(Self {
            pipeline,
            entries,
            descriptors,
            constructors,
            statics,
            bytes: plan.bytes,
            top_slots: plan.top_slots,
            byte_tops,
        })
    }
}

/// Lower a saturated, statically resolved call. Payload SSA values become
/// observable only in the success block and are explicitly marked for GC.
pub(crate) fn emit_direct_call(
    builder: &mut FunctionBuilder<'_>,
    callee: ir::FuncRef,
    arguments: &[SsaValue],
    results: &[RuntimeRep],
) -> Vec<SsaValue> {
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
    let payload = returned[1..].to_vec();
    for (&value, rep) in payload
        .iter()
        .zip(results.iter().filter(|rep| **rep != RuntimeRep::Void))
    {
        if matches!(rep, RuntimeRep::LiftedRef | RuntimeRep::UnliftedRef) {
            builder.declare_value_needs_stack_map(value);
        }
    }
    payload
}

#[cfg(test)]
mod tests;
