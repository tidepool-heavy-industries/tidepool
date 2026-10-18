//! Bounded real HTTP/Engine/Store/notebook workload; only provider replies are scripted.
//!
//! Run alone against an owned matched compiler daemon with `TIDEPOOL_TEST_TRACE`
//! and its daemon JSONL retained. Rows measure reply-to-successor wall time and
//! logical submissions. Physical service/queue time belongs to the daemon trace.

use super::super::test_campaign::{prepare_performance_traces, record_phase};
use super::*;
use std::time::Instant;

#[derive(Clone, Copy)]
enum NotebookHistory {
    OrderedUsecase,
    AABA,
    Persona,
}

impl NotebookHistory {
    fn cohort(self) -> &'static str {
        match self {
            Self::OrderedUsecase => "harness-eight-phase",
            Self::AABA => "notebook-a-a-b-a",
            Self::Persona => "persona-workbench",
        }
    }
}

struct Phase {
    name: &'static str,
    source: &'static str,
    expected: &'static str,
    asynchronous: bool,
}

fn issue(round: RequestedRound, phase: &Phase, call_id: &str) {
    if phase.name == "persona-setup" {
        // Real recorded assistant messages, not tool-output strings supplied
        // with an assumed context reference. The context owner issues targets.
        let mut items = (1..=5)
            .map(|index| {
                Item(json!({
                    "type": "message", "role": "assistant", "phase": "commentary",
                    "content": [{"type": "output_text",
                        "text": format!("persona-original-{index}")}],
                }))
            })
            .collect::<Vec<_>>();
        items.push(Item(json!({
            "type": "custom_tool_call", "call_id": call_id,
            "name": "haskell_sync", "input": phase.source,
        })));
        round
            .reply
            .send(ResponsesTurn {
                response_id: format!("response-{call_id}"),
                items,
                usage: Default::default(),
            })
            .expect("resident Engine still awaits its scripted reply");
        return;
    }
    round.cell_named(
        if phase.asynchronous {
            "haskell"
        } else {
            "haskell_sync"
        },
        call_id,
        phase.source,
        if phase.asynchronous {
            ToolExecution::Asynchronous
        } else {
            ToolExecution::Synchronous
        },
    );
}

#[tokio::test]
#[ignore = "requires exclusive matched compiler daemon and retained host/daemon traces"]
async fn production_harness_persona_workbench_history() {
    let display = include_str!("fixtures/harness_usecase_persona_display.hs");
    let read = include_str!("fixtures/harness_usecase_persona_read.hs");
    notebook_phases(
        NotebookHistory::Persona,
        &[
            Phase {
                name: "persona-setup",
                source: include_str!("fixtures/harness_usecase_persona_setup.hs"),
                expected: "persona-ready",
                asynchronous: false,
            },
            Phase {
                name: "fresh-source-display",
                source: display,
                expected: "module Tidepool.Agent.Context",
                asynchronous: false,
            },
            Phase {
                name: "fresh-context-read",
                source: read,
                expected: "True",
                asynchronous: false,
            },
            Phase {
                name: "accumulate-one",
                source: include_str!("fixtures/harness_usecase_persona_accumulate_one.hs"),
                expected: "36",
                asynchronous: false,
            },
            Phase {
                name: "accumulate-two",
                source: include_str!("fixtures/harness_usecase_persona_accumulate_two.hs"),
                expected: "100",
                asynchronous: false,
            },
            Phase {
                name: "accumulate-three",
                source: include_str!("fixtures/harness_usecase_persona_accumulate_three.hs"),
                expected: "164",
                asynchronous: false,
            },
            Phase {
                name: "accumulate-four",
                source: include_str!("fixtures/harness_usecase_persona_accumulate_four.hs"),
                expected: "228",
                asynchronous: false,
            },
            Phase {
                name: "warm-source-display-first",
                source: display,
                expected: "module Tidepool.Agent.Context",
                asynchronous: false,
            },
            Phase {
                name: "warm-source-display-repeat",
                source: display,
                expected: "module Tidepool.Agent.Context",
                asynchronous: false,
            },
            Phase {
                name: "warm-context-read-first",
                source: read,
                expected: "True",
                asynchronous: false,
            },
            Phase {
                name: "warm-context-read-repeat",
                source: read,
                expected: "True",
                asynchronous: false,
            },
            Phase {
                name: "context-edit-five",
                source: include_str!("fixtures/harness_usecase_persona_edit.hs"),
                expected: "staged-five",
                asynchronous: false,
            },
            Phase {
                name: "context-edit-readback",
                source: include_str!("fixtures/harness_usecase_persona_readback.hs"),
                expected: "True",
                asynchronous: false,
            },
        ],
    )
    .await;
}

