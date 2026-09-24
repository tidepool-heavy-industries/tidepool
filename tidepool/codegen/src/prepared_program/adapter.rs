use super::CompileError;
use crate::{entry_abi::EntryAbi, pipeline::CodegenPipeline};
use cranelift_codegen::{
    ir::{self, types, AbiParam, InstBuilder, MemFlags},
    Context,
};
use cranelift_frontend::{FunctionBuilder, FunctionBuilderContext};
use cranelift_module::{FuncId, Linkage, Module};
use tidepool_repr::execution_schema::RuntimeRep;
use tidepool_repr::execution_schema::Signature;

/// Emit the shared dynamic-call boundary. The argument area starts with the
/// entered callee followed by one 64-bit slot for each non-Void argument; the
/// result area likewise uses one slot per non-Void result. The adapter loads
/// every managed value into rooted SSA before calling code that may safepoint.
/// The two buffers are transport only and are never registered as GC roots.
pub(super) fn emit_dynamic_adapter(
    pipeline: &mut CodegenPipeline,
    name: &str,
    function: FuncId,
    signature: &Signature,
    profile: &crate::entry_abi::NativeAbiProfile,
) -> Result<FuncId, CompileError> {
    let abi = EntryAbi::lower_internal(
        profile,
        signature,
        crate::entry_abi::EnvironmentMode::Captured,
    )?;
    let mut context = Context::new();
    context.func.signature = ir::Signature::new(pipeline.isa.default_call_conv());
    context.func.signature.params = vec![AbiParam::new(types::I64); 3];
    context.func.signature.returns = vec![AbiParam::new(types::I32)];
    let adapter =
        pipeline.declare_function_with_signature(name, Linkage::Local, &context.func.signature)?;
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
    let environment = builder
        .ins()
        .load(types::I64, MemFlags::trusted(), argument_area, 0);
    builder.declare_value_needs_stack_map(environment);
    let mut arguments = vec![vmctx, environment];
    for (index, rep) in abi.physical_arguments().iter().enumerate() {
        let value = builder.ins().load(
            scalar_type(*rep),
            MemFlags::trusted(),
            argument_area,
            ((index + 1) * 8) as i32,
        );
        if matches!(rep, RuntimeRep::LiftedRef | RuntimeRep::UnliftedRef) {
            builder.declare_value_needs_stack_map(value);
        }
        arguments.push(value);
    }
    let callee = pipeline.module.declare_func_in_func(function, builder.func);
    let values = super::emit_direct_call(
        &mut builder,
        pipeline,
        vmctx,
        callee,
        &arguments,
        abi.semantic_results(),
    )?;
    if let Some(values) = values {
        for (index, value) in values.into_iter().enumerate() {
            builder
                .ins()
                .store(MemFlags::trusted(), value, result_area, (index * 8) as i32);
        }
        let success = builder.ins().iconst(types::I32, 0);
        builder.ins().return_(&[success]);
    }
    builder.finalize();
    pipeline.define_function(adapter, &mut context)?;
    Ok(adapter)
}

