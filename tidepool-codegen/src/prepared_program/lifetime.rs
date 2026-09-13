//! keepAlive# calls its state-threaded continuation while retaining the managed
//! owner across every safepoint in that call. An opaque, noncollecting use after
//! success makes this a real SSA liveness obligation, not an unused stack-map
//! annotation. Failure unwinds normally without touching a terminal heap.

use cranelift_codegen::ir::{self, types, AbiParam, InstBuilder, Value};
use cranelift_frontend::FunctionBuilder;
use cranelift_module::{Linkage, Module};
use tidepool_repr::execution_schema::{
    OperationDecl, OperationIdentity, ResultContract, RuntimeRep, Signature,
};

pub(super) fn callback_signature(
    declaration: &OperationDecl,
    signature: &Signature,
) -> Option<Signature> {
    use RuntimeRep::*;
    if !matches!(&declaration.identity, OperationIdentity::PrimOp(name) if name == "keepAlive#")
        || !matches!(
            signature.arguments.as_slice(),
            [LiftedRef | UnliftedRef, Void, LiftedRef]
        )
        // The successful continuation carries the post-call liveness use.
        // A NoSuccess operation has no such continuation and is not this ABI.
        || !matches!(signature.results, ResultContract::Returns(_))
    {
        return None;
    }
    Some(Signature {
        arguments: vec![Void],
        results: signature.results.clone(),
    })
}

pub(super) extern "C" fn keep_alive(reference: usize) {
    std::hint::black_box(reference);
}

pub(super) fn emit(
    builder: &mut FunctionBuilder<'_>,
    pipeline: &mut crate::pipeline::CodegenPipeline,
    dispatchers: &super::apply::Dispatchers,
    vmctx: Value,
    retained: Value,
    callback: Value,
    signature: &Signature,
) -> Result<Option<Vec<Value>>, super::CompileError> {
    builder.declare_value_needs_stack_map(retained);
    builder.declare_value_needs_stack_map(callback);
    let dispatcher = dispatchers
        .find(signature)
        .ok_or_else(|| super::CompileError::MissingKeepAliveDispatcher(signature.clone()))?;
    let dispatcher = pipeline
        .module
        .declare_func_in_func(dispatcher, builder.func);
    let result = super::emit_direct_call(
        builder,
        pipeline,
        vmctx,
        dispatcher,
        &[vmctx, callback],
        &signature.results,
    )?;
    if result.is_some() {
        let mut abi = ir::Signature::new(pipeline.isa.default_call_conv());
        abi.params.push(AbiParam::new(types::I64));
        let host = pipeline
            .module
            .declare_function("prepared_keep_alive", Linkage::Import, &abi)
            .map_err(|error| crate::pipeline::PipelineError::Declaration(error.to_string()))?;
        let host = pipeline.module.declare_func_in_func(host, builder.func);
        builder.ins().call(host, &[retained]);
    }
    Ok(result)
}
