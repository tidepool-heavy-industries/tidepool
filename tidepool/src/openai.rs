//! OpenAI-compatible chat-completions provider for the Llm effect.
//! Used only when TIDEPOOL_LLM_PROVIDER=openai; the default path stays on genai.
use std::time::Duration;
use tidepool_effect::error::EffectError;

/// Cheapest solid structured-output-capable model as of this writing.
pub const DEFAULT_OPENAI_MODEL: &str = "gpt-4o-mini";
const DEFAULT_BASE_URL: &str = "https://api.openai.com/v1";
const TIMEOUT_SECS: u64 = 60;

/// Resolve the OpenAI model name from the server's `--llm` / TIDEPOOL_LLM_MODEL
/// value. Strips an `openai:` prefix; accepts a bare model name; otherwise
/// (empty, or a different provider prefix like `ollama:`/`anthropic:`) falls
/// back to DEFAULT_OPENAI_MODEL.
pub fn resolve_model(llm: &str) -> String {
    if let Some(m) = llm.strip_prefix("openai:") {
        m.to_string()
    } else if !llm.is_empty() && !llm.contains(':') {
        llm.to_string()
    } else {
        DEFAULT_OPENAI_MODEL.to_string()
    }
}

#[derive(Clone)]
pub struct OpenAiClient {
    agent: ureq::Agent,
    api_key: String,
    base_url: String,
    model: String,
}

impl OpenAiClient {
    /// Build from environment. Requires OPENAI_API_KEY; honors optional
    /// OPENAI_BASE_URL (default https://api.openai.com/v1). Returns Err if the
    /// key is unset/empty so the caller can fall back to the default backend.
    pub fn from_env(model: String) -> Result<Self, EffectError> {
        let api_key = std::env::var("OPENAI_API_KEY")
            .ok()
            .filter(|k| !k.trim().is_empty())
            .ok_or_else(|| EffectError::Handler("OPENAI_API_KEY not set".to_string()))?;
        let base_url = std::env::var("OPENAI_BASE_URL")
            .ok()
            .filter(|u| !u.trim().is_empty())
            .unwrap_or_else(|| DEFAULT_BASE_URL.to_string());
        let agent = ureq::AgentBuilder::new()
            .timeout(Duration::from_secs(TIMEOUT_SECS))
            .build();
        Ok(Self {
            agent,
            api_key,
            base_url,
            model,
        })
    }

    /// Plain-text chat completion (mirrors LlmChat).
    pub fn chat(&self, prompt: &str) -> Result<String, EffectError> {
        let body = build_chat_body(&self.model, prompt);
        let resp = self.post_chat(body)?;
        parse_chat_content(&resp)
    }

    /// JSON-schema-constrained completion (mirrors LlmStructured). `schema` is
    /// the JSON-schema Value the Haskell side built via schemaToValue.
    pub fn structured(
        &self,
        prompt: &str,
        schema: serde_json::Value,
    ) -> Result<serde_json::Value, EffectError> {
        let body = build_structured_body(&self.model, prompt, schema);
        let resp = self.post_chat(body)?;
        parse_structured_content(&resp)
    }

    fn post_chat(&self, body: serde_json::Value) -> Result<serde_json::Value, EffectError> {
        let url = format!("{}/chat/completions", self.base_url.trim_end_matches('/'));
        let result = self
            .agent
            .post(&url)
            .set("Authorization", &format!("Bearer {}", self.api_key))
            .set("Content-Type", "application/json")
            .send_json(body);
        match result {
            Ok(resp) => resp
                .into_json::<serde_json::Value>()
                .map_err(|e| EffectError::Handler(format!("OpenAI: invalid JSON response: {}", e))),
            Err(ureq::Error::Status(code, resp)) => {
                let body = resp.into_string().unwrap_or_default();
                Err(EffectError::Handler(sanitize_error(code, &body)))
            }
            Err(ureq::Error::Transport(t)) => Err(EffectError::Handler(format!(
                "OpenAI request failed: {}",
                t
            ))),
        }
    }
}

fn build_chat_body(model: &str, prompt: &str) -> serde_json::Value {
    serde_json::json!({
        "model": model,
        "messages": [{"role": "user", "content": prompt}],
    })
}

fn build_structured_body(
    model: &str,
    prompt: &str,
    schema: serde_json::Value,
) -> serde_json::Value {
    // Mirror the genai path's instruction so behavior matches across providers.
    let content = format!(
        "{}\n\nRespond with ONLY valid JSON matching the provided schema. No markdown, no explanation.",
        prompt
    );
    serde_json::json!({
        "model": model,
        "messages": [{"role": "user", "content": content}],
        "response_format": {
            "type": "json_schema",
            "json_schema": {
                "name": "structured_output",
                // strict=false: the Haskell schemaToValue output uses optional
                // fields not listed in `required`, which strict mode forbids.
                // Non-strict still steers output and mirrors the genai contract.
                "strict": false,
                "schema": schema
            }
        }
    })
}

/// Extract assistant text from a chat-completions response. Missing content
/// yields "" (mirrors genai first_text().unwrap_or("")).
fn parse_chat_content(resp: &serde_json::Value) -> Result<String, EffectError> {
    if let Some(refusal) = resp
        .pointer("/choices/0/message/refusal")
        .and_then(|v| v.as_str())
    {
        return Err(EffectError::Handler(format!("OpenAI refused: {}", refusal)));
    }
    Ok(resp
        .pointer("/choices/0/message/content")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string())
}

