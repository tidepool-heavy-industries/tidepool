use super::*;
use crate::emit::{SsaVal, EmitError};
use core_repr::PrimOpKind;
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
            let a = unbox_int(builder, args[0]);
            let b = unbox_int(builder, args[1]);
            Ok(SsaVal::Raw(builder.ins().iadd(a, b)))
        }
        PrimOpKind::IntSub => {
            let a = unbox_int(builder, args[0]);
            let b = unbox_int(builder, args[1]);
            Ok(SsaVal::Raw(builder.ins().isub(a, b)))
        }
        PrimOpKind::IntMul => {
            let a = unbox_int(builder, args[0]);
            let b = unbox_int(builder, args[1]);
            Ok(SsaVal::Raw(builder.ins().imul(a, b)))
        }
        PrimOpKind::IntNegate => {
            let a = unbox_int(builder, args[0]);
            Ok(SsaVal::Raw(builder.ins().ineg(a)))
        }

        // Int comparison → returns i64 (0=False, 1=True)
        PrimOpKind::IntEq => emit_int_compare(builder, IntCC::Equal, args[0], args[1]),
        PrimOpKind::IntNe => emit_int_compare(builder, IntCC::NotEqual, args[0], args[1]),
        PrimOpKind::IntLt => emit_int_compare(builder, IntCC::SignedLessThan, args[0], args[1]),
        PrimOpKind::IntLe => emit_int_compare(builder, IntCC::SignedLessThanOrEqual, args[0], args[1]),
        PrimOpKind::IntGt => emit_int_compare(builder, IntCC::SignedGreaterThan, args[0], args[1]),
        PrimOpKind::IntGe => emit_int_compare(builder, IntCC::SignedGreaterThanOrEqual, args[0], args[1]),

        // Word arithmetic
        PrimOpKind::WordAdd => {
            let a = unbox_int(builder, args[0]);
            let b = unbox_int(builder, args[1]);
            Ok(SsaVal::Raw(builder.ins().iadd(a, b)))
        }
        PrimOpKind::WordSub => {
            let a = unbox_int(builder, args[0]);
            let b = unbox_int(builder, args[1]);
            Ok(SsaVal::Raw(builder.ins().isub(a, b)))
        }
        PrimOpKind::WordMul => {
            let a = unbox_int(builder, args[0]);
            let b = unbox_int(builder, args[1]);
            Ok(SsaVal::Raw(builder.ins().imul(a, b)))
        }

        // Word comparison (unsigned)
        PrimOpKind::WordEq => emit_int_compare(builder, IntCC::Equal, args[0], args[1]),
        PrimOpKind::WordNe => emit_int_compare(builder, IntCC::NotEqual, args[0], args[1]),
        PrimOpKind::WordLt => emit_int_compare(builder, IntCC::UnsignedLessThan, args[0], args[1]),
        PrimOpKind::WordLe => emit_int_compare(builder, IntCC::UnsignedLessThanOrEqual, args[0], args[1]),
        PrimOpKind::WordGt => emit_int_compare(builder, IntCC::UnsignedGreaterThan, args[0], args[1]),
        PrimOpKind::WordGe => emit_int_compare(builder, IntCC::UnsignedGreaterThanOrEqual, args[0], args[1]),

        // Double arithmetic (unbox_double → f64, fadd/fsub/fmul/fdiv)
        PrimOpKind::DoubleAdd => {
            let a = unbox_double(builder, args[0]);
            let b = unbox_double(builder, args[1]);
            Ok(SsaVal::Raw(builder.ins().fadd(a, b)))
        }
        PrimOpKind::DoubleSub => {
            let a = unbox_double(builder, args[0]);
            let b = unbox_double(builder, args[1]);
            Ok(SsaVal::Raw(builder.ins().fsub(a, b)))
        }
        PrimOpKind::DoubleMul => {
            let a = unbox_double(builder, args[0]);
            let b = unbox_double(builder, args[1]);
            Ok(SsaVal::Raw(builder.ins().fmul(a, b)))
        }
        PrimOpKind::DoubleDiv => {
            let a = unbox_double(builder, args[0]);
            let b = unbox_double(builder, args[1]);
            Ok(SsaVal::Raw(builder.ins().fdiv(a, b)))
        }

        // Double comparison → returns i64 (0 or 1)
        PrimOpKind::DoubleEq => emit_float_compare(builder, FloatCC::Equal, args[0], args[1]),
        PrimOpKind::DoubleNe => emit_float_compare(builder, FloatCC::NotEqual, args[0], args[1]),
        PrimOpKind::DoubleLt => emit_float_compare(builder, FloatCC::LessThan, args[0], args[1]),
        PrimOpKind::DoubleLe => emit_float_compare(builder, FloatCC::LessThanOrEqual, args[0], args[1]),
        PrimOpKind::DoubleGt => emit_float_compare(builder, FloatCC::GreaterThan, args[0], args[1]),
        PrimOpKind::DoubleGe => emit_float_compare(builder, FloatCC::GreaterThanOrEqual, args[0], args[1]),

        // Char comparison → unbox_int (char stored as i64), use IntCC
        PrimOpKind::CharEq => emit_int_compare(builder, IntCC::Equal, args[0], args[1]),
        PrimOpKind::CharNe => emit_int_compare(builder, IntCC::NotEqual, args[0], args[1]),
        PrimOpKind::CharLt => emit_int_compare(builder, IntCC::UnsignedLessThan, args[0], args[1]),
        PrimOpKind::CharLe => emit_int_compare(builder, IntCC::UnsignedLessThanOrEqual, args[0], args[1]),
        PrimOpKind::CharGt => emit_int_compare(builder, IntCC::UnsignedGreaterThan, args[0], args[1]),
        PrimOpKind::CharGe => emit_int_compare(builder, IntCC::UnsignedGreaterThanOrEqual, args[0], args[1]),

        // Special ops
        PrimOpKind::DataToTag => {
            // Load con_tag from HeapObject at CON_TAG_OFFSET
            let obj = args[0].value(); // HeapPtr
            let tag = builder.ins().load(types::I64, MemFlags::trusted(), obj, CON_TAG_OFFSET);
            Ok(SsaVal::Raw(tag))
        }
        PrimOpKind::TagToEnum | PrimOpKind::IndexArray | PrimOpKind::SeqOp => {
            Err(EmitError::NotYetImplemented(format!("{:?}", op)))
        }
    }
}

