//! The ONE toolchain locator — where the GHC→Core extract binary and the
//! Haskell stdlib source tree live — plus the startup **handshake** that
//! refuses to serve an extract/stdlib pair that was not deployed together.
//!
//! The extract precedence (`$TIDEPOOL_EXTRACT` + `$PATH` fallback) and the
//! stdlib precedence (`$TIDEPOOL_PRELUDE_DIR`, the `dist-newstyle` sibling
//! walk, the cwd/bundle search) are consolidated into one place so they
//! cannot disagree. A disagreement between separate policies surfaces as a
//! *wrong answer at eval time* ("not in scope", "Metadata entry must be an
//! array of exactly 8") rather than as a configuration error. Both
//! precedence orders live here, once, and both are documented below.
//!
//! # Precedence: the extract binary
//!
//! Owned by `tidepool-extract-cmd` (`resolve_bin`), the crate that also builds
//! the extract COMMAND — location and invocation of the same binary belong
//! together. Reproduced here because the stdlib table below depends on it.
//!
//! | # | Source | Notes |
//! |---|--------|-------|
//! | 1 | `$TIDEPOOL_EXTRACT` | Explicit override, and STRICT: set-but-unreadable is a hard error, never a silent fall-through to `$PATH` — falling through would run a different binary than the caller believes it is running. |
//! | 2 | `tidepool-extract` on `$PATH` | Normally `~/.nix-profile/bin/tidepool-extract`, a wrapper that prepends the with-packages GHC and `exec`s the store binary. |
//!
//! [`extract_command_name`] returns what to spawn; [`locate_extract`] resolves
//! it to an ABSOLUTE path (the handshake must fingerprint a real file) and
//! fails typed when there is none. Both honor row 1's STRICT clause — a
//! set-but-unreadable `$TIDEPOOL_EXTRACT` is a hard error out of either, never
//! a silent fall-through to row 2.
//!
//! # Precedence: the Haskell stdlib source root
//!
//! The stdlib root is the GHC include dir under which `Tidepool/Prelude.hs`
//! lives. Every step is checked with [`is_stdlib_root`]; a step that names a
//! directory without `Tidepool/Prelude.hs` does not count as a hit.
//!
//! | # | Source | Rationale |
//! |---|--------|-----------|
//! | 1 | `$TIDEPOOL_PRELUDE_DIR` | Operator override. **Set-but-not-a-stdlib-root is a hard error**, never a silent fall-through — a typo'd override that quietly served a different stdlib is exactly the failure this module exists to kill. |
//! | 2 | `./haskell/lib`, then `./lib` (from CWD) | In-repo development: the working tree you are editing wins over anything installed. Preserves the `tidepool` binary's historical behavior. |
//! | 3 | Sibling of the extract's `dist-newstyle` | Absorbs the old `derive_stdlib_include`: walk `$TIDEPOOL_EXTRACT` up to a `dist-newstyle` component and take its sibling `lib/`. Pairs a worktree-built extract with that worktree's stdlib. |
//! | 4 | [`StdlibFallbacks::bundle`] | Installed mode: the stdlib embedded in the server binary, materialized to a content-addressed cache dir. Immutable and guaranteed to match the binary. |
//! | 5 | [`StdlibFallbacks::build_tree`] | Last resort: the source tree this binary was *built* from (`env!("CARGO_MANIFEST_DIR")`-derived). Keeps a repo-installed `tidepool-repl` working when launched outside the repo. |
//! | — | otherwise | [`ToolchainError::StdlibNotFound`], listing every path tried. |
//!
//! # The handshake
//!
//! Deploy coupling — extract, both servers, and the stdlib must move together
//! (`scripts/redeploy.sh`) — is checked at startup:
//!
//! - `scripts/redeploy.sh` finishes by running `tidepool --write-toolchain-stamp`,
//!   which records the **content** fingerprints of the extract binary and the
//!   stdlib tree it just deployed into [`stamp_path`].
//! - Each server calls [`enforce_handshake`] once at startup. It fingerprints
//!   the extract and stdlib it just resolved and compares them to the stamp.
//!   A mismatch means one side moved without the other → loud, actionable
//!   failure naming `scripts/redeploy.sh`.
//!
//! Fingerprints are **content-only, never paths**: the stamp is written from
//! the repo (stdlib = `haskell/lib`) but checked from a server whose stdlib is
//! the materialized bundle at a completely different path. Identical content
//! must compare equal.
//!
//! Cost: one memoized binary content hash (shared with the compile-cache key,
//! so a running server pays it at most once per extract version per machine)
//! plus one walk of ~40 small `.hs` files. Never per-eval.

use std::io::Write;
use std::path::{Path, PathBuf};

/// Env var naming the extract binary (step 1 of the extract precedence).
/// Read here only for the stdlib table's step 3; the extract binary itself is
/// resolved by [`tidepool_extract_cmd::resolve_bin`].
pub const ENV_EXTRACT: &str = "TIDEPOOL_EXTRACT";
/// Env var naming the stdlib root (step 1 of the stdlib precedence).
pub const ENV_PRELUDE_DIR: &str = "TIDEPOOL_PRELUDE_DIR";
/// Env var overriding [`stamp_path`].
pub const ENV_STAMP: &str = "TIDEPOOL_TOOLCHAIN_STAMP";
/// Env var selecting the handshake severity: `error` (default) / `warn` / `off`.
pub const ENV_HANDSHAKE: &str = "TIDEPOOL_TOOLCHAIN_HANDSHAKE";

/// The deploy command every skew message points at.
const REDEPLOY: &str = "scripts/redeploy.sh";

// ---------------------------------------------------------------------------
// Errors
// ---------------------------------------------------------------------------

