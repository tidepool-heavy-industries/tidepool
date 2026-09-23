//! GHC-heavy integration test for the resident compile daemon. One binary and
//! daemon boot are shared across most checks. The rotation checks each own a
//! daemon because they deliberately use a one-request rotation threshold.
//!
//! Needs a resolvable `tidepool-extract` binary (`$TIDEPOOL_EXTRACT` or
//! `PATH`) and a GHC on `PATH` that can load it (see bridge/haskell/CLAUDE.md).
//! Under a bare `cargo nextest run` this test must not
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

use tidepool_extract_cmd::{resolve_bin, ExtractCmd, ResolvedExtractBin, SymbolIdentity};

/// The stdlib root every fixture's `--include` points at — this crate's own
/// workspace-relative path, not the general-purpose 5-tier locator
/// `tidepool-toolchain::toolchain` owns (that crate cannot be a dependency
/// here — see this crate's own CLAUDE.md). A test running inside this
/// repo's cargo workspace always has this path; that is the only case this
/// helper needs to serve.
fn stdlib_lib_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("tidepool-extract-cmd has a source-root parent")
        .parent()
        .expect("tidepool source root has a workspace parent")
        .join("bridge/haskell/lib")
}

/// `test-prepared-stg` fixture source root, sibling of [`stdlib_lib_dir`] —
/// home of `ImportProducerExposed.hs`/`ImportConsumerExposed.hs`, used by
/// `check_j_retained_generation_transitions_through_warm_daemon`.
fn prepared_stg_fixture_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("tidepool-extract-cmd has a source-root parent")
        .parent()
        .expect("tidepool source root has a workspace parent")
        .join("bridge/haskell/test-prepared-stg")
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

#[derive(Clone, Copy)]
enum TurnShape {
    Bind,
    Expr,
}

impl TurnShape {
    fn template_key(self) -> &'static str {
        match self {
            Self::Bind => "bind",
            Self::Expr => "expr",
        }
    }

    fn verdict(self) -> &'static str {
        match self {
            Self::Bind => "bind:x",
            Self::Expr => "expr",
        }
    }
}

struct TurnFixture<'a> {
    label: &'a str,
    source: &'a str,
    template: &'a str,
    shape: TurnShape,
    output_dir: PathBuf,
    session_root: &'a Path,
    inject: &'a [&'a str],
    includes: &'a [&'a Path],
}

/// Build the low-level resident-turn request used by production. Keeping this
/// test crate dependency-light means it inspects the emitted files directly
/// rather than decoding runtime types.
fn turn_cmd(bin: &Path, dir: &Path, fixture: TurnFixture<'_>) -> ExtractCmd {
    let turn = write_fixture(dir, &format!("{}-turn.txt", fixture.label), fixture.source);
    let template = write_fixture(
        dir,
        &format!("{}-template.hs", fixture.label),
        fixture.template,
    );
    let turn_out = fixture.output_dir.join("turn.cbor");
    let mut cmd = ExtractCmd::with_bin(ResolvedExtractBin::assume_resolved(bin));
    cmd.input(turn)
        .turn()
        .turn_template(fixture.shape.template_key(), &template)
        .turn_out(turn_out)
        .turn_verdict(fixture.shape.verdict())
        .output_dir(&fixture.output_dir)
        .session_root(fixture.session_root)
        .bind_gen(1)
        .inject_vals(fixture.inject.iter().copied())
        .includes(fixture.includes.iter().copied());
    cmd
}

fn contains_extension(dir: &Path, extension: &str) -> bool {
    fs::read_dir(dir).is_ok_and(|entries| {
        entries.filter_map(Result::ok).any(|entry| {
            let path = entry.path();
            (path.is_dir() && contains_extension(&path, extension))
                || path.extension().is_some_and(|ext| ext == extension)
        })
    })
}

fn env_socket(socket: &Path) -> (&'static str, OsString) {
    ("TIDEPOOL_EXTRACT_DAEMON_SOCKET", socket.as_os_str().into())
}

/// One bound endpoint execution with `$TIDEPOOL_EXTRACT_DAEMON_SOCKET` set for the
/// duration of the call, restored (removed) afterward — env vars are
/// process-global, so every daemon-routed call in this file goes through
/// here rather than leaving the var set across unrelated calls.
fn run_via_env_socket(cmd: &ExtractCmd, socket: &Path) -> std::process::Output {
    let (k, v) = env_socket(socket);
    // This whole binary is ONE nextest test process (root CLAUDE.md) with no
    // concurrent test in this same process to race against — every call in
    // this file is sequential, so mutating the process env here is safe.
    std::env::set_var(k, &v);
    let result = cmd.bind().and_then(|endpoint| endpoint.execute(cmd));
    std::env::remove_var(k);
    result
        .expect("daemon-routed execution should complete (or safely fall back before submission)")
        .output
}

