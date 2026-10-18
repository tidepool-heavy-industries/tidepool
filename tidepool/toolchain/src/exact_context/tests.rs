use super::*;
use proptest::prelude::*;
use std::collections::BTreeMap;

fn install(store: &MemoryBlockstore, packet: StagedBlocks) {
    for (cid, bytes) in packet.into_blocks() {
        verify_block(&cid, &bytes).unwrap();
        store.put_keyed(&cid, &bytes).unwrap();
    }
}

struct CountingStore<'a> {
    inner: &'a MemoryBlockstore,
    reads: RefCell<BTreeMap<Cid, (u64, u64)>>,
}

impl Blockstore for CountingStore<'_> {
    fn get(&self, cid: &Cid) -> anyhow::Result<Option<Vec<u8>>> {
        let bytes = self.inner.get(cid)?;
        let mut reads = self.reads.borrow_mut();
        let (calls, total) = reads.entry(*cid).or_default();
        *calls += 1;
        *total += bytes.as_ref().map_or(0, |bytes| bytes.len() as u64);
        Ok(bytes)
    }

    fn put_keyed(&self, _: &Cid, _: &[u8]) -> anyhow::Result<()> {
        unreachable!("the materializer owns staged writes")
    }
}

// These are transport facts for the serializer, not compiler/native authority.
fn shared_scope_snapshots() -> (ImmutableScopeProofRef, Vec<Arc<ExactSelectionSnapshot>>) {
    let selection = ArtifactSelectionId([7; 32]);
    let bindings = (1..=8)
        .map(|byte| ArtifactBinding {
            artifact: ArtifactId([byte; 32]),
            selection,
        })
        .collect::<Vec<_>>();
    let proof = ImmutableScopeProofRef::from_issued(Arc::new(ScopeProofV3 {
        selection,
        nodes: bindings
            .iter()
            .copied()
            .map(ArtifactBindingNode::Artifact)
            .collect(),
        dependencies: Vec::new(),
    }));
    let mut snapshots = [0..8, 0..4, 2..6]
        .into_iter()
        .map(|range| {
            let selected = &bindings[range];
            Arc::new(ExactSelectionSnapshot {
                scopes: [(selection, proof.clone())].into_iter().collect(),
                roots: selected
                    .iter()
                    .copied()
                    .map(|binding| (ArtifactBindingNode::Artifact(binding), ()))
                    .collect(),
                namespace: selected
                    .iter()
                    .map(|binding| (binding.artifact, *binding))
                    .collect(),
                custody_roots: OrdMap::new(),
            })
        })
        .collect::<Vec<_>>();
    snapshots.push(Arc::new(ExactSelectionSnapshot {
        scopes: [(selection, proof.clone())].into_iter().collect(),
        roots: OrdMap::new(),
        namespace: OrdMap::new(),
        custody_roots: [(ArtifactBindingNode::Artifact(bindings[0]), ())]
            .into_iter()
            .collect(),
    }));
    (proof, snapshots)
}

#[test]
fn shared_scope_and_selection_materialization_work_is_operation_local() {
    let (proof, snapshots) = shared_scope_snapshots();
    let expected = fvm_ipld_encoding::to_vec(proof.proof().as_ref()).unwrap();
    let proof_cid = Cid::new_v1(DAG_CBOR, Code::Blake2b256.digest(&expected));
    let store = MemoryBlockstore::new();
    // Derive each canonical root independently, without a preceding selection.
    let canonical = snapshots
        .iter()
        .map(|snapshot| {
            let mut materializer = Materializer::new(&store);
            let receipt = materializer.materialize_selection(None, snapshot).unwrap();
            install(&store, materializer.finish().unwrap());
            receipt.root().clone()
        })
        .collect::<Vec<_>>();
    assert_eq!(store.get(&proof_cid).unwrap().unwrap(), expected);
    let full_base = hydrate_selection(&store, &canonical[0]).unwrap();
    let partial_base = hydrate_selection(&store, &canonical[1]).unwrap();
    for (snapshot, canonical) in snapshots.iter().zip(&canonical) {
        let mut materializer = Materializer::new(&store);
        // Canonical identity is independent of the base chosen before or after
        // the operation issues a receipt, including custody-only selections.
        for base in [Some(&full_base), None, Some(&partial_base)] {
            assert_eq!(
                materializer
                    .materialize_selection(base, snapshot)
                    .unwrap()
                    .root(),
                canonical
            );
        }
    }
    // The canonical oracle warms the immutable encoding owner in both arms;
    // this comparison varies physical store custody, not proof encoder warmth.
    for warm in [false, true] {
        let empty = MemoryBlockstore::new();
        let base = if warm { &store } else { &empty };
        let mut observations = Vec::new();
        for repetitions in [1, 4] {
            let observed = CountingStore {
                inner: base,
                reads: Default::default(),
            };
            let mut materializer = Materializer::new(&observed);
            for _ in 0..repetitions {
                for (snapshot, canonical) in snapshots.iter().zip(&canonical) {
                    let receipt = materializer.materialize_selection(None, snapshot).unwrap();
                    assert_eq!(receipt.root(), canonical);
                }
            }
            let packet = materializer.finish().unwrap();
            let scope_reads = observed.reads.borrow().get(&proof_cid).copied().unwrap();
            eprintln!("scope-materialization-work warm={warm} selections={} repetitions={repetitions} scope_gets={} scope_read_bytes={} metrics={:?}", snapshots.len(), scope_reads.0, scope_reads.1, packet.metrics());
            assert_eq!(scope_reads.0, 1);
            assert_eq!(scope_reads.1, if warm { expected.len() as u64 } else { 0 });
            observations.push(packet.metrics());
            let reloaded = MemoryBlockstore::new();
            if !warm {
                install(&reloaded, packet);
            }
            let roundtrip = if warm { &store } else { &reloaded };
            for (root, snapshot) in canonical.iter().zip(&snapshots) {
                assert_eq!(
                    hydrate_selection(roundtrip, root)
                        .unwrap()
                        .snapshot()
                        .as_ref(),
                    snapshot.as_ref()
                );
            }
        }
        assert_eq!(observations[0], observations[1]);
    }
}

