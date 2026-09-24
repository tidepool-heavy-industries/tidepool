//! S4a acceptance: a prepared session's between-turn quiescent point
//! actually drains a major collection, so residency stays bounded across
//! many turns instead of growing one program per turn forever.
//!
//! The collection itself is amortized (`PreparedEngine::quiesce_and_collect`,
//! `tidepool/runtime/src/session/prepared.rs`): it only actually runs a
//! major collection every `MAJOR_COLLECTION_INSTALL_INTERVAL` installed
//! programs (or sooner on enough root-block byte growth), not after every
//! turn. `N` below mirrors that constant -- it is not reachable from here
//! (private to `prepared.rs`, and `ResidentSession` exposes only
//! `residency()`/`old_bytes()`, no install-count accessor), so this file
//! treats it as a fixture constant instead. Each expression/bind turn below
//! installs exactly one program (confirmed by the pre-amortization version
//! of this test), so a major collection lands deterministically every `N`th
//! turn as long as byte growth never trips the earlier threshold first --
//! true for these tiny arithmetic turns. Counters are therefore only
//! expected to be flat turn-over-turn *at* those collection boundaries; in
//! between, `programs` (and the other counts) may grow up to the
//! amortized bound before the next collection folds them back down.
//!
//! Needs a resolvable `$TIDEPOOL_EXTRACT` and its Haskell worker (as
//! `prepared_turn.rs` does); no Core-route counterpart -- `residency()` is
//! `None` on Core, so there is nothing to assert there.

use std::path::{Path, PathBuf};

use tidepool_repr::Generation;
use tidepool_runtime::session::{
    resident_workbench_templates, run_turn, BoundBinder, CompiledTurn, ModuleEnv, ResidentOutcome,
    ResidentSession, SessionLib, TurnRequest, TurnResult, TurnTemplate,
};
use tidepool_testing::effect_surface::TestEffectSurface;
use tidepool_testing::eval_harness;

/// The minimal parts of `prepared_turn.rs`'s own `Notebook` this file needs:
/// one resident session, its compile plumbing, and expression/bind turns.
struct Notebook {
    session: ResidentSession<frunk::HNil, tidepool_mcp::CapturedOutput>,
    preamble: String,
    effect_stack: String,
    include: Vec<PathBuf>,
    root: tempfile::TempDir,
    injected: Vec<String>,
    generation: u64,
}

impl Notebook {
    fn new() -> Self {
        Self::with_effects(&[])
    }

    /// [`Self::with_effects`], but the declaration plane's include directory
    /// IS `root` (rather than a nested `decl-lib` subdirectory) -- the
    /// production wiring (`SessionLib::open(id, session_root, ..)` in
    /// `exomonad-actor`'s workbench). A declaration turn's own GHC downsweep
    /// then resolves `root`-rooted `Val.G<g>` host-carrier stubs the same
    /// way a later cell's does, so a decl can capture a stub-mounted name.
    fn with_shared_root(decls: &[tidepool_mcp::EffectDecl]) -> Self {
        eval_harness::require_extract();
        let effects = TestEffectSurface::minimal(decls).expect("materialize effect surface");
        let preamble = effects.preamble().to_owned();
        let effect_stack = effects.row().to_owned();
        let mut include = effects.include_paths().to_vec();
        let root = tempfile::tempdir().expect("session root");
        let lib = SessionLib::open(
            tidepool_repr::SessionId(1),
            root.path(),
            ModuleEnv::standalone_default(),
        )
        .expect("open decl plane")
        .with_validation_include(vec![eval_harness::prelude_path()]);
        include.push(lib.include_dir().to_path_buf());
        let session = ResidentSession::unbootstrapped(
            frunk::HNil,
            tidepool_mcp::CapturedOutput::new(),
            tidepool_runtime::DEFAULT_NURSERY_SIZE,
            Some(lib),
        );
        Self {
            session,
            preamble,
            effect_stack,
            include,
            root,
            injected: Vec::new(),
            generation: 0,
        }
    }

    fn with_effects(decls: &[tidepool_mcp::EffectDecl]) -> Self {
        eval_harness::require_extract();
        let effects = TestEffectSurface::minimal(decls).expect("materialize effect surface");
        let preamble = effects.preamble().to_owned();
        let effect_stack = effects.row().to_owned();
        let mut include = effects.include_paths().to_vec();
        let root = tempfile::tempdir().expect("session root");
        let lib = SessionLib::open(
            tidepool_repr::SessionId(1),
            root.path().join("decl-lib"),
            ModuleEnv::standalone_default(),
        )
        .expect("open decl plane")
        .with_validation_include(vec![eval_harness::prelude_path()]);
        include.push(lib.include_dir().to_path_buf());
        let session = ResidentSession::unbootstrapped(
            frunk::HNil,
            tidepool_mcp::CapturedOutput::new(),
            tidepool_runtime::DEFAULT_NURSERY_SIZE,
            Some(lib),
        );
        Self {
            session,
            preamble,
            effect_stack,
            include,
            root,
            injected: Vec::new(),
            generation: 0,
        }
    }

    fn templates(&self) -> Vec<TurnTemplate> {
        resident_workbench_templates(
            &self.preamble,
            &self.effect_stack,
            &self.injected.join("\n"),
        )
    }

