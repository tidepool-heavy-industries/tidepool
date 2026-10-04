//! Failure evidence is captured from the selected in-memory offer, never a
//! mutable cache record that a later compilation may have replaced.
use super::*;

impl CandidateSet {
    pub(crate) fn retain_evidence_diagnostics(&self, destination: &Path) -> std::io::Result<()> {
        let root = destination.join("selected-candidate-evidence");
        fs::create_dir_all(&root)?;
        let mut saved = BTreeSet::new();
        let mut bytes = 0usize;
        let mut owners = Vec::new();
        for bundle in self.by_owner.values() {
            if let Some(original) = &bundle.original_execution {
                let graph = &original.graph;
                if saved.insert(format!("execution-{}.cbor", hex(&graph.digest()))) {
                    bytes = bytes
                        .checked_add(graph.bytes().len())
                        .filter(|bytes| *bytes <= PAYLOAD_LIMIT)
                        .ok_or_else(|| {
                            std::io::Error::other(
                                "selected candidate diagnostics exceed aggregate bound",
                            )
                        })?;
                    graph.capture_descriptor(&root)?;
                }
            }
            let evidence = serde_json::to_vec(&bundle.evidence)?;
            let evidence_sha = sha(&evidence);
            let target_sha = sha(bundle.target_source.as_bytes());
            let evidence_name = format!("evidence-{evidence_sha}.json");
            let target_name = format!("target-{target_sha}.hs");
            for (name, content) in [
                (&evidence_name, evidence.as_slice()),
                (&target_name, bundle.target_source.as_bytes()),
            ] {
                if saved.insert(name.clone()) {
                    bytes = bytes
                        .checked_add(content.len())
                        .filter(|bytes| *bytes <= PAYLOAD_LIMIT)
                        .ok_or_else(|| {
                            std::io::Error::new(
                                std::io::ErrorKind::InvalidData,
                                "selected candidate diagnostics exceed aggregate bound",
                            )
                        })?;
                    tidepool_atomic_write::write_best_effort(&root.join(name), content)?;
                }
            }
            owners.push(serde_json::json!({
                "unit":bundle.owner.unit,"module":bundle.owner.module,
                "module_version":hex(&bundle.owner.module_version.0),
                "interface_sha256":hex(&bundle.owner.skinny_iface_sha256),
                "products_sha256":hex(&bundle.owner.product_sha256),
                "source":bundle.source,"source_sha256":bundle.source_sha256,
                "original_evidence":evidence_name,"original_evidence_sha256":evidence_sha,
                "original_target":target_name,"original_target_sha256":target_sha,
                "validation":bundle.evidence.validate(&bundle.target_source),
            }));
        }
        let index = serde_json::to_vec_pretty(&serde_json::json!({
            "scope":"transaction-selected original evidence; diagnostic only; never compiler authority",
            "manifest":self.manifest_path,"unique_artifact_bytes":bytes,"owners":owners,
        }))?;
        Ok(tidepool_atomic_write::write_best_effort(
            &root.join("index.json"),
            &index,
        )?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn diagnostics_preserve_selected_original_after_cache_replacement_and_source_drift() {
        let root = tempfile::tempdir().unwrap();
        let scratch = tempfile::tempdir().unwrap();
        let destination = tempfile::tempdir().unwrap();
        let record = super::super::tests::candidate_fixture(root.path(), "Library");
        let original = serde_json::to_vec(&record.evidence).unwrap();
        let original_target = record.target_source.clone();
        let include = record.include.clone();
        let selected = select_records(
            b"endpoint",
            &include,
            scratch.path(),
            vec![(record, CandidateOrigin::Ordinary)],
        )
        .unwrap();
        assert_eq!(selected.by_owner.len(), 1);
        let bundle = selected.by_owner.values().next().unwrap();
        assert!(bundle.evidence.valid(&bundle.target_source));
        // No later cache bytes are authoritative for the selected snapshot.
        fs::remove_dir_all(root.path().join(RECORD_DIR)).unwrap();
        fs::write(&bundle.source, "module Library where\nchanged = True\n").unwrap();
        selected
            .retain_evidence_diagnostics(destination.path())
            .unwrap();
        let diagnostics = destination.path().join("selected-candidate-evidence");
        let index: serde_json::Value =
            serde_json::from_slice(&fs::read(diagnostics.join("index.json")).unwrap()).unwrap();
        let owner = &index["owners"][0];
        assert_eq!(
            fs::read(diagnostics.join(owner["original_evidence"].as_str().unwrap())).unwrap(),
            original
        );
        assert_eq!(
            fs::read_to_string(diagnostics.join(owner["original_target"].as_str().unwrap()))
                .unwrap(),
            original_target
        );
        assert_eq!(owner["original_evidence_sha256"], sha(&original));
        assert_eq!(owner["validation"]["Err"]["Source"]["index"], 1);
        assert!(owner["validation"]["Err"]["Source"]["reason"]["Changed"].is_object());
    }
}
