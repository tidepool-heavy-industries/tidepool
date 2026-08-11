//! Recursive sum types through the GENERIC JSON DEFAULTS on the real JIT:
//! `deriving (Generic, ToJSON, FromJSON)` on a payload sum whose recursion
//! flows through a list — proven through the one production path, aeson's
//! TaggedObject shape with record constructors.
//!
//! Needs `TIDEPOOL_EXTRACT` (run inside `nix develop`).

use serde_json::json;
use tidepool_testing::eval_harness::{require_extract, EvalHarness};

fn run(source: &str, target: &str) -> serde_json::Value {
    require_extract();
    EvalHarness::new()
        .with_stdlib()
        .run_pure(source, target)
        .expect("compile_and_run_pure failed")
        .to_json()
}

const HEADER: &str =
    "{-# LANGUAGE NoImplicitPrelude, OverloadedStrings, DeriveGeneric, DeriveAnyClass #-}\n\
     module Expr where\n\
     import Tidepool.Prelude hiding (error)\n\n\
     data Plan = Step { what :: Text } | Seq { steps :: [Plan] } deriving (Generic, ToJSON, FromJSON, Eq)\n\n\
     roundTrips :: Plan -> Bool\n\
     roundTrips p = case fromJSON (toJSON p) of { Success v -> v == p; Error _ -> False }\n";

#[test]
fn leaf_round_trips() {
    let src = format!("{HEADER}\nresult :: Bool\nresult = roundTrips (Step \"compile\")\n");
    assert_eq!(run(&src, "result"), json!(true));
}

#[test]
fn empty_recursive_list_round_trips() {
    let src = format!("{HEADER}\nresult :: Bool\nresult = roundTrips (Seq [])\n");
    assert_eq!(run(&src, "result"), json!(true));
}

#[test]
fn nested_recursion_round_trips() {
    let src = format!(
        "{HEADER}\nresult :: Bool\n\
         result = roundTrips (Seq [Step \"a\", Seq [Step \"b\", Seq []], Step \"c\"])\n"
    );
    assert_eq!(run(&src, "result"), json!(true));
}

/// The recursive wire shape, pinned exactly: tag + record fields per level.
#[test]
fn recursive_wire_shape_is_tagged_objects() {
    let src = format!("{HEADER}\nresult :: Value\nresult = toJSON (Seq [Step \"a\", Seq []])\n");
    assert_eq!(
        run(&src, "result"),
        json!({"tag": "Seq", "steps": [{"tag": "Step", "what": "a"}, {"tag": "Seq", "steps": []}]})
    );
}

/// `tag` is the discriminator in a payload sum, so allowing a record field
/// with the same key would make `Map.fromList` silently discard one meaning.
/// Reject the type at derivation instead.
#[test]
fn payload_field_cannot_collide_with_sum_tag() {
    require_extract();
    let src = "{-# LANGUAGE NoImplicitPrelude, DeriveGeneric, DeriveAnyClass #-}\n\
        module Expr where\n\
        import Tidepool.Prelude hiding (error)\n\
        data Bad = Empty | Payload { tag :: Text } deriving (Generic, ToJSON, FromJSON)\n\
        result :: Value\n\
        result = toJSON (Payload \"lost\")\n";
    match EvalHarness::new().with_stdlib().compile(src, "result") {
        Ok(_) => panic!("a payload field named tag must not derive generic JSON"),
        Err(e) => {
            let msg = tidepool_runtime::classify_compile(&e).message;
            assert!(
                msg.contains("reserved for the constructor discriminator"),
                "expected the reserved-tag diagnostic, got:\n{msg}"
            );
        }
    }
}
