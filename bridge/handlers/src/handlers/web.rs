//! Domain web retrieval: Tavily extraction and separate faithful read-only text.
use super::http::HttpHandler;
pub use crate::generated::web::{WebError, WebReq};
use serde_json::{json, Value};
use std::io::Read;
use std::sync::Arc;
use std::time::Duration;
pub use tidepool_bridge_effects::{WebBodyCompleteness, WebText};

// Existing transport allocation budget, not permission to return a prefix.
const RESPONSE_BYTES: u64 = 16 * 1024 * 1024;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WebOperation {
    Search,
    Extract,
}
impl WebOperation {
    fn endpoint(self) -> &'static str {
        match self {
            Self::Search => "search",
            Self::Extract => "extract",
        }
    }
}

/// Raw response observation. The handler owns bounded reading and text decoding.
pub struct WebTextResponse {
    pub final_url: String,
    pub status: u16,
    pub content_type: Option<String>,
    pub reader: Box<dyn Read + Send>,
}

/// Handler-owned transport dependency, shared unchanged by root and child actors.
/// Implementations retain the network policy; this is not authored HTTP authority.
pub trait WebTransport: Send + Sync {
    fn post(&self, operation: WebOperation, body: &Value) -> Result<Value, WebError>;
    fn fetch_text(&self, url: &str) -> Result<WebTextResponse, WebError>;
}

#[derive(Default)]
struct PublicWebTransport;
impl WebTransport for PublicWebTransport {
    fn post(&self, operation: WebOperation, body: &Value) -> Result<Value, WebError> {
        let operation = operation.endpoint();
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
            .send_json(body.clone())
            .map_err(|error| match error {
                // Provider bodies can echo credentials or query data.
                ureq::Error::Status(status, _) => {
                    WebError::WebProvider(format!("Tavily {operation}: HTTP {status}"))
                }
                ureq::Error::Transport(_) => {
                    WebError::WebTransport(format!("Tavily {operation}: transport failed"))
                }
            })?;
        let bytes = read_complete(response.into_reader())?;
        serde_json::from_slice(&bytes)
            .map_err(|_| WebError::WebDecode("invalid JSON response".into()))
    }

    fn fetch_text(&self, url: &str) -> Result<WebTextResponse, WebError> {
        let (final_url, response) = HttpHandler::get_response(url).map_err(|error| {
            // No arbitrary response body is included in this error path.
            match error {
                super::http::HttpError::HttpInvalidUrl(_)
                | super::http::HttpError::HttpRestricted(_) => WebError::WebInvalidRequest(
                    "URL or redirect refused by public-network policy".into(),
                ),
                _ => WebError::WebTransport(
                    "direct text request failed (DNS, connection, timeout, or redirect)".into(),
                ),
            }
        })?;
        Ok(text_response(final_url, response))
    }
}

fn text_response(final_url: url::Url, response: ureq::Response) -> WebTextResponse {
    WebTextResponse {
        final_url: final_url.into(),
        status: response.status(),
        content_type: response.header("Content-Type").map(str::to_owned),
        // ureq applies supported HTTP content decoding (e.g. gzip).
        // Hashes and limits concern decoded entity bytes, not compressed wire bytes.
        reader: response.into_reader(),
    }
}

#[derive(Clone)]
pub struct WebHandler {
    transport: Arc<dyn WebTransport>,
}
impl Default for WebHandler {
    fn default() -> Self {
        Self::with_transport(Arc::new(PublicWebTransport))
    }
}
impl WebHandler {
    pub fn with_transport(transport: Arc<dyn WebTransport>) -> Self {
        Self { transport }
    }

    pub(crate) fn web_fetch_text(&mut self, url: String) -> Result<WebText, WebError> {
        validate_web_url(&url)?;
        let response = self.transport.fetch_text(&url)?;
        validate_utf8_charset(response.content_type.as_deref())?;
        let bytes = read_complete(response.reader)?;
        let retained_bytes = bytes.len() as i64;
        let mut hash = blake3::Hasher::new();
        hash.update(b"tidepool-web-text-utf8-v1\0");
        hash.update(&bytes);
        let body_identity = hash.finalize().to_hex().to_string();
        let text = String::from_utf8(bytes)
            .map_err(|_| WebError::WebDecode("response is not valid UTF-8".into()))?;
        Ok(WebText {
            requested_url: url,
            final_url: response.final_url,
            status: i64::from(response.status),
            content_type: response.content_type,
            text,
            body_identity,
            retained_bytes,
            byte_limit: RESPONSE_BYTES as i64,
            completeness: WebBodyCompleteness::WebTransportEof,
        })
    }

