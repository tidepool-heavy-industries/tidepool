//! Compiler-owned World backing can end while native custody remains usable.
use super::*;
use std::io::BufRead;
use std::time::{Duration, Instant};

#[derive(Clone, Debug, PartialEq, Eq, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
enum WorldEvent {
    AdmittedActiveOnly,
    HeldEndRetiring,
    BackingRemoved,
    NeedsAcquisition,
    #[serde(other)]
    Other,
}

#[derive(Clone, Debug, serde::Deserialize)]
struct WorldObservation {
    event: WorldEvent,
    world: String,
    worker_instance: String,
    realization: String,
    directory: Option<PathBuf>,
    observed_ns: u64,
}

fn observations(world: &str) -> Vec<WorldObservation> {
    let root = PathBuf::from(std::env::var_os("TIDEPOOL_TEST_ARTIFACT_ROOT").unwrap());
    let log = std::fs::File::open(root.join("compiler/compiler.jsonl")).unwrap();
    let mut log = std::io::BufReader::new(log);
    let mut rows = Vec::new();
    let mut line = String::new();
    while log.read_line(&mut line).unwrap() != 0 {
        // Only an unterminated tail can belong to the daemon's current append.
        if !line.ends_with('\n') {
            break;
        }
        let envelope: serde_json::Value = serde_json::from_str(&line)
            .expect("a complete compiler diagnostic record must be valid JSON");
        if let Some(payload) = envelope
            .get("fields")
            .and_then(|fields| fields.get("line"))
            .and_then(serde_json::Value::as_str)
            .and_then(|payload| payload.strip_prefix("tidepool-world-retention "))
        {
            let row: WorldObservation = serde_json::from_str(payload).unwrap();
            if row.world == world {
                rows.push(row);
            }
        }
        line.clear();
    }
    rows
}

fn disposed_world(world: &str, after: u64) -> WorldObservation {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let rows = observations(world);
        if let Some(admitted) = rows
            .iter()
            .rev()
            .find(|row| row.event == WorldEvent::AdmittedActiveOnly && row.observed_ns > after)
        {
            let same_owner = |row: &&WorldObservation| {
                row.worker_instance == admitted.worker_instance
                    && row.realization == admitted.realization
                    && row.directory == admitted.directory
            };
            let retiring = rows.iter().filter(same_owner).find(|row| {
                row.event == WorldEvent::HeldEndRetiring && row.observed_ns > admitted.observed_ns
            });
            if let Some(retiring) = retiring {
                if let Some(removed) = rows.iter().filter(same_owner).find(|row| {
                    row.event == WorldEvent::BackingRemoved
                        && row.observed_ns > retiring.observed_ns
                }) {
                    let directory = removed.directory.as_ref().unwrap();
                    assert!(
                        matches!(std::fs::metadata(directory), Err(error)
                        if error.kind() == std::io::ErrorKind::NotFound),
                        "the compiler's exact backing directory was actually removed"
                    );
                    return removed.clone();
                }
            }
        }
        assert!(
            Instant::now() < deadline,
            "missing complete World disposal observations: {rows:?}"
        );
        std::thread::sleep(Duration::from_millis(20));
    }
}

