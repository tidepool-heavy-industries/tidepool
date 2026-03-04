use super::ReadError;
use crate::frame::CoreFrame;
use crate::tree::RecursiveTree;
use crate::types::{Alt, AltCon, DataConId, JoinId, Literal, PrimOpKind, VarId};
use ciborium::value::Value;

/// Reads a CoreExpr from a CBOR-encoded byte slice.
pub fn read_cbor(bytes: &[u8]) -> Result<RecursiveTree<CoreFrame<usize>>, ReadError> {
    let tree_val: Value =
        ciborium::de::from_reader(bytes).map_err(|e| ReadError::Cbor(e.to_string()))?;

    let root_array = match tree_val {
        Value::Array(a) if a.len() == 2 => a,
        _ => {
            return Err(ReadError::InvalidStructure(
                "Root must be array of 2".to_string(),
            ))
        }
    };

    let nodes_array = match &root_array[0] {
        Value::Array(a) => a,
        _ => {
            return Err(ReadError::InvalidStructure(
                "First element must be array of nodes".to_string(),
            ))
        }
    };

    if nodes_array.is_empty() {
        return Err(ReadError::InvalidStructure(
            "CoreExpr must have at least one node".to_string(),
        ));
    }

    let root_idx = as_usize(&root_array[1])?;
    if root_idx != nodes_array.len() - 1 {
        return Err(ReadError::InvalidStructure(format!(
            "Root index {} does not match expected last node index {}",
            root_idx,
            nodes_array.len() - 1
        )));
    }

    let mut nodes = Vec::with_capacity(nodes_array.len());
    for node_val in nodes_array {
        nodes.push(decode_frame(node_val)?);
    }

    validate_indices(&nodes)?;

    Ok(RecursiveTree { nodes })
}

/// Structured warnings from the Haskell extractor, encoded in meta.cbor.
#[derive(Debug, Default, Clone)]
pub struct MetaWarnings {
    pub has_io: bool,
}

/// Reads a DataConTable and warnings from CBOR-encoded metadata bytes (meta.cbor format).
///
/// New format: 2-element array `[entries_array, warnings_map]`
/// Legacy format: flat array of 5-element entry arrays (backward compatible)
pub fn read_metadata(bytes: &[u8]) -> Result<(crate::DataConTable, MetaWarnings), ReadError> {
    use crate::datacon::{DataCon, SrcBang};
    use crate::datacon_table::DataConTable;
    use crate::types::DataConId;

    let val: Value =
        ciborium::de::from_reader(bytes).map_err(|e| ReadError::Cbor(e.to_string()))?;

    let root = match val {
        Value::Array(a) => a,
        _ => {
            return Err(ReadError::InvalidStructure(
                "Metadata must be a CBOR array".to_string(),
            ))
        }
    };

    // Detect new vs legacy format:
    // New format: root is [entries_array, warnings_map] where entries_array[0] is an array
    // Legacy format: root is [entry1, entry2, ...] where entry1 is a 5-element array
    let (entries, warnings) = if root.len() == 2 {
        if let Value::Array(_) = &root[0] {
            if let Value::Map(_) = &root[1] {
                // New format
                let entries = match &root[0] {
                    Value::Array(a) => a.clone(),
                    _ => unreachable!(),
                };
                let warnings = parse_warnings(&root[1]);
                (entries, warnings)
            } else {
                // Could be legacy with exactly 2 entries
                (root, MetaWarnings::default())
            }
        } else {
            // Legacy: first element is not an array (shouldn't happen, but safe fallback)
            (root, MetaWarnings::default())
        }
    } else {
        // Legacy format (0, 1, or 3+ entries)
        (root, MetaWarnings::default())
    };

    let mut table = DataConTable::new();
    for entry in &entries {
        let arr = match entry {
            Value::Array(a) if a.len() == 5 => a,
            _ => {
                return Err(ReadError::InvalidStructure(
                    "Metadata entry must be array of 5".to_string(),
                ))
            }
        };

        let dcid = as_u64(&arr[0])?;
        let name = match &arr[1] {
            Value::Text(t) => t.clone(),
            _ => {
                return Err(ReadError::InvalidStructure(
                    "DataCon name must be text".to_string(),
                ))
            }
        };
        let tag = as_u64(&arr[2])? as u32;
        let arity = as_u64(&arr[3])? as u32;
        let bangs_arr = match &arr[4] {
            Value::Array(a) => a,
            _ => {
                return Err(ReadError::InvalidStructure(
                    "DataCon bangs must be array".to_string(),
                ))
            }
        };
        let mut bangs = Vec::with_capacity(bangs_arr.len());
        for b in bangs_arr {
            let bang_str = match b {
                Value::Text(t) => t.as_str(),
                _ => return Err(ReadError::InvalidStructure("Bang must be text".to_string())),
            };
            bangs.push(match bang_str {
                "SrcBang" => SrcBang::SrcBang,
                "SrcUnpack" => SrcBang::SrcUnpack,
                "NoSrcBang" => SrcBang::NoSrcBang,
                _ => {
                    return Err(ReadError::InvalidStructure(format!(
                        "Unknown bang: {}",
                        bang_str
                    )))
                }
            });
        }

        table.insert(DataCon {
            id: DataConId(dcid),
            name,
            tag,
            rep_arity: arity,
            field_bangs: bangs,
        });
    }

    Ok((table, warnings))
}

