//! Checked scalar failure paths use the invocation's first-cause protocol.
//!
//! Signed and unsigned quotient/remainder operations report typed failures
//! before hardware division. Signed MIN/-1 quotient reports Overflow; its
//! remainder is zero without executing a trapping divide. Emission receives
//! VMContext and pipeline ownership explicitly and never uses TLS or poison
//! values for these scalar failures.

use super::primitives::{IntegerKind, IntegerOperation};
use crate::{context::VMContext, host_fns::RuntimeError, pipeline::CodegenPipeline};
use cranelift_codegen::ir::{self, types, AbiParam, BlockArg, InstBuilder, Value};
use cranelift_frontend::FunctionBuilder;
use cranelift_module::{Linkage, Module};

#[derive(Clone, Copy)]
#[repr(u32)]
pub(super) enum PrimitiveFailure {
    DivisionByZero = 0,
    Overflow = 1,
}

/// # Safety
/// vmctx names the live invocation; this non-collecting error hook never reads
/// heap payloads and never unwinds through generated native frames.
pub(super) unsafe extern "C" fn prepared_primitive_failure(
    vmctx: *mut VMContext,
    kind: u32,
) -> i32 {
    let machine = unsafe { crate::machine_state::machine_state(vmctx) };
    machine.set_first_cause(match kind {
        0 => RuntimeError::DivisionByZero,
        1 => RuntimeError::Overflow,
        _ => RuntimeError::BadPointer,
    });
    machine.prepared_call_status() as i32
}

/// On failure, terminate this generated entry with its actual recorded status.
/// The continuation is reachable only when the hardware operation is safe.
pub(super) fn guard(
    builder: &mut FunctionBuilder<'_>,
    pipeline: &mut CodegenPipeline,
    vmctx: Value,
    failed: Value,
    cause: PrimitiveFailure,
) -> Result<(), super::CompileError> {
    let mut signature = ir::Signature::new(pipeline.isa.default_call_conv());
    signature.params = vec![AbiParam::new(types::I64), AbiParam::new(types::I32)];
    signature.returns = vec![AbiParam::new(types::I32)];
    let host = pipeline
        .module
        .declare_function("prepared_primitive_failure", Linkage::Import, &signature)
        .map_err(|error| crate::pipeline::PipelineError::Declaration(error.to_string()))?;
    let host = pipeline.module.declare_func_in_func(host, builder.func);
    let failure = builder.create_block();
    let success = builder.create_block();
    builder.ins().brif(failed, failure, &[], success, &[]);
    builder.switch_to_block(failure);
    builder.seal_block(failure);
    let cause = builder.ins().iconst(types::I32, cause as i64);
    let call = builder.ins().call(host, &[vmctx, cause]);
    let status = builder.inst_results(call)[0];
    crate::alloc::emit_prepared_failure_return(builder, status);
    builder.switch_to_block(success);
    builder.seal_block(success);
    Ok(())
}

