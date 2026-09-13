//! Representation-checked primitive operations. Scalar families are pure;
//! descriptor-backed arrays and failure operations use their owning paths.
//!
//! Names are the authoritative spellings emitted by `Tidepool.PrimOps` from
//! GHC's `PrimOp` table. An operation is admitted only after its complete wire
//! signature has been checked; emitters never infer a heap layout from bits.

use crate::pipeline::CodegenPipeline;
use cranelift_codegen::{ir, ir::InstBuilder};
use cranelift_frontend::FunctionBuilder;
use std::sync::Arc;
use tidepool_repr::execution_schema::{
    ForeignConvention, OperationDecl, OperationIdentity, ResultContract, RuntimeRep, Signature,
};

fn result_reps(signature: &Signature) -> Option<&[RuntimeRep]> {
    match &signature.results {
        ResultContract::Returns(reps) => Some(reps),
        ResultContract::NoSuccess => None,
    }
}

fn returns_exact(signature: &Signature, expected: &[RuntimeRep]) -> bool {
    result_reps(signature) == Some(expected)
}

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
pub(super) enum BasicScalarOperation {
    PlusAddr,
    Chr,
    Ord,
    EqChar,
    GeChar,
    Clz8,
    Clz,
}

pub(super) struct BasicScalarFamily;

impl ScalarFamily for BasicScalarFamily {
    type Operation = BasicScalarOperation;

    fn recognize(identity: &OperationIdentity, signature: &Signature) -> Option<Self::Operation> {
        let OperationIdentity::PrimOp(name) = identity else {
            return None;
        };
        use RuntimeRep::*;
        match name.as_str() {
            "plusAddr#"
                if signature.arguments == [Address, Int(64)]
                    && returns_exact(signature, &[Address]) =>
            {
                Some(BasicScalarOperation::PlusAddr)
            }
            "chr#" if signature.arguments == [Int(64)] && returns_exact(signature, &[Word(64)]) => {
                Some(BasicScalarOperation::Chr)
            }
            "ord#" if signature.arguments == [Word(64)] && returns_exact(signature, &[Int(64)]) => {
                Some(BasicScalarOperation::Ord)
            }
            "eqChar#"
                if signature.arguments == [Word(64), Word(64)]
                    && returns_exact(signature, &[Int(64)]) =>
            {
                Some(BasicScalarOperation::EqChar)
            }
            "geChar#"
                if signature.arguments == [Word(64), Word(64)]
                    && returns_exact(signature, &[Int(64)]) =>
            {
                Some(BasicScalarOperation::GeChar)
            }
            "clz8#"
                if signature.arguments == [Word(64)] && returns_exact(signature, &[Word(64)]) =>
            {
                Some(BasicScalarOperation::Clz8)
            }
            "clz#"
                if signature.arguments == [Word(64)] && returns_exact(signature, &[Word(64)]) =>
            {
                Some(BasicScalarOperation::Clz)
            }
            _ => None,
        }
    }

