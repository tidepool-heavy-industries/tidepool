//! Diff verbs (`.tidepool/lib/Diff.hs`) over a stateful in-memory filesystem:
//! `apply` is ALL-OR-NOTHING (one conflict blocks every write — the writes==0
//! proof), and `plan` reports conflicts as data. The dispatcher answers
//! FsExists/FsRead/FsWrite from a `HashMap`, counting writes.
//!
//! The diff rides the eval `input` lane (the spec's payload lane) via
//! `applyDiff`/`planDiff` — `template_haskell` indents every code line, so a
//! multi-line `[patch|…|]` inline in eval code would be corrupted; the quoter
//! is for column-0 source files (it is covered by the Suite fixtures).
//!
//! Run with the worktree extract binary (the verbs pull Tidepool.Prelude →
//! lens, so the with-packages libdir is required):
//!   TIDEPOOL_EXTRACT=<worktree>/haskell/dist-newstyle/.../tidepool-extract-bin \
//!   TIDEPOOL_GHC_LIBDIR=<with-packages>/lib/ghc-9.12.2/lib \
//!   cargo test -p tidepool-runtime --test patch_verbs
use std::collections::HashMap;
use std::path::Path;
use tidepool_bridge::FromCore;
use tidepool_effect::DispatchEffect;
use tidepool_eval::value::Value;
use tidepool_runtime::compile_and_run;

/// An in-memory filesystem answering the Fs effect, counting writes so the
/// atomic-apply guarantee (zero writes on any conflict) is checkable.
#[derive(Default)]
struct FsDispatcher {
    files: HashMap<String, String>,
    writes: usize,
}

impl DispatchEffect<()> for FsDispatcher {
    fn dispatch(
        &mut self,
        _tag: u64,
        request: &Value,
        cx: &tidepool_effect::EffectContext<'_, ()>,
    ) -> Result<tidepool_effect::Response, tidepool_effect::error::EffectError> {
        let table = cx.table();
        if let Value::Con(con_id, fields) = request {
            match table.name_of(*con_id) {
                Some("FsExists") => {
                    let path = String::from_value(&fields[0], table).unwrap();
                    return cx.respond(self.files.contains_key(&path));
                }
                Some("FsRead") => {
                    let path = String::from_value(&fields[0], table).unwrap();
                    let content = self.files.get(&path).cloned().unwrap_or_default();
                    return cx.respond(content);
                }
                Some("FsWrite") => {
                    let path = String::from_value(&fields[0], table).unwrap();
                    let content = String::from_value(&fields[1], table).unwrap();
                    self.files.insert(path, content);
                    self.writes += 1;
                    return cx.respond(());
                }
                _ => {}
            }
        }
        cx.respond(())
    }
}

/// Run `code` (a single-line eval expression) with `diff` injected on the
/// `input` lane; returns the result rendered to JSON.
fn run_eval(code: &str, diff: &str, d: &mut FsDispatcher) -> serde_json::Value {
    let decls = tidepool_mcp::standard_decls();
    let pre = tidepool_mcp::build_preamble(&decls, true);
    let stack = tidepool_mcp::build_effect_stack_type(&decls);
    let nonce = std::env::var("NONCE").unwrap_or_default();
    let full = format!("-- nonce {nonce}\n{code}");
    let input = serde_json::Value::String(diff.to_string());
    let src = tidepool_mcp::template_haskell(
        &pre,
        &stack,
        &tidepool_mcp::wrap_do(&full),
        &tidepool_mcp::aeson_imports(),
        "",
        Some(&input),
        None,
    );
    let effects_dir = tidepool_mcp::ensure_effects_module(&decls)
        .expect("write effects module")
        .leak() as &Path;
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap();
    let hs = root.join("haskell/lib").leak() as &Path;
    let lib = root.join(".tidepool/lib").leak() as &Path;
    let include = [hs, lib, effects_dir];
    match compile_and_run(&src, "result", &include, d, &()) {
        Ok(v) => v.to_json(),
        Err(e) => panic!("patch-verb eval failed: {e}"),
    }
}

fn preload(pairs: &[(&str, &str)]) -> FsDispatcher {
    let mut d = FsDispatcher::default();
    for (k, v) in pairs {
        d.files.insert((*k).to_string(), (*v).to_string());
    }
    d
}

// Pull the diff Text out of the `input` value, then run the verb.
const APPLY_INPUT: &str = "applyDiff (case input of { String s -> s; _ -> error \"no input\" })";
const PLAN_INPUT: &str =
    "toJSON <$> planDiff (case input of { String s -> s; _ -> error \"no input\" })";

