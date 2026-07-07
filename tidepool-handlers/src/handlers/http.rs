// ============================================================================
// Tag 4: Http
// ============================================================================

// HttpReq + DescribeEffect + EffectHandler dispatch are generated from the
// single-source definition; only the handler struct and the per-verb method
// bodies below are hand-written.
tidepool_mcp::http_effect_def!(crate::effect_glue::effect_rust_projection);

/// Headroom below the machine's `MAX_EFFECT_RESPONSE_NODES` (100_000, in
/// `tidepool_codegen::jit_machine`). The count from `bridged_node_count` is the
/// exact size of the response `Value`; the machine additionally counts the
/// `Right`/effect-envelope nodes wrapping it, so this leaves room for those and
/// guarantees a response that passes here can NEVER hit the generic abort. The
/// rejected band (bridged 90k–100k) is only genuinely-huge responses.
const MAX_RESPONSE_NODES: usize = 90_000;

#[derive(Clone)]
pub struct HttpHandler;

impl HttpHandler {
    pub fn validate_url(url_str: &str) -> Result<url::Url, HttpError> {
        let url = url::Url::parse(url_str)
            .map_err(|e| HttpError::HttpInvalidUrl(format!("Invalid URL '{}': {}", url_str, e)))?;

        if url.scheme() != "http" && url.scheme() != "https" {
            return Err(HttpError::HttpInvalidUrl(format!(
                "Unsupported protocol '{}'. Only http/https allowed.",
                url.scheme()
            )));
        }

        if let Some(host) = url.host() {
            match host {
                url::Host::Ipv4(ip) => {
                    if ip.is_loopback() || ip.is_private() || ip.is_link_local() {
                        return Err(HttpError::HttpRestricted(format!(
                            "Access to internal IP '{}' is restricted.",
                            ip
                        )));
                    }
                }
                url::Host::Ipv6(ip) => {
                    if ip.is_loopback() || ip.is_unspecified() {
                        return Err(HttpError::HttpRestricted(format!(
                            "Access to internal IP '{}' is restricted.",
                            ip
                        )));
                    }
                }
                url::Host::Domain(domain) => {
                    if domain == "localhost" {
                        return Err(HttpError::HttpRestricted(
                            "Access to 'localhost' is restricted.".into(),
                        ));
                    }
                }
            }
        }

        Ok(url)
    }

    fn parse_response(_url_str: &str, body: &str) -> Result<serde_json::Value, HttpError> {
        let v = serde_json::from_str(body)
            .unwrap_or_else(|_| serde_json::Value::String(body.to_string()));
        // Count the BRIDGED node size (what the machine's cap measures), not the
        // serde tree — a JSON object bridges several-fold larger, so a serde-side
        // count under-reports and lets object-heavy responses abort.
        let nodes = tidepool_eval::json::bridged_node_count(&v);
        if nodes > MAX_RESPONSE_NODES {
            return Err(HttpError::HttpTooLarge(nodes as i64));
        }
        Ok(v)
    }

    /// Map a `ureq` call failure to a typed `HttpError`: a non-2xx response
    /// carries its status CODE as data (`HttpStatus`); anything else (DNS,
    /// connect, timeout, TLS) is `HttpNetwork`.
    fn map_ureq_err(url_str: &str, e: ureq::Error) -> HttpError {
        match e {
            ureq::Error::Status(code, response) => {
                let body = response
                    .into_string()
                    .unwrap_or_else(|_| "<unreadable body>".to_string());
                HttpError::HttpStatus(code as i64, body)
            }
            ureq::Error::Transport(t) => {
                HttpError::HttpNetwork(format!("HTTP request to '{}' failed: {}", url_str, t))
            }
        }
    }

    pub fn get(&self, url_str: &str) -> Result<serde_json::Value, HttpError> {
        let url = Self::validate_url(url_str)?;
        let resp = ureq::get(url.as_str())
            .timeout(std::time::Duration::from_secs(30))
            .call()
            .map_err(|e| Self::map_ureq_err(url_str, e))?;
        let body = resp.into_string().map_err(|e| {
            HttpError::HttpNetwork(format!("Read body from '{}' failed: {}", url_str, e))
        })?;
        Self::parse_response(url_str, &body)
    }

