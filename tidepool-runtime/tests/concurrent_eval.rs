use std::path::{Path, PathBuf};
use tidepool_runtime::{compile_and_run_pure, EvalResult, value_to_json};
use std::thread;

fn prelude_path() -> PathBuf {
    let manifest = Path::new(env!("CARGO_MANIFEST_DIR"));
    manifest.parent().unwrap().join("haskell").join("lib")
}

fn run_pure(src: &str, target: &str) -> EvalResult {
    let pp = prelude_path();
    compile_and_run_pure(src, target, &[pp.as_path()]).expect("Run failed")
}

#[test]
fn test_concurrent_eval_pure() {
    let mut handles = vec![];

    // Thread 1: Simple math
    handles.push(thread::spawn(|| {
        let res = run_pure("module T1 where\nval = 2 + 2", "val");
        let json = value_to_json(res.value(), res.table(), 0);
        assert_eq!(json, serde_json::json!(4));
    }));

    // Thread 2: String concatenation
    handles.push(thread::spawn(|| {
        let res = run_pure("module T2 where\nval = \"hello \" <> \"world\"", "val");
        let json = value_to_json(res.value(), res.table(), 0);
        assert_eq!(json, serde_json::json!("hello world"));
    }));

    // Thread 3: List operations
    handles.push(thread::spawn(|| {
        let res = run_pure("module T3 where\nimport Data.List (sort)\nval = sort [3, 1, 2]", "val");
        let json = value_to_json(res.value(), res.table(), 0);
        assert_eq!(json, serde_json::json!([1, 2, 3]));
    }));

    // Thread 4: Higher-order functions
    handles.push(thread::spawn(|| {
        let res = run_pure("module T4 where\nval = map (+1) [1, 2, 3]", "val");
        let json = value_to_json(res.value(), res.table(), 0);
        assert_eq!(json, serde_json::json!([2, 3, 4]));
    }));

    for handle in handles {
        handle.join().expect("Thread panicked");
    }
}
