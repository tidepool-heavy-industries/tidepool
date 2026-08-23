//! Filesystem caching for compiled artifacts.

use std::ffi::{OsStr, OsString};
use std::fs;
use std::path::{Path, PathBuf};

/// Returns the cache directory for Tidepool's compiled-artifact memos.
/// Delegates to the canonical resolver ([`crate::paths::compile_cache_dir`]),
/// which is [`crate::paths::cache_dir`] unless `$TIDEPOOL_COMPILE_CACHE_DIR`
/// redirects the memo (and the `binfp-*` sidecars) somewhere shared; wrapped
/// in `Some` since every call site uses `?`/`Option` combinators.
fn cache_dir() -> Option<PathBuf> {
    Some(crate::paths::compile_cache_dir())
}

/// A content-addressed cache key: the blake3 hex digest of a compilation
/// request. A newtype so a raw string can't be mistaken for a computed key at
/// the [`cache_load`]/[`cache_store`] boundary (the digest also names the
/// on-disk artifact files).
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct CacheKey(String);

impl std::fmt::Display for CacheKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// Computes a unique cache key for a compilation request.
/// The key includes the source code, the target binder, and a fingerprint of
/// all include directories to ensure cache invalidation when dependencies change.
/// The unsalted cache key (the common eval path). Retained as the thin,
/// named entry point used by the cache tests; production calls go through
/// [`cache_key_salted`].
#[cfg_attr(not(test), allow(dead_code))]
pub(crate) fn cache_key(source: &str, target: &str, include: &[&Path]) -> CacheKey {
    cache_key_salted(source, target, include, None)
}

/// As [`cache_key`], but additionally mixes an optional `salt` into the key.
/// Sessions pass `Some("session:<id>:gen:<g>")` so two sessions' identical-text
/// `Lib.G<g>` modules never share an entry and a generation bump invalidates
/// correctly. `None` must reproduce `cache_key`'s output byte-for-byte, so
/// existing non-session cache entries stay valid.
pub(crate) fn cache_key_salted(
    source: &str,
    target: &str,
    include: &[&Path],
    salt: Option<&str>,
) -> CacheKey {
    let mut hasher = blake3::Hasher::new();
    // Length-prefixed framing: NUL separators alone let a NUL embedded in one
    // field shift bytes across the boundary (key("a\0b","c") == key("a","b\0c")),
    // serving the wrong artifact.
    frame(&mut hasher, source.as_bytes());
    frame(&mut hasher, target.as_bytes());
    // Salt is framed only when present, so a None call hashes identically to the
    // pre-salt key (no mass cache invalidation for ordinary evals).
    if let Some(s) = salt {
        frame(&mut hasher, b"session-salt");
        frame(&mut hasher, s.as_bytes());
    }

    // Fingerprint include directories in their ORIGINAL order: GHC receives
    // `--include` flags in argument order, and search-path order decides
    // module shadowing — [A,B] and [B,A] are different compilations and must
    // not share a key.
    frame(&mut hasher, &(include.len() as u64).to_le_bytes());
    for root in include {
        frame(&mut hasher, root.as_os_str().as_encoded_bytes());
        fingerprint_dir(root, &mut hasher);
    }

    extract_binary_fingerprint(&mut hasher);

    CacheKey(hasher.finalize().to_hex().to_string())
}

/// Hash a length-prefixed field: unambiguous framing regardless of content.
fn frame(hasher: &mut blake3::Hasher, bytes: &[u8]) {
    hasher.update(&(bytes.len() as u64).to_le_bytes());
    hasher.update(bytes);
}

/// Fingerprints the compiler binary to ensure cache invalidation on upgrades.
/// If the resolved path is a shell wrapper script (e.g. ~/.cargo/bin/tidepool-extract),
/// also fingerprints the target binary it delegates to (e.g. ~/.local/bin/tidepool-extract-bin).
///
/// The binary is located by [`crate::toolchain::locate_extract`] — which
/// delegates to `tidepool-extract-cmd`, the crate that also SPAWNS it — so the
/// cache key, the spawn, and the startup handshake all fingerprint the SAME
/// file. A misconfigured toolchain contributes nothing here and fails loudly at
/// spawn/startup instead.
fn extract_binary_fingerprint(hasher: &mut blake3::Hasher) {
    if let Ok(loc) = crate::toolchain::locate_extract() {
        let path = loc.path;
        fingerprint_single_binary(hasher, &path);
        for target in wrapper_targets(&path) {
            fingerprint_single_binary(hasher, &target);
        }
    }
}

/// If `path` is a short shell wrapper script, the absolute binaries it `exec`s.
/// Empty for a real binary. An unfollowed wrapper target means delegate binary
/// upgrades silently serve stale Core — see [`extract_exec_target`] for the
/// known gap in what a text scanner can resolve.
pub(crate) fn wrapper_targets(path: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let Ok(contents) = fs::read_to_string(path) else {
        return out;
    };
    if contents.len() >= 4096 || !(contents.starts_with("#!") || contents.contains("exec ")) {
        return out;
    }
    for line in contents.lines() {
        if let Some(target) = extract_exec_target(line.trim()) {
            let target_path = PathBuf::from(target);
            if target_path.exists() {
                if let Ok(resolved) = fs::canonicalize(&target_path) {
                    out.push(resolved);
                }
            }
        }
    }
    out
}

/// Fingerprints a single binary by path and CONTENT hash: the path is framed
/// into `hasher` (so the same content at two paths is two cache keys), then the
/// memoized [`binary_content_hash`].
fn fingerprint_single_binary(hasher: &mut blake3::Hasher, path: &Path) {
    frame(hasher, path.as_os_str().as_encoded_bytes());
    hasher.update(&binary_content_hash(path));
}

/// Memoized blake3 of a binary's CONTENT — no path mixed in, so callers that
/// must compare the same binary across install locations (the toolchain
/// handshake) get a stable value.
///
/// (size, mtime) alone is blind to same-size content swaps, and the nix store
/// normalizes ALL mtimes to epoch+1, so for nix-deployed toolchains only
/// content distinguishes versions. Content is blake3-hashed, memoized per
/// (path, size, dev, ino, ctime) so each binary is read once per change per
/// process (~100ms for a GHC-sized binary, amortized to zero), and once per
/// change per MACHINE via the sidecar below.
///
/// An unreadable path hashes to all-zeroes: a missing binary is a distinct,
/// stable value rather than a panic or a silently-skipped input.
pub(crate) fn binary_content_hash(path: &Path) -> [u8; 32] {
    use std::collections::HashMap;
    use std::sync::OnceLock;

    use parking_lot::Mutex;

    tidepool_codegen::debug::init_logging();

    // Memo key includes (dev, ino, ctime): mtime/size are user-settable and
    // preserved by adversarial in-place swaps, but ANY write bumps ctime and
    // no userspace tool can reset it — the tamper-evident field. A same-size
    // same-mtime in-place content swap therefore still re-hashes.
    type MemoKey = (PathBuf, u64, u64, u64, i64, i64);
    static MEMO: OnceLock<Mutex<HashMap<MemoKey, [u8; 32]>>> = OnceLock::new();

    let Ok(meta) = fs::metadata(path) else {
        return [0u8; 32];
    };
    let key: MemoKey = {
        use std::os::unix::fs::MetadataExt;
        (
            path.to_path_buf(),
            meta.len(),
            meta.dev(),
            meta.ino(),
            meta.ctime(),
            meta.ctime_nsec(),
        )
    };
    let memo = MEMO.get_or_init(|| Mutex::new(HashMap::new()));
    let cached = memo.lock().get(&key).copied();
    log::debug!(
        target: "tidepool::fp",
        "path={} key=({},{},{},{},{}) memo_hit={}",
        path.display(),
        key.1,
        key.2,
        key.3,
        key.4,
        key.5,
        cached.is_some()
    );
    let content_hash = match cached {
        Some(h) => h,
        None => {
            // Cross-PROCESS memo: subprocess-per-case test suites spawn
            // hundreds of short-lived processes, and re-hashing a ~79MB
            // GHC-linked binary per process is ~30-50ms each. Persist the
            // content hash in a sidecar keyed by the stat identity (the same
            // tamper-evident (dev, ino, ctime) key as the in-process memo),
            // so the whole machine hashes each binary version exactly once.
            let stat_tag = {
                let mut kh = blake3::Hasher::new();
                kh.update(path.as_os_str().as_encoded_bytes());
                kh.update(&meta.len().to_le_bytes());
                {
                    use std::os::unix::fs::MetadataExt;
                    kh.update(&meta.dev().to_le_bytes());
                    kh.update(&meta.ino().to_le_bytes());
                    kh.update(&meta.ctime().to_le_bytes());
                    kh.update(&meta.ctime_nsec().to_le_bytes());
                }
                kh.finalize().to_hex().to_string()
            };
            let sidecar = cache_dir().map(|d| d.join(format!("binfp-{stat_tag}")));
            let from_disk = sidecar.as_ref().and_then(|p| {
                let bytes = fs::read(p).ok()?;
                <[u8; 32]>::try_from(bytes.as_slice()).ok()
            });
            log::debug!(
                target: "tidepool::fp",
                "stat_tag={} sidecar_hit={}",
                stat_tag,
                from_disk.is_some()
            );
            let h: [u8; 32] = match from_disk {
                Some(h) => h,
                None => {
                    let h: [u8; 32] = match fs::read(path) {
                        Ok(bytes) => *blake3::hash(&bytes).as_bytes(),
                        // Unreadable: degrade to the metadata-only fingerprint
                        // rather than poisoning the key entirely.
                        Err(_) => {
                            let mut mh = blake3::Hasher::new();
                            mh.update(&meta.len().to_le_bytes());
                            if let Ok(mtime) = meta.modified() {
                                if let Ok(dur) = mtime.duration_since(std::time::UNIX_EPOCH) {
                                    mh.update(&dur.as_nanos().to_le_bytes());
                                }
                            }
                            *mh.finalize().as_bytes()
                        }
                    };
                    if let Some(p) = &sidecar {
                        if let Some(parent) = p.parent() {
                            let _ = fs::create_dir_all(parent);
                        }
                        let _ = fs::write(p, h);
                    }
                    h
                }
            };
            memo.lock().insert(key, h);
            h
        }
    };
    content_hash
}