fn parse_warnings(val: &Value) -> MetaWarnings {
    let mut warnings = MetaWarnings::default();
    if let Value::Map(pairs) = val {
        for (k, v) in pairs {
            if let Value::Text(key) = k {
                if key.as_str() == "has_io" {
                    if let Value::Bool(b) = v {
                        warnings.has_io = *b;
                    }
                }
            }
        }
    }
    warnings
}

fn validate_indices(nodes: &[CoreFrame<usize>]) -> Result<(), ReadError> {
    let len = nodes.len();
    for node in nodes {
        match node {
            CoreFrame::App { fun, arg } => {
                if *fun >= len || *arg >= len {
                    return Err(ReadError::InvalidStructure(
                        "App index out of bounds".to_string(),
                    ));
                }
            }
            CoreFrame::Lam { body, .. } => {
                if *body >= len {
                    return Err(ReadError::InvalidStructure(
                        "Lam index out of bounds".to_string(),
                    ));
                }
            }
            CoreFrame::LetNonRec { rhs, body, .. } => {
                if *rhs >= len || *body >= len {
                    return Err(ReadError::InvalidStructure(
                        "LetNonRec index out of bounds".to_string(),
                    ));
                }
            }
            CoreFrame::LetRec { bindings, body } => {
                if *body >= len {
                    return Err(ReadError::InvalidStructure(
                        "LetRec body index out of bounds".to_string(),
                    ));
                }
                for (_, rhs) in bindings {
                    if *rhs >= len {
                        return Err(ReadError::InvalidStructure(
                            "LetRec binding index out of bounds".to_string(),
                        ));
                    }
                }
            }
            CoreFrame::Case {
                scrutinee, alts, ..
            } => {
                if *scrutinee >= len {
                    return Err(ReadError::InvalidStructure(
                        "Case scrutinee index out of bounds".to_string(),
                    ));
                }
                for alt in alts {
                    if alt.body >= len {
                        return Err(ReadError::InvalidStructure(
                            "Case alt body index out of bounds".to_string(),
                        ));
                    }
                }
            }
            CoreFrame::Con { fields, .. } => {
                for f in fields {
                    if *f >= len {
                        return Err(ReadError::InvalidStructure(
                            "Con field index out of bounds".to_string(),
                        ));
                    }
                }
            }
            CoreFrame::Join { rhs, body, .. } => {
                if *rhs >= len || *body >= len {
                    return Err(ReadError::InvalidStructure(
                        "Join index out of bounds".to_string(),
                    ));
                }
            }
            CoreFrame::Jump { args, .. } => {
                for a in args {
                    if *a >= len {
                        return Err(ReadError::InvalidStructure(
                            "Jump argument index out of bounds".to_string(),
                        ));
                    }
                }
            }
            CoreFrame::PrimOp { args, .. } => {
                for a in args {
                    if *a >= len {
                        return Err(ReadError::InvalidStructure(
                            "PrimOp argument index out of bounds".to_string(),
                        ));
                    }
                }
            }
            CoreFrame::Var(_) | CoreFrame::Lit(_) => {}
        }
    }
    Ok(())
}