#[test]
fn independent_scope_owners_authenticate_and_contradictory_bodies_refuse() {
    let (proof, snapshots) = shared_scope_snapshots();
    let store = MemoryBlockstore::new();
    let mut first = Materializer::new(&store);
    first.materialize_selection(None, &snapshots[0]).unwrap();
    install(&store, first.finish().unwrap());
    let (cid, bytes) = proof.encoded().unwrap();
    let observed = CountingStore {
        inner: &store,
        reads: Default::default(),
    };
    let mut materializer = Materializer::new(&observed);
    let first = materializer
        .materialize_selection(None, &snapshots[0])
        .unwrap();
    let independent = ImmutableScopeProofRef::from_issued(Arc::new(proof.proof().as_ref().clone()));
    let before = materializer.cid_validation_bytes.get();
    assert_eq!(
        ScopeProofCodec::encode(&mut materializer, &independent).unwrap(),
        *cid
    );
    assert_eq!(
        materializer.cid_validation_bytes.get() - before,
        bytes.len() as u64
    );
    let checked = materializer.cid_validation_bytes.get();
    assert_eq!(
        ScopeProofCodec::encode(&mut materializer, &independent).unwrap(),
        *cid
    );
    assert_eq!(materializer.cid_validation_bytes.get(), checked);
    let mut equal = snapshots[0].as_ref().clone();
    equal
        .scopes
        .insert(independent.proof().selection, independent);
    let equal = Arc::new(equal);
    assert_eq!(
        materializer
            .materialize_selection(None, &equal)
            .unwrap()
            .root(),
        first.root()
    );
    let work = materializer.finish().unwrap().metrics();
    assert_eq!(observed.reads.borrow().get(cid).unwrap().0, 1);
    assert!(work.cid_validation_bytes >= checked);

    let corrupt = MemoryBlockstore::new();
    corrupt.put_keyed(cid, b"untrusted incoming proof").unwrap();
    assert!(
        matches!(ScopeProofCodec::decode(&SnapshotHydrator::new(&corrupt), *cid), Err(SnapshotError::Integrity(found)) if found == *cid)
    );

    let mut contradictory = proof.proof().as_ref().clone();
    contradictory.nodes.pop();
    let contradictory = ImmutableScopeProofRef::from_issued(Arc::new(contradictory));
    let mut changed = snapshots[0].as_ref().clone();
    changed
        .scopes
        .insert(contradictory.proof().selection, contradictory);
    let mut materializer = Materializer::new(&store);
    materializer
        .materialize_selection(None, &snapshots[0])
        .unwrap();
    assert!(
        matches!(materializer.materialize_selection(None, &Arc::new(changed)), Err(SnapshotError::Malformed(message)) if message == "conflicting immutable scope proof")
    );
    assert_eq!(
        materializer
            .materialize_selection(None, &snapshots[0])
            .unwrap()
            .root(),
        first.root()
    );
    let other = materializer
        .materialize_selection(None, &snapshots[1])
        .unwrap();
    install(&store, materializer.finish().unwrap());
    assert_eq!(
        hydrate_selection(&store, other.root())
            .unwrap()
            .snapshot()
            .as_ref(),
        snapshots[1].as_ref()
    );
}

#[test]
fn scope_receipts_do_not_cross_store_or_failed_operation_boundaries() {
    let (proof, snapshots) = shared_scope_snapshots();
    let store = MemoryBlockstore::new();
    let mut first = Materializer::new(&store);
    let receipt = first.materialize_selection(None, &snapshots[0]).unwrap();
    install(&store, first.finish().unwrap());
    let (cid, bytes) = proof.encoded().unwrap();
    store.put_keyed(cid, b"corrupt physical scope").unwrap();
    let mut next = Materializer::new(&store);
    assert!(
        matches!(next.materialize_selection(None, &snapshots[0]), Err(SnapshotError::Integrity(found)) if found == *cid)
    );
    drop(next);
    // Generic incoming writes retain their independent verification contract.
    let materializer = Materializer::new(&store);
    assert!(materializer.put_keyed(cid, bytes).is_err());
    let other = MemoryBlockstore::new();
    assert!(matches!(
        hydrate_selection(&other, receipt.root()),
        Err(SnapshotError::MissingBlock(_))
    ));
    let mut fresh = Materializer::new(&other);
    fresh.materialize_selection(None, &snapshots[0]).unwrap();
    let packet = fresh.finish().unwrap();
    assert!(packet
        .blocks()
        .iter()
        .any(|(found, body)| found == cid && body.as_ref() == bytes.as_ref()));
    install(&other, packet);
    assert_eq!(
        hydrate_selection(&other, receipt.root())
            .unwrap()
            .snapshot()
            .as_ref(),
        snapshots[0].as_ref()
    );
}

