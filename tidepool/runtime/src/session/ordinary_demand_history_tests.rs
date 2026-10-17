//! Ordinary retained startup and notebook history, separate from exporter breadth checks.
use super::*;
use std::collections::BTreeSet;
use tidepool_repr::execution_schema::{CachedHomeOwner, InventoryDecodeLimits};
use tidepool_toolchain::artifacts::{
    prepare_frozen_production_entry_with_catalog, FrozenEntrySources,
};

const SELECTOR: &str = "session::turn::scaling_tests::ordinary_demand_history_tests::ordinary_entry_late_binding_warm_repeat_and_restore_preserves_work_frontier";
const PROBE: &str = "OrdinaryDemandProbe";

// Expected values are observations of the admitted initial original, never
// authority for compilation, source selection, or recovery.
#[derive(Debug, serde::Serialize, serde::Deserialize)]
pub(in crate::session::turn) struct OriginalExpectation {
    unit: String,
    module: String,
    module_version: String,
    interface_sha256: String,
    product_sha256: String,
    available_ordinals: BTreeSet<u32>,
    entry_ordinal: u32,
    later_ordinal: u32,
}

impl OriginalExpectation {
    fn owner_matches(&self, row: &std::collections::HashMap<String, String>) -> bool {
        let field = |key: &str| row.get(key).map(String::as_str);
        field("group_unit") == Some(self.unit.as_str())
            && field("group_module") == Some(self.module.as_str())
            && field("group_module_version") == Some(self.module_version.as_str())
            && field("group_iface_sha256") == Some(self.interface_sha256.as_str())
            && field("group_product_sha256") == Some(self.product_sha256.as_str())
    }

    fn images(&self, captured: &CapturedCell, ordinal: u32) -> BTreeSet<u64> {
        captured
            .group_work
            .iter()
            .filter(|row| {
                self.owner_matches(row)
                    && row.get("outcome").map(String::as_str) == Some("ready")
                    && row
                        .get("original_ordinal")
                        .and_then(|v| v.parse::<u32>().ok())
                        == Some(ordinal)
            })
            .filter_map(|row| {
                row.get("image_instance")
                    .and_then(|value| value.parse::<u64>().ok())
            })
            .collect()
    }
}

fn digest(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn original_expectation(owner: &CachedHomeOwner, bytes: &[u8]) -> OriginalExpectation {
    let products = tidepool_repr::execution_schema::parse_module_products(
        bytes,
        &tidepool_toolchain::prepared_artifact::production_requirements().unwrap(),
        InventoryDecodeLimits::default(),
    )
    .unwrap();
    let [product] = products.as_slice() else {
        panic!("one certified original per recovery product")
    };
    assert_eq!(
        (&product.unit, &product.module),
        (&owner.unit, &owner.module)
    );
    let ordinal = |name: &str| {
        let groups = product
            .groups
            .iter()
            .filter(|group| {
                group.binders().iter().any(|binder| {
                    binder.unit == owner.unit
                        && binder.module == owner.module
                        && binder.namespace == "value"
                        && binder.record_parent.is_none()
                        && binder.occurrence == name
                })
            })
            .collect::<Vec<_>>();
        assert_eq!(
            groups.len(),
            1,
            "authored binding {name} has one original group"
        );
        groups[0].original_ordinal()
    };
    let expected = OriginalExpectation {
        unit: owner.unit.clone(),
        module: owner.module.clone(),
        module_version: digest(&owner.module_version.0),
        interface_sha256: digest(&owner.skinny_iface_sha256),
        product_sha256: digest(&owner.product_sha256),
        available_ordinals: product
            .groups
            .iter()
            .map(|group| group.original_ordinal())
            .collect(),
        entry_ordinal: ordinal("entry"),
        later_ordinal: ordinal("later"),
    };
    assert_ne!(expected.entry_ordinal, expected.later_ordinal);
    assert!(
        expected.available_ordinals.len() > 2,
        "fixture includes unused bodies"
    );
    expected
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, serde::Deserialize)]
enum GroupWorkStage {
    #[serde(rename = "raw_projection")]
    Projected,
    #[serde(rename = "raw_projection_acquired")]
    Acquired,
    #[serde(rename = "raw_projection_reuse")]
    Reused,
    #[serde(rename = "raw_encoding_demand")]
    EncodingDemand,
    #[serde(rename = "accepted_output")]
    Accepted,
}

