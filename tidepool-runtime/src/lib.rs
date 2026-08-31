//! High-level runtime for compiling and executing Haskell source via Tidepool.
//!
//! Provides `compile_haskell` (source to Core) and `compile_and_run` (source to
//! evaluated result), with filesystem caching of compiled CBOR artifacts.
//!
//! Toolchain location, validation, fingerprinting, and the compile-output
//! cache live in `tidepool-toolchain` (a crate this one depends on and sits
//! above). `paths`, `toolchain`, `cache`, `artifacts`, `diag`, and `timing`
//! below are thin module re-exports of that crate, kept so every existing
//! `tidepool_runtime::<module>::...` call site keeps compiling unchanged;
//! `failclass` is a real local module (its `classify`/`classify_session`
//! dispatch over this crate's own `RuntimeError`/`SessionError`, so they
//! can't live below in `tidepool-toolchain`) that re-exports the rest of the
//! classifier from there.

#![warn(clippy::unwrap_used, clippy::expect_used)]
use std::path::{Path, PathBuf};
use thiserror::Error;
pub use tidepool_codegen::host_fns::{drain_diagnostics, push_diagnostic};
use tidepool_codegen::jit_machine::JitEffectMachine;
pub use tidepool_codegen::jit_machine::{CancelHandle, JitError};
pub use tidepool_codegen::suspension::ResumeInput;
use tidepool_codegen::suspension::{ContinuationId, ParkedOutcome, RealmId, SuspensionRun};
pub use tidepool_effect::dispatch::DispatchEffect;
use tidepool_effect::{EffectRunPolicy, LivePayloadPolicy};
pub use tidepool_eval::value::Value;
use tidepool_repr::serial::MetaWarnings;
use tidepool_repr::{CoreExpr, DataConTable};

pub(crate) use tidepool_toolchain::extract_spawn_error;
pub use tidepool_toolchain::CompileError;
pub use tidepool_toolchain::{artifacts, cache, diag, paths, timing, toolchain};

pub mod failclass;
/// Generated suspension-decode request types (`tidepool-protocol`'s
/// `runtime_generated_files`) — currently just `Ask`, shared by
/// [`session::engine::extract_ask_request`] and `tidepool-harness`'s
/// `RosterRequest::Ask`. `pub` (unlike `tidepool-harness`'s own crate-private
/// `generated` module) because the harness is a genuine second consumer
/// across a crate boundary, not an internal implementation detail.
pub mod generated;
mod render;
pub mod session;

pub use artifacts::{
    compile_targets, compile_targets_with_session_inject, compile_targets_with_stable_inject,
    CompiledArtifacts, NominalHead, SessionInject, SiteType, StableValInject, TargetArtifact,
    YieldSite, YieldSiteCollision, YieldSites,
};
pub use failclass::{
    classify, classify_compile, classify_session, FailureClass, FailureEnvelope, Phase,
};
pub use render::{value_to_json, EvalResult};

/// Result of successful Haskell compilation: a Core expression, DataCon metadata, and warnings.
#[derive(Debug)]
pub struct CompileResult {
    /// The compiled Core expression (the JIT/eval input).
    pub expr: CoreExpr,
    /// DataCon metadata the JIT needs to dispatch on constructors.
    pub table: DataConTable,
    /// Compile warnings (e.g. `has_io`, captured type).
    pub warnings: MetaWarnings,
}