#[test]
fn materialization_retains_exact_owners_once_and_releases_them_at_finish() {
    let (proof, snapshots) = shared_scope_snapshots();
    let snapshot = Arc::clone(&snapshots[0]);
    drop(snapshots);
    let snapshot_owner = Arc::downgrade(&snapshot);
    let scope_owner = Arc::downgrade(&proof.0);
    let store = MemoryBlockstore::new();
    let mut materializer = Materializer::new(&store);
    materializer.materialize_selection(None, &snapshot).unwrap();
    let snapshot_refs = Arc::strong_count(&snapshot);
    let scope_refs = Arc::strong_count(&proof.0);
    for _ in 0..64 {
        materializer.materialize_selection(None, &snapshot).unwrap();
    }
    assert_eq!(Arc::strong_count(&snapshot), snapshot_refs);
    assert_eq!(Arc::strong_count(&proof.0), scope_refs);
    drop(snapshot);
    drop(proof);
    assert!(snapshot_owner.upgrade().is_some());
    assert!(scope_owner.upgrade().is_some());
    let packet = materializer.finish().unwrap();
    assert!(snapshot_owner.upgrade().is_none());
    assert!(scope_owner.upgrade().is_none());
    assert!(!packet.blocks().is_empty());
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(48))]
    #[test]
    fn branching_incremental_roots_match_btree_oracle(
        operations in prop::collection::vec((any::<u16>(), 0u8..80, prop::option::of(any::<u16>())), 1..90)
    ) {
        let store = MemoryBlockstore::new();
        let empty = OrdMap::<u64, u16>::new();
        let mut materializer = Materializer::new(&store);
        let empty_root = materializer.materialize_map(None, &empty).unwrap();
        install(&store, materializer.finish().unwrap());
        let mut history = vec![(empty_root, BTreeMap::<u64,u16>::new())];
        for (branch, key, value) in operations {
            let (base, oracle) = &history[usize::from(branch) % history.len()];
            let mut next = base.facts().clone();
            let mut expected = oracle.clone();
            match value {
                Some(value) => { next.insert(u64::from(key), value); expected.insert(u64::from(key), value); }
                None => { next.remove(&u64::from(key)); expected.remove(&u64::from(key)); }
            }
            let mut materializer = Materializer::new(&store);
            let root = materializer.materialize_map(Some(base), &next).unwrap();
            install(&store, materializer.finish().unwrap());
            let canonical: OrdMap<u64,u16> = expected.iter().map(|(k,v)| (*k,*v)).collect();
            let mut materializer = Materializer::new(&store);
            let canonical_root = materializer.materialize_map(None, &canonical).unwrap();
            prop_assert_eq!(root.root(), canonical_root.root());
            let hydrated: MaterializedMap<u64,u16> = hydrate_map(&store, root.root()).unwrap();
            prop_assert_eq!(hydrated.facts().iter().map(|(k,v)| (*k,*v)).collect::<Vec<_>>(), expected.iter().map(|(k,v)| (*k,*v)).collect::<Vec<_>>());
            for selected in [u64::from(key), 81] {
                prop_assert_eq!(lookup_map::<_,u16,_>(&store, root.root(), &selected).unwrap(), expected.get(&selected).copied());
                prop_assert_eq!(lookup_map::<_,u16,_>(&store, root.root(), &selected).unwrap(), hydrated.facts().get(&selected).copied());
            }
            prop_assert_eq!(base.facts().iter().map(|(k,v)| (*k,*v)).collect::<Vec<_>>(), oracle.iter().map(|(k,v)| (*k,*v)).collect::<Vec<_>>());
            history.push((root,expected));
        }
        // Every retained branch survives later writes unchanged.
        for (root,expected) in history {
            let hydrated: MaterializedMap<u64,u16> = hydrate_map(&store,root.root()).unwrap();
            prop_assert_eq!(hydrated.facts().iter().map(|(k,v)| (*k,*v)).collect::<Vec<_>>(),expected.into_iter().collect::<Vec<_>>());
        }
    }
}

#[test]
fn unchanged_snapshot_stages_no_blocks_or_entries() {
    fn shared<T: Send + Sync>() {}
    shared::<StagedBlocks>();
    shared::<ExactSelectionSnapshot>();
    let store = MemoryBlockstore::new();
    let facts: OrdMap<u64, u64> = (0u64..1024).map(|key| (key, key)).collect();
    let mut materializer = Materializer::new(&store);
    let root = materializer.materialize_map(None, &facts).unwrap();
    install(&store, materializer.finish().unwrap());
    let clone = facts.clone();
    let mut materializer = Materializer::new(&store);
    assert_eq!(
        materializer
            .materialize_map(Some(&root), &clone)
            .unwrap()
            .root(),
        root.root()
    );
    assert_eq!(
        materializer.finish().unwrap().metrics(),
        MaterializationMetrics::default()
    );
}

#[test]
fn materialization_metrics_account_for_existing_cid_reuse_and_validation() {
    let store = MemoryBlockstore::new();
    let value = (42u64, "immutable leaf");
    let encoded = fvm_ipld_encoding::to_vec(&value).unwrap();
    let bytes = encoded.len() as u64;
    let mut materializer = Materializer::new(&store);
    let cid = materializer.materialize_value(&value).unwrap();
    let packet = materializer.finish().unwrap();
    assert_eq!(
        packet.metrics(),
        MaterializationMetrics {
            blocks: 1,
            staged_bytes: bytes,
            offered_bytes: bytes,
            serialized_bytes: bytes,
            cid_validation_bytes: bytes,
            changed_entries: 0,
        }
    );
    install(&store, packet);

    let mut materializer = Materializer::new(&store);
    assert_eq!(materializer.materialize_value(&value).unwrap(), cid);
    assert_eq!(materializer.get(&cid).unwrap().unwrap(), encoded);
    // An already encoded immutable leaf is offered separately; it does not
    // increment the value serializer's counter.
    materializer.put_keyed(&cid, &encoded).unwrap();
    assert_eq!(
        materializer.finish().unwrap().metrics(),
        MaterializationMetrics {
            blocks: 0,
            staged_bytes: 0,
            offered_bytes: 2 * bytes,
            serialized_bytes: bytes,
            cid_validation_bytes: 5 * bytes,
            changed_entries: 0,
        }
    );
}