/// A toolchain *configuration* failure: the extract or the stdlib could not be
/// located, or the located pair is skewed. Distinct from a compile failure —
/// nothing the user's Haskell can cause.
#[derive(thiserror::Error)]
pub enum ToolchainError {
    /// No runnable extract: `$TIDEPOOL_EXTRACT` named an unreadable file, or
    /// the bare name is not on `$PATH`.
    #[error(
        "tidepool-extract not found ({tried}). Set {ENV_EXTRACT} to a built \
         tidepool-extract-bin, or install the harness with `nix profile install .#tidepool-extract`."
    )]
    ExtractNotFound {
        /// What was searched for, as the locator crate reported it.
        tried: String,
    },

    /// `$TIDEPOOL_PRELUDE_DIR` is set but does not name a stdlib root.
    #[error(
        "{ENV_PRELUDE_DIR}={} is not a Tidepool stdlib root (no Tidepool/Prelude.hs under it). \
         Point it at a directory containing Tidepool/Prelude.hs, or unset it to use the \
         bundled stdlib.",
        .dir.display()
    )]
    PreludeDirInvalid {
        /// The offending override.
        dir: PathBuf,
    },

    /// No step of the stdlib precedence found a root.
    #[error(
        "Tidepool stdlib not found — no Tidepool/Prelude.hs under any of: {}. \
         Set {ENV_PRELUDE_DIR}, run from a Tidepool checkout, or reinstall the server \
         (`{REDEPLOY}`).",
        render_tried(.tried)
    )]
    StdlibNotFound {
        /// Every (precedence step, path) pair that was checked, in order.
        tried: Vec<(&'static str, PathBuf)>,
    },

    /// The located extract and stdlib were not deployed together. Boxed: the
    /// report carries both fingerprints plus the whole stamp, and this error
    /// rides in `Result`s on hot session paths where an unboxed 200+ byte
    /// variant would widen every `Ok` too (clippy's `result_large_err`).
    #[error("{0}")]
    Skew(Box<SkewReport>),

    /// Reading or writing the deploy stamp failed.
    #[error("toolchain stamp {}: {source}", .path.display())]
    Stamp {
        /// The stamp path involved.
        path: PathBuf,
        /// Underlying I/O or JSON failure.
        source: std::io::Error,
    },
}

/// `Debug` renders the variant name plus the operator-facing `Display` text,
/// NOT a field dump.
///
/// This error reaches `main()` in both server binaries, and Rust prints a
/// returned `Err` with `Debug` — a derived `Debug` there spilled the whole
/// `SkewReport` struct (every fingerprint, the entire stamp) and buried the
/// one sentence saying to run `scripts/redeploy.sh`. The struct dump had no
/// consumer; the message has one.
impl std::fmt::Debug for ToolchainError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let tag = match self {
            Self::ExtractNotFound { .. } => "ExtractNotFound",
            Self::PreludeDirInvalid { .. } => "PreludeDirInvalid",
            Self::StdlibNotFound { .. } => "StdlibNotFound",
            Self::Skew(_) => "Skew",
            Self::Stamp { .. } => "Stamp",
        };
        write!(f, "{tag}: {self}")
    }
}

