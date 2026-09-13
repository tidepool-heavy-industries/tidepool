use cranelift_codegen::{Context, ir::{self, types, AbiParam, InstBuilder, MemFlags}};
use cranelift_frontend::{FunctionBuilder, FunctionBuilderContext};
use cranelift_module::{FuncId, Linkage, Module};
use tidepool_repr::execution_schema::RuntimeRep;
use super::{CompileError, CompiledEntry};
use crate::{entry_abi::EntryAbi, pipeline::CodegenPipeline};

/// Rust calls one platform signature regardless of semantic arity. Scalars are
/// transported as native-endian u64 slots; only generated code calls Tail ABI.
pub(super) fn emit_adapter(
    pipeline: &mut CodegenPipeline,
    name: &str,
    function: FuncId,
    abi: &EntryAbi,
    top_slot: usize,
) -> Result<FuncId, CompileError> {
    let mut context = Context::new();
    context.func.signature = ir::Signature::new(pipeline.isa.default_call_conv());
    context.func.signature.params = vec![AbiParam::new(types::I64); 3];
    context.func.signature.returns = vec![AbiParam::new(types::I32)];
    let adapter = pipeline.declare_function_with_signature(name, Linkage::Export, &context.func.signature)?;
    let mut frontend = FunctionBuilderContext::new();
    let mut builder = FunctionBuilder::new(&mut context.func, &mut frontend);
    let block = builder.create_block();
    builder.append_block_params_for_function_params(block);
    builder.switch_to_block(block);
    builder.seal_block(block);
    let parameters = builder.block_params(block).to_vec();
    let vmctx = parameters[0];
    let result_area = parameters[1];
    let argument_area = parameters[2];
    let tops = builder.ins().load(types::I64, MemFlags::trusted(), vmctx, crate::layout::VMCTX_PREPARED_TOPS_OFFSET);
    let environment = builder.ins().load(types::I64, MemFlags::trusted(), tops, (top_slot * 8) as i32);
    let mut arguments = vec![vmctx, environment];
    for (index, rep) in abi.physical_arguments().iter().enumerate() {
        let ty = scalar_type(*rep);
        let value = builder.ins().load(ty, MemFlags::trusted(), argument_area, (index * 8) as i32);
        if matches!(rep, RuntimeRep::LiftedRef | RuntimeRep::UnliftedRef) {
            builder.declare_value_needs_stack_map(value);
        }
        arguments.push(value);
    }
    let callee = pipeline.module.declare_func_in_func(function, builder.func);
    let values = super::emit_direct_call(&mut builder, callee, &arguments, abi.semantic_results());
    for (value, field) in values.into_iter().zip(abi.result_layout().fields()) {
        builder.ins().store(MemFlags::trusted(), value, result_area, field.offset() as i32);
    }
    let success = builder.ins().iconst(types::I32, 0);
    builder.ins().return_(&[success]);
    builder.finalize();
    pipeline.define_function(adapter, &mut context)?;
    Ok(adapter)
}

pub(super) fn scalar_type(rep: RuntimeRep) -> ir::Type {
    match rep {
        RuntimeRep::Int(8) | RuntimeRep::Word(8) => types::I8,
        RuntimeRep::Int(16) | RuntimeRep::Word(16) => types::I16,
        RuntimeRep::Int(32) | RuntimeRep::Word(32) => types::I32,
        RuntimeRep::Float(32) => types::F32,
        RuntimeRep::Float(64) => types::F64,
        RuntimeRep::LiftedRef | RuntimeRep::UnliftedRef | RuntimeRep::Address
            | RuntimeRep::Int(64) | RuntimeRep::Word(64) => types::I64,
        _ => unreachable!("validated physical scalar representation"),
    }
}