/// Unified error type for the compile-and-run pipeline.
#[derive(Error, Debug)]
pub enum RuntimeError {
    /// Error during Haskell compilation.
    #[error(transparent)]
    Compile(#[from] CompileError),
    /// Error during JIT execution.
    #[error(transparent)]
    Jit(#[from] JitError),
}

/// Compiles Haskell source code to Tidepool Core at runtime.
///
/// This function shells out to `tidepool-extract` (which must be available on the system `$PATH`)
/// to perform GHC parsing, type-checking, and Core translation. It writes the source to a
/// temporary file, executes the extractor, and reads back the resulting CBOR and metadata.
///
/// Compiled results are cached in the XDG cache directory (typically `~/.cache/tidepool`)
/// to speed up repeated compilations. The cache key is derived from the source code,
/// the target binder, and a fingerprint of any included dependency directories.
///
/// # Arguments
/// * `source` - The Haskell source code to compile.
/// * `target` - The name of the top-level binder to use as the entry point (e.g., "main").
/// * `include` - Paths to directories containing Haskell modules to include in the search path.
///
/// # Returns
/// * `Ok((CoreExpr, DataConTable))` on success.
/// * `Err(CompileError)` if compilation fails, the extractor is missing, or output is invalid.
pub fn compile_haskell(
    source: &str,
    target: &str,
    include: &[&Path],
) -> Result<CompileResult, CompileError> {
    compile_haskell_salted(source, target, include, None)
}

/// As [`compile_haskell`], but mixes `cache_salt` into the cache key. The
/// declaration-accumulation lane ([`session::SessionLib`]) passes its
/// `(session, generation)` salt so per-session, per-generation compilations
/// never collide and a generation bump invalidates correctly. With `None` this
/// is byte-for-byte identical to [`compile_haskell`].
pub fn compile_haskell_salted(
    source: &str,
    target: &str,
    include: &[&Path],
    cache_salt: Option<&str>,
) -> Result<CompileResult, CompileError> {
    let include_owned: Vec<PathBuf> = include.iter().map(|p| p.to_path_buf()).collect();
    let inv = artifacts::CompileInvocation {
        source,
        targets: &[target],
        include: &include_owned,
        bin: None,
        fallback_module_name: "Input",
        cache: artifacts::CacheStrategy::Eval { salt: cache_salt },
        stable_val: None,
        session_inject: None,
    };
    let mut bundle = artifacts::compile_invocation(&inv, |_, _, _| {})?;
    #[allow(
        clippy::expect_used,
        reason = "compile_invocation compiled exactly this target"
    )]
    let TargetArtifact { expr, .. } = bundle
        .targets
        .remove(target)
        .expect("compile_invocation compiled exactly this target");
    let CompiledArtifacts {
        table, warnings, ..
    } = bundle;
    // `artifacts::assemble` (which `compile_invocation` always routes
    // through, cache hit or miss) already registered var names/poisoned
    // externals for this compile — see its doc.

    Ok(CompileResult {
        expr,
        table,
        warnings,
    })
}

/// Default JIT allocation nursery size (64 MiB), used by [`compile_and_run`]
/// and [`compile_and_run_pure`].
pub const DEFAULT_NURSERY_SIZE: usize = 1 << 26; // 64 MiB

/// Stack size for eval threads. The JIT's clean recursion-overflow guard needs
/// stack headroom; too small a stack lets a deep non-tail recursion blow the
/// host stack into corruption ("unexpected heap tag") before the guard fires.
/// Shared by the MCP server's eval thread and the test harness so they can't
/// drift — a smaller test stack made the overflow probes diverge from real evals.
pub const EVAL_STACK_SIZE: usize = 256 * 1024 * 1024; // 256 MiB

/// Compile Haskell source and run it with the given effect handlers,
/// using the specified nursery size.
///
/// # Arguments
/// * `source` - The Haskell source code to compile.
/// * `target` - The name of the entry point binder.
/// * `include` - Search paths for Haskell modules.
/// * `handlers` - Effect dispatchers for the JIT machine.
/// * `user` - User context for effect handlers.
/// * `nursery_size` - Size of the allocation nursery in bytes.
///
/// # Returns
/// * `Ok(EvalResult)` on successful execution.
/// * `Err(RuntimeError)` for compilation or JIT execution errors.
pub fn compile_and_run_with_nursery_size<U, H: DispatchEffect<U>>(
    source: &str,
    target: &str,
    include: &[&Path],
    handlers: &mut H,
    user: &U,
    nursery_size: usize,
) -> Result<EvalResult, RuntimeError> {
    compile_and_run_cancellable(
        source,
        target,
        include,
        handlers,
        user,
        nursery_size,
        |_| {},
    )
}

