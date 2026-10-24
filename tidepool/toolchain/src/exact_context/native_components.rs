//! Scalar storage receipts for the SAME repr component record. These decoded
//! records grant no body acquisition, source permission or runtime authority.

use super::*;
use crate::linked_native::{AcquiredNativeClosure, NativeAcquiredClosureFacts};
use tidepool_repr::native_code::{NativeCodeComponent, NativeCodeComponentFacts};

#[derive(Clone, Debug)]
pub(crate) struct ImmutableNativeAcquiredClosureRecordRef(Arc<NativeAcquiredClosureRecord>);

pub(crate) type NativeAcquiredClosureRecords =
    OrdMap<[u8; 32], ImmutableNativeAcquiredClosureRecordRef>;

#[derive(Debug)]
struct NativeAcquiredClosureRecord {
    facts: Arc<NativeAcquiredClosureFacts>,
    bytes: Arc<[u8]>,
    cid: OnceLock<Cid>,
}

impl ImmutableNativeAcquiredClosureRecordRef {
    pub(crate) fn from_closure(closure: &Arc<AcquiredNativeClosure>) -> Self {
        Self(Arc::new(NativeAcquiredClosureRecord {
            facts: Arc::clone(closure.recorded_facts()),
            bytes: Arc::clone(closure.record_bytes()),
            cid: OnceLock::new(),
        }))
    }

    pub(crate) fn facts(&self) -> &Arc<NativeAcquiredClosureFacts> {
        &self.0.facts
    }

    fn cid(&self) -> Cid {
        *self
            .0
            .cid
            .get_or_init(|| Cid::new_v1(DAG_CBOR, Code::Blake2b256.digest(&self.0.bytes)))
    }
}

impl PartialEq for ImmutableNativeAcquiredClosureRecordRef {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.0, &other.0) || self.0.bytes == other.0.bytes
    }
}
impl Eq for ImmutableNativeAcquiredClosureRecordRef {}

#[derive(Clone, Copy, Debug)]
pub(crate) struct NativeAcquiredClosureRecordCodec;

impl SnapshotValueCodec<ImmutableNativeAcquiredClosureRecordRef>
    for NativeAcquiredClosureRecordCodec
{
    type Wire = Cid;

    fn encode<BS: Blockstore>(
        store: &mut Materializer<'_, BS>,
        record: &ImmutableNativeAcquiredClosureRecordRef,
    ) -> Result<Cid, SnapshotError> {
        let cid = record.cid();
        store
            .put_keyed(&cid, &record.0.bytes)
            .map_err(storage_error)?;
        Ok(cid)
    }

    fn decode<BS: Blockstore>(
        store: &SnapshotHydrator<'_, BS>,
        cid: Cid,
    ) -> Result<ImmutableNativeAcquiredClosureRecordRef, SnapshotError> {
        if let Some(record) = store.native_closures.borrow().get(&cid) {
            return Ok(record.clone());
        }
        let bytes = read_checked_bytes(store, &cid)?;
        let facts = fvm_ipld_encoding::from_slice(&bytes).map_err(malformed)?;
        let record =
            ImmutableNativeAcquiredClosureRecordRef(Arc::new(NativeAcquiredClosureRecord {
                facts: Arc::new(facts),
                bytes: bytes.into(),
                cid: OnceLock::from(cid),
            }));
        store
            .native_closures
            .borrow_mut()
            .insert(cid, record.clone());
        Ok(record)
    }
}

#[derive(Clone, Debug)]
pub(crate) struct ImmutableNativeComponentRecordRef(Arc<NativeComponentRecord>);

pub(crate) type NativeComponentRecords = OrdMap<[u8; 32], ImmutableNativeComponentRecordRef>;

#[derive(Debug)]
struct NativeComponentRecord {
    facts: Arc<NativeCodeComponentFacts>,
    bytes: Arc<[u8]>,
    cid: OnceLock<Cid>,
}

impl ImmutableNativeComponentRecordRef {
    pub(crate) fn from_component(component: &Arc<NativeCodeComponent>) -> Self {
        Self(Arc::new(NativeComponentRecord {
            facts: Arc::clone(component.snapshot_record()),
            bytes: Arc::clone(component.record_bytes()),
            cid: OnceLock::new(),
        }))
    }

    pub(crate) fn facts(&self) -> &Arc<NativeCodeComponentFacts> {
        &self.0.facts
    }

