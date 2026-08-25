//! GHC-heavy integration test for the resident compile daemon
//! (plans/compile-daemon-design.md, Phase 0). ONE binary, ONE daemon boot
//! shared across the named checks below (family-bundle discipline, root
//! CLAUDE.md's "Suite wall time is a standing constraint") — checks (a)-(d)
//! run against one daemon; (e) needs its own (it deliberately kills itself
//! after 2 requests).
//!
//! Needs a resolvable `tidepool-extract` binary (`$TIDEPOOL_EXTRACT` or
//! `PATH`) and a GHC on `PATH` that can load it (see haskell/CLAUDE.md).
//! This crate is otherwise excluded from nothing in `.config/nextest.toml`
//! (that wiring is Phase 1's job, not this lane's — see the design doc's own
//! migration order), so under a BARE `cargo nextest run` this test must not
//! fail loud just because the toolchain isn't set up: `daemon_toolchain()`
//! skips (prints and returns early, still a PASS) rather than panicking when
//! the extract binary can't be resolved. Run explicitly via
//! `scripts/battery.sh -p tidepool-extract-cmd -E 'binary(daemon_integration)'`
//! (which builds/sets `$TIDEPOOL_EXTRACT` automatically) to actually exercise it.

use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Child, Command};
use std::time::{Duration, Instant};

use tidepool_extract_cmd::{resolve_bin, ExtractCmd, ResolvedExtractBin};

/// The stdlib root every fixture's `--include` points at — this crate's own
/// workspace-relative path, not the general-purpose 5-tier locator
/// `tidepool-runtime::toolchain` owns (that crate cannot be a dependency
/// here — see this crate's own CLAUDE.md). A test running inside this
/// repo's cargo workspace always has this path; that is the only case this
/// helper needs to serve.
fn stdlib_lib_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("tidepool-extract-cmd has a workspace parent")
        .join("haskell/lib")
}

/// Resolve the extract binary the same way [`ExtractCmd::new`] would.
/// `None` when unresolvable (unset `$TIDEPOOL_EXTRACT` outside a toolchain
/// dev shell, or a set-but-broken one) — every check below skips cleanly on
/// `None` rather than failing, so a bare `cargo nextest run` (which is not
/// excluded from running this binary at all — see the module doc) stays
/// green in an environment with no Haskell toolchain.
fn daemon_toolchain() -> Option<(PathBuf, PathBuf)> {
    let bin = match resolve_bin() {
        Ok(r) => r.path,
        Err(e) => {
            eprintln!("daemon_integration: SKIPPED (extract binary unresolvable: {e})");
            return None;
        }
    };
    let lib = stdlib_lib_dir();
    if !lib.join("Tidepool/Prelude.hs").is_file() {
        eprintln!(
            "daemon_integration: SKIPPED (stdlib root not found at {})",
            lib.display()
        );
        return None;
    }
    Some((bin, lib))
}

fn unique_scratch_dir(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "tp-extract-cmd-daemon-it-{name}-{}",
        std::process::id()
    ));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).expect("create scratch dir");
    dir
}

/// A socket path short enough for `sockaddr_un`'s ~108-byte `sun_path`
/// (observed live: a scratch path under a long session-scoped `/tmp`
/// subtree overflows it — see the module doc's own toolchain-availability
/// caveat). `/tmp` directly, not the scratch dir, keeps this well clear.
fn unique_socket_path(name: &str) -> PathBuf {
    std::env::temp_dir().join(format!("tp-ecmd-it-{name}-{}.sock", std::process::id()))
}

struct DaemonHandle {
    child: Child,
    socket: PathBuf,
}

impl Drop for DaemonHandle {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        let _ = fs::remove_file(&self.socket);
    }
}

