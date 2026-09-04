//! Public process boundary for the Haskell compiler worker.
//!
//! What lives here:
//!
//! - [`resolve_bin`] — binary resolution, with `tidepool-macro`'s STRICT
//!   policy as the default for everyone: a SET-but-unreadable
//!   `$TIDEPOOL_EXTRACT` is a hard error, never a silent fall-through to
//!   `PATH`; an UNSET one falls back to the bare `tidepool-extract` name.
//! - [`ExtractCmd`] — construction and encoding of typed compiler requests.
//! - [`CompilerEndpoint`] — an opaque identity bound to the exact producer
//!   that will execute one request.
//! - CLI/daemon infrastructure and the process-global [`extract_spawn_count`].
//!
//! This is a small process-boundary leaf because proc-macro crates depend on
//! it. Its only non-std mechanism is BLAKE3, used to report strong immutable
//! producer identity. Parsing JSON diagnostics and CBOR output belongs to
//! callers; the content-addressed compile cache belongs to `tidepool-toolchain`.

#![warn(clippy::unwrap_used, clippy::expect_used)]
use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};
use std::process::Output;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

mod daemon;
mod endpoint;
pub mod exec_check;
pub mod frontend;
mod process;
mod request;
pub use endpoint::{CompilerEndpoint, CompilerIdentity};
use exec_check::is_readable_executable_file;
pub use request::{ExtractRequest, ProtocolError};

/// Verify that `socket` is served by a compatible resident compiler daemon.
///
/// This performs the daemon protocol preflight rather than merely checking
/// that a Unix socket path exists. Composition roots use it to gate dependent
/// process startup without reproducing the compiler endpoint wire protocol.
pub fn preflight_compiler_daemon(socket: &Path) -> std::io::Result<()> {
    daemon::preflight(socket).map(|_| ()).map_err(|error| {
        std::io::Error::new(
            std::io::ErrorKind::ConnectionRefused,
            format!("compiler daemon preflight failed: {error}"),
        )
    })
}

/// The bare binary name, used when `$TIDEPOOL_EXTRACT` is unset (resolved
/// through `PATH` by the OS at spawn time).
pub const DEFAULT_BIN: &str = "tidepool-extract";

/// Environment variable selecting a resident compiler daemon socket.
pub const DAEMON_SOCKET_ENV: &str = "TIDEPOOL_EXTRACT_DAEMON_SOCKET";

/// Process-global count of compiler invocations submitted through a bound
/// endpoint. Tests asserting on it require process isolation.
static EXTRACT_SPAWNS: AtomicU64 = AtomicU64::new(0);

/// Number of compiler invocations this process has submitted so far.
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

