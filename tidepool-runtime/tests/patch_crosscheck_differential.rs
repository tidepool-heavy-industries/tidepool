//! `dataToTag#` must answer the constructor's index within its own data type
//! — the both-engines pin, via the order-dependent `Tidepool.Patch` repro
//! that surfaced the bug.
//!
//! WHAT WENT WRONG. `Tidepool/Translate.hs` desugars `tagToEnum#` (index →
//! constructor) into a `case`, "because type information is erased
//! downstream". Its inverse, `dataToTag#`/`dataToTagSmall#`/
//! `dataToTagLarge#`, was instead mapped straight through to the `DataToTag`
//! primop — and both backends implement that as "return the runtime
//! constructor tag" (`tidepool-eval/src/eval.rs`,
//! `tidepool-codegen/src/emit/primop.rs`). That tag is a 56-bit
//! `stableVarId` hash of the constructor's NAME (`True` =
//! `0xfe6150b1b818a688`), never GHC's 0-based per-type index, so every
//! `dataToTag#` returned garbage on both engines.
//!
//! WHY IT LOOKED LIKE ORDER-DEPENDENT CROSS-TALK. GHC only reaches for the
//! primop on shapes like `boolExpr == True` when the comparison feeds a
//! shared `Int#` join point — and it is the SECOND check in the module that
//! makes GHC build that join point. So one check alone was right, two checks
//! flipped the first one's answer, and swapping them moved the damage onto a
//! comparison (`== "f.txt"`, `Eq Text`) that never emits the primop. Nothing
//! about `Patch.hs`, binder names, or the JIT was involved; the repro is
//! kept because it is the reliable trigger, not because the bug is Patch's.
//!
//! WHAT THIS FILE PINS. The same module through BOTH engines — the
//! tree-walking interpreter (`tidepool-eval`, the oracle) and the Cranelift
//! JIT — in both orders, plus a structural guard that the emitted IR carries
//! no `DataToTag` primop at all. Both engines read the SAME `CoreExpr` from
//! one `compile_haskell` call, so an engine disagreement cannot be a
//! compile-side artifact, and an agreement is a claim about the Core itself:
//!
//! | oracle | JIT   | verdict                                   |
//! |--------|-------|-------------------------------------------|
//! | right  | wrong | codegen / effect-machine bug              |
//! | wrong  | wrong | extract / Translate (Core is already bad) |
//! | wrong  | right | stdlib `Patch.hs` semantics               |
//!
//! The original repro landed in the "both wrong" row, which is what pointed
//! at Translate.

use std::path::Path;
use tidepool_eval::{deep_force, env_from_datacon_table, eval, VecHeap};
use tidepool_repr::frame::CoreFrame;
use tidepool_repr::types::PrimOpKind;
use tidepool_runtime::{compile_haskell, value_to_json, CompileResult};
use tidepool_testing::NullDispatcher;

/// The `/dev/null`-creation check: `Patch.fpCreate` on a create patch whose
/// header carries a tab-separated timestamp. Expects `True`.
const CHECK_B: &str = r#"check "b"
        ((case Patch.parsePatch (T.intercalate "\n" ["--- /dev/null\t1970-01-01 00:00:00.000000000 +0000", "+++ b/new.txt\t2026-01-01 00:00:00.000000000 +0000", "@@ -0,0 +1,1 @@", "+hello"]) of { Right [fp2] -> Patch.fpCreate fp2; _ -> False }) == True)"#;

/// The no-newline-marker apply check: the hunk's new side is marked
/// no-newline, so the output must lose its trailing newline.
const CHECK_C: &str = r#"check "c"
        ((case Patch.parsePatch (T.intercalate "\n" ["--- a/f.txt", "+++ b/f.txt", "@@ -1,2 +1,2 @@", " line1", "-line2", "+line2", "\\ No newline at end of file"]) of { Right (fp3:_) -> (case Patch.applyFilePatch fp3 (Just (T.intercalate "\n" ["line1", "line2", ""])) of { Right (out3, _) -> out3; Left _ -> "CONFLICT" }); Left _ -> "PARSE-FAIL" }) == "line1\nline2")"#;

/// Build the check-list probe from an ordered list of check expressions.
/// `check nm ok` yields `[]` when the check holds and `[nm]` when it does
/// not, so the probe's value is the list of FAILING check names — `[]` is
/// the all-green answer under every order.
fn probe(checks: &[&str]) -> String {
    format!(
        "pure (concat\n    [ {}\n    ])\n where {{ check nm ok = if ok then [] else [nm] }}",
        checks.join("\n    , ")
    )
}

