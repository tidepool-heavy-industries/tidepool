//! The shared wire grammar for a complete nominal symbol.

use super::{ConstructorIdentity, ParseError, SymbolIdentity, TypeIdentity, TypeVersion};
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


pub(crate) fn encode_type(value: &TypeIdentity) -> Value {
    Value::Array(vec![encode(&value.symbol), match &value.version {
        TypeVersion::WiredIn => Value::Array(vec![Value::Integer(0.into())]),
        TypeVersion::GhcAbi(bytes) => Value::Array(vec![Value::Integer(1.into()), Value::Bytes(bytes.to_vec())]),
    }])
}

pub(crate) fn encode_constructor(value: &ConstructorIdentity) -> Value {
    Value::Array(vec![encode(&value.symbol), encode_type(&value.family)])
}

pub(crate) fn decode_type(value: &Value, mut text: impl FnMut(&Value, &str) -> Result<String, ParseError>) -> Result<TypeIdentity, ParseError> {
    let Value::Array(fields) = value else { return Err(ParseError::Malformed("type identity must be an array".into())); };
    let [symbol, Value::Array(version)] = fields.as_slice() else { return Err(ParseError::Malformed("type identity must have symbol and version".into())); };
    let version = match version.as_slice() {
        [Value::Integer(tag)] if *tag == 0.into() => TypeVersion::WiredIn,
        [Value::Integer(tag), Value::Bytes(bytes)] if *tag == 1.into() => TypeVersion::GhcAbi(bytes.as_slice().try_into().map_err(|_| ParseError::Malformed("GHC ABI must be exactly 16 bytes".into()))?),
        _ => return Err(ParseError::Malformed("invalid type version".into())),
    };
    Ok(TypeIdentity { symbol: decode(symbol, &mut text)?, version })
}

pub(crate) fn decode_constructor(value: &Value, mut text: impl FnMut(&Value, &str) -> Result<String, ParseError>) -> Result<ConstructorIdentity, ParseError> {
    let Value::Array(fields) = value else { return Err(ParseError::Malformed("constructor identity must be an array".into())); };
    let [symbol, family] = fields.as_slice() else { return Err(ParseError::Malformed("constructor identity must have symbol and family".into())); };
    Ok(ConstructorIdentity { symbol: decode(symbol, &mut text)?, family: decode_type(family, &mut text)? })
}
