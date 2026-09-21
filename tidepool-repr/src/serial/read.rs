//! Deserialization of constructor metadata from CBOR.

use super::ReadError;
use ciborium::value::Value;

/// Strip and validate the mandatory 8-byte version header. Returns the CBOR
/// payload slice. Input without the `TPLR` magic is rejected loudly — there is
/// ONE current format; regenerate stale payloads instead of tolerating them.
fn strip_header(bytes: &[u8]) -> Result<&[u8], ReadError> {
    if bytes.len() < 4 || bytes[..4] != super::HEADER_MAGIC {
        return Err(ReadError::MissingHeader);
    }
    if bytes.len() < super::HEADER_LEN {
        return Err(ReadError::TruncatedHeader);
    }
    let major = u16::from_be_bytes([bytes[4], bytes[5]]);
    let minor = u16::from_be_bytes([bytes[6], bytes[7]]);
    if major != super::VERSION_MAJOR || minor > super::VERSION_MINOR {
        return Err(ReadError::UnsupportedVersion(major, minor));
    }
    Ok(&bytes[super::HEADER_LEN..])
}

/// Structured warnings from the Haskell extractor, encoded in meta.cbor.
#[derive(Debug, Default, Clone)]
pub struct MetaWarnings {
    /// Whether the extracted code contains IO operations.
    pub has_io: bool,
    /// varId → human name for ids that can surface as runtime "unresolved
    /// variable" errors (0x45-poisoned unresolved externals + dangling refs,
    /// session values included). The JIT registers these so the error names
    /// the symbol instead of a bare hex (friction #12). Empty from older
    /// extractors (the key is optional).
    pub var_names: Vec<(u64, String)>,
    /// The GHC-inferred type of the eval's top-level expression (the `__user`
    /// binding), rendered to a string by `ppr` on the Haskell side. `None` when
    /// the extraction had no `__user` binding (fixture/Suite builds) or used an
    /// older extractor that didn't emit the key.
    ///
    /// NB: `ppr` is not parser-faithful — fine for v1 display / simple synthetic
    /// decls, but cross-turn typechecking of references may eventually need a
    /// structured type rather than this string (see GhcPipeline.PipelineResult).
    pub captured_type: Option<String>,
    /// GHC warnings emitted while compiling the target module (e.g.
    /// `-Wincomplete-patterns`, name shadowing), rendered by GHC's own
    /// diagnostic pretty-printer (`Expr.hs:<line>:<col>: warning: ...`).
    /// Warnings from dependency modules (the preamble, stdlib) are excluded —
    /// only diagnostics whose source span is the target file survive (see
    /// `GhcPipeline.warnCollectorHook`). Empty on a clean compile or from an
    /// older extractor that didn't emit the key.
    pub warnings: Vec<String>,
    /// Sentinel slot → qualified name of the unresolved external that
    /// `Translate.hs` replaced with a `0x45`-kind-4 poison node
    /// (`VarId::sentinel().slot`). This is how the emitted program stays
    /// SELF-DESCRIBING: the node carries the slot, this table carries the
    /// identity, and the JIT names the symbol in its trap instead of
    /// reporting a bare kind=4. Empty from a 2.0 payload (the key is
    /// optional) and from any extraction that poisoned nothing.
    pub poisoned: Vec<(u64, String)>,
}

