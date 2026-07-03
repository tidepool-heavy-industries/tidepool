//! End-to-end proof that a record renders as a named-field JSON object
//! (issue #334). A record value with NO `ToJSON` instance is rendered by the
//! Rust `DataConTable` path in `render.rs`. Before this change it rendered
//! positionally — `{"constructor":"Person","fields":["Alice",30]}` with the
//! field names LOST. After: `{"_con":"Person","name":"Alice","age":30}`.
//!
//! The field labels ride in on the CBOR meta side-map (GHC `dataConFieldLabels`
//! -> 7th meta element -> `DataConTable::field_labels`) so this exercises the
//! full wire path: Haskell extract -> meta.cbor -> DataConTable -> render.rs.
//! This is the wire-format proof that the Rust `render.rs` unit tests
//! (`test_render_record_named_fields`) hand-build a table for.
//!
//! Needs the with-packages GHC on PATH and `TIDEPOOL_EXTRACT` pointing at a
//! freshly built extract binary (see haskell/CLAUDE.md). Skips (passes) when
//! `TIDEPOOL_EXTRACT` is unset so a plain `cargo test` on a checkout without the
//! toolchain does not fail.

mod common;

use tidepool_runtime::compile_and_run_pure;

fn run(src: &str, target: &str) -> tidepool_runtime::EvalResult {
    let pp = common::prelude_path();
    let src = src.to_owned();
    let target = target.to_owned();
    std::thread::Builder::new()
        .stack_size(64 * 1024 * 1024)
        .spawn(move || {
            let include = [pp.as_path()];
            compile_and_run_pure(&src, &target, &include).expect("compile_and_run_pure")
        })
        .unwrap()
        .join()
        .unwrap()
}

/// A flat record with mixed String + Int fields renders with named fields and
/// the constructor riding under `_con`.
#[test]
fn flat_record_renders_named_field() {
    if std::env::var_os("TIDEPOOL_EXTRACT").is_none() {
        eprintln!("skipping: TIDEPOOL_EXTRACT not set (no extract toolchain)");
        return;
    }
    let src = "module Test where\n\
               data Person = Person { name :: String, age :: Int }\n\
               alice :: Person\n\
               alice = Person { name = \"Alice\", age = 30 }\n";
    let got = run(src, "alice").to_json();
    assert_eq!(
        got,
        serde_json::json!({"_con": "Person", "name": "Alice", "age": 30}),
        "flat record must render named-field (got {got})"
    );
}

/// A nested record: the inner record becomes a nested named-field object, so
/// deeply-structured outcomes stay readable (the issue's `FileApplied` case).
#[test]
fn nested_record_renders_named_field() {
    if std::env::var_os("TIDEPOOL_EXTRACT").is_none() {
        eprintln!("skipping: TIDEPOOL_EXTRACT not set (no extract toolchain)");
        return;
    }
    let src = "module Test where\n\
               data Loc = Loc { line :: Int, col :: Int }\n\
               data Node = Node { label :: String, loc :: Loc }\n\
               root :: Node\n\
               root = Node { label = \"root\", loc = Loc { line = 7, col = 3 } }\n";
    let got = run(src, "root").to_json();
    assert_eq!(
        got,
        serde_json::json!({
            "_con": "Node",
            "label": "root",
            "loc": {"_con": "Loc", "line": 7, "col": 3}
        }),
        "nested record must render nested named-field (got {got})"
    );
}
