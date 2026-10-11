//! Actual two-worker reservation proof; the foreground request is compiler-only.
use super::*;
use serde_json::{json, Value};
use tidepool_extract_cmd::{CompileWorkload, CompilerEndpoint};

fn rows(path: &Path) -> Vec<Value> {
    fs::read_to_string(path)
        .unwrap_or_default()
        .lines()
        // An appender may still be writing the final line while we observe it.
        .filter_map(|line| serde_json::from_str(line).ok())
        .collect()
}

fn message(row: &Value, expected: &str) -> bool {
    row["fields"]["message"] == expected
}

fn await_row(path: &Path, predicate: impl Fn(&Value) -> bool) -> Option<Value> {
    let until = Instant::now() + Duration::from_secs(30);
    loop {
        if let Some(row) = rows(path).into_iter().find(&predicate) {
            return Some(row);
        }
        if Instant::now() >= until {
            return None;
        }
        #[allow(
            clippy::disallowed_methods,
            reason = "bounded actual trace observation"
        )]
        std::thread::sleep(Duration::from_millis(5));
    }
}

fn bind(cmd: &ExtractCmd, socket: &Path, identity: &CompilerIdentity) -> CompilerEndpoint {
    let (key, value) = env_socket(socket);
    std::env::set_var(key, value);
    let endpoint = cmd.bind();
    std::env::remove_var(key);
    let endpoint = endpoint.expect("bind the test-owned compiler daemon");
    assert_eq!(
        endpoint.identity(),
        identity,
        "direct fallback is forbidden"
    );
    endpoint
}

#[test]
#[ignore = "requires an admitted matched two-worker compiler and retained trace resources"]
fn real_preparation_preserves_foreground_worker_and_grants() {
    run_fairness(1);
}

#[test]
#[ignore = "requires matched compiler and retained measurement resources"]
fn real_preparation_width_2() {
    run_fairness(2);
}

#[test]
#[ignore = "requires matched compiler and retained measurement resources"]
fn real_preparation_width_8() {
    run_fairness(8);
}

#[test]
#[ignore = "requires matched compiler and retained measurement resources"]
fn real_preparation_width_16() {
    run_fairness(16);
}

