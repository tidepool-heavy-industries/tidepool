//! Typed suspension observations carried inline by the worker's TurnOut.
//! Ordinary module compilations separately carry the same facts as JSON.

use crate::{CompileError, NominalHead, SiteType, YieldSite};
use ciborium::value::Value as CborValue;

fn cbor_shape_error(what: &str, expected: &str, got: &CborValue) -> CompileError {
    CompileError::ExtractFailed(format!(
        "TurnOut CBOR: expected {expected} for {what}, got {got:?}"
    ))
}

fn cbor_expect_array<'a>(v: &'a CborValue, what: &str) -> Result<&'a [CborValue], CompileError> {
    match v {
        CborValue::Array(a) => Ok(a),
        other => Err(cbor_shape_error(what, "array", other)),
    }
}

fn cbor_expect_array_len<'a>(
    v: &'a CborValue,
    n: usize,
    what: &str,
) -> Result<&'a [CborValue], CompileError> {
    let a = cbor_expect_array(v, what)?;
    if a.len() != n {
        return Err(CompileError::ExtractFailed(format!(
            "TurnOut CBOR: expected {what} array of length {n}, got {}",
            a.len()
        )));
    }
    Ok(a)
}

fn cbor_expect_text<'a>(v: &'a CborValue, what: &str) -> Result<&'a str, CompileError> {
    match v {
        CborValue::Text(t) => Ok(t.as_str()),
        other => Err(cbor_shape_error(what, "text", other)),
    }
}

fn cbor_as_u64(v: &CborValue, what: &str) -> Result<u64, CompileError> {
    match v {
        CborValue::Integer(i) => u64::try_from(*i).map_err(|_| cbor_shape_error(what, "u64", v)),
        other => Err(cbor_shape_error(what, "integer", other)),
    }
}

fn decode_string_array(v: &CborValue, what: &str) -> Result<Vec<String>, CompileError> {
    cbor_expect_array(v, what)?
        .iter()
        .map(|s| cbor_expect_text(s, what).map(str::to_string))
        .collect()
}

fn decode_yield_site(v: &CborValue) -> Result<YieldSite, CompileError> {
    let arr = cbor_expect_array(v, "typed suspension site")?;
    if arr.len() != 10 {
        return Err(CompileError::ExtractFailed(format!(
            "TurnOut CBOR: expected typed suspension site with 10 fields, got {}",
            arr.len()
        )));
    }
    let reply_declaration = match &arr[7] {
        CborValue::Null => None,
        value => Some(cbor_expect_text(value, "reply declaration")?.to_owned()),
    };
    let site = cbor_as_u64(&arr[0], "Ask site")?;
    let origin = cbor_expect_text(&arr[1], "Ask origin")?.to_string();
    let ordinal = cbor_as_u64(&arr[2], "Ask ordinal")?;
    let answer_type = cbor_expect_text(&arr[3], "Ask answer type")?.to_string();
    let modules = decode_string_array(&arr[4], "Ask modules")?;
    let heads = decode_turn_nominal_heads(&arr[5], "Ask nominal heads")?;
    let inputs = cbor_expect_array(&arr[6], "site input types")?
        .iter()
        .map(|input| {
            let input = cbor_expect_array_len(input, 3, "site input type")?;
            Ok(SiteType {
                ty: cbor_expect_text(&input[0], "site input type name")?.to_string(),
                modules: decode_string_array(&input[1], "site input type modules")?,
                heads: decode_turn_nominal_heads(&input[2], "site input nominal heads")?,
            })
        })
        .collect::<Result<Vec<_>, CompileError>>()?;
    let values = cbor_expect_array(&arr[8], "canonical input type witnesses")?;
    if values.len() != inputs.len() {
        return Err(CompileError::ExtractFailed(
            "typed site input witness arity".into(),
        ));
    }
    let input_type_witnesses = values
        .iter()
        .map(|value| match value {
            CborValue::Null => Ok(None),
            CborValue::Bytes(bytes) => {
                crate::checked_cell::CanonicalInputTypeWitness::from_bytes(bytes).map(Some)
            }
            _ => Err(cbor_shape_error(
                "canonical input type witness",
                "bytes or null",
                value,
            )),
        })
        .collect::<Result<Vec<_>, CompileError>>()?;
    let request_type_signatures = match &arr[9] {
        CborValue::Null => None,
        CborValue::Bytes(bytes) => Some(crate::checked_cell::RequestTypeSignatures::from_bytes(
            bytes,
        )?),
        value => {
            return Err(cbor_shape_error(
                "request type signatures",
                "bytes or null",
                value,
            ))
        }
    };
    Ok(YieldSite {
        reply_declaration,
        site,
        origin,
        ordinal,
        ty: answer_type,
        modules,
        heads,
        inputs,
        input_type_witnesses,
        request_type_signatures,
    })
}

pub fn decode_turn_nominal_heads(
    value: &CborValue,
    what: &str,
) -> Result<Vec<NominalHead>, CompileError> {
    cbor_expect_array(value, what)?
        .iter()
        .map(|head| {
            let head = cbor_expect_array_len(head, 3, "nominal type head")?;
            Ok(NominalHead {
                unit: cbor_expect_text(&head[0], "nominal type unit")?.to_string(),
                module: cbor_expect_text(&head[1], "nominal type module")?.to_string(),
                name: cbor_expect_text(&head[2], "nominal type name")?.to_string(),
            })
        })
        .collect()
}

