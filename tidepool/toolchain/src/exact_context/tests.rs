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
        .materialize_selection(None, &ExactSelectionSnapshot::default())
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
    for version in [2, 3] {
        let mut materializer = Materializer::new(&store);
        let descriptor = ContextDescriptor {
            version,
            producer: [0; 32],
            selection: selection.root().clone(),
            roles: roles.root().clone(),
            lexical: lexical.root().clone(),
            offers: lexical.root().clone(),
            input_decisions: None,
        };
        let old_root = ContextRoot(materializer.materialize_value(&descriptor).unwrap());
        install(&store, materializer.finish().unwrap());
        assert!(
            matches!(hydrate_context(&store, &old_root), Err(SnapshotError::Malformed(message)) if message == "context encoding requires version 4 with issued input decisions")
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
