use tidepool_bridge_derive::FromCore;
use tidepool_effect::dispatch::{EffectContext, EffectHandler};
use tidepool_effect::error::EffectError;
use tidepool_eval::value::Value;
use tidepool_mcp::{CapturedOutput, DescribeEffect, EffectDecl};

// ============================================================================
// Tag 7 (base stack position 7): Llm
// ============================================================================

// LlmReq + DescribeEffect + EffectHandler dispatch are generated from the
// single-source definition; only the handler struct and the per-verb method
// bodies below are hand-written.
tidepool_mcp::llm_effect_def!(crate::effect_glue::effect_rust_projection);

pub const DEFAULT_OPENAI_MODEL: &str = "gpt-4o-mini";

pub struct LlmHandler {
    client: genai::Client,
    model: String,
    rt: tokio::runtime::Handle,
    call_count: std::sync::Arc<std::sync::atomic::AtomicU32>,
}

/// Fresh call counter per clone — see comment in original source for rationale.
impl Clone for LlmHandler {
    fn clone(&self) -> Self {
        Self {
            client: self.client.clone(),
            model: self.model.clone(),
            rt: self.rt.clone(),
            call_count: std::sync::Arc::new(std::sync::atomic::AtomicU32::new(0)),
        }
    }
}

pub const LLM_MAX_CALLS: u32 = 200;

impl LlmHandler {
    pub fn effective_model(model: String) -> String {
        let provider = std::env::var("TIDEPOOL_LLM_PROVIDER").unwrap_or_default();
        if !provider.eq_ignore_ascii_case("openai") {
            return model;
        }
        let coerced = if let Some(m) = model.strip_prefix("openai:") {
            m.to_string()
        } else if !model.is_empty() && !model.contains(':') {
            model
        } else {
            DEFAULT_OPENAI_MODEL.to_string()
        };
        tracing::warn!(
            "TIDEPOOL_LLM_PROVIDER=openai is deprecated; genai routes by model name \
             (set TIDEPOOL_LLM_MODEL={} instead)",
            coerced
        );
        coerced
    }

    pub fn normalize_model(model: String) -> String {
        const PROVIDERS: &[&str] = &[
            "ollama",
            "openai",
            "anthropic",
            "gemini",
            "groq",
            "cohere",
            "deepseek",
            "xai",
            "fireworks",
            "together",
        ];
        if let Some(idx) = model.find(':') {
            let is_legacy_prefix =
                model.as_bytes().get(idx + 1) != Some(&b':') && PROVIDERS.contains(&&model[..idx]);
            if is_legacy_prefix {
                let fixed = format!("{}::{}", &model[..idx], &model[idx + 1..]);
                tracing::info!("normalized legacy model name {model:?} -> {fixed:?}");
                return fixed;
            }
        }
        model
    }

    /// Must be called inside a tokio runtime (captures Handle::current()).
    pub fn new(model: String) -> Self {
        Self {
            client: genai::Client::default(),
            model: Self::normalize_model(Self::effective_model(model)),
            rt: tokio::runtime::Handle::current(),
            call_count: std::sync::Arc::new(std::sync::atomic::AtomicU32::new(0)),
        }
    }

    pub fn targets_openai(&self) -> bool {
        matches!(
            genai::adapter::AdapterKind::from_model(&self.model),
            Ok(genai::adapter::AdapterKind::OpenAI | genai::adapter::AdapterKind::OpenAIResp)
        )
    }

    pub fn check_rate_limit(&self) -> Result<(), EffectError> {
        let count = self
            .call_count
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        if count >= LLM_MAX_CALLS {
            Err(EffectError::Handler(format!(
                "LLM call limit exceeded ({} calls max per eval)",
                LLM_MAX_CALLS
            )))
        } else {
            Ok(())
        }
    }

    pub fn structured_core(
        &self,
        prompt: String,
        mut schema_json: serde_json::Value,
    ) -> Result<serde_json::Value, EffectError> {
        let wrapped = schema_json.get("type").and_then(|t| t.as_str()) != Some("object");
        if wrapped {
            schema_json = serde_json::json!({
                "type": "object",
                "properties": { "value": schema_json },
                "required": ["value"],
            });
        }
        if self.targets_openai() {
            strictify(&mut schema_json);
        }
        let json_spec = genai::chat::JsonSpec::new("structured_output", schema_json);
        let opts = genai::chat::ChatOptions::default()
            .with_response_format(genai::chat::ChatResponseFormat::JsonSpec(json_spec));
        let req = genai::chat::ChatRequest::from_user(format!(
            "{}\n\nRespond with ONLY valid JSON matching the provided schema. No markdown, no explanation.",
            prompt
        ));
        let resp = self
            .rt
            .block_on(self.client.exec_chat(&self.model, req, Some(&opts)))
            .map_err(|e| EffectError::Handler(format!("LLM structured call failed: {}", e)))?;
        let text = resp.first_text().unwrap_or("null");
        let mut out = serde_json::from_str(text).unwrap_or(serde_json::Value::Null);
        if wrapped {
            out = out
                .get_mut("value")
                .map(serde_json::Value::take)
                .unwrap_or(serde_json::Value::Null);
        }
        Ok(out)
    }
}

