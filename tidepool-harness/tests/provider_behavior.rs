//! ONE shared behavior suite both `ModelProvider` impls pass (60-auth
//! SPEC step 4): record/replay HTTP fixtures via a local mock server, no
//! live network calls. Each mock server is a plain `std::net` HTTP/1.1
//! responder (one request per connection, `Connection: close`) — no
//! external mocking crate needed, and it lets both impls' `genai`-routed
//! chat calls and the OAuth refresh call hit the exact same kind of
//! canned fixture.

use std::collections::{HashMap, VecDeque};
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::Arc;

use parking_lot::Mutex;

use tidepool_harness::provider::api_key::{ApiKeyConfig, ApiKeyProvider};
use tidepool_harness::provider::oauth::{self, OauthConfig, OauthProvider};
use tidepool_harness::provider::{Message, ModelProvider, ProviderError, Role, TurnRequest};

// ---------------------------------------------------------------------
// Mock server: queued (status, json-body) responses keyed by (method, path).
// ---------------------------------------------------------------------

/// Queued (status, raw body) responses per pending request, keyed by
/// `(method, path)`. Bodies are pre-rendered strings so a route can serve
/// either a JSON body (the API-key/genai + token-exchange paths) or a raw
/// SSE stream (the OAuth `/responses` path is hand-rolled and parses SSE).
type RouteTable = HashMap<(String, String), VecDeque<(u16, String)>>;

/// Headers of the most recent request per `(method, path)` — lets a test
/// assert the client sent a specific header (e.g. `chatgpt-account-id`)
/// without the mock server needing to branch behavior on it.
type SeenHeaders = HashMap<(String, String), HashMap<String, String>>;

/// Raw body bytes of the most recent request per `(method, path)` — lets a
/// test inspect exactly what the client sent (e.g. the `input` array on a
/// SECOND `/responses` call, to assert a prior turn's reasoning items were
/// echoed back).
type SeenBodies = HashMap<(String, String), Vec<u8>>;

struct MockServer {
    addr: std::net::SocketAddr,
    routes: Arc<Mutex<RouteTable>>,
    seen_headers: Arc<Mutex<SeenHeaders>>,
    seen_bodies: Arc<Mutex<SeenBodies>>,
}

impl MockServer {
    fn start() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind mock server");
        let addr = listener.local_addr().unwrap();
        let routes: Arc<Mutex<RouteTable>> = Arc::new(Mutex::new(HashMap::new()));
        let seen_headers: Arc<Mutex<SeenHeaders>> = Arc::new(Mutex::new(HashMap::new()));
        let seen_bodies: Arc<Mutex<SeenBodies>> = Arc::new(Mutex::new(HashMap::new()));
        let routes_bg = routes.clone();
        let seen_headers_bg = seen_headers.clone();
        let seen_bodies_bg = seen_bodies.clone();
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(stream) = stream else { continue };
                let routes = routes_bg.clone();
                let seen_headers = seen_headers_bg.clone();
                let seen_bodies = seen_bodies_bg.clone();
                std::thread::spawn(move || {
                    let _ = handle_conn(stream, &routes, &seen_headers, &seen_bodies);
                });
            }
        });
        Self {
            addr,
            routes,
            seen_headers,
            seen_bodies,
        }
    }

    fn base_url(&self) -> String {
        format!("http://{}/", self.addr)
    }

    fn queue(&self, method: &str, path: &str, status: u16, body: serde_json::Value) {
        self.queue_raw(method, path, status, body.to_string());
    }

    /// Queue a verbatim body (not JSON-wrapped) — used to serve an SSE
    /// stream to the hand-rolled OAuth `/responses` path.
    fn queue_raw(&self, method: &str, path: &str, status: u16, body: String) {
        self.routes
            .lock()
            .entry((method.to_string(), path.to_string()))
            .or_default()
            .push_back((status, body));
    }

    /// The value of `header_name` (case-insensitive) on the most recent
    /// request to `(method, path)`, if any request has landed yet.
    fn header_seen(&self, method: &str, path: &str, header_name: &str) -> Option<String> {
        let header_name = header_name.to_ascii_lowercase();
        self.seen_headers
            .lock()
            .get(&(method.to_string(), path.to_string()))
            .and_then(|headers| headers.get(&header_name))
            .cloned()
    }

    /// The JSON body of the most recent request to `(method, path)`, if any
    /// request has landed yet.
    fn body_seen(&self, method: &str, path: &str) -> Option<serde_json::Value> {
        self.seen_bodies
            .lock()
            .get(&(method.to_string(), path.to_string()))
            .and_then(|b| serde_json::from_slice(b).ok())
    }
}

