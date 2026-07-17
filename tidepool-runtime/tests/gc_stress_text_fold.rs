//! GC stress: large-Text corpus folds with the post-GC heap verifier ON.
//!
//! Reproducer hunt for a field SIGSEGV: a fused `Map.map` fold running
//! `T.lines`/`T.isInfixOf` over a ~4 MiB corpus of large Texts (100+ KiB
//! each) segfaulted twice in a live repl session, timing-dependently — the
//! same fold on the same data succeeded on retry and on a fresh heap. That
//! shape (huge allocation volume between safepoints, deep laziness, heap
//! doubling under a big live set) is exactly what the Cheney collector's
//! missed-root failure class looks like, so these probes run it with
//! `set_heap_verify(true)`: every collection walks the full live set and
//! panics on the first dangling/from-space pointer instead of corrupting
//! silently and crashing collections later.
//!
//! A failure here is a GC bug (missed root / bad evacuation), never user error.

use std::path::Path;
use tidepool_runtime::compile_and_run;
use tidepool_testing::NullDispatcher;

fn eval_raw(code: &str) -> Result<serde_json::Value, String> {
    let decls = tidepool_mcp::standard_decls();
    let pre = tidepool_mcp::build_preamble(&decls, true);
    let stack = tidepool_mcp::build_effect_stack_type(&decls);
    let src = tidepool_mcp::template_haskell(&pre, &stack, code, "", "", None, None);
    let effects_dir = tidepool_mcp::ensure_effects_module(&decls).expect("write effects module");
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap();
    let hs = root.join("haskell/lib");
    let lib = root.join(".tidepool/lib");
    let include = [hs.as_path(), lib.as_path(), effects_dir.as_path()];
    let mut d = NullDispatcher;
    match compile_and_run(&src, "result", &include, &mut d, &()) {
        Ok(v) => Ok(v.to_json()),
        Err(e) => Err(tidepool_runtime::classify(&e).message),
    }
}

/// Run one probe on the standard eval-thread stack with signal safety and the
/// heap verifier installed. Distinguishes the three outcomes: clean result,
/// clean error (acceptable), thread panic (verifier fired / hard crash — bug).
fn run_verified(code: &str) -> Result<serde_json::Value, String> {
    let code = code.to_string();
    let result = std::thread::Builder::new()
        .stack_size(tidepool_runtime::EVAL_STACK_SIZE)
        .spawn(move || {
            tidepool_codegen::signal_safety::install();
            tidepool_codegen::host_fns::set_heap_verify(true);
            tidepool_codegen::host_fns::set_gc_poison(true);
            eval_raw(&code)
        })
        .unwrap()
        .join()
        .map_err(|_| "HARD CRASH: heap verifier fired or uncaught signal".to_string())?;
    // A green run proves nothing unless collections actually happened and
    // the verifier walked them. nextest = one process per test, so the
    // process-global counter is this probe's alone.
    let verified = tidepool_codegen::host_fns::heap_verify_run_count();
    assert!(
        verified > 0,
        "probe finished without a single GC — grow the corpus until it collects"
    );
    eprintln!("[gc-stress] verified collections: {verified}");
    result
}

/// The field workload, synthesized: `files` Texts of `lines_per` lines each,
/// grouped into a Map, then the fused per-group `lines`/`isInfixOf` count fold.
fn corpus_fold(files: usize, lines_per: usize) -> String {
    format!(
        r#"do
  let mkText i = T.unlines [ "fn frob" <> showT i <> "_" <> showT j <> "() {{ let x = " <> showT (i * j) <> "; " <> (if (i + j) `mod` 7 == 0 then "unsafe {{ ptr::read(p) }}" else "x + 1") <> " }}" | j <- [1 .. {lines_per} + (i * 37) `mod` 400 :: Int] ]
  let corpus = [ (showT i <> ".rs", mkText i) | i <- [1 .. {files} :: Int] ]
  let byCrate = Map.fromListWith (<>) [ (T.take 2 k, [(k, t)]) | (k, t) <- corpus ]
  let cnts = Map.map (\fs -> sum [ length (filter (T.isInfixOf "unsafe ") (T.lines t)) | (_, t) <- fs ]) byCrate
  pure (sum (Map.elems cnts))"#
    )
}

/// Small enough to finish fast, big enough to force multiple collections.
#[test]
fn text_fold_small_corpus_verified() {
    let v = run_verified(&corpus_fold(120, 200)).expect("fold must succeed");
    assert!(
        v.as_i64().unwrap_or(-1) > 0,
        "expected positive count, got {v}"
    );
}

/// Field-sized: ~4+ MiB of Text, live across the whole fold — forces heap
/// doubling under a large live set (the untracked-intermediate-space class).
#[test]
fn text_fold_field_sized_corpus_verified() {
    let v = run_verified(&corpus_fold(350, 600)).expect("fold must succeed");
    assert!(
        v.as_i64().unwrap_or(-1) > 0,
        "expected positive count, got {v}"
    );
}

/// Same fold repeated with the corpus rebuilt each round inside one machine —
/// maximizes collections per run and varies GC-trigger phase.
#[test]
fn text_fold_repeated_rounds_verified() {
    let code = r#"do
  rounds <- forM [1 .. 6 :: Int] (\r -> do
    let mkText i = T.unlines [ "line " <> showT (r * 1000 + i) <> " " <> (if (i + j) `mod` 11 == 0 then "unsafe marker" else "plain filler text") | j <- [1 .. 300 + (i * 53) `mod` 500 :: Int] ]
    let corpus = [ (showT i, mkText i) | i <- [1 .. 150 :: Int] ]
    let byK = Map.fromListWith (<>) [ (T.take 1 k, [(k, t)]) | (k, t) <- corpus ]
    pure (sum (Map.elems (Map.map (\fs -> sum [ length (filter (T.isInfixOf "unsafe") (T.lines t)) | (_, t) <- fs ]) byK))))
  pure (sum rounds)"#;
    let v = run_verified(code).expect("repeated fold must succeed");
    assert!(
        v.as_i64().unwrap_or(-1) > 0,
        "expected positive count, got {v}"
    );
}