    fn compile(&mut self, text: &str) -> TurnResult {
        self.generation += 1;
        let retained = self.session.prepared_retained();
        let templates = self.templates();
        let include: Vec<&Path> = self.include.iter().map(PathBuf::as_path).collect();
        run_turn(TurnRequest {
            turn_text: text,
            templates: &templates,
            include: &include,
            session_root: self.root.path(),
            inject_modules: &self.injected,
            gen: self.generation,
            verdict: None,
            target: None,
            retained_imports: &retained,
        })
        .unwrap_or_else(|failure| {
            panic!(
                "{text:?} failed to compile: {}\n{}",
                tidepool_runtime::classify_compile(&failure.error).message,
                failure
                    .attempted_source
                    .as_deref()
                    .unwrap_or("<no attempted source>")
            )
        })
    }

    fn prepare_expression(&mut self, text: &str) -> CompiledTurn {
        let TurnResult::Expr { compiled, .. } = self.compile(text) else {
            panic!("{text:?} did not classify as an expression");
        };
        compiled
    }

    /// Compile against the session's current source view while giving the
    /// extractor every still-live value interface needed to link retained
    /// closures. Shadowed generations stay injected but are not imported
    /// unqualified, so a rebound name remains unambiguous to new source.
    fn compile_in_current_value_view(&mut self, text: &str) -> TurnResult {
        self.generation += 1;
        let imports = self
            .session
            .current_val_modules()
            .into_iter()
            .map(|module| format!("{module}\n"))
            .collect::<String>();
        let templates = resident_workbench_templates(&self.preamble, &self.effect_stack, &imports);
        let include: Vec<&Path> = self.include.iter().map(PathBuf::as_path).collect();
        let injected = self.session.inject_val_modules();
        let retained = self.session.prepared_retained();
        run_turn(TurnRequest {
            turn_text: text,
            templates: &templates,
            include: &include,
            session_root: self.root.path(),
            inject_modules: &injected,
            gen: self.generation,
            verdict: None,
            target: None,
            retained_imports: &retained,
        })
        .unwrap_or_else(|failure| {
            panic!(
                "{text:?} failed to compile: {}\n{}",
                tidepool_runtime::classify_compile(&failure.error).message,
                failure
                    .attempted_source
                    .as_deref()
                    .unwrap_or("<no attempted source>")
            )
        })
    }

    /// Install the immutable artifact afresh on every turn. Compilation is
    /// shared; mutable heap state and program retirement are still exercised.
    fn expression(&mut self, compiled: &CompiledTurn) {
        let outcome = self
            .session
            .run_with_sites("residency_expression", compiled.code())
            .expect("prepared expression runs");
        assert!(
            matches!(outcome, ResidentOutcome::Completed { .. }),
            "{outcome:?}"
        );
    }

    fn bind(&mut self, text: &str) -> BoundBinder {
        let TurnResult::Bind {
            bound, compiled, ..
        } = self.compile(text)
        else {
            panic!("{text:?} did not classify as a bind");
        };
        let [binder] = bound.as_slice() else {
            panic!("{text:?} bound {} names", bound.len());
        };
        let outcome = self
            .session
            .run_bind_with_sites(
                "residency_bind",
                compiled.code(),
                binder,
                Generation(self.generation),
            )
            .unwrap_or_else(|error| panic!("{text:?} failed to run: {error}"));
        assert!(
            matches!(outcome, ResidentOutcome::Completed { .. }),
            "{text:?} did not complete: {outcome:?}"
        );
        self.injected.push(binder.module.clone());
        binder.clone()
    }
}