/// OpenAI strict structured outputs require EVERY property in `required`
/// (optionality = null-union). Walks every object schema and wraps originally-
/// optional properties in `{"anyOf": [orig, {"type":"null"}]}`.
pub fn strictify(schema: &mut serde_json::Value) {
    let Some(obj) = schema.as_object_mut() else {
        return;
    };
    match obj.get("type").and_then(|t| t.as_str()).unwrap_or("") {
        "object" => {
            let required: std::collections::HashSet<String> = obj
                .get("required")
                .and_then(|r| r.as_array())
                .map(|a| {
                    a.iter()
                        .filter_map(|v| v.as_str().map(String::from))
                        .collect()
                })
                .unwrap_or_default();
            let mut all_keys: Vec<serde_json::Value> = Vec::new();
            if let Some(props) = obj.get_mut("properties").and_then(|p| p.as_object_mut()) {
                for (k, v) in props.iter_mut() {
                    strictify(v);
                    if !required.contains(k.as_str()) {
                        let orig = v.take();
                        *v = serde_json::json!({"anyOf": [orig, {"type": "null"}]});
                    }
                    all_keys.push(serde_json::Value::String(k.clone()));
                }
            }
            if obj.contains_key("properties") {
                obj.insert("required".into(), serde_json::Value::Array(all_keys));
            }
        }
        "array" => {
            if let Some(items) = obj.get_mut("items") {
                strictify(items);
            }
        }
        _ => {}
    }
}

impl LlmHandler {
    fn llm_structured(
        &mut self,
        cx: &EffectContext<'_, CapturedOutput>,
        prompt: String,
        schema: Value,
    ) -> Result<tidepool_effect::Response, EffectError> {
        let schema_json = tidepool_runtime::value_to_json(&schema, cx.table(), 0);
        cx.respond(self.structured_core(prompt, schema_json)?)
    }