fn render_tried(tried: &[(&'static str, PathBuf)]) -> String {
    tried
        .iter()
        .map(|(what, p)| format!("{what} ({})", p.display()))
        .collect::<Vec<_>>()
        .join(", ")
}

// ---------------------------------------------------------------------------
// Extract location — delegated
// ---------------------------------------------------------------------------

/// The resolved extract binary, typed so a caller cannot substitute a
/// guessed name for a real resolution.
///
/// Deliberately spelled without naming the std spawn constructor: this module
/// resolves and fingerprints, it never spawns, and
/// `tidepool-extract-cmd`'s `no_open_coded_extract_spawns` guard is a source
/// scan that (correctly) cannot tell prose from code. Spawning goes through
/// `ExtractCmd`, which is what makes the spawn counter complete.
///
/// Thin delegation to [`tidepool_extract_cmd::resolve_bin`], which owns the
/// extract-binary precedence (see the table in this module's docs). Kept as a
/// named entry point so a caller that only needs the resolved binary — the
/// harness's `EngineConfig` — does not have to reach into the invocation
/// crate.
///
/// STRICT, per the precedence table: a set-but-unreadable `$TIDEPOOL_EXTRACT`
/// is a hard error here too, never a silent fall-through to the bare
/// [`tidepool_extract_cmd::DEFAULT_BIN`] name — a caller that received the
/// bare name back would spawn a DIFFERENT binary than the override named, and
/// believe it was running the one it configured. An UNSET env falls back to
/// the bare name unchanged (`Ok`, not an error — [`ToolchainError::ExtractNotFound`]
/// is for [`locate_extract`]'s PATH-lookup failure, a distinct case).
///
/// # Errors
/// [`ToolchainError::ExtractNotFound`] when `$TIDEPOOL_EXTRACT` names an
/// unreadable file, naming the path, why it failed, and that unsetting the
/// var falls back to `$PATH`.
pub fn extract_command_name() -> Result<tidepool_extract_cmd::ResolvedExtractBin, ToolchainError> {
    tidepool_extract_cmd::resolve_bin()
        .map(tidepool_extract_cmd::ResolvedBin::into_extract_bin)
        .map_err(|e| ToolchainError::ExtractNotFound {
            tried: format!(
                "{ENV_EXTRACT} is set to {} but that is not a readable file; \
                 unset {ENV_EXTRACT} to fall back to $PATH lookup",
                e.path().display()
            ),
        })
}

/// A resolved extract binary: an ABSOLUTE path plus where it came from.
#[derive(Debug, Clone)]
pub struct ExtractLocation {
    /// Absolute path to the binary (or wrapper script) — resolved through
    /// `PATH` when `$TIDEPOOL_EXTRACT` is unset, so it is always a real file
    /// that can be fingerprinted.
    pub path: PathBuf,
    /// Which precedence step found it, straight from the locator crate.
    pub source: tidepool_extract_cmd::BinSource,
}

/// Resolve the extract to a real file on disk, typed-failing when there is
/// none.
///
/// [`tidepool_extract_cmd::resolve_bin`] decides WHICH binary; this adds the
/// `PATH` lookup its `PathLookup` case defers to the OS, because the handshake
/// and the degraded-setup probe need an absolute path, not a name to spawn.
///
/// # Errors
/// [`ToolchainError::ExtractNotFound`] when `$TIDEPOOL_EXTRACT` names an
/// unreadable file, or when the bare name is not on `$PATH`.
pub fn locate_extract() -> Result<ExtractLocation, ToolchainError> {
    let resolved =
        tidepool_extract_cmd::resolve_bin().map_err(|e| ToolchainError::ExtractNotFound {
            tried: e.to_string(),
        })?;
    // An Env-sourced path is already known-readable; a PathLookup one is the
    // bare name and still needs the OS search.
    which::which(&resolved.path)
        .map(|path| ExtractLocation {
            path,
            source: resolved.source,
        })
        .map_err(|_| ToolchainError::ExtractNotFound {
            tried: format!("{} on $PATH", resolved.path.display()),
        })
}

// ---------------------------------------------------------------------------
// Stdlib location
// ---------------------------------------------------------------------------

/// A directory is a stdlib root iff `Tidepool/Prelude.hs` sits under it — the
/// same probe every historical policy used, now stated once.
#[must_use]
pub fn is_stdlib_root(dir: &Path) -> bool {
    dir.join("Tidepool").join("Prelude.hs").is_file()
}

/// Which precedence step produced a stdlib root.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StdlibSource {
    /// `$TIDEPOOL_PRELUDE_DIR`.
    EnvOverride,
    /// `./haskell/lib` or `./lib`, relative to the process CWD.
    RepoTree,
    /// The `lib/` sibling of the extract's `dist-newstyle` tree.
    ExtractSibling,
    /// The stdlib bundled into the server binary, materialized to the cache.
    Bundle,
    /// The source tree this binary was built from.
    BuildTree,
}

/// A resolved stdlib root.
#[derive(Debug, Clone)]
pub struct StdlibLocation {
    /// The include dir to hand GHC (`Tidepool/Prelude.hs` lives under it).
    pub dir: PathBuf,
    /// Which precedence step found it.
    pub source: StdlibSource,
}

/// Binary-supplied tail steps of the stdlib precedence (steps 4 and 5). A
/// library caller with neither — e.g. session-decl validation inside
/// `tidepool-runtime` — passes [`StdlibFallbacks::default`] and gets steps 1–3.
#[derive(Debug, Default, Clone)]
pub struct StdlibFallbacks {
    /// Step 4: a materialized copy of the stdlib embedded in this binary.
    /// Materialize eagerly (it is sentinel-guarded and idempotent) and pass the
    /// directory; `None` for a binary that embeds no stdlib.
    pub bundle: Option<PathBuf>,
    /// Step 5: the source tree this binary was built from, typically
    /// `Path::new(env!("CARGO_MANIFEST_DIR")).parent()/haskell/lib`.
    pub build_tree: Option<PathBuf>,
}

/// Resolve the Haskell stdlib root by the precedence table in the module docs.
///
/// # Errors
/// - [`ToolchainError::PreludeDirInvalid`] when `$TIDEPOOL_PRELUDE_DIR` is set
///   but is not a stdlib root (a bad override never falls through silently).
/// - [`ToolchainError::StdlibNotFound`] when no step found one; the error names
///   every path tried.
pub fn locate_stdlib(fallbacks: &StdlibFallbacks) -> Result<StdlibLocation, ToolchainError> {
    let mut tried: Vec<(&'static str, PathBuf)> = Vec::new();

    // 1. Operator override — authoritative, and loud when wrong.
    if let Some(dir) = std::env::var_os(ENV_PRELUDE_DIR) {
        let dir = PathBuf::from(dir);
        if is_stdlib_root(&dir) {
            return Ok(StdlibLocation {
                dir,
                source: StdlibSource::EnvOverride,
            });
        }
        return Err(ToolchainError::PreludeDirInvalid { dir });
    }

    // 2. In-repo development: walk up from CWD, git-style. Walking (rather than
    //    probing CWD alone) is what makes this independent of which directory
    //    cargo/nextest/the MCP client happened to launch from — a test running
    //    with CWD=<repo>/tidepool-runtime finds the same stdlib as a server
    //    launched from the repo root.
    if let Ok(cwd) = std::env::current_dir() {
        let mut cur = Some(cwd.as_path());
        while let Some(dir) = cur {
            for candidate in [dir.join("haskell").join("lib"), dir.join("lib")] {
                if is_stdlib_root(&candidate) {
                    return Ok(StdlibLocation {
                        dir: candidate,
                        source: StdlibSource::RepoTree,
                    });
                }
            }
            cur = dir.parent();
        }
        tried.push(("repo tree above cwd", cwd.join("haskell").join("lib")));
    }

    // 3. The `lib/` sibling of the extract's `dist-newstyle` (absorbs the old
    //    `derive_stdlib_include`).
    if let Some(candidate) = extract_sibling_lib() {
        if is_stdlib_root(&candidate) {
            return Ok(StdlibLocation {
                dir: candidate,
                source: StdlibSource::ExtractSibling,
            });
        }
        tried.push(("extract dist-newstyle sibling", candidate));
    }

    // 4/5. Binary-supplied fallbacks.
    for (what, source, candidate) in [
        ("bundled stdlib", StdlibSource::Bundle, &fallbacks.bundle),
        ("build tree", StdlibSource::BuildTree, &fallbacks.build_tree),
    ] {
        let Some(candidate) = candidate else { continue };
        if is_stdlib_root(candidate) {
            return Ok(StdlibLocation {
                dir: candidate.clone(),
                source,
            });
        }
        tried.push((what, candidate.clone()));
    }

    Err(ToolchainError::StdlibNotFound { tried })
}

/// Walk `$TIDEPOOL_EXTRACT` upward to a `dist-newstyle` component and return
/// its sibling `lib/`. `None` when the override is unset or the binary does not
/// live inside a cabal build tree (the installed/nix case).
fn extract_sibling_lib() -> Option<PathBuf> {
    let extract = std::env::var_os(ENV_EXTRACT)?;
    let mut path = PathBuf::from(extract);
    if path.as_os_str().is_empty() {
        return None;
    }
    loop {
        if path.file_name().and_then(|n| n.to_str()) == Some("dist-newstyle") {
            return path.parent().map(|parent| parent.join("lib"));
        }
        if !path.pop() {
            return None;
        }
    }
}

// ---------------------------------------------------------------------------
// Fingerprints
// ---------------------------------------------------------------------------

/// Content fingerprint of the extract binary at `path`, following a one-line
/// wrapper script to its target (the nix-profile wrapper `exec`s the store
/// binary; fingerprinting only the wrapper would miss every upgrade).
///
/// Shares the memoized content hasher with the compile-cache key
/// ([`crate::cache`]), so the ~100ms read of a GHC-linked binary is paid at
/// most once per version per machine.
#[must_use]
pub fn extract_fingerprint(path: &Path) -> String {
    let mut hasher = blake3::Hasher::new();
    hasher.update(&crate::cache::binary_content_hash(path));
    for target in crate::cache::wrapper_targets(path) {
        hasher.update(&crate::cache::binary_content_hash(&target));
    }
    hasher.finalize().to_hex().to_string()
}

/// Content fingerprint of a stdlib tree rooted at `dir`.
///
/// Hashes `(path relative to `dir`, blake3(contents))` for every `.hs` file,
/// in sorted order, so the same content at two different absolute paths — the
/// repo `haskell/lib` the stamp is written from and the materialized bundle a
/// deployed server resolves — fingerprints identically.
///
/// **The filter mirrors `tidepool/build.rs`'s embed filter exactly**: `.hs`
/// only, skipping the `Internal/` probe and the `Prelude_cbor/` build
/// artifacts. If that filter changes, this must change with it, or a deployed
/// server will report skew against its own bundle.
#[must_use]
pub fn stdlib_fingerprint(dir: &Path) -> String {
    // Root the walk at `Tidepool/`, not at `dir`, because that is exactly what
    // `tidepool/build.rs` embeds. Walking `dir` itself would count a stray
    // `haskell/lib/Scratch.hs` that never ships, and every deployed server
    // would then report skew against its own bundle.
    let root = dir.join("Tidepool");
    let mut files: Vec<(String, PathBuf)> = Vec::new();
    collect_stdlib_files(&root, &root, &mut files);
    files.sort_by(|a, b| a.0.cmp(&b.0));

    let mut hasher = blake3::Hasher::new();
    hasher.update(&(files.len() as u64).to_le_bytes());
    for (rel, abs) in &files {
        hasher.update(&(rel.len() as u64).to_le_bytes());
        hasher.update(rel.as_bytes());
        match std::fs::read(abs) {
            Ok(bytes) => hasher.update(blake3::hash(&bytes).as_bytes()),
            Err(_) => hasher.update(b"<unreadable>"),
        };
    }
    hasher.finalize().to_hex().to_string()
}

/// Excluded directory names — kept in lockstep with `tidepool/build.rs`.
const STDLIB_SKIP_DIRS: [&str; 2] = ["Internal", "Prelude_cbor"];

fn collect_stdlib_files(root: &Path, dir: &Path, out: &mut Vec<(String, PathBuf)>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            let name = path.file_name().unwrap_or_default().to_string_lossy();
            if STDLIB_SKIP_DIRS.contains(&name.as_ref()) {
                continue;
            }
            collect_stdlib_files(root, &path, out);
        } else if path.extension().is_some_and(|e| e == "hs") {
            if let Ok(rel) = path.strip_prefix(root) {
                out.push((rel.to_string_lossy().replace('\\', "/"), path.clone()));
            }
        }
    }
}