#[test]
fn structural_resume_classifies_rejection_and_consumed_failure() {
    use tidepool_bridge::{BridgeError, HaskellVisitor, ToHaskell};
    use tidepool_repr::DataConTable;
    use tidepool_runtime::session::{PreparedRuntimeError, ResidentError, ResidentResumeError};

    struct MalformedAnswer;
    impl tidepool_bridge::sealed::ToHaskellSealed for MalformedAnswer {}
    impl ToHaskell for MalformedAnswer {
        fn visit(
            &self,
            table: &DataConTable,
            visitor: &mut dyn HaskellVisitor,
        ) -> Result<(), BridgeError> {
            let unit = tidepool_bridge::get_qualified(table, "GHC.Tuple.()", 0)
                .ok_or_else(|| BridgeError::UnknownDataConName("GHC.Tuple.()".into()))?;
            visitor.begin_constructor(unit, 0)?;
            Err(BridgeError::UnknownDataConName(
                "missing response metadata".into(),
            ))
        }
    }

    let mut notebook = Notebook::with_effects(&[tidepool_mcp::console_decl()]);
    let success = notebook.prepare_expression("say \"pause\" >> pure (42 :: Int)");
    let ResidentOutcome::Suspended { hole, .. } = notebook
        .session
        .run_with_sites("response_rejection", success.code())
        .unwrap()
    else {
        panic!("Console request must suspend");
    };
    let roots = notebook.session.persistent_roots_count();
    let error = notebook
        .session
        .resume_classified(hole.clone(), MalformedAnswer)
        .unwrap_err();
    assert!(
        matches!(error, ResidentResumeError::Rejected(ResidentError::Prepared(
        PreparedRuntimeError::AnswerRejected { source: BridgeError::UnknownDataConName(ref name), .. }
    )) if name == "missing response metadata"),
        "{error:?}"
    );
    assert_eq!(notebook.session.parked_holes(), vec![hole.cont_id()]);
    assert_eq!(notebook.session.persistent_roots_count(), roots);
    assert!(matches!(
        notebook.session.resume_classified(hole, ()).unwrap(),
        ResidentOutcome::Completed { .. }
    ));
    assert!(notebook.session.parked_holes().is_empty());

    let failure =
        notebook.prepare_expression("say \"pause\" >> (error \"after response\" :: M Int)");
    let ResidentOutcome::Suspended { hole, .. } = notebook
        .session
        .run_with_sites("consumed_response", failure.code())
        .unwrap()
    else {
        panic!("Console request must suspend before its failure");
    };
    let error = notebook.session.resume_classified(hole, ()).unwrap_err();
    assert!(
        matches!(error, ResidentResumeError::Consumed(_)),
        "{error:?}"
    );
    assert!(notebook.session.parked_holes().is_empty());
    let ResidentOutcome::Suspended { hole, .. } = notebook
        .session
        .run_with_sites("after_consumed_failure", success.code())
        .unwrap()
    else {
        panic!("the session must remain usable after a consumed failure");
    };
    assert!(matches!(
        notebook.session.resume_classified(hole, ()).unwrap(),
        ResidentOutcome::Completed { .. }
    ));
}

#[test]
fn custody_resume_classifies_rejected_frame_and_consumed_failure() {
    use tidepool_runtime::session::ResidentResumeError;

    let mut notebook = Notebook::with_effects(&[tidepool_mcp::console_decl()]);
    notebook.bind("held <- pure ()");
    let success = notebook.prepare_expression("say \"pause\" >> pure (42 :: Int)");
    let ResidentOutcome::Suspended { hole, .. } = notebook
        .session
        .run_with_sites("framed_rejection", success.code())
        .unwrap()
    else {
        panic!("Console request must suspend");
    };
    let held = notebook.session.prepared_binding_handle("held").unwrap();
    let error = notebook
        .session
        .resume_framed_custody_classified(
            hole.clone(),
            &held,
            tidepool_repr::DataConId(u64::MAX),
            Vec::new(),
        )
        .unwrap_err();
    assert!(
        matches!(error, ResidentResumeError::Rejected(_)),
        "{error:?}"
    );
    assert_eq!(notebook.session.parked_holes(), vec![hole.cont_id()]);
    assert!(matches!(
        notebook
            .session
            .resume_handle_classified(hole, held)
            .unwrap(),
        ResidentOutcome::Completed { .. }
    ));

    let failure = notebook.prepare_expression("say \"pause\" >> (error \"after handle\" :: M Int)");
    let ResidentOutcome::Suspended { hole, .. } = notebook
        .session
        .run_with_sites("consumed_handle", failure.code())
        .unwrap()
    else {
        panic!("Console request must suspend before its failure");
    };
    let held = notebook.session.prepared_binding_handle("held").unwrap();
    let error = notebook
        .session
        .resume_handle_classified(hole, held)
        .unwrap_err();
    assert!(
        matches!(error, ResidentResumeError::Consumed(_)),
        "{error:?}"
    );
    assert!(notebook.session.parked_holes().is_empty());
}

/// Fixture mirror of `PreparedEngine::MAJOR_COLLECTION_INSTALL_INTERVAL`
/// (`tidepool/runtime/src/session/prepared.rs`) -- see the module doc for
/// why this file cannot read the real constant.
const N: usize = 4;

/// Assert every `ResidencyCounts` field, plus `old_bytes`, is identical to
/// `previous` -- the flatness a completed major collection should leave
/// behind, since it folds every earlier collection boundary's counters back
/// to the same live set (no live bindings changed in between).
fn assert_counts_flat(
    label: &str,
    counts: tidepool_codegen::prepared_program::ResidencyCounts,
    previous: tidepool_codegen::prepared_program::ResidencyCounts,
    old_bytes: usize,
    previous_old_bytes: usize,
) {
    assert_eq!(counts, previous, "{label}: counts moved across collections");
    assert_eq!(
        old_bytes, previous_old_bytes,
        "{label}: old_bytes moved across collections"
    );
}

