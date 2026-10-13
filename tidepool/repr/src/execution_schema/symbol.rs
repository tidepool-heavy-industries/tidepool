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
    Value::Array(vec![
        encode(&value.symbol),
        match &value.version {
            TypeVersion::WiredIn => Value::Array(vec![Value::Integer(0.into())]),
            TypeVersion::GhcAbi(bytes) => {
                Value::Array(vec![Value::Integer(1.into()), Value::Bytes(bytes.to_vec())])
            }
        },
    ])
}

pub(crate) fn encode_constructor(value: &ConstructorIdentity) -> Value {
    Value::Array(vec![encode(&value.symbol), encode_type(&value.family)])
}

pub(crate) fn decode_type(
    value: &Value,
    mut text: impl FnMut(&Value, &str) -> Result<String, ParseError>,
) -> Result<TypeIdentity, ParseError> {
    let Value::Array(fields) = value else {
        return Err(ParseError::Malformed(
            "type identity must be an array".into(),
        ));
    };
    let [symbol, Value::Array(version)] = fields.as_slice() else {
        return Err(ParseError::Malformed(
            "type identity must have symbol and version".into(),
        ));
    };
    let version = match version.as_slice() {
        [Value::Integer(tag)] if *tag == 0.into() => TypeVersion::WiredIn,
        [Value::Integer(tag), Value::Bytes(bytes)] if *tag == 1.into() => TypeVersion::GhcAbi(
            bytes
                .as_slice()
                .try_into()
                .map_err(|_| ParseError::Malformed("GHC ABI must be exactly 16 bytes".into()))?,
        ),
        _ => return Err(ParseError::Malformed("invalid type version".into())),
    };
    Ok(TypeIdentity {
        symbol: decode(symbol, &mut text)?,
        version,
    })
}

pub(crate) fn decode_constructor(
    value: &Value,
    mut text: impl FnMut(&Value, &str) -> Result<String, ParseError>,
) -> Result<ConstructorIdentity, ParseError> {
    let Value::Array(fields) = value else {
        return Err(ParseError::Malformed(
            "constructor identity must be an array".into(),
        ));
    };
    let [symbol, family] = fields.as_slice() else {
        return Err(ParseError::Malformed(
            "constructor identity must have symbol and family".into(),
        ));
    };
    Ok(ConstructorIdentity {
        symbol: decode(symbol, &mut text)?,
        family: decode_type(family, &mut text)?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    fn sample(version: TypeVersion) -> ConstructorIdentity {
        let symbol = |namespace: &str, occurrence: &str| SymbolIdentity {
            unit: "u".into(),
            module: "M".into(),
            namespace: namespace.into(),
            occurrence: occurrence.into(),
            record_parent: None,
        };
        ConstructorIdentity {
            symbol: symbol("constructor", "C"),
            family: TypeIdentity {
                symbol: symbol("type", "T"),
                version,
            },
        }
    }
    fn text(value: &Value, _: &str) -> Result<String, ParseError> {
        match value {
            Value::Text(value) => Ok(value.clone()),
            _ => Err(ParseError::Malformed("expected text".into())),
        }
    }

    #[test]
    fn canonical_constructor_bytes_match_haskell_control() {
        // Independently authored CBOR control also asserted by the Haskell encoder test.
        let mut expected = b"\x82\x85\x61u\x61M\x6bconstructor\x61C\x81\x00\x82\x85\x61u\x61M\x64type\x61T\x81\x00\x82\x01\x50".to_vec();
        expected.extend(0_u8..16);
        let identity = sample(TypeVersion::GhcAbi(std::array::from_fn(|i| i as u8)));
        assert_eq!(
            super::super::canonical_constructor_identity_bytes(&identity),
            expected
        );
        let value: Value = ciborium::de::from_reader(expected.as_slice()).unwrap();
        assert_eq!(decode_constructor(&value, text).unwrap(), identity);
    }

    #[test]
    fn type_versions_refuse_missing_unknown_and_malformed_evidence() {
        let good = encode_type(&sample(TypeVersion::WiredIn).family);
        let Value::Array(fields) = good else {
            unreachable!()
        };
        let symbol = fields[0].clone();
        let bad_versions = [
            Value::Null,
            Value::Array(vec![]),
            Value::Array(vec![Value::Integer(2.into())]),
            Value::Array(vec![Value::Integer(0.into()), Value::Null]),
            Value::Array(vec![Value::Integer(1.into())]),
            Value::Array(vec![Value::Integer(1.into()), Value::Bytes(vec![0; 15])]),
            Value::Array(vec![Value::Integer(1.into()), Value::Bytes(vec![0; 17])]),
            Value::Array(vec![
                Value::Integer(1.into()),
                Value::Text("00000000000000000000000000000000".into()),
            ]),
            Value::Array(vec![
                Value::Integer(1.into()),
                Value::Bytes(vec![0; 16]),
                Value::Null,
            ]),
        ];
        assert!(decode_type(&Value::Array(vec![symbol.clone()]), text).is_err());
        for version in bad_versions {
            assert!(decode_type(&Value::Array(vec![symbol.clone(), version]), text).is_err());
        }
        assert!(decode_type(
            &Value::Array(vec![symbol.clone(), fields[1].clone(), Value::Null]),
            text
        )
        .is_err());
        let constructor = sample(TypeVersion::WiredIn);
        assert_eq!(
            decode_constructor(&encode_constructor(&constructor), text).unwrap(),
            constructor
        );
        let mut fields = match encode_constructor(&constructor) {
            Value::Array(fields) => fields,
            _ => unreachable!(),
        };
        fields.push(Value::Null);
        assert!(decode_constructor(&Value::Array(fields), text).is_err());
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(64))]
        #[test]
        fn complete_identity_round_trips_and_family_revisions_distinguish(
            abi in any::<[u8; 16]>(), occurrence in "[A-Za-z][A-Za-z0-9]{0,20}",
            parent in proptest::option::of("[A-Za-z]{1,20}"),
        ) {
            let mut first = sample(TypeVersion::GhcAbi(abi));
            first.symbol.occurrence = occurrence;
            first.symbol.record_parent = parent;
            let value = encode_constructor(&first);
            prop_assert_eq!(decode_constructor(&value, text).unwrap(), first.clone());
            let mut next = first.clone();
            let TypeVersion::GhcAbi(bytes) = &mut next.family.version else { unreachable!() };
            bytes[15] ^= 1;
            prop_assert_eq!(&first.symbol, &next.symbol);
            prop_assert_ne!(&first, &next);
            prop_assert_ne!(super::super::canonical_constructor_identity_bytes(&first), super::super::canonical_constructor_identity_bytes(&next));
        }
    }
}