fn decode_frame(val: &Value) -> Result<CoreFrame<usize>, ReadError> {
    let arr = match val {
        Value::Array(a) => a,
        _ => {
            return Err(ReadError::InvalidStructure(
                "Frame must be array".to_string(),
            ))
        }
    };

    if arr.is_empty() {
        return Err(ReadError::InvalidStructure("Empty frame array".to_string()));
    }

    let tag = match &arr[0] {
        Value::Text(t) => t.as_str(),
        _ => return Err(ReadError::InvalidTag("Tag must be string".to_string())),
    };

    match tag {
        "Var" => {
            if arr.len() != 2 {
                return Err(ReadError::InvalidStructure(
                    "Var expects 1 field".to_string(),
                ));
            }
            Ok(CoreFrame::Var(VarId(as_u64(&arr[1])?)))
        }
        "Lit" => {
            if arr.len() != 2 {
                return Err(ReadError::InvalidStructure(
                    "Lit expects 1 field".to_string(),
                ));
            }
            Ok(CoreFrame::Lit(decode_literal(&arr[1])?))
        }
        "App" => {
            if arr.len() != 3 {
                return Err(ReadError::InvalidStructure(
                    "App expects 2 fields".to_string(),
                ));
            }
            Ok(CoreFrame::App {
                fun: as_usize(&arr[1])?,
                arg: as_usize(&arr[2])?,
            })
        }
        "Lam" => {
            if arr.len() != 3 {
                return Err(ReadError::InvalidStructure(
                    "Lam expects 2 fields".to_string(),
                ));
            }
            Ok(CoreFrame::Lam {
                binder: VarId(as_u64(&arr[1])?),
                body: as_usize(&arr[2])?,
            })
        }
        "LetNonRec" => {
            if arr.len() != 4 {
                return Err(ReadError::InvalidStructure(
                    "LetNonRec expects 3 fields".to_string(),
                ));
            }
            Ok(CoreFrame::LetNonRec {
                binder: VarId(as_u64(&arr[1])?),
                rhs: as_usize(&arr[2])?,
                body: as_usize(&arr[3])?,
            })
        }
        "LetRec" => {
            if arr.len() != 3 {
                return Err(ReadError::InvalidStructure(
                    "LetRec expects 2 fields".to_string(),
                ));
            }
            let bindings_arr = match &arr[1] {
                Value::Array(a) => a,
                _ => {
                    return Err(ReadError::InvalidStructure(
                        "LetRec bindings must be array".to_string(),
                    ))
                }
            };
            let mut bindings = Vec::with_capacity(bindings_arr.len());
            for b_val in bindings_arr {
                let b_arr = match b_val {
                    Value::Array(a) if a.len() == 2 => a,
                    _ => {
                        return Err(ReadError::InvalidStructure(
                            "LetRec binding must be array of 2".to_string(),
                        ))
                    }
                };
                bindings.push((VarId(as_u64(&b_arr[0])?), as_usize(&b_arr[1])?));
            }
            Ok(CoreFrame::LetRec {
                bindings,
                body: as_usize(&arr[2])?,
            })
        }
        "Case" => {
            if arr.len() != 4 {
                return Err(ReadError::InvalidStructure(
                    "Case expects 3 fields".to_string(),
                ));
            }
            let alts_arr = match &arr[3] {
                Value::Array(a) => a,
                _ => {
                    return Err(ReadError::InvalidStructure(
                        "Case alts must be array".to_string(),
                    ))
                }
            };
            let mut alts = Vec::with_capacity(alts_arr.len());
            for alt_val in alts_arr {
                alts.push(decode_alt(alt_val)?);
            }
            Ok(CoreFrame::Case {
                scrutinee: as_usize(&arr[1])?,
                binder: VarId(as_u64(&arr[2])?),
                alts,
            })
        }
        "Con" => {
            if arr.len() != 3 {
                return Err(ReadError::InvalidStructure(
                    "Con expects 2 fields".to_string(),
                ));
            }
            let fields_arr = match &arr[2] {
                Value::Array(a) => a,
                _ => {
                    return Err(ReadError::InvalidStructure(
                        "Con fields must be array".to_string(),
                    ))
                }
            };
            let mut fields = Vec::with_capacity(fields_arr.len());
            for f_val in fields_arr {
                fields.push(as_usize(f_val)?);
            }
            Ok(CoreFrame::Con {
                tag: DataConId(as_u64(&arr[1])?),
                fields,
            })
        }
        "Join" => {
            if arr.len() != 5 {
                return Err(ReadError::InvalidStructure(
                    "Join expects 4 fields".to_string(),
                ));
            }
            let params_arr = match &arr[2] {
                Value::Array(a) => a,
                _ => {
                    return Err(ReadError::InvalidStructure(
                        "Join params must be array".to_string(),
                    ))
                }
            };
            let mut params = Vec::with_capacity(params_arr.len());
            for p_val in params_arr {
                params.push(VarId(as_u64(p_val)?));
            }
            Ok(CoreFrame::Join {
                label: JoinId(as_u64(&arr[1])?),
                params,
                rhs: as_usize(&arr[3])?,
                body: as_usize(&arr[4])?,
            })
        }
        "Jump" => {
            if arr.len() != 3 {
                return Err(ReadError::InvalidStructure(
                    "Jump expects 2 fields".to_string(),
                ));
            }
            let args_arr = match &arr[2] {
                Value::Array(a) => a,
                _ => {
                    return Err(ReadError::InvalidStructure(
                        "Jump args must be array".to_string(),
                    ))
                }
            };
            let mut args = Vec::with_capacity(args_arr.len());
            for a_val in args_arr {
                args.push(as_usize(a_val)?);
            }
            Ok(CoreFrame::Jump {
                label: JoinId(as_u64(&arr[1])?),
                args,
            })
        }
        "PrimOp" => {
            if arr.len() != 3 {
                return Err(ReadError::InvalidStructure(
                    "PrimOp expects 2 fields".to_string(),
                ));
            }
            let op_name = match &arr[1] {
                Value::Text(t) => t,
                _ => {
                    return Err(ReadError::InvalidPrimOp(
                        "PrimOp op must be string".to_string(),
                    ))
                }
            };
            let op = decode_primop(op_name)?;
            let args_arr = match &arr[2] {
                Value::Array(a) => a,
                _ => {
                    return Err(ReadError::InvalidStructure(
                        "PrimOp args must be array".to_string(),
                    ))
                }
            };
            let mut args = Vec::with_capacity(args_arr.len());
            for a_val in args_arr {
                args.push(as_usize(a_val)?);
            }
            Ok(CoreFrame::PrimOp { op, args })
        }
        _ => Err(ReadError::InvalidTag(tag.to_string())),
    }
}

