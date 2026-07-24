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
use std::sync::{Arc, Mutex};

use tidepool_harness::provider::api_key::{ApiKeyConfig, ApiKeyProvider};
use tidepool_harness::provider::oauth::{self, OauthConfig, OauthProvider};
use tidepool_harness::provider::{Message, ModelProvider, ProviderError, Role, TurnRequest};

// ---------------------------------------------------------------------
// Mock server: queued (status, json-body) responses keyed by (method, path).
// ---------------------------------------------------------------------

/// Queued (status, JSON body) responses per pending request, keyed by
/// `(method, path)`.
type RouteTable = HashMap<(String, String), VecDeque<(u16, serde_json::Value)>>;

struct MockServer {
    addr: std::net::SocketAddr,
    routes: Arc<Mutex<RouteTable>>,
}

impl MockServer {
    fn start() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind mock server");
        let addr = listener.local_addr().unwrap();
        let routes: Arc<Mutex<RouteTable>> = Arc::new(Mutex::new(HashMap::new()));
        let routes_bg = routes.clone();
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(stream) = stream else { continue };
                let routes = routes_bg.clone();
                std::thread::spawn(move || {
                    let _ = handle_conn(stream, &routes);
                });
            }
        });
        Self { addr, routes }
    }

    fn base_url(&self) -> String {
        format!("http://{}/", self.addr)
    }

    fn queue(&self, method: &str, path: &str, status: u16, body: serde_json::Value) {
        self.routes
            .lock()
            .unwrap()
            .entry((method.to_string(), path.to_string()))
            .or_default()
            .push_back((status, body));
    }
}

fn handle_conn(mut stream: TcpStream, routes: &Arc<Mutex<RouteTable>>) -> std::io::Result<()> {
    let mut reader = BufReader::new(stream.try_clone()?);
    let mut request_line = String::new();
    reader.read_line(&mut request_line)?;
    let mut parts = request_line.split_whitespace();
    let method = parts.next().unwrap_or("").to_string();
    let path = parts.next().unwrap_or("/").to_string();

    let mut content_length = 0usize;
    loop {
        let mut line = String::new();
        reader.read_line(&mut line)?;
        if line == "\r\n" || line.is_empty() {
            break;
        }
        if let Some(v) = line
            .strip_prefix("Content-Length:")
            .or_else(|| line.strip_prefix("content-length:"))
        {
            content_length = v.trim().parse().unwrap_or(0);
        }
    }
    let mut body = vec![0u8; content_length];
    reader.read_exact(&mut body)?;

    let (status, resp_body) = {
        let mut routes = routes.lock().unwrap();
        match routes.get_mut(&(method.clone(), path.clone())) {
            Some(q) if !q.is_empty() => q.pop_front().unwrap(),
            _ => (
                500,
                serde_json::json!({"error": format!("no fixture queued for {method} {path}")}),
            ),
        }
    };

    let body_str = resp_body.to_string();
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

fn sample_req() -> TurnRequest {
    TurnRequest {
        messages: vec![Message {
            role: Role::User,
            content: "ping".into(),
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
        .complete(sample_req())
        .await
        .expect("complete should succeed");
    assert_eq!(resp.text, "pong");
    assert_eq!(resp.usage.input_tokens, 3);
    assert_eq!(resp.usage.output_tokens, 1);
}

async fn assert_auth_error(provider: &impl ModelProvider) {
    let result = provider.complete(sample_req()).await;
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

    let result = provider.complete(sample_req()).await;
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

#[tokio::test]
async fn oauth_provider_completes_ok_with_valid_token() {
    let chat_server = MockServer::start();
    chat_server.queue("POST", "/chat/completions", 200, chat_ok_body("pong", 3, 1));

    let dir = tempfile::tempdir().unwrap();
    let token_path = dir.path().join("token.json");
    write_token(&token_path, "valid-access-token", "rt", 3600);

    let cfg = oauth_cfg_for_mock(&chat_server, "http://127.0.0.1:1/", token_path);
    let provider = OauthProvider::new(cfg);

    assert_completes_ok(&provider).await;
}

#[tokio::test]
async fn oauth_provider_401_from_chat_is_auth_error() {
    let chat_server = MockServer::start();
    chat_server.queue(
        "POST",
        "/chat/completions",
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
    chat_server.queue("POST", "/chat/completions", 200, chat_ok_body("pong", 3, 1));
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
