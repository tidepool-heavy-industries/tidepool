//! High-level runtime for compiling and executing Haskell source via Tidepool.
//!
//! Provides `compile_haskell` (source to a checked prepared-STG program and
//! constructor metadata) and `compile_and_run` (source to evaluated result
//! through a one-shot [`session::prepared::PreparedEngine`]), with filesystem
//! caching of compiled artifacts.
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
pub use tidepool_bridge::HaskellValue;
pub use tidepool_codegen::host_fns::{drain_diagnostics, push_diagnostic};
pub use tidepool_codegen::machine::CancelHandle;
pub use tidepool_effect::dispatch::DispatchEffect;
pub use tidepool_effect::EffectError;
pub use tidepool_extract_cmd::{
    with_compiler_transaction, with_compiler_transaction_cancellable,
    CompilerTransactionCancellation,
};
use tidepool_repr::serial::MetaWarnings;
use tidepool_repr::DataConTable;

pub(crate) use tidepool_toolchain::extract_spawn_error;
pub use tidepool_toolchain::prepared_artifact::PreparedArtifact;
pub use tidepool_toolchain::CompileError;
pub use tidepool_toolchain::{artifacts, cache, diag, paths, timing, toolchain};

pub mod failclass;
/// Generated suspension-decode request types (`tidepool-protocol`'s
/// `runtime_generated_files`) — currently just `Ask`, shared with
/// `tidepool-harness`'s `RosterRequest::Ask`. `pub` (unlike
/// `tidepool-harness`'s own crate-private `generated` module) because the
/// harness is a genuine second consumer across a crate boundary, not an
/// internal implementation detail.
pub mod generated;
mod render;
pub mod session;
pub use session::prepared as prepared_execution;

pub use artifacts::{
    compile_targets, compile_targets_with_session_inject, compile_targets_with_stable_inject,
    CompiledArtifacts, NominalHead, SessionInject, SiteType, StableValInject, TargetArtifact,
    YieldSite, YieldSiteCollision, YieldSites,
};
pub use failclass::{
    classify, classify_compile, classify_session, FailureClass, FailureEnvelope, Phase,
};
pub use render::{value_to_json, EvalResult};

/// Render a caught panic payload without discarding its useful string detail.
#[must_use]
pub fn panic_payload_message(payload: Box<dyn std::any::Any + Send>) -> String {
    if let Some(message) = payload.downcast_ref::<&str>() {
        (*message).to_owned()
    } else if let Some(message) = payload.downcast_ref::<String>() {
        message.clone()
    } else {
        "unknown panic payload".to_owned()
    }
}

/// Result of successful Haskell compilation.
#[derive(Debug)]
pub struct CompileResult {
    /// DataCon metadata for constructor dispatch.
    pub table: DataConTable,
    /// Compile warnings (e.g. `has_io`, captured type).
    pub warnings: MetaWarnings,
    /// Checked versioned prepared-STG program.
    pub prepared: PreparedArtifact,
}