fn decode_literal(val: &Value) -> Result<Literal, ReadError> {
    let arr = match val {
        Value::Array(a) if a.len() == 2 => a,
        _ => {
            return Err(ReadError::InvalidLiteral(
                "Literal must be array of 2".to_string(),
            ))
        }
    };
    let tag = match &arr[0] {
        Value::Text(t) => t.as_str(),
        _ => {
            return Err(ReadError::InvalidLiteral(
                "Literal tag must be string".to_string(),
            ))
        }
    };
    match tag {
        "LitInt" => Ok(Literal::LitInt(as_i64(&arr[1])?)),
        "LitWord" => Ok(Literal::LitWord(as_u64(&arr[1])?)),
        "LitChar" => {
            let cp = as_u64(&arr[1])? as u32;
            std::char::from_u32(cp)
                .ok_or_else(|| ReadError::InvalidLiteral(format!("Invalid char codepoint: {}", cp)))
                .map(Literal::LitChar)
        }
        "LitString" => match &arr[1] {
            Value::Bytes(b) => Ok(Literal::LitString(b.clone())),
            _ => Err(ReadError::InvalidLiteral(
                "LitString expects bytes".to_string(),
            )),
        },
        "LitFloat" => Ok(Literal::LitFloat(as_u64(&arr[1])?)),
        "LitDouble" => Ok(Literal::LitDouble(as_u64(&arr[1])?)),
        _ => Err(ReadError::InvalidLiteral(tag.to_string())),
    }
}