    pub fn post(
        &self,
        url_str: &str,
        json_body: &serde_json::Value,
    ) -> Result<serde_json::Value, HttpError> {
        let url = Self::validate_url(url_str)?;
        let resp = ureq::post(url.as_str())
            .timeout(std::time::Duration::from_secs(30))
            .send_json(json_body)
            .map_err(|e| Self::map_ureq_err(url_str, e))?;
        let body = resp.into_string().map_err(|e| {
            HttpError::HttpNetwork(format!("Read body from '{}' failed: {}", url_str, e))
        })?;
        Self::parse_response(url_str, &body)
    }
}

impl HttpHandler {
    // Errors-tagged verbs: total in `HttpError`, no `cx` — the dispatch arm
    // wraps the `Result` via `cx.respond` (Ok→Right, Err→Left). See #335.
    fn http_get(&mut self, url: String) -> Result<serde_json::Value, HttpError> {
        self.get(&url)
    }

    fn http_post(
        &mut self,
        url: String,
        body: crate::effect_glue::JsonArg,
    ) -> Result<serde_json::Value, HttpError> {
        self.post(&url, &body.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::*;
    use tidepool_bridge::{FromCore, ToCore};
    use tidepool_eval::value::Value;

    #[test]
    fn test_http_from_core_get() {
        let table = full_effect_test_table();
        let con_id = table.get_by_name("HttpGet").unwrap();
        let url = "https://example.com".to_string().to_value(&table).unwrap();
        let val = Value::Con(con_id, vec![url]);
        let req = HttpReq::from_value(&val, &table).unwrap();
        assert!(matches!(req, HttpReq::HttpGet(ref u) if u == "https://example.com"));
    }

    #[test]
    fn test_http_from_core_post() {
        let table = full_effect_test_table();
        let con_id = table.get_by_name("HttpPost").unwrap();
        let url = "https://example.com/api"
            .to_string()
            .to_value(&table)
            .unwrap();
        let null_id = table.get_by_name("Null").unwrap();
        let body = Value::Con(null_id, vec![]);
        let val = Value::Con(con_id, vec![url, body]);
        let req = HttpReq::from_value(&val, &table).unwrap();
        assert!(matches!(req, HttpReq::HttpPost(ref u, _) if u == "https://example.com/api"));
    }

    /// An over-cap JSON response is a typed `Left (HttpTooLarge n)`, not a
    /// generic mid-effect abort: a 40k-element array bridges past the
    /// materialization cap, so `parse_response` short-circuits.
    #[test]
    fn oversized_json_response_is_typed_too_large() {
        let big = serde_json::Value::Array((0..40_000).map(|i| serde_json::json!(i)).collect());
        let body = serde_json::to_string(&big).unwrap();
        match HttpHandler::parse_response("https://x", &body) {
            Err(HttpError::HttpTooLarge(n)) => assert!(n as usize > MAX_RESPONSE_NODES),
            _ => panic!("expected Left (HttpTooLarge _)"),
        }
    }

    /// A normal-size JSON response is unaffected by the guard.
    #[test]
    fn small_json_response_is_ok() {
        assert!(HttpHandler::parse_response("https://x", r#"{"a":1,"b":[1,2,3]}"#).is_ok());
    }

    /// #335 acceptance: `httpGet` on a malformed URL is a typed
    /// `Left (HttpInvalidUrl _)` the eval pattern-matches — never an abort.
    #[tokio::test]
    async fn http_get_bad_url_is_typed_left_httpinvalidurl() {
        let v = jit_eval(&[
            "r <- httpGet \"not-a-url\"",
            "pure (case r of { Left (HttpInvalidUrl _) -> (\"badurl\" :: Text); Left _ -> \"other\"; Right _ -> \"ok\" })",
        ]);
        assert_eq!(v, serde_json::json!("badurl"));
    }

    /// `httpGet` on a restricted (localhost) URL is `Left (HttpRestricted _)`.
    #[tokio::test]
    async fn http_get_localhost_is_typed_left_httprestricted() {
        let v = jit_eval(&[
            "r <- httpGet \"http://localhost/\"",
            "pure (case r of { Left (HttpRestricted _) -> (\"restricted\" :: Text); Left _ -> \"other\"; Right _ -> \"ok\" })",
        ]);
        assert_eq!(v, serde_json::json!("restricted"));
    }
}
