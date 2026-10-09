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
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use tidepool_repr::execution_schema::{CachedHomeOwner, ModuleVersion, RawModuleProduct};

const PRODUCER: &[u8] = b"preview original native declaration inputs";

fn declaration(generation: u64, native: bool) -> CertifiedRecoveryProduct {
    let module =
        tidepool_repr::SessionModule::lib(tidepool_repr::Generation(generation)).module_name();
    let interface = format!("original interface {generation}").into_bytes();
    let product =
        tidepool_test_data::prepared_encode::encode_module_products(&[RawModuleProduct {
            unit: "main".into(),
            module: module.clone(),
            interface: interface.clone(),
            groups: Vec::new(),
        }]);
    let owner = CachedHomeOwner {
        unit: "main".into(),
        module,
        module_version: ModuleVersion([generation as u8; 32]),
        skinny_iface_sha256: Sha256::digest(&interface).into(),
        product_sha256: Sha256::digest(&product).into(),
    };
    let certificate =
        crate::certified_products::encode_home_certification(&owner, &[], &BTreeMap::new())
            .unwrap();
    let product = CertifiedRecoveryProduct::from_certification(
        owner,
        interface,
        product,
        Vec::new(),
        certificate,
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
    let products = rows
        .iter()
        .enumerate()
        .map(|(index, (native, _, _))| declaration(index as u64 + 1, *native))
        .collect::<Vec<_>>();
    let issued = crate::certified_products::tests::recovered_witness_fixtures(&products)
        .into_iter()
        .map(|original| original.product)
        .collect::<Vec<_>>();
    original_context_with_products(rows, issued)
}

fn original_context_with_products(
    rows: &[(bool, bool, bool)],
    products: Vec<CertifiedRecoveryProduct>,
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
    assert_eq!(rows.len(), products.len());
    for ((_, custody, selected), product) in rows.iter().zip(products) {
        let interface = Arc::new(ArtifactEntry::canonical(
            product.module_interface().unwrap().clone(),
        ));
        if *selected {
            roles.push(CompilerInputRole::InterfaceOnly {
                interface: interface.descriptor.id,
            });
        }
        entries.push(interface);
        if *custody {
            entries.push(Arc::new(
                ArtifactEntry::original(producer, product).unwrap(),
            ));
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
        .map(|row| string(&row.as_array().unwrap()[1]).unwrap().to_owned())
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
    }
}