fn handle_conn(
    mut stream: TcpStream,
    routes: &Arc<Mutex<RouteTable>>,
    seen_headers: &Arc<Mutex<SeenHeaders>>,
    seen_bodies: &Arc<Mutex<SeenBodies>>,
) -> std::io::Result<()> {
    let mut reader = BufReader::new(stream.try_clone()?);
    let mut request_line = String::new();
    reader.read_line(&mut request_line)?;
    let mut parts = request_line.split_whitespace();
    let method = parts.next().unwrap_or("").to_string();
    let path = parts.next().unwrap_or("/").to_string();

    let mut content_length = 0usize;
    let mut headers = HashMap::new();
    loop {
        let mut line = String::new();
        reader.read_line(&mut line)?;
        if line == "\r\n" || line.is_empty() {
            break;
        }
        if let Some((name, value)) = line.trim_end().split_once(':') {
            let name = name.trim().to_ascii_lowercase();
            let value = value.trim().to_string();
            if name == "content-length" {
                content_length = value.parse().unwrap_or(0);
            }
            headers.insert(name, value);
        }
    }
    let mut body = vec![0u8; content_length];
    reader.read_exact(&mut body)?;

    seen_headers
        .lock()
        .insert((method.clone(), path.clone()), headers);
    seen_bodies
        .lock()
        .insert((method.clone(), path.clone()), body.clone());

    let (status, resp_body) = {
        let mut routes = routes.lock();
        match routes.get_mut(&(method.clone(), path.clone())) {
            Some(q) if !q.is_empty() => q.pop_front().unwrap(),
            _ => (
                500,
                serde_json::json!({"error": format!("no fixture queued for {method} {path}")})
                    .to_string(),
            ),
        }
    };

    let body_str = resp_body;
    let reason = if (200..300).contains(&status) {
        "OK"
    } else {
        "Error"
    };
    write!(
        stream,
        "HTTP/1.1 {status} {reason}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
        body_str.len(),
        body_str
    )?;
    stream.flush()
}

// ---------------------------------------------------------------------
// Shared fixture bodies
// ---------------------------------------------------------------------

fn chat_ok_body(text: &str, prompt_tokens: i64, completion_tokens: i64) -> serde_json::Value {
    serde_json::json!({
        "id": "chatcmpl-test",
        "object": "chat.completion",
        "model": "gpt-4o-mini",
        "choices": [{"index": 0, "message": {"role": "assistant", "content": text}, "finish_reason": "stop"}],
        "usage": {"prompt_tokens": prompt_tokens, "completion_tokens": completion_tokens, "total_tokens": prompt_tokens + completion_tokens}
    })
}

/// A Codex-backend `/responses` SSE stream — the OAuth provider's path,
/// which is hand-rolled and parses server-sent events (not genai/JSON, see
/// `oauth.rs`'s module doc). One text delta plus a terminal
/// `response.completed` carrying usage, matching what the parser consumes.
fn responses_sse_body(text: &str, input_tokens: i64, output_tokens: i64) -> String {
    let delta = serde_json::json!({"type": "response.output_text.delta", "delta": text});
    let completed = serde_json::json!({
        "type": "response.completed",
        "response": {"usage": {"input_tokens": input_tokens, "output_tokens": output_tokens}}
    });
    format!(
        "event: response.output_text.delta\ndata: {delta}\n\n\
         event: response.completed\ndata: {completed}\n\n\
         data: [DONE]\n"
    )
}