/// Reads a DataConTable and warnings from CBOR-encoded metadata bytes (meta.cbor format).
///
/// The one accepted shape: 2-element array `[entries_array, warnings_map]`,
/// every entry a 9-element array (id, name, tag, arity, bangs, qualified-name,
/// field-labels, parent-type-name, field-types) — the shape
/// `Tidepool.CborEncode.encodeMetadata` emits.
pub fn read_metadata(bytes: &[u8]) -> Result<(crate::DataConTable, MetaWarnings), ReadError> {
    use crate::datacon::{DataCon, SrcBang};
    use crate::datacon_table::DataConTable;
    use crate::types::DataConId;

    let bytes = strip_header(bytes)?;
    let val: Value = ciborium::de::from_reader(bytes)?;

    let root = match val {
        Value::Array(a) => a,
        _ => {
            return Err(ReadError::InvalidStructure(
                "Metadata must be a CBOR array".to_string(),
            ))
        }
    };

    let (entries, warnings) = match root.as_slice() {
        [Value::Array(entries), warnings_map @ Value::Map(_)] => {
            (entries.clone(), parse_warnings(warnings_map)?)
        }
        _ => {
            return Err(ReadError::InvalidStructure(
                "Metadata root must be [entries_array, warnings_map]".to_string(),
            ))
        }
    };

    let mut table = DataConTable::new();
    for entry in &entries {
        let arr = match entry {
            Value::Array(a) if a.len() == 9 => a,
            _ => {
                return Err(ReadError::InvalidStructure(
                    "Metadata entry must be an array of exactly 9".to_string(),
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
        let tag = u32::try_from(as_u64(&arr[2])?)
            .map_err(|_| ReadError::InvalidStructure("DataCon tag exceeds u32".to_string()))?;
        let arity = u32::try_from(as_u64(&arr[3])?)
            .map_err(|_| ReadError::InvalidStructure("DataCon arity exceeds u32".to_string()))?;
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

        // 6th element: module-qualified name; an empty string encodes `None`,
        // a non-empty string is `Some`. Any non-text value is malformed.
        let qualified_name = match &arr[5] {
            Value::Text(t) if t.is_empty() => None,
            Value::Text(t) => Some(t.clone()),
            _ => {
                return Err(ReadError::MalformedMetadataField {
                    field: "qualified_name",
                    detail: "expected text (empty string encodes None)".to_string(),
                })
            }
        };

        // 7th element: record field labels, in field order (empty array when
        // none). Must be an array; a non-array value is malformed.
        let field_labels: Vec<String> = match &arr[6] {
            Value::Array(labels) => labels
                .iter()
                .map(|l| match l {
                    Value::Text(t) => Ok(t.clone()),
                    _ => Err(ReadError::MalformedMetadataField {
                        field: "field_labels",
                        detail: "each label must be text".to_string(),
                    }),
                })
                .collect::<Result<Vec<_>, ReadError>>()?,
            _ => {
                return Err(ReadError::MalformedMetadataField {
                    field: "field_labels",
                    detail: "expected an array".to_string(),
                })
            }
        };

        // 8th element: rendered name of the constructor's parent TyCon (e.g.
        // "Verdict") — always present, every DataCon has a parent type.
        let type_name = match &arr[7] {
            Value::Text(t) => t.clone(),
            _ => {
                return Err(ReadError::InvalidStructure(
                    "DataCon parent type name must be text".to_string(),
                ))
            }
        };

        // 9th element: rendered field types, in field order (empty array for
        // a nullary constructor). Must be an array; a non-array value is
        // malformed.
        let field_types: Vec<String> = match &arr[8] {
            Value::Array(types) => types
                .iter()
                .map(|t| match t {
                    Value::Text(s) => Ok(s.clone()),
                    _ => Err(ReadError::MalformedMetadataField {
                        field: "field_types",
                        detail: "each type must be text".to_string(),
                    }),
                })
                .collect::<Result<Vec<_>, ReadError>>()?,
            _ => {
                return Err(ReadError::MalformedMetadataField {
                    field: "field_types",
                    detail: "expected an array".to_string(),
                })
            }
        };

        let id = DataConId(dcid);
        table.insert_checked(DataCon {
            id,
            name,
            tag,
            rep_arity: arity,
            field_bangs: bangs,
            qualified_name,
            type_name,
        })?;
        table.set_field_labels(id, field_labels);
        table.set_field_types(id, field_types);
    }

    Ok((table, warnings))
}

/// Decode an `[[id, name], …]` warnings-map value — the shape shared by the
/// `var_names` and `poisoned` keys. `field` names the key in any error.
fn parse_id_name_pairs(field: &'static str, val: &Value) -> Result<Vec<(u64, String)>, ReadError> {
    let items = match val {
        Value::Array(items) => items,
        _ => {
            return Err(ReadError::MalformedMetadataField {
                field,
                detail: "expected an array".to_string(),
            })
        }
    };
    let mut out = Vec::with_capacity(items.len());
    for item in items {
        let kv = match item {
            Value::Array(kv) if kv.len() == 2 => kv,
            _ => {
                return Err(ReadError::MalformedMetadataField {
                    field,
                    detail: "each item must be a 2-element [id, name] array".to_string(),
                })
            }
        };
        let id = match &kv[0] {
            Value::Integer(id) => {
                u64::try_from(*id).map_err(|_| ReadError::MalformedMetadataField {
                    field,
                    detail: "id does not fit in u64".to_string(),
                })?
            }
            _ => {
                return Err(ReadError::MalformedMetadataField {
                    field,
                    detail: "id must be an integer".to_string(),
                })
            }
        };
        let name = match &kv[1] {
            Value::Text(nm) => nm.clone(),
            _ => {
                return Err(ReadError::MalformedMetadataField {
                    field,
                    detail: "name must be text".to_string(),
                })
            }
        };
        out.push((id, name));
    }
    Ok(out)
}

/// Parses the metadata warnings map. Every key a conforming writer emits
/// (`has_io`, `captured_type`, `var_names`, `warnings`, `poisoned`) must carry
/// a value of the exact shape that key's field expects; a non-text map key, a
/// duplicate key, or a key this reader does not recognize are each a decode
/// error, not a silent skip.
fn parse_warnings(val: &Value) -> Result<MetaWarnings, ReadError> {
    let mut warnings = MetaWarnings::default();
    let pairs = match val {
        Value::Map(pairs) => pairs,
        _ => {
            return Err(ReadError::MalformedMetadataField {
                field: "warnings_map",
                detail: "expected a CBOR map".to_string(),
            })
        }
    };

    let mut seen = std::collections::HashSet::new();
    for (k, v) in pairs {
        let key = match k {
            Value::Text(key) => key,
            _ => {
                return Err(ReadError::MalformedMetadataField {
                    field: "warnings_map",
                    detail: "map key must be text".to_string(),
                })
            }
        };
        if !seen.insert(key.as_str()) {
            return Err(ReadError::DuplicateMetadataKey(key.clone()));
        }

        match key.as_str() {
            "has_io" => match v {
                Value::Bool(b) => warnings.has_io = *b,
                _ => {
                    return Err(ReadError::MalformedMetadataField {
                        field: "has_io",
                        detail: "expected a bool".to_string(),
                    })
                }
            },
            "captured_type" => match v {
                Value::Text(t) => warnings.captured_type = Some(t.clone()),
                _ => {
                    return Err(ReadError::MalformedMetadataField {
                        field: "captured_type",
                        detail: "expected text".to_string(),
                    })
                }
            },
            "var_names" => warnings.var_names = parse_id_name_pairs("var_names", v)?,
            "poisoned" => warnings.poisoned = parse_id_name_pairs("poisoned", v)?,
            "warnings" => {
                let items = match v {
                    Value::Array(items) => items,
                    _ => {
                        return Err(ReadError::MalformedMetadataField {
                            field: "warnings",
                            detail: "expected an array".to_string(),
                        })
                    }
                };
                for item in items {
                    match item {
                        Value::Text(t) => warnings.warnings.push(t.clone()),
                        _ => {
                            return Err(ReadError::MalformedMetadataField {
                                field: "warnings",
                                detail: "each item must be text".to_string(),
                            })
                        }
                    }
                }
            }
            _ => return Err(ReadError::UnknownMetadataKey(key.clone())),
        }
    }
    Ok(warnings)
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
