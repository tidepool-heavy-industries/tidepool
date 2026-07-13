//! Core Fs helper verbs `updateAll`/`insertAfter` (#344) — embedded in
//! `tidepool-mcp/src/effect_defs.rs` as always-on preamble helpers, NOT
//! project-lib. Both used to THROW (`error "..."`) on a domain no-match
//! (pattern absent / anchor missing-or-ambiguous), which meant a caller
//! looping over many files (`mapM_ (\f -> updateAll f old new) files`) had
//! the WHOLE EVAL abort the moment one file lacked the pattern — files
//! already processed stayed mutated, files later in the list were never
//! attempted, and the caller got an opaque runtime error instead of a
//! report. They now report their outcome as typed DATA
//! (`UpdateAllOutcome`/`InsertAfterOutcome`, mirroring `UpdateOutcome`), so a
//! batch runs to completion: every file gets its own outcome, and a file
//! that doesn't match is simply reported rejected with zero writes for that
//! file — not an aborted eval.
//!
//! Run with the worktree extract binary (the verbs pull Tidepool.Prelude →
//! lens, so the with-packages libdir is required):
//!   TIDEPOOL_EXTRACT=<worktree>/haskell/dist-newstyle/.../tidepool-extract-bin \
//!   TIDEPOOL_GHC_LIBDIR=<with-packages>/lib/ghc-9.12.2/lib \
//!   cargo test -p tidepool-runtime --test update_verbs
use std::collections::HashMap;
use std::path::Path;
use tidepool_bridge::FromCore;
use tidepool_bridge_effects::FileMeta;
use tidepool_effect::DispatchEffect;
use tidepool_eval::value::Value;
use tidepool_testing::eval_harness::EvalHarness;

/// An in-memory filesystem answering the Fs effect, counting writes so the
/// no-partial-mutation claim (a rejected file gets zero writes) is checkable.
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
                Some("FsMetadata") => {
                    let path = String::from_value(&fields[0], table).unwrap();
                    let meta = self.files.get(&path).map(|c| FileMeta {
                        size: c.len() as i64,
                        is_file: true,
                        is_dir: false,
                    });
                    return cx.respond(meta);
                }
                Some("FsRead") => {
                    let path = String::from_value(&fields[0], table).unwrap();
                    let content = self.files.get(&path).cloned().unwrap_or_default();
                    return cx.respond(Ok::<String, String>(content));
                }
                Some("FsWrite") => {
                    let path = String::from_value(&fields[0], table).unwrap();
                    let content = String::from_value(&fields[1], table).unwrap();
                    self.files.insert(path, content);
                    self.writes += 1;
                    return cx.respond(Ok::<(), String>(()));
                }
                _ => {}
            }
        }
        cx.respond(())
    }
}

/// Run `code` (a single eval expression); returns the result rendered to JSON.
fn run_eval(code: &str, d: &mut FsDispatcher) -> serde_json::Value {
    let decls = tidepool_mcp::standard_decls();
    let pre = tidepool_mcp::build_preamble(&decls, true);
    let stack = tidepool_mcp::build_effect_stack_type(&decls);
    let nonce = std::env::var("NONCE").unwrap_or_default();
    let full = format!("-- nonce {nonce}\n{code}");
    let src = tidepool_mcp::template_haskell(
        &pre,
        &stack,
        &tidepool_mcp::wrap_do(&full),
        &tidepool_mcp::aeson_imports(),
        "",
        None,
        None,
    );
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap();
    let harness = EvalHarness::new()
        .with_stdlib()
        .with_include(root.join(".tidepool/lib"))
        .with_effects_module();
    let handlers = std::mem::take(d);
    let (outcome, handlers) = harness.run_owned(&src, "result", handlers);
    *d = handlers;
    match outcome.into_result() {
        Ok(v) => v.to_json(),
        Err(e) => panic!("update-verb eval failed: {e}"),
    }
}

fn preload(pairs: &[(&str, &str)]) -> FsDispatcher {
    let mut d = FsDispatcher::default();
    for (k, v) in pairs {
        d.files.insert((*k).to_string(), (*v).to_string());
    }
    d
}

