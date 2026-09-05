//! S5 cache-layer property suite for `tidepool-toolchain/src/cache.rs`.
//!
//! TESTABILITY NOTE: `cache_key` / `cache_load` / `cache_store` are
//! `pub(crate)` to that crate, so an integration test cannot
//! call the primitives directly. This suite therefore drives the cache layer
//! *behaviorally* through the public `compile_haskell` API:
//!
//!   - `XDG_CACHE_HOME` -> per-test tempdir (the real `~/.cache/tidepool` is
//!     never touched),
//!   - `TIDEPOOL_EXTRACT` -> a stub worker that consumes the current typed
//!     request protocol, copies fabricated CBOR fixtures into the requested
//!     output dir, and records each invocation.
//!
//! Oracle: the invocation-count delta distinguishes cache HIT (0 new runs)
//! from cache MISS (1 new run). Key equality is therefore observable: if a
//! second compile with different inputs does NOT bump the counter, the two
//! inputs collided on the same cache key.
//!
//! Convention: non-`#[ignore]` tests assert *current* behavior and keep the
//! suite green. A confirmed-but-unfixed bug keeps an `#[ignore = "BUG: ..."]`
//! twin asserting the *correct* behavior (run with `--ignored` to see it
//! fail). All fixed findings' twins are now the ACTIVE regression tests and
//! the old buggy-behavior pins are deleted.

#![cfg(unix)]

use proptest::prelude::*;
use serial_test::serial;
use std::fs;
use std::io::{Read, Write};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::UnixListener;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use tempfile::TempDir;
use tidepool_repr::serial::{read_cbor, write_cbor, write_metadata};
use tidepool_repr::{CoreExpr, CoreFrame, DataConTable, Literal, RecursiveTree};
use tidepool_runtime::{compile_haskell, CompileResult};

// ---------------------------------------------------------------------------
// Harness
// ---------------------------------------------------------------------------

/// RAII guard to set and restore environment variables (mirrors the pattern
/// used by cache.rs's own unit tests). All tests are `#[serial]` because env
/// vars are process-global.
struct EnvGuard {
    key: &'static str,
    old_value: Option<std::ffi::OsString>,
}

impl EnvGuard {
    fn new(key: &'static str, new_value: impl AsRef<std::ffi::OsStr>) -> Self {
        let old_value = std::env::var_os(key);
        std::env::set_var(key, new_value);
        Self { key, old_value }
    }

    fn unset(key: &'static str) -> Self {
        let old_value = std::env::var_os(key);
        std::env::remove_var(key);
        Self { key, old_value }
    }
}

impl Drop for EnvGuard {
    fn drop(&mut self) {
        if let Some(ref old) = self.old_value {
            std::env::set_var(self.key, old);
        } else {
            std::env::remove_var(self.key);
        }
    }
}

/// A single-node `Lit (LitInt n)` Core expression — the smallest valid fixture.
fn lit_expr(n: i64) -> CoreExpr {
    RecursiveTree {
        nodes: vec![CoreFrame::Lit(Literal::LitInt(n))],
    }
}

fn empty_meta_bytes() -> Vec<u8> {
    write_metadata(&DataConTable::new(), &Default::default()).expect("metadata fixture")
}

/// Monotone counter so every test/case gets a unique source string and cache
/// entries never collide across cases by accident.
static CASE: AtomicUsize = AtomicUsize::new(0);

fn unique_src(tag: &str) -> String {
    format!(
        "-- s5 case {} {}\n",
        CASE.fetch_add(1, Ordering::SeqCst),
        tag
    )
}

struct Harness {
    root: TempDir,
    _guards: Vec<EnvGuard>,
}

impl Harness {
    fn new() -> Self {
        Self::with_fixture_bytes(write_cbor(&lit_expr(42)).unwrap())
    }

