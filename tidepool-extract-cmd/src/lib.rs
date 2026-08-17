//! The ONE place a `tidepool-extract` process is built and spawned.
//!
//! What lives here:
//!
//! - [`resolve_bin`] — binary resolution, with `tidepool-macro`'s STRICT
//!   policy as the default for everyone: a SET-but-unreadable
//!   `$TIDEPOOL_EXTRACT` is a hard error, never a silent fall-through to
//!   `PATH`; an UNSET one falls back to the bare `tidepool-extract` name.
//! - [`ExtractCmd`] — typed argument construction for every mode the tree
//!   drives (`--target`/`--targets`/`--turn`/`--classify`/`--session-*`/
//!   `--include`/`--output-dir`/…).
//! - The spawn itself, which increments [`extract_spawn_count`] on every
//!   successful spawn, so the counter is correct BY CONSTRUCTION rather than
//!   by everyone remembering to bump it.
//! - [`ExitPolicy`] — what a NON-ZERO exit means at this call site, as an
//!   explicit named enum rather than a comment copy-pasted between sites.
//!
//! What deliberately does NOT live here: any parsing of the extract's output.
//! Diagnostics reports are JSON and the payloads are CBOR, which would mean
//! `serde_json`/`ciborium` dependencies — and this crate is **std-only on
//! purpose** (D-A: `tidepool-macro` is a proc-macro crate and must not grow a
//! dependency on the runtime graph). [`ExtractCmd::run`] hands back the raw
//! [`std::process::Output`] plus the classification verdict, and each caller
//! maps that onto its own error type (`CompileError`, `SessionError`,
//! `String`).
//!
//! Nor does the **compile memo**, for the same reason. Keying a whole
//! invocation is the natural job for the crate that BUILDS the invocation, and
//! `tidepool_runtime::cache::invocation_key` is written to accept exactly what
//! [`ExtractCmd::argv`] returns so the builder could move down here later. It
//! has not, because a content-addressed memo needs blake3 and atomic
//! tempfile-rename, and D-A's whole point is that this crate's dependency list
//! is paid by every crate that transitively expands `haskell_eval!`. So the
//! memo lives one layer up, over this crate's argv, and the callers that want
//! it (`tidepool_runtime::compile_haskell`, `tidepool_harness::compile`)
//! consult it before calling [`ExtractCmd::run`]. See `plans/compile-memo.md`.

use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

/// The bare binary name, used when `$TIDEPOOL_EXTRACT` is unset (resolved
/// through `PATH` by the OS at spawn time).
pub const DEFAULT_BIN: &str = "tidepool-extract";

// ---------------------------------------------------------------------------
// Process-global spawn counter (`tidepool-harness/src/compile.rs` re-exports
// these three items so its public surface is unchanged).
// ---------------------------------------------------------------------------

/// Process-global count of `tidepool-extract` spawns paid through
/// [`ExtractCmd::run`] — the extract-wave `boot` item's done-criterion needs a
/// live receipt that the self-iterating harness's pre-model-call compile
/// count actually dropped (see `plans/post-restart/extract-wave/boot/00-spec.md`),
/// and since every spawn site in the workspace funnels through this crate,
/// the count covers the PROCESS rather than one lane.
/// PROCESS-GLOBAL, not per-`Harness`/per-node: a test asserting
/// on it must run as its own test binary so no other test's compiles land on
/// the same count (nextest already gives one process per test binary).
static EXTRACT_SPAWNS: AtomicU64 = AtomicU64::new(0);

/// Number of `tidepool-extract` spawns this process has paid so far.
/// `Ordering::SeqCst` so a reader on another thread (the acceptance test's
/// snapshot, taken from inside a model-provider callback running on a
/// different thread than the compiling turn) is guaranteed to see every
/// increment a compiling thread has performed before this call.
pub fn extract_spawn_count() -> u64 {
    EXTRACT_SPAWNS.load(Ordering::SeqCst)
}

/// Reset the process-global spawn counter to zero. For test isolation within
/// a single test binary that drives more than one spawn-reaching launch and
/// wants each launch's count in isolation.
pub fn reset_extract_spawn_count() {
    EXTRACT_SPAWNS.store(0, Ordering::SeqCst);
}

// ---------------------------------------------------------------------------
// Binary resolution
// ---------------------------------------------------------------------------

/// Where a resolved extract binary name came from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BinSource {
    /// `$TIDEPOOL_EXTRACT` was set (and named a readable file).
    Env,
    /// `$TIDEPOOL_EXTRACT` was unset — the bare [`DEFAULT_BIN`] name, to be
    /// resolved through `PATH` by the OS. This is the ONLY source for which a
    /// caller may legitimately fall back to some other launcher on a
    /// not-found spawn error (see [`Launcher`]).
    PathLookup,
    /// The caller supplied the binary itself ([`ExtractCmd::with_bin`]) —
    /// it resolved the env once at construction and holds the result.
    Explicit,
}

