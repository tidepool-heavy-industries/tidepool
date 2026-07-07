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

/// Redirect hops a single `get`/`post` call will follow before giving up.
/// Counts hops only (the initial request is not one) — at most
/// `MAX_REDIRECTS + 1` requests are ever made.
const MAX_REDIRECTS: u32 = 5;

#[derive(Clone)]
pub struct HttpHandler;

impl HttpHandler {
    /// `fc00::/7` (unique local). Checked by hand (not `Ipv6Addr::is_unique_local`,
    /// which is unstable and varies by toolchain) via the top 7 bits of the
    /// first hextet: the range is `fc00::` through `fdff:...`.
    fn ipv6_is_unique_local(ip: &std::net::Ipv6Addr) -> bool {
        (ip.segments()[0] & 0xFE00) == 0xFC00
    }

    /// `fe80::/10` (link-local unicast), checked by hand for the same reason.
    fn ipv6_is_link_local(ip: &std::net::Ipv6Addr) -> bool {
        (ip.segments()[0] & 0xFFC0) == 0xFE80
    }

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
                    // IPv4-mapped (`::ffff:a.b.c.d`) addresses must be judged
                    // by the embedded v4 rules — e.g. `::ffff:127.0.0.1`
                    // reaches loopback but is neither `is_loopback()` nor
                    // `is_unspecified()` as an Ipv6Addr.
                    let restricted = if let Some(v4) = ip.to_ipv4_mapped() {
                        v4.is_loopback() || v4.is_private() || v4.is_link_local()
                    } else {
                        ip.is_loopback()
                            || ip.is_unspecified()
                            || Self::ipv6_is_unique_local(&ip)
                            || Self::ipv6_is_link_local(&ip)
                    };
                    if restricted {
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

    /// A `ureq::Agent` with automatic redirect-following DISABLED
    /// (`redirects(0)`): a 3xx response is returned as-is instead of being
    /// followed blind. `get`/`post` hand-roll the follow loop themselves so
    /// every hop — not just the initial URL — passes [`Self::validate_url`].
    fn agent() -> ureq::Agent {
        ureq::AgentBuilder::new().redirects(0).build()
    }

    /// Resolve a redirect `Location` header (absolute OR relative) against
    /// the URL that produced it.
    fn resolve_redirect(base: &url::Url, location: &str) -> Result<url::Url, HttpError> {
        base.join(location).map_err(|e| {
            HttpError::HttpNetwork(format!(
                "invalid redirect Location {:?} from '{}': {}",
                location, base, e
            ))
        })
    }

    /// Issue `GET`/`POST` (`body = Some(_)` selects POST) and follow up to
    /// [`MAX_REDIRECTS`] redirect hops manually, re-running
    /// [`Self::validate_url`] on every RESOLVED absolute URL before it is
    /// requested — including the first. This is the actual SSRF guard:
    /// `ureq`'s built-in redirect handling validates only the URL the caller
    /// passed in, so a public URL that 302s to `http://169.254.169.254/...`
    /// would otherwise be followed blind. 301/302/303 downgrade POST to GET
    /// (matching curl/browser behavior); 307/308 preserve the method and body.
    fn request_following_redirects(
        url_str: &str,
        body: Option<&serde_json::Value>,
    ) -> Result<(url::Url, ureq::Response), HttpError> {
        let agent = Self::agent();
        let mut url = Self::validate_url(url_str)?;
        let mut use_post = body.is_some();
        for hop in 0..=MAX_REDIRECTS {
            let req = if use_post {
                agent.post(url.as_str())
            } else {
                agent.get(url.as_str())
            }
            .timeout(std::time::Duration::from_secs(30));
            let resp = match (use_post, body) {
                (true, Some(b)) => req.send_json(b.clone()),
                _ => req.call(),
            }
            .map_err(|e| Self::map_ureq_err(url.as_str(), e))?;

            let status = resp.status();
            if !(300..400).contains(&status) {
                return Ok((url, resp));
            }
            if hop == MAX_REDIRECTS {
                return Err(HttpError::HttpNetwork(format!(
                    "too many redirects (> {MAX_REDIRECTS}) starting from '{url_str}'"
                )));
            }
            let location = resp.header("Location").ok_or_else(|| {
                HttpError::HttpNetwork(format!(
                    "redirect ({status}) from '{url}' has no Location header"
                ))
            })?;
            let next = Self::resolve_redirect(&url, location)?;
            url = Self::validate_url(next.as_str())?;
            if use_post && !matches!(status, 307 | 308) {
                use_post = false;
            }
        }
        unreachable!("loop always returns via the hop == MAX_REDIRECTS branch or an Ok/Err above")
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
        let (final_url, resp) = Self::request_following_redirects(url_str, None)?;
        let body = resp.into_string().map_err(|e| {
            HttpError::HttpNetwork(format!("Read body from '{}' failed: {}", final_url, e))
        })?;
        Self::parse_response(final_url.as_str(), &body)
    }

    pub fn post(
        &self,
        url_str: &str,
        json_body: &serde_json::Value,
    ) -> Result<serde_json::Value, HttpError> {
        let (final_url, resp) = Self::request_following_redirects(url_str, Some(json_body))?;
        let body = resp.into_string().map_err(|e| {
            HttpError::HttpNetwork(format!("Read body from '{}' failed: {}", final_url, e))
        })?;
        Self::parse_response(final_url.as_str(), &body)
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

    // -------------------------------------------------------------------
    // F3: SSRF guard bypasses — IPv6 (the exact URLs named in the plan).
    // -------------------------------------------------------------------

    /// `::ffff:127.0.0.1` is the IPv4-mapped form of loopback: an
    /// `Ipv6Addr` fails `is_loopback()`/`is_unspecified()` (those only match
    /// the native v6 loopback `::1`), so the old check let it straight
    /// through to real loopback. Must normalize via `to_ipv4_mapped()` and
    /// re-run the v4 rules.
    #[test]
    fn validate_url_rejects_ipv4_mapped_loopback() {
        match HttpHandler::validate_url("http://[::ffff:127.0.0.1]/") {
            Err(HttpError::HttpRestricted(_)) => {}
            other => panic!("expected HttpRestricted for IPv4-mapped loopback, got {other:?}"),
        }
    }

    /// `::ffff:169.254.169.254` — the IPv4-mapped form of the cloud metadata
    /// link-local address — must also be caught (a private/link-local v4
    /// address behind the mapping, not just loopback).
    #[test]
    fn validate_url_rejects_ipv4_mapped_link_local() {
        match HttpHandler::validate_url("http://[::ffff:169.254.169.254]/") {
            Err(HttpError::HttpRestricted(_)) => {}
            other => panic!("expected HttpRestricted for IPv4-mapped link-local, got {other:?}"),
        }
    }

    /// `fc00::/7` (unique local) previously sailed past the loopback/
    /// unspecified-only v6 check entirely.
    #[test]
    fn validate_url_rejects_unique_local_v6() {
        match HttpHandler::validate_url("http://[fc00::1]/") {
            Err(HttpError::HttpRestricted(_)) => {}
            other => panic!("expected HttpRestricted for fc00::/7, got {other:?}"),
        }
        // fd00:: is also within fc00::/7 (the 8th bit is the only thing that varies).
        match HttpHandler::validate_url("http://[fd12:3456::1]/") {
            Err(HttpError::HttpRestricted(_)) => {}
            other => panic!("expected HttpRestricted for fd00::/7, got {other:?}"),
        }
    }

    /// `fe80::/10` (link-local) previously sailed past the same gap.
    #[test]
    fn validate_url_rejects_link_local_v6() {
        match HttpHandler::validate_url("http://[fe80::1]/") {
            Err(HttpError::HttpRestricted(_)) => {}
            other => panic!("expected HttpRestricted for fe80::/10, got {other:?}"),
        }
    }

    /// A genuinely public v6 address must still be allowed (the fix must not
    /// over-broaden and reject the internet).
    #[test]
    fn validate_url_allows_public_v6() {
        // 2001:4860:4860::8888 is a real public (Google DNS) v6 address.
        assert!(HttpHandler::validate_url("http://[2001:4860:4860::8888]/").is_ok());
    }

    // -------------------------------------------------------------------
    // F3: SSRF guard bypass — redirects.
    // -------------------------------------------------------------------

    /// The core of the redirect fix: every hop's RESOLVED absolute URL must
    /// pass `validate_url` — an absolute redirect Location pointing at an
    /// internal address (e.g. the cloud metadata endpoint) must be rejected,
    /// exactly as if it had been the originally-requested URL.
    #[test]
    fn redirect_to_internal_absolute_location_is_rejected() {
        let base = url::Url::parse("https://example.com/start").unwrap();
        let resolved =
            HttpHandler::resolve_redirect(&base, "http://169.254.169.254/latest/meta-data/")
                .unwrap();
        match HttpHandler::validate_url(resolved.as_str()) {
            Err(HttpError::HttpRestricted(_)) => {}
            other => panic!("expected HttpRestricted, got {other:?}"),
        }
    }

    /// A relative `Location` (common for same-host redirects) must resolve
    /// against the URL that produced it, not error or silently drop.
    #[test]
    fn redirect_location_relative_resolves_against_base() {
        let base = url::Url::parse("https://example.com/a/b").unwrap();
        let resolved = HttpHandler::resolve_redirect(&base, "/c").unwrap();
        assert_eq!(resolved.as_str(), "https://example.com/c");
    }

    /// A relative redirect that lands on a restricted host (e.g. `Location:
    /// //169.254.169.254/x`, a protocol-relative reference) must also be
    /// rejected once resolved.
    #[test]
    fn redirect_location_protocol_relative_to_internal_is_rejected() {
        let base = url::Url::parse("https://example.com/start").unwrap();
        let resolved = HttpHandler::resolve_redirect(&base, "//169.254.169.254/x").unwrap();
        match HttpHandler::validate_url(resolved.as_str()) {
            Err(HttpError::HttpRestricted(_)) => {}
            other => panic!("expected HttpRestricted, got {other:?}"),
        }
    }
}