/// A Codex-backend `/responses` SSE stream whose terminal `response.completed`
/// carries a `reasoning` item (`encrypted_content`) ahead of the `message`
/// item in `output` — the shape the backend uses when `include:
/// ["reasoning.encrypted_content"]` is requested. No `output_text.delta`s (the
/// text-only fallback path, [`extract_output_text`], reads straight off this
/// same `output` array).
fn responses_sse_body_with_reasoning(
    text: &str,
    reasoning_item: &serde_json::Value,
    input_tokens: i64,
    output_tokens: i64,
) -> String {
    let completed = serde_json::json!({
        "type": "response.completed",
        "response": {
            "usage": {"input_tokens": input_tokens, "output_tokens": output_tokens},
            "output": [
                reasoning_item,
                {
                    "type": "message",
                    "role": "assistant",
                    "content": [{"type": "output_text", "text": text}],
                },
            ],
        }
    });
    format!("event: response.completed\ndata: {completed}\n\ndata: [DONE]\n")
}

/// A JWT with the `https://api.openai.com/auth.chatgpt_account_id` claim
/// Codex-flow access tokens carry — unsigned (`alg: none`-shaped; nothing
/// in the harness verifies the signature, matching `openai-auth`'s own
/// trust posture for tokens it already exchanged). Header/signature
/// segments are placeholders; only the payload segment is read.
fn jwt_with_account_id(account_id: &str) -> String {
    use base64::Engine as _;
    let b64 = |v: &serde_json::Value| {
        base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(v.to_string())
    };
    let header = b64(&serde_json::json!({"alg": "none", "typ": "JWT"}));
    let payload = b64(&serde_json::json!({
        "https://api.openai.com/auth": {"chatgpt_account_id": account_id}
    }));
    format!("{header}.{payload}.sig")
}

fn sample_req() -> TurnRequest {
    TurnRequest {
        messages: vec![Message {
            role: Role::User,
            content: "ping".into(),
            reasoning_items: Vec::new(),
        }],
        max_tokens: None,
    }
}

fn with_config_dir<F: FnOnce()>(dir: &std::path::Path, f: F) {
    std::env::set_var("TIDEPOOL_CONFIG_DIR", dir);
    f();
    std::env::remove_var("TIDEPOOL_CONFIG_DIR");
}

// ---------------------------------------------------------------------
// Shared assertions — one function per behavior, called for both impls.
// ---------------------------------------------------------------------

async fn assert_completes_ok(provider: &impl ModelProvider) {
    let resp = provider
        .complete(sample_req(), None)
        .await
        .expect("complete should succeed");
    assert_eq!(resp.text, "pong");
    assert_eq!(resp.usage.input_tokens, 3);
    assert_eq!(resp.usage.output_tokens, 1);
}

async fn assert_auth_error(provider: &impl ModelProvider) {
    let result = provider.complete(sample_req(), None).await;
    assert!(
        matches!(result, Err(ProviderError::Auth(_))),
        "expected ProviderError::Auth, got {result:?}"
    );
}

// ---------------------------------------------------------------------
// API-key impl
// ---------------------------------------------------------------------

#[tokio::test]
async fn api_key_provider_completes_ok() {
    let server = MockServer::start();
    server.queue("POST", "/chat/completions", 200, chat_ok_body("pong", 3, 1));

    std::env::set_var("SHARED_SUITE_API_KEY_OK", "sk-test");
    let mut cfg = ApiKeyConfig::new("SHARED_SUITE_API_KEY_OK", "gpt-4o-mini");
    cfg.base_url = Some(server.base_url());
    let provider = ApiKeyProvider::new(cfg);

    assert_completes_ok(&provider).await;
    std::env::remove_var("SHARED_SUITE_API_KEY_OK");
}

#[tokio::test]
async fn api_key_provider_401_is_auth_error() {
    let server = MockServer::start();
    server.queue(
        "POST",
        "/chat/completions",
        401,
        serde_json::json!({"error": {"message": "invalid api key", "type": "invalid_request_error"}}),
    );

    std::env::set_var("SHARED_SUITE_API_KEY_BAD", "sk-stale");
    let mut cfg = ApiKeyConfig::new("SHARED_SUITE_API_KEY_BAD", "gpt-4o-mini");
    cfg.base_url = Some(server.base_url());
    let provider = ApiKeyProvider::new(cfg);

    assert_auth_error(&provider).await;
    std::env::remove_var("SHARED_SUITE_API_KEY_BAD");
}

