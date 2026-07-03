use tidepool_bridge_derive::FromCore;
use tidepool_effect::dispatch::{EffectContext, EffectHandler};
use tidepool_effect::error::EffectError;
use tidepool_mcp::{CapturedOutput, DescribeEffect, EffectDecl};

// ============================================================================
// Tag 9: Time (UTC wall clock)
// ============================================================================

#[derive(FromCore)]
pub enum TimeReq {
    #[core(name = "TimeNow")]
    Now,
}

#[derive(Clone)]
pub struct TimeHandler;

impl DescribeEffect for TimeHandler {
    fn effect_decl() -> EffectDecl {
        tidepool_mcp::time_decl()
    }
}

impl EffectHandler<CapturedOutput> for TimeHandler {
    type Request = TimeReq;

    fn handle(
        &mut self,
        req: TimeReq,
        cx: &EffectContext<'_, CapturedOutput>,
    ) -> Result<tidepool_effect::Response, EffectError> {
        match req {
            TimeReq::Now => {
                let millis = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map_err(|e| EffectError::Handler(format!("system time error: {}", e)))?
                    .as_millis() as i64;
                cx.respond(millis)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::*;
    use tidepool_bridge::FromCore;
    use tidepool_effect::dispatch::{DispatchEffect, EffectContext};
    use tidepool_eval::value::Value;

    #[test]
    fn test_time_from_core_now() {
        let table = full_effect_test_table();
        let con_id = table.get_by_name("TimeNow").unwrap();
        let val = Value::Con(con_id, vec![]);
        let req = TimeReq::from_value(&val, &table).unwrap();
        assert!(matches!(req, TimeReq::Now));
    }

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
        frunk::hlist![
            crate::ConsoleHandler,
            crate::KvHandler::new(kv_path),
            crate::FsHandler::new(cwd.clone()),
            crate::HttpHandler,
            crate::ExecHandler::new(cwd.clone()),
            crate::LspHandler::new(cwd.clone()),
            crate::LlmHandler::new("ollama:llama3.2".to_string()),
            crate::GitHandler::new(cwd.clone()),
            TimeHandler,
        ]
    }

    fn extract_available() -> bool {
        let bin =
            std::env::var("TIDEPOOL_EXTRACT").unwrap_or_else(|_| "tidepool-extract".to_string());
        std::process::Command::new(&bin)
            .arg("--help")
            .output()
            .is_ok()
    }

    #[tokio::test]
    async fn test_jit_time_format_golden() {
        if !extract_available() {
            eprintln!("skipping: tidepool-extract not available (set TIDEPOOL_EXTRACT)");
            return;
        }
        let decls = tidepool_mcp::standard_decls();
        // Pure computation — no Time effect dispatch; tests civil_from_days + formatting.
        let source = jit_test_source(&[
            "let t0 = UTCTime 0",
            "let t1 = UTCTime 1709164800000",
            "let s0 = formatISO8601 t0",
            "let s1 = formatISO8601 t1",
            "pure (toJSON [s0, s1])",
        ]);
        let include = prelude_include();
        let effects_dir = tidepool_mcp::ensure_effects_module(&decls).unwrap();
        let include_paths: Vec<&std::path::Path> = vec![include.as_path(), effects_dir.as_path()];
        let kv_path = std::env::temp_dir().join("tidepool_time_golden_kv.json");
        let cwd = repo_root();
        let captured = CapturedOutput::new();
        let mut handlers = time_jit_handlers(cwd, kv_path);
        let result = tidepool_runtime::compile_and_run(
            &source,
            "result",
            &include_paths,
            &mut handlers,
            &captured,
        );
        match result {
            Ok(v) => assert_eq!(
                v.to_json(),
                serde_json::json!(["1970-01-01T00:00:00Z", "2024-02-29T00:00:00Z"]),
                "golden ISO-8601 timestamps ([epoch, leap-day] observed)"
            ),
            Err(e) => panic!("JIT time golden eval failed: {:?}", e),
        }
    }

    #[tokio::test]
    async fn test_jit_time_now_e2e() {
        if !extract_available() {
            eprintln!("skipping: tidepool-extract not available (set TIDEPOOL_EXTRACT)");
            return;
        }
        let decls = tidepool_mcp::standard_decls();
        let source = jit_test_source(&["t <- getCurrentTime", "pure (toJSON (epochMillis t))"]);
        let include = prelude_include();
        let effects_dir = tidepool_mcp::ensure_effects_module(&decls).unwrap();
        let include_paths: Vec<&std::path::Path> = vec![include.as_path(), effects_dir.as_path()];
        let kv_path = std::env::temp_dir().join("tidepool_time_now_kv.json");
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
                // Return observed [ms, rust_before, rust_after] on failure so we can see
                // the actual values rather than just a collapsed bool.
                let ms = v
                    .to_json()
                    .as_i64()
                    .unwrap_or_else(|| panic!("expected i64 epoch millis, got {:?}", v.to_json()));
                let five_min_ms = 5 * 60 * 1000_i64;
                assert!(
                    ms >= rust_before - five_min_ms && ms <= rust_after + five_min_ms,
                    "getCurrentTime returned {} ms; expected in [{}, {}] (±5 min of Rust wall clock)",
                    ms,
                    rust_before - five_min_ms,
                    rust_after + five_min_ms
                );
            }
            Err(e) => panic!("JIT getCurrentTime eval failed: {:?}", e),
        }
    }
}
