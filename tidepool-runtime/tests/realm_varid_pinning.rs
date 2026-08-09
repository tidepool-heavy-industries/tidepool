//! Lane C — the Item-2 VarId-keyed cross-realm isolation property, pinned
//! through the REAL production entry point (verdict §7 step 5,
//! `realm-checklist.md` Item 2).
//!
//! `tidepool-codegen/tests/binding_table_realm_isolation.rs` proves the
//! property against a hand-wired `BindingTable` — useful, but not proof that
//! any production caller computes the right `referenced` slice. This test
//! drives a real session through `ResidentSession::run`/`run_bind` — the ONLY
//! production callers of `tidepool_repr::free_vars::free_vars(expr)` followed
//! by `seed_external_env(&referenced)` — so `referenced` is computed by
//! production code, not chosen by the test.
//!
//! Setup: bind the SAME display name ("x") twice as two independent
//! value-plane scopes, each a real `run_bind` turn compiled through the
//! session-aware extract path (`--session-bind`), so each mints its own
//! fresh, extract-sourced `SessionVarId` (never picked by the test). A third
//! turn references "x" AFTER both binds — by construction (`current` is
//! last-bind-wins) this resolves to scope B's binding — compiled and run
//! through `ResidentSession::run`. The env that fragment would be compiled
//! against is captured via `ResidentSession::seed_external_env_for`, which
//! `run`/`run_bind` themselves call on the way to `add_fragment_session` — so
//! the asserted env IS the one a fragment compiles against, not a
//! reconstruction that could drift from it. It must contain scope B's
//! `SessionVarId` and NOT scope A's.
//!
//! GHC-heavy tier — needs `TIDEPOOL_EXTRACT` and `--ignore-default-filter`.

use std::path::Path;

use tidepool_effect::dispatch::{DispatchEffect, EffectContext};
use tidepool_effect::error::EffectError;
use tidepool_effect::Response;
use tidepool_eval::value::Value;
use tidepool_repr::SessionVarId;
use tidepool_runtime::session::{
    compile_session_turn, ResidentOutcome, ResidentSession, SessionBind,
};
use tidepool_runtime::DEFAULT_NURSERY_SIZE;
use tidepool_testing::eval_harness::{self, mock, EvalHarness};

// ---------------------------------------------------------------------------
// Test sink + handler-stack adapter — mirrors `resident_session.rs`, kept
// self-contained here rather than shared (this lane edits no pre-existing
// test file).
// ---------------------------------------------------------------------------

#[derive(Clone, Default)]
struct TestSink {
    lines: std::sync::Arc<std::sync::Mutex<Vec<String>>>,
}

impl tidepool_runtime::session::OutputSink for TestSink {
    fn drain(&self) -> Vec<String> {
        std::mem::take(&mut *self.lines.lock().unwrap())
    }
    fn snapshot(&self) -> Vec<String> {
        self.lines.lock().unwrap().clone()
    }
}

/// Adapt a `DispatchEffect<()>` handler stack (`mock::min_stack`) to the
/// `DispatchEffect<TestSink>` a resident session drives.
struct AsSink<H>(H);

impl<H: DispatchEffect<()>> DispatchEffect<TestSink> for AsSink<H> {
    fn dispatch(
        &mut self,
        tag: u64,
        request: &Value,
        cx: &EffectContext<'_, TestSink>,
    ) -> Result<Response, EffectError> {
        let unit_cx = EffectContext::with_user(cx.table(), &());
        self.0.dispatch(tag, request, &unit_cx)
    }
}

fn ask_tag() -> u64 {
    mock::EFFECT_NAMES
        .iter()
        .position(|&n| n == "Ask")
        .expect("mock::EFFECT_NAMES always contains Ask") as u64
}

fn setup() -> Option<EvalHarness> {
    if !eval_harness::extract_available() {
        eprintln!("Skipping: tidepool-extract toolchain not available (run inside `nix develop`)");
        return None;
    }
    Some(EvalHarness::new().with_stdlib())
}

/// Compile a plain (non-session-aware) turn — used only for the bootstrap
/// seed, which is compiled but never run (it just seeds the machine's
/// ConTags).
fn compile_turn(
    harness: &EvalHarness,
    body: &str,
) -> (tidepool_repr::CoreExpr, tidepool_repr::DataConTable) {
    let source = mock::mcp_module(body);
    let compiled = harness
        .compile(&source, "result")
        .unwrap_or_else(|e| panic!("compile failed for turn body {body:?}: {e}"));
    (compiled.expr, compiled.table)
}

/// [`mock::MCP_PREAMBLE`] with extra `import` lines spliced right before its
/// `default (Int, Text)` decl (every Haskell import must precede all other
/// top-level declarations), followed by `body`.
fn mcp_module_with_imports(imports: &[String], body: &str) -> String {
    let marker = "default (Int, Text)";
    let idx = mock::MCP_PREAMBLE
        .find(marker)
        .expect("mock::MCP_PREAMBLE carries the `default (Int, Text)` marker");
    let mut out = String::new();
    out.push_str(&mock::MCP_PREAMBLE[..idx]);
    for m in imports {
        out.push_str("import ");
        out.push_str(m);
        out.push('\n');
    }
    out.push_str(&mock::MCP_PREAMBLE[idx..]);
    out.push('\n');
    out.push_str(body);
    out.push('\n');
    out
}

