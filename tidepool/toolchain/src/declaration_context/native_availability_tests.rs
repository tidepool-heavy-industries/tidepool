use super::*;
use crate::certified_products::tests::{
    original_groups_fixture_with_interface, recovered_witness_fixtures,
};
use crate::certified_products::{fixture_finalized_product, fixture_module_core};

const PRODUCER: &[u8] = b"protected-native-availability-fixture";

fn product(module: &str, version: u8) -> CertifiedRecoveryProduct {
    let finalized = fixture_finalized_product(
        original_groups_fixture_with_interface(
            module,
            vec![(7, vec![]), (9001, vec![])],
            version,
            &BTreeMap::new(),
            module.as_bytes().to_vec(),
        ),
        CanonicalProducerIdentity::from_producer_bytes(PRODUCER).sha256(),
    );
    recovered_witness_fixtures(&[finalized]).remove(0).product
}

fn type_context(products: &[CertifiedRecoveryProduct]) -> Arc<ExactDeclarationContext> {
    let inventory = ArtifactInventory::default();
    let entries = products
        .iter()
        .map(|product| {
            Arc::new(ArtifactEntry::canonical(
                product.module_interface().unwrap().clone(),
            ))
        })
        .collect::<Vec<_>>();
    let projection = CompilerInputProjection::from_issued_entries(&entries).unwrap();
    let view = inventory
        .admit_shared(&inventory.empty_view(), entries)
        .unwrap();
    Arc::new(ExactDeclarationContext {
        producer: CanonicalProducerIdentity::from_producer_bytes(PRODUCER).sha256(),
        inventory: view,
        compiler_projection: projection,
        lexical: vec![],
        template_imports: None,
        original_instance_environment: OriginalInstanceEnvironment::Unknown,
    })
}

fn scope(request: &ExactCompilationRequest) -> Vec<Value> {
    let bytes = std::fs::read(&request.manifest).unwrap();
    let value: Value = ciborium::de::from_reader(bytes.as_slice()).unwrap();
    let Value::Array(fields) = value else {
        panic!("scope array");
    };
    fields
}

#[test]
fn selected_authored_availability_refuses_source_original_spelling_with_full_groups() {
    let native = product("Tidepool.Session.Lib.G1", 23);
    let producer = CanonicalProducerIdentity::from_producer_bytes(PRODUCER);
    let mut context = ExactDeclarationContext::new(&[], &[], vec![])
        .unwrap()
        .extend_checked_original_products(producer.sha256(), std::slice::from_ref(&native))
        .unwrap();
    context.compiler_projection = context.compiler_projection.interface_only();
    assert!(context
        .compiler_input_roles()
        .iter()
        .all(|role| role.original().is_none()));
    let keys = context.inventory.selected_native_groups();
    assert_eq!(
        keys.iter()
            .map(|key| key.original_ordinal)
            .collect::<Vec<_>>(),
        vec![7, 9001]
    );
    assert!(
        OriginalCompilerInputs::from_selected_authored_declarations(&context, producer)
            .unwrap()
            .is_none()
    );
    // A separately issued configured offer remains legitimate. Full custody
    // and a session-like module name cannot issue that offer implicitly.
    assert!(OriginalCompilerInputs::from_native_availability(
        &context,
        producer,
        std::slice::from_ref(&native),
    )
    .is_ok());
}

