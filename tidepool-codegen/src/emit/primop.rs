use super::*;
use crate::alloc::emit_alloc_fast_path;
use crate::emit::{EmitError, SsaVal};
use crate::pipeline::CodegenPipeline;
use cranelift_codegen::ir::{
    self, condcodes::FloatCC, condcodes::IntCC, types, AbiParam, BlockArg, InstBuilder, MemFlags,
    Signature, Value,
};
use cranelift_frontend::FunctionBuilder;
use cranelift_module::Linkage;
use cranelift_module::Module;
use tidepool_heap::layout;
use tidepool_repr::PrimOpKind;

/// Emit a zero-divisor guard: if `divisor == 0`, trap; otherwise fall through.
fn emit_div_zero_check(builder: &mut FunctionBuilder, divisor: Value) {
    let zero = builder.ins().iconst(types::I64, 0);
    let is_zero = builder.ins().icmp(IntCC::Equal, divisor, zero);
    let ok_block = builder.create_block();
    let trap_block = builder.create_block();
    builder.ins().brif(is_zero, trap_block, &[], ok_block, &[]);

    builder.switch_to_block(trap_block);
    builder.seal_block(trap_block);
    builder
        .ins()
        .trap(cranelift_codegen::ir::TrapCode::unwrap_user(3));

    builder.switch_to_block(ok_block);
    builder.seal_block(ok_block);
}

/// Emit a primitive operation. Unboxes HeapPtr args, performs the op, returns Raw.
/// `n` i64 ABI params, for the uniform-i64 host-fn signatures of the bignum
/// (`__gmpn_*` / `integer_gmp_*`) intercepts.
fn i64_params(n: usize) -> Vec<AbiParam> {
    (0..n).map(|_| AbiParam::new(types::I64)).collect()
}