#[test]
fn updateall_replaces_every_occurrence_and_reports_count() {
    let mut d = preload(&[("f.txt", "foo bar foo baz foo")]);
    let json = run_eval("toJSON <$> updateAll \"f.txt\" \"foo\" \"FOO\"", &mut d);
    assert_eq!(json["ok"], serde_json::json!(true), "got {json}");
    assert_eq!(json["count"], serde_json::json!(3), "got {json}");
    assert_eq!(
        d.files.get("f.txt").map(String::as_str),
        Some("FOO bar FOO baz FOO")
    );
    assert_eq!(d.writes, 1);
}

#[test]
fn updateall_no_match_is_rejected_data_not_a_throw() {
    // The core #344 claim: a missing pattern is DATA, the eval does not abort.
    let mut d = preload(&[("f.txt", "nothing relevant here")]);
    let json = run_eval("toJSON <$> updateAll \"f.txt\" \"foo\" \"FOO\"", &mut d);
    assert_eq!(json["ok"], serde_json::json!(false), "got {json}");
    assert!(
        json["reason"].as_str().map(|s| s.contains("not found")) == Some(true),
        "got {json}"
    );
    assert_eq!(d.writes, 0, "no write on a rejected updateAll");
    assert_eq!(
        d.files.get("f.txt").map(String::as_str),
        Some("nothing relevant here"),
        "file untouched"
    );
}

#[test]
fn updateall_batch_completes_past_a_missing_file_no_partial_mutation() {
    // The #344 friction, reproduced: a batch over THREE files where the
    // pattern is absent in the FIRST one. Before the fix, `updateAll`'s
    // `error` on b.txt would abort the whole eval mid-`mapM`, so a.txt (which
    // sorts after it) would never even be attempted despite matching. Now
    // every file gets its own outcome and the batch runs to completion: the
    // miss is reported as data, and the two files that DO match are both
    // written — nothing is silently skipped, nothing half-applies.
    let mut d = preload(&[
        ("miss.txt", "no pattern here"),
        ("a.txt", "foo one"),
        ("b.txt", "foo two"),
    ]);
    let code =
        "toJSON <$> mapM (\\f -> updateAll f \"foo\" \"FOO\") [\"miss.txt\", \"a.txt\", \"b.txt\"]";
    let json = run_eval(code, &mut d);
    let arr = json.as_array().expect("array of outcomes");
    assert_eq!(arr.len(), 3, "got {json}");
    assert_eq!(
        arr[0]["ok"],
        serde_json::json!(false),
        "miss.txt rejected; got {json}"
    );
    assert_eq!(
        arr[1]["ok"],
        serde_json::json!(true),
        "a.txt applied; got {json}"
    );
    assert_eq!(arr[1]["count"], serde_json::json!(1));
    assert_eq!(
        arr[2]["ok"],
        serde_json::json!(true),
        "b.txt applied; got {json}"
    );
    assert_eq!(arr[2]["count"], serde_json::json!(1));
    assert_eq!(
        d.files.get("miss.txt").map(String::as_str),
        Some("no pattern here"),
        "the rejected file is untouched"
    );
    assert_eq!(d.files.get("a.txt").map(String::as_str), Some("FOO one"));
    assert_eq!(d.files.get("b.txt").map(String::as_str), Some("FOO two"));
    assert_eq!(d.writes, 2, "exactly the two matching files were written");
}

#[test]
fn insertafter_applies_after_the_unique_anchor() {
    let mut d = preload(&[("f.txt", "alpha\nbeta\ngamma")]);
    let json = run_eval(
        "toJSON <$> insertAfter \"f.txt\" \"beta\" \"INSERTED\"",
        &mut d,
    );
    assert_eq!(json["ok"], serde_json::json!(true), "got {json}");
    assert_eq!(
        d.files.get("f.txt").map(String::as_str),
        Some("alpha\nbeta\nINSERTED\ngamma\n"),
        "block inserted after the unique anchor line"
    );
    assert_eq!(d.writes, 1);
}