/// A session-aware BIND turn's wrapped source: run the statement, then yield
/// the bound name — the same shape `tidepool-harness`'s
/// `template_session_bind` produces, targeting the `__result` binder
/// `compile_session_turn` expects.
fn bind_source(literal: i64) -> String {
    mock::mcp_module(&format!(
        "__result :: M Int\n__result = do {{\n  x <- pure ({literal} :: Int)\n ; pure x\n}}\n"
    ))
}

fn bootstrap(
    harness: &EvalHarness,
) -> ResidentSession<AsSink<impl DispatchEffect<()> + Send>, TestSink> {
    let (expr, table) = compile_turn(harness, "result :: M Int\nresult = pure (0 :: Int)");
    let effect_names = mock::EFFECT_NAMES.iter().map(|s| s.to_string()).collect();
    ResidentSession::bootstrap(
        &expr,
        table,
        AsSink(mock::min_stack()),
        ask_tag(),
        effect_names,
        TestSink::default(),
        Vec::new(),
        DEFAULT_NURSERY_SIZE,
        None,
    )
    .expect("bootstrap the resident machine")
}

#[test]
fn second_scope_fragment_env_excludes_first_scopes_session_var_id() {
    let Some(harness) = setup() else { return };
    let mut session = bootstrap(&harness);

    let session_root = tempfile::tempdir().expect("session root tempdir");
    let prelude_dir = eval_harness::prelude_path();
    let base_include: Vec<&Path> = vec![&prelude_dir];

    // ---- scope A: bind x = 41, a real session-aware bind turn ----
    let gen_a = session.val_gen().next();
    let names_a = vec!["x".to_string()];
    let src_a = bind_source(41);
    let compiled_a = compile_session_turn(
        &src_a,
        &base_include,
        session_root.path(),
        &[],
        Some(SessionBind {
            names: &names_a,
            gen: gen_a.0,
        }),
    )
    .expect("compile bind turn (scope A)");
    let binder_a = compiled_a
        .binders
        .into_iter()
        .next()
        .expect("scope A bind emitted a binder");
    match session
        .run_bind(
            "bind_a",
            &compiled_a.expr,
            &compiled_a.table,
            &binder_a,
            gen_a,
        )
        .expect("run bind turn (scope A)")
    {
        ResidentOutcome::Completed { .. } => {}
        ResidentOutcome::Suspended { .. } => panic!("a pure `pure 41` bind must not suspend"),
    }

    // ---- scope B: bind x = 99, an INDEPENDENT scope colliding on display
    // name with scope A (both live in the SAME session `BindingTable`, as two
    // realms sharing one table would) ----
    let gen_b = session.val_gen().next();
    let names_b = vec!["x".to_string()];
    let src_b = bind_source(99);
    let compiled_b = compile_session_turn(
        &src_b,
        &base_include,
        session_root.path(),
        &[],
        Some(SessionBind {
            names: &names_b,
            gen: gen_b.0,
        }),
    )
    .expect("compile bind turn (scope B)");
    let binder_b = compiled_b
        .binders
        .into_iter()
        .next()
        .expect("scope B bind emitted a binder");
    match session
        .run_bind(
            "bind_b",
            &compiled_b.expr,
            &compiled_b.table,
            &binder_b,
            gen_b,
        )
        .expect("run bind turn (scope B)")
    {
        ResidentOutcome::Completed { .. } => {}
        ResidentOutcome::Suspended { .. } => panic!("a pure `pure 99` bind must not suspend"),
    }

    let x_a = SessionVarId::from_extract(binder_a.var_id);
    let x_b = SessionVarId::from_extract(binder_b.var_id);
    assert_ne!(
        x_a, x_b,
        "two independent binds of the same display name must mint distinct \
         SessionVarIds (fresh-id minting is the other half of the isolation \
         property, alongside D9's narrowing)"
    );

    // ---- read turn: reference "x" AFTER both binds. `current` is
    // last-bind-wins, so this resolves to scope B's binding — via the
    // production caller's OWN computation of which Val modules to
    // import/inject (`ResidentSession::current_val_modules`/
    // `inject_val_modules`, the same accessors `tidepool-harness`'s
    // `session_bind_context` calls).
    let import_lines = session.current_val_modules();
    let inject_modules = session.inject_val_modules();
    let mut include_read = base_include.clone();
    include_read.push(session_root.path());
    let src_read = mcp_module_with_imports(&import_lines, "__result :: M Int\n__result = pure x\n");
    let compiled_read = compile_session_turn(
        &src_read,
        &include_read,
        session_root.path(),
        &inject_modules,
        None,
    )
    .expect("compile read turn");

    // Capture the SAME `ExternalEnv` `ResidentSession::run` would build
    // internally (`free_vars` then `seed_external_env`), via the narrow
    // accessor added for this purpose.
    let env = session.seed_external_env_for(&compiled_read.expr);
    assert!(
        env.get(x_b.var()).is_some(),
        "the read fragment references scope B's x — its SessionVarId must be seeded"
    );
    assert!(
        env.get(x_a.var()).is_none(),
        "scope A's x is NOT referenced by this fragment — its SessionVarId \
         must be ABSENT from the env (the cross-realm isolation property)"
    );

    // Functional sanity: the read actually resolves to scope B's value (99),
    // not scope A's (41) or garbage — proves the narrowed env still lets the
    // Var-miss resolve correctly, not just that it's narrow.
    match session
        .run("read_x", &compiled_read.expr, &compiled_read.table)
        .expect("run read turn")
    {
        ResidentOutcome::Completed { result, .. } => {
            assert_eq!(result.to_json(), serde_json::json!(99));
        }
        ResidentOutcome::Suspended { .. } => panic!("the read turn must not suspend"),
    }
}
