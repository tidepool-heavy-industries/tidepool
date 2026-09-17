//! Pure floating operations retain GHC identities and exact representations.

use super::primitives::ScalarFamily;
use cranelift_codegen::ir::{
    self,
    condcodes::{FloatCC, IntCC},
    types, AbiParam, InstBuilder, MemFlags, Value,
};
use cranelift_frontend::FunctionBuilder;
use cranelift_module::{Linkage, Module};
use tidepool_repr::execution_schema::{
    ForeignConvention, OperationIdentity, ResultContract, RuntimeRep, Signature,
};

fn returns_exact(signature: &Signature, expected: &[RuntimeRep]) -> bool {
    match &signature.results {
        ResultContract::Returns(reps) => reps == expected,
        ResultContract::NoSuccess | ResultContract::CallerResult => false,
    }
}

fn classify_symbol(symbol: &str) -> Option<(u8, ClassificationKind)> {
    match symbol {
        "isFloatNaN" => Some((32, ClassificationKind::NaN)),
        "isFloatInfinite" => Some((32, ClassificationKind::Infinite)),
        "isFloatNegativeZero" => Some((32, ClassificationKind::NegativeZero)),
        "isFloatDenormalized" => Some((32, ClassificationKind::Denormalized)),
        "isFloatFinite" => Some((32, ClassificationKind::Finite)),
        "isDoubleNaN" => Some((64, ClassificationKind::NaN)),
        "isDoubleInfinite" => Some((64, ClassificationKind::Infinite)),
        "isDoubleNegativeZero" => Some((64, ClassificationKind::NegativeZero)),
        "isDoubleDenormalized" => Some((64, ClassificationKind::Denormalized)),
        "isDoubleFinite" => Some((64, ClassificationKind::Finite)),
        _ => None,
    }
}

pub(super) const DECODE_DOUBLE_INT64_HOST: &str = "prepared_decode_double_int64";

pub(super) fn recognize_decode_double_int64(
    identity: &OperationIdentity,
    signature: &Signature,
) -> bool {
    matches!(identity, OperationIdentity::PrimOp(name) if name == "decodeDouble_Int64#")
        && signature.arguments == [RuntimeRep::Float(64)]
        && returns_exact(signature, &[RuntimeRep::Int(64), RuntimeRep::Int(64)])
}

/// Decode one bit-exact Double into GHC's `(mantissa, exponent)` result.
///
/// # Safety
/// `output` points to two writable `i64` words in the generated caller's frame.
pub(super) unsafe extern "C" fn prepared_decode_double_int64(bits: u64, output: *mut i64) {
    let (mantissa, exponent) = tidepool_bignum::decode_double_int64(f64::from_bits(bits));
    unsafe {
        output.write(mantissa);
        output.add(1).write(exponent);
    }
}

pub(super) fn emit_decode_double_int64(
    builder: &mut FunctionBuilder<'_>,
    pipeline: &mut crate::pipeline::CodegenPipeline,
    argument: Value,
) -> Result<Vec<Value>, super::CompileError> {
    let mut signature = ir::Signature::new(pipeline.isa.default_call_conv());
    signature.params = vec![AbiParam::new(types::I64), AbiParam::new(types::I64)];
    let host = pipeline
        .module
        .declare_function(DECODE_DOUBLE_INT64_HOST, Linkage::Import, &signature)
        .map_err(|error| crate::pipeline::PipelineError::Declaration(error.to_string()))?;
    let host = pipeline.module.declare_func_in_func(host, builder.func);
    let slot = builder.create_sized_stack_slot(ir::StackSlotData::new(
        ir::StackSlotKind::ExplicitSlot,
        16,
        3,
    ));
    let output = builder.ins().stack_addr(types::I64, slot, 0);
    let bits = builder.ins().bitcast(types::I64, MemFlags::new(), argument);
    builder.ins().call(host, &[bits, output]);
    Ok(vec![
        builder
            .ins()
            .load(types::I64, MemFlags::trusted(), output, 0),
        builder
            .ins()
            .load(types::I64, MemFlags::trusted(), output, 8),
    ])
}

pub(super) const ENCODE_DOUBLE_INT_HOST: &str = "prepared_encode_double_int";
pub(super) const ENCODE_DOUBLE_WORD_HOST: &str = "prepared_encode_double_word";

/// ghc-bignum's `__int_encodeDouble` / `__word_encodeDouble`: `mantissa * 2^exp`,
/// correctly rounded. `Some(signed)` for an exactly catalogued call.
pub(super) fn recognize_encode_double(
    identity: &OperationIdentity,
    signature: &Signature,
) -> Option<bool> {
    let OperationIdentity::Intrinsic {
        symbol,
        convention: ForeignConvention::CCall,
    } = identity
    else {
        return None;
    };
    let signed = match symbol.as_str() {
        "__int_encodeDouble" => true,
        "__word_encodeDouble" => false,
        _ => return None,
    };
    let mantissa = if signed {
        RuntimeRep::Int(64)
    } else {
        RuntimeRep::Word(64)
    };
    (signature.arguments == [mantissa, RuntimeRep::Int(64), RuntimeRep::Void]
        && returns_exact(signature, &[RuntimeRep::Float(64)]))
    .then_some(signed)
}