#[test]
fn initial_private_native_offer_reaches_wire_without_lexical_or_instance_authority() {
    let native = product("HiddenNative", 23);
    let context = type_context(std::slice::from_ref(&native));
    let before = context.as_ref().clone();
    let compiler = ExactCompileContext::new(context.clone());
    let root = tempfile::tempdir().unwrap();
    let baseline = compiler
        .prepare_compilation_with_authorization(
            &root.path().join("baseline"),
            PRODUCER,
            Some(text("same-authorization")),
        )
        .unwrap();
    let input = OriginalCompilerInputs::from_native_availability(
        &context,
        CanonicalProducerIdentity::from_producer_bytes(PRODUCER),
        std::slice::from_ref(&native),
    )
    .unwrap();
    let request = compiler
        .prepare_compilation_with_private_input(
            &root.path().join("native"),
            PRODUCER,
            Some(text("same-authorization")),
            Some(input),
        )
        .unwrap();
    assert_eq!(context.as_ref(), &before);
    assert_eq!(request.context().as_ref(), &before);
    assert_eq!(request.semantic_sha256, baseline.semantic_sha256);
    assert_ne!(request.request_sha256, baseline.request_sha256);
    assert_eq!(
        context.compiler_input_roles(),
        before.compiler_input_roles()
    );
    assert!(context
        .compiler_input_roles()
        .iter()
        .all(|role| role.original().is_none()));
    assert_eq!(
        context.original_instance_environment,
        OriginalInstanceEnvironment::Unknown
    );
    let baseline_scope = scope(&baseline);
    let actual_scope = scope(&request);
    assert!(baseline_scope[6].as_array().unwrap().is_empty());
    assert_eq!(actual_scope[5], baseline_scope[5]);
    assert_eq!(actual_scope[8], baseline_scope[8]);
    let rows = actual_scope[6].as_array().unwrap();
    assert_eq!(rows.len(), 1);
    let row = rows[0].as_array().unwrap();
    assert_eq!(row[2], text(hex(&native.owner().module_version.0)));
    assert_eq!(row[4], text(hex(&native.owner().product_sha256)));
    assert_eq!(
        row[6]
            .as_array()
            .unwrap()
            .iter()
            .map(|group| { group.as_array().unwrap()[0].clone() })
            .collect::<Vec<_>>(),
        vec![Value::Integer(7.into()), Value::Integer(9001.into())]
    );
    assert!(request.groups.is_empty());
    assert_eq!(
        request.compiler_inputs().metadata.selected_native_groups,
        baseline.compiler_inputs().metadata.selected_native_groups
    );
    let mut command = tidepool_extract_cmd::ExtractCmd::with_bin(
        tidepool_extract_cmd::ResolvedExtractBin::assume_resolved(
            "/request-construction-only/compiler",
        ),
    );
    request
        .apply_to(
            &mut command,
            RetainedGenerationPolicy::PreserveCertifiedDemand,
        )
        .unwrap();
    let argv = command.argv();
    let index = argv
        .iter()
        .position(|arg| arg == "--session-artifacts")
        .unwrap();
    assert_eq!(Path::new(&argv[index + 1]), request.manifest);
    let encoded = command.request_bytes();
    assert_eq!(
        tidepool_extract_cmd::ExtractRequest::decode(&encoded)
            .unwrap()
            .encode(),
        encoded
    );
}

#[test]
fn private_native_offer_refuses_core_drift_and_retained_resume_ambiguity() {
    let first = product("AmbiguousNative", 23);
    let second = product("AmbiguousNative", 24);
    assert_eq!(first.module_interface(), second.module_interface());
    assert_ne!(first.owner(), second.owner());
    let context = type_context(std::slice::from_ref(&first));
    let mut changed = context.as_ref().clone();
    let interface =
        fixture_module_core(first.module_interface().unwrap(), b"changed-Core".to_vec());
    let entry = Arc::new(ArtifactEntry::canonical(interface));
    changed.inventory = changed
        .inventory
        .inventory()
        .admit_shared(
            &changed.inventory.inventory().empty_view(),
            vec![Arc::clone(&entry)],
        )
        .unwrap();
    changed.compiler_projection = CompilerInputProjection::from_issued_entries(&[entry]).unwrap();
    assert!(OriginalCompilerInputs::from_native_availability(
        &changed,
        CanonicalProducerIdentity::from_producer_bytes(PRODUCER),
        std::slice::from_ref(&first),
    )
    .is_err());
    let entries = [&first, &second]
        .into_iter()
        .map(|product| {
            Arc::new(
                ArtifactEntry::original_with_validation(
                    CanonicalProducerIdentity::from_producer_bytes(PRODUCER).sha256(),
                    product.clone(),
                    &mut PackageInterfaceValidation::default(),
                )
                .unwrap(),
            )
        })
        .collect::<Vec<_>>();
    let mut ambiguous = context.as_ref().clone();
    ambiguous.inventory = ambiguous
        .inventory
        .inventory()
        .admit_shared(&ambiguous.inventory, entries.clone())
        .unwrap();
    assert!(ambiguous
        .inventory
        .metadata_snapshot()
        .ambiguous_native_owners
        .contains(&identity("fixture", "AmbiguousNative"),));
    assert!(OriginalCompilerInputs::from_native_availability(
        &ambiguous,
        CanonicalProducerIdentity::from_producer_bytes(PRODUCER),
        std::slice::from_ref(&first),
    )
    .is_err());
    // An issued winner remains explicit even when other versions stay in custody.
    ambiguous.compiler_projection =
        CompilerInputProjection::from_issued_entries(&[Arc::clone(&entries[0])]).unwrap();
    assert!(OriginalCompilerInputs::from_native_availability(
        &ambiguous,
        CanonicalProducerIdentity::from_producer_bytes(PRODUCER),
        std::slice::from_ref(&first),
    )
    .is_ok());
    assert!(OriginalCompilerInputs::from_native_availability(
        &ambiguous,
        CanonicalProducerIdentity::from_producer_bytes(PRODUCER),
        std::slice::from_ref(&second),
    )
    .is_err());
}

