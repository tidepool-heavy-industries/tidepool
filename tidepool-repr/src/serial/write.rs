use super::WriteError;
use crate::frame::CoreFrame;
use crate::tree::RecursiveTree;
use crate::types::{Alt, AltCon, Literal, PrimOpKind};
use ciborium::value::Value;

/// Writes a CoreExpr to a CBOR-encoded byte vector.
pub fn write_cbor(expr: &RecursiveTree<CoreFrame<usize>>) -> Result<Vec<u8>, WriteError> {
    if expr.nodes.is_empty() {
        return Err(WriteError::Cbor(
            "attempted to write an empty RecursiveTree as a CoreExpr".to_string(),
        ));
    }

    let mut nodes_val = Vec::with_capacity(expr.nodes.len());
    for node in &expr.nodes {
        nodes_val.push(encode_frame(node));
    }

    let root_idx = (expr.nodes.len() - 1) as u64;

    let tree_val = Value::Array(vec![
        Value::Array(nodes_val),
        Value::Integer(root_idx.into()),
    ]);

    let mut bytes = Vec::new();
    ciborium::ser::into_writer(&tree_val, &mut bytes)
        .map_err(|e| WriteError::Cbor(e.to_string()))?;
    Ok(bytes)
}

/// Writes a DataConTable to CBOR-encoded metadata bytes (new format with warnings).
pub fn write_metadata(table: &crate::datacon_table::DataConTable) -> Result<Vec<u8>, WriteError> {
    use crate::datacon::SrcBang;

    let mut entries = Vec::with_capacity(table.len());
    for dc in table.iter() {
        let dcid = dc.id.0;
        let name = &dc.name;
        let tag = dc.tag as u64;
        let arity = dc.rep_arity as u64;
        let bangs = Value::Array(
            dc.field_bangs
                .iter()
                .map(|b| {
                    Value::Text(
                        match b {
                            SrcBang::SrcBang => "SrcBang",
                            SrcBang::SrcUnpack => "SrcUnpack",
                            SrcBang::NoSrcBang => "NoSrcBang",
                        }
                        .to_string(),
                    )
                })
                .collect(),
        );

        let mut entry = vec![
            Value::Integer(dcid.into()),
            Value::Text(name.clone()),
            Value::Integer(tag.into()),
            Value::Integer(arity.into()),
            bangs,
        ];
        if let Some(ref qn) = dc.qualified_name {
            entry.push(Value::Text(qn.clone()));
        }
        entries.push(Value::Array(entry));
    }

    // New format: [entries_array, warnings_map]
    let warnings_map = Value::Map(vec![(
        Value::Text("has_io".to_string()),
        Value::Bool(false),
    )]);

    let root = Value::Array(vec![Value::Array(entries), warnings_map]);

    let mut bytes = Vec::new();
    ciborium::ser::into_writer(&root, &mut bytes).map_err(|e| WriteError::Cbor(e.to_string()))?;
    Ok(bytes)
}