    /// Build a harness whose stub extractor emits `expr_bytes` as the compiled
    /// artifact.
    fn with_fixture_bytes(expr_bytes: Vec<u8>) -> Self {
        let root = TempDir::new().unwrap();
        let r = root.path();
        fs::create_dir_all(r.join("cache")).unwrap();
        fs::create_dir_all(r.join("fx")).unwrap();
        fs::create_dir_all(r.join("bin")).unwrap();

        fs::write(r.join("fx/a.cbor"), &expr_bytes).unwrap();
        fs::write(r.join("fx/meta.cbor"), empty_meta_bytes()).unwrap();

        // Implement the bound endpoint protocol, then delegate worker behavior
        // back to this integration-test binary. The helper decodes the real
        // request frame and typed request rather than duplicating a CLI.
        let stub = r.join("bin/extract-stub");
        let test_binary = std::env::current_exe().unwrap();
        let script = format!(
            r#"#!/bin/sh
test "$1" = --compiler-endpoint-v1 || exit 2
printf TPCID001
dd if=/dev/zero bs=32 count=1 2>/dev/null
request='{request}'
cat > "$request"
echo run >> '{count}'
TIDEPOOL_FAKE_EXTRACT_REQUEST_FILE="$request" \
TIDEPOOL_FAKE_EXTRACT_EXPR='{fx}/a.cbor' \
TIDEPOOL_FAKE_EXTRACT_META='{fx}/meta.cbor' \
'{test_binary}' --exact proptest_cache_layer::fake_extract_worker --nocapture >/dev/null 2>/dev/null || exit $?
report='{{"version":2,"outcome":"success","diagnostics":[]}}'
printf '\000\000\000\000\062\000\000\000%s\000\000\000\000' "$report"
"#,
            request = r.join("request.bin").display(),
            count = r.join("count").display(),
            fx = r.join("fx").display(),
            test_binary = test_binary.display(),
        );
        fs::write(&stub, script).unwrap();
        fs::set_permissions(&stub, fs::Permissions::from_mode(0o755)).unwrap();

        let guards = vec![
            EnvGuard::new("XDG_CACHE_HOME", r.join("cache")),
            EnvGuard::new("TIDEPOOL_EXTRACT", &stub),
            EnvGuard::unset("TIDEPOOL_EXTRACT_DAEMON_SOCKET"),
        ];

        Self {
            root,
            _guards: guards,
        }
    }

    fn path(&self) -> &Path {
        self.root.path()
    }

    /// How many times the stub extractor has run (the HIT/MISS oracle).
    fn runs(&self) -> usize {
        fs::read_to_string(self.path().join("count"))
            .map(|s| s.lines().count())
            .unwrap_or(0)
    }

    fn compile(
        &self,
        src: &str,
        target: &str,
        include: &[&Path],
    ) -> Result<CompileResult, tidepool_runtime::CompileError> {
        compile_haskell(src, target, include)
    }

    fn use_daemon(&mut self, socket: &Path) {
        self._guards
            .push(EnvGuard::new("TIDEPOOL_EXTRACT_DAEMON_SOCKET", socket));
    }

    /// Swap the primary fixture (what a "recompile" would now produce).
    /// Was used by the deleted F3a buggy-behavior pin; kept for future
    /// staleness scenarios.
    #[allow(dead_code)]
    fn set_fixture(&self, expr: &CoreExpr) {
        fs::write(self.path().join("fx/a.cbor"), write_cbor(expr).unwrap()).unwrap();
    }

    /// The cache entry directory used by cache.rs under our XDG override.
    fn cache_dir(&self) -> PathBuf {
        self.path().join("cache/tidepool")
    }

    /// Keys of all completed entries (stems of `*.ok` sentinel files).
    fn entry_keys(&self) -> Vec<String> {
        let Ok(rd) = fs::read_dir(self.cache_dir()) else {
            return vec![];
        };
        let mut keys: Vec<String> = rd
            .filter_map(|e| e.ok())
            .filter_map(|e| {
                let name = e.file_name().into_string().ok()?;
                name.strip_suffix(".ok").map(str::to_string)
            })
            .collect();
        keys.sort();
        keys
    }

    /// Paths (cbor, meta, asks, ok) for the single cache entry; panics if not exactly one.
    fn entry_paths(&self) -> (PathBuf, PathBuf, PathBuf, PathBuf) {
        let keys = self.entry_keys();
        assert_eq!(keys.len(), 1, "expected exactly one cache entry");
        let d = self.cache_dir();
        (
            d.join(format!("{}.cbor", keys[0])),
            d.join(format!("{}.meta.cbor", keys[0])),
            d.join(format!("{}.asks.json", keys[0])),
            d.join(format!("{}.ok", keys[0])),
        )
    }
}

