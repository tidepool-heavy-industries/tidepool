use tidepool_effect::dispatch::EffectContext;
use tidepool_effect::error::EffectError;
use tidepool_mcp::CapturedOutput;

// ============================================================================
// Tag 8: Time (UTC wall clock)
// ============================================================================

// TimeReq + DescribeEffect + EffectHandler dispatch are generated from the
// single-source definition; only the handler struct and the per-verb method
// bodies below are hand-written.
tidepool_mcp::time_effect_def!(crate::effect_glue::effect_rust_projection);

#[derive(Clone)]
pub struct TimeHandler;

impl TimeHandler {
    fn time_now(
        &mut self,
        cx: &EffectContext<'_, CapturedOutput>,
    ) -> Result<tidepool_effect::Response, EffectError> {
        let millis = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_err(|e| EffectError::Handler(format!("system time error: {}", e)))?
            .as_millis() as i64;
        cx.respond(millis)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::*;
    use tidepool_effect::dispatch::{DispatchEffect, EffectContext};
    use tidepool_eval::value::Value;

    #[test]
    fn test_time_dispatch_now() {
        let table = full_effect_test_table();
        let captured = CapturedOutput::new();
        let cx = EffectContext::with_user(&table, &captured);
        let mut handlers = frunk::hlist![TimeHandler];
        let con_id = table.get_by_name("TimeNow").unwrap();
        let request = Value::Con(con_id, vec![]);
        let result = response_value(handlers.dispatch(0, &request, &cx).unwrap(), &table);
        // i64::to_value boxes as Con(I#, [LitInt(n)]).
        let ms = match &result {
            Value::Con(_, fields) if fields.len() == 1 => match &fields[0] {
                Value::Lit(tidepool_repr::Literal::LitInt(n)) => *n,
                _ => panic!("Expected Con(_, [LitInt]), got {:?}", result),
            },
            _ => panic!("Expected Con(I#, [LitInt(ms)]), got {:?}", result),
        };
        assert!(ms > 0, "epoch millis should be positive, got {}", ms);
    }

    // === Time JIT e2e tests ===

    fn time_jit_handlers(
        cwd: std::path::PathBuf,
        kv_path: std::path::PathBuf,
    ) -> impl tidepool_effect::dispatch::DispatchEffect<CapturedOutput> {
        crate::build_base_stack(&crate::HandlerConfig {
            cwd,
            kv_path,
            llm_model: "ollama:llama3.2".to_string(),
        })
    }

    /// Bundles `test_jit_time_format_golden` (pure computation — no Time
    /// effect dispatch; tests civil_from_days + formatting on two literal
    /// `UTCTime`s) + `test_jit_time_now_e2e` (dispatches the real Time effect
    /// via `getCurrentTime`, checked against the Rust wall clock) into one
    /// tidepool-extract compile.
    #[tokio::test]
    async fn test_jit_time_family() {
        if !tidepool_testing::eval_harness::extract_available() {
            eprintln!("skipping: tidepool-extract not available (set TIDEPOOL_EXTRACT)");
            return;
        }
        let decls = tidepool_mcp::standard_decls();
        let source = jit_test_source(&[
            "let t0 = UTCTime 0",
            "let t1 = UTCTime 1709164800000",
            "let s0 = formatISO8601 t0",
            "let s1 = formatISO8601 t1",
            "t <- getCurrentTime",
            "pure (object [\"golden\" .= [s0, s1], \"nowMs\" .= epochMillis t])",
        ]);
        let include = prelude_include();
        let effects_dir = tidepool_mcp::ensure_effects_module(&decls).unwrap();
        let include_paths: Vec<&std::path::Path> = vec![include.as_path(), effects_dir.as_path()];
        let kv_path = std::env::temp_dir().join("tidepool_time_family_kv.json");
        let cwd = repo_root();
        let captured = CapturedOutput::new();
        let mut handlers = time_jit_handlers(cwd, kv_path);
        let rust_before = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_millis() as i64;
        let result = tidepool_runtime::compile_and_run(
            &source,
            "result",
            &include_paths,
            &mut handlers,
            &captured,
        );
        let rust_after = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_millis() as i64;
        match result {
            Ok(v) => {
                let json = v.to_json();
                assert_eq!(
                    json["golden"],
                    serde_json::json!(["1970-01-01T00:00:00Z", "2024-02-29T00:00:00Z"]),
                    "golden ISO-8601 timestamps ([epoch, leap-day] observed)"
                );
                // Return observed [ms, rust_before, rust_after] on failure so we can see
                // the actual values rather than just a collapsed bool.
                let ms = json["nowMs"].as_i64().unwrap_or_else(|| {
                    panic!("expected i64 epoch millis, got {:?}", json["nowMs"])
                });
                let five_min_ms = 5 * 60 * 1000_i64;
                assert!(
                    ms >= rust_before - five_min_ms && ms <= rust_after + five_min_ms,
                    "getCurrentTime returned {} ms; expected in [{}, {}] (±5 min of Rust wall clock)",
                    ms,
                    rust_before - five_min_ms,
                    rust_after + five_min_ms
                );
            }
            Err(e) => panic!("JIT time family eval failed: {:?}", e),
        }
    }
}
