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

        // Int bitwise
        PrimOpKind::IntAnd => {
            check_arity(op, 2, args.len())?;
            let a = unbox_int(builder, args[0]);
            let b = unbox_int(builder, args[1]);
            Ok(SsaVal::Raw(builder.ins().band(a, b), LIT_TAG_INT))
        }
        PrimOpKind::IntOr => {
            check_arity(op, 2, args.len())?;
            let a = unbox_int(builder, args[0]);
            let b = unbox_int(builder, args[1]);
            Ok(SsaVal::Raw(builder.ins().bor(a, b), LIT_TAG_INT))
        }
        PrimOpKind::IntXor => {
            check_arity(op, 2, args.len())?;
            let a = unbox_int(builder, args[0]);
            let b = unbox_int(builder, args[1]);
            Ok(SsaVal::Raw(builder.ins().bxor(a, b), LIT_TAG_INT))
        }
        PrimOpKind::IntNot => {
            check_arity(op, 1, args.len())?;
            let a = unbox_int(builder, args[0]);
            Ok(SsaVal::Raw(builder.ins().bnot(a), LIT_TAG_INT))
        }

        // Int shifts
        PrimOpKind::IntShl => {
            check_arity(op, 2, args.len())?;
            let a = unbox_int(builder, args[0]);
            let b = unbox_int(builder, args[1]);
            Ok(SsaVal::Raw(builder.ins().ishl(a, b), LIT_TAG_INT))
        }
        PrimOpKind::IntShra => {
            check_arity(op, 2, args.len())?;
            let a = unbox_int(builder, args[0]);
            let b = unbox_int(builder, args[1]);
            Ok(SsaVal::Raw(builder.ins().sshr(a, b), LIT_TAG_INT))
        }
        PrimOpKind::IntShrl => {
            check_arity(op, 2, args.len())?;
            let a = unbox_int(builder, args[0]);
            let b = unbox_int(builder, args[1]);
            Ok(SsaVal::Raw(builder.ins().ushr(a, b), LIT_TAG_INT))
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

        PrimOpKind::WordQuot => {
            check_arity(op, 2, args.len())?;
            let a = unbox_int(builder, args[0]);
            let b = unbox_int(builder, args[1]);
            Ok(SsaVal::Raw(builder.ins().udiv(a, b), LIT_TAG_WORD))
        }
        PrimOpKind::WordRem => {
            check_arity(op, 2, args.len())?;
            let a = unbox_int(builder, args[0]);
            let b = unbox_int(builder, args[1]);
            Ok(SsaVal::Raw(builder.ins().urem(a, b), LIT_TAG_WORD))
        }

        // Word bitwise
        PrimOpKind::WordAnd => {
            check_arity(op, 2, args.len())?;
            let a = unbox_int(builder, args[0]);
            let b = unbox_int(builder, args[1]);
            Ok(SsaVal::Raw(builder.ins().band(a, b), LIT_TAG_WORD))
        }
        PrimOpKind::WordOr => {
            check_arity(op, 2, args.len())?;
            let a = unbox_int(builder, args[0]);
            let b = unbox_int(builder, args[1]);
            Ok(SsaVal::Raw(builder.ins().bor(a, b), LIT_TAG_WORD))
        }
        PrimOpKind::WordXor => {
            check_arity(op, 2, args.len())?;
            let a = unbox_int(builder, args[0]);
            let b = unbox_int(builder, args[1]);
            Ok(SsaVal::Raw(builder.ins().bxor(a, b), LIT_TAG_WORD))
        }
        PrimOpKind::WordNot => {
            check_arity(op, 1, args.len())?;
            let a = unbox_int(builder, args[0]);
            Ok(SsaVal::Raw(builder.ins().bnot(a), LIT_TAG_WORD))
        }

        // Word shifts
        PrimOpKind::WordShl => {
            check_arity(op, 2, args.len())?;
            let a = unbox_int(builder, args[0]);
            let b = unbox_int(builder, args[1]);
            Ok(SsaVal::Raw(builder.ins().ishl(a, b), LIT_TAG_WORD))
        }
        PrimOpKind::WordShrl => {
            check_arity(op, 2, args.len())?;
            let a = unbox_int(builder, args[0]);
            let b = unbox_int(builder, args[1]);
            Ok(SsaVal::Raw(builder.ins().ushr(a, b), LIT_TAG_WORD))
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

        // Conversions
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
        PrimOpKind::Int2Word | PrimOpKind::Word2Int => {
            check_arity(op, 1, args.len())?;
            let v = unbox_int(builder, args[0]);
            let tag = if matches!(op, PrimOpKind::Int2Word) { LIT_TAG_WORD } else { LIT_TAG_INT };
            Ok(SsaVal::Raw(v, tag))
        }
        PrimOpKind::Int2Double => {
            check_arity(op, 1, args.len())?;
            let v = unbox_int(builder, args[0]);
            Ok(SsaVal::Raw(builder.ins().fcvt_from_sint(types::F64, v), LIT_TAG_DOUBLE))
        }
        PrimOpKind::Double2Int => {
            check_arity(op, 1, args.len())?;
            let v = unbox_double(builder, args[0]);
            Ok(SsaVal::Raw(builder.ins().fcvt_to_sint_sat(types::I64, v), LIT_TAG_INT))
        }
        PrimOpKind::Int2Float => {
            check_arity(op, 1, args.len())?;
            let v = unbox_int(builder, args[0]);
            Ok(SsaVal::Raw(builder.ins().fcvt_from_sint(types::F32, v), LIT_TAG_FLOAT))
        }
        PrimOpKind::Float2Int => {
            check_arity(op, 1, args.len())?;
            let v = unbox_float(builder, args[0]);
            Ok(SsaVal::Raw(builder.ins().fcvt_to_sint_sat(types::I64, v), LIT_TAG_INT))
        }
        PrimOpKind::Double2Float => {
            check_arity(op, 1, args.len())?;
            let v = unbox_double(builder, args[0]);
            Ok(SsaVal::Raw(builder.ins().fdemote(types::F32, v), LIT_TAG_FLOAT))
        }
        PrimOpKind::Float2Double => {
            check_arity(op, 1, args.len())?;
            let v = unbox_float(builder, args[0]);
            Ok(SsaVal::Raw(builder.ins().fpromote(types::F64, v), LIT_TAG_DOUBLE))
        }

        // Narrowing (truncate then sign/zero-extend back to i64)
        PrimOpKind::Narrow8Int => {
            check_arity(op, 1, args.len())?;
            let v = unbox_int(builder, args[0]);
            let narrow = builder.ins().ireduce(types::I8, v);
            Ok(SsaVal::Raw(builder.ins().sextend(types::I64, narrow), LIT_TAG_INT))
        }
        PrimOpKind::Narrow16Int => {
            check_arity(op, 1, args.len())?;
            let v = unbox_int(builder, args[0]);
            let narrow = builder.ins().ireduce(types::I16, v);
            Ok(SsaVal::Raw(builder.ins().sextend(types::I64, narrow), LIT_TAG_INT))
        }
        PrimOpKind::Narrow32Int => {
            check_arity(op, 1, args.len())?;
            let v = unbox_int(builder, args[0]);
            let narrow = builder.ins().ireduce(types::I32, v);
            Ok(SsaVal::Raw(builder.ins().sextend(types::I64, narrow), LIT_TAG_INT))
        }
        PrimOpKind::Narrow8Word => {
            check_arity(op, 1, args.len())?;
            let v = unbox_int(builder, args[0]);
            let narrow = builder.ins().ireduce(types::I8, v);
            Ok(SsaVal::Raw(builder.ins().uextend(types::I64, narrow), LIT_TAG_WORD))
        }
        PrimOpKind::Narrow16Word => {
            check_arity(op, 1, args.len())?;
            let v = unbox_int(builder, args[0]);
            let narrow = builder.ins().ireduce(types::I16, v);
            Ok(SsaVal::Raw(builder.ins().uextend(types::I64, narrow), LIT_TAG_WORD))
        }
        PrimOpKind::Narrow32Word => {
            check_arity(op, 1, args.len())?;
            let v = unbox_int(builder, args[0]);
            let narrow = builder.ins().ireduce(types::I32, v);
            Ok(SsaVal::Raw(builder.ins().uextend(types::I64, narrow), LIT_TAG_WORD))
        }

        // Special ops
        PrimOpKind::DataToTag => {
            check_arity(op, 1, args.len())?;
            let obj = args[0].value();
            let tag = builder.ins().load(types::I64, MemFlags::trusted(), obj, CON_TAG_OFFSET);
            Ok(SsaVal::Raw(tag, LIT_TAG_INT))
        }
        PrimOpKind::DoubleNegate => {
            check_arity(op, 1, args.len())?;
            let a = unbox_double(builder, args[0]);
            Ok(SsaVal::Raw(builder.ins().fneg(a), LIT_TAG_DOUBLE))
        }
        PrimOpKind::FloatNegate => {
            check_arity(op, 1, args.len())?;
            let a = unbox_float(builder, args[0]);
            Ok(SsaVal::Raw(builder.ins().fneg(a), LIT_TAG_FLOAT))
        }

        // Pointer equality polyfill: always return 0 (not equal).
        // Safe — just disables GHC's structural sharing optimization.
        PrimOpKind::ReallyUnsafePtrEquality => {
            check_arity(op, 2, args.len())?;
            Ok(SsaVal::Raw(builder.ins().iconst(types::I64, 0), LIT_TAG_INT))
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

pub fn unbox_float(builder: &mut FunctionBuilder, val: SsaVal) -> Value {
    match val {
        SsaVal::Raw(v, _) => v,
        SsaVal::HeapPtr(v) => builder.ins().load(types::F32, MemFlags::trusted(), v, LIT_VALUE_OFFSET),
    }
}