#[test]
fn small_edit_does_not_encode_history_and_tampering_is_refused() {
    let store = MemoryBlockstore::new();
    let base: OrdMap<u64, u64> = (0u64..4096).map(|key| (key, key)).collect();
    let mut materializer = Materializer::new(&store);
    let root = materializer.materialize_map(None, &base).unwrap();
    let baseline = materializer.finish().unwrap();
    let baseline_bytes = baseline.metrics().staged_bytes;
    let baseline_offered = baseline.metrics().offered_bytes;
    install(&store, baseline);
    let mut next = base.clone();
    next.insert(2000u64, 9999u64);
    let mut materializer = Materializer::new(&store);
    let edited = materializer.materialize_map(Some(&root), &next).unwrap();
    let packet = materializer.finish().unwrap();
    assert_eq!(packet.metrics().changed_entries, 1);
    assert!(packet.metrics().staged_bytes < baseline_bytes / 8);
    assert!(packet.metrics().offered_bytes < baseline_offered / 8);
    assert!(packet.metrics().blocks < 12);
    eprintln!(
        "4096 facts: baseline_bytes={baseline_bytes}, edit={:?}",
        packet.metrics()
    );
    install(&store, packet);
    let mut corrupted = store.get(edited.root().cid()).unwrap().unwrap();
    corrupted.push(0);
    store.put_keyed(edited.root().cid(), &corrupted).unwrap();
    assert!(matches!(
        hydrate_map::<u64, u64, _>(&store, edited.root()),
        Err(SnapshotError::Integrity(_))
    ));
    assert!(matches!(
        read_value::<u64, _>(&MemoryBlockstore::new(), edited.root().cid()),
        Err(SnapshotError::MissingBlock(_))
    ));
}

#[test]
fn existing_key_corruption_and_storage_errors_keep_their_types() {
    let store = MemoryBlockstore::new();
    let mut materializer = Materializer::new(&store);
    let cid = materializer.materialize_value(&42u64).unwrap();
    install(&store, materializer.finish().unwrap());
    assert!(matches!(
        hydrate_map::<u64, u64, _>(&store, &SnapshotMapRoot(cid)),
        Err(SnapshotError::Malformed(_))
    ));
    store.put_keyed(&cid, &[0]).unwrap();
    let mut materializer = Materializer::new(&store);
    assert!(
        matches!(materializer.materialize_value(&42u64), Err(SnapshotError::Integrity(found)) if found == cid)
    );
    struct Refused;
    impl Serialize for Refused {
        fn serialize<S: serde::Serializer>(&self, _: S) -> Result<S::Ok, S::Error> {
            Err(serde::ser::Error::custom("refused leaf encoding"))
        }
    }
    assert!(matches!(
        materializer.materialize_value(&Refused),
        Err(SnapshotError::Malformed(_))
    ));
    struct BrokenStore;
    impl Blockstore for BrokenStore {
        fn get(&self, _: &Cid) -> anyhow::Result<Option<Vec<u8>>> {
            Err(std::io::Error::from(std::io::ErrorKind::PermissionDenied).into())
        }
        fn put_keyed(&self, _: &Cid, _: &[u8]) -> anyhow::Result<()> {
            unreachable!()
        }
    }
    let mut materializer = Materializer::new(&BrokenStore);
    assert!(matches!(
        materializer.materialize_value(&42u64),
        Err(SnapshotError::Storage(_))
    ));
    assert!(matches!(
        read_value::<u64, _>(&BrokenStore, &cid),
        Err(SnapshotError::Storage(_))
    ));
    let root = SnapshotMapRoot(cid);
    assert!(matches!(
        hydrate_map::<u64, u64, _>(&BrokenStore, &root),
        Err(SnapshotError::Storage(_))
    ));
    assert!(matches!(
        hydrate_map::<u64, u64, _>(&MemoryBlockstore::new(), &root),
        Err(SnapshotError::MissingBlock(_))
    ));
    struct ClassifiedStore;
    impl Blockstore for ClassifiedStore {
        fn get(&self, cid: &Cid) -> anyhow::Result<Option<Vec<u8>>> {
            Err(SnapshotError::Integrity(*cid).into())
        }
        fn put_keyed(&self, _: &Cid, _: &[u8]) -> anyhow::Result<()> {
            unreachable!()
        }
    }
    assert!(matches!(
        hydrate_map::<u64, u64, _>(&ClassifiedStore, &root),
        Err(SnapshotError::Integrity(_))
    ));
}

#[test]
fn selected_lookup_authenticates_only_its_route_and_preserves_refusals() {
    struct ObservedStore<'a> {
        inner: &'a MemoryBlockstore,
        reads: RefCell<std::collections::BTreeSet<Cid>>,
    }
    impl Blockstore for ObservedStore<'_> {
        fn get(&self, cid: &Cid) -> anyhow::Result<Option<Vec<u8>>> {
            self.reads.borrow_mut().insert(*cid);
            self.inner.get(cid)
        }
        fn put_keyed(&self, _: &Cid, _: &[u8]) -> anyhow::Result<()> {
            unreachable!()
        }
    }
    let store = MemoryBlockstore::new();
    let facts: OrdMap<u64, u64> = (0u64..4096).map(|key| (key, key + 100)).collect();
    let mut materializer = Materializer::new(&store);
    let root = materializer.materialize_map(None, &facts).unwrap();
    let blocks = materializer.finish().unwrap();
    let total_blocks = blocks.blocks().len();
    install(&store, blocks);
    let observed = ObservedStore {
        inner: &store,
        reads: RefCell::new(Default::default()),
    };
    assert_eq!(
        lookup_map::<_, u64, _>(&observed, root.root(), &2000u64).unwrap(),
        Some(2100)
    );
    let route = observed.reads.into_inner();
    assert!(
        route.len() < total_blocks / 8,
        "selected route {route:?}, map blocks {total_blocks}"
    );
    let selected_only = MemoryBlockstore::new();
    for cid in &route {
        selected_only
            .put_keyed(cid, &store.get(cid).unwrap().unwrap())
            .unwrap();
    }
    assert_eq!(
        lookup_map::<_, u64, _>(&selected_only, root.root(), &2000u64).unwrap(),
        Some(2100)
    );
    assert!(
        matches!(
            hydrate_map::<u64, u64, _>(&selected_only, root.root()),
            Err(SnapshotError::MissingBlock(_))
        ),
        "selected observation must not imply complete snapshot availability"
    );
    assert_eq!(
        lookup_map::<_, u64, _>(&store, root.root(), &5000u64).unwrap(),
        None
    );
    for selected_block in &route {
        let missing = MemoryBlockstore::new();
        for cid in route.iter().filter(|cid| *cid != selected_block) {
            missing
                .put_keyed(cid, &store.get(cid).unwrap().unwrap())
                .unwrap();
        }
        assert!(
            matches!(lookup_map::<_,u64,_>(&missing, root.root(), &2000u64),
            Err(SnapshotError::MissingBlock(cid)) if cid == *selected_block)
        );
        missing
            .put_keyed(selected_block, b"corrupt selected block")
            .unwrap();
        assert!(
            matches!(lookup_map::<_,u64,_>(&missing, root.root(), &2000u64),
            Err(SnapshotError::Integrity(cid)) if cid == *selected_block)
        );
    }
    struct BrokenStore;
    impl Blockstore for BrokenStore {
        fn get(&self, _: &Cid) -> anyhow::Result<Option<Vec<u8>>> {
            Err(std::io::Error::from(std::io::ErrorKind::PermissionDenied).into())
        }
        fn put_keyed(&self, _: &Cid, _: &[u8]) -> anyhow::Result<()> {
            unreachable!()
        }
    }
    assert!(matches!(
        lookup_map::<_, u64, _>(&BrokenStore, root.root(), &2000u64),
        Err(SnapshotError::Storage(_))
    ));
}