fn decode_alt(val: &Value) -> Result<Alt<usize>, ReadError> {
    let arr = match val {
        Value::Array(a) if a.len() == 3 => a,
        _ => {
            return Err(ReadError::InvalidStructure(
                "Alt must be array of 3".to_string(),
            ))
        }
    };
    let con = decode_alt_con(&arr[0])?;
    let binders_arr = match &arr[1] {
        Value::Array(a) => a,
        _ => {
            return Err(ReadError::InvalidStructure(
                "Alt binders must be array".to_string(),
            ))
        }
    };
    let mut binders = Vec::with_capacity(binders_arr.len());
    for b_val in binders_arr {
        binders.push(VarId(as_u64(b_val)?));
    }
    let body = as_usize(&arr[2])?;
    Ok(Alt { con, binders, body })
}

fn decode_alt_con(val: &Value) -> Result<AltCon, ReadError> {
    let arr = match val {
        Value::Array(a) => a,
        _ => return Err(ReadError::InvalidAltCon("AltCon must be array".to_string())),
    };
    if arr.is_empty() {
        return Err(ReadError::InvalidAltCon("Empty AltCon array".to_string()));
    }
    let tag = match &arr[0] {
        Value::Text(t) => t.as_str(),
        _ => {
            return Err(ReadError::InvalidAltCon(
                "AltCon tag must be string".to_string(),
            ))
        }
    };
    match tag {
        "DataAlt" => {
            if arr.len() != 2 {
                return Err(ReadError::InvalidAltCon(
                    "DataAlt expects 1 field".to_string(),
                ));
            }
            Ok(AltCon::DataAlt(DataConId(as_u64(&arr[1])?)))
        }
        "LitAlt" => {
            if arr.len() != 2 {
                return Err(ReadError::InvalidAltCon(
                    "LitAlt expects 1 field".to_string(),
                ));
            }
            Ok(AltCon::LitAlt(decode_literal(&arr[1])?))
        }
        "Default" => {
            if arr.len() != 1 {
                return Err(ReadError::InvalidAltCon(
                    "Default expects 0 fields".to_string(),
                ));
            }
            Ok(AltCon::Default)
        }
        _ => Err(ReadError::InvalidAltCon(tag.to_string())),
    }
}

