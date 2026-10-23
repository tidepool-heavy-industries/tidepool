use super::*;
use crate::artifact_inventory::{
    ArtifactEntry, ArtifactInventory, CanonicalProducerIdentity, CompilerInputProjection,
    CompilerInputRole, NativeArtifactDemand,
};
use crate::certified_products::{
    fixture_finalized_product, fixture_module_interface, fixture_native_declaration_product,
};
use crate::declaration_context::{ExactCompileContext, OriginalCompilerInputs};
use crate::declaration_join::{ExactLexicalNode, ExactModuleIdentity};
use crate::recovery_artifacts::CertifiedRecoveryProduct;
use std::collections::{BTreeMap, BTreeSet};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::OnceLock;

const PRODUCER: &[u8] = b"preview original native declaration inputs";
// Four prefix rows, the complete eight-role matrix and four later rows.
const MAX_HISTORY_OWNERS: usize = 16;

#[derive(Clone)]
struct DeclarationFixture {
    canonical: Arc<ArtifactEntry>,
    original: Arc<ArtifactEntry>,
}

impl DeclarationFixture {
    fn new(product: CertifiedRecoveryProduct) -> Self {
        Self {
            canonical: Arc::new(ArtifactEntry::canonical(product.module_interface().clone())),
            original: Arc::new(
                ArtifactEntry::original(
                    CanonicalProducerIdentity::from_producer_bytes(PRODUCER).sha256(),
                    product,
                )
                .unwrap(),
            ),
        }
    }
}

fn issued_declarations(native: bool) -> &'static [DeclarationFixture] {
    static FIXTURES: OnceLock<[Vec<DeclarationFixture>; 2]> = OnceLock::new();
    &FIXTURES.get_or_init(|| {
        let fixtures = [false, true].map(|native| {
            let products = (1..=MAX_HISTORY_OWNERS)
                .map(|generation| declaration(generation as u64, native))
                .collect::<Vec<_>>();
            // Immutable evidence is issued once through the production recovery
            // path. Every history still builds fresh custody and request roles.
            crate::certified_products::tests::recovered_witness_fixtures(&products)
                .into_iter()
                .map(|original| DeclarationFixture::new(original.product))
                .collect()
        });
        let products = fixtures.iter().map(Vec::len).sum::<usize>();
        eprintln!(
            "activation preview fixture issuance: recovery_batches={}, products={products}, canonical_entries={products}, original_entries={products}",
            fixtures.len(),
        );
        fixtures
    })[usize::from(native)]
}

fn declaration(generation: u64, native: bool) -> CertifiedRecoveryProduct {
    declaration_groups(generation, native, generation as u8, &[3, 17])
}

fn declaration_groups(
    generation: u64,
    native: bool,
    version: u8,
    ordinals: &[u32],
) -> CertifiedRecoveryProduct {
    let module =
        tidepool_repr::SessionModule::lib(tidepool_repr::Generation(generation)).module_name();
    let interface = format!("original interface {generation}").into_bytes();
    let product = crate::certified_products::tests::original_groups_fixture_in_unit_with_sites(
        "main",
        &module,
        ordinals
            .iter()
            .map(|ordinal| (*ordinal, Vec::new()))
            .collect(),
        version,
        &BTreeMap::new(),
        interface,
        &BTreeMap::new(),
        &[],
    );
    let producer = CanonicalProducerIdentity::from_producer_bytes(PRODUCER).sha256();
    if native {
        fixture_native_declaration_product(product, producer, generation)
    } else {
        fixture_finalized_product(product, producer)
    }
}

// Each row independently varies source/native origin, native byte custody and
// the issued compiler interface role. The lexical graph selects only Target.
fn original_context(rows: &[(bool, bool, bool)]) -> Arc<ExactDeclarationContext> {
    original_context_with_roles(rows, false)
}