#[test]
fn old_selection_and_context_versions_are_rejected() {
    let store = MemoryBlockstore::new();
    let mut materializer = Materializer::new(&store);
    let selection = materializer
        .materialize_selection(None, &Arc::new(ExactSelectionSnapshot::default()))
        .unwrap();
    install(&store, materializer.finish().unwrap());
    assert!(hydrate_selection(&store, selection.root()).is_ok());
    let mut descriptor: SelectionDescriptor = read_value(&store, selection.root().cid()).unwrap();
    assert_eq!(descriptor.version, 2);
    descriptor.version = 1;
    let mut materializer = Materializer::new(&store);
    let old_root = SelectionRoot(materializer.materialize_value(&descriptor).unwrap());
    install(&store, materializer.finish().unwrap());
    assert!(
        matches!(hydrate_selection(&store, &old_root), Err(SnapshotError::Malformed(message)) if message == "selection encoding version")
    );
    let mut materializer = Materializer::new(&store);
    let roles = materializer
        .materialize_map::<ArtifactId, u64>(None, &OrdMap::new())
        .unwrap();
    let lexical = materializer
        .materialize_map::<String, u64>(None, &OrdMap::new())
        .unwrap();
    install(&store, materializer.finish().unwrap());
    for version in [2, 3, 4] {
        let mut materializer = Materializer::new(&store);
        let descriptor = ContextDescriptor {
            version,
            producer: [0; 32],
            selection: selection.root().clone(),
            roles: roles.root().clone(),
            lexical: lexical.root().clone(),
            offers: lexical.root().clone(),
            native_availability: lexical.root().clone(),
            input_decisions: None,
        };
        let old_root = ContextRoot(materializer.materialize_value(&descriptor).unwrap());
        install(&store, materializer.finish().unwrap());
        assert!(
            matches!(hydrate_context(&store, &old_root), Err(SnapshotError::Malformed(message)) if message == "context encoding requires version 5 with normalized native availability")
        );
    }
}

#[test]
fn native_offer_classes_round_trip_and_change_context_identity() {
    let store = MemoryBlockstore::new();
    let selection = crate::artifact_inventory::ExactArtifactSelection::from_snapshot(Arc::new(
        ExactSelectionSnapshot::default(),
    ));
    let mut snapshot = ExactContextSnapshot::empty();
    for body in [
        NativeOfferSnapshotBody::Empty(selection.clone()),
        NativeOfferSnapshotBody::Partial(selection.clone()),
        NativeOfferSnapshotBody::Full(selection),
    ] {
        let fact = NativeOfferSnapshotFacts {
            original: ArtifactId([1; 32]),
            interface: ArtifactId([2; 32]),
            owner: ExactModuleIdentity {
                unit: "home".into(),
                module: "Offer".into(),
            },
            body,
        };
        snapshot.offers.insert(fact.identity().unwrap(), fact);
    }
    let mut reordered = snapshot.clone();
    reordered.offers = snapshot
        .offers
        .values()
        .rev()
        .cloned()
        .chain(snapshot.offers.values().cloned())
        .map(|fact| (fact.identity().unwrap(), fact))
        .collect();
    assert_eq!(reordered.offers.len(), 3);
    let mut canonical = Materializer::new(&store);
    let reordered_receipt = canonical.materialize_context(None, &reordered).unwrap();
    install(&store, canonical.finish().unwrap());
    let mut materializer = Materializer::new(&store);
    let receipt = materializer.materialize_context(None, &snapshot).unwrap();
    install(&store, materializer.finish().unwrap());
    assert_eq!(receipt.root(), reordered_receipt.root());
    let restored = hydrate_context(&store, receipt.root()).unwrap();
    assert_eq!(restored.snapshot().offers, snapshot.offers);
    let offered_selections = restored
        .snapshot()
        .native_offers()
        .map(|fact| match &fact.body {
            NativeOfferSnapshotBody::Empty(selection)
            | NativeOfferSnapshotBody::Partial(selection)
            | NativeOfferSnapshotBody::Full(selection) => selection.snapshot(),
        })
        .collect::<Vec<_>>();
    assert_eq!(offered_selections.len(), 3);
    for selection in &offered_selections {
        assert!(Arc::ptr_eq(selection, restored.snapshot().selection()));
    }

    let mut materializer = Materializer::new(&store);
    let same = materializer
        .materialize_context(Some(&restored), &snapshot)
        .unwrap();
    assert_eq!(same.root(), receipt.root());
    assert_eq!(
        materializer.finish().unwrap().metrics(),
        MaterializationMetrics::default()
    );
    let mut changed = snapshot.clone();
    let full = changed
        .offers
        .iter()
        .find(|(_, fact)| matches!(fact.body, NativeOfferSnapshotBody::Full(_)))
        .map(|(key, _)| *key)
        .unwrap();
    changed.offers.remove(&full);
    let mut materializer = Materializer::new(&store);
    let changed = materializer
        .materialize_context(Some(&restored), &changed)
        .unwrap();
    assert_ne!(changed.root(), receipt.root());
    install(&store, materializer.finish().unwrap());
    assert_eq!(
        hydrate_context(&store, changed.root())
            .unwrap()
            .snapshot()
            .offers
            .len(),
        2
    );
}