fn encode_frame(frame: &CoreFrame<usize>) -> Value {
    match frame {
        CoreFrame::Var(id) => Value::Array(vec![
            Value::Text("Var".to_string()),
            Value::Integer(id.0.into()),
        ]),
        CoreFrame::Lit(lit) => {
            Value::Array(vec![Value::Text("Lit".to_string()), encode_literal(lit)])
        }
        CoreFrame::App { fun, arg } => Value::Array(vec![
            Value::Text("App".to_string()),
            Value::Integer((*fun as u64).into()),
            Value::Integer((*arg as u64).into()),
        ]),
        CoreFrame::Lam { binder, body } => Value::Array(vec![
            Value::Text("Lam".to_string()),
            Value::Integer(binder.0.into()),
            Value::Integer((*body as u64).into()),
        ]),
        CoreFrame::LetNonRec { binder, rhs, body } => Value::Array(vec![
            Value::Text("LetNonRec".to_string()),
            Value::Integer(binder.0.into()),
            Value::Integer((*rhs as u64).into()),
            Value::Integer((*body as u64).into()),
        ]),
        CoreFrame::LetRec { bindings, body } => {
            let bindings_val = Value::Array(
                bindings
                    .iter()
                    .map(|(id, rhs)| {
                        Value::Array(vec![
                            Value::Integer(id.0.into()),
                            Value::Integer((*rhs as u64).into()),
                        ])
                    })
                    .collect(),
            );
            Value::Array(vec![
                Value::Text("LetRec".to_string()),
                bindings_val,
                Value::Integer((*body as u64).into()),
            ])
        }
        CoreFrame::Case {
            scrutinee,
            binder,
            alts,
        } => {
            let alts_val = Value::Array(alts.iter().map(encode_alt).collect());
            Value::Array(vec![
                Value::Text("Case".to_string()),
                Value::Integer((*scrutinee as u64).into()),
                Value::Integer(binder.0.into()),
                alts_val,
            ])
        }
        CoreFrame::Con { tag, fields } => {
            let fields_val = Value::Array(
                fields
                    .iter()
                    .map(|f| Value::Integer((*f as u64).into()))
                    .collect(),
            );
            Value::Array(vec![
                Value::Text("Con".to_string()),
                Value::Integer(tag.0.into()),
                fields_val,
            ])
        }
        CoreFrame::Join {
            label,
            params,
            rhs,
            body,
        } => {
            let params_val =
                Value::Array(params.iter().map(|p| Value::Integer(p.0.into())).collect());
            Value::Array(vec![
                Value::Text("Join".to_string()),
                Value::Integer(label.0.into()),
                params_val,
                Value::Integer((*rhs as u64).into()),
                Value::Integer((*body as u64).into()),
            ])
        }
        CoreFrame::Jump { label, args } => {
            let args_val = Value::Array(
                args.iter()
                    .map(|a| Value::Integer((*a as u64).into()))
                    .collect(),
            );
            Value::Array(vec![
                Value::Text("Jump".to_string()),
                Value::Integer(label.0.into()),
                args_val,
            ])
        }
        CoreFrame::PrimOp { op, args } => {
            let args_val = Value::Array(
                args.iter()
                    .map(|a| Value::Integer((*a as u64).into()))
                    .collect(),
            );
            Value::Array(vec![
                Value::Text("PrimOp".to_string()),
                Value::Text(encode_primop(op).to_string()),
                args_val,
            ])
        }
    }
}

