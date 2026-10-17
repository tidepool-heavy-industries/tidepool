use super::*;
use crate::artifact_inventory::{IssuedNativeOffer, NativeOriginalRequirement};
use crate::certified_products::{
    fixture_finalized_product, tests::original_groups_fixture_with_interface,
    tests::recovered_witness_fixtures,
};
use crate::exact_context::{hydrate_context, Materializer};
use fvm_ipld_blockstore::{Blockstore, MemoryBlockstore};

const PRODUCER: &[u8] = b"distinct-original-candidate-history";

#[test]
fn fresh_candidate_selects_its_original_beside_same_interface_historical_custody() {
    let producer = CanonicalProducerIdentity::from_producer_bytes(PRODUCER);
    let products = [141, 142].map(|version| {
        fixture_finalized_product(
            original_groups_fixture_with_interface(
                "CandidateHistory",
                vec![(7, vec![]), (9001, vec![])],
                version,
                &BTreeMap::new(),
                b"one canonical interface".to_vec(),
            ),
            producer.sha256(),
        )
    });
    let products = recovered_witness_fixtures(&products)
        .into_iter()
        .map(|verified| verified.product)
        .collect::<Vec<_>>();
    let entries = products
        .iter()
        .map(|product| {
            Arc::new(ArtifactEntry::original(producer.sha256(), product.clone()).unwrap())
        })
        .collect::<Vec<_>>();
    let a = &entries[0];
    let b = &entries[1];
    assert_eq!(
        products[0].module_interface(),
        products[1].module_interface()
    );
    assert_ne!(a.descriptor.id, b.descriptor.id);
    let inventory = ArtifactInventory::default();
    let historical = inventory
        .admit_shared(&inventory.empty_view(), vec![a.clone()])
        .unwrap();
    let proof = NativeOriginalRequirement::from_selected_view(a.descriptor.id, &historical)
        .unwrap()
        .unwrap();
    let offer = IssuedNativeOffer::issue(a.descriptor.id, &historical, Some(proof)).unwrap();
    assert!(offer.is_full());
    let original_offer = offer.snapshot_facts();
    let canonical = Arc::new(ArtifactEntry::canonical(
        products[1].module_interface().unwrap().clone(),
    ));
    let projection = CompilerInputProjection::from_issued_entries(&[canonical.clone()]).unwrap();
    let view = inventory
        .admit_shared(&inventory.empty_view(), vec![canonical])
        .unwrap();
    let context = |offers: Vec<IssuedNativeOffer>| {
        ExactDeclarationContext::from_facts(DeclarationContextFacts {
            producer: producer.sha256(),
            compiler_support: recover_fixture_compiler_support(
                projection.clone(),
                view.clone(),
                offers,
            )
            .unwrap(),
            lexical: Default::default(),
            template_imports: None,
            original_instance_environment: OriginalInstanceEnvironment::Unknown,
        })
    };
    let candidates = crate::module_candidates::CandidateSet {
        manifest_path: PathBuf::from("candidate-history-does-not-read-manifest"),
        by_owner: BTreeMap::new(),
        native_availability: vec![products[1].clone()],
        catalog_owner: None,
        input_transport: Vec::new(),
    };
    let assert_candidate = |context: ExactDeclarationContext, historical_expected: bool| {
        assert!(
            context
                .compiler_input_projection()
                .roles()
                .iter()
                .all(|role| role.original().is_none()),
            "the baseline deliberately has no selected native winner"
        );
        assert!(context.artifact_view().selected_native_groups().is_empty());
        let inputs = OriginalCompilerInputs::from_candidates(&context, producer, &candidates)
            .expect("the candidate owner issues B without promoting historical A")
            .unwrap();
        assert_eq!(
            inputs
                .role_projection()
                .roles()
                .iter()
                .filter_map(|role| role.original())
                .collect::<BTreeSet<_>>(),
            BTreeSet::from([b.descriptor.id])
        );
        assert!(
            inputs
                .issued_artifacts()
                .selected_native_groups()
                .is_empty(),
            "fresh availability does not select executable demand"
        );
        let [offered_b] = inputs.issued_offers() else {
            panic!("one fresh B offer")
        };
        assert_eq!(offered_b.original(), b.descriptor.id);
        assert!(offered_b.is_full());
        assert_eq!(
            offered_b
                .issued_group_carriers()
                .unwrap()
                .keys()
                .map(|group| (group.binding.artifact(), group.original_ordinal))
                .collect::<BTreeSet<_>>(),
            BTreeSet::from([(b.descriptor.id, 7), (b.descriptor.id, 9001)])
        );
        let directory = tempfile::tempdir().unwrap();
        let request = ExactCompileContext::new(Arc::new(context))
            .prepare_compilation_with_private_input(directory.path(), PRODUCER, None, Some(inputs))
            .unwrap();
        let selected = request
            .compiler_inputs()
            .artifacts()
            .selected_native_groups();
        assert!(
            selected.is_empty(),
            "the final request preserves zero executable demand"
        );
        if historical_expected {
            assert!(request
                .compiler_inputs()
                .artifacts()
                .artifact_ids()
                .contains(&a.descriptor.id));
            let retained_offer = request
                .compiler_inputs()
                .compiler_source_support()
                .native_offers()
                .iter()
                .find(|retained| retained.snapshot_facts() == offer.snapshot_facts())
                .expect("request owns historical availability independently of its transport");
            assert_eq!(
                retained_offer
                    .custody_snapshot()
                    .binding_dependencies()
                    .unwrap(),
                offer.custody_snapshot().binding_dependencies().unwrap()
            );
        }
    };
    // This positive control establishes B through the actual candidate issuer,
    // rather than inferring a winner from equal canonical interfaces.
    assert_candidate(context(vec![]), false);
    for copies in [1, 2] {
        let live = context(vec![offer.clone(); copies]);
        let snapshot = live.snapshot();
        let store = MemoryBlockstore::new();
        let mut materializer = Materializer::new(&store);
        let receipt = materializer.materialize_context(None, &snapshot).unwrap();
        for (cid, bytes) in materializer.finish().unwrap().into_blocks() {
            store.put_keyed(&cid, &bytes).unwrap();
        }
        let receipt = hydrate_context(&store, receipt.root()).unwrap();
        let decoded = receipt.snapshot();
        let graph = decoded.graph_selection().unwrap();
        let artifacts = live.artifact_view();
        // This component recovery authenticates decoded owner facts against
        // genuine certified fixture entries. It does not exercise catalog
        // acquisition or production public-root capture_bound restoration.
        let recovered = RecoveredArtifactInventory {
            producer: producer.sha256(),
            recorded_inventory: true,
            entries: artifacts
                .entries()
                .into_iter()
                .map(|entry| (entry.descriptor.id, entry))
                .collect(),
            bindings: graph.bindings.iter().copied().collect(),
            binding_edges: decoded.binding_dependencies().unwrap(),
            interfaces: artifacts.interface_dependencies(),
        };
        let mut cold = recovered.context_with_snapshot(decoded).unwrap();
        cold.bind_snapshot_baseline(&receipt).unwrap();
        assert!(cold
            .compiler_source_support()
            .native_offers()
            .iter()
            .any(|restored| restored.snapshot_facts() == original_offer));
        assert_candidate(live, true);
        assert_candidate(cold, true);
        assert_eq!(offer.snapshot_facts(), original_offer);
    }
    // An explicit incompatible current claim still refuses; availability does
    // not authorize changing an already selected native original.
    let selected_a = ExactDeclarationContext::from_facts(DeclarationContextFacts {
        producer: producer.sha256(),
        compiler_support: issue_fixture_compiler_support(
            CompilerInputProjection::from_issued_entries(&[a.clone()]).unwrap(),
            historical,
        )
        .unwrap(),
        lexical: Default::default(),
        template_imports: None,
        original_instance_environment: OriginalInstanceEnvironment::Unknown,
    });
    assert!(OriginalCompilerInputs::from_candidates(&selected_a, producer, &candidates).is_err());
}
