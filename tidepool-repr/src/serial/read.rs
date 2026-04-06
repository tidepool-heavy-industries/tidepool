//! Deserialization of Tidepool IR from CBOR.

use super::ReadError;
use crate::frame::CoreFrame;
use crate::tree::RecursiveTree;
use crate::types::{Alt, AltCon, DataConId, JoinId, Literal, PrimOpKind, VarId};
use ciborium::value::Value;

/// Strip and validate the 8-byte version header. Returns the CBOR payload slice.
/// For backward compatibility, if the first 4 bytes are NOT the magic, assume
/// legacy headerless format and return the entire slice.
fn strip_header(bytes: &[u8]) -> Result<&[u8], ReadError> {
    if bytes.len() >= 4 && bytes[..4] == super::HEADER_MAGIC {
        if bytes.len() < super::HEADER_LEN {
            return Err(ReadError::TruncatedHeader);
        }
        let major = u16::from_be_bytes([bytes[4], bytes[5]]);
        let minor = u16::from_be_bytes([bytes[6], bytes[7]]);
        if major != super::VERSION_MAJOR || minor > super::VERSION_MINOR {
            return Err(ReadError::UnsupportedVersion(major, minor));
        }
        Ok(&bytes[super::HEADER_LEN..])
    } else {
        // Legacy headerless CBOR — pass through
        Ok(bytes)
    }
}

/// Reads a CoreExpr from a CBOR-encoded byte slice.
pub fn read_cbor(bytes: &[u8]) -> Result<RecursiveTree<CoreFrame<usize>>, ReadError> {
    let bytes = strip_header(bytes)?;
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

    let nodes = nodes_array
        .iter()
        .map(decode_frame)
        .collect::<Result<Vec<_>, _>>()?;

    validate_indices(&nodes)?;

    Ok(RecursiveTree { nodes })
}

/// Structured warnings from the Haskell extractor, encoded in meta.cbor.
#[derive(Debug, Default, Clone)]
pub struct MetaWarnings {
    /// Whether the extracted code contains IO operations.
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