/// Twenty installations of one prepared constant expression on the prepared route, each installing
/// exactly one program: `quiesce_and_collect` only actually collects every
/// `N`th install (see the module doc), so `programs` is checked against the
/// amortized bound every turn, and exact flatness across every counter --
/// `old_bytes` included -- is only checked collection-boundary to
/// collection-boundary (every `N`th turn). The first boundary is a warm-up:
/// the first few installs may still be settling machine bootstrap/site-index
/// bookkeeping, and descriptor-arena compaction commits before retirement
/// per the lifetime contract's decision 7, so a retiring program's bytes can
/// still be one collection behind its other counters at that point.
/// Comparisons start at the second boundary, once both have stabilized.
///
/// A binding is then introduced (`x <- pure 1`) and twelve more expression
/// turns run importing nothing new: the same amortized-bound-every-turn,
/// flat-at-every-boundary structure applies, now against the post-bind
/// baseline -- the bound bind's own program does not accumulate one
/// retained program per subsequent collection.
#[test]
fn prepared_session_residency_stays_bounded_across_many_turns() {
    let mut notebook = Notebook::new();

    // A multiple of `N`, so the phase ends exactly on a collection boundary
    // and the bind below is the first install of the next window.
    const TOTAL: usize = 20;
    let live_bindings = 0;
    let mut boundary_counts: Option<tidepool_codegen::prepared_program::ResidencyCounts> = None;
    let mut boundary_old_bytes: Option<usize> = None;
    let mut boundaries_seen = 0;
    let expression = notebook.prepare_expression("1 + (2 :: Int)");
    for i in 0..TOTAL {
        notebook.expression(&expression);
        let counts = notebook
            .session
            .residency()
            .expect("the prepared route reports residency counts");
        assert!(
            counts.programs <= live_bindings + 1 + N,
            "turn {i}: programs={} exceeds the amortized bound ({live_bindings} live bindings + 1 + N)",
            counts.programs
        );
        // A collection boundary: `installs_since_major` (turn number, since
        // each turn installs exactly one program) hit the window.
        if (i + 1) % N == 0 {
            boundaries_seen += 1;
            let old_bytes = notebook
                .session
                .old_bytes()
                .expect("the prepared route reports old-space bytes");
            if boundaries_seen > 1 {
                assert_counts_flat(
                    &format!("turn {i} (boundary {boundaries_seen})"),
                    counts,
                    boundary_counts.expect("a prior boundary was recorded"),
                    old_bytes,
                    boundary_old_bytes.expect("a prior boundary was recorded"),
                );
            }
            boundary_counts = Some(counts);
            boundary_old_bytes = Some(old_bytes);
        }
    }
    assert!(
        boundaries_seen >= 2,
        "expected at least two collection boundaries in {TOTAL} turns at N={N}"
    );

    // Introduce a live binding, then keep running: the bound value's
    // producing program may or may not stay resident (it depends on whether
    // the binding's value keeps any of its code reachable), but no further
    // program should accumulate collection over collection.
    notebook.bind("x <- pure 1");
    let live_bindings = 1;

    const POST_BIND: usize = 12;
    let mut boundary_counts: Option<tidepool_codegen::prepared_program::ResidencyCounts> = None;
    let mut boundary_old_bytes: Option<usize> = None;
    let mut boundaries_seen = 0;
    let expression = notebook.prepare_expression("x + (2 :: Int)");
    for i in 0..POST_BIND {
        notebook.expression(&expression);
        let counts = notebook
            .session
            .residency()
            .expect("the prepared route reports residency counts");
        assert!(
            counts.programs <= live_bindings + 1 + N,
            "post-bind turn {i}: programs={} exceeds the amortized bound ({live_bindings} live binding + 1 + N)",
            counts.programs
        );
        // The bind turn installed one program right after the pre-bind
        // phase's final boundary, so this window's installs run one ahead
        // of the turn index: the collection lands where `i + 2` fills it.
        if (i + 2) % N == 0 {
            boundaries_seen += 1;
            let old_bytes = notebook
                .session
                .old_bytes()
                .expect("the prepared route reports old-space bytes");
            if boundaries_seen > 1 {
                assert_counts_flat(
                    &format!("post-bind turn {i} (boundary {boundaries_seen})"),
                    counts,
                    boundary_counts.expect("a prior boundary was recorded"),
                    old_bytes,
                    boundary_old_bytes.expect("a prior boundary was recorded"),
                );
            }
            boundary_counts = Some(counts);
            boundary_old_bytes = Some(old_bytes);
        }
    }
    assert!(
        boundaries_seen >= 2,
        "expected at least two post-bind collection boundaries in {POST_BIND} turns at N={N}"
    );
}

/// A large promotion trips `PreparedEngine::major_collection_due`'s live
/// old-space growth check (`tidepool/runtime/src/session/prepared.rs`)
/// well before the install-count window (`N`) would close on its own.
///
/// The growth check is disabled until a real baseline exists (the very
/// first major collection is always gated by the install count alone --
/// see that function's doc), so this first runs exactly `N` tiny turns to
/// land on a collection boundary and establish a nonzero `old_bytes`
/// baseline. The bind below then FORCES a 200,000-element list's whole
/// spine (`length ys \`seq\` ys`) before returning it, so the list is
/// actually built rather than left as an unevaluated `enumFromTo` thunk --
/// tenured prepared bindings are kept exactly as given, never deep-forced
/// (`ResidentSession::current_binding_in`'s doc), so an unforced `pure
/// [1..200000]` binds a thunk of a few words and never promotes anything.
/// The forced list is well past the 1 MiB / 50% growth threshold; a
/// following turn or two should fold that promotion into `old_bytes` even
/// though only one or two programs have installed since the baseline --
/// far short of `N`.
#[test]
fn prepared_session_large_promotion_triggers_an_early_major_collection() {
    let mut notebook = Notebook::new();

    let expression = notebook.prepare_expression("1 + (2 :: Int)");
    for _ in 0..N {
        notebook.expression(&expression);
    }
    let old_bytes_before = notebook
        .session
        .old_bytes()
        .expect("the prepared route reports old-space bytes");

    notebook.bind("xs <- (let ys = [1..200000 :: Int] in seq (length ys) (pure ys))");

    let mut collected_within = None;
    for i in 0..2 {
        notebook.expression(&expression);
        let old_bytes_now = notebook
            .session
            .old_bytes()
            .expect("the prepared route reports old-space bytes");
        if old_bytes_now >= old_bytes_before + 1024 * 1024 {
            collected_within = Some(i + 1);
            break;
        }
    }
    assert!(
        collected_within.is_some(),
        "expected a major collection to fold the large list's promoted bytes into `old_bytes` \
         within 2 turns of binding it (stayed at {old_bytes_before}); the live old-space growth \
         trigger in `PreparedEngine::major_collection_due` should fire well before the next \
         install-count boundary at N={N}"
    );
}