/// Child-process entry point used by [`Harness`]. A normal test invocation has
/// no request environment and returns immediately; the endpoint stub above
/// invokes this exact test with the framed request in a temporary file.
#[test]
fn fake_extract_worker() {
    let Some(request_file) = std::env::var_os("TIDEPOOL_FAKE_EXTRACT_REQUEST_FILE") else {
        return;
    };
    let request_frame = fs::read(request_file).unwrap();
    let argv = decode_endpoint_request(&request_frame);
    assert_eq!(argv.len(), 2, "typed worker request argv");
    let bytes = decode_hex(argv[1].to_str().expect("typed request is ASCII hex"));
    let request = tidepool_extract_cmd::ExtractRequest::decode(&bytes).unwrap();
    let output_dir = PathBuf::from(request.output_directory().unwrap());
    let expr = PathBuf::from(std::env::var_os("TIDEPOOL_FAKE_EXTRACT_EXPR").unwrap());
    let meta = PathBuf::from(std::env::var_os("TIDEPOOL_FAKE_EXTRACT_META").unwrap());
    for target in request.target_names() {
        fs::copy(&expr, output_dir.join(format!("{target}.cbor"))).unwrap();
    }
    fs::copy(meta, output_dir.join("meta.cbor")).unwrap();
    fs::write(output_dir.join("asks.json"), b"[]").unwrap();
}

fn decode_endpoint_request(bytes: &[u8]) -> Vec<std::ffi::OsString> {
    use std::os::unix::ffi::OsStringExt;

    fn take_frame<'a>(bytes: &mut &'a [u8]) -> &'a [u8] {
        let (size, rest) = bytes.split_at(4);
        let size = u32::from_le_bytes(size.try_into().unwrap()) as usize;
        let (frame, rest) = rest.split_at(size);
        *bytes = rest;
        frame
    }

    let mut remaining = bytes;
    let _cwd = take_frame(&mut remaining);
    let (argc, rest) = remaining.split_at(4);
    remaining = rest;
    let argc = u32::from_le_bytes(argc.try_into().unwrap()) as usize;
    let argv = (0..argc)
        .map(|_| std::ffi::OsString::from_vec(take_frame(&mut remaining).to_vec()))
        .collect();
    assert!(remaining.is_empty(), "trailing endpoint request bytes");
    argv
}

fn decode_hex(hex: &str) -> Vec<u8> {
    assert_eq!(hex.len() % 2, 0, "odd-length typed request");
    hex.as_bytes()
        .chunks_exact(2)
        .map(|pair| {
            let digit = |byte| match byte {
                b'0'..=b'9' => byte - b'0',
                b'a'..=b'f' => byte - b'a' + 10,
                _ => panic!("non-hex typed request"),
            };
            digit(pair[0]) << 4 | digit(pair[1])
        })
        .collect()
}

fn rejecting_daemon(socket: &Path) -> std::thread::JoinHandle<()> {
    let _ = fs::remove_file(socket);
    let listener = UnixListener::bind(socket).unwrap();
    let socket = socket.to_path_buf();
    std::thread::spawn(move || {
        let (mut preflight, _) = listener.accept().unwrap();
        let mut magic = [0u8; 8];
        preflight.read_exact(&mut magic).unwrap();
        assert_eq!(&magic, b"TPDPF001");
        preflight.write_all(b"TPDPI001").unwrap();
        preflight.write_all(&[1; 32]).unwrap();
        preflight.write_all(&[2; 32]).unwrap();
        drop(preflight);

        let (mut request, _) = listener.accept().unwrap();
        let mut header = [0u8; 40];
        request.read_exact(&mut header).unwrap();
        assert_eq!(&header[..8], b"TPDRQ001");
        assert_eq!(&header[8..], &[2; 32]);
        let _cwd = read_endpoint_frame(&mut request);
        let argc = read_endpoint_u32(&mut request);
        for _ in 0..argc {
            let _arg = read_endpoint_frame(&mut request);
        }

        // Remove the listening name before rejecting. The caller can safely
        // rebind immediately, and that bind must select the direct endpoint.
        fs::remove_file(&socket).unwrap();
        request.write_all(&[0]).unwrap();
        request.write_all(&8u32.to_le_bytes()).unwrap();
        request.write_all(b"rejected").unwrap();
    })
}

fn read_endpoint_u32(reader: &mut impl Read) -> u32 {
    let mut bytes = [0u8; 4];
    reader.read_exact(&mut bytes).unwrap();
    u32::from_le_bytes(bytes)
}

fn read_endpoint_frame(reader: &mut impl Read) -> Vec<u8> {
    let mut bytes = vec![0; read_endpoint_u32(reader) as usize];
    reader.read_exact(&mut bytes).unwrap();
    bytes
}

/// Rewrite a file with `new_bytes` and restore its original mtime, simulating
/// a content swap that is invisible to (size, mtime) fingerprints when the
/// length is unchanged (nix store epoch mtimes, `cp -p`, `rsync -t`).
fn swap_content_preserving_mtime(path: &Path, new_bytes: &[u8]) {
    let mtime = fs::metadata(path).unwrap().modified().unwrap();
    fs::write(path, new_bytes).unwrap();
    filetime::set_file_mtime(path, filetime::FileTime::from_system_time(mtime)).unwrap();
}

