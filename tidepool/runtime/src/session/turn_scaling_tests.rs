//! Complete compiled-cell measurements. Each row reports owning counters; compiler
//! interface I/O remains in the independent `TIDEPOOL_TIMING=1` worker trace.

use super::*;
use crate::session::{
    resident_cell_check_template, resident_workbench_templates, CertifiedDeclarationPublication,
    ExecutionPublication, ModuleEnv, OutputSink, PersistentSession, PublicManifestCommit,
    PublicationDecision, RecoveryPublicOwner, ResidentSession, SessionLib, SessionRunContext,
    SourceImports,
};
use std::time::{Duration, Instant};
use tidepool_codegen::{prepared_program::ImageRegistry, scope::ScopeId};
use tidepool_repr::SessionId;
use tidepool_testing::effect_surface::TestEffectSurface;
use tidepool_toolchain::checked_cell::CheckedItemKind;

#[derive(Clone)]
struct QuietOutput;

impl OutputSink for QuietOutput {
    fn drain(&self) -> Vec<String> {
        Vec::new()
    }

    fn snapshot(&self) -> Vec<String> {
        Vec::new()
    }
}

type ScaleSession = ResidentSession<frunk::HNil, QuietOutput>;

/// Choose the existing publication owner; durable measurements never use the
/// ephemeral publication shortcut.
enum ScalePublication {
    Ephemeral,
    Durable {
        owner: RecoveryPublicOwner,
        manifest: PathBuf,
    },
}

fn counters(resident: &ScaleSession, images: &ImageRegistry) -> serde_json::Value {
    let residency = resident.residency().unwrap_or_default();
    let (functions, code_bytes) = resident.codegen_totals().unwrap_or_default();
    serde_json::json!({
        "compiler_submissions": tidepool_extract_cmd::extract_spawn_count(),
        "image_elections": images.misses(), "image_hits": images.hits(),
        "codegen_functions": functions, "codegen_bytes": code_bytes,
        "programs": residency.programs, "block_words": residency.block_words,
        "persistent_roots": residency.persistent_roots, "handles": residency.handles,
        "code_exports": residency.code_exports, "parked": residency.parked,
        "static_regions": residency.static_regions,
        "descriptor_rows": residency.descriptor_rows,
        "callable_rows": residency.callable_rows, "enter_rows": residency.enter_rows,
    })
}

fn measured_duration<T>(
    resident: &mut ScaleSession,
    images: &ImageRegistry,
    scenario: (usize, usize),
    phase: &str,
    item: Option<usize>,
    action: impl FnOnce(&mut ScaleSession) -> T,
) -> (T, u128) {
    let before = counters(resident, images);
    let started = Instant::now();
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| action(resident)));
    let elapsed_ns = started.elapsed().as_nanos();
    eprintln!(
        "protected-scale {}",
        serde_json::json!({
            "schema": 1, "prefix": scenario.0, "baseline": scenario.1,
            "phase": phase, "item": item, "elapsed_ns": elapsed_ns,
            "completed": result.is_ok(),
            "before": before, "after": counters(resident, images),
        })
    );
    match result {
        Ok(value) => (value, elapsed_ns),
        Err(panic) => std::panic::resume_unwind(panic),
    }
}

fn measured<T>(
    resident: &mut ScaleSession,
    images: &ImageRegistry,
    scenario: (usize, usize),
    phase: &str,
    item: Option<usize>,
    action: impl FnOnce(&mut ScaleSession) -> T,
) -> T {
    measured_duration(resident, images, scenario, phase, item, action).0
}