/// Run the extractor's no-input role probe through the process-boundary owner.
///
/// A probe performs no compilation and therefore does not increment
/// [`extract_spawn_count`]. Callers interpret the returned banner according to
/// their own frontend/worker compatibility policy.
pub fn probe_binary(path: &Path) -> std::io::Result<Output> {
    std::process::Command::new(path)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .output()
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
    /// caller may bind the explicit Nix fallback when direct endpoint binding
    /// reports not-found.
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

/// Renders as the selected frontend path for diagnostics.
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

/// Resolve the `tidepool-extract` binary, STRICTLY.
///
/// `$TIDEPOOL_EXTRACT` (the same override every test tier honors) wins over
/// `PATH` — a repo with a freshly built extract must never be trumped by a
/// stale installed one. A SET-but-unreadable `$TIDEPOOL_EXTRACT` is a hard
/// error, not a silent fall-through to `PATH`: falling through would run a
/// DIFFERENT binary than the caller believes it is running (in
/// `tidepool-macro`'s case, a different endpoint than the content key names).
/// An UNSET env falls back to the bare [`DEFAULT_BIN`] name.
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
// Spawn result / error
// ---------------------------------------------------------------------------

/// Binding or executing a compiler endpoint failed.
#[derive(Debug)]
pub struct SpawnError {
    /// The frontend, Nix launcher, or daemon socket involved.
    pub bin: OsString,
    pub source: std::io::Error,
    settlement: Settlement,
}

#[derive(Debug)]
enum Settlement {
    NotSubmitted,
    Submitted,
}

impl SpawnError {
    /// The binary genuinely wasn't there. `tidepool-macro`'s nix fallback
    /// keys on this (and only when the bin came from [`BinSource::PathLookup`]).
    pub fn is_not_found(&self) -> bool {
        self.source.kind() == std::io::ErrorKind::NotFound
    }

    /// True only when the endpoint proved the request was not accepted.
    pub fn permits_rebind(&self) -> bool {
        matches!(self.settlement, Settlement::NotSubmitted)
    }

    fn not_submitted(bin: impl AsRef<OsStr>, source: std::io::Error) -> Self {
        Self {
            bin: bin.as_ref().to_owned(),
            source,
            settlement: Settlement::NotSubmitted,
        }
    }

    fn indeterminate(bin: impl AsRef<OsStr>, source: std::io::Error) -> Self {
        Self {
            bin: bin.as_ref().to_owned(),
            source,
            settlement: Settlement::Submitted,
        }
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
}

// ---------------------------------------------------------------------------
// The builder
// ---------------------------------------------------------------------------

/// A typed compiler-worker request and its frontend selection.
///
/// Request fields retain their types through [`ExtractRequest::encode`]. A
/// CLI argv rendering remains available for cache key classification and
/// diagnostics; normal execution binds a [`CompilerEndpoint`] first and the
/// Haskell worker consumes only the versioned request payload.
#[derive(Clone, Debug)]
pub struct ExtractCmd {
    program: OsString,
    bin_source: BinSource,
    request: ExtractRequest,
}

impl ExtractCmd {
    /// Resolve the binary per [`resolve_bin`] and start building.
    pub fn new() -> Result<Self, BinError> {
        let resolved = resolve_bin()?;
        Ok(ExtractCmd {
            program: resolved.path.into_os_string(),
            bin_source: resolved.source,
            request: ExtractRequest::default(),
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
            program: bin.into_os_string(),
            bin_source: BinSource::Explicit,
            request: ExtractRequest::default(),
        }
    }

    /// Where this command's binary name came from.
    pub fn bin_source(&self) -> BinSource {
        self.bin_source
    }

    /// Resolve and bind the exact producer that will execute this request.
    /// Daemon discovery, direct frontend loading, worker selection, and GHC
    /// libdir capture all happen here, before callers derive cache or build
    /// product identity.
    pub fn bind(&self) -> Result<CompilerEndpoint, SpawnError> {
        CompilerEndpoint::bind(self)
    }

    /// Bind the macro's explicit `nix run <flake>#tidepool-extract` fallback.
    /// The returned endpoint reports the producer loaded by Nix itself.
    pub fn bind_nix_fallback(&self, flake_root: &Path) -> Result<CompilerEndpoint, SpawnError> {
        CompilerEndpoint::bind_nix(flake_root)
    }

    /// A positional input file. Repeatable; order is preserved (the classify
    /// mode's verdict list is positional).
    pub fn input(&mut self, path: impl AsRef<OsStr>) -> &mut Self {
        self.request.input(path);
        self
    }

    /// `--output-dir <dir>`.
    pub fn output_dir(&mut self, dir: impl AsRef<OsStr>) -> &mut Self {
        self.request.output_dir(dir);
        self
    }

    /// `--target <name>` — compile one named top-level binder.
    pub fn target(&mut self, name: impl AsRef<OsStr>) -> &mut Self {
        self.request.target(name);
        self
    }

    /// `--targets a,b,c` — compile N named binders in ONE spawn against a
    /// shared merged `meta.cbor`.
    pub fn targets<I, S>(&mut self, names: I) -> &mut Self
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        self.request.targets(names);
        self
    }

    /// `--include <dir>`. Repeatable; order preserved.
    pub fn include(&mut self, dir: impl AsRef<OsStr>) -> &mut Self {
        self.request.include(dir);
        self
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
        self.request.turn();
        self
    }

    /// `--turn-template <kind>=<path>`. Repeatable; order preserved.
    pub fn turn_template(&mut self, kind: &str, path: &Path) -> &mut Self {
        self.request.turn_template(kind, path);
        self
    }

    /// `--turn-out <path>` — where the `TurnOut` CBOR sidecar is written.
    pub fn turn_out(&mut self, path: impl AsRef<OsStr>) -> &mut Self {
        self.request.turn_out(path);
        self
    }

    /// `--turn-verdict <kind[:binders]>` — a caller-supplied verdict, which
    /// skips the extract's internal re-parse.
    pub fn turn_verdict(&mut self, verdict: impl AsRef<OsStr>) -> &mut Self {
        self.request.turn_verdict(verdict);
        self
    }

    /// `--classify` — the parse-only batch classification mode.
    pub fn classify(&mut self) -> &mut Self {
        self.request.classify();
        self
    }

    /// `--classify-out <path>` — where the verdict JSON is written.
    pub fn classify_out(&mut self, path: impl AsRef<OsStr>) -> &mut Self {
        self.request.classify_out(path);
        self
    }

    /// `--build-products-dir <dir>` — a persistent, shared `-fwrite-interface`
    /// output dir the extract points `hiDir`/`objectDir` at, so a LATER spawn's
    /// `load'` can skip an unchanged home module via GHC's own `checkOldIface`
    /// (spike-verified: `plans/turn-latency-state-injection.md`). Dropped from
    /// the compile-memo key (`tidepool_runtime::cache::invocation_key`), same
    /// bucket as `--output-dir`: it changes nothing about the OUTPUT bytes,
    /// only whether GHC's frontend can skip work to produce them.
    pub fn build_products_dir(&mut self, dir: impl AsRef<OsStr>) -> &mut Self {
        self.request.build_products_dir(dir);
        self
    }

    /// `--session-root <dir>` — where `Tidepool.Session.Val.G<g>` ifaces are
    /// written and where `--inject-val` ifaces are looked up.
    pub fn session_root(&mut self, dir: impl AsRef<OsStr>) -> &mut Self {
        self.request.session_root(dir);
        self
    }

    /// `--inject-val <module>`. Repeatable; order preserved.
    pub fn inject_val(&mut self, module: impl AsRef<OsStr>) -> &mut Self {
        self.request.inject_val(module);
        self
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

    /// `--bind-gen <n>` — the session generation this turn binds into.
    pub fn bind_gen(&mut self, gen: u64) -> &mut Self {
        self.request.bind_gen(gen);
        self
    }

    /// Ask the compiler worker for GHC's type of an expression.
    pub fn inspect_type(&mut self, expression: &str) -> &mut Self {
        self.request.inspect_type(expression);
        self
    }

    /// Ask the compiler worker for GHC's information about an in-scope name.
    pub fn inspect_info(&mut self, name: &str) -> &mut Self {
        self.request.inspect_info(name);
        self
    }

    pub fn inspect_browse(&mut self, module: &str, expanded: bool) -> &mut Self {
        self.request.inspect_browse(module, expanded);
        self
    }

    /// Inspection-result CBOR sidecar.
    pub fn inspect_out(&mut self, path: impl AsRef<OsStr>) -> &mut Self {
        self.request.inspect_out(path);
        self
    }

    /// The full argv (positional inputs first, then flags in the order they
    /// were set), without the program. Exposed for tests and diagnostics.
    pub fn argv(&self) -> Vec<OsString> {
        self.request.cli_argv()
    }

    /// Versioned worker request bytes. Unlike [`Self::argv`], this preserves
    /// field types and cannot reinterpret an unknown option as an input file.
    pub fn request_bytes(&self) -> Vec<u8> {
        self.request.encode()
    }

    /// Exact argv consumed by the compiler worker: the version marker and
    /// encoded typed request. Most callers should bind and execute an endpoint;
    /// this is for endpoint infrastructure and tests that inspect the
    /// transport encoding.
    pub fn worker_argv(&self) -> Vec<OsString> {
        self.request.worker_argv()
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    /// Serializes tests that mutate process-global environment or counters.
    /// The crate must also pass ordinary multi-threaded `cargo test`; process
    /// isolation by a particular test runner is not part of its contract.
    static PROCESS_STATE: Mutex<()> = Mutex::new(());

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
    fn build_products_dir_emits_the_flag() {
        let mut cmd = ExtractCmd::with_bin(ResolvedExtractBin::assume_resolved("x"));
        cmd.input("/tmp/Expr.hs")
            .output_dir("/tmp/out")
            .build_products_dir("/tmp/bp")
            .targets(["a"]);
        assert_eq!(
            strs(&cmd.argv()),
            vec![
                "/tmp/Expr.hs",
                "--output-dir",
                "/tmp/out",
                "--build-products-dir",
                "/tmp/bp",
                "--targets",
                "a",
            ]
        );
    }

    #[test]
    fn inspection_request_keeps_query_and_output_typed() {
        let mut cmd = ExtractCmd::with_bin(ResolvedExtractBin::assume_resolved("x"));
        cmd.input("Expr.hs")
            .inspect_type("fmap")
            .inspect_out("inspection.cbor");
        assert_eq!(
            strs(&cmd.argv()),
            vec![
                "Expr.hs",
                "--inspect-type",
                "fmap",
                "--inspect-out",
                "inspection.cbor",
            ]
        );
        let decoded = ExtractRequest::decode(&cmd.request_bytes()).unwrap();
        assert_eq!(decoded.cli_argv(), cmd.argv());
    }

    /// One test owns `$TIDEPOOL_EXTRACT` for this binary (all cases in
    /// sequence) so no two tests race on the same process-global env var.
    #[test]
    fn bin_resolution_is_strict() {
        let _state = PROCESS_STATE.lock().unwrap();
        use std::os::unix::fs::PermissionsExt;
        let old_extract = std::env::var_os("TIDEPOOL_EXTRACT");
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

        // Endpoint binding preserves that interpretation: the bare name is
        // resolved by the actual launch through PATH, and the identity comes
        // from that launched producer rather than a separate path scan.
        std::fs::write(
            &real,
            b"#!/bin/sh\nprintf TPCID001\nhead -c 32 /dev/zero\ncat >/dev/null\n",
        )
        .unwrap();
        let old_path = std::env::var_os("PATH");
        let mut paths = vec![dir.clone()];
        if let Some(path) = &old_path {
            paths.extend(std::env::split_paths(path));
        }
        std::env::set_var("PATH", std::env::join_paths(paths).unwrap());
        let old_socket = std::env::var_os("TIDEPOOL_EXTRACT_DAEMON_SOCKET");
        std::env::remove_var("TIDEPOOL_EXTRACT_DAEMON_SOCKET");
        let endpoint = ExtractCmd::new().unwrap().bind().unwrap();
        assert_eq!(endpoint.identity().producer_bytes(), &[0; 32]);
        drop(endpoint);
        match old_path {
            Some(path) => std::env::set_var("PATH", path),
            None => std::env::remove_var("PATH"),
        }
        match old_socket {
            Some(socket) => std::env::set_var("TIDEPOOL_EXTRACT_DAEMON_SOCKET", socket),
            None => std::env::remove_var("TIDEPOOL_EXTRACT_DAEMON_SOCKET"),
        }
        match old_extract {
            Some(extract) => std::env::set_var("TIDEPOOL_EXTRACT", extract),
            None => std::env::remove_var("TIDEPOOL_EXTRACT"),
        }

        std::fs::remove_dir_all(&dir).ok();
    }

    /// The counter increments on a spawn that RAN, whatever its exit status,
    /// and not on a spawn that never launched.
    #[test]
    fn counter_tracks_spawns_that_ran() {
        let _state = PROCESS_STATE.lock().unwrap();
        use std::os::unix::fs::PermissionsExt;
        let dir = std::env::temp_dir().join(format!(
            "tidepool-extract-cmd-counter-{}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let fake = dir.join("fake-extract");
        std::fs::write(
            &fake,
            b"#!/bin/sh\nprintf TPCID001\nhead -c 32 /dev/zero\ncat >/dev/null\nprintf '\\003\\000\\000\\000\\000\\000\\000\\000\\000\\000\\000\\000'\n",
        )
        .unwrap();
        std::fs::set_permissions(&fake, std::fs::Permissions::from_mode(0o755)).unwrap();

        reset_extract_spawn_count();

        // This test is about the DIRECT-SPAWN counter: under an inherited
        // live daemon socket execution would
        // route to the daemon and return Ok(diagnostics) instead of the
        // NotFound this asserts. The shared test lock prevents another test
        // from observing the temporary removal.
        std::env::remove_var("TIDEPOOL_EXTRACT_DAEMON_SOCKET");

        // Never launched: not counted.
        let mut missing = ExtractCmd::with_bin(ResolvedExtractBin::assume_resolved(
            dir.join("does-not-exist"),
        ));
        missing.input("x.hs");
        let err = missing.bind().unwrap_err();
        assert!(err.is_not_found(), "expected NotFound, got {err}");
        assert_eq!(extract_spawn_count(), 0);

        // Ran and failed: still counted.
        let mut cmd = ExtractCmd::with_bin(ResolvedExtractBin::assume_resolved(&fake));
        cmd.input("x.hs");
        let endpoint = cmd.bind().unwrap();
        let run = endpoint.execute(&cmd).unwrap();
        assert!(!run.success());
        assert_eq!(extract_spawn_count(), 1);

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn dead_socket_falls_back_to_direct() {
        let _state = PROCESS_STATE.lock().unwrap();
        use std::os::unix::fs::PermissionsExt;
        let dir = std::env::temp_dir().join(format!(
            "tidepool-extract-cmd-daemon-fallback-{}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let fake_bin = dir.join("fake-extract");
        std::fs::write(
            &fake_bin,
            b"#!/bin/sh\nprintf TPCID001\nhead -c 32 /dev/zero\ncat >/dev/null\nprintf '\\000\\000\\000\\000\\015\\000\\000\\000fallback-ran\\012\\000\\000\\000\\000'\n",
        )
        .unwrap();
        std::fs::set_permissions(&fake_bin, std::fs::Permissions::from_mode(0o755)).unwrap();

        // Never bound by anything — connecting must fail with NotFound.
        let dead_sock = dir.join("dead.sock");
        let _ = std::fs::remove_file(&dead_sock);

        std::env::set_var("TIDEPOOL_EXTRACT_DAEMON_SOCKET", &dead_sock);
        reset_extract_spawn_count();
        let mut cmd = ExtractCmd::with_bin(ResolvedExtractBin::assume_resolved(&fake_bin));
        cmd.input("Expr.hs");
        let endpoint = cmd.bind().unwrap();
        let run = endpoint.execute(&cmd).unwrap();
        std::env::remove_var("TIDEPOOL_EXTRACT_DAEMON_SOCKET");

        assert!(run.success());
        assert_eq!(
            String::from_utf8_lossy(&run.output.stdout).trim(),
            "fallback-ran"
        );
        // Exactly one spawn counted — the Direct fallback, not a phantom
        // daemon attempt plus a real spawn.
        assert_eq!(extract_spawn_count(), 1);

        std::fs::remove_dir_all(&dir).ok();
    }
}