    fn cid(&self) -> Cid {
        *self
            .0
            .cid
            .get_or_init(|| Cid::new_v1(DAG_CBOR, Code::Blake2b256.digest(&self.0.bytes)))
    }
}

impl PartialEq for ImmutableNativeComponentRecordRef {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.0, &other.0) || self.0.bytes == other.0.bytes
    }
}
impl Eq for ImmutableNativeComponentRecordRef {}

#[derive(Clone, Copy, Debug)]
pub(crate) struct NativeComponentRecordCodec;

impl SnapshotValueCodec<ImmutableNativeComponentRecordRef> for NativeComponentRecordCodec {
    type Wire = Cid;

    fn encode<BS: Blockstore>(
        store: &mut Materializer<'_, BS>,
        record: &ImmutableNativeComponentRecordRef,
    ) -> Result<Cid, SnapshotError> {
        let cid = record.cid();
        // The linked issuer already encoded its canonical record. Reuse those
        // exact bytes; the store still verifies their integrity on admission.
        store
            .put_keyed(&cid, &record.0.bytes)
            .map_err(storage_error)?;
        Ok(cid)
    }

    fn decode<BS: Blockstore>(
        store: &SnapshotHydrator<'_, BS>,
        cid: Cid,
    ) -> Result<ImmutableNativeComponentRecordRef, SnapshotError> {
        if let Some(record) = store.native_components.borrow().get(&cid) {
            return Ok(record.clone());
        }
        let bytes = read_checked_bytes(store, &cid)?;
        let facts = fvm_ipld_encoding::from_slice(&bytes).map_err(malformed)?;
        let record = ImmutableNativeComponentRecordRef(Arc::new(NativeComponentRecord {
            facts: Arc::new(facts),
            bytes: bytes.into(),
            cid: OnceLock::from(cid),
        }));
        store
            .native_components
            .borrow_mut()
            .insert(cid, record.clone());
        Ok(record)
    }
}

#[cfg(test)]
pub(super) mod tests {
    use super::*;
    use tidepool_repr::native_code::{
        NativeCodeBodyKey, NativeCodeNodeFacts, NativeCodeNominalKey, NativeHomeOwnerKey,
    };

