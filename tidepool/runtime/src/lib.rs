//! High-level runtime for compiling and executing Haskell source via Tidepool.
//!
//! Provides `compile_haskell` (source to a checked prepared-STG program and
//! constructor metadata) and `compile_and_run` (source to evaluated result
//! through a one-shot [`session::prepared::PreparedEngine`]), with filesystem
//! caching of compiled artifacts.
//!
//! Toolchain location, validation, fingerprinting, and the compile-output
//! cache live in `tidepool-toolchain` (a crate this one depends on and sits
//! above). Internal module names below refer to `tidepool-toolchain` directly.
//! `failclass` is a real local module (its `classify`/`classify_session`
//! dispatch over this crate's own `RuntimeError`/`SessionError`, so they
//! can't live below in `tidepool-toolchain`) that re-exports the rest of the
//! classifier from there.

use std::path::{Path, PathBuf};
use thiserror::Error;
pub use tidepool_bridge::HaskellValue;
pub use tidepool_codegen::host_fns::{drain_diagnostics, push_diagnostic};
pub use tidepool_codegen::machine::CancelHandle;
pub use tidepool_effect::dispatch::DispatchEffect;
pub use tidepool_effect::EffectError;
pub use tidepool_extract_cmd::{
    with_compiler_transaction, with_compiler_transaction_cancellable,
    CompilerTransactionCancellation, CompilerTransactionClose, CompilerTransactionOutcome,
};
use tidepool_repr::serial::MetaWarnings;
use tidepool_repr::DataConTable;

pub(crate) use tidepool_toolchain::extract_spawn_error;
pub use tidepool_toolchain::prepared_artifact::PreparedArtifact;
pub use tidepool_toolchain::CompileError;
pub(crate) use tidepool_toolchain::{artifacts, diag, paths, timing, toolchain};

pub mod failclass;
/// Generated suspension-decode request types (`tidepool-protocol`'s
/// `runtime_generated_files`) — currently just `Ask`, shared with
/// `exomonad-harness`'s `RosterRequest::Ask`. `pub` (unlike
/// `exomonad-harness`'s own crate-private `generated` module) because the
/// harness is a genuine second consumer across a crate boundary, not an
/// internal implementation detail.
pub mod generated;
mod render;
pub mod session;
pub mod span_blocking;
pub use session::prepared as prepared_execution;
pub use span_blocking::spawn_blocking_in_span;

pub use artifacts::{
    compile_targets, CompiledArtifacts, NominalHead, RequestInputLayout, RequestInputLayoutError,
    SiteType, TargetArtifact, YieldSite, YieldSiteCollision, YieldSites,
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
    settlement: &mut dyn FnMut(crate::CompilerTransactionClose),
) -> Result<CompileResult, CompileError> {
    let include_owned: Vec<PathBuf> = include.iter().map(|p| p.to_path_buf()).collect();
    let inv = artifacts::CompileInvocation {
        source,
        targets: &[target],
        include: &include_owned,
        fallback_module_name: "Input",
    };
    let mut bundle = artifacts::compile_invocation(&inv, |_, _, _| {}, settlement)?;
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
/// helpers write. Splicing that scaffold into an arbitrary ALREADY-ASSEMBLED
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
    settlement: &mut dyn FnMut(crate::CompilerTransactionClose),
) -> Result<EvalResult, RuntimeError> {
    compile_and_run_cancellable(
        source,
        target,
        include,
        handlers,
        user,
        nursery_size,
        |_| {},
        settlement,
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
    settlement: &mut dyn FnMut(crate::CompilerTransactionClose),
) -> Result<EvalResult, RuntimeError> {
    let _ = target;
    let include_owned = include
        .iter()
        .map(|path| path.to_path_buf())
        .collect::<Vec<_>>();
    let artifacts = compile_targets(
        source,
        &[session::PREPARED_SCAFFOLD_TARGET],
        &include_owned,
        |_, _, _| {},
        settlement,
    )?;
    let value = run_compiled_target(
        &artifacts,
        session::PREPARED_SCAFFOLD_TARGET,
        nursery_size,
        handlers,
        user,
        on_ready,
    )?;
    Ok(EvalResult::new(
        value,
        artifacts.table,
        artifacts.warnings.warnings,
    ))
}

/// Run an ALREADY-COMPILED prepared program to completion against `handlers`,
/// as a bare one-shot: no session, no actor, no persistent declaration environment. Bootstraps a
/// standalone [`session::prepared::PreparedEngine`], settles the entry, and
/// drives any parked request through `handlers` via the resident turn
/// machinery's own suspend/dispatch/resume loop
/// ([`session::resident`]'s `finish_prepared`, `pub(crate)` there) — reusing
/// exactly what a resident session turn already uses, not a second engine.
///
/// This entry point requires a self-contained program. Compiler artifact
/// consumers use [`run_compiled_target`] to install certified imported groups
/// alongside the target before entering the same settlement loop.
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
    let (mut engine, program) =
        session::prepared::PreparedEngine::bootstrap_with_nursery_bytes(prepared, nursery_size)?;
    run_installed_program(&mut engine, program, table, handlers, user, on_ready)
}