/// Emit the platform ABI bridge used by later observers to force one managed
/// reference. The generated adapter owns the ABI transition: callers pass a
/// VMContext, result slot, and managed reference; the Tail lazy entry remains
/// an internal relocation and is never cast to a Rust function pointer.
pub(super) fn emit_force_adapter(
    pipeline: &mut CodegenPipeline,
    name: &str,
    prepared_enter: FuncId,
) -> Result<FuncId, CompileError> {
    let mut context = Context::new();
    context.func.signature = ir::Signature::new(pipeline.isa.default_call_conv());
    context.func.signature.params = vec![AbiParam::new(types::I64); 3];
    context.func.signature.returns = vec![AbiParam::new(types::I32)];
    let adapter =
        pipeline.declare_function_with_signature(name, Linkage::Local, &context.func.signature)?;
    let mut frontend = FunctionBuilderContext::new();
    let mut builder = FunctionBuilder::new(&mut context.func, &mut frontend);
    let block = builder.create_block();
    builder.append_block_params_for_function_params(block);
    builder.switch_to_block(block);
    builder.seal_block(block);
    let parameters = builder.block_params(block);
    let vmctx = parameters[0];
    let result_out = parameters[1];
    let reference = parameters[2];
    builder.declare_value_needs_stack_map(reference);
    let callee = pipeline
        .module
        .declare_func_in_func(prepared_enter, builder.func);
    let call = builder.ins().call(callee, &[vmctx, reference]);
    let returned = builder.inst_results(call).to_vec();
    let status = returned[0];
    let success = builder.create_block();
    let failure = builder.create_block();
    let ok = builder.ins().icmp_imm(
        ir::condcodes::IntCC::Equal,
        status,
        crate::prepared_control::CallStatus::Success as i64,
    );
    builder.ins().brif(ok, success, &[], failure, &[]);
    builder.switch_to_block(failure);
    builder.seal_block(failure);
    builder.ins().return_(&[status]);
    builder.switch_to_block(success);
    builder.seal_block(success);
    let value = returned[1];
    builder.declare_value_needs_stack_map(value);
    builder
        .ins()
        .store(MemFlags::trusted(), value, result_out, 0);
    builder.ins().return_(&[status]);
    builder.finalize();
    pipeline.define_function(adapter, &mut context)?;
    Ok(adapter)
}

/// Rust calls one platform signature regardless of semantic arity. Scalars are
/// transported as native-endian u64 slots; only generated code calls Tail ABI.
pub(super) fn emit_adapter(
    pipeline: &mut CodegenPipeline,
    name: &str,
    function: FuncId,
    abi: &EntryAbi,
    image_slot: super::roots::ImageSlot,
    top_slot: usize,
) -> Result<FuncId, CompileError> {
    let mut context = Context::new();
    context.func.signature = ir::Signature::new(pipeline.isa.default_call_conv());
    context.func.signature.params = vec![AbiParam::new(types::I64); 3];
    context.func.signature.returns = vec![AbiParam::new(types::I32)];
    let adapter =
        pipeline.declare_function_with_signature(name, Linkage::Export, &context.func.signature)?;
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
    // The entry's environment is its own top, read through the running
    // machine's root-block table (see `emit::root_slot_value`).
    let environment = super::emit::root_slot_value(
        &mut builder,
        vmctx,
        image_slot,
        top_slot,
        RuntimeRep::LiftedRef,
    );
    let mut arguments = vec![vmctx, environment];
    for (index, rep) in abi.physical_arguments().iter().enumerate() {
        let ty = scalar_type(*rep);
        let value = builder
            .ins()
            .load(ty, MemFlags::trusted(), argument_area, (index * 8) as i32);
        if matches!(rep, RuntimeRep::LiftedRef | RuntimeRep::UnliftedRef) {
            builder.declare_value_needs_stack_map(value);
        }
        arguments.push(value);
    }
    let callee = pipeline.module.declare_func_in_func(function, builder.func);
    let values = super::emit_direct_call(
        &mut builder,
        pipeline,
        vmctx,
        callee,
        &arguments,
        abi.semantic_results(),
    )?;
    if let Some(values) = values {
        for (value, field) in values.into_iter().zip(abi.result_layout().fields()) {
            builder.ins().store(
                MemFlags::trusted(),
                value,
                result_area,
                field.offset() as i32,
            );
        }
        let success = builder.ins().iconst(types::I32, 0);
        builder.ins().return_(&[success]);
    }
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
        RuntimeRep::LiftedRef
        | RuntimeRep::UnliftedRef
        | RuntimeRep::Address
        | RuntimeRep::Int(64)
        | RuntimeRep::Word(64) => types::I64,
        _ => unreachable!("validated physical scalar representation"),
    }
}
