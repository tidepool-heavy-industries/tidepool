//! Round-2 LSP ergonomics: `LspGraph.walk`/`dedupNodes` and `concatMapM`
//! (Tidepool.Prelude) typecheck and run over SYNTHETIC `LspNode`s — no
//! `tidepool-lsp-daemon` involved. The step functions below are plain `pure`
//! lookups (never `send` a real `Lsp` effect op), so a `NullDispatcher` that's
//! never actually called is enough; see `lspnode_show_dot.rs` for the same
//! harness shape used to prove `LspNode` is Show/record-dot accessible.
//!
//! Also proves `lspCallers`/`lspCallees`/`lspRefs` are plain `LspNode -> M
//! [LspNode]` post-round-2 (no `Maybe` to unwrap) by using that exact shape
//! for the synthetic step functions passed to `walk`.

use std::path::Path;
use tidepool_eval::value::Value;
use tidepool_testing::eval_harness::EvalHarness;
use tidepool_testing::NullDispatcher;

fn eval_raw(code: &str) -> Result<serde_json::Value, String> {
    let decls = tidepool_mcp::standard_decls();
    let pre = tidepool_mcp::build_preamble(&decls, true);
    let stack = tidepool_mcp::build_effect_stack_type(&decls);
    let src = tidepool_mcp::template_haskell(&pre, &stack, code, "", "", None, None);
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap();
    let lib = root.join(".tidepool/lib");
    EvalHarness::new()
        .with_stdlib()
        .with_include(lib)
        .with_effects_module()
        .run(&src, "result", NullDispatcher)
        .into_result()
        .map(|v| v.to_json())
        .map_err(|e| format!("{e}"))
}

fn eval_ok(code: &str, expected: serde_json::Value) {
    match eval_raw(code) {
        Ok(got) => assert_eq!(
            got, expected,
            "\n  code: {code}\n  want: {expected}\n  got: {got}"
        ),
        Err(e) => panic!("\n  code: {code}\n  compile/run error: {e}"),
    }
}

fn mk(name: &str) -> String {
    format!(r#"(LspNode "{name}" "" "" "f.rs" (Position 1 0) "")"#)
}

#[test]
fn dedup_nodes_drops_repeats_order_preserving() {
    if !tidepool_testing::eval_harness::extract_available() {
        eprintln!("skipping: TIDEPOOL_EXTRACT not set (no extract toolchain)");
        return;
    }
    let a = mk("a");
    let b = mk("b");
    let code = format!(r#"pure (map nodeName (dedupNodes [{a}, {b}, {a}]))"#);
    eval_ok(&code, serde_json::json!(["a", "b"]));
}

/// `walk` over a synthetic 4-node graph with a CYCLE (a -> b,c -> d -> a).
/// Each step function is exactly the post-round-2 `lspCallers`/`lspCallees`
/// shape: `LspNode -> M [LspNode]`, no `Maybe`. Without the visited-set this
/// loops forever (d re-discovers a every round); with it, the walk halts once
/// a round discovers nothing new — proving both the cycle-safety and that
/// `walk`/`concatMapM` compose over `[LspNode]` directly.
#[test]
fn walk_bfs_terminates_on_cycle_and_dedupes() {
    if !tidepool_testing::eval_harness::extract_available() {
        eprintln!("skipping: TIDEPOOL_EXTRACT not set (no extract toolchain)");
        return;
    }
    let code = format!(
        r#"
let step n = case nodeName n of
      "a" -> pure [{b}, {c}]
      "b" -> pure [{d}]
      "c" -> pure [{d}]
      "d" -> pure [{a}]
      _   -> pure []
in map nodeName <$> walk step 10 {a}
"#,
        a = mk("a"),
        b = mk("b"),
        c = mk("c"),
        d = mk("d"),
    );
    eval_ok(&code, serde_json::json!(["b", "c", "d"]));
}

/// `concatMapM` (Tidepool.Prelude) over a step function of the round-2 shape:
/// no `fromMaybe`/unwrap needed to fan a frontier of nodes out and flatten.
#[test]
fn concat_map_m_composes_with_plain_list_step() {
    if !tidepool_testing::eval_harness::extract_available() {
        eprintln!("skipping: TIDEPOOL_EXTRACT not set (no extract toolchain)");
        return;
    }
    let code = format!(
        r#"
let step n = case nodeName n of
      "a" -> pure [{b}, {c}]
      _   -> pure []
in map nodeName <$> concatMapM step [{a}]
"#,
        a = mk("a"),
        b = mk("b"),
        c = mk("c"),
    );
    eval_ok(&code, serde_json::json!(["b", "c"]));
}
