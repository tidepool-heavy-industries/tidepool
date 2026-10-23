use super::*;
use proptest::prelude::*;
use std::collections::BTreeMap;

#[test]
fn lexical_append_rejects_duplicate_edges_without_changing_held_facts() {
    let owner = ExactModuleIdentity {
        unit: "main".into(),
        module: "Owner".into(),
    };
    let dependency = ExactModuleIdentity {
        unit: "main".into(),
        module: "Dependency".into(),
    };
    let valid = ExactLexicalNode {
        owner: owner.clone(),
        imports: vec![dependency.clone()],
    };
    let malformed = ExactLexicalNode {
        owner: owner.clone(),
        imports: vec![dependency.clone(), dependency.clone()],
    };
    let empty = ExactLexicalSnapshot::default();
    let held = empty.append_issued([valid.clone()]).unwrap();
    for baseline in [&empty, &held] {
        let error = baseline.append_issued([malformed.clone()]).unwrap_err();
        assert!(matches!(&error, crate::CompileError::LexicalGraph(cause)
        if cause.as_ref() == &LexicalGraphError::DuplicateImport {
            owner: owner.clone(), import: dependency.clone(),
        }));
        let envelope = crate::classify_compile(&error);
        assert_eq!(envelope.class, crate::FailureClass::VersionSkew);
        assert_eq!(
            envelope.cause,
            Some(crate::failclass::CompileFailureCause::LexicalGraph {
                failure: LexicalGraphError::DuplicateImport {
                    owner: owner.clone(),
                    import: dependency.clone(),
                },
            })
        );
    }
    assert_eq!(held.get(&owner), Some(&valid));
    assert!(empty.facts.is_empty());
    assert!(matches!(
        empty.replace(vec![malformed]),
        Err(crate::CompileError::LexicalGraph(_))
    ));
}

proptest! {
    #[test]
    fn lexical_composition_preserves_row_identity(
        edges in proptest::collection::btree_set(0u16..128, 0..24),
        repeat in 1usize..12,
    ) {
        let owner = ExactModuleIdentity { unit: "main".into(), module: "Owner".into() };
        let imports = edges.iter().map(|i| ExactModuleIdentity {
            unit: "main".into(), module: format!("Dependency{i}"),
        }).collect::<Vec<_>>();
        let row = ExactLexicalNode { owner: owner.clone(), imports };
        let mut reordered = row.clone();
        reordered.imports.reverse();
        let empty = ExactLexicalSnapshot::default();
        let held = empty.append_issued([row.clone()]).unwrap();
        let repeated = held.append_issued(std::iter::repeat_n(reordered.clone(), repeat)).unwrap();
        prop_assert_eq!(&held, &repeated);
        prop_assert!(Arc::ptr_eq(&held.rows, &repeated.rows));
        let recomputed = empty.replace(vec![row.clone()]).unwrap();
        prop_assert_eq!(&held, &recomputed);
        let duplicate = empty.replace(vec![row.clone(), reordered]).unwrap_err();
        prop_assert!(matches!(duplicate, crate::CompileError::LexicalGraph(cause)
            if matches!(*cause, LexicalGraphError::DuplicateOwner { .. })),
            "raw duplicate owners must refuse");

        let mut changed = row.clone();
        changed.imports.push(ExactModuleIdentity { unit: "other".into(), module: "NewDependency".into() });
        for (first, second) in [(row.clone(), changed.clone()), (changed, row)] {
            let selected = empty.append_issued([first.clone()]).unwrap();
            let expected = selected.get(&owner).unwrap().imports.clone();
            let error = selected.append_issued([second.clone()]).unwrap_err();
            let mut incoming = second.imports;
            incoming.sort();
            prop_assert!(matches!(error, crate::CompileError::LexicalGraph(cause)
                if *cause == LexicalGraphError::ConflictingAdjacency {
                    owner: owner.clone(), previous: expected, incoming,
                }), "conflicting adjacency must retain both exact rows");
            prop_assert_eq!(selected.get(&owner).unwrap().owner.clone(), owner.clone());
        }
    }
}

#[test]
fn lexical_append_shares_unchanged_facts_and_refuses_changed_adjacency() {
    for width in [1, 32, 128] {
        let owner = |index| ExactModuleIdentity {
            unit: "main".into(),
            module: format!("Home{index}"),
        };
        let baseline = ExactLexicalSnapshot::default()
            .replace(
                (0..width)
                    .map(|index| ExactLexicalNode {
                        owner: owner(index),
                        imports: vec![],
                    })
                    .collect(),
            )
            .unwrap();
        let added = ExactLexicalNode {
            owner: owner(width),
            imports: vec![owner(0)],
        };
        LEXICAL_APPEND_WORK.with(|work| work.set((0, 0)));
        let next = baseline.append_issued([added.clone()]).unwrap();
        assert_eq!(LEXICAL_APPEND_WORK.with(Cell::get), (1, 1));
        assert_eq!(next.facts.len(), width + 1);
        assert_eq!(baseline.facts.len(), width);
        for (key, fact) in &baseline.facts {
            assert!(Arc::ptr_eq(&fact.node, &next.facts[key].node));
        }
        let equal = next.append_issued([added.clone()]).unwrap();
        assert!(Arc::ptr_eq(&next.rows, &equal.rows));
        assert!(next
            .append_issued([ExactLexicalNode {
                owner: owner(width),
                imports: vec![]
            }])
            .is_err());
        let recomputed = ExactLexicalSnapshot::default()
            .replace(baseline.iter().cloned().chain([added]).collect())
            .unwrap();
        assert_eq!(next, recomputed);
        LEXICAL_APPEND_WORK.with(|work| work.set((0, 0)));
        let overlap = next
            .append_issued([
                baseline.get(&owner(0)).unwrap().clone(),
                ExactLexicalNode {
                    owner: owner(width + 1),
                    imports: vec![owner(width)],
                },
            ])
            .unwrap();
        assert_eq!(LEXICAL_APPEND_WORK.with(Cell::get), (2, 1));
        assert_eq!(overlap.facts.len(), width + 2);
        assert!(Arc::ptr_eq(
            &next.facts[&owner(0)].node,
            &overlap.facts[&owner(0)].node
        ));
    }
}