fn run_fairness(preparation_jobs: u32) {
    assert!(
        std::env::var_os(tidepool_extract_cmd::DAEMON_SOCKET_ENV).is_none(),
        "this fixture owns its daemon; use the counted runner's direct compiler mode"
    );
    let (bin, lib) = daemon_toolchain();
    let artifact = PathBuf::from(
        std::env::var_os("TIDEPOOL_TEST_ARTIFACT_ROOT")
            .expect("counted runner must supply a retained artifact root"),
    );
    let root = artifact.join("native-fairness");
    fs::create_dir(&root).expect("fresh fairness artifact directory");
    let graph = root.join("graph");
    fs::create_dir(&graph).unwrap();
    let mut imports = String::new();
    let mut terms = Vec::new();
    for n in 0..64 {
        let name = format!("FairnessLeaf{n}");
        fs::write(
            graph.join(format!("{name}.hs")),
            format!("module {name} where\nvalue{n} :: Int\nvalue{n} = {n}\n"),
        )
        .unwrap();
        imports.push_str(&format!("import {name}\n"));
        terms.push(format!("value{n}"));
    }
    fs::write(
        graph.join("FairnessGraph.hs"),
        format!(
            "module FairnessGraph where\n{imports}result :: Int\nresult = {}\n",
            terms.join(" + ")
        ),
    )
    .unwrap();
    fs::write(
        root.join("Foreground.hs"),
        "module Foreground where\nresult :: Int\nresult = 42\n",
    )
    .unwrap();
    let socket = unique_socket_path("native-fairness");
    let log = root.join("compiler.log");
    let trace = log.with_extension("jsonl");
    let preparation_jobs_text = preparation_jobs.to_string();
    let mut daemon = spawn_daemon(
        &bin,
        &socket,
        &[
            "--persistent",
            "--workers",
            "2",
            "--foreground-jobs",
            "2",
            "--preparation-jobs",
            &preparation_jobs_text,
            "--rss-ceiling-mb",
            "10240",
            "--request-deadline-secs",
            "300",
            "--run-id",
            "native-fairness",
            "--log-path",
            log.to_str().unwrap(),
        ],
    );
    // Retain startup diagnostics on successful runs as well as failures.
    daemon.ready = false;
    let identity = preflight_compiler_daemon(&socket).unwrap();
    let warm = cmd_for(&bin, &root, "foreground-warm", "Foreground.hs", &lib);
    let warm_result = bind(&warm, &socket, &identity).execute(&warm).unwrap();
    assert!(
        warm_result.success(),
        "foreground warmup: {:?}",
        warm_result.output
    );
    let foreground = cmd_for(&bin, &root, "foreground-repeat", "Foreground.hs", &lib);
    let mut preparation_one = cmd_for(&bin, &graph, "prep-one", "FairnessGraph.hs", &lib);
    preparation_one
        .include(&graph)
        .workload(CompileWorkload::Preparation);
    let mut preparation_two = cmd_for(&bin, &graph, "prep-two", "FairnessGraph.hs", &lib);
    preparation_two
        .include(&graph)
        .workload(CompileWorkload::Preparation);
    // Bind before starting threads: process environment is never mutated concurrently.
    let prep_one_endpoint = bind(&preparation_one, &socket, &identity);
    let prep_two_endpoint = bind(&preparation_two, &socket, &identity);
    let foreground_endpoint = bind(&foreground, &socket, &identity);
    let memory_pids: Vec<_> = rows(&trace)
        .into_iter()
        .filter(|row| message(row, "compiler worker ready"))
        .map(|row| row["fields"]["worker_pid"].as_u64().unwrap())
        .collect();
    assert_eq!(memory_pids.len(), 2);
    let sampling = std::sync::atomic::AtomicBool::new(true);
    let measured_started = Instant::now();
    let (observation, memory) = std::thread::scope(|scope| {
        let sampler = scope.spawn(|| sample_memory(&memory_pids, &sampling));
        let one = scope.spawn(|| prep_one_endpoint.execute(&preparation_one));
        let started = await_row(&trace, |r| {
            message(r, "compiler request started")
                && r["span"]["compiler_workload"] == "preparation"
        });
        let two = scope.spawn(|| prep_two_endpoint.execute(&preparation_two));
        let waiting = await_row(&trace, |r| {
            message(r, "compiler request waiting for capacity")
                && r["fields"]["compiler_workload"] == "preparation"
        });
        let foreground_result = foreground_endpoint.execute(&foreground);
        let one = one.join().expect("preparation one thread");
        let two = two.join().expect("preparation two thread");
        sampling.store(false, std::sync::atomic::Ordering::Release);
        (
            (started, waiting, foreground_result, one, two),
            sampler.join().unwrap(),
        )
    });
    let measured_elapsed_ns = measured_started.elapsed().as_nanos();
    #[allow(clippy::disallowed_methods, reason = "existing compiler stop owner")]
    let stop = Command::new(&bin)
        .args(["--stop-daemon", "--socket"])
        .arg(&socket)
        .status()
        .expect("stop the owned compiler daemon");
    assert!(stop.success(), "owned daemon stop refused");
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        if let Some(status) = daemon.child.try_wait().unwrap() {
            assert!(status.success(), "daemon shutdown: {status}");
            break;
        }
        assert!(Instant::now() < deadline, "daemon shutdown timed out");
        #[allow(clippy::disallowed_methods, reason = "bounded owned daemon shutdown")]
        std::thread::sleep(Duration::from_millis(10));
    }
    let all: Vec<Value> = fs::read_to_string(&trace)
        .expect("settled compiler trace")
        .lines()
        .map(|line| serde_json::from_str(line).expect("complete trace row"))
        .collect();
    let worker_pids: Vec<_> = all
        .iter()
        .filter(|r| message(r, "compiler worker ready"))
        .map(|r| {
            r["fields"]["worker_pid"]
                .as_u64()
                .expect("actual worker PID")
        })
        .collect();
    assert_eq!(
        worker_pids.len(),
        2,
        "exactly two original workers; no replacement"
    );
    for pid in &worker_pids {
        assert!(
            !Path::new(&format!("/proc/{pid}")).exists(),
            "owned worker still live after stop"
        );
    }
    for row in &all {
        assert!(row["timestamp"].is_string(), "trace timestamp missing");
    }
    let physical: Vec<_> = all
        .iter()
        .filter(|r| message(r, "compiler request finished"))
        .collect();
    let starts: Vec<_> = all
        .iter()
        .filter(|r| message(r, "compiler request started"))
        .collect();
    let prep_one_id = observation
        .0
        .as_ref()
        .and_then(|r| r["span"]["admission_id"].as_u64());
    let foreground_candidates: Vec<_> = starts
        .iter()
        .copied()
        .filter(|r| {
            r["span"]["compiler_workload"] == "foreground"
                && prep_one_id
                    .is_some_and(|id| r["span"]["admission_id"].as_u64().is_some_and(|n| n > id))
        })
        .collect();
    let foreground_start = if foreground_candidates.len() == 1 {
        Some(foreground_candidates[0])
    } else {
        None
    };
    let waiting_digest = observation
        .1
        .as_ref()
        .and_then(|r| r["fields"]["compile_request"].as_str())
        .filter(|digest| digest.len() == 16 && digest.bytes().all(|c| c.is_ascii_hexdigit()));
    let second_preparation: Vec<_> = physical
        .iter()
        .copied()
        .filter(|r| {
            waiting_digest
                .is_some_and(|digest| r["fields"]["compile_request"].as_str() == Some(digest))
                && r["span"]["compiler_workload"] == "preparation"
                && prep_one_id
                    .is_some_and(|id| r["span"]["admission_id"].as_u64().is_some_and(|n| n > id))
        })
        .collect();
    let prep_finish = observation.0.as_ref().and_then(|start| {
        physical
            .iter()
            .copied()
            .find(|r| r["span"]["physical_execution"] == start["span"]["physical_execution"])
    });
    let overlap = match (
        &observation.0,
        &observation.1,
        foreground_start,
        prep_finish,
    ) {
        (Some(start), Some(waiting), Some(foreground), Some(finish)) => {
            start["timestamp"].as_str() < waiting["timestamp"].as_str()
                && waiting["timestamp"].as_str() < foreground["timestamp"].as_str()
                && foreground["timestamp"].as_str() < finish["timestamp"].as_str()
        }
        _ => false,
    };
    let queued_digest_matches = second_preparation.len() == 1
        && observation.0.as_ref().is_some_and(|start| {
            waiting_digest.is_some_and(|digest| {
                start["fields"]["compile_request"]
                    .as_str()
                    .is_some_and(|first| first != digest)
            })
        });
    let concurrency = observed_concurrency(&all);
    let report = json!({"schema":1,
        "measurement_scope":"component compiler scheduling; excludes Harness latency",
        "requested_foreground_jobs":2,"requested_preparation_jobs":preparation_jobs,
        "worker_processes":2,"measured_elapsed_ns":measured_elapsed_ns,
        "completed_measured_requests":3,"completed_preparation_requests":2,
        "completed_preparation_requests_per_second":2.0 / (measured_elapsed_ns as f64 / 1e9),
        "throughput_scope":"measured mixed workload wall interval after foreground warmup; includes queueing and service",
        "completed_measured_requests_per_second":3.0 / (measured_elapsed_ns as f64 / 1e9),
        "observed_concurrency":concurrency,"worker_memory":memory,
        "module_ready_width":{"status":"not_collected","scope":"service concurrency and grants do not establish ready module width"},"workload":"compiler-only source graph and warm source repeat",
        "graph_modules":65,"physical_requests":physical,"queue_observations":all.iter()
            .filter(|r| message(r,"compiler job dequeued")).collect::<Vec<_>>(),
        "preparation_started":observation.0,"second_preparation_waiting":observation.1,
        "queued_digest_matches":queued_digest_matches,"foreground_start":foreground_start,
        "second_preparation_finished":second_preparation,"reserved_slot_overlap_observed":overlap,
        "worker_pids":worker_pids,"cleanup_confirmed":true,"status":if overlap {"observed"} else {"inconclusive"}});
    publish_fairness_report(&root.join("fairness.json"), report, || {
        for (name, result) in [
            ("foreground", observation.2),
            ("preparation one", observation.3),
            ("preparation two", observation.4),
        ] {
            let result = result.unwrap_or_else(|error| panic!("{name}: {error}"));
            assert!(result.success(), "{name}: {:?}", result.output);
        }
        assert_eq!(
            physical.len(),
            4,
            "exactly one warmup, foreground repeat, and two preparation requests"
        );
        assert_eq!(
            physical
                .iter()
                .filter(|r| r["span"]["compiler_workload"] == "preparation")
                .count(),
            2
        );
        assert_eq!(
            physical
                .iter()
                .filter(|r| r["span"]["compiler_workload"] == "foreground")
                .count(),
            2
        );
        let mut identities = std::collections::HashSet::new();
        for row in &physical {
            let span = &row["span"];
            let digest = row["fields"]["compile_request"]
                .as_str()
                .expect("actual logical request digest");
            assert!(digest.len() == 16 && digest.bytes().all(|c| c.is_ascii_hexdigit()));
            let physical_id = span["physical_execution"]
                .as_str()
                .expect("actual physical request identity");
            assert!(!physical_id.is_empty());
            assert!(identities.insert(physical_id));
            assert_eq!(span["request_ordinal"], 1);
            assert_eq!(row["fields"]["exit_code"], 0);
            let preparation = span["compiler_workload"] == "preparation";
            assert_eq!(span["worker"], if preparation { 1 } else { 0 });
            assert_eq!(
                span["compiler_jobs"],
                if preparation { preparation_jobs } else { 2 }
            );
            assert_eq!(
                span["compiler_capabilities"],
                if preparation { preparation_jobs } else { 2 }
            );
        }
        assert_eq!(concurrency["status"], "observed");
        assert_eq!(concurrency["peak_active_physical_requests"], 2);
        assert_eq!(memory["status"], "observed");
        assert!(
            queued_digest_matches,
            "waiting request must settle under the same logical identity"
        );
        assert!(overlap, "inconclusive: preparation finished before reserved-slot overlap; retain report and retry a controlled larger graph");
    });
}