pub(super) extern "C" fn prepared_encode_double_int(mantissa: i64, exponent: i64) -> u64 {
    tidepool_bignum::encode_double(mantissa, exponent).to_bits()
}

pub(super) extern "C" fn prepared_encode_double_word(mantissa: u64, exponent: i64) -> u64 {
    tidepool_bignum::encode_double_word(mantissa, exponent).to_bits()
}

pub(super) fn emit_encode_double(
    builder: &mut FunctionBuilder<'_>,
    pipeline: &mut crate::pipeline::CodegenPipeline,
    signed: bool,
    mantissa: Value,
    exponent: Value,
) -> Result<Vec<Value>, super::CompileError> {
    let mut signature = ir::Signature::new(pipeline.isa.default_call_conv());
    signature.params = vec![AbiParam::new(types::I64), AbiParam::new(types::I64)];
    signature.returns = vec![AbiParam::new(types::I64)];
    let name = if signed {
        ENCODE_DOUBLE_INT_HOST
    } else {
        ENCODE_DOUBLE_WORD_HOST
    };
    let host = pipeline
        .module
        .declare_function(name, Linkage::Import, &signature)
        .map_err(|error| crate::pipeline::PipelineError::Declaration(error.to_string()))?;
    let host = pipeline.module.declare_func_in_func(host, builder.func);
    let call = builder.ins().call(host, &[mantissa, exponent]);
    let bits = builder.inst_results(call)[0];
    Ok(vec![builder.ins().bitcast(types::F64, MemFlags::new(), bits)])
}

pub(super) const LIBM_HOST: &str = "prepared_float_libm";

/// GHC's transcendental Double/Float primops, evaluated by Rust's `f64`/`f32`
/// methods (the platform libm GHC itself calls).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub(super) enum LibmFunction {
    Exp = 0,
    Expm1,
    Log,
    Log1p,
    Sin,
    Cos,
    Tan,
    Asin,
    Acos,
    Atan,
    Sinh,
    Cosh,
    Tanh,
    Asinh,
    Acosh,
    Atanh,
    Power,
}

impl LibmFunction {
    const ALL: [Self; 17] = [
        Self::Exp,
        Self::Expm1,
        Self::Log,
        Self::Log1p,
        Self::Sin,
        Self::Cos,
        Self::Tan,
        Self::Asin,
        Self::Acos,
        Self::Atan,
        Self::Sinh,
        Self::Cosh,
        Self::Tanh,
        Self::Asinh,
        Self::Acosh,
        Self::Atanh,
        Self::Power,
    ];

    /// `(function, width)` for a GHC primop name.
    fn of_primop(name: &str) -> Option<(Self, u8)> {
        let (stem, width) = if let Some(stem) = name.strip_suffix("Double#") {
            (stem, 64)
        } else if let Some(stem) = name.strip_suffix("Float#") {
            (stem, 32)
        } else {
            return match name {
                "**##" => Some((Self::Power, 64)),
                _ => None,
            };
        };
        let function = match stem {
            "exp" => Self::Exp,
            "expm1" => Self::Expm1,
            "log" => Self::Log,
            "log1p" => Self::Log1p,
            "sin" => Self::Sin,
            "cos" => Self::Cos,
            "tan" => Self::Tan,
            "asin" => Self::Asin,
            "acos" => Self::Acos,
            "atan" => Self::Atan,
            "sinh" => Self::Sinh,
            "cosh" => Self::Cosh,
            "tanh" => Self::Tanh,
            "asinh" => Self::Asinh,
            "acosh" => Self::Acosh,
            "atanh" => Self::Atanh,
            "power" => Self::Power,
            _ => return None,
        };
        Some((function, width))
    }

    fn arity(self) -> usize {
        if self == Self::Power {
            2
        } else {
            1
        }
    }
}

/// `code` names a [`LibmFunction`]; operands and result are IEEE bits of
/// `width` (32 or 64).
pub(super) extern "C" fn prepared_float_libm(code: i64, width: i64, x: u64, y: u64) -> u64 {
    let Some(function) = LibmFunction::ALL.get(code as usize).copied() else {
        return f64::NAN.to_bits();
    };
    macro_rules! apply {
        ($x:expr, $y:expr) => {
            match function {
                LibmFunction::Exp => $x.exp(),
                LibmFunction::Expm1 => $x.exp_m1(),
                LibmFunction::Log => $x.ln(),
                LibmFunction::Log1p => $x.ln_1p(),
                LibmFunction::Sin => $x.sin(),
                LibmFunction::Cos => $x.cos(),
                LibmFunction::Tan => $x.tan(),
                LibmFunction::Asin => $x.asin(),
                LibmFunction::Acos => $x.acos(),
                LibmFunction::Atan => $x.atan(),
                LibmFunction::Sinh => $x.sinh(),
                LibmFunction::Cosh => $x.cosh(),
                LibmFunction::Tanh => $x.tanh(),
                LibmFunction::Asinh => $x.asinh(),
                LibmFunction::Acosh => $x.acosh(),
                LibmFunction::Atanh => $x.atanh(),
                LibmFunction::Power => $x.powf($y),
            }
        };
    }
    if width == 32 {
        let (x, y) = (f32::from_bits(x as u32), f32::from_bits(y as u32));
        u64::from(apply!(x, y).to_bits())
    } else {
        let (x, y) = (f64::from_bits(x), f64::from_bits(y));
        apply!(x, y).to_bits()
    }
}