    pub(crate) fn web_search(
        &mut self,
        query: String,
        limit: i64,
        include_domains: Vec<String>,
        exclude_domains: Vec<String>,
    ) -> Result<Value, WebError> {
        let body = search_body(&query, limit, include_domains, exclude_domains)?;
        let response = self.transport.post(WebOperation::Search, &body)?;
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
        validate_web_url(&url)?;
        let response = self
            .transport
            .post(WebOperation::Extract, &extract_body(&url))?;
        extracted_page(&response)
    }
}

fn validate_web_url(url: &str) -> Result<(), WebError> {
    let parsed = url::Url::parse(url).map_err(|_| {
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
    HttpHandler::validate_url(url).map_err(|_| {
        WebError::WebInvalidRequest("URL is invalid or restricted by public-network policy".into())
    })?;
    Ok(())
}

fn read_complete(reader: impl Read) -> Result<Vec<u8>, WebError> {
    let mut bytes = Vec::new();
    reader
        .take(RESPONSE_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| WebError::WebTransport("response body read failed".into()))?;
    if bytes.len() as u64 > RESPONSE_BYTES {
        return Err(WebError::WebBudgetExceeded(format!("response byte limit {RESPONSE_BYTES}; observed at least {} decoded bytes; body refused", bytes.len())));
    }
    Ok(bytes)
}

fn validate_utf8_charset(content_type: Option<&str>) -> Result<(), WebError> {
    for parameter in content_type.unwrap_or("").split(';').skip(1) {
        let Some((name, value)) = parameter.split_once('=') else {
            continue;
        };
        if name.trim().eq_ignore_ascii_case("charset") {
            let charset = value.trim().trim_matches('"');
            if !charset.eq_ignore_ascii_case("utf-8") && !charset.eq_ignore_ascii_case("utf8") {
                return Err(WebError::WebDecode(
                    "direct text requires UTF-8; declared charset is unsupported".into(),
                ));
            }
        }
    }
    Ok(())
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
impl exomonad_tool::InstalledEffectSupport for WebHandler {
    fn installed_effect_support(&self) -> Vec<exomonad_tool::ToolEffectKey> {
        vec![exomonad_tool::ActorEffectKey::Web.into()]
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    struct Controlled {
        bytes: Vec<u8>,
        content_type: Option<String>,
        status: u16,
    }
    impl WebTransport for Controlled {
        fn post(&self, _: WebOperation, _: &Value) -> Result<Value, WebError> {
            Err(WebError::WebUnavailable(
                "unused controlled operation".into(),
            ))
        }
        fn fetch_text(&self, _: &str) -> Result<WebTextResponse, WebError> {
            Ok(WebTextResponse {
                final_url: "https://example.org/final".into(),
                status: self.status,
                content_type: self.content_type.clone(),
                reader: Box::new(std::io::Cursor::new(self.bytes.clone())),
            })
        }
    }
    fn fetch(bytes: &[u8], content_type: Option<&str>, status: u16) -> Result<WebText, WebError> {
        WebHandler::with_transport(Arc::new(Controlled {
            bytes: bytes.to_vec(),
            content_type: content_type.map(str::to_owned),
            status,
        }))
        .web_fetch_text("https://example.org/requested".into())
    }
    struct HttpFixture {
        agent: ureq::Agent,
    }
    impl WebTransport for HttpFixture {
        fn post(&self, _: WebOperation, _: &Value) -> Result<Value, WebError> {
            unreachable!()
        }
        fn fetch_text(&self, url: &str) -> Result<WebTextResponse, WebError> {
            let (url, response) = HttpHandler::get_response_with_agent(&self.agent, url)
                .map_err(|_| WebError::WebTransport("controlled HTTP failed".into()))?;
            Ok(text_response(url, response))
        }
    }
    fn http_fixture(responses: Vec<Vec<u8>>) -> (WebHandler, std::thread::JoinHandle<()>) {
        use std::io::Write;
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let server = std::thread::spawn(move || {
            for response in responses {
                let (mut stream, _) = listener.accept().unwrap();
                stream
                    .set_read_timeout(Some(Duration::from_secs(5)))
                    .unwrap();
                let mut header = Vec::new();
                while !header.ends_with(b"\r\n\r\n") {
                    let mut byte = [0];
                    stream.read_exact(&mut byte).unwrap();
                    header.extend(byte);
                }
                assert!(header.starts_with(b"GET "));
                assert!(!String::from_utf8_lossy(&header)
                    .to_lowercase()
                    .contains("authorization:"));
                stream.write_all(&response).unwrap();
            }
        });
        // Inject only the socket lookup for a public test name. Production
        // pinning refusals are separately exercised in the HTTP owner's tests.
        let agent = ureq::AgentBuilder::new()
            .redirects(0)
            .timeout(Duration::from_secs(5))
            .resolver(move |_: &str| Ok(vec![address]))
            .build();
        (
            WebHandler::with_transport(Arc::new(HttpFixture { agent })),
            server,
        )
    }
    #[test]
    fn faithful_text_http_redirect_status_and_content_decoding() {
        // gzip representation of exact retained UTF-8 bytes, generated independently.
        let compressed: &[u8] = &[
            31, 139, 8, 0, 0, 0, 0, 0, 2, 255, 83, 80, 72, 72, 72, 224, 229, 122, 57, 123, 85, 76,
            30, 47, 23, 144, 205, 5, 0, 79, 8, 9, 131, 18, 0, 0, 0,
        ];
        let mut response = format!("HTTP/1.1 404 Not Found\r\nContent-Length: {}\r\nContent-Type: text/plain; charset=utf-8\r\nContent-Encoding: gzip\r\nConnection: close\r\n\r\n", compressed.len()).into_bytes();
        response.extend_from_slice(compressed);
        let (mut handler, server) = http_fixture(vec![b"HTTP/1.1 302 Found\r\nLocation: /final\r\nContent-Length: 0\r\nConnection: close\r\n\r\n".to_vec(), response]);
        let result = handler
            .web_fetch_text("http://example.org/start".into())
            .unwrap();
        server.join().unwrap();
        assert_eq!(result.final_url, "http://example.org/final");
        assert_eq!(result.status, 404);
        assert_eq!(result.text, "  ```\r\n雪\\n\r\n```\n");
        assert_eq!(
            result.body_identity,
            fetch(result.text.as_bytes(), None, 200)
                .unwrap()
                .body_identity
        );
    }
    #[test]
    fn faithful_text_http_nonredirect_status_does_not_require_location() {
        for status in [300, 304, 305] {
            let (mut handler, server) = http_fixture(vec![format!(
                "HTTP/1.1 {status} Observation\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
            )
            .into_bytes()]);
            let result = handler
                .web_fetch_text("http://example.org/start".into())
                .unwrap();
            assert_eq!(result.status, status);
            assert!(result.text.is_empty());
            assert_eq!(result.completeness, WebBodyCompleteness::WebTransportEof);
            server.join().unwrap();
        }
    }
    #[test]
    fn faithful_text_http_read_failure_is_not_complete() {
        let (mut handler, server) = http_fixture(vec![
            b"HTTP/1.1 200 OK\r\nContent-Length: 99\r\nConnection: close\r\n\r\nprefix".to_vec(),
        ]);
        assert!(matches!(
            handler.web_fetch_text("http://example.org/start".into()),
            Err(WebError::WebTransport(_))
        ));
        server.join().unwrap();
    }
    #[test]
    fn faithful_text_http_redirect_cannot_add_credentials_or_internal_target() {
        for location in [
            "http://169.254.169.254/",
            "http://user:secret@example.org/next",
        ] {
            let (mut handler, server) = http_fixture(vec![format!("HTTP/1.1 302 Found\r\nLocation: {location}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n").into_bytes()]);
            assert!(handler
                .web_fetch_text("http://example.org/start".into())
                .is_err());
            server.join().unwrap();
        }
    }
    #[test]
    fn faithful_text_preserves_bytes_status_provenance_and_identity() {
        let bytes = "  ```hs\r\n  putStrLn \"\\n\"  \r\n```\r\n雪\n\n".as_bytes();
        let first = fetch(bytes, Some("text/markdown; CHARSET=\"UTF-8\""), 404).unwrap();
        assert_eq!(first.text.as_bytes(), bytes);
        assert_eq!(first.status, 404);
        assert_eq!(first.requested_url, "https://example.org/requested");
        assert_eq!(first.final_url, "https://example.org/final");
        assert_eq!(first.retained_bytes, bytes.len() as i64);
        assert_eq!(first.byte_limit, RESPONSE_BYTES as i64);
        assert_eq!(first.completeness, WebBodyCompleteness::WebTransportEof);
        assert_eq!(
            first.body_identity,
            fetch(bytes, None, 200).unwrap().body_identity
        );
        assert_ne!(
            first.body_identity,
            fetch(b"changed", None, 200).unwrap().body_identity
        );
        // JSON-looking input is text, never parsed or unescaped.
        let json = br#"{ "literal": "\n\\" }"#;
        assert_eq!(
            fetch(json, Some("application/json"), 200)
                .unwrap()
                .text
                .as_bytes(),
            json
        );
    }
    #[test]
    fn faithful_text_rejects_decode_failures_without_lossy_substitution() {
        assert!(matches!(
            fetch(&[0xff], None, 200),
            Err(WebError::WebDecode(_))
        ));
        assert!(matches!(
            fetch(b"plain", Some("text/plain; charset=iso-8859-1"), 200),
            Err(WebError::WebDecode(_))
        ));
        assert!(matches!(
            fetch(b"plain", Some("text/plain; charset="), 200),
            Err(WebError::WebDecode(_))
        ));
        assert_eq!(fetch(b"", None, 204).unwrap().text, "");
    }
    #[test]
    fn faithful_text_limit_is_whole_body_or_explicit_refusal() {
        let at_limit = vec![b'x'; RESPONSE_BYTES as usize];
        assert_eq!(
            fetch(&at_limit, None, 200).unwrap().retained_bytes,
            RESPONSE_BYTES as i64
        );
        let over = std::io::repeat(b'x');
        match read_complete(over) {
            Err(WebError::WebBudgetExceeded(detail)) => {
                assert!(detail.contains(&RESPONSE_BYTES.to_string()));
                assert!(detail.contains(&(RESPONSE_BYTES + 1).to_string()));
                assert!(detail.contains("refused"));
            }
            other => panic!("expected explicit refusal, got {other:?}"),
        }
        assert!(matches!(
            fetch(&[b'x'; 1], Some("text/plain; charset=shift_jis"), 200),
            Err(WebError::WebDecode(_))
        ));
    }
    #[test]
    fn faithful_text_read_failure_never_publishes_prefix() {
        struct Broken {
            prefix: bool,
        }
        impl Read for Broken {
            fn read(&mut self, out: &mut [u8]) -> std::io::Result<usize> {
                if !self.prefix {
                    self.prefix = true;
                    out[0] = b'x';
                    return Ok(1);
                }
                Err(std::io::Error::other("controlled read failure"))
            }
        }
        assert!(matches!(
            read_complete(Broken { prefix: false }),
            Err(WebError::WebTransport(_))
        ));
    }
    #[test]
    fn faithful_text_rejects_credentials_and_internal_urls_before_transport() {
        for url in [
            "file:///etc/passwd",
            "http://user:secret@example.org/",
            "http://127.0.0.1/",
            "http://[::ffff:127.0.0.1]/",
            "http://localhost/",
            "http://169.254.169.254/",
        ] {
            assert!(
                matches!(
                    WebHandler::with_transport(Arc::new(Controlled {
                        bytes: vec![],
                        content_type: None,
                        status: 200
                    }))
                    .web_fetch_text(url.into()),
                    Err(WebError::WebInvalidRequest(_))
                ),
                "{url}"
            );
        }
    }
    #[test]
    #[ignore = "bounded real-network owner-path acceptance, selected explicitly"]
    fn faithful_text_typesafe_owner_path_acceptance() {
        let url = "https://docs.typesafe.ai/cookbooks/function_calling.md";
        let observed = WebHandler::default().web_fetch_text(url.into()).unwrap();
        assert_eq!(observed.requested_url, url);
        assert_eq!(observed.final_url, url);
        assert_eq!(observed.status, 200);
        let capture =
            std::env::var_os("TIDEPOOL_WEB_CAPTURE").expect("explicit private evidence path");
        std::fs::write(capture, observed.text.as_bytes()).unwrap();
        println!(
            "status={} retained_bytes={} body_identity={} content_type={:?} completeness={:?}",
            observed.status,
            observed.retained_bytes,
            observed.body_identity,
            observed.content_type,
            observed.completeness
        );
        assert!(observed.text.contains("# Function calling"));
        assert!(observed.text.contains("```"));
    }
    proptest::proptest! {
        #![proptest_config(proptest::test_runner::Config::with_cases(32))]
        #[test]
        fn faithful_text_read_chunking_preserves_exact_bytes(
            text in ".{0,1024}", width in 1_usize..33
        ) {
            struct Chunks { bytes: std::io::Cursor<Vec<u8>>, width: usize }
            impl Read for Chunks {
                fn read(&mut self, out: &mut [u8]) -> std::io::Result<usize> {
                    let n = out.len().min(self.width);
                    self.bytes.read(&mut out[..n])
                }
            }
            let bytes = text.into_bytes();
            let result = read_complete(Chunks { bytes: std::io::Cursor::new(bytes.clone()), width }).unwrap();
            proptest::prop_assert_eq!(result, bytes);
        }
    }
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