// ---------------------------------------------------------------------------
// Property group 1+2: key sensitivity and stability (behavioral, via oracle)
// ---------------------------------------------------------------------------

fn arb_source_body() -> impl Strategy<Value = String> {
    // Printable ASCII plus newlines, excluding `#`: line-leading CPP
    // directives are intentionally uncacheable and therefore outside a
    // property whose oracle requires the second compile to hit.
    proptest::string::string_regex(r##"[ -"$-~\n]{1,120}"##).unwrap()
}

#[derive(Debug, Clone)]
enum Edit {
    Insert(usize, char),
    Delete(usize),
    Replace(usize, char),
}

fn apply_edit(s: &str, edit: &Edit) -> String {
    let chars: Vec<char> = s.chars().collect();
    let mut out = chars.clone();
    match edit {
        Edit::Insert(i, c) => out.insert(i % (chars.len() + 1), *c),
        Edit::Delete(i) => {
            out.remove(i % chars.len());
        }
        Edit::Replace(i, c) => {
            let i = i % chars.len();
            // Guarantee the replacement differs from the original char.
            out[i] = if chars[i] == *c {
                if *c == 'z' {
                    'a'
                } else {
                    'z'
                }
            } else {
                *c
            };
        }
    }
    out.into_iter().collect()
}

fn arb_edit() -> impl Strategy<Value = Edit> {
    let cacheable_char = || {
        prop_oneof![
            proptest::char::range(' ', '"'),
            proptest::char::range('$', '~')
        ]
    };
    prop_oneof![
        (any::<usize>(), cacheable_char()).prop_map(|(i, c)| Edit::Insert(i, c)),
        any::<usize>().prop_map(Edit::Delete),
        (any::<usize>(), cacheable_char()).prop_map(|(i, c)| Edit::Replace(i, c)),
    ]
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(48))]

    /// (1)+(2): the key is stable for identical inputs (second compile is a
    /// HIT) and sensitive to ANY single-char source edit — including
    /// whitespace and comment bytes — and to the target name.
    /// Source sensitivity is byte-exact by design (blake3 over raw bytes);
    /// conservative over-invalidation is intentional.
    #[test]
    #[serial]
    fn prop_key_stable_and_sensitive(base in arb_source_body(), edit in arb_edit(), t in "[a-z][a-z0-9_]{0,8}") {
        let h = Harness::new();
        let pfx = unique_src("prop-sens");
        let s = format!("{pfx}{base}");
        let edited = format!("{pfx}{}", apply_edit(&base, &edit));
        prop_assume!(s != edited);

        prop_assert!(h.compile(&s, &t, &[]).is_ok());
        prop_assert_eq!(h.runs(), 1, "first compile must MISS");

        prop_assert!(h.compile(&s, &t, &[]).is_ok());
        prop_assert_eq!(h.runs(), 1, "identical inputs must HIT (key stability)");

        prop_assert!(h.compile(&edited, &t, &[]).is_ok());
        prop_assert_eq!(h.runs(), 2, "single-char source edit must MISS: {:?}", edit);

        let t2 = format!("{t}x");
        prop_assert!(h.compile(&s, &t2, &[]).is_ok());
        prop_assert_eq!(h.runs(), 3, "target change must MISS");
    }
}

// ---------------------------------------------------------------------------
// Property group 3: load-after-store identity
// ---------------------------------------------------------------------------

proptest! {
    #![proptest_config(ProptestConfig::with_cases(32))]

    /// (3): what the cache serves on a HIT is byte-for-byte what was stored,
    /// for random valid Core payloads. (Random *byte* payloads can't be
    /// pushed through `cache_store` directly — it's pub(crate); see F8 —
    /// so payloads are random valid trees from `arb_core_expr`.)
    #[test]
    #[serial]
    fn prop_load_after_store_identity(t in tidepool_testing::gen::arb_core_expr()) {
        let bytes = write_cbor(&t).unwrap();
        // Compare against what the consumer-path decoder yields for these
        // bytes — identical decode proves the cache returned identical bytes.
        let expected = read_cbor(&bytes).unwrap();
        let h = Harness::with_fixture_bytes(bytes);
        let src = unique_src("prop-identity");

        let first = h.compile(&src, "t", &[]).unwrap();
        prop_assert_eq!(h.runs(), 1);
        prop_assert_eq!(&first.expr, &expected, "MISS path must yield the stored payload");

        let second = h.compile(&src, "t", &[]).unwrap();
        prop_assert_eq!(h.runs(), 1, "second compile must HIT");
        prop_assert_eq!(&second.expr, &expected, "HIT must yield identical payload");
    }
}

