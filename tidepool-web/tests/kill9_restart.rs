//! PRD §11 acceptance: kill -9 the REAL `tidepool-harness` binary mid-suspension,
//! restart it with `--replay`, and prove the tree state re-derives to the SAME
//! suspended hole and that hole is still answerable — driving the actual
//! observatory binary over HTTP (E1/E2), not `fold_tree_state` directly.
//!
//! Zero live API calls: this test hand-authors the "prior recording" a live
//! run would have produced (one recorded assistant turn — see
//! `write_source_log`), and boots BOTH the pre-kill and the post-restart
//! process in `--replay` mode over it. GHC-tier: it drives real extract
//! compiles through the spawned binary.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpStream;
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde_json::json;
use tidepool_harness::log::{Event, LogHeader, LogWriter};
use tidepool_harness::provider::{Role, Usage};
use tidepool_harness::tree::NodeId;

fn extract_available() -> bool {
    std::env::var("TIDEPOOL_EXTRACT").is_ok()
        || std::process::Command::new("tidepool-extract")
            .arg("--help")
            .output()
            .is_ok()
}

fn extract_bin() -> String {
    std::env::var("TIDEPOOL_EXTRACT").unwrap_or_else(|_| "tidepool-extract".to_string())
}

fn unique_dir(label: &str) -> std::path::PathBuf {
    let ts = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let dir = std::env::temp_dir().join(format!(
        "tidepool-kill9-{label}-{}-{ts}",
        std::process::id()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn free_port() -> u16 {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind ephemeral port");
    listener.local_addr().unwrap().port()
}

/// One request/response over a fresh TCP connection with `Connection: close`,
/// so reading to EOF is a complete response — no chunked-transfer decoder
/// needed. Mirrors `tidepool-harness/tests/provider_behavior.rs`'s own
/// from-scratch HTTP handling (that one's a server; this is the client half).
fn http(addr: &str, method: &str, path: &str, json_body: Option<&serde_json::Value>) -> (u16, String) {
    let mut stream = TcpStream::connect(addr).unwrap_or_else(|e| panic!("connect {addr}: {e}"));
    stream.set_read_timeout(Some(Duration::from_secs(60))).unwrap();
    let body = json_body.map(|v| v.to_string()).unwrap_or_default();
    let mut req = format!("{method} {path} HTTP/1.1\r\nHost: {addr}\r\nConnection: close\r\n");
    if !body.is_empty() {
        req.push_str("Content-Type: application/json\r\n");
        req.push_str(&format!("Content-Length: {}\r\n", body.len()));
    }
    req.push_str("\r\n");
    req.push_str(&body);
    stream.write_all(req.as_bytes()).unwrap();
    let mut raw = Vec::new();
    stream.read_to_end(&mut raw).unwrap();
    let text = String::from_utf8_lossy(&raw).to_string();
    let mut parts = text.splitn(2, "\r\n\r\n");
    let head = parts.next().unwrap_or("");
    let resp_body = parts.next().unwrap_or("").to_string();
    let status = head
        .lines()
        .next()
        .and_then(|l| l.split_whitespace().nth(1))
        .and_then(|s| s.parse().ok())
        .unwrap_or(0);
    (status, resp_body)
}

fn wait_for_http_ready(addr: &str, timeout: Duration) {
    let start = Instant::now();
    loop {
        if TcpStream::connect(addr).is_ok() {
            return;
        }
        assert!(
            start.elapsed() <= timeout,
            "server at {addr} never became reachable within {timeout:?}"
        );
        std::thread::sleep(Duration::from_millis(100));
    }
}

/// Poll `GET /` until the response body satisfies `pred`, or panic on timeout
/// (dumping the last body seen, for diagnosis).
fn wait_for_page(addr: &str, timeout: Duration, pred: impl Fn(&str) -> bool) -> String {
    let start = Instant::now();
    loop {
        let (status, body) = http(addr, "GET", "/", None);
        if status == 200 && pred(&body) {
            return body;
        }
        assert!(
            start.elapsed() <= timeout,
            "condition not met within {timeout:?}; last body:\n{body}"
        );
        std::thread::sleep(Duration::from_millis(300));
    }
}

/// Drain `stderr` into a shared line buffer on a background thread — avoids
/// blocking the child on a full pipe and lets the test grep for the
/// `[boot] run log: <path>` line the binary prints at startup.
fn drain_stderr(child: &mut std::process::Child) -> Arc<Mutex<Vec<String>>> {
    let lines = Arc::new(Mutex::new(Vec::new()));
    let stderr = child.stderr.take().expect("piped stderr");
    let lines_bg = lines.clone();
    std::thread::spawn(move || {
        let reader = BufReader::new(stderr);
        for line in reader.lines().map_while(Result::ok) {
            eprintln!("[child] {line}");
            lines_bg.lock().unwrap().push(line);
        }
    });
    lines
}

fn find_run_log(lines: &Arc<Mutex<Vec<String>>>, timeout: Duration) -> std::path::PathBuf {
    let start = Instant::now();
    loop {
        if let Some(l) = lines
            .lock()
            .unwrap()
            .iter()
            .find(|l| l.contains("[boot] run log: "))
        {
            let path = l
                .split("[boot] run log: ")
                .nth(1)
                .expect("prefix matched")
                .trim()
                .to_string();
            return std::path::PathBuf::from(path);
        }
        assert!(
            start.elapsed() <= timeout,
            "never saw a '[boot] run log:' line from the child within {timeout:?}"
        );
        std::thread::sleep(Duration::from_millis(50));
    }
}

fn kill9(pid: u32) {
    let status = Command::new("kill")
        .arg("-9")
        .arg(pid.to_string())
        .status()
        .expect("spawn kill -9");
    assert!(status.success(), "kill -9 {pid} failed");
}

const TITLE: &str = "kill9 root";
const PROMPT: &str = "Confirm, finish.";

/// Write a minimal "prior recording" log: the ONE assistant turn a real live
/// run would have produced before suspending on the dialog hole. Only
/// `TurnDelta{role: Assistant}` events are replayable (`ReplayProvider::
/// from_log`'s contract) — that's the whole of what a from-scratch
/// hand-authored source log needs to carry; no live model call ever happens.
fn write_source_log(path: &std::path::Path) {
    let header = LogHeader {
        prelude_hash: "kill9-source".into(),
        extract_fingerprint: "kill9-source".into(),
        harness_version: "test".into(),
    };
    let mut w = LogWriter::create(path, &header).unwrap();
    w.append(Event::TurnDelta {
        node: NodeId(0),
        turn: 0,
        role: Role::Assistant,
        content: "```haskell\ndo\n  _ <- dialogAsk (toJSON (card \"Confirm\" [choice \"Proceed?\" \
                  [(\"yes\", \"Yes\"), (\"no\", \"No\")]]))\n  pure (toJSON (1 :: Int))\n```"
            .to_string(),
        usage: Some(Usage {
            input_tokens: 50,
            output_tokens: 10,
        }),
    })
    .unwrap();
}

struct Spawned {
    child: std::process::Child,
    addr: String,
    stderr_lines: Arc<Mutex<Vec<String>>>,
}

fn spawn_harness(
    replay_log: &std::path::Path,
    port: u16,
    state_home: &std::path::Path,
    config_home: &std::path::Path,
) -> Spawned {
    let bin = env!("CARGO_BIN_EXE_tidepool-harness");
    let mut child = Command::new(bin)
        .arg("--port")
        .arg(port.to_string())
        .arg("--replay")
        .arg(replay_log)
        .env("TIDEPOOL_EXTRACT", extract_bin())
        .env("XDG_STATE_HOME", state_home)
        .env("TIDEPOOL_CONFIG_DIR", config_home)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn tidepool-harness binary");
    let stderr_lines = drain_stderr(&mut child);
    // Drain stdout too (unused, but avoid a full-pipe stall).
    if let Some(stdout) = child.stdout.take() {
        std::thread::spawn(move || {
            let reader = BufReader::new(stdout);
            for _line in reader.lines().map_while(Result::ok) {}
        });
    }
    let addr = format!("127.0.0.1:{port}");
    wait_for_http_ready(&addr, Duration::from_secs(120));
    Spawned {
        child,
        addr,
        stderr_lines,
    }
}

/// Kill -9 the real `tidepool-harness` binary while its one node is suspended
/// on an operator (dialog) hole, restart a FRESH process with `--replay <its
/// own log>`, re-drive the SAME deterministic turn sequence via HTTP
/// (`/create` + `/force`) to reconstruct the identical suspension, then
/// answer it via the mechanical `/answer/:node/:key` verb (D6, zero model
/// turns — the replay queue being exhausted after the one recorded turn does
/// not block completion).
#[test]
fn kill9_restart_reconstructs_tree_via_replay_and_hole_is_answerable() {
    if !extract_available() {
        eprintln!(
            "Skipping: tidepool-extract not available (set TIDEPOOL_EXTRACT, run in nix develop)"
        );
        return;
    }

    let scratch = unique_dir("scratch");
    let source_log = scratch.join("source.jsonl");
    write_source_log(&source_log);

    // ---- run 1: boot, drive to the dialog hole, kill -9 ---------------------
    let state1 = unique_dir("state1");
    let config1 = unique_dir("config1");
    let port1 = free_port();
    let mut run1 = spawn_harness(&source_log, port1, &state1, &config1);

    let (status, body) = http(
        &run1.addr,
        "POST",
        "/create",
        Some(&json!({"title": TITLE, "prompt": PROMPT})),
    );
    assert_eq!(status, 200, "create failed: {body}");

    let (status, body) = http(&run1.addr, "POST", "/force/0", None);
    assert_eq!(status, 200, "force failed: {body}");

    // Wait for the node to reach the suspended dialog hole (the extract
    // compile behind this takes several seconds).
    let page = wait_for_page(&run1.addr, Duration::from_secs(120), |b| {
        b.contains("class=\"chip state-suspended\"")
    });
    assert!(
        page.contains("class=\"chip state-suspended\""),
        "expected a suspended node before the kill, got:\n{page}"
    );

    let run1_log = find_run_log(&run1.stderr_lines, Duration::from_secs(5));
    assert!(
        run1_log.exists(),
        "run1's own log file must exist on disk: {}",
        run1_log.display()
    );

    let pid = run1.child.id();
    kill9(pid);
    // Reap the killed process so it doesn't linger as a zombie.
    let _ = run1.child.wait();

    // ---- run 2: restart from run1's OWN log, re-derive the same state -------
    let state2 = unique_dir("state2");
    let config2 = unique_dir("config2");
    let port2 = free_port();
    let mut run2 = spawn_harness(&run1_log, port2, &state2, &config2);

    let (status, body) = http(
        &run2.addr,
        "POST",
        "/create",
        Some(&json!({"title": TITLE, "prompt": PROMPT})),
    );
    assert_eq!(status, 200, "re-create failed: {body}");

    let (status, body) = http(&run2.addr, "POST", "/force/0", None);
    assert_eq!(status, 200, "re-force failed: {body}");

    // The SAME recorded turn re-drives the SAME suspension — "tree
    // reconstructed" via deterministic replay, through the real binary.
    let page = wait_for_page(&run2.addr, Duration::from_secs(120), |b| {
        b.contains("class=\"chip state-suspended\"")
    });
    assert!(
        page.contains("class=\"chip state-suspended\""),
        "the restarted process must re-derive the SAME suspended hole via replay, got:\n{page}"
    );

    // The hole is answerable post-restart.
    let (status, body) = http(&run2.addr, "POST", "/answer/0/yes", None);
    assert_eq!(status, 200, "answer failed: {body}");

    let page = wait_for_page(&run2.addr, Duration::from_secs(30), |b| {
        b.contains("class=\"chip state-done\"")
    });
    assert!(
        page.contains("class=\"chip state-done\"") && !page.contains("class=\"chip state-suspended\""),
        "the node must complete once the reconstructed hole is answered, got:\n{page}"
    );

    kill9(run2.child.id());
    let _ = run2.child.wait();
}
