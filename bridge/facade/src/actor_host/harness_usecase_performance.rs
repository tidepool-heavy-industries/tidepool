//! Bounded real HTTP/Engine/Store/notebook workload; only provider replies are scripted.
//!
//! Run alone against an owned matched compiler daemon with `TIDEPOOL_TEST_TRACE`
//! and its daemon JSONL retained. Rows measure reply-to-successor wall time and
//! logical submissions. Physical service/queue time belongs to the daemon trace.

use super::*;
use std::time::Instant;

fn prepare_performance_traces() -> (std::path::PathBuf, std::path::PathBuf, Value) {
    let artifact_root = std::path::PathBuf::from(
        std::env::var_os("TIDEPOOL_TEST_ARTIFACT_ROOT")
            .expect("owned-resident counted runner supplies per-case artifacts"),
    );
    assert!(
        artifact_root.is_absolute(),
        "per-case artifact root is absolute"
    );
    let host_trace = artifact_root.join("host.jsonl");
    let compiler_trace = artifact_root.join("compiler/compiler.jsonl");
    let lifecycle: Value = serde_json::from_slice(
        &std::fs::read(artifact_root.join("compiler/lifecycle.json"))
            .expect("owned compiler runner records its live daemon identity before the test"),
    )
    .expect("owned compiler lifecycle record is JSON");
    assert_eq!(
        lifecycle["cleanup_confirmed"], false,
        "daemon is live for the case"
    );
    assert!(
        compiler_trace.is_file(),
        "owned daemon created its JSONL trace"
    );
    std::env::set_var("TIDEPOOL_TEST_TRACE", &host_trace);
    std::env::set_var("TIDEPOOL_PERFORMANCE_COMPILER_TRACE", &compiler_trace);
    (host_trace, compiler_trace, lifecycle)
}

struct Phase {
    name: &'static str,
    source: &'static str,
    expected: &'static str,
    asynchronous: bool,
}

fn issue(round: RequestedRound, phase: &Phase, call_id: &str) {
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
    let (host_trace, compiler_trace, compiler_lifecycle) = prepare_performance_traces();
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
    println!(
        "harness-usecase {}",
        json!({
            "schema": 1,
            "phase": "environment",
            "prepared_root_entry_supplied": deployment["TIDEPOOL_PREPARED_ROOT_ENTRY"].is_some(),
            "host_trace": host_trace,
            "compiler_trace": compiler_trace,
            "owned_compiler_lifecycle": compiler_lifecycle,
            "deployment": deployment,
        })
    );
    let activation_started = Instant::now();
    let activation_requests = tidepool_extract_cmd::extract_spawn_count();
    let (_files, fixture, mut rounds) = start_with_spec(None).await;
    let mut round = next_round(&mut rounds).await;
    let actor = fixture.context.actor.identity();
    let session = round.request.session_id.clone();
    println!(
        "harness-usecase {}",
        json!({
            "schema": 1, "phase": "activation", "completed": true,
            "wall_ns": activation_started.elapsed().as_nanos(),
            "logical_compiler_requests": tidepool_extract_cmd::extract_spawn_count() - activation_requests,
            "provider_calls": round.provider_calls.load(Ordering::SeqCst),
            "context_items": round.request.input.len(),
            "context_json_bytes": serde_json::to_vec(&round.request.input).unwrap().len(),
            "cold_scope": "fresh host/session; daemon and artifact cache history recorded externally",
        })
    );
    let phases = [
        Phase { name: "first-arithmetic", source: "_ <- display (40 + 2 :: Int)", expected: "42", asynchronous: false },
        Phase { name: "publish-retained", source: include_str!("fixtures/harness_usecase_publish.hs"), expected: "6", asynchronous: false },
        Phase { name: "lookup-retained", source: "inspected <- LookupApi.lookupRaw (LookupApi.lookupRequest [\"perfSamples\", \"perfAction\"])\n_ <- display (show inspected)", expected: "perfSamples", asynchronous: false },
        Phase { name: "reuse-retained", source: "value <- perfAction\n_ <- display (value + 36 :: Int)", expected: "42", asynchronous: false },
        Phase { name: "repeat-retained", source: "value <- perfAction\n_ <- display (value + 36 :: Int)", expected: "42", asynchronous: false },
        Phase { name: "async-yield-result", source: include_str!("fixtures/harness_usecase_async.hs"), expected: "42", asynchronous: true },
        Phase { name: "reuse-async-action", source: "value <- perfDelayed\n_ <- display value", expected: "42", asynchronous: false },
        Phase { name: "repeat-arithmetic", source: "_ <- display (40 + 2 :: Int)", expected: "42", asynchronous: false },
    ];
    for (index, phase) in phases.iter().enumerate() {
        let call_id = format!("usecase-{index}-{}", phase.name);
        let before = tidepool_extract_cmd::extract_spawn_count();
        let provider_before = round.provider_calls.load(Ordering::SeqCst);
        let started = Instant::now();
        issue(round, phase, &call_id);
        let successor = next_round(&mut rounds).await;
        let first_successor_ns = started.elapsed().as_nanos();
        let yielded_before_terminal = !has_output(&successor, &call_id);
        round = if phase.asynchronous && yielded_before_terminal {
            successor.wait_for_pending();
            next_round_with_output(&mut rounds, &call_id).await
        } else {
            successor
        };
        let completed_ns = started.elapsed().as_nanos();
        let output = successful_output(&round.request, &call_id);
        let displayed = test_campaign::explicit_display_text(&output);
        if phase.name == "lookup-retained" {
            assert!(
                displayed.contains("perfSamples") && displayed.contains("perfAction"),
                "{output}"
            );
        } else {
            assert_eq!(displayed.trim(), phase.expected, "{output}");
        }
        assert_eq!(round.request.session_id, session);
        assert_eq!(fixture.context.actor.identity(), actor);
        let logical_compiler_requests = tidepool_extract_cmd::extract_spawn_count() - before;
        println!(
            "harness-usecase {}",
            json!({
                "schema": 1, "phase": phase.name, "sequence": index, "call_id": call_id,
                "completed": true, "wall_ns": completed_ns, "first_successor_ns": first_successor_ns,
                "logical_compiler_requests": logical_compiler_requests,
                "provider_calls": round.provider_calls.load(Ordering::SeqCst) - provider_before,
                "context_items": round.request.input.len(),
                "context_json_bytes": serde_json::to_vec(&round.request.input).unwrap().len(),
                "invocation_execution": if phase.asynchronous { "asynchronous" } else { "synchronous" },
                "yielded_before_terminal": yielded_before_terminal,
                "source": phase.source, "expected": phase.expected, "displayed": displayed,
                "history": if index == 0 { "first authored cell in fresh session" } else { "ordered prior phases in same session" },
            })
        );
    }
    round.finish();
    fixture.stop().await.unwrap();
}
