//! The ordinary disk record references a complete immutable dependency proof.
//! Resolution restores the full proof before any existing candidate validation.

use super::*;
use std::io::Cursor;

#[derive(Debug, Clone)]
pub(crate) struct SharedEvidence(Arc<SharedProof>);

#[derive(Debug, Clone)]
struct SharedProof {
    evidence: DependencyEvidence,
    reference: std::sync::OnceLock<Option<EvidenceRef>>,
}

impl From<DependencyEvidence> for SharedEvidence {
    fn from(evidence: DependencyEvidence) -> Self {
        Self(Arc::new(SharedProof {
            evidence,
            reference: std::sync::OnceLock::new(),
        }))
    }
}

impl std::ops::Deref for SharedEvidence {
    type Target = DependencyEvidence;
    fn deref(&self) -> &Self::Target {
        &self.0.evidence
    }
}

impl Serialize for SharedEvidence {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        self.0.evidence.serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for SharedEvidence {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        DependencyEvidence::deserialize(deserializer).map(Self::from)
    }
}

impl SharedEvidence {
    pub(super) fn reference(&self) -> Option<&EvidenceRef> {
        self.0
            .reference
            .get_or_init(|| encode_evidence(self).map(|(reference, _)| reference))
            .as_ref()
    }
}