#[test]
fn insertafter_missing_anchor_is_rejected_data_not_a_throw() {
    let mut d = preload(&[("f.txt", "alpha\nbeta\ngamma")]);
    let json = run_eval(
        "toJSON <$> insertAfter \"f.txt\" \"nope\" \"INSERTED\"",
        &mut d,
    );
    assert_eq!(json["ok"], serde_json::json!(false), "got {json}");
    assert_eq!(json["matches"], serde_json::json!(0), "got {json}");
    assert_eq!(d.writes, 0, "no write on a rejected insertAfter");
    assert_eq!(
        d.files.get("f.txt").map(String::as_str),
        Some("alpha\nbeta\ngamma"),
        "file untouched"
    );
}

#[test]
fn insertafter_ambiguous_anchor_is_rejected_data_not_a_throw() {
    let mut d = preload(&[("f.txt", "x one\nplain\nx two")]);
    let json = run_eval(
        "toJSON <$> insertAfter \"f.txt\" \"x\" \"INSERTED\"",
        &mut d,
    );
    assert_eq!(json["ok"], serde_json::json!(false), "got {json}");
    assert_eq!(json["matches"], serde_json::json!(2), "got {json}");
    assert_eq!(d.writes, 0);
}

#[test]
fn insertafter_batch_completes_past_a_missing_anchor_no_partial_mutation() {
    let mut d = preload(&[
        ("miss.txt", "no anchor here"),
        ("a.txt", "before\nANCHOR\nafter"),
    ]);
    let code =
        "toJSON <$> mapM (\\f -> insertAfter f \"ANCHOR\" \"NEW\") [\"miss.txt\", \"a.txt\"]";
    let json = run_eval(code, &mut d);
    let arr = json.as_array().expect("array of outcomes");
    assert_eq!(arr[0]["ok"], serde_json::json!(false), "got {json}");
    assert_eq!(arr[1]["ok"], serde_json::json!(true), "got {json}");
    assert_eq!(
        d.files.get("miss.txt").map(String::as_str),
        Some("no anchor here"),
        "rejected file untouched"
    );
    assert_eq!(
        d.files.get("a.txt").map(String::as_str),
        Some("before\nANCHOR\nNEW\nafter\n")
    );
    assert_eq!(d.writes, 1, "only the matching file was written");
}

#[test]
fn updatej_malformed_payload_is_rejected_data_not_a_throw() {
    // Same #344 contract as its siblings: a malformed input-lane payload
    // (missing `old`/`new` keys) is `UpdateOneRejected` DATA — the eval (and
    // any batch it is part of) completes instead of aborting on `error`.
    let mut d = preload(&[("f.txt", "foo bar")]);
    let json = run_eval(
        "toJSON <$> updateJ (object [\"file\" .= (\"f.txt\" :: Text)])",
        &mut d,
    );
    assert_eq!(json["ok"], serde_json::json!(false), "got {json}");
    assert!(
        json["reason"]
            .as_str()
            .map(|s| s.contains("need {file, old, new}"))
            == Some(true),
        "got {json}"
    );
    assert_eq!(d.writes, 0, "no write on a rejected updateJ");
}

#[test]
fn updatej_batch_completes_past_a_malformed_item() {
    // One malformed payload among well-formed ones: every item gets its own
    // outcome, the good edits still apply.
    let mut d = preload(&[("a.txt", "foo bar"), ("b.txt", "foo baz")]);
    let code = "toJSON <$> mapM updateJ\n\
                \x20 [ object [\"file\" .= (\"a.txt\" :: Text), \"old\" .= (\"foo\" :: Text), \"new\" .= (\"FOO\" :: Text)]\n\
                \x20 , object [\"file\" .= (\"b.txt\" :: Text)]\n\
                \x20 , object [\"file\" .= (\"b.txt\" :: Text), \"old\" .= (\"baz\" :: Text), \"new\" .= (\"BAZ\" :: Text)]\n\
                \x20 ]";
    let json = run_eval(code, &mut d);
    let arr = json.as_array().expect("array of outcomes");
    assert_eq!(arr[0]["ok"], serde_json::json!(true), "got {json}");
    assert_eq!(arr[1]["ok"], serde_json::json!(false), "got {json}");
    assert_eq!(arr[2]["ok"], serde_json::json!(true), "got {json}");
    assert_eq!(d.files.get("a.txt").map(String::as_str), Some("FOO bar"));
    assert_eq!(d.files.get("b.txt").map(String::as_str), Some("foo BAZ"));
    assert_eq!(d.writes, 2, "both well-formed edits applied");
}
