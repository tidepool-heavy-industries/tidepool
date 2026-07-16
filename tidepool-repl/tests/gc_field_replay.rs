//! Field-replay GC stress: the 2026-07-10 SIGSEGV session, re-driven through
//! the REAL full-stack repl server with from-space poisoning + the post-GC
//! heap verifier ON.
//!
//! The synthetic suites (`gc_heap_verify_stress.rs`,
//! `tidepool-runtime/tests/gc_stress_text_fold.rs`) run the same fold shapes
//! clean across thousands of verified collections — but they build their
//! substrate in-JIT. The field session's substrate arrived through the EFFECT
//! BRIDGE (`gitLog 500`, `readGlob "**/*.rs"` → `value_to_heap`), which is a
//! different allocation path (host-side spine dismantling, rust-root
//! registration windows, bridge-then-tenure). This suite replays that exact
//! turn sequence over the real repository so the bridged-substrate surface is
//! under the same fail-loud instrumentation.
//!
//! A failure here is a GC/bridge/tenure bug, never user error.

mod common;
use common::*;

use tidepool_handlers::{base_decls_with_ask, build_base_stack, HandlerConfig};
use tidepool_repl::{ReplServerConfig, TidepoolReplServer};

/// Build a repl server over the FULL base handler stack (Fs/Git/Exec/…),
/// sandboxed at the repo root, with the harness's small 2 MiB nursery.
/// Must be called inside a tokio runtime (LlmHandler captures the handle).
fn build_full_stack_repl() -> Repl {
    let root = tidepool_testing::eval_harness::repo_root();
    let scratch = std::env::temp_dir().join(format!(
        "tidepool-gc-field-replay-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0)
    ));
    std::fs::create_dir_all(&scratch).expect("scratch dir");
    let stack = build_base_stack(&HandlerConfig {
        cwd: root,
        kv_path: scratch.join("kv.json"),
        llm_model: "claude-haiku-4-5-20251001".to_string(),
    });
    let (decls, ask_tag) = base_decls_with_ask(&stack);
    let effects_dir =
        tidepool_mcp::ensure_effects_module(&decls).expect("write Tidepool.Effects module");
    let prelude_dir = tidepool_testing::eval_harness::prelude_path();
    let module_env = tidepool_mcp::session_decl_module_env(&decls, false);
    let cfg = ReplServerConfig {
        decls,
        ask_tag,
        base_include: vec![effects_dir, prelude_dir],
        module_env,
        session_root_base: scratch.join("sessions"),
        nursery_size: Some(1 << 21), // 2 MiB — force organic GC under load
        continuation_ttl: None,
        wedged_ttl: None,
        turn_timeout: None,
    };
    Repl {
        server: TidepoolReplServer::new(stack, cfg),
    }
}