fn emit_int_compare(
    builder: &mut FunctionBuilder,
    cc: IntCC,
    a: SsaVal,
    b: SsaVal,
) -> Result<SsaVal, EmitError> {
    let a = unbox_int(builder, a);
    let b = unbox_int(builder, b);
    let cmp = builder.ins().icmp(cc, a, b);
    Ok(SsaVal::Raw(builder.ins().uextend(types::I64, cmp)))
}

fn emit_float_compare(
    builder: &mut FunctionBuilder,
    cc: FloatCC,
    a: SsaVal,
    b: SsaVal,
) -> Result<SsaVal, EmitError> {
    let a = unbox_double(builder, a);
    let b = unbox_double(builder, b);
    let cmp = builder.ins().fcmp(cc, a, b);
    Ok(SsaVal::Raw(builder.ins().uextend(types::I64, cmp)))
}

pub fn unbox_int(builder: &mut FunctionBuilder, val: SsaVal) -> Value {
    match val {
        SsaVal::Raw(v) => v,
        SsaVal::HeapPtr(v) => builder.ins().load(types::I64, MemFlags::trusted(), v, LIT_VALUE_OFFSET),
    }
}

pub fn unbox_double(builder: &mut FunctionBuilder, val: SsaVal) -> Value {
    match val {
        SsaVal::Raw(v) => v,
        SsaVal::HeapPtr(v) => builder.ins().load(types::F64, MemFlags::trusted(), v, LIT_VALUE_OFFSET),
    }
}
