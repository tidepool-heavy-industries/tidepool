//! Edit verbs (`.tidepool/lib/Edit.hs`): declarative line-range / anchor edits
//! that LOWER to a `Tidepool.Patch` FilePatch and ride the shipped atomic
//! apply. The point of the surface is that small structural edits inherit every
//! keystone property of the diff flow — pre-flight (`planEdits`), all-or-nothing
//! apply (`applyEdits` delegates to `Diff.apply`), and conflict-as-data — so the
//! assertions mirror `patch_verbs.rs`: writes==0 on any conflict, and the
//! `planEdits` → `applyDiff` round trip proving the edit front-end and the diff
//! back-end meet at the rendered patch text.
//!
//! Run with the worktree extract binary (the verbs pull Tidepool.Prelude →
//! lens, so the with-packages libdir is required):
//!   TIDEPOOL_EXTRACT=<worktree>/haskell/dist-newstyle/.../tidepool-extract-bin \
//!   TIDEPOOL_GHC_LIBDIR=<with-packages>/lib/ghc-9.12.2/lib \
//!   cargo test -p tidepool-runtime --test edit_verbs
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

/// Run `code` (a single eval expression) with `input` injected on the `input`
/// lane; returns the result rendered to JSON.
fn run_eval(code: &str, input: &str, d: &mut FsDispatcher) -> serde_json::Value {
    let decls = tidepool_mcp::standard_decls();
    let pre = tidepool_mcp::build_preamble(&decls, true);
    let stack = tidepool_mcp::build_effect_stack_type(&decls);
    let nonce = std::env::var("NONCE").unwrap_or_default();
    let full = format!("-- nonce {nonce}\n{code}");
    let input = serde_json::Value::String(input.to_string());
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
        Err(e) => panic!("edit-verb eval failed: {e}"),
    }
}

fn preload(pairs: &[(&str, &str)]) -> FsDispatcher {
    let mut d = FsDispatcher::default();
    for (k, v) in pairs {
        d.files.insert((*k).to_string(), (*v).to_string());
    }
    d
}

/// Each test runs on a 64MB thread — genPatch's Myers + the apply engine are
/// list-heavy, matching the patch_verbs harness.
fn on_big_stack<F: FnOnce() + Send + 'static>(f: F) {
    std::thread::Builder::new()
        .stack_size(64 * 1024 * 1024)
        .spawn(f)
        .unwrap()
        .join()
        .unwrap();
}

const F3: &str = "alpha\nbeta\ngamma";

#[test]
fn applyedits_replace_line_range() {
    on_big_stack(|| {
        let mut d = preload(&[("f.txt", F3)]);
        let json = run_eval("applyEdits \"f.txt\" [ReplaceLines 2 2 [\"BETA\"]]", "", &mut d);
        assert_eq!(json["applied"], serde_json::json!(true), "got {json}");
        assert_eq!(d.writes, 1, "exactly one write");
        assert_eq!(
            d.files.get("f.txt").map(String::as_str),
            Some("alpha\nBETA\ngamma"),
            "line 2 replaced, lowered through a context-anchored patch"
        );
    });
}

#[test]
fn applyedits_insert_after_anchor() {
    on_big_stack(|| {
        let mut d = preload(&[("f.txt", F3)]);
        let json = run_eval(
            "applyEdits \"f.txt\" [InsertAfterAnchor \"beta\" [\"INSERTED\"]]",
            "",
            &mut d,
        );
        assert_eq!(json["applied"], serde_json::json!(true), "got {json}");
        assert_eq!(d.writes, 1, "one write");
        assert_eq!(
            d.files.get("f.txt").map(String::as_str),
            Some("alpha\nbeta\nINSERTED\ngamma"),
            "block inserted after the unique anchor line"
        );
    });
}

#[test]
fn applyedits_ambiguous_anchor_is_conflict_data() {
    on_big_stack(|| {
        // "x" appears on lines 1 and 3 → AnchorAmbiguous, reported as data, no write.
        let mut d = preload(&[("f.txt", "xa\nbbb\nxc")]);
        let json = run_eval("applyEdits \"f.txt\" [ReplaceAnchor \"x\" [\"Y\"]]", "", &mut d);
        assert_eq!(json["applied"], serde_json::json!(false), "got {json}");
        assert_eq!(
            json["conflicts"][0]["kind"],
            serde_json::json!("anchor-ambiguous"),
            "ambiguous anchor surfaced as data; got {json}"
        );
        assert_eq!(
            json["conflicts"][0]["lines"],
            serde_json::json!([1, 3]),
            "the candidate lines are reported; got {json}"
        );
        assert_eq!(d.writes, 0, "no write on a resolution conflict");
        assert_eq!(d.files.get("f.txt").map(String::as_str), Some("xa\nbbb\nxc"));
    });
}

