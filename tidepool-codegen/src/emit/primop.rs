use super::*;
use crate::emit::{SsaVal, EmitError};
use tidepool_repr::PrimOpKind;
use cranelift_codegen::ir::{types, InstBuilder, MemFlags, Value, condcodes::IntCC, condcodes::FloatCC};
use cranelift_frontend::FunctionBuilder;

/// Emit a primitive operation. Unboxes HeapPtr args, performs the op, returns Raw.
pub fn emit_primop(
    builder: &mut FunctionBuilder,
    op: &PrimOpKind,
    args: &[SsaVal],
) -> Result<SsaVal, EmitError> {
    match op {
        // Int arithmetic (binary)
        PrimOpKind::IntAdd => {
            check_arity(op, 2, args.len())?;
            let a = unbox_int(builder, args[0]);
            let b = unbox_int(builder, args[1]);
            Ok(SsaVal::Raw(builder.ins().iadd(a, b), LIT_TAG_INT))
        }
        PrimOpKind::IntSub => {
            check_arity(op, 2, args.len())?;
            let a = unbox_int(builder, args[0]);
            let b = unbox_int(builder, args[1]);
            Ok(SsaVal::Raw(builder.ins().isub(a, b), LIT_TAG_INT))
        }
        PrimOpKind::IntMul => {
            check_arity(op, 2, args.len())?;
            let a = unbox_int(builder, args[0]);
            let b = unbox_int(builder, args[1]);
            Ok(SsaVal::Raw(builder.ins().imul(a, b), LIT_TAG_INT))
        }
        PrimOpKind::IntNegate => {
            check_arity(op, 1, args.len())?;
            let a = unbox_int(builder, args[0]);
            Ok(SsaVal::Raw(builder.ins().ineg(a), LIT_TAG_INT))
        }

        // Int comparison → returns i64 (0=False, 1=True)
        PrimOpKind::IntEq => emit_int_compare(builder, op, IntCC::Equal, args, LIT_TAG_INT),
        PrimOpKind::IntNe => emit_int_compare(builder, op, IntCC::NotEqual, args, LIT_TAG_INT),
        PrimOpKind::IntLt => emit_int_compare(builder, op, IntCC::SignedLessThan, args, LIT_TAG_INT),
        PrimOpKind::IntLe => emit_int_compare(builder, op, IntCC::SignedLessThanOrEqual, args, LIT_TAG_INT),
        PrimOpKind::IntGt => emit_int_compare(builder, op, IntCC::SignedGreaterThan, args, LIT_TAG_INT),
        PrimOpKind::IntGe => emit_int_compare(builder, op, IntCC::SignedGreaterThanOrEqual, args, LIT_TAG_INT),

        // Word arithmetic
        PrimOpKind::WordAdd => {
            check_arity(op, 2, args.len())?;
            let a = unbox_int(builder, args[0]);
            let b = unbox_int(builder, args[1]);
            Ok(SsaVal::Raw(builder.ins().iadd(a, b), LIT_TAG_WORD))
        }
        PrimOpKind::WordSub => {
            check_arity(op, 2, args.len())?;
            let a = unbox_int(builder, args[0]);
            let b = unbox_int(builder, args[1]);
            Ok(SsaVal::Raw(builder.ins().isub(a, b), LIT_TAG_WORD))
        }
        PrimOpKind::WordMul => {
            check_arity(op, 2, args.len())?;
            let a = unbox_int(builder, args[0]);
            let b = unbox_int(builder, args[1]);
            Ok(SsaVal::Raw(builder.ins().imul(a, b), LIT_TAG_WORD))
        }

        // Word comparison (unsigned)
        PrimOpKind::WordEq => emit_int_compare(builder, op, IntCC::Equal, args, LIT_TAG_INT),
        PrimOpKind::WordNe => emit_int_compare(builder, op, IntCC::NotEqual, args, LIT_TAG_INT),
        PrimOpKind::WordLt => emit_int_compare(builder, op, IntCC::UnsignedLessThan, args, LIT_TAG_INT),
        PrimOpKind::WordLe => emit_int_compare(builder, op, IntCC::UnsignedLessThanOrEqual, args, LIT_TAG_INT),
        PrimOpKind::WordGt => emit_int_compare(builder, op, IntCC::UnsignedGreaterThan, args, LIT_TAG_INT),
        PrimOpKind::WordGe => emit_int_compare(builder, op, IntCC::UnsignedGreaterThanOrEqual, args, LIT_TAG_INT),

        // Double arithmetic (unbox_double → f64, fadd/fsub/fmul/fdiv)
        PrimOpKind::DoubleAdd => {
            check_arity(op, 2, args.len())?;
            let a = unbox_double(builder, args[0]);
            let b = unbox_double(builder, args[1]);
            Ok(SsaVal::Raw(builder.ins().fadd(a, b), LIT_TAG_DOUBLE))
        }
        PrimOpKind::DoubleSub => {
            check_arity(op, 2, args.len())?;
            let a = unbox_double(builder, args[0]);
            let b = unbox_double(builder, args[1]);
            Ok(SsaVal::Raw(builder.ins().fsub(a, b), LIT_TAG_DOUBLE))
        }
        PrimOpKind::DoubleMul => {
            check_arity(op, 2, args.len())?;
            let a = unbox_double(builder, args[0]);
            let b = unbox_double(builder, args[1]);
            Ok(SsaVal::Raw(builder.ins().fmul(a, b), LIT_TAG_DOUBLE))
        }
        PrimOpKind::DoubleDiv => {
            check_arity(op, 2, args.len())?;
            let a = unbox_double(builder, args[0]);
            let b = unbox_double(builder, args[1]);
            Ok(SsaVal::Raw(builder.ins().fdiv(a, b), LIT_TAG_DOUBLE))
        }

        // Double comparison → returns i64 (0 or 1)
        PrimOpKind::DoubleEq => emit_float_compare(builder, op, FloatCC::Equal, args, LIT_TAG_INT),
        PrimOpKind::DoubleNe => emit_float_compare(builder, op, FloatCC::NotEqual, args, LIT_TAG_INT),
        PrimOpKind::DoubleLt => emit_float_compare(builder, op, FloatCC::LessThan, args, LIT_TAG_INT),
        PrimOpKind::DoubleLe => emit_float_compare(builder, op, FloatCC::LessThanOrEqual, args, LIT_TAG_INT),
        PrimOpKind::DoubleGt => emit_float_compare(builder, op, FloatCC::GreaterThan, args, LIT_TAG_INT),
        PrimOpKind::DoubleGe => emit_float_compare(builder, op, FloatCC::GreaterThanOrEqual, args, LIT_TAG_INT),

        // Char comparison → unbox_int (char stored as i64), use IntCC
        PrimOpKind::CharEq => emit_int_compare(builder, op, IntCC::Equal, args, LIT_TAG_INT),
        PrimOpKind::CharNe => emit_int_compare(builder, op, IntCC::NotEqual, args, LIT_TAG_INT),
        PrimOpKind::CharLt => emit_int_compare(builder, op, IntCC::UnsignedLessThan, args, LIT_TAG_INT),
        PrimOpKind::CharLe => emit_int_compare(builder, op, IntCC::UnsignedLessThanOrEqual, args, LIT_TAG_INT),
        PrimOpKind::CharGt => emit_int_compare(builder, op, IntCC::UnsignedGreaterThan, args, LIT_TAG_INT),
        PrimOpKind::CharGe => emit_int_compare(builder, op, IntCC::UnsignedGreaterThanOrEqual, args, LIT_TAG_INT),

        // Special ops
        PrimOpKind::DataToTag => {
            check_arity(op, 1, args.len())?;
            // Load con_tag from HeapObject at CON_TAG_OFFSET
            let obj = args[0].value(); // HeapPtr
            let tag = builder.ins().load(types::I64, MemFlags::trusted(), obj, CON_TAG_OFFSET);
            Ok(SsaVal::Raw(tag, LIT_TAG_INT))
        }
        PrimOpKind::IntQuot => {
            check_arity(op, 2, args.len())?;
            let a = unbox_int(builder, args[0]);
            let b = unbox_int(builder, args[1]);
            Ok(SsaVal::Raw(builder.ins().sdiv(a, b), LIT_TAG_INT))
        }
        PrimOpKind::IntRem => {
            check_arity(op, 2, args.len())?;
            let a = unbox_int(builder, args[0]);
            let b = unbox_int(builder, args[1]);
            Ok(SsaVal::Raw(builder.ins().srem(a, b), LIT_TAG_INT))
        }
        PrimOpKind::Chr => {
            check_arity(op, 1, args.len())?;
            let v = unbox_int(builder, args[0]);
            Ok(SsaVal::Raw(v, LIT_TAG_CHAR))
        }
        PrimOpKind::Ord => {
            check_arity(op, 1, args.len())?;
            let v = unbox_int(builder, args[0]);
            Ok(SsaVal::Raw(v, LIT_TAG_INT))
        }
        PrimOpKind::TagToEnum | PrimOpKind::IndexArray | PrimOpKind::SeqOp => {
            Err(EmitError::NotYetImplemented(format!("{:?}", op)))
        }
        _ => Err(EmitError::NotYetImplemented(format!("{:?}", op))),
    }
}