/// The field session, turn for turn: git-history substrate (bridged), derived
/// folds, corpus via readGlob (bridged), grouped Map, then the two crash
/// shapes — the fused fold as a bare expression in a multi-item block, and
/// the fold-result bind.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn field_session_replay_bridged_substrate_verified() {
    if !extract_available() {
        eprintln!("skipping: tidepool-extract not available (set TIDEPOOL_EXTRACT)");
        return;
    }
    tidepool_codegen::host_fns::set_heap_verify(true);
    tidepool_codegen::host_fns::set_gc_poison(true);
    let repl = build_full_stack_repl();

    // Turn 1: helper decl.
    repl.def("topOf :: T.Text -> T.Text\ntopOf p = case T.splitOn \"/\" p of { (x:_:_) -> x; _ -> \"<root>\" }")
        .await
        .expect_ok("decl topOf");

    // Turn 2: bridged git substrate — 500 commits with file lists.
    repl.eval("Right commits <- gitLog 500")
        .await
        .expect_ok("bind commits (bridged)");

    // Turn 3: derived binds over the bridged substrate (field: crossLang +
    // crateTouches folds).
    repl.run(&[
        "let crossLang = [ c | c <- commits, any (T.isSuffixOf \".hs\") c.files, any (T.isSuffixOf \".rs\") c.files ]",
        "let crateTouches = Map.fromListWith (+) [ (d, 1::Int) | c <- commits, d <- L.nub (map topOf c.files), T.isPrefixOf \"tidepool\" d ]",
        "(length commits, length crossLang, Map.size crateTouches)",
    ])
    .await
    .expect_ok("git-history folds");

    // Turn 4: bridged corpus — every .rs file in the workspace via readGlob.
    repl.eval("corpus <- readGlob \"**/*.rs\"")
        .await
        .expect_ok("bind corpus (bridged)");

    // Turn 5 — CRASH #1 SHAPE: one block: grouped-Map bind + closure decl +
    // fused fold expression over the bridged corpus.
    repl.run(&[
        "byCrate <- pure (Map.fromListWith (<>) [ (topOf r.path, [(r.path, txt)]) | r <- corpus, Right txt <- [r.contents], not (T.isInfixOf \"/target/\" r.path) ])",
        "let density needle = [ (k, hits) | (k, fs) <- Map.toList byCrate, let hits = sum [ length (filter (T.isInfixOf needle) (T.lines t)) | (_,t) <- fs ], hits > 0 ]",
        "toJSON (density \"unsafe \")",
    ])
    .await
    .expect_ok("crash-shape #1: grouped bind + fused fold expr");

    // Turn 6 — CRASH #2 SHAPE: bind of a whole-map fold result (tenure of a
    // value computed by folding old-space data through heavy nursery churn).
    repl.run(&[
        "let countIn needle = Map.map (\\fs -> sum [ length (filter (T.isInfixOf needle) (T.lines t)) | (_,t) <- fs ]) byCrate",
        "unsafeCounts <- pure (countIn \"unsafe \")",
    ])
    .await
    .expect_ok("crash-shape #2: bind of whole-map fold");

    // Turn 7: keep folding over the same tenured+bridged substrate.
    repl.eval("sum (Map.elems unsafeCounts) + sum (Map.elems (countIn \"fn \"))")
        .await
        .expect_ok("repeat fold");

    let verified = tidepool_codegen::host_fns::heap_verify_run_count();
    assert!(
        verified > 0,
        "replay finished without a single verified GC — substrate too small?"
    );
    eprintln!("[gc-field-replay] verified collections: {verified}");
}

/// CONTROL: the same replay with every item in its OWN turn (no within-block
/// let-closure handoff). Both field crashes applied a closure `let`-bound in
/// an EARLIER ITEM OF THE SAME BLOCK; every field success either inlined the
/// lambda or used a closure from a previous turn. If this control passes while
/// the block version fails, the missed root lives in the within-block binding
/// handoff, not the GC core.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn field_session_replay_split_turns_control() {
    if !extract_available() {
        eprintln!("skipping: tidepool-extract not available (set TIDEPOOL_EXTRACT)");
        return;
    }
    tidepool_codegen::host_fns::set_heap_verify(true);
    tidepool_codegen::host_fns::set_gc_poison(true);
    let repl = build_full_stack_repl();

    repl.def("topOf :: T.Text -> T.Text\ntopOf p = case T.splitOn \"/\" p of { (x:_:_) -> x; _ -> \"<root>\" }")
        .await
        .expect_ok("decl topOf");
    repl.eval("corpus <- readGlob \"**/*.rs\"")
        .await
        .expect_ok("bind corpus (bridged)");
    repl.eval("byCrate <- pure (Map.fromListWith (<>) [ (topOf r.path, [(r.path, txt)]) | r <- corpus, Right txt <- [r.contents], not (T.isInfixOf \"/target/\" r.path) ])")
        .await
        .expect_ok("bind byCrate");
    repl.eval("let density needle = [ (k, hits) | (k, fs) <- Map.toList byCrate, let hits = sum [ length (filter (T.isInfixOf needle) (T.lines t)) | (_,t) <- fs ], hits > 0 ]")
        .await
        .expect_ok("bind density (own turn)");
    repl.eval("toJSON (density \"unsafe \")")
        .await
        .expect_ok("fused fold expr (own turn)");

    let verified = tidepool_codegen::host_fns::heap_verify_run_count();
    assert!(verified > 0, "control ran without a single verified GC");
    eprintln!("[gc-field-replay-control] verified collections: {verified}");
}
