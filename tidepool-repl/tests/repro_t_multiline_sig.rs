//! Regression guard: `:t` on a binding whose inferred type is wide enough
//! that GHC's `ppr` line-wraps it must not crash.
//!
//! Root cause (found 2026-06-30/07-01): `:t`'s bound-binder JSON is hand-built
//! by `renderBoundBindersJson` (`haskell/app/Main.hs`) with a hand-rolled
//! escaper that only escapes `"` and `\`. `renderType` (`GhcPipeline.hs`)
//! pretty-prints the type via `ppr` under `defaultSDocContext`'s default page
//! width, which inserts a real newline when the rendered type is wide — GHC's
//! `Type` carries no source comments, so the type's own width is what
//! triggers this, not the multi-line-with-Haddock source presentation (that
//! was the original but incorrect hypothesis). The raw `\n` byte then lands
//! unescaped inside a JSON string field, and the Rust side
//! (`tidepool-runtime/src/session/turn.rs`, a real `serde_json` parser)
//! rejects it with "invalid bound-binder JSON: control character...".
//!
//! Repro shape mirrors a real case found dogfooding the repl (`steer` in
//! `.tidepool/lib/Lsp.hs`) — three curried function-typed arguments, wide
//! enough that the rendered type wraps under default GHC pretty-printing.

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

/// Extracts the `"bindings"` array (as raw JSON text) from a `:bindings`
/// response — used to assert "nothing was registered" without also comparing
/// `generation`/`valGeneration` counters, which a `:t` probe legitimately
/// bumps (it consumes a throwaway generation to avoid an iface collision with
/// the NEXT real bind — see `Session::query_inner_type`'s doc).
fn bindings_only(s: &str) -> serde_json::Value {
    serde_json::from_str::<serde_json::Value>(s)
        .ok()
        .and_then(|v| v.get("bindings").cloned())
        .unwrap_or(serde_json::Value::Null)
}

/// Friction 2 (round-2 test-user report): `:t` on an M-returning expression
/// used to crash — the probe bind's captured type (`Int -> M Int`) mentions
/// the effect row, and the cross-row bind guard in `Main.mkBoundBinders`
/// (`typeMentionsEffectMonad`) rejected it exactly as it would a REAL
/// session bind, even though a `:t` probe is read-and-discarded and never
/// crosses into a later turn's compile. `:t` must be exempt by construction
/// (`SessionBind::probe_only`), for ANY well-typed expression, and must
/// mutate nothing — `:bindings` lists the same names before and after.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn t_on_m_returning_helper_does_not_trip_cross_row_guard() {
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
    let out = t.expect_ok(
        ":t commitDeltaRepro (M-returning expression must not crash the cross-row bind guard)",
    );
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
        bindings_only(&before),
        bindings_only(&after),
        ":t must mutate nothing — the probe bind is never registered: before={before} after={after}"
    );
}

/// Same friction, for a stdlib EFFECT VERB whose type is `Either <Err> T`
/// (`run :: forall effs. Member Exec effs => Text -> Eff effs (Either
/// ExecError Proc)`) — `ExecError` is defined in the STABLE
/// `Tidepool.Effects.Core` module (stable-effects-core), so `:t run`'s probe
/// bind must not trip the cross-row bind guard. `run` is queried at the
/// session's own concrete `M` (`:t (run :: Text -> M (Either ExecError
/// Proc))`) rather than bare: bare `run` is a genuinely AMBIGUOUS type query
/// now that it is row-polymorphic (`Member Exec effs0` alone can never
/// default `effs0` — the same GHC defaulting gap `__anchor`
/// (`tidepool-mcp::eval_prep::TurnTemplate`) works around for `finalize`'s
/// free result type — an orthogonal, pre-existing `:t`-on-an-unapplied-
/// polymorphic-value limitation, not a cross-row-bind-guard regression).
/// Needs the full
/// effect stack (Exec) rather than `Repl::new()`'s Console-only minimal one.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn t_on_either_returning_effect_verb_does_not_trip_cross_row_guard() {
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
    let out =
        t.expect_ok(":t run (Either-returning verb type must not crash the cross-row bind guard)");
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
        bindings_only(&before),
        bindings_only(&after),
        ":t must mutate nothing — the probe bind is never registered: before={before} after={after}"
    );
}

/// STABLE-EFFECTS-CORE ACCEPTANCE: a GENUINE bind (not `:t`) of an
/// `Either ExecError Proc` value now PERSISTS instead of being rejected.
/// Before this change, `ExecError`/`Proc` were declared inline in the
/// per-session generated `Tidepool.Effects` module (fragment-nominal — a
/// fresh nominal identity every turn), so the cross-row bind guard
/// (`typeMentionsEffectMonad`) rejected ANY bind mentioning them, with a
/// "destructure at the bind" hint as the only workaround. Now `ExecError`/
/// `Proc` live in the STABLE `Tidepool.Effects.Core` module (a pure function
/// of the effect vocabulary alone), so a bind mentioning them is exactly as
/// safe to persist as any other plain data value — the guard only fires for
/// a type that itself mentions the `Eff` tycon (a genuinely row-typed value),
/// which `Either ExecError Proc` never did once the effect monad's OWN tycon
/// is the only thing checked. The bound value must also be genuinely
/// CALLABLE in a LATER turn, not merely accepted — that's the persistence
/// payoff, not just an absence of rejection.
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