#[cfg(test)]
impl SharedEvidence {
    pub(crate) fn make_mut(&mut self) -> &mut DependencyEvidence {
        let proof = Arc::make_mut(&mut self.0);
        proof.reference.take();
        &mut proof.evidence
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct EvidenceRef {
    sha256: String,
    encoded_len: u64,
}

pub(super) fn encode_evidence(evidence: &DependencyEvidence) -> Option<(EvidenceRef, Vec<u8>)> {
    let mut bytes = Vec::new();
    ciborium::ser::into_writer(evidence, &mut bytes).ok()?;
    if bytes.len() > RECORD_LIMIT {
        return None;
    }
    Some((
        EvidenceRef {
            sha256: sha(&bytes),
            encoded_len: bytes.len() as u64,
        },
        bytes,
    ))
}

fn evidence_path(root: &Path, digest: &str) -> PathBuf {
    root.join(format!("evidence-{digest}.cbor"))
}

pub(super) fn publish(root: &Path, evidence: &DependencyEvidence) -> Option<()> {
    let (reference, bytes) = encode_evidence(evidence)?;
    fs::create_dir_all(root).ok()?;
    tidepool_atomic_write::write_best_effort(&evidence_path(root, &reference.sha256), &bytes).ok()
}

pub(super) fn encode_record(record: &Record) -> Option<Vec<u8>> {
    // The resolved dependency proof must authenticate the retained wire reference.
    if record.evidence.reference()? != &record.data.evidence
        || record.module_interface.is_none()
        || record.original_certification.is_empty()
    {
        return None;
    }
    let mut bytes = Vec::new();
    ciborium::ser::into_writer(&record.data, &mut bytes).ok()?;
    Some(bytes)
}

#[derive(Default)]
pub(super) struct ReadBudget {
    bytes: u64,
    pub(super) exhausted: bool,
    pub(super) evidence_bytes: u64,
    evidence: BTreeMap<String, (u64, SharedEvidence)>,
}

impl ReadBudget {
    pub(super) fn charge(&mut self, bytes: u64) -> Option<()> {
        let Some(total) = self
            .bytes
            .checked_add(bytes)
            .filter(|total| *total <= PAYLOAD_LIMIT as u64)
        else {
            self.exhausted = true;
            return None;
        };
        self.bytes = total;
        Some(())
    }

    pub(super) fn evidence_count(&self) -> usize {
        self.evidence.len()
    }

    fn resolve(&mut self, root: &Path, reference: &EvidenceRef) -> Option<SharedEvidence> {
        if reference.sha256.len() != 64
            || !reference
                .sha256
                .bytes()
                .all(|byte| byte.is_ascii_hexdigit())
            || reference.encoded_len > RECORD_LIMIT as u64
        {
            return None;
        }
        if let Some((len, evidence)) = self.evidence.get(&reference.sha256) {
            return (*len == reference.encoded_len).then(|| evidence.clone());
        }
        let mut file = fs::File::open(evidence_path(root, &reference.sha256))
            .inspect_err(|_error| {
                #[cfg(test)]
                eprintln!("candidate record dependency proof open refused: {_error}");
            })
            .ok()?;
        if file.metadata().ok()?.len() != reference.encoded_len {
            return None;
        }
        self.charge(reference.encoded_len)?;
        let mut bytes = Vec::new();
        (&mut file)
            .take(reference.encoded_len + 1)
            .read_to_end(&mut bytes)
            .ok()?;
        if bytes.len() as u64 != reference.encoded_len || sha(&bytes) != reference.sha256 {
            return None;
        }
        let mut cursor = Cursor::new(bytes.as_slice());
        let evidence: SharedEvidence = ciborium::de::from_reader(&mut cursor)
            .inspect_err(|_error| {
                #[cfg(test)]
                eprintln!("candidate record dependency proof decode refused: {_error}");
            })
            .ok()?;
        if cursor.position() != reference.encoded_len {
            return None;
        }
        self.evidence_bytes += reference.encoded_len;
        self.evidence.insert(
            reference.sha256.clone(),
            (reference.encoded_len, evidence.clone()),
        );
        Some(evidence)
    }
}

pub(super) fn decode_record(
    payload: &[u8],
    header: &RecordHeader,
    root: &Path,
    budget: &mut ReadBudget,
) -> Option<Record> {
    if payload.len() > RECORD_LIMIT || payload.len() as u64 != header.payload_len {
        return None;
    }
    let mut cursor = Cursor::new(payload);
    let data: RecordData = ciborium::de::from_reader(&mut cursor)
        .inspect_err(|_error| {
            #[cfg(test)]
            eprintln!("candidate record body decode refused: {_error}");
        })
        .ok()?;
    if cursor.position() != payload.len() as u64
        || data.tag != "TPMCAN"
        || data.version != RECORD_VERSION
        || data.module_interface.is_none()
        || data.original_certification.is_empty()
        || RecordHeader::for_record(&data, payload) != *header
    {
        return None;
    }
    let evidence = budget.resolve(root, &data.evidence)?;
    Some(Record {
        data,
        evidence,
        module_interface_proof: None,
        execution_source: None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture() -> (tempfile::TempDir, Record, Vec<u8>) {
        let root = tempfile::tempdir().unwrap();
        let record = super::super::tests::candidate_fixture(root.path(), "Library");
        publish(root.path(), &record.evidence).unwrap();
        let payload = encode_record(&record).unwrap();
        (root, record, payload)
    }

    fn decode(
        payload: &[u8],
        record: &Record,
        root: &Path,
        budget: &mut ReadBudget,
    ) -> Option<Record> {
        budget.charge(payload.len() as u64)?;
        decode_record(
            payload,
            &RecordHeader::for_record(record, payload),
            root,
            budget,
        )
    }

    #[test]
    fn records_share_full_proof_and_preserve_original_identity() {
        let (root, record, payload) = fixture();
        let mut budget = ReadBudget::default();
        let first = decode(&payload, &record, root.path(), &mut budget).unwrap();
        let second = decode(&payload, &record, root.path(), &mut budget).unwrap();
        assert!(Arc::ptr_eq(&first.evidence.0, &second.evidence.0));
        assert_eq!(budget.evidence_count(), 1);
        let (_, proof) = encode_evidence(&record.evidence).unwrap();
        assert_eq!(budget.bytes, 2 * payload.len() as u64 + proof.len() as u64);
        assert_eq!(
            serde_json::to_vec(&first.evidence).unwrap(),
            serde_json::to_vec(&record.evidence).unwrap()
        );
        assert_eq!(version_hash(&first), version_hash(&record));
        assert_eq!(first.products, record.products);
        assert_eq!(first.interface, record.interface);
        assert_eq!(first.package_imports, record.package_imports);
        assert_eq!(first.original_certification, record.original_certification);
        assert!(record.module_interface.is_some());
        assert_eq!(first.module_interface, record.module_interface);
        assert_eq!(second.module_interface, record.module_interface);
        assert!(first.module_interface_proof.is_none());
        // A descriptor survives serialization, but durable proof still requires
        // the canonical recovery owner to authenticate its certificate and Core.
        let recovered = crate::recovery_artifacts::recover_module_interface(
            root.path()
                .join(RECORD_DIR)
                .join(sha(b"endpoint"))
                .as_path(),
            first.module_interface.as_ref().unwrap(),
            &mut crate::recovery_artifacts::PackageInterfaceValidation::default(),
        )
        .unwrap();
        let original = record.module_interface_proof.as_ref().unwrap();
        assert_eq!(recovered.certificate_bytes(), original.certificate_bytes());
        assert_eq!(recovered.core_bytes(), original.core_bytes());
        assert_eq!(encode_record(&first), Some(payload));
    }

    #[test]
    fn durable_projection_roundtrips_complete_canonical_and_native_evidence() {
        let (root, mut record, _) = fixture();
        record.execution_source_sha256 = Some([91; 32]);
        let payload = encode_record(&record).unwrap();
        let decoded = decode(&payload, &record, root.path(), &mut ReadBudget::default()).unwrap();
        let mut original_data = Vec::new();
        ciborium::ser::into_writer(&record.data, &mut original_data).unwrap();
        let mut decoded_data = Vec::new();
        ciborium::ser::into_writer(&decoded.data, &mut decoded_data).unwrap();
        assert_eq!(original_data, payload);
        assert_eq!(decoded_data, payload);
        assert_eq!(decoded.execution_source_sha256, Some([91; 32]));
        assert!(decoded.module_interface.as_ref().unwrap().core.is_some());
        assert!(!decoded.original_certification.is_empty());
        assert!(decoded.module_interface_proof.is_none());
        assert!(decoded.execution_source.is_none());
    }

    #[test]
    fn changed_resolved_proof_cannot_publish_an_old_reference() {
        let (_, mut record, _) = fixture();
        record.evidence.make_mut().sources[0].sha256 = sha(b"changed invocation");
        assert!(encode_record(&record).is_none());
    }

    #[test]
    fn durable_records_require_canonical_and_native_certificates() {
        let (root, record, _) = fixture();
        for missing_canonical in [true, false] {
            let mut incomplete = record.clone();
            if missing_canonical {
                incomplete.module_interface = None;
            } else {
                incomplete.original_certification.clear();
            }
            assert!(encode_record(&incomplete).is_none());
            let mut payload = Vec::new();
            ciborium::ser::into_writer(&incomplete.data, &mut payload).unwrap();
            let mut budget = ReadBudget::default();
            assert!(decode(&payload, &incomplete, root.path(), &mut budget).is_none());
            assert_eq!(budget.evidence_count(), 0);
        }
    }

    #[test]
    fn unknown_durable_fields_are_refused_before_shared_proof_resolution() {
        let (root, record, payload) = fixture();
        let Value::Map(mut fields) = ciborium::de::from_reader(payload.as_slice()).unwrap() else {
            unreachable!()
        };
        fields.push((Value::Text("unsealed_evidence".into()), Value::Bool(true)));
        let mut payload = Vec::new();
        ciborium::ser::into_writer(&Value::Map(fields), &mut payload).unwrap();
        let mut budget = ReadBudget::default();
        assert!(decode(&payload, &record, root.path(), &mut budget).is_none());
        assert_eq!(budget.evidence_count(), 0);
        assert_eq!(budget.evidence_bytes, 0);
    }

    #[test]
    fn storage_format_refuses_inline_proof_and_previous_version() {
        let (root, mut record, _) = fixture();
        let mut inline = Vec::new();
        ciborium::ser::into_writer(&record, &mut inline).unwrap();
        assert!(decode(&inline, &record, root.path(), &mut ReadBudget::default()).is_none());
        record.version = RECORD_VERSION - 1;
        let previous = encode_record(&record).unwrap();
        assert!(decode(&previous, &record, root.path(), &mut ReadBudget::default()).is_none());
    }

    #[test]
    fn distinct_proofs_charge_the_aggregate_and_remain_separate() {
        let (root, record, payload) = fixture();
        let mut different = record.clone();
        different.evidence.make_mut().sources[0].sha256 = sha(b"different invocation");
        different.data.evidence = different.evidence.reference().unwrap().clone();
        publish(root.path(), &different.evidence).unwrap();
        let second_payload = encode_record(&different).unwrap();
        let mut budget = ReadBudget::default();
        let first = decode(&payload, &record, root.path(), &mut budget).unwrap();
        let second = decode(&second_payload, &different, root.path(), &mut budget).unwrap();
        assert!(!Arc::ptr_eq(&first.evidence.0, &second.evidence.0));
        assert_eq!(budget.evidence_count(), 2);
        let (_, proof) = encode_evidence(&record.evidence).unwrap();
        let (_, different_proof) = encode_evidence(&different.evidence).unwrap();
        assert_eq!(
            budget.bytes,
            (payload.len() + second_payload.len() + proof.len() + different_proof.len()) as u64
        );

        let mut budget = ReadBudget::default();
        decode(&payload, &record, root.path(), &mut budget).unwrap();
        budget.bytes = PAYLOAD_LIMIT as u64 - second_payload.len() as u64;
        assert!(decode(&second_payload, &different, root.path(), &mut budget).is_none());
        assert!(budget.exhausted);
    }

    #[test]
    fn missing_changed_or_trailing_shared_proof_is_a_miss() {
        let (root, record, payload) = fixture();
        let (reference, mut proof) = encode_evidence(&record.evidence).unwrap();
        let path = evidence_path(root.path(), &reference.sha256);
        fs::remove_file(&path).unwrap();
        assert!(decode(&payload, &record, root.path(), &mut ReadBudget::default()).is_none());
        *proof.last_mut().unwrap() ^= 1;
        fs::write(&path, &proof).unwrap();
        assert!(decode(&payload, &record, root.path(), &mut ReadBudget::default()).is_none());

        let (_, mut proof) = encode_evidence(&record.evidence).unwrap();
        proof.push(0);
        let trailing = EvidenceRef {
            sha256: sha(&proof),
            encoded_len: proof.len() as u64,
        };
        fs::write(evidence_path(root.path(), &trailing.sha256), &proof).unwrap();
        let mut changed = Vec::new();
        let mut altered_data = record.data.clone();
        altered_data.evidence = trailing;
        ciborium::ser::into_writer(&altered_data, &mut changed).unwrap();
        assert!(decode(&changed, &record, root.path(), &mut ReadBudget::default()).is_none());
    }

    #[test]
    fn references_and_aggregate_input_remain_bounded() {
        let (root, record, payload) = fixture();
        let (reference, _) = encode_evidence(&record.evidence).unwrap();
        for altered in [
            EvidenceRef {
                sha256: "../proof".into(),
                encoded_len: reference.encoded_len,
            },
            EvidenceRef {
                sha256: reference.sha256.clone(),
                encoded_len: reference.encoded_len + 1,
            },
            EvidenceRef {
                sha256: reference.sha256.clone(),
                encoded_len: RECORD_LIMIT as u64 + 1,
            },
        ] {
            let mut changed = Vec::new();
            let mut altered_data = record.data.clone();
            altered_data.evidence = altered;
            ciborium::ser::into_writer(&altered_data, &mut changed).unwrap();
            assert!(decode(&changed, &record, root.path(), &mut ReadBudget::default()).is_none());
        }
        let mut budget = ReadBudget {
            bytes: PAYLOAD_LIMIT as u64 - payload.len() as u64,
            ..Default::default()
        };
        assert!(decode(&payload, &record, root.path(), &mut budget).is_none());
        assert!(
            budget.exhausted,
            "the unique proof is charged to the aggregate input"
        );

        let mut trailing = payload;
        trailing.push(0);
        assert!(decode(&trailing, &record, root.path(), &mut ReadBudget::default()).is_none());
    }
}