#[test]
fn recorded_input_domain_distinguishes_empty_from_absent_without_issuing_authority() {
    let store = MemoryBlockstore::new();
    let owner = ExactModuleIdentity {
        unit: "home".into(),
        module: "RecordedOnly".into(),
    };
    // These are untrusted wire facts. Hydration is deliberately not a compiler
    // admission and this test never constructs an issued decision owner.
    let mut snapshot = ExactContextSnapshot::empty();
    let mut writer = Materializer::new(&store);
    let absent = writer.materialize_context(None, &snapshot).unwrap();
    install(&store, writer.finish().unwrap());
    snapshot.input_decisions.insert(
        owner.clone(),
        crate::artifact_inventory::RecordedInputNativeDecision {
            role: CompilerInputRole::ReusableOriginal {
                interface: ArtifactId([1; 32]),
                original: ArtifactId([2; 32]),
            },
            groups: OrdMap::new(),
        },
    );
    let mut writer = Materializer::new(&store);
    let empty = writer
        .materialize_context(Some(&absent), &snapshot)
        .unwrap();
    install(&store, writer.finish().unwrap());
    assert_ne!(absent.root(), empty.root());
    let reopened = hydrate_context(&store, empty.root()).unwrap();
    assert_eq!(
        reopened.snapshot().input_decisions,
        snapshot.input_decisions
    );
    assert!(reopened.snapshot().input_decisions[&owner]
        .groups
        .is_empty());
    assert!(hydrate_context(&store, absent.root())
        .unwrap()
        .snapshot()
        .input_decisions
        .is_empty());
}

#[test]
fn input_decision_codec_refuses_non_original_roles_and_duplicate_ordinals() {
    let store = MemoryBlockstore::new();
    let mut writer = Materializer::new(&store);
    let no_original = CompilerRoleCodec::encode(
        &mut writer,
        &CompilerInputRole::InterfaceOnly {
            interface: ArtifactId([1; 32]),
        },
    )
    .unwrap();
    let original = CompilerRoleCodec::encode(
        &mut writer,
        &CompilerInputRole::ReusableOriginal {
            interface: ArtifactId([1; 32]),
            original: ArtifactId([2; 32]),
        },
    )
    .unwrap();
    let group: crate::artifact_inventory::NativeGroupBinding = serde_json::from_value(serde_json::json!({
        "binding": { "artifact": vec![2u8; 32], "selection": vec![3u8; 32] }, "original_ordinal": 7
    })).unwrap();
    let interface_fact = writer
        .materialize_value(&InputNativeDecisionWire {
            role: no_original,
            groups: vec![],
        })
        .unwrap();
    let duplicate_fact = writer
        .materialize_value(&InputNativeDecisionWire {
            role: original,
            groups: vec![group, group],
        })
        .unwrap();
    install(&store, writer.finish().unwrap());
    let reader = SnapshotHydrator::new(&store);
    assert!(
        matches!(InputNativeDecisionCodec::decode(&reader, interface_fact), Err(SnapshotError::Malformed(message)) if message == "input decision has no original role")
    );
    assert!(
        matches!(InputNativeDecisionCodec::decode(&reader, duplicate_fact), Err(SnapshotError::Malformed(message)) if message == "input decision repeats an ordinal")
    );
}

// Serializer fixtures are untrusted facts; they never enter a native issuer.
fn availability_wire_snapshot(
    mode: NativeTransportMode,
    reachable: Option<Vec<ArtifactBindingNode>>,
) -> ExactContextSnapshot {
    let original = ArtifactId([1; 32]);
    let offer = NativeOfferSnapshotFacts {
        original,
        interface: ArtifactId([2; 32]),
        owner: ExactModuleIdentity {
            unit: "home".into(),
            module: "Availability".into(),
        },
        body: NativeOfferSnapshotBody::Full(
            crate::artifact_inventory::ExactArtifactSelection::from_snapshot(Arc::new(
                ExactSelectionSnapshot::default(),
            )),
        ),
    };
    let offer_id = offer.identity().unwrap();
    let mut snapshot = ExactContextSnapshot::empty();
    snapshot.offers.insert(offer_id, offer);
    let mut contributions = OrdMap::new();
    if let Some(reachable) = reachable {
        let fact = NativeContributionSnapshotFacts {
            offer: offer_id,
            reachable: reachable.into_iter().collect(),
        };
        contributions.insert(fact.identity().unwrap(), fact);
    }
    snapshot.native_availability.insert(
        original,
        NativeAvailabilitySnapshotFacts {
            original,
            owner: tidepool_repr::execution_schema::CachedHomeOwner {
                unit: "home".into(),
                module: "Availability".into(),
                module_version: tidepool_repr::execution_schema::ModuleVersion([3; 32]),
                skinny_iface_sha256: [4; 32],
                product_sha256: [5; 32],
            },
            mode,
            contributions,
        },
    );
    snapshot
}