#[test]
fn fresh_host_binders_preserve_prior_request_input_for_captured_closures() {
    use tidepool_codegen::scope::ScopeId;

    let mut notebook = Notebook::new();
    let mount = |notebook: &mut Notebook, payload: serde_json::Value| {
        let TurnResult::Bind {
            bound, compiled, ..
        } = notebook.compile_in_current_value_view(
            "input <- pure (object [\"anchor\" .= toJSON [Aeson.String \"\", Aeson.Number (Aeson.scientific 0 0), Aeson.Bool True, Aeson.Null]])",
        )
        else {
            panic!("input interface must compile as a bind");
        };
        let [binder] = bound.as_slice() else {
            panic!("input interface must produce exactly one binder");
        };
        let constructors = compiled
            .table
            .iter()
            .filter_map(|con| con.qualified_name.clone())
            .collect::<Vec<_>>();
        notebook
            .session
            .mount_json_binding_in(
                ScopeId::ROOT,
                binder,
                Generation(notebook.generation),
                compiled.into_code(),
                &payload,
            )
            .unwrap_or_else(|error| {
                panic!(
                    "mount compiler-authenticated JSON input ({binder:?}; {constructors:?}): {error}"
                )
            });
    };

    mount(&mut notebook, serde_json::json!({"request": "A"}));

    let TurnResult::Bind {
        bound, compiled, ..
    } = notebook.compile_in_current_value_view(
        "fromA <- pure (\\() -> case input of { Aeson.Object fields -> if Map.member \"request\" fields then 11 :: Int else 0; _ -> 0 })",
    )
    else {
        panic!("request-A closure must compile as a bind");
    };
    let [from_a] = bound.as_slice() else {
        panic!("request-A closure must produce exactly one binder");
    };
    let outcome = notebook
        .session
        .run_bind_with_sites(
            "request_a_capture",
            compiled.into_code(),
            from_a,
            Generation(notebook.generation),
        )
        .expect("capture request-A input in closure");
    assert!(matches!(outcome, ResidentOutcome::Completed { .. }));

    mount(&mut notebook, serde_json::json!({"request": "B"}));
    assert_eq!(notebook.session.val_gen(), Generation(notebook.generation));

    let TurnResult::Expr { compiled, .. } = notebook.compile_in_current_value_view("fromA ()")
    else {
        panic!("request-A closure invocation must compile as an expression");
    };
    let outcome = notebook
        .session
        .run_with_sites("request_a_snapshot", compiled.into_code())
        .expect("request-A closure remains runnable after request-B mount");
    let ResidentOutcome::Completed { result, .. } = outcome else {
        panic!("request-A closure invocation did not complete: {outcome:?}");
    };
    assert_eq!(result.to_json(), serde_json::json!([11, "11"]));
}

#[test]
fn rust_session_var_id_matches_extract_minted_bound_binder_var_id() {
    let mut notebook = Notebook::new();
    let binder = notebook.bind("parityVal <- pure (1 :: Int)");
    let expected = tidepool_codegen::prepared_program::session_var_id(&binder.module, &binder.name);
    assert_eq!(
        expected, binder.var_id,
        "Rust-minted session_var_id({:?}, {:?}) = 0x{expected:016x} does not match \
         the extract-minted BoundBinder.var_id 0x{:016x}",
        binder.module, binder.name, binder.var_id
    );
}