fn original_context_with_roles(
    rows: &[(bool, bool, bool)],
    reusable_originals: bool,
) -> Arc<ExactDeclarationContext> {
    let fixtures = rows
        .iter()
        .enumerate()
        .map(|(index, (native, _, _))| {
            issued_declarations(*native)
                .get(index)
                .expect("history exceeds its declared fixture bound")
                .clone()
        })
        .collect::<Vec<_>>();
    original_context_with_fixtures(rows, fixtures, reusable_originals)
}

fn original_context_with_products(
    rows: &[(bool, bool, bool)],
    products: Vec<CertifiedRecoveryProduct>,
) -> Arc<ExactDeclarationContext> {
    original_context_with_fixtures(
        rows,
        products.into_iter().map(DeclarationFixture::new).collect(),
        false,
    )
}

fn original_context_with_fixtures(
    rows: &[(bool, bool, bool)],
    fixtures: Vec<DeclarationFixture>,
    reusable_originals: bool,
) -> Arc<ExactDeclarationContext> {
    let producer = CanonicalProducerIdentity::from_producer_bytes(PRODUCER).sha256();
    let target = ExactModuleIdentity {
        unit: "main".into(),
        module: "OriginalTarget".into(),
    };
    let target_entry = Arc::new(ArtifactEntry::canonical(fixture_module_interface(
        producer,
        &target.unit,
        &target.module,
        BTreeMap::new(),
    )));
    let mut roles = vec![CompilerInputRole::InterfaceOnly {
        interface: target_entry.descriptor.id,
    }];
    let mut entries = vec![target_entry];
    assert_eq!(rows.len(), fixtures.len());
    for ((_, custody, selected), fixture) in rows.iter().zip(fixtures) {
        if *selected {
            roles.push(if reusable_originals && *custody {
                CompilerInputRole::ReusableOriginal {
                    interface: fixture.canonical.descriptor.id,
                    original: fixture.original.descriptor.id,
                }
            } else {
                CompilerInputRole::InterfaceOnly {
                    interface: fixture.canonical.descriptor.id,
                }
            });
        }
        entries.push(fixture.canonical);
        if *custody {
            entries.push(fixture.original);
        }
    }
    let inventory = ArtifactInventory::default();
    let view = inventory
        .admit_shared_with_demand(
            &inventory.empty_view(),
            entries,
            NativeArtifactDemand::ScopeInterfaces,
        )
        .unwrap();
    let context = ExactDeclarationContext::retained_execution_fixture(
        producer,
        &view,
        CompilerInputProjection::restore(&view, &roles).unwrap(),
        vec![ExactLexicalNode {
            owner: target,
            imports: Vec::new(),
        }],
        ExactModuleIdentity {
            unit: "main".into(),
            module: "OriginalTarget".into(),
        },
        &[],
    )
    .unwrap();
    Arc::new(context)
}