#[tokio::test]
async fn api_key_provider_missing_key_is_auth_error() {
    let dir = tempfile::tempdir().unwrap();
    with_config_dir(dir.path(), || {});
    std::env::set_var("TIDEPOOL_CONFIG_DIR", dir.path());
    std::env::remove_var("SHARED_SUITE_API_KEY_MISSING");
    let cfg = ApiKeyConfig::new("SHARED_SUITE_API_KEY_MISSING", "gpt-4o-mini");
    let provider = ApiKeyProvider::new(cfg);

    let result = provider.complete(sample_req(), None).await;
    assert!(matches!(result, Err(ProviderError::Auth(_))));
    std::env::remove_var("TIDEPOOL_CONFIG_DIR");
}

// ---------------------------------------------------------------------
// OAuth impl
// ---------------------------------------------------------------------

fn oauth_cfg_for_mock(
    chat_server: &MockServer,
    token_server_base: &str,
    token_path: std::path::PathBuf,
) -> OauthConfig {
    let mut cfg = OauthConfig::new("gpt-4o-mini");
    cfg.oauth.token_url = format!("{token_server_base}oauth/token");
    cfg.chat_base_url = Some(chat_server.base_url());
    cfg.token_path = token_path;
    cfg
}

fn write_token(
    path: &std::path::Path,
    access_token: &str,
    refresh_token: &str,
    expires_at_from_now: i64,
) {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64;
    let expires_at = (now + expires_at_from_now).max(0) as u64;
    let token = openai_auth::TokenSet {
        access_token: access_token.to_string(),
        id_token: None,
        refresh_token: refresh_token.to_string(),
        expires_at,
        api_key: None,
    };
    oauth::save_token(path, &token).unwrap();
}

/// The OAuth provider routes through `/v1/responses` (not
/// `/v1/chat/completions`) with a `chatgpt-account-id` header derived from
/// the access-token JWT's `chatgpt_account_id` claim — Codex-flow
/// subscription tokens 401 on chat-completions regardless of `aud`, but
/// pass on responses with this header. See `oauth.rs`'s module doc.
#[tokio::test]
async fn oauth_provider_completes_ok_with_valid_token() {
    let chat_server = MockServer::start();
    chat_server.queue_raw("POST", "/responses", 200, responses_sse_body("pong", 3, 1));

    let dir = tempfile::tempdir().unwrap();
    let token_path = dir.path().join("token.json");
    let token = jwt_with_account_id("acct-123");
    write_token(&token_path, &token, "rt", 3600);

    let cfg = oauth_cfg_for_mock(&chat_server, "http://127.0.0.1:1/", token_path);
    let provider = OauthProvider::new(cfg);

    assert_completes_ok(&provider).await;
    assert_eq!(
        chat_server.header_seen("POST", "/responses", "chatgpt-account-id"),
        Some("acct-123".to_string()),
        "responses request must carry the chatgpt-account-id header"
    );
}