// A two-file patch: -a/+A on one.txt, -b/+B on two.txt.
const TWO_FILE_DIFF: &str =
    "--- a/one.txt\n+++ b/one.txt\n@@ -1,1 +1,1 @@\n-a\n+A\n--- a/two.txt\n+++ b/two.txt\n@@ -1,1 +1,1 @@\n-b\n+B\n";

#[test]
fn patch_apply_multifile_atomic() {
    std::thread::Builder::new()
        .stack_size(64 * 1024 * 1024)
        .spawn(|| {
            let mut d = preload(&[("one.txt", "a"), ("two.txt", "b")]);
            let json = run_eval(APPLY_INPUT, TWO_FILE_DIFF, &mut d);
            assert_eq!(
                json["applied"],
                serde_json::json!(true),
                "both hunks clean → applied; got {json}"
            );
            assert_eq!(
                json["files"].as_array().map(|a| a.len()),
                Some(2),
                "two files reported"
            );
            assert_eq!(d.writes, 2, "exactly two writes (one per file)");
            assert_eq!(d.files.get("one.txt").map(String::as_str), Some("A"));
            assert_eq!(d.files.get("two.txt").map(String::as_str), Some("B"));
        })
        .unwrap()
        .join()
        .unwrap();
}

#[test]
fn patch_apply_conflict_blocks_all_writes() {
    std::thread::Builder::new()
        .stack_size(64 * 1024 * 1024)
        .spawn(|| {
            // one.txt is clean, two.txt's old side ("b") is absent → NoMatch.
            // The atomic guarantee: NEITHER file is written.
            let mut d = preload(&[("one.txt", "a"), ("two.txt", "DIFFERENT")]);
            let json = run_eval(APPLY_INPUT, TWO_FILE_DIFF, &mut d);
            assert_eq!(
                json["applied"],
                serde_json::json!(false),
                "a conflict blocks application; got {json}"
            );
            assert!(
                json["conflicts"]
                    .as_array()
                    .map(|a| !a.is_empty())
                    .unwrap_or(false),
                "conflicts array should be non-empty; got {json}"
            );
            assert_eq!(d.writes, 0, "ATOMIC: zero writes when any file conflicts");
            assert_eq!(
                d.files.get("one.txt").map(String::as_str),
                Some("a"),
                "clean file left untouched"
            );
        })
        .unwrap()
        .join()
        .unwrap();
}

#[test]
fn patch_plan_reports() {
    std::thread::Builder::new()
        .stack_size(64 * 1024 * 1024)
        .spawn(|| {
            // x.txt already holds the NEW content "A" → AlreadyApplied, no write.
            let mut d = preload(&[("x.txt", "A")]);
            let diff = "--- a/x.txt\n+++ b/x.txt\n@@ -1,1 +1,1 @@\n-a\n+A\n";
            let json = run_eval(PLAN_INPUT, diff, &mut d);
            let arr = json
                .as_array()
                .expect("plan returns a JSON array of conflicts");
            assert_eq!(arr.len(), 1, "one conflict reported; got {json}");
            assert_eq!(
                arr[0]["kind"],
                serde_json::json!("already-applied"),
                "kind tag; got {json}"
            );
            assert_eq!(
                arr[0]["line"],
                serde_json::json!(1),
                "already-applied at line 1; got {json}"
            );
            assert_eq!(arr[0]["file"], serde_json::json!("x.txt"));
            assert_eq!(d.writes, 0, "plan is a dry run — no writes");
        })
        .unwrap()
        .join()
        .unwrap();
}

// genPatchTo reads the current file, diffs it to the new content (Myers), and
// renders the unified diff. Chaining `>>= applyDiff` proves the generated diff
// is well-formed and reproduces the new content through the Fs effect: write
// old → genPatchTo new → applyDiff → file == new. The diff rides nothing; the
// NEW content rides the `input` lane.
const GENPATCHTO_F: &str =
    "genPatchTo \"f.txt\" (case input of { String s -> s; _ -> error \"no input\" }) >>= applyDiff";
const GENPATCHTO_NEW: &str =
    "genPatchTo \"new.txt\" (case input of { String s -> s; _ -> error \"no input\" }) >>= applyDiff";
// checkDiff is pure: parse the input diff and report what it is, as data.
const CHECKDIFF: &str =
    "pure (checkDiff (case input of { String s -> s; _ -> error \"no input\" }))";

#[test]
fn genpatchto_roundtrip_through_fs() {
    std::thread::Builder::new()
        .stack_size(64 * 1024 * 1024)
        .spawn(|| {
            // f.txt currently holds `old`; ask for a diff to `new` (a change plus
            // an appended line), apply it, and confirm the file becomes `new`.
            let old = "alpha\nbeta\ngamma";
            let new = "alpha\nBETA\ngamma\ndelta";
            let mut d = preload(&[("f.txt", old)]);
            let json = run_eval(GENPATCHTO_F, new, &mut d);
            assert_eq!(
                json["applied"],
                serde_json::json!(true),
                "genPatchTo → applyDiff applies cleanly; got {json}"
            );
            assert_eq!(
                d.files.get("f.txt").map(String::as_str),
                Some(new),
                "file now holds the new content"
            );
            assert_eq!(
                d.writes, 1,
                "exactly one write (applyDiff); genPatchTo only reads"
            );
        })
        .unwrap()
        .join()
        .unwrap();
}