/// As [`compile_and_run_with_nursery_size`], but hands the freshly-built machine's
/// [`CancelHandle`] to `on_ready` BEFORE the (blocking) run begins.
///
/// The handle is `Send + Sync + Clone`, so a caller running this on a worker
/// thread can ship a clone to a watchdog/timeout task that flips it; the running
/// program then aborts at its next GC/tail-call safepoint with
/// `YieldError::Cancelled`, freeing the thread (and any resources it pins). This
/// is how the eval/repl servers turn a turn timeout into an actual abort instead
/// of a permanently-parked thread.
pub fn compile_and_run_cancellable<U, H: DispatchEffect<U>>(
    source: &str,
    target: &str,
    include: &[&Path],
    handlers: &mut H,
    user: &U,
    nursery_size: usize,
    on_ready: impl FnOnce(CancelHandle),
) -> Result<EvalResult, RuntimeError> {
    let CompileResult {
        expr,
        mut table,
        warnings,
    } = compile_haskell(source, target, include)?;
    if warnings.has_io {
        return Err(RuntimeError::Compile(CompileError::IOTypeDetected));
    }
    // Populate type-sibling groups from case branches so that get_companion
    // can disambiguate constructors sharing unqualified names (e.g. Bin/Tip
    // from Data.Map vs Data.Set).
    table.populate_siblings_from_expr(&expr);
    let mut machine = JitEffectMachine::compile(&expr, &table, nursery_size)?;
    on_ready(machine.cancel_handle());
    let value = machine.run(&table, handlers, user)?;
    Ok(EvalResult::new(value, table, warnings.warnings))
}

/// The outcome of driving a turn that may SUSPEND at the ask boundary
/// (threadless suspension). On [`SuspendableRun::Suspended`] the machine's heap
/// is retained (session machinery) and the whole `JitEffectMachine` — plus the
/// `DataConTable` — is handed back so the caller can stow it as data (no parked
/// thread) and resume it later, on any thread, via [`resume_suspended_turn`].
// The `Suspended` variant carries a whole `JitEffectMachine` by design (that IS
// the stowed value); this enum is constructed and destructured immediately at
// the eval-thread boundary, so the size asymmetry is inherent, not a leak.
#[allow(clippy::large_enum_variant)]
pub enum SuspendableRun {
    /// The turn ran to completion.
    Completed(EvalResult),
    /// The turn suspended at the ask boundary.
    Suspended {
        /// The stowed machine (heap retained; continuation held internally).
        machine: JitEffectMachine,
        /// The constructor table this turn compiled against (needed to extract
        /// the prompt/meta from `request` and to convert the answer on resume).
        table: DataConTable,
        /// Registry identity of the parked continuation.
        continuation: ContinuationId,
        /// The bridged `Ask` request value.
        request: tidepool_eval::value::Value,
    },
}

/// The outcome of resuming a stowed turn (see [`resume_suspended_turn`]).
// `Completed(EvalResult)` is the large variant; like `SuspendableRun` this is a
// transient boundary carrier, destructured immediately by the caller.
#[allow(clippy::large_enum_variant)]
pub enum ResumedRun {
    /// The turn ran to completion.
    Completed(EvalResult),
    /// The turn suspended again at a further ask boundary. The machine (borrowed
    /// `&mut` by the resume) holds the new continuation internally, ready for
    /// another [`resume_suspended_turn`].
    Suspended {
        continuation: ContinuationId,
        request: tidepool_eval::value::Value,
    },
}

/// Compile `source` and drive it until it completes or reaches a request that
/// the installed handlers do not recognize. Sibling of
/// [`compile_and_run_cancellable`] that hands the machine back as data at that
/// point instead of blocking a thread —
/// the substrate for threadless session suspension. The machine is compiled
/// as a SESSION machine so its heap is retained across the suspension (the drive
/// itself is byte-identical to the one-shot path for a turn that never asks).
#[allow(clippy::too_many_arguments)]
pub fn compile_and_run_suspendable<U, H: DispatchEffect<U>>(
    source: &str,
    target: &str,
    include: &[&Path],
    handlers: &mut H,
    user: &U,
    nursery_size: usize,
    on_ready: impl FnOnce(CancelHandle),
) -> Result<SuspendableRun, RuntimeError> {
    let CompileResult {
        expr,
        mut table,
        warnings,
    } = compile_haskell(source, target, include)?;
    if warnings.has_io {
        return Err(RuntimeError::Compile(CompileError::IOTypeDetected));
    }
    table.populate_siblings_from_expr(&expr);
    let mut machine = JitEffectMachine::compile_session(&expr, &table, nursery_size)?;
    let realm = RealmId::ROOT;
    on_ready(machine.realm_cancel_handle(realm));
    let run = SuspensionRun::main(&table, EffectRunPolicy::HandleOrSuspend, realm)
        .with_live_payload(LivePayloadPolicy::HASKELL_EFFECT_VALUE);
    match machine.run_until_suspension(run, handlers, user)? {
        ParkedOutcome::CompletedValue(value) => Ok(SuspendableRun::Completed(EvalResult::new(
            value,
            table,
            warnings.warnings,
        ))),
        ParkedOutcome::Suspended {
            id,
            request,
            has_live_payload: _,
        } => Ok(SuspendableRun::Suspended {
            machine,
            table,
            continuation: id,
            request,
        }),
        other => unreachable!("plain one-shot run returned {other:?}"),
    }
}