/// The dial's whole point (60-model-dial DONE CRITERIA): mutating a
/// [`SharedModelSettings`] handle a provider was built with
/// ([`OauthProvider::with_live_settings`]) changes the very NEXT request's
/// `model`/`reasoning.effort` — no restart, no new provider. A provider
/// built via the plain [`OauthProvider::new`] (no live handle) is
/// unaffected by this test's existence: that path is covered by
/// `oauth_provider_completes_ok_with_valid_token` above, still constructing
/// a provider the old way.
#[tokio::test]
async fn oauth_provider_with_live_settings_reflects_a_dial_change_on_the_next_request() {
    let chat_server = MockServer::start();
    chat_server.queue_raw("POST", "/responses", 200, responses_sse_body("pong", 3, 1));
    chat_server.queue_raw("POST", "/responses", 200, responses_sse_body("pong2", 3, 1));

    let dir = tempfile::tempdir().unwrap();
    let token_path = dir.path().join("token.json");
    write_token(&token_path, "at", "rt", 3600);

    let mut cfg = oauth_cfg_for_mock(&chat_server, "http://127.0.0.1:1/", token_path);
    cfg.model = "gpt-5.6-terra".to_string();
    cfg.tuning.effort = oauth::ReasoningEffort::Medium;

    let settings_path = dir.path().join("settings.json");
    let live = tidepool_harness::provider::settings::SharedModelSettings::load_or(
        settings_path,
        tidepool_harness::provider::settings::ModelSettings::new(
            "gpt-5.6-terra",
            oauth::ReasoningEffort::Medium,
        ),
    );
    let provider = OauthProvider::with_live_settings(cfg, live.clone());

    provider
        .complete(sample_req(), None)
        .await
        .expect("first request completes");
    let first_body = chat_server
        .body_seen("POST", "/responses")
        .expect("first request landed");
    assert_eq!(first_body["model"], "gpt-5.6-terra");
    assert_eq!(first_body["reasoning"]["effort"], "medium");

    // The dial: mutate the SAME handle the provider holds, no new provider.
    live.set(tidepool_harness::provider::settings::ModelSettings::new(
        "gpt-5.6-sol",
        oauth::ReasoningEffort::High,
    ))
    .expect("dial change persists");

    provider
        .complete(sample_req(), None)
        .await
        .expect("second request completes");
    let second_body = chat_server
        .body_seen("POST", "/responses")
        .expect("second request landed");
    assert_eq!(
        second_body["model"], "gpt-5.6-sol",
        "the next request must use the dialed model"
    );
    assert_eq!(
        second_body["reasoning"]["effort"], "high",
        "the next request must use the dialed effort"
    );
}

#[tokio::test]
async fn oauth_provider_401_from_chat_is_auth_error() {
    let chat_server = MockServer::start();
    chat_server.queue(
        "POST",
        "/responses",
        401,
        serde_json::json!({"error": {"message": "token revoked", "type": "invalid_request_error"}}),
    );

    let dir = tempfile::tempdir().unwrap();
    let token_path = dir.path().join("token.json");
    write_token(&token_path, "revoked-access-token", "rt", 3600);

    let cfg = oauth_cfg_for_mock(&chat_server, "http://127.0.0.1:1/", token_path);
    let provider = OauthProvider::new(cfg);

    assert_auth_error(&provider).await;
}

/// Auth-expiry surfaces `ProviderError::Auth` (60-auth SPEC step 4): an
/// expired access token triggers a refresh; when the refresh token is
/// dead (`invalid_grant`, the standard OAuth error for a revoked/expired
/// refresh token), the whole `complete()` call surfaces `Auth`, not a
/// generic `Api` failure — the caller's next move is `auth/start`, not a
/// retry.
#[tokio::test]
async fn oauth_provider_dead_refresh_token_is_auth_error() {
    let chat_server = MockServer::start(); // never hit — refresh fails first
    let token_server = MockServer::start();
    token_server.queue(
        "POST",
        "/oauth/token",
        400,
        serde_json::json!({"error": "invalid_grant", "error_description": "refresh token expired"}),
    );

    let dir = tempfile::tempdir().unwrap();
    let token_path = dir.path().join("token.json");
    write_token(
        &token_path,
        "stale-access-token",
        "dead-refresh-token",
        -3600,
    );

    let cfg = oauth_cfg_for_mock(&chat_server, &token_server.base_url(), token_path);
    let provider = OauthProvider::new(cfg);

    assert_auth_error(&provider).await;
}

#[tokio::test]
async fn oauth_provider_refreshes_expired_token_and_persists_new_one() {
    let chat_server = MockServer::start();
    chat_server.queue_raw("POST", "/responses", 200, responses_sse_body("pong", 3, 1));
    let token_server = MockServer::start();
    token_server.queue(
        "POST",
        "/oauth/token",
        200,
        serde_json::json!({"access_token": "fresh-access-token", "refresh_token": "fresh-refresh-token", "expires_in": 3600}),
    );

    let dir = tempfile::tempdir().unwrap();
    let token_path = dir.path().join("token.json");
    write_token(
        &token_path,
        "stale-access-token",
        "still-good-refresh-token",
        -3600,
    );

    let cfg = oauth_cfg_for_mock(&chat_server, &token_server.base_url(), token_path.clone());
    let provider = OauthProvider::new(cfg);

    assert_completes_ok(&provider).await;

    let persisted = oauth::load_token(&token_path).expect("refreshed token persisted");
    assert_eq!(persisted.access_token, "fresh-access-token");
    assert_eq!(persisted.refresh_token, "fresh-refresh-token");
}

