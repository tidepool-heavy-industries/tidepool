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
            "1",
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
    let observation = std::thread::scope(|scope| {
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
        (started, waiting, foreground_result, one, two)
    });
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
    let foreground_start = starts
        .iter()
        .filter(|r| r["span"]["compiler_workload"] == "foreground")
        .last()
        .copied();
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
    let queued_digest_matches = observation.1.as_ref().is_some_and(|waiting| {
        physical.iter().any(|r| {
            r["fields"]["compile_request"] == waiting["fields"]["compile_request"]
                && r["span"]["compiler_workload"] == "preparation"
        })
    });
    let report = json!({"schema":1,"workload":"compiler-only source graph and warm source repeat",
        "graph_modules":65,"physical_requests":physical,"queue_observations":all.iter()
            .filter(|r| message(r,"compiler job dequeued")).collect::<Vec<_>>(),
        "preparation_started":observation.0,"second_preparation_waiting":observation.1,
        "queued_digest_matches":queued_digest_matches,"reserved_slot_overlap_observed":overlap,
        "worker_pids":worker_pids,"cleanup_confirmed":true,"status":if overlap {"observed"} else {"inconclusive"}});
    fs::write(
        root.join("fairness.json"),
        serde_json::to_vec_pretty(&report).unwrap(),
    )
    .unwrap();
    eprintln!("native-fairness-summary {report}");
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
        assert!(identities.insert(span["physical_execution"].as_str().unwrap()));
        assert_eq!(span["request_ordinal"], 1);
        assert_eq!(row["fields"]["exit_code"], 0);
        let preparation = span["compiler_workload"] == "preparation";
        assert_eq!(span["worker"], if preparation { 1 } else { 0 });
        assert_eq!(span["compiler_jobs"], if preparation { 1 } else { 2 });
        assert_eq!(
            span["compiler_capabilities"],
            if preparation { 1 } else { 2 }
        );
    }
    assert!(
        queued_digest_matches,
        "waiting request must settle under the same logical identity"
    );
    assert!(overlap, "inconclusive: preparation finished before reserved-slot overlap; retain report and retry a controlled larger graph");
}