/// A resolved extract binary plus where it came from.
#[derive(Clone, Debug)]
pub struct ResolvedBin {
    pub path: PathBuf,
    pub source: BinSource,
}

impl ResolvedBin {
    /// Discard [`BinSource`] and keep just the resolved path, typed as the
    /// one thing [`ExtractCmd::with_bin`] and `EngineConfig::extract_bin`
    /// accept. The source is irrelevant past this point: a caller that
    /// resolved once and threads the binary through many spawns treats it as
    /// [`BinSource::Explicit`] from here on, same as [`ExtractCmd::with_bin`]
    /// always has.
    #[must_use]
    pub fn into_extract_bin(self) -> ResolvedExtractBin {
        ResolvedExtractBin(self.path)
    }
}

/// A `tidepool-extract` binary path that came from [`resolve_bin`] (via
/// [`ResolvedBin::into_extract_bin`]) or from the explicit
/// [`ResolvedExtractBin::assume_resolved`] escape hatch — the ONLY two ways
/// to construct one.
///
/// This is what [`ExtractCmd::with_bin`] and `EngineConfig::extract_bin`
/// require, instead of an arbitrary string: an adapter that catches a
/// [`BinError`] can no longer paper over it by substituting a guessed
/// binary name, because there is no `From<String>`/`FromStr` impl to reach
/// for — only a real resolution or a call to the named escape hatch, which
/// shows up in review.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ResolvedExtractBin(PathBuf);

impl ResolvedExtractBin {
    /// Bypass resolution entirely. For tests, fixtures, and callers that
    /// already hold a pre-verified path — NOT a general-purpose way to turn a
    /// guessed string into a "resolved" binary. Every call site is a
    /// deliberate, reviewable exception to "only `resolve_bin` decides".
    #[must_use]
    pub fn assume_resolved(path: impl Into<PathBuf>) -> Self {
        ResolvedExtractBin(path.into())
    }

    #[must_use]
    pub fn as_path(&self) -> &Path {
        &self.0
    }

    #[must_use]
    pub fn into_os_string(self) -> OsString {
        self.0.into_os_string()
    }
}

/// Read-only path access — `.display()`, `.is_file()`, and friends work
/// without every caller reaching for [`ResolvedExtractBin::as_path`] first.
/// This does not weaken construction: `Deref` only exposes `Path`'s
/// existing shared-reference methods, none of which can produce a new
/// `ResolvedExtractBin`.
impl std::ops::Deref for ResolvedExtractBin {
    type Target = Path;

    fn deref(&self) -> &Path {
        &self.0
    }
}

impl AsRef<Path> for ResolvedExtractBin {
    fn as_ref(&self) -> &Path {
        &self.0
    }
}

impl AsRef<OsStr> for ResolvedExtractBin {
    fn as_ref(&self) -> &OsStr {
        self.0.as_os_str()
    }
}

/// Renders as the path's display form — the same text
/// `resolve_bin().path.to_string_lossy()` produced before this type existed,
/// so a call site that stringifies for a fingerprint or log line keeps a
/// byte-identical rendering.
impl std::fmt::Display for ResolvedExtractBin {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0.display())
    }
}

/// `$TIDEPOOL_EXTRACT` is set but does not name a readable file.
#[derive(Clone, Debug)]
pub struct BinError {
    path: PathBuf,
}

impl BinError {
    /// The unreadable path `$TIDEPOOL_EXTRACT` named.
    pub fn path(&self) -> &Path {
        &self.path
    }
}

impl std::fmt::Display for BinError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "$TIDEPOOL_EXTRACT is set to {} but that is not a readable file",
            self.path.display()
        )
    }
}

impl std::error::Error for BinError {}

/// A misconfigured toolchain reads as "the extractor isn't there", which is
/// what every caller's `NotFound` arm already says — so callers whose error
/// type is (or wraps) [`std::io::Error`] can map with a plain `.into()`.
impl From<BinError> for std::io::Error {
    fn from(e: BinError) -> Self {
        std::io::Error::new(std::io::ErrorKind::NotFound, e.to_string())
    }
}

