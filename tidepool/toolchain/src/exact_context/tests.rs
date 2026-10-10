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
        operations in prop::collection::vec((0u8..16, 0u8..80, prop::option::of(any::<u16>())), 1..90)
    ) {
        let store = MemoryBlockstore::new();
        let empty = OrdMap::<u64, u16>::new();
        let mut materializer = Materializer::new(&store);
        let empty_root = materializer.materialize_map(None, &empty).unwrap();
        install(&store, materializer.finish().unwrap());
        let mut history = vec![(empty_root, empty, BTreeMap::<u64,u16>::new())];
        for (branch, key, value) in operations {
            let (base_root, base, oracle) = &history[usize::from(branch) % history.len()];
            let mut next = base.clone();
            let mut expected = oracle.clone();
            match value {
                Some(value) => { next.insert(u64::from(key), value); expected.insert(u64::from(key), value); }
                None => { next.remove(&u64::from(key)); expected.remove(&u64::from(key)); }
            }
            let mut materializer = Materializer::new(&store);
            let root = materializer.materialize_map(Some((base_root, base)), &next).unwrap();
            install(&store, materializer.finish().unwrap());
            let canonical: OrdMap<u64,u16> = expected.iter().map(|(k,v)| (*k,*v)).collect();
            let mut materializer = Materializer::new(&store);
            let canonical_root = materializer.materialize_map(None, &canonical).unwrap();
            prop_assert_eq!(&root, &canonical_root);
            let hydrated: OrdMap<u64,u16> = hydrate_map(&store, &root).unwrap();
            prop_assert_eq!(hydrated.iter().map(|(k,v)| (*k,*v)).collect::<Vec<_>>(), expected.iter().map(|(k,v)| (*k,*v)).collect::<Vec<_>>());
            prop_assert_eq!(base.iter().map(|(k,v)| (*k,*v)).collect::<Vec<_>>(), oracle.iter().map(|(k,v)| (*k,*v)).collect::<Vec<_>>());
            history.push((root,next,expected));
        }
        // Every retained branch survives later writes unchanged.
        for (root,_,expected) in history {
            let hydrated: OrdMap<u64,u16> = hydrate_map(&store,&root).unwrap();
            prop_assert_eq!(hydrated.iter().map(|(k,v)| (*k,*v)).collect::<Vec<_>>(),expected.into_iter().collect::<Vec<_>>());
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
            .materialize_map(Some((&root, &facts)), &clone)
            .unwrap(),
        root
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
    let baseline_bytes = baseline.metrics().encoded_bytes;
    install(&store, baseline);
    let mut next = base.clone();
    next.insert(2000u64, 9999u64);
    let mut materializer = Materializer::new(&store);
    let edited = materializer
        .materialize_map(Some((&root, &base)), &next)
        .unwrap();
    let packet = materializer.finish().unwrap();
    assert_eq!(packet.metrics().changed_entries, 1);
    assert!(packet.metrics().encoded_bytes < baseline_bytes / 8);
    assert!(packet.metrics().blocks < 12);
    eprintln!(
        "4096 facts: baseline_bytes={baseline_bytes}, edit={:?}",
        packet.metrics()
    );
    install(&store, packet);
    let mut corrupted = store.get(edited.cid()).unwrap().unwrap();
    corrupted.push(0);
    assert!(matches!(
        verify_block(edited.cid(), &corrupted),
        Err(SnapshotError::Integrity(_))
    ));
    assert!(matches!(
        read_value::<u64, _>(&MemoryBlockstore::new(), edited.cid()),
        Err(SnapshotError::MissingBlock(_))
    ));
}