/// (3): identity holds for a large (~1.5 MB) non-UTF8 payload.
#[test]
#[serial]
fn load_after_store_identity_huge_payload() {
    let blob: Vec<u8> = (0..1_500_000u32).map(|i| (i % 251) as u8).collect();
    let tree = RecursiveTree {
        nodes: vec![CoreFrame::Lit(Literal::LitString(blob))],
    };
    let bytes = write_cbor(&tree).unwrap();
    let expected = read_cbor(&bytes).unwrap();
    let h = Harness::with_fixture_bytes(bytes);
    let src = unique_src("huge");

    let first = h.compile(&src, "t", &[]).unwrap();
    assert_eq!(h.runs(), 1);
    assert_eq!(first.expr, expected);

    let second = h.compile(&src, "t", &[]).unwrap();
    assert_eq!(h.runs(), 1, "must HIT");
    assert_eq!(second.expr, expected);
}

#[test]
#[serial]
fn daemon_rejection_rebinds_and_rekeys_before_direct_execution() {
    let mut h = Harness::new();
    let socket = h.path().join("reject.sock");
    h.use_daemon(&socket);
    let src = unique_src("daemon-rekey");

    let first_daemon = rejecting_daemon(&socket);
    assert!(h.compile(&src, "t", &[]).is_ok());
    first_daemon.join().unwrap();
    assert_eq!(h.runs(), 1, "the rejected request must execute direct once");

    // Recreate the same daemon identity. If the first result was incorrectly
    // stored under that rejected endpoint, this compile would hit before the
    // daemon saw a request and the server thread would remain blocked. It must
    // instead reject again, rebind direct, then hit the direct endpoint's key.
    let second_daemon = rejecting_daemon(&socket);
    assert!(h.compile(&src, "t", &[]).is_ok());
    second_daemon.join().unwrap();
    assert_eq!(
        h.runs(),
        1,
        "the rebound direct identity must hit without another execution"
    );
}

// ---------------------------------------------------------------------------
// F1: key non-injectivity — NUL separator injection
// ---------------------------------------------------------------------------

#[test]
#[serial]
fn key_should_separate_source_from_target() {
    let h = Harness::new();
    let pfx = unique_src("nul-collide-fix");
    let first = h.compile(&format!("{pfx}a\0b"), "c", &[]);
    assert!(first.is_ok(), "first compile failed: {first:?}");
    // Distinct keys ⇒ cache MISS ⇒ the extractor is actually invoked — and a
    // NUL target cannot be exec'd, so the compile must ERROR. (The pre-fix
    // collision returned Ok served from the first entry's artifact; runs()
    // cannot distinguish miss-then-spawn-fail from hit, but the Result can.)
    let res = h.compile(&format!("{pfx}a"), "b\0c", &[]);
    assert!(
        res.is_err(),
        "distinct (source,target) must derive distinct keys: Ok here means the \
         colliding cache entry was served (F1a regressed)"
    );
}

#[test]
#[serial]
fn key_should_separate_target_from_include_roots() {
    let h = Harness::new();
    let pfx = unique_src("nul-include-fix");
    let ghost = "/nonexistent/tidepool-s5-cache-probe";
    assert!(h.compile(&pfx, "t", &[Path::new(ghost)]).is_ok());
    // Same hit-vs-miss discrimination as key_should_separate_source_from_target:
    // a NUL target only returns Ok if the colliding entry was served.
    let res = h.compile(&pfx, &format!("t\0{ghost}"), &[]);
    assert!(
        res.is_err(),
        "target vs include-root boundary must be framed (F1b regressed)"
    );
}

// ---------------------------------------------------------------------------
// F2: include-dir ORDER is sorted out of the key, but GHC honors order
// ---------------------------------------------------------------------------

#[test]
#[serial]
fn key_should_be_sensitive_to_include_order() {
    let h = Harness::new();
    let dir_a = h.path().join("incA");
    let dir_b = h.path().join("incB");
    fs::create_dir_all(&dir_a).unwrap();
    fs::create_dir_all(&dir_b).unwrap();
    fs::write(dir_a.join("Lib.hs"), "libVal = 1\n").unwrap();
    fs::write(dir_b.join("Lib.hs"), "libVal = 2\n").unwrap();
    let src = unique_src("inc-order-fix");
    assert!(h.compile(&src, "t", &[&dir_a, &dir_b]).is_ok());
    assert!(h.compile(&src, "t", &[&dir_b, &dir_a]).is_ok());
    assert_eq!(h.runs(), 2, "different include order must MISS");
}

