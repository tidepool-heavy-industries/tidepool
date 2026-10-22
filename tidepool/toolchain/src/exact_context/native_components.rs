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
mod tests {
    use super::*;
    use tidepool_repr::native_code::{
        NativeCodeBodyKey, NativeCodeNodeFacts, NativeCodeNominalKey, NativeHomeOwnerKey,
    };

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
            let aliases =
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