/// Sample actual worker RSS together; this is a sampled aggregate, not a cgroup peak.
fn sample_memory(pids: &[u64], sampling: &std::sync::atomic::AtomicBool) -> Value {
    let mut samples = 0;
    let mut failures = 0;
    let mut aggregate_peak_kib = 0;
    let mut individual_peak_kib = 0;
    while sampling.load(std::sync::atomic::Ordering::Acquire) {
        let values: Option<Vec<u64>> = pids
            .iter()
            .map(|pid| {
                fs::read_to_string(format!("/proc/{pid}/status"))
                    .ok()
                    .and_then(|status| {
                        status
                            .lines()
                            .find(|line| line.starts_with("VmRSS:"))
                            .and_then(|line| line.split_whitespace().nth(1))
                            .and_then(|value| value.parse().ok())
                    })
            })
            .collect();
        if let Some(values) = values {
            samples += 1;
            aggregate_peak_kib = aggregate_peak_kib.max(values.iter().sum::<u64>());
            individual_peak_kib = individual_peak_kib.max(values.into_iter().max().unwrap_or(0));
        } else {
            failures += 1;
        }
        #[allow(
            clippy::disallowed_methods,
            reason = "bounded component measurement sampler"
        )]
        std::thread::sleep(Duration::from_millis(5));
    }
    json!({"status":if samples > 0 && failures == 0 {"observed"} else {"partial_or_unknown"},
        "samples":samples,"failed_samples":failures,"sample_interval_ms":5,
        "sampled_aggregate_worker_peak_kib":(samples > 0).then_some(aggregate_peak_kib),
        "sampled_individual_worker_peak_kib":(samples > 0).then_some(individual_peak_kib),
        "scope":"both original compiler workers; excludes daemon and client; sampled RSS, not process/cgroup peak"})
}

