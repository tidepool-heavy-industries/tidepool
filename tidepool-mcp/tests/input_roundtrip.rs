//! Regression guard for #315: bare JSON-string `input` param double-encodes.
//!
//! The fix landed in commit 48a7dc3 (`fix(313/315): input string unwrap, …`):
//! `normalize_input` now unwraps stringified bare-string payloads in addition
//! to the already-fixed object/array case.
//!
//! Fast unit-level guards for the normalize → source-generation pipeline live
//! in `tidepool-mcp/src/lib.rs` (`test_input_source_gen_*`). The tests in
//! this file exercise the full `compile_and_run` path (one GHC invoke each,
//! ~90s/test) and are **`#[ignore]`** to keep `cargo test` fast. Run them
//! explicitly:
//!
//! ```text
//! cargo test -p tidepool-mcp --test input_roundtrip -- --ignored
//! ```

#[cfg(test)]
mod jit_roundtrip {
    use serde_json::json;
    use std::path::Path;
    use tidepool_effect::DispatchEffect;
    use tidepool_eval::value::Value;
    use tidepool_runtime::compile_and_run;

    fn prelude_dir() -> &'static Path {
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .unwrap()
            .join("haskell/lib")
            .leak()
    }

    fn user_lib_dir() -> &'static Path {
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .unwrap()
            .join(".tidepool/lib")
            .leak()
    }

    struct MockDispatcher;

    impl DispatchEffect<()> for MockDispatcher {
        fn dispatch(
            &mut self,
            tag: u64,
            _request: &Value,
            _cx: &tidepool_effect::EffectContext<'_, ()>,
        ) -> Result<tidepool_effect::Response, tidepool_effect::error::EffectError> {
            Err(tidepool_effect::error::EffectError::UnhandledEffect { tag })
        }
    }

    /// Compile and run `code` with `raw_input` as the payload, applying
    /// `normalize_input` as the server does before injection.
    fn run_with_input(code: &str, raw_input: serde_json::Value) -> serde_json::Value {
        let normalized = tidepool_mcp::normalize_input(&raw_input);
        let decls = tidepool_mcp::standard_decls();
        let preamble = tidepool_mcp::build_preamble(&decls, true);
        let stack = tidepool_mcp::build_effect_stack_type(&decls);
        let source = tidepool_mcp::template_haskell(
            &preamble,
            &stack,
            code,
            "",
            "",
            Some(&normalized),
            None,
        );

        let pp = prelude_dir();
        let ulp = user_lib_dir();
        assert!(
            ulp.join("Library.hs").exists(),
            ".tidepool/lib/Library.hs not found"
        );
        let eff = tidepool_mcp::ensure_effects_module(&decls)
            .expect("write effects module")
            .leak() as &std::path::Path;
        let include = [pp, ulp, eff];

        let mut dispatcher = MockDispatcher;
        compile_and_run(&source, "result", &include, &mut dispatcher, &())
            .expect("compile_and_run failed")
            .to_json()
    }

    /// THE #315 CASE: an MCP client that JSON-encodes the payload produces a
    /// `String("\"hello world\"")`. After `normalize_input` the quotes must
    /// unwrap so Haskell sees `Aeson.String "hello world"`, not a
    /// double-quoted literal.
    #[test]
    #[ignore = "slow: ~90s GHC compile per test; run with --ignored when needed"]
    fn issue_315_double_encoded_string_roundtrip() {
        let raw = json!("\"hello world\"");
        assert_eq!(
            run_with_input(
                r#"case input of { Aeson.String s -> pure s; _ -> pure "wrong shape" }"#,
                raw
            ),
            json!("hello world"),
        );
    }

    /// Plain string (not double-encoded) passes through unchanged.
    #[test]
    #[ignore = "slow: ~90s GHC compile per test; run with --ignored when needed"]
    fn plain_string_passthrough() {
        assert_eq!(
            run_with_input(
                r#"case input of { Aeson.String s -> pure s; _ -> pure "wrong shape" }"#,
                json!("hello world"),
            ),
            json!("hello world"),
        );
    }

    /// Number input: extract and return as Int.
    #[test]
    #[ignore = "slow: ~90s GHC compile per test; run with --ignored when needed"]
    fn number_input_roundtrip() {
        assert_eq!(
            run_with_input(
                r#"case input of { Aeson.NumberI n -> pure n; _ -> pure (-999 :: Int) }"#,
                json!(42),
            ),
            json!(42),
        );
    }

    /// Bool input: extract and return.
    #[test]
    #[ignore = "slow: ~90s GHC compile per test; run with --ignored when needed"]
    fn bool_input_roundtrip() {
        assert_eq!(
            run_with_input(
                r#"case input of { Aeson.Bool b -> pure b; _ -> error "wrong shape" }"#,
                json!(true),
            ),
            json!(true),
        );
    }

    /// Object input: extract a key's string value.
    #[test]
    #[ignore = "slow: ~90s GHC compile per test; run with --ignored when needed"]
    fn object_input_roundtrip() {
        assert_eq!(
            run_with_input(
                r#"case input ^? key "greeting" . _String of { Just s -> pure s; Nothing -> pure "missing" }"#,
                json!({"greeting": "hello", "n": 1}),
            ),
            json!("hello"),
        );
    }

    /// Array input: extract element count.
    #[test]
    #[ignore = "slow: ~90s GHC compile per test; run with --ignored when needed"]
    fn array_input_roundtrip() {
        assert_eq!(
            run_with_input(
                r#"case input of { Aeson.Array xs -> pure (P.length xs :: Int); _ -> pure (-1 :: Int) }"#,
                json!(["a", "b", "c"]),
            ),
            json!(3),
        );
    }
}