/// On Unix, verify `path` names a regular file this process can actually
/// READ, with at least one EXECUTE permission bit set — the two properties
/// [`BinError`]'s "not a readable file" message (and this module's "readable
/// file" precedence-table language) promise but `is_file` alone never
/// checked, so a `TIDEPOOL_EXTRACT=/some/chmod-000-file` used to resolve
/// successfully here and only fail later, as an opaque OS error, at spawn.
///
/// `File::open` is the real read-access check (it honors the same
/// permission/ACL evaluation a later spawn's read of the binary would hit);
/// the execute-bit check on `mode()` is the accessible without-`libc`
/// approximation of "executable" this std-only crate can perform (see the
/// crate doc's D-A note) — the same thing a later spawn ultimately depends
/// on to succeed.
#[cfg(unix)]
fn is_readable_executable_file(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    if !path.is_file() || std::fs::File::open(path).is_err() {
        return false;
    }
    std::fs::metadata(path)
        .map(|meta| meta.permissions().mode() & 0o111 != 0)
        .unwrap_or(false)
}

/// Off Unix, there is no portable, dependency-free access check this
/// std-only crate can perform (see the crate doc's D-A note); `is_file` is
/// what this precedence step has always checked here, and a genuinely
/// unusable binary still fails loudly at spawn time.
#[cfg(not(unix))]
fn is_readable_executable_file(path: &Path) -> bool {
    path.is_file()
}

/// Resolve the `tidepool-extract` binary, STRICTLY.
///
/// `$TIDEPOOL_EXTRACT` (the same override every test tier honors) wins over
/// `PATH` — a repo with a freshly built extract must never be trumped by a
/// stale installed one. A SET-but-unreadable `$TIDEPOOL_EXTRACT` is a hard
/// error, not a silent fall-through to `PATH`: falling through would run a
/// DIFFERENT binary than the caller believes it is running (in
/// `tidepool-macro`'s case, a different one than `extract_identity()` hashed
/// into its content key — a producer/key divergence). An UNSET env falls back
/// to the bare [`DEFAULT_BIN`] name.
pub fn resolve_bin() -> Result<ResolvedBin, BinError> {
    match std::env::var_os("TIDEPOOL_EXTRACT") {
        Some(v) => {
            let path = PathBuf::from(v);
            if !is_readable_executable_file(&path) {
                return Err(BinError { path });
            }
            Ok(ResolvedBin {
                path,
                source: BinSource::Env,
            })
        }
        None => Ok(ResolvedBin {
            path: PathBuf::from(DEFAULT_BIN),
            source: BinSource::PathLookup,
        }),
    }
}

// ---------------------------------------------------------------------------
// Launcher
// ---------------------------------------------------------------------------

/// How the built argv is actually launched.
///
/// Almost every site runs the resolved binary directly. `tidepool-macro`'s
/// `run_tidepool_extract` additionally has a `nix run <flake>#tidepool-extract
/// --` fallback for the case where the bare name isn't on `PATH` — a property
/// of THAT call site, not of the arguments. Expressing it as a second
/// launcher over the SAME [`ExtractCmd`] means the argument construction is
/// shared even though the fallback is not.
#[derive(Clone, Debug)]
pub enum Launcher {
    /// Spawn the extract binary directly.
    Direct(OsString),
    /// Spawn `program`, passing `prefix` before the extract argv.
    Wrapped {
        program: OsString,
        prefix: Vec<OsString>,
    },
}

impl Launcher {
    /// `nix run <flake_root>#tidepool-extract -- <argv>`.
    pub fn nix_run(flake_root: &Path) -> Self {
        Launcher::Wrapped {
            program: OsString::from("nix"),
            prefix: vec![
                OsString::from("run"),
                OsString::from(format!("{}#{DEFAULT_BIN}", flake_root.display())),
                OsString::from("--"),
            ],
        }
    }

    /// The program this launcher spawns (`nix`, or the extract binary itself).
    pub fn program(&self) -> &OsStr {
        match self {
            Launcher::Direct(bin) => bin,
            Launcher::Wrapped { program, .. } => program,
        }
    }