// ---------------------------------------------------------------------------
// The deploy stamp + handshake
// ---------------------------------------------------------------------------

/// Wire-format version of the stamp. Bump when the compared fields change; a
/// stamp with a different schema is treated as absent (warn, don't fail — an
/// old stamp must not brick a newer server).
pub const STAMP_SCHEMA: u32 = 1;

/// The (extract, stdlib) pair that was last deployed together.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ToolchainStamp {
    /// [`STAMP_SCHEMA`] at write time.
    pub schema: u32,
    /// [`extract_fingerprint`] of the deployed extract.
    pub extract: String,
    /// [`stdlib_fingerprint`] of the deployed stdlib tree.
    pub stdlib: String,
    /// Informational: where the extract was when the stamp was written.
    pub extract_path: String,
    /// Informational: where the stdlib was when the stamp was written.
    pub stdlib_path: String,
    /// Informational: what wrote it (binary name + version).
    pub written_by: String,
}

/// Where the deploy stamp lives: `$TIDEPOOL_TOOLCHAIN_STAMP`, else
/// `<cache_dir>/toolchain-stamp.json`.
///
/// It sits in the cache root deliberately: `scripts/redeploy.sh` clears that
/// root and then rewrites the stamp, so a hand-cleared cache degrades to
/// "no stamp" (a warning) rather than to a stale stamp (a false alarm).
#[must_use]
pub fn stamp_path() -> PathBuf {
    if let Some(p) = std::env::var_os(ENV_STAMP) {
        return PathBuf::from(p);
    }
    crate::paths::cache_dir().join("toolchain-stamp.json")
}

