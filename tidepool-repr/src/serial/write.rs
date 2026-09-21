//! Serialization of Tidepool IR to CBOR.

use super::WriteError;
use ciborium::value::Value;

/// Write the 8-byte version header into a buffer.
fn write_header(buf: &mut Vec<u8>) {
    buf.extend_from_slice(&super::HEADER_MAGIC);
    buf.extend_from_slice(&super::VERSION_MAJOR.to_be_bytes());
    buf.extend_from_slice(&super::VERSION_MINOR.to_be_bytes());
}

/// Encode an `[(id, name)]` table as the `[[id, name], …]` CBOR array shared
/// by the `var_names` and `poisoned` warnings keys.
fn id_name_pairs_value(pairs: &[(u64, String)]) -> Value {
    Value::Array(
        pairs
            .iter()
            .map(|(id, nm)| {
                Value::Array(vec![Value::Integer((*id).into()), Value::Text(nm.clone())])
            })
            .collect(),
    )
}

/// Encode a slice of record field labels as a CBOR array of text.
fn field_labels_value(labels: &[String]) -> Value {
    Value::Array(labels.iter().map(|l| Value::Text(l.clone())).collect())
}

/// Encode a slice of rendered field types as a CBOR array of text. Same shape
/// as [`field_labels_value`]; kept distinct so a future divergence in either
/// encoding does not have to un-share a helper.
fn field_types_value(types: &[String]) -> Value {
    Value::Array(types.iter().map(|t| Value::Text(t.clone())).collect())
}

/// Writes a DataConTable to CBOR-encoded metadata bytes (new format with warnings).
pub fn write_metadata(
    table: &crate::datacon_table::DataConTable,
    warnings: &super::read::MetaWarnings,
) -> Result<Vec<u8>, WriteError> {
    use crate::datacon::SrcBang;

    // Ascending DataConId order — the order `encodeMetadata` emits — so the
    // encoding is deterministic and byte-comparable against Haskell output.
    let mut cons: Vec<_> = table.iter().collect();
    cons.sort_by_key(|dc| dc.id.0);

    let mut entries = Vec::with_capacity(table.len());
    for dc in cons {
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

        // Always the full 9-element shape (matching
        // `Tidepool.CborEncode.encodeMetaEntry`): an absent qualified name is
        // the empty string, absent field labels/types the empty array. The
        // parent type name (8th element) is always present; the field types
        // (9th element) are always present, empty for a nullary constructor.
        let entry = vec![
            Value::Integer(dcid.into()),
            Value::Text(name.clone()),
            Value::Integer(tag.into()),
            Value::Integer(arity.into()),
            bangs,
            Value::Text(dc.qualified_name.clone().unwrap_or_default()),
            field_labels_value(table.field_labels_of(dc.id).unwrap_or(&[])),
            Value::Text(dc.type_name.clone()),
            field_types_value(table.field_types_of(dc.id).unwrap_or(&[])),
        ];
        entries.push(Value::Array(entry));
    }

    // Warnings map mirrors `encodeMetadata`'s emission exactly (key order and
    // presence rules) so a read→re-encode of Haskell-produced meta is
    // byte-identical: `has_io` always; `captured_type` only when present;
    // `var_names`/`warnings`/`poisoned` only when non-empty.
    let mut warnings_pairs = vec![(
        Value::Text("has_io".to_string()),
        Value::Bool(warnings.has_io),
    )];
    if let Some(ty) = &warnings.captured_type {
        warnings_pairs.push((
            Value::Text("captured_type".to_string()),
            Value::Text(ty.clone()),
        ));
    }
    if !warnings.var_names.is_empty() {
        warnings_pairs.push((
            Value::Text("var_names".to_string()),
            id_name_pairs_value(&warnings.var_names),
        ));
    }
    if !warnings.warnings.is_empty() {
        warnings_pairs.push((
            Value::Text("warnings".to_string()),
            Value::Array(
                warnings
                    .warnings
                    .iter()
                    .map(|w| Value::Text(w.clone()))
                    .collect(),
            ),
        ));
    }
    // `poisoned` is emitted LAST, matching `encodeMetadata`'s key order (2.1).
    if !warnings.poisoned.is_empty() {
        warnings_pairs.push((
            Value::Text("poisoned".to_string()),
            id_name_pairs_value(&warnings.poisoned),
        ));
    }
    let root = Value::Array(vec![Value::Array(entries), Value::Map(warnings_pairs)]);

    let mut bytes = Vec::new();
    write_header(&mut bytes);
    ciborium::ser::into_writer(&root, &mut bytes)?;

    Ok(bytes)
}