/// Extracts an absolute path from a shell exec line.
/// Handles `exec /path/to/bin "$@"`, bare `/path/to/bin "$@"`, and QUOTED
/// targets — `exec "/path/to/bin" "$@"` (the shellcheck-recommended form): an
/// unfollowed wrapper target means delegate binary upgrades silently serve
/// stale Core.
///
/// KNOWN GAP: a target spelled via a shell variable or relative path
/// (`exec "$DIR/bin"`, `exec ./bin`) is NOT resolved — only a literal
/// absolute path is followed. Fixing this in general requires interpreting
/// shell variable assignment, which is unbounded — a small text scanner
/// cannot soundly evaluate arbitrary shell. Direct-path wrappers (the common
/// case for nix/cargo-installed binaries) ARE followed; only the
/// variable/relative-path spelling silently misses a delegate-only upgrade.
fn extract_exec_target(line: &str) -> Option<&str> {
    let line = line.strip_prefix("exec ").unwrap_or(line);
    if line.is_empty() || line.starts_with('#') {
        return None;
    }
    let token = line.split_whitespace().next()?;
    // Strip one layer of matching quotes.
    let token = token
        .strip_prefix('"')
        .and_then(|t| t.strip_suffix('"'))
        .or_else(|| token.strip_prefix('\'').and_then(|t| t.strip_suffix('\'')))
        .unwrap_or(token);
    // Reject env-var assignments (FOO=bar cmd) but not quoted paths that
    // happen to contain '=' AFTER unquoting was already handled above.
    if token.contains('=') {
        return None;
    }
    if token.starts_with('/') {
        Some(token)
    } else {
        None
    }
}

/// Recursively walks a directory to fingerprint its contents.
/// Hashes the CONTENT (not size/mtime — see the content-hash note below) of
/// every `.hs` and `.hs-boot` file, keyed by path.
fn fingerprint_dir(dir: &Path, hasher: &mut blake3::Hasher) {
    let mut visited = std::collections::HashSet::new();
    fingerprint_dir_inner(dir, hasher, &mut visited);
}

/// `visited` holds the CANONICALIZED path of every directory already walked:
/// `path.is_dir()` follows symlinks, so a directory symlink under an include
/// dir that (directly or transitively) points back at an ancestor would
/// otherwise recurse forever. Canonicalizing and checking membership before
/// descending breaks the cycle (and, as a side effect, a diamond of two
/// symlinks to the same real directory is only hashed once).
fn fingerprint_dir_inner(
    dir: &Path,
    hasher: &mut blake3::Hasher,
    visited: &mut std::collections::HashSet<PathBuf>,
) {
    if let Ok(canon) = fs::canonicalize(dir) {
        if !visited.insert(canon) {
            return;
        }
    }

    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    let mut paths: Vec<_> = entries.filter_map(std::result::Result::ok).collect();
    paths.sort_by_key(std::fs::DirEntry::path);

    for entry in paths {
        let path = entry.path();
        if path.is_dir() {
            fingerprint_dir_inner(&path, hasher, visited);
            continue;
        }
        let Some(ext) = path.extension() else {
            continue;
        };
        if ext != "hs" && ext != "hs-boot" {
            continue;
        }
        // Content hash: (size, mtime) misses same-size edits, and
        // `DirEntry::metadata()` is lstat — fingerprinting a symlinked .hs by
        // the LINK's metadata would miss edits to the real file. `fs::read`
        // follows symlinks and hashes what GHC will actually compile. Source
        // files are small; no memo needed.
        frame(hasher, path.as_os_str().as_encoded_bytes());
        match fs::read(&path) {
            Ok(bytes) => frame(hasher, blake3::hash(&bytes).as_bytes()),
            Err(_) => frame(hasher, b"<unreadable>"),
        }
    }
}

/// Sentinel payload: blake3(expr_bytes) || blake3(meta_bytes), 64 raw bytes.
/// Anything else (missing, empty, wrong length — an old-format entry from
/// before this checksum existed) is treated as absent, forcing a MISS.
const SENTINEL_LEN: usize = 64;

/// Attempts to load the Core expression and metadata from the cache.
/// Returns `Some((expr_bytes, meta_bytes))` on success.
/// Beyond mere sentinel existence (completeness), the sentinel's two blake3
/// digests are recomputed over the loaded bytes and compared: a bit-flip that
/// still decodes as valid CBOR would otherwise be served as a different
/// program, so a checksum mismatch falls through to a MISS/recompile instead.
pub(crate) fn cache_load(key: &CacheKey) -> Option<(Vec<u8>, Vec<u8>)> {
    let dir = cache_dir()?;
    let sentinel_path = dir.join(format!("{}.ok", key));
    let sentinel = fs::read(&sentinel_path).ok()?;
    if sentinel.len() != SENTINEL_LEN {
        return None;
    }

    let expr_path = dir.join(format!("{}.cbor", key));
    let meta_path = dir.join(format!("{}.meta.cbor", key));

    let expr = fs::read(&expr_path).ok()?;
    let meta = fs::read(&meta_path).ok()?;

    if blake3::hash(&expr).as_bytes() != &sentinel[0..32]
        || blake3::hash(&meta).as_bytes() != &sentinel[32..64]
    {
        return None;
    }

    Some((expr, meta))
}