#[test]
fn host_carrier_mounts_json_text_and_job_payloads_from_one_compile_each() {
    use tidepool_codegen::scope::ScopeId;
    use tidepool_runtime::session::{HostBindingType, HostCarrier, HostPayload};

    #[derive(tidepool_bridge_derive::ToHaskell)]
    enum TestCommandJob {
        #[haskell(module = "Tidepool.Command.Types", name = "Job")]
        Job(String),
    }

    let mut notebook = Notebook::new();
    // Host-carrier stub modules are hand-written source, found by GHC's
    // ordinary downsweep on the session root -- unlike a real bind's `.hi`
    // (injected explicitly), the root itself must be on the include path.
    notebook.include.push(notebook.root.path().to_path_buf());

    // Extra qualified imports the Text/Job anchors and their read-back
    // turns need, beyond the default preamble.
    let extra_imports = "qualified Data.Text as TidepoolHostText\n\
                          qualified Data.Text.Internal as TidepoolHostTextInternal\n\
                          qualified GHC.Exts as TidepoolHostExts\n\
                          qualified Tidepool.Command.Types as TidepoolHostJob\n";

    // Compile against the session's current value view PLUS the extra
    // imports above -- the same shape as `Notebook::compile_in_current_value_view`,
    // duplicated here because that helper has no hook for extra imports.
    let compile_extra = |notebook: &mut Notebook, text: &str| -> TurnResult {
        notebook.generation += 1;
        let mut imports = notebook
            .session
            .current_val_modules()
            .into_iter()
            .map(|module| format!("{module}\n"))
            .collect::<String>();
        imports.push_str(extra_imports);
        let templates =
            resident_workbench_templates(&notebook.preamble, &notebook.effect_stack, &imports);
        let include: Vec<&Path> = notebook.include.iter().map(PathBuf::as_path).collect();
        let injected = notebook.session.inject_val_modules();
        let retained = notebook.session.prepared_retained();
        run_turn(TurnRequest {
            turn_text: text,
            templates: &templates,
            include: &include,
            session_root: notebook.root.path(),
            inject_modules: &injected,
            gen: notebook.generation,
            verdict: None,
            target: None,
            retained_imports: &retained,
        })
        .unwrap_or_else(|failure| {
            panic!(
                "{text:?} failed to compile: {}\n{}",
                tidepool_runtime::classify_compile(&failure.error).message,
                failure
                    .attempted_source
                    .as_deref()
                    .unwrap_or("<no attempted source>")
            )
        })
    };

    // --- one anchor compile per host-binding kind ---------------------

    let TurnResult::Bind {
        bound, compiled, ..
    } = notebook.compile_in_current_value_view(
        "carrierAnchor <- pure (object [\"anchor\" .= toJSON [Aeson.String \"\", Aeson.Number (Aeson.scientific 0 0), Aeson.Bool True, Aeson.Null]])",
    ) else {
        panic!("json anchor must compile as a bind");
    };
    let [json_anchor_binder] = bound.as_slice() else {
        panic!("json anchor must produce exactly one binder");
    };
    let json_carrier = HostCarrier::from_compiled(
        json_anchor_binder,
        compiled.code(),
        HostBindingType::JSON_VALUE,
    );

    let TurnResult::Bind {
        bound, compiled, ..
    } = compile_extra(
        &mut notebook,
        "textAnchor <- pure (case TidepoolHostExts.noinline (TidepoolHostText.pack \"\") of \
         TidepoolHostTextInternal.Text bytes offset length -> \
         TidepoolHostTextInternal.Text bytes offset length)",
    )
    else {
        panic!("text anchor must compile as a bind");
    };
    let [text_anchor_binder] = bound.as_slice() else {
        panic!("text anchor must produce exactly one binder");
    };
    let text_carrier =
        HostCarrier::from_compiled(text_anchor_binder, compiled.code(), HostBindingType::TEXT);

    let TurnResult::Bind {
        bound, compiled, ..
    } = compile_extra(
        &mut notebook,
        "jobAnchor <- pure (TidepoolHostJob.Job (case TidepoolHostExts.noinline (TidepoolHostText.pack \"\") of \
         TidepoolHostTextInternal.Text bytes offset length -> \
         TidepoolHostTextInternal.Text bytes offset length))",
    ) else {
        panic!("job anchor must compile as a bind");
    };
    let [job_anchor_binder] = bound.as_slice() else {
        panic!("job anchor must produce exactly one binder");
    };
    let job_carrier = HostCarrier::from_compiled(
        job_anchor_binder,
        compiled.code(),
        HostBindingType::COMMAND_JOB,
    );

    // --- mount TWO json payloads through the SAME carrier, plus one text
    // and one job payload, all with NO further GHC compile -------------

    let gen_a = tidepool_repr::Generation(notebook.generation);
    let binder_a = notebook
        .session
        .mount_carrier_in(
            notebook.root.path(),
            ScopeId::ROOT,
            "carriedA",
            gen_a,
            &json_carrier,
            HostPayload::Json(&serde_json::json!({"tag": "A"})),
        )
        .expect("mount first carrier payload");
    notebook.injected.push(binder_a.module.clone());

    notebook.generation += 1;
    let gen_b = tidepool_repr::Generation(notebook.generation);
    let binder_b = notebook
        .session
        .mount_carrier_in(
            notebook.root.path(),
            ScopeId::ROOT,
            "carriedB",
            gen_b,
            &json_carrier,
            HostPayload::Json(&serde_json::json!({"tag": "B"})),
        )
        .expect("mount second carrier payload");
    notebook.injected.push(binder_b.module.clone());

    notebook.generation += 1;
    let gen_text = tidepool_repr::Generation(notebook.generation);
    let binder_text = notebook
        .session
        .mount_carrier_in(
            notebook.root.path(),
            ScopeId::ROOT,
            "carriedText",
            gen_text,
            &text_carrier,
            HostPayload::Text("hello-carrier-text"),
        )
        .expect("mount text carrier payload");
    notebook.injected.push(binder_text.module.clone());

    notebook.generation += 1;
    let gen_job = tidepool_repr::Generation(notebook.generation);
    let binder_job = notebook
        .session
        .mount_carrier_in(
            notebook.root.path(),
            ScopeId::ROOT,
            "carriedJob",
            gen_job,
            &job_carrier,
            HostPayload::Job(&TestCommandJob::Job("job-command".to_string())),
        )
        .expect("mount job carrier payload");
    notebook.injected.push(binder_job.module.clone());

    // Every stub generation must never be offered as --inject-val (no .hi
    // exists for any of them) -- the whole point of a stub mount.
    let injected = notebook.session.inject_val_modules();
    assert!(!injected.contains(&binder_a.module));
    assert!(!injected.contains(&binder_b.module));
    assert!(!injected.contains(&binder_text.module));
    assert!(!injected.contains(&binder_job.module));

    // A later turn reads all four mounted values back through their
    // hand-written stub modules, with NO further GHC compile for any of
    // the four mounts above -- and, crucially, by PATTERN MATCHING their
    // constructors (the `GHC.Magic.lazy`-wrapped stub body must not earn a
    // bottoming strictness signature that would collapse these cases; see
    // `HostCarrier::stub_source`'s doc comment for the failure this guards
    // against).
    let TurnResult::Expr { compiled, .. } = compile_extra(
        &mut notebook,
        "case (carriedA, carriedB, carriedText, carriedJob) of \
         { (Aeson.Object a, Aeson.Object b, TidepoolHostTextInternal.Text _ _ _, TidepoolHostJob.Job jobText) -> \
         (Map.lookup \"tag\" a, Map.lookup \"tag\" b, \
          carriedText == TidepoolHostText.pack \"hello-carrier-text\", \
          jobText == TidepoolHostText.pack \"job-command\") }",
    ) else {
        panic!("reading all four carried values must compile as an expression");
    };
    let outcome = notebook
        .session
        .run_with_sites("read_all_carried", compiled.into_code())
        .expect("read all carried values");
    let ResidentOutcome::Completed { result, .. } = outcome else {
        panic!("reading carried values did not complete: {outcome:?}");
    };
    assert_eq!(
        result.to_json(),
        serde_json::json!([
            ["A", "B", true, true],
            "(Just (String \"A\"),Just (String \"B\"),True,True)"
        ]),
        "expected all four carrier-mounted payloads to read back correctly \
         through their stub modules, including constructor pattern matches"
    );
}

