//! S5 cache-layer property suite for `tidepool-runtime/src/cache.rs`.
//!
//! TESTABILITY NOTE (finding F8): `mod cache` is private and `cache_key` /
//! `cache_load` / `cache_store` are `pub(crate)`, so an integration test cannot
//! call the primitives directly. This suite therefore drives the cache layer
//! *behaviorally* through the public `compile_haskell` API:
//!
//!   - `XDG_CACHE_HOME` -> per-test tempdir (the real `~/.cache/tidepool` is
//!     never touched),
//!   - `TIDEPOOL_EXTRACT` -> a stub shell script that "compiles" by copying
//!     fabricated CBOR fixtures into the output dir and appending one line to
//!     an invocation-count file.
//!
//! Oracle: the invocation-count delta distinguishes cache HIT (0 new runs)
//! from cache MISS (1 new run). Key equality is therefore observable: if a
//! second compile with different inputs does NOT bump the counter, the two
//! inputs collided on the same cache key.
//!
//! Convention: non-`#[ignore]` tests assert *current* behavior (including
//! buggy behavior, with comments) and keep the suite green. Each confirmed
//! bug also has an `#[ignore = "BUG: ..."]` twin asserting the *correct*
//! behavior; run with `--ignored` to see them fail.
//!
//! Findings table: `plans/proptest-findings-cache.md`.

#![cfg(unix)]