/// Stores the compilation results in the cache. Each file is replaced atomically
/// via rename. A sentinel file `{key}.ok` is written last to mark the entry as
/// complete — `cache_load` checks for this before reading. The sentinel body is
/// blake3(expr_bytes) || blake3(meta_bytes), letting `cache_load` detect a
/// bit-flip that still decodes as plausible CBOR.
pub(crate) fn cache_store(key: &CacheKey, expr_bytes: &[u8], meta_bytes: &[u8]) {
    let Some(dir) = cache_dir() else { return };
    if fs::create_dir_all(&dir).is_err() {
        return;
    }

    use std::io::Write;

    let Ok(mut tmp_expr) = tempfile::NamedTempFile::new_in(&dir) else {
        return;
    };
    let Ok(mut tmp_meta) = tempfile::NamedTempFile::new_in(&dir) else {
        return;
    };

    if tmp_expr.write_all(expr_bytes).is_err() {
        return;
    }
    if tmp_meta.write_all(meta_bytes).is_err() {
        return;
    }

    let final_expr = dir.join(format!("{}.cbor", key));
    let final_meta = dir.join(format!("{}.meta.cbor", key));
    let sentinel = dir.join(format!("{}.ok", key));

    // Remove sentinel first — marks the entry as incomplete during update.
    let _ = fs::remove_file(&sentinel);

    if tmp_expr.persist(&final_expr).is_err() {
        return;
    }
    if tmp_meta.persist(&final_meta).is_err() {
        return;
    }

    // Sentinel written last — entry is only valid when this exists. Its body
    // binds the checksums, not just completeness.
    let mut checksum = [0u8; SENTINEL_LEN];
    checksum[0..32].copy_from_slice(blake3::hash(expr_bytes).as_bytes());
    checksum[32..64].copy_from_slice(blake3::hash(meta_bytes).as_bytes());
    let _ = fs::write(&sentinel, checksum);
}

// ---------------------------------------------------------------------------
// Invocation-keyed artifact sets
//
// The second consumer of this module. `compile_haskell` above memoizes ONE
// eval compile as a fixed (expr, meta) pair keyed by (source, target,
// includes-by-path, binary). `crate::artifacts::compile_targets` needs a
// memo for a whole `tidepool-extract` INVOCATION: N targets, a variable artifact set
// (per-target Core, one shared meta, an asks sidecar whose very FILENAME
// depends on the target count), and a key that survives the same content
// appearing under a different absolute path. Rather than fork the fingerprint/
// staleness discipline solved above, that shape is expressed here, over the
// same primitives. See `plans/compile-memo.md`.
// ---------------------------------------------------------------------------

/// Leads an [`InvocationKey`]'s hash, so an invocation key can never collide
/// with an eval [`CacheKey`] — the two name DIFFERENT artifact sets under the
/// same `<key>.*` filenames, and a collision would serve one caller the
/// other's bytes. The eval key's own bytes are untouched by this module: no
/// mass invalidation of anyone's `~/.cache/tidepool`.
const INVOCATION_NAMESPACE: &[u8] = b"tidepool-invocation-artifacts-v1";

/// A content-addressed key for a COMPLETE `tidepool-extract` invocation.
/// A newtype for the same reason [`CacheKey`] is one — it also names the
/// on-disk artifact files.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InvocationKey(String);

impl std::fmt::Display for InvocationKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// Everything about a `tidepool-extract` invocation that can reach its output
/// bytes. Built by the caller from the very `ExtractCmd` it is about to run,
/// so the key describes the invocation that actually happens.
pub struct Invocation<'a> {
    /// The module source, by CONTENT. Never the path — the on-disk
    /// `<Module>.hs` lives in a per-invocation tempdir.
    pub source: &'a str,
    /// The built argv, exactly as `ExtractCmd::argv()` returns it. Walked
    /// against an allowlist (see [`invocation_key`]).
    pub argv: &'a [OsString],
    /// The positional input path in `argv`, so the walk can recognize (and
    /// drop) it rather than treating it as an unknown argument.
    pub input_path: &'a Path,
    /// `--include` roots in ORIGINAL order. Fingerprinted by CONTENT, with
    /// paths RELATIVE to each root — the absolute location is deliberately not
    /// keyed.
    pub include: &'a [PathBuf],
    /// The binary this invocation will spawn. Fingerprinted by content
    /// (following wrapper `exec` targets); resolved through `$PATH` first if
    /// it is a bare name.
    pub bin: &'a Path,
    /// A single session `Val` module permitted as a CACHEABLE `--inject-val`
    /// target, despite `--session-root`/`--inject-val` otherwise making an
    /// invocation uncacheable (see [`invocation_key`]'s doc, hazard (b) in
    /// `plans/compile-memo.md`). This is the harness driver's ONE stable,
    /// never-rotating "harness context" module
    /// (`plans/turn-latency-state-injection.md`): its NAME and TYPE never
    /// change turn to turn — only the heap value a later run resolves it to
    /// does, and the iface never encodes a value — so the compiled artifact
    /// really is independent of which turn produced it, which is what makes
    /// this safe to memoize.
    ///
    /// `None` (the default) means every `--inject-val`/`--session-root` in
    /// `argv` still makes the invocation uncacheable, exactly as before this
    /// field existed. When `Some`, the walk accepts `--session-root <dir>`
    /// unconditionally (like `--output-dir`) and `--inject-val <module>` ONLY
    /// when its value equals this module's name — any OTHER `--inject-val`
    /// value (a real, generation-numbered `Val.G<g>` session bind) still
    /// makes the invocation uncacheable. The accepted `--session-root`'s
    /// VALUE, read out of `argv` itself (never supplied separately), locates
    /// the `.hi` iface this module's CONTENT is fingerprinted from — so the
    /// fingerprinted path can never drift from what the invocation actually
    /// reads.
    pub stable_val: Option<tidepool_repr::SessionModule>,
}

/// Compute the key for an invocation, or `None` when the invocation is
/// **uncacheable** and must be compiled cold.
///
/// The argv walk is an ALLOWLIST, and that is the point: it makes "the key
/// covers every input that affects the output" a structural property instead
/// of a standing obligation to remember. Recognized elements are
///
/// - `--output-dir <dir>` — dropped. Per-invocation; where the bytes are
///   written cannot change what they are.
/// - `--build-products-dir <dir>` — dropped, same reasoning as `--output-dir`:
///   it only ever points GHC's OWN `hiDir`/`objectDir` at a warm-cache
///   location so `checkOldIface` can skip an unchanged home module — a
///   directory whose CONTENT never changes what the extract PRODUCES, only
///   how much frontend work it redoes to produce it (spike-verified:
///   `plans/turn-latency-state-injection.md`; a cold-dir and a warm-dir
///   compile of the same source/argv/include/binary are asserted
///   byte-identical by `build_products_dir_is_deterministic` below). Its
///   mutable CONTENTS are therefore never hashed into the key either — doing
///   so would cost a walk of the whole warm dir for a property this
///   determinism argument already gives for free.
/// - `--include <dir>` — dropped HERE and content-fingerprinted below.
/// - `--target <name>` / `--targets <a,b>` — keyed verbatim, in order. The
///   target list decides what is compiled, and (via `targets.len() > 1`)
///   which asks-sidecar shape the extract writes.
/// - the positional input, iff it equals `input_path` — dropped (its CONTENT
///   is keyed as `source`).
///
/// **Anything else makes the invocation uncacheable.** A flag added to
/// `ExtractCmd` tomorrow and threaded into a calling site does not ride along
/// unkeyed; it goes cold until someone classifies it. The failure direction is
/// a miss, never a false hit. This is also how session-scope compiles
/// (`--session-bind`/`--inject-val`/`--session-root`, which read per-session
/// MUTABLE directories nothing here fingerprints) are excluded: not by a
/// comment, but because those flags are not on the list.
///
/// An unresolvable binary is likewise uncacheable rather than keyed with an
/// empty fingerprint — a key that cannot see the compiler would survive an
/// extract rebuild and serve stale Core.
///
/// A caller carrying [`Invocation::stable_val`] additionally accepts
/// `--session-root <dir>` (dropped, like `--output-dir`) and one matching
/// `--inject-val <module>` (dropped, and separately CONTENT-fingerprinted —
/// see that field's doc) — every other `--inject-val`/`--session-root` still
/// falls through to the default-deny arm below.
pub fn invocation_key(inv: &Invocation<'_>) -> Option<InvocationKey> {
    // Walk first, so an uncacheable invocation costs no hashing.
    let mut fields: Vec<&OsStr> = Vec::new();
    let mut stable_session_root: Option<&OsStr> = None;
    let mut args = inv.argv.iter();
    while let Some(arg) = args.next() {
        match arg.to_str() {
            Some("--output-dir" | "--include" | "--build-products-dir") => {
                args.next()?;
            }
            Some(flag @ ("--target" | "--targets")) => {
                let value = args.next()?;
                fields.push(OsStr::new(flag));
                fields.push(value);
            }
            Some("--session-root") if inv.stable_val.is_some() => {
                stable_session_root = Some(args.next()?.as_os_str());
            }
            Some("--inject-val") => {
                let value = args.next()?;
                match inv.stable_val {
                    Some(sv) if value.to_str() == Some(sv.module_name().as_str()) => {}
                    _ => return None,
                }
            }
            _ if arg.as_os_str() == inv.input_path.as_os_str() => {}
            _ => return None,
        }
    }
    // A caller supplying `stable_val` must have actually put both matching
    // argv elements there — an invocation that half-carries the flags (a
    // caller bug) is uncacheable rather than fingerprinted from a stale or
    // guessed location.
    if inv.stable_val.is_some() && stable_session_root.is_none() {
        return None;
    }

    let bin = resolve_for_fingerprint(inv.bin)?;

    let mut hasher = blake3::Hasher::new();
    frame(&mut hasher, INVOCATION_NAMESPACE);
    frame(&mut hasher, inv.source.as_bytes());

    frame(&mut hasher, &(fields.len() as u64).to_le_bytes());
    for field in fields {
        frame(&mut hasher, field.as_encoded_bytes());
    }

    // ORIGINAL order: GHC receives `--include` in argument order and
    // search-path order decides module shadowing, so [A,B] and [B,A] are
    // different compilations and must not share a key.
    frame(&mut hasher, &(inv.include.len() as u64).to_le_bytes());
    for root in inv.include {
        fingerprint_dir_relative(root, &mut hasher);
    }

    if let (Some(sv), Some(root)) = (inv.stable_val, stable_session_root) {
        let hi_path = Path::new(root).join(sv.relative_hi_path());
        fingerprint_stable_val_iface(&hi_path, &mut hasher);
    }

    fingerprint_binary_content(&bin, &mut hasher);

    Some(InvocationKey(hasher.finalize().to_hex().to_string()))
}

