// ============================================================================
// Tag 4: Http
// ============================================================================

use std::error::Error as _;

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

/// The actual DNS/socket-address lookup a [`PinningResolver`] validates
/// before handing addresses to ureq. Split out from `PinningResolver` so a
/// test can inject a fake lookup (a hostname that "resolves" to a local
/// listener's address, simulating DNS rebinding) without touching real DNS —
/// see `mod tests`'s `FakeRawResolve`.
trait RawResolve: Send + Sync {
    fn raw_resolve(&self, netloc: &str) -> std::io::Result<Vec<std::net::SocketAddr>>;
}

/// The production lookup: plain OS resolution via `ToSocketAddrs`, same as
/// ureq's own built-in `StdResolver`.
#[derive(Debug, Default)]
struct StdRawResolve;

impl RawResolve for StdRawResolve {
    fn raw_resolve(&self, netloc: &str) -> std::io::Result<Vec<std::net::SocketAddr>> {
        use std::net::ToSocketAddrs;
        netloc.to_socket_addrs().map(Iterator::collect)
    }
}

/// Resolves `netloc` (`"host:port"`) via `inner`, rejects the lookup outright
/// if ANY candidate address is loopback/private/link-local/unspecified, and
/// otherwise returns exactly the validated addresses. ureq connects to ONE
/// of THESE — never re-resolving — which is what pins the connection: a
/// second, unchecked lookup (the classic DNS-rebinding TOCTOU) never
/// happens, because there is no second lookup at all.
struct PinningResolver<R> {
    inner: R,
}

