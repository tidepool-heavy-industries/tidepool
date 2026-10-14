use super::*;
use crate::artifact_inventory::{
    ArtifactEntry, ArtifactInventory, CanonicalProducerIdentity, CompilerInputProjection,
    CompilerInputRole, NativeArtifactDemand,
};
use crate::certified_products::{
    fixture_finalized_product, fixture_module_interface, fixture_native_declaration_product,
};
use crate::declaration_context::ExactCompileContext;
use crate::declaration_join::{ExactLexicalNode, ExactModuleIdentity};
use crate::recovery_artifacts::CertifiedRecoveryProduct;
use std::collections::BTreeMap;
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
            canonical: Arc::new(ArtifactEntry::canonical(
                product.module_interface().unwrap().clone(),
            )),
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
    let module =
        tidepool_repr::SessionModule::lib(tidepool_repr::Generation(generation)).module_name();
    let interface = format!("original interface {generation}").into_bytes();
    let product = crate::certified_products::tests::original_groups_fixture_in_unit_with_sites(
        "main",
        &module,
        vec![(3, Vec::new()), (17, Vec::new())],
        generation as u8,
        &BTreeMap::new(),
        interface,
        &BTreeMap::new(),
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
    original_context_with_fixtures(rows, fixtures)
}

fn original_context_with_products(
    rows: &[(bool, bool, bool)],
    products: Vec<CertifiedRecoveryProduct>,
) -> Arc<ExactDeclarationContext> {
    original_context_with_fixtures(
        rows,
        products.into_iter().map(DeclarationFixture::new).collect(),
    )
}

