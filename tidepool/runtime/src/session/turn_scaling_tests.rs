//! Real checked-prefix measurements. Each row reports owning counters; compiler
//! interface I/O remains in the independent `TIDEPOOL_TIMING=1` worker trace.

use super::*;
use crate::session::{
    resident_cell_check_template, resident_workbench_templates, CertifiedDeclarationPublication,
    ExecutionPublication, ModuleEnv, OutputSink, PersistentSession, PublicManifestCommit,
    PublicationDecision, ResidentSession, SessionLib, SessionRunContext, SourceImports,
};
use std::time::Instant;
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

fn measured<T>(
    resident: &mut ScaleSession,
    images: &ImageRegistry,
    scenario: (usize, usize),
    phase: &str,
    item: Option<usize>,
    action: impl FnOnce(&mut ScaleSession) -> T,
) -> T {
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
        Ok(value) => value,
        Err(panic) => std::panic::resume_unwind(panic),
    }
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
) {
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
    let admission = resident
        .admit_cell_for_execution(
            execution.clone(),
            declarations,
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
    let (checked, fold) = measured(
        resident,
        images,
        scenario,
        &format!("{label}.check"),
        None,
        |_| {
            check_cell_admitted(
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
                None,
            )
            .unwrap()
        },
    );
    assert!(
        fold.is_none(),
        "this fixture measures separate check and native compile"
    );
    assert!(!checked.items.is_empty());
    let prefix = resident
        .begin_checked_prefix(admission, checked.checked_item(0).unwrap())
        .unwrap();
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
            let snapshot = reservation.snapshot();
            let view = snapshot.view();
            let include = view.include_paths(effects.include_paths());
            let include = include.iter().map(PathBuf::as_path).collect::<Vec<_>>();
            let injected = snapshot.compiler_prefix().injected_modules();
            let TurnResult::Bind {
                bound, compiled, ..
            } = measured(
                resident,
                images,
                scenario,
                &format!("{label}.compile"),
                Some(index),
                |_| {
                    run_checked_item(
                        TurnRequest {
                            exact_context: view.exact_declaration_context().cloned(),
                            session_id: Some(view.session()),
                            turn_text: item.source(),
                            templates: &templates,
                            include: &include,
                            session_root: view.session_root(),
                            inject_modules: &injected,
                            gen: reservation.generation().0,
                            verdict: Some(checked.items[index].verdict.clone()),
                            target: None,
                            retained_imports: &[],
                        },
                        reservation.clone(),
                    )
                    .unwrap()
                },
            )
            else {
                panic!("checked native item did not return its binding recipe")
            };
            if item.kind() == CheckedItemKind::Bind {
                assert_eq!(bound.len(), 1);
                measured(
                    resident,
                    images,
                    scenario,
                    &format!("{label}.native_bind"),
                    Some(index),
                    |resident| {
                        resident
                            .run_bind_with_sites(
                                &bound[0].name,
                                compiled.code(),
                                &bound[0],
                                reservation.generation(),
                            )
                            .unwrap();
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
                    &format!("{label}.display_compile"),
                    Some(index),
                    |_| run_checked_display(display.clone(), &include).unwrap(),
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
    let publication = resident
        .restage_ephemeral_execution_publication(intent)
        .unwrap();
    let ticket = measured(
        resident,
        images,
        scenario,
        &format!("{label}.certify_stage"),
        None,
        |_| match publication {
            ExecutionPublication::Bindings(base) => base.stage().unwrap(),
            ExecutionPublication::Declarations(base) => {
                let CertifiedDeclarationPublication::Accepted(accepted) = base.certify().unwrap()
                else {
                    panic!("actual original declaration/prefix publication was rejected")
                };
                accepted.stage().unwrap()
            }
        },
    );
    measured(
        resident,
        images,
        scenario,
        &format!("{label}.publish"),
        None,
        |resident| {
            assert_eq!(
                resident
                    .publish_staged_public_manifest(ticket, &PublicationDecision::new())
                    .unwrap(),
                PublicManifestCommit::Ephemeral
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
}

fn growing_prefix(prefix: usize, baseline: usize) {
    tidepool_testing::eval_harness::require_extract();
    assert_eq!(
        std::env::var("TIDEPOOL_EXTRACT_NO_DAEMON").as_deref(),
        Ok("1"),
        "the measurement must use isolated compiler submissions"
    );
    let root = tempfile::tempdir().unwrap();
    let effects = TestEffectSurface::minimal(&[]).unwrap();
    let images = Arc::new(ImageRegistry::new());
    let lib = SessionLib::open(SessionId(999), root.path(), ModuleEnv::standalone_default())
        .unwrap()
        .with_validation_include(effects.include_paths().to_vec());
    let mut persistent = PersistentSession::new(Some(lib), 1024 * 1024);
    persistent.set_image_registry(images.clone());
    let public = persistent.mint_scope(ScopeId::ROOT).unwrap();
    let mut resident =
        ResidentSession::from_persistent_for_test(frunk::HNil, QuietOutput, persistent);
    let scenario = (prefix, baseline);
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
    );
    let original = resident
        .compile_view_in(public)
        .unwrap()
        .exact_declaration_context()
        .unwrap()
        .recovery_products()
        .to_vec();
    if baseline != 0 {
        let source = (0..baseline)
            .map(|index| format!("let baseline_{index:04} = ({index} :: Int)\n"))
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
    source.push_str(&format!("scale_value_{prefix:04}\n"));
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
fn protected_growing_prefix_10_baseline_100() {
    growing_prefix(10, 100);
}
#[test]
fn protected_growing_prefix_100_baseline_100() {
    growing_prefix(100, 100);
}