fn execute_cell(
    resident: &mut ScaleSession,
    public: ScopeId,
    effects: &TestEffectSurface,
    images: &ImageRegistry,
    scenario: (usize, usize),
    label: &str,
    source: &str,
    declarations: usize,
    expected_display: Option<&str>,
    publication_target: &ScalePublication,
) -> Duration {
    let cell_started = Instant::now();
    let execution = Arc::new(resident.begin_private_execution(public).unwrap());
    let view = execution.view();
    let imports = view.turn_imports(&SourceImports::new());
    let template = resident_cell_check_template(effects.preamble(), effects.row(), &imports);
    let templates = resident_workbench_templates(effects.preamble(), effects.row(), &imports);
    let specification = CheckedCellSpecification {
        admission_digest: [0; 32],
        cell_source: source.into(),
        template_source: template.clone(),
        turn_templates: templates
            .iter()
            .map(|template| (template.kind.wire_name().into(), template.source.clone()))
            .collect(),
        injected_modules: view.injected_module_names(),
        reserved_declaration_modules: Vec::new(),
    };
    let admitted_include = view.include_paths(effects.include_paths());
    let plan = measured(
        resident,
        images,
        scenario,
        &format!("{label}.parse"),
        None,
        |_| {
            tidepool_toolchain::artifacts::parse_cell_plan(
                Arc::new(specification.clone()),
                &admitted_include,
            )
            .unwrap()
        },
    );
    assert_eq!(
        plan.items()
            .iter()
            .filter(|item| matches!(
                item.kind(),
                tidepool_toolchain::cell_plan::ParsedCellPlanKind::Prologue
                    | tidepool_toolchain::cell_plan::ParsedCellPlanKind::Declaration
            ))
            .count(),
        declarations
    );
    let admission = resident
        .admit_planned_cell_for_execution(
            execution.clone(),
            plan,
            Arc::new(specification.clone()),
            specification.specification_digest(),
            [1; 32],
            admitted_include,
        )
        .unwrap();
    let view = admission.view();
    let include = view.include_paths(effects.include_paths());
    let include = include.iter().map(PathBuf::as_path).collect::<Vec<_>>();
    let injected = view.injected_module_names();
    let (checked, program) = measured(
        resident,
        images,
        scenario,
        &format!("{label}.compile_cell"),
        None,
        |_| {
            compile_cell_program_admitted(
                CellCheckRequest {
                    exact_context: view.exact_declaration_context().cloned(),
                    session_id: Some(view.session()),
                    cell_text: source,
                    template: &template,
                    include: &include,
                    session_root: view.session_root(),
                    inject_modules: &injected,
                    compile_generation: admission.initial_value_generation().0,
                    compile_view_evidence: "",
                },
                admission.clone(),
                &templates,
            )
            .unwrap()
        },
    );
    assert!(!checked.items.is_empty());
    eprintln!(
        "protected-scale {}",
        serde_json::json!({
            "schema": 1, "prefix": scenario.0, "baseline": scenario.1,
            "phase": format!("{label}.checked_inventory"),
            "binder_counts": (0..checked.items.len())
                .map(|index| checked.checked_item(index).unwrap().binders().len())
                .collect::<Vec<_>>(),
        })
    );
    let prefix = resident
        .begin_cell_program(admission, program)
        .unwrap()
        .expect("nonempty compiled cell has an ordered prefix");
    let submissions_before_effects = tidepool_extract_cmd::extract_spawn_count();
    resident
        .set_run_context(SessionRunContext {
            lexical_scope: execution.private_scope(),
            ..Default::default()
        })
        .unwrap();
    let mut rendered_count = 0;
    for index in 0..checked.items.len() {
        let item = checked.checked_item(index).unwrap();
        let reservation = resident
            .admit_checked_item(prefix.clone(), item.clone())
            .unwrap();
        if item.kind() == CheckedItemKind::Declaration {
            measured(
                resident,
                images,
                scenario,
                &format!("{label}.adopt"),
                Some(index),
                |resident| {
                    resident.adopt_checked_declaration(reservation).unwrap();
                },
            );
        } else {
            let TurnResult::Bind {
                bound, compiled, ..
            } = measured(
                resident,
                images,
                scenario,
                &format!("{label}.native_materialize"),
                Some(index),
                |_| consume_cell_program_item(reservation.clone()).unwrap(),
            )
            else {
                panic!("checked native item did not return its binding recipe")
            };
            if item.kind() == CheckedItemKind::Bind {
                assert_eq!(
                    bound
                        .iter()
                        .map(|binder| binder.name.as_str())
                        .collect::<Vec<_>>(),
                    item.binders()
                        .iter()
                        .map(String::as_str)
                        .collect::<Vec<_>>(),
                    "native recipe must preserve the complete checked binder inventory"
                );
                measured(
                    resident,
                    images,
                    scenario,
                    &format!("{label}.native_bind"),
                    Some(index),
                    |resident| {
                        if bound.len() == 1 {
                            resident
                                .run_bind_with_sites(
                                    &bound[0].name,
                                    compiled.code(),
                                    &bound[0],
                                    reservation.generation(),
                                )
                                .unwrap();
                        } else {
                            assert!(!bound.is_empty());
                            resident
                                .run_projected_bind_with_sites(
                                    label,
                                    compiled.code(),
                                    &bound,
                                    reservation.generation(),
                                )
                                .unwrap();
                        }
                    },
                );
            } else {
                measured(
                    resident,
                    images,
                    scenario,
                    &format!("{label}.native_observe"),
                    Some(index),
                    |resident| {
                        resident
                            .run_observation_with_sites(
                                compiled.code(),
                                &bound[0],
                                reservation.generation(),
                                false,
                            )
                            .unwrap();
                    },
                );
                let display = resident
                    .admit_checked_display(
                        prefix.clone(),
                        compiled
                            .certification
                            .as_ref()
                            .unwrap()
                            .checked_execution()
                            .unwrap()
                            .clone(),
                        &bound[0],
                        256,
                        Vec::new(),
                    )
                    .unwrap();
                let TurnResult::Bind {
                    bound, compiled, ..
                } = measured(
                    resident,
                    images,
                    scenario,
                    &format!("{label}.display_materialize"),
                    Some(index),
                    |_| consume_cell_program_display(display.clone()).unwrap(),
                )
                else {
                    panic!("checked display did not return its binding recipe")
                };
                let [page, metadata, alias] = bound.as_slice() else {
                    panic!("display requires three rows")
                };
                let rendered = measured(
                    resident,
                    images,
                    scenario,
                    &format!("{label}.native_display"),
                    Some(index),
                    |resident| {
                        resident
                            .run_checked_display_bundle_with_sites(
                                compiled.code(),
                                page,
                                metadata,
                                alias,
                                display.generation(),
                                display,
                            )
                            .unwrap()
                    },
                );
                assert_eq!(
                    rendered.result().to_json(),
                    serde_json::json!([expected_display.unwrap(), false, false])
                );
                rendered_count += 1;
            }
        }
        assert_eq!(prefix.snapshot().compiler_prefix().next_item(), index + 1);
        assert_eq!(
            tidepool_extract_cmd::extract_spawn_count(), submissions_before_effects,
            "native execution and display must consume the immutable cell without compiler requests"
        );
    }
    assert_eq!(rendered_count, usize::from(expected_display.is_some()));
    let work = checked.checked_item(0).unwrap().input_work();
    eprintln!(
        "protected-scale {}",
        serde_json::json!({
            "schema": 1, "prefix": scenario.0, "baseline": scenario.1, "phase": format!("{label}.checked_input_work"),
            "initial_files_written": work.initial_files_written,
            "initial_bytes_written_and_hashed": work.initial_bytes_written_and_hashed,
            "output_files_hashed": work.output_files_hashed, "output_bytes_hashed": work.output_bytes_hashed,
        })
    );
    let private_winners = resident
        .public_visibility_snapshot_in(execution.private_scope())
        .unwrap()
        .bindings;
    let intent = measured(
        resident,
        images,
        scenario,
        &format!("{label}.freeze"),
        None,
        |resident| resident.freeze_private_execution(&execution).unwrap(),
    );
    let publication = match publication_target {
        ScalePublication::Ephemeral => resident.restage_ephemeral_execution_publication(intent),
        ScalePublication::Durable { owner, .. } => {
            resident.restage_execution_publication(owner.clone(), intent)
        }
    }
    .unwrap();
    let (ticket, certification_ns, stage_ns) = match publication {
        ExecutionPublication::Bindings(base) => {
            let (ticket, stage_ns) = measured_duration(
                resident,
                images,
                scenario,
                &format!("{label}.metadata_stage_file_sync"),
                None,
                |_| base.stage().unwrap(),
            );
            (ticket, None, stage_ns)
        }
        ExecutionPublication::Declarations(base) => {
            let (certified, certification_ns) = measured_duration(
                resident,
                images,
                scenario,
                &format!("{label}.declaration_certification"),
                None,
                |_| base.certify().unwrap(),
            );
            let CertifiedDeclarationPublication::Accepted(accepted) = certified else {
                panic!("actual original declaration/prefix publication was rejected")
            };
            let (ticket, stage_ns) = measured_duration(
                resident,
                images,
                scenario,
                &format!("{label}.metadata_stage_file_sync"),
                None,
                |_| accepted.stage().unwrap(),
            );
            (ticket, Some(certification_ns), stage_ns)
        }
    };
    let (_, publication_ns) = measured_duration(
        resident,
        images,
        scenario,
        &format!("{label}.publication_rename_directory_sync"),
        None,
        |resident| {
            let result = resident
                .publish_staged_public_manifest(ticket, &PublicationDecision::new())
                .unwrap();
            assert_eq!(
                result,
                match publication_target {
                    ScalePublication::Ephemeral => PublicManifestCommit::Ephemeral,
                    ScalePublication::Durable { .. } => PublicManifestCommit::Durable,
                }
            );
        },
    );
    assert_eq!(
        resident
            .public_visibility_snapshot_in(public)
            .unwrap()
            .bindings,
        private_winners,
        "publication must preserve the actual native winners"
    );
    // Retained snapshots and diagnostic reads do not belong to cell latency.
    let elapsed = cell_started.elapsed();
    if let ScalePublication::Durable { manifest, .. } = publication_target {
        let bytes = std::fs::read(manifest).unwrap();
        let document: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        let snapshot = manifest.with_file_name(format!("declarations-{label}.json"));
        std::fs::write(&snapshot, &bytes).unwrap();
        eprintln!(
            "durable-performance {}",
            serde_json::json!({
                "schema": 1, "composition": "durable-publication", "cell": label,
                "prefix": scenario.0, "baseline": scenario.1, "completed": true,
                "elapsed_ns": elapsed.as_nanos(), "certification_ns": certification_ns,
                "metadata_stage_file_sync_ns": stage_ns,
                "publication_rename_directory_sync_ns": publication_ns,
                "manifest_path": snapshot, "manifest_bytes": bytes.len(),
                "manifest_blake3": blake3::hash(&bytes).to_hex().to_string(),
                "manifest_checksum": document.get("checksum"),
                "public_schema": document.get("public_schema"),
                "checksum_encode_bytes": null, "artifact_hash_bytes": null,
                "manifest_write_bytes": null, "whole_graph_copies": null,
            })
        );
    }
    let inventory = resident.compile_view_in(public).and_then(|view| {
        view.exact_declaration_context()
            .map(|context| context.artifact_view().inventory().metrics())
    });
    eprintln!(
        "protected-scale {}",
        serde_json::json!({
            "schema": 1, "prefix": scenario.0, "baseline": scenario.1,
            "phase": format!("{label}.artifact_inventory"), "inventory": inventory,
        })
    );
    elapsed
}