pub fn emit_primop(
    sess: &mut EmitSession,
    builder: &mut FunctionBuilder,
    op: &PrimOpKind,
    args: &[SsaVal],
) -> Result<SsaVal, EmitError> {
    match op {
        // Int arithmetic (binary)
        PrimOpKind::IntAdd => {
            check_arity(op, 2, args.len())?;
            let a = unbox_int(sess.pipeline, builder, sess.vmctx, args[0]);
            let b = unbox_int(sess.pipeline, builder, sess.vmctx, args[1]);
            Ok(SsaVal::Raw(builder.ins().iadd(a, b), LIT_TAG_INT))
        }
        PrimOpKind::IntSub => {
            check_arity(op, 2, args.len())?;
            let a = unbox_int(sess.pipeline, builder, sess.vmctx, args[0]);
            let b = unbox_int(sess.pipeline, builder, sess.vmctx, args[1]);
            Ok(SsaVal::Raw(builder.ins().isub(a, b), LIT_TAG_INT))
        }
        PrimOpKind::IntMul => {
            check_arity(op, 2, args.len())?;
            let a = unbox_int(sess.pipeline, builder, sess.vmctx, args[0]);
            let b = unbox_int(sess.pipeline, builder, sess.vmctx, args[1]);
            Ok(SsaVal::Raw(builder.ins().imul(a, b), LIT_TAG_INT))
        }
        PrimOpKind::IntNegate => {
            check_arity(op, 1, args.len())?;
            let a = unbox_int(sess.pipeline, builder, sess.vmctx, args[0]);
            Ok(SsaVal::Raw(builder.ins().ineg(a), LIT_TAG_INT))
        }
        PrimOpKind::IntQuot => {
            check_arity(op, 2, args.len())?;
            let a = unbox_int(sess.pipeline, builder, sess.vmctx, args[0]);
            let b = unbox_int(sess.pipeline, builder, sess.vmctx, args[1]);
            emit_div_zero_check(builder, b);
            Ok(SsaVal::Raw(builder.ins().sdiv(a, b), LIT_TAG_INT))
        }
        PrimOpKind::IntRem => {
            check_arity(op, 2, args.len())?;
            let a = unbox_int(sess.pipeline, builder, sess.vmctx, args[0]);
            let b = unbox_int(sess.pipeline, builder, sess.vmctx, args[1]);
            emit_div_zero_check(builder, b);
            Ok(SsaVal::Raw(builder.ins().srem(a, b), LIT_TAG_INT))
        }

        // Int bitwise
        PrimOpKind::IntAnd => {
            check_arity(op, 2, args.len())?;
            let a = unbox_int(sess.pipeline, builder, sess.vmctx, args[0]);
            let b = unbox_int(sess.pipeline, builder, sess.vmctx, args[1]);
            Ok(SsaVal::Raw(builder.ins().band(a, b), LIT_TAG_INT))
        }
        PrimOpKind::IntOr => {
            check_arity(op, 2, args.len())?;
            let a = unbox_int(sess.pipeline, builder, sess.vmctx, args[0]);
            let b = unbox_int(sess.pipeline, builder, sess.vmctx, args[1]);
            Ok(SsaVal::Raw(builder.ins().bor(a, b), LIT_TAG_INT))
        }
        PrimOpKind::IntXor => {
            check_arity(op, 2, args.len())?;
            let a = unbox_int(sess.pipeline, builder, sess.vmctx, args[0]);
            let b = unbox_int(sess.pipeline, builder, sess.vmctx, args[1]);
            Ok(SsaVal::Raw(builder.ins().bxor(a, b), LIT_TAG_INT))
        }
        PrimOpKind::IntNot => {
            check_arity(op, 1, args.len())?;
            let a = unbox_int(sess.pipeline, builder, sess.vmctx, args[0]);
            Ok(SsaVal::Raw(builder.ins().bnot(a), LIT_TAG_INT))
        }

        // Int shifts
        PrimOpKind::IntShl => {
            check_arity(op, 2, args.len())?;
            let a = unbox_int(sess.pipeline, builder, sess.vmctx, args[0]);
            let b = unbox_int(sess.pipeline, builder, sess.vmctx, args[1]);
            Ok(SsaVal::Raw(builder.ins().ishl(a, b), LIT_TAG_INT))
        }
        PrimOpKind::IntShra => {
            check_arity(op, 2, args.len())?;
            let a = unbox_int(sess.pipeline, builder, sess.vmctx, args[0]);
            let b = unbox_int(sess.pipeline, builder, sess.vmctx, args[1]);
            Ok(SsaVal::Raw(builder.ins().sshr(a, b), LIT_TAG_INT))
        }
        PrimOpKind::IntShrl => {
            check_arity(op, 2, args.len())?;
            let a = unbox_int(sess.pipeline, builder, sess.vmctx, args[0]);
            let b = unbox_int(sess.pipeline, builder, sess.vmctx, args[1]);
            Ok(SsaVal::Raw(builder.ins().ushr(a, b), LIT_TAG_INT))
        }

        // Int comparison \u2192 returns i64 (0=False, 1=True)
        PrimOpKind::IntEq => emit_int_compare(
            sess.pipeline,
            builder,
            sess.vmctx,
            op,
            IntCC::Equal,
            args,
            LIT_TAG_INT,
        ),
        PrimOpKind::IntNe => emit_int_compare(
            sess.pipeline,
            builder,
            sess.vmctx,
            op,
            IntCC::NotEqual,
            args,
            LIT_TAG_INT,
        ),
        PrimOpKind::IntLt => emit_int_compare(
            sess.pipeline,
            builder,
            sess.vmctx,
            op,
            IntCC::SignedLessThan,
            args,
            LIT_TAG_INT,
        ),
        PrimOpKind::IntLe => emit_int_compare(
            sess.pipeline,
            builder,
            sess.vmctx,
            op,
            IntCC::SignedLessThanOrEqual,
            args,
            LIT_TAG_INT,
        ),
        PrimOpKind::IntGt => emit_int_compare(
            sess.pipeline,
            builder,
            sess.vmctx,
            op,
            IntCC::SignedGreaterThan,
            args,
            LIT_TAG_INT,
        ),
        PrimOpKind::IntGe => emit_int_compare(
            sess.pipeline,
            builder,
            sess.vmctx,
            op,
            IntCC::SignedGreaterThanOrEqual,
            args,
            LIT_TAG_INT,
        ),

        // Word arithmetic
        PrimOpKind::WordAdd => {
            check_arity(op, 2, args.len())?;
            let a = unbox_int(sess.pipeline, builder, sess.vmctx, args[0]);
            let b = unbox_int(sess.pipeline, builder, sess.vmctx, args[1]);
            Ok(SsaVal::Raw(builder.ins().iadd(a, b), LIT_TAG_WORD))
        }
        PrimOpKind::WordSub => {
            check_arity(op, 2, args.len())?;
            let a = unbox_int(sess.pipeline, builder, sess.vmctx, args[0]);
            let b = unbox_int(sess.pipeline, builder, sess.vmctx, args[1]);
            Ok(SsaVal::Raw(builder.ins().isub(a, b), LIT_TAG_WORD))
        }
        PrimOpKind::WordMul => {
            check_arity(op, 2, args.len())?;
            let a = unbox_int(sess.pipeline, builder, sess.vmctx, args[0]);
            let b = unbox_int(sess.pipeline, builder, sess.vmctx, args[1]);
            Ok(SsaVal::Raw(builder.ins().imul(a, b), LIT_TAG_WORD))
        }

        PrimOpKind::WordQuot => {
            check_arity(op, 2, args.len())?;
            let a = unbox_int(sess.pipeline, builder, sess.vmctx, args[0]);
            let b = unbox_int(sess.pipeline, builder, sess.vmctx, args[1]);
            emit_div_zero_check(builder, b);
            Ok(SsaVal::Raw(builder.ins().udiv(a, b), LIT_TAG_WORD))
        }
        PrimOpKind::WordRem => {
            check_arity(op, 2, args.len())?;
            let a = unbox_int(sess.pipeline, builder, sess.vmctx, args[0]);
            let b = unbox_int(sess.pipeline, builder, sess.vmctx, args[1]);
            emit_div_zero_check(builder, b);
            Ok(SsaVal::Raw(builder.ins().urem(a, b), LIT_TAG_WORD))
        }

        // Word bitwise
        PrimOpKind::WordAnd => {
            check_arity(op, 2, args.len())?;
            let a = unbox_int(sess.pipeline, builder, sess.vmctx, args[0]);
            let b = unbox_int(sess.pipeline, builder, sess.vmctx, args[1]);
            Ok(SsaVal::Raw(builder.ins().band(a, b), LIT_TAG_WORD))
        }
        PrimOpKind::WordOr => {
            check_arity(op, 2, args.len())?;
            let a = unbox_int(sess.pipeline, builder, sess.vmctx, args[0]);
            let b = unbox_int(sess.pipeline, builder, sess.vmctx, args[1]);
            Ok(SsaVal::Raw(builder.ins().bor(a, b), LIT_TAG_WORD))
        }
        PrimOpKind::WordXor => {
            check_arity(op, 2, args.len())?;
            let a = unbox_int(sess.pipeline, builder, sess.vmctx, args[0]);
            let b = unbox_int(sess.pipeline, builder, sess.vmctx, args[1]);
            Ok(SsaVal::Raw(builder.ins().bxor(a, b), LIT_TAG_WORD))
        }
        PrimOpKind::WordNot => {
            check_arity(op, 1, args.len())?;
            let a = unbox_int(sess.pipeline, builder, sess.vmctx, args[0]);
            Ok(SsaVal::Raw(builder.ins().bnot(a), LIT_TAG_WORD))
        }

        // Word shifts
        PrimOpKind::WordShl => {
            check_arity(op, 2, args.len())?;
            let a = unbox_int(sess.pipeline, builder, sess.vmctx, args[0]);
            let b = unbox_int(sess.pipeline, builder, sess.vmctx, args[1]);
            Ok(SsaVal::Raw(builder.ins().ishl(a, b), LIT_TAG_WORD))
        }
        PrimOpKind::WordShrl => {
            check_arity(op, 2, args.len())?;
            let a = unbox_int(sess.pipeline, builder, sess.vmctx, args[0]);
            let b = unbox_int(sess.pipeline, builder, sess.vmctx, args[1]);
            Ok(SsaVal::Raw(builder.ins().ushr(a, b), LIT_TAG_WORD))
        }

        // Word comparison (unsigned)
        PrimOpKind::WordEq | PrimOpKind::Word64Eq => emit_int_compare(
            sess.pipeline,
            builder,
            sess.vmctx,
            op,
            IntCC::Equal,
            args,
            LIT_TAG_INT,
        ),
        PrimOpKind::WordNe | PrimOpKind::Word64Ne => emit_int_compare(
            sess.pipeline,
            builder,
            sess.vmctx,
            op,
            IntCC::NotEqual,
            args,
            LIT_TAG_INT,
        ),
        PrimOpKind::WordLt | PrimOpKind::Word64Lt => emit_int_compare(
            sess.pipeline,
            builder,
            sess.vmctx,
            op,
            IntCC::UnsignedLessThan,
            args,
            LIT_TAG_INT,
        ),
        PrimOpKind::WordLe | PrimOpKind::Word64Le => emit_int_compare(
            sess.pipeline,
            builder,
            sess.vmctx,
            op,
            IntCC::UnsignedLessThanOrEqual,
            args,
            LIT_TAG_INT,
        ),
        PrimOpKind::WordGt | PrimOpKind::Word64Gt => emit_int_compare(
            sess.pipeline,
            builder,
            sess.vmctx,
            op,
            IntCC::UnsignedGreaterThan,
            args,
            LIT_TAG_INT,
        ),
        PrimOpKind::WordGe | PrimOpKind::Word64Ge => emit_int_compare(
            sess.pipeline,
            builder,
            sess.vmctx,
            op,
            IntCC::UnsignedGreaterThanOrEqual,
            args,
            LIT_TAG_INT,
        ),

        // Double arithmetic
        PrimOpKind::DoubleAdd => {
            check_arity(op, 2, args.len())?;
            let a = unbox_double(sess.pipeline, builder, sess.vmctx, args[0]);
            let b = unbox_double(sess.pipeline, builder, sess.vmctx, args[1]);
            Ok(SsaVal::Raw(builder.ins().fadd(a, b), LIT_TAG_DOUBLE))
        }
        PrimOpKind::DoubleSub => {
            check_arity(op, 2, args.len())?;
            let a = unbox_double(sess.pipeline, builder, sess.vmctx, args[0]);
            let b = unbox_double(sess.pipeline, builder, sess.vmctx, args[1]);
            Ok(SsaVal::Raw(builder.ins().fsub(a, b), LIT_TAG_DOUBLE))
        }
        PrimOpKind::DoubleMul => {
            check_arity(op, 2, args.len())?;
            let a = unbox_double(sess.pipeline, builder, sess.vmctx, args[0]);
            let b = unbox_double(sess.pipeline, builder, sess.vmctx, args[1]);
            Ok(SsaVal::Raw(builder.ins().fmul(a, b), LIT_TAG_DOUBLE))
        }
        PrimOpKind::DoubleDiv => {
            check_arity(op, 2, args.len())?;
            let a = unbox_double(sess.pipeline, builder, sess.vmctx, args[0]);
            let b = unbox_double(sess.pipeline, builder, sess.vmctx, args[1]);
            Ok(SsaVal::Raw(builder.ins().fdiv(a, b), LIT_TAG_DOUBLE))
        }

        // Double comparison
        PrimOpKind::DoubleEq => emit_double_compare(
            sess.pipeline,
            builder,
            sess.vmctx,
            op,
            FloatCC::Equal,
            args,
            LIT_TAG_INT,
        ),
        PrimOpKind::DoubleNe => emit_double_compare(
            sess.pipeline,
            builder,
            sess.vmctx,
            op,
            FloatCC::NotEqual,
            args,
            LIT_TAG_INT,
        ),
        PrimOpKind::DoubleLt => emit_double_compare(
            sess.pipeline,
            builder,
            sess.vmctx,
            op,
            FloatCC::LessThan,
            args,
            LIT_TAG_INT,
        ),
        PrimOpKind::DoubleLe => emit_double_compare(
            sess.pipeline,
            builder,
            sess.vmctx,
            op,
            FloatCC::LessThanOrEqual,
            args,
            LIT_TAG_INT,
        ),
        PrimOpKind::DoubleGt => emit_double_compare(
            sess.pipeline,
            builder,
            sess.vmctx,
            op,
            FloatCC::GreaterThan,
            args,
            LIT_TAG_INT,
        ),
        PrimOpKind::DoubleGe => emit_double_compare(
            sess.pipeline,
            builder,
            sess.vmctx,
            op,
            FloatCC::GreaterThanOrEqual,
            args,
            LIT_TAG_INT,
        ),

        // Char comparison
        PrimOpKind::CharEq => emit_int_compare(
            sess.pipeline,
            builder,
            sess.vmctx,
            op,
            IntCC::Equal,
            args,
            LIT_TAG_INT,
        ),
        PrimOpKind::CharNe => emit_int_compare(
            sess.pipeline,
            builder,
            sess.vmctx,
            op,
            IntCC::NotEqual,
            args,
            LIT_TAG_INT,
        ),
        PrimOpKind::CharLt => emit_int_compare(
            sess.pipeline,
            builder,
            sess.vmctx,
            op,
            IntCC::UnsignedLessThan,
            args,
            LIT_TAG_INT,
        ),
        PrimOpKind::CharLe => emit_int_compare(
            sess.pipeline,
            builder,
            sess.vmctx,
            op,
            IntCC::UnsignedLessThanOrEqual,
            args,
            LIT_TAG_INT,
        ),
        PrimOpKind::CharGt => emit_int_compare(
            sess.pipeline,
            builder,
            sess.vmctx,
            op,
            IntCC::UnsignedGreaterThan,
            args,
            LIT_TAG_INT,
        ),
        PrimOpKind::CharGe => emit_int_compare(
            sess.pipeline,
            builder,
            sess.vmctx,
            op,
            IntCC::UnsignedGreaterThanOrEqual,
            args,
            LIT_TAG_INT,
        ),

        // Conversions
        PrimOpKind::Chr => {
            check_arity(op, 1, args.len())?;
            let v = unbox_int(sess.pipeline, builder, sess.vmctx, args[0]);

            // Validate codepoint range (match interpreter behavior in eval.rs)
            // Valid: 0..=0xD7FF or 0xE000..=0x10FFFF
            // Invalid: negative, > 0x10FFFF, or surrogate 0xD800..=0xDFFF
            let zero = builder.ins().iconst(types::I64, 0);
            let max_valid = builder.ins().iconst(types::I64, 0x10FFFF);
            let is_negative = builder.ins().icmp(IntCC::SignedLessThan, v, zero);
            let is_too_large = builder.ins().icmp(IntCC::SignedGreaterThan, v, max_valid);
            let surrogate_lo = builder.ins().iconst(types::I64, 0xD800);
            let surrogate_hi = builder.ins().iconst(types::I64, 0xDFFF);
            let is_surr_lo = builder
                .ins()
                .icmp(IntCC::SignedGreaterThanOrEqual, v, surrogate_lo);
            let is_surr_hi = builder
                .ins()
                .icmp(IntCC::SignedLessThanOrEqual, v, surrogate_hi);
            let is_surrogate = builder.ins().band(is_surr_lo, is_surr_hi);
            let out_of_range = builder.ins().bor(is_negative, is_too_large);
            let is_invalid = builder.ins().bor(out_of_range, is_surrogate);
            builder
                .ins()
                .trapnz(is_invalid, cranelift_codegen::ir::TrapCode::unwrap_user(1));

            Ok(SsaVal::Raw(v, LIT_TAG_CHAR))
        }
        PrimOpKind::Ord => {
            check_arity(op, 1, args.len())?;
            let v = unbox_int(sess.pipeline, builder, sess.vmctx, args[0]);
            Ok(SsaVal::Raw(v, LIT_TAG_INT))
        }
        PrimOpKind::Int2Word | PrimOpKind::Word2Int => {
            check_arity(op, 1, args.len())?;
            let v = unbox_int(sess.pipeline, builder, sess.vmctx, args[0]);
            let tag = if matches!(op, PrimOpKind::Int2Word) {
                LIT_TAG_WORD
            } else {
                LIT_TAG_INT
            };
            Ok(SsaVal::Raw(v, tag))
        }
        PrimOpKind::Word2Double => {
            check_arity(op, 1, args.len())?;
            let v = unbox_int(sess.pipeline, builder, sess.vmctx, args[0]);
            Ok(SsaVal::Raw(
                builder.ins().fcvt_from_uint(types::F64, v),
                LIT_TAG_DOUBLE,
            ))
        }
        PrimOpKind::Int2Double => {
            check_arity(op, 1, args.len())?;
            let v = unbox_int(sess.pipeline, builder, sess.vmctx, args[0]);
            Ok(SsaVal::Raw(
                builder.ins().fcvt_from_sint(types::F64, v),
                LIT_TAG_DOUBLE,
            ))
        }
        PrimOpKind::Double2Int => {
            check_arity(op, 1, args.len())?;
            let v = unbox_double(sess.pipeline, builder, sess.vmctx, args[0]);
            Ok(SsaVal::Raw(
                builder.ins().fcvt_to_sint_sat(types::I64, v),
                LIT_TAG_INT,
            ))
        }
        PrimOpKind::DecodeDoubleMantissa => {
            check_arity(op, 1, args.len())?;
            let d = unbox_double(sess.pipeline, builder, sess.vmctx, args[0]);
            let bits = builder.ins().bitcast(types::I64, MemFlags::new(), d);
            let result = emit_runtime_call(
                sess.pipeline,
                builder,
                "runtime_decode_double_mantissa",
                &[AbiParam::new(types::I64)],
                &[AbiParam::new(types::I64)],
                &[bits],
            )?;
            Ok(SsaVal::Raw(result, LIT_TAG_INT))
        }
        PrimOpKind::DecodeDoubleExponent => {
            check_arity(op, 1, args.len())?;
            let d = unbox_double(sess.pipeline, builder, sess.vmctx, args[0]);
            let bits = builder.ins().bitcast(types::I64, MemFlags::new(), d);
            let result = emit_runtime_call(
                sess.pipeline,
                builder,
                "runtime_decode_double_exponent",
                &[AbiParam::new(types::I64)],
                &[AbiParam::new(types::I64)],
                &[bits],
            )?;
            Ok(SsaVal::Raw(result, LIT_TAG_INT))
        }
        PrimOpKind::ShowDoubleAddr => {
            check_arity(op, 1, args.len())?;
            let d = unbox_double(sess.pipeline, builder, sess.vmctx, args[0]);
            let bits = builder.ins().bitcast(types::I64, MemFlags::new(), d);
            let result = emit_runtime_call(
                sess.pipeline,
                builder,
                "runtime_show_double_addr",
                &[AbiParam::new(types::I64)],
                &[AbiParam::new(types::I64)],
                &[bits],
            )?;
            Ok(SsaVal::Raw(result, LIT_TAG_ADDR))
        }
        PrimOpKind::Int2Float => {
            check_arity(op, 1, args.len())?;
            let v = unbox_int(sess.pipeline, builder, sess.vmctx, args[0]);
            Ok(SsaVal::Raw(
                builder.ins().fcvt_from_sint(types::F32, v),
                LIT_TAG_FLOAT,
            ))
        }
        PrimOpKind::Float2Int => {
            check_arity(op, 1, args.len())?;
            let v = unbox_float(sess.pipeline, builder, sess.vmctx, args[0]);
            Ok(SsaVal::Raw(
                builder.ins().fcvt_to_sint_sat(types::I64, v),
                LIT_TAG_INT,
            ))
        }
        PrimOpKind::Double2Float => {
            check_arity(op, 1, args.len())?;
            let v = unbox_double(sess.pipeline, builder, sess.vmctx, args[0]);
            Ok(SsaVal::Raw(
                builder.ins().fdemote(types::F32, v),
                LIT_TAG_FLOAT,
            ))
        }
        PrimOpKind::Float2Double => {
            check_arity(op, 1, args.len())?;
            let v = unbox_float(sess.pipeline, builder, sess.vmctx, args[0]);
            Ok(SsaVal::Raw(
                builder.ins().fpromote(types::F64, v),
                LIT_TAG_DOUBLE,
            ))
        }

        // Narrowing
        PrimOpKind::Narrow8Int => {
            check_arity(op, 1, args.len())?;
            let v = unbox_int(sess.pipeline, builder, sess.vmctx, args[0]);
            let narrow = builder.ins().ireduce(types::I8, v);
            Ok(SsaVal::Raw(
                builder.ins().sextend(types::I64, narrow),
                LIT_TAG_INT,
            ))
        }
        PrimOpKind::Narrow16Int => {
            check_arity(op, 1, args.len())?;
            let v = unbox_int(sess.pipeline, builder, sess.vmctx, args[0]);
            let narrow = builder.ins().ireduce(types::I16, v);
            Ok(SsaVal::Raw(
                builder.ins().sextend(types::I64, narrow),
                LIT_TAG_INT,
            ))
        }
        PrimOpKind::Narrow32Int => {
            check_arity(op, 1, args.len())?;
            let v = unbox_int(sess.pipeline, builder, sess.vmctx, args[0]);
            let narrow = builder.ins().ireduce(types::I32, v);
            Ok(SsaVal::Raw(
                builder.ins().sextend(types::I64, narrow),
                LIT_TAG_INT,
            ))
        }
        PrimOpKind::Narrow8Word => {
            check_arity(op, 1, args.len())?;
            let v = unbox_int(sess.pipeline, builder, sess.vmctx, args[0]);
            let narrow = builder.ins().ireduce(types::I8, v);
            Ok(SsaVal::Raw(
                builder.ins().uextend(types::I64, narrow),
                LIT_TAG_WORD,
            ))
        }
        PrimOpKind::Narrow16Word => {
            check_arity(op, 1, args.len())?;
            let v = unbox_int(sess.pipeline, builder, sess.vmctx, args[0]);
            let narrow = builder.ins().ireduce(types::I16, v);
            Ok(SsaVal::Raw(
                builder.ins().uextend(types::I64, narrow),
                LIT_TAG_WORD,
            ))
        }
        PrimOpKind::Narrow32Word => {
            check_arity(op, 1, args.len())?;
            let v = unbox_int(sess.pipeline, builder, sess.vmctx, args[0]);
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
            let a = unbox_double(sess.pipeline, builder, sess.vmctx, args[0]);
            Ok(SsaVal::Raw(builder.ins().fneg(a), LIT_TAG_DOUBLE))
        }
        PrimOpKind::DoubleFabs => {
            check_arity(op, 1, args.len())?;
            let a = unbox_double(sess.pipeline, builder, sess.vmctx, args[0]);
            Ok(SsaVal::Raw(builder.ins().fabs(a), LIT_TAG_DOUBLE))
        }
        PrimOpKind::FfiRintDouble => {
            // ghc-internal:rintDouble (C rint): round to nearest, ties to even.
            // Cranelift's `nearest` has exactly these semantics — pure codegen,
            // no host call. Unblocks GHC's specialized round @Double @Int.
            check_arity(op, 1, args.len())?;
            let a = unbox_double(sess.pipeline, builder, sess.vmctx, args[0]);
            Ok(SsaVal::Raw(builder.ins().nearest(a), LIT_TAG_DOUBLE))
        }
        // Double math unary: sqrt, exp, log, trig, etc. All via libm runtime calls.
        PrimOpKind::DoubleSqrt => {
            check_arity(op, 1, args.len())?;
            let a = unbox_double(sess.pipeline, builder, sess.vmctx, args[0]);
            Ok(SsaVal::Raw(builder.ins().sqrt(a), LIT_TAG_DOUBLE))
        }
        PrimOpKind::DoubleExp
        | PrimOpKind::DoubleExpM1
        | PrimOpKind::DoubleLog
        | PrimOpKind::DoubleLog1P
        | PrimOpKind::DoubleSin
        | PrimOpKind::DoubleCos
        | PrimOpKind::DoubleTan
        | PrimOpKind::DoubleAsin
        | PrimOpKind::DoubleAcos
        | PrimOpKind::DoubleAtan
        | PrimOpKind::DoubleSinh
        | PrimOpKind::DoubleCosh
        | PrimOpKind::DoubleTanh
        | PrimOpKind::DoubleAsinh
        | PrimOpKind::DoubleAcosh
        | PrimOpKind::DoubleAtanh => {
            check_arity(op, 1, args.len())?;
            let a = unbox_double(sess.pipeline, builder, sess.vmctx, args[0]);
            let fn_name = match op {
                PrimOpKind::DoubleExp => "runtime_double_exp",
                PrimOpKind::DoubleExpM1 => "runtime_double_expm1",
                PrimOpKind::DoubleLog => "runtime_double_log",
                PrimOpKind::DoubleLog1P => "runtime_double_log1p",
                PrimOpKind::DoubleSin => "runtime_double_sin",
                PrimOpKind::DoubleCos => "runtime_double_cos",
                PrimOpKind::DoubleTan => "runtime_double_tan",
                PrimOpKind::DoubleAsin => "runtime_double_asin",
                PrimOpKind::DoubleAcos => "runtime_double_acos",
                PrimOpKind::DoubleAtan => "runtime_double_atan",
                PrimOpKind::DoubleSinh => "runtime_double_sinh",
                PrimOpKind::DoubleCosh => "runtime_double_cosh",
                PrimOpKind::DoubleTanh => "runtime_double_tanh",
                PrimOpKind::DoubleAsinh => "runtime_double_asinh",
                PrimOpKind::DoubleAcosh => "runtime_double_acosh",
                PrimOpKind::DoubleAtanh => "runtime_double_atanh",
                _ => {
                    return Err(EmitError::InternalError(format!(
                        "unexpected double primop variant: {:?}",
                        op
                    )))
                }
            };
            let bits = builder.ins().bitcast(types::I64, MemFlags::new(), a);
            let result = emit_runtime_call(
                sess.pipeline,
                builder,
                fn_name,
                &[AbiParam::new(types::I64)],
                &[AbiParam::new(types::I64)],
                &[bits],
            )?;
            let d = builder.ins().bitcast(types::F64, MemFlags::new(), result);
            Ok(SsaVal::Raw(d, LIT_TAG_DOUBLE))
        }
        PrimOpKind::DoublePower => {
            check_arity(op, 2, args.len())?;
            let a = unbox_double(sess.pipeline, builder, sess.vmctx, args[0]);
            let b = unbox_double(sess.pipeline, builder, sess.vmctx, args[1]);
            let bits_a = builder.ins().bitcast(types::I64, MemFlags::new(), a);
            let bits_b = builder.ins().bitcast(types::I64, MemFlags::new(), b);
            let result = emit_runtime_call(
                sess.pipeline,
                builder,
                "runtime_double_power",
                &[AbiParam::new(types::I64), AbiParam::new(types::I64)],
                &[AbiParam::new(types::I64)],
                &[bits_a, bits_b],
            )?;
            let d = builder.ins().bitcast(types::F64, MemFlags::new(), result);
            Ok(SsaVal::Raw(d, LIT_TAG_DOUBLE))
        }
        PrimOpKind::FloatNegate => {
            check_arity(op, 1, args.len())?;
            let a = unbox_float(sess.pipeline, builder, sess.vmctx, args[0]);
            Ok(SsaVal::Raw(builder.ins().fneg(a), LIT_TAG_FLOAT))
        }
        // sqrtFloat# / fabsFloat# — native f32 opcodes (cranelift `sqrt`/`fabs`
        // are type-polymorphic). Parallel to DoubleSqrt/DoubleFabs; bit-exact, no
        // libm. (Float transcendentals are desugared to the Double path upstream.)
        PrimOpKind::FloatSqrt => {
            check_arity(op, 1, args.len())?;
            let a = unbox_float(sess.pipeline, builder, sess.vmctx, args[0]);
            Ok(SsaVal::Raw(builder.ins().sqrt(a), LIT_TAG_FLOAT))
        }
        PrimOpKind::FloatFabs => {
            check_arity(op, 1, args.len())?;
            let a = unbox_float(sess.pipeline, builder, sess.vmctx, args[0]);
            Ok(SsaVal::Raw(builder.ins().fabs(a), LIT_TAG_FLOAT))
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
            let addr = unbox_addr(sess.pipeline, builder, args[0]);
            let idx = unbox_int(sess.pipeline, builder, sess.vmctx, args[1]);
            let effective = builder.ins().iadd(addr, idx);
            let byte_val = builder.ins().load(types::I8, MemFlags::new(), effective, 0);
            let char_val = builder.ins().uextend(types::I64, byte_val);
            Ok(SsaVal::Raw(char_val, LIT_TAG_CHAR))
        }

        PrimOpKind::PlusAddr => {
            check_arity(op, 2, args.len())?;
            let addr = unbox_addr(sess.pipeline, builder, args[0]);
            let offset = unbox_int(sess.pipeline, builder, sess.vmctx, args[1]);
            Ok(SsaVal::Raw(builder.ins().iadd(addr, offset), LIT_TAG_ADDR))
        }

        // ---------------------------------------------------------------
        // Int64/Word64/Word8 \u2014 on 64-bit, these are just Int#/Word# with
        // different tags. GHC treats them identically at runtime.
        // ---------------------------------------------------------------

        // Int64 arithmetic
        PrimOpKind::Int64Add => {
            check_arity(op, 2, args.len())?;
            let a = unbox_int(sess.pipeline, builder, sess.vmctx, args[0]);
            let b = unbox_int(sess.pipeline, builder, sess.vmctx, args[1]);
            Ok(SsaVal::Raw(builder.ins().iadd(a, b), LIT_TAG_INT))
        }
        PrimOpKind::Int64Sub => {
            check_arity(op, 2, args.len())?;
            let a = unbox_int(sess.pipeline, builder, sess.vmctx, args[0]);
            let b = unbox_int(sess.pipeline, builder, sess.vmctx, args[1]);
            Ok(SsaVal::Raw(builder.ins().isub(a, b), LIT_TAG_INT))
        }
        PrimOpKind::Int64Mul => {
            check_arity(op, 2, args.len())?;
            let a = unbox_int(sess.pipeline, builder, sess.vmctx, args[0]);
            let b = unbox_int(sess.pipeline, builder, sess.vmctx, args[1]);
            Ok(SsaVal::Raw(builder.ins().imul(a, b), LIT_TAG_INT))
        }
        PrimOpKind::Int64Negate => {
            check_arity(op, 1, args.len())?;
            let a = unbox_int(sess.pipeline, builder, sess.vmctx, args[0]);
            Ok(SsaVal::Raw(builder.ins().ineg(a), LIT_TAG_INT))
        }
        PrimOpKind::Int64Shl => {
            check_arity(op, 2, args.len())?;
            let a = unbox_int(sess.pipeline, builder, sess.vmctx, args[0]);
            let b = unbox_int(sess.pipeline, builder, sess.vmctx, args[1]);
            Ok(SsaVal::Raw(builder.ins().ishl(a, b), LIT_TAG_INT))
        }
        PrimOpKind::Int64Shra => {
            check_arity(op, 2, args.len())?;
            let a = unbox_int(sess.pipeline, builder, sess.vmctx, args[0]);
            let b = unbox_int(sess.pipeline, builder, sess.vmctx, args[1]);
            Ok(SsaVal::Raw(builder.ins().sshr(a, b), LIT_TAG_INT))
        }

        // Int64 comparison
        PrimOpKind::Int64Lt => emit_int_compare(
            sess.pipeline,
            builder,
            sess.vmctx,
            op,
            IntCC::SignedLessThan,
            args,
            LIT_TAG_INT,
        ),
        PrimOpKind::Int64Le => emit_int_compare(
            sess.pipeline,
            builder,
            sess.vmctx,
            op,
            IntCC::SignedLessThanOrEqual,
            args,
            LIT_TAG_INT,
        ),
        PrimOpKind::Int64Gt => emit_int_compare(
            sess.pipeline,
            builder,
            sess.vmctx,
            op,
            IntCC::SignedGreaterThan,
            args,
            LIT_TAG_INT,
        ),
        PrimOpKind::Int64Ge => emit_int_compare(
            sess.pipeline,
            builder,
            sess.vmctx,
            op,
            IntCC::SignedGreaterThanOrEqual,
            args,
            LIT_TAG_INT,
        ),

        // Word64 arithmetic/bitwise
        PrimOpKind::Word64And => {
            check_arity(op, 2, args.len())?;
            let a = unbox_int(sess.pipeline, builder, sess.vmctx, args[0]);
            let b = unbox_int(sess.pipeline, builder, sess.vmctx, args[1]);
            Ok(SsaVal::Raw(builder.ins().band(a, b), LIT_TAG_WORD))
        }
        PrimOpKind::Word64Shl => {
            check_arity(op, 2, args.len())?;
            let a = unbox_int(sess.pipeline, builder, sess.vmctx, args[0]);
            let b = unbox_int(sess.pipeline, builder, sess.vmctx, args[1]);
            Ok(SsaVal::Raw(builder.ins().ishl(a, b), LIT_TAG_WORD))
        }
        PrimOpKind::Word64Shrl => {
            check_arity(op, 2, args.len())?;
            let a = unbox_int(sess.pipeline, builder, sess.vmctx, args[0]);
            let b = unbox_int(sess.pipeline, builder, sess.vmctx, args[1]);
            Ok(SsaVal::Raw(builder.ins().ushr(a, b), LIT_TAG_WORD))
        }
        PrimOpKind::Word64Or => {
            check_arity(op, 2, args.len())?;
            let a = unbox_int(sess.pipeline, builder, sess.vmctx, args[0]);
            let b = unbox_int(sess.pipeline, builder, sess.vmctx, args[1]);
            Ok(SsaVal::Raw(builder.ins().bor(a, b), LIT_TAG_WORD))
        }

        // Conversions between sized int/word types (no-ops on 64-bit)
        PrimOpKind::Word64ToInt64 | PrimOpKind::Int64ToInt | PrimOpKind::Int64ToWord64 => {
            check_arity(op, 1, args.len())?;
            let v = unbox_int(sess.pipeline, builder, sess.vmctx, args[0]);
            Ok(SsaVal::Raw(v, LIT_TAG_INT))
        }
        PrimOpKind::Word64ToWord | PrimOpKind::WordToWord64 => {
            // Identity on 64-bit; Word64# and Word# share the machine representation.
            check_arity(op, 1, args.len())?;
            let v = unbox_int(sess.pipeline, builder, sess.vmctx, args[0]);
            Ok(SsaVal::Raw(v, LIT_TAG_WORD))
        }
        PrimOpKind::IntToInt64 => {
            check_arity(op, 1, args.len())?;
            let v = unbox_int(sess.pipeline, builder, sess.vmctx, args[0]);
            Ok(SsaVal::Raw(v, LIT_TAG_INT))
        }
        PrimOpKind::Word8ToWord => {
            check_arity(op, 1, args.len())?;
            let v = unbox_int(sess.pipeline, builder, sess.vmctx, args[0]);
            Ok(SsaVal::Raw(v, LIT_TAG_WORD))
        }
        PrimOpKind::WordToWord8 => {
            // wordToWord8# :: Word# -> Word8#
            // Narrow to 8 bits
            check_arity(op, 1, args.len())?;
            let v = unbox_int(sess.pipeline, builder, sess.vmctx, args[0]);
            let mask = builder.ins().iconst(types::I64, 0xFF);
            let narrow = builder.ins().band(v, mask);
            Ok(SsaVal::Raw(narrow, LIT_TAG_WORD))
        }

        // Word8 arithmetic/comparison
        PrimOpKind::Word8Add => {
            check_arity(op, 2, args.len())?;
            let a = unbox_int(sess.pipeline, builder, sess.vmctx, args[0]);
            let b = unbox_int(sess.pipeline, builder, sess.vmctx, args[1]);
            let sum = builder.ins().iadd(a, b);
            Ok(SsaVal::Raw(builder.ins().band_imm(sum, 0xFF), LIT_TAG_WORD))
        }
        PrimOpKind::Word8Sub => {
            check_arity(op, 2, args.len())?;
            let a = unbox_int(sess.pipeline, builder, sess.vmctx, args[0]);
            let b = unbox_int(sess.pipeline, builder, sess.vmctx, args[1]);
            let diff = builder.ins().isub(a, b);
            Ok(SsaVal::Raw(
                builder.ins().band_imm(diff, 0xFF),
                LIT_TAG_WORD,
            ))
        }
        PrimOpKind::Word8Lt => emit_int_compare(
            sess.pipeline,
            builder,
            sess.vmctx,
            op,
            IntCC::UnsignedLessThan,
            args,
            LIT_TAG_INT,
        ),
        PrimOpKind::Word8Le => emit_int_compare(
            sess.pipeline,
            builder,
            sess.vmctx,
            op,
            IntCC::UnsignedLessThanOrEqual,
            args,
            LIT_TAG_INT,
        ),
        PrimOpKind::Word8Ge => emit_int_compare(
            sess.pipeline,
            builder,
            sess.vmctx,
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
            let a = unbox_int(sess.pipeline, builder, sess.vmctx, args[0]);
            let b = unbox_int(sess.pipeline, builder, sess.vmctx, args[1]);
            Ok(SsaVal::Raw(builder.ins().iadd(a, b), LIT_TAG_INT))
        }
        PrimOpKind::AddIntCCarry => {
            check_arity(op, 2, args.len())?;
            let a = unbox_int(sess.pipeline, builder, sess.vmctx, args[0]);
            let b = unbox_int(sess.pipeline, builder, sess.vmctx, args[1]);
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
            let a = unbox_int(sess.pipeline, builder, sess.vmctx, args[0]);
            let b = unbox_int(sess.pipeline, builder, sess.vmctx, args[1]);
            Ok(SsaVal::Raw(builder.ins().isub(a, b), LIT_TAG_WORD))
        }
        PrimOpKind::SubWordCCarry => {
            check_arity(op, 2, args.len())?;
            let a = unbox_int(sess.pipeline, builder, sess.vmctx, args[0]);
            let b = unbox_int(sess.pipeline, builder, sess.vmctx, args[1]);
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
            let a = unbox_int(sess.pipeline, builder, sess.vmctx, args[0]);
            let b = unbox_int(sess.pipeline, builder, sess.vmctx, args[1]);
            Ok(SsaVal::Raw(builder.ins().iadd(a, b), LIT_TAG_WORD))
        }
        PrimOpKind::AddWordCCarry => {
            check_arity(op, 2, args.len())?;
            let a = unbox_int(sess.pipeline, builder, sess.vmctx, args[0]);
            let b = unbox_int(sess.pipeline, builder, sess.vmctx, args[1]);
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
            let a = unbox_int(sess.pipeline, builder, sess.vmctx, args[0]);
            let b = unbox_int(sess.pipeline, builder, sess.vmctx, args[1]);
            Ok(SsaVal::Raw(builder.ins().smulhi(a, b), LIT_TAG_INT))
        }
        PrimOpKind::TimesInt2Lo => {
            check_arity(op, 2, args.len())?;
            let a = unbox_int(sess.pipeline, builder, sess.vmctx, args[0]);
            let b = unbox_int(sess.pipeline, builder, sess.vmctx, args[1]);
            Ok(SsaVal::Raw(builder.ins().imul(a, b), LIT_TAG_INT))
        }
        PrimOpKind::TimesInt2Overflow => {
            check_arity(op, 2, args.len())?;
            let a = unbox_int(sess.pipeline, builder, sess.vmctx, args[0]);
            let b = unbox_int(sess.pipeline, builder, sess.vmctx, args[1]);
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
            let a = unbox_int(sess.pipeline, builder, sess.vmctx, args[0]);
            let b = unbox_int(sess.pipeline, builder, sess.vmctx, args[1]);
            Ok(SsaVal::Raw(builder.ins().umulhi(a, b), LIT_TAG_WORD))
        }
        PrimOpKind::TimesWord2Lo => {
            check_arity(op, 2, args.len())?;
            let a = unbox_int(sess.pipeline, builder, sess.vmctx, args[0]);
            let b = unbox_int(sess.pipeline, builder, sess.vmctx, args[1]);
            Ok(SsaVal::Raw(builder.ins().imul(a, b), LIT_TAG_WORD))
        }
        // plusWord2# :: Word# -> Word# -> (# high, low #)
        PrimOpKind::WordAdd2Lo => {
            check_arity(op, 2, args.len())?;
            let a = unbox_int(sess.pipeline, builder, sess.vmctx, args[0]);
            let b = unbox_int(sess.pipeline, builder, sess.vmctx, args[1]);
            Ok(SsaVal::Raw(builder.ins().iadd(a, b), LIT_TAG_WORD))
        }
        PrimOpKind::WordAdd2Hi => {
            check_arity(op, 2, args.len())?;
            let a = unbox_int(sess.pipeline, builder, sess.vmctx, args[0]);
            let b = unbox_int(sess.pipeline, builder, sess.vmctx, args[1]);
            let sum = builder.ins().iadd(a, b);
            // High word of a+b is the carry-out: 1 iff the sum wrapped (sum < a).
            let carry = builder.ins().icmp(IntCC::UnsignedLessThan, sum, a);
            Ok(SsaVal::Raw(
                builder.ins().uextend(types::I64, carry),
                LIT_TAG_WORD,
            ))
        }
        // quotRemWord2# :: Word#(hi) -> Word#(lo) -> Word#(d) -> (# quot, rem #)
        // 128/64 division; native ghc-bignum's multi-precision division core.
        PrimOpKind::WordQuotRem2Quot | PrimOpKind::WordQuotRem2Rem => {
            check_arity(op, 3, args.len())?;
            let hi = unbox_int(sess.pipeline, builder, sess.vmctx, args[0]);
            let lo = unbox_int(sess.pipeline, builder, sess.vmctx, args[1]);
            let d = unbox_int(sess.pipeline, builder, sess.vmctx, args[2]);
            let name = if matches!(op, PrimOpKind::WordQuotRem2Quot) {
                "runtime_word2_quot"
            } else {
                "runtime_word2_rem"
            };
            let result = emit_runtime_call(
                sess.pipeline,
                builder,
                name,
                &i64_params(3),
                &[AbiParam::new(types::I64)],
                &[hi, lo, d],
            )?;
            Ok(SsaVal::Raw(result, LIT_TAG_WORD))
        }

        // quotRemWord# :: Word# -> Word# -> (# Word#, Word# #)
        PrimOpKind::QuotRemWordVal => {
            check_arity(op, 2, args.len())?;
            let a = unbox_int(sess.pipeline, builder, sess.vmctx, args[0]);
            let b = unbox_int(sess.pipeline, builder, sess.vmctx, args[1]);
            emit_div_zero_check(builder, b);
            Ok(SsaVal::Raw(builder.ins().udiv(a, b), LIT_TAG_WORD))
        }
        PrimOpKind::QuotRemWordRem => {
            check_arity(op, 2, args.len())?;
            let a = unbox_int(sess.pipeline, builder, sess.vmctx, args[0]);
            let b = unbox_int(sess.pipeline, builder, sess.vmctx, args[1]);
            emit_div_zero_check(builder, b);
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
            let size = unbox_int(sess.pipeline, builder, sess.vmctx, args[0]);
            let ba_ptr = emit_runtime_call(
                sess.pipeline,
                builder,
                "runtime_new_byte_array",
                &[AbiParam::new(types::I64)],
                &[AbiParam::new(types::I64)],
                &[size],
            )?;
            // Wrap in a Lit on the managed heap
            Ok(emit_lit_bytearray(
                builder,
                sess.vmctx,
                sess.gc_sig,
                sess.oom_func,
                ba_ptr,
            ))
        }

        PrimOpKind::UnsafeFreezeByteArray => {
            // unsafeFreezeByteArray# :: MutableByteArray# s -> State# s -> (# State# s, ByteArray# #)
            // Identity \u2014 mutable and immutable have the same representation
            Ok(args[0])
        }

        PrimOpKind::SizeofByteArray | PrimOpKind::SizeofMutableByteArray => {
            // sizeofByteArray# :: ByteArray# -> Int#
            let ba_ptr = unbox_bytearray(sess.pipeline, builder, args[0]);
            // Read u64 length from offset 0
            let len = builder.ins().load(types::I64, MemFlags::new(), ba_ptr, 0);
            Ok(SsaVal::Raw(len, LIT_TAG_INT))
        }

        PrimOpKind::ReadWord8Array | PrimOpKind::IndexWord8Array => {
            // readWord8Array# :: MutableByteArray# s -> Int# -> State# s -> (# State# s, Word# #)
            let ba_ptr = unbox_bytearray(sess.pipeline, builder, args[0]);
            let idx = unbox_int(sess.pipeline, builder, sess.vmctx, args[1]);
            // Data starts at offset 8
            let base = builder.ins().iadd_imm(ba_ptr, 8);
            let effective = builder.ins().iadd(base, idx);
            let byte = builder.ins().load(types::I8, MemFlags::new(), effective, 0);
            let val = builder.ins().uextend(types::I64, byte);
            Ok(SsaVal::Raw(val, LIT_TAG_WORD))
        }

        PrimOpKind::WriteWord8Array => {
            // writeWord8Array# :: MutableByteArray# s -> Int# -> Word# -> State# s -> State# s
            let ba_ptr = unbox_bytearray(sess.pipeline, builder, args[0]);
            let idx = unbox_int(sess.pipeline, builder, sess.vmctx, args[1]);
            let val = unbox_int(sess.pipeline, builder, sess.vmctx, args[2]);
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
            let ba_ptr = unbox_bytearray(sess.pipeline, builder, args[0]);
            let idx = unbox_int(sess.pipeline, builder, sess.vmctx, args[1]);
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
            let ba_ptr = unbox_bytearray(sess.pipeline, builder, args[0]);
            let idx = unbox_int(sess.pipeline, builder, sess.vmctx, args[1]);
            let val = unbox_int(sess.pipeline, builder, sess.vmctx, args[2]);
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
            let src = unbox_addr(sess.pipeline, builder, args[0]);
            let dest_ba = unbox_bytearray(sess.pipeline, builder, args[1]);
            let dest_off = unbox_int(sess.pipeline, builder, sess.vmctx, args[2]);
            let len = unbox_int(sess.pipeline, builder, sess.vmctx, args[3]);
            let _ = emit_runtime_call(
                sess.pipeline,
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
            let ba = unbox_bytearray(sess.pipeline, builder, args[0]);
            let off = unbox_int(sess.pipeline, builder, sess.vmctx, args[1]);
            let len = unbox_int(sess.pipeline, builder, sess.vmctx, args[2]);
            let val = unbox_int(sess.pipeline, builder, sess.vmctx, args[3]);
            let _ = emit_runtime_call(
                sess.pipeline,
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
            let ba = unbox_bytearray(sess.pipeline, builder, args[0]);
            let new_size = unbox_int(sess.pipeline, builder, sess.vmctx, args[1]);
            let _ = emit_runtime_call(
                sess.pipeline,
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
            let ba = unbox_bytearray(sess.pipeline, builder, args[0]);
            let new_size = unbox_int(sess.pipeline, builder, sess.vmctx, args[1]);
            let result = emit_runtime_call(
                sess.pipeline,
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
            let src = unbox_bytearray(sess.pipeline, builder, args[0]);
            let src_off = unbox_int(sess.pipeline, builder, sess.vmctx, args[1]);
            let dest = unbox_bytearray(sess.pipeline, builder, args[2]);
            let dest_off = unbox_int(sess.pipeline, builder, sess.vmctx, args[3]);
            let len = unbox_int(sess.pipeline, builder, sess.vmctx, args[4]);
            let _ = emit_runtime_call(
                sess.pipeline,
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
            let src = unbox_bytearray(sess.pipeline, builder, args[0]);
            let src_off = unbox_int(sess.pipeline, builder, sess.vmctx, args[1]);
            let dest = unbox_bytearray(sess.pipeline, builder, args[2]);
            let dest_off = unbox_int(sess.pipeline, builder, sess.vmctx, args[3]);
            let len = unbox_int(sess.pipeline, builder, sess.vmctx, args[4]);
            let _ = emit_runtime_call(
                sess.pipeline,
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
                return Err(EmitError::InvalidArity(
                    PrimOpKind::CompareByteArrays,
                    5,
                    args.len(),
                ));
            }
            let a = unbox_bytearray(sess.pipeline, builder, args[0]);
            let a_off = unbox_int(sess.pipeline, builder, sess.vmctx, args[1]);
            let b = unbox_bytearray(sess.pipeline, builder, args[2]);
            let b_off = unbox_int(sess.pipeline, builder, sess.vmctx, args[3]);
            let len = unbox_int(sess.pipeline, builder, sess.vmctx, args[4]);
            let result = emit_runtime_call(
                sess.pipeline,
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
            let addr = unbox_addr(sess.pipeline, builder, args[0]);
            let off = unbox_int(sess.pipeline, builder, sess.vmctx, args[1]);
            let ptr = builder.ins().iadd(addr, off);
            let byte = builder.ins().load(types::I8, MemFlags::trusted(), ptr, 0);
            let word = builder.ins().uextend(types::I64, byte);
            Ok(SsaVal::Raw(word, LIT_TAG_WORD))
        }
        PrimOpKind::ByteArrayContents => {
            // byteArrayContents# / mutableByteArrayContents# :: ByteArray# -> Addr#
            // The payload starts after the 8-byte length prefix.
            check_arity(op, 1, args.len())?;
            let ba = unbox_bytearray(sess.pipeline, builder, args[0]);
            Ok(SsaVal::Raw(
                builder.ins().iadd_imm(ba, 8),
                crate::layout::LIT_TAG_ADDR,
            ))
        }
        PrimOpKind::WriteWord8OffAddr => {
            // writeWord8OffAddr# :: Addr# -> Int# -> Word8# -> State# s -> State# s
            let addr = unbox_addr(sess.pipeline, builder, args[0]);
            let off = unbox_int(sess.pipeline, builder, sess.vmctx, args[1]);
            let val = unbox_int(sess.pipeline, builder, sess.vmctx, args[2]);
            let ptr = builder.ins().iadd(addr, off);
            let byte = builder.ins().ireduce(types::I8, val);
            builder.ins().store(MemFlags::trusted(), byte, ptr, 0);
            Ok(SsaVal::Raw(
                builder.ins().iconst(types::I64, 0),
                LIT_TAG_INT,
            ))
        }
        PrimOpKind::Clz8 => {
            // clz8# :: Word# -> Word#
            check_arity(op, 1, args.len())?;
            let v = unbox_int(sess.pipeline, builder, sess.vmctx, args[0]);
            let narrow = builder.ins().ireduce(types::I8, v);
            let clz8 = builder.ins().clz(narrow);
            let result = builder.ins().uextend(types::I64, clz8);
            Ok(SsaVal::Raw(result, LIT_TAG_WORD))
        }
        PrimOpKind::Clz => {
            // clz# :: Word# -> Word# (count leading zeros of a 64-bit word)
            check_arity(op, 1, args.len())?;
            let v = unbox_int(sess.pipeline, builder, sess.vmctx, args[0]);
            Ok(SsaVal::Raw(builder.ins().clz(v), LIT_TAG_WORD))
        }
        // subIntC# :: Int# -> Int# -> (# Int#, Int# #) (result, overflow flag)
        PrimOpKind::SubIntCVal => {
            check_arity(op, 2, args.len())?;
            let a = unbox_int(sess.pipeline, builder, sess.vmctx, args[0]);
            let b = unbox_int(sess.pipeline, builder, sess.vmctx, args[1]);
            Ok(SsaVal::Raw(builder.ins().isub(a, b), LIT_TAG_INT))
        }
        PrimOpKind::SubIntCCarry => {
            check_arity(op, 2, args.len())?;
            let a = unbox_int(sess.pipeline, builder, sess.vmctx, args[0]);
            let b = unbox_int(sess.pipeline, builder, sess.vmctx, args[1]);
            let diff = builder.ins().isub(a, b);
            // Signed-sub overflow: signs of a and b differ AND sign of result differs
            // from a, i.e. (a ^ b) & (a ^ diff) has bit 63 set.
            let xor_ab = builder.ins().bxor(a, b);
            let xor_ad = builder.ins().bxor(a, diff);
            let overflow_bits = builder.ins().band(xor_ab, xor_ad);
            let shifted = builder.ins().ushr_imm(overflow_bits, 63);
            Ok(SsaVal::Raw(shifted, LIT_TAG_INT))
        }
        PrimOpKind::RaiseUnderflow | PrimOpKind::RaiseOverflow | PrimOpKind::RaiseDivZero => {
            // raise{Underflow,Overflow,DivZero}# :: (# #) -> b — bottoming arithmetic
            // exceptions in ghc-bignum's check branches. GHC hoists these into shared
            // `let` bindings (e.g. `let z = raiseDivZero# in case d of 0## -> z; ...`),
            // and the JIT binds lets eagerly — so an EAGER error would fire even when
            // the divisor is nonzero. Emit a LAZY poison instead (same discipline as
            // the `error` sentinel): it only raises when actually forced in the taken
            // branch. kind: 0=DivZero, 1=Overflow/Underflow.
            let kind = match op {
                PrimOpKind::RaiseDivZero => 0u64,
                _ => 1u64,
            };
            let addr = crate::host_fns::error_poison_ptr_lazy(kind) as i64;
            let v = builder.ins().iconst(types::I64, addr);
            Ok(SsaVal::HeapPtr(v))
        }
        PrimOpKind::Raise => {
            // raise# :: a -> b — always errors
            let kind = 2; // UserError
            let kind_val = builder.ins().iconst(types::I64, kind as i64);

            if !args.is_empty() {
                let arg_ptr = crate::emit::expr::ensure_heap_ptr(
                    builder,
                    sess.vmctx,
                    sess.gc_sig,
                    sess.oom_func,
                    args[0],
                );
                // Materialize the message from the live argument in the host
                // (LitString, Text, String cons-lists, thunk forcing, fallback).
                let result = emit_runtime_call(
                    sess.pipeline,
                    builder,
                    "runtime_error_dynamic",
                    &[
                        AbiParam::new(types::I64), // vmctx
                        AbiParam::new(types::I64), // kind
                        AbiParam::new(types::I64), // arg
                    ],
                    &[AbiParam::new(types::I64)],
                    &[sess.vmctx, kind_val, arg_ptr],
                )?;
                return Ok(SsaVal::HeapPtr(result));
            }

            let _ = emit_runtime_call(
                sess.pipeline,
                builder,
                "runtime_error",
                &[AbiParam::new(types::I64)],
                &[AbiParam::new(types::I64)],
                &[kind_val],
            )?;
            Ok(SsaVal::Raw(
                builder.ins().iconst(types::I64, 0),
                crate::layout::LIT_TAG_INT,
            ))
        }
        PrimOpKind::FfiStrlen => {
            // strlen :: Addr# -> Int#
            let addr = unbox_addr(sess.pipeline, builder, args[0]);
            let result = emit_runtime_call(
                sess.pipeline,
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
            let ba = unbox_bytearray(sess.pipeline, builder, args[0]);
            let data_ptr = builder.ins().iadd_imm(ba, 8); // skip length prefix
            let off = unbox_int(sess.pipeline, builder, sess.vmctx, args[1]);
            let len = unbox_int(sess.pipeline, builder, sess.vmctx, args[2]);
            let cnt = unbox_int(sess.pipeline, builder, sess.vmctx, args[3]);
            let result = emit_runtime_call(
                sess.pipeline,
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
            let ba = unbox_bytearray(sess.pipeline, builder, args[0]);
            let data_ptr = builder.ins().iadd_imm(ba, 8); // skip length prefix
            let off = unbox_int(sess.pipeline, builder, sess.vmctx, args[1]);
            let len = unbox_int(sess.pipeline, builder, sess.vmctx, args[2]);
            let needle = unbox_int(sess.pipeline, builder, sess.vmctx, args[3]);
            let result = emit_runtime_call(
                sess.pipeline,
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
            let dest_ba = unbox_bytearray(sess.pipeline, builder, args[0]);
            let dest_ptr = builder.ins().iadd_imm(dest_ba, 8); // skip length prefix
            let src_ba = unbox_bytearray(sess.pipeline, builder, args[1]);
            let src_ptr = builder.ins().iadd_imm(src_ba, 8); // skip length prefix
            let off = unbox_int(sess.pipeline, builder, sess.vmctx, args[2]);
            let len = unbox_int(sess.pipeline, builder, sess.vmctx, args[3]);
            let _ = emit_runtime_call(
                sess.pipeline,
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

        PrimOpKind::FfiIntEncodeDouble | PrimOpKind::FfiWordEncodeDouble => {
            // __{int,word}_encodeDouble(mantissa, exp) -> Double# (returned as raw bits).
            check_arity(op, 2, args.len())?;
            let m = unbox_int(sess.pipeline, builder, sess.vmctx, args[0]);
            let e = unbox_int(sess.pipeline, builder, sess.vmctx, args[1]);
            let name = if matches!(op, PrimOpKind::FfiIntEncodeDouble) {
                "runtime_int_encode_double"
            } else {
                "runtime_word_encode_double"
            };
            let result = emit_runtime_call(
                sess.pipeline,
                builder,
                name,
                &i64_params(2),
                &[AbiParam::new(types::I64)],
                &[m, e],
            )?;
            let d = builder.ins().bitcast(types::F64, MemFlags::new(), result);
            Ok(SsaVal::Raw(d, LIT_TAG_DOUBLE))
        }

        // Float arithmetic + comparison
        PrimOpKind::FloatAdd => {
            check_arity(op, 2, args.len())?;
            let a = unbox_float(sess.pipeline, builder, sess.vmctx, args[0]);
            let b = unbox_float(sess.pipeline, builder, sess.vmctx, args[1]);
            Ok(SsaVal::Raw(builder.ins().fadd(a, b), LIT_TAG_FLOAT))
        }
        PrimOpKind::FloatSub => {
            check_arity(op, 2, args.len())?;
            let a = unbox_float(sess.pipeline, builder, sess.vmctx, args[0]);
            let b = unbox_float(sess.pipeline, builder, sess.vmctx, args[1]);
            Ok(SsaVal::Raw(builder.ins().fsub(a, b), LIT_TAG_FLOAT))
        }
        PrimOpKind::FloatMul => {
            check_arity(op, 2, args.len())?;
            let a = unbox_float(sess.pipeline, builder, sess.vmctx, args[0]);
            let b = unbox_float(sess.pipeline, builder, sess.vmctx, args[1]);
            Ok(SsaVal::Raw(builder.ins().fmul(a, b), LIT_TAG_FLOAT))
        }
        PrimOpKind::FloatDiv => {
            check_arity(op, 2, args.len())?;
            let a = unbox_float(sess.pipeline, builder, sess.vmctx, args[0]);
            let b = unbox_float(sess.pipeline, builder, sess.vmctx, args[1]);
            Ok(SsaVal::Raw(builder.ins().fdiv(a, b), LIT_TAG_FLOAT))
        }
        PrimOpKind::FloatEq => emit_f32_compare(
            sess.pipeline,
            builder,
            sess.vmctx,
            op,
            FloatCC::Equal,
            args,
            LIT_TAG_INT,
        ),
        PrimOpKind::FloatNe => emit_f32_compare(
            sess.pipeline,
            builder,
            sess.vmctx,
            op,
            FloatCC::NotEqual,
            args,
            LIT_TAG_INT,
        ),
        PrimOpKind::FloatLt => emit_f32_compare(
            sess.pipeline,
            builder,
            sess.vmctx,
            op,
            FloatCC::LessThan,
            args,
            LIT_TAG_INT,
        ),
        PrimOpKind::FloatLe => emit_f32_compare(
            sess.pipeline,
            builder,
            sess.vmctx,
            op,
            FloatCC::LessThanOrEqual,
            args,
            LIT_TAG_INT,
        ),
        PrimOpKind::FloatGt => emit_f32_compare(
            sess.pipeline,
            builder,
            sess.vmctx,
            op,
            FloatCC::GreaterThan,
            args,
            LIT_TAG_INT,
        ),
        PrimOpKind::FloatGe => emit_f32_compare(
            sess.pipeline,
            builder,
            sess.vmctx,
            op,
            FloatCC::GreaterThanOrEqual,
            args,
            LIT_TAG_INT,
        ),

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
            let size = unbox_int(sess.pipeline, builder, sess.vmctx, args[0]);
            let init_ptr = args[1].value();
            let arr_ptr = emit_runtime_call(
                sess.pipeline,
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
            Ok(emit_lit_boxed_array(
                builder,
                sess.vmctx,
                sess.gc_sig,
                sess.oom_func,
                arr_ptr,
                lit_tag,
            ))
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
                    op,
                    args.len(),
                    args
                )));
            }
            let arr_ptr = unbox_bytearray(sess.pipeline, builder, args[0]);
            let idx = unbox_int(sess.pipeline, builder, sess.vmctx, args[1]);
            let base = builder.ins().iadd_imm(arr_ptr, 8);
            let byte_offset = builder.ins().imul_imm(idx, 8);
            let effective = builder.ins().iadd(base, byte_offset);
            let loaded = builder
                .ins()
                .load(types::I64, MemFlags::new(), effective, 0);
            builder.declare_value_needs_stack_map(loaded);
            Ok(SsaVal::HeapPtr(loaded))
        }

        PrimOpKind::WriteSmallArray | PrimOpKind::WriteArray => {
            // writeSmallArray# :: SmallMutableArray# s a -> Int# -> a -> State# -> State#
            let arr_ptr = unbox_bytearray(sess.pipeline, builder, args[0]);
            let idx = unbox_int(sess.pipeline, builder, sess.vmctx, args[1]);
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
            let arr_ptr = unbox_bytearray(sess.pipeline, builder, args[0]);
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
            let src = unbox_bytearray(sess.pipeline, builder, args[0]);
            let src_off = unbox_int(sess.pipeline, builder, sess.vmctx, args[1]);
            let dest = unbox_bytearray(sess.pipeline, builder, args[2]);
            let dest_off = unbox_int(sess.pipeline, builder, sess.vmctx, args[3]);
            let len = unbox_int(sess.pipeline, builder, sess.vmctx, args[4]);
            let _ = emit_runtime_call(
                sess.pipeline,
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
            let arr = unbox_bytearray(sess.pipeline, builder, args[0]);
            let off = unbox_int(sess.pipeline, builder, sess.vmctx, args[1]);
            let len = unbox_int(sess.pipeline, builder, sess.vmctx, args[2]);
            let result = emit_runtime_call(
                sess.pipeline,
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
                sess.vmctx,
                sess.gc_sig,
                sess.oom_func,
                result,
                LIT_TAG_SMALLARRAY,
            ))
        }

        PrimOpKind::CloneArray | PrimOpKind::CloneMutableArray => {
            let arr = unbox_bytearray(sess.pipeline, builder, args[0]);
            let off = unbox_int(sess.pipeline, builder, sess.vmctx, args[1]);
            let len = unbox_int(sess.pipeline, builder, sess.vmctx, args[2]);
            let result = emit_runtime_call(
                sess.pipeline,
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
                sess.vmctx,
                sess.gc_sig,
                sess.oom_func,
                result,
                LIT_TAG_ARRAY,
            ))
        }

        PrimOpKind::ShrinkSmallMutableArray => {
            let arr = unbox_bytearray(sess.pipeline, builder, args[0]);
            let new_len = unbox_int(sess.pipeline, builder, sess.vmctx, args[1]);
            let _ = emit_runtime_call(
                sess.pipeline,
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
            let arr = unbox_bytearray(sess.pipeline, builder, args[0]);
            let idx = unbox_int(sess.pipeline, builder, sess.vmctx, args[1]);
            let expected = args[2].value();
            let new_val = args[3].value();
            let old = emit_runtime_call(
                sess.pipeline,
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
            let a = unbox_int(sess.pipeline, builder, sess.vmctx, args[0]);
            Ok(SsaVal::Raw(builder.ins().popcnt(a), LIT_TAG_WORD))
        }
        PrimOpKind::PopCnt8 => {
            check_arity(op, 1, args.len())?;
            let a = unbox_int(sess.pipeline, builder, sess.vmctx, args[0]);
            let masked = builder.ins().band_imm(a, 0xFF);
            Ok(SsaVal::Raw(builder.ins().popcnt(masked), LIT_TAG_WORD))
        }
        PrimOpKind::PopCnt16 => {
            check_arity(op, 1, args.len())?;
            let a = unbox_int(sess.pipeline, builder, sess.vmctx, args[0]);
            let masked = builder.ins().band_imm(a, 0xFFFF);
            Ok(SsaVal::Raw(builder.ins().popcnt(masked), LIT_TAG_WORD))
        }
        PrimOpKind::PopCnt32 => {
            check_arity(op, 1, args.len())?;
            let a = unbox_int(sess.pipeline, builder, sess.vmctx, args[0]);
            let masked = builder.ins().band_imm(a, 0xFFFF_FFFF);
            Ok(SsaVal::Raw(builder.ins().popcnt(masked), LIT_TAG_WORD))
        }
        PrimOpKind::Ctz | PrimOpKind::Ctz64 => {
            check_arity(op, 1, args.len())?;
            let a = unbox_int(sess.pipeline, builder, sess.vmctx, args[0]);
            Ok(SsaVal::Raw(builder.ins().ctz(a), LIT_TAG_WORD))
        }
        PrimOpKind::Ctz8 => {
            check_arity(op, 1, args.len())?;
            let a = unbox_int(sess.pipeline, builder, sess.vmctx, args[0]);
            // Set bit 8 so ctz stops there if all lower 8 bits are zero
            let with_sentinel = builder.ins().bor_imm(a, 0x100);
            Ok(SsaVal::Raw(builder.ins().ctz(with_sentinel), LIT_TAG_WORD))
        }
        PrimOpKind::Ctz16 => {
            check_arity(op, 1, args.len())?;
            let a = unbox_int(sess.pipeline, builder, sess.vmctx, args[0]);
            let with_sentinel = builder.ins().bor_imm(a, 0x10000);
            Ok(SsaVal::Raw(builder.ins().ctz(with_sentinel), LIT_TAG_WORD))
        }
        PrimOpKind::Ctz32 => {
            check_arity(op, 1, args.len())?;
            let a = unbox_int(sess.pipeline, builder, sess.vmctx, args[0]);
            let with_sentinel = builder.ins().bor_imm(a, 0x1_0000_0000);
            Ok(SsaVal::Raw(builder.ins().ctz(with_sentinel), LIT_TAG_WORD))
        }
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
    vmctx: Value,
    op: &PrimOpKind,
    cc: IntCC,
    args: &[SsaVal],
    tag: i64,
) -> Result<SsaVal, EmitError> {
    check_arity(op, 2, args.len())?;
    let a = unbox_int(pipeline, builder, vmctx, args[0]);
    let b = unbox_int(pipeline, builder, vmctx, args[1]);
    let cmp = builder.ins().icmp(cc, a, b);
    Ok(SsaVal::Raw(builder.ins().uextend(types::I64, cmp), tag))
}

fn emit_double_compare(
    pipeline: &mut CodegenPipeline,
    builder: &mut FunctionBuilder,
    vmctx: Value,
    op: &PrimOpKind,
    cc: FloatCC,
    args: &[SsaVal],
    tag: i64,
) -> Result<SsaVal, EmitError> {
    check_arity(op, 2, args.len())?;
    let a = unbox_double(pipeline, builder, vmctx, args[0]);
    let b = unbox_double(pipeline, builder, vmctx, args[1]);
    let cmp = builder.ins().fcmp(cc, a, b);
    Ok(SsaVal::Raw(builder.ins().uextend(types::I64, cmp), tag))
}

fn emit_f32_compare(
    pipeline: &mut CodegenPipeline,
    builder: &mut FunctionBuilder,
    vmctx: Value,
    op: &PrimOpKind,
    cc: FloatCC,
    args: &[SsaVal],
    tag: i64,
) -> Result<SsaVal, EmitError> {
    check_arity(op, 2, args.len())?;
    let a = unbox_float(pipeline, builder, vmctx, args[0]);
    let b = unbox_float(pipeline, builder, vmctx, args[1]);
    let cmp = builder.ins().fcmp(cc, a, b);
    Ok(SsaVal::Raw(builder.ins().uextend(types::I64, cmp), tag))
}

/// Guard a Con-unwrap step inside an unboxing loop.
///
/// Numeric/addr/bytearray boxing wrappers (`I#`, `W#`, `D#`, `Addr#`,
/// `ByteArray#`, ...) always have exactly ONE field. Blindly unwrapping
/// field 0 of an arbitrary multi-field Con (e.g. a `Text` response where
/// `Int#` was expected) silently turns a heap pointer into a "number" —
/// observed as proptest_jit_dispatch B2 (shape-mismatched effect resume
/// returned pointer-derived garbage instead of erroring).
///
/// Emits a `num_fields == 1` check. The failing path calls
/// `runtime_case_trap` (records the offending Con in the diagnostics, sets
/// the pending `RuntimeError` that the machine surfaces before the result is
/// used) and jumps to `next_block` with the returned poison pointer. Leaves
/// the builder positioned in a fresh sealed block where the single-field
/// unwrap should be emitted.
fn emit_boxing_wrapper_guard(
    pipeline: &mut CodegenPipeline,
    builder: &mut FunctionBuilder,
    curr_v: Value,
    next_block: ir::Block,
) {
    let num_fields = builder.ins().load(
        types::I16,
        MemFlags::trusted(),
        curr_v,
        layout::CON_NUM_FIELDS_OFFSET as i32,
    );
    let is_single = builder.ins().icmp_imm(IntCC::Equal, num_fields, 1);
    let unwrap_block = builder.create_block();
    let shape_trap_block = builder.create_block();
    builder
        .ins()
        .brif(is_single, unwrap_block, &[], shape_trap_block, &[]);

    builder.switch_to_block(shape_trap_block);
    builder.seal_block(shape_trap_block);
    let trap_fn = pipeline
        .module
        .declare_function(
            "runtime_case_trap",
            Linkage::Import,
            &crate::emit::runtime_case_trap_sig(pipeline.isa.default_call_conv()),
        )
        .expect("declare runtime_case_trap");
    let trap_ref = pipeline.module.declare_func_in_func(trap_fn, builder.func);
    // No expected-tags list (the expectation is "a 1-field boxing wrapper",
    // not a constructor set); pass a valid dummy slot so the host fn's slice
    // construction stays sound with num_alts = 0.
    let dummy_ss = builder.create_sized_stack_slot(ir::StackSlotData::new(
        ir::StackSlotKind::ExplicitSlot,
        8,
        3, // align 8
    ));
    let dummy_addr = builder.ins().stack_addr(types::I64, dummy_ss, 0);
    let zero = builder.ins().iconst(types::I64, 0);
    let call = builder
        .ins()
        .call(trap_ref, &[curr_v, zero, dummy_addr, zero, zero]);
    let poison = builder.inst_results(call)[0];
    builder.ins().jump(next_block, &[BlockArg::Value(poison)]);

    builder.switch_to_block(unwrap_block);
    builder.seal_block(unwrap_block);
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

            builder.ins().jump(start_block, &[BlockArg::Value(v)]);

            builder.switch_to_block(start_block);
            let curr_v = builder.block_params(start_block)[0];
            let tag = builder
                .ins()
                .load(types::I8, MemFlags::trusted(), curr_v, 0);
            let is_con = builder
                .ins()
                .icmp_imm(IntCC::Equal, tag, layout::TAG_CON as i64);

            let con_block = builder.create_block();
            builder.ins().brif(
                is_con,
                con_block,
                &[],
                next_block,
                &[BlockArg::Value(curr_v)],
            );

            builder.switch_to_block(con_block);
            builder.seal_block(con_block);
            // Boxing wrappers have exactly one field; trap cleanly otherwise
            // (proptest_jit_dispatch B2 — see emit_boxing_wrapper_guard).
            emit_boxing_wrapper_guard(pipeline, builder, curr_v, next_block);
            let field0 = builder.ins().load(
                types::I64,
                MemFlags::trusted(),
                curr_v,
                layout::CON_FIELDS_OFFSET as i32,
            );
            builder.ins().jump(start_block, &[BlockArg::Value(field0)]);

            builder.switch_to_block(next_block);
            builder.seal_block(start_block);
            builder.seal_block(next_block);
            let v_final = builder.block_params(next_block)[0];

            let raw_val =
                builder
                    .ins()
                    .load(types::I64, MemFlags::trusted(), v_final, LIT_VALUE_OFFSET);
            let lit_tag =
                builder
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
fn unbox_bytearray(
    pipeline: &mut CodegenPipeline,
    builder: &mut FunctionBuilder,
    val: SsaVal,
) -> Value {
    match val {
        SsaVal::Raw(v, _) => v,
        SsaVal::HeapPtr(v) => {
            let start_block = builder.create_block();
            let next_block = builder.create_block();
            builder.append_block_param(start_block, types::I64);
            builder.append_block_param(next_block, types::I64);

            builder.ins().jump(start_block, &[BlockArg::Value(v)]);

            builder.switch_to_block(start_block);
            let curr_v = builder.block_params(start_block)[0];
            let tag = builder
                .ins()
                .load(types::I8, MemFlags::trusted(), curr_v, 0);
            let is_con = builder
                .ins()
                .icmp_imm(IntCC::Equal, tag, layout::TAG_CON as i64);

            let con_block = builder.create_block();
            builder.ins().brif(
                is_con,
                con_block,
                &[],
                next_block,
                &[BlockArg::Value(curr_v)],
            );

            builder.switch_to_block(con_block);
            builder.seal_block(con_block);
            // Boxing wrappers have exactly one field; trap cleanly otherwise
            // (proptest_jit_dispatch B2 — see emit_boxing_wrapper_guard).
            emit_boxing_wrapper_guard(pipeline, builder, curr_v, next_block);
            let field0 = builder.ins().load(
                types::I64,
                MemFlags::trusted(),
                curr_v,
                layout::CON_FIELDS_OFFSET as i32,
            );
            builder.ins().jump(start_block, &[BlockArg::Value(field0)]);

            builder.switch_to_block(next_block);
            builder.seal_block(start_block);
            builder.seal_block(next_block);
            let v_final = builder.block_params(next_block)[0];

            let raw_val =
                builder
                    .ins()
                    .load(types::I64, MemFlags::trusted(), v_final, LIT_VALUE_OFFSET);
            let lit_tag =
                builder
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

/// Unbox a numeric literal from a heap object. Handles Raw passthrough,
/// Con wrapper recursion, and thunk forcing.
fn unbox_numeric(
    pipeline: &mut CodegenPipeline,
    builder: &mut FunctionBuilder,
    vmctx: Value,
    val: SsaVal,
    load_type: types::Type,
) -> Value {
    match val {
        SsaVal::Raw(v, _) => v,
        SsaVal::HeapPtr(v) => {
            let start_block = builder.create_block();
            let next_block = builder.create_block();
            builder.append_block_param(start_block, types::I64);
            builder.append_block_param(next_block, types::I64);

            builder.ins().jump(start_block, &[BlockArg::Value(v)]);

            builder.switch_to_block(start_block);
            let curr_v = builder.block_params(start_block)[0];
            let tag = builder
                .ins()
                .load(types::I8, MemFlags::trusted(), curr_v, 0);
            let is_con = builder
                .ins()
                .icmp_imm(IntCC::Equal, tag, layout::TAG_CON as i64);

            let con_block = builder.create_block();
            let check_thunk_block = builder.create_block();
            builder
                .ins()
                .brif(is_con, con_block, &[], check_thunk_block, &[]);

            builder.switch_to_block(con_block);
            builder.seal_block(con_block);
            // Boxing wrappers have exactly one field; trap cleanly otherwise
            // (proptest_jit_dispatch B2 — see emit_boxing_wrapper_guard).
            emit_boxing_wrapper_guard(pipeline, builder, curr_v, next_block);
            let field0 = builder.ins().load(
                types::I64,
                MemFlags::trusted(),
                curr_v,
                layout::CON_FIELDS_OFFSET as i32,
            );
            builder.ins().jump(start_block, &[BlockArg::Value(field0)]);

            // Defense in depth: force thunks instead of trapping
            builder.switch_to_block(check_thunk_block);
            builder.seal_block(check_thunk_block);
            let is_thunk = builder
                .ins()
                .icmp_imm(IntCC::Equal, tag, layout::TAG_THUNK as i64);
            let thunk_force_block = builder.create_block();
            builder.ins().brif(
                is_thunk,
                thunk_force_block,
                &[],
                next_block,
                &[BlockArg::Value(curr_v)],
            );
            builder.switch_to_block(thunk_force_block);
            builder.seal_block(thunk_force_block);
            let force_fn = pipeline
                .module
                .declare_function(
                    "heap_force",
                    Linkage::Import,
                    &crate::emit::heap_force_sig(pipeline.isa.default_call_conv()),
                )
                .expect("declare heap_force");
            let force_ref = pipeline.module.declare_func_in_func(force_fn, builder.func);
            let inst = builder.ins().call(force_ref, &[vmctx, curr_v]);
            let forced = builder.inst_results(inst)[0];
            builder.ins().jump(start_block, &[BlockArg::Value(forced)]);

            builder.switch_to_block(next_block);
            builder.seal_block(start_block);
            builder.seal_block(next_block);
            let v_final = builder.block_params(next_block)[0];

            // Guard the final load: a numeric unbox must land on a TAG_LIT whose
            // lit-tag holds a value of THIS unbox's class, not a foreign payload
            // reinterpreted as one. Accepted lit-tags by load width:
            //   * I64 integer unbox → INT / WORD / CHAR (each stores a 64-bit
            //     integer or codepoint; Ord# feeds a CHAR lit, Word ops feed a
            //     WORD lit — all legitimate, so they must pass);
            //   * F64 double unbox → DOUBLE only;
            //   * F32 float  unbox → FLOAT only.
            // Anything else is rejected: a pointer-valued STRING / BYTEARRAY /
            // SMALLARRAY / ARRAY lit (whose payload is an ADDRESS — the original
            // f137d34 witness Str("")), a non-Lit object, OR a numeric lit of
            // the wrong float/integer class (e.g. a DOUBLE response forced by an
            // Int# continuation — witness Double(3.5), whose IEEE-754 bits would
            // otherwise load as a garbage i64). Trap cleanly via
            // runtime_case_trap instead; the poison object it returns is loaded
            // from below, but the pending RuntimeError is surfaced before the
            // garbage can be observed. Only ill-typed Core reaches a class
            // mismatch — valid GHC output emits explicit Int2Double/Double2Int —
            // so this cannot regress well-typed programs.
            let obj_tag = builder
                .ins()
                .load(types::I8, MemFlags::trusted(), v_final, 0);
            let not_lit = builder
                .ins()
                .icmp_imm(IntCC::NotEqual, obj_tag, layout::TAG_LIT as i64);
            let lit_tag =
                builder
                    .ins()
                    .load(types::I8, MemFlags::trusted(), v_final, LIT_TAG_OFFSET);
            let wrong_class = if load_type == types::F64 {
                builder
                    .ins()
                    .icmp_imm(IntCC::NotEqual, lit_tag, LIT_TAG_DOUBLE)
            } else if load_type == types::F32 {
                builder
                    .ins()
                    .icmp_imm(IntCC::NotEqual, lit_tag, LIT_TAG_FLOAT)
            } else {
                // Integer-width unbox: reject any lit-tag above CHAR — i.e.
                // FLOAT(3) / DOUBLE(4) / STRING(5) / BYTEARRAY(7) / SMALLARRAY(8)
                // / ARRAY(9). INT(0) / WORD(1) / CHAR(2) pass.
                builder
                    .ins()
                    .icmp_imm(IntCC::UnsignedGreaterThan, lit_tag, LIT_TAG_CHAR)
            };
            let bad = builder.ins().bor(not_lit, wrong_class);
            let load_block = builder.create_block();
            builder.append_block_param(load_block, types::I64);
            let lit_trap_block = builder.create_block();
            builder.ins().brif(
                bad,
                lit_trap_block,
                &[],
                load_block,
                &[BlockArg::Value(v_final)],
            );

            builder.switch_to_block(lit_trap_block);
            builder.seal_block(lit_trap_block);
            let trap_fn = pipeline
                .module
                .declare_function(
                    "runtime_case_trap",
                    Linkage::Import,
                    &crate::emit::runtime_case_trap_sig(pipeline.isa.default_call_conv()),
                )
                .expect("declare runtime_case_trap");
            let trap_ref = pipeline.module.declare_func_in_func(trap_fn, builder.func);
            let dummy_ss = builder.create_sized_stack_slot(ir::StackSlotData::new(
                ir::StackSlotKind::ExplicitSlot,
                8,
                3, // align 8
            ));
            let dummy_addr = builder.ins().stack_addr(types::I64, dummy_ss, 0);
            let zero = builder.ins().iconst(types::I64, 0);
            let call = builder
                .ins()
                .call(trap_ref, &[v_final, zero, dummy_addr, zero, zero]);
            let poison = builder.inst_results(call)[0];
            builder.ins().jump(load_block, &[BlockArg::Value(poison)]);

            builder.switch_to_block(load_block);
            builder.seal_block(load_block);
            let v_load = builder.block_params(load_block)[0];
            builder
                .ins()
                .load(load_type, MemFlags::trusted(), v_load, LIT_VALUE_OFFSET)
        }
    }
}

pub fn unbox_int(
    pipeline: &mut CodegenPipeline,
    builder: &mut FunctionBuilder,
    vmctx: Value,
    val: SsaVal,
) -> Value {
    unbox_numeric(pipeline, builder, vmctx, val, types::I64)
}

pub fn unbox_double(
    pipeline: &mut CodegenPipeline,
    builder: &mut FunctionBuilder,
    vmctx: Value,
    val: SsaVal,
) -> Value {
    unbox_numeric(pipeline, builder, vmctx, val, types::F64)
}

pub fn unbox_float(
    pipeline: &mut CodegenPipeline,
    builder: &mut FunctionBuilder,
    vmctx: Value,
    val: SsaVal,
) -> Value {
    unbox_numeric(pipeline, builder, vmctx, val, types::F32)
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