fn offered_native_owners(context: &Arc<ExactDeclarationContext>) -> Vec<String> {
    let scratch = tempfile::tempdir().unwrap();
    let lexical = context.lexical_graph().to_vec();
    let roles = context.compiler_input_roles();
    let groups = context.artifact_view().selected_native_groups();
    let private = OriginalCompilerInputs::reuse_selected_authored(context).unwrap();
    let request = ExactCompileContext::new(context.clone())
        .prepare_compilation_with_private_input(scratch.path(), PRODUCER, None, private)
        .unwrap();
    request.validate_artifacts().unwrap();
    assert_eq!(context.lexical_graph(), lexical);
    assert_eq!(context.compiler_input_roles(), roles);
    assert_eq!(context.artifact_view().selected_native_groups(), groups);
    assert!(
        groups.is_empty(),
        "availability must not select executable groups"
    );
    let manifest: Value =
        ciborium::de::from_reader(std::fs::read(&request.manifest).unwrap().as_slice()).unwrap();
    let fields = manifest.as_array().unwrap();
    let originals = request.compiler_original_products().unwrap();
    let mut owners = fields[6]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|row| {
            let row = row.as_array().unwrap();
            let module = string(&row[1]).unwrap();
            let original = originals
                .iter()
                .find(|product| product.owner().module == module)
                .unwrap();
            if !matches!(
                original.module_interface().origin(),
                crate::certified_products::CanonicalOrigin::NativeAuthoredDeclaration { .. }
            ) {
                assert_eq!(
                    row[6],
                    Value::Array(Vec::new()),
                    "ordinary original custody does not advertise private native availability"
                );
                return None;
            }
            let owner = ExactModuleIdentity {
                unit: original.owner().unit.clone(),
                module: original.owner().module.clone(),
            };
            let explicit_original = context
                .compiler_input_projection()
                .snapshot_roles()
                .get(&owner)
                .is_some_and(|role| role.original().is_some());
            // The recovered fixture selects no executable groups. Its explicit
            // Original keeps that bounded-empty permission; authored fallback
            // under an interface-only role carries its complete sparse offer.
            assert_eq!(
                row[6],
                Value::Array(if explicit_original {
                    Vec::new()
                } else {
                    [3u32, 17]
                        .map(|ordinal| Value::Integer(ordinal.into()))
                        .into()
                }),
                "advertised ordinals follow the fixture's issued role and permission"
            );
            assert_eq!(
                row[8],
                if explicit_original {
                    Value::Array(vec![
                        Value::Text("issued-groups".into()),
                        Value::Array(vec![]),
                    ])
                } else {
                    Value::Array(vec![Value::Text("full-carrier".into())])
                }
            );
            assert_eq!(
                row[9],
                Value::Text(
                    if explicit_original {
                        "compiler-original"
                    } else {
                        "private-native"
                    }
                    .into()
                )
            );
            // Authenticate the complete sparse body independently of demand.
            let groups = crate::certified_products::original_available_groups(original)
                .unwrap()
                .collect::<Vec<_>>();
            assert_eq!(groups.len(), 2);
            for (group, ordinal) in groups.into_iter().zip([3u32, 17]) {
                assert_eq!(group.original_ordinal(), ordinal);
                assert_eq!(
                    group.binders(),
                    &[tidepool_repr::execution_schema::SymbolIdentity {
                        unit: "main".into(),
                        module: module.into(),
                        namespace: "value".into(),
                        occurrence: format!("entry_{ordinal}"),
                        record_parent: None,
                    }]
                );
            }
            Some(module.to_owned())
        })
        .collect::<Vec<_>>();
    owners.sort();
    owners
}

pub(crate) fn display_prototype(
    context: &Arc<ExactDeclarationContext>,
) -> Arc<ExactHostBindingPrototype> {
    // Canonical type codec fixture over retained input data. Neither these
    // nominal bytes nor this context issue output-completion evidence.
    let mut structure = Vec::new();
    ciborium::into_writer(
        &array([text("literal"), text("nat"), text("1")]),
        &mut structure,
    )
    .unwrap();
    let wire = array([
        text("TPCANONICALINPUTTYPE1"),
        text("1"),
        array([
            text("TPCHECKEDSIGNATURE2"),
            text("activation-input"),
            text("fixture"),
            Value::Bytes(vec![1]),
            array([]),
        ]),
        Value::Bytes(structure),
        array([]),
    ]);
    let mut bytes = Vec::new();
    ciborium::into_writer(&wire, &mut bytes).unwrap();
    let witness = CanonicalInputTypeWitness::from_bytes(&bytes).unwrap();
    let interfaces = Arc::new(
        context
            .select_original_type_interfaces(&[&witness], &[])
            .unwrap(),
    );
    ExactHostBindingPrototype::from_original_input(witness, interfaces).unwrap()
}

pub(crate) fn with_irrelevant_values(
    context: &Arc<ExactDeclarationContext>,
    versions: &[u8],
    lexical_value: bool,
) -> Arc<ExactDeclarationContext> {
    with_irrelevant_values_named(context, versions, lexical_value, "Tidepool.Session.Val.G")
}