fn growing_prefix_with_publication(prefix: usize, baseline: usize, durable: bool) {
    tidepool_testing::eval_harness::require_extract();
    let no_daemon = std::env::var("TIDEPOOL_EXTRACT_NO_DAEMON");
    if durable {
        assert_ne!(
            no_daemon.as_deref(),
            Ok("1"),
            "durable scaling requires the resident compiler"
        );
        let socket = PathBuf::from(
            std::env::var_os(tidepool_extract_cmd::DAEMON_SOCKET_ENV)
                .expect("resident scaling socket"),
        );
        tidepool_extract_cmd::preflight_compiler_daemon(&socket).unwrap();
    } else {
        assert_eq!(
            no_daemon.as_deref(),
            Ok("1"),
            "the historical comparison uses isolated compiler submissions"
        );
    }
    let root = tempfile::tempdir().unwrap();
    let root_path = root.path().to_path_buf();
    let root_guard = if durable {
        eprintln!("durable-workspace retained={}", root.keep().display());
        None
    } else {
        Some(root)
    };
    let effects = TestEffectSurface::minimal(&[]).unwrap();
    let images = Arc::new(ImageRegistry::new());
    let mut lib = SessionLib::open(SessionId(999), &root_path, ModuleEnv::standalone_default())
        .unwrap()
        .with_validation_include(effects.include_paths().to_vec());
    let publication = if durable {
        let manifest = root_path.join("declarations.json");
        lib.attach_recovery_graph_v2(&manifest).unwrap();
        ScalePublication::Durable {
            owner: RecoveryPublicOwner::new(
                &tidepool_repr::ActorPath::parse("root/performance").unwrap(),
                1,
            )
            .unwrap(),
            manifest,
        }
    } else {
        ScalePublication::Ephemeral
    };
    let mut persistent = PersistentSession::new(Some(lib), 1024 * 1024);
    persistent.set_image_registry(images.clone());
    let public = persistent.mint_scope(ScopeId::ROOT).unwrap();
    let mut resident =
        ResidentSession::from_persistent_for_test(frunk::HNil, QuietOutput, persistent);
    let scenario = (prefix, baseline);
    if let ScalePublication::Durable { owner, .. } = &publication {
        measured(
            &mut resident,
            &images,
            scenario,
            "durable_initialization",
            None,
            |resident| {
                assert_eq!(
                    resident
                        .initialize_durable_public_scope(owner.clone(), public)
                        .unwrap(),
                    PublicManifestCommit::Durable
                );
            },
        );
    }
    execute_cell(
        &mut resident,
        public,
        &effects,
        &images,
        scenario,
        "foundation",
        include_str!("fixtures/protected-scale-foundation.hs"),
        1,
        None,
        &publication,
    );
    let original = resident
        .compile_view_in(public)
        .unwrap()
        .exact_declaration_context()
        .unwrap()
        .recovery_products()
        .to_vec();
    if baseline != 0 {
        // GHC tuples permit at most 62 fields; two 50-field items provide
        // 100 real bindings without 100 separately compiled setup items.
        let source = (0..baseline)
            .collect::<Vec<_>>()
            .chunks(50)
            .map(|indices| {
                let names = indices
                    .iter()
                    .map(|index| format!("baseline_{index:04}"))
                    .collect::<Vec<_>>()
                    .join(", ");
                let values = indices
                    .iter()
                    .map(|index| format!("({index} :: Int)"))
                    .collect::<Vec<_>>()
                    .join(", ");
                format!("let ({names}) = ({values})\n")
            })
            .collect::<String>();
        execute_cell(
            &mut resident,
            public,
            &effects,
            &images,
            scenario,
            "baseline",
            &source,
            0,
            None,
            &publication,
        );
    }
    let mut source = String::new();
    for index in 1..=prefix {
        let previous = if index == 1 {
            "0".to_owned()
        } else {
            format!("scale_value_{:04}", index - 1)
        };
        source.push_str(
            &include_str!("fixtures/protected-scale-bind.hs")
                .replace("SCALE_BINDING", &format!("scale_value_{index:04}"))
                .replace("SCALE_PREVIOUS", &previous),
        );
    }
    if baseline == 0 {
        source.push_str(&format!("scale_value_{prefix:04}\n"));
    } else {
        source.push_str(&format!(
            "scale_value_{prefix:04} + baseline_0000 + (baseline_{:04} - {})\n",
            baseline - 1,
            baseline - 1,
        ));
    }
    execute_cell(
        &mut resident,
        public,
        &effects,
        &images,
        scenario,
        "prefix",
        &source,
        0,
        Some(&prefix.to_string()),
        &publication,
    );
    let visible = resident.binding_names_in(public);
    assert_eq!(
        visible
            .iter()
            .filter(|name| name.starts_with("scale_value_"))
            .count(),
        prefix
    );
    assert_eq!(
        visible
            .iter()
            .filter(|name| name.starts_with("baseline_"))
            .count(),
        baseline
    );
    let final_view = resident.compile_view_in(public).unwrap();
    let retained = final_view
        .exact_declaration_context()
        .unwrap()
        .recovery_products();
    for product in &original {
        assert!(
            retained.contains(product),
            "the compiled original product changed during settlement"
        );
    }
    drop(resident);
    drop(root_guard);
}