#[test]
fn availability_refuses_wrong_external_group_before_advertising_binders() {
    use crate::certified_products::{
        certify_candidate_original_with_validation, OriginalNativeCandidate, PendingImportOwner,
    };
    let required = product("RequiredNative", 30);
    let wrong = fixture_finalized_product(
        original_groups_fixture_with_interface(
            "DependentNative",
            vec![(
                7,
                vec![PendingImportOwner::Source {
                    owner: required.owner().clone(),
                    original_ordinal: 999,
                    binder: tidepool_repr::execution_schema::testing::identity(
                        "RequiredNative",
                        "entry_7",
                    ),
                }],
            )],
            31,
            &BTreeMap::new(),
            b"DependentNative".to_vec(),
        ),
        CanonicalProducerIdentity::from_producer_bytes(PRODUCER).sha256(),
    );
    let wrong = certify_candidate_original_with_validation(
        OriginalNativeCandidate {
            owner: wrong.owner().clone(),
            product: crate::module_candidates::CandidateProduct::decode(
                wrong.product_bytes().to_vec(),
            )
            .unwrap(),
            certification_bytes: wrong.certification_bytes().to_vec(),
            module_interface: wrong.module_interface().unwrap().clone(),
            execution_source: None,
        },
        &mut PackageInterfaceValidation::default(),
    )
    .unwrap();
    let context = type_context(&[wrong.clone(), required.clone()]);
    assert!(OriginalCompilerInputs::from_native_availability(
        &context,
        CanonicalProducerIdentity::from_producer_bytes(PRODUCER),
        &[wrong, required],
    )
    .is_err());
}

// Called by the existing genuinely compiled native-origin fixture. The same
// immutable certificate supplies every narrowed history; none recompiles it.
pub(crate) fn assert_selected_authored_private_inputs(
    certificate: &Arc<CertifiedAuthoredDeclaration>,
    generation: u64,
    root: &Path,
    producer_bytes: &[u8],
) {
    let native_context =
        ExactDeclarationContext::new(std::slice::from_ref(certificate), &[], vec![]).unwrap();
    let producer = CanonicalProducerIdentity::from_producer_bytes(producer_bytes);
    let authored_root = native_context.authored_native_root(generation).unwrap();
    let selected_before = native_context.inventory.selected_native_groups();
    let authored_groups = selected_before
        .iter()
        .filter(|group| group.artifact == authored_root)
        .copied()
        .collect::<BTreeSet<_>>();
    assert!(
        authored_groups.len() >= 2,
        "the real fixture must issue independent original groups"
    );
    assert!(matches!(
        native_context.compiler_metadata_snapshot().unwrap().entries[&identity(
            &certificate.product().owner().unit,
            &certificate.product().owner().module
        )]
            .payload,
        ArtifactPayload::Canonical(_)
    ));
    let native =
        OriginalCompilerInputs::from_selected_authored_declarations(&native_context, producer)
            .unwrap()
            .expect("actual authored selection supplies private native availability");
    let native_request = ExactCompileContext::new(Arc::new(native_context.clone()))
        .prepare_compilation_with_private_input(
            &root.join("selected-authored-scope"),
            producer_bytes,
            None,
            Some(native),
        )
        .unwrap();
    assert!(native_request
        .compiler_original_products()
        .unwrap()
        .iter()
        .any(|product| product.owner() == certificate.product().owner()));
    assert_eq!(native_request.context().as_ref(), &native_context);
    assert_eq!(
        native_request
            .compiler_inputs()
            .metadata
            .selected_native_groups,
        selected_before
    );
    // Reuse the genuine certificate and independently remove execution
    // roots. Neither type custody nor an incomplete group selection can
    // supply the complete authored original to another compiler request.
    let mut valid_sparse_refusals = 0;
    for omitted in std::iter::once(None).chain(authored_groups.iter().map(Some)) {
        let selected = match omitted {
            None => BTreeSet::new(),
            Some(omitted) => selected_before
                .iter()
                .filter(|group| *group != omitted)
                .copied()
                .collect(),
        };
        let mut partial = native_context.clone();
        let view = partial.inventory.inventory().admit_recovery_selection(
            &partial.inventory.inventory().empty_view(),
            partial.inventory.entries(),
            &selected,
        );
        let Ok(view) = view else {
            assert!(
                omitted.is_some(),
                "removing every native root must retain valid type custody"
            );
            // Removing a required dependency is refused by exact closure
            // admission before private compiler input can be constructed.
            continue;
        };
        partial.inventory = view;
        assert_eq!(partial.inventory.selected_native_groups(), selected);
        let private =
            OriginalCompilerInputs::from_selected_authored_declarations(&partial, producer)
                .unwrap();
        assert!(private.as_ref().is_none_or(|private| private
            .projection
            .roles()
            .iter()
            .all(|role| role.original() != Some(authored_root))));
        if omitted.is_some()
            && partial
                .inventory
                .selected_native_groups()
                .iter()
                .any(|group| group.artifact == authored_root)
        {
            valid_sparse_refusals += 1;
        }
    }
    assert!(
        valid_sparse_refusals > 0,
        "a valid nonempty sparse selection must reach private offer refusal"
    );
}