pub(crate) fn with_irrelevant_values_named(
    context: &Arc<ExactDeclarationContext>,
    versions: &[u8],
    lexical_value: bool,
    module_prefix: &str,
) -> Arc<ExactDeclarationContext> {
    let producer = context.toolchain_identity_sha256();
    let values = versions
        .iter()
        .enumerate()
        .map(|(index, version)| {
            let module = format!("{module_prefix}{}", index + 100);
            let bytes = vec![*version];
            let package_roots = array([
                text("TPPKGROOTS"),
                text("2"),
                array([text("main"), text(&module), text(hash(&bytes))]),
                array([]),
                array([]),
            ]);
            let mut packages = Vec::new();
            ciborium::into_writer(&package_roots, &mut packages).unwrap();
            Arc::new(
                crate::recovery_artifacts::CertifiedValueInterface::from_checked_compilation(
                    producer,
                    ExactModuleIdentity {
                        unit: "main".into(),
                        module,
                    },
                    bytes,
                    packages,
                    Vec::new(),
                )
                .unwrap(),
            )
        })
        .collect::<Vec<_>>();
    let mut lexical = context.lexical_graph().to_vec();
    if lexical_value && !values.is_empty() {
        lexical.push(ExactLexicalNode {
            owner: ExactModuleIdentity {
                unit: values[0].interface().unit().into(),
                module: values[0].interface().module().into(),
            },
            imports: vec![],
        });
    }
    let expanded = (**context)
        .clone()
        .extend_with_value_interfaces(&values)
        .unwrap();
    Arc::new(expanded.extend(&[], &[], lexical).unwrap())
}

pub(crate) fn admit_irrelevant_value_root(
    context: &Arc<ExactDeclarationContext>,
    module: &str,
) -> Arc<ExactDeclarationContext> {
    let producer = context.toolchain_identity_sha256();
    let bytes = vec![0x6b];
    let package_roots = array([
        text("TPPKGROOTS"),
        text("2"),
        array([text("main"), text(module), text(hash(&bytes))]),
        array([]),
        array([]),
    ]);
    let mut packages = Vec::new();
    ciborium::into_writer(&package_roots, &mut packages).unwrap();
    let interface = Arc::new(crate::artifact_inventory::ArtifactEntry::interface(
        crate::recovery_artifacts::CertifiedJoinedInterface::from_certification(
            producer,
            "main".into(),
            module.into(),
            bytes,
            packages,
        )
        .unwrap(),
        crate::artifact_inventory::JoinedInterfaceRole::ValueInterface,
        Vec::new(),
    ));
    let interface_id = interface.descriptor.id;
    let view = context
        .artifact_view()
        .inventory()
        .admit_shared_with_demand(
            context.artifact_view(),
            vec![interface],
            crate::artifact_inventory::NativeArtifactDemand::ScopeInterfaces,
        )
        .unwrap();
    let support = context
        .compiler_source_support()
        .retain_custody(&view)
        .unwrap();
    let expanded = context
        .as_ref()
        .clone()
        .extend_issued_compiler_support(&support)
        .unwrap();
    assert!(expanded
        .artifact_view()
        .artifact_ids()
        .contains(&interface_id));
    assert_eq!(
        expanded.compiler_input_projection(),
        context.compiler_input_projection()
    );
    Arc::new(expanded)
}

fn retained_history_property_config() -> proptest::test_runner::Config {
    let mut config = property_config();
    config.cases = 64;
    config
}

proptest::proptest! {
    #![proptest_config(retained_history_property_config())]
    #[test]
    fn retained_native_inputs_ignore_unselected_value_histories(versions in proptest::collection::vec(1u8..255, 1..12)) {
        let original = original_context_with_roles(&[(true, true, true)], true);
        let expected = offered_native_owners(&original);
        let mut previous = original.semantic_sha256();
        for count in 1..=versions.len() {
            let expanded = with_irrelevant_values(&original, &versions[..count], false);
            proptest::prop_assert_ne!(expanded.semantic_sha256(), previous);
            previous = expanded.semantic_sha256();
            proptest::prop_assert_eq!(expanded.value_interfaces().len(), count);
            proptest::prop_assert_eq!(expanded.lexical_graph(), original.lexical_graph());
            proptest::prop_assert_eq!(offered_native_owners(&expanded), expected.clone());
            proptest::prop_assert!(expanded.artifact_view().selected_native_groups().is_empty());
        }
        let visible = with_irrelevant_values(&original, &versions[..1], true);
        proptest::prop_assert_ne!(visible.lexical_graph(), original.lexical_graph());
        proptest::prop_assert_eq!(offered_native_owners(&visible), expected);
        proptest::prop_assert!(visible.source_scope().rows().is_empty());
    }
}