/// Stateless Responses usage requires echoing prior reasoning items back in
/// the NEXT request's `input`, in position (reasoning-continuity fix): a
/// reasoning item present in a fixture response must reappear, verbatim,
/// immediately ahead of the message it informed, on the following call.
#[tokio::test]
async fn oauth_provider_echoes_reasoning_item_into_next_request() {
    let chat_server = MockServer::start();
    let reasoning_item = serde_json::json!({
        "type": "reasoning",
        "id": "rs_test_1",
        "encrypted_content": "opaque-blob-from-backend",
        "summary": [],
    });
    chat_server.queue_raw(
        "POST",
        "/responses",
        200,
        responses_sse_body_with_reasoning("first answer", &reasoning_item, 10, 5),
    );
    chat_server.queue_raw(
        "POST",
        "/responses",
        200,
        responses_sse_body("second answer", 20, 6),
    );

    let dir = tempfile::tempdir().unwrap();
    let token_path = dir.path().join("token.json");
    write_token(&token_path, &jwt_with_account_id("acct-1"), "rt", 3600);
    let cfg = oauth_cfg_for_mock(&chat_server, "http://127.0.0.1:1/", token_path);
    let provider = OauthProvider::new(cfg);

    let turn1 = provider
        .complete(sample_req(), None)
        .await
        .expect("first turn completes");
    assert_eq!(
        turn1.reasoning_items.len(),
        1,
        "the reasoning item must be captured off the SSE stream"
    );
    assert_eq!(
        turn1.reasoning_items[0].0, reasoning_item,
        "captured item must be verbatim"
    );

    let turn2_req = TurnRequest {
        messages: vec![
            Message {
                role: Role::User,
                content: "ping".into(),
                reasoning_items: Vec::new(),
            },
            Message {
                role: Role::Assistant,
                content: turn1.text.clone(),
                reasoning_items: turn1.reasoning_items.clone(),
            },
        ],
        max_tokens: None,
    };
    provider
        .complete(turn2_req, None)
        .await
        .expect("second turn completes");

    let body = chat_server
        .body_seen("POST", "/responses")
        .expect("second request landed");
    let input = body["input"].as_array().expect("input is an array");
    assert_eq!(
        input.len(),
        3,
        "expected [user, reasoning, assistant] input items, got {input:?}"
    );
    assert_eq!(input[0]["role"], "user");
    assert_eq!(
        input[1], reasoning_item,
        "the reasoning item must be echoed back verbatim"
    );
    assert_eq!(input[2]["role"], "assistant");
    assert_eq!(
        input[2]["content"][0]["text"], "first answer",
        "the reasoning item must be echoed IN POSITION, ahead of the message it informed"
    );
}