#[test]
fn normalized_availability_wire_preserves_modes_membership_and_unchanged_sharing() {
    let store = MemoryBlockstore::new();
    let nodes = (0..64u8)
        .map(|byte| {
            ArtifactBindingNode::Artifact(ArtifactBinding {
                artifact: ArtifactId([byte; 32]),
                selection: ArtifactSelectionId([9; 32]),
            })
        })
        .collect::<Vec<_>>();
    let mut roots = Vec::new();
    for snapshot in [
        ExactContextSnapshot::empty(),
        availability_wire_snapshot(NativeTransportMode::IssuedGroups, None),
        availability_wire_snapshot(NativeTransportMode::IssuedGroups, Some(nodes.clone())),
        availability_wire_snapshot(NativeTransportMode::Full, Some(nodes.clone())),
    ] {
        let mut writer = Materializer::new(&store);
        let receipt = writer.materialize_context(None, &snapshot).unwrap();
        install(&store, writer.finish().unwrap());
        let reopened = hydrate_context(&store, receipt.root()).unwrap();
        assert_eq!(
            reopened.snapshot().native_availability,
            snapshot.native_availability
        );
        let mut writer = Materializer::new(&store);
        let same = writer
            .materialize_context(Some(&reopened), &snapshot)
            .unwrap();
        assert_eq!(same.root(), receipt.root());
        assert_eq!(
            writer.finish().unwrap().metrics(),
            MaterializationMetrics::default()
        );
        roots.push(receipt.root().clone());
    }
    assert_eq!(
        roots
            .iter()
            .collect::<std::collections::BTreeSet<_>>()
            .len(),
        4
    );

    let subset = availability_wire_snapshot(NativeTransportMode::IssuedGroups, Some(nodes));
    let mut full = subset.clone();
    full.native_availability
        .get_mut(&ArtifactId([1; 32]))
        .unwrap()
        .mode = NativeTransportMode::Full;
    let mut writer = Materializer::new(&store);
    let subset_row = NativeAvailabilityCodec::encode(
        &mut writer,
        &subset.native_availability[&ArtifactId([1; 32])],
    )
    .unwrap();
    let full_row = NativeAvailabilityCodec::encode(
        &mut writer,
        &full.native_availability[&ArtifactId([1; 32])],
    )
    .unwrap();
    install(&store, writer.finish().unwrap());
    let reader = SnapshotHydrator::new(&store);
    let subset_wire: NativeAvailabilityWire = read_value(&reader, &subset_row).unwrap();
    let full_wire: NativeAvailabilityWire = read_value(&reader, &full_row).unwrap();
    assert_ne!(subset_row, full_row);
    assert_eq!(
        subset_wire.contributions, full_wire.contributions,
        "mode changes reuse the contribution and membership maps"
    );
}

#[test]
fn normalized_availability_hydration_refuses_broken_structural_references() {
    let store = MemoryBlockstore::new();
    for wrong_row_key in [false, true] {
        let mut snapshot =
            availability_wire_snapshot(NativeTransportMode::IssuedGroups, Some(vec![]));
        if wrong_row_key {
            let row = snapshot
                .native_availability
                .remove(&ArtifactId([1; 32]))
                .unwrap();
            snapshot
                .native_availability
                .insert(ArtifactId([99; 32]), row);
        } else {
            snapshot.offers.clear();
        }
        let mut writer = Materializer::new(&store);
        let receipt = writer.materialize_context(None, &snapshot).unwrap();
        install(&store, writer.finish().unwrap());
        let expected = if wrong_row_key {
            "native availability key differs from original"
        } else {
            "native availability references a missing offer"
        };
        assert!(matches!(hydrate_context(&store, receipt.root()),
            Err(SnapshotError::Malformed(message)) if message == expected));
    }
    let mut snapshot = availability_wire_snapshot(NativeTransportMode::IssuedGroups, Some(vec![]));
    let row = snapshot
        .native_availability
        .get_mut(&ArtifactId([1; 32]))
        .unwrap();
    let fact = row.contributions.values().next().unwrap().clone();
    row.contributions = OrdMap::new();
    row.contributions.insert([99; 32], fact);
    let mut writer = Materializer::new(&store);
    let receipt = writer.materialize_context(None, &snapshot).unwrap();
    install(&store, writer.finish().unwrap());
    assert!(matches!(hydrate_context(&store, receipt.root()),
        Err(SnapshotError::Malformed(message)) if message == "native contribution key differs from fact identity"));

    let mut writer = Materializer::new(&store);
    let broken = writer
        .materialize_value(&NativeContributionWire {
            offer: [1; 32],
            reachable: SnapshotMapRoot(Cid::new_v1(
                DAG_CBOR,
                Code::Blake2b256.digest(b"absent membership"),
            )),
        })
        .unwrap();
    install(&store, writer.finish().unwrap());
    assert!(NativeContributionCodec::decode(&SnapshotHydrator::new(&store), broken).is_err());
}

#[test]
fn context_v5_requires_normalized_availability_root() {
    #[derive(Serialize)]
    struct MissingAvailability {
        version: u32,
        producer: [u8; 32],
        selection: SelectionRoot,
        roles: SnapshotMapRoot,
        lexical: SnapshotMapRoot,
        offers: SnapshotMapRoot,
        input_decisions: Option<SnapshotMapRoot>,
    }
    let store = MemoryBlockstore::new();
    let mut writer = Materializer::new(&store);
    let valid = writer
        .materialize_context(None, &ExactContextSnapshot::empty())
        .unwrap();
    let missing = ContextRoot(
        writer
            .materialize_value(&MissingAvailability {
                version: 5,
                producer: [0; 32],
                selection: valid.selection.root.clone(),
                roles: valid.roles.root.clone(),
                lexical: valid.lexical.root.clone(),
                offers: valid.offers.root.clone(),
                input_decisions: Some(valid.input_decisions.root.clone()),
            })
            .unwrap(),
    );
    install(&store, writer.finish().unwrap());
    assert!(
        matches!(hydrate_context(&store, &missing), Err(SnapshotError::Malformed(message))
        if message.contains("native_availability"))
    );
}

