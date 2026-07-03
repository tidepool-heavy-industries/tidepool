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
fn test_pat_error_message() {
    let src = r#"{-# LANGUAGE NoImplicitPrelude, OverloadedStrings #-}
module Test where
import Tidepool.Prelude

f :: Int -> Int
f 0 = 1
-- f 1 is missing, should trigger patError

result :: Int
result = f 1
"#;
    let res = EvalHarness::new()
        .with_stdlib()
        .run_pure(src, "result")
        .into_result();

    match res {
        Err(e) => {
            let msg = format!("{}", e);
            // patError usually contains the location and function name
            assert!(
                msg.contains("function f"),
                "Error message should contain 'function f', got: {}",
                msg
            );
        }
        Ok(_) => panic!("Expected error, got success"),
    }
}