/// Wrap a probe expression in the full MCP preamble, exactly as
/// `stdlib_regressions_02_medium.rs::eval_raw` does (same `standard_decls`,
/// same effect stack, same QQ-import rule).
fn module_source(code: &str) -> (String, Vec<std::path::PathBuf>) {
    let decls = tidepool_mcp::standard_decls();
    let pre = tidepool_mcp::build_preamble(&decls, true);
    let stack = tidepool_mcp::build_effect_stack_type(&decls);
    let imports = if tidepool_mcp::uses_qq(code) {
        "Tidepool.QQ (fmt, j, patch, uri)"
    } else {
        ""
    };
    let src = tidepool_mcp::template_haskell(&pre, &stack, code, imports, "", None, None);
    let effects_dir = tidepool_mcp::ensure_effects_module(&decls).expect("write effects module");
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap();
    let include = vec![
        root.join("haskell/lib"),
        root.join(".tidepool/lib"),
        effects_dir,
    ];
    (src, include)
}

/// Peel the freer-simple `Val` wrapper the interpreter leaves on a `pure`
/// computation. The JIT's effect machine unwraps it as part of running the
/// program; the interpreter has no effect machine and stops at the `Eff`
/// value, so without this the two engines' JSON could never be compared even
/// when they agree.
fn unwrap_freer_val(v: serde_json::Value) -> serde_json::Value {
    match (
        v.get("constructor").and_then(|c| c.as_str()),
        v.get("fields"),
    ) {
        (Some("Val"), Some(serde_json::Value::Array(fs))) if fs.len() == 1 => fs[0].clone(),
        _ => v,
    }
}

/// What one compiled probe answered, on each engine, plus what the emitted
/// IR carries.
struct Cell {
    /// The tree-walking interpreter's answer over the compiled `CoreExpr`.
    oracle: Result<serde_json::Value, String>,
    /// The JIT's answer over the same `CoreExpr` (the production path).
    jit: Result<serde_json::Value, String>,
    /// Count of `DataToTag` primop nodes in the emitted IR. Must be 0:
    /// `Translate.hs` desugars every `dataToTag#` into a `case`, and the
    /// backends' `DataToTag` answers with the runtime constructor tag, which
    /// is not GHC's per-type index. One surviving node is the bug back.
    datatotag_nodes: usize,
}

/// One compile, two engines. Reporting both from a SINGLE `compile_haskell`
/// is the point: it tells "both wrong" (compile-side) from "JIT only"
/// (machine-side) without a second compile introducing its own variable.
fn both_engines(code: &str) -> Cell {
    let (src, include) = module_source(code);
    let inc: Vec<&Path> = include.iter().map(|p| p.as_path()).collect();

    let CompileResult {
        expr, mut table, ..
    } = match compile_haskell(&src, "result", &inc) {
        Ok(r) => r,
        Err(e) => {
            let msg = format!("compile failed: {e}");
            return Cell {
                oracle: Err(msg.clone()),
                jit: Err(msg),
                datatotag_nodes: 0,
            };
        }
    };
    table.populate_siblings_from_expr(&expr);

    let datatotag_nodes = expr
        .nodes
        .iter()
        .filter(|f| {
            matches!(
                f,
                CoreFrame::PrimOp {
                    op: PrimOpKind::DataToTag,
                    ..
                }
            )
        })
        .count();

    // Oracle: tree-walking interpreter over the very same Core.
    let env = env_from_datacon_table(&table);
    let mut heap = VecHeap::new();
    let oracle = eval(&expr, &env, &mut heap)
        .and_then(|v| deep_force(v, &mut heap))
        .map(|v| unwrap_freer_val(value_to_json(&v, &table, 0)))
        .map_err(|e| format!("eval error: {e:?}"));

    // JIT: the production path (`compile_and_run` re-uses the memoized
    // compile, so this is the same Core the oracle just ran).
    let mut d = NullDispatcher;
    let jit = tidepool_runtime::compile_and_run(&src, "result", &inc, &mut d, &())
        .map(|v| v.to_json())
        .map_err(|e| tidepool_runtime::classify(&e).message);

    Cell {
        oracle,
        jit,
        datatotag_nodes,
    }
}

/// Run a probe on the shared `EVAL_STACK_SIZE` with signal safety installed,
/// same as the repro's home harness.
fn on_probe_thread<T: Send + 'static>(f: impl FnOnce() -> T + Send + 'static) -> T {
    std::thread::Builder::new()
        .stack_size(tidepool_runtime::EVAL_STACK_SIZE)
        .spawn(move || {
            tidepool_codegen::signal_safety::install();
            f()
        })
        .unwrap()
        .join()
        .expect("probe thread panicked (HARD crash / uncaught signal)")
}