/// Read the stamp. `Ok(None)` when the file is absent (or unreadable at the
/// filesystem level, e.g. a permission error — nothing to compare against,
/// same as absent), or when it parses cleanly but was written by a different
/// [`STAMP_SCHEMA`] (an old/future stamp must never brick a server).
///
/// A stamp file that EXISTS, was read, and fails to parse as valid JSON in
/// the current schema's shape is CORRUPT, not absent, and is no longer
/// indistinguishable from it: this returns [`ToolchainError::Stamp`], and
/// [`enforce_handshake`] applies the severity policy (fail closed in `Error`,
/// log-and-continue in `Warn`) on top.
///
/// # Errors
/// [`ToolchainError::Stamp`] when the stamp file exists but its content does
/// not parse as JSON in the [`ToolchainStamp`] shape — a truncated/partial
/// write that somehow survived [`write_stamp`]'s atomic rename, or
/// hand-corrupted content.
pub fn read_stamp(path: &Path) -> Result<Option<ToolchainStamp>, ToolchainError> {
    let Ok(text) = std::fs::read_to_string(path) else {
        return Ok(None);
    };
    match serde_json::from_str::<ToolchainStamp>(&text) {
        Ok(s) if s.schema == STAMP_SCHEMA => Ok(Some(s)),
        Ok(_) => Ok(None),
        Err(source) => Err(ToolchainError::Stamp {
            path: path.to_path_buf(),
            source: std::io::Error::new(std::io::ErrorKind::InvalidData, source),
        }),
    }
}

/// Fingerprint `extract` + `stdlib` and write the stamp to [`stamp_path`].
/// Called by `scripts/redeploy.sh` via `tidepool --write-toolchain-stamp`, so
/// the writer and the checker share one implementation and cannot drift.
///
/// Written ATOMICALLY: the content lands in a uniquely-named temp file in the
/// stamp's own directory (so the rename below stays on one filesystem),
/// `sync_all`'d to disk, then renamed over the live stamp in one syscall —
/// a reader (this process's own next startup, or a concurrent one) can never
/// observe a short/partial write. The directory entry is synced afterward so
/// the rename itself, not just the temp file's bytes, survives a crash.
///
/// # Errors
/// [`ToolchainError::Stamp`] if the stamp cannot be created or written.
pub fn write_stamp(extract: &Path, stdlib: &Path) -> Result<ToolchainStamp, ToolchainError> {
    let stamp = ToolchainStamp {
        schema: STAMP_SCHEMA,
        extract: extract_fingerprint(extract),
        stdlib: stdlib_fingerprint(stdlib),
        extract_path: extract.display().to_string(),
        stdlib_path: stdlib.display().to_string(),
        written_by: format!("tidepool {}", env!("CARGO_PKG_VERSION")),
    };
    let path = stamp_path();
    let write = || -> std::io::Result<()> {
        let parent = path
            .parent()
            .filter(|p| !p.as_os_str().is_empty())
            .unwrap_or_else(|| Path::new("."));
        std::fs::create_dir_all(parent)?;
        let json = serde_json::to_string_pretty(&stamp)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;

        let mut tmp = tempfile::NamedTempFile::new_in(parent)?;
        tmp.write_all(json.as_bytes())?;
        tmp.as_file().sync_all()?;
        tmp.persist(&path).map_err(|e| e.error)?;

        std::fs::File::open(parent)?.sync_all()?;
        Ok(())
    };
    write().map_err(|source| ToolchainError::Stamp {
        path: path.clone(),
        source,
    })?;
    Ok(stamp)
}

/// Which side of the pair moved.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SkewSide {
    /// The extract binary differs from the deployed one.
    Extract,
    /// The stdlib tree differs from the deployed one.
    Stdlib,
}

/// A detected extract/stdlib skew, rendered as the operator-facing message.
#[derive(Debug, Clone)]
pub struct SkewReport {
    /// Which side(s) moved.
    pub sides: Vec<SkewSide>,
    /// The extract path in use now.
    pub extract_path: PathBuf,
    /// The stdlib path in use now.
    pub stdlib_path: PathBuf,
    /// The stamp that was compared against.
    pub stamp: ToolchainStamp,
    /// Fingerprint of the extract in use now.
    pub extract_now: String,
    /// Fingerprint of the stdlib in use now.
    pub stdlib_now: String,
}

impl std::fmt::Display for SkewReport {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        writeln!(
            f,
            "toolchain skew: the extract binary and the Haskell stdlib in use were not deployed together."
        )?;
        for side in &self.sides {
            match side {
                SkewSide::Extract => writeln!(
                    f,
                    "  extract CHANGED: {} (now {}, deployed {})",
                    self.extract_path.display(),
                    short(&self.extract_now),
                    short(&self.stamp.extract),
                )?,
                SkewSide::Stdlib => writeln!(
                    f,
                    "  stdlib  CHANGED: {} (now {}, deployed {})",
                    self.stdlib_path.display(),
                    short(&self.stdlib_now),
                    short(&self.stamp.stdlib),
                )?,
            }
        }
        write!(
            f,
            "Running this pair produces wrong answers at eval time (unresolved imports, \
             malformed extract metadata) rather than a clean error. Fix it with `{REDEPLOY}`, \
             which moves the extract, both servers, and the stdlib together and rewrites the \
             stamp. To run a deliberately mixed pair (e.g. testing a worktree extract), set \
             {ENV_HANDSHAKE}=warn."
        )
    }
}

fn short(hex: &str) -> &str {
    &hex[..hex.len().min(12)]
}

/// What [`check_handshake`] found.
#[derive(Debug, Clone)]
pub enum HandshakeOutcome {
    /// Fingerprints match the stamp.
    Match,
    /// No usable stamp — nothing was ever deployed through `scripts/redeploy.sh`
    /// on this machine (or the cache was cleared since). Informational only.
    NoStamp {
        /// Where the stamp was looked for.
        path: PathBuf,
    },
    /// The pair in use is not the pair that was deployed.
    Skew(Box<SkewReport>),
}

