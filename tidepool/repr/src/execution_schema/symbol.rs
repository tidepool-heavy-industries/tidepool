//! The shared wire grammar for a complete nominal symbol.

use ciborium::value::Value;
use super::{ParseError, SymbolIdentity};

pub(crate) fn encode(value: &SymbolIdentity) -> Value {
    Value::Array(vec![
        Value::Text(value.unit.clone()),
        Value::Text(value.module.clone()),
        Value::Text(value.namespace.clone()),
        Value::Text(value.occurrence.clone()),
        Value::Array(match &value.record_parent {
            None => vec![Value::Integer(0.into())],
            Some(parent) => vec![Value::Integer(1.into()), Value::Text(parent.clone())],
        }),
    ])
}

/// The caller accounts for decoded text in its owning operation budget.
pub(crate) fn decode(
    value: &Value,
    mut text: impl FnMut(&Value, &str) -> Result<String, ParseError>,
) -> Result<SymbolIdentity, ParseError> {
    let Value::Array(fields) = value else {
        return Err(ParseError::Malformed("symbol must be a five-element array".into()));
    };
    if fields.len() != 5 {
        return Err(ParseError::Malformed("symbol must have five fields".into()));
    }
    let record_parent = match &fields[4] {
        Value::Array(parent) => match parent.as_slice() {
            [Value::Integer(tag)] if u64::try_from(*tag).ok() == Some(0) => None,
            [Value::Integer(tag), parent] if u64::try_from(*tag).ok() == Some(1) =>
                Some(text(parent, "record parent")?),
            _ => return Err(ParseError::Malformed("invalid record parent".into())),
        },
        _ => return Err(ParseError::Malformed("invalid record parent".into())),
    };
    Ok(SymbolIdentity {
        unit: text(&fields[0], "symbol unit")?,
        module: text(&fields[1], "symbol module")?,
        namespace: text(&fields[2], "symbol namespace")?,
        occurrence: text(&fields[3], "symbol occurrence")?,
        record_parent,
    })
}