#[test]
fn retained_native_inputs_require_exact_selected_custody() {
    let without_native = original_context_with_roles(&[(true, false, true)], true);
    let with_native = original_context_with_roles(&[(true, true, true)], true);
    assert!(offered_native_owners(&without_native).is_empty());
    assert_eq!(
        offered_native_owners(&with_native),
        ["Tidepool.Session.Lib.G1"]
    );
    let type_only = ExactDeclarationContext::from_authenticated_interfaces(
        with_native.toolchain_identity_sha256(),
        with_native.artifact_view(),
        with_native.compiler_input_projection().clone(),
    )
    .unwrap();
    assert!(OriginalCompilerInputs::reuse_selected_authored(&type_only)
        .unwrap()
        .is_none());
    assert!(type_only.source_scope().rows().is_empty());
}

#[test]
fn retained_native_inputs_do_not_promote_group_demand() {
    let original = original_context_with_roles(&[(true, true, true)], true);
    let expanded = with_irrelevant_values(&original, &[1], false);
    let before_groups = expanded.artifact_view().selected_native_groups();
    assert!(before_groups.is_empty());
    assert_eq!(expanded.value_interfaces().len(), 1);
    assert_eq!(
        offered_native_owners(&expanded),
        ["Tidepool.Session.Lib.G1"]
    );
    assert_eq!(
        expanded.artifact_view().selected_native_groups(),
        before_groups
    );
    assert_eq!(expanded.value_interfaces().len(), 1);
    assert_eq!(expanded.lexical_graph(), original.lexical_graph());
    assert!(expanded.source_scope().rows().is_empty());
}

#[test]
fn retained_inputs_preserve_exact_canonical_implementation_dependencies() {
    use crate::certified_products::fixture_module_core;
    let producer = CanonicalProducerIdentity::from_producer_bytes(PRODUCER).sha256();
    let child = fixture_module_interface(producer, "main", "CapturedDependency", BTreeMap::new());
    let target = fixture_module_interface(
        producer,
        "main",
        "OriginalTarget",
        BTreeMap::from([(
            ("main".into(), "CapturedDependency".into()),
            child.interface_sha256(),
        )]),
    );
    let contexts = [
        b"old implementation".as_slice(),
        b"changed implementation".as_slice(),
    ]
    .map(|core| {
        let inventory = ArtifactInventory::default();
        let entries = vec![
            Arc::new(ArtifactEntry::canonical(target.clone())),
            Arc::new(ArtifactEntry::canonical(fixture_module_core(
                &child,
                core.to_vec(),
            ))),
        ];
        let roles = CompilerInputProjection::from_issued_entries(&entries).unwrap();
        let view = inventory
            .admit_shared(&inventory.empty_view(), entries)
            .unwrap();
        let target = ExactModuleIdentity {
            unit: "main".into(),
            module: "OriginalTarget".into(),
        };
        let dependency = ExactModuleIdentity {
            unit: "main".into(),
            module: "CapturedDependency".into(),
        };
        Arc::new(
            ExactDeclarationContext::retained_execution_fixture(
                producer,
                &view,
                roles,
                vec![
                    ExactLexicalNode {
                        owner: target.clone(),
                        imports: vec![dependency.clone()],
                    },
                    ExactLexicalNode {
                        owner: dependency,
                        imports: vec![],
                    },
                ],
                target,
                &[],
            )
            .unwrap(),
        )
    });
    let selected_types = |context: &ExactDeclarationContext| {
        context
            .compiler_metadata_snapshot()
            .unwrap()
            .entries
            .iter()
            .map(|(owner, entry)| match &entry.payload {
                crate::artifact_inventory::ArtifactPayload::Canonical(interface) => {
                    (owner.clone(), interface.interface_sha256())
                }
                _ => panic!("canonical interface fixture"),
            })
            .collect::<BTreeMap<_, _>>()
    };
    assert_eq!(selected_types(&contexts[0]), selected_types(&contexts[1]));
    assert_eq!(contexts[0].lexical_graph(), contexts[1].lexical_graph());
    assert_ne!(
        contexts[0].semantic_sha256(),
        contexts[1].semantic_sha256(),
        "equal type and lexical interfaces retain distinct implementation dependencies"
    );
    for context in contexts {
        let scratch = tempfile::tempdir().unwrap();
        ExactCompileContext::new(context)
            .prepare_compilation(scratch.path(), PRODUCER)
            .unwrap()
            .validate_artifacts()
            .unwrap();
    }
}