/// HIGH regression: retiring a carrier-mounted binding after a declaration
/// captured its name must delete the binding's `Val.G<g>` stub source (not
/// just stop excluding it from `--inject-val`), so a later turn that still
/// imports it through the declaration module fails to find the module
/// instead of silently compiling and evaluating the stub's own
/// self-referential body. Bounded by a timeout: a regression here is an
/// infinite loop at evaluation time, and this test must fail loudly rather
/// than hang the suite.
#[test]
fn retired_carrier_binding_captured_by_declaration_fails_to_compile_not_hang() {
    use std::sync::mpsc;
    use std::time::Duration;
    use tidepool_codegen::scope::ScopeId;
    use tidepool_runtime::session::{HostBindingType, HostCarrier, HostPayload};

    let mut notebook = Notebook::with_shared_root(&[]);

    let TurnResult::Bind {
        bound, compiled, ..
    } = notebook.compile_in_current_value_view(
        "carrierAnchor <- pure (object [\"anchor\" .= toJSON [Aeson.String \"\"]])",
    )
    else {
        panic!("anchor must compile as a bind");
    };
    let [anchor_binder] = bound.as_slice() else {
        panic!("anchor must produce exactly one binder");
    };
    let carrier =
        HostCarrier::from_compiled(anchor_binder, compiled.code(), HostBindingType::JSON_VALUE);

    notebook.generation += 1;
    let gen = Generation(notebook.generation);
    let binder = notebook
        .session
        .mount_carrier_in(
            notebook.root.path(),
            ScopeId::ROOT,
            "capturedCarrier",
            gen,
            &carrier,
            HostPayload::Json(&serde_json::json!({"tag": "captured"})),
        )
        .expect("mount carrier binding");
    notebook.injected.push(binder.module.clone());

    let stub_path = notebook
        .root
        .path()
        .join(tidepool_repr::SessionModule::val(gen).relative_hs_path());
    assert!(stub_path.exists(), "mount must write its stub source");

    // A declaration captures the carrier binding by name -- `define_scoped`
    // auto-imports every currently-visible value's exact `Val.G<g>` module,
    // so the committed `Lib.G<g>` source now names this stub generation.
    notebook
        .session
        .define_scoped(&["capturedUse = capturedCarrier"])
        .expect("declaration captures the carrier binding");

    // Retire the carrier binding: it must both stop being excluded from
    // `--inject-val` bookkeeping under its own generation AND delete its
    // stub source, matching a real binding's eviction instead of leaving a
    // phantom stub the declaration module can still find.
    notebook
        .session
        .retire_host_binding_owner(notebook.root.path(), &binder);
    assert!(
        !stub_path.exists(),
        "retiring a captured carrier binding must delete its stub source"
    );

    // A later turn using the declaration must fail loudly with a compile
    // error -- never hang evaluating the retired stub's self-referential
    // body. `compile_in_current_value_view` panics on its own compile
    // failure, so the expected outcome is the compile thread panicking, not
    // returning a successful classification.
    let (tx, rx) = mpsc::channel();
    let handle = std::thread::spawn(move || {
        let result = notebook.compile_in_current_value_view("capturedUse");
        tx.send(result).ok();
    });
    match rx.recv_timeout(Duration::from_secs(120)) {
        Ok(result) => panic!(
            "a turn referencing a retired carrier binding through a declaration must fail to \
             compile, not succeed: {result:?}"
        ),
        Err(mpsc::RecvTimeoutError::Timeout) => panic!(
            "compiling a turn that references a retired carrier binding through a declaration \
             hung instead of failing with a compile error"
        ),
        Err(mpsc::RecvTimeoutError::Disconnected) => {
            // The compile thread panicked before sending a result --
            // `compile_in_current_value_view`'s own `unwrap_or_else(panic!)`
            // on a real compile failure, exactly the expected outcome.
            handle.join().expect_err(
                "the compile thread must have panicked from a compile failure, not from \
                 something else",
            );
        }
    }
}