/// Re-enter a stowed turn (from [`compile_and_run_suspendable`]) with the
/// answer or an abort, driving to the next suspension or completion. Runs on
/// ANY thread — the machine re-installs its per-thread reach and re-points GC
/// state at its retained heap (never a nursery reset). `on_ready` receives the
/// machine's cancel handle before the (blocking) resume begins, exactly as
/// [`compile_and_run_cancellable`] does, so a runaway resume can be aborted.
pub fn resume_suspended_turn<U, H: DispatchEffect<U>>(
    machine: &mut JitEffectMachine,
    table: &DataConTable,
    handlers: &mut H,
    user: &U,
    continuation: ContinuationId,
    input: ResumeInput,
    on_ready: impl FnOnce(CancelHandle),
) -> Result<ResumedRun, RuntimeError> {
    on_ready(machine.realm_cancel_handle(RealmId::ROOT));
    match machine.resume_continuation(continuation, handlers, user, input)? {
        ParkedOutcome::CompletedValue(value) => {
            // No recompile happens on resume (the JIT machine is reused as-is),
            // so there are no new warnings to report here — they were already
            // surfaced on the turn that produced this continuation.
            Ok(ResumedRun::Completed(EvalResult::new(
                value,
                table.clone(),
                Vec::new(),
            )))
        }
        ParkedOutcome::Suspended {
            id,
            request,
            has_live_payload: _,
        } => Ok(ResumedRun::Suspended {
            continuation: id,
            request,
        }),
        other => unreachable!("plain one-shot resume returned {other:?}"),
    }
}

/// Compile Haskell source and run it as a pure (non-effectful) program.
///
/// Skips freer-simple effect dispatch — the result is converted directly
/// from the heap. Use this for programs that don't use an `Eff` wrapper.
pub fn compile_and_run_pure(
    source: &str,
    target: &str,
    include: &[&Path],
) -> Result<EvalResult, RuntimeError> {
    compile_and_run_pure_salted(source, target, include, None)
}

/// As [`compile_and_run_pure`], but threads a `(session, generation)` cache salt
/// (see [`compile_haskell_salted`]). The declaration-accumulation lane passes
/// [`session::SessionLib::cache_salt`] so per-session, per-generation compiles
/// of identical-text probes never collide and a generation bump invalidates.
pub fn compile_and_run_pure_salted(
    source: &str,
    target: &str,
    include: &[&Path],
    cache_salt: Option<&str>,
) -> Result<EvalResult, RuntimeError> {
    let CompileResult {
        expr,
        mut table,
        warnings,
    } = compile_haskell_salted(source, target, include, cache_salt)?;
    if warnings.has_io {
        return Err(RuntimeError::Compile(CompileError::IOTypeDetected));
    }
    table.populate_siblings_from_expr(&expr);
    let mut machine = JitEffectMachine::compile(&expr, &table, DEFAULT_NURSERY_SIZE)?;
    let value = machine.run_pure()?;
    Ok(EvalResult::new(value, table, warnings.warnings))
}