// ---------------------------------------------------------------------------
// Include membership / dir fingerprint sensitivity table
// ---------------------------------------------------------------------------

/// Sensitivity table for the include-dir fingerprint (documented in findings):
/// membership +/- sensitive, duplicates sensitive (spurious but safe miss),
/// empty dirs sensitive, non-.hs files insensitive (intentional), .hs edits
/// sensitive when size or mtime changes.
#[test]
#[serial]
fn key_sensitivity_include_membership_matrix() {
    let h = Harness::new();
    let inc1 = h.path().join("inc1");
    fs::create_dir_all(&inc1).unwrap();
    fs::write(inc1.join("One.hs"), "one = 1\n").unwrap();
    let src = unique_src("membership");

    assert!(h.compile(&src, "t", &[&inc1]).is_ok());
    assert_eq!(h.runs(), 1, "baseline MISS");

    assert!(h.compile(&src, "t", &[&inc1]).is_ok());
    assert_eq!(h.runs(), 1, "same include list must HIT");

    assert!(h.compile(&src, "t", &[]).is_ok());
    assert_eq!(h.runs(), 2, "removing an include dir must MISS");

    assert!(h.compile(&src, "t", &[&inc1, &inc1]).is_ok());
    assert_eq!(
        h.runs(),
        3,
        "duplicate include dir changes the key (spurious but safe MISS)"
    );

    let inc2 = h.path().join("inc2-empty");
    fs::create_dir_all(&inc2).unwrap();
    assert!(h.compile(&src, "t", &[&inc1, &inc2]).is_ok());
    assert_eq!(h.runs(), 4, "adding an (empty) include dir must MISS");

    // Non-.hs files are not fingerprinted — INTENTIONAL (GHC only reads .hs
    // from the search path), so this is a HIT.
    fs::write(inc1.join("README.md"), "docs\n").unwrap();
    assert!(h.compile(&src, "t", &[&inc1]).is_ok());
    assert_eq!(
        h.runs(),
        4,
        "non-.hs file additions are intentionally key-neutral"
    );

    // Editing a .hs file (different size) must MISS.
    fs::write(inc1.join("One.hs"), "one = 1\n-- edited, longer\n").unwrap();
    assert!(h.compile(&src, "t", &[&inc1]).is_ok());
    assert_eq!(h.runs(), 5, ".hs content edit (size change) must MISS");
}

// ---------------------------------------------------------------------------
// F3: include source contents are fingerprinted
// ---------------------------------------------------------------------------

#[test]
#[serial]
fn key_should_change_when_hs_content_changes() {
    let h = Harness::new();
    let inc = h.path().join("inc");
    fs::create_dir_all(&inc).unwrap();
    let lib = inc.join("Lib.hs");
    fs::write(&lib, "libVal = 1\n").unwrap();
    let src = unique_src("hs-swap-fix");
    assert!(h.compile(&src, "t", &[&inc]).is_ok());
    swap_content_preserving_mtime(&lib, b"libVal = 2\n");
    assert!(h.compile(&src, "t", &[&inc]).is_ok());
    assert_eq!(h.runs(), 2, ".hs content change must MISS");
}

// ---------------------------------------------------------------------------
// F4: symlinked .hs files fingerprinted via lstat — target edits invisible
// ---------------------------------------------------------------------------

#[test]
#[serial]
fn key_should_change_when_symlinked_hs_target_changes() {
    let h = Harness::new();
    let ext = h.path().join("ext");
    let inc = h.path().join("inc");
    fs::create_dir_all(&ext).unwrap();
    fs::create_dir_all(&inc).unwrap();
    let real = ext.join("Real.hs");
    fs::write(&real, "v1\n").unwrap();
    std::os::unix::fs::symlink(&real, inc.join("Lib.hs")).unwrap();
    let src = unique_src("symlink-hs-fix");
    assert!(h.compile(&src, "t", &[&inc]).is_ok());
    fs::write(&real, "v2 with a much longer body\n").unwrap();
    assert!(h.compile(&src, "t", &[&inc]).is_ok());
    assert_eq!(h.runs(), 2, "symlink target edit must MISS");
}

// ---------------------------------------------------------------------------
// Property group 4: corruption tolerance
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy)]
enum Corruption {
    Delete,
    TruncateZero,
    TruncateHalf,
    FlipByteAt(usize),
    WriteGarbage,
}