fn growing_prefix(prefix: usize, baseline: usize) {
    growing_prefix_with_publication(prefix, baseline, false);
}

#[test]
#[ignore = "resident durable scaling attribution; run after the two-cell baseline"]
fn resident_durable_growing_prefix_10_baseline_0() {
    growing_prefix_with_publication(10, 0, true);
}

#[test]
#[ignore = "resident durable scaling attribution; run after the two-cell baseline"]
fn resident_durable_growing_prefix_10_baseline_100() {
    growing_prefix_with_publication(10, 100, true);
}

#[test]
fn protected_growing_prefix_1_baseline_0() {
    growing_prefix(1, 0);
}
#[test]
fn protected_growing_prefix_10_baseline_0() {
    growing_prefix(10, 0);
}
#[test]
fn protected_growing_prefix_100_baseline_0() {
    growing_prefix(100, 0);
}
#[test]
fn protected_growing_prefix_1_baseline_100() {
    growing_prefix(1, 100);
}
#[test]
fn protected_growing_prefix_1_baseline_2() {
    growing_prefix(1, 2);
}
#[test]
fn protected_growing_prefix_10_baseline_100() {
    growing_prefix(10, 100);
}
#[test]
fn protected_growing_prefix_100_baseline_100() {
    growing_prefix(100, 100);
}

