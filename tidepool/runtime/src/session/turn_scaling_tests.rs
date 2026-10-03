//! Complete compiled-cell measurements. Each row reports owning counters; compiler
//! interface I/O remains in the independent `TIDEPOOL_TIMING=1` worker trace.

use super::*;
use crate::session::{
    resident_cell_check_template, resident_workbench_templates, CertifiedDeclarationPublication,
    ExecutionPublication, ModuleEnv, OutputSink, PersistentSession, PreparedRuntimeError,
    PublicManifestCommit, PublicationDecision, RecoveryPublicOwner, RecoveryRunAuthority,
    ResidentError, ResidentSession, SessionLib, SessionRunContext, SourceImports,
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

/// Hold the reference workspace's real exclusive file lock for the session.
struct ScaleRunOwner {
    root: PathBuf,
    _lock: std::fs::File,
}
impl RecoveryRunAuthority for ScaleRunOwner {
    fn owns_run(&self, root: &Path) -> std::io::Result<bool> {
        Ok(root.canonicalize()? == self.root)
    }
}
fn scale_run_owner(root: &Path) -> Arc<ScaleRunOwner> {
    let lock = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(root.join("performance-run-owner.lock"))
        .unwrap();
    lock.try_lock().unwrap();
    Arc::new(ScaleRunOwner {
        root: root.canonicalize().unwrap(),
        _lock: lock,
    })
}

fn scale_workspace(durable: bool) -> (PathBuf, Option<tempfile::TempDir>) {
    let root = if durable {
        let parent = PathBuf::from(
            std::env::var_os("TIDEPOOL_PERFORMANCE_WORKSPACE_ROOT")
                .expect("durable evidence requires an explicit retained workspace parent"),
        );
        assert!(parent.is_absolute());
        tempfile::Builder::new()
            .prefix("durable-workspace-")
            .tempdir_in(parent)
            .unwrap()
    } else {
        tempfile::tempdir().unwrap()
    };
    let path = root.path().to_path_buf();
    if durable {
        eprintln!("durable-workspace retained={}", root.keep().display());
        (path, None)
    } else {
        (path, Some(root))
    }
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
    execute_cell_with_authority_checks(
        resident,
        public,
        effects,
        images,
        scenario,
        label,
        source,
        declarations,
        expected_display,
        publication_target,
        AuthorityChecks::Configured,
    )
}

#[derive(Clone, Copy)]
enum AuthorityChecks {
    Configured,
    RefusalBranches,
}

fn execute_cell_with_authority_checks(
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
    authority_checks: AuthorityChecks,
) -> Duration {
    try_execute_cell_with_authority_checks(
        resident,
        public,
        effects,
        images,
        scenario,
        label,
        source,
        declarations,
        expected_display,
        publication_target,
        authority_checks,
    )
    .unwrap()
}

fn try_execute_cell_with_authority_checks(
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
    authority_checks: AuthorityChecks,
) -> Result<Duration, ResidentError> {
    try_execute_cell_with_template_imports(
        resident,
        public,
        effects,
        images,
        scenario,
        label,
        source,
        declarations,
        expected_display,
        publication_target,
        authority_checks,
        &SourceImports::new(),
    )
}

fn try_execute_cell_with_template_imports(
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
    authority_checks: AuthorityChecks,
    template_imports: &SourceImports,
) -> Result<Duration, ResidentError> {
    let cell_started = Instant::now();
    let mut expected_public_winners: std::collections::BTreeMap<_, _> = resident
        .public_visibility_snapshot_in(public)
        .unwrap()
        .bindings
        .into_iter()
        .collect();
    let execution = Arc::new(resident.begin_private_execution(public).unwrap());
    let view = execution.view();
    let imports = view.turn_imports(template_imports);
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
    let compile_cell = || {
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
    };
    if matches!(authority_checks, AuthorityChecks::RefusalBranches) {
        struct RestoreDeployment(std::ffi::OsString);
        impl Drop for RestoreDeployment {
            fn drop(&mut self) {
                std::env::set_var("TIDEPOOL_COMPILER_DEPLOYMENT", &self.0);
            }
        }
        let configured = std::env::var_os("TIDEPOOL_COMPILER_DEPLOYMENT")
            .expect("vertical test requires configured deployment");
        let guard = RestoreDeployment(configured.clone());
        let before = tidepool_extract_cmd::extract_spawn_count();
        std::env::remove_var("TIDEPOOL_COMPILER_DEPLOYMENT");
        assert!(
            compile_cell().is_err(),
            "unconfigured planned compile must refuse admission"
        );
        assert_eq!(tidepool_extract_cmd::extract_spawn_count(), before);
        let mut mismatch: serde_json::Value =
            serde_json::from_slice(&std::fs::read(configured).unwrap()).unwrap();
        let producer = mismatch["producer_identity"].as_array_mut().unwrap();
        producer[0] = serde_json::json!(producer[0].as_u64().unwrap() ^ 1);
        let wrong = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(wrong.path(), serde_json::to_vec(&mismatch).unwrap()).unwrap();
        std::env::set_var("TIDEPOOL_COMPILER_DEPLOYMENT", wrong.path());
        assert!(
            compile_cell().is_err(),
            "wrong producer planned compile must refuse admission"
        );
        assert_eq!(tidepool_extract_cmd::extract_spawn_count(), before);
        drop(guard);
    }
    let (checked, program) = measured(
        resident,
        images,
        scenario,
        &format!("{label}.compile_cell"),
        None,
        |_| compile_cell().unwrap(),
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
                let observed = measured(
                    resident,
                    images,
                    scenario,
                    &format!("{label}.native_observe"),
                    Some(index),
                    |resident| {
                        resident.run_observation_with_sites(
                            compiled.code(),
                            &bound[0],
                            reservation.generation(),
                            false,
                        )
                    },
                );
                assert_eq!(
                    tidepool_extract_cmd::extract_spawn_count(),
                    submissions_before_effects
                );
                observed?;
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
        |resident| {
            resident
                .freeze_private_execution(
                    &execution,
                    crate::session::ExecutionPublicationIntent::CompletedCell,
                )
                .unwrap()
        },
    );
    for id in intent.native_write_ids() {
        let (name, _) = private_winners
            .iter()
            .find(|(_, winner)| winner == id)
            .expect("sealed native write must be a final private winner");
        expected_public_winners.insert(name.clone(), *id);
    }
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
    let recovery_work = ticket.recovery_work();
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
        expected_public_winners.into_iter().collect::<Vec<_>>(),
        "publication must preserve prior public winners and publish sealed native writes"
    );
    // Retained snapshots and diagnostic reads do not belong to cell latency.
    let elapsed = cell_started.elapsed();
    let inventory = resident.compile_view_in(public).and_then(|view| {
        view.exact_declaration_context()
            .map(|context| context.artifact_view().inventory().metrics())
    });
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
                "checksum_encode_bytes": recovery_work.checksum_encode_bytes,
                "recovery_validation_hash_bytes": recovery_work.recovery_validation_hash_bytes,
                "recovery_materialization_hash_bytes": recovery_work.recovery_materialization_hash_bytes,
                "manifest_write_bytes": recovery_work.manifest_write_bytes,
                "artifact_inventory": inventory,
                "inventory_counter_scope": "shared-artifact-inventory-owner",
            })
        );
    }
    eprintln!(
        "protected-scale {}",
        serde_json::json!({
            "schema": 1, "prefix": scenario.0, "baseline": scenario.1,
            "phase": format!("{label}.artifact_inventory"), "inventory": inventory,
        })
    );
    Ok(elapsed)
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
    let (root_path, root_guard) = scale_workspace(durable);
    let effects = TestEffectSurface::minimal(&[]).unwrap();
    let images = Arc::new(ImageRegistry::new());
    let mut lib = SessionLib::open(SessionId(999), &root_path, ModuleEnv::standalone_default())
        .unwrap()
        .with_validation_include(effects.include_paths().to_vec());
    let publication = if durable {
        let manifest = root_path.join("declarations.json");
        lib.attach_owned_recovery_graph_v3(&manifest, scale_run_owner(&root_path))
            .unwrap();
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
fn resident_durable_growing_prefix_1_baseline_0() {
    growing_prefix_with_publication(1, 0, true);
}

#[test]
#[ignore = "resident durable scaling attribution; run after the two-cell baseline"]
fn resident_durable_growing_prefix_100_baseline_0() {
    growing_prefix_with_publication(100, 0, true);
}

#[test]
#[ignore = "resident durable scaling attribution; run after the two-cell baseline"]
fn resident_durable_growing_prefix_1_baseline_100() {
    growing_prefix_with_publication(1, 100, true);
}

#[test]
#[ignore = "resident durable scaling attribution; run after the two-cell baseline"]
fn resident_durable_growing_prefix_100_baseline_100() {
    growing_prefix_with_publication(100, 100, true);
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
    let (root_path, root_guard) = scale_workspace(durable);
    let effects = TestEffectSurface::minimal(&[]).unwrap();
    let images = Arc::new(ImageRegistry::new());
    let mut lib = SessionLib::open(SessionId(1000), &root_path, ModuleEnv::standalone_default())
        .unwrap()
        .with_validation_include(effects.include_paths().to_vec());
    let publication = if durable {
        let manifest = root_path.join("declarations.json");
        lib.attach_owned_recovery_graph_v3(&manifest, scale_run_owner(&root_path))
            .unwrap();
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

fn simple_cell_vertical(
    label: &str,
    source: &str,
    declarations: usize,
    expected: &str,
    authority_checks: AuthorityChecks,
) -> Vec<String> {
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
    execute_cell_with_authority_checks(
        &mut resident,
        public,
        &effects,
        &images,
        (0, 0),
        label,
        source,
        declarations,
        Some(expected),
        &ScalePublication::Ephemeral,
        authority_checks,
    );
    resident.binding_names_in(public)
}

#[test]
fn complete_cell_consumes_item_and_display_without_compiler_requests() {
    simple_cell_vertical(
        "complete_cell",
        include_str!("fixtures/compiled-cell-simple.hs"),
        0,
        "42",
        AuthorityChecks::RefusalBranches,
    );
}

#[test]
fn following_declaration_retains_original_native_binding_inventory() {
    tidepool_testing::eval_harness::require_extract();
    let root = tempfile::tempdir().unwrap();
    let effects = TestEffectSurface::minimal(&[]).unwrap();
    let images = Arc::new(ImageRegistry::new());
    let lib = SessionLib::open(
        SessionId(1004),
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
        "original_binding",
        "x <- pure (1 :: Int)",
        0,
        None,
        &ScalePublication::Ephemeral,
    );
    let original = resident.current_binding_in(public, "x").unwrap();
    assert_eq!(
        original.1,
        tidepool_repr::SessionModule::val(tidepool_repr::Generation(1))
    );
    execute_cell(
        &mut resident,
        public,
        &effects,
        &images,
        (0, 0),
        "following_declaration",
        include_str!("fixtures/compiled-cell-native-binding-declaration.hs"),
        1,
        Some("1"),
        &ScalePublication::Ephemeral,
    );
    assert_eq!(resident.current_binding_in(public, "x").unwrap(), original);
    std::fs::write(
        root.path().join("HiddenValSupport.hs"),
        include_str!("fixtures/checked-native-support.hs"),
    )
    .unwrap();
    execute_cell(
        &mut resident,
        public,
        &effects,
        &images,
        (0, 0),
        "following_support_declaration",
        include_str!("fixtures/compiled-cell-native-binding-support.hs"),
        2,
        Some("1"),
        &ScalePublication::Ephemeral,
    );
    assert_eq!(resident.current_binding_in(public, "x").unwrap(), original);
    let public_view = resident.compile_view_in(public).unwrap();
    let context = public_view.exact_declaration_context().unwrap();
    assert!(context
        .artifact_view()
        .descriptors()
        .iter()
        .any(|descriptor| descriptor.owner.module == original.1.module_name()));
    assert!(!context.lexical_graph().iter().any(|node| node.owner.module
        == original.1.module_name()
        || node
            .imports
            .iter()
            .any(|owner| owner.module == original.1.module_name())));
    execute_cell(
        &mut resident,
        public,
        &effects,
        &images,
        (0, 0),
        "following_support_consumer",
        "dependentThroughSupport",
        0,
        Some("1"),
        &ScalePublication::Ephemeral,
    );
    assert_eq!(resident.current_binding_in(public, "x").unwrap(), original);
    execute_cell(
        &mut resident,
        public,
        &effects,
        &images,
        (0, 0),
        "support_rebind_x",
        "let x = (2 :: Int)",
        0,
        None,
        &ScalePublication::Ephemeral,
    );
    assert_ne!(resident.current_binding_in(public, "x").unwrap(), original);
    execute_cell(
        &mut resident,
        public,
        &effects,
        &images,
        (0, 0),
        "support_original_after_rebind",
        "dependentThroughSupport",
        0,
        Some("1"),
        &ScalePublication::Ephemeral,
    );
}

#[test]
fn following_declaration_publishes_current_source_selected_originals() {
    let subscriber = tracing_subscriber::fmt()
        .with_test_writer()
        .with_env_filter(tracing_subscriber::EnvFilter::new(
            "tidepool_toolchain::planned_source_admission=debug",
        ))
        .finish();
    let _subscriber = tracing::subscriber::set_default(subscriber);
    tidepool_testing::eval_harness::require_extract();
    let root = tempfile::tempdir().unwrap();
    std::fs::write(
        root.path().join("CheckedHomeValue.hs"),
        include_str!("fixtures/checked-home-value.hs"),
    )
    .unwrap();
    std::fs::write(
        root.path().join("UnrelatedHomeValue.hs"),
        include_str!("fixtures/unrelated-home-value.hs"),
    )
    .unwrap();
    let effects = TestEffectSurface::minimal(&[]).unwrap();
    let images = Arc::new(ImageRegistry::new());
    let lib = SessionLib::open(
        SessionId(1006),
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
    try_execute_cell_with_template_imports(
        &mut resident,
        public,
        &effects,
        &images,
        (0, 0),
        "retained_hidden_home",
        "let home = CheckedHomeValue.homeValue",
        0,
        None,
        &ScalePublication::Ephemeral,
        AuthorityChecks::Configured,
        &SourceImports::from_specs(["qualified CheckedHomeValue"]),
    )
    .unwrap();
    try_execute_cell_with_template_imports(
        &mut resident,
        public,
        &effects,
        &images,
        (0, 0),
        "retained_unrelated_home",
        "let unrelated = UnrelatedHomeValue.unrelatedValue",
        0,
        None,
        &ScalePublication::Ephemeral,
        AuthorityChecks::Configured,
        &SourceImports::from_specs(["qualified UnrelatedHomeValue"]),
    )
    .unwrap();
    assert!(resident
        .compile_view_in(public)
        .unwrap()
        .exact_declaration_context()
        .is_none_or(|context| !context
            .lexical_graph()
            .iter()
            .any(|node| node.owner.module == "CheckedHomeValue")));
    execute_cell(
        &mut resident,
        public,
        &effects,
        &images,
        (0, 0),
        "source_selected_declaration",
        include_str!("fixtures/compiled-cell-source-selected-declaration.hs"),
        2,
        Some("41"),
        &ScalePublication::Ephemeral,
    );
    let view = resident.compile_view_in(public).unwrap();
    let context = view.exact_declaration_context().unwrap();
    assert!(context
        .lexical_graph()
        .iter()
        .any(|node| node.owner.module == "CheckedHomeValue"));
    assert!(!context
        .lexical_graph()
        .iter()
        .any(|node| node.owner.module == "UnrelatedHomeValue"));
    execute_cell(
        &mut resident,
        public,
        &effects,
        &images,
        (0, 0),
        "source_selected_consumer",
        "sourceSelected home",
        0,
        Some("41"),
        &ScalePublication::Ephemeral,
    );
    assert!(!resident
        .compile_view_in(public)
        .unwrap()
        .exact_declaration_context()
        .unwrap()
        .lexical_graph()
        .iter()
        .any(|node| node.owner.module == "UnrelatedHomeValue"));
}

#[test]
fn following_cells_reprove_template_imports_of_retained_rich_originals() {
    tidepool_testing::eval_harness::require_extract();
    let root = tempfile::tempdir().unwrap();
    let effects = TestEffectSurface::minimal(&[]).unwrap();
    let images = Arc::new(ImageRegistry::new());
    let lib = SessionLib::open(
        SessionId(1007),
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
    let imports = SourceImports::from_specs(["qualified Tidepool.Aeson as Aeson"]);
    for (label, source, expected) in [
        (
            "original_rich_value",
            "let originalJSON = Aeson.object []",
            None,
        ),
        (
            "following_rich_value",
            "let nextJSON = Aeson.object []\n(42 :: Int)",
            Some("42"),
        ),
        ("following_rich_consumer", "(42 :: Int)", Some("42")),
    ] {
        try_execute_cell_with_template_imports(
            &mut resident,
            public,
            &effects,
            &images,
            (0, 0),
            label,
            source,
            0,
            expected,
            &ScalePublication::Ephemeral,
            AuthorityChecks::Configured,
            &imports,
        )
        .unwrap();
        assert!(resident
            .compile_view_in(public)
            .unwrap()
            .exact_declaration_context()
            .is_none_or(|context| !context
                .lexical_graph()
                .iter()
                .any(|node| node.owner.module == "Tidepool.Aeson")));
    }
    try_execute_cell_with_template_imports(
        &mut resident,
        public,
        &effects,
        &images,
        (0, 0),
        "following_without_rich_import",
        "let independent = 42",
        0,
        None,
        &ScalePublication::Ephemeral,
        AuthorityChecks::Configured,
        &SourceImports::default(),
    )
    .unwrap();
    assert!(resident
        .compile_view_in(public)
        .unwrap()
        .exact_declaration_context()
        .is_none_or(|context| !context
            .lexical_graph()
            .iter()
            .any(|node| node.owner.module == "Tidepool.Aeson")));
}

#[test]
fn late_record_selector_replaces_earlier_cell_value() {
    let names = simple_cell_vertical(
        "record_selector",
        include_str!("fixtures/compiled-cell-record-selector.hs"),
        1,
        "2",
        AuthorityChecks::Configured,
    );
    assert!(
        !names.iter().any(|name| name == "f"),
        "old heap f must not survive its exact declaration selector"
    );
}

#[test]
#[ignore = "local fixity guard remains until production prepared-cell native8 passes"]
fn complete_cell_preserves_exact_local_fixity() {
    simple_cell_vertical(
        "local_fixity",
        include_str!("fixtures/compiled-cell-local-fixity.hs"),
        0,
        "8",
        AuthorityChecks::Configured,
    );
}

fn persisted_root_state(
    manifest: &Path,
    owner: &RecoveryPublicOwner,
) -> crate::session::recovery::RecoveryNodeState {
    let graph: crate::session::recovery::RecoveryGraph =
        serde_json::from_slice(&std::fs::read(manifest).unwrap()).unwrap();
    let root = graph
        .public_surfaces()
        .find(|surface| &surface.owner == owner)
        .unwrap()
        .declaration_root
        .unwrap();
    graph.node(root).unwrap().state.clone()
}

#[test]
#[ignore = "requires an admitted real compiler, retained durable workspace, and fresh native test process"]
fn durable_mixed_originals_recover_independent_native_entry() {
    tidepool_testing::eval_harness::require_extract();
    let child_root = std::env::var_os("TIDEPOOL_RECOVERY_CHILD_WORKSPACE").map(PathBuf::from);
    let (root_path, root_guard) = if let Some(root) = &child_root {
        (root.clone(), None)
    } else {
        scale_workspace(true)
    };
    let manifest = root_path.join("declarations.json");
    let owner = RecoveryPublicOwner::new(
        &tidepool_repr::ActorPath::parse("root/performance").unwrap(),
        1,
    )
    .unwrap();
    let effects = TestEffectSurface::minimal(&[]).unwrap();
    let images = Arc::new(ImageRegistry::new());
    let source_root = if child_root.is_some() {
        let source = root_path.join("fresh-process-source");
        std::fs::create_dir(&source).unwrap();
        source
    } else {
        root_path.clone()
    };
    let mut lib = SessionLib::open(
        SessionId(if child_root.is_some() { 1003 } else { 1002 }),
        &source_root,
        ModuleEnv::standalone_default(),
    )
    .unwrap()
    .with_validation_include(effects.include_paths().to_vec());
    lib.attach_owned_recovery_graph_v3(&manifest, scale_run_owner(&root_path))
        .unwrap();
    let mut persistent = PersistentSession::new(Some(lib), 1024 * 1024);
    persistent.set_image_registry(images.clone());
    let public = if child_root.is_some() {
        persistent.recover_public_scope(&owner).unwrap()
    } else {
        persistent.mint_scope(ScopeId::ROOT).unwrap()
    };
    let mut resident =
        ResidentSession::from_persistent_for_test(frunk::HNil, QuietOutput, persistent);
    let publication = ScalePublication::Durable {
        owner: owner.clone(),
        manifest: manifest.clone(),
    };
    if child_root.is_some() {
        assert!(!resident
            .binding_names_in(public)
            .iter()
            .any(|name| name == "x"));
        eprintln!("durable-recovery fresh_process_pid={}", std::process::id());
        // One original owns both entries. Hydration must retain its complete
        // typed interface while native demand enforces the exact old x lease.
        execute_cell(
            &mut resident,
            public,
            &effects,
            &images,
            (0, 0),
            "recovered_independent",
            "independent",
            0,
            Some("42"),
            &publication,
        );
        let missing_before = demand_missing_retained(
            &mut resident,
            public,
            &effects,
            &images,
            "lost_dependent_before_rebind",
            &publication,
        );
        execute_cell(
            &mut resident,
            public,
            &effects,
            &images,
            (0, 0),
            "rebind_x",
            "let x = (2 :: Int)",
            0,
            None,
            &publication,
        );
        assert!(resident
            .binding_names_in(public)
            .iter()
            .any(|name| name == "x"));
        let missing_after = demand_missing_retained(
            &mut resident,
            public,
            &effects,
            &images,
            "lost_dependent_after_rebind",
            &publication,
        );
        assert_eq!(
            missing_after, missing_before,
            "same-spelled x must not satisfy the original's exact old Val import"
        );
        return;
    }
    assert_eq!(
        resident
            .initialize_durable_public_scope(owner.clone(), public)
            .unwrap(),
        PublicManifestCommit::Durable
    );
    execute_cell(
        &mut resident,
        public,
        &effects,
        &images,
        (0, 0),
        "live_x",
        "x <- pure (1 :: Int)",
        0,
        None,
        &publication,
    );
    execute_cell(
        &mut resident,
        public,
        &effects,
        &images,
        (0, 0),
        "independent_original",
        include_str!("fixtures/compiled-cell-independent-original.hs"),
        1,
        None,
        &publication,
    );
    assert!(matches!(
        persisted_root_state(&manifest, &owner),
        crate::session::recovery::RecoveryNodeState::ExactArtifactClosure
    ));
    execute_cell(
        &mut resident,
        public,
        &effects,
        &images,
        (0, 0),
        "mixed_original",
        include_str!("fixtures/compiled-cell-mixed-original.hs"),
        1,
        None,
        &publication,
    );
    assert!(matches!(
        persisted_root_state(&manifest, &owner),
        crate::session::recovery::RecoveryNodeState::LiveValueDependency { .. }
    ));
    drop(resident);
    // Execute the already-built native test binary in a fresh process. The
    // battery still owns the one compiler daemon and configured endpoint.
    let output = std::process::Command::new(std::env::current_exe().unwrap())
        .args(["session::turn::scaling_tests::durable_mixed_originals_recover_independent_native_entry", "--ignored", "--exact", "--nocapture"])
        .env("TIDEPOOL_RECOVERY_CHILD_WORKSPACE", &root_path).output().unwrap();
    eprintln!("{}", String::from_utf8_lossy(&output.stderr));
    println!("{}", String::from_utf8_lossy(&output.stdout));
    assert!(
        output.status.success(),
        "fresh process must execute the mixed original independent entry and refuse its exact lost dependency"
    );
    assert!(
        String::from_utf8_lossy(&output.stdout).contains("1 passed;"),
        "fresh process must select exactly its one test"
    );
    drop(root_guard);
}

/// Any compile, authority, source, or unrelated runtime failure fails this test.
/// Only the native resolver's exact missing retained owner counts as refusal.
fn demand_missing_retained(
    resident: &mut ScaleSession,
    public: ScopeId,
    effects: &TestEffectSurface,
    images: &ImageRegistry,
    label: &str,
    publication: &ScalePublication,
) -> (tidepool_repr::execution_schema::SymbolIdentity, u64) {
    let error = try_execute_cell_with_authority_checks(
        resident,
        public,
        effects,
        images,
        (0, 0),
        label,
        "dependent",
        0,
        Some("1"),
        publication,
        AuthorityChecks::Configured,
    )
    .expect_err("demanding the old retained x must refuse native installation");
    let ResidentError::Prepared(PreparedRuntimeError::MissingRetainedCertifiedOwner {
        identity,
        generation,
    }) = error
    else {
        panic!("expected exact retained native refusal, got {error:?}");
    };
    eprintln!("durable-recovery exact_missing_retained={identity:?} generation={generation}");
    (identity, generation)
}