/// Fingerprint a stable `--inject-val` module's `.hi` iface by CONTENT — the
/// same "unreadable is a distinct hashed value, not a skip" discipline
/// [`collect_relative`] uses for an include file, so a missing/unreadable
/// iface still yields a stable (if uncacheable-in-practice) key rather than
/// panicking.
fn fingerprint_stable_val_iface(hi_path: &Path, hasher: &mut blake3::Hasher) {
    match fs::read(hi_path) {
        Ok(bytes) => {
            frame(hasher, &[1u8]);
            frame(hasher, blake3::hash(&bytes).as_bytes());
        }
        Err(_) => frame(hasher, &[0u8]),
    }
}

/// The binary that will actually be spawned, as an absolute readable file.
/// A bare name (`$TIDEPOOL_EXTRACT` unset, so `ExtractCmd` spawns through
/// `PATH`) is resolved the same way the OS will resolve it, so the key
/// fingerprints the binary the spawn reaches. `None` when nothing resolves.
fn resolve_for_fingerprint(bin: &Path) -> Option<PathBuf> {
    if bin.is_file() {
        return Some(bin.to_path_buf());
    }
    which::which(bin).ok()
}

/// Fingerprint the compiler by CONTENT only — no path, deliberately. The same
/// binary bytes at two install locations IS the same compiler, and pinning the
/// path would defeat sharing one memo across processes that resolved the
/// extract differently. Wrapper `exec` targets are followed, so a delegate-only
/// upgrade still forces a miss ([`wrapper_targets`]).
fn fingerprint_binary_content(bin: &Path, hasher: &mut blake3::Hasher) {
    frame(hasher, &binary_content_hash(bin));
    let targets = wrapper_targets(bin);
    frame(hasher, &(targets.len() as u64).to_le_bytes());
    for target in &targets {
        frame(hasher, &binary_content_hash(target));
    }
}

/// Fingerprints an include root by CONTENT, keyed by each file's path
/// RELATIVE to that root.
///
/// The relative keying is the deliberate divergence from [`fingerprint_dir`],
/// which frames absolute paths. It is what makes one memo shareable across
/// processes that materialized identical trees at different locations (the
/// generated effects module and test fixtures both live under a per-process
/// tempdir), and it is sound because the absolute location of an include dir
/// does not reach the output bytes: Cast/Tick/Type erasure happens in the
/// Haskell serializer, so Core carries no source spans. Module identity comes
/// from the path relative to the search root — which IS keyed.
///
/// Files are collected then sorted globally, so the digest does not depend on
/// directory traversal order. The canonicalized-`visited` set is the same
/// cycle guard [`fingerprint_dir_inner`] carries: `path.is_dir()` follows
/// symlinks, so a directory symlink pointing back at an ancestor would
/// otherwise recurse forever.
fn fingerprint_dir_relative(root: &Path, hasher: &mut blake3::Hasher) {
    let mut files: Vec<(String, Vec<u8>)> = Vec::new();
    let mut visited = std::collections::HashSet::new();
    collect_relative(root, root, &mut files, &mut visited);
    files.sort();

    frame(hasher, &(files.len() as u64).to_le_bytes());
    for (rel, digest) in &files {
        frame(hasher, rel.as_bytes());
        frame(hasher, digest);
    }
}

fn collect_relative(
    root: &Path,
    dir: &Path,
    out: &mut Vec<(String, Vec<u8>)>,
    visited: &mut std::collections::HashSet<PathBuf>,
) {
    if let Ok(canon) = fs::canonicalize(dir) {
        if !visited.insert(canon) {
            return;
        }
    }
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.filter_map(std::result::Result::ok) {
        let path = entry.path();
        if path.is_dir() {
            collect_relative(root, &path, out, visited);
            continue;
        }
        let Some(ext) = path.extension() else {
            continue;
        };
        if ext != "hs" && ext != "hs-boot" {
            continue;
        }
        let rel = path
            .strip_prefix(root)
            .unwrap_or(&path)
            .to_string_lossy()
            .into_owned();
        // A leading tag byte keeps "unreadable" a distinct value from any
        // content digest instead of aliasing onto one. `fs::read` follows
        // symlinks and hashes what GHC will actually compile.
        let digest = match fs::read(&path) {
            Ok(bytes) => {
                let mut d = vec![1u8];
                d.extend_from_slice(blake3::hash(&bytes).as_bytes());
                d
            }
            Err(_) => vec![0u8],
        };
        out.push((rel, digest));
    }
}

/// Manifest tag, so a truncated or foreign file cannot be read as a manifest.
const ARTIFACT_MANIFEST_TAG: &[u8] = b"artifact-set-v1";

/// Load a cached invocation's FULL artifact set, in the order `names` requests
/// it. `Some(v)` only when the stored manifest names exactly `names`, in
/// order, and every present artifact's bytes still hash to what the manifest
/// recorded — a crash mid-store, a slot-set mismatch, or a bit-flip that would
/// still decode as plausible CBOR all read as a MISS.
///
/// An entry is `None` when the extract did not write that artifact at all.
/// That is distinct from empty bytes and must stay so: an extract predating
/// the asks pass writes no `asks.json`, and the caller's "no file" branch
/// yields an empty sidecar rather than parsing `[]`.
pub fn artifacts_load(key: &InvocationKey, names: &[&str]) -> Option<Vec<Option<Vec<u8>>>> {
    let dir = cache_dir()?;
    let manifest = fs::read(dir.join(format!("{key}.ok"))).ok()?;
    let entries = parse_manifest(&manifest)?;
    if entries.len() != names.len() {
        return None;
    }

    let mut out = Vec::with_capacity(names.len());
    for (i, (expected, (name, digest))) in names.iter().zip(entries.iter()).enumerate() {
        if name.as_str() != *expected {
            return None;
        }
        let Some(digest) = digest else {
            out.push(None);
            continue;
        };
        let bytes = fs::read(dir.join(format!("{key}.a{i}"))).ok()?;
        if blake3::hash(&bytes).as_bytes() != digest {
            return None;
        }
        out.push(Some(bytes));
    }
    Some(out)
}