#[test]
fn context_source_scope_preserves_distinct_roots_shared_facts_and_incremental_growth() {
    // DATA-only storage controls; these rows do not issue source permission.
    for width in [128, 512] {
        let owner = |index| ExactModuleIdentity {
            unit: "main".into(),
            module: format!("SourceScopeOwner{index}"),
        };
        let rows = (0..=width)
            .map(|index| ExactLexicalNode {
                owner: owner(index),
                imports: vec![],
            })
            .collect::<Vec<_>>();
        let mut first = ExactContextSnapshot::empty();
        first.lexical = ExactLexicalSnapshot::default()
            .replace(rows.clone())
            .unwrap();
        first.source_scope = first.lexical.replace(rows[..width].to_vec()).unwrap();
        let mut second = first.clone();
        second.source_scope = first
            .source_scope
            .append_issued([rows[width].clone()])
            .unwrap();
        assert_eq!(first.lexical, second.lexical);
        assert_ne!(first.source_scope, second.source_scope);

        let store = MemoryBlockstore::new();
        let mut writer = Materializer::new(&store);
        let base = writer.materialize_context(None, &first).unwrap();
        let cold = writer.finish().unwrap();
        let cold_bytes = cold.metrics().offered_bytes;
        install(&store, cold);
        let mut writer = Materializer::new(&store);
        let next = writer.materialize_context(Some(&base), &second).unwrap();
        let delta = writer.finish().unwrap();
        assert_eq!(delta.metrics().changed_entries, 1);
        assert!(delta.metrics().offered_bytes < cold_bytes / 2);
        assert_eq!(base.lexical.root(), next.lexical.root());
        assert_ne!(base.source_scope.root(), next.source_scope.root());
        assert_ne!(base.root(), next.root());
        install(&store, delta);

        let observed = CountingStore {
            inner: &store,
            reads: RefCell::new(BTreeMap::new()),
        };
        let restored =
            hydrate_contexts(&observed, &[base.root().clone(), next.root().clone()]).unwrap();
        assert_eq!(restored[0].snapshot().source_scope, first.source_scope);
        assert_eq!(restored[1].snapshot().source_scope, second.source_scope);
        let shared_owner = owner(0);
        let shared = &restored[0].snapshot().source_scope.facts[&shared_owner];
        assert!(Arc::ptr_eq(
            &shared.node,
            &restored[1].snapshot().source_scope.facts[&shared_owner].node
        ));
        assert!(Arc::ptr_eq(
            &shared.node,
            &restored[1].snapshot().lexical.facts[&shared_owner].node
        ));
        assert_eq!(observed.reads.borrow()[&shared.cid].0, 1);
        let mut writer = Materializer::new(&store);
        let unchanged = writer
            .materialize_context(Some(&restored[1]), restored[1].snapshot())
            .unwrap();
        assert_eq!(unchanged.root(), next.root());
        assert_eq!(
            writer.finish().unwrap().metrics(),
            MaterializationMetrics::default()
        );
    }
}