pub(super) fn emit_libm(
    builder: &mut FunctionBuilder<'_>,
    pipeline: &mut crate::pipeline::CodegenPipeline,
    function: LibmFunction,
    width: u8,
    arguments: &[Value],
) -> Result<Vec<Value>, super::CompileError> {
    let mut signature = ir::Signature::new(pipeline.isa.default_call_conv());
    signature.params = vec![AbiParam::new(types::I64); 4];
    signature.returns = vec![AbiParam::new(types::I64)];
    let host = pipeline
        .module
        .declare_function(LIBM_HOST, Linkage::Import, &signature)
        .map_err(|error| crate::pipeline::PipelineError::Declaration(error.to_string()))?;
    let host = pipeline.module.declare_func_in_func(host, builder.func);
    let (int_ty, float_ty) = if width == 32 {
        (types::I32, types::F32)
    } else {
        (types::I64, types::F64)
    };
    let mut words = Vec::with_capacity(2);
    for index in 0..2 {
        let word = match arguments.get(index) {
            Some(&operand) if index < function.arity() => {
                let bits = builder.ins().bitcast(int_ty, MemFlags::new(), operand);
                if width == 32 {
                    builder.ins().uextend(types::I64, bits)
                } else {
                    bits
                }
            }
            _ => builder.ins().iconst(types::I64, 0),
        };
        words.push(word);
    }
    let code = builder.ins().iconst(types::I64, function as i64);
    let width_word = builder.ins().iconst(types::I64, i64::from(width));
    let call = builder
        .ins()
        .call(host, &[code, width_word, words[0], words[1]]);
    let bits = builder.inst_results(call)[0];
    let bits = if width == 32 {
        builder.ins().ireduce(types::I32, bits)
    } else {
        bits
    };
    Ok(vec![builder.ins().bitcast(float_ty, MemFlags::new(), bits)])
}

/// A catalogued transcendental primop with its exact signature.
pub(super) fn recognize_libm(
    identity: &OperationIdentity,
    signature: &Signature,
) -> Option<(LibmFunction, u8)> {
    let OperationIdentity::PrimOp(name) = identity else {
        return None;
    };
    let (function, width) = LibmFunction::of_primop(name)?;
    (signature.arguments == vec![RuntimeRep::Float(width); function.arity()]
        && returns_exact(signature, &[RuntimeRep::Float(width)]))
    .then_some((function, width))
}

pub(super) struct FloatingFamily;

#[derive(Clone, Copy)]
pub(super) enum FloatingOperation {
    NearestDouble,
    Negate,
    Unary(UnaryKind),
    Binary(BinaryKind),
    Compare(CompareKind),
    Convert { from_float: bool },
    /// A 64-bit integer (signed or unsigned) to a float of `width` bits.
    FromInteger { signed: bool, width: u8 },
    Classify { width: u8, kind: ClassificationKind },
}

#[derive(Clone, Copy)]
pub(super) enum ClassificationKind {
    NaN,
    Infinite,
    NegativeZero,
    /// Subnormal: zero exponent bits, nonzero mantissa.
    Denormalized,
    Finite,
}

#[derive(Clone, Copy)]
pub(super) enum UnaryKind {
    Abs,
    Sqrt,
}

#[derive(Clone, Copy)]
pub(super) enum BinaryKind {
    Add,
    Sub,
    Mul,
    Div,
}

#[derive(Clone, Copy)]
pub(super) enum CompareKind {
    Eq,
    Ne,
    Lt,
    Le,
    Gt,
    Ge,
}

impl ScalarFamily for FloatingFamily {
    type Operation = FloatingOperation;

