use tidepool_testing::eval_harness::EvalHarness;

#[test]
fn test_error_message() {
    let src = r#"{-# LANGUAGE NoImplicitPrelude, OverloadedStrings #-}
module Test where
import Tidepool.Prelude

{-# NOINLINE f #-}
f :: Int -> Int
f x = if x == 0 then error "head: empty list" else x

result :: Int
result = f 0
"#;
    let res = EvalHarness::new()
        .with_stdlib()
        .run_pure(src, "result")
        .into_result();

    match res {
        Err(e) => {
            let msg = format!("{}", e);
            assert!(
                msg.contains("head: empty list"),
                "Error message should contain 'head: empty list', got: {}",
                msg
            );
        }
        Ok(_) => panic!("Expected error, got success"),
    }
}

#[test]
fn first_class_error_function_is_whnf() {
    let src = include_str!("fixtures/strict-demand/FirstClassError.hs");
    let value = EvalHarness::new().with_stdlib().run_pure(src, "result");
    assert_eq!(value.json(), serde_json::json!(42));
}