#[derive(serde::Deserialize)]
struct GroupWorkRow {
    worker_pid: u64,
    projection_observation: Option<u64>,
    stage: GroupWorkStage,
    unit: String,
    module: String,
    ordinals: BTreeSet<u32>,
    refused_ordinals: BTreeSet<u32>,
    complete: bool,
}

fn validate_group_work(rows: &[serde_json::Value]) {
    let typed = rows
        .iter()
        .map(|row| serde_json::from_value::<GroupWorkRow>(row.clone()).unwrap())
        .collect::<Vec<_>>();
    let mut observed: std::collections::BTreeMap<(u64, u64), &GroupWorkRow> =
        std::collections::BTreeMap::new();
    let mut owners = std::collections::BTreeMap::new();
    for row in &typed {
        assert!(row.complete);
        assert!(row.ordinals.is_disjoint(&row.refused_ordinals));
        let Some(token) = row.projection_observation else {
            continue;
        };
        let key = (row.worker_pid, token);
        if let Some(previous) = owners.insert(key, (&row.unit, &row.module)) {
            assert_eq!(
                (&row.unit, &row.module),
                previous,
                "one token retains its exact raw owner"
            );
        }
        if row.stage != GroupWorkStage::Accepted {
            if let Some(previous) = observed.insert(key, row) {
                assert_eq!(
                    (&row.ordinals, &row.refused_ordinals),
                    (&previous.ordinals, &previous.refused_ordinals),
                    "same raw result retains group outcomes across acquisition and encoding demand"
                );
            }
        }
    }
    for row in &typed {
        let Some(token) = row.projection_observation else {
            continue;
        };
        if row.stage == GroupWorkStage::Accepted {
            if let Some(raw) = observed.get(&(row.worker_pid, token)) {
                assert_eq!((&row.unit, &row.module), (&raw.unit, &raw.module));
                assert!(
                    row.ordinals.is_subset(&raw.ordinals),
                    "accepted output is selected from the same observed raw result"
                );
            }
            // A prior-request token with no local raw row remains unknown.
        }
    }
}

// This callback observes only the physical output of the request which has just
// passed the ordinary admission owner. It does not create or modify any product.
pub(in crate::session::turn) fn observe_compiler_work(
    program: &tidepool_toolchain::checked_cell::CellProgram,
    directory: &Path,
    stderr: &[u8],
    expected: &OriginalExpectation,
    demanded: u32,
) {
    assert!(!program.items().is_empty());
    let rows = String::from_utf8_lossy(stderr)
        .lines()
        .filter_map(|line| {
            line.strip_prefix("tidepool-group-work ").map(|json| {
                serde_json::from_str::<serde_json::Value>(json)
                    .expect("complete worker group observation")
            })
        })
        .collect::<Vec<_>>();
    assert!(
        !rows.is_empty(),
        "run requires the actual compiler's timing-enabled trace profile"
    );
    validate_group_work(&rows);
    let reuse = String::from_utf8_lossy(stderr)
        .lines()
        .filter_map(|line| {
            line.strip_prefix("tidepool-reuse ")
                .map(|json| serde_json::from_str::<serde_json::Value>(json).unwrap())
        })
        .collect::<Vec<_>>();
    assert!(
        !reuse.is_empty(),
        "actual captured/prepared module observations are enabled"
    );
    let scope_path = directory.join("exact-scope/exact-declaration-scope.cbor");
    let scope: CborValue =
        ciborium::de::from_reader(std::fs::File::open(&scope_path).unwrap()).unwrap();
    let scope = scope.as_array().unwrap();
    assert_eq!(scope[0].as_text(), Some("TPEXACTSCOPE"));
    assert_eq!(scope[1].as_text(), Some("15"));
    let originals = scope[6].as_array().unwrap();
    let matching = originals
        .iter()
        .filter_map(|row| {
            let fields = row.as_array().unwrap();
            (fields[0].as_text() == Some(expected.unit.as_str())
                && fields[1].as_text() == Some(expected.module.as_str()))
            .then_some(fields)
        })
        .collect::<Vec<_>>();
    assert_eq!(
        matching.len(),
        1,
        "ordinary cell consumes the retained original"
    );
    let original = matching[0];
    for (index, wanted) in [
        (2, &expected.module_version),
        (3, &expected.interface_sha256),
        (4, &expected.product_sha256),
    ] {
        assert_eq!(
            &original[index].as_text().unwrap().to_owned(),
            wanted,
            "exact retained original identity"
        );
    }
    let selected = original[6]
        .as_array()
        .unwrap()
        .iter()
        .map(|value| u32::try_from(u64::try_from(value.as_integer().unwrap()).unwrap()).unwrap())
        .collect::<BTreeSet<_>>();
    assert!(selected.is_subset(&expected.available_ordinals));
    let availability = original[8].as_array().unwrap();
    match availability[0].as_text().unwrap() {
        "full-carrier" => assert_eq!(availability.len(), 1),
        "issued-groups" => {
            let issued = availability[1]
                .as_array()
                .unwrap()
                .iter()
                .map(|value| {
                    u32::try_from(u64::try_from(value.as_integer().unwrap()).unwrap()).unwrap()
                })
                .collect::<BTreeSet<_>>();
            assert!(
                issued.contains(&demanded),
                "this cell's actual demanded ordinal is issued"
            );
            assert!(issued.is_subset(&expected.available_ordinals));
        }
        other => panic!("unclassified admitted original availability: {other}"),
    }
    eprintln!(
        "ordinary-original-work {}",
        serde_json::json!({
            "schema": 1, "request_output": directory, "scope_path": scope_path,
            "expected_original": expected, "demanded_ordinal": demanded, "context_selected_ordinals": selected,
            "availability": original[8], "worker_work": rows, "module_reuse": reuse,
            "encoding_counts_are_demands_not_evaluations": true,
        })
    );
}