fn check_arity(op: &PrimOpKind, expected: usize, got: usize) -> Result<(), EmitError> {
    if expected != got {
        Err(EmitError::InvalidArity(*op, expected, got))
    } else {
        Ok(())
    }
}

fn emit_int_compare(
    builder: &mut FunctionBuilder,
    op: &PrimOpKind,
    cc: IntCC,
    args: &[SsaVal],
    tag: i64,
) -> Result<SsaVal, EmitError> {
    check_arity(op, 2, args.len())?;
    let a = unbox_int(builder, args[0]);
    let b = unbox_int(builder, args[1]);
    let cmp = builder.ins().icmp(cc, a, b);
    Ok(SsaVal::Raw(builder.ins().uextend(types::I64, cmp), tag))
}

fn emit_float_compare(
    builder: &mut FunctionBuilder,
    op: &PrimOpKind,
    cc: FloatCC,
    args: &[SsaVal],
    tag: i64,
) -> Result<SsaVal, EmitError> {
    check_arity(op, 2, args.len())?;
    let a = unbox_double(builder, args[0]);
    let b = unbox_double(builder, args[1]);
    let cmp = builder.ins().fcmp(cc, a, b);
    Ok(SsaVal::Raw(builder.ins().uextend(types::I64, cmp), tag))
}

pub fn unbox_int(builder: &mut FunctionBuilder, val: SsaVal) -> Value {
    match val {
        SsaVal::Raw(v, _) => v,
        SsaVal::HeapPtr(v) => builder.ins().load(types::I64, MemFlags::trusted(), v, LIT_VALUE_OFFSET),
    }
}

pub fn unbox_double(builder: &mut FunctionBuilder, val: SsaVal) -> Value {
    match val {
        SsaVal::Raw(v, _) => v,
        SsaVal::HeapPtr(v) => builder.ins().load(types::F64, MemFlags::trusted(), v, LIT_VALUE_OFFSET),
    }
}