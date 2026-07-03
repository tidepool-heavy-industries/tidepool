use tidepool_bridge_derive::FromCore;
use tidepool_effect::dispatch::{EffectContext, EffectHandler};
use tidepool_effect::error::EffectError;
use tidepool_eval::value::Value;
use tidepool_mcp::{CapturedOutput, DescribeEffect, EffectDecl};

// ============================================================================
// Tag 4: Http
// ============================================================================

#[derive(FromCore)]
pub enum HttpReq {
    #[core(name = "HttpGet")]
    Get(String),
    #[core(name = "HttpPost")]
    Post(String, Value),
    #[core(name = "TryHttpGet")]
    TryGet(String),
    #[core(name = "TryHttpPost")]
    TryPost(String, Value),
    #[core(name = "ParseJson")]
    ParseJson(String),
    #[core(name = "TryParseJson")]
    TryParseJson(String),
}

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

impl DescribeEffect for HttpHandler {
    fn effect_decl() -> EffectDecl {
        tidepool_mcp::http_decl()
    }
}

impl EffectHandler<CapturedOutput> for HttpHandler {
    type Request = HttpReq;
    fn handle(
        &mut self,
        req: HttpReq,
        cx: &EffectContext<'_, CapturedOutput>,
    ) -> Result<tidepool_effect::Response, EffectError> {
        match req {
            HttpReq::Get(url_str) => cx.respond(self.get(&url_str)?),
            HttpReq::Post(url_str, body_val) => {
                let json_body = tidepool_runtime::value_to_json(&body_val, cx.table(), 0);
                cx.respond(self.post(&url_str, &json_body)?)
            }
            HttpReq::TryGet(url_str) => cx.respond_caught(self.get(&url_str)),
            HttpReq::TryPost(url_str, body_val) => {
                let json_body = tidepool_runtime::value_to_json(&body_val, cx.table(), 0);
                cx.respond_caught(self.post(&url_str, &json_body))
            }
            HttpReq::ParseJson(s) => cx.respond(parse_json_str(&s)?),
            HttpReq::TryParseJson(s) => cx.respond_caught(parse_json_str(&s)),
        }
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
        assert!(matches!(req, HttpReq::Get(ref u) if u == "https://example.com"));
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
        assert!(matches!(req, HttpReq::Post(ref u, _) if u == "https://example.com/api"));
    }
}