#[test]
fn applyedits_range_out_of_bounds_is_conflict_data() {
    on_big_stack(|| {
        let mut d = preload(&[("f.txt", F3)]);
        let json = run_eval("applyEdits \"f.txt\" [ReplaceLines 5 6 [\"Z\"]]", "", &mut d);
        assert_eq!(json["applied"], serde_json::json!(false), "got {json}");
        assert_eq!(
            json["conflicts"][0]["kind"],
            serde_json::json!("range-out-of-bounds"),
            "out-of-range surfaced as data; got {json}"
        );
        assert_eq!(json["conflicts"][0]["fileLines"], serde_json::json!(3), "got {json}");
        assert_eq!(d.writes, 0, "no write");
    });
}

#[test]
fn applyedits_overlapping_batch_is_atomic() {
    on_big_stack(|| {
        // Two edits whose ranges [1,2] and [2,3] intersect → EditsOverlap. The
        // whole batch is rejected before any write (single-file atomicity).
        let mut d = preload(&[("f.txt", F3)]);
        let json = run_eval(
            "applyEdits \"f.txt\" [ReplaceLines 1 2 [\"A\"], ReplaceLines 2 3 [\"B\"]]",
            "",
            &mut d,
        );
        assert_eq!(json["applied"], serde_json::json!(false), "got {json}");
        assert_eq!(
            json["conflicts"][0]["kind"],
            serde_json::json!("edits-overlap"),
            "overlap detected as a resolution conflict; got {json}"
        );
        assert_eq!(d.writes, 0, "ATOMIC: one bad edit blocks the whole batch");
        assert_eq!(d.files.get("f.txt").map(String::as_str), Some(F3), "file untouched");
    });
}

#[test]
fn planedits_is_a_dry_run_with_a_diff() {
    on_big_stack(|| {
        let mut d = preload(&[("f.txt", F3)]);
        let json = run_eval("planEdits \"f.txt\" [ReplaceLines 2 2 [\"BETA\"]]", "", &mut d);
        assert_eq!(json["ok"], serde_json::json!(true), "got {json}");
        assert_eq!(json["changed"], serde_json::json!(true), "got {json}");
        assert!(
            json["diff"]
                .as_str()
                .map(|s| s.contains("-beta") && s.contains("+BETA"))
                == Some(true),
            "the rendered review diff carries the change; got {json}"
        );
        assert_eq!(d.writes, 0, "plan is a dry run — no writes");
    });
}

#[test]
fn planedits_then_applydiff_roundtrips() {
    on_big_stack(|| {
        // The meet-at-patch-text claim: planEdits renders a diff, applyDiff
        // commits it, and the file becomes what an in-eval applyEdits would have
        // produced. One write — applyDiff's; planEdits only reads.
        let mut d = preload(&[("f.txt", F3)]);
        let code = "planEdits \"f.txt\" [ReplaceLines 2 2 [\"BETA\"]] \
                    >>= \\r -> case r ?. \"diff\" of \
                    { Just (String dd) -> applyDiff dd; _ -> pure (object [\"err\" .= (\"no diff\" :: Text)]) }";
        let json = run_eval(code, "", &mut d);
        assert_eq!(
            json["applied"],
            serde_json::json!(true),
            "planEdits's diff applies through applyDiff; got {json}"
        );
        assert_eq!(
            d.files.get("f.txt").map(String::as_str),
            Some("alpha\nBETA\ngamma"),
            "round-trip reproduces the edit"
        );
        assert_eq!(d.writes, 1, "exactly one write — applyDiff's");
    });
}

// The JSON front door: {file, edits:[{op,…}]} on the input lane (the quote-heavy
// / batch primary, mirroring patchJ/applyDiff).
const EDITSJ: &str = "editsJ (case input of { Object _ -> input; _ -> error \"no input\" })";

#[test]
fn editsj_json_front_door_applies() {
    on_big_stack(|| {
        let mut d = preload(&[("f.txt", F3)]);
        let payload = r#"{"file":"f.txt","edits":[{"op":"replaceAnchor","anchor":"beta","lines":["BETA"]}]}"#;
        // editsJ reads the structured payload off the input lane directly.
        let decls = tidepool_mcp::standard_decls();
        let pre = tidepool_mcp::build_preamble(&decls, true);
        let stack = tidepool_mcp::build_effect_stack_type(&decls);
        let full = format!("-- nonce \n{EDITSJ}");
        let input: serde_json::Value = serde_json::from_str(payload).unwrap();
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
        let json = match compile_and_run(&src, "result", &include, &mut d, &()) {
            Ok(v) => v.to_json(),
            Err(e) => panic!("editsJ eval failed: {e}"),
        };
        assert_eq!(json["applied"], serde_json::json!(true), "got {json}");
        assert_eq!(
            d.files.get("f.txt").map(String::as_str),
            Some("alpha\nBETA\ngamma"),
            "editsJ resolved the anchor op and applied it"
        );
        assert_eq!(d.writes, 1, "one write");
    });
}