#[test]
fn context_source_scope_refuses_mismatched_fact_owner() {
    let owner = ExactModuleIdentity {
        unit: "main".into(),
        module: "Actual".into(),
    };
    let mut snapshot = ExactContextSnapshot::empty();
    snapshot.source_scope.facts.insert(
        ExactModuleIdentity {
            unit: "main".into(),
            module: "WrongKey".into(),
        },
        ImmutableLexicalFactRef::new(
            CanonicalLexicalNode::try_from(ExactLexicalNode {
                owner,
                imports: vec![],
            })
            .unwrap(),
        )
        .unwrap(),
    );
    let store = MemoryBlockstore::new();
    let mut writer = Materializer::new(&store);
    let invalid = writer.materialize_context(None, &snapshot).unwrap();
    install(&store, writer.finish().unwrap());
    assert!(matches!(hydrate_context(&store, invalid.root()),
        Err(SnapshotError::Malformed(message)) if message == "source scope key differs from fact owner"));
}

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
fn publication_native_descriptor_refuses_old_version_and_missing_map_roots() {
    // This actual maintained owner supplies immutable descriptor data only.
    // No decoded fact below is bound to an acquired native view.
    let support = crate::artifact_inventory::CompilerSourceSupport::empty();
    let facts = support.native_fact_owner();
    let store = MemoryBlockstore::new();
    let mut writer = Materializer::new(&store);
    let valid_root = writer.materialize_publication_native(facts).unwrap();
    install(&store, writer.finish().unwrap());
    assert_eq!(valid_root, facts.commitment());
    for (cid, bytes) in facts.receipt.frontier.blocks() {
        assert_eq!(store.get(cid).unwrap().as_deref(), Some(bytes.as_ref()));
        verify_block(cid, bytes).unwrap();
    }
    let descriptor: PublicationNativeDescriptor = read_value(&store, &valid_root).unwrap();
    assert_eq!(descriptor.version, 3);
    for root in [
        &descriptor.roles,
        &descriptor.offers,
        &descriptor.native_availability,
    ] {
        let bytes = store.get(root.cid()).unwrap().unwrap();
        verify_block(root.cid(), &bytes).unwrap();
    }
    let restored = hydrate_publication_native(&SnapshotHydrator::new(&store), &valid_root).unwrap();
    assert_eq!(restored.commitment(), valid_root);
    assert_eq!(restored.roles, facts.roles);
    assert_eq!(restored.offers, facts.offers);
    assert_eq!(restored.native_availability, facts.native_availability);

    #[derive(Clone, Serialize)]
    struct DescriptorData {
        version: u32,
        #[serde(skip_serializing_if = "Option::is_none")]
        roles: Option<SnapshotMapRoot>,
        #[serde(skip_serializing_if = "Option::is_none")]
        offers: Option<SnapshotMapRoot>,
        #[serde(skip_serializing_if = "Option::is_none")]
        native_availability: Option<SnapshotMapRoot>,
    }
    let valid = DescriptorData {
        version: 3,
        roles: Some(descriptor.roles),
        offers: Some(descriptor.offers),
        native_availability: Some(descriptor.native_availability),
    };
    let mut writer = Materializer::new(&store);
    assert_eq!(writer.materialize_value(&valid).unwrap(), valid_root);
    install(&store, writer.finish().unwrap());
    for (label, data) in [
        (
            "old version",
            DescriptorData {
                version: 1,
                ..valid.clone()
            },
        ),
        (
            "roles",
            DescriptorData {
                roles: None,
                ..valid.clone()
            },
        ),
        (
            "offers",
            DescriptorData {
                offers: None,
                ..valid.clone()
            },
        ),
        (
            "native_availability",
            DescriptorData {
                native_availability: None,
                ..valid.clone()
            },
        ),
    ] {
        let mut writer = Materializer::new(&store);
        let changed_root = writer.materialize_value(&data).unwrap();
        install(&store, writer.finish().unwrap());
        assert_ne!(
            changed_root, valid_root,
            "the {label} mutation changes actual bytes"
        );
        let bytes = store.get(&changed_root).unwrap().unwrap();
        verify_block(&changed_root, &bytes).unwrap();
        assert!(
            matches!(
                hydrate_publication_native(&SnapshotHydrator::new(&store), &changed_root),
                Err(SnapshotError::Malformed(_))
            ),
            "the {label} descriptor must fail its format, not CID integrity"
        );
    }
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
    for version in [2, 3, 4, 5, 6, 7, 8, 9] {
        let mut materializer = Materializer::new(&store);
        let descriptor = ContextDescriptor {
            version,
            producer: [0; 32],
            selection: selection.root().clone(),
            roles: roles.root().clone(),
            lexical: lexical.root().clone(),
            source_scope: lexical.root().clone(),
            offers: lexical.root().clone(),
            native_availability: lexical.root().clone(),
            native_components: lexical.root().clone(),
            native_closures: lexical.root().clone(),
        };
        let old_root = ContextRoot(materializer.materialize_value(&descriptor).unwrap());
        install(&store, materializer.finish().unwrap());
        assert!(matches!(hydrate_context(&store, &old_root),
                Err(SnapshotError::UnsupportedContextVersion { found, expected })
                    if found == version && expected == CONTEXT_ENCODING_VERSION));
    }
}

#[test]
fn recorded_compiler_row_codec_preserves_unissued_and_issued_empty_choices() {
    let store = MemoryBlockstore::new();
    let mut writer = Materializer::new(&store);
    let row = |native_choices| RecordedCompilerRow::Original {
        interface: ArtifactId([1; 32]),
        original: ArtifactId([2; 32]),
        provenance: RecordedOriginalProvenance::Reusable,
        native_choices,
    };
    let unissued = row(RecordedNativeChoices::Unissued);
    let empty = row(RecordedNativeChoices::Issued {
        groups: OrdMap::new(),
    });
    let unissued_cid = CompilerRowCodec::encode(&mut writer, &unissued).unwrap();
    let empty_cid = CompilerRowCodec::encode(&mut writer, &empty).unwrap();
    assert_ne!(unissued_cid, empty_cid);
    install(&store, writer.finish().unwrap());
    let reader = SnapshotHydrator::new(&store);
    for (cid, expected) in [(unissued_cid, unissued), (empty_cid, empty)] {
        let decoded = CompilerRowCodec::decode(&reader, cid).unwrap();
        assert_eq!(decoded, expected);
        assert_eq!(decoded.interface(), ArtifactId([1; 32]));
        assert_eq!(decoded.original(), Some(ArtifactId([2; 32])));
        assert!(decoded.publication().is_none());
        assert_eq!(decoded.native_choices(), expected.native_choices());
    }
}

