//! Tavily transport behind a domain capability, with no caller-selected headers or endpoint.
pub use crate::generated::web::{WebError, WebReq};
use serde_json::{json, Value};
use std::io::Read;
use std::time::Duration;

// This is a transport allocation safeguard, not a truncation policy. Refusal
// reports the budget; callers can request fewer search results or another URL.
const RESPONSE_BYTES: u64 = 16 * 1024 * 1024;

#[derive(Clone, Default)]
pub struct WebHandler;

impl WebHandler {
    fn post(&self, operation: &str, body: Value) -> Result<Value, WebError> {
        let key = std::env::var("TAVILY_API_KEY")
            .ok()
            .filter(|key| !key.trim().is_empty())
            .ok_or_else(|| WebError::WebUnavailable("TAVILY_API_KEY is not configured".into()))?;
        let agent = ureq::AgentBuilder::new()
            .redirects(0)
            .timeout(Duration::from_secs(45))
            .build();
        let response = agent
            .post(&format!("https://api.tavily.com/{operation}"))
            .set("Authorization", &format!("Bearer {key}"))
            .send_json(body)
            .map_err(|error| match error {
                // Never expose provider bodies, which can echo credentials or query data.
                ureq::Error::Status(status, _) => {
                    WebError::WebProvider(format!("Tavily {operation}: HTTP {status}"))
                }
                ureq::Error::Transport(_) => {
                    WebError::WebTransport(format!("Tavily {operation}: transport failed"))
                }
            })?;
        let mut bytes = Vec::new();
        response
            .into_reader()
            .take(RESPONSE_BYTES + 1)
            .read_to_end(&mut bytes)
            .map_err(|_| WebError::WebTransport("response body read failed".into()))?;
        if bytes.len() as u64 > RESPONSE_BYTES {
            return Err(WebError::WebBudgetExceeded(format!(
                "response exceeds {RESPONSE_BYTES} bytes"
            )));
        }
        serde_json::from_slice(&bytes)
            .map_err(|_| WebError::WebDecode("invalid JSON response".into()))
    }

    pub(crate) fn web_search(
        &mut self,
        query: String,
        limit: i64,
        include_domains: Vec<String>,
        exclude_domains: Vec<String>,
    ) -> Result<Value, WebError> {
        let body = search_body(&query, limit, include_domains, exclude_domains)?;
        let response = self.post("search", body)?;
        let results = response
            .get("results")
            .and_then(Value::as_array)
            .ok_or_else(|| WebError::WebDecode("search response has no results array".into()))?;
        let results = results.iter().map(|hit| {
            Ok(json!({"title": required_text(hit, "title")?, "url": required_text(hit, "url")?, "content": required_text(hit, "content")?}))
        }).collect::<Result<Vec<_>, WebError>>()?;
        Ok(json!(results))
    }

    pub(crate) fn web_follow(&mut self, base: String, target: String) -> Result<Value, WebError> {
        let url = url::Url::parse(&base)
            .and_then(|base| base.join(&target))
            .map_err(|_| WebError::WebInvalidRequest("invalid base URL or link target".into()))?;
        self.web_extract(url.into())
    }

    pub(crate) fn web_extract(&mut self, url: String) -> Result<Value, WebError> {
        let parsed = url::Url::parse(&url).map_err(|_| {
            WebError::WebInvalidRequest("expected an absolute public HTTP(S) URL".into())
        })?;
        if !matches!(parsed.scheme(), "http" | "https")
            || parsed.host_str().is_none()
            || !parsed.username().is_empty()
            || parsed.password().is_some()
        {
            return Err(WebError::WebInvalidRequest(
                "expected an HTTP(S) URL without credentials".into(),
            ));
        }
        let response = self.post("extract", extract_body(&url))?;
        extracted_page(&response)
    }
}

fn required_text<'a>(object: &'a Value, key: &str) -> Result<&'a str, WebError> {
    object
        .get(key)
        .and_then(Value::as_str)
        .ok_or_else(|| WebError::WebDecode(format!("missing text field {key}")))
}
fn search_body(
    query: &str,
    limit: i64,
    include_domains: Vec<String>,
    exclude_domains: Vec<String>,
) -> Result<Value, WebError> {
    if query.trim().is_empty() || !(1..=20).contains(&limit) {
        return Err(WebError::WebInvalidRequest(
            "query must be nonempty and max results must be 1–20".into(),
        ));
    }
    Ok(
        json!({"query": query, "max_results": limit, "search_depth": "basic", "include_answer": false, "include_raw_content": false, "include_domains": include_domains, "exclude_domains": exclude_domains}),
    )
}
fn extract_body(url: &str) -> Value {
    // Supplying a query would ask Tavily to select chunks before our Jev experiment.
    json!({"urls": [url], "extract_depth": "basic", "format": "markdown"})
}
fn extracted_page(response: &Value) -> Result<Value, WebError> {
    let rows = response
        .get("results")
        .and_then(Value::as_array)
        .ok_or_else(|| WebError::WebDecode("extract response has no results array".into()))?;
    let page = rows
        .first()
        .ok_or_else(|| WebError::WebProvider("Tavily could not extract this URL".into()))?;
    let url = required_text(page, "url")?;
    let content = required_text(page, "raw_content")?;
    let reference = blake3::hash(format!("{url}\0{content}").as_bytes())
        .to_hex()
        .to_string();
    Ok(json!({"url": url, "content": content, "reference": reference}))
}
impl tidepool_mcp::InstalledEffectSupport for WebHandler {
    fn installed_effect_support(&self) -> Vec<exomonad_tool::ToolEffectKey> {
        vec![exomonad_tool::ActorEffectKey::Web.into()]
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn retrieval_does_not_preselect_content_or_generate_answers() {
        let search = search_body("curious query", 3, vec!["example.org".into()], vec![]).unwrap();
        assert_eq!(search["search_depth"], "basic");
        assert_eq!(search["include_answer"], false);
        let extract = extract_body("https://example.org/");
        assert_eq!(extract["extract_depth"], "basic");
        assert!(extract.get("query").is_none());
        assert!(extract.get("chunks_per_source").is_none());
        assert!(search_body("", 3, vec![], vec![]).is_err());
        assert!(search_body("x", 21, vec![], vec![]).is_err());
    }
    #[test]
    fn extracted_evidence_is_complete_and_content_addressed() {
        let input = json!({"results": [{"url":"https://example.org/", "raw_content":"opening\n\n[branch](https://example.org/b)\n\nending"}]});
        let first = extracted_page(&input).unwrap();
        assert_eq!(first["content"], input["results"][0]["raw_content"]);
        assert_eq!(first, extracted_page(&input).unwrap());
        let mut changed = input.clone();
        changed["results"][0]["raw_content"] = json!("different");
        assert_ne!(
            first["reference"],
            extracted_page(&changed).unwrap()["reference"]
        );
        assert!(matches!(
            extracted_page(&json!({"results":[], "failed_results":[{}]})),
            Err(WebError::WebProvider(_))
        ));
        assert!(matches!(
            extracted_page(&json!({"results":[{}]})),
            Err(WebError::WebDecode(_))
        ));
    }
}