fn observed_concurrency(rows: &[Value]) -> Value {
    let mut active = std::collections::HashSet::new();
    let mut starts = std::collections::HashSet::new();
    let mut terminals = std::collections::HashSet::new();
    let mut peak = 0;
    let mut complete = false;
    let mut events: Vec<_> = rows
        .iter()
        .filter(|row| {
            message(row, "compiler request started") || message(row, "compiler request finished")
        })
        .collect();
    if !events.is_empty() {
        complete = true;
    }
    events.sort_by_key(|row| row["timestamp"].as_str());
    for row in events {
        let Some(identity) = row["span"]["physical_execution"]
            .as_str()
            .filter(|value| !value.is_empty())
        else {
            complete = false;
            continue;
        };
        complete &= row["timestamp"]
            .as_str()
            .is_some_and(|value| !value.is_empty());
        if message(row, "compiler request started") {
            complete &= starts.insert(identity);
            complete &= active.insert(identity);
            peak = peak.max(active.len());
        } else {
            complete &= terminals.insert(identity);
            complete &= active.remove(identity);
        }
    }
    complete &= active.is_empty() && starts == terminals;
    json!({"status":if complete {"observed"} else {"partial_or_unknown"},
        "peak_active_physical_requests":peak,"scope":"daemon physical service intervals; excludes queue wait"})
}

