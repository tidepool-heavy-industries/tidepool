use tidepool_effect::dispatch::EffectContext;
use tidepool_effect::error::EffectError;
use tidepool_eval::value::Value;
use tidepool_mcp::CapturedOutput;

// ============================================================================
// Tag 4: Http
// ============================================================================

// HttpReq + DescribeEffect + EffectHandler dispatch are generated from the
// single-source definition; only the handler struct and the per-verb method
// bodies below are hand-written.
tidepool_mcp::http_effect_def!(crate::effect_glue::effect_rust_projection);

pub fn parse_json_str(s: &str) -> Result<serde_json::Value, EffectError> {
    serde_json::from_str(s).map_err(|e| EffectError::Handler(format!("invalid JSON: {e}")))
}

#[derive(Clone)]
pub struct HttpHandler;

impl HttpHandler {
    pub fn validate_url(url_str: &str) -> Result<url::Url, EffectError> {
        let url = url::Url::parse(url_str)
            .map_err(|e| EffectError::Handler(format!("Invalid URL '{}': {}", url_str, e)))?;

        if url.scheme() != "http" && url.scheme() != "https" {
            return Err(EffectError::Handler(format!(
                "Unsupported protocol '{}'. Only http/https allowed.",
                url.scheme()
            )));
        }

        if let Some(host) = url.host() {
            match host {
                url::Host::Ipv4(ip) => {
                    if ip.is_loopback() || ip.is_private() || ip.is_link_local() {
                        return Err(EffectError::Handler(format!(
                            "Access to internal IP '{}' is restricted.",
                            ip
                        )));
                    }
                }
                url::Host::Ipv6(ip) => {
                    if ip.is_loopback() || ip.is_unspecified() {
                        return Err(EffectError::Handler(format!(
                            "Access to internal IP '{}' is restricted.",
                            ip
                        )));
                    }
                }
                url::Host::Domain(domain) => {
                    if domain == "localhost" {
                        return Err(EffectError::Handler(
                            "Access to 'localhost' is restricted.".into(),
                        ));
                    }
                }
            }
        }

        Ok(url)
    }

    fn parse_response(_url_str: &str, body: &str) -> Result<serde_json::Value, EffectError> {
        serde_json::from_str(body).or_else(|_| Ok(serde_json::Value::String(body.to_string())))
    }

    pub fn get(&self, url_str: &str) -> Result<serde_json::Value, EffectError> {
        let url = Self::validate_url(url_str)?;
        let resp = ureq::get(url.as_str())
            .timeout(std::time::Duration::from_secs(30))
            .call()
            .map_err(|e| EffectError::Handler(format!("HTTP GET '{}' failed: {}", url_str, e)))?;
        let body = resp.into_string().map_err(|e| {
            EffectError::Handler(format!("Read body from '{}' failed: {}", url_str, e))
        })?;
        Self::parse_response(url_str, &body)
    }

    pub fn post(
        &self,
        url_str: &str,
        json_body: &serde_json::Value,
    ) -> Result<serde_json::Value, EffectError> {
        let url = Self::validate_url(url_str)?;
        let resp = ureq::post(url.as_str())
            .timeout(std::time::Duration::from_secs(30))
            .send_json(json_body)
            .map_err(|e| EffectError::Handler(format!("HTTP POST '{}' failed: {}", url_str, e)))?;
        let body = resp.into_string().map_err(|e| {
            EffectError::Handler(format!("Read body from '{}' failed: {}", url_str, e))
        })?;
        Self::parse_response(url_str, &body)
    }
}

impl HttpHandler {
    fn http_get(
        &mut self,
        cx: &EffectContext<'_, CapturedOutput>,
        url: String,
    ) -> Result<tidepool_effect::Response, EffectError> {
        cx.respond(self.get(&url)?)
    }

    fn http_post(
        &mut self,
        cx: &EffectContext<'_, CapturedOutput>,
        url: String,
        body: Value,
    ) -> Result<tidepool_effect::Response, EffectError> {
        let json_body = tidepool_runtime::value_to_json(&body, cx.table(), 0);
        cx.respond(self.post(&url, &json_body)?)
    }

    fn http_try_get(
        &mut self,
        cx: &EffectContext<'_, CapturedOutput>,
        url: String,
    ) -> Result<tidepool_effect::Response, EffectError> {
        cx.respond_caught(self.get(&url))
    }

    fn http_try_post(
        &mut self,
        cx: &EffectContext<'_, CapturedOutput>,
        url: String,
        body: Value,
    ) -> Result<tidepool_effect::Response, EffectError> {
        let json_body = tidepool_runtime::value_to_json(&body, cx.table(), 0);
        cx.respond_caught(self.post(&url, &json_body))
    }

    fn http_parse_json(
        &mut self,
        cx: &EffectContext<'_, CapturedOutput>,
        s: String,
    ) -> Result<tidepool_effect::Response, EffectError> {
        cx.respond(parse_json_str(&s)?)
    }

    fn http_try_parse_json(
        &mut self,
        cx: &EffectContext<'_, CapturedOutput>,
        s: String,
    ) -> Result<tidepool_effect::Response, EffectError> {
        cx.respond_caught(parse_json_str(&s))
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
}