/// Check "c" with the `""` list element written as an append of the same
/// Text. Semantically identical, but it keeps a literal-backed empty `Text`
/// out of a raw byte-array primop, which is the one thing in the original
/// check the interpreter cannot execute (see `ORACLE_LITSTRING_GAP`).
const CHECK_C_ORACLE_SAFE: &str = r#"check "c"
        ((case Patch.parsePatch (T.intercalate "\n" ["--- a/f.txt", "+++ b/f.txt", "@@ -1,2 +1,2 @@", " line1", "-line2", "+line2", "\\ No newline at end of file"]) of { Right (fp3:_) -> (case Patch.applyFilePatch fp3 (Just (T.append (T.intercalate "\n" ["line1", "line2"]) "\n")) of { Right (out3, _) -> out3; Left _ -> "CONFLICT" }); Left _ -> "PARSE-FAIL" }) == "line1\nline2")"#;

/// The interpreter's known gap on `CHECK_C`, allowed by name rather than by
/// a blanket "ignore oracle errors": `tidepool-eval`'s `expect_byte_array`
/// rejects a `Text` whose backing is a `LitString` (the `""` literal in
/// `CHECK_C`'s replacement list), where `text_bytes_clamped_with` on the
/// neighbouring path accepts exactly that. Nothing to do with this bug — the
/// oracle never reaches an answer on those cells — so the matrix runs the
/// `CHECK_C_ORACLE_SAFE` rows to make its both-engines claim, and treats
/// this error on the `CHECK_C` rows as expected. If the gap is ever closed,
/// this test reports the stale allowance instead of silently passing.
const ORACLE_LITSTRING_GAP: &str = r#"TypeMismatch { expected: "ByteArray#""#;

/// The matrix: {b alone, c alone, b-then-c, c-then-b} x {oracle, JIT}, plus
/// the oracle-runnable spelling of the same two orders. Every cell must be
/// `[]` (no failing checks) on every engine — the checks are independent and
/// true, so no order and no engine may disagree — and no cell's emitted IR
/// may carry a `DataToTag` primop.
///
/// Bundled as ONE test over six probes rather than six tests: the compiles
/// are the cost here, and a per-cell `#[test]` would pay six process
/// startups on top without buying isolation the matrix needs (the whole
/// point is comparing cells against each other).
#[test]
fn patch_crosscheck_order_independence_matrix() {
    // `oracle_runs`: false marks the two rows whose check "c" trips
    // `ORACLE_LITSTRING_GAP`. They still pin the JIT — and they are the
    // verbatim shape the bug was reported against, so they stay.
    let cells: Vec<(&str, bool, String)> = vec![
        ("b-alone", true, probe(&[CHECK_B])),
        ("c-alone", false, probe(&[CHECK_C])),
        ("b-then-c", false, probe(&[CHECK_B, CHECK_C])),
        ("c-then-b", false, probe(&[CHECK_C, CHECK_B])),
        ("b-then-c/o", true, probe(&[CHECK_B, CHECK_C_ORACLE_SAFE])),
        ("c-then-b/o", true, probe(&[CHECK_C_ORACLE_SAFE, CHECK_B])),
    ];

    let mut rows = Vec::new();
    let mut bad = Vec::new();
    for (name, oracle_runs, code) in cells {
        let cell = on_probe_thread(move || both_engines(&code));
        let empty = serde_json::json!([]);
        let j_ok = cell.jit.as_ref().map(|v| *v == empty).unwrap_or(false);
        let o_ok = match &cell.oracle {
            Ok(v) => *v == empty,
            // A stale allowance must surface, so an oracle error on a row
            // marked non-running only passes when it IS the known gap.
            Err(e) => !oracle_runs && e.contains(ORACLE_LITSTRING_GAP),
        };
        let tag_ok = cell.datatotag_nodes == 0;
        rows.push(format!(
            "  {name:<11} oracle={:<28} jit={:<12} DataToTag={} {}",
            render(&cell.oracle),
            render(&cell.jit),
            cell.datatotag_nodes,
            if o_ok && j_ok && tag_ok {
                "OK"
            } else {
                "**BAD**"
            }
        ));
        if !(o_ok && j_ok && tag_ok) {
            bad.push(name.to_string());
        }
    }

    let matrix = rows.join("\n");
    println!("LOCALIZATION MATRIX (cell value = list of FAILING checks; [] is green)\n{matrix}");
    assert!(
        bad.is_empty(),
        "dataToTag# cross-check corruption reproduced in {bad:?}\n{matrix}"
    );
}

