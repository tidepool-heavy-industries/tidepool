//! Connected prepared entry ownership and native lowering.
//!
//! Compiled addresses/descriptors are program-owned; heap pointers and top
//! tables are invocation-owned. No invocation pointer is embedded in code.

use std::collections::BTreeMap;
use std::sync::Arc;
use cranelift_codegen::ir::{self, InstBuilder, Value as SsaValue};
use cranelift_frontend::FunctionBuilder;
use cranelift_module::FuncId;
use tidepool_heap::execution_descriptor::ObjectDescriptor;
use tidepool_heap::static_region::{StaticImage, StaticImageError};
use tidepool_repr::execution_schema::{GlobalId, LinkedProgram, RuntimeRep, Signature, ValueId};
use tidepool_repr::DataConId;
use crate::entry_abi::EntryAbi;
use crate::pipeline::{CodegenPipeline, PipelineError};

mod admission;
mod adapter;
mod emit;
mod image;
mod plan;
pub use admission::admit_program;

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum Unsupported {
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
        admit_program(linked)?;
        use cranelift_codegen::isa::CallConv;
        use cranelift_module::Linkage;
        use tidepool_repr::execution_schema::{HeapRhs, RuntimeRep};
        use crate::entry_abi::{EnvironmentMode, NativeAbiProfile};
        let plan = plan::ProgramPlan::new(linked.prepared())?;
        let profile = NativeAbiProfile::new(plan.program.envelope().target.clone(), 0)?;
        let statics = image::build_static_image(&plan)?;
        let mut pipeline = CodegenPipeline::new(&[
            ("prepared_gc_trigger", crate::host_fns::prepared_gc_trigger as *const u8),
        ])?;
        let mut signatures = BTreeMap::new();
        for (&id, function) in &plan.functions {
            signatures.insert(id, function.signature.clone());
        }
        for (&id, binding) in &plan.top_bindings {
            signatures.entry(id).or_insert_with(|| Signature {
                arguments: vec![],
                results: vec![match &binding.rhs {
                    HeapRhs::Bytes(_) => RuntimeRep::Address,
                    HeapRhs::Constructor { constructor, .. } => plan.program.constructors()[constructor.0 as usize].result_rep,
                    _ => RuntimeRep::LiftedRef,
                }],
            });
        }
        let mut functions = BTreeMap::new();
        let mut abis = BTreeMap::new();
        for (&id, signature) in &signatures {
            let abi = EntryAbi::lower_internal(&profile, signature, EnvironmentMode::Captured)?;
            let native = abi.cranelift_signature(&profile, CallConv::Tail)?;
            let function = pipeline.declare_function_with_signature(&format!("prepared_entry_{}", id.0), Linkage::Local, &native)?;
            functions.insert(id, function);
            abis.insert(id, abi);
        }
        // Every function address has been declared, including recursive peers.
        for &id in functions.keys() {
            emit::emit_function(&plan, id, &functions, &mut pipeline)?;
        }
        let mut entries = BTreeMap::new();
        for (&id, &slot) in &plan.top_slots {
            let abi = abis[&id].clone();
            let adapter = adapter::emit_adapter(&mut pipeline, &format!("prepared_adapter_{}", id.0), functions[&id], &abi, slot)?;
            entries.insert(id, CompiledEntry { function: functions[&id], adapter, signature: signatures[&id].clone(), abi });
        }
        pipeline.finalize()?;
        let mut descriptors = plan.constructors.clone();
        descriptors.extend(plan.functions.values().map(|function| function.descriptor.clone()));
        let constructors = plan.program.constructors().iter().zip(&plan.constructors)
            .map(|(declaration, descriptor)| (descriptor.initial_header_word(), ConstructorObservation {
                identity: declaration.host_id, fields: declaration.field_reps.clone(),
            })).collect();
        let byte_tops = plan.top_bindings.iter().filter_map(|(&id, binding)| match &binding.rhs {
            HeapRhs::Bytes(bytes) => Some((id, plan.bytes[bytes].clone())), _ => None,
        }).collect();
        Ok(Self { pipeline, entries, descriptors, constructors, statics,
            bytes: plan.bytes, top_slots: plan.top_slots, byte_tops })
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
    let ok = builder.ins().icmp_imm(ir::condcodes::IntCC::Equal, returned[0], 0);
    builder.ins().brif(ok, success, &[], failure, &[]);
    builder.switch_to_block(failure);
    builder.seal_block(failure);
    crate::alloc::emit_prepared_failure_return(builder, returned[0]);
    builder.switch_to_block(success);
    builder.seal_block(success);
    let payload = returned[1..].to_vec();
    for (&value, rep) in payload.iter().zip(results.iter().filter(|rep| **rep != RuntimeRep::Void)) {
        if matches!(rep, RuntimeRep::LiftedRef | RuntimeRep::UnliftedRef) {
            builder.declare_value_needs_stack_map(value);
        }
    }
    payload
}

#[cfg(test)]
mod tests {
    #[test]
    fn multivalue_managed_results_survive_collection_after_return() {
        // wave4:ABI_CONTRACT — real adapter, enough mixed scalar/reference
        // results to force implicit sret, live caller across callee collection,
        // live returned managed values across a subsequent collection.
        todo!("wave4:ABI_CONTRACT")
    }
}