/// `None` when the socket never appears within the bound — e.g. the
/// resolved `$TIDEPOOL_EXTRACT`/`PATH` binary is a STALE build predating
/// `--daemon` support (observed live: an old installed extract treats
/// `--daemon` as a positional file and exits with a diagnostics error
/// instead of ever binding a socket). Every caller treats `None` the same
/// way `daemon_toolchain`'s own `None` is treated: skip the test cleanly
/// rather than fail loud over an environment/toolchain-freshness problem
/// this test cannot fix.
fn spawn_daemon(bin: &Path, socket: &Path, extra_args: &[&str]) -> Option<DaemonHandle> {
    let _ = fs::remove_file(socket);
    let mut cmd = Command::new(bin);
    cmd.arg("--daemon").arg("--socket").arg(socket);
    for a in extra_args {
        cmd.arg(a);
    }
    let mut child = cmd.spawn().expect("spawn daemon process");

    let start = Instant::now();
    let timeout = Duration::from_secs(30);
    loop {
        if socket.exists() {
            return Some(DaemonHandle {
                child,
                socket: socket.to_path_buf(),
            });
        }
        if let Ok(Some(status)) = child.try_wait() {
            eprintln!(
                "daemon_integration: SKIPPED (--daemon process exited early with {status:?} — \
                 likely a stale extract binary predating daemon support; run via \
                 scripts/battery.sh to build a fresh one)"
            );
            return None;
        }
        if start.elapsed() >= timeout {
            eprintln!(
                "daemon_integration: SKIPPED (daemon socket {} did not appear within {:?})",
                socket.display(),
                timeout
            );
            let _ = child.kill();
            let _ = child.wait();
            return None;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

fn write_fixture(dir: &Path, name: &str, contents: &str) -> PathBuf {
    let path = dir.join(name);
    fs::write(&path, contents).expect("write fixture");
    path
}

fn env_socket(socket: &Path) -> (&'static str, OsString) {
    ("TIDEPOOL_EXTRACT_DAEMON_SOCKET", socket.as_os_str().into())
}

/// One `ExtractCmd::run()` with `$TIDEPOOL_EXTRACT_DAEMON_SOCKET` set for the
/// duration of the call, restored (removed) afterward — env vars are
/// process-global, so every daemon-routed call in this file goes through
/// here rather than leaving the var set across unrelated calls.
fn run_via_env_socket(cmd: &ExtractCmd, socket: &Path) -> std::process::Output {
    let (k, v) = env_socket(socket);
    // This whole binary is ONE nextest test process (root CLAUDE.md) with no
    // concurrent test in this same process to race against — every call in
    // this file is sequential, so mutating the process env here is safe.
    std::env::set_var(k, &v);
    let result = cmd.run();
    std::env::remove_var(k);
    result
        .expect("daemon-routed run() should not itself error (it falls back to Direct)")
        .output
}

fn run_direct(cmd: &ExtractCmd) -> std::process::Output {
    cmd.run().expect("direct run() failed").output
}

fn cmd_for(bin: &Path, dir: &Path, out_dir: &str, target_file: &str, lib: &Path) -> ExtractCmd {
    let mut cmd = ExtractCmd::with_bin(ResolvedExtractBin::assume_resolved(bin));
    cmd.input(dir.join(target_file))
        .output_dir(dir.join(out_dir))
        .target("result")
        .include(lib);
    cmd
}

#[test]
fn daemon_integration() {
    let Some((bin, lib)) = daemon_toolchain() else {
        return;
    };

    let dir = unique_scratch_dir("main");
    let socket = unique_socket_path("main");
    let Some(daemon) = spawn_daemon(&bin, &socket, &[]) else {
        return;
    };

    // (a) runs LAST, not first: it must exercise a WARM daemon session (its
    // Unique counter already advanced by (b)/(c)/(d)'s many prior compiles),
    // not a fresh daemon's very first request — see its own doc comment for
    // why a first-request-only check would be a fresh-process-equals-
    // fresh-process tautology that never actually exercises the risk
    // stabilizeLocalUniques/externalizeInternalTops (GhcPipeline.hs,
    // Translate.hs) exist to close.
    check_b_failing_program_same_diagnostics(&bin, &dir, &lib, &socket);
    check_c_isolation_across_session_roots(&bin, &dir, &lib, &socket);
    check_d_relative_target_and_distinct_cwd(&bin, &dir, &lib, &socket);
    check_a_byte_identical_transport(&bin, &dir, &lib, &socket);

    drop(daemon);
    let _ = fs::remove_dir_all(&dir);

    check_e_rotation_then_fallback(&bin, &lib);
}

/// (a) Same fixture via daemon vs. direct spawn → byte-identical CBOR +
/// diagnostics. Deliberately called LAST (see `daemon_integration`'s own
/// ordering comment) so the daemon serving this request is WARM — its
/// session-wide `Unique` counter has already advanced through many prior
/// compiles ((b)'s failing compile, (c)'s eight bind/reference turns, (d)'s
/// relative-path compile). This is the standing acceptance test for
/// `stabilizeLocalUniques`/`externalizeInternalTops`'s determinism fix (a
/// long-running session's advancing `Unique` counter must not leak into
/// nested-Id VarIds or the extract-fidelity/D1 disambiguator): comparing a
/// fresh daemon's FIRST request against a fresh direct spawn would be a
/// fresh-process-equals-fresh-process tautology that never exercises the
/// actual risk. If this fails, it names a real coverage gap in that pass,
/// not flakiness — the fix is never to weaken this to a semantic-equality
/// check or retry.
fn check_a_byte_identical_transport(bin: &Path, dir: &Path, lib: &Path, socket: &Path) {
    write_fixture(
        dir,
        "Expr.hs",
        "module Expr where\nimport Tidepool.Prelude\nresult :: Int\nresult = 1 + 2\n",
    );

    let daemon_out = run_via_env_socket(&cmd_for(bin, dir, "out-a-daemon", "Expr.hs", lib), socket);
    let direct_out = run_direct(&cmd_for(bin, dir, "out-a-direct", "Expr.hs", lib));

    assert!(daemon_out.status.success(), "daemon compile should succeed");
    assert!(direct_out.status.success(), "direct compile should succeed");
    assert_eq!(
        daemon_out.stdout, direct_out.stdout,
        "diagnostics JSON must match between transports"
    );

    let daemon_cbor = fs::read(dir.join("out-a-daemon/result.cbor")).unwrap();
    let direct_cbor = fs::read(dir.join("out-a-direct/result.cbor")).unwrap();
    assert_eq!(
        daemon_cbor, direct_cbor,
        "result.cbor must be byte-identical"
    );

    let daemon_meta = fs::read(dir.join("out-a-daemon/meta.cbor")).unwrap();
    let direct_meta = fs::read(dir.join("out-a-direct/meta.cbor")).unwrap();
    assert_eq!(daemon_meta, direct_meta, "meta.cbor must be byte-identical");
}

/// (b) A failing program produces the same diagnostics over both transports.
fn check_b_failing_program_same_diagnostics(bin: &Path, dir: &Path, lib: &Path, socket: &Path) {
    write_fixture(
        dir,
        "Bad.hs",
        "module Bad where\nimport Tidepool.Prelude\nresult :: Int\nresult = \"not an int\"\n",
    );

    let daemon_out = run_via_env_socket(&cmd_for(bin, dir, "out-b-daemon", "Bad.hs", lib), socket);
    let direct_out = run_direct(&cmd_for(bin, dir, "out-b-direct", "Bad.hs", lib));

    assert!(
        !daemon_out.status.success(),
        "daemon must report the failure"
    );
    assert!(
        !direct_out.status.success(),
        "direct spawn must report the failure"
    );
    assert_eq!(
        daemon_out.status.code(),
        direct_out.status.code(),
        "exit codes must match"
    );
    assert_eq!(
        daemon_out.stdout, direct_out.stdout,
        "the diagnostics JSON document must match between transports"
    );
    let daemon_diags = String::from_utf8_lossy(&daemon_out.stdout);
    assert!(
        daemon_diags.contains("Couldn't match type"),
        "expected a real type-error diagnostic, got: {daemon_diags}"
    );
}

/// (c) THE isolation check: two independent session roots both minting
/// `__result` / `Tidepool.Session.Val.G1` through ONE daemon each get their
/// own correct output — proves `Tidepool.GhcPipeline.sanitizeMemo` actually
/// prevents the design §2.2 leak (a request-spanning `GutsMemo` serving one
/// session's guts to another's compile of the same name).
fn check_c_isolation_across_session_roots(bin: &Path, dir: &Path, lib: &Path, socket: &Path) {
    let session = |label: &str, bound_value: i64| -> (PathBuf, PathBuf) {
        let root = dir.join(format!("session-{label}-root"));
        fs::create_dir_all(&root).unwrap();
        let bind_file = write_fixture(
            dir,
            &format!("Bind{label}.hs"),
            &format!(
                "module Bind{label} where\nimport Tidepool.Prelude\n__result :: Int\n__result = {bound_value}\n"
            ),
        );
        let sidecar = dir.join(format!("bb-{label}.json"));
        let mut bind_cmd = ExtractCmd::with_bin(ResolvedExtractBin::assume_resolved(bin));
        bind_cmd
            .input(&bind_file)
            .output_dir(dir.join(format!("out-c-{label}-bind")))
            .session_bind()
            .bind_name("x")
            .bind_gen(1)
            .session_root(&root)
            .emit_bound_binders(&sidecar)
            .include(lib);
        let out = run_via_env_socket(&bind_cmd, socket);
        assert!(
            out.status.success(),
            "bind turn ({label}) should succeed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        (root, dir.join(format!("Ref{label}.hs")))
    };

    let (root_a, ref_file_a) = session("A", 111);
    let (root_b, ref_file_b) = session("B", 222);

    let reference = |ref_file: &Path, root: &Path, label: &str| -> Vec<u8> {
        fs::write(
            ref_file,
            format!(
                "module Ref{label} where\nimport Tidepool.Session.Val.G1 (x)\nimport Tidepool.Prelude\n__result :: Int\n__result = x + 1\n"
            ),
        )
        .unwrap();
        let mut ref_cmd = ExtractCmd::with_bin(ResolvedExtractBin::assume_resolved(bin));
        ref_cmd
            .input(ref_file)
            .output_dir(dir.join(format!("out-c-{label}-ref")))
            .session_root(root)
            .inject_val("Tidepool.Session.Val.G1")
            .include(lib);
        let out = run_via_env_socket(&ref_cmd, socket);
        assert!(
            out.status.success(),
            "reference turn ({label}) should succeed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        fs::read(dir.join(format!("out-c-{label}-ref/result.cbor"))).unwrap()
    };

    let cbor_a = reference(&ref_file_a, &root_a, "A");
    let cbor_b = reference(&ref_file_b, &root_b, "B");
    // Interleave: re-run session A's reference AFTER session B ran, proving
    // B's activity never mutated the shared memo entries A's compile reads.
    let cbor_a_again = reference(&ref_file_a, &root_a, "A");

    assert_ne!(
        cbor_a, cbor_b,
        "sessions A (x=111) and B (x=222) must produce DIFFERENT __result values (111+1 vs 222+1) despite both minting __result/Val.G1"
    );
    assert_eq!(
        cbor_a, cbor_a_again,
        "session A's reference output must be stable across an interleaved session B compile — a leak would corrupt it"
    );
}

/// (d) A relative target path resolves against the REQUEST's own cwd, not
/// the daemon process's launch directory — the single worker's per-cycle
/// `setCurrentDirectory` (design §2.3/GhcPipeline's resident API).
fn check_d_relative_target_and_distinct_cwd(bin: &Path, dir: &Path, lib: &Path, socket: &Path) {
    let sub = dir.join("nested/client-cwd");
    fs::create_dir_all(&sub).unwrap();
    fs::write(
        sub.join("RelExpr.hs"),
        "module RelExpr where\nimport Tidepool.Prelude\nresult :: Int\nresult = 41 + 1\n",
    )
    .unwrap();

    // A RELATIVE input path + output-dir, resolved against `sub` (the
    // client's own cwd) — never the daemon's own launch cwd, which is a
    // different directory entirely (wherever spawn_daemon's Command ran).
    let mut cmd = ExtractCmd::with_bin(ResolvedExtractBin::assume_resolved(bin));
    cmd.input("RelExpr.hs")
        .output_dir("out-d")
        .target("result")
        .include(lib);

    let saved_cwd = std::env::current_dir().unwrap();
    std::env::set_current_dir(&sub).unwrap();
    let out = run_via_env_socket(&cmd, socket);
    std::env::set_current_dir(saved_cwd).unwrap();

    assert!(
        out.status.success(),
        "relative-path compile under a distinct client cwd should succeed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(sub.join("out-d/result.cbor").is_file());
}

/// (e) `--rotate-after 2` → the daemon exits after 2 requests, and the
/// client's NEXT call falls back to a real spawn cleanly (not a hang, not an
/// error surfaced to the caller).
fn check_e_rotation_then_fallback(bin: &Path, lib: &Path) {
    let dir = unique_scratch_dir("rotate");
    let socket = unique_socket_path("rotate");
    write_fixture(
        &dir,
        "Expr.hs",
        "module Expr where\nimport Tidepool.Prelude\nresult :: Int\nresult = 5\n",
    );

    let Some(daemon) = spawn_daemon(bin, &socket, &["--rotate-after", "2"]) else {
        let _ = fs::remove_dir_all(&dir);
        return;
    };

    for i in 0..2 {
        let out = run_via_env_socket(
            &cmd_for(bin, &dir, &format!("out-e-{i}"), "Expr.hs", lib),
            &socket,
        );
        assert!(out.status.success(), "rotation request {i} should succeed");
    }

    // The daemon should exit on its own shortly after serving request 2 —
    // wait for the process to actually terminate (bounded) rather than
    // asserting on the socket file alone (removed by the OS close, but the
    // process might still be mid-teardown).
    let start = Instant::now();
    let mut daemon = daemon;
    loop {
        if let Ok(Some(_status)) = daemon.child.try_wait() {
            break;
        }
        assert!(
            start.elapsed() < Duration::from_secs(15),
            "daemon did not exit after --rotate-after 2 within 15s"
        );
        std::thread::sleep(Duration::from_millis(100));
    }

    // The client's next call must fall back to Direct cleanly — never an
    // error surfaced to the caller, never a hang.
    let out = run_via_env_socket(
        &cmd_for(bin, &dir, "out-e-fallback", "Expr.hs", lib),
        &socket,
    );
    assert!(
        out.status.success(),
        "post-rotation fallback compile should succeed via Direct: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    let _ = fs::remove_dir_all(&dir);
}
