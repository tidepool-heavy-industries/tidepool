//! Semantic payloads of the in-flight, request-owned native demand exchange.
//! Transport correlation never grants Original membership or body permission.

use super::*;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum DependencyQueryKind {
    SourceDefinition,
    NativeOriginal,
}
impl DependencyQueryKind {
    pub(crate) fn tag(self) -> u64 {
        match self {
            Self::SourceDefinition => 0,
            Self::NativeOriginal => 1,
        }
    }
}

pub(crate) struct NativeDemandPacket {
    kind: DependencyQueryKind,
    originals: Vec<ReceiptImportOwner>,
}

impl NativeDemandPacket {
    pub(crate) fn kind(&self) -> DependencyQueryKind {
        self.kind
    }
    pub(crate) fn originals(&self) -> &[ReceiptImportOwner] {
        &self.originals
    }

    pub(crate) fn into_originals(self) -> Vec<ReceiptImportOwner> {
        self.originals
    }

    pub(crate) fn without_known(
        &self,
        known: impl Fn(&ReceiptImportOwner) -> bool,
        operation: &InventoryOperation,
    ) -> CertResult<Self> {
        operation.reserve::<ReceiptImportOwner>(self.originals.len())?;
        let mut originals = Vec::new();
        for original in &self.originals {
            if known(original) {
                continue;
            }
            let ReceiptImportOwner::OriginalBinding {
                unit,
                module,
                binder,
                ..
            } = original
            else {
                return Err(CertificationError::Receipt("native demand reference kind"));
            };
            operation.charge(unit.len().saturating_add(module.len()))?;
            charge_symbol(operation, binder)?;
            originals.push(original.clone());
        }
        Ok(Self {
            kind: self.kind,
            originals,
        })
    }

    pub(crate) fn decode(bytes: &[u8], operation: &InventoryOperation) -> CertResult<Self> {
        let limit = COMPILER_RECEIPT_BYTES_LIMIT.min(operation.limits().max_bytes);
        let value = operation.decode_value(bytes, limit)?;
        operation.charge_value_copies(&value, 2)?;
        let packet = sized(&value, 3)?;
        if string(&packet[0])? != "TPDEPENDENCYQUERY1" {
            return Err(CertificationError::Receipt("native demand version"));
        }
        let kind = match number(&packet[1])? {
            0 => DependencyQueryKind::SourceDefinition,
            1 => DependencyQueryKind::NativeOriginal,
            _ => return Err(CertificationError::Receipt("dependency query kind")),
        };
        let mut originals = Vec::new();
        for value in array(&packet[2])? {
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
        Ok(Self { kind, originals })
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

pub(crate) struct NativeDemandUseReply<'a> {
    pub(crate) original: &'a ReceiptImportOwner,
    pub(crate) obligations: &'a [&'a crate::declaration_context::NativePackageObligation],
    pub(crate) retained: &'a [crate::declaration_context::NativeRetainedOccurrence<'a>],
}

