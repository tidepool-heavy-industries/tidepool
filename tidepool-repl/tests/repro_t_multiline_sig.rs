//! REPL type-query and effectful-value persistence regressions. Wide GHC type
//! renderings exercise bound-binder JSON escaping; the later cases exercise
//! stable effect rows and values across turn-module boundaries.

mod common;
use common::*;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn t_on_wide_multiline_signature_does_not_crash() {
    require_extract();
    let repl = Repl::new();

    // Same TYPE shape as the real repro (steer cascade: pure rule -> local
    // model -> suspend-to-human) — three curried function-typed arguments,
    // wide enough that GHC wraps the RENDERED type on `:t`. Source is
    // deliberately single-line: the wrap happens in `ppr`'s output of the
    // inferred `Type`, not from how the source itself was formatted (a
    // `Type` carries no source layout/comments), so a single-line source
    // avoids unrelated layout-rule edge cases while still exercising the bug.
    // Uses only Prelude types (not the session `M` monad, which decl modules
    // don't currently import — a separate, tracked gap) so this test isolates
    // the `:t`-wrap bug specifically.
    let decl = [
        "steerRepro :: (a -> Maybe b) -> (a -> Either String (Maybe b)) \
         -> (a -> Either String b) -> a -> Either String b",
        "steerRepro _ _ human x = human x",
    ]
    .join("\n");
    repl.def(&decl).await.expect_ok("def steerRepro");

    let t = repl.cmd(":t steerRepro").await;
    let out = t.expect_ok(":t steerRepro (must not crash on a wide wrapped type)");
    assert!(
        !out.contains("invalid bound-binder JSON") && !out.contains("control character"),
        ":t must not surface a JSON-parse error from an unescaped newline: {}",
        t.text
    );
    assert!(
        out.contains("->"),
        ":t steerRepro should report a function type: {}",
        t.text
    );
}

/// `:t` on an M-returning expression must report the authored spelling and
/// mutate nothing. Persistence normalizes `M` internally, but type display is
/// deliberately the original readable type.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn t_on_m_returning_helper_reports_authored_type() {
    require_extract();
    let repl = Repl::new();

    let decl = [
        "commitDeltaRepro :: Int -> M Int",
        "commitDeltaRepro x = pure x",
    ]
    .join("\n");
    repl.def(&decl).await.expect_ok("def commitDeltaRepro");

    let before = repl
        .cmd(":bindings")
        .await
        .expect_ok(":bindings before :t")
        .to_string();

    let t = repl.cmd(":t commitDeltaRepro").await;
    let out = t.expect_ok(":t commitDeltaRepro (M-returning expression must typecheck)");
    assert!(
        out.contains("->") && out.contains('M'),
        ":t commitDeltaRepro should report a function type mentioning M: {out}"
    );

    let after = repl
        .cmd(":bindings")
        .await
        .expect_ok(":bindings after :t")
        .to_string();
    assert_eq!(
        before, after,
        ":t must not mutate bindings or either logical generation: before={before} after={after}"
    );
}

/// A named value whose constructor contains an effectful closure persists and
/// can be invoked by a later turn. This specifically guards removal of the old
/// recursive policy walk: a stable product type is not rejected merely because
/// one of its fields mentions `Eff`.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn effectful_binds_persist_across_turns() {
    require_extract();
    let repl = Repl::new();

    repl.def(
        "data EffectBox = EffectBox { runEffectBox :: Int -> Eff '[Console, Ask, RunLLMTurn] Int }",
    )
    .await
    .expect_ok("define a product containing an exact-row closure");

    repl.cmd(
        "(step, box) <- putStrLn \"\" >> pure ((\\n -> (pure (n + 1) :: M Int)), EffectBox (\\n -> pure (n + 1)))",
    )
        .await
        .expect_ok("bind a direct and a nested effectful function");

    let later = repl.cmd("step 40 >>= runEffectBox box").await;
    let out = later.expect_ok("invoke both persisted effectful functions");
    assert!(
        out.contains("42"),
        "expected persisted closure result, got: {out}"
    );
}

/// Same friction, for a stdlib EFFECT VERB whose type is `Either <Err> T`
/// (`run :: forall effs. Member Exec effs => Text -> Eff effs (Either
/// ExecError Proc)`) — `ExecError` is defined in the STABLE
/// universal `Tidepool.Effects.Core` module, so `:t run`'s probe
/// bind must remain a valid type query. `run` is queried at the
/// session's own concrete `M` (`:t (run :: Text -> M (Either ExecError
/// Proc))`) rather than bare: bare `run` is a genuinely AMBIGUOUS type query
/// now that it is row-polymorphic (`Member Exec effs0` alone can never
/// default `effs0` — the same GHC defaulting gap `__anchor`
/// (`tidepool-mcp::eval_prep::TurnTemplate`) works around for `finalize`'s
/// free result type — an orthogonal, pre-existing `:t`-on-an-unapplied-
/// polymorphic-value limitation).
/// Needs the full
/// effect stack (Exec) rather than `Repl::new()`'s Console-only minimal one.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn t_on_either_returning_effect_verb_reports_type() {
    require_extract();
    let tmp = tempfile::tempdir().expect("tempdir");
    let repl = Repl {
        server: build_full_server(tmp.path().to_path_buf(), "t-either-verb", false),
    };

    let before = repl
        .cmd(":bindings")
        .await
        .expect_ok(":bindings before :t")
        .to_string();

    let t = repl
        .cmd(":t (run :: Text -> M (Either ExecError Proc))")
        .await;
    let out = t.expect_ok(":t run (Either-returning verb type must typecheck)");
    assert!(
        out.contains("Either"),
        ":t run should report an Either-shaped type: {out}"
    );

    let after = repl
        .cmd(":bindings")
        .await
        .expect_ok(":bindings after :t")
        .to_string();
    assert_eq!(
        before, after,
        ":t must not mutate bindings or either logical generation: before={before} after={after}"
    );
}

/// A generated effect-domain value persists and remains callable in a later
/// turn. Universal Core gives these types one nominal identity independent of
/// the executable row.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn either_returning_verb_bind_now_persists_across_turns() {
    require_extract();
    let tmp = tempfile::tempdir().expect("tempdir");
    let repl = Repl {
        server: build_full_server(tmp.path().to_path_buf(), "either-bind-persists", false),
    };

    let bind = repl.cmd("p <- run \"echo hi\"").await;
    let out = bind.expect_ok("a real `Either ExecError Proc` session bind must now succeed");
    assert!(
        out.contains("Either"),
        "the bound name's reported type should mention Either: {out}"
    );

    // Genuine cross-turn persistence: a LATER turn references `p` — not just
    // "the bind didn't error", but the bound value is a live name in scope.
    let later = repl
        .cmd("case p of { Right proc -> proc.exitCode; Left _ -> (-1) }")
        .await;
    let later_out = later.expect_ok("a later turn must be able to reference the earlier bind `p`");
    assert!(
        later_out.contains('0'),
        "expected the earlier `echo hi` bind's exitCode (0) to be reachable from a later turn: {later_out}"
    );
}