    fn command(&self) -> Command {
        match self {
            Launcher::Direct(bin) => Command::new(bin),
            Launcher::Wrapped { program, prefix } => {
                let mut cmd = Command::new(program);
                cmd.args(prefix);
                cmd
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Non-zero-exit classification policy
// ---------------------------------------------------------------------------

/// What a NON-ZERO exit MEANS at a given call site.
///
/// This is a policy, not a parse: this crate never reads the diagnostics
/// report (that needs `serde_json` — see the module doc). It records which
/// reading the caller is entitled to make, so the one site whose reading
/// differs says so by name instead of by comment.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ExitPolicy {
    /// The default, and what six of the seven sites want: a non-zero exit MAY
    /// be the user's Haskell, so the caller parses the stdout diagnostics
    /// report — a parseable report is a real GHC diagnostic, an unparseable
    /// one is a stale/skewed extractor.
    #[default]
    DiagnosticReport,
    /// `tidepool_runtime::session::turn::classify_block`'s deliberate
    /// exception. THIS LANE HAS NO USER-ERROR MODE, so a non-zero exit is
    /// always an infrastructure problem and never the user's Haskell.
    /// `classifyTurn`'s rule 6 turns an item that parses as neither a
    /// declaration nor a statement into an `expr` verdict — the classify
    /// itself cannot reject input. What a non-zero exit really means is a
    /// stale extract: one predating `--classify` swallows the flag as a
    /// positional file and falls through to the ordinary compile path,
    /// which then reports a perfectly parseable GHC diagnostic about a
    /// target it cannot find. Classifying that as a user-Haskell failure
    /// would route a version skew into the caller's user-Haskell lane, where
    /// the repl degrades resiliently and the operator sees `parse error on
    /// input '<-'` on every bind instead of "your extract is stale".
    ///
    /// So a caller under this policy reports BOTH shapes as version skew,
    /// the same fails-loud reading every other call site gives an unparseable
    /// report. This is what makes the one-format wire policy true there:
    /// `--emit-stmt-binders`' removal means a new runtime REQUIRES a
    /// matching extract, and `scripts/redeploy.sh` ships both together.
    InfrastructureOnly,
}

/// The classification of one finished spawn — [`ExitPolicy`] applied to the
/// process's actual exit status.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ExitVerdict {
    /// Exited 0.
    Success,
    /// Non-zero exit under [`ExitPolicy::DiagnosticReport`]: parse the stdout
    /// report — a parseable one is the user's Haskell, an unparseable one is
    /// version skew.
    UserOrSkew,
    /// Non-zero exit under [`ExitPolicy::InfrastructureOnly`]: infrastructure
    /// (a stale extract), never the user's Haskell, whatever the report says.
    Infrastructure,
}

// ---------------------------------------------------------------------------
// Spawn result / error
// ---------------------------------------------------------------------------

/// The spawn itself failed — the process never ran, so it never paid a real
/// `tidepool-extract` cost and is NOT counted by [`extract_spawn_count`].
#[derive(Debug)]
pub struct SpawnError {
    /// The program that failed to launch (the extract binary, or `nix` for a
    /// wrapped [`Launcher`]).
    pub bin: OsString,
    pub source: std::io::Error,
}

impl SpawnError {
    /// The binary genuinely wasn't there. `tidepool-macro`'s nix fallback
    /// keys on this (and only when the bin came from [`BinSource::PathLookup`]).
    pub fn is_not_found(&self) -> bool {
        self.source.kind() == std::io::ErrorKind::NotFound
    }
}

impl std::fmt::Display for SpawnError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "failed to spawn {}: {}",
            Path::new(&self.bin).display(),
            self.source
        )
    }
}

impl std::error::Error for SpawnError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(&self.source)
    }
}

/// One finished `tidepool-extract` process.
#[derive(Debug)]
pub struct ExtractRun {
    /// The raw process output. Parsing it (diagnostics JSON, CBOR payloads)
    /// is the caller's job — see the module doc.
    pub output: Output,
    /// [`ExitPolicy`] applied to `output.status`.
    pub verdict: ExitVerdict,
    /// Wall time from just before the spawn to the process's exit. Callers
    /// forward this to their own timing collector (`timing::record_stage` /
    /// `record_turn_stage`), so the "extract_spawn" stage is measured
    /// identically everywhere.
    pub elapsed: Duration,
}

impl ExtractRun {
    /// Exited 0.
    pub fn success(&self) -> bool {
        self.output.status.success()
    }

    /// `stderr` as text — the human diagnostic channel, and where the
    /// `tidepool-timing phase=… ms=…` lines callers forward come from.
    pub fn stderr_lossy(&self) -> std::borrow::Cow<'_, str> {
        String::from_utf8_lossy(&self.output.stderr)
    }

    /// `stdout` as text — the authoritative contract channel.
    pub fn stdout_lossy(&self) -> std::borrow::Cow<'_, str> {
        String::from_utf8_lossy(&self.output.stdout)
    }
}

// ---------------------------------------------------------------------------
// The builder
// ---------------------------------------------------------------------------

/// A `tidepool-extract` invocation: positional inputs, typed flags, a
/// [`Launcher`], and an [`ExitPolicy`].
///
/// Flags are emitted in the order they are set, after all positional inputs.
/// The extractor's own `parseArgs` (`haskell/app/Main.hs`) folds each flag
/// independently and accumulates positional files, `--include` dirs,
/// `--inject-val` modules, `--bind-name`s and `--turn-template`s in
/// occurrence order — so relative order WITHIN each of those lists is
/// preserved here and everything else is order-independent.
///
/// Reusable: [`run`](ExtractCmd::run) borrows, so the same built argv can be
/// launched twice (that is exactly what `tidepool-macro`'s nix fallback does).
#[derive(Clone, Debug)]
pub struct ExtractCmd {
    launcher: Launcher,
    bin_source: BinSource,
    inputs: Vec<OsString>,
    flags: Vec<OsString>,
    policy: ExitPolicy,
}

