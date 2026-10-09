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
    .unwrap()
    .with_compiler_input_projection(CompilerInputProjection::restore(&view, &roles).unwrap())
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
        ciborium::de::from_reader(std::fs::read(request.manifest).unwrap().as_slice()).unwrap();
    let fields = manifest.as_array().unwrap();
    let mut owners = fields[6]
        .as_array()
        .unwrap()
        .iter()
        .map(|row| {
            let row = row.as_array().unwrap();
            let module = string(&row[1]).unwrap();
            // Literal fixture expectations do not consult the issued census or
            // selected groups, so truncation and ordinal renumbering are visible.
            let expected = Value::Array(
                [3u32, 17]
                    .into_iter()
                    .map(|ordinal| {
                        Value::Array(vec![
                            Value::Integer(ordinal.into()),
                            Value::Array(vec![Value::Array(vec![
                                Value::Text("main".into()),
                                Value::Text(module.into()),
                                Value::Text("value".into()),
                                Value::Text(format!("entry_{ordinal}")),
                                Value::Null,
                            ])]),
                            Value::Array(Vec::new()),
                        ])
                    })
                    .collect(),
            );
            assert_eq!(
                row[6], expected,
                "complete sparse native availability census"
            );
            module.to_owned()
        })
        .collect::<Vec<_>>();
    owners.sort();
    owners
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
    let unissued =
        original_context_with_products(&[(true, true, true)], vec![declaration(1, true)]);
    assert!(
        matches!(
            original_native_declaration_inputs(&unissued, PRODUCER),
            Err(CompileError::CompilerEvidence(error)) if matches!(error.as_ref(),
                crate::certified_products::CertificationError::Mismatch(
                    "native availability lacks authenticated census"
                ))
        ),
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