/// Run an ordinary compiler target with its complete certified source closure.
/// Source groups and the target install together through the resident linker,
/// with fresh mutable state for this one-shot execution.
pub fn run_compiled_target<U, H: DispatchEffect<U>>(
    artifacts: &CompiledArtifacts,
    target: &str,
    nursery_size: usize,
    handlers: &mut H,
    user: &U,
    on_ready: impl FnOnce(CancelHandle),
) -> Result<HaskellValue, RuntimeError> {
    let mut state = session::PersistentSession::new(None, nursery_size);
    let program = install_compiled_target(&mut state, artifacts, target)?;
    let engine = state.require_prepared()?;
    let result = run_installed_program(engine, program, &artifacts.table, handlers, user, on_ready);
    engine.unpin(program);
    result
}

/// Install an ordinary compiler target with its complete certified closure in
/// the selected session. Keep the target pinned until its caller finishes.
fn install_compiled_target(
    state: &mut session::PersistentSession,
    artifacts: &CompiledArtifacts,
    target: &str,
) -> Result<tidepool_codegen::prepared_program::ProgramId, RuntimeError> {
    use session::prepared::CertifiedTargetImage;
    use tidepool_codegen::scope::ScopeId;

    if artifacts.warnings.has_io {
        return Err(CompileError::IOTypeDetected.into());
    }
    let target = artifacts.targets.get(target).ok_or_else(|| {
        CompileError::ExtractFailed(format!("compiled artifact has no target {target:?}"))
    })?;
    artifacts
        .table
        .validate_program(target.prepared.prepared())
        .map_err(tidepool_repr::serial::ReadError::ConstructorMetadata)
        .map_err(CompileError::from)?;
    let certification = session::turn::TurnCertification::from_artifacts(artifacts, target);
    let resolved = state.resolve_certification_in(
        ScopeId::ROOT,
        target.prepared.prepared(),
        &certification,
    )?;
    let registry = state.certified_image_registry();
    let (target, demanded) = CertifiedTargetImage::compile_scoped(
        target.prepared.prepared().clone(),
        &resolved,
        &registry,
    )?;
    let (program, _) = state.install_certified_turn_in(
        ScopeId::ROOT,
        target,
        &resolved.target_owners,
        &resolved.source_evidence,
        demanded,
        &resolved.inherited_needed,
    )?;
    Ok(program)
}