#[test]
fn recorded_compiler_row_codec_refuses_foreign_group_and_ordinal() {
    let store = MemoryBlockstore::new();
    let group: crate::artifact_inventory::NativeGroupBinding = serde_json::from_value(
        serde_json::json!({"binding": {"artifact": vec![2u8; 32], "selection": vec![3u8; 32]}, "original_ordinal": 7}),
    ).unwrap();
    for (original, ordinal) in [(ArtifactId([4; 32]), 7), (ArtifactId([2; 32]), 8)] {
        let row = RecordedCompilerRow::Original {
            interface: ArtifactId([1; 32]),
            original,
            provenance: RecordedOriginalProvenance::Reusable,
            native_choices: RecordedNativeChoices::Issued {
                groups: std::iter::once((ordinal, group)).collect(),
            },
        };
        let mut writer = Materializer::new(&store);
        let cid = CompilerRowCodec::encode(&mut writer, &row).unwrap();
        install(&store, writer.finish().unwrap());
        assert!(
            matches!(CompilerRowCodec::decode(&SnapshotHydrator::new(&store), cid),
            Err(SnapshotError::Malformed(message)) if message == "native choice differs from its recorded original row")
        );
    }
}

#[test]
fn context10_decodes_shared_publication_records_without_acquired_owners() {
    let store = MemoryBlockstore::new();
    let native_facts = PublicationNativeFacts::maintain(
        None,
        PublicationNativeMaps {
            roles: OrdMap::new(),
            offers: OrdMap::new(),
            native_availability: OrdMap::new(),
        },
    )
    .unwrap();
    let owner = ExactModuleIdentity {
        unit: "record".into(),
        module: "Owner".into(),
    };
    let mut first = ExactContextSnapshot::empty();
    first.roles.insert(
        owner.clone(),
        RecordedCompilerRow::Original {
            interface: ArtifactId([1; 32]),
            original: ArtifactId([2; 32]),
            provenance: RecordedOriginalProvenance::Published(RecordedPublicationBirth {
                source_revision: "recorded revision".into(),
                original_input_identity: "recorded input".into(),
                selection: crate::artifact_inventory::ExactArtifactSelection::from_snapshot(
                    Arc::new(ExactSelectionSnapshot::default()),
                ),
                selection_sha256: [3; 32],
                native_facts,
            }),
            native_choices: RecordedNativeChoices::Unissued,
        },
    );
    let mut second = first.clone();
    second.producer = [4; 32];
    let mut writer = Materializer::new(&store);
    let first_record = writer.materialize_context(None, &first).unwrap();
    let second_record = writer
        .materialize_context(Some(&first_record), &second)
        .unwrap();
    install(&store, writer.finish().unwrap());
    assert_ne!(first_record.root(), second_record.root());
    let decoded = hydrate_contexts(
        &store,
        &[first_record.root().clone(), second_record.root().clone()],
    )
    .unwrap();
    let left = decoded[0].recorded_rows()[&owner].publication().unwrap();
    let right = decoded[1].recorded_rows()[&owner].publication().unwrap();
    assert!(Arc::ptr_eq(&left.native_facts, &right.native_facts));
    assert!(Arc::ptr_eq(
        left.selection.snapshot(),
        right.selection.snapshot()
    ));
    assert_eq!(decoded[0].recorded_rows(), first.recorded_rows());
    assert_eq!(decoded[1].recorded_rows(), second.recorded_rows());
}

#[test]
fn recorded_publication_full_proof_changes_incremental_row_leaf() {
    let native_facts = PublicationNativeFacts::maintain(
        None,
        PublicationNativeMaps {
            roles: OrdMap::new(),
            offers: OrdMap::new(),
            native_availability: OrdMap::new(),
        },
    )
    .unwrap();
    let owner = ExactModuleIdentity {
        unit: "record".into(),
        module: "Owner".into(),
    };
    let row = |extra_unselected_node| {
        let scope = ArtifactSelectionId([1; 32]);
        let binding = ArtifactBinding {
            artifact: ArtifactId([2; 32]),
            selection: scope,
        };
        let node = ArtifactBindingNode::Artifact(binding);
        let mut nodes = vec![node];
        if extra_unselected_node {
            nodes.push(ArtifactBindingNode::Artifact(ArtifactBinding {
                artifact: ArtifactId([3; 32]),
                selection: scope,
            }));
        }
        // These are contradictory untrusted DATA records, not live issuers.
        let proof = ImmutableScopeProofRef::from_issued(Arc::new(ScopeProofV3 {
            selection: scope,
            nodes,
            dependencies: vec![],
        }));
        let selection = Arc::new(ExactSelectionSnapshot {
            scopes: std::iter::once((scope, proof)).collect(),
            roots: std::iter::once((node, ())).collect(),
            namespace: std::iter::once((binding.artifact, binding)).collect(),
            custody_roots: OrdMap::new(),
        });
        RecordedCompilerRow::Original {
            interface: ArtifactId([4; 32]),
            original: binding.artifact,
            provenance: RecordedOriginalProvenance::Published(RecordedPublicationBirth {
                source_revision: "recorded revision".into(),
                original_input_identity: "recorded input".into(),
                selection: crate::artifact_inventory::ExactArtifactSelection::from_snapshot(
                    selection,
                ),
                selection_sha256: [5; 32],
                native_facts: Arc::clone(&native_facts),
            }),
            native_choices: RecordedNativeChoices::Unissued,
        }
    };
    let first_row = row(false);
    let second_row = row(true);
    assert_eq!(
        first_row.publication().unwrap().selection,
        second_row.publication().unwrap().selection
    );
    assert_ne!(
        first_row, second_row,
        "record equality includes complete encoded proof facts"
    );
    let mut first = ExactContextSnapshot::empty();
    first.roles.insert(owner.clone(), first_row);
    let mut second = first.clone();
    second.roles.insert(owner, second_row);
    let store = MemoryBlockstore::new();
    let mut writer = Materializer::new(&store);
    let base = writer.materialize_context(None, &first).unwrap();
    install(&store, writer.finish().unwrap());
    let mut writer = Materializer::new(&store);
    let incremental = writer.materialize_context(Some(&base), &second).unwrap();
    let packet = writer.finish().unwrap();
    assert_ne!(incremental.roles.root(), base.roles.root());
    install(&store, packet);
    let mut writer = Materializer::new(&store);
    let cold = writer.materialize_context(None, &second).unwrap();
    assert_eq!(incremental.root(), cold.root());
    assert_ne!(incremental.root(), base.root());
    assert_eq!(
        hydrate_context(&store, incremental.root())
            .unwrap()
            .recorded_rows(),
        second.recorded_rows()
    );
}