#[test]
fn original_native_declaration_body_is_available_without_heap_or_lexical_promotion() {
    let earlier = original_context(&[]);
    let type_only = original_context(&[(true, true, true)]);
    let later = original_context_with_roles(&[(true, true, true)], true);
    assert!(offered_native_owners(&earlier).is_empty());
    assert_eq!(offered_native_owners(&later), ["Tidepool.Session.Lib.G1"]);
    assert!(
        offered_native_owners(&earlier).is_empty(),
        "a later declaration cannot enter an earlier capture"
    );
    let scratch = tempfile::tempdir().unwrap();
    let private = OriginalCompilerInputs::reuse_selected_authored(&later).unwrap();
    let request = ExactCompileContext::new(later.clone())
        .prepare_compilation_with_private_input(scratch.path(), PRODUCER, None, private)
        .unwrap();
    request.validate_artifacts().unwrap();
    assert!(
        type_only.validate_artifacts(&request.artifacts).is_err(),
        "request availability must not promote persistent type-only roles"
    );
    let native = request
        .artifacts
        .iter()
        .position(|artifact| artifact.product.is_some())
        .unwrap();
    let mut missing_native = request.clone();
    missing_native.artifacts[native].product = None;
    assert!(
        missing_native.validate_artifacts().is_err(),
        "an issued private native role requires its original product"
    );
    let canonical = request
        .artifacts
        .iter()
        .position(|artifact| artifact.product.is_none())
        .unwrap();
    let mut extra_native = request.clone();
    extra_native.artifacts[canonical].product = request.artifacts[native].product.clone();
    assert!(
        extra_native.validate_artifacts().is_err(),
        "private availability cannot give unrelated type-only roles executable products"
    );
    let canonical_only =
        original_context_with_products(&[(true, false, true)], vec![declaration(1, true)]);
    assert!(
        OriginalCompilerInputs::reuse_selected_authored(&canonical_only)
            .unwrap()
            .is_none(),
        "finalized canonical custody cannot issue a native availability census"
    );
}

fn property_config() -> proptest::test_runner::Config {
    let mut config = proptest::test_runner::Config::default();
    if let Some(path) = option_env!("TIDEPOOL_PROPTEST_REGRESSIONS") {
        config.failure_persistence = Some(Box::new(
            proptest::test_runner::FileFailurePersistence::Direct(path),
        ));
    }
    config
}