#[test]
#[ignore = "requires the rooted runtime_world_disposal_history_test resources"]
fn released_compiler_world_restores_while_retained_native_custody_executes() {
    use tidepool_toolchain::timing::{HostWorkCounter as Counter, HostWorkObservation};
    assert_eq!(
        std::env::var("TIDEPOOL_RETAINED_ORIGINAL_INPUT_BYTES").as_deref(),
        Ok("0")
    );
    assert_eq!(
        std::env::var("TIDEPOOL_TEST_TRACE_PROFILE").as_deref(),
        Ok("full")
    );
    tidepool_testing::eval_harness::require_extract();
    let effects = TestEffectSurface::minimal(&[]).unwrap();
    let recipe = InputRecipe::capture(
        effects.preamble().to_owned(),
        effects.row().to_owned(),
        effects.include_paths().to_vec(),
    );
    let root = tempfile::tempdir().unwrap();
    let library = SessionLib::open(
        SessionId(1800),
        root.path(),
        ModuleEnv::standalone_default(),
    )
    .unwrap()
    .with_validation_include(recipe.include.clone());
    let mut resident = TestSession::unbootstrapped(
        frunk::HNil,
        EmptyOutput,
        crate::DEFAULT_NURSERY_SIZE,
        Some(library),
    );
    resident.bootstrap_world(&recipe);
    let execution = begin_fixture_execution(&mut resident);
    resident
        .set_run_context(SessionRunContext {
            lexical_scope: execution.private_scope(),
            ..resident.public_context()
        })
        .unwrap();
    let checked = check_fixture_cell(
        &mut resident,
        &recipe,
        "heldNative <- pure ((41 :: Int), T.pack (P.show (41 :: Int)))",
        execution.clone(),
        0,
    );
    let world = checked.prefix.snapshot().world().unwrap().clone();
    let logical = *world.logical_id();
    let world_name = logical
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    let (bound, compiled, reservation) = checked.compile_binding(&mut resident, 0);
    let held_program = compiled.prepared().clone();
    let held_target = compiled
        .certification
        .as_ref()
        .unwrap()
        .linked_target
        .clone()
        .unwrap();
    let disposed = disposed_world(&world_name, 0);
    assert!(matches!(
        world.last_admission_residency().unwrap(),
        Some(tidepool_extract_cmd::WorldResidency::ActiveOnly(_))
    ));

    let requests = tidepool_extract_cmd::extract_spawn_count();
    let result = source_action(&mut resident, |resident, settlement| {
        resident.run_bind_with_sites(
            "heldNative",
            compiled.code(),
            &bound[0],
            reservation.generation(),
            settlement,
        )
    })
    .expect("retained native execution survives actual compiler World release");
    let ResidentOutcome::Completed { result, .. } = result else {
        panic!("retained pure native execution must complete");
    };
    assert_eq!(result.to_json(), serde_json::json!([41, "41"]));
    assert_eq!(
        requests,
        tidepool_extract_cmd::extract_spawn_count(),
        "execution cannot compile a replacement for retained native custody"
    );

    let configured =
        tidepool_toolchain::toolchain::CompilerDeploymentConfiguration::from_env().unwrap();
    let (endpoint, _, _) =
        tidepool_toolchain::toolchain::bind_admitted_extract_endpoint(&configured).unwrap();
    let endpoint =
        tidepool_toolchain::toolchain::AdmittedCompilerEndpoint::from_bound(endpoint).unwrap();
    let work = HostWorkObservation::new();
    work.scope(|| {
        let before = work.snapshot();
        source_action(&mut resident, |_, _| {
            let answer = tidepool_toolchain::compiler_world::query_public_module(
                &endpoint,
                &world,
                tidepool_toolchain::compiler_world::PublicModuleQuestion::structured_type(
                    "Data.List".into(),
                    tidepool_extract_cmd::InspectionNamespace::Value,
                    "map".into(),
                ),
            )?;
            assert!(Arc::ptr_eq(answer.world(), &world));
            assert!(matches!(
                answer.answer(),
                tidepool_toolchain::compiler_world::WorldAnswer::Type { .. }
            ));
            Ok::<_, tidepool_toolchain::CompileError>(())
        })
        .expect("the same logical World restores through its production acquisition owner");
        let after = work.snapshot();
        assert!(
            after.get(Counter::WorldEnsureNeedsAcquisition)
                > before.get(Counter::WorldEnsureNeedsAcquisition)
        );
        assert!(after.get(Counter::WorldEnsureComplete) > before.get(Counter::WorldEnsureComplete));
    });
    let restored = disposed_world(&world_name, disposed.observed_ns);
    assert_eq!(restored.worker_instance, disposed.worker_instance);
    assert_ne!(restored.realization, disposed.realization);
    assert!(observations(&world_name).iter().any(|row| {
        row.event == WorldEvent::NeedsAcquisition
            && row.worker_instance == disposed.worker_instance
            && row.realization == disposed.realization
            && row.observed_ns > disposed.observed_ns
            && row.observed_ns < restored.observed_ns
    }));
    assert_eq!(*world.logical_id(), logical);
    assert!(Arc::ptr_eq(compiled.prepared(), &held_program));
    assert!(Arc::ptr_eq(
        compiled
            .certification
            .as_ref()
            .unwrap()
            .linked_target
            .as_ref()
            .unwrap(),
        &held_target
    ));
    publish_checked_fixture(&mut resident, &execution, 1);
    original_value_probe(&mut resident, &recipe, "P.fst heldNative", 41);
}