    pub(crate) fn assert_linked_snapshot_recovery(
        support: &crate::artifact_inventory::CompilerSourceSupport,
        targets: &[Arc<crate::linked_native::LinkedNativeTarget>],
    ) {
        use crate::artifact_inventory::{ArtifactPayload, CompilerSourceSupport};
        use crate::declaration_context::{ExactDeclarationContext, RecoveredArtifactAcquisition};
        use crate::linked_native::{AcquiredNativeImport, NativeGrantSelector, NativeOccurrenceId};
        assert_eq!(targets.len(), 2);
        let first_support = CompilerSourceSupport::empty()
            .with_linked_native_target(&targets[0])
            .unwrap();
        assert!(
            first_support.artifacts().artifact_ids().is_empty(),
            "the first target must exercise package-only producer transfer"
        );
        assert!(first_support
            .acquired_native_closures()
            .package_products()
            .next()
            .is_some());
        let first = ExactDeclarationContext::new(&[], &[], vec![])
            .unwrap()
            .extend_issued_compiler_support(&first_support)
            .unwrap();
        assert_ne!(first.toolchain_identity_sha256(), [0; 32]);
        let second = first
            .clone()
            .extend_issued_compiler_support(support)
            .unwrap();
        let first_snapshot = first.snapshot();
        let second_snapshot = second.snapshot();
        assert!(!first_snapshot.native_components.is_empty());
        assert!(second_snapshot.native_components.len() >= first_snapshot.native_components.len());
        let common = *first_snapshot
            .native_components
            .keys()
            .find(|id| second_snapshot.native_components.contains_key(*id))
            .unwrap();

        let store = MemoryBlockstore::new();
        let mut writer = Materializer::new(&store);
        let first_record = writer.materialize_context(None, &first_snapshot).unwrap();
        install(&store, writer.finish().unwrap());
        let mut writer = Materializer::new(&store);
        let second_record = writer
            .materialize_context(Some(&first_record), &second_snapshot)
            .unwrap();
        install(&store, writer.finish().unwrap());
        assert_ne!(first_record.root(), second_record.root());
        let roots = [first_record.root().clone(), second_record.root().clone()];
        let decoded = hydrate_contexts(&store, &roots).unwrap();
        let receipts = roots
            .into_iter()
            .zip(decoded.into_iter().map(Arc::new))
            .collect::<Vec<_>>();
        assert!(Arc::ptr_eq(
            &receipts[0].1.snapshot().native_components[&common].0,
            &receipts[1].1.snapshot().native_components[&common].0
        ));

        let source_ids = second_snapshot
            .selection()
            .artifact_ids_introduced(None)
            .unwrap();
        let package_ids = second_snapshot.native_package_artifacts_introduced(None);
        assert!(!package_ids.is_empty());
        let mut payload_ids = source_ids.clone();
        payload_ids.extend(package_ids.iter().copied());
        let directory = tempfile::tempdir().unwrap();
        let references = second
            .materialize_recovery_artifacts(directory.path(), &payload_ids)
            .unwrap();
        let entries = second
            .artifact_view()
            .entries_for_artifacts(&source_ids)
            .unwrap();
        let mut descriptors = entries
            .iter()
            .map(|entry| entry.descriptor.clone())
            .collect::<Vec<_>>();
        let packages = support.selected_package_products(&package_ids).unwrap();
        assert_eq!(
            packages
                .keys()
                .copied()
                .collect::<std::collections::BTreeSet<_>>(),
            package_ids
        );
        descriptors.extend(
            packages
                .values()
                .map(|product| product.descriptor().clone()),
        );
        let graph = second_snapshot.graph_selection().unwrap();
        let edges = second_snapshot.binding_dependencies().unwrap();
        let mut acquisition = RecoveredArtifactAcquisition::new(directory.path());
        let inventory = acquisition
            .capture_payloads_bound(&references, &descriptors, &graph.bindings, &edges)
            .unwrap();
        let restored = inventory
            .contexts_for_expected_snapshots(&receipts)
            .unwrap();
        assert_eq!(restored.len(), 2);
        let mut shared_components = 0;
        for (context, (_, receipt)) in restored.iter().zip(&receipts) {
            assert_eq!(
                context.snapshot().selection(),
                receipt.snapshot().selection(),
                "restoration preserves roots, namespace, custody seeds and proof scopes"
            );
            let mut rebound = context.clone();
            for _ in 0..2 {
                rebound.bind_snapshot_baseline(receipt).unwrap();
                assert_eq!(
                    rebound.snapshot().selection(),
                    receipt.snapshot().selection(),
                    "baseline sharing must not replay live custody composition"
                );
            }
            let held = context.compiler_source_support();
            assert!(held
                .native_component_records()
                .ptr_eq(&receipt.snapshot().native_components));
            assert!(held
                .native_closure_records()
                .ptr_eq(&receipt.snapshot().native_closures));
            for closure in held.acquired_native_closures().iter() {
                for acquired in closure.acquired() {
                    for target in targets {
                        assert!(
                            !target.owns_acquired(&acquired),
                            "durable code cannot restore receiving-target source authority"
                        );
                        for (index, import) in acquired.imports().enumerate() {
                            if let AcquiredNativeImport::Retained {
                                requirement,
                                binding,
                            } = import
                            {
                                assert!(target
                                    .verify_retained_slot(
                                        &acquired,
                                        index.try_into().unwrap(),
                                        binding,
                                        &requirement
                                    )
                                    .is_err());
                            }
                        }
                    }
                }
            }
            let mut writer = Materializer::new(&store);
            let same = writer
                .materialize_context(Some(receipt), &context.snapshot())
                .unwrap();
            assert_eq!(same.root(), receipt.root());
            assert_eq!(
                writer.finish().unwrap().metrics(),
                MaterializationMetrics::default()
            );
        }
        let first_components = restored[0]
            .compiler_source_support()
            .acquired_native_closures()
            .iter()
            .flat_map(|owner| owner.acquired())
            .map(|code| {
                (
                    *code.code().component().identity(),
                    Arc::clone(code.code().component()),
                )
            })
            .collect::<std::collections::BTreeMap<_, _>>();
        for later_owner in restored[1]
            .compiler_source_support()
            .acquired_native_closures()
            .iter()
        {
            for later_code in later_owner.acquired() {
                if let Some(first) = first_components.get(later_code.code().component().identity())
                {
                    shared_components += 1;
                    assert!(Arc::ptr_eq(first, later_code.code().component()));
                }
            }
        }
        assert!(
            shared_components > 0,
            "independent acquired contexts must share actual restored components"
        );

        // The first context fills the operation's pure component cache. A later
        // context must still supply its own exact component record.
        let mut missing = second_snapshot.clone();
        missing.native_components.remove(&common);
        let mut writer = Materializer::new(&store);
        let missing = writer.materialize_context(None, &missing).unwrap();
        install(&store, writer.finish().unwrap());
        let missing = Arc::new(hydrate_context(&store, missing.root()).unwrap());
        let refusal = inventory
            .contexts_for_expected_snapshots(&[
                receipts[0].clone(),
                (missing.root().clone(), Arc::clone(&missing)),
            ])
            .unwrap_err();
        assert!(matches!(refusal, crate::CompileError::ExtractFailed(detail)
            if detail.contains("recorded code component is absent")));

        let mut corrupt = second_snapshot.clone();
        let mut facts = corrupt.native_components[&common].facts().as_ref().clone();
        facts.nodes[0].body.definitions_sha256[0] ^= 1;
        let mut bytes = Vec::new();
        ciborium::ser::into_writer(&facts, &mut bytes).unwrap();
        corrupt.native_components.insert(
            common,
            ImmutableNativeComponentRecordRef(Arc::new(NativeComponentRecord {
                facts: Arc::new(facts),
                bytes: bytes.into(),
                cid: OnceLock::new(),
            })),
        );
        let mut writer = Materializer::new(&store);
        let corrupt = writer.materialize_context(None, &corrupt).unwrap();
        install(&store, writer.finish().unwrap());
        let corrupt = Arc::new(hydrate_context(&store, corrupt.root()).unwrap());
        let refusal = inventory
            .contexts_for_expected_snapshots(&[
                receipts[0].clone(),
                (corrupt.root().clone(), Arc::clone(&corrupt)),
            ])
            .unwrap_err();
        assert!(matches!(refusal, crate::CompileError::ExtractFailed(detail)
            if detail.contains("cached native component differs from current acquired facts")));

        let crossing = second_snapshot
            .native_closures
            .iter()
            .find_map(|(identity, record)| {
                record.facts().package_receipts.iter().enumerate().find_map(
                    |(receipt_index, receipt)| {
                        receipt.occurrences.iter().enumerate().find_map(|(row_index, row)| {
                            let parent = &packages[&row.product];
                            row.children.iter().enumerate().find_map(|(slot, original)| {
                                let required = &parent.code().globals()[slot].identity;
                                let original_row = receipt.occurrences.iter()
                                    .find(|row| row.occurrence == *original)?;
                                let original_product = &packages[&original_row.product];
                                receipt.occurrences.iter().find(|other| {
                                    let candidate = &packages[&other.product];
                                    other.occurrence != *original
                                        && candidate.code().owner().unit == required.unit
                                        && candidate.code().owner().module == required.module
                                        && candidate.defining_interface().sha256()
                                            == original_product.defining_interface().sha256()
                                        && candidate.code().owner().compiler_sha256
                                            == parent.code().owner().compiler_sha256
                                        && candidate.code().owner().profile_sha256
                                            == parent.code().owner().profile_sha256
                                        && !candidate.code().exported_values()
                                            .any(|(identity, _)| identity == required)
                                }).map(|other| (*identity, receipt_index, row_index, slot,
                                    *original, other.occurrence))
                            })
                        })
                    },
                )
            })
            .expect("actual package closure must retain a same-interface sibling without the demanded export");
        let (identity, receipt_index, row_index, slot, original, replacement) = crossing;
        let original_record = &second_snapshot.native_closures[&identity];
        let original_facts = original_record.facts();
        let mut original_bytes = Vec::new();
        ciborium::ser::into_writer(original_facts.as_ref(), &mut original_bytes).unwrap();
        assert_eq!(
            original_bytes.as_slice(),
            original_record.0.bytes.as_ref(),
            "unchanged closure facts preserve their captured record bytes"
        );
        let receipt = &original_facts.package_receipts[receipt_index];
        let parent = &packages[&receipt.occurrences[row_index].product];
        let required = &parent.code().globals()[slot].identity;
        let product_at = |occurrence| {
            let row = receipt
                .occurrences
                .iter()
                .find(|row| row.occurrence == occurrence)
                .unwrap();
            &packages[&row.product]
        };
        let original_product = product_at(original);
        let replacement_product = product_at(replacement);
        assert_eq!(
            original_product.code().owner().unit,
            replacement_product.code().owner().unit
        );
        assert_eq!(
            original_product.code().owner().module,
            replacement_product.code().owner().module
        );
        assert_eq!(
            original_product.defining_interface().sha256(),
            replacement_product.defining_interface().sha256()
        );
        assert!(original_product
            .code()
            .exported_values()
            .any(|(identity, _)| identity == required));
        assert!(!replacement_product
            .code()
            .exported_values()
            .any(|(identity, _)| identity == required));

        // The snapshot writer owns each changed record's CID. The recorded
        // closure key remains untrusted input; the asserted semantic refusals
        // must precede the final inner closure commitment check.
        let refuse_corrupted_closure = |facts: NativeAcquiredClosureFacts| {
            let mut crossed = second_snapshot.clone();
            let mut bytes = Vec::new();
            ciborium::ser::into_writer(&facts, &mut bytes).unwrap();
            crossed.native_closures.insert(
                identity,
                ImmutableNativeAcquiredClosureRecordRef(Arc::new(NativeAcquiredClosureRecord {
                    facts: Arc::new(facts),
                    bytes: bytes.into(),
                    cid: OnceLock::new(),
                })),
            );
            let mut writer = Materializer::new(&store);
            let crossed = writer.materialize_context(None, &crossed).unwrap();
            install(&store, writer.finish().unwrap());
            let crossed = hydrate_context(&store, crossed.root()).unwrap();
            inventory
                .context_for_expected_snapshot(crossed.root(), &crossed)
                .unwrap_err()
        };

        let mut wrong_export = original_facts.as_ref().clone();
        let receipt = Arc::make_mut(&mut wrong_export.package_receipts[receipt_index]);
        assert_eq!(receipt.occurrences[row_index].children[slot], original);
        receipt.occurrences[row_index].children[slot] = replacement;
        let refusal = refuse_corrupted_closure(wrong_export);
        assert!(
            matches!(&refusal, crate::CompileError::ExtractFailed(detail)
            if detail == "native acquisition: selected package does not export the demanded identity"),
            "same defining interface cannot supply a sibling's missing export: {refusal:?}"
        );

        // Retain the genuine receipt and products, changing only the recorded
        // selector. Receipt acquisition still chooses the original valid export.
        let receipt = &original_facts.package_receipts[receipt_index];
        let parent = NativeOccurrenceId::Package {
            stage_id: receipt.stage_id,
            occurrence: receipt.occurrences[row_index].occurrence,
        };
        let expected = NativeGrantSelector::Code(NativeOccurrenceId::Package {
            stage_id: receipt.stage_id,
            occurrence: original,
        });
        let changed = NativeGrantSelector::Code(NativeOccurrenceId::Package {
            stage_id: receipt.stage_id,
            occurrence: replacement,
        });
        let mut wrong_selector = original_facts.as_ref().clone();
        let row = wrong_selector
            .occurrences
            .iter_mut()
            .find(|row| row.occurrence == parent)
            .unwrap();
        let mut selectors = row.imports.to_vec();
        assert_eq!(selectors[slot], expected);
        assert_ne!(expected, changed);
        selectors[slot] = changed;
        row.imports = selectors.into();
        assert_eq!(
            wrong_selector.package_receipts,
            original_facts.package_receipts
        );
        let refusal = refuse_corrupted_closure(wrong_selector);
        assert!(
            matches!(&refusal, crate::CompileError::ExtractFailed(detail)
            if detail == "native acquisition: recorded slot differs from its exact acquired selector"),
            "recorded slot cannot replace its receipt's exact occurrence: {refusal:?}"
        );

        // Bytes remain on disk and in the acquisition's retained cache. Omitting
        // the explicit requested reference cannot borrow either as permission.
        let omitted = *package_ids.iter().next().unwrap();
        let missing_references = references
            .iter()
            .filter(|reference| match reference {
                crate::declaration_context::RecoveryPayloadReference::PackageCode(reference) => {
                    crate::artifact_inventory::ArtifactDescriptor::from_recovery_package_code(
                        reference,
                    )
                    .id != omitted
                }
                _ => true,
            })
            .cloned()
            .collect::<Vec<_>>();
        let missing_descriptors = descriptors
            .iter()
            .filter(|descriptor| descriptor.id != omitted)
            .cloned()
            .collect::<Vec<_>>();
        let mut stage = acquisition.stage_payloads(&missing_references).unwrap();
        let missing_inventory = stage
            .capture_payloads_bound(
                &missing_references,
                &missing_descriptors,
                &graph.bindings,
                &edges,
            )
            .unwrap();
        let refusal = missing_inventory
            .context_for_expected_snapshot(&receipts[1].0, &receipts[1].1)
            .unwrap_err();
        assert!(matches!(refusal, crate::CompileError::ExtractFailed(detail)
            if detail.contains("recorded package occurrence lacks acquired product custody")));

        let (original, canonical) = references
            .iter()
            .find_map(|reference| match reference {
                crate::declaration_context::RecoveryPayloadReference::Original(original) => {
                    original
                        .module_interface
                        .as_ref()
                        .map(|canonical| (original, canonical))
                }
                _ => None,
            })
            .expect("actual HOME dependency retains its canonical companion");
        let omitted_interface =
            crate::artifact_inventory::ArtifactDescriptor::from_recovery_module_interface(
                canonical,
            )
            .id;
        assert!(entries
            .iter()
            .any(|entry| entry.descriptor.id == omitted_interface
                && matches!(entry.payload, ArtifactPayload::Canonical(_))));
        assert!(directory.path().join(&original.product_path).is_file());
        // Originals name their exact canonical companion implicitly. Remove its
        // actual payload, rather than only an explicit duplicate reference, and
        // acquire afresh so held immutable custody cannot satisfy the read.
        let missing_path = directory.path().join(&canonical.certificate_path);
        let certificate_bytes = std::fs::read(&missing_path).unwrap();
        std::fs::remove_file(&missing_path).unwrap();
        let mut fresh = RecoveredArtifactAcquisition::new(directory.path());
        let refused =
            fresh.capture_payloads_bound(&references, &descriptors, &graph.bindings, &edges);
        std::fs::write(&missing_path, &certificate_bytes).unwrap();
        let refusal = refused
            .err()
            .expect("missing required interface must refuse");
        assert!(
            matches!(&refusal, crate::declaration_context::RecoveryInventoryError::Artifacts(losses)
                if losses.iter().any(|(id, error)| *id == omitted_interface
                    && matches!(error, crate::recovery_artifacts::RecoveryArtifactError::Unavailable(path)
                        if path == &missing_path))),
            "the native payload cannot replace required canonical interface {omitted_interface:?}: {refusal:?}"
        );
        fresh
            .capture_payloads_bound(&references, &descriptors, &graph.bindings, &edges)
            .unwrap();
    }