/// Compiler/native attribution only; the packaged Engine/Store gate is separate.
fn resident_display_cells(count: usize, durable: bool) {
    tidepool_testing::eval_harness::require_extract();
    assert_ne!(
        std::env::var("TIDEPOOL_EXTRACT_NO_DAEMON").as_deref(),
        Ok("1")
    );
    let socket = PathBuf::from(
        std::env::var_os(tidepool_extract_cmd::DAEMON_SOCKET_ENV)
            .expect("resident measurement requires its owned compiler socket"),
    );
    let identity = tidepool_extract_cmd::preflight_compiler_daemon(&socket)
        .expect("resident measurement cannot use direct fallback");
    let root = tempfile::tempdir().unwrap();
    let root_path = root.path().to_path_buf();
    // Failure evidence must survive a panic in the real compiler/runtime path.
    let root_guard = if durable {
        eprintln!("durable-workspace retained={}", root.keep().display());
        None
    } else {
        Some(root)
    };
    let effects = TestEffectSurface::minimal(&[]).unwrap();
    let images = Arc::new(ImageRegistry::new());
    let mut lib = SessionLib::open(SessionId(1000), &root_path, ModuleEnv::standalone_default())
        .unwrap()
        .with_validation_include(effects.include_paths().to_vec());
    let publication = if durable {
        let manifest = root_path.join("declarations.json");
        lib.attach_recovery_graph_v2(&manifest).unwrap();
        ScalePublication::Durable {
            owner: RecoveryPublicOwner::new(
                &tidepool_repr::ActorPath::parse("root/performance").unwrap(),
                1,
            )
            .unwrap(),
            manifest,
        }
    } else {
        ScalePublication::Ephemeral
    };
    let mut persistent = PersistentSession::new(Some(lib), 1024 * 1024);
    persistent.set_image_registry(images.clone());
    let public = persistent.mint_scope(ScopeId::ROOT).unwrap();
    let mut resident =
        ResidentSession::from_persistent_for_test(frunk::HNil, QuietOutput, persistent);
    if let ScalePublication::Durable { owner, .. } = &publication {
        measured(
            &mut resident,
            &images,
            (0, 0),
            "durable_initialization",
            None,
            |resident| {
                assert_eq!(
                    resident
                        .initialize_durable_public_scope(owner.clone(), public)
                        .unwrap(),
                    PublicManifestCommit::Durable
                );
            },
        );
        execute_cell(
            &mut resident,
            public,
            &effects,
            &images,
            (0, 0),
            "foundation",
            include_str!("fixtures/protected-scale-foundation.hs"),
            1,
            None,
            &publication,
        );
    }
    // Record warm-up separately. Daemon request traces decide whether every
    // measured cell actually reused a warm worker, including a second slot.
    execute_cell(
        &mut resident,
        public,
        &effects,
        &images,
        (0, 0),
        "warmup",
        "(42 :: Int)",
        0,
        Some("42"),
        &publication,
    );
    for index in 0..count {
        let source = format!(
            "({index} + {} :: Int)",
            42_i64 - i64::try_from(index).unwrap()
        );
        assert_eq!(
            tidepool_extract_cmd::preflight_compiler_daemon(&socket).unwrap(),
            identity
        );
        let elapsed = execute_cell(
            &mut resident,
            public,
            &effects,
            &images,
            (0, 0),
            &format!("resident_cell_{index}"),
            &source,
            0,
            Some("42"),
            &publication,
        );
        assert_eq!(
            tidepool_extract_cmd::preflight_compiler_daemon(&socket).unwrap(),
            identity
        );
        eprintln!(
            "resident-performance {}",
            serde_json::json!({
                "schema": 1, "composition": "private-session", "kind": "warm_cell",
                "index": index, "elapsed_ns": elapsed.as_nanos(),
                "completed": true, "displayed": true, "workload": "integer-addition",
                "source_blake3": blake3::hash(source.as_bytes()).to_hex().to_string(),
                "endpoint": identity.to_hex(), "producer": identity.producer_hex(),
            })
        );
    }
    drop(resident);
    drop(root_guard);
}