fn apply_corruption(path: &Path, c: Corruption) {
    match c {
        Corruption::Delete => {
            fs::remove_file(path).unwrap();
        }
        Corruption::TruncateZero => {
            fs::write(path, b"").unwrap();
        }
        Corruption::TruncateHalf => {
            let bytes = fs::read(path).unwrap();
            fs::write(path, &bytes[..bytes.len() / 2]).unwrap();
        }
        Corruption::FlipByteAt(i) => {
            let mut bytes = fs::read(path).unwrap();
            if bytes.is_empty() {
                return;
            }
            let i = i.min(bytes.len() - 1);
            bytes[i] ^= 0x01;
            fs::write(path, bytes).unwrap();
        }
        Corruption::WriteGarbage => {
            fs::write(path, b"garbage-not-cbor").unwrap();
        }
    }
}

/// (4): corruption matrix — {cbor, meta, asks, ok} x {delete, truncate, flips,
/// garbage}. Invariant asserted: the consumer NEVER panics, and either the
/// corruption is detected (counted recompile = MISS fallthrough) or the
/// served payload is identical to the original. Structural flips (header,
/// frame tags, root index) are all caught by the decoder; the one class that
/// escapes — value-byte flips that keep the CBOR valid — is pinned by the
/// dedicated F6 test below.
#[test]
#[serial]
fn corruption_matrix_no_panic_no_silent_divergence() {
    // usize::MAX clamps to the last byte (root-index byte for the cbor file).
    let ops = [
        Corruption::Delete,
        Corruption::TruncateZero,
        Corruption::TruncateHalf,
        Corruption::FlipByteAt(0), // header magic -> legacy-decode path -> reject
        Corruption::FlipByteAt(4), // version major -> UnsupportedVersion
        Corruption::FlipByteAt(8), // first CBOR byte -> structure error
        Corruption::FlipByteAt(usize::MAX), // last byte
        Corruption::WriteGarbage,
    ];
    for file_idx in 0..4usize {
        for &op in &ops {
            let h = Harness::new();
            let src = unique_src("corrupt");
            let original = h.compile(&src, "t", &[]).unwrap();
            assert_eq!(h.runs(), 1);
            let (cbor, meta, asks, ok) = h.entry_paths();
            let target = [&cbor, &meta, &asks, &ok][file_idx];
            apply_corruption(target, op);

            // Must not panic; must not serve divergent data silently.
            let res = h.compile(&src, "t", &[]);
            let reran = h.runs() == 2;
            match res {
                Ok(r) => assert!(
                    reran || r.expr == original.expr,
                    "corrupted {:?} via {:?} served DIVERGENT data as a cache hit",
                    target.file_name(),
                    op
                ),
                Err(e) => panic!(
                    "corruption {:?} on {:?} must fall through to recompile, got Err: {e}",
                    op,
                    target.file_name()
                ),
            }
        }
    }
}

/// (4): explicit partial-write / interrupted-store states. cache_store's
/// sequence is: remove .ok -> persist .cbor -> persist .meta.cbor -> persist
/// .asks.json -> write .ok. A crash at any point leaves no sentinel, so every
/// prefix state must be a MISS. Conversely a sentinel with missing payload
/// files must also be a MISS (read failure), never a panic.
#[test]
#[serial]
fn partial_write_states_are_misses() {
    // States: (keep_cbor, keep_meta, keep_asks, keep_ok)
    let states = [
        (true, false, false, false), // crashed after persisting expr
        (true, true, false, false),  // crashed after persisting metadata
        (true, true, true, false),   // crashed before writing sentinel
        (false, false, false, true), // payloads lost, sentinel intact
        (false, true, true, true),   // expr lost, sentinel intact
        (true, false, true, true),   // meta lost, sentinel intact
        (true, true, false, true),   // asks lost, sentinel intact
    ];
    for &(keep_cbor, keep_meta, keep_asks, keep_ok) in &states {
        let h = Harness::new();
        let src = unique_src("partial");
        let original = h.compile(&src, "t", &[]).unwrap();
        assert_eq!(h.runs(), 1);
        let (cbor, meta, asks, ok) = h.entry_paths();
        if !keep_cbor {
            fs::remove_file(&cbor).unwrap();
        }
        if !keep_meta {
            fs::remove_file(&meta).unwrap();
        }
        if !keep_asks {
            fs::remove_file(&asks).unwrap();
        }
        if !keep_ok {
            fs::remove_file(&ok).unwrap();
        }

        let res = h.compile(&src, "t", &[]).unwrap_or_else(|e| {
            panic!("partial state ({keep_cbor},{keep_meta},{keep_asks},{keep_ok}) errored: {e}")
        });
        assert_eq!(
            h.runs(),
            2,
            "partial state ({keep_cbor},{keep_meta},{keep_asks},{keep_ok}) must be a MISS"
        );
        assert_eq!(
            res.expr, original.expr,
            "recompile must restore the artifact"
        );
    }
}

