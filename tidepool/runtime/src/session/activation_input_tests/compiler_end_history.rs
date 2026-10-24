//! A real compiled response is insufficient when its owning END is unobserved.
use super::*;
use rustix::fd::OwnedFd;
use rustix::process::{pidfd_open, pidfd_send_signal, Pid, PidfdFlags, Signal};
use std::cell::{Cell, RefCell};
use std::time::{Duration, Instant};

struct PausedOwnedDaemon {
    process: OwnedFd,
    pid: u32,
    resumed: bool,
}

impl PausedOwnedDaemon {
    fn pause(artifact_root: &Path) -> Self {
        let report_path = artifact_root.join("compiler/owned-service.json");
        let report_bytes = std::fs::read(&report_path).unwrap();
        let report: serde_json::Value = serde_json::from_slice(&report_bytes).unwrap();
        assert_eq!(report["schema"], 2);
        assert_eq!(report["kind"], "owned-compiler-service");
        assert_eq!(
            report["owner_directory"],
            artifact_root.join("compiler").to_str().unwrap()
        );
        assert_eq!(report["complete"], false);
        assert_eq!(report["action"]["status"], "running");
        let socket = std::env::var(tidepool_extract_cmd::DAEMON_SOCKET_ENV)
            .expect("this case's runner selected its owned daemon socket");
        assert_eq!(report["socket_path"], socket);
        let (endpoint, _) = tidepool_toolchain::toolchain::bind_extract_endpoint().unwrap();
        assert_eq!(report["endpoint"], endpoint.identity().to_hex());
        assert_eq!(report["producer"], endpoint.identity().producer_hex());
        let pid = u32::try_from(report["daemon_pid"].as_u64().unwrap()).unwrap();
        assert_ne!(pid, std::process::id());
        let process = pidfd_open(
            Pid::from_raw(i32::try_from(pid).unwrap()).unwrap(),
            PidfdFlags::empty(),
        )
        .unwrap();
        assert_eq!(std::fs::read(&report_path).unwrap(), report_bytes);
        let guard = Self {
            process,
            pid,
            resumed: false,
        };
        pidfd_send_signal(&guard.process, Signal::STOP).unwrap();
        let deadline = Instant::now() + Duration::from_secs(2);
        loop {
            if guard.stopped() {
                return guard;
            }
            assert!(Instant::now() < deadline, "owned daemon did not enter STOP");
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    fn stopped(&self) -> bool {
        std::fs::read_to_string(format!("/proc/{}/status", self.pid))
            .unwrap()
            .lines()
            .any(|line| line.starts_with("State:") && line.split_whitespace().nth(1) == Some("T"))
    }

    fn resume(&mut self) {
        pidfd_send_signal(&self.process, Signal::CONT).unwrap();
        let deadline = Instant::now() + Duration::from_secs(2);
        while self.stopped() {
            assert!(Instant::now() < deadline, "owned daemon did not resume");
            std::thread::sleep(Duration::from_millis(10));
        }
        self.resumed = true;
    }
}

impl Drop for PausedOwnedDaemon {
    fn drop(&mut self) {
        if !self.resumed {
            // The runner still owns shutdown/reaping. Never leave its exact
            // daemon stopped when this case panics or its action fails early.
            if let Err(error) = pidfd_send_signal(&self.process, Signal::CONT) {
                if error != rustix::io::Errno::SRCH {
                    let _ = pidfd_send_signal(&self.process, Signal::KILL);
                    eprintln!("owned daemon resume failed before runner cleanup: {error}");
                }
            }
        }
    }
}

#[test]
#[ignore = "requires the rooted runtime_compiler_end_history_test owned daemon"]
fn physical_cell_response_without_end_ack_refuses_publication() {
    tidepool_testing::eval_harness::require_extract();
    let effects = TestEffectSurface::minimal(&[]).unwrap();
    let recipe = InputRecipe::capture(
        effects.preamble().to_owned(),
        effects.row().into(),
        effects.include_paths().to_vec(),
    );
    let root = tempfile::tempdir().unwrap();
    let library = SessionLib::open(
        SessionId(1798),
        root.path().join("session"),
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
    let public = resident.public_context();
    resident
        .set_actor_execution(
            public,
            EffectRunPolicy::SuspendAll,
            LivePayloadPolicy::HASKELL_EFFECT_VALUE,
        )
        .unwrap();
    resident.bootstrap_world(&recipe);
    let execution = begin_fixture_execution(&mut resident);
    resident
        .set_run_context(SessionRunContext {
            lexical_scope: execution.private_scope(),
            ..public
        })
        .unwrap();
    let admission = plan_fixture_cell(
        &mut resident,
        &recipe,
        "neverPublished <- pure (777 :: Int)",
        execution.clone(),
    );
    let before = resident
        .public_visibility_snapshot_in(public.lexical_scope)
        .unwrap();
    let artifact_root = PathBuf::from(std::env::var_os("TIDEPOOL_TEST_ARTIFACT_ROOT").unwrap());
    let paused = RefCell::new(None);
    let request_before = tidepool_extract_cmd::extract_spawn_count();
    let requests_after_response = Cell::new(request_before);
    let original = resident.public_source.original_source().clone();
    let outcome = original
        .with_source_compiler_transaction(
            tidepool_toolchain::artifacts::CompileWorkload::Foreground,
            crate::CompilerTransactionCancellation::new(),
            None,
            |_| {},
            |owner, settlement| {
                let prepared = turn::prepare_cell_program_admitted(owner, admission, settlement);
                assert!(
                    prepared.is_ok(),
                    "the real compiler response succeeds before END"
                );
                let request_count = tidepool_extract_cmd::extract_spawn_count();
                assert!(
                    request_count > request_before,
                    "preparation performed actual physical compiler work"
                );
                requests_after_response.set(request_count);
                // This action has not returned, so the source owner cannot yet call
                // CompilerTransaction::close or send this transaction's first END.
                *paused.borrow_mut() = Some(PausedOwnedDaemon::pause(&artifact_root));
                prepared
            },
        )
        .unwrap();
    paused.borrow_mut().as_mut().unwrap().resume();
    let requests_after_response = requests_after_response.get();
    assert!(
        outcome.action().is_ok(),
        "the successful response body survives failed END"
    );
    let crate::CompilerTransactionClose::Unconfirmed(evidence) = outcome.close() else {
        panic!(
            "withheld actual END acknowledgement must remain unconfirmed: {:?}",
            outcome.close()
        );
    };
    assert!(matches!(
        &evidence.retirement,
        tidepool_extract_cmd::CompilerTransactionRetirement::DaemonUnobserved { .. }
    ));
    assert!(matches!(
        &evidence.reason,
        tidepool_extract_cmd::CompilerTransactionCloseReason::EndFailed(failure)
            if failure.phase == tidepool_extract_cmd::CompilerTransactionClosePhase::DaemonEndAcknowledgement
    ));
    let close = format!("{:?}", outcome.close());
    let failure = turn::finish_cell_program_admitted(outcome).unwrap_err();
    assert!(matches!(
        &failure.error,
        tidepool_toolchain::CompileError::CompilerPreparationCloseUnconfirmed(_)
    ));
    assert_eq!(
        resident
            .public_visibility_snapshot_in(public.lexical_scope)
            .unwrap(),
        before
    );
    assert!(resident
        .current_binding_in(public.lexical_scope, "neverPublished")
        .is_none());
    assert_eq!(
        tidepool_extract_cmd::extract_spawn_count(),
        requests_after_response,
        "failed END permits no publication retry or replay"
    );
    std::fs::write(
        artifact_root.join("physical-response-withheld-end.json"),
        serde_json::to_vec_pretty(&serde_json::json!({
            "schema": 1,
            "successful_compiler_response": true,
            "physical_requests": requests_after_response - request_before,
            "end": close,
            "publication_refused": true,
            "public_state_unchanged": true,
            "replayed_requests": 0,
        }))
        .unwrap(),
    )
    .unwrap();
    drop(failure);
    drop(paused);
    drop(execution);
    drop(resident);
}