/// Store an invocation's full artifact set. Each present artifact is replaced
/// atomically via rename; the `{key}.ok` manifest is REMOVED first (marking
/// the entry incomplete) and rewritten LAST, so [`artifacts_load`] never reads
/// a half-written set. Best-effort throughout — a cache that cannot be written
/// degrades to recompiling, never to failing the compile.
pub fn artifacts_store(key: &InvocationKey, artifacts: &[(&str, Option<&[u8]>)]) {
    let Some(dir) = cache_dir() else { return };
    if fs::create_dir_all(&dir).is_err() {
        return;
    }
    let sentinel = dir.join(format!("{key}.ok"));
    let _ = fs::remove_file(&sentinel);

    let mut manifest = Vec::new();
    frame_bytes(&mut manifest, ARTIFACT_MANIFEST_TAG);
    frame_bytes(&mut manifest, &(artifacts.len() as u64).to_le_bytes());
    for (i, (name, bytes)) in artifacts.iter().enumerate() {
        frame_bytes(&mut manifest, name.as_bytes());
        match bytes {
            Some(bytes) => {
                use std::io::Write;
                let Ok(mut tmp) = tempfile::NamedTempFile::new_in(&dir) else {
                    return;
                };
                if tmp.write_all(bytes).is_err() {
                    return;
                }
                if tmp.persist(dir.join(format!("{key}.a{i}"))).is_err() {
                    return;
                }
                manifest.push(1u8);
                frame_bytes(&mut manifest, blake3::hash(bytes).as_bytes());
            }
            None => manifest.push(0u8),
        }
    }
    let _ = fs::write(&sentinel, &manifest);
}

/// Length-prefixed field into a byte buffer — the [`frame`] discipline, for
/// the manifest rather than a hasher.
fn frame_bytes(out: &mut Vec<u8>, bytes: &[u8]) {
    out.extend_from_slice(&(bytes.len() as u64).to_le_bytes());
    out.extend_from_slice(bytes);
}

/// Parse a manifest into `(name, Some(digest) | None)` entries. Any
/// malformation — wrong tag, truncation, trailing bytes — is `None`, i.e. a
/// miss.
fn parse_manifest(bytes: &[u8]) -> Option<Vec<(String, Option<[u8; 32]>)>> {
    let mut cur = 0usize;
    if take_framed(bytes, &mut cur)? != ARTIFACT_MANIFEST_TAG {
        return None;
    }
    let count = u64::from_le_bytes(take_framed(bytes, &mut cur)?.try_into().ok()?);
    let count = usize::try_from(count).ok()?;
    let mut out = Vec::with_capacity(count.min(1024));
    for _ in 0..count {
        let name = String::from_utf8(take_framed(bytes, &mut cur)?.to_vec()).ok()?;
        let digest = match take(bytes, &mut cur, 1)?[0] {
            0 => None,
            1 => Some(<[u8; 32]>::try_from(take_framed(bytes, &mut cur)?).ok()?),
            _ => return None,
        };
        out.push((name, digest));
    }
    // Trailing bytes mean this is not the manifest we wrote.
    if cur != bytes.len() {
        return None;
    }
    Some(out)
}

fn take<'a>(bytes: &'a [u8], cur: &mut usize, n: usize) -> Option<&'a [u8]> {
    let end = cur.checked_add(n)?;
    let slice = bytes.get(*cur..end)?;
    *cur = end;
    Some(slice)
}