fn encode_primop(op: &PrimOpKind) -> &'static str {
    use PrimOpKind::*;
    match op {
        IntAdd => "IntAdd",
        IntSub => "IntSub",
        IntMul => "IntMul",
        IntNegate => "IntNegate",
        IntEq => "IntEq",
        IntNe => "IntNe",
        IntLt => "IntLt",
        IntLe => "IntLe",
        IntGt => "IntGt",
        IntGe => "IntGe",
        WordAdd => "WordAdd",
        WordSub => "WordSub",
        WordMul => "WordMul",
        WordEq => "WordEq",
        WordNe => "WordNe",
        WordLt => "WordLt",
        WordLe => "WordLe",
        WordGt => "WordGt",
        WordGe => "WordGe",
        DoubleAdd => "DoubleAdd",
        DoubleSub => "DoubleSub",
        DoubleMul => "DoubleMul",
        DoubleDiv => "DoubleDiv",
        DoubleEq => "DoubleEq",
        DoubleNe => "DoubleNe",
        DoubleLt => "DoubleLt",
        DoubleLe => "DoubleLe",
        DoubleGt => "DoubleGt",
        DoubleGe => "DoubleGe",
        CharEq => "CharEq",
        CharNe => "CharNe",
        CharLt => "CharLt",
        CharLe => "CharLe",
        CharGt => "CharGt",
        CharGe => "CharGe",
        IndexArray => "IndexArray",
        SeqOp => "SeqOp",
        TagToEnum => "TagToEnum",
        DataToTag => "DataToTag",
        IntQuot => "IntQuot",
        IntRem => "IntRem",
        DecodeDoubleMantissa => "DecodeDoubleMantissa",
        DecodeDoubleExponent => "DecodeDoubleExponent",
        Chr => "Chr",
        Ord => "Ord",
        IntAnd => "IntAnd",
        IntOr => "IntOr",
        IntXor => "IntXor",
        IntNot => "IntNot",
        IntShl => "IntShl",
        IntShra => "IntShra",
        IntShrl => "IntShrl",
        WordQuot => "WordQuot",
        WordRem => "WordRem",
        WordAnd => "WordAnd",
        WordOr => "WordOr",
        WordXor => "WordXor",
        WordNot => "WordNot",
        WordShl => "WordShl",
        WordShrl => "WordShrl",
        Int2Word => "Int2Word",
        Word2Int => "Word2Int",
        Narrow8Int => "Narrow8Int",
        Narrow16Int => "Narrow16Int",
        Narrow32Int => "Narrow32Int",
        Narrow8Word => "Narrow8Word",
        Narrow16Word => "Narrow16Word",
        Narrow32Word => "Narrow32Word",
        FloatAdd => "FloatAdd",
        FloatSub => "FloatSub",
        FloatMul => "FloatMul",
        FloatDiv => "FloatDiv",
        FloatNegate => "FloatNegate",
        FloatEq => "FloatEq",
        FloatNe => "FloatNe",
        FloatLt => "FloatLt",
        FloatLe => "FloatLe",
        FloatGt => "FloatGt",
        FloatGe => "FloatGe",
        DoubleNegate => "DoubleNegate",
        DoubleFabs => "DoubleFabs",
        DoubleSqrt => "DoubleSqrt",
        DoubleExp => "DoubleExp",
        DoubleExpM1 => "DoubleExpM1",
        DoubleLog => "DoubleLog",
        DoubleLog1P => "DoubleLog1P",
        DoubleSin => "DoubleSin",
        DoubleCos => "DoubleCos",
        DoubleTan => "DoubleTan",
        DoubleAsin => "DoubleAsin",
        DoubleAcos => "DoubleAcos",
        DoubleAtan => "DoubleAtan",
        DoubleSinh => "DoubleSinh",
        DoubleCosh => "DoubleCosh",
        DoubleTanh => "DoubleTanh",
        DoubleAsinh => "DoubleAsinh",
        DoubleAcosh => "DoubleAcosh",
        DoubleAtanh => "DoubleAtanh",
        DoublePower => "DoublePower",
        Int2Double => "Int2Double",
        Double2Int => "Double2Int",
        Int2Float => "Int2Float",
        Float2Int => "Float2Int",
        Double2Float => "Double2Float",
        Float2Double => "Float2Double",
        ReallyUnsafePtrEquality => "ReallyUnsafePtrEquality",
        IndexCharOffAddr => "IndexCharOffAddr",
        PlusAddr => "PlusAddr",
        Raise => "Raise",
        NewByteArray => "NewByteArray",
        ReadWord8Array => "ReadWord8Array",
        WriteWord8Array => "WriteWord8Array",
        SizeofMutableByteArray => "SizeofMutableByteArray",
        UnsafeFreezeByteArray => "UnsafeFreezeByteArray",
        CopyByteArray => "CopyByteArray",
        CopyMutableByteArray => "CopyMutableByteArray",
        CopyAddrToByteArray => "CopyAddrToByteArray",
        ShrinkMutableByteArray => "ShrinkMutableByteArray",
        ResizeMutableByteArray => "ResizeMutableByteArray",
        Clz8 => "Clz8",
        IntToInt64 => "IntToInt64",
        Int64ToWord64 => "Int64ToWord64",
        TimesInt2Hi => "TimesInt2Hi",
        TimesInt2Lo => "TimesInt2Lo",
        TimesInt2Overflow => "TimesInt2Overflow",
        IndexWord8Array => "IndexWord8Array",
        IndexWord8OffAddr => "IndexWord8OffAddr",
        CompareByteArrays => "CompareByteArrays",
        WordToWord8 => "WordToWord8",
        Word64And => "Word64And",
        Int64ToInt => "Int64ToInt",
        Word64ToInt64 => "Word64ToInt64",
        Word8ToWord => "Word8ToWord",
        Word8Lt => "Word8Lt",
        Int64Ge => "Int64Ge",
        Int64Negate => "Int64Negate",
        Int64Shra => "Int64Shra",
        Word64Shl => "Word64Shl",
        Word8Ge => "Word8Ge",
        Word8Sub => "Word8Sub",
        SizeofByteArray => "SizeofByteArray",
        IndexWordArray => "IndexWordArray",
        Int64Add => "Int64Add",
        Int64Gt => "Int64Gt",
        Int64Lt => "Int64Lt",
        Int64Le => "Int64Le",
        Int64Sub => "Int64Sub",
        Int64Mul => "Int64Mul",
        Int64Shl => "Int64Shl",
        WriteWordArray => "WriteWordArray",
        ReadWordArray => "ReadWordArray",
        SetByteArray => "SetByteArray",
        Word64Or => "Word64Or",
        Word8Add => "Word8Add",
        Word8Le => "Word8Le",
        AddIntCVal => "AddIntCVal",
        AddIntCCarry => "AddIntCCarry",
        SubWordCVal => "SubWordCVal",
        SubWordCCarry => "SubWordCCarry",
        AddWordCVal => "AddWordCVal",
        AddWordCCarry => "AddWordCCarry",
        TimesWord2Hi => "TimesWord2Hi",
        TimesWord2Lo => "TimesWord2Lo",
        QuotRemWordVal => "QuotRemWordVal",
        QuotRemWordRem => "QuotRemWordRem",
        FfiStrlen => "FfiStrlen",
        FfiTextMeasureOff => "FfiTextMeasureOff",
        FfiTextMemchr => "FfiTextMemchr",
        FfiTextReverse => "FfiTextReverse",
        // SmallArray#
        NewSmallArray => "NewSmallArray",
        ReadSmallArray => "ReadSmallArray",
        WriteSmallArray => "WriteSmallArray",
        IndexSmallArray => "IndexSmallArray",
        SizeofSmallArray => "SizeofSmallArray",
        SizeofSmallMutableArray => "SizeofSmallMutableArray",
        UnsafeFreezeSmallArray => "UnsafeFreezeSmallArray",
        UnsafeThawSmallArray => "UnsafeThawSmallArray",
        CopySmallArray => "CopySmallArray",
        CopySmallMutableArray => "CopySmallMutableArray",
        CloneSmallArray => "CloneSmallArray",
        CloneSmallMutableArray => "CloneSmallMutableArray",
        ShrinkSmallMutableArray => "ShrinkSmallMutableArray",
        CasSmallArray => "CasSmallArray",
        // Array#
        NewArray => "NewArray",
        ReadArray => "ReadArray",
        WriteArray => "WriteArray",
        SizeofArray => "SizeofArray",
        SizeofMutableArray => "SizeofMutableArray",
        UnsafeFreezeArray => "UnsafeFreezeArray",
        UnsafeThawArray => "UnsafeThawArray",
        CopyArray => "CopyArray",
        CopyMutableArray => "CopyMutableArray",
        CloneArray => "CloneArray",
        CloneMutableArray => "CloneMutableArray",
        // Bit operations
        PopCnt => "PopCnt",
        PopCnt8 => "PopCnt8",
        PopCnt16 => "PopCnt16",
        PopCnt32 => "PopCnt32",
        PopCnt64 => "PopCnt64",
        Ctz => "Ctz",
        Ctz8 => "Ctz8",
        Ctz16 => "Ctz16",
        Ctz32 => "Ctz32",
        Ctz64 => "Ctz64",
        ShowDoubleAddr => "ShowDoubleAddr",
    }
}