pub(crate) fn encode_reply(
    kind: DependencyQueryKind,
    replies: &[NativeDemandUseReply<'_>],
    operation: &InventoryOperation,
) -> CertResult<Vec<u8>> {
    operation.reserve::<Value>(replies.len())?;
    let mut entries = Vec::with_capacity(replies.len());
    let mut previous = None;
    for reply in replies {
        crate::host_work::checkpoint().map_err(CertificationError::Interrupted)?;
        if previous.is_some_and(|previous| previous >= reply.original) {
            return Err(CertificationError::Receipt(
                "native reply use order or duplicate",
            ));
        }
        previous = Some(reply.original);
        let ReceiptImportOwner::OriginalBinding {
            unit,
            module,
            interface,
            original,
            binder,
        } = reply.original
        else {
            return Err(CertificationError::Receipt("native reply use kind"));
        };
        operation.reserve::<Value>(16)?;
        operation.charge(unit.len().saturating_add(module.len()).saturating_add(128))?;
        charge_symbol(operation, binder)?;
        let binding = |binding: &crate::artifact_inventory::ArtifactBinding| {
            value_array([
                Value::Bytes(binding.artifact.0.to_vec()),
                Value::Bytes(binding.selection.0.to_vec()),
            ])
        };
        let reference = value_array([
            value_text(unit),
            value_text(module),
            binding(interface),
            binding(original),
            value_identity(binder),
        ]);
        if kind == DependencyQueryKind::SourceDefinition && !reply.obligations.is_empty() {
            return Err(CertificationError::Receipt(
                "source dependency query acquired packages",
            ));
        }
        entries.push(value_array([
            reference,
            encode_obligations(reply.obligations, operation)?,
            encode_retained(reply.retained, operation)?,
        ]));
    }
    encode_value_with_operation(
        &value_array([
            value_text("TPDEPENDENCYREPLY1"),
            Value::Integer(kind.tag().into()),
            value_array(entries),
        ]),
        CertificationFormat::ProductReceipt,
        COMPILER_RECEIPT_BYTES_LIMIT.min(operation.limits().max_bytes),
        operation,
    )
}

fn encode_retained(
    occurrences: &[crate::declaration_context::NativeRetainedOccurrence<'_>],
    operation: &InventoryOperation,
) -> CertResult<Value> {
    operation.reserve::<Value>(occurrences.len())?;
    let mut rows = Vec::with_capacity(occurrences.len());
    let mut previous = None;
    for occurrence in occurrences {
        if previous.is_some_and(|prior| prior >= occurrence) {
            return Err(CertificationError::Receipt(
                "dependency terminal order or duplicate",
            ));
        }
        previous = Some(occurrence);
        operation.reserve::<Value>(12)?;
        operation.charge(128)?;
        charge_symbol(operation, occurrence.identity)?;
        rows.push(value_array([
            value_array([
                Value::Bytes(occurrence.demander.binding.artifact().0.to_vec()),
                Value::Bytes(occurrence.demander.binding.selection().0.to_vec()),
                Value::Integer(occurrence.demander.original_ordinal.into()),
            ]),
            Value::Integer(occurrence.global_index.into()),
            value_array([
                Value::Bytes(occurrence.binding.artifact.0.to_vec()),
                Value::Bytes(occurrence.binding.selection.0.to_vec()),
            ]),
            value_identity(occurrence.identity),
            Value::Integer(occurrence.generation.into()),
        ]));
    }
    Ok(value_array(rows))
}

fn encode_obligations(
    obligations: &[&crate::declaration_context::NativePackageObligation],
    operation: &InventoryOperation,
) -> CertResult<Value> {
    operation.reserve::<(&SymbolIdentity, [u8; 32])>(obligations.len())?;
    let roots = obligations
        .iter()
        .map(|obligation| (&obligation.identity, obligation.interface_digest))
        .collect::<BTreeSet<_>>();
    operation.reserve::<Value>(roots.len())?;
    let mut rows = Vec::with_capacity(roots.len());
    let mut previous = None;
    for (identity, digest) in roots {
        if previous == Some(identity) {
            return Err(CertificationError::Mismatch(
                "native package obligation seal conflict",
            ));
        }
        previous = Some(identity);
        charge_symbol(operation, identity)?;
        rows.push(value_array([
            value_identity(identity),
            Value::Bytes(digest.to_vec()),
        ]));
    }
    Ok(value_array(rows))
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
                Value::Text("TPDEPENDENCYQUERY1".into()),
                Value::Integer(1u64.into()),
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

    #[test]
    fn native_reply_entries_preserve_exact_use_pairs_and_refuse_duplicate_entries() {
        let operation = InventoryOperation::new(Default::default());
        let originals = vec![original("A"), original("B")];
        let decoded = NativeDemandPacket::decode(&packet(originals.clone()), &operation).unwrap();
        let demander = serde_json::from_value(serde_json::json!({
            "binding": {"artifact": vec![1u8;32], "selection": vec![2u8;32]},
            "original_ordinal": 7,
        }))
        .unwrap();
        let package = SymbolIdentity {
            unit: "pkg".into(),
            module: "Pkg".into(),
            namespace: "value".into(),
            occurrence: "root".into(),
            record_parent: None,
        };
        let obligations = [crate::declaration_context::NativePackageObligation {
            demander,
            global_index: 0,
            identity: package.clone(),
            interface_digest: [8; 32],
        }];
        let borrowed = obligations.iter().collect::<Vec<_>>();
        let replies = decoded
            .originals()
            .iter()
            .enumerate()
            .map(|(index, original)| NativeDemandUseReply {
                original,
                obligations: if index == 0 { &borrowed } else { &[] },
                retained: &[],
            })
            .collect::<Vec<_>>();
        let bytes =
            encode_reply(DependencyQueryKind::NativeOriginal, &replies, &operation).unwrap();
        let value: Value = ciborium::de::from_reader(bytes.as_slice()).unwrap();
        assert_eq!(
            value,
            Value::Array(vec![
                Value::Text("TPDEPENDENCYREPLY1".into()),
                Value::Integer(1u64.into()),
                Value::Array(
                    originals
                        .into_iter()
                        .enumerate()
                        .map(|(index, original)| Value::Array(vec![
                            original,
                            if index == 0 {
                                value_array([value_array([
                                    value_identity(&package),
                                    Value::Bytes(vec![8; 32]),
                                ])])
                            } else {
                                value_array([])
                            },
                            value_array([]),
                        ]))
                        .collect()
                )
            ])
        );
        let duplicate = vec![
            NativeDemandUseReply {
                original: replies[0].original,
                obligations: &[],
                retained: &[],
            },
            NativeDemandUseReply {
                original: replies[0].original,
                obligations: &[],
                retained: &[],
            },
        ];
        assert!(matches!(
            encode_reply(DependencyQueryKind::NativeOriginal, &duplicate, &operation),
            Err(CertificationError::Receipt(
                "native reply use order or duplicate"
            ))
        ));
        let reversed = vec![
            NativeDemandUseReply {
                original: replies[1].original,
                obligations: &[],
                retained: &[],
            },
            NativeDemandUseReply {
                original: replies[0].original,
                obligations: &[],
                retained: &[],
            },
        ];
        assert!(encode_reply(DependencyQueryKind::NativeOriginal, &reversed, &operation).is_err());
        let mut conflicting = obligations[0].clone();
        conflicting.interface_digest = [9; 32];
        let conflicting = [obligations[0].clone(), conflicting];
        assert!(matches!(
            encode_reply(
                DependencyQueryKind::NativeOriginal,
                &[NativeDemandUseReply {
                    original: replies[0].original,
                    obligations: &conflicting.iter().collect::<Vec<_>>(),
                    retained: &[],
                }],
                &operation
            ),
            Err(CertificationError::Mismatch(
                "native package obligation seal conflict"
            ))
        ));
    }

    #[test]
    fn dependency_reply_preserves_retained_selection_and_occurrence_slots() {
        use crate::artifact_inventory::{ArtifactBinding, ArtifactId, ArtifactSelectionId};
        use crate::declaration_context::NativeRetainedOccurrence;
        let operation = InventoryOperation::new(Default::default());
        let packet = NativeDemandPacket::decode(&packet(vec![original("A")]), &operation).unwrap();
        let demander = serde_json::from_value(serde_json::json!({
            "binding": {"artifact": vec![3u8;32], "selection": vec![4u8;32]},
            "original_ordinal": 7,
        }))
        .unwrap();
        let identity = SymbolIdentity {
            unit: "unit".into(),
            module: "Val.G1".into(),
            namespace: "value".into(),
            occurrence: "captured".into(),
            record_parent: None,
        };
        let first = ArtifactBinding {
            artifact: ArtifactId([9; 32]),
            selection: ArtifactSelectionId([10; 32]),
        };
        let second = ArtifactBinding {
            selection: ArtifactSelectionId([11; 32]),
            ..first
        };
        let retained = [
            NativeRetainedOccurrence {
                demander,
                global_index: 0,
                binding: &first,
                identity: &identity,
                generation: 1,
            },
            NativeRetainedOccurrence {
                demander,
                global_index: 1,
                binding: &second,
                identity: &identity,
                generation: 1,
            },
        ];
        let reply = |retained| {
            encode_reply(
                DependencyQueryKind::SourceDefinition,
                &[NativeDemandUseReply {
                    original: &packet.originals()[0],
                    obligations: &[],
                    retained,
                }],
                &operation,
            )
        };
        let encoded = reply(&retained).unwrap();
        let value: Value = ciborium::de::from_reader(encoded.as_slice()).unwrap();
        let rows = array(&array(&value).unwrap()[2]).unwrap();
        let terminals = array(&array(&rows[0]).unwrap()[2]).unwrap();
        assert_eq!(terminals.len(), 2);
        assert_eq!(
            array(&terminals[0]).unwrap()[1],
            Value::Integer(0u64.into())
        );
        assert_eq!(
            array(&terminals[1]).unwrap()[1],
            Value::Integer(1u64.into())
        );
        assert_ne!(
            array(&terminals[0]).unwrap()[2],
            array(&terminals[1]).unwrap()[2]
        );
        let duplicate = [retained[0], retained[0]];
        let reordered = [retained[1], retained[0]];
        assert!(reply(&duplicate).is_err());
        assert!(reply(&reordered).is_err());
    }

    #[test]
    fn native_demand_progress_filters_exact_uses_without_collapsing_new_binders_or_pairs() {
        let operation = InventoryOperation::new(Default::default());
        let first = original("A");
        let mut second = first.clone();
        let Value::Array(row) = &mut second else {
            unreachable!()
        };
        let Value::Array(binder) = &mut row[4] else {
            unreachable!()
        };
        binder[3] = Value::Text("second".into());
        let decoded = NativeDemandPacket::decode(
            &packet(vec![first.clone(), second, original("B")]),
            &operation,
        )
        .unwrap();
        let known = &decoded.originals()[0];
        let fresh = decoded
            .without_known(|reference| reference == known, &operation)
            .unwrap();
        assert_eq!(fresh.originals(), &decoded.originals()[1..]);
        assert!(decoded
            .without_known(|_| true, &operation)
            .unwrap()
            .originals()
            .is_empty());

        let mut changed = first;
        let Value::Array(row) = &mut changed else {
            unreachable!()
        };
        let Value::Array(pair) = &mut row[3] else {
            unreachable!()
        };
        pair[1] = Value::Bytes(vec![5; 32]);
        let changed = NativeDemandPacket::decode(&packet(vec![changed]), &operation).unwrap();
        assert_eq!(
            changed
                .without_known(|reference| reference == known, &operation)
                .unwrap()
                .originals(),
            changed.originals()
        );
    }
}