    let bytes = strip_header(bytes)?;
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
                    _ => {
                        return Err(ReadError::InvalidStructure(
                            "expected Array for entries".to_string(),
                        ))
                    }
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
            Value::Array(a) if a.len() == 5 || a.len() == 6 => a,
            _ => {
                return Err(ReadError::InvalidStructure(
                    "Metadata entry must be array of 5 or 6".to_string(),
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
        let bangs = bangs_arr
            .iter()
            .map(|b| {
                let bang_str = match b {
                    Value::Text(t) => t.as_str(),
                    _ => return Err(ReadError::InvalidStructure("Bang must be text".to_string())),
                };
                Ok(match bang_str {
                    "SrcBang" => SrcBang::SrcBang,
                    "SrcUnpack" => SrcBang::SrcUnpack,
                    "NoSrcBang" => SrcBang::NoSrcBang,
                    _ => {
                        return Err(ReadError::InvalidStructure(format!(
                            "Unknown bang: {}",
                            bang_str
                        )))
                    }
                })
            })
            .collect::<Result<Vec<_>, ReadError>>()?;

        // 6th element (optional): module-qualified name
        let qualified_name = if arr.len() >= 6 {
            match &arr[5] {
                Value::Text(t) => Some(t.clone()),
                _ => None,
            }
        } else {
            None
        };

        table.insert(DataCon {
            id: DataConId(dcid),
            name,
            tag,
            rep_arity: arity,
            field_bangs: bangs,
            qualified_name,
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
    let arr = expect_array(val)?;

    if arr.is_empty() {
        return Err(ReadError::InvalidStructure("Empty frame array".to_string()));
    }

    let tag = expect_text(&arr[0])?;

    match tag {
        "Var" => {
            expect_array_len(val, 2)?;
            Ok(CoreFrame::Var(VarId(as_u64(&arr[1])?)))
        }
        "Lit" => {
            expect_array_len(val, 2)?;
            Ok(CoreFrame::Lit(decode_literal(&arr[1])?))
        }
        "App" => {
            expect_array_len(val, 3)?;
            Ok(CoreFrame::App {
                fun: as_usize(&arr[1])?,
                arg: as_usize(&arr[2])?,
            })
        }
        "Lam" => {
            expect_array_len(val, 3)?;
            Ok(CoreFrame::Lam {
                binder: VarId(as_u64(&arr[1])?),
                body: as_usize(&arr[2])?,
            })
        }
        "LetNonRec" => {
            expect_array_len(val, 4)?;
            Ok(CoreFrame::LetNonRec {
                binder: VarId(as_u64(&arr[1])?),
                rhs: as_usize(&arr[2])?,
                body: as_usize(&arr[3])?,
            })
        }
        "LetRec" => {
            expect_array_len(val, 3)?;
            let bindings_arr = expect_array(&arr[1])?;
            let bindings = bindings_arr
                .iter()
                .map(|b_val| {
                    let b_arr = expect_array_len(b_val, 2)?;
                    Ok((VarId(as_u64(&b_arr[0])?), as_usize(&b_arr[1])?))
                })
                .collect::<Result<Vec<_>, ReadError>>()?;
            Ok(CoreFrame::LetRec {
                bindings,
                body: as_usize(&arr[2])?,
            })
        }
        "Case" => {
            expect_array_len(val, 4)?;
            let alts_arr = expect_array(&arr[3])?;
            let alts = alts_arr
                .iter()
                .map(decode_alt)
                .collect::<Result<Vec<_>, _>>()?;
            Ok(CoreFrame::Case {
                scrutinee: as_usize(&arr[1])?,
                binder: VarId(as_u64(&arr[2])?),
                alts,
            })
        }
        "Con" => {
            expect_array_len(val, 3)?;
            let fields_arr = expect_array(&arr[2])?;
            let fields = fields_arr
                .iter()
                .map(as_usize)
                .collect::<Result<Vec<_>, _>>()?;
            Ok(CoreFrame::Con {
                tag: DataConId(as_u64(&arr[1])?),
                fields,
            })
        }
        "Join" => {
            expect_array_len(val, 5)?;
            let params_arr = expect_array(&arr[2])?;
            let params = params_arr
                .iter()
                .map(|p| Ok(VarId(as_u64(p)?)))
                .collect::<Result<Vec<_>, ReadError>>()?;
            Ok(CoreFrame::Join {
                label: JoinId(as_u64(&arr[1])?),
                params,
                rhs: as_usize(&arr[3])?,
                body: as_usize(&arr[4])?,
            })
        }
        "Jump" => {
            expect_array_len(val, 3)?;
            let args_arr = expect_array(&arr[2])?;
            let args = args_arr
                .iter()
                .map(as_usize)
                .collect::<Result<Vec<_>, _>>()?;
            Ok(CoreFrame::Jump {
                label: JoinId(as_u64(&arr[1])?),
                args,
            })
        }
        "PrimOp" => {
            expect_array_len(val, 3)?;
            let op_name = expect_text(&arr[1])?;
            let op = decode_primop(op_name)?;
            let args_arr = expect_array(&arr[2])?;
            let args = args_arr
                .iter()
                .map(as_usize)
                .collect::<Result<Vec<_>, _>>()?;
            Ok(CoreFrame::PrimOp { op, args })
        }
        _ => Err(ReadError::InvalidTag(tag.to_string())),
    }
}

fn decode_literal(val: &Value) -> Result<Literal, ReadError> {
    let arr = expect_array_len(val, 2)?;
    let tag = expect_text(&arr[0])?;
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
    let arr = expect_array_len(val, 3)?;
    let con = decode_alt_con(&arr[0])?;
    let binders_arr = expect_array(&arr[1])?;
    let binders = binders_arr
        .iter()
        .map(|b| Ok(VarId(as_u64(b)?)))
        .collect::<Result<Vec<_>, ReadError>>()?;
    let body = as_usize(&arr[2])?;
    Ok(Alt { con, binders, body })
}

fn decode_alt_con(val: &Value) -> Result<AltCon, ReadError> {
    let arr = expect_array(val)?;
    if arr.is_empty() {
        return Err(ReadError::InvalidAltCon("Empty AltCon array".to_string()));
    }
    let tag = expect_text(&arr[0])?;
    match tag {
        "DataAlt" => {
            expect_array_len(val, 2)?;
            Ok(AltCon::DataAlt(DataConId(as_u64(&arr[1])?)))
        }
        "LitAlt" => {
            expect_array_len(val, 2)?;
            Ok(AltCon::LitAlt(decode_literal(&arr[1])?))
        }
        "Default" => {
            expect_array_len(val, 1)?;
            Ok(AltCon::Default)
        }
        _ => Err(ReadError::InvalidAltCon(tag.to_string())),
    }
}

fn decode_primop(s: &str) -> Result<PrimOpKind, ReadError> {
    PrimOpKind::from_serial_name(s).ok_or_else(|| ReadError::InvalidPrimOp(s.to_string()))
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

fn expect_array(val: &Value) -> Result<&Vec<Value>, ReadError> {
    match val {
        Value::Array(a) => Ok(a),
        _ => Err(ReadError::InvalidStructure("expected array".to_string())),
    }
}

fn expect_array_len(val: &Value, n: usize) -> Result<&Vec<Value>, ReadError> {
    match val {
        Value::Array(a) if a.len() == n => Ok(a),
        Value::Array(a) => Err(ReadError::InvalidStructure(format!(
            "expected array of length {}, got {}",
            n,
            a.len()
        ))),
        _ => Err(ReadError::InvalidStructure("expected array".to_string())),
    }
}

fn expect_text(val: &Value) -> Result<&str, ReadError> {
    match val {
        Value::Text(t) => Ok(t.as_str()),
        _ => Err(ReadError::InvalidStructure("expected text".to_string())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_strip_header_valid() {
        let mut bytes = Vec::new();
        bytes.extend_from_slice(&super::super::HEADER_MAGIC);
        bytes.extend_from_slice(&super::super::VERSION_MAJOR.to_be_bytes());
        bytes.extend_from_slice(&super::super::VERSION_MINOR.to_be_bytes());
        bytes.push(0x80); // empty array in CBOR
        let stripped = strip_header(&bytes).expect("should succeed");
        assert_eq!(stripped, &[0x80]);
    }

    #[test]
    fn test_strip_header_legacy() {
        let bytes = [0x80];
        let stripped = strip_header(&bytes).expect("should succeed");
        assert_eq!(stripped, &[0x80]);
    }

    #[test]
    fn test_strip_header_truncated() {
        let mut bytes = Vec::new();
        bytes.extend_from_slice(&super::super::HEADER_MAGIC);
        bytes.push(0x00);
        let err = strip_header(&bytes).expect_err("should fail");
        assert!(matches!(err, ReadError::TruncatedHeader));
    }

    #[test]
    fn test_strip_header_unsupported_major() {
        let mut bytes = Vec::new();
        bytes.extend_from_slice(&super::super::HEADER_MAGIC);
        bytes.extend_from_slice(&2u16.to_be_bytes());
        bytes.extend_from_slice(&0u16.to_be_bytes());
        let err = strip_header(&bytes).expect_err("should fail");
        assert!(matches!(err, ReadError::UnsupportedVersion(2, 0)));
    }

    #[test]
    fn test_strip_header_unsupported_minor() {
        let mut bytes = Vec::new();
        bytes.extend_from_slice(&super::super::HEADER_MAGIC);
        bytes.extend_from_slice(&super::super::VERSION_MAJOR.to_be_bytes());
        bytes.extend_from_slice(&(super::super::VERSION_MINOR + 1).to_be_bytes());
        let err = strip_header(&bytes).expect_err("should fail");
        assert!(matches!(
            err,
            ReadError::UnsupportedVersion(super::super::VERSION_MAJOR, m) if m == super::super::VERSION_MINOR + 1
        ));
    }
}