impl ExtractCmd {
    /// Resolve the binary per [`resolve_bin`] and start building.
    pub fn new() -> Result<Self, BinError> {
        let resolved = resolve_bin()?;
        Ok(ExtractCmd {
            launcher: Launcher::Direct(resolved.path.into_os_string()),
            bin_source: resolved.source,
            inputs: Vec::new(),
            flags: Vec::new(),
            policy: ExitPolicy::default(),
        })
    }

    /// Start building against an already-resolved binary. For a caller that
    /// resolves once at construction and reuses the result across many
    /// invocations (`tidepool_harness::compile`, which is handed the binary
    /// path rather than re-reading the env per turn).
    ///
    /// Takes [`ResolvedExtractBin`] rather than an arbitrary string, so the
    /// only way to reach this constructor is through a real [`resolve_bin`]
    /// call or the named escape hatch — never a guessed fallback name.
    pub fn with_bin(bin: ResolvedExtractBin) -> Self {
        ExtractCmd {
            launcher: Launcher::Direct(bin.into_os_string()),
            bin_source: BinSource::Explicit,
            inputs: Vec::new(),
            flags: Vec::new(),
            policy: ExitPolicy::default(),
        }
    }

    /// Where this command's binary name came from.
    pub fn bin_source(&self) -> BinSource {
        self.bin_source
    }

    /// The launcher this command spawns by default.
    pub fn launcher(&self) -> &Launcher {
        &self.launcher
    }

    fn flag(&mut self, name: &str, value: impl AsRef<OsStr>) -> &mut Self {
        self.flags.push(OsString::from(name));
        self.flags.push(value.as_ref().to_os_string());
        self
    }

    fn bare_flag(&mut self, name: &str) -> &mut Self {
        self.flags.push(OsString::from(name));
        self
    }

    /// A positional input file. Repeatable; order is preserved (the classify
    /// mode's verdict list is positional).
    pub fn input(&mut self, path: impl AsRef<OsStr>) -> &mut Self {
        self.inputs.push(path.as_ref().to_os_string());
        self
    }

    /// `--output-dir <dir>`.
    pub fn output_dir(&mut self, dir: impl AsRef<OsStr>) -> &mut Self {
        self.flag("--output-dir", dir)
    }

    /// `--target <name>` — compile one named top-level binder.
    pub fn target(&mut self, name: impl AsRef<OsStr>) -> &mut Self {
        self.flag("--target", name)
    }