fn run_cell(
    resident: &mut ScaleSession,
    public: ScopeId,
    effects: &TestEffectSurface,
    images: &ImageRegistry,
    publication: &ScalePublication,
    observer: &CellRequestObserver,
    expected: &OriginalExpectation,
    label: &str,
    binding: &str,
    value: i64,
) -> CapturedCell {
    let source = format!("import qualified {PROBE}\nif {PROBE}.{binding} == ({value} :: Int) then pure () else error \"ordinary original returned wrong value\"");
    let ordinal = if binding == "entry" {
        expected.entry_ordinal
    } else {
        expected.later_ordinal
    };
    let (result, captured) =
        with_cell_request_capture(observer, "ordinary-demand-history", label, &source, || {
            try_execute_cell_with_template_preamble_observed(
                resident,
                public,
                effects,
                images,
                (0, 0),
                label,
                &source,
                CellDeclarationExpectation::Total(1),
                publication,
                AuthorityChecks::OrdinaryGroupWork(expected, ordinal),
                &SourceImports::default(),
                None,
                "",
                |_| {},
            )
        });
    result.unwrap();
    assert!(
        !captured.compiler_requests.is_empty(),
        "actual ordinary compiler front door"
    );
    assert!(
        !expected.images(&captured, ordinal).is_empty(),
        "native owner and ordinal must join the admitted original"
    );
    captured
}

