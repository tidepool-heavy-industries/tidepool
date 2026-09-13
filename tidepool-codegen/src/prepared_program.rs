//! Connected prepared entry ownership and native lowering.
//!
//! Compiled addresses/descriptors are program-owned; heap pointers and top
//! tables are invocation-owned. No invocation pointer is embedded in code.

use std::collections::BTreeMap;
use std::sync::Arc;
use cranelift_codegen::ir::{self, types, InstBuilder, Value as SsaValue};
use cranelift_frontend::FunctionBuilder;
use cranelift_module::FuncId;
use tidepool_heap::execution_descriptor::ObjectDescriptor;
use tidepool_heap::static_region::{StaticImage, StaticImageError};
use tidepool_repr::execution_schema::{GlobalId, LinkedProgram, RuntimeRep, Signature, ValueId};
use tidepool_repr::DataConId;
use crate::entry_abi::EntryAbi;
use crate::pipeline::{CodegenPipeline, PipelineError};

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
    pub(crate) bytes: Vec<Arc<[u8]>>,
}

impl CompiledProgram {
    pub fn compile(linked: &LinkedProgram) -> Result<Self, CompileError> {
        admit_program(linked)?;
        // wave4:PROGRAM_OWNER — declare all top/local function entries before
        // defining any; pin descriptors and literal bytes before embedding
        // their addresses; finalize all code/maps together, then publish Self.
        todo!("wave4:PROGRAM_OWNER")
    }
}

pub fn admit_program(linked: &LinkedProgram) -> Result<(), Unsupported> {
    if !linked.prepared().globals().is_empty() {
        return Err(Unsupported::Global(GlobalId(0)));
    }
    // wave4:ADMISSION — inspect every nested RHS/frame, not just selected entry.
    // Host argument admission applies at adapter selection, not internal calls.
    todo!("wave4:ADMISSION")
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