/// Compile Haskell source and run it with the given effect handlers,
/// using the default nursery size (64 MiB).
///
/// # Arguments
/// * `source` - The Haskell source code to compile.
/// * `target` - The name of the entry point binder.
/// * `include` - Search paths for Haskell modules.
/// * `handlers` - Effect dispatchers for the JIT machine.
/// * `user` - User context for effect handlers.
///
/// # Returns
/// * `Ok(EvalResult)` on successful execution.
/// * `Err(RuntimeError)` for compilation or JIT execution errors.
pub fn compile_and_run<U, H: DispatchEffect<U>>(
    source: &str,
    target: &str,
    include: &[&Path],
    handlers: &mut H,
    user: &U,
) -> Result<EvalResult, RuntimeError> {
    compile_and_run_with_nursery_size(
        source,
        target,
        include,
        handlers,
        user,
        DEFAULT_NURSERY_SIZE,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use serial_test::serial;

    #[test]
    #[serial]
    fn test_compile_identity() {
        tidepool_testing::eval_harness::require_extract();
        let source = "module Test where\nidentity x = x";
        let CompileResult { expr, .. } =
            compile_haskell(source, "identity", &[]).expect("Failed to compile identity");

        // identity = \x -> x — node count varies with GHC optimization level
        assert!(expr.nodes.len() >= 2);
    }

    /// The extractor captures the GHC-inferred type of the eval's top
    /// expression (the `__user` binding) and threads it out as
    /// `MetaWarnings::captured_type`. A module whose `__user` is `[1,2,3] :: [Int]`
    /// must report `[Int]` (GHC's `ppr` rendering of the list type).
    #[test]
    #[serial]
    fn test_captured_type_simple_list() {
        tidepool_testing::eval_harness::require_extract();
        let source = "module Probe where\n__user :: [Int]\n__user = [1, 2, 3]\n";
        let CompileResult { warnings, .. } =
            compile_haskell(source, "__user", &[]).expect("Failed to compile probe");
        eprintln!("captured_type = {:?}", warnings.captured_type);
        assert_eq!(warnings.captured_type.as_deref(), Some("[Int]"));
    }

    /// A binding with no explicit signature still gets a captured type; and an
    /// extraction with no `__user` binding reports `None` (fixture-style build).
    #[test]
    #[serial]
    fn test_captured_type_absent_without_user() {
        tidepool_testing::eval_harness::require_extract();
        let source = "module Probe where\nidentity x = x\n";
        let CompileResult { warnings, .. } =
            compile_haskell(source, "identity", &[]).expect("Failed to compile identity");
        assert_eq!(warnings.captured_type, None);
    }

    #[test]
    #[serial]
    fn test_compile_error() {
        tidepool_testing::eval_harness::require_extract();
        let source = "module Test where\nfoo = garbage";
        let res = compile_haskell(source, "foo", &[]);
        assert!(res.is_err());
        if let Err(CompileError::Diagnostics(diags)) = res {
            assert!(diags
                .iter()
                .any(|d| d.message.contains("not in scope: garbage")));
        } else {
            panic!("Expected Diagnostics error, got {:?}", res);
        }
    }

    /// A compile that SUCCEEDS but triggers a GHC diagnostic (overlapping
    /// patterns — on by default, no -Wall needed) surfaces that warning in
    /// `MetaWarnings::warnings` instead of dropping it silently.
    #[test]
    #[serial]
    fn test_compile_warnings_captured() {
        tidepool_testing::eval_harness::require_extract();
        let source = "module WarnProbe where\n\
                       f :: Int -> Int\n\
                       f x = 1\n\
                       f x = 2\n\
                       \n\
                       result :: Int\n\
                       result = f 0\n";
        let CompileResult { warnings, .. } =
            compile_haskell(source, "result", &[]).expect("Failed to compile result");
        assert!(
            !warnings.warnings.is_empty(),
            "expected at least one GHC warning for the overlapping `f` clauses"
        );
        assert!(
            warnings
                .warnings
                .iter()
                .any(|w| w.to_lowercase().contains("overlapping")),
            "expected an overlapping-patterns warning, got: {:?}",
            warnings.warnings
        );
        assert!(
            warnings.warnings.iter().all(|warning| {
                warning.contains("WarnProbe.hs:")
                    && !warning.contains(std::env::temp_dir().to_string_lossy().as_ref())
            }),
            "warnings must retain the module-relative source location without a temporary path: {:?}",
            warnings.warnings
        );

        // The second compile is the cache-hit half of the relocation proof:
        // it must return byte-equivalent warning text that is meaningful in
        // this process rather than the first producer's temporary directory.
        let CompileResult {
            warnings: cached, ..
        } = compile_haskell(source, "result", &[]).expect("Failed to reload cached result");
        assert_eq!(warnings.warnings, cached.warnings);
    }

    /// A clean compile (no diagnostics) reports no warnings.
    #[test]
    #[serial]
    fn test_compile_no_warnings_on_clean_source() {
        tidepool_testing::eval_harness::require_extract();
        let source = "module CleanProbe where\nresult :: Int\nresult = 1 + 1\n";
        let CompileResult { warnings, .. } =
            compile_haskell(source, "result", &[]).expect("Failed to compile result");
        assert!(
            warnings.warnings.is_empty(),
            "expected no warnings for a clean compile, got: {:?}",
            warnings.warnings
        );
    }
}