fn original_context_with_fixtures(
    rows: &[(bool, bool, bool)],
    fixtures: Vec<DeclarationFixture>,
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
            roles.push(CompilerInputRole::InterfaceOnly {
                interface: fixture.canonical.descriptor.id,
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
    let context = ExactDeclarationContext::from_authenticated_execution(
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
    let groups = context.artifact_view().selected_native_groups();
    let private = original_native_declaration_inputs(context, PRODUCER).unwrap();
    let request = ExactCompileContext::new(context.clone())
        .prepare_compilation_with_private_input(scratch.path(), PRODUCER, None, private)
        .unwrap();
    request.validate_artifacts().unwrap();
    assert_eq!(context.lexical_graph(), lexical);
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
        .map(|row| {
            let row = row.as_array().unwrap();
            let module = string(&row[1]).unwrap();
            // The manifest advertises ordinals; authenticated original groups
            // retain their independent binder identities and definitions.
            assert_eq!(
                row[6],
                Value::Array(
                    [3u32, 17]
                        .map(|ordinal| Value::Integer(ordinal.into()))
                        .into()
                ),
                "complete sparse native availability census"
            );
            let original = originals
                .iter()
                .find(|product| product.owner().module == module)
                .unwrap();
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
            module.to_owned()
        })
        .collect::<Vec<_>>();
    owners.sort();
    owners
}

pub(crate) fn display_prototype(
    context: &Arc<ExactDeclarationContext>,
) -> Arc<ExactHostBindingPrototype> {
    // Component codec fixture. Context authority comes from the original
    // issuer above, rather than from these nominal signature bytes.
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
    ExactHostBindingPrototype::from_original_input(
        CanonicalInputTypeWitness::from_bytes(&bytes).unwrap(),
        context.clone(),
    )
    .unwrap()
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
        .extend_with_value_interfaces(&values, lexical.clone())
        .unwrap();
    Arc::new(
        ExactDeclarationContext::from_authenticated_execution(
            producer,
            expanded.artifact_view(),
            expanded.compiler_input_projection().clone(),
            lexical,
            context.original_instance_target().unwrap().clone(),
            &[],
        )
        .unwrap(),
    )
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
    let view = context
        .artifact_view()
        .inventory()
        .admit_shared_with_demand(
            context.artifact_view(),
            vec![interface],
            crate::artifact_inventory::NativeArtifactDemand::ScopeInterfaces,
        )
        .unwrap();
    Arc::new(
        ExactDeclarationContext::from_authenticated_execution(
            producer,
            &view,
            context.compiler_input_projection().clone(),
            context.lexical_graph().to_vec(),
            context.original_instance_target().unwrap().clone(),
            &[],
        )
        .unwrap(),
    )
}

fn display_property_config() -> proptest::test_runner::Config {
    let mut config = property_config();
    config.cases = 64;
    config
}

proptest::proptest! {
    #![proptest_config(display_property_config())]
    #[test]
    fn display_projection_ignores_only_unreachable_value_histories(versions in proptest::collection::vec(1u8..255, 1..12)) {
        let original = original_context(&[(true, true, true)]);
        let prototype = display_prototype(&original);
        let expected = OriginalDisplayContext::issue(&prototype, &original).unwrap().unwrap();
        let mut previous = original.semantic_sha256();
        for count in 1..=versions.len() {
            let expanded = with_irrelevant_values(&original, &versions[..count], false);
            proptest::prop_assert_ne!(expanded.semantic_sha256(), previous);
            previous = expanded.semantic_sha256();
            let prototype = display_prototype(&expanded);
            let projected = OriginalDisplayContext::issue(&prototype, &expanded).unwrap().unwrap();
            proptest::prop_assert_eq!(&projected, &expected);
            proptest::prop_assert_eq!(projected.identity(), expected.identity());
            proptest::prop_assert!(projected.declarations().value_interfaces().is_empty());
            proptest::prop_assert_eq!(offered_native_owners(projected.declarations()), vec!["Tidepool.Session.Lib.G1".to_owned()]);
            let reopened = OriginalDisplayContext::issue(&prototype, projected.declarations()).unwrap().unwrap();
            proptest::prop_assert_eq!(reopened, projected);
        }
        let visible = with_irrelevant_values(&original, &versions[..1], true);
        let projected = OriginalDisplayContext::issue(&prototype, &visible).unwrap().unwrap();
        proptest::prop_assert_ne!(&projected, &expected);
        proptest::prop_assert_eq!(projected.declarations().value_interfaces().len(), 1);
    }
}

#[test]
fn display_projection_preserves_native_availability_and_refuses_missing_instance_evidence() {
    let without_native = original_context(&[(true, false, true)]);
    let with_native = original_context(&[(true, true, true)]);
    let prototype = display_prototype(&with_native);
    let absent = OriginalDisplayContext::issue(&prototype, &without_native)
        .unwrap()
        .unwrap();
    let available = OriginalDisplayContext::issue(&prototype, &with_native)
        .unwrap()
        .unwrap();
    assert_ne!(
        absent, available,
        "canonical type equality cannot erase original native custody"
    );
    assert!(offered_native_owners(absent.declarations()).is_empty());
    assert_eq!(
        offered_native_owners(available.declarations()),
        ["Tidepool.Session.Lib.G1"]
    );
    let unknown = ExactDeclarationContext::from_authenticated_interfaces(
        prototype.producer(),
        with_native.artifact_view(),
        with_native.compiler_input_projection().clone(),
    )
    .unwrap();
    assert!(OriginalDisplayContext::issue(&prototype, &unknown)
        .unwrap()
        .is_none());
}

#[test]
fn display_projection_keeps_native_custody_without_promoting_group_demand() {
    let original = original_context(&[(true, true, true)]);
    let prototype = display_prototype(&original);
    let expanded = with_irrelevant_values(&original, &[1], false);
    let before_groups = expanded.artifact_view().selected_native_groups();
    assert!(before_groups.is_empty());
    assert_eq!(expanded.value_interfaces().len(), 1);

    let projected = OriginalDisplayContext::issue(&prototype, &expanded)
        .unwrap()
        .unwrap();
    let declarations = projected.declarations();
    assert!(declarations.value_interfaces().is_empty());
    assert_eq!(
        declarations.artifact_view().selected_native_groups(),
        before_groups,
        "display visibility must not promote retained native custody into executable demand"
    );
    assert_eq!(
        offered_native_owners(declarations),
        ["Tidepool.Session.Lib.G1"],
        "the retained original native body remains available"
    );
}

#[test]
fn display_projection_preserves_exact_canonical_implementation_dependencies() {
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
        let view = inventory
            .admit(
                &inventory.empty_view(),
                vec![
                    ArtifactEntry::canonical(target.clone()),
                    ArtifactEntry::canonical(fixture_module_core(&child, core.to_vec())),
                ],
            )
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
            ExactDeclarationContext::from_authenticated_execution(
                producer,
                &view,
                CompilerInputProjection::from_interface_view(&view).unwrap(),
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
    assert_eq!(
        contexts[0].original_preview_interface_graph().unwrap(),
        contexts[1].original_preview_interface_graph().unwrap(),
        "the full original interface graph is unchanged"
    );
    let displays = contexts.map(|context| {
        OriginalDisplayContext::issue(&display_prototype(&context), &context)
            .unwrap()
            .unwrap()
    });
    assert_ne!(
        displays[0], displays[1],
        "equal type and instance interfaces cannot erase their exact implementation dependency"
    );
    for display in displays {
        let scratch = tempfile::tempdir().unwrap();
        ExactCompileContext::new(display.declarations().clone())
            .prepare_compilation(scratch.path(), PRODUCER)
            .unwrap()
            .validate_artifacts()
            .unwrap();
    }
}

#[test]
fn original_native_declaration_body_is_available_without_heap_or_lexical_promotion() {
    let earlier = original_context(&[]);
    let later = original_context(&[(true, true, true)]);
    assert!(offered_native_owners(&earlier).is_empty());
    assert_eq!(offered_native_owners(&later), ["Tidepool.Session.Lib.G1"]);
    assert!(
        offered_native_owners(&earlier).is_empty(),
        "a later declaration cannot enter an earlier capture"
    );
    let scratch = tempfile::tempdir().unwrap();
    let private = original_native_declaration_inputs(&later, PRODUCER).unwrap();
    let request = ExactCompileContext::new(later.clone())
        .prepare_compilation_with_private_input(scratch.path(), PRODUCER, None, private)
        .unwrap();
    request.validate_artifacts().unwrap();
    assert!(
        later.validate_artifacts(&request.artifacts).is_err(),
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
        original_native_declaration_inputs(&canonical_only, PRODUCER)
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
        let captured = original_context(&before);
        let mut extended = before.clone();
        extended.extend(after);
        let later = original_context(&extended);
        proptest::prop_assert_eq!(offered_native_owners(&captured), expected(&before));
        proptest::prop_assert_eq!(offered_native_owners(&later), expected(&extended));
        drop(later);
        proptest::prop_assert_eq!(offered_native_owners(&captured), expected(&before));
        static COMPLETED_HISTORIES: AtomicUsize = AtomicUsize::new(0);
        let completed = COMPLETED_HISTORIES.fetch_add(1, Ordering::Relaxed) + 1;
        if completed.is_power_of_two() {
            eprintln!("activation preview completed history checks: {completed}");
        }
    }
}