pub fn decode_turn_yield_sites(v: &CborValue) -> Result<Vec<YieldSite>, CompileError> {
    cbor_expect_array(v, "asks")?
        .iter()
        .map(decode_yield_site)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn request_site_native_metadata_round_trips_json_and_cbor_and_seals_full_payload() {
        let text = |value: &str| CborValue::Text(value.into());
        let payload = |native: u8| {
            let mut bytes = Vec::new();
            // Structural codec fixture only; no GHC payload executes here.
            ciborium::into_writer(
                &CborValue::Array(vec![
                    text("TPREQUESTTYPESIGNATURES1"),
                    text("1"),
                    CborValue::Array(vec![
                        text("TPCHECKEDSIGNATURE2"),
                        text("request-reply"),
                        text("Report"),
                        CborValue::Bytes(vec![native]),
                        CborValue::Array(vec![]),
                    ]),
                    CborValue::Null,
                ]),
                &mut bytes,
            )
            .unwrap();
            bytes
        };
        let site = |bytes: Vec<u8>| {
            CborValue::Array(vec![
                CborValue::Integer(7.into()),
                text("Owner.request"),
                CborValue::Integer(0.into()),
                text("Report"),
                CborValue::Array(vec![]),
                CborValue::Array(vec![]),
                CborValue::Array(vec![]),
                CborValue::Null,
                CborValue::Array(vec![]),
                CborValue::Bytes(bytes),
            ])
        };
        let bytes = payload(1);
        let original = decode_yield_site(&site(bytes.clone())).unwrap();
        let hex = bytes
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>();
        let json = serde_json::json!({
            "site": 7, "origin": "Owner.request", "ordinal": 0, "type": "Report", "modules": [], "heads": [], "inputs": [],
            "input_type_witnesses": [], "reply_declaration": null, "request_type_signatures": hex,
        });
        assert_eq!(
            serde_json::from_value::<YieldSite>(json.clone()).unwrap(),
            original
        );
        let substituted = decode_yield_site(&site(payload(2))).unwrap();
        assert_eq!(original.ty, substituted.ty);
        assert!(!original.same_metadata(&substituted));
        assert!(crate::artifacts::yield_sites_metadata_digest(&[original, substituted]).is_err());
        let mut absent = json.clone();
        absent
            .as_object_mut()
            .unwrap()
            .remove("request_type_signatures");
        assert!(serde_json::from_value::<YieldSite>(absent).is_err());
        let mut nonrequest = json;
        nonrequest["request_type_signatures"] = serde_json::Value::Null;
        assert!(serde_json::from_value::<YieldSite>(nonrequest)
            .unwrap()
            .request_type_signatures
            .is_none());
        let mut malformed = site(payload(1));
        malformed.as_array_mut().unwrap()[9] = text("untrusted printed type");
        assert!(decode_yield_site(&malformed).is_err());
    }

    #[test]
    fn inline_turn_sites_preserve_nominal_owners_and_live_inputs() {
        let head = || {
            CborValue::Array(vec![
                CborValue::Text("owner-unit".into()),
                CborValue::Text("Owner".into()),
                CborValue::Text("Report".into()),
            ])
        };
        let site = CborValue::Array(vec![
            CborValue::Integer(7.into()),
            CborValue::Text("Owner.request".into()),
            CborValue::Integer(2.into()),
            CborValue::Text("Report".into()),
            CborValue::Array(vec![CborValue::Text("Owner".into())]),
            CborValue::Array(vec![head()]),
            CborValue::Array(vec![CborValue::Array(vec![
                CborValue::Text("Report".into()),
                CborValue::Array(vec![]),
                CborValue::Array(vec![head()]),
            ])]),
            CborValue::Text("data Report = Report Int".into()),
            CborValue::Array(vec![CborValue::Null]),
            CborValue::Null,
        ]);
        let sites = decode_turn_yield_sites(&CborValue::Array(vec![site.clone()])).unwrap();
        assert_eq!(sites[0].input_type_witnesses, vec![None]);
        assert!(sites[0].request_type_signatures.is_none());
        let CborValue::Array(mut fields) = site else {
            unreachable!()
        };
        for legacy_fields in [7, 8, 9] {
            assert!(
                decode_turn_yield_sites(&CborValue::Array(vec![CborValue::Array(
                    fields[..legacy_fields].to_vec()
                ),]))
                .is_err()
            );
        }
        let current =
            decode_turn_yield_sites(&CborValue::Array(vec![CborValue::Array(fields.clone())]))
                .unwrap();
        assert_eq!(current[0].input_type_witnesses, vec![None]);
        fields[8] = CborValue::Array(vec![]);
        assert!(
            decode_turn_yield_sites(&CborValue::Array(vec![CborValue::Array(fields)])).is_err()
        );
        assert_eq!(sites.len(), 1);
        assert_eq!(sites[0].site, 7);
        assert_eq!(sites[0].heads[0].unit, "owner-unit");
        assert_eq!(sites[0].inputs[0].heads, sites[0].heads);
        assert_eq!(
            sites[0].reply_declaration.as_deref(),
            Some("data Report = Report Int")
        );
        assert!(decode_turn_yield_sites(&CborValue::Array(vec![]))
            .unwrap()
            .is_empty());
    }

    #[test]
    fn inline_turn_sites_reject_malformed_rows_and_negative_ids() {
        assert!(decode_turn_yield_sites(&CborValue::Null).is_err());
        assert!(
            decode_turn_yield_sites(&CborValue::Array(vec![CborValue::Array(vec![])])).is_err()
        );
        let negative = CborValue::Array(vec![
            CborValue::Integer((-1).into()),
            CborValue::Text("Owner.request".into()),
            CborValue::Integer(0.into()),
            CborValue::Text("Int".into()),
            CborValue::Array(vec![]),
            CborValue::Array(vec![]),
            CborValue::Array(vec![]),
            CborValue::Null,
            CborValue::Array(vec![]),
            CborValue::Null,
        ]);
        assert!(decode_turn_yield_sites(&CborValue::Array(vec![negative])).is_err());
    }
}