/// Unified error type for the compile-and-run pipeline.
#[derive(Error, Debug)]
pub enum RuntimeError {
    /// Error during Haskell compilation.
    #[error(transparent)]
    Compile(#[from] CompileError),
    /// A runtime or effect-handler failure from prepared execution.
    #[error(transparent)]
    Jit(#[from] EffectError),
    /// The prepared engine refused or failed a bare one-shot run (bootstrap,
    /// settle, or resume) — distinct from [`Self::Jit`], which covers a
    /// handler/effect failure once a run is under way.
    #[error(transparent)]
    Prepared(#[from] session::prepared::PreparedRuntimeError),
}

/// Compile Haskell source to a checked prepared-STG program.
///
/// GHC parses, typechecks, desugars, optimizes, and lowers the requested target
/// to STG. Tidepool serializes the prepared program and constructor metadata;
/// repeated compilations may be served from the toolchain cache.
pub fn compile_haskell(
    source: &str,
    target: &str,
    include: &[&Path],
) -> Result<CompileResult, CompileError> {
    let include_owned: Vec<PathBuf> = include.iter().map(|p| p.to_path_buf()).collect();
    let inv = artifacts::CompileInvocation {
        source,
        targets: &[target],
        include: &include_owned,
        bin: None,
        fallback_module_name: "Input",
        cache: artifacts::CacheStrategy::Immutable,
        stable_val: None,
        session_inject: None,
    };
    let mut bundle = artifacts::compile_invocation(&inv, |_, _, _| {})?;
    #[allow(
        clippy::expect_used,
        reason = "compile_invocation compiled exactly this target"
    )]
    let TargetArtifact { prepared, .. } = bundle
        .targets
        .remove(target)
        .expect("compile_invocation compiled exactly this target");
    let CompiledArtifacts {
        table, warnings, ..
    } = bundle;
    Ok(CompileResult {
        table,
        warnings,
        prepared,
    })
}

/// Default JIT allocation nursery size (64 MiB), used by [`compile_and_run`]
/// and [`compile_and_run`].
pub const DEFAULT_NURSERY_SIZE: usize = 1 << 26; // 64 MiB

/// Stack size for eval threads. The JIT's clean recursion-overflow guard needs
/// stack headroom; too small a stack lets a deep non-tail recursion blow the
/// host stack into corruption ("unexpected heap tag") before the guard fires.
/// Shared by the MCP server's eval thread and the test harness so they can't
/// drift — a smaller test stack made the overflow probes diverge from real evals.
pub const EVAL_STACK_SIZE: usize = 256 * 1024 * 1024; // 256 MiB

/// Compile one `Eff` expression and run it with the given effect handlers,
/// using the specified nursery size.
///
/// # Arguments
/// * `preamble` - Module header, pragmas, and imports (e.g. from
///   `tidepool_mcp::build_preamble`) — everything before the compiled
///   binding. Must NOT itself import `Tidepool.Internal.Resume` or define
///   anything named `__resume`/`__applyEntry`/
///   `__applyValue`/`__prepared`/`__tidepoolInEffectRow`/`__workbenchValue`:
///   [`session::assemble_expression_module`] owns those names.
/// * `target` - Name for the assembled top-level binding (only used inside
///   the assembled module; the actual compile target is always
///   [`session::PREPARED_SCAFFOLD_TARGET`] — see below).
/// * `effect_stack` - The row `expression`'s `Eff` type is pinned to while
///   GHC infers it (e.g. `"'[Console, Ask]"`), exactly
///   [`session::assemble_expression_module`]'s `effect_stack` parameter.
/// * `expression` - The `Eff effect_stack a` computation to run — a single
///   Haskell expression (a `do { ... }` block is one expression, so a
///   caller with a statement sequence can pass `tidepool_mcp::wrap_do(...)`
///   of it directly).
/// * `include` - Search paths for Haskell modules.
/// * `handlers` - Effect dispatchers for the prepared machine.
/// * `user` - User context for effect handlers.
/// * `nursery_size` - Size of the allocation nursery in bytes.
///
/// # Why pieces, not a whole module string
///
/// A prepared program's `Tidepool.Internal.Resume.Done`/`Suspended` are only
/// reachable — hence observable via `run_settled` — when the compiled module
/// defines the fixed-named scaffold bindings `session::turn`'s assembly
/// helpers write (see `plans/core-engine-removal.md`'s "The `UnsettledEntry`
/// condition"). Splicing that scaffold into an arbitrary ALREADY-ASSEMBLED
/// module string is not safe in general (a bare append breaks Haskell's
/// import-before-declarations layout rule); every caller that already
/// builds its source from a preamble + one expression already has the
/// pieces this needs, so building the scaffolded module is this crate's
/// job via the SAME assembly `session::turn`'s own resident-turn machinery
/// uses ([`session::assemble_expression_module`]), not a second assembler
/// or a caller-side splice.
///
/// # Returns
/// * `Ok(EvalResult)` on successful execution.
/// * `Err(RuntimeError)` for compilation or execution errors.
#[allow(clippy::too_many_arguments)]
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

/// As [`compile_and_run_with_nursery_size`], but hands the freshly-built
/// engine's [`CancelHandle`] to `on_ready` BEFORE the (blocking) run begins.
///
/// The handle is `Send + Sync + Clone`, so a caller running this on a worker
/// thread can ship a clone to a watchdog/timeout task that flips it; the
/// running program then aborts at its next safepoint with a cancellation
/// failure, freeing the thread (and any resources it pins). This is how the
/// eval/repl servers turn a turn timeout into an actual abort instead of a
/// permanently-parked thread.
///
/// A bare one-shot run outside any session: it assembles the expression
/// through the exact same scaffold `session::turn`'s resident-turn assembly
/// uses ([`session::assemble_expression_module`] +
/// [`session::PREPARED_SCAFFOLD_TARGET`] as the compile target — see this
/// function's sibling doc for why), bootstraps a standalone
/// [`session::prepared::PreparedEngine`] from the compiled prepared program,
/// runs its settled entry to completion, and drops the engine (and its
/// heap) once done. A request no installed handler recognizes is reported
/// as [`tidepool_effect::error::EffectError::UnhandledEffect`],
/// matching what a plain (non-suspendable) run has always reported for an
/// unclaimed effect — there is no resume path here for a caller to answer
/// it later.
#[allow(clippy::too_many_arguments)]
pub fn compile_and_run_cancellable<U, H: DispatchEffect<U>>(
    source: &str,
    target: &str,
    include: &[&Path],
    handlers: &mut H,
    user: &U,
    nursery_size: usize,
    on_ready: impl FnOnce(CancelHandle),
) -> Result<EvalResult, RuntimeError> {
    let _ = target;
    let CompileResult {
        table,
        warnings,
        prepared,
    } = compile_haskell(source, session::PREPARED_SCAFFOLD_TARGET, include)?;
    if warnings.has_io {
        return Err(RuntimeError::Compile(CompileError::IOTypeDetected));
    }
    let value = run_prepared_program(
        prepared.into_prepared(),
        &table,
        nursery_size,
        handlers,
        user,
        on_ready,
    )?;
    Ok(EvalResult::new(value, table, warnings.warnings))
}

/// Run an ALREADY-COMPILED prepared program to completion against `handlers`,
/// as a bare one-shot: no session, no actor, no decl plane. Bootstraps a
/// standalone [`session::prepared::PreparedEngine`], settles the entry, and
/// drives any parked request through `handlers` via the resident turn
/// machinery's own suspend/dispatch/resume loop
/// ([`session::resident`]'s `finish_prepared`, `pub(crate)` there) — reusing
/// exactly what a resident session turn already uses, not a second engine.
///
/// [`compile_and_run_cancellable`] is this plus its own `compile_haskell`
/// call; this lower-level entry point is for a caller that already holds a
/// [`tidepool_repr::execution_schema::PreparedProgram`] from its own earlier
/// compile (e.g. `tidepool-testing::eval_harness::EvalHarness::run_target*`,
/// which extracts several targets from one `tidepool-extract` invocation and
/// must not spawn a second one per target it runs).
///
/// `on_ready` receives the freshly-built engine's [`CancelHandle`] BEFORE
/// the (blocking) run begins, exactly as [`compile_and_run_cancellable`]
/// uses it. A request no installed handler recognizes is reported as
/// [`tidepool_effect::error::EffectError::UnhandledEffect`],
/// matching what a plain (non-suspendable) run has always reported for an
/// unclaimed effect — there is no resume path here for a caller to answer
/// it later.
pub fn run_prepared_program<U, H: DispatchEffect<U>>(
    prepared: tidepool_repr::execution_schema::PreparedProgram,
    table: &DataConTable,
    nursery_size: usize,
    handlers: &mut H,
    user: &U,
    on_ready: impl FnOnce(CancelHandle),
) -> Result<HaskellValue, RuntimeError> {
    use session::prepared::{ParkPolicy, PreparedEngine};
    use session::resident::{finish_prepared, PreparedRun, SettlePlan};
    use tidepool_codegen::suspension::RealmId;
    use tidepool_effect::dispatch::request_constructor;
    use tidepool_effect::error::EffectError;
    use tidepool_effect::{EffectRunPolicy, LivePayloadPolicy};
    use tidepool_repr::PrincipalId;

    let (mut engine, program) =
        PreparedEngine::bootstrap_with_nursery_bytes(prepared, nursery_size)?;
    let realm = RealmId::ROOT;
    on_ready(engine.cancel_handle(realm));
    let park = ParkPolicy {
        principal: PrincipalId::SYSTEM,
        effect_policy: EffectRunPolicy::HandleOrSuspend,
        live_payload: LivePayloadPolicy::HASKELL_EFFECT_VALUE,
    };
    let settlement = engine.run_settled(program, realm)?;
    let run = finish_prepared(
        &mut engine,
        program,
        realm,
        SettlePlan::Observe,
        park,
        table,
        handlers,
        user,
        settlement,
    )?;
    match run {
        PreparedRun::Done { value, .. } => Ok(value),
        PreparedRun::Suspended { id, request } => {
            let constructor = request_constructor(&request, table);
            // No handler claimed it and there is no resume path in a
            // one-shot run: release the parked frame rather than leak it.
            let _ = engine.abort_parked(id);
            Err(RuntimeError::Jit(EffectError::UnhandledEffect {
                constructor,
            }))
        }
        PreparedRun::Projected { .. } => {
            unreachable!("SettlePlan::Observe never produces a projected run")
        }
    }
}

/// Compile one `Eff` expression and run it with the given effect handlers,
/// using the default nursery size (64 MiB). See
/// [`compile_and_run_with_nursery_size`] for the full argument doc and why
/// this takes assembly pieces rather than a whole module string.
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
        let CompileResult { prepared, .. } =
            compile_haskell(source, "identity", &[]).expect("Failed to compile identity");
        assert!(!prepared.bytes().is_empty());
        assert!(!prepared.prepared().bindings().is_empty());
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