pub(super) fn emit(
    operation: IntegerOperation,
    builder: &mut FunctionBuilder<'_>,
    vmctx: Value,
    pipeline: &mut CodegenPipeline,
    arguments: &[Value],
) -> Result<Vec<Value>, super::CompileError> {
    let divisor = arguments[1];
    let divisor_ty = builder.func.dfg.value_type(divisor);
    let zero = builder.ins().iconst(divisor_ty, 0);
    let divide_by_zero = builder
        .ins()
        .icmp(ir::condcodes::IntCC::Equal, divisor, zero);
    guard(
        builder,
        pipeline,
        vmctx,
        divide_by_zero,
        PrimitiveFailure::DivisionByZero,
    )?;

    let signed_overflow = if operation.signed {
        let ty = builder.func.dfg.value_type(arguments[0]);
        let min_value = if operation.bits == 64 {
            i64::MIN
        } else {
            -(1_i64 << (operation.bits - 1))
        };
        let min = builder.ins().iconst(ty, min_value);
        let minus_one = builder.ins().iconst(ty, -1);
        let lhs_min = builder
            .ins()
            .icmp(ir::condcodes::IntCC::Equal, arguments[0], min);
        let rhs_minus_one = builder
            .ins()
            .icmp(ir::condcodes::IntCC::Equal, divisor, minus_one);
        Some(builder.ins().band(lhs_min, rhs_minus_one))
    } else {
        None
    };

    if let Some(overflow) = signed_overflow {
        if matches!(operation.kind, IntegerKind::Quot | IntegerKind::QuotRem) {
            guard(
                builder,
                pipeline,
                vmctx,
                overflow,
                PrimitiveFailure::Overflow,
            )?;
        } else {
            // GHC's `remInt#` defines MIN/-1 as zero. Keep the trapping
            // hardware remainder on a branch that is unreachable for that
            // pair; a select would still execute the faulting instruction.
            let ty = builder.func.dfg.value_type(arguments[0]);
            let zero_result = builder.ins().iconst(ty, 0);
            let overflow_block = builder.create_block();
            let remainder_block = builder.create_block();
            let result_block = builder.create_block();
            builder.append_block_param(result_block, ty);
            builder
                .ins()
                .brif(overflow, overflow_block, &[], remainder_block, &[]);
            builder.switch_to_block(overflow_block);
            builder.seal_block(overflow_block);
            builder
                .ins()
                .jump(result_block, &[BlockArg::Value(zero_result)]);
            builder.switch_to_block(remainder_block);
            builder.seal_block(remainder_block);
            let remainder = builder.ins().srem(arguments[0], divisor);
            builder
                .ins()
                .jump(result_block, &[BlockArg::Value(remainder)]);
            builder.switch_to_block(result_block);
            builder.seal_block(result_block);
            return Ok(vec![builder.block_params(result_block)[0]]);
        }
    }

    let values = match (operation.signed, operation.kind) {
        (true, IntegerKind::Quot) => builder.ins().sdiv(arguments[0], divisor),
        (true, IntegerKind::Rem) => builder.ins().srem(arguments[0], divisor),
        (false, IntegerKind::Quot) => builder.ins().udiv(arguments[0], divisor),
        (false, IntegerKind::Rem) => builder.ins().urem(arguments[0], divisor),
        (true, IntegerKind::QuotRem) => {
            let quotient = builder.ins().sdiv(arguments[0], divisor);
            let remainder = builder.ins().srem(arguments[0], divisor);
            return Ok(vec![quotient, remainder]);
        }
        (false, IntegerKind::QuotRem) => {
            let quotient = builder.ins().udiv(arguments[0], divisor);
            let remainder = builder.ins().urem(arguments[0], divisor);
            return Ok(vec![quotient, remainder]);
        }
        _ => unreachable!("fallible emitter only receives quotient/remainder operations"),
    };
    Ok(vec![values])
}

/// GHC double2Int# truncates toward zero. Its out-of-range/NaN domain is
/// undefined; the prepared engine reports Overflow rather than allowing a
/// native conversion trap. For this pinned 64-bit profile, both bounds are
/// exactly representable doubles and the upper bound is exclusive.
pub(super) fn emit_double_to_int(
    builder: &mut FunctionBuilder<'_>,
    vmctx: Value,
    pipeline: &mut CodegenPipeline,
    value: Value,
) -> Result<Vec<Value>, super::CompileError> {
    use ir::condcodes::{FloatCC, IntCC};

    let lower = builder.ins().f64const(-9_223_372_036_854_775_808.0);
    let upper = builder.ins().f64const(9_223_372_036_854_775_808.0);
    let above_lower = builder
        .ins()
        .fcmp(FloatCC::GreaterThanOrEqual, value, lower);
    let below_upper = builder.ins().fcmp(FloatCC::LessThan, value, upper);
    let in_range = builder.ins().band(above_lower, below_upper);
    let invalid = builder.ins().icmp_imm(IntCC::Equal, in_range, 0);
    guard(
        builder,
        pipeline,
        vmctx,
        invalid,
        PrimitiveFailure::Overflow,
    )?;
    Ok(vec![builder.ins().fcvt_to_sint(types::I64, value)])
}