fn decode_primop(s: &str) -> Result<PrimOpKind, ReadError> {
    use PrimOpKind::*;
    match s {
        "IntAdd" => Ok(IntAdd),
        "IntSub" => Ok(IntSub),
        "IntMul" => Ok(IntMul),
        "IntNegate" => Ok(IntNegate),
        "IntEq" => Ok(IntEq),
        "IntNe" => Ok(IntNe),
        "IntLt" => Ok(IntLt),
        "IntLe" => Ok(IntLe),
        "IntGt" => Ok(IntGt),
        "IntGe" => Ok(IntGe),
        "WordAdd" => Ok(WordAdd),
        "WordSub" => Ok(WordSub),
        "WordMul" => Ok(WordMul),
        "WordEq" => Ok(WordEq),
        "WordNe" => Ok(WordNe),
        "WordLt" => Ok(WordLt),
        "WordLe" => Ok(WordLe),
        "WordGt" => Ok(WordGt),
        "WordGe" => Ok(WordGe),
        "DoubleAdd" => Ok(DoubleAdd),
        "DoubleSub" => Ok(DoubleSub),
        "DoubleMul" => Ok(DoubleMul),
        "DoubleDiv" => Ok(DoubleDiv),
        "DoubleEq" => Ok(DoubleEq),
        "DoubleNe" => Ok(DoubleNe),
        "DoubleLt" => Ok(DoubleLt),
        "DoubleLe" => Ok(DoubleLe),
        "DoubleGt" => Ok(DoubleGt),
        "DoubleGe" => Ok(DoubleGe),
        "CharEq" => Ok(CharEq),
        "CharNe" => Ok(CharNe),
        "CharLt" => Ok(CharLt),
        "CharLe" => Ok(CharLe),
        "CharGt" => Ok(CharGt),
        "CharGe" => Ok(CharGe),
        "IndexArray" => Ok(IndexArray),
        "SeqOp" => Ok(SeqOp),
        "TagToEnum" => Ok(TagToEnum),
        "DataToTag" => Ok(DataToTag),
        "IntQuot" => Ok(IntQuot),
        "IntRem" => Ok(IntRem),
        "DecodeDoubleMantissa" => Ok(DecodeDoubleMantissa),
        "DecodeDoubleExponent" => Ok(DecodeDoubleExponent),
        "Chr" => Ok(Chr),
        "Ord" => Ok(PrimOpKind::Ord),
        "IntAnd" => Ok(IntAnd),
        "IntOr" => Ok(IntOr),
        "IntXor" => Ok(IntXor),
        "IntNot" => Ok(IntNot),
        "IntShl" => Ok(IntShl),
        "IntShra" => Ok(IntShra),
        "IntShrl" => Ok(IntShrl),
        "WordQuot" => Ok(WordQuot),
        "WordRem" => Ok(WordRem),
        "WordAnd" => Ok(WordAnd),
        "WordOr" => Ok(WordOr),
        "WordXor" => Ok(WordXor),
        "WordNot" => Ok(WordNot),
        "WordShl" => Ok(WordShl),
        "WordShrl" => Ok(WordShrl),
        "Int2Word" => Ok(Int2Word),
        "Word2Int" => Ok(Word2Int),
        "Narrow8Int" => Ok(Narrow8Int),
        "Narrow16Int" => Ok(Narrow16Int),
        "Narrow32Int" => Ok(Narrow32Int),
        "Narrow8Word" => Ok(Narrow8Word),
        "Narrow16Word" => Ok(Narrow16Word),
        "Narrow32Word" => Ok(Narrow32Word),
        "FloatAdd" => Ok(FloatAdd),
        "FloatSub" => Ok(FloatSub),
        "FloatMul" => Ok(FloatMul),
        "FloatDiv" => Ok(FloatDiv),
        "FloatNegate" => Ok(FloatNegate),
        "FloatEq" => Ok(FloatEq),
        "FloatNe" => Ok(FloatNe),
        "FloatLt" => Ok(FloatLt),
        "FloatLe" => Ok(FloatLe),
        "FloatGt" => Ok(FloatGt),
        "FloatGe" => Ok(FloatGe),
        "DoubleNegate" => Ok(DoubleNegate),
        "DoubleFabs" => Ok(DoubleFabs),
        "DoubleSqrt" => Ok(DoubleSqrt),
        "DoubleExp" => Ok(DoubleExp),
        "DoubleExpM1" => Ok(DoubleExpM1),
        "DoubleLog" => Ok(DoubleLog),
        "DoubleLog1P" => Ok(DoubleLog1P),
        "DoubleSin" => Ok(DoubleSin),
        "DoubleCos" => Ok(DoubleCos),
        "DoubleTan" => Ok(DoubleTan),
        "DoubleAsin" => Ok(DoubleAsin),
        "DoubleAcos" => Ok(DoubleAcos),
        "DoubleAtan" => Ok(DoubleAtan),
        "DoubleSinh" => Ok(DoubleSinh),
        "DoubleCosh" => Ok(DoubleCosh),
        "DoubleTanh" => Ok(DoubleTanh),
        "DoubleAsinh" => Ok(DoubleAsinh),
        "DoubleAcosh" => Ok(DoubleAcosh),
        "DoubleAtanh" => Ok(DoubleAtanh),
        "DoublePower" => Ok(DoublePower),
        "Int2Double" => Ok(Int2Double),
        "Double2Int" => Ok(Double2Int),
        "Int2Float" => Ok(Int2Float),
        "Float2Int" => Ok(Float2Int),
        "Double2Float" => Ok(Double2Float),
        "Float2Double" => Ok(Float2Double),
        "ReallyUnsafePtrEquality" => Ok(ReallyUnsafePtrEquality),
        "IndexCharOffAddr" => Ok(IndexCharOffAddr),
        "PlusAddr" => Ok(PlusAddr),
        "Raise" => Ok(Raise),
        "NewByteArray" => Ok(NewByteArray),
        "ReadWord8Array" => Ok(ReadWord8Array),
        "WriteWord8Array" => Ok(WriteWord8Array),
        "SizeofMutableByteArray" => Ok(SizeofMutableByteArray),
        "UnsafeFreezeByteArray" => Ok(UnsafeFreezeByteArray),
        "CopyByteArray" => Ok(CopyByteArray),
        "CopyMutableByteArray" => Ok(CopyMutableByteArray),
        "CopyAddrToByteArray" => Ok(CopyAddrToByteArray),
        "ShrinkMutableByteArray" => Ok(ShrinkMutableByteArray),
        "ResizeMutableByteArray" => Ok(ResizeMutableByteArray),
        "Clz8" => Ok(Clz8),
        "IntToInt64" => Ok(IntToInt64),
        "Int64ToWord64" => Ok(Int64ToWord64),
        "TimesInt2Hi" => Ok(TimesInt2Hi),
        "TimesInt2Lo" => Ok(TimesInt2Lo),
        "TimesInt2Overflow" => Ok(TimesInt2Overflow),
        "IndexWord8Array" => Ok(IndexWord8Array),
        "IndexWord8OffAddr" => Ok(IndexWord8OffAddr),
        "CompareByteArrays" => Ok(CompareByteArrays),
        "WordToWord8" => Ok(WordToWord8),
        "Word64And" => Ok(Word64And),
        "Int64ToInt" => Ok(Int64ToInt),
        "Word64ToInt64" => Ok(Word64ToInt64),
        "Word8ToWord" => Ok(Word8ToWord),
        "Word8Lt" => Ok(Word8Lt),
        "Int64Ge" => Ok(Int64Ge),
        "Int64Negate" => Ok(Int64Negate),
        "Int64Shra" => Ok(Int64Shra),
        "Word64Shl" => Ok(Word64Shl),
        "Word8Ge" => Ok(Word8Ge),
        "Word8Sub" => Ok(Word8Sub),
        "SizeofByteArray" => Ok(SizeofByteArray),
        "IndexWordArray" => Ok(IndexWordArray),
        "Int64Add" => Ok(Int64Add),
        "Int64Gt" => Ok(Int64Gt),
        "Int64Lt" => Ok(Int64Lt),
        "Int64Le" => Ok(Int64Le),
        "Int64Sub" => Ok(Int64Sub),
        "Int64Mul" => Ok(Int64Mul),
        "Int64Shl" => Ok(Int64Shl),
        "WriteWordArray" => Ok(WriteWordArray),
        "ReadWordArray" => Ok(ReadWordArray),
        "SetByteArray" => Ok(SetByteArray),
        "Word64Or" => Ok(Word64Or),
        "Word8Add" => Ok(Word8Add),
        "Word8Le" => Ok(Word8Le),
        "AddIntCVal" => Ok(AddIntCVal),
        "AddIntCCarry" => Ok(AddIntCCarry),
        "SubWordCVal" => Ok(SubWordCVal),
        "SubWordCCarry" => Ok(SubWordCCarry),
        "AddWordCVal" => Ok(AddWordCVal),
        "AddWordCCarry" => Ok(AddWordCCarry),
        "TimesWord2Hi" => Ok(TimesWord2Hi),
        "TimesWord2Lo" => Ok(TimesWord2Lo),
        "QuotRemWordVal" => Ok(QuotRemWordVal),
        "QuotRemWordRem" => Ok(QuotRemWordRem),
        "FfiStrlen" => Ok(FfiStrlen),
        "FfiTextMeasureOff" => Ok(FfiTextMeasureOff),
        "FfiTextMemchr" => Ok(FfiTextMemchr),
        "FfiTextReverse" => Ok(FfiTextReverse),
        // SmallArray#
        "NewSmallArray" => Ok(NewSmallArray),
        "ReadSmallArray" => Ok(ReadSmallArray),
        "WriteSmallArray" => Ok(WriteSmallArray),
        "IndexSmallArray" => Ok(IndexSmallArray),
        "SizeofSmallArray" => Ok(SizeofSmallArray),
        "SizeofSmallMutableArray" => Ok(SizeofSmallMutableArray),
        "UnsafeFreezeSmallArray" => Ok(UnsafeFreezeSmallArray),
        "UnsafeThawSmallArray" => Ok(UnsafeThawSmallArray),
        "CopySmallArray" => Ok(CopySmallArray),
        "CopySmallMutableArray" => Ok(CopySmallMutableArray),
        "CloneSmallArray" => Ok(CloneSmallArray),
        "CloneSmallMutableArray" => Ok(CloneSmallMutableArray),
        "ShrinkSmallMutableArray" => Ok(ShrinkSmallMutableArray),
        // Array#
        "NewArray" => Ok(NewArray),
        "ReadArray" => Ok(ReadArray),
        "WriteArray" => Ok(WriteArray),
        "SizeofArray" => Ok(SizeofArray),
        "SizeofMutableArray" => Ok(SizeofMutableArray),
        "UnsafeFreezeArray" => Ok(UnsafeFreezeArray),
        "UnsafeThawArray" => Ok(UnsafeThawArray),
        "CopyArray" => Ok(CopyArray),
        "CopyMutableArray" => Ok(CopyMutableArray),
        "CloneArray" => Ok(CloneArray),
        "CloneMutableArray" => Ok(CloneMutableArray),
        // Bit operations
        "PopCnt" => Ok(PopCnt),
        "PopCnt8" => Ok(PopCnt8),
        "PopCnt16" => Ok(PopCnt16),
        "PopCnt32" => Ok(PopCnt32),
        "PopCnt64" => Ok(PopCnt64),
        "Ctz" => Ok(Ctz),
        "Ctz8" => Ok(Ctz8),
        "Ctz16" => Ok(Ctz16),
        "Ctz32" => Ok(Ctz32),
        "Ctz64" => Ok(Ctz64),
        "CasSmallArray" => Ok(CasSmallArray),
        "ShowDoubleAddr" => Ok(ShowDoubleAddr),
        _ => Err(ReadError::InvalidPrimOp(s.to_string())),
    }
}

fn as_u64(val: &Value) -> Result<u64, ReadError> {
    match val {
        Value::Integer(i) => {
            let u: u64 = (*i)
                .try_into()
                .map_err(|_| ReadError::InvalidStructure("Expected u64".to_string()))?;
            Ok(u)
        }
        _ => Err(ReadError::InvalidStructure("Expected integer".to_string())),
    }
}

fn as_i64(val: &Value) -> Result<i64, ReadError> {
    match val {
        Value::Integer(i) => {
            let i: i64 = (*i)
                .try_into()
                .map_err(|_| ReadError::InvalidStructure("Expected i64".to_string()))?;
            Ok(i)
        }
        _ => Err(ReadError::InvalidStructure("Expected integer".to_string())),
    }
}

fn as_usize(val: &Value) -> Result<usize, ReadError> {
    match val {
        Value::Integer(i) => {
            let u: u64 = (*i)
                .try_into()
                .map_err(|_| ReadError::InvalidStructure("Expected integer (u64)".to_string()))?;
            usize::try_from(u)
                .map_err(|_| ReadError::InvalidStructure("Integer too large for usize".to_string()))
        }
        _ => Err(ReadError::InvalidStructure("Expected integer".to_string())),
    }
}