/// FIXED (F6): the sentinel carries hashes of expr, metadata, and asks, so
/// garbage .ok content fails the checksum recompute and falls through to a
/// MISS/recompile — it no longer validates the entry.
#[test]
#[serial]
fn garbage_sentinel_forces_recompile() {
    let h = Harness::new();
    let src = unique_src("sentinel");
    let original = h.compile(&src, "t", &[]).unwrap();
    assert_eq!(h.runs(), 1);
    let (_, _, _, ok) = h.entry_paths();
    fs::write(&ok, b"garbage-not-a-checksum").unwrap();
    let res = h.compile(&src, "t", &[]).unwrap();
    assert_eq!(
        h.runs(),
        2,
        "garbage sentinel content must MISS and recompile"
    );
    assert_eq!(
        res.expr, original.expr,
        "recompile must restore the artifact"
    );
}

// ---------------------------------------------------------------------------
// F6: bit-flip integrity — FIXED, blake3 checksum lives in the sentinel
// ---------------------------------------------------------------------------

/// FIXED (F6): the .ok sentinel used to guard COMPLETENESS only, so a single
/// bit-flip in a value byte of the cached .cbor that still decoded as a VALID
/// CoreExpr — for a different program — was served as a cache hit with no
/// recompile. The sentinel now also carries hashes of expr, metadata, and asks
/// (see `cache_store`/`cache_load`), so a surviving flip fails the checksum
/// and falls through to a MISS/recompile instead. This is now the ACTIVE
/// regression test (the old buggy-behavior pin has been deleted, per this
/// suite's convention for fixed findings).
#[test]
#[serial]
fn corrupted_payload_should_be_rejected_or_recompiled() {
    let h = Harness::new();
    let src = unique_src("bitflip-fix");
    let original = h.compile(&src, "t", &[]).unwrap();
    assert_eq!(h.runs(), 1);
    let (cbor, _, _, _) = h.entry_paths();
    let bytes = fs::read(&cbor).unwrap();

    // Find a flip that the consumer decoder still accepts as valid CBOR but
    // that changes meaning (the class of corruption the checksum must catch).
    let mut found = None;
    'outer: for i in (0..bytes.len()).rev() {
        for bit in 0..8u8 {
            let mut m = bytes.clone();
            m[i] ^= 1 << bit;
            if let Ok(t) = read_cbor(&m) {
                if t != original.expr {
                    found = Some(m);
                    break 'outer;
                }
            }
        }
    }
    let corrupted =
        found.expect("no surviving bit-flip found — re-evaluate whether F6 still applies");
    fs::write(&cbor, &corrupted).unwrap();

    let served = h.compile(&src, "t", &[]).unwrap();
    assert_eq!(
        h.runs(),
        2,
        "corrupted payload must MISS the checksum and recompile"
    );
    assert_eq!(
        served.expr, original.expr,
        "recompile must restore the original program, not the corrupted one"
    );
}

// ---------------------------------------------------------------------------
// F7: symlink cycle in an include dir
// ---------------------------------------------------------------------------

/// `fingerprint_dir` recurses via `path.is_dir()`, which follows symlinks, so
/// a self-referencing symlink (`inc/loop -> inc`) LOOKS like unbounded
/// recursion. Historically the kernel bounded it by accident (each recursion
/// level adds a symlink component to the path, and path resolution fails with
/// ELOOP after ~40 symlink traversals, so `read_dir` errors and the walker
/// unwinds) — but that safety net is an incidental property of path-length
/// limits, not a guarantee (filesystem/OS-dependent, and ~40 stack frames +
/// syscalls deep before it kicks in). `fingerprint_dir` now tracks visited
/// CANONICALIZED directories explicitly and skips a repeat, so the cycle is
/// broken in O(1) instead of relying on ELOOP. This test pins the (now
/// deliberate) termination as a regression guard.
#[test]
#[serial]
fn symlink_cycle_in_include_dir_terminates_gracefully() {
    let h = Harness::new();
    let inc = h.path().join("inc");
    fs::create_dir_all(&inc).unwrap();
    std::os::unix::fs::symlink(&inc, inc.join("loop")).unwrap();
    let src = unique_src("symlink-cycle");
    assert!(h.compile(&src, "t", &[&inc]).is_ok());
    assert_eq!(h.runs(), 1);
    // And the key is still stable: a second compile with the cycle HITs.
    assert!(h.compile(&src, "t", &[&inc]).is_ok());
    assert_eq!(
        h.runs(),
        1,
        "cycle-bearing include dir must still cache stably"
    );
}