    /// This C symbol has no Haskell unfolding: ghc-internal implements it in
    /// C as ties-even rounding. Cranelift nearest implements that operation,
    /// preserving signed zero and IEEE exceptional values without a host call.
    /// A similarly named primop or a different signature has no such authority.
    fn recognize(identity: &OperationIdentity, signature: &Signature) -> Option<Self::Operation> {
        let OperationIdentity::Intrinsic {
            symbol,
            convention: ForeignConvention::CCall,
        } = identity
        else {
            let OperationIdentity::PrimOp(name) = identity else {
                return None;
            };
            let operation = match name.as_str() {
                "negateDouble#" | "negateFloat#" => FloatingOperation::Negate,
                "fabsDouble#" | "fabsFloat#" => FloatingOperation::Unary(UnaryKind::Abs),
                "sqrtDouble#" | "sqrtFloat#" => FloatingOperation::Unary(UnaryKind::Sqrt),
                "+##" | "-##" | "*##" | "/##" => FloatingOperation::Binary(match name.as_str() {
                    "+##" => BinaryKind::Add,
                    "-##" => BinaryKind::Sub,
                    "*##" => BinaryKind::Mul,
                    _ => BinaryKind::Div,
                }),
                "plusFloat#" | "minusFloat#" | "timesFloat#" | "divideFloat#" => {
                    FloatingOperation::Binary(match name.as_str() {
                        "plusFloat#" => BinaryKind::Add,
                        "minusFloat#" => BinaryKind::Sub,
                        "timesFloat#" => BinaryKind::Mul,
                        _ => BinaryKind::Div,
                    })
                }
                "==##" | "/=##" | "<##" | "<=##" | ">##" | ">=##" => {
                    FloatingOperation::Compare(match name.as_str() {
                        "==##" => CompareKind::Eq,
                        "/=##" => CompareKind::Ne,
                        "<##" => CompareKind::Lt,
                        "<=##" => CompareKind::Le,
                        ">##" => CompareKind::Gt,
                        _ => CompareKind::Ge,
                    })
                }
                "eqFloat#" | "neFloat#" | "ltFloat#" | "leFloat#" | "gtFloat#" | "geFloat#" => {
                    FloatingOperation::Compare(match name.as_str() {
                        "eqFloat#" => CompareKind::Eq,
                        "neFloat#" => CompareKind::Ne,
                        "ltFloat#" => CompareKind::Lt,
                        "leFloat#" => CompareKind::Le,
                        "gtFloat#" => CompareKind::Gt,
                        _ => CompareKind::Ge,
                    })
                }
                "int2Double#" => FloatingOperation::FromInteger { signed: true, width: 64 },
                "word2Double#" => FloatingOperation::FromInteger { signed: false, width: 64 },
                "int2Float#" => FloatingOperation::FromInteger { signed: true, width: 32 },
                "word2Float#" => FloatingOperation::FromInteger { signed: false, width: 32 },
                "float2Double#" => FloatingOperation::Convert { from_float: true },
                "double2Float#" => FloatingOperation::Convert { from_float: false },
                _ => return None,
            };
            let width = if name.contains("Float") || name.contains("float") {
                32
            } else {
                64
            };
            let valid = match operation {
                FloatingOperation::Negate | FloatingOperation::Unary(_) => {
                    signature.arguments == [RuntimeRep::Float(width)]
                        && returns_exact(signature, &[RuntimeRep::Float(width)])
                }
                FloatingOperation::Compare(_) => {
                    signature.arguments == [RuntimeRep::Float(width), RuntimeRep::Float(width)]
                        && returns_exact(signature, &[RuntimeRep::Int(64)])
                }
                FloatingOperation::FromInteger { signed, width } => {
                    signature.arguments
                        == [if signed {
                            RuntimeRep::Int(64)
                        } else {
                            RuntimeRep::Word(64)
                        }]
                        && returns_exact(signature, &[RuntimeRep::Float(width)])
                }
                FloatingOperation::Convert { from_float } => {
                    signature.arguments == [RuntimeRep::Float(if from_float { 32 } else { 64 })]
                        && returns_exact(
                            signature,
                            &[RuntimeRep::Float(if from_float { 64 } else { 32 })],
                        )
                }
                _ => {
                    signature.arguments == [RuntimeRep::Float(width); 2]
                        && returns_exact(signature, &[RuntimeRep::Float(width)])
                }
            };
            return valid.then_some(operation);
        };
        match symbol.as_str() {
            "rintDouble"
                if (signature.arguments == [RuntimeRep::Float(64)]
                    || signature.arguments == [RuntimeRep::Float(64), RuntimeRep::Void])
                    && returns_exact(signature, &[RuntimeRep::Float(64)]) =>
            {
                Some(FloatingOperation::NearestDouble)
            }
            _ => classify_symbol(symbol).and_then(|(width, kind)| {
                (signature.arguments == [RuntimeRep::Float(width), RuntimeRep::Void]
                    && returns_exact(signature, &[RuntimeRep::Int(64)]))
                .then_some(FloatingOperation::Classify { width, kind })
            }),
        }
    }