fn has_output(round: &RequestedRound, call_id: &str) -> bool {
    round
        .request
        .input
        .iter()
        .any(|item| item.0["type"] == "custom_tool_call_output" && item.0["call_id"] == call_id)
}

#[tokio::test]
#[ignore = "requires exclusive matched compiler daemon and retained host/daemon traces"]
async fn production_harness_notebook_usecase_phases() {
    notebook_phases(NotebookHistory::OrderedUsecase, &[
        Phase {
            name: "first-arithmetic",
            source: "_ <- display (40 + 2 :: Int)",
            expected: "42",
            asynchronous: false,
        },
        Phase {
            name: "publish-retained",
            source: include_str!("fixtures/harness_usecase_publish.hs"),
            expected: "6",
            asynchronous: false,
        },
        Phase {
            name: "lookup-retained",
            source: "inspected <- LookupApi.lookupRaw (LookupApi.lookupRequest [\"perfSamples\", \"perfAction\"])\n_ <- display (show inspected)",
            expected: "perfSamples",
            asynchronous: false,
        },
        Phase {
            name: "reuse-retained",
            source: "value <- perfAction\n_ <- display (value + 36 :: Int)",
            expected: "42",
            asynchronous: false,
        },
        Phase {
            name: "repeat-retained",
            source: "value <- perfAction\n_ <- display (value + 36 :: Int)",
            expected: "42",
            asynchronous: false,
        },
        Phase {
            name: "async-yield-result",
            source: include_str!("fixtures/harness_usecase_async.hs"),
            expected: "42",
            asynchronous: true,
        },
        Phase {
            name: "reuse-async-action",
            source: "value <- perfDelayed\n_ <- display value",
            expected: "42",
            asynchronous: false,
        },
        Phase {
            name: "repeat-arithmetic",
            source: "_ <- display (40 + 2 :: Int)",
            expected: "42",
            asynchronous: false,
        },
    ]).await;
}

#[tokio::test]
#[ignore = "requires exclusive matched compiler daemon and retained host/daemon traces"]
async fn production_harness_notebook_a_a_b_a() {
    notebook_phases(
        NotebookHistory::AABA,
        &[
            Phase {
                name: "first-a",
                source: "_ <- display (40 + 2 :: Int)",
                expected: "42",
                asynchronous: false,
            },
            Phase {
                name: "repeat-a",
                source: "_ <- display (40 + 2 :: Int)",
                expected: "42",
                asynchronous: false,
            },
            Phase {
                name: "changed-b",
                source: "_ <- display (40 + 3 :: Int)",
                expected: "43",
                asynchronous: false,
            },
            Phase {
                name: "return-a",
                source: "_ <- display (40 + 2 :: Int)",
                expected: "42",
                asynchronous: false,
            },
        ],
    )
    .await;
}