#[test]
fn context_version_gate_precedes_layout_and_child_hydration() {
    #[derive(Serialize)]
    struct ForeignContext {
        version: u32,
        foreign_native_rows: Cid,
    }
    let store = MemoryBlockstore::new();
    let missing = Cid::new_v1(
        DAG_CBOR,
        Code::Blake2b256.digest(b"unavailable context child"),
    );
    for version in [
        0,
        CONTEXT_ENCODING_VERSION - 1,
        CONTEXT_ENCODING_VERSION + 1,
    ] {
        let mut writer = Materializer::new(&store);
        let root = ContextRoot(
            writer
                .materialize_value(&ForeignContext {
                    version,
                    foreign_native_rows: missing,
                })
                .unwrap(),
        );
        install(&store, writer.finish().unwrap());
        let observed = CountingStore {
            inner: &store,
            reads: RefCell::new(BTreeMap::new()),
        };
        assert!(matches!(hydrate_context(&observed, &root),
            Err(SnapshotError::UnsupportedContextVersion { found, expected })
                if found == version && expected == CONTEXT_ENCODING_VERSION));
        let reads = observed.reads.borrow();
        assert_eq!(
            reads.len(),
            1,
            "unsupported contexts must not acquire child blocks"
        );
        assert_eq!(reads.get(root.cid()).unwrap().0, 1);
        assert!(!reads.contains_key(&missing));
    }
    let mut writer = Materializer::new(&store);
    let root = ContextRoot(
        writer
            .materialize_value(&ForeignContext {
                version: CONTEXT_ENCODING_VERSION,
                foreign_native_rows: missing,
            })
            .unwrap(),
    );
    install(&store, writer.finish().unwrap());
    assert!(
        matches!(
            hydrate_context(&store, &root),
            Err(SnapshotError::Malformed(_))
        ),
        "the current version still requires the complete current layout"
    );
    assert!(matches!(hydrate_context(&MemoryBlockstore::new(), &root),
        Err(SnapshotError::MissingBlock(cid)) if cid == *root.cid()));
    struct InaccessibleStore;
    impl Blockstore for InaccessibleStore {
        fn get(&self, _: &Cid) -> anyhow::Result<Option<Vec<u8>>> {
            Err(std::io::Error::from(std::io::ErrorKind::PermissionDenied).into())
        }
        fn put_keyed(&self, _: &Cid, _: &[u8]) -> anyhow::Result<()> {
            unreachable!("context hydration does not publish blocks")
        }
    }
    assert!(matches!(
        hydrate_context(&InaccessibleStore, &root),
        Err(SnapshotError::Storage(_))
    ));
    store.put_keyed(root.cid(), &[0]).unwrap();
    assert!(matches!(hydrate_context(&store, &root),
        Err(SnapshotError::Integrity(cid)) if cid == *root.cid()));
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
fn recorded_original_choices_distinguish_empty_from_unissued_without_authority() {
    let store = MemoryBlockstore::new();
    let owner = ExactModuleIdentity {
        unit: "home".into(),
        module: "RecordedOnly".into(),
    };
    let row = |native_choices| RecordedCompilerRow::Original {
        interface: ArtifactId([1; 32]),
        original: ArtifactId([2; 32]),
        provenance: RecordedOriginalProvenance::Reusable,
        native_choices,
    };
    let mut snapshot = ExactContextSnapshot::empty();
    snapshot
        .roles
        .insert(owner.clone(), row(RecordedNativeChoices::Unissued));
    let mut writer = Materializer::new(&store);
    let unissued = writer.materialize_context(None, &snapshot).unwrap();
    install(&store, writer.finish().unwrap());
    snapshot.roles.insert(
        owner.clone(),
        row(RecordedNativeChoices::Issued {
            groups: OrdMap::new(),
        }),
    );
    let mut writer = Materializer::new(&store);
    let empty = writer
        .materialize_context(Some(&unissued), &snapshot)
        .unwrap();
    let packet = writer.finish().unwrap();
    assert_eq!(
        packet.metrics().changed_entries,
        1,
        "native choices change only their owning row leaf"
    );
    install(&store, packet);
    assert_ne!(unissued.root(), empty.root());
    let reopened = hydrate_context(&store, empty.root()).unwrap();
    assert_eq!(reopened.recorded_rows(), snapshot.recorded_rows());
    assert!(matches!(reopened.recorded_rows()[&owner].native_choices(),
        Some(RecordedNativeChoices::Issued { groups }) if groups.is_empty()));
    assert!(matches!(
        hydrate_context(&store, unissued.root())
            .unwrap()
            .recorded_rows()[&owner]
            .native_choices(),
        Some(RecordedNativeChoices::Unissued)
    ));
}

#[test]
fn recorded_row_codec_refuses_type_choices_and_duplicate_ordinals() {
    let store = MemoryBlockstore::new();
    let group: crate::artifact_inventory::NativeGroupBinding = serde_json::from_value(
        serde_json::json!({"binding": {"artifact": vec![2u8; 32], "selection": vec![3u8; 32]}, "original_ordinal": 7}),
    ).unwrap();
    let malformed = [
        serde_json::json!({"kind":"interface_only", "interface": vec![1u8;32],
            "native_choices":{"kind":"unissued"}}),
        serde_json::json!({"kind":"original", "interface": vec![1u8;32], "original":vec![2u8;32],
            "provenance":{"kind":"reusable"},
            "native_choices":{"kind":"issued", "groups":[[7, group], [7, group]]}}),
        serde_json::json!({"kind":"original", "interface": vec![1u8;32], "original":vec![2u8;32],
            "provenance":{"kind":"reusable"}}),
    ];
    for record in malformed {
        let mut writer = Materializer::new(&store);
        let cid = writer.materialize_value(&record).unwrap();
        install(&store, writer.finish().unwrap());
        assert!(matches!(
            CompilerRowCodec::decode(&SnapshotHydrator::new(&store), cid),
            Err(SnapshotError::Malformed(_))
        ));
    }
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
fn context_v10_requires_normalized_availability_root() {
    #[derive(Serialize)]
    struct MissingAvailability {
        version: u32,
        producer: [u8; 32],
        selection: SelectionRoot,
        roles: SnapshotMapRoot,
        lexical: SnapshotMapRoot,
        source_scope: SnapshotMapRoot,
        offers: SnapshotMapRoot,
        native_components: SnapshotMapRoot,
        native_closures: SnapshotMapRoot,
    }
    let store = MemoryBlockstore::new();
    let mut writer = Materializer::new(&store);
    let valid = writer
        .materialize_context(None, &ExactContextSnapshot::empty())
        .unwrap();
    let missing = ContextRoot(
        writer
            .materialize_value(&MissingAvailability {
                version: CONTEXT_ENCODING_VERSION,
                producer: [0; 32],
                selection: valid.selection.root.clone(),
                roles: valid.roles.root.clone(),
                lexical: valid.lexical.root.clone(),
                source_scope: valid.source_scope.root.clone(),
                offers: valid.offers.root.clone(),
                native_components: valid.native_components.root.clone(),
                native_closures: valid.native_closures.root.clone(),
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
        ArtifactEntry, ArtifactInventory, CompilerInputProjection, NativeGroupKey,
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
        let issuer = crate::declaration_context::issue_fixture_compiler_support(
            CompilerInputProjection::from_issued_entries(&[root.clone()]).unwrap(),
            view.clone(),
        )
        .unwrap();
        let offer = issuer
            .issue_native_offer(&root.descriptor.owner, Some(proof))
            .unwrap();
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

fn reachable_fixture() -> (StagedBlocks, Vec<Cid>, BTreeMap<Cid, Vec<Cid>>) {
    let store = MemoryBlockstore::new();
    let mut writer = Materializer::new(&store);
    let leaf = writer.materialize_value(&("leaf", 0u64)).unwrap();
    let a = writer.materialize_value(&("a", vec![leaf, leaf])).unwrap();
    let b = writer.materialize_value(&("b", vec![leaf])).unwrap();
    let top = writer.materialize_value(&("top", vec![a, b])).unwrap();
    let orphan = writer.materialize_value(&("unreachable", 1u64)).unwrap();
    let edges = [
        (leaf, vec![]),
        (a, vec![leaf, leaf]),
        (b, vec![leaf]),
        (top, vec![a, b]),
        (orphan, vec![]),
    ]
    .into_iter()
    .collect();
    (
        writer.finish().unwrap(),
        vec![leaf, a, b, top, orphan],
        edges,
    )
}

// Independent graph oracle uses the fixture's declared edges, not the
// production CBOR link scanner or incremental retain/release algorithm.
fn full_incoming(roots: &[Cid], edges: &BTreeMap<Cid, Vec<Cid>>) -> BTreeMap<Cid, u64> {
    let mut pending = roots.to_vec();
    let mut counts = BTreeMap::new();
    let mut seen = std::collections::BTreeSet::new();
    for root in roots {
        *counts.entry(*root).or_default() += 1;
    }
    while let Some(cid) = pending.pop() {
        if seen.insert(cid) {
            for child in &edges[&cid] {
                *counts.entry(*child).or_default() += 1;
                pending.push(*child);
            }
        }
    }
    counts
}

fn frontier_counts(frontier: &ReachableBlocks) -> BTreeMap<Cid, u64> {
    frontier
        .blocks
        .iter()
        .map(|(cid, block)| (*cid, block.incoming))
        .collect()
}

#[test]
fn reachable_blocks_count_duplicate_edges_and_release_obsolete_fork_paths() {
    let (staged, nodes, edges) = reachable_fixture();
    let roots = vec![nodes[1], nodes[1], nodes[0], nodes[2]];
    let original = ReachableBlocks::default()
        .replace_roots(&staged, roots.clone())
        .unwrap();
    assert_eq!(frontier_counts(&original), full_incoming(&roots, &edges));
    assert_eq!(original.blocks[&nodes[1]].incoming, 2);
    assert_eq!(original.blocks[&nodes[0]].incoming, 4);
    assert!(!original.blocks.contains_key(&nodes[4]));
    let weak = Arc::downgrade(&original.blocks[&nodes[1]].bytes);
    let fork = original.clone();
    let changed = original.replace_roots(&staged, vec![nodes[2]]).unwrap();
    assert_eq!(
        frontier_counts(&changed),
        full_incoming(&[nodes[2]], &edges)
    );
    assert!(!changed.blocks.contains_key(&nodes[1]));
    drop(original);
    drop(staged);
    assert!(
        weak.upgrade().is_some(),
        "live fork still owns the old path"
    );
    drop(fork);
    assert!(
        weak.upgrade().is_none(),
        "successor does not own the predecessor path"
    );
    let empty = changed
        .replace_roots(
            &StagedBlocks {
                blocks: vec![],
                metrics: Default::default(),
            },
            vec![],
        )
        .unwrap();
    assert!(empty.blocks.is_empty());
}

#[test]
fn current_block_frontier_materializes_only_changed_hamt_paths() {
    let empty = ReachableBlocks::default();
    // A corpus larger than a single HAMT fanout separates bounded path
    // reference edits from a whole-frontier traversal.
    let facts = (0u64..8192)
        .map(|n| (n, format!("value-{n}")))
        .collect::<OrdMap<_, _>>();
    let mut first = Materializer::new(&empty);
    let base = first.materialize_map(None, &facts).unwrap();
    let cold = first.finish().unwrap();
    let frontier = empty
        .replace_roots(&cold, vec![*base.root().cid()])
        .unwrap();
    let mut next_facts = facts.clone();
    next_facts.insert(500, "changed".into());
    let mut writer = Materializer::new(&frontier);
    let next = writer.materialize_map(Some(&base), &next_facts).unwrap();
    let delta = writer.finish().unwrap();
    assert_eq!(delta.metrics().changed_entries, 1);
    assert!(delta.metrics().offered_bytes < cold.metrics().offered_bytes / 4);
    assert!(delta.metrics().cid_validation_bytes < cold.metrics().cid_validation_bytes / 4);
    let successor = frontier
        .replace_roots(&delta, vec![*next.root().cid()])
        .unwrap();
    let cold_work = frontier.work();
    let changed_work = successor.work();
    assert!(changed_work.verified_bytes > 0);
    assert!(changed_work.verified_bytes < cold_work.verified_bytes / 4);
    assert!(changed_work.decoded_bytes < cold_work.decoded_bytes / 4);
    assert!(changed_work.link_visits + changed_work.unlink_visits < cold_work.link_visits / 2);
    let restored = hydrate_map::<u64, String, _>(&successor, next.root()).unwrap();
    assert_eq!(restored.facts(), &next_facts);
    let mut unchanged = Materializer::new(&successor);
    let same = unchanged.materialize_map(Some(&next), &next_facts).unwrap();
    let none = unchanged.finish().unwrap();
    assert_eq!(none.metrics().changed_entries, 0);
    assert_eq!(none.metrics().serialized_bytes, 0);
    assert_eq!(none.metrics().offered_bytes, 0);
    assert_eq!(none.metrics().cid_validation_bytes, 0);
    assert!(none.blocks().is_empty());
    let same_frontier = successor
        .replace_roots(&none, vec![*same.root().cid()])
        .unwrap();
    assert_eq!(frontier_counts(&same_frontier), frontier_counts(&successor));
    assert_eq!(same_frontier.work(), FrontierWork::default());
}

#[test]
fn reachable_frontier_missing_child_does_not_change_existing_or_fork() {
    let (staged, nodes, edges) = reachable_fixture();
    let initial = ReachableBlocks::default()
        .replace_roots(&staged, vec![nodes[0]])
        .unwrap();
    let fork = initial.clone();
    // The top first retains A successfully, then encounters the omitted B.
    let incomplete = StagedBlocks {
        blocks: staged
            .blocks
            .iter()
            .filter(|(cid, _)| *cid != nodes[2])
            .cloned()
            .collect(),
        metrics: Default::default(),
    };
    assert!(matches!(initial.replace_roots(&incomplete, vec![nodes[3]]),
        Err(SnapshotError::MissingBlock(cid)) if cid == nodes[2]));
    assert_eq!(
        frontier_counts(&initial),
        full_incoming(&[nodes[0]], &edges)
    );
    assert_eq!(frontier_counts(&fork), frontier_counts(&initial));
    assert!(initial.get(&nodes[0]).unwrap().is_some());
}

#[test]
fn reachable_frontier_refuses_malformed_cid_link_shapes() {
    use ciborium::value::Value;
    for value in [
        Value::Tag(7, Box::new(Value::Null)),
        Value::Tag(42, Box::new(Value::Null)),
        Value::Tag(42, Box::new(Value::Bytes(vec![1, 2, 3]))),
        Value::Tag(42, Box::new(Value::Bytes(vec![0, 1, 2, 3]))),
    ] {
        let mut body = Vec::new();
        ciborium::ser::into_writer(&value, &mut body).unwrap();
        let cid = Cid::new_v1(DAG_CBOR, Code::Blake2b256.digest(&body));
        let staged = StagedBlocks {
            blocks: vec![(cid, body.into())],
            metrics: Default::default(),
        };
        assert!(ReachableBlocks::default()
            .replace_roots(&staged, vec![cid])
            .is_err());
    }
}

proptest! {
    #[test]
    fn reachable_frontier_histories_match_full_recomputation(
        edits in prop::collection::vec((0usize..3, prop::collection::vec(0usize..5, 0..6)), 1..48)
    ) {
        let (staged, nodes, edges) = reachable_fixture();
        let mut forks = vec![ReachableBlocks::default(); 3];
        for (slot, selected) in edits {
            let roots = selected.into_iter().map(|index| nodes[index]).collect::<Vec<_>>();
            let base = forks[(slot+1)%3].clone();
            forks[slot] = base.replace_roots(&staged, roots.clone()).unwrap();
            prop_assert_eq!(frontier_counts(&forks[slot]), full_incoming(&roots, &edges));
            for other in &forks {
                prop_assert_eq!(frontier_counts(other), full_incoming(&other.roots, &edges));
            }
        }
    }
}

#[test]
fn context_v10_requires_linked_and_source_roots_and_distinguishes_missing_storage() {
    let store = MemoryBlockstore::new();
    let mut writer = Materializer::new(&store);
    let valid = writer
        .materialize_context(None, &ExactContextSnapshot::empty())
        .unwrap();
    install(&store, writer.finish().unwrap());
    let descriptor: ContextDescriptor = read_value(&store, valid.root().cid()).unwrap();
    assert_eq!(descriptor.version, 10);
    let bytes = store.get(valid.root().cid()).unwrap().unwrap();
    let value: ciborium::Value = ciborium::de::from_reader(bytes.as_slice()).unwrap();
    let encode = |value: &ciborium::Value| {
        let mut bytes = Vec::new();
        ciborium::ser::into_writer(value, &mut bytes).unwrap();
        bytes
    };
    // Mutation must preserve untouched CID tags, including their wire encoding.
    // Ciborium's Value tag protocol is not interchangeable with DAG-CBOR Serde.
    assert_eq!(encode(&value), bytes);
    // Remove each required root from the actual current descriptor encoding.
    for field in ["native_components", "native_closures", "source_scope"] {
        let mut value = value.clone();
        let ciborium::Value::Map(fields) = &mut value else {
            panic!("context descriptor map")
        };
        let before = fields.len();
        fields.retain(|(key, _)| key.as_text() != Some(field));
        assert_eq!(fields.len() + 1, before);
        let writer = Materializer::new(&store);
        let bytes = encode(&value);
        let missing_field = ContextRoot(Cid::new_v1(DAG_CBOR, Code::Blake2b256.digest(&bytes)));
        writer.put_keyed(missing_field.cid(), &bytes).unwrap();
        install(&store, writer.finish().unwrap());
        let outcome = hydrate_context(&store, &missing_field).err();
        assert!(
            matches!(&outcome,
                Some(SnapshotError::Malformed(message)) if message.contains(field)),
            "removed {field}: {outcome:?}"
        );

        let mut descriptor = descriptor.clone();
        let absent = Cid::new_v1(DAG_CBOR, Code::Blake2b256.digest(field.as_bytes()));
        assert!(store.get(&absent).unwrap().is_none());
        match field {
            "native_components" => descriptor.native_components = SnapshotMapRoot(absent),
            "native_closures" => descriptor.native_closures = SnapshotMapRoot(absent),
            "source_scope" => descriptor.source_scope = SnapshotMapRoot(absent),
            _ => unreachable!(),
        }
        let mut writer = Materializer::new(&store);
        let missing_map = ContextRoot(writer.materialize_value(&descriptor).unwrap());
        install(&store, writer.finish().unwrap());
        let outcome = hydrate_context(&store, &missing_map).err();
        assert!(
            matches!(&outcome,
                Some(SnapshotError::MissingBlock(cid)) if *cid == absent),
            "missing {field} block: {outcome:?}"
        );
    }
    let restored = hydrate_context(&store, valid.root()).unwrap();
    assert!(restored.snapshot().native_components.is_empty());
    assert!(restored.snapshot().native_closures.is_empty());
    assert!(restored
        .snapshot()
        .selection()
        .graph_selection()
        .unwrap()
        .native_groups
        .is_empty());
    let mut writer = Materializer::new(&store);
    let unchanged = writer
        .materialize_context(Some(&restored), restored.snapshot())
        .unwrap();
    assert_eq!(unchanged.root(), valid.root());
    assert_eq!(
        unchanged.native_components.root,
        valid.native_components.root
    );
    assert_eq!(unchanged.native_closures.root, valid.native_closures.root);
}