    fn emit(
        operation: Self::Operation,
        builder: &mut FunctionBuilder<'_>,
        arguments: &[ir::Value],
    ) -> Vec<ir::Value> {
        let value = match operation {
            // Addr# is an untagged machine word here. Arithmetic never inspects memory.
            BasicScalarOperation::PlusAddr => builder.ins().iadd(arguments[0], arguments[1]),
            BasicScalarOperation::Chr | BasicScalarOperation::Ord => arguments[0],
            BasicScalarOperation::EqChar => {
                let equal =
                    builder
                        .ins()
                        .icmp(ir::condcodes::IntCC::Equal, arguments[0], arguments[1]);
                let one = builder.ins().iconst(ir::types::I64, 1);
                let zero = builder.ins().iconst(ir::types::I64, 0);
                builder.ins().select(equal, one, zero)
            }
            BasicScalarOperation::GeChar => {
                let greater_or_equal = builder.ins().icmp(
                    ir::condcodes::IntCC::UnsignedGreaterThanOrEqual,
                    arguments[0],
                    arguments[1],
                );
                builder.ins().uextend(ir::types::I64, greater_or_equal)
            }
            BasicScalarOperation::Clz8 => {
                let low = builder.ins().ireduce(ir::types::I8, arguments[0]);
                let count = builder.ins().clz(low);
                builder.ins().uextend(ir::types::I64, count)
            }
            BasicScalarOperation::Clz => builder.ins().clz(arguments[0]),
        };
        vec![value]
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum IntegerKind {
    Add,
    Sub,
    SubWordC,
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
    if signature.arguments.as_slice() == [lhs, rhs] && returns_exact(signature, &[result]) {
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
    if signature.arguments.as_slice() == [arg] && returns_exact(signature, &[result]) {
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
    let result_bits = match result_reps(signature)? {
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
    (signature.arguments.as_slice() == [lhs, int_rep(64)] && returns_exact(signature, &[lhs]))
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
    (signature.arguments.as_slice() == [rep, rep] && returns_exact(signature, &[rep, rep]))
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
    (signature.arguments.as_slice() == [value, int_rep(64)] && returns_exact(signature, &[value]))
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
    let result = match (to_signed, result_reps(signature)?) {
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
    (signature.arguments.as_slice() == [source] && returns_exact(signature, &[result])).then_some(
        IntegerOperation {
            kind: IntegerKind::Convert,
            signed: from_signed,
            bits: from_bits,
            result_bits: to_bits,
            narrow_bits: 0,
        },
    )
}

fn narrow(signature: &Signature, signed: bool, result_bits: u8) -> Option<IntegerOperation> {
    let source = match (signed, signature.arguments.as_slice()) {
        (true, [RuntimeRep::Int(64)]) => 64,
        (false, [RuntimeRep::Word(64)]) => 64,
        _ => return None,
    };
    let result = if signed { int_rep(64) } else { word_rep(64) };
    returns_exact(signature, &[result]).then_some(IntegerOperation {
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
        "subWordC#"
            if signature.arguments == [word_rep(64), word_rep(64)]
                && returns_exact(signature, &[word_rep(64), int_rep(64)]) =>
        {
            Some(IntegerOperation {
                kind: IntegerKind::SubWordC,
                signed: false,
                bits: 64,
                result_bits: 64,
                narrow_bits: 0,
            })
        }
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

/// Recognize admitted primitive identities. Primops and typed C-call intrinsics
/// use separate recognition authorities even when their spellings resemble
/// one another.
pub(super) fn recognize_operation(
    declaration: &OperationDecl,
    signature: &Signature,
) -> Option<PrimitiveOperation> {
    if let OperationIdentity::Capability { name } = &declaration.identity {
        return super::capabilities::recognize(name, signature).map(PrimitiveOperation::Capability);
    }
    if let Some(operation) = super::arrays::recognize(&declaration.identity, signature) {
        return Some(PrimitiveOperation::Array(operation));
    }
    if let Some(operation) = super::byte_arrays::recognize(&declaration.identity, signature) {
        return Some(PrimitiveOperation::ByteArray(operation));
    }
    if let Some(operation) = super::addresses::recognize(&declaration.identity, signature) {
        return Some(PrimitiveOperation::Address(operation));
    }
    if let Some(operation) = super::fingerprint::recognize(&declaration.identity, signature) {
        return Some(PrimitiveOperation::Fingerprint(operation));
    }
    if let Some(operation) = super::formatting::recognize(&declaration.identity, signature) {
        return Some(PrimitiveOperation::Formatting(operation));
    }
    if let Some(operation) = super::wide_words::recognize(&declaration.identity, signature) {
        return Some(PrimitiveOperation::WideWord(operation));
    }
    if matches!(&declaration.identity, OperationIdentity::PrimOp(name) if name == "double2Int#")
        && signature.arguments == [RuntimeRep::Float(64)]
        && returns_exact(signature, &[RuntimeRep::Int(64)])
    {
        return Some(PrimitiveOperation::DoubleToInt);
    }
    if matches!(&declaration.identity, OperationIdentity::PrimOp(name) if name == "indexCharOffAddr#")
        && signature.arguments == [RuntimeRep::Address, RuntimeRep::Int(64)]
        && returns_exact(signature, &[RuntimeRep::Word(64)])
    {
        return Some(PrimitiveOperation::IndexCharOffAddr);
    }
    if matches!(&declaration.identity, OperationIdentity::PrimOp(name) if name == "copyAddrToByteArray#")
        && signature.arguments
            == [
                RuntimeRep::Address,
                RuntimeRep::UnliftedRef,
                RuntimeRep::Int(64),
                RuntimeRep::Int(64),
                RuntimeRep::Void,
            ]
        && returns_exact(signature, &[])
    {
        return Some(PrimitiveOperation::CopyAddrToByteArray);
    }
    if matches!(&declaration.identity, OperationIdentity::Intrinsic { symbol, convention: ForeignConvention::CCall } if symbol == "strlen")
        && signature.arguments == [RuntimeRep::Address, RuntimeRep::Void]
        && returns_exact(signature, &[RuntimeRep::Int(64)])
    {
        return Some(PrimitiveOperation::CStringLen);
    }
    if matches!(&declaration.identity, OperationIdentity::PrimOp(name) if name == "raise#")
        && signature.arguments == [RuntimeRep::LiftedRef]
        && matches!(&signature.results, ResultContract::NoSuccess)
    {
        return Some(PrimitiveOperation::Raise);
    }
    if matches!(&declaration.identity, OperationIdentity::PrimOp(name) if name == "dataToTagSmall#")
        && signature.arguments == [RuntimeRep::LiftedRef]
        && returns_exact(signature, &[RuntimeRep::Int(64)])
    {
        return Some(PrimitiveOperation::DataToTagSmall);
    }
    if let OperationIdentity::WiredInError { kind } = &declaration.identity {
        let arguments =
            if *kind == tidepool_repr::execution_schema::WiredInErrorKind::AbsentSumField {
                &[][..]
            } else {
                &[RuntimeRep::Address][..]
            };
        if signature.arguments == arguments && signature.results == ResultContract::NoSuccess {
            return Some(PrimitiveOperation::WiredInError(*kind));
        }
    }
    if matches!(&declaration.identity, OperationIdentity::PrimOp(name) if name == "noDuplicate#")
        && signature.arguments == [RuntimeRep::Void]
        && returns_exact(signature, &[])
    {
        return Some(PrimitiveOperation::NoDuplicate);
    }
    if signature.arguments == [RuntimeRep::Void] && signature.results == ResultContract::NoSuccess {
        if let OperationIdentity::PrimOp(name) = &declaration.identity {
            let cause = match name.as_str() {
                "raiseDivZero#" => Some(super::fallible::PrimitiveFailure::DivisionByZero),
                "raiseUnderflow#" => Some(super::fallible::PrimitiveFailure::Underflow),
                _ => None,
            };
            if let Some(cause) = cause {
                return Some(PrimitiveOperation::PrimitiveFailure(cause));
            }
        }
    }
    BasicScalarFamily::recognize(&declaration.identity, signature)
        .map(PrimitiveOperation::BasicScalar)
        .or_else(|| {
            IntegerFamily::recognize(&declaration.identity, signature)
                .map(PrimitiveOperation::Integer)
        })
        .or_else(|| {
            super::floating::FloatingFamily::recognize(&declaration.identity, signature)
                .map(PrimitiveOperation::Floating)
        })
}

#[derive(Clone, Copy)]
pub(super) enum PrimitiveOperation {
    Fingerprint(super::fingerprint::FingerprintOperation),
    Address(super::addresses::AddressOperation),
    Capability(super::capabilities::Capability),
    Array(super::arrays::ArrayOperation),
    ByteArray(super::byte_arrays::ByteOperation),
    Formatting(super::formatting::FormattingOperation),
    DoubleToInt,
    IndexCharOffAddr,
    CopyAddrToByteArray,
    CStringLen,
    Raise,
    DataToTagSmall,
    WiredInError(tidepool_repr::execution_schema::WiredInErrorKind),
    NoDuplicate,
    PrimitiveFailure(super::fallible::PrimitiveFailure),
    BasicScalar(BasicScalarOperation),
    WideWord(super::wide_words::WideWordOperation),
    Integer(IntegerOperation),
    Floating(super::floating::FloatingOperation),
}

impl PrimitiveOperation {
    pub(super) fn is_terminal(self) -> bool {
        matches!(
            self,
            Self::Capability(_) | Self::WiredInError(_) | Self::Raise | Self::PrimitiveFailure(_)
        )
    }
}

pub(super) fn emit_operation(
    operation: PrimitiveOperation,
    builder: &mut FunctionBuilder<'_>,
    arguments: &[ir::Value],
    vmctx: ir::Value,
    pipeline: &mut CodegenPipeline,
    bytes: &Arc<super::static_bytes::PinnedBytes>,
    prepared_enter: cranelift_module::FuncId,
    gc: cranelift_module::FuncId,
    boxed_array: &tidepool_heap::execution_descriptor::ObjectDescriptor,
    mut_var: &tidepool_heap::execution_descriptor::ObjectDescriptor,
    bytes_array: &tidepool_heap::execution_descriptor::ObjectDescriptor,
) -> Result<Option<Vec<ir::Value>>, super::CompileError> {
    match operation {
        PrimitiveOperation::Fingerprint(operation) => {
            super::fingerprint::emit(builder, pipeline, vmctx, bytes, operation, arguments)
                .map(Some)
        }
        PrimitiveOperation::Address(super::addresses::AddressOperation::ReadWord8) => {
            super::addresses::emit_read_word8(builder, pipeline, vmctx, bytes, arguments).map(Some)
        }
        PrimitiveOperation::Address(super::addresses::AddressOperation::WriteWord8) => {
            super::addresses::emit_write_word8(builder, pipeline, vmctx, arguments).map(Some)
        }
        PrimitiveOperation::ByteArray(super::byte_arrays::ByteOperation::Contents) => {
            super::byte_arrays::emit_byte_array_contents(
                builder,
                pipeline,
                vmctx,
                bytes_array,
                arguments,
            )
            .map(Some)
        }
        PrimitiveOperation::Capability(capability) => {
            super::capabilities::emit_unsupported(builder, pipeline, vmctx, capability)?;
            Ok(None)
        }
        PrimitiveOperation::DataToTagSmall => {
            super::data_tag::emit(builder, pipeline, vmctx, prepared_enter, arguments[0]).map(Some)
        }
        PrimitiveOperation::WiredInError(kind) => {
            super::failures::emit_wired_in_error(
                builder,
                pipeline,
                vmctx,
                bytes,
                kind,
                arguments.first().copied(),
            )?;
            Ok(None)
        }
        // Prepared invocations have one private, serialized evaluator, eager
        // blackholes, and no scheduler. There can be no duplicate evaluation
        // for this primop to suppress under that execution contract.
        PrimitiveOperation::NoDuplicate => Ok(Some(Vec::new())),
        PrimitiveOperation::PrimitiveFailure(cause) => {
            super::fallible::emit_terminal(builder, pipeline, vmctx, cause)?;
            Ok(None)
        }
        PrimitiveOperation::Array(super::arrays::ArrayOperation::NewBoxed) => {
            super::arrays::emit_new_boxed(builder, pipeline, vmctx, gc, boxed_array, arguments)
                .map(Some)
        }
        PrimitiveOperation::Array(super::arrays::ArrayOperation::ReadBoxed) => {
            super::arrays::emit_read_boxed(builder, pipeline, vmctx, boxed_array, arguments)
                .map(Some)
        }
        PrimitiveOperation::Array(super::arrays::ArrayOperation::WriteBoxed) => {
            super::arrays::emit_write_boxed(builder, pipeline, vmctx, boxed_array, arguments)
                .map(Some)
        }
        PrimitiveOperation::Array(super::arrays::ArrayOperation::NewMutVar) => {
            super::arrays::emit_new_mut_var(builder, pipeline, vmctx, gc, mut_var, arguments)
                .map(Some)
        }
        PrimitiveOperation::Array(super::arrays::ArrayOperation::ReadMutVar) => {
            super::arrays::emit_read_mut_var(builder, pipeline, vmctx, mut_var, arguments).map(Some)
        }
        PrimitiveOperation::Array(super::arrays::ArrayOperation::WriteMutVar) => {
            super::arrays::emit_write_mut_var(builder, pipeline, vmctx, mut_var, arguments)
                .map(Some)
        }
        PrimitiveOperation::Array(super::arrays::ArrayOperation::SizeofBoxed) => {
            super::arrays::emit_sizeof_boxed(builder, pipeline, vmctx, boxed_array, arguments)
                .map(Some)
        }
        PrimitiveOperation::Array(super::arrays::ArrayOperation::UnsafeFreezeBoxed) => {
            super::arrays::emit_freeze_boxed(builder, pipeline, vmctx, boxed_array, arguments)
                .map(Some)
        }
        PrimitiveOperation::Array(super::arrays::ArrayOperation::ShrinkSmallBoxed) => {
            super::arrays::emit_shrink_boxed(builder, pipeline, vmctx, boxed_array, arguments)
                .map(Some)
        }
        PrimitiveOperation::Array(super::arrays::ArrayOperation::CopyBoxed) => {
            super::arrays::emit_copy_boxed(builder, pipeline, vmctx, boxed_array, arguments)
                .map(Some)
        }
        PrimitiveOperation::Array(super::arrays::ArrayOperation::CasBoxed) => {
            super::arrays::emit_cas_boxed(builder, pipeline, vmctx, boxed_array, arguments)
                .map(Some)
        }
        PrimitiveOperation::ByteArray(super::byte_arrays::ByteOperation::New) => {
            super::byte_arrays::emit_new_bytes(builder, pipeline, vmctx, gc, bytes_array, arguments)
                .map(Some)
        }
        PrimitiveOperation::ByteArray(super::byte_arrays::ByteOperation::Resize) => {
            super::byte_arrays::emit_resize_bytes(
                builder,
                pipeline,
                vmctx,
                gc,
                bytes_array,
                arguments,
            )
            .map(Some)
        }
        PrimitiveOperation::ByteArray(super::byte_arrays::ByteOperation::Freeze) => {
            super::byte_arrays::emit_freeze_bytes(builder, pipeline, vmctx, bytes_array, arguments)
                .map(Some)
        }
        PrimitiveOperation::ByteArray(super::byte_arrays::ByteOperation::Size) => {
            super::byte_arrays::emit_sizeof_bytes(builder, pipeline, vmctx, bytes_array, arguments)
                .map(Some)
        }
        PrimitiveOperation::ByteArray(super::byte_arrays::ByteOperation::Shrink) => {
            super::byte_arrays::emit_shrink_bytes(builder, pipeline, vmctx, bytes_array, arguments)
                .map(Some)
        }
        PrimitiveOperation::ByteArray(super::byte_arrays::ByteOperation::Copy) => {
            super::byte_arrays::emit_copy_bytes(builder, pipeline, vmctx, bytes_array, arguments)
                .map(Some)
        }
        PrimitiveOperation::ByteArray(super::byte_arrays::ByteOperation::Compare) => {
            super::byte_arrays::emit_compare_bytes(builder, pipeline, vmctx, bytes_array, arguments)
                .map(Some)
        }
        PrimitiveOperation::ByteArray(super::byte_arrays::ByteOperation::Read(element)) => {
            super::byte_arrays::emit_read_bytes(
                builder,
                pipeline,
                vmctx,
                bytes_array,
                arguments,
                element,
            )
            .map(Some)
        }
        PrimitiveOperation::ByteArray(super::byte_arrays::ByteOperation::Write(element)) => {
            super::byte_arrays::emit_write_bytes(
                builder,
                pipeline,
                vmctx,
                bytes_array,
                arguments,
                element,
            )
            .map(Some)
        }
        PrimitiveOperation::Formatting(super::formatting::FormattingOperation::NeedsPrecedence) => {
            Ok(Some(super::formatting::emit_needs_precedence(
                builder,
                arguments[0],
            )))
        }
        PrimitiveOperation::Formatting(super::formatting::FormattingOperation::Bytes) => {
            super::formatting::emit_render_bytes(
                builder,
                pipeline,
                vmctx,
                gc,
                bytes_array,
                arguments,
                false,
            )
            .map(Some)
        }
        PrimitiveOperation::Formatting(super::formatting::FormattingOperation::PrecBytes) => {
            super::formatting::emit_render_bytes(
                builder,
                pipeline,
                vmctx,
                gc,
                bytes_array,
                arguments,
                true,
            )
            .map(Some)
        }
        PrimitiveOperation::Raise => {
            super::no_success::emit_terminal(
                builder,
                pipeline,
                vmctx,
                super::no_success::TerminalCause::Raised(arguments[0]),
            )?;
            Ok(None)
        }
        PrimitiveOperation::DoubleToInt => {
            super::fallible::emit_double_to_int(builder, vmctx, pipeline, arguments[0]).map(Some)
        }
        PrimitiveOperation::IndexCharOffAddr => super::static_bytes::emit_index_char(
            builder,
            pipeline,
            vmctx,
            bytes,
            arguments[0],
            arguments[1],
        )
        .map(Some),
        PrimitiveOperation::CopyAddrToByteArray => {
            super::static_bytes::emit_copy_addr_to_byte_array(
                builder,
                pipeline,
                vmctx,
                bytes,
                bytes_array,
                arguments,
            )
            .map(Some)
        }
        PrimitiveOperation::CStringLen => {
            super::static_bytes::emit_c_string_len(builder, pipeline, vmctx, bytes, arguments[0])
                .map(Some)
        }
        PrimitiveOperation::Integer(operation)
            if matches!(
                operation.kind,
                IntegerKind::Quot | IntegerKind::Rem | IntegerKind::QuotRem
            ) =>
        {
            super::fallible::emit(operation, builder, vmctx, pipeline, arguments).map(Some)
        }
        PrimitiveOperation::Integer(operation) => {
            Ok(Some(IntegerFamily::emit(operation, builder, arguments)))
        }
        PrimitiveOperation::BasicScalar(operation) => {
            Ok(Some(BasicScalarFamily::emit(operation, builder, arguments)))
        }
        PrimitiveOperation::WideWord(operation) => {
            super::wide_words::emit(operation, builder, pipeline, vmctx, arguments).map(Some)
        }
        PrimitiveOperation::Floating(operation) => Ok(Some(super::floating::FloatingFamily::emit(
            operation, builder, arguments,
        ))),
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
            IntegerKind::SubWordC => {
                let difference = builder.ins().isub(arguments[0], arguments[1]);
                let borrowed = builder.ins().icmp(
                    ir::condcodes::IntCC::UnsignedLessThan,
                    arguments[0],
                    arguments[1],
                );
                return vec![difference, builder.ins().uextend(ir::types::I64, borrowed)];
            }
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
        Signature {
            arguments,
            results: ResultContract::Returns(results),
        }
    }

    fn run_scalar(
        name: &str,
        argument_reps: Vec<RuntimeRep>,
        result_rep: RuntimeRep,
        arguments: Vec<Atom>,
    ) -> tidepool_bridge::Value {
        use tidepool_repr::execution_schema::{testing, *};

        let mut wire = testing::wire_program();
        wire.signatures[0].results = ResultContract::Returns(vec![result_rep]);
        wire.signatures.push(Signature {
            arguments: argument_reps,
            results: ResultContract::Returns(vec![result_rep]),
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
        wire.signatures[0].results = ResultContract::Returns(vec![result_rep]);
        wire.signatures.push(Signature {
            arguments: argument_reps,
            results: ResultContract::Returns(vec![result_rep]),
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
        result_reps: [RuntimeRep; 2],
        arguments: Vec<Atom>,
    ) -> Result<Vec<tidepool_bridge::Value>, super::super::ExecutionError> {
        use tidepool_repr::execution_schema::{testing, *};

        let mut wire = testing::wire_program();
        wire.signatures[0].results = ResultContract::Returns(result_reps.to_vec());
        wire.signatures.push(Signature {
            arguments: argument_reps,
            results: ResultContract::Returns(result_reps.to_vec()),
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
    fn basic_scalars_require_exact_primop_signatures() {
        use RuntimeRep::*;
        for (name, accepted, wrong_argument, wrong_result) in [
            (
                "plusAddr#",
                sig(vec![Address, Int(64)], vec![Address]),
                sig(vec![Word(64), Int(64)], vec![Address]),
                sig(vec![Address, Int(64)], vec![Word(64)]),
            ),
            (
                "chr#",
                sig(vec![Int(64)], vec![Word(64)]),
                sig(vec![Word(64)], vec![Word(64)]),
                sig(vec![Int(64)], vec![Int(64)]),
            ),
            (
                "eqChar#",
                sig(vec![Word(64), Word(64)], vec![Int(64)]),
                sig(vec![Word(64), Int(64)], vec![Int(64)]),
                sig(vec![Word(64), Word(64)], vec![Word(64)]),
            ),
            (
                "clz8#",
                sig(vec![Word(64)], vec![Word(64)]),
                sig(vec![Word(8)], vec![Word(64)]),
                sig(vec![Word(64)], vec![Word(8)]),
            ),
        ] {
            let identity = OperationIdentity::PrimOp(name.into());
            assert!(BasicScalarFamily::recognize(&identity, &accepted).is_some());
            assert!(BasicScalarFamily::recognize(&identity, &wrong_argument).is_none());
            assert!(BasicScalarFamily::recognize(&identity, &wrong_result).is_none());
            assert!(BasicScalarFamily::recognize(
                &identity,
                &Signature {
                    arguments: accepted.arguments.clone(),
                    results: ResultContract::NoSuccess,
                },
            )
            .is_none());
        }
        assert!(BasicScalarFamily::recognize(
            &OperationIdentity::Intrinsic {
                symbol: "chr#".into(),
                convention: tidepool_repr::execution_schema::ForeignConvention::CCall,
            },
            &sig(vec![Int(64)], vec![Word(64)]),
        )
        .is_none());
    }

    #[test]
    fn basic_scalars_run_through_the_prepared_adapter() {
        let chr = run_scalar(
            "chr#",
            vec![RuntimeRep::Int(64)],
            RuntimeRep::Word(64),
            vec![int(64, -1)],
        );
        assert!(matches!(
            chr,
            tidepool_bridge::Value::Lit(tidepool_repr::Literal::LitWord(u64::MAX))
        ));

        for (left, right, expected) in [(0x10ffff, 0x10ffff, 1), (0x10ffff, 65, 0)] {
            let equal = run_scalar(
                "eqChar#",
                vec![RuntimeRep::Word(64); 2],
                RuntimeRep::Int(64),
                vec![word(64, left), word(64, right)],
            );
            assert!(matches!(
                equal,
                tidepool_bridge::Value::Lit(tidepool_repr::Literal::LitInt(value)) if value == expected
            ));
        }

        for (input, expected) in [(0, 8), (0x80, 0), (0x10, 3), (0x100, 8)] {
            let count = run_scalar(
                "clz8#",
                vec![RuntimeRep::Word(64)],
                RuntimeRep::Word(64),
                vec![word(64, input)],
            );
            assert!(matches!(
                count,
                tidepool_bridge::Value::Lit(tidepool_repr::Literal::LitWord(value)) if value == expected
            ));
        }
    }

    #[test]
    fn plus_addr_moves_within_pinned_bytes_before_adapter_observation() {
        use tidepool_repr::execution_schema::{testing, *};

        let mut wire = testing::wire_program();
        wire.signatures[0].results = ResultContract::Returns(vec![RuntimeRep::Word(64)]);
        wire.signatures.push(sig(
            vec![RuntimeRep::Address, RuntimeRep::Int(64)],
            vec![RuntimeRep::Address],
        ));
        wire.signatures.push(sig(
            vec![RuntimeRep::Address, RuntimeRep::Int(64)],
            vec![RuntimeRep::Word(64)],
        ));
        wire.operations.push(OperationDecl {
            identity: OperationIdentity::PrimOp("plusAddr#".into()),
            signature: SignatureId(1),
        });
        wire.operations.push(OperationDecl {
            identity: OperationIdentity::PrimOp("indexCharOffAddr#".into()),
            signature: SignatureId(2),
        });
        wire.expressions.nodes = vec![
            ExprFrame::Operation {
                operation: OperationId(0),
                arguments: vec![
                    Atom::Scalar(ScalarLiteral::Bytes(b"AB".to_vec())),
                    int(64, 1),
                ],
            },
            ExprFrame::Operation {
                operation: OperationId(1),
                arguments: vec![Atom::Ref(ValueRef::Local(ValueId(1))), int(64, 0)],
            },
            ExprFrame::Case {
                scrutinee: 0,
                binder: ValueId(1),
                scrutinee_results: ResultContract::Returns(vec![RuntimeRep::Address]),
                kind: CaseKind::Polymorphic,
                alternatives: vec![Alternative {
                    pattern: AlternativePattern::Default,
                    binders: vec![],
                    body: 1,
                }],
            },
        ];
        let Group::NonRecursive(entry) = &mut wire.bindings[0] else {
            unreachable!("fixture entry is nonrecursive")
        };
        let HeapRhs::Function { body, .. } = &mut entry.binding.rhs else {
            unreachable!("fixture entry is a function")
        };
        *body = 2;

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
            [tidepool_bridge::Value::Lit(
                tidepool_repr::Literal::LitWord(66)
            )]
        ));
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
    fn sub_word_c_requires_exact_mixed_result_signature() {
        use RuntimeRep::*;

        let identity = OperationIdentity::PrimOp("subWordC#".into());
        let accepted = sig(vec![Word(64), Word(64)], vec![Word(64), Int(64)]);
        assert!(matches!(
            IntegerFamily::recognize(&identity, &accepted),
            Some(IntegerOperation {
                kind: IntegerKind::SubWordC,
                ..
            })
        ));
        for rejected in [
            sig(vec![Int(64), Word(64)], vec![Word(64), Int(64)]),
            sig(vec![Word(32), Word(32)], vec![Word(64), Int(64)]),
            sig(vec![Word(64), Word(64)], vec![Word(64)]),
            sig(vec![Word(64), Word(64)], vec![Word(64), Word(64)]),
            sig(vec![Word(64), Word(64)], vec![Int(64), Word(64)]),
            Signature {
                arguments: vec![Word(64), Word(64)],
                results: ResultContract::NoSuccess,
            },
        ] {
            assert!(IntegerFamily::recognize(&identity, &rejected).is_none());
        }
    }

    #[test]
    fn sub_word_c_returns_wrapped_difference_and_unsigned_borrow() {
        use RuntimeRep::*;

        for (left, right, difference, borrow) in [
            (0, 1, u64::MAX, 1),
            (u64::MAX, 1, u64::MAX - 1, 0),
            (0, 0, 0, 0),
            (1_u64 << 63, 1, (1_u64 << 63) - 1, 0),
            (0, 1_u64 << 63, 1_u64 << 63, 1),
        ] {
            let values = run_tuple_result(
                "subWordC#",
                vec![Word(64), Word(64)],
                [Word(64), Int(64)],
                vec![word(64, left), word(64, right)],
            )
            .unwrap();
            assert!(
                matches!(
                    values.as_slice(),
                    [
                        tidepool_bridge::Value::Lit(tidepool_repr::Literal::LitWord(actual_difference)),
                        tidepool_bridge::Value::Lit(tidepool_repr::Literal::LitInt(actual_borrow)),
                    ] if *actual_difference == difference && *actual_borrow == borrow
                ),
                "subWordC#({left}, {right}) returned {values:?}"
            );
        }
    }

    #[test]
    fn copy_addr_to_byte_array_requires_exact_primop_signature() {
        use RuntimeRep::*;

        let declaration = OperationDecl {
            identity: OperationIdentity::PrimOp("copyAddrToByteArray#".into()),
            signature: tidepool_repr::execution_schema::SignatureId(0),
        };
        let accepted = sig(vec![Address, UnliftedRef, Int(64), Int(64), Void], vec![]);
        assert!(matches!(
            recognize_operation(&declaration, &accepted),
            Some(PrimitiveOperation::CopyAddrToByteArray)
        ));
        for rejected in [
            sig(vec![Word(64), UnliftedRef, Int(64), Int(64), Void], vec![]),
            sig(vec![Address, UnliftedRef, Int(64), Int(64)], vec![]),
            sig(
                vec![Address, UnliftedRef, Int(64), Int(64), Void],
                vec![Void],
            ),
        ] {
            assert!(recognize_operation(&declaration, &rejected).is_none());
        }
    }

    #[test]
    fn no_duplicate_requires_exact_void_to_empty_returns_contract() {
        use RuntimeRep::*;
        let declaration = OperationDecl {
            identity: OperationIdentity::PrimOp("noDuplicate#".into()),
            signature: tidepool_repr::execution_schema::SignatureId(0),
        };
        assert!(matches!(
            recognize_operation(&declaration, &sig(vec![Void], vec![])),
            Some(PrimitiveOperation::NoDuplicate)
        ));
        for rejected in [
            sig(vec![], vec![]),
            sig(vec![Void], vec![Void]),
            sig(vec![UnliftedRef], vec![]),
        ] {
            assert!(recognize_operation(&declaration, &rejected).is_none());
        }
    }

    #[test]
    fn raise_requires_exact_nonsuccess_contract() {
        let declaration = OperationDecl {
            identity: OperationIdentity::PrimOp("raise#".into()),
            signature: tidepool_repr::execution_schema::SignatureId(0),
        };
        let signature = Signature {
            arguments: vec![RuntimeRep::LiftedRef],
            results: ResultContract::NoSuccess,
        };
        assert!(matches!(
            recognize_operation(&declaration, &signature),
            Some(PrimitiveOperation::Raise)
        ));

        let returning = Signature {
            arguments: vec![RuntimeRep::LiftedRef],
            results: ResultContract::Returns(vec![RuntimeRep::LiftedRef]),
        };
        assert!(recognize_operation(&declaration, &returning).is_none());

        let wrong_argument = Signature {
            arguments: vec![RuntimeRep::Int(64)],
            results: ResultContract::NoSuccess,
        };
        assert!(recognize_operation(&declaration, &wrong_argument).is_none());
    }

    #[test]
    fn wired_errors_require_authoritative_identity_and_exact_bottoming_signature() {
        use tidepool_repr::execution_schema::WiredInErrorKind;

        let declaration = OperationDecl {
            identity: OperationIdentity::WiredInError {
                kind: WiredInErrorKind::PatternMatch,
            },
            signature: tidepool_repr::execution_schema::SignatureId(0),
        };
        let accepted = Signature {
            arguments: vec![RuntimeRep::Address],
            results: ResultContract::NoSuccess,
        };
        assert!(matches!(
            recognize_operation(&declaration, &accepted),
            Some(PrimitiveOperation::WiredInError(
                WiredInErrorKind::PatternMatch
            ))
        ));
        for rejected in [
            Signature {
                arguments: vec![],
                results: ResultContract::NoSuccess,
            },
            Signature {
                arguments: vec![RuntimeRep::Address],
                results: ResultContract::Returns(vec![]),
            },
        ] {
            assert!(recognize_operation(&declaration, &rejected).is_none());
        }

        let nullary = OperationDecl {
            identity: OperationIdentity::WiredInError {
                kind: WiredInErrorKind::AbsentSumField,
            },
            signature: tidepool_repr::execution_schema::SignatureId(0),
        };
        assert!(matches!(
            recognize_operation(
                &nullary,
                &Signature {
                    arguments: vec![],
                    results: ResultContract::NoSuccess,
                }
            ),
            Some(PrimitiveOperation::WiredInError(
                WiredInErrorKind::AbsentSumField
            ))
        ));

        let foreign_spelling = OperationDecl {
            identity: OperationIdentity::Intrinsic {
                symbol: "patError".into(),
                convention: ForeignConvention::CCall,
            },
            signature: tidepool_repr::execution_schema::SignatureId(0),
        };
        assert!(recognize_operation(&foreign_spelling, &accepted).is_none());
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
            [RuntimeRep::Int(64); 2],
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
            [RuntimeRep::Word(64); 2],
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
            [RuntimeRep::Int(64); 2],
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
            [RuntimeRep::Int(64); 2],
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
            results: ResultContract::Returns(vec![RuntimeRep::Int(64)]),
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