#[test]
fn genpatchto_creation_absent_file() {
    std::thread::Builder::new()
        .stack_size(64 * 1024 * 1024)
        .spawn(|| {
            // new.txt does not exist → genPatchTo emits a --- /dev/null creation
            // patch; applyDiff creates the file. Trailing-newline content round-
            // trips exactly (each new-side line is newline-terminated).
            let content = "line1\nline2\n";
            let mut d = FsDispatcher::default();
            let json = run_eval(GENPATCHTO_NEW, content, &mut d);
            assert_eq!(
                json["applied"],
                serde_json::json!(true),
                "creation patch applies; got {json}"
            );
            assert_eq!(
                d.files.get("new.txt").map(String::as_str),
                Some("line1\nline2\n"),
                "absent file created with the new content"
            );
            assert_eq!(d.writes, 1, "one write — the created file");
        })
        .unwrap()
        .join()
        .unwrap();
}

#[test]
fn difffiles_reports_patch_and_stats() {
    std::thread::Builder::new()
        .stack_size(64 * 1024 * 1024)
        .spawn(|| {
            // Two existing files: diffFiles labels the patch with (and makes it
            // apply onto) the OLD path, and reports rendered text plus stats.
            let mut d = preload(&[("a.txt", "x\ny\nz"), ("b.txt", "x\nY\nz")]);
            let json = run_eval("diffFiles \"a.txt\" \"b.txt\"", "", &mut d);
            assert_eq!(json["changed"], serde_json::json!(true), "got {json}");
            assert_eq!(json["path"], serde_json::json!("a.txt"), "got {json}");
            assert!(
                json["patch"].as_str().map(|s| s.contains("-y") && s.contains("+Y"))
                    == Some(true),
                "rendered patch carries the change; got {json}"
            );
            assert_eq!(json["stats"]["hunks"], serde_json::json!(1), "got {json}");
            assert_eq!(d.writes, 0, "diffFiles is read-only");
        })
        .unwrap()
        .join()
        .unwrap();
}

#[test]
fn difffiles_identical_reports_unchanged() {
    std::thread::Builder::new()
        .stack_size(64 * 1024 * 1024)
        .spawn(|| {
            let mut d = preload(&[("a.txt", "same\nlines"), ("b.txt", "same\nlines")]);
            let json = run_eval("diffFiles \"a.txt\" \"b.txt\"", "", &mut d);
            assert_eq!(
                json["changed"],
                serde_json::json!(false),
                "identical content → unchanged; got {json}"
            );
            assert_eq!(d.writes, 0, "diffFiles is read-only");
        })
        .unwrap()
        .join()
        .unwrap();
}

#[test]
fn checkdiff_valid_reports_structure() {
    std::thread::Builder::new()
        .stack_size(64 * 1024 * 1024)
        .spawn(|| {
            let diff = "--- a/x.txt\n+++ b/x.txt\n@@ -1,1 +1,1 @@\n-a\n+A\n";
            let mut d = FsDispatcher::default();
            let json = run_eval(CHECKDIFF, diff, &mut d);
            assert_eq!(
                json["parses"],
                serde_json::json!(true),
                "a well-formed diff parses; got {json}"
            );
            assert_eq!(
                json["files"][0]["path"],
                serde_json::json!("x.txt"),
                "file path surfaced; got {json}"
            );
            assert_eq!(
                json["files"][0]["hunks"],
                serde_json::json!(1),
                "one hunk; got {json}"
            );
        })
        .unwrap()
        .join()
        .unwrap();
}

#[test]
fn checkdiff_invalid_reports_parse_error() {
    std::thread::Builder::new()
        .stack_size(64 * 1024 * 1024)
        .spawn(|| {
            // The dogfood failure mode: an unparseable diff is indistinguishable
            // from a pattern shape mismatch — checkDiff separates the two.
            let diff = "this is not a diff at all";
            let mut d = FsDispatcher::default();
            let json = run_eval(CHECKDIFF, diff, &mut d);
            assert_eq!(
                json["parses"],
                serde_json::json!(false),
                "garbage does not parse; got {json}"
            );
            assert!(
                json["error"].is_string(),
                "a parse-error message is present; got {json}"
            );
        })
        .unwrap()
        .join()
        .unwrap();
}