/// Compare the located toolchain against the deploy stamp. Pure detection —
/// [`enforce_handshake`] applies the severity policy.
///
/// # Errors
/// [`ToolchainError::Stamp`] when the stamp file exists but its content is
/// corrupt ([`read_stamp`] fails closed rather than treating it as absent).
pub fn check_handshake(extract: &Path, stdlib: &Path) -> Result<HandshakeOutcome, ToolchainError> {
    let path = stamp_path();
    let Some(stamp) = read_stamp(&path)? else {
        return Ok(HandshakeOutcome::NoStamp { path });
    };

    let extract_now = extract_fingerprint(extract);
    let stdlib_now = stdlib_fingerprint(stdlib);
    let mut sides = Vec::new();
    if extract_now != stamp.extract {
        sides.push(SkewSide::Extract);
    }
    if stdlib_now != stamp.stdlib {
        sides.push(SkewSide::Stdlib);
    }
    if sides.is_empty() {
        return Ok(HandshakeOutcome::Match);
    }
    Ok(HandshakeOutcome::Skew(Box::new(SkewReport {
        sides,
        extract_path: extract.to_path_buf(),
        stdlib_path: stdlib.to_path_buf(),
        stamp,
        extract_now,
        stdlib_now,
    })))
}

/// Handshake severity, from `$TIDEPOOL_TOOLCHAIN_HANDSHAKE`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HandshakeSeverity {
    /// Default: a skew aborts startup.
    Error,
    /// Log the skew and continue — the escape hatch for deliberately testing a
    /// worktree extract against an installed server.
    Warn,
    /// Skip the check entirely (also skips the fingerprint work).
    Off,
}

impl HandshakeSeverity {
    /// Read `$TIDEPOOL_TOOLCHAIN_HANDSHAKE`. An unrecognized value is `Error`:
    /// a typo must not silently disable the check.
    #[must_use]
    pub fn from_env() -> Self {
        match std::env::var(ENV_HANDSHAKE).unwrap_or_default().as_str() {
            "warn" => Self::Warn,
            "off" => Self::Off,
            _ => Self::Error,
        }
    }
}