#[test]
fn concurrency_report_distinguishes_overlap_from_grant_width_and_incomplete_trace() {
    let event = |timestamp, message, identity| {
        json!({"timestamp":timestamp,
        "fields":{"message":message},"span":{"physical_execution":identity,"compiler_jobs":16}})
    };
    let rows = vec![
        event("1", "compiler request started", "prep"),
        event("2", "compiler request started", "foreground"),
        event("3", "compiler request finished", "foreground"),
        event("4", "compiler request finished", "prep"),
    ];
    assert_eq!(
        observed_concurrency(&rows)["peak_active_physical_requests"],
        2
    );
    assert_eq!(observed_concurrency(&rows)["status"], "observed");
    for invalid in [
        vec![rows[0].clone(), rows[0].clone(), rows[3].clone()],
        vec![rows[0].clone(), rows[3].clone(), rows[3].clone()],
        vec![
            rows[0].clone(),
            rows[3].clone(),
            event("5", "compiler request started", "prep"),
            event("6", "compiler request finished", "prep"),
        ],
        vec![rows[3].clone()],
        vec![],
    ] {
        assert_eq!(
            observed_concurrency(&invalid)["status"],
            "partial_or_unknown"
        );
    }
    let mut missing_timestamp = rows.clone();
    missing_timestamp[0]["timestamp"] = Value::Null;
    assert_eq!(
        observed_concurrency(&missing_timestamp)["status"],
        "partial_or_unknown"
    );

    assert_eq!(
        observed_concurrency(&rows[..3])["status"],
        "partial_or_unknown"
    );
}

#[test]
fn unsampled_worker_memory_is_unknown() {
    let disabled = std::sync::atomic::AtomicBool::new(false);
    let report = sample_memory(&[0], &disabled);
    assert_eq!(report["status"], "partial_or_unknown");
    assert!(report["sampled_aggregate_worker_peak_kib"].is_null());
    assert!(report["sampled_individual_worker_peak_kib"].is_null());
}

/// Preserve failure evidence before checking the workload; only validated work has throughput.
fn publish_fairness_report(path: &Path, mut report: Value, validate: impl FnOnce()) {
    let mut pending = report.clone();
    pending["status"] = json!("unqualified");
    pending["measurement_completed"] = json!(false);
    for field in [
        "completed_measured_requests",
        "completed_preparation_requests",
        "completed_preparation_requests_per_second",
        "completed_measured_requests_per_second",
    ] {
        pending[field] = Value::Null;
    }
    fs::write(path, serde_json::to_vec_pretty(&pending).unwrap()).unwrap();
    validate();
    report["status"] = json!("observed");
    report["measurement_completed"] = json!(true);
    fs::write(path, serde_json::to_vec_pretty(&report).unwrap()).unwrap();
    eprintln!("native-fairness-summary {report}");
}

#[test]
fn failed_fairness_validation_retains_unqualified_report_without_throughput() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("fairness.json");
    let report = json!({"status":"observed", "completed_measured_requests":3,
        "completed_preparation_requests":2,"completed_preparation_requests_per_second":1.0,
        "completed_measured_requests_per_second":1.5,"reserved_slot_overlap_observed":true});
    let outcome = std::panic::catch_unwind(|| {
        publish_fairness_report(&path, report.clone(), || panic!("request failed"))
    });
    assert!(outcome.is_err());
    let failed: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    assert_eq!(failed["status"], "unqualified");
    assert_eq!(failed["measurement_completed"], false);
    assert!(failed["completed_measured_requests_per_second"].is_null());
    assert!(failed["completed_preparation_requests_per_second"].is_null());
    assert!(failed["completed_measured_requests"].is_null());
    publish_fairness_report(&path, report, || {});
    let passed: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    assert_eq!(passed["measurement_completed"], true);
    assert_eq!(passed["completed_measured_requests"], 3);
}