    fn emit(
        operation: Self::Operation,
        builder: &mut FunctionBuilder<'_>,
        arguments: &[Value],
    ) -> Vec<Value> {
        match operation {
            FloatingOperation::NearestDouble => vec![builder.ins().nearest(arguments[0])],
            FloatingOperation::Negate => vec![builder.ins().fneg(arguments[0])],
            FloatingOperation::FromInteger { signed, width } => {
                let ty = if width == 32 { types::F32 } else { types::F64 };
                vec![if signed {
                    builder.ins().fcvt_from_sint(ty, arguments[0])
                } else {
                    builder.ins().fcvt_from_uint(ty, arguments[0])
                }]
            }
            FloatingOperation::Unary(UnaryKind::Abs) => vec![builder.ins().fabs(arguments[0])],
            FloatingOperation::Unary(UnaryKind::Sqrt) => vec![builder.ins().sqrt(arguments[0])],
            FloatingOperation::Binary(kind) => {
                let value = match kind {
                    BinaryKind::Add => builder.ins().fadd(arguments[0], arguments[1]),
                    BinaryKind::Sub => builder.ins().fsub(arguments[0], arguments[1]),
                    BinaryKind::Mul => builder.ins().fmul(arguments[0], arguments[1]),
                    BinaryKind::Div => builder.ins().fdiv(arguments[0], arguments[1]),
                };
                vec![value]
            }
            FloatingOperation::Compare(kind) => {
                let cc = match kind {
                    CompareKind::Eq => FloatCC::Equal,
                    CompareKind::Ne => FloatCC::NotEqual,
                    CompareKind::Lt => FloatCC::LessThan,
                    CompareKind::Le => FloatCC::LessThanOrEqual,
                    CompareKind::Gt => FloatCC::GreaterThan,
                    CompareKind::Ge => FloatCC::GreaterThanOrEqual,
                };
                let cmp = builder.ins().fcmp(cc, arguments[0], arguments[1]);
                vec![builder.ins().uextend(types::I64, cmp)]
            }
            FloatingOperation::Convert { from_float } => {
                let value = if from_float {
                    builder.ins().fpromote(types::F64, arguments[0])
                } else {
                    builder.ins().fdemote(types::F32, arguments[0])
                };
                vec![value]
            }
            FloatingOperation::Classify { width, kind } => {
                let result = if width == 32 {
                    let bits = builder
                        .ins()
                        .bitcast(types::I32, MemFlags::new(), arguments[0]);
                    let magnitude = builder.ins().band_imm(bits, 0x7fff_ffff);
                    match kind {
                        ClassificationKind::NaN => builder.ins().icmp_imm(
                            IntCC::UnsignedGreaterThan,
                            magnitude,
                            0x7f80_0000,
                        ),
                        ClassificationKind::Infinite => {
                            builder.ins().icmp_imm(IntCC::Equal, magnitude, 0x7f80_0000)
                        }
                        ClassificationKind::NegativeZero => {
                            builder.ins().icmp_imm(IntCC::Equal, bits, 0x8000_0000)
                        }
                        ClassificationKind::Denormalized => {
                            // magnitude - 1 wraps for zero, so one unsigned
                            // compare selects 0 < magnitude < smallest normal.
                            let below = builder.ins().iadd_imm(magnitude, -1);
                            builder.ins().icmp_imm(IntCC::UnsignedLessThan, below, 0x007f_ffff)
                        }
                        ClassificationKind::Finite => builder.ins().icmp_imm(
                            IntCC::UnsignedLessThan,
                            magnitude,
                            0x7f80_0000,
                        ),
                    }
                } else {
                    let bits = builder
                        .ins()
                        .bitcast(types::I64, MemFlags::new(), arguments[0]);
                    let magnitude = builder.ins().band_imm(bits, 0x7fff_ffff_ffff_ffff);
                    match kind {
                        ClassificationKind::NaN => builder.ins().icmp_imm(
                            IntCC::UnsignedGreaterThan,
                            magnitude,
                            0x7ff0_0000_0000_0000,
                        ),
                        ClassificationKind::Infinite => {
                            builder
                                .ins()
                                .icmp_imm(IntCC::Equal, magnitude, 0x7ff0_0000_0000_0000)
                        }
                        ClassificationKind::NegativeZero => {
                            builder.ins().icmp_imm(IntCC::Equal, bits, i64::MIN)
                        }
                        ClassificationKind::Denormalized => {
                            let below = builder.ins().iadd_imm(magnitude, -1);
                            builder.ins().icmp_imm(
                                IntCC::UnsignedLessThan,
                                below,
                                0x000f_ffff_ffff_ffff,
                            )
                        }
                        ClassificationKind::Finite => builder.ins().icmp_imm(
                            IntCC::UnsignedLessThan,
                            magnitude,
                            0x7ff0_0000_0000_0000,
                        ),
                    }
                };
                vec![builder.ins().uextend(types::I64, result)]
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{atomic::AtomicBool, Arc};
    use tidepool_repr::execution_schema::{testing, *};

    fn run(
        identity: &str,
        arguments: Vec<RuntimeRep>,
        results: Vec<RuntimeRep>,
        atoms: Vec<Atom>,
    ) -> Vec<tidepool_bridge::Value> {
        run_identity(
            OperationIdentity::PrimOp(identity.into()),
            arguments,
            results,
            atoms,
        )
    }

    fn run_identity(
        identity: OperationIdentity,
        arguments: Vec<RuntimeRep>,
        results: Vec<RuntimeRep>,
        atoms: Vec<Atom>,
    ) -> Vec<tidepool_bridge::Value> {
        let mut wire = testing::wire_program();
        wire.signatures[0].results = ResultContract::Returns(results.clone());
        wire.signatures.push(Signature {
            arguments,
            results: ResultContract::Returns(results),
        });
        wire.operations.push(OperationDecl {
            identity,
            signature: SignatureId(1),
        });
        wire.expressions.nodes[0] = ExprFrame::Operation {
            operation: OperationId(0),
            arguments: atoms,
        };
        let linked =
            link_program(testing::prepare(wire).unwrap(), &MachineImports::default()).unwrap();
        super::super::CompiledProgram::compile(&linked)
            .unwrap()
            .run_entry(
                ValueId(0),
                &[],
                &super::super::RunOptions::default(),
                Arc::new(AtomicBool::new(false)),
            )
            .unwrap()
            .values
    }

    fn float(bits: u8, value: u64) -> Atom {
        let bytes = if bits == 32 {
            (value as u32).to_be_bytes().to_vec()
        } else {
            value.to_be_bytes().to_vec()
        };
        Atom::Scalar(ScalarLiteral::Float { bits, bytes })
    }

    #[test]
    fn w5_a3_rint_double_exact_identity_rounds_ties_even() {
        // GHC's foreign call carries a final State# token even for this pure
        // intrinsic. Preserve its logical position; only physical ABI erases it.
        for state_token in [false, true] {
            for (input, expected) in [(0.5_f64, 0.0_f64), (1.5, 2.0), (-0.5, -0.0), (-1.5, -2.0)] {
                let mut wire = testing::wire_program();
                wire.signatures[0].results = ResultContract::Returns(vec![RuntimeRep::Float(64)]);
                wire.signatures.push(Signature {
                    arguments: vec![RuntimeRep::Float(64)],
                    results: ResultContract::Returns(vec![RuntimeRep::Float(64)]),
                });
                if state_token {
                    wire.signatures[1].arguments.push(RuntimeRep::Void);
                }
                wire.operations.push(OperationDecl {
                    identity: OperationIdentity::Intrinsic {
                        symbol: "rintDouble".into(),
                        convention: ForeignConvention::CCall,
                    },
                    signature: SignatureId(1),
                });
                wire.expressions.nodes[0] = ExprFrame::Operation {
                    operation: OperationId(0),
                    arguments: vec![Atom::Scalar(ScalarLiteral::Float {
                        bits: 64,
                        bytes: input.to_bits().to_be_bytes().to_vec(),
                    })],
                };
                if state_token {
                    let ExprFrame::Operation { arguments, .. } = &mut wire.expressions.nodes[0]
                    else {
                        unreachable!()
                    };
                    arguments.push(Atom::Void);
                }
                let linked =
                    link_program(testing::prepare(wire).unwrap(), &MachineImports::default())
                        .unwrap();
                let compiled = super::super::CompiledProgram::compile(&linked).unwrap();
                let result = compiled
                    .run_entry(
                        ValueId(0),
                        &[],
                        &super::super::RunOptions::default(),
                        Arc::new(AtomicBool::new(false)),
                    )
                    .unwrap();
                assert!(matches!(result.values.as_slice(),
                [tidepool_bridge::Value::Lit(tidepool_repr::Literal::LitDouble(bits))]
                if *bits == expected.to_bits()));
            }
        }
    }

    #[test]
    fn ghc_float_families_preserve_ieee_values_and_convert() {
        let values = run(
            "plusFloat#",
            vec![RuntimeRep::Float(32), RuntimeRep::Float(32)],
            vec![RuntimeRep::Float(32)],
            vec![
                float(32, f32::from_bits(0x8000_0000).to_bits() as u64),
                float(32, 0),
            ],
        );
        assert!(
            matches!(values.as_slice(), [tidepool_bridge::Value::Lit(tidepool_repr::Literal::LitFloat(bits))] if *bits == 0)
        );
        let values = run(
            "eqFloat#",
            vec![RuntimeRep::Float(32), RuntimeRep::Float(32)],
            vec![RuntimeRep::Int(64)],
            vec![float(32, 0x7fc0_0001), float(32, 0x7fc0_0001)],
        );
        assert!(
            matches!(values.as_slice(), [tidepool_bridge::Value::Lit(tidepool_repr::Literal::LitInt(value))] if *value == 0)
        );
        let values = run(
            "float2Double#",
            vec![RuntimeRep::Float(32)],
            vec![RuntimeRep::Float(64)],
            vec![float(32, 0x8000_0000)],
        );
        assert!(
            matches!(values.as_slice(), [tidepool_bridge::Value::Lit(tidepool_repr::Literal::LitDouble(bits))] if *bits == (-0.0f64).to_bits())
        );
    }

    #[test]
    fn ghc_negate_uses_native_fneg_and_preserves_signed_zero() {
        for (input, expected) in [(0.0_f64, -0.0_f64), (-0.0, 0.0), (1.5, -1.5)] {
            let values = run(
                "negateDouble#",
                vec![RuntimeRep::Float(64)],
                vec![RuntimeRep::Float(64)],
                vec![float(64, input.to_bits())],
            );
            assert!(matches!(values.as_slice(),
                [tidepool_bridge::Value::Lit(tidepool_repr::Literal::LitDouble(bits))]
                    if *bits == expected.to_bits()));
        }
        for (input, expected) in [(0.0_f32, -0.0_f32), (-0.0, 0.0), (1.5, -1.5)] {
            let values = run(
                "negateFloat#",
                vec![RuntimeRep::Float(32)],
                vec![RuntimeRep::Float(32)],
                vec![float(32, u64::from(input.to_bits()))],
            );
            assert!(matches!(values.as_slice(),
                [tidepool_bridge::Value::Lit(tidepool_repr::Literal::LitFloat(bits))]
                    if *bits == u64::from(expected.to_bits())));
        }
    }

    #[test]
    fn integer_to_float_conversions_respect_signedness() {
        let values = run(
            "word2Double#",
            vec![RuntimeRep::Word(64)],
            vec![RuntimeRep::Float(64)],
            vec![Atom::Scalar(ScalarLiteral::Word {
                bits: 64,
                bytes: u64::MAX.to_be_bytes().to_vec(),
            })],
        );
        assert!(matches!(values.as_slice(),
            [tidepool_bridge::Value::Lit(tidepool_repr::Literal::LitDouble(bits))]
                if *bits == (u64::MAX as f64).to_bits()));
        let values = run(
            "int2Double#",
            vec![RuntimeRep::Int(64)],
            vec![RuntimeRep::Float(64)],
            vec![Atom::Scalar(ScalarLiteral::Int {
                bits: 64,
                bytes: (-3_i64).to_be_bytes().to_vec(),
            })],
        );
        assert!(matches!(values.as_slice(),
            [tidepool_bridge::Value::Lit(tidepool_repr::Literal::LitDouble(bits))]
                if *bits == (-3.0_f64).to_bits()));
    }

    #[test]
    fn transcendental_primops_call_the_host_libm() {
        for (name, arguments, expected) in [
            ("logDouble#", vec![std::f64::consts::E], 1.0_f64),
            ("expDouble#", vec![0.0], 1.0),
            ("**##", vec![2.0, 10.0], 1024.0),
            ("atanDouble#", vec![0.0], 0.0),
        ] {
            let reps = vec![RuntimeRep::Float(64); arguments.len()];
            let atoms = arguments.iter().map(|x| float(64, x.to_bits())).collect();
            let values = run(name, reps, vec![RuntimeRep::Float(64)], atoms);
            assert!(
                matches!(values.as_slice(),
                    [tidepool_bridge::Value::Lit(tidepool_repr::Literal::LitDouble(bits))]
                        if (f64::from_bits(*bits) - expected).abs() < 1e-12),
                "{name}: {values:?}"
            );
        }
        let values = run(
            "sqrtFloat#",
            vec![RuntimeRep::Float(32)],
            vec![RuntimeRep::Float(32)],
            vec![float(32, u64::from(4.0_f32.to_bits()))],
        );
        assert!(matches!(values.as_slice(),
            [tidepool_bridge::Value::Lit(tidepool_repr::Literal::LitFloat(bits))]
                if *bits == u64::from(2.0_f32.to_bits())));
        let values = run(
            "logFloat#",
            vec![RuntimeRep::Float(32)],
            vec![RuntimeRep::Float(32)],
            vec![float(32, u64::from(1.0_f32.to_bits()))],
        );
        assert!(matches!(values.as_slice(),
            [tidepool_bridge::Value::Lit(tidepool_repr::Literal::LitFloat(bits))]
                if *bits == u64::from(0.0_f32.to_bits())));
    }

    #[test]
    fn ghc_fabs_and_sqrt_lower_natively() {
        for (name, input, expected) in [
            ("fabsDouble#", -2.5_f64, 2.5_f64),
            ("fabsDouble#", -0.0, 0.0),
            ("sqrtDouble#", 6.25, 2.5),
        ] {
            let values = run(
                name,
                vec![RuntimeRep::Float(64)],
                vec![RuntimeRep::Float(64)],
                vec![float(64, input.to_bits())],
            );
            assert!(
                matches!(values.as_slice(),
                    [tidepool_bridge::Value::Lit(tidepool_repr::Literal::LitDouble(bits))]
                        if *bits == expected.to_bits()),
                "{name}"
            );
        }
        let values = run(
            "fabsFloat#",
            vec![RuntimeRep::Float(32)],
            vec![RuntimeRep::Float(32)],
            vec![float(32, u64::from((-1.5_f32).to_bits()))],
        );
        assert!(matches!(values.as_slice(),
            [tidepool_bridge::Value::Lit(tidepool_repr::Literal::LitFloat(bits))]
                if *bits == u64::from(1.5_f32.to_bits())));
    }

    #[test]
    fn ghc_float_family_rejects_wrong_signatures() {
        assert!(FloatingFamily::recognize(
            &OperationIdentity::PrimOp("plusFloat#".into()),
            &Signature {
                arguments: vec![RuntimeRep::Float(64), RuntimeRep::Float(64)],
                results: ResultContract::Returns(vec![RuntimeRep::Float(64)])
            }
        )
        .is_none());
        assert!(FloatingFamily::recognize(
            &OperationIdentity::PrimOp("eqFloat#".into()),
            &Signature {
                arguments: vec![RuntimeRep::Float(32), RuntimeRep::Float(32)],
                results: ResultContract::Returns(vec![RuntimeRep::Float(32)])
            }
        )
        .is_none());
        for (name, arguments, results) in [
            (
                "negateDouble#",
                vec![RuntimeRep::Float(32)],
                vec![RuntimeRep::Float(32)],
            ),
            (
                "negateDouble#",
                vec![RuntimeRep::Float(64); 2],
                vec![RuntimeRep::Float(64)],
            ),
            (
                "negateFloat#",
                vec![RuntimeRep::Float(64)],
                vec![RuntimeRep::Float(64)],
            ),
            (
                "negateFloat#",
                vec![RuntimeRep::Float(32)],
                vec![RuntimeRep::Float(64)],
            ),
        ] {
            assert!(FloatingFamily::recognize(
                &OperationIdentity::PrimOp(name.into()),
                &Signature {
                    arguments,
                    results: ResultContract::Returns(results),
                }
            )
            .is_none());
        }
    }

    #[test]
    fn ghc_internal_float_classifiers_require_exact_catalogued_abis() {
        for (name, width) in [
            ("isFloatNaN", 32),
            ("isFloatInfinite", 32),
            ("isFloatNegativeZero", 32),
            ("isDoubleNaN", 64),
            ("isDoubleInfinite", 64),
            ("isDoubleNegativeZero", 64),
            ("isFloatDenormalized", 32),
            ("isFloatFinite", 32),
            ("isDoubleDenormalized", 64),
            ("isDoubleFinite", 64),
        ] {
            let identity = OperationIdentity::Intrinsic {
                symbol: name.into(),
                convention: ForeignConvention::CCall,
            };
            let exact = Signature {
                arguments: vec![RuntimeRep::Float(width), RuntimeRep::Void],
                results: ResultContract::Returns(vec![RuntimeRep::Int(64)]),
            };
            assert!(
                FloatingFamily::recognize(&identity, &exact).is_some(),
                "{name}"
            );
            assert!(FloatingFamily::recognize(
                &identity,
                &Signature {
                    arguments: vec![RuntimeRep::Float(width)],
                    ..exact.clone()
                }
            )
            .is_none());
            assert!(FloatingFamily::recognize(
                &identity,
                &Signature {
                    arguments: vec![
                        RuntimeRep::Float(if width == 32 { 64 } else { 32 }),
                        RuntimeRep::Void,
                    ],
                    ..exact.clone()
                }
            )
            .is_none());
            assert!(FloatingFamily::recognize(
                &identity,
                &Signature {
                    results: ResultContract::Returns(vec![RuntimeRep::Int(32)]),
                    ..exact.clone()
                }
            )
            .is_none());
            assert!(FloatingFamily::recognize(
                &OperationIdentity::Intrinsic {
                    symbol: format!("{name}Suffix"),
                    convention: ForeignConvention::CCall,
                },
                &exact,
            )
            .is_none());
        }
    }

    #[test]
    fn ghc_internal_float_classifiers_lower_ieee_bit_patterns() {
        for (name, width, bits, expected) in [
            ("isFloatNaN", 32, 0x7fc0_0001, 1),
            ("isFloatInfinite", 32, 0x7f80_0000, 1),
            ("isFloatNegativeZero", 32, 0x8000_0000, 1),
            ("isDoubleNaN", 64, 0x7ff8_0000_0000_0001, 1),
            ("isDoubleInfinite", 64, 0x7ff0_0000_0000_0000, 1),
            ("isDoubleNegativeZero", 64, 0x8000_0000_0000_0000, 1),
            ("isFloatDenormalized", 32, 0x0000_0001, 1),
            ("isFloatDenormalized", 32, 0x8000_0000, 0),
            ("isFloatDenormalized", 32, 0x0080_0000, 0),
            ("isFloatFinite", 32, 0x3f80_0000, 1),
            ("isFloatFinite", 32, 0x7f80_0000, 0),
            ("isDoubleDenormalized", 64, 0x800f_ffff_ffff_ffff, 1),
            ("isDoubleDenormalized", 64, 0x0000_0000_0000_0000, 0),
            ("isDoubleDenormalized", 64, 0x0010_0000_0000_0000, 0),
            ("isDoubleFinite", 64, 0x3ff0_0000_0000_0000, 1),
            ("isDoubleFinite", 64, 0x7ff8_0000_0000_0000, 0),
        ] {
            let values = run_identity(
                OperationIdentity::Intrinsic {
                    symbol: name.into(),
                    convention: ForeignConvention::CCall,
                },
                vec![RuntimeRep::Float(width), RuntimeRep::Void],
                vec![RuntimeRep::Int(64)],
                vec![float(width, bits), Atom::Void],
            );
            assert!(
                matches!(
                    values.as_slice(),
                    [tidepool_bridge::Value::Lit(tidepool_repr::Literal::LitInt(value))]
                        if *value == expected
                ),
                "{name} returned {values:?}"
            );
        }
    }

    #[test]
    fn decode_double_int64_real_adapter_matches_pinned_ieee_results() {
        for (bits, expected) in [
            (0x3ff0_0000_0000_0000, (1_i64 << 52, -52)),
            (0x0000_0000_0000_0001, (1_i64 << 52, -1126)),
            (0x7ff0_0000_0000_0000, (1_i64 << 52, 972)),
            (0x7ff8_0000_0000_0001, (0x0018_0000_0000_0001, 972)),
            (0xfff8_0000_0000_0abc, (-0x0018_0000_0000_0abc, 972)),
        ] {
            let values = run(
                "decodeDouble_Int64#",
                vec![RuntimeRep::Float(64)],
                vec![RuntimeRep::Int(64), RuntimeRep::Int(64)],
                vec![float(64, bits)],
            );
            assert!(matches!(values.as_slice(),
                [tidepool_bridge::Value::Lit(tidepool_repr::Literal::LitInt(mantissa)),
                 tidepool_bridge::Value::Lit(tidepool_repr::Literal::LitInt(exponent))]
                    if (*mantissa, *exponent) == expected));
        }
    }

    #[test]
    fn decode_double_int64_requires_exact_identity_and_signature() {
        let valid = Signature {
            arguments: vec![RuntimeRep::Float(64)],
            results: ResultContract::Returns(vec![RuntimeRep::Int(64), RuntimeRep::Int(64)]),
        };
        assert!(recognize_decode_double_int64(
            &OperationIdentity::PrimOp("decodeDouble_Int64#".into()),
            &valid,
        ));
        for signature in [
            Signature {
                arguments: vec![RuntimeRep::Float(32)],
                ..valid.clone()
            },
            Signature {
                results: ResultContract::Returns(vec![RuntimeRep::Int(64)]),
                ..valid.clone()
            },
            Signature {
                results: ResultContract::NoSuccess,
                ..valid.clone()
            },
        ] {
            assert!(!recognize_decode_double_int64(
                &OperationIdentity::PrimOp("decodeDouble_Int64#".into()),
                &signature,
            ));
        }
        assert!(!recognize_decode_double_int64(
            &OperationIdentity::PrimOp("decodeDouble_Int64#lookalike".into()),
            &valid,
        ));
    }
}
