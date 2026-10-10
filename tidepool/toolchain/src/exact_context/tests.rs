use super::*;
use proptest::prelude::*;
use std::collections::BTreeMap;

fn install(store: &MemoryBlockstore, packet: StagedBlocks) {
    for (cid, bytes) in packet.into_blocks() {
        verify_block(&cid, &bytes).unwrap();
        store.put_keyed(&cid, &bytes).unwrap();
    }
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
fn small_edit_does_not_encode_history_and_tampering_is_refused() {
    let store = MemoryBlockstore::new();
    let base: OrdMap<u64, u64> = (0u64..4096).map(|key| (key, key)).collect();
    let mut materializer = Materializer::new(&store);
    let root = materializer.materialize_map(None, &base).unwrap();
    let baseline = materializer.finish().unwrap();
    let baseline_bytes = baseline.metrics().staged_bytes;
    install(&store, baseline);
    let mut next = base.clone();
    next.insert(2000u64, 9999u64);
    let mut materializer = Materializer::new(&store);
    let edited = materializer.materialize_map(Some(&root), &next).unwrap();
    let packet = materializer.finish().unwrap();
    assert_eq!(packet.metrics().changed_entries, 1);
    assert!(packet.metrics().staged_bytes < baseline_bytes / 8);
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
    store.put_keyed(&cid, &[0]).unwrap();
    let mut materializer = Materializer::new(&store);
    assert!(
        matches!(materializer.materialize_value(&42u64), Err(SnapshotError::Integrity(found)) if found == cid)
    );
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