use proptest::prelude::*;
use serial_test::serial;
use std::fs;
use std::os::unix::fs::PermissionsExt;
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
    write_metadata(&DataConTable::new()).expect("metadata fixture")
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
    /// artifact. A secondary fixture `b.cbor` (Lit 43) is also written so
    /// tests can simulate a "toolchain upgrade" by retargeting the stub.
    fn with_fixture_bytes(expr_bytes: Vec<u8>) -> Self {
        let root = TempDir::new().unwrap();
        let r = root.path();
        fs::create_dir_all(r.join("cache")).unwrap();
        fs::create_dir_all(r.join("fx")).unwrap();
        fs::create_dir_all(r.join("bin")).unwrap();

        fs::write(r.join("fx/a.cbor"), &expr_bytes).unwrap();
        fs::write(r.join("fx/b.cbor"), write_cbor(&lit_expr(43)).unwrap()).unwrap();
        fs::write(r.join("fx/meta.cbor"), empty_meta_bytes()).unwrap();

        // Stub extractor. Arg layout fixed by lib.rs:
        //   $1=input.hs $2=--output-dir $3=<dir> $4=--target $5=<target> ...
        let stub = r.join("bin/extract-stub");
        let script = format!(
            "#!/bin/sh\necho run >> '{count}'\ncp '{fx}/a.cbor' \"$3/$5.cbor\"\ncp '{fx}/meta.cbor' \"$3/meta.cbor\"\n",
            count = r.join("count").display(),
            fx = r.join("fx").display(),
        );
        fs::write(&stub, script).unwrap();
        fs::set_permissions(&stub, fs::Permissions::from_mode(0o755)).unwrap();

        let guards = vec![
            EnvGuard::new("XDG_CACHE_HOME", r.join("cache")),
            EnvGuard::new("TIDEPOOL_EXTRACT", &stub),
        ];

        Self {
            root,
            _guards: guards,
        }
    }

    fn path(&self) -> &Path {
        self.root.path()
    }

    fn stub(&self) -> PathBuf {
        self.path().join("bin/extract-stub")
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

    /// Swap the primary fixture (what a "recompile" would now produce).
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

    /// Paths (cbor, meta, ok) for the single cache entry; panics if not exactly one.
    fn entry_paths(&self) -> (PathBuf, PathBuf, PathBuf) {
        let keys = self.entry_keys();
        assert_eq!(keys.len(), 1, "expected exactly one cache entry");
        let d = self.cache_dir();
        (
            d.join(format!("{}.cbor", keys[0])),
            d.join(format!("{}.meta.cbor", keys[0])),
            d.join(format!("{}.ok", keys[0])),
        )
    }

    /// Replace TIDEPOOL_EXTRACT with a wrapper script that delegates to the
    /// stub, either with a quoted or unquoted exec target path.
    fn install_wrapper(&mut self, quoted: bool) {
        let wrapper = self.path().join("bin/wrapper");
        let stub = self.stub();
        let line = if quoted {
            format!("exec \"{}\" \"$@\"\n", stub.display())
        } else {
            format!("exec {} \"$@\"\n", stub.display())
        };
        fs::write(&wrapper, format!("#!/bin/sh\n{}", line)).unwrap();
        fs::set_permissions(&wrapper, fs::Permissions::from_mode(0o755)).unwrap();
        self._guards
            .push(EnvGuard::new("TIDEPOOL_EXTRACT", &wrapper));
    }
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
    // Printable ASCII plus newlines: covers whitespace and comment-looking text.
    proptest::string::string_regex("[ -~\n]{1,120}").unwrap()
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
    prop_oneof![
        (any::<usize>(), proptest::char::range(' ', '~')).prop_map(|(i, c)| Edit::Insert(i, c)),
        any::<usize>().prop_map(Edit::Delete),
        (any::<usize>(), proptest::char::range(' ', '~')).prop_map(|(i, c)| Edit::Replace(i, c)),
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
        prop_assert_eq!(&first.0, &expected, "MISS path must yield the stored payload");

        let second = h.compile(&src, "t", &[]).unwrap();
        prop_assert_eq!(h.runs(), 1, "second compile must HIT");
        prop_assert_eq!(&second.0, &expected, "HIT must yield identical payload");
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
    assert_eq!(first.0, expected);

    let second = h.compile(&src, "t", &[]).unwrap();
    assert_eq!(h.runs(), 1, "must HIT");
    assert_eq!(second.0, expected);
}

// ---------------------------------------------------------------------------
// F1: key non-injectivity — NUL separator injection
// ---------------------------------------------------------------------------

/// BUG F1a (B5, confirmed): cache_key hashes `source \0 target \0 ...` without
/// length framing, so a NUL in the source shifts bytes across the field
/// boundary: key("a\0b", "c") == key("a", "b\0c"). The second compile below
/// HITS the first one's entry and is served an artifact that was never
/// compiled for its (source, target) pair. Green test asserts the collision
/// exists; the `#[ignore]` twin asserts the fix.
#[test]
#[serial]
fn key_collision_nul_source_target_boundary() {
    let h = Harness::new();
    let pfx = unique_src("nul-collide");

    let src_a = format!("{pfx}a\0b");
    assert!(h.compile(&src_a, "c", &[]).is_ok());
    assert_eq!(h.runs(), 1);

    let src_b = format!("{pfx}a");
    let res = h.compile(&src_b, "b\0c", &[]);
    // Served from cache: the extractor never ran for this (source, target).
    // (A NUL target can't even be exec'd — Command would error — proving
    // this Ok comes purely from the colliding cache entry.)
    assert!(res.is_ok(), "expected cache HIT via key collision");
    assert_eq!(
        h.runs(),
        1,
        "BUG F1a: distinct (source,target) pairs collided on one cache key"
    );
}

#[test]
#[serial]
#[ignore = "BUG F1a: cache_key is not injective — NUL in source shifts bytes into target (no length framing)"]
fn key_should_separate_source_from_target() {
    let h = Harness::new();
    let pfx = unique_src("nul-collide-fix");
    assert!(h.compile(&format!("{pfx}a\0b"), "c", &[]).is_ok());
    let _ = h.compile(&format!("{pfx}a"), "b\0c", &[]);
    assert_eq!(
        h.runs(),
        2,
        "distinct (source,target) must derive distinct keys"
    );
}

/// BUG F1b (B5, confirmed): same construction across the target/include-list
/// boundary. An include root's path bytes are hashed even when the directory
/// does not exist, so key(s, "t", ["/p"]) == key(s, "t\0/p", []).
#[test]
#[serial]
fn key_collision_target_vs_include_root() {
    let h = Harness::new();
    let pfx = unique_src("nul-include");
    let ghost = "/nonexistent/tidepool-s5-cache-probe";

    assert!(h.compile(&pfx, "t", &[Path::new(ghost)]).is_ok());
    assert_eq!(h.runs(), 1);

    let res = h.compile(&pfx, &format!("t\0{ghost}"), &[]);
    assert!(res.is_ok(), "expected cache HIT via key collision");
    assert_eq!(
        h.runs(),
        1,
        "BUG F1b: target and include-root bytes collided across the field boundary"
    );
}

#[test]
#[serial]
#[ignore = "BUG F1b: cache_key is not injective — target vs include-root boundary (no length framing)"]
fn key_should_separate_target_from_include_roots() {
    let h = Harness::new();
    let pfx = unique_src("nul-include-fix");
    let ghost = "/nonexistent/tidepool-s5-cache-probe";
    assert!(h.compile(&pfx, "t", &[Path::new(ghost)]).is_ok());
    let _ = h.compile(&pfx, &format!("t\0{ghost}"), &[]);
    assert_eq!(h.runs(), 2);
}

// ---------------------------------------------------------------------------
// F2: include-dir ORDER is sorted out of the key, but GHC honors order
// ---------------------------------------------------------------------------

/// BUG F2 (B5, confirmed): cache_key sorts include roots, so [A,B] and [B,A]
/// share a key — but lib.rs passes `--include` flags in the ORIGINAL order,
/// and GHC's search-path order decides which module wins when both dirs
/// provide the same module name. Result: compiling with [B,A] can be served
/// the [A,B] artifact (wrong module resolution) from cache.
#[test]
#[serial]
fn key_insensitive_to_include_order_serves_other_orders_artifact() {
    let h = Harness::new();
    let dir_a = h.path().join("incA");
    let dir_b = h.path().join("incB");
    fs::create_dir_all(&dir_a).unwrap();
    fs::create_dir_all(&dir_b).unwrap();
    // Both dirs provide Lib.hs — a module-shadowing setup where order matters.
    fs::write(dir_a.join("Lib.hs"), "-- A\nlibVal = 1\n").unwrap();
    fs::write(dir_b.join("Lib.hs"), "-- B (different)\nlibVal = 2\n").unwrap();

    let src = unique_src("inc-order");
    let first = h.compile(&src, "t", &[&dir_a, &dir_b]).unwrap();
    assert_eq!(h.runs(), 1);
    assert_eq!(first.0, lit_expr(42));

    // Simulate the toolchain producing a different artifact for the swapped
    // order (as real GHC would under module shadowing).
    h.set_fixture(&lit_expr(43));

    let swapped = h.compile(&src, "t", &[&dir_b, &dir_a]).unwrap();
    assert_eq!(
        h.runs(),
        1,
        "BUG F2: swapped include order HIT the other order's cache entry"
    );
    assert_eq!(
        swapped.0,
        lit_expr(42),
        "BUG F2: artifact for order [A,B] served for order [B,A]"
    );

    // Control: a fresh source with the swapped order really does produce 43,
    // proving the stub fixture switch works and the 42 above came from cache.
    let src2 = unique_src("inc-order-control");
    let fresh = h.compile(&src2, "t", &[&dir_b, &dir_a]).unwrap();
    assert_eq!(h.runs(), 2);
    assert_eq!(fresh.0, lit_expr(43));
}

#[test]
#[serial]
#[ignore = "BUG F2: include order is sorted out of cache_key but passed in-order to GHC where it affects module resolution"]
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
// F3: fingerprints are (path, size, mtime) — content swaps are invisible
// ---------------------------------------------------------------------------

/// Sanity (covered case, passes): the extractor binary IS fingerprinted into
/// the key — a size change invalidates the cache. This is the fix for the
/// historical "stale cache after toolchain upgrade" (#313-class) footgun.
#[test]
#[serial]
fn staleness_binary_size_change_invalidates() {
    let h = Harness::new();
    let src = unique_src("bin-size");

    assert!(h.compile(&src, "t", &[]).is_ok());
    assert_eq!(h.runs(), 1);

    // "Upgrade" the toolchain: append to the stub (size + mtime change).
    let mut script = fs::read_to_string(h.stub()).unwrap();
    script.push_str("# upgraded\n");
    fs::write(h.stub(), script).unwrap();
    fs::set_permissions(h.stub(), fs::Permissions::from_mode(0o755)).unwrap();

    assert!(h.compile(&src, "t", &[]).is_ok());
    assert_eq!(
        h.runs(),
        2,
        "binary size/mtime change must MISS (toolchain fingerprint works)"
    );
}

/// BUG F3a (B5 staleness, confirmed): the fingerprint never hashes binary
/// CONTENT. A same-length content swap with restored mtime (realistic: nix
/// store normalizes all mtimes to epoch+1; `cp -p`; `rsync -t`) leaves the
/// key unchanged, and the cache silently serves artifacts from the OLD
/// toolchain. This is exactly the #313 footgun shape surviving the fix.
#[test]
#[serial]
fn staleness_binary_content_swap_same_size_mtime_served_stale() {
    let h = Harness::new();
    let src = unique_src("bin-swap");

    let first = h.compile(&src, "t", &[]).unwrap();
    assert_eq!(h.runs(), 1);
    assert_eq!(first.0, lit_expr(42));

    // Swap which fixture the "toolchain" produces: a.cbor -> b.cbor is a
    // same-length edit; restore the stub's mtime afterwards.
    let script = fs::read_to_string(h.stub()).unwrap();
    let swapped = script.replace("/fx/a.cbor", "/fx/b.cbor");
    assert_eq!(
        script.len(),
        swapped.len(),
        "perturbation must preserve size"
    );
    swap_content_preserving_mtime(&h.stub(), swapped.as_bytes());
    fs::set_permissions(h.stub(), fs::Permissions::from_mode(0o755)).unwrap();

    let second = h.compile(&src, "t", &[]).unwrap();
    assert_eq!(
        h.runs(),
        1,
        "BUG F3a: content-swapped toolchain (same size+mtime) HIT the stale entry"
    );
    assert_eq!(
        second.0,
        lit_expr(42),
        "BUG F3a: stale artifact served; new toolchain would produce Lit 43"
    );

    // Control: a fresh source proves the swapped toolchain now emits 43.
    let src2 = unique_src("bin-swap-control");
    let fresh = h.compile(&src2, "t", &[]).unwrap();
    assert_eq!(fresh.0, lit_expr(43));
}

#[test]
#[serial]
#[ignore = "BUG F3a: binary fingerprint is (path,size,mtime) only — content swap with preserved size+mtime serves stale Core (nix epoch-mtime hazard)"]
fn key_should_change_when_binary_content_changes() {
    let h = Harness::new();
    let src = unique_src("bin-swap-fix");
    assert!(h.compile(&src, "t", &[]).is_ok());
    let script = fs::read_to_string(h.stub()).unwrap();
    let swapped = script.replace("/fx/a.cbor", "/fx/b.cbor");
    swap_content_preserving_mtime(&h.stub(), swapped.as_bytes());
    fs::set_permissions(h.stub(), fs::Permissions::from_mode(0o755)).unwrap();
    assert!(h.compile(&src, "t", &[]).is_ok());
    assert_eq!(h.runs(), 2, "binary content change must MISS");
}

/// BUG F3b (B5 staleness, confirmed): same gap for include-dir .hs files —
/// a same-size content edit with restored mtime is invisible to
/// `fingerprint_dir`, so stale Core is served for changed dependency source.
#[test]
#[serial]
fn staleness_hs_content_swap_same_size_mtime_served_stale() {
    let h = Harness::new();
    let inc = h.path().join("inc");
    fs::create_dir_all(&inc).unwrap();
    let lib = inc.join("Lib.hs");
    fs::write(&lib, "libVal = 1\n").unwrap();
    let src = unique_src("hs-swap");

    assert!(h.compile(&src, "t", &[&inc]).is_ok());
    assert_eq!(h.runs(), 1);

    swap_content_preserving_mtime(&lib, b"libVal = 2\n"); // same length
    assert!(h.compile(&src, "t", &[&inc]).is_ok());
    assert_eq!(
        h.runs(),
        1,
        "BUG F3b: same-size+mtime .hs edit HIT the stale entry"
    );
}

#[test]
#[serial]
#[ignore = "BUG F3b: include-dir fingerprint is (path,size,mtime) only — same-size+mtime .hs edits serve stale Core"]
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

/// BUG F4 (B5 staleness, confirmed): `fingerprint_dir` uses
/// `DirEntry::metadata()`, which does NOT traverse symlinks. A symlinked .hs
/// file (common in nix / symlink-farm setups) is fingerprinted by the LINK's
/// size/mtime, so ANY edit to the real file — even with new size and mtime —
/// leaves the key unchanged and serves stale Core.
#[test]
#[serial]
fn staleness_symlinked_hs_target_edit_served_stale() {
    let h = Harness::new();
    let ext = h.path().join("ext");
    let inc = h.path().join("inc");
    fs::create_dir_all(&ext).unwrap();
    fs::create_dir_all(&inc).unwrap();
    let real = ext.join("Real.hs");
    fs::write(&real, "v1\n").unwrap();
    std::os::unix::fs::symlink(&real, inc.join("Lib.hs")).unwrap();
    let src = unique_src("symlink-hs");

    assert!(h.compile(&src, "t", &[&inc]).is_ok());
    assert_eq!(h.runs(), 1);

    // Edit the real file: new content, new size, new mtime.
    fs::write(&real, "v2 with a much longer body\n").unwrap();

    assert!(h.compile(&src, "t", &[&inc]).is_ok());
    assert_eq!(
        h.runs(),
        1,
        "BUG F4: edit behind a symlinked .hs HIT the stale entry (lstat fingerprint)"
    );
}

#[test]
#[serial]
#[ignore = "BUG F4: fingerprint_dir lstats symlinked .hs files — edits to the link target serve stale Core"]
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
// F5: wrapper-script exec parser misses quoted targets
// ---------------------------------------------------------------------------

/// Sanity (covered case, passes): an UNQUOTED absolute exec target in a
/// wrapper script is followed and fingerprinted — this matches the real
/// `~/.cargo/bin/tidepool-extract` wrapper today.
#[test]
#[serial]
fn staleness_unquoted_wrapper_target_followed() {
    let mut h = Harness::new();
    h.install_wrapper(false);
    let src = unique_src("wrapper-unquoted");

    assert!(h.compile(&src, "t", &[]).is_ok());
    assert_eq!(h.runs(), 1);

    // Upgrade the delegate binary (size + mtime change), wrapper untouched.
    let mut script = fs::read_to_string(h.stub()).unwrap();
    script.push_str("# upgraded\n");
    fs::write(h.stub(), script).unwrap();
    fs::set_permissions(h.stub(), fs::Permissions::from_mode(0o755)).unwrap();

    assert!(h.compile(&src, "t", &[]).is_ok());
    assert_eq!(
        h.runs(),
        2,
        "delegate change behind unquoted wrapper must MISS"
    );
}

/// BUG F5 (B5 staleness, confirmed): `extract_exec_target` only accepts a
/// bare token starting with '/'. A QUOTED exec target — `exec "/path" "$@"`,
/// the shellcheck-recommended form — is not followed, so upgrading the
/// delegate binary behind such a wrapper silently serves stale Core. (Lines
/// using `$HOME`, `env`, or any `=` are rejected the same way.)
#[test]
#[serial]
fn staleness_quoted_wrapper_target_not_fingerprinted() {
    let mut h = Harness::new();
    h.install_wrapper(true);
    let src = unique_src("wrapper-quoted");

    assert!(h.compile(&src, "t", &[]).is_ok());
    assert_eq!(h.runs(), 1);

    let mut script = fs::read_to_string(h.stub()).unwrap();
    script.push_str("# upgraded\n");
    fs::write(h.stub(), script).unwrap();
    fs::set_permissions(h.stub(), fs::Permissions::from_mode(0o755)).unwrap();

    assert!(h.compile(&src, "t", &[]).is_ok());
    assert_eq!(
        h.runs(),
        1,
        "BUG F5: delegate change behind QUOTED wrapper HIT the stale entry"
    );
}

#[test]
#[serial]
#[ignore = "BUG F5: extract_exec_target ignores quoted exec targets — delegate upgrades behind quoted wrappers serve stale Core"]
fn key_should_follow_quoted_wrapper_targets() {
    let mut h = Harness::new();
    h.install_wrapper(true);
    let src = unique_src("wrapper-quoted-fix");
    assert!(h.compile(&src, "t", &[]).is_ok());
    let mut script = fs::read_to_string(h.stub()).unwrap();
    script.push_str("# upgraded\n");
    fs::write(h.stub(), script).unwrap();
    fs::set_permissions(h.stub(), fs::Permissions::from_mode(0o755)).unwrap();
    assert!(h.compile(&src, "t", &[]).is_ok());
    assert_eq!(
        h.runs(),
        2,
        "delegate change must MISS regardless of quoting"
    );
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

/// (4): corruption matrix — {cbor, meta, ok} x {delete, truncate, flips,
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
    for file_idx in 0..3usize {
        for &op in &ops {
            let h = Harness::new();
            let src = unique_src("corrupt");
            let original = h.compile(&src, "t", &[]).unwrap();
            assert_eq!(h.runs(), 1);
            let (cbor, meta, ok) = h.entry_paths();
            let target = [&cbor, &meta, &ok][file_idx];
            apply_corruption(target, op);

            // Must not panic; must not serve divergent data silently.
            let res = h.compile(&src, "t", &[]);
            let reran = h.runs() == 2;
            match res {
                Ok(r) => assert!(
                    reran || r.0 == original.0,
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
/// sequence is: remove .ok -> persist .cbor -> persist .meta.cbor -> write
/// .ok. A crash at any point leaves no sentinel, so every prefix state must
/// be a MISS. Conversely a sentinel with missing payload files must also be
/// a MISS (read failure), never a panic.
#[test]
#[serial]
fn partial_write_states_are_misses() {
    // States: (keep_cbor, keep_meta, keep_ok)
    let states = [
        (true, false, false), // crashed after persisting expr
        (true, true, false),  // crashed before writing sentinel
        (false, false, true), // payloads lost, sentinel intact
        (false, true, true),  // expr lost, sentinel intact
        (true, false, true),  // meta lost, sentinel intact
    ];
    for &(keep_cbor, keep_meta, keep_ok) in &states {
        let h = Harness::new();
        let src = unique_src("partial");
        let original = h.compile(&src, "t", &[]).unwrap();
        assert_eq!(h.runs(), 1);
        let (cbor, meta, ok) = h.entry_paths();
        if !keep_cbor {
            fs::remove_file(&cbor).unwrap();
        }
        if !keep_meta {
            fs::remove_file(&meta).unwrap();
        }
        if !keep_ok {
            fs::remove_file(&ok).unwrap();
        }

        let res = h.compile(&src, "t", &[]).unwrap_or_else(|e| {
            panic!("partial state ({keep_cbor},{keep_meta},{keep_ok}) errored: {e}")
        });
        assert_eq!(
            h.runs(),
            2,
            "partial state ({keep_cbor},{keep_meta},{keep_ok}) must be a MISS"
        );
        assert_eq!(res.0, original.0, "recompile must restore the artifact");
    }
}

/// Sentinel content is ignored — only its existence is checked. Garbage in
/// .ok still validates the entry (documented; harmless today, but it means
/// the natural home for an integrity hash is currently unused).
#[test]
#[serial]
fn sentinel_content_is_ignored() {
    let h = Harness::new();
    let src = unique_src("sentinel");
    let original = h.compile(&src, "t", &[]).unwrap();
    let (_, _, ok) = h.entry_paths();
    fs::write(&ok, b"garbage-not-a-checksum").unwrap();
    let res = h.compile(&src, "t", &[]).unwrap();
    assert_eq!(
        h.runs(),
        1,
        "entry still HITs with garbage sentinel content"
    );
    assert_eq!(res.0, original.0);
}

// ---------------------------------------------------------------------------
// F6: no integrity check — surviving bit-flips are served as valid
// ---------------------------------------------------------------------------

/// BUG F6 (B1, confirmed): there is no checksum binding the cached payloads.
/// The .ok sentinel guards COMPLETENESS only. A single bit-flip in a value
/// byte of the cached .cbor still decodes as a VALID CoreExpr — for a
/// different program — and is served as a cache hit with no recompile. The
/// test scans for such a surviving flip (the LitInt value byte qualifies)
/// and proves it is served. Same applies to meta.cbor (e.g. the has_io
/// warning bit, which gates IOTypeDetected rejection downstream).
#[test]
#[serial]
fn corruption_bitflip_served_as_valid_different_program() {
    let h = Harness::new();
    let src = unique_src("bitflip");
    let original = h.compile(&src, "t", &[]).unwrap();
    assert_eq!(h.runs(), 1);
    let (cbor, _, _) = h.entry_paths();
    let bytes = fs::read(&cbor).unwrap();

    // Find a flip that the consumer decoder accepts but that changes meaning.
    let mut corrupted: Option<Vec<u8>> = None;
    'outer: for i in (0..bytes.len()).rev() {
        for bit in 0..8u8 {
            let mut m = bytes.clone();
            m[i] ^= 1 << bit;
            if let Ok(t) = read_cbor(&m) {
                if t != original.0 {
                    corrupted = Some(m);
                    break 'outer;
                }
            }
        }
    }
    let corrupted = corrupted
        .expect("no surviving bit-flip found — integrity may have been added (re-evaluate F6)");
    fs::write(&cbor, &corrupted).unwrap();

    let served = h.compile(&src, "t", &[]).unwrap();
    assert_eq!(
        h.runs(),
        1,
        "BUG F6: bit-flipped entry was served as a HIT (no integrity check)"
    );
    assert_ne!(
        served.0, original.0,
        "BUG F6: corrupted payload decoded to a DIFFERENT program and was served as valid"
    );
}

#[test]
#[serial]
#[ignore = "BUG F6: cached payloads carry no checksum — bit-flips that survive CBOR decoding are served as a different, 'valid' program"]
fn corrupted_payload_should_be_rejected_or_recompiled() {
    let h = Harness::new();
    let src = unique_src("bitflip-fix");
    let original = h.compile(&src, "t", &[]).unwrap();
    let (cbor, _, _) = h.entry_paths();
    let bytes = fs::read(&cbor).unwrap();
    let mut found = None;
    'outer: for i in (0..bytes.len()).rev() {
        for bit in 0..8u8 {
            let mut m = bytes.clone();
            m[i] ^= 1 << bit;
            if let Ok(t) = read_cbor(&m) {
                if t != original.0 {
                    found = Some(m);
                    break 'outer;
                }
            }
        }
    }
    if let Some(m) = found {
        fs::write(&cbor, m).unwrap();
        let served = h.compile(&src, "t", &[]).unwrap();
        assert!(
            h.runs() == 2 || served.0 == original.0,
            "corrupted cache payload must never be served as a different program"
        );
    }
}

// ---------------------------------------------------------------------------
// F7 (REFUTED — verified negative): symlink cycle in an include dir
// ---------------------------------------------------------------------------

/// VERIFIED NEGATIVE F7: `fingerprint_dir` recurses via `path.is_dir()`,
/// which follows symlinks, so a self-referencing symlink (`inc/loop -> inc`)
/// LOOKS like unbounded recursion. In practice the kernel bounds it: each
/// recursion level adds a symlink component to the path, and path resolution
/// fails with ELOOP after ~40 symlink traversals (and PATH_MAX bounds
/// physical nesting), so `read_dir` errors and the walker unwinds gracefully.
/// This test pins that accidental safety net as a regression guard.
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