    // Serializer DATA only; no body, acquired grant or usable CodeRef is issued.
    fn data_record(index: u64) -> ImmutableNativeComponentRecordRef {
        let mut definitions_sha256 = [0; 32];
        definitions_sha256[..8].copy_from_slice(&index.to_be_bytes());
        let facts = Arc::new(NativeCodeComponentFacts {
            version: 1,
            nodes: vec![NativeCodeNodeFacts {
                body: NativeCodeBodyKey {
                    nominal: NativeCodeNominalKey::Home {
                        owner: NativeHomeOwnerKey {
                            unit: "storage-data".into(),
                            module: format!("{index}{}", "x".repeat(2048)),
                            module_version: definitions_sha256,
                            skinny_iface_sha256: [0; 32],
                            product_sha256: definitions_sha256,
                        },
                        original_ordinal: 0,
                    },
                    definitions_sha256,
                },
                imports: vec![],
            }],
        });
        let mut bytes = Vec::new();
        ciborium::ser::into_writer(facts.as_ref(), &mut bytes).unwrap();
        ImmutableNativeComponentRecordRef(Arc::new(NativeComponentRecord {
            facts,
            bytes: bytes.into(),
            cid: OnceLock::new(),
        }))
    }

    fn install(store: &MemoryBlockstore, packet: StagedBlocks) {
        for (cid, bytes) in packet.into_blocks() {
            store.put_keyed(&cid, &bytes).unwrap();
        }
    }