impl<R: RawResolve + 'static> ureq::Resolver for PinningResolver<R> {
    fn resolve(&self, netloc: &str) -> std::io::Result<Vec<std::net::SocketAddr>> {
        let addrs = self.inner.raw_resolve(netloc)?;
        if let Some(bad) = addrs.iter().find(|a| HttpHandler::ip_is_restricted(a.ip())) {
            // `PermissionDenied` is the sentinel `map_ureq_err` recognizes as
            // an SSRF-guard rejection (vs. an ordinary DNS/connect failure),
            // so it surfaces as the typed `HttpRestricted`, not `HttpNetwork`.
            return Err(std::io::Error::new(
                std::io::ErrorKind::PermissionDenied,
                format!("'{netloc}' resolved to restricted address '{}'", bad.ip()),
            ));
        }
        if addrs.is_empty() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::NotFound,
                format!("no addresses for '{netloc}'"),
            ));
        }
        Ok(addrs)
    }
}

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

    /// IPv4 restriction rules, shared between the text-level [`Self::validate_url`]
    /// check (an IP literal in the URL) and [`PinningResolver`] (an IP a
    /// hostname actually resolved to) — one definition, so the two checks
    /// cannot drift apart.
    fn ipv4_is_restricted(ip: &std::net::Ipv4Addr) -> bool {
        ip.is_loopback() || ip.is_private() || ip.is_link_local() || ip.is_unspecified()
    }

    /// IPv6 restriction rules, same sharing rationale as [`Self::ipv4_is_restricted`].
    /// IPv4-mapped (`::ffff:a.b.c.d`) addresses are judged by the embedded v4
    /// rules — e.g. `::ffff:127.0.0.1` reaches loopback but is neither
    /// `is_loopback()` nor `is_unspecified()` as an `Ipv6Addr`.
    fn ipv6_is_restricted(ip: &std::net::Ipv6Addr) -> bool {
        if let Some(v4) = ip.to_ipv4_mapped() {
            Self::ipv4_is_restricted(&v4)
        } else {
            ip.is_loopback()
                || ip.is_unspecified()
                || Self::ipv6_is_unique_local(ip)
                || Self::ipv6_is_link_local(ip)
        }
    }

    /// [`Self::ipv4_is_restricted`]/[`Self::ipv6_is_restricted`] over a
    /// resolved socket address — what [`PinningResolver`] checks per
    /// candidate address.
    fn ip_is_restricted(ip: std::net::IpAddr) -> bool {
        match ip {
            std::net::IpAddr::V4(ip) => Self::ipv4_is_restricted(&ip),
            std::net::IpAddr::V6(ip) => Self::ipv6_is_restricted(&ip),
        }
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
                    if Self::ipv4_is_restricted(&ip) {
                        return Err(HttpError::HttpRestricted(format!(
                            "Access to internal IP '{}' is restricted.",
                            ip
                        )));
                    }
                }
                url::Host::Ipv6(ip) => {
                    if Self::ipv6_is_restricted(&ip) {
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
    ///
    /// The real SSRF guard against DNS rebinding is [`PinningResolver`]
    /// (below), wired in here: it resolves each hop's hostname itself,
    /// rejects if ANY resolved address is restricted, and hands ureq back
    /// exactly those validated addresses to connect to — so there is no
    /// second, unchecked lookup between validation and connect for an
    /// attacker to race.
    fn agent() -> ureq::Agent {
        ureq::AgentBuilder::new()
            .redirects(0)
            .resolver(PinningResolver {
                inner: StdRawResolve,
            })
            .build()
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
        Self::request_following_redirects_with_agent(&Self::agent(), url_str, body)
    }

    /// The actual follow loop, over an injected `agent` — split out from
    /// [`Self::request_following_redirects`] so a test can pass an agent
    /// wired with a [`PinningResolver`] over a [`RawResolve`] fake (no real
    /// DNS) instead of the production one.
    fn request_following_redirects_with_agent(
        agent: &ureq::Agent,
        url_str: &str,
        body: Option<&serde_json::Value>,
    ) -> Result<(url::Url, ureq::Response), HttpError> {
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

    /// Raw-byte ceiling enforced WHILE reading a response body
    /// ([`Self::read_body_capped`]), independent of [`MAX_RESPONSE_NODES`]'s
    /// post-parse structural check below: a response can be node-light but
    /// byte-heavy (one huge JSON string is a SINGLE bridged node), so
    /// materialization itself — not just the parsed shape — needs a hard
    /// ceiling. 16 MiB comfortably covers any legitimate API payload that
    /// would also pass the node-count check.
    const MAX_RESPONSE_BYTES: usize = 16 * 1024 * 1024;

    /// Read `resp`'s body to a `String`, enforcing [`Self::MAX_RESPONSE_BYTES`]
    /// DURING the read rather than after full materialization — a response
    /// that would otherwise exceed it is rejected as soon as the cap is
    /// crossed, before the rest of the body is ever read into memory.
    fn read_body_capped(resp: ureq::Response, final_url: &url::Url) -> Result<String, HttpError> {
        use std::io::Read;
        let mut reader = resp.into_reader();
        let mut buf = Vec::new();
        let mut chunk = [0u8; 64 * 1024];
        loop {
            let n = reader.read(&mut chunk).map_err(|e| {
                HttpError::HttpNetwork(format!("Read body from '{}' failed: {}", final_url, e))
            })?;
            if n == 0 {
                break;
            }
            if buf.len() + n > Self::MAX_RESPONSE_BYTES {
                return Err(HttpError::HttpTooLarge((buf.len() + n) as i64));
            }
            buf.extend_from_slice(&chunk[..n]);
        }
        String::from_utf8(buf).map_err(|e| {
            HttpError::HttpNetwork(format!(
                "Response body from '{}' is not valid UTF-8: {}",
                final_url, e
            ))
        })
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
    /// carries its status CODE as data (`HttpStatus`); a [`PinningResolver`]
    /// rejection (tagged `PermissionDenied`, see its `resolve`) is
    /// `HttpRestricted`; anything else (DNS, connect, timeout, TLS) is
    /// `HttpNetwork`.
    fn map_ureq_err(url_str: &str, e: ureq::Error) -> HttpError {
        match e {
            ureq::Error::Status(code, response) => {
                let body = response
                    .into_string()
                    .unwrap_or_else(|_| "<unreadable body>".to_string());
                HttpError::HttpStatus(code as i64, body)
            }
            ureq::Error::Transport(t) => {
                let io_err = t.source().and_then(|s| s.downcast_ref::<std::io::Error>());
                if let Some(io_err) = io_err {
                    if io_err.kind() == std::io::ErrorKind::PermissionDenied {
                        return HttpError::HttpRestricted(format!(
                            "Access to '{}' is restricted: {}",
                            url_str, io_err
                        ));
                    }
                }
                HttpError::HttpNetwork(format!("HTTP request to '{}' failed: {}", url_str, t))
            }
        }
    }

    pub fn get(&self, url_str: &str) -> Result<serde_json::Value, HttpError> {
        let (final_url, resp) = Self::request_following_redirects(url_str, None)?;
        let body = Self::read_body_capped(resp, &final_url)?;
        Self::parse_response(final_url.as_str(), &body)
    }

    pub fn post(
        &self,
        url_str: &str,
        json_body: &serde_json::Value,
    ) -> Result<serde_json::Value, HttpError> {
        let (final_url, resp) = Self::request_following_redirects(url_str, Some(json_body))?;
        let body = Self::read_body_capped(resp, &final_url)?;
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

    /// Bundles `http_get_bad_url_is_typed_left_httpinvalidurl` (#335
    /// acceptance: `httpGet` on a malformed URL is a typed
    /// `Left (HttpInvalidUrl _)` the eval pattern-matches, never an abort) +
    /// `http_get_localhost_is_typed_left_httprestricted` (`httpGet` on a
    /// restricted/localhost URL is `Left (HttpRestricted _)`) into one
    /// tidepool-extract compile. Returns the list of FAILED check names
    /// (empty on success) — see
    /// `tidepool-runtime/tests/generic_form_roundtrip.rs`'s `check` helper.
    #[tokio::test]
    async fn test_jit_http_family() {
        let result = jit_eval(&[
            "let check nm ok = if ok then [] else [nm]",
            "badUrl <- httpGet \"not-a-url\"",
            "let badUrlOk = case badUrl of { Left (HttpInvalidUrl _) -> True; _ -> False }",
            "restricted <- httpGet \"http://localhost/\"",
            "let restrictedOk = case restricted of { Left (HttpRestricted _) -> True; _ -> False }",
            "let c1 = check \"http-get-bad-url-is-typed-left-httpinvalidurl\" badUrlOk",
            "let c2 = check \"http-get-localhost-is-typed-left-httprestricted\" restrictedOk",
            "pure (concat [c1, c2])",
        ]);
        assert_eq!(result, serde_json::json!([]), "failed checks: {result}");
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

    /// `0.0.0.0` (IPv4 unspecified) previously sailed straight through: it is
    /// neither `is_loopback()` nor `is_private()` nor `is_link_local()`, but
    /// on most stacks connecting to it reaches the same listener loopback
    /// would.
    #[test]
    fn validate_url_rejects_ipv4_unspecified() {
        match HttpHandler::validate_url("http://0.0.0.0/") {
            Err(HttpError::HttpRestricted(_)) => {}
            other => panic!("expected HttpRestricted for 0.0.0.0, got {other:?}"),
        }
    }

    // -------------------------------------------------------------------
    // Resolve-and-pin: reject on the RESOLVED address, not just URL text.
    // -------------------------------------------------------------------

    /// A `RawResolve` fake that returns one fixed address for any hostname —
    /// simulating DNS rebinding (an ordinary-looking domain that resolves to
    /// a restricted address) without touching real DNS.
    struct FakeRawResolve(std::net::SocketAddr);

    impl RawResolve for FakeRawResolve {
        fn raw_resolve(&self, _netloc: &str) -> std::io::Result<Vec<std::net::SocketAddr>> {
            Ok(vec![self.0])
        }
    }

    /// The core of the resolve-and-pin fix: a hostname that passes
    /// `validate_url`'s TEXT check (it's neither a literal IP nor
    /// `localhost`) but resolves — via the injected fake, standing in for a
    /// rebinding attacker's DNS — to a loopback address must still be
    /// rejected, typed as `HttpRestricted`. And critically, the local
    /// listener it "resolves" to must never actually be contacted: the
    /// rejection happens at resolve time, before connect, so a second,
    /// unguarded lookup can't be raced against the check.
    #[test]
    fn resolver_rejects_hostname_resolving_to_loopback_and_never_connects() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let addr = listener.local_addr().unwrap();

        let agent = ureq::AgentBuilder::new()
            .redirects(0)
            .resolver(PinningResolver {
                inner: FakeRawResolve(addr),
            })
            .build();

        let result = HttpHandler::request_following_redirects_with_agent(
            &agent,
            "http://rebinding-target.invalid/",
            None,
        );
        match result {
            Err(HttpError::HttpRestricted(_)) => {}
            other => panic!("expected HttpRestricted, got {other:?}"),
        }

        match listener.accept() {
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {}
            other => panic!(
                "listener should never have been contacted (SSRF guard should reject \
                 before connect), got {other:?}"
            ),
        }
    }

    /// A hostname resolving only to genuinely public addresses is unaffected.
    #[test]
    fn resolver_allows_hostname_resolving_to_public_address() {
        let public_addr: std::net::SocketAddr = "93.184.216.34:80".parse().unwrap();
        let resolver = PinningResolver {
            inner: FakeRawResolve(public_addr),
        };
        assert_eq!(
            ureq::Resolver::resolve(&resolver, "example.invalid:80").unwrap(),
            vec![public_addr]
        );
    }

    // -------------------------------------------------------------------
    // Streamed body cap: the cap fires DURING the read, not after full
    // materialization.
    // -------------------------------------------------------------------

    /// A response whose declared `Content-Length` is well past
    /// `MAX_RESPONSE_BYTES` errors as `HttpTooLarge` — and, because
    /// `read_body_capped` stops retaining bytes the moment the cap is
    /// crossed, this holds even though the server is willing to keep
    /// sending far more than the cap. Exercises `read_body_capped` directly
    /// against a real local listener (not the SSRF-guarded path, which
    /// would reject a loopback URL outright and is tested separately).
    #[test]
    fn read_body_capped_errors_without_full_materialization() {
        use std::io::Write;

        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let total_len = HttpHandler::MAX_RESPONSE_BYTES + 4 * 1024 * 1024;

        std::thread::spawn(move || {
            let Ok((mut stream, _)) = listener.accept() else {
                return;
            };
            let header = format!(
                "HTTP/1.1 200 OK\r\nContent-Length: {total_len}\r\nConnection: close\r\n\r\n"
            );
            if stream.write_all(header.as_bytes()).is_err() {
                return;
            }
            let chunk = vec![b'a'; 256 * 1024];
            let mut written = 0usize;
            while written < total_len {
                let take = chunk.len().min(total_len - written);
                if stream.write_all(&chunk[..take]).is_err() {
                    break;
                }
                written += take;
            }
        });

        let url_str = format!("http://{addr}/");
        let resp = ureq::get(&url_str)
            .call()
            .expect("request to local listener should succeed");
        let final_url = url::Url::parse(&url_str).unwrap();
        match HttpHandler::read_body_capped(resp, &final_url) {
            Err(HttpError::HttpTooLarge(n)) => {
                assert!(n as usize > HttpHandler::MAX_RESPONSE_BYTES)
            }
            other => panic!("expected HttpTooLarge, got {other:?}"),
        }
    }

    /// A normal-size body streams through `read_body_capped` unaffected.
    #[test]
    fn read_body_capped_allows_small_body() {
        use std::io::{Read, Write};

        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();

        let server = std::thread::spawn(move || {
            let Ok((mut stream, _)) = listener.accept() else {
                return;
            };
            // Consume the request before closing the socket. Closing with
            // unread peer data is allowed to produce a TCP RST, which made
            // this otherwise-local body test intermittently fail in ureq.
            let mut request = [0_u8; 1024];
            let _ = stream.read(&mut request);
            let body = b"{\"hello\":\"world\"}";
            let header = format!(
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                body.len()
            );
            let _ = stream.write_all(header.as_bytes());
            let _ = stream.write_all(body);
        });

        let url_str = format!("http://{addr}/");
        let resp = ureq::get(&url_str)
            .call()
            .expect("request to local listener should succeed");
        let final_url = url::Url::parse(&url_str).unwrap();
        assert_eq!(
            HttpHandler::read_body_capped(resp, &final_url).unwrap(),
            "{\"hello\":\"world\"}"
        );
        server.join().unwrap();
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
