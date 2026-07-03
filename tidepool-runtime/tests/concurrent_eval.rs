use std::thread;
use tidepool_testing::eval_harness::EvalHarness;

fn run_pure(src: &str, target: &str) -> serde_json::Value {
    EvalHarness::new().with_stdlib().run_pure(src, target).json()
}

#[test]
fn test_concurrent_eval_pure() {
    let mut handles = vec![];

    // Thread 1: Simple math (explicit Int to avoid Integer defaulting)
    handles.push(thread::spawn(|| {
        let json = run_pure("module T1 where\nval :: Int\nval = 2 + 2", "val");
        assert_eq!(json, serde_json::json!(4));
    }));

    // Thread 2: String concatenation
    handles.push(thread::spawn(|| {
        let json = run_pure("module T2 where\nval = \"hello \" <> \"world\"", "val");
        assert_eq!(json, serde_json::json!("hello world"));
    }));

    // Thread 3: List operations (explicit [Int] to avoid Integer/gmpn_cmp)
    handles.push(thread::spawn(|| {
        let json = run_pure(
            "module T3 where\nimport Data.List (sort)\nval :: [Int]\nval = sort [3, 1, 2]",
            "val",
        );
        assert_eq!(json, serde_json::json!([1, 2, 3]));
    }));

    // Thread 4: Higher-order functions (explicit [Int])
    handles.push(thread::spawn(|| {
        let json = run_pure(
            "module T4 where\nval :: [Int]\nval = map (+1) [1, 2, 3]",
            "val",
        );
        assert_eq!(json, serde_json::json!([2, 3, 4]));
    }));

    for handle in handles {
        handle.join().expect("Thread panicked");
    }
}
