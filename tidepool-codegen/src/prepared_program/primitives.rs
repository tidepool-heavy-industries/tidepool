//! Representation-checked, non-allocating primitive operations.
//!
//! Names are the authoritative spellings emitted by `Tidepool.PrimOps` from
//! GHC's `PrimOp` table. An operation is admitted only after its complete wire
//! signature has been checked; emitters never infer a heap layout from bits.

use crate::pipeline::CodegenPipeline;
use cranelift_codegen::{ir, ir::InstBuilder};
use cranelift_frontend::FunctionBuilder;
use std::sync::Arc;
use tidepool_repr::execution_schema::{OperationDecl, OperationIdentity, RuntimeRep, Signature};

/// Pure scalar families cannot allocate, call hosts, or inspect managed data.
pub(super) trait ScalarFamily {
    type Operation: Copy;

    fn recognize(identity: &OperationIdentity, signature: &Signature) -> Option<Self::Operation>;
    fn emit(
        operation: Self::Operation,
        builder: &mut FunctionBuilder<'_>,
        arguments: &[ir::Value],
    ) -> Vec<ir::Value>;
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum IntegerKind {
    Add,
    Sub,
    Mul,
    Quot,
    Rem,
    QuotRem,
    Negate,
    And,
    Or,
    Xor,
    Not,
    Shl,
    Shra,
    Shrl,
    Compare(ir::condcodes::IntCC),
    Convert,
    Narrow,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct IntegerOperation {
    pub(super) kind: IntegerKind,
    pub(super) signed: bool,
    pub(super) bits: u8,
    pub(super) result_bits: u8,
    pub(super) narrow_bits: u8,
}

pub(super) struct IntegerFamily;

fn int_rep(bits: u8) -> RuntimeRep {
    RuntimeRep::Int(bits)
}

fn word_rep(bits: u8) -> RuntimeRep {
    RuntimeRep::Word(bits)
}

fn valid_bits(bits: u8) -> bool {
    matches!(bits, 8 | 16 | 32 | 64)
}

fn binary_same(
    signature: &Signature,
    lhs: RuntimeRep,
    rhs: RuntimeRep,
    result: RuntimeRep,
) -> Option<u8> {
    let bits = match lhs {
        RuntimeRep::Int(bits) | RuntimeRep::Word(bits) if valid_bits(bits) => bits,
        _ => return None,
    };
    if signature.arguments.as_slice() == [lhs, rhs] && signature.results.as_slice() == [result] {
        Some(bits)
    } else {
        None
    }
}

fn unary_same(signature: &Signature, arg: RuntimeRep, result: RuntimeRep) -> Option<u8> {
    let bits = match arg {
        RuntimeRep::Int(bits) | RuntimeRep::Word(bits) if valid_bits(bits) => bits,
        _ => return None,
    };
    if signature.arguments.as_slice() == [arg] && signature.results.as_slice() == [result] {
        Some(bits)
    } else {
        None
    }
}

fn generic_binary(
    signature: &Signature,
    signed: bool,
    kind: IntegerKind,
) -> Option<IntegerOperation> {
    fixed_binary(signature, signed, 64, kind)
}

fn generic_unary(
    signature: &Signature,
    signed: bool,
    kind: IntegerKind,
) -> Option<IntegerOperation> {
    fixed_unary(signature, signed, 64, kind)
}

fn compare(
    signature: &Signature,
    signed: bool,
    bits: u8,
    cc: ir::condcodes::IntCC,
) -> Option<IntegerOperation> {
    let rep = if signed {
        int_rep(bits)
    } else {
        word_rep(bits)
    };
    let result_bits = match signature.results.as_slice() {
        [RuntimeRep::Int(64)] => 64,
        _ => return None,
    };
    if signature.arguments.as_slice() != [rep, rep] {
        return None;
    }
    Some(IntegerOperation {
        kind: IntegerKind::Compare(cc),
        signed,
        bits,
        result_bits,
        narrow_bits: 0,
    })
}

fn generic_compare(
    signature: &Signature,
    signed: bool,
    cc: ir::condcodes::IntCC,
) -> Option<IntegerOperation> {
    compare(signature, signed, 64, cc)
}

fn generic_shift(
    signature: &Signature,
    signed: bool,
    kind: IntegerKind,
) -> Option<IntegerOperation> {
    let lhs = if signed { int_rep(64) } else { word_rep(64) };
    (signature.arguments.as_slice() == [lhs, int_rep(64)] && signature.results.as_slice() == [lhs])
        .then_some(IntegerOperation {
            kind,
            signed,
            bits: 64,
            result_bits: 64,
            narrow_bits: 0,
        })
}

fn fixed_binary(
    signature: &Signature,
    signed: bool,
    bits: u8,
    kind: IntegerKind,
) -> Option<IntegerOperation> {
    let rep = if signed {
        int_rep(bits)
    } else {
        word_rep(bits)
    };
    binary_same(signature, rep, rep, rep).map(|_| IntegerOperation {
        kind,
        signed,
        bits,
        result_bits: bits,
        narrow_bits: 0,
    })
}

fn fixed_quot_rem(signature: &Signature, signed: bool, bits: u8) -> Option<IntegerOperation> {
    let rep = if signed {
        int_rep(bits)
    } else {
        word_rep(bits)
    };
    (signature.arguments.as_slice() == [rep, rep] && signature.results.as_slice() == [rep, rep])
        .then_some(IntegerOperation {
            kind: IntegerKind::QuotRem,
            signed,
            bits,
            result_bits: bits,
            narrow_bits: 0,
        })
}

fn fixed_unary(
    signature: &Signature,
    signed: bool,
    bits: u8,
    kind: IntegerKind,
) -> Option<IntegerOperation> {
    let rep = if signed {
        int_rep(bits)
    } else {
        word_rep(bits)
    };
    unary_same(signature, rep, rep).map(|_| IntegerOperation {
        kind,
        signed,
        bits,
        result_bits: bits,
        narrow_bits: 0,
    })
}

fn fixed_shift(
    signature: &Signature,
    signed: bool,
    bits: u8,
    kind: IntegerKind,
) -> Option<IntegerOperation> {
    let value = if signed {
        int_rep(bits)
    } else {
        word_rep(bits)
    };
    (signature.arguments.as_slice() == [value, int_rep(64)]
        && signature.results.as_slice() == [value])
    .then_some(IntegerOperation {
        kind,
        signed,
        bits,
        result_bits: bits,
        narrow_bits: 0,
    })
}

fn convert(signature: &Signature, from_signed: bool, to_signed: bool) -> Option<IntegerOperation> {
    let source = match (from_signed, signature.arguments.as_slice()) {
        (true, [RuntimeRep::Int(64)]) => 64,
        (false, [RuntimeRep::Word(64)]) => 64,
        _ => return None,
    };
    let result = match (to_signed, signature.results.as_slice()) {
        (true, [RuntimeRep::Int(64)]) => 64,
        (false, [RuntimeRep::Word(64)]) => 64,
        _ => return None,
    };
    (source == result).then_some(IntegerOperation {
        kind: IntegerKind::Convert,
        signed: from_signed,
        bits: source,
        result_bits: result,
        narrow_bits: 0,
    })
}

fn convert_between(
    signature: &Signature,
    from_signed: bool,
    from_bits: u8,
    to_signed: bool,
    to_bits: u8,
) -> Option<IntegerOperation> {
    let source = if from_signed {
        int_rep(from_bits)
    } else {
        word_rep(from_bits)
    };
    let result = if to_signed {
        int_rep(to_bits)
    } else {
        word_rep(to_bits)
    };
    (signature.arguments.as_slice() == [source] && signature.results.as_slice() == [result])
        .then_some(IntegerOperation {
            kind: IntegerKind::Convert,
            signed: from_signed,
            bits: from_bits,
            result_bits: to_bits,
            narrow_bits: 0,
        })
}

fn narrow(signature: &Signature, signed: bool, result_bits: u8) -> Option<IntegerOperation> {
    let source = match (signed, signature.arguments.as_slice()) {
        (true, [RuntimeRep::Int(64)]) => 64,
        (false, [RuntimeRep::Word(64)]) => 64,
        _ => return None,
    };
    let result = if signed { int_rep(64) } else { word_rep(64) };
    (signature.results.as_slice() == [result]).then_some(IntegerOperation {
        kind: IntegerKind::Narrow,
        signed,
        bits: source,
        result_bits: source,
        narrow_bits: result_bits,
    })
}

fn operation_for_name(name: &str, signature: &Signature) -> Option<IntegerOperation> {
    use ir::condcodes::IntCC;

    match name {
        "+#" => generic_binary(signature, true, IntegerKind::Add),
        "-#" => generic_binary(signature, true, IntegerKind::Sub),
        "*#" => generic_binary(signature, true, IntegerKind::Mul),
        "quotInt#" => fixed_binary(signature, true, 64, IntegerKind::Quot),
        "remInt#" => fixed_binary(signature, true, 64, IntegerKind::Rem),
        "quotRemInt#" => fixed_quot_rem(signature, true, 64),
        "quotInt8#" => fixed_binary(signature, true, 8, IntegerKind::Quot),
        "remInt8#" => fixed_binary(signature, true, 8, IntegerKind::Rem),
        "quotRemInt8#" => fixed_quot_rem(signature, true, 8),
        "quotInt16#" => fixed_binary(signature, true, 16, IntegerKind::Quot),
        "remInt16#" => fixed_binary(signature, true, 16, IntegerKind::Rem),
        "quotRemInt16#" => fixed_quot_rem(signature, true, 16),
        "quotInt32#" => fixed_binary(signature, true, 32, IntegerKind::Quot),
        "remInt32#" => fixed_binary(signature, true, 32, IntegerKind::Rem),
        "quotRemInt32#" => fixed_quot_rem(signature, true, 32),
        "negateInt#" => generic_unary(signature, true, IntegerKind::Negate),
        "andI#" => generic_binary(signature, true, IntegerKind::And),
        "orI#" => generic_binary(signature, true, IntegerKind::Or),
        "xorI#" => generic_binary(signature, true, IntegerKind::Xor),
        "notI#" => generic_unary(signature, true, IntegerKind::Not),
        "uncheckedIShiftL#" => generic_shift(signature, true, IntegerKind::Shl),
        "uncheckedIShiftRA#" => generic_shift(signature, true, IntegerKind::Shra),
        "uncheckedIShiftRL#" => generic_shift(signature, true, IntegerKind::Shrl),
        "plusWord#" => generic_binary(signature, false, IntegerKind::Add),
        "minusWord#" => generic_binary(signature, false, IntegerKind::Sub),
        "timesWord#" => generic_binary(signature, false, IntegerKind::Mul),
        "quotWord#" => fixed_binary(signature, false, 64, IntegerKind::Quot),
        "remWord#" => fixed_binary(signature, false, 64, IntegerKind::Rem),
        "quotRemWord#" => fixed_quot_rem(signature, false, 64),
        "and#" => generic_binary(signature, false, IntegerKind::And),
        "or#" => generic_binary(signature, false, IntegerKind::Or),
        "xor#" => generic_binary(signature, false, IntegerKind::Xor),
        "not#" => generic_unary(signature, false, IntegerKind::Not),
        "uncheckedShiftL#" => generic_shift(signature, false, IntegerKind::Shl),
        "uncheckedShiftRL#" => generic_shift(signature, false, IntegerKind::Shrl),
        "==#" => generic_compare(signature, true, IntCC::Equal),
        "/=#" => generic_compare(signature, true, IntCC::NotEqual),
        "<#" => generic_compare(signature, true, IntCC::SignedLessThan),
        "<=#" => generic_compare(signature, true, IntCC::SignedLessThanOrEqual),
        ">#" => generic_compare(signature, true, IntCC::SignedGreaterThan),
        ">=#" => generic_compare(signature, true, IntCC::SignedGreaterThanOrEqual),
        "eqWord#" => generic_compare(signature, false, IntCC::Equal),
        "neWord#" => generic_compare(signature, false, IntCC::NotEqual),
        "ltWord#" => generic_compare(signature, false, IntCC::UnsignedLessThan),
        "leWord#" => generic_compare(signature, false, IntCC::UnsignedLessThanOrEqual),
        "gtWord#" => generic_compare(signature, false, IntCC::UnsignedGreaterThan),
        "geWord#" => generic_compare(signature, false, IntCC::UnsignedGreaterThanOrEqual),
        "int2Word#" => convert(signature, true, false),
        "word2Int#" => convert(signature, false, true),
        "int8ToInt#" => convert_between(signature, true, 8, true, 64),
        "int8ToWord8#" => convert_between(signature, true, 8, false, 8),
        "word8ToInt8#" => convert_between(signature, false, 8, true, 8),
        "int32ToInt#" => convert_between(signature, true, 32, true, 64),
        "int64ToInt#" => convert_between(signature, true, 64, true, 64),
        "intToInt64#" => convert_between(signature, true, 64, true, 64),
        "int64ToWord64#" => convert_between(signature, true, 64, false, 64),
        "word64ToInt64#" => convert_between(signature, false, 64, true, 64),
        "word64ToWord#" => convert_between(signature, false, 64, false, 64),
        "wordToWord64#" => convert_between(signature, false, 64, false, 64),
        "word8ToWord#" => convert_between(signature, false, 8, false, 64),
        "wordToWord8#" => convert_between(signature, false, 64, false, 8),
        "word32ToWord#" => convert_between(signature, false, 32, false, 64),
        "wordToWord32#" => convert_between(signature, false, 64, false, 32),
        "narrow8Int#" => narrow(signature, true, 8),
        "narrow16Int#" => narrow(signature, true, 16),
        "narrow32Int#" => narrow(signature, true, 32),
        "narrow8Word#" => narrow(signature, false, 8),
        "narrow16Word#" => narrow(signature, false, 16),
        "narrow32Word#" => narrow(signature, false, 32),
        // Fixed-width names are emitted by GHC's native Word8/Word32/Int64
        // primops. Their signatures remain explicit here, not inferred by a
        // display-name alias.
        "plusWord8#" => fixed_binary(signature, false, 8, IntegerKind::Add),
        "subWord8#" => fixed_binary(signature, false, 8, IntegerKind::Sub),
        "timesWord8#" => fixed_binary(signature, false, 8, IntegerKind::Mul),
        "quotWord8#" => fixed_binary(signature, false, 8, IntegerKind::Quot),
        "remWord8#" => fixed_binary(signature, false, 8, IntegerKind::Rem),
        "quotRemWord8#" => fixed_quot_rem(signature, false, 8),
        "quotWord16#" => fixed_binary(signature, false, 16, IntegerKind::Quot),
        "remWord16#" => fixed_binary(signature, false, 16, IntegerKind::Rem),
        "quotRemWord16#" => fixed_quot_rem(signature, false, 16),
        "quotWord32#" => fixed_binary(signature, false, 32, IntegerKind::Quot),
        "remWord32#" => fixed_binary(signature, false, 32, IntegerKind::Rem),
        "quotRemWord32#" => fixed_quot_rem(signature, false, 32),
        "ltWord8#" => compare(signature, false, 8, IntCC::UnsignedLessThan),
        "leWord8#" => compare(signature, false, 8, IntCC::UnsignedLessThanOrEqual),
        "gtWord8#" => compare(signature, false, 8, IntCC::UnsignedGreaterThan),
        "geWord8#" => compare(signature, false, 8, IntCC::UnsignedGreaterThanOrEqual),
        "plusWord64#" => fixed_binary(signature, false, 64, IntegerKind::Add),
        "subWord64#" => fixed_binary(signature, false, 64, IntegerKind::Sub),
        "timesWord64#" => fixed_binary(signature, false, 64, IntegerKind::Mul),
        "quotWord64#" => fixed_binary(signature, false, 64, IntegerKind::Quot),
        "remWord64#" => fixed_binary(signature, false, 64, IntegerKind::Rem),
        "quotRemWord64#" => fixed_quot_rem(signature, false, 64),
        "and64#" => fixed_binary(signature, false, 64, IntegerKind::And),
        "or64#" => fixed_binary(signature, false, 64, IntegerKind::Or),
        "xor64#" => fixed_binary(signature, false, 64, IntegerKind::Xor),
        "not64#" => fixed_unary(signature, false, 64, IntegerKind::Not),
        "uncheckedShiftL64#" => fixed_shift(signature, false, 64, IntegerKind::Shl),
        "uncheckedShiftRL64#" => fixed_shift(signature, false, 64, IntegerKind::Shrl),
        "plusInt64#" => fixed_binary(signature, true, 64, IntegerKind::Add),
        "subInt64#" => fixed_binary(signature, true, 64, IntegerKind::Sub),
        "timesInt64#" => fixed_binary(signature, true, 64, IntegerKind::Mul),
        "quotInt64#" => fixed_binary(signature, true, 64, IntegerKind::Quot),
        "remInt64#" => fixed_binary(signature, true, 64, IntegerKind::Rem),
        "quotRemInt64#" => fixed_quot_rem(signature, true, 64),
        "negateInt64#" => fixed_unary(signature, true, 64, IntegerKind::Negate),
        "uncheckedIShiftL64#" => fixed_shift(signature, true, 64, IntegerKind::Shl),
        "uncheckedIShiftRA64#" => fixed_shift(signature, true, 64, IntegerKind::Shra),
        "uncheckedIShiftRL64#" => fixed_shift(signature, true, 64, IntegerKind::Shrl),
        "eqInt64#" => compare(signature, true, 64, IntCC::Equal),
        "neInt64#" => compare(signature, true, 64, IntCC::NotEqual),
        "ltInt64#" => compare(signature, true, 64, IntCC::SignedLessThan),
        "leInt64#" => compare(signature, true, 64, IntCC::SignedLessThanOrEqual),
        "gtInt64#" => compare(signature, true, 64, IntCC::SignedGreaterThan),
        "geInt64#" => compare(signature, true, 64, IntCC::SignedGreaterThanOrEqual),
        "eqWord64#" => compare(signature, false, 64, IntCC::Equal),
        "neWord64#" => compare(signature, false, 64, IntCC::NotEqual),
        "ltWord64#" => compare(signature, false, 64, IntCC::UnsignedLessThan),
        "leWord64#" => compare(signature, false, 64, IntCC::UnsignedLessThanOrEqual),
        "gtWord64#" => compare(signature, false, 64, IntCC::UnsignedGreaterThan),
        "geWord64#" => compare(signature, false, 64, IntCC::UnsignedGreaterThanOrEqual),
        _ => None,
    }
}

/// Recognize admitted scalar identities. Primops and typed C-call intrinsics
/// use separate recognition authorities even when their spellings resemble
/// one another.
pub(super) fn recognize_operation(
    declaration: &OperationDecl,
    signature: &Signature,
) -> Option<ScalarOperation> {
    if matches!(&declaration.identity, OperationIdentity::PrimOp(name) if name == "double2Int#")
        && signature.arguments == [RuntimeRep::Float(64)]
        && signature.results == [RuntimeRep::Int(64)]
    {
        return Some(ScalarOperation::DoubleToInt);
    }
    if matches!(&declaration.identity, OperationIdentity::PrimOp(name) if name == "indexCharOffAddr#")
        && signature.arguments == [RuntimeRep::Address, RuntimeRep::Int(64)]
        && signature.results == [RuntimeRep::Word(64)]
    {
        return Some(ScalarOperation::IndexCharOffAddr);
    }
    IntegerFamily::recognize(&declaration.identity, signature)
        .map(ScalarOperation::Integer)
        .or_else(|| {
            super::floating::FloatingFamily::recognize(&declaration.identity, signature)
                .map(ScalarOperation::Floating)
        })
}

#[derive(Clone, Copy)]
pub(super) enum ScalarOperation {
    DoubleToInt,
    IndexCharOffAddr,
    Integer(IntegerOperation),
    Floating(super::floating::FloatingOperation),
}

pub(super) fn emit_operation(
    operation: ScalarOperation,
    builder: &mut FunctionBuilder<'_>,
    arguments: &[ir::Value],
    vmctx: ir::Value,
    pipeline: &mut CodegenPipeline,
    bytes: &Arc<super::static_bytes::PinnedBytes>,
) -> Result<Vec<ir::Value>, super::CompileError> {
    match operation {
        ScalarOperation::DoubleToInt => {
            super::fallible::emit_double_to_int(builder, vmctx, pipeline, arguments[0])
        }
        ScalarOperation::IndexCharOffAddr => super::static_bytes::emit_index_char(
            builder,
            pipeline,
            vmctx,
            bytes,
            arguments[0],
            arguments[1],
        ),
        ScalarOperation::Integer(operation)
            if matches!(
                operation.kind,
                IntegerKind::Quot | IntegerKind::Rem | IntegerKind::QuotRem
            ) =>
        {
            super::fallible::emit(operation, builder, vmctx, pipeline, arguments)
        }
        ScalarOperation::Integer(operation) => {
            Ok(IntegerFamily::emit(operation, builder, arguments))
        }
        ScalarOperation::Floating(operation) => Ok(super::floating::FloatingFamily::emit(
            operation, builder, arguments,
        )),
    }
}

impl ScalarFamily for IntegerFamily {
    type Operation = IntegerOperation;

    fn recognize(identity: &OperationIdentity, signature: &Signature) -> Option<Self::Operation> {
        let OperationIdentity::PrimOp(name) = identity else {
            return None;
        };
        operation_for_name(name, signature)
    }

    fn emit(
        operation: Self::Operation,
        builder: &mut FunctionBuilder<'_>,
        arguments: &[ir::Value],
    ) -> Vec<ir::Value> {
        let result_ty = integer_type(operation.result_bits);
        let value = match operation.kind {
            IntegerKind::Add => builder.ins().iadd(arguments[0], arguments[1]),
            IntegerKind::Sub => builder.ins().isub(arguments[0], arguments[1]),
            IntegerKind::Mul => builder.ins().imul(arguments[0], arguments[1]),
            IntegerKind::Quot | IntegerKind::Rem | IntegerKind::QuotRem => {
                unreachable!("fallible integer operation routed through fallible emitter")
            }
            IntegerKind::Negate => builder.ins().ineg(arguments[0]),
            IntegerKind::And => builder.ins().band(arguments[0], arguments[1]),
            IntegerKind::Or => builder.ins().bor(arguments[0], arguments[1]),
            IntegerKind::Xor => builder.ins().bxor(arguments[0], arguments[1]),
            IntegerKind::Not => builder.ins().bnot(arguments[0]),
            IntegerKind::Shl => builder.ins().ishl(arguments[0], arguments[1]),
            IntegerKind::Shra => builder.ins().sshr(arguments[0], arguments[1]),
            IntegerKind::Shrl => builder.ins().ushr(arguments[0], arguments[1]),
            IntegerKind::Compare(cc) => {
                let cmp = builder.ins().icmp(cc, arguments[0], arguments[1]);
                let one = builder.ins().iconst(result_ty, 1);
                let zero = builder.ins().iconst(result_ty, 0);
                builder.ins().select(cmp, one, zero)
            }
            IntegerKind::Convert => {
                if operation.bits == operation.result_bits {
                    arguments[0]
                } else if operation.bits < operation.result_bits {
                    if operation.signed {
                        builder.ins().sextend(result_ty, arguments[0])
                    } else {
                        builder.ins().uextend(result_ty, arguments[0])
                    }
                } else {
                    builder.ins().ireduce(result_ty, arguments[0])
                }
            }
            IntegerKind::Narrow => {
                let narrow_ty = integer_type(operation.narrow_bits);
                let narrowed = if operation.narrow_bits == operation.bits {
                    arguments[0]
                } else {
                    builder.ins().ireduce(narrow_ty, arguments[0])
                };
                if operation.narrow_bits == operation.result_bits {
                    narrowed
                } else if operation.signed {
                    builder.ins().sextend(result_ty, narrowed)
                } else {
                    builder.ins().uextend(result_ty, narrowed)
                }
            }
        };
        vec![value]
    }
}

fn integer_type(bits: u8) -> ir::Type {
    match bits {
        8 => ir::types::I8,
        16 => ir::types::I16,
        32 => ir::types::I32,
        64 => ir::types::I64,
        _ => unreachable!("validated integer width"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::host_fns::RuntimeError;
    use tidepool_repr::execution_schema::{Atom, ScalarLiteral};

    use std::sync::{atomic::AtomicBool, Arc};

    fn sig(arguments: Vec<RuntimeRep>, results: Vec<RuntimeRep>) -> Signature {
        Signature { arguments, results }
    }

    fn run_scalar(
        name: &str,
        argument_reps: Vec<RuntimeRep>,
        result_rep: RuntimeRep,
        arguments: Vec<Atom>,
    ) -> tidepool_bridge::Value {
        use tidepool_repr::execution_schema::{testing, *};

        let mut wire = testing::wire_program();
        wire.signatures[0].results = vec![result_rep];
        wire.signatures.push(Signature {
            arguments: argument_reps,
            results: vec![result_rep],
        });
        wire.operations.push(OperationDecl {
            identity: OperationIdentity::PrimOp(name.into()),
            signature: SignatureId(1),
        });
        wire.expressions.nodes[0] = ExprFrame::Operation {
            operation: OperationId(0),
            arguments,
        };
        let prepared = testing::prepare(wire).unwrap();
        let linked = link_program(prepared, &MachineImports::default()).unwrap();
        let compiled = super::super::CompiledProgram::compile(&linked).unwrap();
        compiled
            .run_entry(
                ValueId(0),
                &[],
                &super::super::RunOptions::default(),
                Arc::new(AtomicBool::new(false)),
            )
            .unwrap()
            .values
            .into_iter()
            .next()
            .unwrap()
    }

    fn run_scalar_result(
        name: &str,
        argument_reps: Vec<RuntimeRep>,
        result_rep: RuntimeRep,
        arguments: Vec<Atom>,
    ) -> Result<super::super::RunResult, super::super::ExecutionError> {
        use tidepool_repr::execution_schema::{testing, *};

        let mut wire = testing::wire_program();
        wire.signatures[0].results = vec![result_rep];
        wire.signatures.push(Signature {
            arguments: argument_reps,
            results: vec![result_rep],
        });
        wire.operations.push(OperationDecl {
            identity: OperationIdentity::PrimOp(name.into()),
            signature: SignatureId(1),
        });
        wire.expressions.nodes[0] = ExprFrame::Operation {
            operation: OperationId(0),
            arguments,
        };
        let prepared = testing::prepare(wire).unwrap();
        let linked = link_program(prepared, &MachineImports::default()).unwrap();
        let compiled = super::super::CompiledProgram::compile(&linked).unwrap();
        compiled.run_entry(
            ValueId(0),
            &[],
            &super::super::RunOptions::default(),
            Arc::new(AtomicBool::new(false)),
        )
    }

    fn run_tuple_result(
        name: &str,
        argument_reps: Vec<RuntimeRep>,
        result_rep: RuntimeRep,
        arguments: Vec<Atom>,
    ) -> Result<Vec<tidepool_bridge::Value>, super::super::ExecutionError> {
        use tidepool_repr::execution_schema::{testing, *};

        let mut wire = testing::wire_program();
        wire.signatures[0].results = vec![result_rep, result_rep];
        wire.signatures.push(Signature {
            arguments: argument_reps,
            results: vec![result_rep, result_rep],
        });
        wire.operations.push(OperationDecl {
            identity: OperationIdentity::PrimOp(name.into()),
            signature: SignatureId(1),
        });
        wire.expressions.nodes[0] = ExprFrame::Operation {
            operation: OperationId(0),
            arguments,
        };
        let prepared = testing::prepare(wire).unwrap();
        let linked = link_program(prepared, &MachineImports::default()).unwrap();
        let compiled = super::super::CompiledProgram::compile(&linked).unwrap();
        compiled
            .run_entry(
                ValueId(0),
                &[],
                &super::super::RunOptions::default(),
                Arc::new(AtomicBool::new(false)),
            )
            .map(|result| result.values)
    }

    fn int(bits: u8, value: i64) -> Atom {
        let bytes = value.to_be_bytes();
        Atom::Scalar(ScalarLiteral::Int {
            bits,
            bytes: bytes[8 - usize::from(bits / 8)..].to_vec(),
        })
    }

    fn word(bits: u8, value: u64) -> Atom {
        let bytes = value.to_be_bytes();
        Atom::Scalar(ScalarLiteral::Word {
            bits,
            bytes: bytes[8 - usize::from(bits / 8)..].to_vec(),
        })
    }

    #[test]
    fn integer_identity_requires_exact_signature() {
        let identity = OperationIdentity::PrimOp("+#".into());
        let signature = sig(vec![RuntimeRep::Int(64); 2], vec![RuntimeRep::Int(64)]);
        assert!(IntegerFamily::recognize(&identity, &signature).is_some());
        let wrong = sig(
            vec![RuntimeRep::Int(64), RuntimeRep::Word(64)],
            vec![RuntimeRep::Int(64)],
        );
        assert!(IntegerFamily::recognize(&identity, &wrong).is_none());

        let fabricated_width = sig(vec![RuntimeRep::Int(8); 2], vec![RuntimeRep::Int(8)]);
        assert!(IntegerFamily::recognize(&identity, &fabricated_width).is_none());
    }

    #[test]
    fn division_is_admitted_only_for_exact_scalar_signature() {
        let identity = OperationIdentity::PrimOp("quotInt#".into());
        let signature = sig(vec![RuntimeRep::Int(64); 2], vec![RuntimeRep::Int(64)]);
        assert!(IntegerFamily::recognize(&identity, &signature).is_some());
        let wrong = sig(
            vec![RuntimeRep::Int(64), RuntimeRep::Word(64)],
            vec![RuntimeRep::Int(64)],
        );
        assert!(IntegerFamily::recognize(&identity, &wrong).is_none());

        for (quot, rem, rep) in [
            ("quotInt8#", "remInt8#", RuntimeRep::Int(8)),
            ("quotInt16#", "remInt16#", RuntimeRep::Int(16)),
            ("quotInt32#", "remInt32#", RuntimeRep::Int(32)),
            ("quotInt64#", "remInt64#", RuntimeRep::Int(64)),
            ("quotWord8#", "remWord8#", RuntimeRep::Word(8)),
            ("quotWord16#", "remWord16#", RuntimeRep::Word(16)),
            ("quotWord32#", "remWord32#", RuntimeRep::Word(32)),
            ("quotWord64#", "remWord64#", RuntimeRep::Word(64)),
        ] {
            let signature = sig(vec![rep, rep], vec![rep]);
            assert!(
                IntegerFamily::recognize(&OperationIdentity::PrimOp(quot.into()), &signature,)
                    .is_some()
            );
            assert!(
                IntegerFamily::recognize(&OperationIdentity::PrimOp(rem.into()), &signature,)
                    .is_some()
            );
        }
        for (name, rep) in [
            ("quotRemInt8#", RuntimeRep::Int(8)),
            ("quotRemInt16#", RuntimeRep::Int(16)),
            ("quotRemInt32#", RuntimeRep::Int(32)),
            ("quotRemInt64#", RuntimeRep::Int(64)),
            ("quotRemWord8#", RuntimeRep::Word(8)),
            ("quotRemWord16#", RuntimeRep::Word(16)),
            ("quotRemWord32#", RuntimeRep::Word(32)),
            ("quotRemWord64#", RuntimeRep::Word(64)),
        ] {
            let signature = sig(vec![rep, rep], vec![rep, rep]);
            assert!(
                IntegerFamily::recognize(&OperationIdentity::PrimOp(name.into()), &signature,)
                    .is_some()
            );
        }
    }

    #[test]
    fn quotient_and_remainder_use_real_ghc_names_and_truncate() {
        let quotient = run_scalar(
            "quotInt#",
            vec![RuntimeRep::Int(64); 2],
            RuntimeRep::Int(64),
            vec![int(64, -7), int(64, 3)],
        );
        assert!(matches!(
            quotient,
            tidepool_bridge::Value::Lit(tidepool_repr::Literal::LitInt(-2))
        ));
        let remainder = run_scalar(
            "remInt#",
            vec![RuntimeRep::Int(64); 2],
            RuntimeRep::Int(64),
            vec![int(64, -7), int(64, 3)],
        );
        assert!(matches!(
            remainder,
            tidepool_bridge::Value::Lit(tidepool_repr::Literal::LitInt(-1))
        ));

        let word_quotient = run_scalar(
            "quotWord#",
            vec![RuntimeRep::Word(64); 2],
            RuntimeRep::Word(64),
            vec![word(64, 7), word(64, 3)],
        );
        assert!(matches!(
            word_quotient,
            tidepool_bridge::Value::Lit(tidepool_repr::Literal::LitWord(2))
        ));
        let word_remainder = run_scalar(
            "remWord#",
            vec![RuntimeRep::Word(64); 2],
            RuntimeRep::Word(64),
            vec![word(64, 7), word(64, 3)],
        );
        assert!(matches!(
            word_remainder,
            tidepool_bridge::Value::Lit(tidepool_repr::Literal::LitWord(1))
        ));

        let tuple = run_tuple_result(
            "quotRemInt#",
            vec![RuntimeRep::Int(64); 2],
            RuntimeRep::Int(64),
            vec![int(64, -7), int(64, 3)],
        )
        .unwrap();
        assert!(matches!(
            tuple.as_slice(),
            [
                tidepool_bridge::Value::Lit(tidepool_repr::Literal::LitInt(-2)),
                tidepool_bridge::Value::Lit(tidepool_repr::Literal::LitInt(-1)),
            ]
        ));
        let word_tuple = run_tuple_result(
            "quotRemWord#",
            vec![RuntimeRep::Word(64); 2],
            RuntimeRep::Word(64),
            vec![word(64, 7), word(64, 3)],
        )
        .unwrap();
        assert!(matches!(
            word_tuple.as_slice(),
            [
                tidepool_bridge::Value::Lit(tidepool_repr::Literal::LitWord(2)),
                tidepool_bridge::Value::Lit(tidepool_repr::Literal::LitWord(1)),
            ]
        ));

        let narrow_signed = run_scalar(
            "quotInt8#",
            vec![RuntimeRep::Int(8); 2],
            RuntimeRep::Int(8),
            vec![int(8, -7), int(8, 3)],
        );
        assert!(matches!(
            narrow_signed,
            tidepool_bridge::Value::Lit(tidepool_repr::Literal::LitInt(-2))
        ));
        let narrow_word = run_scalar(
            "remWord16#",
            vec![RuntimeRep::Word(16); 2],
            RuntimeRep::Word(16),
            vec![word(16, 7), word(16, 3)],
        );
        assert!(matches!(
            narrow_word,
            tidepool_bridge::Value::Lit(tidepool_repr::Literal::LitWord(1))
        ));
    }

    #[test]
    fn quotient_failures_are_typed_and_rem_min_overflow_is_zero() {
        let zero = run_scalar_result(
            "quotInt#",
            vec![RuntimeRep::Int(64); 2],
            RuntimeRep::Int(64),
            vec![int(64, 7), int(64, 0)],
        )
        .unwrap_err();
        assert!(matches!(
            zero,
            super::super::ExecutionError::Runtime(crate::machine_state::MachineFailure {
                cause: RuntimeError::DivisionByZero,
                ..
            })
        ));

        let overflow = run_scalar_result(
            "quotInt#",
            vec![RuntimeRep::Int(64); 2],
            RuntimeRep::Int(64),
            vec![int(64, i64::MIN), int(64, -1)],
        )
        .unwrap_err();
        assert!(matches!(
            overflow,
            super::super::ExecutionError::Runtime(crate::machine_state::MachineFailure {
                cause: RuntimeError::Overflow,
                ..
            })
        ));

        let remainder = run_scalar(
            "remInt#",
            vec![RuntimeRep::Int(64); 2],
            RuntimeRep::Int(64),
            vec![int(64, i64::MIN), int(64, -1)],
        );
        assert!(matches!(
            remainder,
            tidepool_bridge::Value::Lit(tidepool_repr::Literal::LitInt(0))
        ));

        let tuple_zero = run_tuple_result(
            "quotRemInt#",
            vec![RuntimeRep::Int(64); 2],
            RuntimeRep::Int(64),
            vec![int(64, 7), int(64, 0)],
        )
        .unwrap_err();
        assert!(matches!(
            tuple_zero,
            super::super::ExecutionError::Runtime(crate::machine_state::MachineFailure {
                cause: RuntimeError::DivisionByZero,
                ..
            })
        ));

        let tuple_overflow = run_tuple_result(
            "quotRemInt#",
            vec![RuntimeRep::Int(64); 2],
            RuntimeRep::Int(64),
            vec![int(64, i64::MIN), int(64, -1)],
        )
        .unwrap_err();
        assert!(matches!(
            tuple_overflow,
            super::super::ExecutionError::Runtime(crate::machine_state::MachineFailure {
                cause: RuntimeError::Overflow,
                ..
            })
        ));
    }

    #[test]
    fn words_compare_unsigned_and_return_an_integer() {
        let identity = OperationIdentity::PrimOp("ltWord#".into());
        let signature = sig(vec![RuntimeRep::Word(64); 2], vec![RuntimeRep::Int(64)]);
        assert!(IntegerFamily::recognize(&identity, &signature).is_some());

        let fabricated_result = sig(vec![RuntimeRep::Word(64); 2], vec![RuntimeRep::Int(8)]);
        assert!(IntegerFamily::recognize(&identity, &fabricated_result).is_none());
    }

    #[test]
    fn shifts_require_a_target_int_count_even_for_words() {
        let identity = OperationIdentity::PrimOp("uncheckedShiftL#".into());
        let genuine = sig(
            vec![RuntimeRep::Word(64), RuntimeRep::Int(64)],
            vec![RuntimeRep::Word(64)],
        );
        assert!(IntegerFamily::recognize(&identity, &genuine).is_some());
        let fabricated = sig(
            vec![RuntimeRep::Word(64), RuntimeRep::Word(64)],
            vec![RuntimeRep::Word(64)],
        );
        assert!(IntegerFamily::recognize(&identity, &fabricated).is_none());

        let shifted = run_scalar(
            "uncheckedShiftL#",
            vec![RuntimeRep::Word(64), RuntimeRep::Int(64)],
            RuntimeRep::Word(64),
            vec![word(64, 1), int(64, 8)],
        );
        assert!(matches!(
            shifted,
            tidepool_bridge::Value::Lit(tidepool_repr::Literal::LitWord(256))
        ));
    }

    #[test]
    fn fixed_width_word64_primops_use_ghc_names_and_int_counts() {
        let word64_binary = sig(vec![RuntimeRep::Word(64); 2], vec![RuntimeRep::Word(64)]);
        assert!(IntegerFamily::recognize(
            &OperationIdentity::PrimOp("and64#".into()),
            &word64_binary,
        )
        .is_some());
        assert!(IntegerFamily::recognize(
            &OperationIdentity::PrimOp("andWord64#".into()),
            &word64_binary,
        )
        .is_none());

        let signed_shift = sig(
            vec![RuntimeRep::Int(64), RuntimeRep::Int(64)],
            vec![RuntimeRep::Int(64)],
        );
        let word_shift = sig(
            vec![RuntimeRep::Word(64), RuntimeRep::Int(64)],
            vec![RuntimeRep::Word(64)],
        );
        let word_count = sig(vec![RuntimeRep::Word(64); 2], vec![RuntimeRep::Word(64)]);
        let signed_word_count = sig(
            vec![RuntimeRep::Int(64), RuntimeRep::Word(64)],
            vec![RuntimeRep::Int(64)],
        );
        for name in ["uncheckedShiftL64#", "uncheckedShiftRL64#"] {
            let identity = OperationIdentity::PrimOp(name.into());
            assert!(IntegerFamily::recognize(&identity, &word_shift).is_some());
            assert!(IntegerFamily::recognize(&identity, &word_count).is_none());
        }
        for name in [
            "uncheckedIShiftL64#",
            "uncheckedIShiftRA64#",
            "uncheckedIShiftRL64#",
        ] {
            let identity = OperationIdentity::PrimOp(name.into());
            assert!(IntegerFamily::recognize(&identity, &signed_shift).is_some());
            assert!(IntegerFamily::recognize(&identity, &signed_word_count).is_none());
        }
    }

    #[test]
    fn narrow_primops_are_target_width_in_and_out() {
        let identity = OperationIdentity::PrimOp("narrow8Int#".into());
        assert!(IntegerFamily::recognize(
            &identity,
            &sig(vec![RuntimeRep::Int(64)], vec![RuntimeRep::Int(64)])
        )
        .is_some());
        assert!(IntegerFamily::recognize(
            &identity,
            &sig(vec![RuntimeRep::Int(8)], vec![RuntimeRep::Int(8)])
        )
        .is_none());
    }

    #[test]
    fn integer_arithmetic_and_bits_wrap_at_the_declared_width() {
        let add = run_scalar(
            "+#",
            vec![RuntimeRep::Int(64); 2],
            RuntimeRep::Int(64),
            vec![int(64, i64::MAX), int(64, 1)],
        );
        assert!(matches!(
            add,
            tidepool_bridge::Value::Lit(tidepool_repr::Literal::LitInt(i64::MIN))
        ));

        let mul = run_scalar(
            "*#",
            vec![RuntimeRep::Int(64); 2],
            RuntimeRep::Int(64),
            vec![int(64, 1_i64 << 32), int(64, 1_i64 << 32)],
        );
        assert!(matches!(
            mul,
            tidepool_bridge::Value::Lit(tidepool_repr::Literal::LitInt(0))
        ));

        let bits = run_scalar(
            "xorI#",
            vec![RuntimeRep::Int(64); 2],
            RuntimeRep::Int(64),
            vec![int(64, -16), int(64, 15)],
        );
        assert!(matches!(
            bits,
            tidepool_bridge::Value::Lit(tidepool_repr::Literal::LitInt(-1))
        ));

        let word_sub = run_scalar(
            "subWord8#",
            vec![RuntimeRep::Word(8); 2],
            RuntimeRep::Word(8),
            vec![word(8, 0), word(8, 1)],
        );
        assert!(matches!(
            word_sub,
            tidepool_bridge::Value::Lit(tidepool_repr::Literal::LitWord(255))
        ));
    }

    #[test]
    fn signed_and_unsigned_comparisons_use_the_declared_domain() {
        let signed = run_scalar(
            "<#",
            vec![RuntimeRep::Int(64); 2],
            RuntimeRep::Int(64),
            vec![int(64, -1), int(64, 1)],
        );
        assert!(matches!(
            signed,
            tidepool_bridge::Value::Lit(tidepool_repr::Literal::LitInt(1))
        ));

        let unsigned = run_scalar(
            "ltWord8#",
            vec![RuntimeRep::Word(8); 2],
            RuntimeRep::Int(64),
            vec![word(8, 255), word(8, 1)],
        );
        assert!(matches!(
            unsigned,
            tidepool_bridge::Value::Lit(tidepool_repr::Literal::LitInt(0))
        ));
    }

    #[test]
    fn conversions_and_narrowing_preserve_signedness() {
        let to_word = run_scalar(
            "int2Word#",
            vec![RuntimeRep::Int(64)],
            RuntimeRep::Word(64),
            vec![int(64, -1)],
        );
        assert!(matches!(
            to_word,
            tidepool_bridge::Value::Lit(tidepool_repr::Literal::LitWord(u64::MAX))
        ));

        let to_int = run_scalar(
            "word2Int#",
            vec![RuntimeRep::Word(64)],
            RuntimeRep::Int(64),
            vec![word(64, u64::MAX)],
        );
        assert!(matches!(
            to_int,
            tidepool_bridge::Value::Lit(tidepool_repr::Literal::LitInt(-1))
        ));

        let narrow = run_scalar(
            "narrow8Int#",
            vec![RuntimeRep::Int(64)],
            RuntimeRep::Int(64),
            vec![int(64, 0x180)],
        );
        assert!(matches!(
            narrow,
            tidepool_bridge::Value::Lit(tidepool_repr::Literal::LitInt(-128))
        ));
    }

    #[test]
    fn w5_a3_integer_add_runs_through_real_adapter() {
        use std::sync::{atomic::AtomicBool, Arc};
        use tidepool_repr::execution_schema::{testing, *};

        let mut wire = testing::wire_program();
        wire.signatures.push(Signature {
            arguments: vec![RuntimeRep::Int(64); 2],
            results: vec![RuntimeRep::Int(64)],
        });
        wire.operations.push(OperationDecl {
            identity: OperationIdentity::PrimOp("+#".into()),
            signature: SignatureId(1),
        });
        wire.expressions.nodes[0] = ExprFrame::Operation {
            operation: OperationId(0),
            arguments: [20_i64, 22]
                .into_iter()
                .map(|value| {
                    Atom::Scalar(ScalarLiteral::Int {
                        bits: 64,
                        bytes: value.to_be_bytes().to_vec(),
                    })
                })
                .collect(),
        };
        let prepared = testing::prepare(wire).unwrap();
        let linked = link_program(prepared, &MachineImports::default()).unwrap();
        let compiled = super::super::CompiledProgram::compile(&linked).unwrap();
        let result = compiled
            .run_entry(
                ValueId(0),
                &[],
                &super::super::RunOptions::default(),
                Arc::new(AtomicBool::new(false)),
            )
            .unwrap();
        assert!(matches!(
            result.values.as_slice(),
            [tidepool_bridge::Value::Lit(tidepool_repr::Literal::LitInt(
                42
            ))]
        ));
    }
}