#[test]
fn normalized_dependency_rows_reference_distinct_root_offer_issuers() {
    use crate::artifact_inventory::{
        ArtifactEntry, ArtifactInventory, IssuedNativeOffer, NativeGroupKey,
        NativeOriginalRequirement,
    };
    use crate::certified_products::tests::{
        original_groups_fixture_with_interface, recovered_witness_fixtures,
    };
    use crate::certified_products::{
        fixture_finalized_product, original_native_group_definitions, PendingImportOwner,
    };
    let producer = [2; 32];
    let hidden = recovered_witness_fixtures(&[fixture_finalized_product(
        original_groups_fixture_with_interface(
            "CodecHidden",
            vec![(7, vec![]), (9, vec![])],
            7,
            &BTreeMap::new(),
            b"CodecHidden".to_vec(),
        ),
        producer,
    )])
    .remove(0)
    .product;
    let source = |ordinal| {
        let definitions = original_native_group_definitions(&hidden, ordinal).unwrap();
        let binding = match &definitions.bindings()[0] {
            tidepool_repr::execution_schema::Group::NonRecursive(binding) => binding,
            tidepool_repr::execution_schema::Group::Recursive(bindings) => &bindings[0],
        };
        PendingImportOwner::Source {
            owner: hidden.owner().clone(),
            original_ordinal: ordinal,
            binder: binding.identity.clone(),
        }
    };
    let root = recovered_witness_fixtures(&[fixture_finalized_product(
        original_groups_fixture_with_interface(
            "CodecRoot",
            vec![(0, vec![source(7)]), (1, vec![source(9)])],
            8,
            &BTreeMap::new(),
            b"CodecRoot".to_vec(),
        ),
        producer,
    )])
    .remove(0)
    .product;
    let root = Arc::new(ArtifactEntry::original(producer, root).unwrap());
    let hidden_entry = Arc::new(ArtifactEntry::original(producer, hidden.clone()).unwrap());
    let inventory = ArtifactInventory::default();
    let mut snapshot = ExactContextSnapshot::empty();
    let mut contributions = OrdMap::new();
    let mut observed = std::collections::BTreeSet::new();
    for (root_ordinal, hidden_ordinal) in [(0, 7), (1, 9)] {
        let view = inventory
            .admit_recovery_selection(
                &inventory.empty_view(),
                vec![root.clone(), hidden_entry.clone()],
                &std::collections::BTreeSet::from([
                    NativeGroupKey {
                        artifact: root.descriptor.id,
                        original_ordinal: root_ordinal,
                    },
                    NativeGroupKey {
                        artifact: hidden_entry.descriptor.id,
                        original_ordinal: hidden_ordinal,
                    },
                ]),
            )
            .unwrap();
        let proof = NativeOriginalRequirement::from_selected_view(root.descriptor.id, &view)
            .unwrap()
            .unwrap();
        let offer = IssuedNativeOffer::issue(root.descriptor.id, &view, Some(proof)).unwrap();
        let facts = offer.snapshot_facts();
        let offer_id = facts.identity().unwrap();
        assert_ne!(facts.original, hidden_entry.descriptor.id);
        let contribution = offer
            .contribution_for(&std::collections::BTreeSet::from([hidden_entry
                .descriptor
                .id]))
            .unwrap()
            .unwrap();
        let groups = contribution
            .groups(NativeGroupKey {
                artifact: hidden_entry.descriptor.id,
                original_ordinal: hidden_ordinal,
            })
            .collect::<Vec<_>>();
        assert_eq!(groups.len(), 1);
        observed.insert(groups[0].original_ordinal);
        let contribution = contribution.snapshot_facts().unwrap();
        assert_eq!(contribution.offer, offer_id);
        assert!(contribution.reachable.iter().any(|node| matches!(node,
            ArtifactBindingNode::Artifact(binding) if binding.artifact == hidden_entry.descriptor.id)));
        contributions.insert(contribution.identity().unwrap(), contribution);
        snapshot.offers.insert(offer_id, facts);
    }
    assert_eq!(observed, std::collections::BTreeSet::from([7, 9]));
    assert_eq!(contributions.len(), 2);
    snapshot.native_availability.insert(
        hidden_entry.descriptor.id,
        NativeAvailabilitySnapshotFacts {
            original: hidden_entry.descriptor.id,
            owner: hidden.owner().clone(),
            mode: NativeTransportMode::IssuedGroups,
            contributions,
        },
    );
    let store = MemoryBlockstore::new();
    let mut writer = Materializer::new(&store);
    let receipt = writer.materialize_context(None, &snapshot).unwrap();
    install(&store, writer.finish().unwrap());
    assert_eq!(
        hydrate_context(&store, receipt.root())
            .unwrap()
            .snapshot()
            .native_availability,
        snapshot.native_availability
    );
}

#[test]
fn accumulating_availability_encodes_only_the_added_membership() {
    let store = MemoryBlockstore::new();
    let mut snapshot = availability_wire_snapshot(NativeTransportMode::IssuedGroups, None);
    let offer = *snapshot.offers.keys().next().unwrap();
    let mut writer = Materializer::new(&store);
    let mut previous = writer.materialize_context(None, &snapshot).unwrap();
    install(&store, writer.finish().unwrap());
    for generation in 0u8..8 {
        let fact = NativeContributionSnapshotFacts {
            offer,
            reachable: (0u8..8)
                .map(|byte| {
                    ArtifactBindingNode::Artifact(ArtifactBinding {
                        artifact: ArtifactId([byte; 32]),
                        selection: ArtifactSelectionId([generation; 32]),
                    })
                })
                .collect(),
        };
        snapshot
            .native_availability
            .get_mut(&ArtifactId([1; 32]))
            .unwrap()
            .contributions
            .insert(fact.identity().unwrap(), fact);
        let mut writer = Materializer::new(&store);
        let next = writer
            .materialize_context(Some(&previous), &snapshot)
            .unwrap();
        let packet = writer.finish().unwrap();
        assert_eq!(
            packet.metrics().changed_entries,
            10,
            "only eight new membership entries, one contribution and one row are encoded"
        );
        install(&store, packet);
        assert_eq!(
            hydrate_context(&store, next.root())
                .unwrap()
                .snapshot()
                .native_availability,
            snapshot.native_availability
        );
        previous = next;
    }
}