/// Run the startup handshake and apply the severity policy. Call once, at
/// server startup, after the toolchain is located — never per-eval.
///
/// Returns the outcome so the caller can log the non-fatal cases with its own
/// subscriber (this crate stays quiet by default) — EXCEPT a corrupt stamp
/// under [`HandshakeSeverity::Warn`], which this function logs directly via
/// `tracing::warn!` before degrading to [`HandshakeOutcome::NoStamp`]: the
/// corrupt-vs-genuinely-absent distinction has no home in that variant (it
/// carries only a path), and widening [`HandshakeOutcome`] with a new arm
/// would break every existing exhaustive match on it outside this crate.
///
/// # Errors
/// [`ToolchainError::Skew`] when a skew is detected and severity is
/// [`HandshakeSeverity::Error`]. [`ToolchainError::Stamp`] when the stamp is
/// corrupt (fails to parse) and severity is [`HandshakeSeverity::Error`] —
/// fail closed rather than silently treat corruption as no stamp.
pub fn enforce_handshake(
    extract: &Path,
    stdlib: &Path,
) -> Result<HandshakeOutcome, ToolchainError> {
    let severity = HandshakeSeverity::from_env();
    if severity == HandshakeSeverity::Off {
        return Ok(HandshakeOutcome::NoStamp { path: stamp_path() });
    }
    let outcome = match check_handshake(extract, stdlib) {
        Ok(outcome) => outcome,
        // The only error `check_handshake` can produce is a corrupt stamp
        // (`read_stamp` fails closed on a parse failure). `Error` severity
        // propagates it as-is below (fail closed); `Warn` logs it here and
        // degrades to the same no-stamp-detected outcome rather than
        // aborting startup.
        Err(e) if severity == HandshakeSeverity::Warn => {
            tracing::warn!("toolchain stamp is corrupt, treating as absent: {e}");
            HandshakeOutcome::NoStamp { path: stamp_path() }
        }
        Err(e) => return Err(e),
    };
    match (&outcome, severity) {
        (HandshakeOutcome::Skew(report), HandshakeSeverity::Error) => {
            Err(ToolchainError::Skew(report.clone()))
        }
        _ => Ok(outcome),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serial_test::serial;

    /// A set-but-nonexistent `$TIDEPOOL_EXTRACT` must be a hard error out of
    /// [`extract_command_name`], naming the offending path — the exact
    /// silent-degrade-to-`$PATH` failure this module exists to close (a
    /// caller must never spawn a different binary than the override named).
    /// Unsetting the var is the other half of the contract: it must fall
    /// back to the bare `$PATH` name unchanged, not become an error too.
    #[test]
    #[serial]
    fn extract_command_name_is_strict_about_a_bad_override() {
        let original = std::env::var(ENV_EXTRACT).ok();

        let bad_path = "/nonexistent/tidepool-extract-loudfail-test";
        std::env::set_var(ENV_EXTRACT, bad_path);
        let err = extract_command_name().unwrap_err();
        assert!(
            matches!(err, ToolchainError::ExtractNotFound { .. }),
            "expected ExtractNotFound, got {err:?}"
        );
        let msg = err.to_string();
        assert!(msg.contains(bad_path), "message must name the path: {msg}");
        assert!(
            msg.contains("$PATH"),
            "message must say unsetting falls back to $PATH: {msg}"
        );

        std::env::remove_var(ENV_EXTRACT);
        assert_eq!(
            extract_command_name().unwrap().as_path(),
            Path::new(tidepool_extract_cmd::DEFAULT_BIN),
            "an unset override must still fall back to the bare $PATH name"
        );

        match original {
            Some(v) => std::env::set_var(ENV_EXTRACT, v),
            None => std::env::remove_var(ENV_EXTRACT),
        }
    }

    fn write_stdlib(root: &Path, prelude_body: &str) {
        let tp = root.join("Tidepool");
        std::fs::create_dir_all(tp.join("Internal")).unwrap();
        std::fs::write(tp.join("Prelude.hs"), prelude_body).unwrap();
        std::fs::write(tp.join("Table.hs"), "module Tidepool.Table where\n").unwrap();
        // Excluded by the filter — must not move the fingerprint.
        std::fs::write(tp.join("Internal").join("Probe.hs"), "probe\n").unwrap();
        std::fs::write(tp.join("notes.txt"), "not haskell\n").unwrap();
    }

    /// The fingerprint is content-addressed, not path-addressed: the same tree
    /// materialized at two paths (repo vs bundle) must compare equal, or every
    /// deployed server would report skew against its own bundle.
    #[test]
    fn stdlib_fingerprint_is_path_independent() {
        let a = tempfile::TempDir::new().unwrap();
        let b = tempfile::TempDir::new().unwrap();
        write_stdlib(a.path(), "module Tidepool.Prelude where\n");
        write_stdlib(b.path(), "module Tidepool.Prelude where\n");
        assert_eq!(
            stdlib_fingerprint(a.path()),
            stdlib_fingerprint(b.path()),
            "same content at different paths must fingerprint identically"
        );
    }

    /// A content edit to any shipped `.hs` moves the fingerprint; an edit to a
    /// filtered-out file does not (it is not part of what ships).
    #[test]
    fn stdlib_fingerprint_tracks_shipped_content_only() {
        let dir = tempfile::TempDir::new().unwrap();
        write_stdlib(dir.path(), "module Tidepool.Prelude where\n");
        let base = stdlib_fingerprint(dir.path());

        std::fs::write(
            dir.path()
                .join("Tidepool")
                .join("Internal")
                .join("Probe.hs"),
            "different probe\n",
        )
        .unwrap();
        assert_eq!(
            base,
            stdlib_fingerprint(dir.path()),
            "Internal/ is excluded from the embed, so it must not move the fingerprint"
        );

        std::fs::write(
            dir.path().join("Tidepool").join("Prelude.hs"),
            "module Tidepool.Prelude where\nnewThing = ()\n",
        )
        .unwrap();
        assert_ne!(
            base,
            stdlib_fingerprint(dir.path()),
            "a shipped stdlib edit must move the fingerprint"
        );
    }

    #[test]
    fn is_stdlib_root_probes_prelude() {
        let dir = tempfile::TempDir::new().unwrap();
        assert!(!is_stdlib_root(dir.path()));
        write_stdlib(dir.path(), "module Tidepool.Prelude where\n");
        assert!(is_stdlib_root(dir.path()));
    }

    /// A set-but-wrong `$TIDEPOOL_PRELUDE_DIR` must be a hard error, never a
    /// silent fall-through to some other stdlib — a typo'd override that
    /// quietly served a different tree is the exact failure this module exists
    /// to kill.
    #[test]
    fn prelude_dir_override_that_is_not_a_stdlib_root_is_fatal() {
        let dir = tempfile::TempDir::new().unwrap();
        std::env::set_var(ENV_PRELUDE_DIR, dir.path());
        let err = locate_stdlib(&StdlibFallbacks::default()).unwrap_err();
        std::env::remove_var(ENV_PRELUDE_DIR);
        assert!(
            matches!(err, ToolchainError::PreludeDirInvalid { .. }),
            "expected PreludeDirInvalid, got {err:?}"
        );
        assert!(err.to_string().contains("Tidepool/Prelude.hs"));
    }

    /// Isolate the machine-wide fingerprint sidecar + stamp so a test never
    /// reads or writes the developer's real cache.
    fn isolate_cache(tmp: &Path) {
        std::env::set_var("XDG_CACHE_HOME", tmp.join("cache"));
        std::env::set_var(ENV_STAMP, tmp.join("stamp.json"));
    }

    /// The handshake's whole job: a stamp recorded at deploy time, then ONE
    /// side of the pair moves, and startup says so loudly instead of serving a
    /// mixed toolchain that fails at eval time.
    #[test]
    fn handshake_detects_a_skewed_extract_and_names_the_redeploy_script() {
        let tmp = tempfile::TempDir::new().unwrap();
        isolate_cache(tmp.path());
        let stdlib = tmp.path().join("lib");
        write_stdlib(&stdlib, "module Tidepool.Prelude where\n");
        let extract = tmp.path().join("tidepool-extract");
        std::fs::write(&extract, b"deployed extract v1").unwrap();

        // Deploy: both sides blessed together.
        write_stamp(&extract, &stdlib).unwrap();
        assert!(
            matches!(
                check_handshake(&extract, &stdlib).unwrap(),
                HandshakeOutcome::Match
            ),
            "the pair that was just stamped must match"
        );

        // A `nix profile upgrade tidepool-extract` without a full redeploy.
        std::fs::write(&extract, b"upgraded extract v2 -- larger").unwrap();

        let outcome = check_handshake(&extract, &stdlib).unwrap();
        let HandshakeOutcome::Skew(report) = outcome else {
            panic!("expected skew, got {outcome:?}");
        };
        assert_eq!(report.sides, vec![SkewSide::Extract]);

        // Default severity aborts startup, and the message is actionable.
        std::env::remove_var(ENV_HANDSHAKE);
        let err = enforce_handshake(&extract, &stdlib).unwrap_err();
        let msg = err.to_string();
        assert!(matches!(err, ToolchainError::Skew(_)), "got {err:?}");
        assert!(msg.contains(REDEPLOY), "message must name the fix: {msg}");
        assert!(
            msg.contains(ENV_HANDSHAKE),
            "message must name the escape hatch: {msg}"
        );

        // `warn` is the documented escape hatch for a deliberately mixed pair.
        std::env::set_var(ENV_HANDSHAKE, "warn");
        assert!(
            matches!(
                enforce_handshake(&extract, &stdlib).unwrap(),
                HandshakeOutcome::Skew(_)
            ),
            "warn severity reports the skew but does not fail"
        );
        std::env::remove_var(ENV_HANDSHAKE);
    }

    /// A stdlib edit with an unchanged extract is the other half of the pair,
    /// and must be caught the same way — this is the "edited haskell/lib, never
    /// redeployed" case.
    #[test]
    fn handshake_detects_a_skewed_stdlib() {
        let tmp = tempfile::TempDir::new().unwrap();
        isolate_cache(tmp.path());
        let stdlib = tmp.path().join("lib");
        write_stdlib(&stdlib, "module Tidepool.Prelude where\n");
        let extract = tmp.path().join("tidepool-extract");
        std::fs::write(&extract, b"deployed extract v1").unwrap();
        write_stamp(&extract, &stdlib).unwrap();

        std::fs::write(
            stdlib.join("Tidepool").join("Prelude.hs"),
            "module Tidepool.Prelude where\nadded = ()\n",
        )
        .unwrap();

        let HandshakeOutcome::Skew(report) = check_handshake(&extract, &stdlib).unwrap() else {
            panic!("a stdlib edit must skew");
        };
        assert_eq!(report.sides, vec![SkewSide::Stdlib]);
    }

    /// No stamp means nobody has deployed through `scripts/redeploy.sh` on this
    /// machine (or the cache was hand-cleared). That is informational — a fresh
    /// checkout must not be unable to start.
    #[test]
    fn handshake_without_a_stamp_is_informational() {
        let tmp = tempfile::TempDir::new().unwrap();
        isolate_cache(tmp.path());
        let stdlib = tmp.path().join("lib");
        write_stdlib(&stdlib, "module Tidepool.Prelude where\n");
        let extract = tmp.path().join("tidepool-extract");
        std::fs::write(&extract, b"extract").unwrap();
        std::env::remove_var(ENV_HANDSHAKE);

        assert!(matches!(
            enforce_handshake(&extract, &stdlib).unwrap(),
            HandshakeOutcome::NoStamp { .. }
        ));
    }

    /// A stamp from a future (or ancient) schema is treated as absent, not as a
    /// skew: an old stamp must never brick a newer server.
    #[test]
    fn stamp_with_a_foreign_schema_reads_as_absent() {
        let tmp = tempfile::TempDir::new().unwrap();
        let path = tmp.path().join("stamp.json");
        std::fs::write(
            &path,
            serde_json::json!({
                "schema": STAMP_SCHEMA + 1,
                "extract": "aa", "stdlib": "bb",
                "extract_path": "/x", "stdlib_path": "/y", "written_by": "future",
            })
            .to_string(),
        )
        .unwrap();
        assert!(read_stamp(&path).unwrap().is_none());
    }

    /// Simulates a crash between `NamedTempFile` creation and `persist`'s
    /// rename: a leftover temp file sits next to a real, already-written
    /// stamp. The real stamp must read back intact (the leftover is a
    /// different path entirely — atomic rename means a torn write can never
    /// land ON the stamp's own path), and a later, ordinary write must still
    /// succeed (its own uniquely-named temp file never collides with the
    /// leftover).
    #[test]
    fn write_stamp_is_atomic_against_an_interrupted_write() {
        let tmp = tempfile::TempDir::new().unwrap();
        isolate_cache(tmp.path());
        let stdlib = tmp.path().join("lib");
        write_stdlib(&stdlib, "module Tidepool.Prelude where\n");
        let extract = tmp.path().join("tidepool-extract");
        std::fs::write(&extract, b"deployed extract v1").unwrap();

        let deployed = write_stamp(&extract, &stdlib).unwrap();

        let path = stamp_path();
        let leftover = path.parent().unwrap().join(".toolchain-stamp.tmp-leftover");
        std::fs::write(&leftover, b"{ garbage, not json, never persisted").unwrap();

        let read_back = read_stamp(&path).unwrap().expect("stamp intact");
        assert_eq!(read_back.extract, deployed.extract);
        assert_eq!(read_back.stdlib, deployed.stdlib);

        // A fresh write is unaffected by the stray leftover and still lands
        // cleanly.
        write_stamp(&extract, &stdlib).unwrap();
        assert!(
            read_stamp(&path).unwrap().is_some(),
            "a later write must still succeed with a leftover temp file present"
        );
        assert!(
            leftover.exists(),
            "a fresh write must not touch an unrelated leftover file"
        );
    }

    /// The stamp protocol's other half: content-level corruption (not a
    /// partial write, but e.g. a hand-edited or truncated-and-then-appended
    /// file) must fail closed under the default `Error` severity, and be
    /// reported — not silently swallowed as "no stamp" — under `Warn`.
    #[test]
    fn corrupt_stamp_fails_closed_in_error_and_warns_in_warn() {
        let tmp = tempfile::TempDir::new().unwrap();
        isolate_cache(tmp.path());
        let stdlib = tmp.path().join("lib");
        write_stdlib(&stdlib, "module Tidepool.Prelude where\n");
        let extract = tmp.path().join("tidepool-extract");
        std::fs::write(&extract, b"deployed extract v1").unwrap();

        let path = stamp_path();
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, b"{ not valid json at all").unwrap();

        // `read_stamp` itself distinguishes corrupt from absent.
        let err = read_stamp(&path).unwrap_err();
        assert!(matches!(err, ToolchainError::Stamp { .. }), "got {err:?}");

        // Default severity fails closed rather than proceeding as if
        // nothing were deployed.
        std::env::remove_var(ENV_HANDSHAKE);
        let err = enforce_handshake(&extract, &stdlib).unwrap_err();
        assert!(matches!(err, ToolchainError::Stamp { .. }), "got {err:?}");

        // `warn` degrades gracefully (logs and continues) instead of
        // aborting — observably, it must not error, and must not be
        // mistaken for a genuine deploy (it degrades to the same
        // no-stamp-detected outcome `check_handshake` would report for an
        // absent file).
        std::env::set_var(ENV_HANDSHAKE, "warn");
        let outcome = enforce_handshake(&extract, &stdlib).unwrap();
        assert!(
            matches!(outcome, HandshakeOutcome::NoStamp { .. }),
            "got {outcome:?}"
        );
        std::env::remove_var(ENV_HANDSHAKE);
    }
}