#[test]
#[ignore = "requires a timing-enabled resident compiler and retained fresh-process workspace"]
fn ordinary_entry_late_binding_warm_repeat_and_restore_preserves_work_frontier() {
    tidepool_testing::eval_harness::require_extract();
    assert_eq!(
        std::env::var("TIDEPOOL_TIMING").as_deref(),
        Ok("1"),
        "use --trace-profile full at compiler startup"
    );
    let child_root =
        std::env::var_os("TIDEPOOL_ORDINARY_DEMAND_CHILD_WORKSPACE").map(PathBuf::from);
    let root_path = child_root.clone().unwrap_or_else(|| {
        let artifacts = PathBuf::from(
            std::env::var_os("TIDEPOOL_TEST_ARTIFACT_ROOT")
                .expect("declared runner retains the durable workspace"),
        );
        tempfile::Builder::new()
            .prefix("ordinary-demand-workspace-")
            .tempdir_in(artifacts)
            .unwrap()
            .keep()
    });
    let expected_path = root_path.join("ordinary-original-expectation.json");
    let effects = TestEffectSurface::minimal(&[]).unwrap();
    let recovery = if child_root.is_some() {
        crate::session::recovery_test_fixture::RecoveryFixture::reopen(
            crate::session::recovery_test_fixture::run_owner(&root_path),
        )
    } else {
        crate::session::recovery_test_fixture::RecoveryFixture::create(&root_path)
    };
    let owner = RecoveryPublicOwner::new(
        &tidepool_repr::ActorPath::parse("root/ordinary-demand").unwrap(),
        1,
    )
    .unwrap();
    let source_root = root_path.join("original-sources");
    let mut includes = vec![source_root.clone()];
    includes.extend_from_slice(effects.include_paths());
    let mut lib = SessionLib::open(
        SessionId(if child_root.is_some() { 1103 } else { 1102 }),
        root_path.join("compiler"),
        ModuleEnv::standalone_default(),
    )
    .unwrap()
    .with_validation_include(includes.clone());
    tidepool_testing::with_settlement(|settlement| recovery.attach_root(&mut lib, settlement))
        .unwrap();
    let images = Arc::new(ImageRegistry::new());
    let mut persistent = PersistentSession::new(Some(lib), 1024 * 1024);
    persistent.set_image_registry(images.clone());
    let public = if child_root.is_some() {
        persistent.recover_public_scope(&owner).unwrap()
    } else {
        persistent.mint_scope(ScopeId::ROOT).unwrap()
    };
    let mut resident =
        ResidentSession::from_persistent_for_test(frunk::HNil, QuietOutput, persistent);
    resident
        .set_run_context(SessionRunContext {
            lexical_scope: public,
            ..SessionRunContext::ROOT
        })
        .unwrap();
    let observer = CellRequestObserver::default();
    observer.0.lock().capture_group_work = true;
    let expected = if child_root.is_some() {
        serde_json::from_slice::<OriginalExpectation>(&std::fs::read(&expected_path).unwrap())
            .unwrap()
    } else {
        std::fs::create_dir_all(&source_root).unwrap();
        std::fs::write(
            source_root.join("OrdinaryDemandProbe.hs"),
            include_str!("fixtures/ordinary-demand-probe.hs"),
        )
        .unwrap();
        let entry_source = source_root.join("OrdinaryDemandEntry.hs");
        let mut entry_module =
            with_resume_import(include_str!("fixtures/ordinary-demand-entry.hs"));
        entry_module.push_str(&prepared_scaffold_binding("__result"));
        std::fs::write(&entry_source, entry_module).unwrap();
        let frozen = FrozenEntrySources::capture(&includes, &entry_source).unwrap();
        let scratch = root_path.join("entry-preparation");
        std::fs::create_dir_all(&scratch).unwrap();
        let entry = tidepool_testing::with_settlement(|settlement| {
            prepare_frozen_production_entry_with_catalog(
                &frozen,
                &scratch,
                &scratch.join("entry"),
                &tidepool_toolchain::toolchain::CatalogSelection::Acquired(None),
                settlement,
            )
        })
        .unwrap();
        let originals = entry
            .products()
            .recovery_products
            .iter()
            .filter(|product| product.owner().unit == "main" && product.owner().module == PROBE)
            .collect::<Vec<_>>();
        assert_eq!(originals.len(), 1);
        let expected = original_expectation(originals[0].owner(), originals[0].product_bytes());
        std::fs::write(
            &expected_path,
            serde_json::to_vec_pretty(&expected).unwrap(),
        )
        .unwrap();
        let compiled = CompiledTurn::from_production_entry(&entry).unwrap();
        let capsule = resident.prepare_startup_entry(compiled.code()).unwrap();
        assert!(matches!(
            resident
                .initialize_durable_public_scope(owner.clone(), public)
                .unwrap(),
            PublicManifestCommit::Durable
        ));
        let bootstrap = resident
            .begin_durable_public_bootstrap(owner.clone(), public)
            .unwrap();
        assert!(matches!(
            tidepool_testing::with_settlement(
                |settlement| resident.run_startup_entry(capsule, settlement)
            )
            .unwrap(),
            crate::session::ResidentOutcome::Completed { .. }
        ));
        assert!(matches!(
            resident
                .publish_durable_public_bootstrap(bootstrap)
                .unwrap(),
            PublicManifestCommit::Durable
        ));
        expected
    };
    let publication = ScalePublication::Durable { owner, recovery };
    if child_root.is_some() {
        run_cell(
            &mut resident,
            public,
            &effects,
            &images,
            &publication,
            &observer,
            &expected,
            "fresh-process-restore",
            "later",
            73,
        );
        eprintln!(
            "ordinary-demand-restore {}",
            serde_json::json!({"process_id": std::process::id(), "original": expected, "outcome": 73})
        );
        return;
    }
    let first = run_cell(
        &mut resident,
        public,
        &effects,
        &images,
        &publication,
        &observer,
        &expected,
        "small-entry",
        "entry",
        42,
    );
    let later = run_cell(
        &mut resident,
        public,
        &effects,
        &images,
        &publication,
        &observer,
        &expected,
        "late-binding",
        "later",
        73,
    );
    let warm = run_cell(
        &mut resident,
        public,
        &effects,
        &images,
        &publication,
        &observer,
        &expected,
        "warm-repeat",
        "later",
        73,
    );
    let later_images = expected.images(&later, expected.later_ordinal);
    let mut warm_keys = BTreeSet::new();
    for row in warm.group_work.iter().filter(|row| {
        expected.owner_matches(row)
            && row.get("outcome").map(String::as_str) == Some("ready")
            && row
                .get("original_ordinal")
                .and_then(|value| value.parse::<u32>().ok())
                == Some(expected.later_ordinal)
    }) {
        let key = (
            row.get("image_registry").unwrap(),
            row.get("key_domain").unwrap(),
            row.get("image_entry").unwrap().parse::<u64>().unwrap(),
        );
        // Only the first decision can describe reuse from the prior cell.
        // Later hits may refer to an image acquired within this cell.
        if !warm_keys.insert(key) {
            continue;
        }
        let image = row.get("image_instance").unwrap().parse::<u64>().unwrap();
        if row.get("disposition").map(String::as_str) == Some("live_hit") {
            assert!(
                later.group_work.iter().any(|previous| {
                    expected.owner_matches(previous)
                        && previous.get("outcome").map(String::as_str) == Some("ready")
                        && previous.get("original_ordinal") == row.get("original_ordinal")
                        && previous.get("image_registry") == row.get("image_registry")
                        && previous.get("key_domain") == row.get("key_domain")
                        && previous.get("image_entry") == row.get("image_entry")
                        && previous
                            .get("image_instance")
                            .and_then(|v| v.parse::<u64>().ok())
                            == Some(image)
                }),
                "a warm live hit retains the prior exact owner/key image"
            );
        }
        eprintln!(
            "ordinary-demand-warm-decision {}",
            serde_json::to_string(row).unwrap()
        );
    }
    assert!(
        !warm_keys.is_empty(),
        "warm execution observes its exact original image decision"
    );
    eprintln!(
        "ordinary-demand-frontier {}",
        serde_json::json!({"original": expected, "entry_images": expected.images(&first, expected.entry_ordinal), "later_images": later_images, "warm_images": expected.images(&warm, expected.later_ordinal), "unused_unattempted_assertion": "pending demand-lowering repair"})
    );
    drop(resident);
    drop(publication);
    std::fs::remove_file(source_root.join("OrdinaryDemandProbe.hs")).unwrap();
    std::fs::remove_file(source_root.join("OrdinaryDemandEntry.hs")).unwrap();
    let output = std::process::Command::new(std::env::current_exe().unwrap())
        .args([SELECTOR, "--ignored", "--exact", "--nocapture"])
        .env("TIDEPOOL_ORDINARY_DEMAND_CHILD_WORKSPACE", &root_path)
        .output()
        .unwrap();
    eprintln!("{}", String::from_utf8_lossy(&output.stderr));
    println!("{}", String::from_utf8_lossy(&output.stdout));
    std::fs::write(root_path.join("fresh-process.stdout.log"), &output.stdout).unwrap();
    std::fs::write(root_path.join("fresh-process.stderr.log"), &output.stderr).unwrap();
    assert!(
        output.status.success(),
        "fresh process must restore exact original and execute later binding"
    );
    assert!(String::from_utf8_lossy(&output.stdout).contains("1 passed; 0 failed;"));
}