#[test]
fn original_role_preview_keeps_exact_native_identity_and_refuses_substitution() {
    let canonical = original_context(&[(true, true, true)]);
    let original = original_context_with_roles(&[(true, true, true)], true);
    assert_eq!(
        offered_native_owners(&original).len(),
        1,
        "repeated availability of the same issued original retains one owner"
    );
    let selected_interfaces = |context: &ExactDeclarationContext| {
        context
            .compiler_input_roles()
            .iter()
            .map(|role| role.interface())
            .collect::<BTreeSet<_>>()
    };
    assert_eq!(
        selected_interfaces(&original),
        selected_interfaces(&canonical),
        "an original role retains its selected canonical type evidence"
    );
    assert!(
        offered_native_owners(&canonical).is_empty(),
        "raw native custody under interface-only roles cannot issue an offer"
    );
    let recovered =
        crate::certified_products::tests::recovered_witness_fixtures(&[declaration_groups(
            1,
            true,
            240,
            &[3, 17, 88],
        )]);
    let substitute = &recovered[0].product;
    let selected = original.compiler_original_products().unwrap();
    assert_eq!(
        selected[0].module_interface(),
        substitute.module_interface()
    );
    assert_ne!(selected[0].product_bytes(), substitute.product_bytes());
    assert!(!selected[0].same_durable_artifact(substitute));
    let substitute_entry = Arc::new(
        ArtifactEntry::original(
            CanonicalProducerIdentity::from_producer_bytes(PRODUCER).sha256(),
            substitute.clone(),
        )
        .unwrap(),
    );
    let inventory = ArtifactInventory::default();
    let substitute_view = inventory
        .admit_shared_with_demand(
            &inventory.empty_view(),
            vec![substitute_entry.clone()],
            NativeArtifactDemand::ScopeInterfaces,
        )
        .unwrap();
    let substitute_projection =
        CompilerInputProjection::from_issued_entries(&[substitute_entry.clone()]).unwrap();
    let mut validation = crate::recovery_artifacts::PackageInterfaceValidation::default();
    let mut substitute_selection =
        crate::certified_products::CertifiedSourceSelection::from_compiler_projection(
            &substitute_projection,
            &substitute_view.metadata_snapshot(),
            &validation.inventory,
        )
        .unwrap();
    let substitute_output = crate::declaration_context::CapturedCompilerStage::issue(
        &mut substitute_selection,
        &substitute_view,
        vec![substitute_entry],
        &mut validation,
    )
    .unwrap();
    let substitution_directory = tempfile::tempdir().unwrap();
    assert!(
        ExactCompileContext::new(original.clone())
            .prepare_compilation_with_private_input(
                substitution_directory.path(),
                PRODUCER,
                None,
                Some(substitute_output.compiler_inputs().as_ref().clone()),
            )
            .is_err(),
        "an equal interface cannot substitute another issued implementation"
    );

    let extra = Arc::new(
        ArtifactEntry::original(
            CanonicalProducerIdentity::from_producer_bytes(PRODUCER).sha256(),
            substitute.clone(),
        )
        .unwrap(),
    );
    let expanded = original
        .artifact_view()
        .inventory()
        .admit_shared_with_demand(
            original.artifact_view(),
            vec![extra],
            NativeArtifactDemand::ScopeInterfaces,
        )
        .unwrap();
    let projection = original.compiler_input_projection();
    let available = projection
        .native_declaration_availability(&expanded)
        .unwrap();
    assert_eq!(
        available.len(),
        1,
        "extra available implementations are not selected"
    );
    assert!(available[0].same_durable_artifact(&selected[0]));
    assert!(expanded.selected_native_groups().is_empty());

    let types = original
        .compiler_source_support()
        .selected_interfaces()
        .unwrap()
        .select_owners(projection.snapshot_roles().keys().cloned().collect())
        .unwrap();
    assert!(
        projection.native_declaration_availability(&types).is_err(),
        "a selected original role cannot silently lose native custody"
    );
    let wrong_producer = tempfile::tempdir().unwrap();
    assert!(ExactCompileContext::new(original.clone())
        .prepare_compilation_with_private_input(
            wrong_producer.path(),
            b"wrong preview issuer",
            None,
            OriginalCompilerInputs::reuse_selected_authored(&original).unwrap(),
        )
        .is_err());
}