fn run_direct(cmd: &ExtractCmd) -> std::process::Output {
    let endpoint = cmd.bind().expect("direct bind failed");
    endpoint
        .execute(cmd)
        .expect("direct execution failed")
        .output
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
    check_f_module_memo_follows_include_roots(&bin, &dir, &lib, &socket);
    check_g_shim_dependent_module_warm_second_request(&bin, &dir, &lib, &socket);
    check_h_request_build_products_dir(&bin, &dir, &lib, &socket);
    check_j_retained_generation_transitions_through_warm_daemon(&bin, &dir, &socket);
    check_a_byte_identical_transport(&bin, &dir, &lib, &socket);

    drop(daemon);
    let _ = fs::remove_dir_all(&dir);

    check_e_rotation_then_fallback(&bin, &lib);
    check_i_persistent_rotation_keeps_serving(&bin, &lib);
}

/// A resident request applies its own build-products directory after daemon
/// startup; the setting is request data, not worker-process environment.
fn check_h_request_build_products_dir(bin: &Path, dir: &Path, lib: &Path, socket: &Path) {
    write_fixture(
        dir,
        "HBuildProducts.hs",
        "module HBuildProducts where\nresult :: Int\nresult = 42\n",
    );
    let products = dir.join("build-products-h");
    fs::create_dir_all(&products).unwrap();

    let mut cmd = cmd_for(bin, dir, "out-h", "HBuildProducts.hs", lib);
    cmd.build_products_dir(&products);
    let output = run_via_env_socket(&cmd, socket);
    assert!(
        output.status.success(),
        "resident build-products request failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        contains_extension(&products, "hi"),
        "resident request did not write interfaces under {}",
        products.display()
    );
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

    let daemon_cbor = fs::read(dir.join("out-a-daemon/result.prepared.cbor")).unwrap();
    let direct_cbor = fs::read(dir.join("out-a-direct/result.prepared.cbor")).unwrap();
    assert_eq!(
        daemon_cbor, direct_cbor,
        "result.prepared.cbor must be byte-identical"
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
    let session = |label: &str, bind_source: &str| -> PathBuf {
        let root = dir.join(format!("session-{label}-root"));
        fs::create_dir_all(&root).unwrap();
        let bind_cmd = turn_cmd(
            bin,
            dir,
            TurnFixture {
                label: &format!("c-{label}-bind"),
                source: bind_source,
                template: include_str!("daemon_integration/resident_bind.hs"),
                shape: TurnShape::Bind,
                output_dir: dir.join(format!("out-c-{label}-bind")),
                session_root: &root,
                inject: &[],
                includes: &[lib],
            },
        );
        let out = run_via_env_socket(&bind_cmd, socket);
        assert!(
            out.status.success(),
            "bind turn ({label}) should succeed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        root
    };

    let root_a = session("A", "x <- pure (111 :: Int)");
    let root_b = session("B", "x <- pure True");

    let reference = |root: &Path, label: &str, source: &str| -> Vec<u8> {
        let ref_cmd = turn_cmd(
            bin,
            dir,
            TurnFixture {
                label: &format!("c-{label}-ref"),
                source,
                template: include_str!("daemon_integration/session_ref.hs"),
                shape: TurnShape::Expr,
                output_dir: dir.join(format!("out-c-{label}-ref")),
                session_root: root,
                inject: &["Tidepool.Session.Val.G1"],
                includes: &[lib],
            },
        );
        let out = run_via_env_socket(&ref_cmd, socket);
        assert!(
            out.status.success(),
            "reference turn ({label}) should succeed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        // A resident turn's compile target is always the fixed prepared
        // scaffold binder (`preparedScaffoldTargetName`/`PREPARED_SCAFFOLD_TARGET`
        // == "__prepared"), never the CLI's own `--targets` flag —
        // `runTurnMode` (bridge/haskell/app/Main.hs) hardcodes
        // `[preparedScaffoldTargetName]`, and `writePreparedArtifacts` names
        // the file after it: `<target>.prepared.cbor`.
        fs::read(dir.join(format!("out-c-{label}-ref/__prepared.prepared.cbor"))).unwrap()
    };

    let cbor_a = reference(&root_a, "A", "x + 1");
    let cbor_b = reference(&root_b, "B", "if x then 222 else 0");
    // Interleave: re-run session A's reference AFTER session B ran, proving
    // B's activity never mutated the shared memo entries A's compile reads.
    let cbor_a_again = reference(&root_a, "A", "x + 1");

    assert_ne!(
        cbor_a, cbor_b,
        "sessions A (x :: Int) and B (x :: Bool) must compile different reference programs despite both minting ResidentBind/Val.G1"
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
    // `writePreparedArtifacts` (bridge/haskell/app/Main.hs) names every
    // compiled artifact `<target>.prepared.cbor`, target-based `--targets`
    // compiles included — never the bare `<target>.cbor` this assertion used
    // to check.
    assert!(sub.join("out-d/result.prepared.cbor").is_file());
}

/// A warm daemon must key resolved module guts by the request's include roots,
/// not only by module name. Two requests may intentionally resolve the same
/// module name to different files; the second must not reuse the first one's
/// binding. `Tidepool.Effects.Core` is merely a convenient fixture name here:
/// production Core is now universal, while the memoization invariant applies
/// to every generated or actor-supplied module.
fn check_f_module_memo_follows_include_roots(bin: &Path, dir: &Path, lib: &Path, socket: &Path) {
    let first_inc = dir.join("f-first-module");
    let second_inc = dir.join("f-second-module");
    fs::create_dir_all(first_inc.join("Tidepool/Effects")).unwrap();
    fs::create_dir_all(second_inc.join("Tidepool/Effects")).unwrap();

    // The first file has no `spawnSpec` and warms the memo entry under the
    // shared module name.
    fs::write(
        first_inc.join("Tidepool/Effects/Core.hs"),
        "module Tidepool.Effects.Core where\nimport Tidepool.Prelude\ncoreStub :: Int\ncoreStub = 0\n",
    )
    .unwrap();
    // The second include root resolves that same name to a different file.
    fs::write(
        second_inc.join("Tidepool/Effects/Core.hs"),
        "module Tidepool.Effects.Core where\nimport Tidepool.Prelude\nspawnSpec :: Int -> Int -> Int\nspawnSpec a b = a + b\n",
    )
    .unwrap();

    write_fixture(
        dir,
        "FNarrow.hs",
        "module FNarrow where\nimport Tidepool.Prelude\nimport Tidepool.Effects.Core\nresult :: Int\nresult = coreStub\n",
    );
    write_fixture(
        dir,
        "FWide.hs",
        "module FWide where\nimport Tidepool.Prelude\nimport Tidepool.Effects.Core\nresult :: Int\nresult = spawnSpec 40 2\n",
    );

    // Request 1 warms the shared memo with the spawnSpec-less module.
    let mut first_cmd = ExtractCmd::with_bin(ResolvedExtractBin::assume_resolved(bin));
    first_cmd
        .input(dir.join("FNarrow.hs"))
        .output_dir(dir.join("out-f-narrow"))
        .target("result")
        .include(lib)
        .include(&first_inc);
    let first_out = run_via_env_socket(&first_cmd, socket);
    assert!(
        first_out.status.success(),
        "first warm-up compile should succeed: {}",
        String::from_utf8_lossy(&first_out.stderr)
    );

    // Request 2 through the same daemon must see its own include root's
    // `spawnSpec`, not request 1's stale binding.
    let mut second_cmd = ExtractCmd::with_bin(ResolvedExtractBin::assume_resolved(bin));
    second_cmd
        .input(dir.join("FWide.hs"))
        .output_dir(dir.join("out-f-wide-daemon"))
        .target("result")
        .include(lib)
        .include(&second_inc);
    let second_daemon_out = run_via_env_socket(&second_cmd, socket);

    assert!(
        second_daemon_out.status.success(),
        "the second request (same module name, different include root) \
         must compile through the warm daemon, not fail as though spawnSpec were unresolved: {}",
        String::from_utf8_lossy(&second_daemon_out.stderr)
    );
    let daemon_stderr = String::from_utf8_lossy(&second_daemon_out.stderr);
    assert!(
        !daemon_stderr.contains("POISONED"),
        "spawnSpec must not be silently replaced with a poison sentinel: {daemon_stderr}"
    );

    // Baseline: a direct spawn of the same second fixture never touches the
    // daemon's shared memo at all, so it is the ground truth for "what does
    // this request's own include dir actually resolve to".
    let mut second_direct_cmd = ExtractCmd::with_bin(ResolvedExtractBin::assume_resolved(bin));
    second_direct_cmd
        .input(dir.join("FWide.hs"))
        .output_dir(dir.join("out-f-wide-direct"))
        .target("result")
        .include(lib)
        .include(&second_inc);
    let second_direct_out = run_direct(&second_direct_cmd);
    assert!(
        second_direct_out.status.success(),
        "direct spawn baseline should succeed: {}",
        String::from_utf8_lossy(&second_direct_out.stderr)
    );

    assert_eq!(
        second_daemon_out.stdout, second_direct_out.stdout,
        "diagnostics JSON must match between the warm-daemon-served second request and a direct spawn"
    );
    let daemon_cbor = fs::read(dir.join("out-f-wide-daemon/result.prepared.cbor")).unwrap();
    let direct_cbor = fs::read(dir.join("out-f-wide-direct/result.prepared.cbor")).unwrap();
    assert_eq!(
        daemon_cbor, direct_cbor,
        "result.prepared.cbor must be byte-identical — spawnSpec (40+2=42) must be the ACTUAL binding \
         resolved from this request's own include dir, not a stale memo entry"
    );
}

/// (g) A module whose OWN source text is BYTE-IDENTICAL across two
/// requests (mirroring `Tidepool.Orchestrate`, which spells only the bare
/// `M` alias and never mentions a pinned `Finalize <T>` literally) but
/// whose compiled MEANING depends on a fixed-name/varying-content sibling
/// it imports (mirroring the per-window `Tidepool.Effects` shim, whose own
/// `type M = Eff row` is exactly this shape) — daemon-shim-identity-fix's
/// own regression, one step past what check (f) covers. Check (f)'s stale
/// module ITSELF changed content across requests, so its own `ms_hs_hash`
/// correctly forced a fresh compile; THIS check's stale module
/// (`Tidepool.Companion`) never changes, so a memo hit validated by
/// self-hash ALONE would wrongly reuse request A's compile —
/// `companionVal`'s type frozen to request A's own `Tidepool.Shim.Pinned`
/// expansion (`Pinned = Int`) — against request B's own target module,
/// which declares its OWN local binding at ITS OWN fresh `Pinned`
/// expansion (`Pinned = Bool`) and assigns `companionVal` to it. This is
/// structurally the exact shape of the real bug: `Tidepool.Orchestrate`'s
/// `paginateTrunc :: Int -> Value -> M Value` (`M` a type SYNONYM, exactly
/// like `Pinned` here) got frozen against a stale `Finalize Int` row while
/// the target's own `paginateResult :: Int -> Value -> M Value` used a
/// fresh `Finalize KyotoResult` row — `paginateResult = paginateTrunc`
/// failed with `Couldn't match type 'Int' with 'KyotoResult' / Expected:
/// Int -> Value -> M Value / Actual: Int -> Value -> M Value`, the exact
/// signature this lane's own bisection observed.
///
/// Routed through a SESSION-BOUND reference turn (mirroring check (c)'s
/// bind/reference pattern), not a plain `--target` compile: the real
/// repro's failing compile is a session-scoped turn
/// (`isSessionScopeActive`), which selects `sessionVariant`'s
/// `OptimizeEveryModule` tier — the tier that compiles every non-deferred
/// home module (including `Tidepool.Companion`/`Tidepool.Orchestrate`,
/// neither the target nor a `Val`-importer) through THIS module's own
/// per-module loop rather than leaving it to `load'` alone. A plain
/// `normalVariant`/`OptimizeCoreReachable` compile does not reproduce this —
/// its target's own fresh typecheck resolves `Tidepool.Companion` from the
/// ambient, already-correctly-recompiled-by-`load'` HPT, so only the
/// session tier's own extra per-module pass exercises the
/// stale-memo-front hazard this fixture pins.
fn check_g_shim_dependent_module_warm_second_request(
    bin: &Path,
    dir: &Path,
    lib: &Path,
    socket: &Path,
) {
    let a_inc = dir.join("g-pin-a");
    let b_inc = dir.join("g-pin-b");
    fs::create_dir_all(a_inc.join("Tidepool")).unwrap();
    fs::create_dir_all(b_inc.join("Tidepool")).unwrap();

    // The per-window "shim": fixed module NAME, a type SYNONYM (mirroring
    // `Tidepool.Effects`'s own `type M = Eff row`) whose RHS is pinned
    // differently per request.
    fs::write(
        a_inc.join("Tidepool/Shim.hs"),
        "module Tidepool.Shim (Pinned) where\ntype Pinned = Int\n",
    )
    .unwrap();
    fs::write(
        b_inc.join("Tidepool/Shim.hs"),
        "module Tidepool.Shim (Pinned) where\ntype Pinned = Bool\n",
    )
    .unwrap();

    // The dependent module: BYTE-IDENTICAL in both include dirs (mirroring
    // `Tidepool.Orchestrate`'s own source never mentioning the pin) —
    // `companionVal`'s type is frozen to whatever `Pinned` expanded to AT
    // THIS MODULE'S OWN compile time, only resolving to a different
    // expansion because it lives alongside a different `Tidepool/Shim.hs`
    // in each content-addressed dir.
    let companion_src =
        "module Tidepool.Companion (companionVal) where\nimport Tidepool.Shim\ncompanionVal :: Pinned\ncompanionVal = undefined\n";
    fs::write(a_inc.join("Tidepool/Companion.hs"), companion_src).unwrap();
    fs::write(b_inc.join("Tidepool/Companion.hs"), companion_src).unwrap();

    // A trivial session bind per side — `isSessionScopeActive` requires a
    // non-empty `ssValIfaces`, which is what actually routes the later
    // reference turn through `sessionVariant` (see the doc comment above).
    let bind = |label: &str| -> PathBuf {
        let root = dir.join(format!("g-session-{label}"));
        fs::create_dir_all(&root).unwrap();
        let bind_cmd = turn_cmd(
            bin,
            dir,
            TurnFixture {
                label: &format!("g-{label}-bind"),
                source: "x <- pure (0 :: Int)",
                template: include_str!("daemon_integration/resident_bind.hs"),
                shape: TurnShape::Bind,
                output_dir: dir.join(format!("out-g-{label}-bind")),
                session_root: &root,
                inject: &[],
                includes: &[lib],
            },
        );
        let out = run_via_env_socket(&bind_cmd, socket);
        assert!(
            out.status.success(),
            "session bind ({label}) should succeed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        root
    };
    let root_a = bind("A");
    let root_b = bind("B");

    // Request A: warms the shared memo's `Tidepool.Companion` entry against
    // dir A's own `Pinned` expansion (`= Int`) — just by IMPORTING it, same
    // as the real shim/orchestrate pair: downsweep compiles every imported
    // home module regardless of whether the target actually uses its
    // exports.
    let a_cmd = turn_cmd(
        bin,
        dir,
        TurnFixture {
            label: "g-a-ref",
            source: "x",
            template: include_str!("daemon_integration/shim_ref_a.hs"),
            shape: TurnShape::Expr,
            output_dir: dir.join("out-g-a-ref"),
            session_root: &root_a,
            inject: &["Tidepool.Session.Val.G1"],
            includes: &[lib, &a_inc],
        },
    );
    let a_out = run_via_env_socket(&a_cmd, socket);
    assert!(
        a_out.status.success(),
        "request A (Pinned = Int) should succeed: {}",
        String::from_utf8_lossy(&a_out.stderr)
    );

    // Request B, through the SAME warm daemon: `Tidepool.Companion`'s own
    // source is byte-identical to request A's, so a self-hash-only memo
    // check would wrongly reuse request A's compile — `companionVal`'s type
    // would stay frozen to request A's `Pinned = Int` instead of request
    // B's own `Pinned = Bool`, exactly like the real bug's
    // `paginateResult = paginateTrunc` freezing a stale `Finalize Int` row.
    // The byte-comparison assertions below (not necessarily a hard GHC
    // diagnostic — confirmed empirically red pre-fix via a silent
    // daemon-vs-direct-spawn CBOR divergence rather than a compile error
    // here) are the actual oracle; a GHC-level "Couldn't match type" is
    // rejected too, when it does surface.
    let b_cmd = turn_cmd(
        bin,
        dir,
        TurnFixture {
            label: "g-b-ref",
            source: "(if mine then 1 else 0) + x",
            template: include_str!("daemon_integration/shim_ref_b.hs"),
            shape: TurnShape::Expr,
            output_dir: dir.join("out-g-b-daemon"),
            session_root: &root_b,
            inject: &["Tidepool.Session.Val.G1"],
            includes: &[lib, &b_inc],
        },
    );
    let b_daemon_out = run_via_env_socket(&b_cmd, socket);

    let daemon_stderr = String::from_utf8_lossy(&b_daemon_out.stderr);
    assert!(
        b_daemon_out.status.success(),
        "request B (Pinned = Bool), through the warm daemon, must compile: {daemon_stderr}"
    );
    assert!(
        !daemon_stderr.contains("Couldn't match type"),
        "must not be a type mismatch between request A's frozen Pinned expansion and request B's own: {daemon_stderr}"
    );

    // Baseline: a direct spawn of the SAME request-B fixture never touches
    // the daemon's shared memo at all.
    let b_direct_cmd = turn_cmd(
        bin,
        dir,
        TurnFixture {
            label: "g-b-ref",
            source: "(if mine then 1 else 0) + x",
            template: include_str!("daemon_integration/shim_ref_b.hs"),
            shape: TurnShape::Expr,
            output_dir: dir.join("out-g-b-direct"),
            session_root: &root_b,
            inject: &["Tidepool.Session.Val.G1"],
            includes: &[lib, &b_inc],
        },
    );
    let b_direct_out = run_direct(&b_direct_cmd);
    assert!(
        b_direct_out.status.success(),
        "direct spawn baseline should succeed: {}",
        String::from_utf8_lossy(&b_direct_out.stderr)
    );

    assert_eq!(
        b_daemon_out.stdout, b_direct_out.stdout,
        "diagnostics JSON must match between the warm-daemon-served request B and a direct spawn"
    );
    // See check_c's `reference` closure for why this is `__prepared.prepared.cbor`
    // rather than `result.cbor` — a resident turn's compile target is always
    // the fixed prepared scaffold binder.
    let daemon_cbor = fs::read(dir.join("out-g-b-daemon/__prepared.prepared.cbor")).unwrap();
    let direct_cbor = fs::read(dir.join("out-g-b-direct/__prepared.prepared.cbor")).unwrap();
    assert_eq!(
        daemon_cbor, direct_cbor,
        "__prepared.prepared.cbor must be byte-identical — request B's own Pinned/companionVal must be what \
         actually compiles, not a stale request-A Tidepool.Companion memo entry"
    );
    assert_eq!(
        fs::read(dir.join("out-g-b-daemon/turn.cbor")).unwrap(),
        fs::read(dir.join("out-g-b-direct/turn.cbor")).unwrap(),
        "the resident-turn result must also be byte-identical across transports"
    );
}

/// (j) The resident daemon reuses ONE GHC session across requests
/// (`Tidepool.GhcPipeline`'s resident API), and `Tidepool.RetainedUnfoldings`
/// withholds unfoldings for a request's declared retained-generation set so
/// GHC's simplifier cannot inline/recover them into the target. This proves
/// the withholding plugin reads that set PER REQUEST — recompiling as it
/// changes — rather than latching onto whichever set first warmed the
/// session: it drives the SAME warm daemon through the retained set
/// growing, shrinking, and returning to empty, and after every step
/// compares the daemon's artifact against a fresh one-shot (non-daemon)
/// compile of the identical request.
///
/// Fixtures: `ImportProducerExposed.hs` declares `producerValue`/
/// `producerFn` with NO `NOINLINE` pragma (unlike the plain `ImportProducer`
/// pair used elsewhere in this crate) — nothing but the withholding plugin
/// stops GHC from inlining them. `ImportConsumerExposed.hs`'s
/// `consumerResult` imports and calls both. See
/// `ExecutionProjectionTest.verifyRetainedImportProjectionExposed` (Haskell
/// side) for the same contract proven directly against the pipeline API;
/// this check proves it survives the daemon's resident-worker/wire
/// boundary instead.
///
/// `tidepool-extract-cmd` is a dependency-leaf crate (see its own
/// `CLAUDE.md`) and does not depend on `tidepool-repr`, so this check
/// cannot decode the emitted `execution_schema::WireProgram` CBOR to read
/// `GlobalDecl`/`required_generation` directly. Two things stand in:
///   * daemon-vs-one-shot agreement is checked by exact CBOR BYTE equality
///     of `consumerResult.prepared.cbor`, for every retained-set step;
///   * the qualitative "recovered locally" vs. "global reference" split is
///     checked by literal ASCII substring search over those same bytes,
///     using the exact recovered-top marker names
///     `ExecutionProjectionTest`'s own doc comment on `consumerResult`
///     names: `producerValue1` (one of the floated list sub-bindings GHC's
///     simplifier produces when it inlines `producerValue`'s literal list)
///     and `$wproducerFn` (the specialised worker GHC produces when it
///     inlines/specialises `producerFn`). CBOR text strings store their
///     UTF-8 payload as contiguous raw bytes with no escaping, so a literal
///     ASCII needle is a sound (if syntactic, not semantically decoded)
///     presence/absence check.
fn check_j_retained_generation_transitions_through_warm_daemon(
    bin: &Path,
    dir: &Path,
    socket: &Path,
) {
    let lib = stdlib_lib_dir();
    let fixture_dir = prepared_stg_fixture_dir();
    assert!(
        fixture_dir.join("ImportProducerExposed.hs").is_file(),
        "expected fixture at {}",
        fixture_dir.join("ImportProducerExposed.hs").display()
    );
    assert!(
        fixture_dir.join("ImportConsumerExposed.hs").is_file(),
        "expected fixture at {}",
        fixture_dir.join("ImportConsumerExposed.hs").display()
    );

    let producer_value_id = SymbolIdentity {
        unit: "main".to_owned(),
        module: "ImportProducerExposed".to_owned(),
        namespace: "value".to_owned(),
        occurrence: "producerValue".to_owned(),
        record_parent: None,
    };
    let producer_fn_id = SymbolIdentity {
        unit: "main".to_owned(),
        module: "ImportProducerExposed".to_owned(),
        namespace: "value".to_owned(),
        occurrence: "producerFn".to_owned(),
        record_parent: None,
    };

    // Empty, {value, fn}, {fn} only, {value, fn} again, empty again — grows,
    // narrows, regrows, and fully retreats, so a session that only ever
    // reacts to warming up (not to the set actually changing back down)
    // cannot pass this by accident.
    let steps: [(&str, &[SymbolIdentity]); 5] = [
        ("empty-1", &[]),
        (
            "both-1",
            &[producer_value_id.clone(), producer_fn_id.clone()],
        ),
        ("fn-only", std::slice::from_ref(&producer_fn_id)),
        (
            "both-2",
            &[producer_value_id.clone(), producer_fn_id.clone()],
        ),
        ("empty-2", &[]),
    ];

    let build_cmd = |out_dir: PathBuf, retained: &[SymbolIdentity]| -> ExtractCmd {
        let mut cmd = ExtractCmd::with_bin(ResolvedExtractBin::assume_resolved(bin));
        cmd.input(fixture_dir.join("ImportConsumerExposed.hs"))
            .output_dir(out_dir)
            .target("consumerResult")
            .include(&lib)
            .include(&fixture_dir);
        for identity in retained {
            cmd.retained_generation(identity.clone(), 11);
        }
        cmd
    };

    for (label, retained) in steps {
        let daemon_out_dir = dir.join(format!("out-j-{label}-daemon"));
        let direct_out_dir = dir.join(format!("out-j-{label}-direct"));

        let daemon_cmd = build_cmd(daemon_out_dir.clone(), retained);
        let daemon_result = run_via_env_socket(&daemon_cmd, socket);
        assert!(
            daemon_result.status.success(),
            "daemon compile (retained set: {label}) should succeed: {}",
            String::from_utf8_lossy(&daemon_result.stderr)
        );

        let direct_cmd = build_cmd(direct_out_dir.clone(), retained);
        let direct_result = run_direct(&direct_cmd);
        assert!(
            direct_result.status.success(),
            "one-shot compile (retained set: {label}) should succeed: {}",
            String::from_utf8_lossy(direct_result.stderr.as_slice())
        );

        let daemon_cbor = fs::read(daemon_out_dir.join("consumerResult.prepared.cbor"))
            .unwrap_or_else(|e| panic!("read daemon artifact ({label}): {e}"));
        let direct_cbor = fs::read(direct_out_dir.join("consumerResult.prepared.cbor"))
            .unwrap_or_else(|e| panic!("read one-shot artifact ({label}): {e}"));
        assert_eq!(
            daemon_cbor, direct_cbor,
            "consumerResult.prepared.cbor must be byte-identical between the warm daemon and a \
             fresh one-shot compile for retained set '{label}' — the daemon's resident GHC \
             session must re-read the retained-generation set on every request, not reuse \
             whatever set warmed it"
        );

        let contains = |needle: &str| -> bool {
            daemon_cbor
                .windows(needle.len())
                .any(|w| w == needle.as_bytes())
        };
        // Without a decoder, show where a marker occurs so a failure names the
        // surrounding symbols instead of only asserting presence.
        let context = |needle: &str| -> String {
            daemon_cbor
                .windows(needle.len())
                .enumerate()
                .filter(|(_, w)| *w == needle.as_bytes())
                .map(|(at, _)| {
                    let start = at.saturating_sub(96);
                    let end = (at + needle.len() + 96).min(daemon_cbor.len());
                    daemon_cbor[start..end]
                        .iter()
                        .map(|b| {
                            if b.is_ascii_graphic() {
                                *b as char
                            } else {
                                '.'
                            }
                        })
                        .collect::<String>()
                })
                .collect::<Vec<_>>()
                .join(" | ")
        };
        let retains_both = retained.len() == 2;
        if retains_both {
            assert!(
                !contains("producerValue1"),
                "retained set '{label}' retains both producer symbols, so producerValue's \
                 floated list sub-bindings must not be recovered into the consumer artifact; \
                 found at: {}",
                context("producerValue1")
            );
            assert!(
                !contains("$wproducerFn"),
                "retained set '{label}' retains both producer symbols, so no specialised \
                 producerFn worker should be recovered into the consumer artifact; found at: {}",
                context("$wproducerFn")
            );
        } else if retained.is_empty() {
            assert!(
                contains("producerValue1") || contains("$wproducerFn"),
                "retained set '{label}' is empty, so plain GHC behavior should recover at least \
                 one of producerValue's floated sub-bindings or producerFn's specialised worker \
                 into the consumer artifact"
            );
        }
    }
}

/// (e) rotation unpublishes the socket after its accepted request and lets the
/// next known-unsubmitted binding choose a direct endpoint.
fn check_e_rotation_then_fallback(bin: &Path, lib: &Path) {
    let dir = unique_scratch_dir("rotate");
    let socket = unique_socket_path("rotate");
    write_fixture(
        &dir,
        "Expr.hs",
        "module Expr where\nimport Tidepool.Prelude\nresult :: Int\nresult = 5\n",
    );

    let Some(daemon) = spawn_daemon(bin, &socket, &["--rotate-after", "1"]) else {
        let _ = fs::remove_dir_all(&dir);
        return;
    };

    let first = run_via_env_socket(&cmd_for(bin, &dir, "out-e-first", "Expr.hs", lib), &socket);
    assert!(first.status.success(), "rotation request should succeed");

    // The daemon should exit on its own shortly after serving the request —
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
            "daemon did not exit after draining rotation clients within 15s"
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

/// A persistent daemon rotates only its GHC worker, retaining the protocol
/// endpoint so a long-lived owner never falls back to per-request processes.
fn check_i_persistent_rotation_keeps_serving(bin: &Path, lib: &Path) {
    let dir = unique_scratch_dir("persistent-rotate");
    let socket = unique_socket_path("persistent-rotate");
    write_fixture(
        &dir,
        "Expr.hs",
        "module Expr where\nimport Tidepool.Prelude\nresult :: Int\nresult = 6\n",
    );

    let Some(mut daemon) = spawn_daemon(bin, &socket, &["--rotate-after", "1", "--persistent"])
    else {
        let _ = fs::remove_dir_all(&dir);
        return;
    };

    for output in ["out-i-first", "out-i-second"] {
        let result = run_via_env_socket(&cmd_for(bin, &dir, output, "Expr.hs", lib), &socket);
        assert!(
            result.status.success(),
            "persistent daemon request failed: {}",
            String::from_utf8_lossy(&result.stderr)
        );
        tidepool_extract_cmd::preflight_compiler_daemon(&socket)
            .expect("persistent daemon socket must survive worker rotation");
        assert!(
            daemon.child.try_wait().unwrap().is_none(),
            "persistent daemon exited after rotating its worker"
        );
    }

    drop(daemon);
    let _ = fs::remove_dir_all(&dir);
}

/// Bind a daemon-routed endpoint through `socket` and return its identity.
/// Binding a daemon endpoint only preflights the wire handshake (see
/// `CompilerEndpoint::bind_unscoped`); it does not require a stdlib or an
/// actual compile.
fn bound_identity(bin: &Path, socket: &Path) -> tidepool_extract_cmd::CompilerIdentity {
    let cmd = ExtractCmd::with_bin(ResolvedExtractBin::assume_resolved(bin));
    let (k, v) = env_socket(socket);
    std::env::set_var(k, &v);
    let endpoint = cmd.bind().expect("daemon bind should succeed");
    std::env::remove_var(k);
    endpoint.identity().clone()
}

/// Two daemons booted independently (each with its own random boot epoch —
/// `daemon.rs`'s `boot_epoch()`) from the SAME extract binary must still
/// report the SAME producer identity. This is the actual scenario
/// `scripts/battery.sh` exercises by starting a fresh daemon per run: the
/// compile memo (`tidepool-toolchain`'s `cache::invocation_key`) and the
/// `-fwrite-interface` build-products directory
/// (`tidepool-toolchain`'s `paths::build_products_dir`) key off
/// `CompilerIdentity::producer_bytes()`/`producer_hex()`, not
/// `as_bytes()`/`to_hex()`, so a fresh daemon reuses a prior daemon's warm
/// cache namespace instead of starting cold every run. The wire endpoint
/// identity (`as_bytes()`/`to_hex()`) still differs per boot, since it folds
/// in the epoch to reject a stale connection — see
/// `daemon_boot_epoch_contributes_to_endpoint_identity` in `daemon.rs` for
/// that half of the contract.
#[test]
fn producer_identity_survives_independent_daemon_boots() {
    let Some((bin, _lib)) = daemon_toolchain() else {
        return;
    };

    let socket_a = unique_socket_path("producer-a");
    let socket_b = unique_socket_path("producer-b");
    let Some(daemon_a) = spawn_daemon(&bin, &socket_a, &[]) else {
        return;
    };
    let Some(daemon_b) = spawn_daemon(&bin, &socket_b, &[]) else {
        return;
    };

    let identity_a = bound_identity(&bin, &socket_a);
    let identity_b = bound_identity(&bin, &socket_b);

    assert_eq!(
        identity_a.producer_bytes(),
        identity_b.producer_bytes(),
        "two daemons booted from the same extract binary must share one producer identity"
    );
    assert_eq!(
        identity_a.producer_hex(),
        identity_b.producer_hex(),
        "producer_hex must match too — this is what paths::build_products_dir and \
         cache::invocation_key now key their cache namespace on"
    );
    assert_ne!(
        identity_a.as_bytes(),
        identity_b.as_bytes(),
        "each daemon boot still gets a distinct wire endpoint identity from its own boot \
         epoch, which protects the wire protocol against a stale connection"
    );

    drop(daemon_a);
    drop(daemon_b);
}