fn run_installed_program<U, H: DispatchEffect<U>>(
    engine: &mut session::prepared::PreparedEngine,
    program: tidepool_codegen::prepared_program::ProgramId,
    table: &DataConTable,
    handlers: &mut H,
    user: &U,
    on_ready: impl FnOnce(CancelHandle),
) -> Result<HaskellValue, RuntimeError> {
    use session::prepared::{ParkPolicy, PreparedRuntimeError};
    use session::resident::{finish_prepared, PreparedRun, SettlePlan};
    use tidepool_codegen::suspension::RealmId;
    use tidepool_effect::dispatch::{request_constructor, DeferredEffect};
    use tidepool_effect::error::EffectError;
    use tidepool_effect::{EffectRunPolicy, LivePayloadPolicy};
    use tidepool_repr::PrincipalId;

    let realm = RealmId::ROOT;
    on_ready(engine.cancel_handle(realm));
    let park = ParkPolicy {
        principal: PrincipalId::SYSTEM,
        effect_policy: EffectRunPolicy::HandleOrSuspend,
        live_payload: LivePayloadPolicy::HASKELL_EFFECT_VALUE,
    };
    let settlement = engine.run_settled(program, realm)?;
    let mut run = finish_prepared(
        engine,
        program,
        realm,
        SettlePlan::Observe,
        park,
        table,
        handlers,
        user,
        settlement,
    )?;
    loop {
        match run {
            PreparedRun::Done { value, .. } => return Ok(value),
            PreparedRun::Deferred { id, work, .. } => {
                let response = match work {
                    DeferredEffect::Blocking(work) => {
                        work.into_inner()
                            .unwrap_or_else(|poison| poison.into_inner())()
                        .map_err(RuntimeError::Jit)
                    }
                    DeferredEffect::Async(_) => {
                        Err(PreparedRuntimeError::DeferredRequiresAsyncHost.into())
                    }
                };
                let response = match response {
                    Ok(response) => response,
                    Err(error) => {
                        if let Err(abort_error) = engine.abort_parked(id) {
                            tracing::warn!(?abort_error, ?id, "failed to abort deferred frame");
                        }
                        return Err(error);
                    }
                };
                let resumed = match engine.resume_with_structural_answer(id, &response, table) {
                    Ok(resumed) => resumed,
                    Err(error) => {
                        if let Err(abort_error) = engine.abort_parked(id) {
                            tracing::warn!(
                                ?abort_error,
                                ?id,
                                "failed to abort refused deferred answer"
                            );
                        }
                        return Err(error.into());
                    }
                };
                run = finish_prepared(
                    engine,
                    resumed.runner,
                    resumed.realm,
                    SettlePlan::Observe,
                    park,
                    table,
                    handlers,
                    user,
                    resumed.settlement,
                )?;
            }
            PreparedRun::Suspended { id, request } => {
                let constructor = request_constructor(&request, table);
                // No handler claimed it and there is no resume path in a
                // one-shot run: release the parked frame rather than leak it.
                if let Err(err) = engine.abort_parked(id) {
                    tracing::warn!(
                        ?err,
                        ?id,
                        "failed to abort parked frame for unhandled effect"
                    );
                }
                return Err(RuntimeError::Jit(EffectError::UnhandledEffect {
                    constructor,
                }));
            }
            PreparedRun::Projected { .. } => {
                unreachable!("SettlePlan::Observe never produces a projected run")
            }
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
    settlement: &mut dyn FnMut(crate::CompilerTransactionClose),
) -> Result<EvalResult, RuntimeError> {
    compile_and_run_with_nursery_size(
        source,
        target,
        include,
        handlers,
        user,
        DEFAULT_NURSERY_SIZE,
        settlement,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use serial_test::serial;

    #[test]
    fn selected_artifact_revalidates_nominal_metadata_before_installation() {
        tidepool_testing::eval_harness::require_extract();
        let source = "module NominalAdmissionControl where\nresult = Just (42 :: Int)\n";
        let mut artifacts = tidepool_testing::with_settlement(|settlement| {
            tidepool_toolchain::artifacts::compile_invocation(
                &tidepool_toolchain::artifacts::CompileInvocation {
                    source,
                    targets: &["result"],
                    include: &[],
                    fallback_module_name: "NominalAdmissionControl",
                },
                |_, _, _| {},
                settlement,
            )
        })
        .unwrap();
        let target = artifacts.targets["result"].prepared.prepared();
        artifacts.table.validate_program(target).unwrap();
        let declared = target
            .constructors()
            .first()
            .expect("real compiler constructor")
            .clone();
        let mut changed = tidepool_repr::DataConTable::new();
        changed
            .extend_checked(artifacts.table.iter().cloned().map(|mut row| {
                if row.id == declared.host_id {
                    row.identity.unit.push_str("-foreign");
                }
                row
            }))
            .unwrap();
        artifacts.table = changed;
        let mut state = session::PersistentSession::new(None, DEFAULT_NURSERY_SIZE);
        let error = install_compiled_target(&mut state, &artifacts, "result").unwrap_err();
        assert!(matches!(
            error,
            RuntimeError::Compile(CompileError::ReadError(
                tidepool_repr::serial::ReadError::ConstructorMetadata(
                    tidepool_repr::datacon_table::ConstructorMetadataMismatch::Identity { .. }
                )
            ))
        ));
        assert!(state.prepared().is_none());
    }

    struct DiagnosticTestScope {
        _root: tempfile::TempDir,
        previous: Vec<(&'static str, Option<std::ffi::OsString>)>,
    }

    impl DiagnosticTestScope {
        fn when_not_owned_by_runner() -> Option<Self> {
            if std::env::var("TIDEPOOL_TEST_DIAGNOSTIC_SCOPE").as_deref() == Ok("1") {
                return None;
            }
            let root = tempfile::tempdir().unwrap();
            let previous = [
                "TIDEPOOL_TEST_DIAGNOSTIC_SCOPE",
                "TIDEPOOL_TEST_ARTIFACT_ROOT",
            ]
            .into_iter()
            .map(|key| (key, std::env::var_os(key)))
            .collect();
            // Native libtest cases run in separate processes. This control is
            // also serial for direct libtest invocation and starts no compiler
            // work until its two diagnostic-only coordinates are installed.
            unsafe {
                std::env::set_var("TIDEPOOL_TEST_DIAGNOSTIC_SCOPE", "1");
                std::env::set_var("TIDEPOOL_TEST_ARTIFACT_ROOT", root.path());
            }
            Some(Self {
                _root: root,
                previous,
            })
        }
    }

    impl Drop for DiagnosticTestScope {
        fn drop(&mut self) {
            for (key, value) in &self.previous {
                unsafe {
                    match value {
                        Some(value) => std::env::set_var(key, value),
                        None => std::env::remove_var(key),
                    }
                }
            }
        }
    }

    #[test]
    #[serial]
    fn test_compile_identity() {
        tidepool_testing::eval_harness::require_extract();
        let source = "module Test where\nidentity x = x";
        let CompileResult { prepared, .. } = tidepool_testing::with_settlement(|settlement| {
            compile_haskell(source, "identity", &[], settlement)
        })
        .expect("Failed to compile identity");
        assert!(!prepared.bytes().is_empty());
        assert!(!prepared.prepared().bindings().is_empty());
    }

    #[test]
    #[serial]
    fn successful_compilation_retains_original_inputs_after_native_execution_failure() {
        use std::sync::{atomic::AtomicBool, Arc};
        use tidepool_codegen::host_fns::RuntimeError as NativeError;
        use tidepool_codegen::prepared_program::{CompiledProgram, ExecutionError, RunOptions};
        use tidepool_repr::execution_schema::{link_program, MachineImports};

        tidepool_testing::eval_harness::require_extract();
        let _diagnostic_scope = DiagnosticTestScope::when_not_owned_by_runner();
        let root = PathBuf::from(std::env::var_os("TIDEPOOL_TEST_ARTIFACT_ROOT").unwrap());
        // A new authored module forces a real transaction even with a warm memo.
        let unique = tempfile::tempdir().unwrap();
        let module = format!(
            "DiagnosticFailure{}",
            unique
                .path()
                .file_name()
                .unwrap()
                .to_string_lossy()
                .chars()
                .filter(|character| character.is_ascii_alphanumeric())
                .collect::<String>()
        );
        let source = format!(
            "module {module} where\nresult :: Int\nresult = error \"diagnostic native execution failure\"\n"
        );
        let compiled = tidepool_testing::with_settlement(|settlement| {
            compile_haskell(&source, "result", &[], settlement)
        })
        .expect("genuine compiler control");
        let prepared = compiled.prepared.into_prepared();
        let entry = prepared.entry();
        let linked = link_program(prepared, &MachineImports::default())
            .expect("genuine prepared native admission");
        let image = CompiledProgram::compile(&linked).expect("genuine prepared native compilation");
        // This authored entry returns a plain Int. The codegen entry owner
        // executes it directly; a resident turn's Settled scaffold is separate.
        let failure = image
            .run_entry(
                entry,
                &[],
                &RunOptions::default(),
                Arc::new(AtomicBool::new(false)),
            )
            .expect_err("the admitted program must fail during native execution");
        assert!(
            matches!(failure, ExecutionError::Runtime(ref error)
                if matches!(error.cause, NativeError::UserError | NativeError::RaisedException)
                || matches!(&error.cause, NativeError::RaisedExceptionMessage(message)
                    | NativeError::WiredInError { message, .. }
                    if message.contains("diagnostic native execution failure"))),
            "{failure:?}"
        );
        drop(image);
        let transaction = std::fs::read_dir(root.join("compiler-transactions"))
            .unwrap()
            .filter_map(Result::ok)
            .find(|entry| {
                std::fs::read_to_string(entry.path().join(format!("{module}.hs")))
                    .is_ok_and(|retained| retained == source)
            })
            .expect("the original source directory survives compiler return and native failure");
        let report: serde_json::Value = serde_json::from_slice(
            &std::fs::read(transaction.path().join("transaction.json")).unwrap(),
        )
        .unwrap();
        assert_eq!(report["compiler_process_success"], true);
        assert_eq!(report["authority"], false);
        assert_eq!(
            report["original_directory"],
            transaction.path().to_string_lossy().as_ref()
        );
        for name in [
            "compiler-request.bin",
            "result.prepared.cbor",
            "dependencies.json",
            "certified-products.cbor",
            "module-products.cbor",
            "meta.cbor",
            "consumed-sources.json",
        ] {
            let path = transaction.path().join(name);
            assert!(
                path.is_file() && path.metadata().unwrap().len() > 0,
                "{path:?}"
            );
        }
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
        let CompileResult { warnings, .. } = tidepool_testing::with_settlement(|settlement| {
            compile_haskell(source, "__user", &[], settlement)
        })
        .expect("Failed to compile probe");
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
        let CompileResult { warnings, .. } = tidepool_testing::with_settlement(|settlement| {
            compile_haskell(source, "identity", &[], settlement)
        })
        .expect("Failed to compile identity");
        assert_eq!(warnings.captured_type, None);
    }

    #[test]
    #[serial]
    fn test_compile_error() {
        tidepool_testing::eval_harness::require_extract();
        let source = "module Test where\nfoo = garbage";
        let res = tidepool_testing::with_settlement(|settlement| {
            compile_haskell(source, "foo", &[], settlement)
        });
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
        let CompileResult { warnings, .. } = tidepool_testing::with_settlement(|settlement| {
            compile_haskell(source, "result", &[], settlement)
        })
        .expect("Failed to compile result");
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
        } = tidepool_testing::with_settlement(|settlement| {
            compile_haskell(source, "result", &[], settlement)
        })
        .expect("Failed to reload cached result");
        assert_eq!(warnings.warnings, cached.warnings);
    }

    /// A clean compile (no diagnostics) reports no warnings.
    #[test]
    #[serial]
    fn test_compile_no_warnings_on_clean_source() {
        tidepool_testing::eval_harness::require_extract();
        let source = "module CleanProbe where\nresult :: Int\nresult = 1 + 1\n";
        let CompileResult { warnings, .. } = tidepool_testing::with_settlement(|settlement| {
            compile_haskell(source, "result", &[], settlement)
        })
        .expect("Failed to compile result");
        assert!(
            warnings.warnings.is_empty(),
            "expected no warnings for a clean compile, got: {:?}",
            warnings.warnings
        );
    }
}