/// Wire-level pin for the cache-affinity routing key (`oauth.rs`'s
/// `session_id_for`): the `session-id` header must actually reach the
/// `/responses` request (not just be correct as a pure function), stay
/// IDENTICAL across two calls sharing the same (instructions, opening) — a
/// later round of the SAME window — and DIFFER once the window's opening
/// message changes. `session_id_for`'s own unit tests already pin the pure
/// function; this is the one test that would fail if the header were ever
/// dropped or wired to the wrong value on the actual HTTP request.
#[tokio::test]
async fn oauth_provider_session_id_header_is_present_stable_and_distinct_across_windows() {
    let chat_server = MockServer::start();
    chat_server.queue_raw("POST", "/responses", 200, responses_sse_body("pong", 3, 1));
    chat_server.queue_raw("POST", "/responses", 200, responses_sse_body("pong2", 3, 1));
    chat_server.queue_raw("POST", "/responses", 200, responses_sse_body("pong3", 3, 1));

    let dir = tempfile::tempdir().unwrap();
    let token_path = dir.path().join("token.json");
    write_token(&token_path, &jwt_with_account_id("acct-1"), "rt", 3600);
    let cfg = oauth_cfg_for_mock(&chat_server, "http://127.0.0.1:1/", token_path);
    let provider = OauthProvider::new(cfg);

    let window = TurnRequest {
        messages: vec![
            Message {
                role: Role::System,
                content: "You answer typed holes.".into(),
                reasoning_items: Vec::new(),
            },
            Message {
                role: Role::User,
                content: "opening one".into(),
                reasoning_items: Vec::new(),
            },
        ],
        max_tokens: None,
    };
    provider
        .complete(window.clone(), None)
        .await
        .expect("first round completes");
    let id1 = chat_server
        .header_seen("POST", "/responses", "session-id")
        .expect("session-id header must be present on the request");

    // A later round of the SAME window: append the assistant reply, same
    // instructions and opening.
    let mut window_round2 = window;
    window_round2.messages.push(Message {
        role: Role::Assistant,
        content: "pong".into(),
        reasoning_items: Vec::new(),
    });
    provider
        .complete(window_round2, None)
        .await
        .expect("second round completes");
    let id2 = chat_server
        .header_seen("POST", "/responses", "session-id")
        .expect("session-id header must be present on the request");
    assert_eq!(
        id1, id2,
        "a later round of the same window must reuse the same session-id header"
    );

    // A genuinely different window: a different opening message.
    let other_window = TurnRequest {
        messages: vec![
            Message {
                role: Role::System,
                content: "You answer typed holes.".into(),
                reasoning_items: Vec::new(),
            },
            Message {
                role: Role::User,
                content: "opening two".into(),
                reasoning_items: Vec::new(),
            },
        ],
        max_tokens: None,
    };
    provider
        .complete(other_window, None)
        .await
        .expect("third round completes");
    let id3 = chat_server
        .header_seen("POST", "/responses", "session-id")
        .expect("session-id header must be present on the request");
    assert_ne!(
        id1, id3,
        "a different window's opening message must get a different session-id header"
    );
}

/// Alignment with the official Codex client's cache-routing contract: the
/// request body's `prompt_cache_key` must equal the `session-id` header on
/// the SAME request — the official client sends both with the same session
/// identity (`codex-rs`'s `core/tests/suite/prompt_cache_key.rs`).
#[tokio::test]
async fn oauth_provider_request_body_prompt_cache_key_matches_session_id_header() {
    let chat_server = MockServer::start();
    chat_server.queue_raw("POST", "/responses", 200, responses_sse_body("pong", 3, 1));

    let dir = tempfile::tempdir().unwrap();
    let token_path = dir.path().join("token.json");
    write_token(&token_path, &jwt_with_account_id("acct-1"), "rt", 3600);
    let cfg = oauth_cfg_for_mock(&chat_server, "http://127.0.0.1:1/", token_path);
    let provider = OauthProvider::new(cfg);

    provider
        .complete(sample_req(), None)
        .await
        .expect("turn completes");

    let session_id = chat_server
        .header_seen("POST", "/responses", "session-id")
        .expect("session-id header must be present");
    let body = chat_server
        .body_seen("POST", "/responses")
        .expect("request landed");
    assert_eq!(
        body["prompt_cache_key"],
        serde_json::Value::String(session_id),
        "body's prompt_cache_key must equal the session-id header value"
    );
}

/// Token persistence lands at the config-dir/secrets convention with 0600
/// perms — the same path `complete_login` writes to after the loopback
/// callback exchange.
#[tokio::test]
async fn oauth_token_lands_0600_under_config_dir_convention() {
    let dir = tempfile::tempdir().unwrap();
    with_config_dir(dir.path(), || {
        let cfg = OauthConfig::new("gpt-4o-mini");
        assert!(cfg.token_path.starts_with(dir.path().join("secrets")));

        let token = openai_auth::TokenSet {
            access_token: "at".into(),
            id_token: None,
            refresh_token: "rt".into(),
            expires_at: 0,
            api_key: None,
        };
        oauth::save_token(&cfg.token_path, &token).unwrap();

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&cfg.token_path)
                .unwrap()
                .permissions()
                .mode();
            assert_eq!(mode & 0o777, 0o600);
        }
        assert_eq!(
            oauth::load_token(&cfg.token_path).unwrap().access_token,
            "at"
        );
    });
}
