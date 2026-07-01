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
    if !extract_available() {
        return;
    }
    let repl = Repl::new();
    repl.open_ok().await;

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

    repl.close().await.expect_ok("close");
}