    #[test]
    fn context_records_preserve_overlapping_data_and_only_new_package_payload_ids() {
        use crate::linked_native::{NativePackageOccurrenceFacts, NativePackageReceiptFacts};
        let closure_record = |product: ArtifactId| {
            let facts = Arc::new(NativeAcquiredClosureFacts {
                version: 1,
                occurrences: vec![],
                package_receipts: vec![Arc::new(NativePackageReceiptFacts {
                    stage_id: [product.0[0]; 32],
                    occurrences: vec![NativePackageOccurrenceFacts {
                        occurrence: 1,
                        product,
                        children: vec![],
                    }],
                })],
            });
            let mut bytes = Vec::new();
            ciborium::ser::into_writer(facts.as_ref(), &mut bytes).unwrap();
            // This deliberately is DATA, not an acquired closure: its receipt
            // has no corresponding occurrence. Only B's issuer may admit it.
            ImmutableNativeAcquiredClosureRecordRef(Arc::new(NativeAcquiredClosureRecord {
                facts,
                bytes: bytes.into(),
                cid: OnceLock::new(),
            }))
        };
        let component = data_record(9);
        let first_product = ArtifactId([1; 32]);
        let second_product = ArtifactId([2; 32]);
        let mut first = ExactContextSnapshot::empty();
        first.native_components.insert([9; 32], component.clone());
        first
            .native_closures
            .insert([1; 32], closure_record(first_product));
        let mut second = first.clone();
        second
            .native_closures
            .insert([2; 32], closure_record(second_product));
        assert_eq!(
            second.native_package_artifacts_introduced(Some(&first)),
            std::collections::BTreeSet::from([second_product])
        );
        assert_eq!(
            second.native_package_artifacts_introduced(None),
            std::collections::BTreeSet::from([first_product, second_product])
        );
        let store = MemoryBlockstore::new();
        let mut writer = Materializer::new(&store);
        let first = writer.materialize_context(None, &first).unwrap();
        install(&store, writer.finish().unwrap());
        let mut writer = Materializer::new(&store);
        let second = writer.materialize_context(Some(&first), &second).unwrap();
        assert_eq!(
            first.native_components.root(),
            second.native_components.root()
        );
        install(&store, writer.finish().unwrap());
        assert_ne!(first.root(), second.root());
        struct Observed<'a> {
            store: &'a MemoryBlockstore,
            watched: Cid,
            reads: Cell<usize>,
        }
        impl Blockstore for Observed<'_> {
            fn get(&self, cid: &Cid) -> anyhow::Result<Option<Vec<u8>>> {
                if cid == &self.watched {
                    self.reads.set(self.reads.get() + 1);
                }
                self.store.get(cid)
            }
            fn put_keyed(&self, _: &Cid, _: &[u8]) -> anyhow::Result<()> {
                unreachable!()
            }
        }
        let observed = Observed {
            store: &store,
            watched: component.cid(),
            reads: Cell::new(0),
        };
        let mut restored =
            hydrate_contexts(&observed, &[first.root().clone(), second.root().clone()]).unwrap();
        assert_eq!(restored.len(), 2);
        let successor = restored.pop().unwrap();
        let predecessor = restored.pop().unwrap();
        assert!(Arc::ptr_eq(
            &predecessor.snapshot().native_components[&[9; 32]].0,
            &successor.snapshot().native_components[&[9; 32]].0
        ));
        assert!(Arc::ptr_eq(
            &predecessor.snapshot().native_closures[&[1; 32]].0,
            &successor.snapshot().native_closures[&[1; 32]].0
        ));
        assert_eq!(
            observed.reads.get(),
            1,
            "overlapping context roots read the shared record once"
        );
        let restored = successor;
        assert_eq!(
            restored.snapshot().native_components,
            second.snapshot().native_components
        );
        assert_eq!(
            restored.snapshot().native_closures,
            second.snapshot().native_closures
        );
        let mut writer = Materializer::new(&store);
        let same = writer
            .materialize_context(Some(&restored), restored.snapshot())
            .unwrap();
        assert_eq!(same.root(), second.root());
        assert_eq!(
            writer.finish().unwrap().metrics(),
            MaterializationMetrics::default()
        );
    }

    #[test]
    fn component_record_storage_preserves_exact_bytes_and_restored_sharing() {
        let record = data_record(3);
        let store = MemoryBlockstore::new();
        let mut writer = Materializer::new(&store);
        let cid = NativeComponentRecordCodec::encode(&mut writer, &record).unwrap();
        let packet = writer.finish().unwrap();
        assert_eq!(
            packet.metrics().serialized_bytes,
            0,
            "the graph issuer already encoded its record"
        );
        install(&store, packet);
        assert_eq!(
            store.get(&cid).unwrap().unwrap().as_slice(),
            record.0.bytes.as_ref()
        );
        let reader = SnapshotHydrator::new(&store);
        let first = NativeComponentRecordCodec::decode(&reader, cid).unwrap();
        let second = NativeComponentRecordCodec::decode(&reader, cid).unwrap();
        assert!(Arc::ptr_eq(&first.0, &second.0));
        assert_eq!(first.facts(), record.facts());
        assert_eq!(first.cid(), cid);
    }

    #[test]
    fn component_record_growth_tracks_new_records_and_shared_map_paths() {
        for width in [32u64, 256] {
            let store = MemoryBlockstore::new();
            let records: OrdMap<_, _> = (0..width)
                .map(|index| (index, data_record(index)))
                .collect();
            let mut writer = Materializer::new(&store);
            let base = writer
                .materialize_map_with::<_, _, NativeComponentRecordCodec>(None, &records)
                .unwrap();
            let cold = writer.finish().unwrap();
            let cold_bytes = cold.metrics().offered_bytes;
            install(&store, cold);

            let mut next = records.clone();
            next.insert(width, data_record(width));
            let mut writer = Materializer::new(&store);
            let changed = writer
                .materialize_map_with::<_, _, NativeComponentRecordCodec>(Some(&base), &next)
                .unwrap();
            let delta = writer.finish().unwrap();
            assert_eq!(delta.metrics().changed_entries, 1);
            assert!(
                delta.metrics().offered_bytes < cold_bytes / 4,
                "one new record must not offer the historical component bodies"
            );
            install(&store, delta);

            let mut writer = Materializer::new(&store);
            let same = writer
                .materialize_map_with::<_, _, NativeComponentRecordCodec>(Some(&changed), &next)
                .unwrap();
            assert_eq!(same.root(), changed.root());
            assert_eq!(
                writer.finish().unwrap().metrics(),
                MaterializationMetrics::default()
            );

            // Two distinct lookup keys deliberately alias one DATA record. This
            // checks restored block sharing without granting either key code authority.
            let aliases: OrdMap<u64, ImmutableNativeComponentRecordRef> =
                OrdMap::from_iter([(0u64, records[&0].clone()), (1, records[&0].clone())]);
            let mut writer = Materializer::new(&store);
            let alias_root = writer
                .materialize_map_with::<_, _, NativeComponentRecordCodec>(None, &aliases)
                .unwrap();
            install(&store, writer.finish().unwrap());
            let reader = SnapshotHydrator::new(&store);
            let restored = hydrate_map_owned::<
                u64,
                ImmutableNativeComponentRecordRef,
                NativeComponentRecordCodec,
                _,
            >(&reader, alias_root.root())
            .unwrap();
            assert!(Arc::ptr_eq(&restored.facts[&0].0, &restored.facts[&1].0));
        }
    }

    #[test]
    fn component_record_storage_distinguishes_missing_inaccessible_and_corrupt_data() {
        let record = data_record(5);
        let cid = record.cid();
        assert!(
            matches!(NativeComponentRecordCodec::decode(&SnapshotHydrator::new(&MemoryBlockstore::new()), cid),
            Err(SnapshotError::MissingBlock(found)) if found == cid)
        );
        struct InaccessibleStore;
        impl Blockstore for InaccessibleStore {
            fn get(&self, _: &Cid) -> anyhow::Result<Option<Vec<u8>>> {
                Err(std::io::Error::from(std::io::ErrorKind::PermissionDenied).into())
            }
            fn put_keyed(&self, _: &Cid, _: &[u8]) -> anyhow::Result<()> {
                unreachable!()
            }
        }
        assert!(matches!(
            NativeComponentRecordCodec::decode(&SnapshotHydrator::new(&InaccessibleStore), cid),
            Err(SnapshotError::Storage(_))
        ));
        let store = MemoryBlockstore::new();
        store.put_keyed(&cid, &[0]).unwrap();
        assert!(
            matches!(NativeComponentRecordCodec::decode(&SnapshotHydrator::new(&store), cid),
            Err(SnapshotError::Integrity(found)) if found == cid)
        );
        let mut writer = Materializer::new(&store);
        let malformed_cid = writer.materialize_value(&0u64).unwrap();
        install(&store, writer.finish().unwrap());
        assert!(matches!(
            NativeComponentRecordCodec::decode(&SnapshotHydrator::new(&store), malformed_cid),
            Err(SnapshotError::Malformed(_))
        ));
    }
}