/// MEDIUM regression: `mount_carrier_in` writes its stub source before
/// mounting the host value. A payload that does not match the carrier's own
/// compiled type fails inside the mount step, AFTER the stub is already on
/// disk -- the write, mark, and mount must be one transaction, so a failed
/// mount leaves no stub behind for a later turn to stumble over.
#[test]
fn failed_carrier_mount_leaves_no_stub_source() {
    use tidepool_bridge::{BridgeError, HaskellVisitor, ToHaskell};
    use tidepool_codegen::scope::ScopeId;
    use tidepool_repr::DataConTable;
    use tidepool_runtime::session::{HostBindingType, HostCarrier, HostPayload};

    /// A payload whose `ToHaskell::visit` always fails -- forces
    /// `mount_host_value_in` to fail deep inside the build step, AFTER
    /// `mount_carrier_in` has already written the stub source to disk.
    struct AlwaysFailsToHaskell;
    impl tidepool_bridge::sealed::ToHaskellSealed for AlwaysFailsToHaskell {}
    impl ToHaskell for AlwaysFailsToHaskell {
        fn visit(
            &self,
            _table: &DataConTable,
            _visitor: &mut dyn HaskellVisitor,
        ) -> Result<(), BridgeError> {
            Err(BridgeError::UnknownDataConName(
                "deliberately unresolvable".into(),
            ))
        }
    }

    let mut notebook = Notebook::with_shared_root(&[]);

    let TurnResult::Bind {
        bound, compiled, ..
    } = notebook.compile_in_current_value_view(
        "carrierAnchor <- pure (object [\"anchor\" .= toJSON [Aeson.String \"\"]])",
    )
    else {
        panic!("anchor must compile as a bind");
    };
    let [anchor_binder] = bound.as_slice() else {
        panic!("anchor must produce exactly one binder");
    };
    let carrier =
        HostCarrier::from_compiled(anchor_binder, compiled.code(), HostBindingType::JSON_VALUE);

    notebook.generation += 1;
    let gen = Generation(notebook.generation);
    let error = notebook
        .session
        .mount_carrier_in(
            notebook.root.path(),
            ScopeId::ROOT,
            "unmountable",
            gen,
            &carrier,
            HostPayload::Job(&AlwaysFailsToHaskell),
        )
        .expect_err("a payload that fails to visit must fail to mount");

    let stub_path = notebook
        .root
        .path()
        .join(tidepool_repr::SessionModule::val(gen).relative_hs_path());
    assert!(
        !stub_path.exists(),
        "a failed carrier mount must not leave its stub source on disk (mount error: {error})"
    );
}

/// MEDIUM regression: a fresh `PersistentSession` reopening a session root
/// resets `val_gen`/`stub_generations`, so a leftover stub `.hs` from a
/// prior incarnation could coexist with a REAL, `.hi`-backed bind that
/// reissues its generation number. `SessionLib::open` must sweep every
/// stale stub source under the root's `Tidepool/Session/Val` so none can be
/// found by the new incarnation.
#[test]
fn reopening_session_root_removes_stale_stub_sources() {
    let root = tempfile::tempdir().expect("session root");
    let stub_dir = root.path().join("Tidepool/Session/Val");
    std::fs::create_dir_all(&stub_dir).expect("stub directory");
    let stale_stub = stub_dir.join("G3.hs");
    std::fs::write(
        &stale_stub,
        "module Tidepool.Session.Val.G3 (leftover) where\n\
         leftover :: ()\n\
         leftover = ()\n",
    )
    .expect("write stale stub");
    assert!(stale_stub.exists());

    SessionLib::open(
        tidepool_repr::SessionId(1),
        root.path(),
        ModuleEnv::standalone_default(),
    )
    .expect("reopen session root");

    assert!(
        !stale_stub.exists(),
        "opening a session must sweep stale stub sources left by a prior incarnation"
    );
}