fn take_framed<'a>(bytes: &'a [u8], cur: &mut usize) -> Option<&'a [u8]> {
    let len = u64::from_le_bytes(take(bytes, cur, 8)?.try_into().ok()?);
    take(bytes, cur, usize::try_from(len).ok()?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serial_test::serial;
    use tempfile::TempDir;

    /// RAII guard to safely set and restore environment variables in tests.
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

    #[test]
    #[serial]
    fn test_cache_key_determinism() {
        let source = "main = print 42";
        let target = "main";
        let k1 = cache_key(source, target, &[]);
        let k2 = cache_key(source, target, &[]);
        assert_eq!(k1, k2);

        let k3 = cache_key("main = print 43", target, &[]);
        assert_ne!(k1, k3);
    }

    #[test]
    #[serial]
    fn test_cache_key_salt_isolates_sessions_and_gens() {
        let (src, tgt) = ("import Tidepool.Session.Lib.G1\nr = 1", "r");
        // No salt reproduces the unsalted key exactly (no mass invalidation).
        assert_eq!(
            cache_key(src, tgt, &[]),
            cache_key_salted(src, tgt, &[], None)
        );
        // Distinct (session, gen) salts never collide — even on identical text.
        let a = cache_key_salted(src, tgt, &[], Some("session:1:gen:1"));
        let b = cache_key_salted(src, tgt, &[], Some("session:2:gen:1"));
        let c = cache_key_salted(src, tgt, &[], Some("session:1:gen:2"));
        assert_ne!(a, b, "different sessions must not share a key");
        assert_ne!(a, c, "a generation bump must invalidate");
        // A salt always diverges from the unsalted key.
        assert_ne!(a, cache_key(src, tgt, &[]));
    }

    #[test]
    #[serial]
    fn test_cache_roundtrip() {
        let temp_dir = TempDir::new().unwrap();
        let _guard = EnvGuard::new("XDG_CACHE_HOME", temp_dir.path());

        let key = CacheKey("test-key".to_string());
        let expr = b"expr-data";
        let meta = b"meta-data";

        // Before store, load should miss.
        assert!(cache_load(&key).is_none());

        cache_store(&key, expr, meta);

        // Sentinel must exist after store.
        let sentinel = temp_dir.path().join("tidepool").join(format!("{}.ok", key));
        assert!(sentinel.exists(), "sentinel file should exist after store");

        let loaded = cache_load(&key).expect("cache should load after store");
        assert_eq!(loaded.0, expr);
        assert_eq!(loaded.1, meta);
    }

    #[test]
    #[serial]
    fn test_cache_load_fails_without_sentinel() {
        let temp_dir = TempDir::new().unwrap();
        let _guard = EnvGuard::new("XDG_CACHE_HOME", temp_dir.path());

        let key = CacheKey("no-sentinel".to_string());
        let dir = temp_dir.path().join("tidepool");
        fs::create_dir_all(&dir).unwrap();

        // Write cbor files but no sentinel — simulates a crash mid-store.
        fs::write(dir.join(format!("{}.cbor", key)), b"expr").unwrap();
        fs::write(dir.join(format!("{}.meta.cbor", key)), b"meta").unwrap();

        assert!(
            cache_load(&key).is_none(),
            "cache_load should return None without sentinel"
        );
    }

    #[test]
    #[serial]
    fn test_cache_key_include_fingerprint() {
        let include_dir = TempDir::new().unwrap();
        let hs_file = include_dir.path().join("Lib.hs");
        fs::write(&hs_file, "module Lib where").unwrap();

        let source = "import Lib\nmain = print 42";
        let target = "main";
        let includes = [include_dir.path()];

        let k1 = cache_key(source, target, &includes);

        // Wait a bit to ensure mtime changes if we overwrite (though some filesystems have low precision)
        // or just write different content/size.
        fs::write(&hs_file, "module Lib where\nfoo = 1").unwrap();
        let k2 = cache_key(source, target, &includes);

        assert_ne!(
            k1, k2,
            "Cache key should change when dependency file changes"
        );
    }

    #[cfg(unix)]
    #[test]
    #[serial]
    fn test_cache_key_handles_symlink_cycle_in_include_dir() {
        use std::os::unix::fs::symlink;

        let include_dir = TempDir::new().unwrap();
        let hs_file = include_dir.path().join("Lib.hs");
        fs::write(&hs_file, "module Lib where").unwrap();

        // A cyclic directory symlink: `include_dir/loop` points straight back
        // at `include_dir` itself. `path.is_dir()` follows symlinks, so a
        // naive recursive walk would descend into `loop`, find `loop` again
        // inside it, and never terminate.
        let loop_link = include_dir.path().join("loop");
        symlink(include_dir.path(), &loop_link).unwrap();

        let source = "import Lib\nmain = print 42";
        let target = "main";
        let includes = [include_dir.path()];

        // Must return promptly (the cycle guard breaks the recursion) rather
        // than hang the process.
        let _key = cache_key(source, target, &includes);
    }

    #[cfg(unix)]
    #[test]
    #[serial]
    fn test_cache_key_binary_fingerprint_mtime() {
        use std::os::unix::fs::PermissionsExt;

        let temp_dir = TempDir::new().unwrap();
        let bin_path = temp_dir.path().join("fake-extract");
        fs::write(&bin_path, b"#!/bin/sh\n").unwrap();
        fs::set_permissions(&bin_path, fs::Permissions::from_mode(0o755)).unwrap();

        // Point directly to the binary to avoid PATH mutation
        let _guard = EnvGuard::new("TIDEPOOL_EXTRACT", &bin_path);

        let k1 = cache_key("source", "target", &[]);

        // mtime-only change: the fingerprint is content-defined (blake3 of
        // the bytes — nix normalizes all store mtimes to epoch+1, so mtime
        // can't distinguish versions). Key must NOT change.
        let past = filetime::FileTime::from_unix_time(100, 0);
        filetime::set_file_mtime(&bin_path, past).unwrap();

        let k2 = cache_key("source", "target", &[]);
        assert_eq!(
            k1, k2,
            "mtime-only change must not change the cache key (content-defined fingerprint)"
        );

        // Same-size content swap ((size, mtime) alone is blind to this):
        // ctime bumps on write, forcing a re-hash that sees the new content.
        // Sleep first: kernel ctime is coarse-grained (tick granularity, ~ms);
        // a swap within the same tick as the create gets an IDENTICAL ctime
        // and the memo legitimately serves the old hash. Real binary swaps
        // are never sub-tick; the test must not be either.
        std::thread::sleep(std::time::Duration::from_millis(50));
        fs::write(&bin_path, b"#!/bin/SH\n").unwrap();
        filetime::set_file_mtime(&bin_path, past).unwrap();

        let k3 = cache_key("source", "target", &[]);
        assert_ne!(
            k1, k3,
            "Cache key should change on a same-size, same-mtime content swap"
        );
    }

    #[cfg(unix)]
    #[test]
    #[serial]
    fn test_cache_key_wrapper_script_fingerprints_target() {
        use std::os::unix::fs::PermissionsExt;

        let temp_dir = TempDir::new().unwrap();

        // Create the "real" binary.
        let real_bin = temp_dir.path().join("tidepool-extract-bin");
        fs::write(&real_bin, b"real-binary-v1").unwrap();
        fs::set_permissions(&real_bin, fs::Permissions::from_mode(0o755)).unwrap();

        // Create a wrapper script that execs the real binary.
        let wrapper = temp_dir.path().join("tidepool-extract");
        fs::write(
            &wrapper,
            format!("#!/bin/sh\nexec {} \"$@\"\n", real_bin.display()),
        )
        .unwrap();
        fs::set_permissions(&wrapper, fs::Permissions::from_mode(0o755)).unwrap();

        let _guard = EnvGuard::new("TIDEPOOL_EXTRACT", &wrapper);

        let k1 = cache_key("source", "target", &[]);

        // Change the real binary (wrapper unchanged) — key must change.
        fs::write(&real_bin, b"real-binary-v2-longer").unwrap();
        let k2 = cache_key("source", "target", &[]);

        assert_ne!(
            k1, k2,
            "Cache key should change when the target binary behind a wrapper changes"
        );
    }

    #[test]
    fn test_extract_exec_target() {
        assert_eq!(
            extract_exec_target("exec /usr/local/bin/foo \"$@\""),
            Some("/usr/local/bin/foo")
        );
        assert_eq!(
            extract_exec_target("/usr/local/bin/foo \"$@\""),
            Some("/usr/local/bin/foo")
        );
        assert_eq!(extract_exec_target("#!/bin/sh"), None);
        assert_eq!(extract_exec_target("FOO=bar"), None);
        assert_eq!(extract_exec_target(""), None);
        assert_eq!(extract_exec_target("relative-path arg"), None);
    }

    // -----------------------------------------------------------------------
    // Invocation keying — adversarial.
    //
    // A keying bug here poisons every downstream consumer SILENTLY: a false
    // hit serves one compilation's Core for another and nothing fails loudly.
    // So the discipline is one test per DIMENSION, each varying exactly one
    // thing and asserting a MISS, plus the one dimension that must NOT change
    // the key (absolute include path — the property that makes the memo
    // shareable across processes).
    // -----------------------------------------------------------------------

    /// A dummy extract binary, so the key's compiler fingerprint resolves.
    #[cfg(unix)]
    fn fake_bin(dir: &Path, contents: &[u8]) -> PathBuf {
        use std::os::unix::fs::PermissionsExt;
        let path = dir.join("fake-extract");
        fs::write(&path, contents).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
        path
    }

    /// The argv `crate::artifacts::compile_targets` builds.
    fn turn_argv(input: &Path, out: &Path, targets: &str, includes: &[&Path]) -> Vec<OsString> {
        let mut argv = vec![
            input.as_os_str().to_os_string(),
            OsString::from("--output-dir"),
            out.as_os_str().to_os_string(),
            OsString::from("--targets"),
            OsString::from(targets),
        ];
        for inc in includes {
            argv.push(OsString::from("--include"));
            argv.push(inc.as_os_str().to_os_string());
        }
        argv
    }

    /// Writes `Lib.hs` with the given body under a fresh subdir of `root`.
    fn include_dir(root: &Path, name: &str, body: &str) -> PathBuf {
        let dir = root.join(name);
        fs::create_dir_all(dir.join("Nested")).unwrap();
        fs::write(dir.join("Lib.hs"), body).unwrap();
        fs::write(
            dir.join("Nested").join("Deep.hs"),
            "module Nested.Deep where",
        )
        .unwrap();
        dir
    }

    #[cfg(unix)]
    #[test]
    #[serial]
    fn invocation_key_misses_on_every_input_dimension() {
        let tmp = TempDir::new().unwrap();
        let bin = fake_bin(tmp.path(), b"#!/bin/sh\nexit 0\n");
        let input = tmp.path().join("Expr.hs");
        fs::write(&input, "module Expr where").unwrap();
        let out = tmp.path().join("out");
        let inc_a = include_dir(tmp.path(), "a", "module Lib where\nx = 1");
        let inc_b = include_dir(tmp.path(), "b", "module Other where\ny = 2");

        let key = |source: &str, targets: &str, includes: &[&Path]| {
            let argv = turn_argv(&input, &out, targets, includes);
            let include: Vec<PathBuf> = includes.iter().map(|p| p.to_path_buf()).collect();
            invocation_key(&Invocation {
                source,
                argv: &argv,
                input_path: &input,
                include: &include,
                bin: &bin,
                stable_val: None,
            })
            .expect("this invocation is cacheable")
        };

        let base = key("main = pure ()", "result", &[&inc_a, &inc_b]);
        assert_eq!(
            base,
            key("main = pure ()", "result", &[&inc_a, &inc_b]),
            "the key must be deterministic"
        );

        // Source content.
        assert_ne!(base, key("main = pure 1", "result", &[&inc_a, &inc_b]));
        // A target NAME (an argv field).
        assert_ne!(base, key("main = pure ()", "other", &[&inc_a, &inc_b]));
        // Target ORDER: `--targets a,b` and `--targets b,a` are different
        // invocations (and for >1 they select the per-target asks shape).
        assert_ne!(
            key("main = pure ()", "a,b", &[&inc_a]),
            key("main = pure ()", "b,a", &[&inc_a])
        );
        // Include ORDER: search-path order decides module shadowing.
        assert_ne!(base, key("main = pure ()", "result", &[&inc_b, &inc_a]));
        // Include SET.
        assert_ne!(base, key("main = pure ()", "result", &[&inc_a]));

        // Include CONTENT — one byte in one file under one include root.
        fs::write(inc_a.join("Lib.hs"), "module Lib where\nx = 2").unwrap();
        let after_edit = key("main = pure ()", "result", &[&inc_a, &inc_b]);
        assert_ne!(base, after_edit, "an include-content edit must miss");

        // A NEW file under an include root (a module that was not there).
        fs::write(inc_a.join("Extra.hs"), "module Extra where").unwrap();
        assert_ne!(
            after_edit,
            key("main = pure ()", "result", &[&inc_a, &inc_b])
        );
    }

    /// The compiler itself is keyed by CONTENT: an extract rebuild must miss
    /// even when the path, size and mtime are unchanged.
    #[cfg(unix)]
    #[test]
    #[serial]
    fn invocation_key_misses_on_extract_binary_content() {
        let tmp = TempDir::new().unwrap();
        let bin = fake_bin(tmp.path(), b"#!/bin/sh\nexit 0\n");
        let input = tmp.path().join("Expr.hs");
        fs::write(&input, "module Expr where").unwrap();
        let argv = turn_argv(&input, &tmp.path().join("out"), "result", &[]);
        let key = || {
            invocation_key(&Invocation {
                source: "main = pure ()",
                argv: &argv,
                input_path: &input,
                include: &[],
                bin: &bin,
                stable_val: None,
            })
            .unwrap()
        };

        let before = key();
        // Sleep first: ctime granularity is a kernel tick, and the content-hash
        // memo is keyed on (dev, ino, ctime). A real rebuild is never sub-tick.
        std::thread::sleep(std::time::Duration::from_millis(50));
        let past = filetime::FileTime::from_unix_time(100, 0);
        filetime::set_file_mtime(&bin, past).unwrap();
        fs::write(&bin, b"#!/bin/sh\nexit 1\n").unwrap(); // same size
        filetime::set_file_mtime(&bin, past).unwrap();
        assert_ne!(before, key(), "an extract rebuild must invalidate");
    }

    /// The property the shared test memo rests on: identical CONTENT at
    /// different absolute paths is the same compilation and keys identically.
    /// If this ever flips, every harness test process misses and the suite
    /// silently returns to its cold cost.
    #[cfg(unix)]
    #[test]
    #[serial]
    fn invocation_key_is_independent_of_absolute_paths() {
        let tmp = TempDir::new().unwrap();
        let bin = fake_bin(tmp.path(), b"#!/bin/sh\nexit 0\n");
        let body = "module Lib where\nx = 1";

        let key_at = |root: &Path| {
            let input = root.join("Expr.hs");
            fs::write(&input, "module Expr where").unwrap();
            let inc = include_dir(root, "inc", body);
            let argv = turn_argv(&input, &root.join("out"), "result", &[&inc]);
            invocation_key(&Invocation {
                source: "main = pure ()",
                argv: &argv,
                input_path: &input,
                include: std::slice::from_ref(&inc),
                bin: &bin,
                stable_val: None,
            })
            .unwrap()
        };

        let one = TempDir::new().unwrap();
        let two = TempDir::new().unwrap();
        assert_eq!(
            key_at(one.path()),
            key_at(two.path()),
            "identical content at different absolute paths must share a key"
        );
    }

    /// `--build-products-dir <dir>` is DROPPED, same bucket as `--output-dir`:
    /// the invocation stays cacheable, and the key is blind to the flag's
    /// value entirely (two otherwise-identical invocations pointing it at
    /// different paths — the realistic case, since the dir is
    /// content-addressed per toolchain fingerprint — must still share a key).
    #[cfg(unix)]
    #[test]
    #[serial]
    fn invocation_key_drops_build_products_dir() {
        let tmp = TempDir::new().unwrap();
        let bin = fake_bin(tmp.path(), b"#!/bin/sh\nexit 0\n");
        let input = tmp.path().join("Expr.hs");
        fs::write(&input, "module Expr where").unwrap();

        let key = |bp: Option<&str>| {
            let mut argv = turn_argv(&input, &tmp.path().join("out"), "result", &[]);
            if let Some(dir) = bp {
                argv.push(OsString::from("--build-products-dir"));
                argv.push(OsString::from(dir));
            }
            invocation_key(&Invocation {
                source: "main = pure ()",
                argv: &argv,
                input_path: &input,
                include: &[],
                bin: &bin,
                stable_val: None,
            })
        };

        let without = key(None).expect("cacheable without the flag");
        let with_a = key(Some("/tmp/bp-a")).expect("cacheable with the flag");
        let with_b = key(Some("/tmp/bp-b")).expect("cacheable with a different path");
        assert_eq!(without, with_a, "the flag must not change the key");
        assert_eq!(with_a, with_b, "the key must be blind to the dir's path");
    }

    /// Default-deny: an argv element the allowlist does not classify makes the
    /// invocation UNCACHEABLE rather than silently unkeyed. This is how
    /// session-scope compiles (which read per-session MUTABLE dirs nothing
    /// here fingerprints) stay out, and how a flag added tomorrow goes cold
    /// instead of wrong.
    #[cfg(unix)]
    #[test]
    #[serial]
    fn invocation_key_refuses_unclassified_and_session_scoped_flags() {
        let tmp = TempDir::new().unwrap();
        let bin = fake_bin(tmp.path(), b"#!/bin/sh\nexit 0\n");
        let input = tmp.path().join("Expr.hs");
        fs::write(&input, "module Expr where").unwrap();

        let key = |extra: &[&str]| {
            let mut argv = turn_argv(&input, &tmp.path().join("out"), "result", &[]);
            argv.extend(extra.iter().map(OsString::from));
            invocation_key(&Invocation {
                source: "main = pure ()",
                argv: &argv,
                input_path: &input,
                include: &[],
                bin: &bin,
                stable_val: None,
            })
        };

        assert!(key(&[]).is_some(), "the plain turn invocation is cacheable");
        for flags in [
            &["--session-bind"][..],
            &["--session-root", "/tmp/sessions"][..],
            &["--inject-val", "Tidepool.Session.Val.G1"][..],
            &["--bind-gen", "2"][..],
            &["--turn"][..],
            &["--classify"][..],
            &["--some-future-flag", "v"][..],
        ] {
            assert!(
                key(flags).is_none(),
                "{flags:?} must make the invocation uncacheable"
            );
        }
        // A dangling flag (no value) is likewise uncacheable, not a panic.
        assert!(key(&["--target"]).is_none());
        // An unresolvable binary is uncacheable — a key blind to the compiler
        // would survive an extract rebuild.
        let argv = turn_argv(&input, &tmp.path().join("out"), "result", &[]);
        assert!(invocation_key(&Invocation {
            source: "main = pure ()",
            argv: &argv,
            input_path: &input,
            include: &[],
            bin: &tmp.path().join("no-such-extract"),
            stable_val: None,
        })
        .is_none());
    }

    /// A caller carrying `stable_val` DOES make an otherwise-uncacheable
    /// `--session-root`/`--inject-val` pair cacheable, keyed by the iface
    /// FILE's content — path-independent (two different `--session-root`
    /// locations with byte-identical iface content share a key), content-
    /// sensitive (editing the iface's bytes misses), and still refuses any
    /// OTHER `--inject-val` value (a real, generation-numbered session bind)
    /// even with `stable_val` set.
    #[cfg(unix)]
    #[test]
    #[serial]
    fn invocation_key_accepts_matching_stable_val_inject() {
        let tmp = TempDir::new().unwrap();
        let bin = fake_bin(tmp.path(), b"#!/bin/sh\nexit 0\n");
        let input = tmp.path().join("Expr.hs");
        fs::write(&input, "module Expr where").unwrap();
        let module = tidepool_repr::SessionModule::val(tidepool_repr::Generation(0));

        let key_at = |session_root: &Path, iface_bytes: &[u8]| {
            let hi_path = session_root.join(module.relative_hi_path());
            fs::create_dir_all(hi_path.parent().unwrap()).unwrap();
            fs::write(&hi_path, iface_bytes).unwrap();
            let mut argv = turn_argv(&input, &tmp.path().join("out"), "result", &[]);
            argv.push(OsString::from("--session-root"));
            argv.push(session_root.as_os_str().to_os_string());
            argv.push(OsString::from("--inject-val"));
            argv.push(OsString::from(module.module_name()));
            invocation_key(&Invocation {
                source: "main = pure ()",
                argv: &argv,
                input_path: &input,
                include: &[],
                bin: &bin,
                stable_val: Some(module),
            })
        };

        let root_a = tmp.path().join("session-a");
        let root_b = tmp.path().join("session-b");
        fs::create_dir_all(&root_a).unwrap();
        fs::create_dir_all(&root_b).unwrap();

        let base = key_at(&root_a, b"iface-v1").expect("stable-val invocation is cacheable");
        assert_eq!(
            base,
            key_at(&root_a, b"iface-v1").expect("deterministic"),
            "the key must be deterministic"
        );
        assert_eq!(
            base,
            key_at(&root_b, b"iface-v1").expect("cacheable at a different root"),
            "identical iface CONTENT at a different --session-root must share a key"
        );
        assert_ne!(
            base,
            key_at(&root_a, b"iface-v2").expect("still cacheable"),
            "an iface content edit must miss"
        );

        // Even with `stable_val` set, a DIFFERENT --inject-val value (a real
        // Val.G<g> session bind, g != 0) stays uncacheable — hazard (b) is
        // preserved for everything except the one named stable module.
        let mut argv = turn_argv(&input, &tmp.path().join("out"), "result", &[]);
        argv.push(OsString::from("--session-root"));
        argv.push(root_a.as_os_str().to_os_string());
        argv.push(OsString::from("--inject-val"));
        argv.push(OsString::from("Tidepool.Session.Val.G7"));
        assert!(
            invocation_key(&Invocation {
                source: "main = pure ()",
                argv: &argv,
                input_path: &input,
                include: &[],
                bin: &bin,
                stable_val: Some(module),
            })
            .is_none(),
            "a non-stable --inject-val value must stay uncacheable"
        );
    }

    /// A caller that sets `stable_val` but the argv it actually built never
    /// carries a `--session-root` (a caller bug, or an `--inject-val` with no
    /// paired root) is uncacheable rather than fingerprinted from a
    /// guessed/absent location — the guard reads the root out of argv itself,
    /// never out of `stable_val`.
    #[cfg(unix)]
    #[test]
    #[serial]
    fn invocation_key_stable_val_without_matching_argv_is_uncacheable() {
        let tmp = TempDir::new().unwrap();
        let bin = fake_bin(tmp.path(), b"#!/bin/sh\nexit 0\n");
        let input = tmp.path().join("Expr.hs");
        fs::write(&input, "module Expr where").unwrap();
        let module = tidepool_repr::SessionModule::val(tidepool_repr::Generation(0));

        // --inject-val without a --session-root: uncacheable (the walk never
        // saw a session root to fingerprint from).
        let mut argv_no_root = turn_argv(&input, &tmp.path().join("out"), "result", &[]);
        argv_no_root.push(OsString::from("--inject-val"));
        argv_no_root.push(OsString::from(module.module_name()));
        assert!(invocation_key(&Invocation {
            source: "main = pure ()",
            argv: &argv_no_root,
            input_path: &input,
            include: &[],
            bin: &bin,
            stable_val: Some(module),
        })
        .is_none());
    }

    /// An invocation key can never name the same on-disk entry as an eval
    /// key — the two store DIFFERENT artifact sets under `<key>.*`.
    #[cfg(unix)]
    #[test]
    #[serial]
    fn invocation_key_is_namespaced_away_from_eval_keys() {
        let tmp = TempDir::new().unwrap();
        let bin = fake_bin(tmp.path(), b"#!/bin/sh\nexit 0\n");
        let _guard = EnvGuard::new("TIDEPOOL_EXTRACT", &bin);
        let input = tmp.path().join("Expr.hs");
        fs::write(&input, "module Expr where").unwrap();
        let argv = turn_argv(&input, &tmp.path().join("out"), "result", &[]);
        let inv = invocation_key(&Invocation {
            source: "main = pure ()",
            argv: &argv,
            input_path: &input,
            include: &[],
            bin: &bin,
            stable_val: None,
        })
        .unwrap();
        assert_ne!(
            inv.to_string(),
            cache_key("main = pure ()", "result", &[]).to_string()
        );
    }

    #[test]
    #[serial]
    fn artifacts_roundtrip_present_and_absent() {
        let tmp = TempDir::new().unwrap();
        let _guard = EnvGuard::new("XDG_CACHE_HOME", tmp.path());
        let key = InvocationKey("artifacts-roundtrip".to_string());
        let names = ["meta.cbor", "result.cbor", "asks.json"];

        assert!(artifacts_load(&key, &names).is_none(), "empty cache misses");

        // The asks sidecar is ABSENT — an extract predating that pass writes
        // no file at all, and `None` must survive as `None`.
        artifacts_store(
            &key,
            &[
                ("meta.cbor", Some(b"meta".as_slice())),
                ("result.cbor", Some(b"expr".as_slice())),
                ("asks.json", None),
            ],
        );

        let loaded = artifacts_load(&key, &names).expect("stored set must load");
        assert_eq!(loaded[0].as_deref(), Some(b"meta".as_slice()));
        assert_eq!(loaded[1].as_deref(), Some(b"expr".as_slice()));
        assert_eq!(loaded[2], None, "absent must not become empty");

        // A DIFFERENT expected name set is a miss, not a silent mismatch.
        assert!(artifacts_load(&key, &["meta.cbor", "other.cbor", "asks.json"]).is_none());
        assert!(artifacts_load(&key, &["meta.cbor", "result.cbor"]).is_none());
    }

    #[test]
    #[serial]
    fn artifacts_load_misses_on_missing_sentinel_or_corrupt_bytes() {
        let tmp = TempDir::new().unwrap();
        let _guard = EnvGuard::new("XDG_CACHE_HOME", tmp.path());
        let dir = tmp.path().join("tidepool");
        let key = InvocationKey("artifacts-corrupt".to_string());
        let names = ["meta.cbor", "result.cbor"];
        artifacts_store(
            &key,
            &[
                ("meta.cbor", Some(b"meta".as_slice())),
                ("result.cbor", Some(b"expr".as_slice())),
            ],
        );
        assert!(artifacts_load(&key, &names).is_some());

        // A bit-flip that would still decode as plausible CBOR: the manifest's
        // recorded digest no longer matches, so the entry reads as a MISS.
        fs::write(dir.join(format!("{key}.a1")), b"EXPR").unwrap();
        assert!(artifacts_load(&key, &names).is_none());

        // A crash mid-store leaves artifacts with no sentinel.
        fs::write(dir.join(format!("{key}.a1")), b"expr").unwrap();
        assert!(artifacts_load(&key, &names).is_some());
        fs::remove_file(dir.join(format!("{key}.ok"))).unwrap();
        assert!(artifacts_load(&key, &names).is_none());

        // A truncated/foreign manifest is a miss, never a panic.
        fs::write(dir.join(format!("{key}.ok")), b"not-a-manifest").unwrap();
        assert!(artifacts_load(&key, &names).is_none());
    }

    #[cfg(unix)]
    #[test]
    #[serial]
    fn test_cache_key_binary_fingerprint_size() {
        use std::io::Write;
        use std::os::unix::fs::PermissionsExt;

        let temp_dir = TempDir::new().unwrap();
        let bin_path = temp_dir.path().join("fake-extract-size");
        fs::write(&bin_path, b"#!/bin/sh\n").unwrap();
        fs::set_permissions(&bin_path, fs::Permissions::from_mode(0o755)).unwrap();

        // Point directly to the binary to avoid PATH mutation
        let _guard = EnvGuard::new("TIDEPOOL_EXTRACT", &bin_path);

        let k1 = cache_key("source", "target", &[]);

        // Change size
        let mut file = fs::OpenOptions::new().append(true).open(&bin_path).unwrap();
        file.write_all(b"extra").unwrap();
        drop(file);

        let k2 = cache_key("source", "target", &[]);
        assert_ne!(k1, k2, "Cache key should change when binary size changes");
    }
}