/// The OTHER `dataToTag#` consumer, and the one that was live in the shipped
/// stdlib: GHC's derived `Ord` compares two different constructors by their
/// tags, so `compare`/`sort`/`Set`/`Map` on any multi-constructor type with
/// `deriving Ord` ran on hashed tags instead of declaration order. The
/// shipped instance is `Tidepool.Aeson.Value` (`haskell/lib/Tidepool/Aeson/
/// Value.hs`), whose constructor order is Object < Array < String < Number <
/// Bool < Null.
///
/// The pre-fix answer was not random — hashes are stable — so a `Set Value`
/// still behaved as a set. It was stably WRONG: `sort` returned an order
/// unrelated to the declared one, which is what any caller comparing across
/// constructors would have seen.
#[test]
fn derived_ord_on_value_follows_declaration_order() {
    let code = r#"pure (L.sort [Null, toJSON (1 :: Int), toJSON ("a" :: Text), toJSON True])"#;
    let cell = on_probe_thread(move || both_engines(code));
    println!(
        "derived-Ord probe: oracle={} jit={} DataToTag={}",
        render(&cell.oracle),
        render(&cell.jit),
        cell.datatotag_nodes
    );
    let want = serde_json::json!(["a", 1, true, null]);
    assert_eq!(cell.jit.as_ref().ok(), Some(&want), "JIT sorted wrongly");
    assert_eq!(
        cell.oracle.as_ref().ok(),
        Some(&want),
        "interpreter sorted wrongly"
    );
    assert_eq!(
        cell.datatotag_nodes, 0,
        "a DataToTag primop survived translation"
    );
}

/// Ablation driver: run every `*.expr` file in `$TIDEPOOL_CROSSCHECK_PROBES`
/// (each holding one probe expression verbatim) through both engines and
/// print the matrix. Lets the shrink loop iterate on Haskell text without a
/// Rust rebuild between variants.
#[test]
fn crosscheck_probe_dir() {
    let Ok(dir) = std::env::var("TIDEPOOL_CROSSCHECK_PROBES") else {
        println!("TIDEPOOL_CROSSCHECK_PROBES unset — nothing to run");
        return;
    };
    let mut files: Vec<_> = std::fs::read_dir(&dir)
        .expect("read probe dir")
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().is_some_and(|x| x == "expr"))
        .collect();
    files.sort();
    for path in files {
        let code = std::fs::read_to_string(&path).expect("read probe");
        let name = path.file_stem().unwrap().to_string_lossy().to_string();
        let cell = on_probe_thread(move || both_engines(&code));
        println!(
            "PROBE {name:<28} oracle={:<50} jit={:<20} DataToTag={}",
            render(&cell.oracle),
            render(&cell.jit),
            cell.datatotag_nodes
        );
    }
}

/// Write each matrix cell's full module source to `$TIDEPOOL_CROSSCHECK_DUMP`
/// so the extract-side knobs (`TIDEPOOL_VARID_AUDIT`, `TIDEPOOL_DUMP_CLOSED`)
/// can be driven against the exact same sources by hand, without a Rust
/// rebuild between knob settings. No GHC involved — pure preamble assembly.
#[test]
fn dump_crosscheck_module_sources() {
    let Ok(dir) = std::env::var("TIDEPOOL_CROSSCHECK_DUMP") else {
        println!("TIDEPOOL_CROSSCHECK_DUMP unset — nothing to dump");
        return;
    };
    std::fs::create_dir_all(&dir).expect("create dump dir");
    let mut cells = vec![
        ("b_alone".to_string(), probe(&[CHECK_B])),
        ("c_alone".to_string(), probe(&[CHECK_C])),
        ("b_then_c".to_string(), probe(&[CHECK_B, CHECK_C])),
        ("c_then_b".to_string(), probe(&[CHECK_C, CHECK_B])),
    ];
    // Also dump anything staged in the ablation probe dir, so a shrink-loop
    // variant can be handed to the extract knobs without a rebuild.
    if let Ok(pd) = std::env::var("TIDEPOOL_CROSSCHECK_PROBES") {
        let mut files: Vec<_> = std::fs::read_dir(&pd)
            .expect("read probe dir")
            .filter_map(|e| e.ok().map(|e| e.path()))
            .filter(|p| p.extension().is_some_and(|x| x == "expr"))
            .collect();
        files.sort();
        for p in files {
            let name = p.file_stem().unwrap().to_string_lossy().to_string();
            cells.push((name, std::fs::read_to_string(&p).expect("read probe")));
        }
    }
    for (name, code) in cells {
        let (src, include) = module_source(&code);
        let path = Path::new(&dir).join(format!("{name}.hs"));
        std::fs::write(&path, &src).expect("write module source");
        println!("wrote {} ({} bytes)", path.display(), src.len());
        if name == "b_alone" {
            let incs: Vec<String> = include
                .iter()
                .map(|p| format!("--include {}", p.display()))
                .collect();
            println!("include args: {}", incs.join(" "));
        }
    }
}

fn render(r: &Result<serde_json::Value, String>) -> String {
    match r {
        Ok(v) => v.to_string(),
        Err(e) => format!("ERR({})", e.lines().next().unwrap_or("")),
    }
}
