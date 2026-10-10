//! The shared wire grammar for a complete nominal symbol.

use super::{ParseError, SymbolIdentity};
use ciborium::value::Value;

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
        return Err(ParseError::Malformed(
            "symbol must be a five-element array".into(),
        ));
    };
    if fields.len() != 5 {
        return Err(ParseError::Malformed("symbol must have five fields".into()));
    }
    let Value::Array(parent) = &fields[4] else {
        return Err(ParseError::Malformed(
            "record parent must be an array".into(),
        ));
    };
    let Some(Value::Integer(tag)) = parent.first() else {
        return Err(ParseError::Malformed(
            "record parent tag must be unsigned integer".into(),
        ));
    };
    let tag = u64::try_from(*tag)
        .map_err(|_| ParseError::Malformed("record parent tag must be unsigned integer".into()))?;
    let record_parent = match (tag, parent.as_slice()) {
        (0, [_]) => None,
        (1, [_, value]) => Some(text(value, "record parent")?),
        (0 | 1, _) => return Err(ParseError::Malformed("invalid record parent".into())),
        (tag, _) => return Err(ParseError::InvalidTag(tag)),
    };
    Ok(SymbolIdentity {
        unit: text(&fields[0], "symbol unit")?,
        module: text(&fields[1], "symbol module")?,
        namespace: text(&fields[2], "symbol namespace")?,
        occurrence: text(&fields[3], "symbol occurrence")?,
        record_parent,
    })
}