async fn notebook_phases(history: NotebookHistory, phases: &[Phase]) {
    let case_started = Instant::now();
    let cohort = history.cohort();
    let prepared_workspace = matches!(history, NotebookHistory::AABA | NotebookHistory::Persona);
    let (host_trace, compiler_trace, phase_trace, compiler_lifecycle, trace_capture) =
        prepare_performance_traces();
    assert!(
        std::env::var_os(tidepool_extract_cmd::DAEMON_SOCKET_ENV).is_some(),
        "measure resident production compilation, not per-request worker spawning"
    );
    assert!(
        std::env::var_os("TIDEPOOL_TEST_TRACE").is_some(),
        "retain complete host scope and request correlation evidence"
    );
    assert!(
        std::env::var_os("TIDEPOOL_PERFORMANCE_COMPILER_TRACE").is_some(),
        "retain owning daemon queue/service evidence; host durations alone cannot establish it"
    );
    let deployment: BTreeMap<_, _> = [
        "TIDEPOOL_TEST_ARTIFACT_ROOT",
        "TIDEPOOL_EXTRACT_REQUIRED_DAEMON_ENDPOINT",
        "TIDEPOOL_TEST_TRACE",
        "TIDEPOOL_PREPARED_ROOT_ENTRY",
        "TIDEPOOL_COMPILER_DEPLOYMENT",
        "TIDEPOOL_COMPILER_MODULES",
        "TIDEPOOL_EXTRACT_WORKER",
        "TIDEPOOL_PERFORMANCE_COMPILER_TRACE",
        "TIDEPOOL_TEST_TRACE",
        "TIDEPOOL_EXTRACT_MEASUREMENT_DAEMON_PID",
        "TIDEPOOL_EXTRACT_MEASUREMENT_DAEMON_PRODUCER",
        "TIDEPOOL_EXTRACT_MEASUREMENT_DAEMON_EPOCH",
    ]
    .into_iter()
    .map(|key| {
        (
            key,
            std::env::var_os(key).map(|value| value.to_string_lossy().into_owned()),
        )
    })
    .collect();
    record_phase(
        &phase_trace,
        json!({
            "schema": 1,
            "phase": "environment",
            "workload_cohort": cohort,
            "workspace_route": if prepared_workspace { "prepared-workspace" } else { "unprepared-workspace" },
            "campaign_trace_profile": super::super::test_campaign::campaign_trace_profile(),
            "campaign_trace_capture": trace_capture,
            "workload_roster": phases.iter().map(|phase| phase.name).collect::<Vec<_>>(),
            "prepared_root_entry_supplied": deployment["TIDEPOOL_PREPARED_ROOT_ENTRY"].is_some(),
            "host_trace": host_trace,
            "compiler_trace": compiler_trace,
            "phase_trace": phase_trace,
            "owned_compiler_lifecycle": compiler_lifecycle,
            "deployment": deployment,
        }),
    );
    let activation_start_ns = case_started.elapsed().as_nanos();
    let activation_requests = tidepool_extract_cmd::extract_spawn_count();
    let (_files, fixture, mut rounds) = if prepared_workspace {
        let (files, settings) = settings();
        let (requests, rounds) = mpsc::unbounded_channel();
        let transport: Arc<dyn ResponsesTransport> = Arc::new(ScriptedProvider {
            requests,
            calls: Arc::new(AtomicUsize::new(0)),
        });
        let fixture =
            HostedTestRuntime::start_prepared_configured(&settings, &transport, |config| {
                crate::exomonad::write_fixture_project_config(
                    &config.workspace.join(".exomonad"),
                    "test-model",
                    |project| {
                        project.launch.embedded = Some(settings.clone());
                    },
                );
                super::super::test_campaign::commit_workspace(&config.workspace);
            })
            .await
            .unwrap();
        fixture
            .http_input("Exercise the prepared notebook history.")
            .await
            .unwrap();
        (files, fixture, rounds)
    } else {
        start_with_spec(None).await
    };
    let mut round = next_round(&mut rounds, &fixture.context).await;
    let actor = fixture.context.actor.identity();
    let session = round.request.session_id.clone();
    let activation_end_ns = case_started.elapsed().as_nanos();
    let verification_start_ns = case_started.elapsed().as_nanos();
    let prepared_selection = if prepared_workspace {
        Some(
            fixture
                .assert_prepared_workspace_installation()
                .await
                .selection(),
        )
    } else {
        None
    };
    let verification_end_ns = case_started.elapsed().as_nanos();
    record_phase(
        &phase_trace,
        json!({
            "schema": 1, "phase": "activation", "completed": true,
            "wall_ns": activation_end_ns - activation_start_ns,
            "start_ns": activation_start_ns, "end_ns": activation_end_ns,
            "prepared_workspace_selection": prepared_selection,
            "installation_verification_start_ns": verification_start_ns,
            "installation_verification_end_ns": verification_end_ns,
            "installation_verification_ns": verification_end_ns - verification_start_ns,
            "logical_compiler_requests": tidepool_extract_cmd::extract_spawn_count() - activation_requests,
            "provider_calls": round.provider_calls.load(Ordering::SeqCst),
            "context_items": round.request.input.len(),
            "context_json_bytes": serde_json::to_vec(&round.request.input).unwrap().len(),
            "workspace_preparation_ns": fixture.preparation_elapsed_ns(),
            "host_start_readiness_ns": fixture.startup_readiness_elapsed_ns(),
            "cold_scope": "fresh host/session; daemon and artifact cache history recorded externally",
        }),
    );
    for (index, phase) in phases.iter().enumerate() {
        let call_id = format!("usecase-{index}-{}", phase.name);
        let before = tidepool_extract_cmd::extract_spawn_count();
        let images_before =
            tidepool_codegen::prepared_program::CompiledProgram::successful_image_compilations();
        let provider_before = round.provider_calls.load(Ordering::SeqCst);
        let start_ns = case_started.elapsed().as_nanos();
        issue(round, phase, &call_id);
        let successor = next_round(&mut rounds, &fixture.context).await;
        let first_successor_ns = case_started.elapsed().as_nanos() - start_ns;
        let yielded_before_terminal = !has_output(&successor, &call_id);
        round = if phase.asynchronous && yielded_before_terminal {
            successor.wait_for_pending();
            next_round_with_output(&mut rounds, &fixture.context, &call_id).await
        } else {
            successor
        };
        let end_ns = case_started.elapsed().as_nanos();
        let completed_ns = end_ns - start_ns;
        let output = successful_output(&round.request, &call_id);
        let displayed = test_campaign::retained_committed_display_text(
            &fixture.runtime.store(),
            &test_campaign::actor_output_origin(fixture.runtime.run_identity(), actor),
            &output,
        );
        if phase.name == "lookup-retained" {
            assert!(
                displayed.contains("perfSamples") && displayed.contains("perfAction"),
                "{output}"
            );
        } else {
            assert_eq!(displayed, phase.expected, "{output}");
        }
        assert_eq!(round.request.session_id, session);
        assert_eq!(fixture.context.actor.identity(), actor);
        let logical_compiler_requests = tidepool_extract_cmd::extract_spawn_count() - before;
        record_phase(
            &phase_trace,
            json!({
                "schema": 1, "phase": phase.name, "sequence": index, "call_id": call_id,
                "actor_id": actor.to_string(),
                "completed": true, "wall_ns": completed_ns, "first_successor_ns": first_successor_ns,
                "start_ns": start_ns, "end_ns": end_ns,
                "logical_compiler_requests": logical_compiler_requests,
                "successful_native_image_constructions": tidepool_codegen::prepared_program::CompiledProgram::successful_image_compilations() - images_before,
                "provider_calls": round.provider_calls.load(Ordering::SeqCst) - provider_before,
                "context_items": round.request.input.len(),
                "context_json_bytes": serde_json::to_vec(&round.request.input).unwrap().len(),
                "invocation_execution": if phase.asynchronous { "asynchronous" } else { "synchronous" },
                "yielded_before_terminal": yielded_before_terminal,
                "source": phase.source, "expected": phase.expected, "displayed": displayed,
                "history": if index == 0 { "first authored cell in fresh session" } else { "ordered prior phases in same session" },
            }),
        );
    }
    let shutdown_start_ns = case_started.elapsed().as_nanos();
    let shutdown_requests = tidepool_extract_cmd::extract_spawn_count();
    round.finish();
    fixture
        .stop(&crate::actor_host::hosted_test_context::ScenarioOutcome::Passed)
        .await
        .unwrap();
    let shutdown_end_ns = case_started.elapsed().as_nanos();
    record_phase(
        &phase_trace,
        json!({"schema": 1, "phase": "measurement-summary", "completed": true,
            "wall_ns": shutdown_end_ns - shutdown_start_ns,
            "start_ns": shutdown_start_ns, "end_ns": shutdown_end_ns,
            "case_wall_ns": shutdown_end_ns,
            "logical_compiler_requests": tidepool_extract_cmd::extract_spawn_count() - shutdown_requests}),
    );
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "requires exclusive matched compiler daemon and frozen prepared bundle"]
async fn production_harness_prepare_once_repeated_launch() {
    use super::super::test_campaign::{
        commit_workspace, hosted_script_provider, hosted_test_settings, next_hosted_script_round,
    };
    use crate::exomonad::workspace::{prepare_workspace, FrozenWorkspace};
    use harness::model::AgentPath;
    use std::collections::{HashSet, VecDeque};
    let (host_trace, compiler_trace, phase_trace, lifecycle, trace_capture) =
        prepare_performance_traces();
    let roster = [
        "fresh-preparation",
        "launch-1",
        "launch-2",
        "launch-3",
        "host-cleanup",
    ];
    record_phase(
        &phase_trace,
        json!({"schema": 1, "phase": "environment",
        "workload_cohort": "prepare-once-repeated-launch", "workload_roster": roster,
        "campaign_trace_profile": super::super::test_campaign::campaign_trace_profile(),
            "campaign_trace_capture": trace_capture,
        "prepared_root_entry_supplied": std::env::var_os("TIDEPOOL_PREPARED_ROOT_ENTRY").is_some(),
        "host_trace": host_trace, "compiler_trace": compiler_trace, "phase_trace": phase_trace,
        "owned_compiler_lifecycle": lifecycle,
        "deployment": (["TIDEPOOL_COMPILER_MODULES", "TIDEPOOL_PREPARED_ROOT_ENTRY", "TIDEPOOL_COMPILER_DEPLOYMENT"].into_iter().map(|key| (key, std::env::var(key).unwrap())).collect::<BTreeMap<_, _>>())}),
    );
    let artifact_root = PathBuf::from(std::env::var_os("TIDEPOOL_TEST_ARTIFACT_ROOT").unwrap());
    let mut files = tempfile::Builder::new()
        .prefix("repeat-launch-")
        .tempdir_in(&artifact_root)
        .unwrap();
    files.disable_cleanup(true);
    let settings = hosted_test_settings(&files, 1);
    let mut repository = exomonad_worktree::testing::TestRepo::init_in(&artifact_root).unwrap();
    repository.disable_cleanup(true);
    crate::exomonad::write_fixture_project_config(
        &repository.path().join(".exomonad"),
        "test-model",
        |project| {
            project.launch.embedded = Some(settings.clone());
        },
    );
    commit_workspace(repository.path());
    let directory = Arc::new(
        tidepool_atomic_write::DirectoryAnchor::open_existing(files.path())
            .unwrap()
            .child("prepared-once")
            .unwrap(),
    );
    let started = Instant::now();
    let requests_before = tidepool_extract_cmd::extract_spawn_count();
    let prepared = prepare_workspace(repository.path(), Arc::clone(&directory))
        .await
        .unwrap();
    let preparation_ns = started.elapsed().as_nanos();
    let selection = prepared.pointer().unwrap().selection_digest;
    record_phase(
        &phase_trace,
        json!({"schema": 1, "phase": "fresh-preparation", "sequence": 0,
        "completed": true, "wall_ns": preparation_ns, "logical_compiler_requests": tidepool_extract_cmd::extract_spawn_count() - requests_before,
        "evidence": {"deployment": directory.path(), "selection_digest": selection, "fresh_preparation": true}}),
    );
    let mut actors = HashSet::new();
    let mut journals = HashSet::new();
    let mut shutdown_ns = 0;
    let mut shutdown_requests = 0;
    for index in 0..3 {
        let (provider, mut rounds) = hosted_script_provider();
        let started = Instant::now();
        let requests_before = tidepool_extract_cmd::extract_spawn_count();
        let images_before =
            tidepool_codegen::prepared_program::CompiledProgram::successful_image_compilations();
        let host = HostedTestRuntime::start_configured(&settings, &provider, |config| {
            config.workspace = repository.path().to_owned();
            let inputs = FrozenWorkspace::select_prepared(
                repository.path(),
                config.run_directory.path(),
                Some(directory.path()),
            )
            .unwrap();
            assert!(
                inputs.prepared_toolset.is_none(),
                "each launch authenticates the completed deployment independently"
            );
            config.haskell_root = inputs.runtime_actors();
            config.workspace_inputs = Some(inputs);
        })
        .await
        .unwrap();
        let launch_ns = started.elapsed().as_nanos();
        if index == 0 {
            record_phase(
                &phase_trace,
                json!({"schema": 1, "phase": "activation", "completed": true,
                "wall_ns": launch_ns, "logical_compiler_requests": 0, "workspace_preparation_ns": preparation_ns,
                "host_start_readiness_ns": host.startup_readiness_elapsed_ns(),
                "evidence": {"scope": "first durable production launch; fresh preparation recorded separately"}}),
            );
        }
        assert_eq!(
            tidepool_extract_cmd::extract_spawn_count(),
            requests_before,
            "repeat launch submits no compiler request"
        );
        assert!(actors.insert(host.context.actor.identity()));
        let run_id = host
            .context
            .config
            .run_directory
            .path()
            .file_name()
            .unwrap()
            .to_owned();
        assert!(journals.insert(crate::exomonad::exomonad_journal_path(
            repository.path(),
            run_id.to_str().unwrap()
        )));
        record_phase(
            &phase_trace,
            json!({"schema": 1, "phase": roster[index + 1], "sequence": index + 1,
            "completed": true, "wall_ns": launch_ns, "logical_compiler_requests": 0,
            "successful_native_image_constructions": tidepool_codegen::prepared_program::CompiledProgram::successful_image_compilations() - images_before,
            "evidence": {"fresh_preparation": false, "selection_digest": selection,
                "host_start_readiness_ns": host.startup_readiness_elapsed_ns(), "fresh_actor_identity": host.context.actor.identity().to_string(),
                "selection_and_fixture_ns": launch_ns.saturating_sub(host.startup_readiness_elapsed_ns())}}),
        );
        let shutdown_started = Instant::now();
        let shutdown_requests_before = tidepool_extract_cmd::extract_spawn_count();
        host.run_scenario(|host| {
            Box::pin(async move {
                host.http_input("Start this independently launched resident actor.")
                    .await
                    .unwrap();
                let mut pending = VecDeque::new();
                let round =
                    next_hosted_script_round(&mut rounds, &mut pending, &AgentPath("/root".into()))
                        .await;
                round.finish();
            })
        })
        .await;
        shutdown_ns += shutdown_started.elapsed().as_nanos();
        shutdown_requests += tidepool_extract_cmd::extract_spawn_count() - shutdown_requests_before;
    }
    record_phase(
        &phase_trace,
        json!({"schema": 1, "phase": "host-cleanup", "sequence": 4,
        "completed": true, "wall_ns": shutdown_ns, "logical_compiler_requests": shutdown_requests,
        "evidence": {"production_hosts_shutdown_acknowledged": 3, "scope": "sum of disjoint HTTP input, provider completion and host shutdown windows"}}),
    );
}
