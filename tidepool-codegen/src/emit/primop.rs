use super::*;
use crate::alloc::emit_alloc_fast_path;
use crate::emit::{EmitError, SsaVal};
use crate::pipeline::CodegenPipeline;
use cranelift_codegen::ir::{
    self, condcodes::FloatCC, condcodes::IntCC, types, AbiParam, InstBuilder, MemFlags, Signature,
    Value,
};
use cranelift_frontend::FunctionBuilder;
use cranelift_module::Linkage;
use cranelift_module::Module;
use tidepool_heap::layout;
use tidepool_repr::PrimOpKind;

/// Emit a primitive operation. Unboxes HeapPtr args, performs the op, returns Raw.
pub fn emit_primop(
    pipeline: &mut CodegenPipeline,
    builder: &mut FunctionBuilder,
    vmctx: Value,
    gc_sig: ir::SigRef,
    oom_func: ir::FuncRef,
    op: &PrimOpKind,
    args: &[SsaVal],
) -> Result<SsaVal, EmitError> {
    match op {
        // Int arithmetic (binary)
        PrimOpKind::IntAdd => {
            check_arity(op, 2, args.len())?;
            let a = unbox_int(pipeline, builder, args[0]);
            let b = unbox_int(pipeline, builder, args[1]);
            Ok(SsaVal::Raw(builder.ins().iadd(a, b), LIT_TAG_INT))
        }
        PrimOpKind::IntSub => {
            check_arity(op, 2, args.len())?;
            let a = unbox_int(pipeline, builder, args[0]);
            let b = unbox_int(pipeline, builder, args[1]);
            Ok(SsaVal::Raw(builder.ins().isub(a, b), LIT_TAG_INT))
        }
        PrimOpKind::IntMul => {
            check_arity(op, 2, args.len())?;
            let a = unbox_int(pipeline, builder, args[0]);
            let b = unbox_int(pipeline, builder, args[1]);
            Ok(SsaVal::Raw(builder.ins().imul(a, b), LIT_TAG_INT))
        }
        PrimOpKind::IntNegate => {
            check_arity(op, 1, args.len())?;
            let a = unbox_int(pipeline, builder, args[0]);
            Ok(SsaVal::Raw(builder.ins().ineg(a), LIT_TAG_INT))
        }
        PrimOpKind::IntQuot => {
            check_arity(op, 2, args.len())?;
            let a = unbox_int(pipeline, builder, args[0]);
            let b = unbox_int(pipeline, builder, args[1]);
            Ok(SsaVal::Raw(builder.ins().sdiv(a, b), LIT_TAG_INT))
        }
        PrimOpKind::IntRem => {
            check_arity(op, 2, args.len())?;
            let a = unbox_int(pipeline, builder, args[0]);
            let b = unbox_int(pipeline, builder, args[1]);
            Ok(SsaVal::Raw(builder.ins().srem(a, b), LIT_TAG_INT))
        }

        // Int bitwise
        PrimOpKind::IntAnd => {
            check_arity(op, 2, args.len())?;
            let a = unbox_int(pipeline, builder, args[0]);
            let b = unbox_int(pipeline, builder, args[1]);
            Ok(SsaVal::Raw(builder.ins().band(a, b), LIT_TAG_INT))
        }
        PrimOpKind::IntOr => {
            check_arity(op, 2, args.len())?;
            let a = unbox_int(pipeline, builder, args[0]);
            let b = unbox_int(pipeline, builder, args[1]);
            Ok(SsaVal::Raw(builder.ins().bor(a, b), LIT_TAG_INT))
        }
        PrimOpKind::IntXor => {
            check_arity(op, 2, args.len())?;
            let a = unbox_int(pipeline, builder, args[0]);
            let b = unbox_int(pipeline, builder, args[1]);
            Ok(SsaVal::Raw(builder.ins().bxor(a, b), LIT_TAG_INT))
        }
        PrimOpKind::IntNot => {
            check_arity(op, 1, args.len())?;
            let a = unbox_int(pipeline, builder, args[0]);
            Ok(SsaVal::Raw(builder.ins().bnot(a), LIT_TAG_INT))
        }

        // Int shifts
        PrimOpKind::IntShl => {
            check_arity(op, 2, args.len())?;
            let a = unbox_int(pipeline, builder, args[0]);
            let b = unbox_int(pipeline, builder, args[1]);
            Ok(SsaVal::Raw(builder.ins().ishl(a, b), LIT_TAG_INT))
        }
        PrimOpKind::IntShra => {
            check_arity(op, 2, args.len())?;
            let a = unbox_int(pipeline, builder, args[0]);
            let b = unbox_int(pipeline, builder, args[1]);
            Ok(SsaVal::Raw(builder.ins().sshr(a, b), LIT_TAG_INT))
        }
        PrimOpKind::IntShrl => {
            check_arity(op, 2, args.len())?;
            let a = unbox_int(pipeline, builder, args[0]);
            let b = unbox_int(pipeline, builder, args[1]);
            Ok(SsaVal::Raw(builder.ins().ushr(a, b), LIT_TAG_INT))
        }

        // Int comparison \u2192 returns i64 (0=False, 1=True)
        PrimOpKind::IntEq => emit_int_compare(pipeline, builder, op, IntCC::Equal, args, LIT_TAG_INT),
        PrimOpKind::IntNe => emit_int_compare(pipeline, builder, op, IntCC::NotEqual, args, LIT_TAG_INT),
        PrimOpKind::IntLt => {
            emit_int_compare(pipeline, builder, op, IntCC::SignedLessThan, args, LIT_TAG_INT)
        }
        PrimOpKind::IntLe => {
            emit_int_compare(pipeline, builder, op, IntCC::SignedLessThanOrEqual, args, LIT_TAG_INT)
        }
        PrimOpKind::IntGt => {
            emit_int_compare(pipeline, builder, op, IntCC::SignedGreaterThan, args, LIT_TAG_INT)
        }
        PrimOpKind::IntGe => emit_int_compare(
            pipeline,
            builder,
            op,
            IntCC::SignedGreaterThanOrEqual,
            args,
            LIT_TAG_INT,
        ),

        // Word arithmetic
        PrimOpKind::WordAdd => {
            check_arity(op, 2, args.len())?;
            let a = unbox_int(pipeline, builder, args[0]);
            let b = unbox_int(pipeline, builder, args[1]);
            Ok(SsaVal::Raw(builder.ins().iadd(a, b), LIT_TAG_WORD))
        }
        PrimOpKind::WordSub => {
            check_arity(op, 2, args.len())?;
            let a = unbox_int(pipeline, builder, args[0]);
            let b = unbox_int(pipeline, builder, args[1]);
            Ok(SsaVal::Raw(builder.ins().isub(a, b), LIT_TAG_WORD))
        }
        PrimOpKind::WordMul => {
            check_arity(op, 2, args.len())?;
            let a = unbox_int(pipeline, builder, args[0]);
            let b = unbox_int(pipeline, builder, args[1]);
            Ok(SsaVal::Raw(builder.ins().imul(a, b), LIT_TAG_WORD))
        }

        PrimOpKind::WordQuot => {
            check_arity(op, 2, args.len())?;
            let a = unbox_int(pipeline, builder, args[0]);
            let b = unbox_int(pipeline, builder, args[1]);
            Ok(SsaVal::Raw(builder.ins().udiv(a, b), LIT_TAG_WORD))
        }
        PrimOpKind::WordRem => {
            check_arity(op, 2, args.len())?;
            let a = unbox_int(pipeline, builder, args[0]);
            let b = unbox_int(pipeline, builder, args[1]);
            Ok(SsaVal::Raw(builder.ins().urem(a, b), LIT_TAG_WORD))
        }

        // Word bitwise
        PrimOpKind::WordAnd => {
            check_arity(op, 2, args.len())?;
            let a = unbox_int(pipeline, builder, args[0]);
            let b = unbox_int(pipeline, builder, args[1]);
            Ok(SsaVal::Raw(builder.ins().band(a, b), LIT_TAG_WORD))
        }
        PrimOpKind::WordOr => {
            check_arity(op, 2, args.len())?;
            let a = unbox_int(pipeline, builder, args[0]);
            let b = unbox_int(pipeline, builder, args[1]);
            Ok(SsaVal::Raw(builder.ins().bor(a, b), LIT_TAG_WORD))
        }
        PrimOpKind::WordXor => {
            check_arity(op, 2, args.len())?;
            let a = unbox_int(pipeline, builder, args[0]);
            let b = unbox_int(pipeline, builder, args[1]);
            Ok(SsaVal::Raw(builder.ins().bxor(a, b), LIT_TAG_WORD))
        }
        PrimOpKind::WordNot => {
            check_arity(op, 1, args.len())?;
            let a = unbox_int(pipeline, builder, args[0]);
            Ok(SsaVal::Raw(builder.ins().bnot(a), LIT_TAG_WORD))
        }

        // Word shifts
        PrimOpKind::WordShl => {
            check_arity(op, 2, args.len())?;
            let a = unbox_int(pipeline, builder, args[0]);
            let b = unbox_int(pipeline, builder, args[1]);
            Ok(SsaVal::Raw(builder.ins().ishl(a, b), LIT_TAG_WORD))
        }
        PrimOpKind::WordShrl => {
            check_arity(op, 2, args.len())?;
            let a = unbox_int(pipeline, builder, args[0]);
            let b = unbox_int(pipeline, builder, args[1]);
            Ok(SsaVal::Raw(builder.ins().ushr(a, b), LIT_TAG_WORD))
        }

        // Word comparison (unsigned)
        PrimOpKind::WordEq => emit_int_compare(pipeline, builder, op, IntCC::Equal, args, LIT_TAG_INT),
        PrimOpKind::WordNe => emit_int_compare(pipeline, builder, op, IntCC::NotEqual, args, LIT_TAG_INT),
        PrimOpKind::WordLt => {
            emit_int_compare(pipeline, builder, op, IntCC::UnsignedLessThan, args, LIT_TAG_INT)
        }
        PrimOpKind::WordLe => emit_int_compare(
            pipeline,
            builder,
            op,
            IntCC::UnsignedLessThanOrEqual,
            args,
            LIT_TAG_INT,
        ),
        PrimOpKind::WordGt => {
            emit_int_compare(pipeline, builder, op, IntCC::UnsignedGreaterThan, args, LIT_TAG_INT)
        }
        PrimOpKind::WordGe => emit_int_compare(
            pipeline,
            builder,
            op,
            IntCC::UnsignedGreaterThanOrEqual,
            args,
            LIT_TAG_INT,
        ),

        // Double arithmetic
        PrimOpKind::DoubleAdd => {
            check_arity(op, 2, args.len())?;
            let a = unbox_double(pipeline, builder, args[0]);
            let b = unbox_double(pipeline, builder, args[1]);
            Ok(SsaVal::Raw(builder.ins().fadd(a, b), LIT_TAG_DOUBLE))
        }
        PrimOpKind::DoubleSub => {
            check_arity(op, 2, args.len())?;
            let a = unbox_double(pipeline, builder, args[0]);
            let b = unbox_double(pipeline, builder, args[1]);
            Ok(SsaVal::Raw(builder.ins().fsub(a, b), LIT_TAG_DOUBLE))
        }
        PrimOpKind::DoubleMul => {
            check_arity(op, 2, args.len())?;
            let a = unbox_double(pipeline, builder, args[0]);
            let b = unbox_double(pipeline, builder, args[1]);
            Ok(SsaVal::Raw(builder.ins().fmul(a, b), LIT_TAG_DOUBLE))
        }
        PrimOpKind::DoubleDiv => {
            check_arity(op, 2, args.len())?;
            let a = unbox_double(pipeline, builder, args[0]);
            let b = unbox_double(pipeline, builder, args[1]);
            Ok(SsaVal::Raw(builder.ins().fdiv(a, b), LIT_TAG_DOUBLE))
        }

        // Double comparison
        PrimOpKind::DoubleEq => emit_float_compare(pipeline, builder, op, FloatCC::Equal, args, LIT_TAG_INT),
        PrimOpKind::DoubleNe => {
            emit_float_compare(pipeline, builder, op, FloatCC::NotEqual, args, LIT_TAG_INT)
        }
        PrimOpKind::DoubleLt => {
            emit_float_compare(pipeline, builder, op, FloatCC::LessThan, args, LIT_TAG_INT)
        }
        PrimOpKind::DoubleLe => {
            emit_float_compare(pipeline, builder, op, FloatCC::LessThanOrEqual, args, LIT_TAG_INT)
        }
        PrimOpKind::DoubleGt => {
            emit_float_compare(pipeline, builder, op, FloatCC::GreaterThan, args, LIT_TAG_INT)
        }
        PrimOpKind::DoubleGe => {
            emit_float_compare(pipeline, builder, op, FloatCC::GreaterThanOrEqual, args, LIT_TAG_INT)
        }

        // Char comparison
        PrimOpKind::CharEq => emit_int_compare(pipeline, builder, op, IntCC::Equal, args, LIT_TAG_INT),
        PrimOpKind::CharNe => emit_int_compare(pipeline, builder, op, IntCC::NotEqual, args, LIT_TAG_INT),
        PrimOpKind::CharLt => {
            emit_int_compare(pipeline, builder, op, IntCC::UnsignedLessThan, args, LIT_TAG_INT)
        }
        PrimOpKind::CharLe => emit_int_compare(
            pipeline,
            builder,
            op,
            IntCC::UnsignedLessThanOrEqual,
            args,
            LIT_TAG_INT,
        ),
        PrimOpKind::CharGt => {
            emit_int_compare(pipeline, builder, op, IntCC::UnsignedGreaterThan, args, LIT_TAG_INT)
        }
        PrimOpKind::CharGe => emit_int_compare(
            pipeline,
            builder,
            op,
            IntCC::UnsignedGreaterThanOrEqual,
            args,
            LIT_TAG_INT,
        ),

        // Conversions
        PrimOpKind::Chr => {
            check_arity(op, 1, args.len())?;
            let v = unbox_int(pipeline, builder, args[0]);
            Ok(SsaVal::Raw(v, LIT_TAG_CHAR))
        }
        PrimOpKind::Ord => {
            check_arity(op, 1, args.len())?;
            let v = unbox_int(pipeline, builder, args[0]);
            Ok(SsaVal::Raw(v, LIT_TAG_INT))
        }
        PrimOpKind::Int2Word | PrimOpKind::Word2Int => {
            check_arity(op, 1, args.len())?;
            let v = unbox_int(pipeline, builder, args[0]);
            let tag = if matches!(op, PrimOpKind::Int2Word) {
                LIT_TAG_WORD
            } else {
                LIT_TAG_INT
            };
            Ok(SsaVal::Raw(v, tag))
        }
        PrimOpKind::Int2Double => {
            check_arity(op, 1, args.len())?;
            let v = unbox_int(pipeline, builder, args[0]);
            Ok(SsaVal::Raw(
                builder.ins().fcvt_from_sint(types::F64, v),
                LIT_TAG_DOUBLE,
            ))
        }
        PrimOpKind::Double2Int => {
            check_arity(op, 1, args.len())?;
            let v = unbox_double(pipeline, builder, args[0]);
            Ok(SsaVal::Raw(
                builder.ins().fcvt_to_sint_sat(types::I64, v),
                LIT_TAG_INT,
            ))
        }
        PrimOpKind::Int2Float => {
            check_arity(op, 1, args.len())?;
            let v = unbox_int(pipeline, builder, args[0]);
            Ok(SsaVal::Raw(
                builder.ins().fcvt_from_sint(types::F32, v),
                LIT_TAG_FLOAT,
            ))
        }
        PrimOpKind::Float2Int => {
            check_arity(op, 1, args.len())?;
            let v = unbox_float(pipeline, builder, args[0]);
            Ok(SsaVal::Raw(
                builder.ins().fcvt_to_sint_sat(types::I64, v),
                LIT_TAG_INT,
            ))
        }
        PrimOpKind::Double2Float => {
            check_arity(op, 1, args.len())?;
            let v = unbox_double(pipeline, builder, args[0]);
            Ok(SsaVal::Raw(
                builder.ins().fdemote(types::F32, v),
                LIT_TAG_FLOAT,
            ))
        }
        PrimOpKind::Float2Double => {
            check_arity(op, 1, args.len())?;
            let v = unbox_float(pipeline, builder, args[0]);
            Ok(SsaVal::Raw(
                builder.ins().fpromote(types::F64, v),
                LIT_TAG_DOUBLE,
            ))
        }

        // Narrowing
        PrimOpKind::Narrow8Int => {
            check_arity(op, 1, args.len())?;
            let v = unbox_int(pipeline, builder, args[0]);
            let narrow = builder.ins().ireduce(types::I8, v);
            Ok(SsaVal::Raw(
                builder.ins().sextend(types::I64, narrow),
                LIT_TAG_INT,
            ))
        }
        PrimOpKind::Narrow16Int => {
            check_arity(op, 1, args.len())?;
            let v = unbox_int(pipeline, builder, args[0]);
            let narrow = builder.ins().ireduce(types::I16, v);
            Ok(SsaVal::Raw(
                builder.ins().sextend(types::I64, narrow),
                LIT_TAG_INT,
            ))
        }
        PrimOpKind::Narrow32Int => {
            check_arity(op, 1, args.len())?;
            let v = unbox_int(pipeline, builder, args[0]);
            let narrow = builder.ins().ireduce(types::I32, v);
            Ok(SsaVal::Raw(
                builder.ins().sextend(types::I64, narrow),
                LIT_TAG_INT,
            ))
        }
        PrimOpKind::Narrow8Word => {
            check_arity(op, 1, args.len())?;
            let v = unbox_int(pipeline, builder, args[0]);
            let narrow = builder.ins().ireduce(types::I8, v);
            Ok(SsaVal::Raw(
                builder.ins().uextend(types::I64, narrow),
                LIT_TAG_WORD,
            ))
        }
        PrimOpKind::Narrow16Word => {
            check_arity(op, 1, args.len())?;
            let v = unbox_int(pipeline, builder, args[0]);
            let narrow = builder.ins().ireduce(types::I16, v);
            Ok(SsaVal::Raw(
                builder.ins().uextend(types::I64, narrow),
                LIT_TAG_WORD,
            ))
        }
        PrimOpKind::Narrow32Word => {
            check_arity(op, 1, args.len())?;
            let v = unbox_int(pipeline, builder, args[0]);
            let narrow = builder.ins().ireduce(types::I32, v);
            Ok(SsaVal::Raw(
                builder.ins().uextend(types::I64, narrow),
                LIT_TAG_WORD,
            ))
        }

        // Special ops
        PrimOpKind::DataToTag => {
            check_arity(op, 1, args.len())?;
            let obj = args[0].value();
            let tag = builder
                .ins()
                .load(types::I64, MemFlags::trusted(), obj, CON_TAG_OFFSET);
            Ok(SsaVal::Raw(tag, LIT_TAG_INT))
        }
        PrimOpKind::DoubleNegate => {
            check_arity(op, 1, args.len())?;
            let a = unbox_double(pipeline, builder, args[0]);
            Ok(SsaVal::Raw(builder.ins().fneg(a), LIT_TAG_DOUBLE))
        }
        PrimOpKind::FloatNegate => {
            check_arity(op, 1, args.len())?;
            let a = unbox_float(pipeline, builder, args[0]);
            Ok(SsaVal::Raw(builder.ins().fneg(a), LIT_TAG_FLOAT))
        }

        PrimOpKind::ReallyUnsafePtrEquality => {
            check_arity(op, 2, args.len())?;
            Ok(SsaVal::Raw(
                builder.ins().iconst(types::I64, 0),
                LIT_TAG_INT,
            ))
        }

        PrimOpKind::IndexCharOffAddr => {
            check_arity(op, 2, args.len())?;
            let addr = unbox_addr(pipeline, builder, args[0]);
            let idx = unbox_int(pipeline, builder, args[1]);
            let effective = builder.ins().iadd(addr, idx);
            let byte_val = builder.ins().load(types::I8, MemFlags::new(), effective, 0);
            let char_val = builder.ins().uextend(types::I64, byte_val);
            Ok(SsaVal::Raw(char_val, LIT_TAG_CHAR))
        }

        PrimOpKind::PlusAddr => {
            check_arity(op, 2, args.len())?;
            let addr = unbox_addr(pipeline, builder, args[0]);
            let offset = unbox_int(pipeline, builder, args[1]);
            Ok(SsaVal::Raw(builder.ins().iadd(addr, offset), LIT_TAG_ADDR))
        }

        // ---------------------------------------------------------------
        // Int64/Word64/Word8 \u2014 on 64-bit, these are just Int#/Word# with
        // different tags. GHC treats them identically at runtime.
        // ---------------------------------------------------------------

        // Int64 arithmetic
        PrimOpKind::Int64Add => {
            check_arity(op, 2, args.len())?;
            let a = unbox_int(pipeline, builder, args[0]);
            let b = unbox_int(pipeline, builder, args[1]);
            Ok(SsaVal::Raw(builder.ins().iadd(a, b), LIT_TAG_INT))
        }
        PrimOpKind::Int64Sub => {
            check_arity(op, 2, args.len())?;
            let a = unbox_int(pipeline, builder, args[0]);
            let b = unbox_int(pipeline, builder, args[1]);
            Ok(SsaVal::Raw(builder.ins().isub(a, b), LIT_TAG_INT))
        }
        PrimOpKind::Int64Mul => {
            check_arity(op, 2, args.len())?;
            let a = unbox_int(pipeline, builder, args[0]);
            let b = unbox_int(pipeline, builder, args[1]);
            Ok(SsaVal::Raw(builder.ins().imul(a, b), LIT_TAG_INT))
        }
        PrimOpKind::Int64Negate => {
            check_arity(op, 1, args.len())?;
            let a = unbox_int(pipeline, builder, args[0]);
            Ok(SsaVal::Raw(builder.ins().ineg(a), LIT_TAG_INT))
        }
        PrimOpKind::Int64Shl => {
            check_arity(op, 2, args.len())?;
            let a = unbox_int(pipeline, builder, args[0]);
            let b = unbox_int(pipeline, builder, args[1]);
            Ok(SsaVal::Raw(builder.ins().ishl(a, b), LIT_TAG_INT))
        }
        PrimOpKind::Int64Shra => {
            check_arity(op, 2, args.len())?;
            let a = unbox_int(pipeline, builder, args[0]);
            let b = unbox_int(pipeline, builder, args[1]);
            Ok(SsaVal::Raw(builder.ins().sshr(a, b), LIT_TAG_INT))
        }

        // Int64 comparison
        PrimOpKind::Int64Lt => {
            emit_int_compare(pipeline, builder, op, IntCC::SignedLessThan, args, LIT_TAG_INT)
        }
        PrimOpKind::Int64Le => {
            emit_int_compare(pipeline, builder, op, IntCC::SignedLessThanOrEqual, args, LIT_TAG_INT)
        }
        PrimOpKind::Int64Gt => {
            emit_int_compare(pipeline, builder, op, IntCC::SignedGreaterThan, args, LIT_TAG_INT)
        }
        PrimOpKind::Int64Ge => emit_int_compare(
            pipeline,
            builder,
            op,
            IntCC::SignedGreaterThanOrEqual,
            args,
            LIT_TAG_INT,
        ),

        // Word64 arithmetic/bitwise
        PrimOpKind::Word64And => {
            check_arity(op, 2, args.len())?;
            let a = unbox_int(pipeline, builder, args[0]);
            let b = unbox_int(pipeline, builder, args[1]);
            Ok(SsaVal::Raw(builder.ins().band(a, b), LIT_TAG_WORD))
        }
        PrimOpKind::Word64Shl => {
            check_arity(op, 2, args.len())?;
            let a = unbox_int(pipeline, builder, args[0]);
            let b = unbox_int(pipeline, builder, args[1]);
            Ok(SsaVal::Raw(builder.ins().ishl(a, b), LIT_TAG_WORD))
        }
        PrimOpKind::Word64Or => {
            check_arity(op, 2, args.len())?;
            let a = unbox_int(pipeline, builder, args[0]);
            let b = unbox_int(pipeline, builder, args[1]);
            Ok(SsaVal::Raw(builder.ins().bor(a, b), LIT_TAG_WORD))
        }

        // Conversions between sized int/word types (no-ops on 64-bit)
        PrimOpKind::Word64ToInt64 | PrimOpKind::Int64ToInt | PrimOpKind::Int64ToWord64 => {
            check_arity(op, 1, args.len())?;
            let v = unbox_int(pipeline, builder, args[0]);
            Ok(SsaVal::Raw(v, LIT_TAG_INT))
        }
        PrimOpKind::IntToInt64 => {
            check_arity(op, 1, args.len())?;
            let v = unbox_int(pipeline, builder, args[0]);
            Ok(SsaVal::Raw(v, LIT_TAG_INT))
        }
        PrimOpKind::Word8ToWord => {
            check_arity(op, 1, args.len())?;
            let v = unbox_int(pipeline, builder, args[0]);
            Ok(SsaVal::Raw(v, LIT_TAG_WORD))
        }
        PrimOpKind::WordToWord8 => {
            // wordToWord8# :: Word# -> Word8#
            // Narrow to 8 bits
            check_arity(op, 1, args.len())?;
            let v = unbox_int(pipeline, builder, args[0]);
            let mask = builder.ins().iconst(types::I64, 0xFF);
            let narrow = builder.ins().band(v, mask);
            Ok(SsaVal::Raw(narrow, LIT_TAG_WORD))
        }

        // Word8 arithmetic/comparison
        PrimOpKind::Word8Add => {
            check_arity(op, 2, args.len())?;
            let a = unbox_int(pipeline, builder, args[0]);
            let b = unbox_int(pipeline, builder, args[1]);
            let sum = builder.ins().iadd(a, b);
            Ok(SsaVal::Raw(builder.ins().band_imm(sum, 0xFF), LIT_TAG_WORD))
        }
        PrimOpKind::Word8Sub => {
            check_arity(op, 2, args.len())?;
            let a = unbox_int(pipeline, builder, args[0]);
            let b = unbox_int(pipeline, builder, args[1]);
            let diff = builder.ins().isub(a, b);
            Ok(SsaVal::Raw(
                builder.ins().band_imm(diff, 0xFF),
                LIT_TAG_WORD,
            ))
        }
        PrimOpKind::Word8Lt => {
            emit_int_compare(pipeline, builder, op, IntCC::UnsignedLessThan, args, LIT_TAG_INT)
        }
        PrimOpKind::Word8Le => emit_int_compare(
            pipeline,
            builder,
            op,
            IntCC::UnsignedLessThanOrEqual,
            args,
            LIT_TAG_INT,
        ),
        PrimOpKind::Word8Ge => emit_int_compare(
            pipeline,
            builder,
            op,
            IntCC::UnsignedGreaterThanOrEqual,
            args,
            LIT_TAG_INT,
        ),

        // ---------------------------------------------------------------
        // Carry/overflow arithmetic
        // ---------------------------------------------------------------

        // addIntC# :: Int# -> Int# -> (# Int#, Int# #)
        // We emit just the value or carry depending on which variant.
        PrimOpKind::AddIntCVal => {
            check_arity(op, 2, args.len())?;
            let a = unbox_int(pipeline, builder, args[0]);
            let b = unbox_int(pipeline, builder, args[1]);
            Ok(SsaVal::Raw(builder.ins().iadd(a, b), LIT_TAG_INT))
        }
        PrimOpKind::AddIntCCarry => {
            check_arity(op, 2, args.len())?;
            let a = unbox_int(pipeline, builder, args[0]);
            let b = unbox_int(pipeline, builder, args[1]);
            let sum = builder.ins().iadd(a, b);
            // Signed overflow: (a > 0 && b > 0 && sum < 0) || (a < 0 && b < 0 && sum >= 0)
            // Simplified: overflow if sign(a) == sign(b) && sign(sum) != sign(a)
            let xor_ab = builder.ins().bxor(a, b);
            let xor_as = builder.ins().bxor(a, sum);
            // If signs of a,b are same (xor_ab bit 63 = 0) AND sign of sum differs from a (xor_as bit 63 = 1)
            let not_xor_ab = builder.ins().bnot(xor_ab);
            let overflow_bits = builder.ins().band(not_xor_ab, xor_as);
            // Shift bit 63 to bit 0
            let shifted = builder.ins().ushr_imm(overflow_bits, 63);
            Ok(SsaVal::Raw(shifted, LIT_TAG_INT))
        }

        // subWordC# :: Word# -> Word# -> (# Word#, Int# #)
        PrimOpKind::SubWordCVal => {
            check_arity(op, 2, args.len())?;
            let a = unbox_int(pipeline, builder, args[0]);
            let b = unbox_int(pipeline, builder, args[1]);
            Ok(SsaVal::Raw(builder.ins().isub(a, b), LIT_TAG_WORD))
        }
        PrimOpKind::SubWordCCarry => {
            check_arity(op, 2, args.len())?;
            let a = unbox_int(pipeline, builder, args[0]);
            let b = unbox_int(pipeline, builder, args[1]);
            // Borrow if a < b (unsigned)
            let borrow = builder.ins().icmp(IntCC::UnsignedLessThan, a, b);
            Ok(SsaVal::Raw(
                builder.ins().uextend(types::I64, borrow),
                LIT_TAG_INT,
            ))
        }

        // addWordC# :: Word# -> Word# -> (# Word#, Int# #)
        PrimOpKind::AddWordCVal => {
            check_arity(op, 2, args.len())?;
            let a = unbox_int(pipeline, builder, args[0]);
            let b = unbox_int(pipeline, builder, args[1]);
            Ok(SsaVal::Raw(builder.ins().iadd(a, b), LIT_TAG_WORD))
        }
        PrimOpKind::AddWordCCarry => {
            check_arity(op, 2, args.len())?;
            let a = unbox_int(pipeline, builder, args[0]);
            let b = unbox_int(pipeline, builder, args[1]);
            let sum = builder.ins().iadd(a, b);
            // Carry if sum < a (unsigned)
            let carry = builder.ins().icmp(IntCC::UnsignedLessThan, sum, a);
            Ok(SsaVal::Raw(
                builder.ins().uextend(types::I64, carry),
                LIT_TAG_INT,
            ))
        }

        // timesInt2# :: Int# -> Int# -> (# Int#, Int#, Int# #)
        // Signed widening multiply: (hi, lo, overflow)
        PrimOpKind::TimesInt2Hi => {
            check_arity(op, 2, args.len())?;
            let a = unbox_int(pipeline, builder, args[0]);
            let b = unbox_int(pipeline, builder, args[1]);
            Ok(SsaVal::Raw(builder.ins().smulhi(a, b), LIT_TAG_INT))
        }
        PrimOpKind::TimesInt2Lo => {
            check_arity(op, 2, args.len())?;
            let a = unbox_int(pipeline, builder, args[0]);
            let b = unbox_int(pipeline, builder, args[1]);
            Ok(SsaVal::Raw(builder.ins().imul(a, b), LIT_TAG_INT))
        }
        PrimOpKind::TimesInt2Overflow => {
            check_arity(op, 2, args.len())?;
            let a = unbox_int(pipeline, builder, args[0]);
            let b = unbox_int(pipeline, builder, args[1]);
            // Overflow if smulhi(a,b) != (imul(a,b) >>s 63)
            // i.e., the high word differs from sign-extending the low word
            let hi = builder.ins().smulhi(a, b);
            let lo = builder.ins().imul(a, b);
            let lo_sign = builder.ins().sshr_imm(lo, 63);
            let overflow = builder.ins().icmp(IntCC::NotEqual, hi, lo_sign);
            Ok(SsaVal::Raw(
                builder.ins().uextend(types::I64, overflow),
                LIT_TAG_INT,
            ))
        }

        // timesWord2# :: Word# -> Word# -> (# Word#, Word# #)
        // High and low words of 128-bit multiply
        PrimOpKind::TimesWord2Hi => {
            check_arity(op, 2, args.len())?;
            let a = unbox_int(pipeline, builder, args[0]);
            let b = unbox_int(pipeline, builder, args[1]);
            Ok(SsaVal::Raw(builder.ins().umulhi(a, b), LIT_TAG_WORD))
        }
        PrimOpKind::TimesWord2Lo => {
            check_arity(op, 2, args.len())?;
            let a = unbox_int(pipeline, builder, args[0]);
            let b = unbox_int(pipeline, builder, args[1]);
            Ok(SsaVal::Raw(builder.ins().imul(a, b), LIT_TAG_WORD))
        }

        // quotRemWord# :: Word# -> Word# -> (# Word#, Word# #)
        PrimOpKind::QuotRemWordVal => {
            check_arity(op, 2, args.len())?;
            let a = unbox_int(pipeline, builder, args[0]);
            let b = unbox_int(pipeline, builder, args[1]);
            Ok(SsaVal::Raw(builder.ins().udiv(a, b), LIT_TAG_WORD))
        }
        PrimOpKind::QuotRemWordRem => {
            check_arity(op, 2, args.len())?;
            let a = unbox_int(pipeline, builder, args[0]);
            let b = unbox_int(pipeline, builder, args[1]);
            Ok(SsaVal::Raw(builder.ins().urem(a, b), LIT_TAG_WORD))
        }

        // ---------------------------------------------------------------
        // ByteArray# primops \u2014 mutable byte arrays for Data.Text etc.
        // ByteArray is stored as a Lit with LIT_TAG_BYTEARRAY, value = ptr
        // to malloc'd buffer: [u64 length][u8 bytes...]
        // ---------------------------------------------------------------
        PrimOpKind::NewByteArray => {
            // newByteArray# :: Int# -> State# s -> (# State# s, MutableByteArray# s #)
            // State# token may or may not be passed (1 or 2 args)
            let size = unbox_int(pipeline, builder, args[0]);
            let ba_ptr = emit_runtime_call(
                pipeline,
                builder,
                "runtime_new_byte_array",
                &[AbiParam::new(types::I64)],
                &[AbiParam::new(types::I64)],
                &[size],
            )?;
            // Wrap in a Lit on the managed heap
            Ok(emit_lit_bytearray(builder, vmctx, gc_sig, oom_func, ba_ptr))
        }

        PrimOpKind::UnsafeFreezeByteArray => {
            // unsafeFreezeByteArray# :: MutableByteArray# s -> State# s -> (# State# s, ByteArray# #)
            // Identity \u2014 mutable and immutable have the same representation
            Ok(args[0])
        }

        PrimOpKind::SizeofByteArray | PrimOpKind::SizeofMutableByteArray => {
            // sizeofByteArray# :: ByteArray# -> Int#
            let ba_ptr = unbox_bytearray(pipeline, builder, args[0]);
            // Read u64 length from offset 0
            let len = builder.ins().load(types::I64, MemFlags::new(), ba_ptr, 0);
            Ok(SsaVal::Raw(len, LIT_TAG_INT))
        }

        PrimOpKind::ReadWord8Array | PrimOpKind::IndexWord8Array => {
            // readWord8Array# :: MutableByteArray# s -> Int# -> State# s -> (# State# s, Word# #)
            let ba_ptr = unbox_bytearray(pipeline, builder, args[0]);
            let idx = unbox_int(pipeline, builder, args[1]);
            // Data starts at offset 8
            let base = builder.ins().iadd_imm(ba_ptr, 8);
            let effective = builder.ins().iadd(base, idx);
            let byte = builder.ins().load(types::I8, MemFlags::new(), effective, 0);
            let val = builder.ins().uextend(types::I64, byte);
            Ok(SsaVal::Raw(val, LIT_TAG_WORD))
        }

        PrimOpKind::WriteWord8Array => {
            // writeWord8Array# :: MutableByteArray# s -> Int# -> Word# -> State# s -> State# s
            let ba_ptr = unbox_bytearray(pipeline, builder, args[0]);
            let idx = unbox_int(pipeline, builder, args[1]);
            let val = unbox_int(pipeline, builder, args[2]);
            let base = builder.ins().iadd_imm(ba_ptr, 8);
            let effective = builder.ins().iadd(base, idx);
            let byte = builder.ins().ireduce(types::I8, val);
            builder.ins().store(MemFlags::new(), byte, effective, 0);
            // Return dummy state token
            Ok(SsaVal::Raw(
                builder.ins().iconst(types::I64, 0),
                LIT_TAG_INT,
            ))
        }

        PrimOpKind::IndexWordArray | PrimOpKind::ReadWordArray => {
            // indexWordArray# :: ByteArray# -> Int# -> Word#
            let ba_ptr = unbox_bytearray(pipeline, builder, args[0]);
            let idx = unbox_int(pipeline, builder, args[1]);
            let base = builder.ins().iadd_imm(ba_ptr, 8);
            // Word-sized (8 bytes) indexing
            let byte_offset = builder.ins().imul_imm(idx, 8);
            let effective = builder.ins().iadd(base, byte_offset);
            let word = builder
                .ins()
                .load(types::I64, MemFlags::new(), effective, 0);
            Ok(SsaVal::Raw(word, LIT_TAG_WORD))
        }

        PrimOpKind::WriteWordArray => {
            // writeWordArray# :: MutableByteArray# s -> Int# -> Word# -> State# s -> State# s
            let ba_ptr = unbox_bytearray(pipeline, builder, args[0]);
            let idx = unbox_int(pipeline, builder, args[1]);
            let val = unbox_int(pipeline, builder, args[2]);
            let base = builder.ins().iadd_imm(ba_ptr, 8);
            let byte_offset = builder.ins().imul_imm(idx, 8);
            let effective = builder.ins().iadd(base, byte_offset);
            builder.ins().store(MemFlags::new(), val, effective, 0);
            Ok(SsaVal::Raw(
                builder.ins().iconst(types::I64, 0),
                LIT_TAG_INT,
            ))
        }

        PrimOpKind::CopyAddrToByteArray => {
            // copyAddrToByteArray# :: Addr# -> MutableByteArray# s -> Int# -> Int# -> State# s -> State# s
            let src = unbox_addr(pipeline, builder, args[0]);
            let dest_ba = unbox_bytearray(pipeline, builder, args[1]);
            let dest_off = unbox_int(pipeline, builder, args[2]);
            let len = unbox_int(pipeline, builder, args[3]);
            let _ = emit_runtime_call(
                pipeline,
                builder,
                "runtime_copy_addr_to_byte_array",
                &[
                    AbiParam::new(types::I64),
                    AbiParam::new(types::I64),
                    AbiParam::new(types::I64),
                    AbiParam::new(types::I64),
                ],
                &[],
                &[src, dest_ba, dest_off, len],
            )?;
            Ok(SsaVal::Raw(
                builder.ins().iconst(types::I64, 0),
                LIT_TAG_INT,
            ))
        }

        PrimOpKind::SetByteArray => {
            // setByteArray# :: MutableByteArray# s -> Int# -> Int# -> Int# -> State# s -> State# s
            let ba = unbox_bytearray(pipeline, builder, args[0]);
            let off = unbox_int(pipeline, builder, args[1]);
            let len = unbox_int(pipeline, builder, args[2]);
            let val = unbox_int(pipeline, builder, args[3]);
            let _ = emit_runtime_call(
                pipeline,
                builder,
                "runtime_set_byte_array",
                &[
                    AbiParam::new(types::I64),
                    AbiParam::new(types::I64),
                    AbiParam::new(types::I64),
                    AbiParam::new(types::I64),
                ],
                &[],
                &[ba, off, len, val],
            )?;
            Ok(SsaVal::Raw(
                builder.ins().iconst(types::I64, 0),
                LIT_TAG_INT,
            ))
        }

        PrimOpKind::ShrinkMutableByteArray => {
            // shrinkMutableByteArray# :: MutableByteArray# s -> Int# -> State# s -> State# s
            let ba = unbox_bytearray(pipeline, builder, args[0]);
            let new_size = unbox_int(pipeline, builder, args[1]);
            let _ = emit_runtime_call(
                pipeline,
                builder,
                "runtime_shrink_byte_array",
                &[AbiParam::new(types::I64), AbiParam::new(types::I64)],
                &[],
                &[ba, new_size],
            )?;
            Ok(SsaVal::Raw(
                builder.ins().iconst(types::I64, 0),
                LIT_TAG_INT,
            ))
        }
        PrimOpKind::ResizeMutableByteArray => {
            // resizeMutableByteArray# :: MutableByteArray# s -> Int# -> State# s
            //   -> (# State# s, MutableByteArray# s #)
            // Returns the (possibly reallocated) byte array pointer.
            let ba = unbox_bytearray(pipeline, builder, args[0]);
            let new_size = unbox_int(pipeline, builder, args[1]);
            let result = emit_runtime_call(
                pipeline,
                builder,
                "runtime_resize_byte_array",
                &[AbiParam::new(types::I64), AbiParam::new(types::I64)],
                &[AbiParam::new(types::I64)],
                &[ba, new_size],
            )?;
            Ok(SsaVal::Raw(result, LIT_TAG_BYTEARRAY))
        }

        PrimOpKind::CopyByteArray => {
            // copyByteArray# :: ByteArray# -> Int# -> MutableByteArray# s -> Int# -> Int# -> State# s -> State# s
            // src, src_off, dest, dest_off, len
            let src = unbox_bytearray(pipeline, builder, args[0]);
            let src_off = unbox_int(pipeline, builder, args[1]);
            let dest = unbox_bytearray(pipeline, builder, args[2]);
            let dest_off = unbox_int(pipeline, builder, args[3]);
            let len = unbox_int(pipeline, builder, args[4]);
            let _ = emit_runtime_call(
                pipeline,
                builder,
                "runtime_copy_byte_array",
                &[
                    AbiParam::new(types::I64),
                    AbiParam::new(types::I64),
                    AbiParam::new(types::I64),
                    AbiParam::new(types::I64),
                    AbiParam::new(types::I64),
                ],
                &[],
                &[src, src_off, dest, dest_off, len],
            )?;
            Ok(SsaVal::Raw(
                builder.ins().iconst(types::I64, 0),
                LIT_TAG_INT,
            ))
        }
        PrimOpKind::CopyMutableByteArray => {
            // copyMutableByteArray# \u2014 same args as CopyByteArray
            let src = unbox_bytearray(pipeline, builder, args[0]);
            let src_off = unbox_int(pipeline, builder, args[1]);
            let dest = unbox_bytearray(pipeline, builder, args[2]);
            let dest_off = unbox_int(pipeline, builder, args[3]);
            let len = unbox_int(pipeline, builder, args[4]);
            let _ = emit_runtime_call(
                pipeline,
                builder,
                "runtime_copy_byte_array",
                &[
                    AbiParam::new(types::I64),
                    AbiParam::new(types::I64),
                    AbiParam::new(types::I64),
                    AbiParam::new(types::I64),
                    AbiParam::new(types::I64),
                ],
                &[],
                &[src, src_off, dest, dest_off, len],
            )?;
            Ok(SsaVal::Raw(
                builder.ins().iconst(types::I64, 0),
                LIT_TAG_INT,
            ))
        }
        PrimOpKind::CompareByteArrays => {
            // compareByteArrays# :: ByteArray# -> Int# -> ByteArray# -> Int# -> Int# -> Int#
            if args.len() != 5 {
                return Err(EmitError::InvalidArity(PrimOpKind::CompareByteArrays, 5, args.len()));
            }
            let a = unbox_bytearray(pipeline, builder, args[0]);
            let a_off = unbox_int(pipeline, builder, args[1]);
            let b = unbox_bytearray(pipeline, builder, args[2]);
            let b_off = unbox_int(pipeline, builder, args[3]);
            let len = unbox_int(pipeline, builder, args[4]);
            let result = emit_runtime_call(
                pipeline,
                builder,
                "runtime_compare_byte_arrays",
                &[
                    AbiParam::new(types::I64),
                    AbiParam::new(types::I64),
                    AbiParam::new(types::I64),
                    AbiParam::new(types::I64),
                    AbiParam::new(types::I64),
                ],
                &[AbiParam::new(types::I64)],
                &[a, a_off, b, b_off, len],
            )?;
            Ok(SsaVal::Raw(result, LIT_TAG_INT))
        }
        PrimOpKind::IndexWord8OffAddr => {
            // indexWord8OffAddr# :: Addr# -> Int# -> Word#
            let addr = unbox_addr(pipeline, builder, args[0]);
            let off = unbox_int(pipeline, builder, args[1]);
            let ptr = builder.ins().iadd(addr, off);
            let byte = builder.ins().load(types::I8, MemFlags::trusted(), ptr, 0);
            let word = builder.ins().uextend(types::I64, byte);
            Ok(SsaVal::Raw(word, LIT_TAG_WORD))
        }
        PrimOpKind::Clz8 => {
            // clz8# :: Word# -> Word#
            check_arity(op, 1, args.len())?;
            let v = unbox_int(pipeline, builder, args[0]);
            let narrow = builder.ins().ireduce(types::I8, v);
            let clz8 = builder.ins().clz(narrow);
            let result = builder.ins().uextend(types::I64, clz8);
            Ok(SsaVal::Raw(result, LIT_TAG_WORD))
        }
        PrimOpKind::Raise => {
            // raise# :: a -> b \u2014 always errors
            let kind = builder.ins().iconst(types::I64, 2); // 2 = UserError
            let _ = emit_runtime_call(
                pipeline,
                builder,
                "runtime_error",
                &[AbiParam::new(types::I64)],
                &[AbiParam::new(types::I64)],
                &[kind],
            )?;
            Ok(SsaVal::Raw(
                builder.ins().iconst(types::I64, 0),
                LIT_TAG_INT,
            ))
        }
        PrimOpKind::FfiStrlen => {
            // strlen :: Addr# -> Int#
            let addr = unbox_addr(pipeline, builder, args[0]);
            let result = emit_runtime_call(
                pipeline,
                builder,
                "runtime_strlen",
                &[AbiParam::new(types::I64)],
                &[AbiParam::new(types::I64)],
                &[addr],
            )?;
            Ok(SsaVal::Raw(result, LIT_TAG_INT))
        }
        PrimOpKind::FfiTextMeasureOff => {
            // _hs_text_measure_off :: ByteArray# -> CSize -> CSize -> CSize -> CSsize
            let ba = unbox_bytearray(pipeline, builder, args[0]);
            let data_ptr = builder.ins().iadd_imm(ba, 8); // skip length prefix
            let off = unbox_int(pipeline, builder, args[1]);
            let len = unbox_int(pipeline, builder, args[2]);
            let cnt = unbox_int(pipeline, builder, args[3]);
            let result = emit_runtime_call(
                pipeline,
                builder,
                "runtime_text_measure_off",
                &[
                    AbiParam::new(types::I64),
                    AbiParam::new(types::I64),
                    AbiParam::new(types::I64),
                    AbiParam::new(types::I64),
                ],
                &[AbiParam::new(types::I64)],
                &[data_ptr, off, len, cnt],
            )?;
            Ok(SsaVal::Raw(result, LIT_TAG_INT))
        }
        PrimOpKind::FfiTextMemchr => {
            // _hs_text_memchr :: ByteArray# -> CSize -> CSize -> Word8 -> CSsize
            let ba = unbox_bytearray(pipeline, builder, args[0]);
            let data_ptr = builder.ins().iadd_imm(ba, 8); // skip length prefix
            let off = unbox_int(pipeline, builder, args[1]);
            let len = unbox_int(pipeline, builder, args[2]);
            let needle = unbox_int(pipeline, builder, args[3]);
            let result = emit_runtime_call(
                pipeline,
                builder,
                "runtime_text_memchr",
                &[
                    AbiParam::new(types::I64),
                    AbiParam::new(types::I64),
                    AbiParam::new(types::I64),
                    AbiParam::new(types::I64),
                ],
                &[AbiParam::new(types::I64)],
                &[data_ptr, off, len, needle],
            )?;
            Ok(SsaVal::Raw(result, LIT_TAG_INT))
        }
        PrimOpKind::FfiTextReverse => {
            // _hs_text_reverse :: MutableByteArray# -> ByteArray# -> CSize -> CSize -> ()
            let dest_ba = unbox_bytearray(pipeline, builder, args[0]);
            let dest_ptr = builder.ins().iadd_imm(dest_ba, 8); // skip length prefix
            let src_ba = unbox_bytearray(pipeline, builder, args[1]);
            let src_ptr = builder.ins().iadd_imm(src_ba, 8); // skip length prefix
            let off = unbox_int(pipeline, builder, args[2]);
            let len = unbox_int(pipeline, builder, args[3]);
            let _ = emit_runtime_call(
                pipeline,
                builder,
                "runtime_text_reverse",
                &[
                    AbiParam::new(types::I64),
                    AbiParam::new(types::I64),
                    AbiParam::new(types::I64),
                    AbiParam::new(types::I64),
                ],
                &[],
                &[dest_ptr, src_ptr, off, len],
            )?;
            Ok(SsaVal::Raw(
                builder.ins().iconst(types::I64, 0),
                LIT_TAG_INT,
            ))
        }

        // Float arithmetic + comparison
        PrimOpKind::FloatAdd => {
            check_arity(op, 2, args.len())?;
            let a = unbox_float(pipeline, builder, args[0]);
            let b = unbox_float(pipeline, builder, args[1]);
            Ok(SsaVal::Raw(builder.ins().fadd(a, b), LIT_TAG_FLOAT))
        }
        PrimOpKind::FloatSub => {
            check_arity(op, 2, args.len())?;
            let a = unbox_float(pipeline, builder, args[0]);
            let b = unbox_float(pipeline, builder, args[1]);
            Ok(SsaVal::Raw(builder.ins().fsub(a, b), LIT_TAG_FLOAT))
        }
        PrimOpKind::FloatMul => {
            check_arity(op, 2, args.len())?;
            let a = unbox_float(pipeline, builder, args[0]);
            let b = unbox_float(pipeline, builder, args[1]);
            Ok(SsaVal::Raw(builder.ins().fmul(a, b), LIT_TAG_FLOAT))
        }
        PrimOpKind::FloatDiv => {
            check_arity(op, 2, args.len())?;
            let a = unbox_float(pipeline, builder, args[0]);
            let b = unbox_float(pipeline, builder, args[1]);
            Ok(SsaVal::Raw(builder.ins().fdiv(a, b), LIT_TAG_FLOAT))
        }
        PrimOpKind::FloatEq => emit_float_compare(pipeline, builder, op, FloatCC::Equal, args, LIT_TAG_INT),
        PrimOpKind::FloatNe => {
            emit_float_compare(pipeline, builder, op, FloatCC::NotEqual, args, LIT_TAG_INT)
        }
        PrimOpKind::FloatLt => {
            emit_float_compare(pipeline, builder, op, FloatCC::LessThan, args, LIT_TAG_INT)
        }
        PrimOpKind::FloatLe => {
            emit_float_compare(pipeline, builder, op, FloatCC::LessThanOrEqual, args, LIT_TAG_INT)
        }
        PrimOpKind::FloatGt => {
            emit_float_compare(pipeline, builder, op, FloatCC::GreaterThan, args, LIT_TAG_INT)
        }
        PrimOpKind::FloatGe => {
            emit_float_compare(pipeline, builder, op, FloatCC::GreaterThanOrEqual, args, LIT_TAG_INT)
        }

        PrimOpKind::TagToEnum | PrimOpKind::SeqOp => {
            Err(EmitError::NotYetImplemented(format!("{:?}", op)))
        }

        // ---------------------------------------------------------------
        // SmallArray# / Array# primops \u2014 boxed pointer arrays
        // Layout: [u64 length][ptr0][ptr1]...[ptrN-1]
        // Stored in Lit with LIT_TAG_SMALLARRAY (8) or LIT_TAG_ARRAY (9)
        // ---------------------------------------------------------------
        PrimOpKind::NewSmallArray | PrimOpKind::NewArray => {
            // newSmallArray# :: Int# -> a -> State# -> (# State#, SmallMutableArray# s a #)
            let size = unbox_int(pipeline, builder, args[0]);
            let init_ptr = args[1].value();
            let arr_ptr = emit_runtime_call(
                pipeline,
                builder,
                "runtime_new_boxed_array",
                &[AbiParam::new(types::I64), AbiParam::new(types::I64)],
                &[AbiParam::new(types::I64)],
                &[size, init_ptr],
            )?;
            let lit_tag = if matches!(op, PrimOpKind::NewSmallArray) {
                LIT_TAG_SMALLARRAY
            } else {
                LIT_TAG_ARRAY
            };
            Ok(emit_lit_boxed_array(builder, vmctx, gc_sig, oom_func, arr_ptr, lit_tag))
        }

        PrimOpKind::ReadSmallArray
        | PrimOpKind::IndexSmallArray
        | PrimOpKind::ReadArray
        | PrimOpKind::IndexArray => {
            // readSmallArray# :: SmallMutableArray# s a -> Int# -> State# -> (# State#, a #)
            // indexSmallArray# :: SmallArray# a -> Int# -> (# a #)
            if args.len() < 2 {
                return Err(EmitError::NotYetImplemented(format!(
                    "{:?}: expected >=2 args, got {} (args: {:?})",
                    op, args.len(), args
                )));
            }
            let arr_ptr = unbox_bytearray(pipeline, builder, args[0]);
            let idx = unbox_int(pipeline, builder, args[1]);
            let base = builder.ins().iadd_imm(arr_ptr, 8);
            let byte_offset = builder.ins().imul_imm(idx, 8);
            let effective = builder.ins().iadd(base, byte_offset);
            let loaded = builder.ins().load(types::I64, MemFlags::new(), effective, 0);
            builder.declare_value_needs_stack_map(loaded);
            Ok(SsaVal::HeapPtr(loaded))
        }

        PrimOpKind::WriteSmallArray | PrimOpKind::WriteArray => {
            // writeSmallArray# :: SmallMutableArray# s a -> Int# -> a -> State# -> State#
            let arr_ptr = unbox_bytearray(pipeline, builder, args[0]);
            let idx = unbox_int(pipeline, builder, args[1]);
            let val = args[2].value();
            let base = builder.ins().iadd_imm(arr_ptr, 8);
            let byte_offset = builder.ins().imul_imm(idx, 8);
            let effective = builder.ins().iadd(base, byte_offset);
            builder.ins().store(MemFlags::new(), val, effective, 0);
            Ok(SsaVal::Raw(
                builder.ins().iconst(types::I64, 0),
                LIT_TAG_INT,
            ))
        }

        PrimOpKind::SizeofSmallArray
        | PrimOpKind::SizeofSmallMutableArray
        | PrimOpKind::SizeofArray
        | PrimOpKind::SizeofMutableArray => {
            // sizeofSmallArray# :: SmallArray# a -> Int#
            let arr_ptr = unbox_bytearray(pipeline, builder, args[0]);
            let len = builder.ins().load(types::I64, MemFlags::new(), arr_ptr, 0);
            Ok(SsaVal::Raw(len, LIT_TAG_INT))
        }

        PrimOpKind::UnsafeFreezeSmallArray
        | PrimOpKind::UnsafeThawSmallArray
        | PrimOpKind::UnsafeFreezeArray
        | PrimOpKind::UnsafeThawArray => {
            // Identity \u2014 mutable and immutable have the same representation
            Ok(args[0])
        }

        PrimOpKind::CopySmallArray
        | PrimOpKind::CopySmallMutableArray
        | PrimOpKind::CopyArray
        | PrimOpKind::CopyMutableArray => {
            // copySmallArray# :: src -> src_off -> dest -> dest_off -> len -> State# -> State#
            let src = unbox_bytearray(pipeline, builder, args[0]);
            let src_off = unbox_int(pipeline, builder, args[1]);
            let dest = unbox_bytearray(pipeline, builder, args[2]);
            let dest_off = unbox_int(pipeline, builder, args[3]);
            let len = unbox_int(pipeline, builder, args[4]);
            let _ = emit_runtime_call(
                pipeline,
                builder,
                "runtime_copy_boxed_array",
                &[
                    AbiParam::new(types::I64),
                    AbiParam::new(types::I64),
                    AbiParam::new(types::I64),
                    AbiParam::new(types::I64),
                    AbiParam::new(types::I64),
                ],
                &[],
                &[src, src_off, dest, dest_off, len],
            )?;
            Ok(SsaVal::Raw(
                builder.ins().iconst(types::I64, 0),
                LIT_TAG_INT,
            ))
        }

        PrimOpKind::CloneSmallArray | PrimOpKind::CloneSmallMutableArray => {
            // cloneSmallArray# :: SmallArray# a -> Int# -> Int# -> SmallArray# a
            let arr = unbox_bytearray(pipeline, builder, args[0]);
            let off = unbox_int(pipeline, builder, args[1]);
            let len = unbox_int(pipeline, builder, args[2]);
            let result = emit_runtime_call(
                pipeline,
                builder,
                "runtime_clone_boxed_array",
                &[
                    AbiParam::new(types::I64),
                    AbiParam::new(types::I64),
                    AbiParam::new(types::I64),
                ],
                &[AbiParam::new(types::I64)],
                &[arr, off, len],
            )?;
            Ok(emit_lit_boxed_array(
                builder,
                vmctx,
                gc_sig,
                oom_func,
                result,
                LIT_TAG_SMALLARRAY,
            ))
        }

        PrimOpKind::CloneArray | PrimOpKind::CloneMutableArray => {
            let arr = unbox_bytearray(pipeline, builder, args[0]);
            let off = unbox_int(pipeline, builder, args[1]);
            let len = unbox_int(pipeline, builder, args[2]);
            let result = emit_runtime_call(
                pipeline,
                builder,
                "runtime_clone_boxed_array",
                &[
                    AbiParam::new(types::I64),
                    AbiParam::new(types::I64),
                    AbiParam::new(types::I64),
                ],
                &[AbiParam::new(types::I64)],
                &[arr, off, len],
            )?;
            Ok(emit_lit_boxed_array(
                builder, vmctx, gc_sig, oom_func, result, LIT_TAG_ARRAY,
            ))
        }

        PrimOpKind::ShrinkSmallMutableArray => {
            let arr = unbox_bytearray(pipeline, builder, args[0]);
            let new_len = unbox_int(pipeline, builder, args[1]);
            let _ = emit_runtime_call(
                pipeline,
                builder,
                "runtime_shrink_boxed_array",
                &[AbiParam::new(types::I64), AbiParam::new(types::I64)],
                &[],
                &[arr, new_len],
            )?;
            Ok(SsaVal::Raw(
                builder.ins().iconst(types::I64, 0),
                LIT_TAG_INT,
            ))
        }

        PrimOpKind::CasSmallArray => {
            // casSmallArray# :: SmallMutableArray# s a -> Int# -> a -> a -> State# s
            //   -> (# State# s, Int#, a #)
            // Returns (0#, old) if CAS succeeded, (1#, old) if failed.
            // We simplify: return the old value (caller checks).
            let arr = unbox_bytearray(pipeline, builder, args[0]);
            let idx = unbox_int(pipeline, builder, args[1]);
            let expected = args[2].value();
            let new_val = args[3].value();
            let old = emit_runtime_call(
                pipeline,
                builder,
                "runtime_cas_boxed_array",
                &[
                    AbiParam::new(types::I64),
                    AbiParam::new(types::I64),
                    AbiParam::new(types::I64),
                    AbiParam::new(types::I64),
                ],
                &[AbiParam::new(types::I64)],
                &[arr, idx, expected, new_val],
            )?;
            // CAS returns the old value as a heap pointer
            builder.declare_value_needs_stack_map(old);
            Ok(SsaVal::HeapPtr(old))
        }

        // ---------------------------------------------------------------
        // Bit operations \u2014 popCount, ctz
        // ---------------------------------------------------------------
        PrimOpKind::PopCnt | PrimOpKind::PopCnt64 => {
            check_arity(op, 1, args.len())?;
            let a = unbox_int(pipeline, builder, args[0]);
            Ok(SsaVal::Raw(builder.ins().popcnt(a), LIT_TAG_WORD))
        }
        PrimOpKind::PopCnt8 => {
            check_arity(op, 1, args.len())?;
            let a = unbox_int(pipeline, builder, args[0]);
            let masked = builder.ins().band_imm(a, 0xFF);
            Ok(SsaVal::Raw(builder.ins().popcnt(masked), LIT_TAG_WORD))
        }
        PrimOpKind::PopCnt16 => {
            check_arity(op, 1, args.len())?;
            let a = unbox_int(pipeline, builder, args[0]);
            let masked = builder.ins().band_imm(a, 0xFFFF);
            Ok(SsaVal::Raw(builder.ins().popcnt(masked), LIT_TAG_WORD))
        }
        PrimOpKind::PopCnt32 => {
            check_arity(op, 1, args.len())?;
            let a = unbox_int(pipeline, builder, args[0]);
            let masked = builder.ins().band_imm(a, 0xFFFF_FFFF);
            Ok(SsaVal::Raw(builder.ins().popcnt(masked), LIT_TAG_WORD))
        }
        PrimOpKind::Ctz | PrimOpKind::Ctz64 => {
            check_arity(op, 1, args.len())?;
            let a = unbox_int(pipeline, builder, args[0]);
            Ok(SsaVal::Raw(builder.ins().ctz(a), LIT_TAG_WORD))
        }
        PrimOpKind::Ctz8 => {
            check_arity(op, 1, args.len())?;
            let a = unbox_int(pipeline, builder, args[0]);
            // Set bit 8 so ctz stops there if all lower 8 bits are zero
            let with_sentinel = builder.ins().bor_imm(a, 0x100);
            Ok(SsaVal::Raw(builder.ins().ctz(with_sentinel), LIT_TAG_WORD))
        }
        PrimOpKind::Ctz16 => {
            check_arity(op, 1, args.len())?;
            let a = unbox_int(pipeline, builder, args[0]);
            let with_sentinel = builder.ins().bor_imm(a, 0x10000);
            Ok(SsaVal::Raw(builder.ins().ctz(with_sentinel), LIT_TAG_WORD))
        }
        PrimOpKind::Ctz32 => {
            check_arity(op, 1, args.len())?;
            let a = unbox_int(pipeline, builder, args[0]);
            let with_sentinel = builder.ins().bor_imm(a, 0x1_0000_0000);
            Ok(SsaVal::Raw(builder.ins().ctz(with_sentinel), LIT_TAG_WORD))
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
    pipeline: &mut CodegenPipeline,
    builder: &mut FunctionBuilder,
    op: &PrimOpKind,
    cc: IntCC,
    args: &[SsaVal],
    tag: i64,
) -> Result<SsaVal, EmitError> {
    check_arity(op, 2, args.len())?;
    let a = unbox_int(pipeline, builder, args[0]);
    let b = unbox_int(pipeline, builder, args[1]);
    let cmp = builder.ins().icmp(cc, a, b);
    Ok(SsaVal::Raw(builder.ins().uextend(types::I64, cmp), tag))
}

fn emit_float_compare(
    pipeline: &mut CodegenPipeline,
    builder: &mut FunctionBuilder,
    op: &PrimOpKind,
    cc: FloatCC,
    args: &[SsaVal],
    tag: i64,
) -> Result<SsaVal, EmitError> {
    check_arity(op, 2, args.len())?;
    let a = unbox_double(pipeline, builder, args[0]);
    let b = unbox_double(pipeline, builder, args[1]);
    let cmp = builder.ins().fcmp(cc, a, b);
    Ok(SsaVal::Raw(builder.ins().uextend(types::I64, cmp), tag))
}

/// Unbox an Addr# value recursively.
fn unbox_addr(pipeline: &mut CodegenPipeline, builder: &mut FunctionBuilder, val: SsaVal) -> Value {
    match val {
        SsaVal::Raw(v, _) => v,
        SsaVal::HeapPtr(v) => {
            let start_block = builder.create_block();
            let next_block = builder.create_block();
            builder.append_block_param(start_block, types::I64);
            builder.append_block_param(next_block, types::I64);
            
            builder.ins().jump(start_block, &[v]);
            
            builder.switch_to_block(start_block);
            let curr_v = builder.block_params(start_block)[0];
            let tag = builder.ins().load(types::I8, MemFlags::trusted(), curr_v, 0);
            let is_con = builder.ins().icmp_imm(IntCC::Equal, tag, layout::TAG_CON as i64);
            
            let con_block = builder.create_block();
            builder.ins().brif(is_con, con_block, &[], next_block, &[curr_v]);
            
            builder.switch_to_block(con_block);
            builder.seal_block(con_block);
            let field0 = builder.ins().load(types::I64, MemFlags::trusted(), curr_v, layout::CON_FIELDS_OFFSET as i32);
            builder.ins().jump(start_block, &[field0]);
            
            builder.switch_to_block(next_block);
            builder.seal_block(start_block);
            builder.seal_block(next_block);
            let v_final = builder.block_params(next_block)[0];
            
            let raw_val = builder
                .ins()
                .load(types::I64, MemFlags::trusted(), v_final, LIT_VALUE_OFFSET);
            let lit_tag = builder
                .ins()
                .load(types::I8, MemFlags::trusted(), v_final, LIT_TAG_OFFSET);
            let lit_tag_ext = builder.ins().uextend(types::I64, lit_tag);
            
            let is_string = builder
                .ins()
                .icmp_imm(IntCC::Equal, lit_tag_ext, LIT_TAG_STRING);
            let is_ba = builder
                .ins()
                .icmp_imm(IntCC::Equal, lit_tag_ext, LIT_TAG_BYTEARRAY);
            let needs_adj = builder.ins().bor(is_string, is_ba);
            let adjusted = builder.ins().iadd_imm(raw_val, 8);
            builder.ins().select(needs_adj, adjusted, raw_val)
        }
    }
}

/// Extract the raw ByteArray pointer from a Lit(BYTEARRAY) heap object recursively.
fn unbox_bytearray(pipeline: &mut CodegenPipeline, builder: &mut FunctionBuilder, val: SsaVal) -> Value {
    match val {
        SsaVal::Raw(v, _) => v,
        SsaVal::HeapPtr(v) => {
            let start_block = builder.create_block();
            let next_block = builder.create_block();
            builder.append_block_param(start_block, types::I64);
            builder.append_block_param(next_block, types::I64);
            
            builder.ins().jump(start_block, &[v]);
            
            builder.switch_to_block(start_block);
            let curr_v = builder.block_params(start_block)[0];
            let tag = builder.ins().load(types::I8, MemFlags::trusted(), curr_v, 0);
            let is_con = builder.ins().icmp_imm(IntCC::Equal, tag, layout::TAG_CON as i64);
            
            let con_block = builder.create_block();
            builder.ins().brif(is_con, con_block, &[], next_block, &[curr_v]);
            
            builder.switch_to_block(con_block);
            builder.seal_block(con_block);
            let field0 = builder.ins().load(types::I64, MemFlags::trusted(), curr_v, layout::CON_FIELDS_OFFSET as i32);
            builder.ins().jump(start_block, &[field0]);
            
            builder.switch_to_block(next_block);
            builder.seal_block(start_block);
            builder.seal_block(next_block);
            let v_final = builder.block_params(next_block)[0];
            
            let raw_val = builder
                .ins()
                .load(types::I64, MemFlags::trusted(), v_final, LIT_VALUE_OFFSET);
            let lit_tag = builder
                .ins()
                .load(types::I8, MemFlags::trusted(), v_final, LIT_TAG_OFFSET);
            let lit_tag_ext = builder.ins().uextend(types::I64, lit_tag);
            
            // ByteArray# should also adjust for LIT_TAG_STRING if passed one.
            let is_string = builder
                .ins()
                .icmp_imm(IntCC::Equal, lit_tag_ext, LIT_TAG_STRING);
            let adjusted = builder.ins().iadd_imm(raw_val, 8);
            builder.ins().select(is_string, adjusted, raw_val)
        }
    }
}

pub fn unbox_int(pipeline: &mut CodegenPipeline, builder: &mut FunctionBuilder, val: SsaVal) -> Value {
    match val {
        SsaVal::Raw(v, _) => v,
        SsaVal::HeapPtr(v) => {
            let start_block = builder.create_block();
            let next_block = builder.create_block();
            builder.append_block_param(start_block, types::I64);
            builder.append_block_param(next_block, types::I64);
            
            builder.ins().jump(start_block, &[v]);
            
            builder.switch_to_block(start_block);
            let curr_v = builder.block_params(start_block)[0];
            let tag = builder.ins().load(types::I8, MemFlags::trusted(), curr_v, 0);
            let is_con = builder.ins().icmp_imm(IntCC::Equal, tag, layout::TAG_CON as i64);
            
            let con_block = builder.create_block();
            builder.ins().brif(is_con, con_block, &[], next_block, &[curr_v]);
            
            builder.switch_to_block(con_block);
            builder.seal_block(con_block);
            let field0 = builder.ins().load(types::I64, MemFlags::trusted(), curr_v, layout::CON_FIELDS_OFFSET as i32);
            builder.ins().jump(start_block, &[field0]);
            
            builder.switch_to_block(next_block);
            builder.seal_block(start_block);
            builder.seal_block(next_block);
            let v_final = builder.block_params(next_block)[0];
            
            builder
                .ins()
                .load(types::I64, MemFlags::trusted(), v_final, LIT_VALUE_OFFSET)
        }
    }
}

pub fn unbox_double(pipeline: &mut CodegenPipeline, builder: &mut FunctionBuilder, val: SsaVal) -> Value {
    match val {
        SsaVal::Raw(v, _) => v,
        SsaVal::HeapPtr(v) => {
            let start_block = builder.create_block();
            let next_block = builder.create_block();
            builder.append_block_param(start_block, types::I64);
            builder.append_block_param(next_block, types::I64);
            
            builder.ins().jump(start_block, &[v]);
            
            builder.switch_to_block(start_block);
            let curr_v = builder.block_params(start_block)[0];
            let tag = builder.ins().load(types::I8, MemFlags::trusted(), curr_v, 0);
            let is_con = builder.ins().icmp_imm(IntCC::Equal, tag, layout::TAG_CON as i64);
            
            let con_block = builder.create_block();
            builder.ins().brif(is_con, con_block, &[], next_block, &[curr_v]);
            
            builder.switch_to_block(con_block);
            builder.seal_block(con_block);
            let field0 = builder.ins().load(types::I64, MemFlags::trusted(), curr_v, layout::CON_FIELDS_OFFSET as i32);
            builder.ins().jump(start_block, &[field0]);
            
            builder.switch_to_block(next_block);
            builder.seal_block(start_block);
            builder.seal_block(next_block);
            let v_final = builder.block_params(next_block)[0];
            
            builder
                .ins()
                .load(types::F64, MemFlags::trusted(), v_final, LIT_VALUE_OFFSET)
        }
    }
}

pub fn unbox_float(pipeline: &mut CodegenPipeline, builder: &mut FunctionBuilder, val: SsaVal) -> Value {
    match val {
        SsaVal::Raw(v, _) => v,
        SsaVal::HeapPtr(v) => {
            let start_block = builder.create_block();
            let next_block = builder.create_block();
            builder.append_block_param(start_block, types::I64);
            builder.append_block_param(next_block, types::I64);
            
            builder.ins().jump(start_block, &[v]);
            
            builder.switch_to_block(start_block);
            let curr_v = builder.block_params(start_block)[0];
            let tag = builder.ins().load(types::I8, MemFlags::trusted(), curr_v, 0);
            let is_con = builder.ins().icmp_imm(IntCC::Equal, tag, layout::TAG_CON as i64);
            
            let con_block = builder.create_block();
            builder.ins().brif(is_con, con_block, &[], next_block, &[curr_v]);
            
            builder.switch_to_block(con_block);
            builder.seal_block(con_block);
            let field0 = builder.ins().load(types::I64, MemFlags::trusted(), curr_v, layout::CON_FIELDS_OFFSET as i32);
            builder.ins().jump(start_block, &[field0]);
            
            builder.switch_to_block(next_block);
            builder.seal_block(start_block);
            builder.seal_block(next_block);
            let v_final = builder.block_params(next_block)[0];
            
            builder
                .ins()
                .load(types::F32, MemFlags::trusted(), v_final, LIT_VALUE_OFFSET)
        }
    }
}

/// Allocate a Lit heap object with LIT_TAG_BYTEARRAY, storing a raw pointer.
fn emit_lit_bytearray(
    builder: &mut FunctionBuilder,
    vmctx: Value,
    gc_sig: ir::SigRef,
    oom_func: ir::FuncRef,
    ba_ptr: Value,
) -> SsaVal {
    let ptr = emit_alloc_fast_path(builder, vmctx, LIT_TOTAL_SIZE, gc_sig, oom_func);
    let tag = builder.ins().iconst(types::I8, layout::TAG_LIT as i64);
    builder.ins().store(MemFlags::trusted(), tag, ptr, 0);
    let size = builder.ins().iconst(types::I16, LIT_TOTAL_SIZE as i64);
    builder.ins().store(MemFlags::trusted(), size, ptr, 1);
    let lit_tag = builder.ins().iconst(types::I8, LIT_TAG_BYTEARRAY);
    builder
        .ins()
        .store(MemFlags::trusted(), lit_tag, ptr, LIT_TAG_OFFSET);
    builder
        .ins()
        .store(MemFlags::trusted(), ba_ptr, ptr, LIT_VALUE_OFFSET);
    builder.declare_value_needs_stack_map(ptr);
    SsaVal::HeapPtr(ptr)
}

/// Allocate a Lit heap object for a boxed array (SmallArray# or Array#).
fn emit_lit_boxed_array(
    builder: &mut FunctionBuilder,
    vmctx: Value,
    gc_sig: ir::SigRef,
    oom_func: ir::FuncRef,
    arr_ptr: Value,
    lit_tag: i64,
) -> SsaVal {
    let ptr = emit_alloc_fast_path(builder, vmctx, LIT_TOTAL_SIZE, gc_sig, oom_func);
    let tag = builder.ins().iconst(types::I8, layout::TAG_LIT as i64);
    builder.ins().store(MemFlags::trusted(), tag, ptr, 0);
    let size = builder.ins().iconst(types::I16, LIT_TOTAL_SIZE as i64);
    builder.ins().store(MemFlags::trusted(), size, ptr, 1);
    let lt = builder.ins().iconst(types::I8, lit_tag);
    builder
        .ins()
        .store(MemFlags::trusted(), lt, ptr, LIT_TAG_OFFSET);
    builder
        .ins()
        .store(MemFlags::trusted(), arr_ptr, ptr, LIT_VALUE_OFFSET);
    builder.declare_value_needs_stack_map(ptr);
    SsaVal::HeapPtr(ptr)
}

/// Call a runtime function by name. Returns the result value (or a dummy if no returns).
fn emit_runtime_call(
    pipeline: &mut CodegenPipeline,
    builder: &mut FunctionBuilder,
    name: &str,
    params: &[AbiParam],
    returns: &[AbiParam],
    arg_vals: &[Value],
) -> Result<Value, EmitError> {
    let mut sig = Signature::new(pipeline.isa.default_call_conv());
    for p in params {
        sig.params.push(*p);
    }
    for r in returns {
        sig.returns.push(*r);
    }
    let func_id = pipeline
        .module
        .declare_function(name, Linkage::Import, &sig)
        .map_err(|e| EmitError::CraneliftError(e.to_string()))?;
    let func_ref = pipeline.module.declare_func_in_func(func_id, builder.func);
    let inst = builder.ins().call(func_ref, arg_vals);
    if returns.is_empty() {
        Ok(builder.ins().iconst(types::I64, 0))
    } else {
        Ok(builder.inst_results(inst)[0])
    }
}