fn encode_literal(lit: &Literal) -> Value {
    match lit {
        Literal::LitInt(i) => Value::Array(vec![
            Value::Text("LitInt".to_string()),
            Value::Integer((*i).into()),
        ]),
        Literal::LitWord(w) => Value::Array(vec![
            Value::Text("LitWord".to_string()),
            Value::Integer((*w).into()),
        ]),
        Literal::LitChar(c) => Value::Array(vec![
            Value::Text("LitChar".to_string()),
            Value::Integer((*c as u32).into()),
        ]),
        Literal::LitString(s) => Value::Array(vec![
            Value::Text("LitString".to_string()),
            Value::Bytes(s.clone()),
        ]),
        Literal::LitFloat(f) => Value::Array(vec![
            Value::Text("LitFloat".to_string()),
            Value::Integer((*f).into()),
        ]),
        Literal::LitDouble(d) => Value::Array(vec![
            Value::Text("LitDouble".to_string()),
            Value::Integer((*d).into()),
        ]),
    }
}

fn encode_alt(alt: &Alt<usize>) -> Value {
    let binders_val = Value::Array(
        alt.binders
            .iter()
            .map(|b| Value::Integer(b.0.into()))
            .collect(),
    );
    Value::Array(vec![
        encode_alt_con(&alt.con),
        binders_val,
        Value::Integer((alt.body as u64).into()),
    ])
}

fn encode_alt_con(con: &AltCon) -> Value {
    match con {
        AltCon::DataAlt(id) => Value::Array(vec![
            Value::Text("DataAlt".to_string()),
            Value::Integer(id.0.into()),
        ]),
        AltCon::LitAlt(lit) => {
            Value::Array(vec![Value::Text("LitAlt".to_string()), encode_literal(lit)])
        }
        AltCon::Default => Value::Array(vec![Value::Text("Default".to_string())]),
    }
}