proptest::proptest! {
    #![proptest_config(property_config())]
    #[test]
    fn original_native_availability_matches_issued_roles_and_survives_later_histories(
        before in proptest::collection::vec((proptest::bool::ANY, proptest::bool::ANY, proptest::bool::ANY), 0..5),
        after in proptest::collection::vec((proptest::bool::ANY, proptest::bool::ANY, proptest::bool::ANY), 0..5),
    ) {
        let expected = |rows: &[(bool, bool, bool)]| {
            let mut owners = rows.iter().enumerate().filter_map(|(index, row)| {
                (row.0 && row.1 && row.2).then(|| format!("Tidepool.Session.Lib.G{}", index + 1))
            }).collect::<Vec<_>>();
            owners.sort();
            owners
        };
        // Every case reaches the complete role/custody/origin matrix, alongside
        // an arbitrary captured prefix and later extension that shrink normally.
        let mut before = before;
        before.extend((0..8).map(|mask| (mask & 1 != 0, mask & 2 != 0, mask & 4 != 0)));
        let captured = original_context_with_roles(&before, true);
        let interface_only = original_context(&before);
        let mut extended = before.clone();
        extended.extend(after);
        let later = original_context_with_roles(&extended, true);
        proptest::prop_assert_eq!(offered_native_owners(&captured), expected(&before));
        proptest::prop_assert!(
            offered_native_owners(&interface_only).is_empty(),
            "interface-only roles carry custody without issuing a native offer"
        );
        proptest::prop_assert_eq!(offered_native_owners(&later), expected(&extended));
        drop(later);
        proptest::prop_assert_eq!(offered_native_owners(&captured), expected(&before));
        proptest::prop_assert!(offered_native_owners(&interface_only).is_empty());
        static COMPLETED_HISTORIES: AtomicUsize = AtomicUsize::new(0);
        let completed = COMPLETED_HISTORIES.fetch_add(1, Ordering::Relaxed) + 1;
        if completed.is_power_of_two() {
            eprintln!("activation preview completed history checks: {completed}");
        }
    }
}

#[test]
fn preview_reuses_selected_original_with_issued_group_custody() {
    let producer = CanonicalProducerIdentity::from_producer_bytes(PRODUCER).sha256();
    let fixture = issued_declarations(true)[0].clone();
    let crate::artifact_inventory::ArtifactPayload::Original(_) = &fixture.original.payload else {
        panic!("preview fixture must carry its certified original product");
    };
    let inventory = ArtifactInventory::default();
    let artifacts = inventory
        .admit_shared_with_demand(
            &inventory.empty_view(),
            vec![fixture.original.clone()],
            NativeArtifactDemand::ScopeInterfaces,
        )
        .unwrap();
    let projection =
        CompilerInputProjection::from_issued_entries(&[fixture.original.clone()]).unwrap();
    let mut validation = crate::recovery_artifacts::PackageInterfaceValidation::default();
    let mut selection =
        crate::certified_products::CertifiedSourceSelection::from_compiler_projection(
            &projection,
            &artifacts.metadata_snapshot(),
            &validation.inventory,
        )
        .unwrap();
    let candidate = crate::declaration_context::CapturedCompilerStage::issue(
        &mut selection,
        &artifacts,
        vec![fixture.original.clone()],
        &mut validation,
    )
    .unwrap();
    let target = fixture.original.descriptor.owner.clone();
    let context = Arc::new(
        ExactDeclarationContext::retained_execution_support_fixture(
            producer,
            crate::artifact_inventory::CompilerSourceSupport::close_complete_output(
                candidate.compiler_inputs(),
            )
            .unwrap(),
            vec![ExactLexicalNode {
                owner: target.clone(),
                imports: vec![],
            }],
            target,
            &[],
        )
        .unwrap(),
    );
    assert!(context.source_scope().rows().is_empty());
    let before = context.as_ref().clone();
    let private = OriginalCompilerInputs::reuse_selected_authored(&context)
        .unwrap()
        .expect("selected Original role retains its issued private availability proof");
    let scratch = tempfile::tempdir().unwrap();
    let request = ExactCompileContext::new(context.clone())
        .prepare_compilation_with_private_input(scratch.path(), PRODUCER, None, Some(private))
        .unwrap();
    request.validate_artifacts().unwrap();
    let availability = request
        .compiler_inputs()
        .native_availability(fixture.original.descriptor.id)
        .unwrap();
    assert!(
        availability
            .offers()
            .any(|offer| offer.original() == fixture.original.descriptor.id
                && offer.proof().is_some())
    );
    assert!(context.same_authenticated_context(&before));
    assert!(context.artifact_view().selected_native_groups().is_empty());
}