/// Extract + parse the JSON content of a structured response. A refusal maps to
/// an error; unparseable/missing content maps to JSON null (mirrors the genai
/// structured path's unwrap_or(Value::Null)).
fn parse_structured_content(resp: &serde_json::Value) -> Result<serde_json::Value, EffectError> {
    if let Some(refusal) = resp
        .pointer("/choices/0/message/refusal")
        .and_then(|v| v.as_str())
    {
        return Err(EffectError::Handler(format!("OpenAI refused: {}", refusal)));
    }
    let content = resp
        .pointer("/choices/0/message/content")
        .and_then(|v| v.as_str())
        .unwrap_or("null");
    Ok(serde_json::from_str(content).unwrap_or(serde_json::Value::Null))
}

/// Build a safe error string from an HTTP error response. Surfaces the OpenAI
/// `error.message` field if present (never the request, never the key),
/// truncated to a sane length.
fn sanitize_error(status: u16, body: &str) -> String {
    let msg = serde_json::from_str::<serde_json::Value>(body)
        .ok()
        .and_then(|v| {
            v.pointer("/error/message")
                .and_then(|m| m.as_str())
                .map(str::to_string)
        })
        .unwrap_or_else(|| "<no message>".to_string());
    let mut msg = msg;
    if msg.len() > 500 {
        msg.truncate(500);
    }
    format!("OpenAI API error (HTTP {}): {}", status, msg)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn test_resolve_model() {
        assert_eq!(resolve_model("openai:gpt-4o"), "gpt-4o");
        assert_eq!(resolve_model("gpt-4o"), "gpt-4o");
        assert_eq!(resolve_model("ollama:llama3.2"), DEFAULT_OPENAI_MODEL);
        assert_eq!(resolve_model("anthropic:x"), DEFAULT_OPENAI_MODEL);
        assert_eq!(resolve_model(""), DEFAULT_OPENAI_MODEL);
    }

    #[test]
    fn test_build_chat_body() {
        let body = build_chat_body("m", "p");
        assert_eq!(body["model"], "m");
        assert_eq!(body["messages"][0]["role"], "user");
        assert_eq!(body["messages"][0]["content"], "p");
    }

    #[test]
    fn test_build_structured_body() {
        let schema = json!({"type": "object"});
        let body = build_structured_body("m", "p", schema.clone());
        assert_eq!(body["model"], "m");
        let content = body["messages"][0]["content"].as_str().unwrap();
        assert!(content.starts_with("p"));
        assert!(content.contains("Respond with ONLY valid JSON"));
        assert_eq!(body["response_format"]["type"], "json_schema");
        assert_eq!(
            body["response_format"]["json_schema"]["name"],
            "structured_output"
        );
        assert_eq!(body["response_format"]["json_schema"]["schema"], schema);
        assert_eq!(body["response_format"]["json_schema"]["strict"], false);
    }

    #[test]
    fn test_parse_chat_content() {
        let resp = json!({"choices":[{"message":{"content":"Hello!"}}]});
        assert_eq!(parse_chat_content(&resp).unwrap(), "Hello!");

        let resp = json!({});
        assert_eq!(parse_chat_content(&resp).unwrap(), "");

        let resp = json!({"choices":[{"message":{"refusal":"no"}}]});
        let err = parse_chat_content(&resp).unwrap_err();
        assert!(format!("{}", err).contains("refused"));
    }

    #[test]
    fn test_parse_structured_content() {
        let resp = json!({"choices":[{"message":{"content":"{\"answer\":42}"}}]});
        assert_eq!(
            parse_structured_content(&resp).unwrap(),
            json!({"answer": 42})
        );

        let resp = json!({"choices":[{"message":{"content":"garbage"}}]});
        assert_eq!(
            parse_structured_content(&resp).unwrap(),
            serde_json::Value::Null
        );

        let resp = json!({"choices":[{"message":{"refusal":"no"}}]});
        let err = parse_structured_content(&resp).unwrap_err();
        assert!(format!("{}", err).contains("refused"));
    }

    #[test]
    fn test_sanitize_error() {
        let body = json!({"error":{"message":"Incorrect API key provided"}}).to_string();
        let err = sanitize_error(401, &body);
        assert!(err.contains("HTTP 401"));
        assert!(err.contains("Incorrect API key provided"));
        assert!(!err.contains("Bearer"));
    }

    #[test]
    fn live_smoke_openai() {
        if std::env::var("OPENAI_API_KEY")
            .map(|k| k.trim().is_empty())
            .unwrap_or(true)
        {
            eprintln!("skipping live_smoke_openai: OPENAI_API_KEY not set");
            return;
        }
        let client = OpenAiClient::from_env(resolve_model("gpt-4o-mini")).expect("client");
        let out = client
            .chat("Reply with the single word: pong")
            .expect("chat");
        assert!(!out.is_empty());
        let schema = serde_json::json!({"type":"object","properties":{"n":{"type":"number"}},"required":["n"]});
        let v = client
            .structured("Return JSON with field n set to 7.", schema)
            .expect("structured");
        assert!(v.is_object() || v.is_null());
    }
}
