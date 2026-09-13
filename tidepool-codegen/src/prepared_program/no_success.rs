//! Terminal result paths never publish a fabricated successful payload.

use cranelift_codegen::ir::{self, types, AbiParam, InstBuilder, Value};
use cranelift_frontend::FunctionBuilder;
use cranelift_module::{Linkage, Module};

pub(super) enum TerminalCause {
    Raised(Value),
    UnexpectedSuccess,
}

/// The generated caller supplied a valid VMContext. This call cannot collect,
/// force a diagnostic, or replace the invocation's first cause.
pub(super) unsafe extern "C" fn unexpected_success(vmctx: *mut crate::context::VMContext) -> i32 {
    let machine = unsafe { crate::machine_state::machine_state(vmctx) };
    machine.set_first_cause(crate::host_fns::RuntimeError::NoSuccessReturned);
    machine.prepared_call_status() as i32
}

/// The operand is a managed value admitted by generated code. Its independent
/// machine-owned root survives observation's temporary-root cleanup. Do not
/// observe or force it here: raising records a cause, not a diagnostic demand.
pub(super) unsafe extern "C" fn raise(
    vmctx: *mut crate::context::VMContext,
    operand: *mut u8,
) -> i32 {
    let machine = unsafe { crate::machine_state::machine_state(vmctx) };
    unsafe { machine.record_prepared_raise(operand) };
    machine.prepared_call_status() as i32
}

/// Finish the current block with the actual machine status. Both paths are
/// noncollecting; there is no successful continuation and no result-area access.
pub(super) fn emit_terminal(
    builder: &mut FunctionBuilder<'_>,
    pipeline: &mut crate::pipeline::CodegenPipeline,
    vmctx: Value,
    cause: TerminalCause,
) -> Result<(), super::CompileError> {
    let mut signature = ir::Signature::new(pipeline.isa.default_call_conv());
    signature.params.push(AbiParam::new(types::I64));
    let mut arguments = vec![vmctx];
    let name = match cause {
        TerminalCause::Raised(operand) => {
            signature.params.push(AbiParam::new(types::I64));
            arguments.push(operand);
            "prepared_raise"
        }
        TerminalCause::UnexpectedSuccess => "prepared_no_success_returned",
    };
    signature.returns.push(AbiParam::new(types::I32));
    let function = pipeline
        .module
        .declare_function(name, Linkage::Import, &signature)
        .map_err(|error| crate::pipeline::PipelineError::Declaration(error.to_string()))?;
    let function = pipeline.module.declare_func_in_func(function, builder.func);
    let call = builder.ins().call(function, &arguments);
    let status = builder.inst_results(call)[0];
    crate::alloc::emit_prepared_failure_return(builder, status);
    Ok(())
}
