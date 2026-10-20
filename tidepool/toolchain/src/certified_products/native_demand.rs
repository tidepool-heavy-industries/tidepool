//! Semantic payloads of the in-flight, request-owned native demand exchange.
//! Transport correlation never grants Original membership or body permission.

use super::*;

pub(crate) struct NativeDemandPacket {
    originals: Vec<ReceiptImportOwner>,
}

impl NativeDemandPacket {
    pub(crate) fn originals(&self) -> &[ReceiptImportOwner] {
        &self.originals
    }

    pub(crate) fn decode(bytes: &[u8], operation: &InventoryOperation) -> CertResult<Self> {
        let limit = COMPILER_RECEIPT_BYTES_LIMIT.min(operation.limits().max_bytes);
        let value = operation.decode_value(bytes, limit)?;
        operation.charge_value_copies(&value, 2)?;
        let packet = sized(&value, 2)?;
        if string(&packet[0])? != "TPNATIVEDEMAND1" {
            return Err(CertificationError::Receipt("native demand version"));
        }
        let mut originals = Vec::new();
        for value in array(&packet[1])? {
            crate::host_work::checkpoint().map_err(CertificationError::Interrupted)?;
            let row = sized(value, 5)?;
            let original = ReceiptImportOwner::OriginalBinding {
                unit: string(&row[0])?.to_owned(),
                module: string(&row[1])?.to_owned(),
                interface: receipt_artifact_binding(&row[2])?,
                original: receipt_artifact_binding(&row[3])?,
                binder: identity(&row[4])?,
            };
            if originals
                .last()
                .is_some_and(|previous| previous >= &original)
            {
                return Err(CertificationError::Receipt(
                    "native demand originals are not unique and ordered",
                ));
            }
            operation.reserve::<ReceiptImportOwner>(1)?;
            originals.push(original);
        }
        Ok(Self { originals })
    }
}

/// Only semantic protocol categories. The request owner retains the original
/// typed CompileError; diagnostics and these tags never drive Rust selection.
#[derive(Clone, Copy, Debug)]
pub(crate) enum NativeDemandRefusalReason {
    InvalidWitness = 0,
    OriginalUnavailable = 1,
    NativeGroupUnavailable = 2,
    PackageProductUnavailable = 3,
    InputConflict = 4,
    AdmissionLimit = 5,
}

pub(crate) fn encode_reply(
    obligations: &[crate::declaration_context::NativePackageObligation],
    operation: &InventoryOperation,
) -> CertResult<Vec<u8>> {
    operation.reserve::<(&SymbolIdentity, [u8; 32])>(obligations.len())?;
    let roots = obligations
        .iter()
        .map(|obligation| (&obligation.identity, obligation.interface_digest))
        .collect::<BTreeSet<_>>();
    operation.reserve::<Value>(roots.len())?;
    let readiness = if roots.is_empty() { 0_u64 } else { 1_u64 };
    let mut rows = Vec::with_capacity(roots.len());
    for (identity, digest) in roots {
        charge_symbol(operation, identity)?;
        rows.push(value_array([
            value_identity(identity),
            Value::Bytes(digest.to_vec()),
        ]));
    }
    encode_value_with_operation(
        &value_array([
            value_text("TPNATIVEREPLY1"),
            Value::Integer(readiness.into()),
            value_array(rows),
        ]),
        CertificationFormat::ProductReceipt,
        COMPILER_RECEIPT_BYTES_LIMIT.min(operation.limits().max_bytes),
        operation,
    )
}

pub(crate) fn encode_refusal(
    reason: NativeDemandRefusalReason,
    diagnostic: &str,
    operation: &InventoryOperation,
) -> CertResult<Vec<u8>> {
    operation.charge(diagnostic.len())?;
    encode_value_with_operation(
        &value_array([
            value_text("TPNATIVEREFUSAL1"),
            Value::Integer((reason as u64).into()),
            value_text(diagnostic),
        ]),
        CertificationFormat::ProductReceipt,
        COMPILER_RECEIPT_BYTES_LIMIT.min(operation.limits().max_bytes),
        operation,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn packet(rows: Vec<Value>) -> Vec<u8> {
        let mut bytes = Vec::new();
        ciborium::ser::into_writer(
            &Value::Array(vec![
                Value::Text("TPNATIVEDEMAND1".into()),
                Value::Array(rows),
            ]),
            &mut bytes,
        )
        .unwrap();
        bytes
    }

    fn original(module: &str) -> Value {
        let binding = |artifact, selection| {
            Value::Array(vec![
                Value::Bytes(vec![artifact; 32]),
                Value::Bytes(vec![selection; 32]),
            ])
        };
        Value::Array(vec![
            Value::Text("unit".into()),
            Value::Text(module.into()),
            binding(1, 2),
            binding(3, 4),
            Value::Array(vec![
                Value::Text("unit".into()),
                Value::Text(module.into()),
                Value::Text("value".into()),
                Value::Text("entry".into()),
                Value::Null,
            ]),
        ])
    }

    #[test]
    fn native_demand_semantics_preserve_exact_pairs_and_refuse_unordered_or_malformed_uses() {
        let operation = InventoryOperation::new(Default::default());
        let decoded =
            NativeDemandPacket::decode(&packet(vec![original("A"), original("B")]), &operation)
                .unwrap();
        assert_eq!(decoded.originals().len(), 2);
        let ReceiptImportOwner::OriginalBinding {
            interface,
            original,
            binder,
            ..
        } = &decoded.originals()[0]
        else {
            unreachable!()
        };
        assert_eq!(interface.artifact.0, [1; 32]);
        assert_eq!(interface.selection.0, [2; 32]);
        assert_eq!(original.artifact.0, [3; 32]);
        assert_eq!(original.selection.0, [4; 32]);
        assert_eq!(binder.module, "A");
        for rows in [
            vec![self::original("B"), self::original("A")],
            vec![self::original("A"), self::original("A")],
        ] {
            assert!(matches!(
                NativeDemandPacket::decode(&packet(rows), &operation),
                Err(CertificationError::Receipt(
                    "native demand originals are not unique and ordered"
                ))
            ));
        }
        let mut malformed = self::original("A");
        let Value::Array(row) = &mut malformed else {
            unreachable!()
        };
        let Value::Array(pair) = &mut row[2] else {
            unreachable!()
        };
        pair[0] = Value::Bytes(vec![1; 31]);
        assert!(NativeDemandPacket::decode(&packet(vec![malformed]), &operation).is_err());
        let mut trailing = packet(vec![]);
        trailing.push(0);
        assert!(NativeDemandPacket::decode(&trailing, &operation).is_err());
    }
}