#[test]
#[ignore = "requires an admitted real resident compiler and 50 compiled display cells"]
fn resident_warm_display_cells_50() {
    resident_display_cells(50, false);
}

#[test]
#[ignore = "requires an admitted real resident compiler for a bounded baseline"]
fn resident_display_cells_2_baseline() {
    resident_display_cells(2, false);
}

#[test]
#[ignore = "requires an admitted real resident compiler and retains its durable workspace"]
fn resident_durable_display_cells_2_baseline() {
    resident_display_cells(2, true);
}

#[test]
fn complete_cell_consumes_item_and_display_without_compiler_requests() {
    tidepool_testing::eval_harness::require_extract();
    let root = tempfile::tempdir().unwrap();
    let effects = TestEffectSurface::minimal(&[]).unwrap();
    let images = Arc::new(ImageRegistry::new());
    let lib = SessionLib::open(
        SessionId(1001),
        root.path(),
        ModuleEnv::standalone_default(),
    )
    .unwrap()
    .with_validation_include(effects.include_paths().to_vec());
    let mut persistent = PersistentSession::new(Some(lib), 1024 * 1024);
    persistent.set_image_registry(images.clone());
    let public = persistent.mint_scope(ScopeId::ROOT).unwrap();
    let mut resident =
        ResidentSession::from_persistent_for_test(frunk::HNil, QuietOutput, persistent);
    execute_cell(
        &mut resident,
        public,
        &effects,
        &images,
        (0, 0),
        "complete_cell",
        include_str!("fixtures/compiled-cell-simple.hs"),
        0,
        Some("42"),
        &ScalePublication::Ephemeral,
    );
}