    /// `--targets a,b,c` — compile N named binders in ONE spawn against a
    /// shared merged `meta.cbor`.
    pub fn targets<I, S>(&mut self, names: I) -> &mut Self
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        let joined = names
            .into_iter()
            .map(|s| s.as_ref().to_string())
            .collect::<Vec<_>>()
            .join(",");
        self.flag("--targets", joined)
    }

    /// `--include <dir>`. Repeatable; order preserved.
    pub fn include(&mut self, dir: impl AsRef<OsStr>) -> &mut Self {
        self.flag("--include", dir)
    }

    /// `--include <dir>` for each entry, in order.
    pub fn includes<I, P>(&mut self, dirs: I) -> &mut Self
    where
        I: IntoIterator<Item = P>,
        P: AsRef<OsStr>,
    {
        for dir in dirs {
            self.include(dir);
        }
        self
    }

    /// `--turn` — the session-turn mode (extract classifies, picks its own
    /// wrapper template, compiles, and writes the `TurnOut` sidecar).
    pub fn turn(&mut self) -> &mut Self {
        self.bare_flag("--turn")
    }

    /// `--turn-batch <plan.json>` — one spawn compiles N items in execution
    /// order (`plans/post-restart/batch-turns-feasibility.md` §8's wire
    /// contract), each writing its own `<batch-out>/i<k>/` directory
    /// containing exactly today's single-turn output set.
    pub fn turn_batch(&mut self, plan_path: impl AsRef<OsStr>) -> &mut Self {
        self.flag("--turn-batch", plan_path)
    }

    /// `--batch-out <dir>` — the parent directory `--turn-batch` writes its
    /// per-item `i<k>/` output directories into.
    pub fn batch_out(&mut self, dir: impl AsRef<OsStr>) -> &mut Self {
        self.flag("--batch-out", dir)
    }

    /// `--turn-template <kind>=<path>`. Repeatable; order preserved.
    pub fn turn_template(&mut self, kind: &str, path: &Path) -> &mut Self {
        self.flag("--turn-template", format!("{kind}={}", path.display()))
    }

    /// `--turn-out <path>` — where the `TurnOut` CBOR sidecar is written.
    pub fn turn_out(&mut self, path: impl AsRef<OsStr>) -> &mut Self {
        self.flag("--turn-out", path)
    }

    /// `--turn-verdict <kind[:binders]>` — a caller-supplied verdict, which
    /// skips the extract's internal re-parse.
    pub fn turn_verdict(&mut self, verdict: impl AsRef<OsStr>) -> &mut Self {
        self.flag("--turn-verdict", verdict)
    }

    /// `--classify` — the parse-only batch classification mode.
    pub fn classify(&mut self) -> &mut Self {
        self.bare_flag("--classify")
    }

    /// `--classify-out <path>` — where the verdict JSON is written.
    pub fn classify_out(&mut self, path: impl AsRef<OsStr>) -> &mut Self {
        self.flag("--classify-out", path)
    }

    /// `--session-root <dir>` — where `Tidepool.Session.Val.G<g>` ifaces are
    /// written and where `--inject-val` ifaces are looked up.
    pub fn session_root(&mut self, dir: impl AsRef<OsStr>) -> &mut Self {
        self.flag("--session-root", dir)
    }

    /// `--inject-val <module>`. Repeatable; order preserved.
    pub fn inject_val(&mut self, module: impl AsRef<OsStr>) -> &mut Self {
        self.flag("--inject-val", module)
    }

    /// `--inject-val <module>` for each entry, in order.
    pub fn inject_vals<I, S>(&mut self, modules: I) -> &mut Self
    where
        I: IntoIterator<Item = S>,
        S: AsRef<OsStr>,
    {
        for m in modules {
            self.inject_val(m);
        }
        self
    }

    /// `--session-bind` — this turn binds a name, so write the thin session
    /// iface and emit the bound-binder sidecar.
    pub fn session_bind(&mut self) -> &mut Self {
        self.bare_flag("--session-bind")
    }

    /// `--bind-name <name>`. Repeatable; order preserved.
    pub fn bind_name(&mut self, name: impl AsRef<OsStr>) -> &mut Self {
        self.flag("--bind-name", name)
    }

    /// `--bind-gen <n>` — the session generation this turn binds into.
    pub fn bind_gen(&mut self, gen: u64) -> &mut Self {
        self.flag("--bind-gen", gen.to_string())
    }

    /// `--emit-bound-binders <path>` — where the bound-binder JSON sidecar is
    /// written.
    pub fn emit_bound_binders(&mut self, path: impl AsRef<OsStr>) -> &mut Self {
        self.flag("--emit-bound-binders", path)
    }

    /// Set what a non-zero exit means here. Defaults to
    /// [`ExitPolicy::DiagnosticReport`].
    pub fn exit_policy(&mut self, policy: ExitPolicy) -> &mut Self {
        self.policy = policy;
        self
    }

    /// The full argv (positional inputs first, then flags in the order they
    /// were set), without the program. Exposed for tests and diagnostics.
    pub fn argv(&self) -> Vec<OsString> {
        let mut argv = self.inputs.clone();
        argv.extend(self.flags.iter().cloned());
        argv
    }

    /// Spawn, wait, and classify — through this command's own [`Launcher`].
    pub fn run(&self) -> Result<ExtractRun, SpawnError> {
        self.run_with(&self.launcher)
    }

    /// As [`run`](ExtractCmd::run), but through a caller-supplied
    /// [`Launcher`] over the same argv — `tidepool-macro`'s nix fallback.
    pub fn run_with(&self, launcher: &Launcher) -> Result<ExtractRun, SpawnError> {
        let mut cmd = launcher.command();
        cmd.args(&self.inputs);
        cmd.args(&self.flags);

        let start = Instant::now();
        let output = cmd.output().map_err(|source| SpawnError {
            bin: launcher.program().to_os_string(),
            source,
        })?;
        let elapsed = start.elapsed();
        // Counted on a successful spawn (the process actually launched and ran to
        // exit) — a `SpawnError` above (bad path, `Command::output` I/O failure)
        // never paid a real `tidepool-extract` cost and must not count as one.
        EXTRACT_SPAWNS.fetch_add(1, Ordering::Relaxed);

        let verdict = if output.status.success() {
            ExitVerdict::Success
        } else {
            match self.policy {
                ExitPolicy::DiagnosticReport => ExitVerdict::UserOrSkew,
                ExitPolicy::InfrastructureOnly => ExitVerdict::Infrastructure,
            }
        };
        Ok(ExtractRun {
            output,
            verdict,
            elapsed,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn strs(argv: &[OsString]) -> Vec<String> {
        argv.iter()
            .map(|s| s.to_string_lossy().into_owned())
            .collect()
    }

    #[test]
    fn argv_puts_inputs_first_then_flags_in_order() {
        let mut cmd = ExtractCmd::with_bin(ResolvedExtractBin::assume_resolved("tidepool-extract"));
        cmd.input("/tmp/Expr.hs")
            .output_dir("/tmp/out")
            .targets(["a", "b"])
            .includes(["/inc/one", "/inc/two"]);
        assert_eq!(
            strs(&cmd.argv()),
            vec![
                "/tmp/Expr.hs",
                "--output-dir",
                "/tmp/out",
                "--targets",
                "a,b",
                "--include",
                "/inc/one",
                "--include",
                "/inc/two",
            ]
        );
    }

    /// The classify mode is positional-order-sensitive: verdict N belongs to
    /// input N.
    #[test]
    fn classify_mode_preserves_input_order() {
        let mut cmd = ExtractCmd::with_bin(ResolvedExtractBin::assume_resolved("x"));
        cmd.input("item-0.hs")
            .input("item-1.hs")
            .classify()
            .classify_out("/tmp/classify.json");
        assert_eq!(
            strs(&cmd.argv()),
            vec![
                "item-0.hs",
                "item-1.hs",
                "--classify",
                "--classify-out",
                "/tmp/classify.json",
            ]
        );
    }

    #[test]
    fn turn_mode_spells_every_flag() {
        let mut cmd = ExtractCmd::with_bin(ResolvedExtractBin::assume_resolved("x"));
        cmd.input("turn.txt")
            .turn()
            .turn_template("expr", Path::new("/tmp/t.hs"))
            .turn_out("/tmp/turn.cbor")
            .session_root("/tmp/session")
            .inject_vals(["Tidepool.Session.Val.G1"])
            .bind_gen(2)
            .turn_verdict("bind:x,y");
        assert_eq!(
            strs(&cmd.argv()),
            vec![
                "turn.txt",
                "--turn",
                "--turn-template",
                "expr=/tmp/t.hs",
                "--turn-out",
                "/tmp/turn.cbor",
                "--session-root",
                "/tmp/session",
                "--inject-val",
                "Tidepool.Session.Val.G1",
                "--bind-gen",
                "2",
                "--turn-verdict",
                "bind:x,y",
            ]
        );
    }

    #[test]
    fn turn_batch_mode_spells_every_flag() {
        let mut cmd = ExtractCmd::with_bin(ResolvedExtractBin::assume_resolved("x"));
        cmd.turn_batch("/tmp/plan.json")
            .batch_out("/tmp/batch-out")
            .includes(["/inc/one"]);
        assert_eq!(
            strs(&cmd.argv()),
            vec![
                "--turn-batch",
                "/tmp/plan.json",
                "--batch-out",
                "/tmp/batch-out",
                "--include",
                "/inc/one",
            ]
        );
    }

    #[test]
    fn session_bind_mode_spells_every_flag() {
        let mut cmd = ExtractCmd::with_bin(ResolvedExtractBin::assume_resolved("x"));
        cmd.session_bind()
            .bind_gen(3)
            .emit_bound_binders("/tmp/bb.json")
            .bind_name("x")
            .bind_name("y");
        assert_eq!(
            strs(&cmd.argv()),
            vec![
                "--session-bind",
                "--bind-gen",
                "3",
                "--emit-bound-binders",
                "/tmp/bb.json",
                "--bind-name",
                "x",
                "--bind-name",
                "y",
            ]
        );
    }

    #[test]
    fn nix_run_launcher_prefixes_the_same_argv() {
        let l = Launcher::nix_run(Path::new("/repo"));
        match l {
            Launcher::Wrapped { program, prefix } => {
                assert_eq!(program, OsString::from("nix"));
                assert_eq!(strs(&prefix), vec!["run", "/repo#tidepool-extract", "--"]);
            }
            Launcher::Direct(_) => panic!("nix_run must be a wrapped launcher"),
        }
    }

    /// One test owns `$TIDEPOOL_EXTRACT` for this binary (all cases in
    /// sequence) so no two tests race on the same process-global env var.
    #[test]
    fn bin_resolution_is_strict() {
        use std::os::unix::fs::PermissionsExt;
        let dir = std::env::temp_dir().join(format!(
            "tidepool-extract-cmd-resolve-{}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).unwrap();

        // Set-but-unreadable is a hard error, NEVER a fall-through to PATH.
        let missing = dir.join("nope");
        let _ = std::fs::remove_file(&missing);
        std::env::set_var("TIDEPOOL_EXTRACT", &missing);
        let err = resolve_bin().unwrap_err();
        assert_eq!(err.path(), missing.as_path());
        assert!(
            err.to_string().contains("not a readable file"),
            "unexpected message: {err}"
        );
        // ...and it reads as NotFound to an io-typed caller.
        let io: std::io::Error = err.into();
        assert_eq!(io.kind(), std::io::ErrorKind::NotFound);

        // A file that EXISTS but has no permission bits at all (`chmod 000`)
        // must be rejected AT RESOLUTION, with the same "not a readable
        // file" error — not accepted here and left to fail later, as an
        // opaque OS error, at spawn.
        let unreadable = dir.join("chmod-000-extract");
        std::fs::write(&unreadable, b"#!/bin/sh\nexit 0\n").unwrap();
        std::fs::set_permissions(&unreadable, std::fs::Permissions::from_mode(0o000)).unwrap();
        std::env::set_var("TIDEPOOL_EXTRACT", &unreadable);
        let err = resolve_bin().unwrap_err();
        assert_eq!(err.path(), unreadable.as_path());
        assert!(
            err.to_string().contains("not a readable file"),
            "unexpected message: {err}"
        );
        std::fs::set_permissions(&unreadable, std::fs::Permissions::from_mode(0o755)).unwrap();
        std::fs::remove_file(&unreadable).ok();

        // A readable file with no EXECUTE bit set is rejected the same way —
        // `is_file` alone would have accepted it.
        let not_executable = dir.join("not-executable-extract");
        std::fs::write(&not_executable, b"#!/bin/sh\nexit 0\n").unwrap();
        std::fs::set_permissions(&not_executable, std::fs::Permissions::from_mode(0o644)).unwrap();
        std::env::set_var("TIDEPOOL_EXTRACT", &not_executable);
        let err = resolve_bin().unwrap_err();
        assert_eq!(err.path(), not_executable.as_path());

        // Set-and-readable-and-executable wins over PATH.
        let real = dir.join("tidepool-extract");
        std::fs::write(&real, b"#!/bin/sh\nexit 0\n").unwrap();
        std::fs::set_permissions(&real, std::fs::Permissions::from_mode(0o755)).unwrap();
        std::env::set_var("TIDEPOOL_EXTRACT", &real);
        let resolved = resolve_bin().unwrap();
        assert_eq!(resolved.path, real);
        assert_eq!(resolved.source, BinSource::Env);

        // Unset falls back to the bare name.
        std::env::remove_var("TIDEPOOL_EXTRACT");
        let resolved = resolve_bin().unwrap();
        assert_eq!(resolved.path, PathBuf::from(DEFAULT_BIN));
        assert_eq!(resolved.source, BinSource::PathLookup);

        std::fs::remove_dir_all(&dir).ok();
    }

    /// The counter increments on a spawn that RAN, whatever its exit status,
    /// and not on a spawn that never launched. Also pins the two exit
    /// policies against the same non-zero exit.
    #[test]
    fn counter_and_verdicts() {
        use std::os::unix::fs::PermissionsExt;
        let dir = std::env::temp_dir().join(format!(
            "tidepool-extract-cmd-counter-{}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let fake = dir.join("fake-extract");
        std::fs::write(&fake, b"#!/bin/sh\nexit 3\n").unwrap();
        std::fs::set_permissions(&fake, std::fs::Permissions::from_mode(0o755)).unwrap();

        reset_extract_spawn_count();

        // Never launched: not counted.
        let mut missing = ExtractCmd::with_bin(ResolvedExtractBin::assume_resolved(
            dir.join("does-not-exist"),
        ));
        let err = missing.input("x.hs").run().unwrap_err();
        assert!(err.is_not_found(), "expected NotFound, got {err}");
        assert_eq!(extract_spawn_count(), 0);

        // Ran and failed: counted, and classified by policy.
        let mut cmd = ExtractCmd::with_bin(ResolvedExtractBin::assume_resolved(&fake));
        let run = cmd.input("x.hs").run().unwrap();
        assert!(!run.success());
        assert_eq!(run.verdict, ExitVerdict::UserOrSkew);
        assert_eq!(extract_spawn_count(), 1);

        cmd.exit_policy(ExitPolicy::InfrastructureOnly);
        let run = cmd.run().unwrap();
        assert_eq!(run.verdict, ExitVerdict::Infrastructure);
        assert_eq!(extract_spawn_count(), 2);

        std::fs::remove_dir_all(&dir).ok();
    }
}