    fn llm_try_structured(
        &mut self,
        cx: &EffectContext<'_, CapturedOutput>,
        prompt: String,
        schema: Value,
    ) -> Result<tidepool_effect::Response, EffectError> {
        let schema_json = tidepool_runtime::value_to_json(&schema, cx.table(), 0);
        cx.respond_caught(self.structured_core(prompt, schema_json))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::*;
    use crate::{ConsoleHandler, ExecHandler, FsHandler, HttpHandler, KvHandler, LspHandler};
    use tidepool_effect::dispatch::EffectContext;

    // === Mock LLM handler for JIT structured-output tests ===

    #[derive(Clone)]
    struct MockLlmHandler {
        response: serde_json::Value,
    }

    impl DescribeEffect for MockLlmHandler {
        fn effect_decl() -> EffectDecl {
            tidepool_mcp::llm_decl()
        }
    }

    impl EffectHandler<CapturedOutput> for MockLlmHandler {
        type Request = LlmReq;
        fn handle(
            &mut self,
            req: LlmReq,
            cx: &EffectContext<'_, CapturedOutput>,
        ) -> Result<tidepool_effect::Response, EffectError> {
            match req {
                LlmReq::LlmStructured(_, _) => cx.respond(self.response.clone()),
                LlmReq::TryLlmStructured(_, _) => {
                    cx.respond_caught(Ok::<serde_json::Value, EffectError>(self.response.clone()))
                }
            }
        }
    }

    fn jit_eval_with_mock_llm(
        code: &[&str],
        mock_response: serde_json::Value,
    ) -> serde_json::Value {
        let source = jit_test_source(code);
        let include = prelude_include();
        let effects_dir =
            tidepool_mcp::ensure_effects_module(&tidepool_mcp::standard_decls()).unwrap();
        let include_paths: Vec<&std::path::Path> = vec![include.as_path(), effects_dir.as_path()];
        let kv_path = std::env::temp_dir().join("tidepool_mock_llm_kv.json");
        let cwd = repo_root();
        let captured = CapturedOutput::new();
        let mut handlers = frunk::hlist![
            ConsoleHandler,
            KvHandler::new(kv_path),
            FsHandler::new(cwd.clone()),
            HttpHandler,
            ExecHandler::new(cwd.clone()),
            LspHandler::new(cwd.clone()),
            MockLlmHandler {
                response: mock_response
            }
        ];
        let result = tidepool_runtime::compile_and_run(
            &source,
            "result",
            &include_paths,
            &mut handlers,
            &captured,
        );
        match result {
            Ok(eval_result) => eval_result.to_json(),
            Err(e) => panic!("JIT eval failed: {:?}", e),
        }
    }

    #[test]
    fn test_llm_structured_simple_object() {
        let mock = serde_json::json!({"greeting": "hello"});
        let result = jit_eval_with_mock_llm(&["llm (SObj [(\"greeting\", SStr)]) \"test\""], mock);
        assert_eq!(result["greeting"], "hello");
    }

    #[test]
    fn test_llm_structured_nested_object() {
        let mock = serde_json::json!({
            "languages": [
                {"name": "Haskell", "year": 1990},
                {"name": "Rust", "year": 2010},
                {"name": "Python", "year": 1991}
            ]
        });
        let result = jit_eval_with_mock_llm(
            &["llm (SObj [(\"languages\", SArr (SObj [(\"name\", SStr), (\"year\", SNum)]))]) \"test\""],
            mock,
        );
        let langs = result["languages"]
            .as_array()
            .expect("languages should be array");
        assert_eq!(langs.len(), 3);
        assert_eq!(langs[0]["name"], "Haskell");
    }

    #[test]
    fn test_llm_structured_encode_roundtrip() {
        let mock = serde_json::json!({"greeting": "hello"});
        let result = jit_eval_with_mock_llm(
            &[
                "r <- llm (SObj [(\"greeting\", SStr)]) \"test\"",
                "pure (object [\"result\" .= r, \"field\" .= (r ?. \"greeting\")])",
            ],
            mock,
        );
        assert_eq!(result["result"]["greeting"], "hello");
        assert_eq!(result["field"], "hello");
    }

    #[test]
    fn test_llm_structured_nested_encode_roundtrip() {
        let mock = serde_json::json!({
            "languages": [
                {"name": "Haskell", "year": 1990},
                {"name": "Rust", "year": 2010}
            ]
        });
        let result = jit_eval_with_mock_llm(
            &[
                "r <- llm (SObj [(\"languages\", SArr (SObj [(\"name\", SStr), (\"year\", SNum)]))]) \"test\"",
                "pure r",
            ],
            mock,
        );
        let langs = result["languages"]
            .as_array()
            .expect("languages should be array");
        assert_eq!(langs.len(), 2);
    }

    #[test]
    fn test_llm_structured_empty_object() {
        let mock = serde_json::json!({});
        let result = jit_eval_with_mock_llm(&["llm (SObj []) \"test\""], mock);
        assert!(result.is_object());
        assert_eq!(result.as_object().unwrap().len(), 0);
    }

    #[test]
    fn test_llm_structured_mixed_types() {
        let mock = serde_json::json!({
            "name": "test",
            "count": 42,
            "active": true
        });
        let result = jit_eval_with_mock_llm(
            &["llm (SObj [(\"name\", SStr), (\"count\", SNum), (\"active\", SBool)]) \"test\""],
            mock,
        );
        assert_eq!(result["name"], "test");
        assert_eq!(result["count"], 42);
        assert_eq!(result["active"], true);
    }

    #[test]
    fn test_llm_structured_mixed_encode_roundtrip() {
        let mock = serde_json::json!({
            "name": "test",
            "count": 42,
            "active": true
        });
        let result = jit_eval_with_mock_llm(
            &[
                "r <- llm (SObj [(\"name\", SStr), (\"count\", SNum), (\"active\", SBool)]) \"test\"",
                "pure r",
            ],
            mock,
        );
        assert_eq!(result["name"], "test");
        assert_eq!(result["count"], 42);
        assert_eq!(result["active"], true);
    }

    #[tokio::test]
    async fn test_llm_effective_model() {
        std::env::remove_var("TIDEPOOL_LLM_PROVIDER");
        assert_eq!(
            LlmHandler::effective_model("ollama:llama3.2".into()),
            "ollama:llama3.2"
        );
        assert_eq!(LlmHandler::effective_model("gpt-4o".into()), "gpt-4o");

        std::env::set_var("TIDEPOOL_LLM_PROVIDER", "openai");
        assert_eq!(
            LlmHandler::effective_model("openai:gpt-4o".into()),
            "gpt-4o"
        );
        assert_eq!(LlmHandler::effective_model("gpt-4o".into()), "gpt-4o");
        assert_eq!(
            LlmHandler::effective_model("ollama:llama3.2".into()),
            DEFAULT_OPENAI_MODEL
        );
        assert_eq!(
            LlmHandler::effective_model(String::new()),
            DEFAULT_OPENAI_MODEL
        );
        std::env::remove_var("TIDEPOOL_LLM_PROVIDER");
    }

    #[test]
    fn test_normalize_model() {
        assert_eq!(
            LlmHandler::normalize_model("ollama:llama3.2".into()),
            "ollama::llama3.2"
        );
        assert_eq!(
            LlmHandler::normalize_model("anthropic:claude-haiku-4-5".into()),
            "anthropic::claude-haiku-4-5"
        );
        assert_eq!(
            LlmHandler::normalize_model("ollama::llama3.2".into()),
            "ollama::llama3.2"
        );
        assert_eq!(
            LlmHandler::normalize_model("gpt-4o-mini".into()),
            "gpt-4o-mini"
        );
        assert_eq!(
            LlmHandler::normalize_model("qwen2.5:7b".into()),
            "qwen2.5:7b"
        );
        assert_eq!(
            LlmHandler::normalize_model("tinyllama:latest".into()),
            "tinyllama:latest"
        );
    }

    #[tokio::test]
    async fn test_llm_call_budget_resets_per_clone() {
        let handler = LlmHandler::new("gpt-4o-mini".into());
        handler
            .call_count
            .store(LLM_MAX_CALLS, std::sync::atomic::Ordering::Relaxed);
        assert!(handler.check_rate_limit().is_err());
        let fresh = handler.clone();
        assert!(fresh.check_rate_limit().is_ok());
        assert!(handler.check_rate_limit().is_err());
    }

    #[test]
    fn test_strictify_optional_fields() {
        let mut schema = serde_json::json!({
            "type": "object",
            "properties": {
                "a": {"type": "string"},
                "b": {"type": "number"}
            },
            "required": ["a"]
        });
        strictify(&mut schema);
        let req: Vec<&str> = schema["required"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_str().unwrap())
            .collect();
        assert!(req.contains(&"a") && req.contains(&"b"));
        assert_eq!(
            schema["properties"]["a"],
            serde_json::json!({"type": "string"})
        );
        assert_eq!(
            schema["properties"]["b"],
            serde_json::json!({"anyOf": [{"type": "number"}, {"type": "null"}]})
        );
    }

    #[test]
    fn test_strictify_nested_and_arrays() {
        let mut schema = serde_json::json!({
            "type": "object",
            "properties": {
                "items": {
                    "type": "array",
                    "items": {
                        "type": "object",
                        "properties": {"opt": {"type": "string"}},
                        "required": []
                    }
                }
            },
            "required": ["items"]
        });
        strictify(&mut schema);
        let inner = &schema["properties"]["items"]["items"];
        assert_eq!(inner["required"], serde_json::json!(["opt"]));
        assert_eq!(
            inner["properties"]["opt"],
            serde_json::json!({"anyOf": [{"type": "string"}, {"type": "null"}]})
        );
    }

    #[test]
    fn test_strictify_all_required_unchanged() {
        let mut schema = serde_json::json!({
            "type": "object",
            "properties": {"answer": {"type": "boolean"}},
            "required": ["answer"]
        });
        let before = schema.clone();
        strictify(&mut schema);
        assert_eq!(schema, before);
    }

    /// Live smoke against the real OpenAI API — runs only when OPENAI_API_KEY is set.
    #[tokio::test(flavor = "multi_thread")]
    async fn live_smoke_openai() {
        if std::env::var("OPENAI_API_KEY")
            .map(|k| k.trim().is_empty())
            .unwrap_or(true)
        {
            eprintln!("skipping live_smoke_openai: OPENAI_API_KEY not set");
            return;
        }
        let client = genai::Client::default();
        let model = "gpt-4o-mini";

        let resp = client
            .exec_chat(
                model,
                genai::chat::ChatRequest::from_user("Reply with the single word: pong"),
                None,
            )
            .await
            .expect("chat");
        assert!(!resp.first_text().unwrap_or("").is_empty());

        let mut schema = serde_json::json!({
            "type": "object",
            "properties": {
                "n": {"type": "number"},
                "note": {"type": "string"}
            },
            "required": ["n"]
        });
        strictify(&mut schema);
        let spec = genai::chat::JsonSpec::new("structured_output", schema);
        let opts = genai::chat::ChatOptions::default()
            .with_response_format(genai::chat::ChatResponseFormat::JsonSpec(spec));
        let resp = client
            .exec_chat(
                model,
                genai::chat::ChatRequest::from_user(
                    "Return JSON with field n set to 7. Omit or null the note field.\n\n\
                     Respond with ONLY valid JSON matching the provided schema.",
                ),
                Some(&opts),
            )
            .await
            .expect("structured");
        let parsed: serde_json::Value =
            serde_json::from_str(resp.first_text().unwrap_or("null")).unwrap();
        assert_eq!(parsed["n"], serde_json::json!(7));
    }
}
