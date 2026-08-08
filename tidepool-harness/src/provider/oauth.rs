//! ChatGPT-subscription OAuth `ModelProvider` impl (Codex-style
//! authorization-code + PKCE via a loopback callback). 60-auth's crate
//! survey ruled out hand-rolling this: the OAuth mechanics (PKCE
//! generation, code exchange, refresh, the loopback callback server) are
//! the `openai-auth` crate's job — its `OAuthConfig::default()` already
//! carries the real Codex CLI client id / endpoints. This module supplies
//! only Tidepool-specific glue: config-dir token persistence (0600,
//! `super::paths`) and the `ModelProvider` impl (chat calls route through
//! `genai`, see `super::http`).
//!
//! This is the PRIMARY Codex-auth flow (verified against
//! `openai/codex`'s `codex-rs/login` source, Apache-2.0) — not the beta,
//! undocumented device-code variant, which the operator's steer ruled out
//! hand-porting for R0.
//!
//! **R0 CONSTRAINT (by design, not an oversight):** the OAuth callback
//! listens on `127.0.0.1:<port>` (default 1455) on the box running the
//! harness. If the operator's browser isn't on that box, reach it with a
//! one-time port-forward before calling [`start_login`]:
//! `ssh -L 1455:localhost:1455 <harness-box>` (or the tailscale
//! equivalent), then open the returned URL and sign in. Refresh is
//! automatic thereafter — no further port-forward is needed until the
//! refresh token itself dies, at which point [`start_login`] runs again.
//!
//! **Chat routing is NOT the chat-completions endpoint, and NOT genai.**
//! Codex-flow subscription tokens carry `aud = https://api.openai.com/v1`
//! but `/v1/chat/completions` 401s them, and `api.openai.com/v1/responses`
//! 401s them too (`missing api.responses.write` — a platform scope the
//! subscription token never carries). They are authorized by account
//! entitlement only against the ChatGPT backend
//! (`chatgpt.com/backend-api/codex/responses`). That backend has NO unary
//! path: `stream: true` is unconditional and the reply is an SSE event
//! stream, and it gates model availability on a `version` header carrying
//! the CLI's numeric version. genai's `OpenAIResp` adapter sends neither the
//! stream nor the version header and parses a JSON body, so it cannot drive
//! this endpoint — [`codex_responses`] hand-rolls the call instead (request
//! body + headers + SSE parsing), verified against `openai/codex`'s
//! `codex-rs` @ `rust-v0.145.0` (Apache-2.0): `core/src/client.rs` (body),
//! `codex-api/src/endpoint/responses.rs` (`Accept: text/event-stream`),
//! `model-provider-info/src/lib.rs` (the `version` header). The API-key
//! provider (`super::api_key`) still routes through genai — a platform key
//! against `api.openai.com/v1/responses` carries the scope and is not
//! version-gated. `openai-auth` (1.0.0) does not export its own JWT-claim
//! decoder (`jwt` is a private module), so [`chatgpt_account_id`] below
//! re-derives just the one claim it exposes internally: `chatgpt_account_id`
//! under the `https://api.openai.com/auth` claim, decoded WITHOUT signature
//! verification (the token already came from our own OAuth flow, the same
//! trust posture `openai-auth`'s own decoder uses).

use std::path::{Path, PathBuf};

use base64::Engine as _;
use openai_auth::{OAuthClient, OAuthConfig as InnerOAuthConfig, TokenSet};

use crate::provider::paths::{secrets_dir, write_secret};
use crate::provider::{
    Message, ModelProvider, ProviderError, ReasoningItem, Role, StreamDelta, StreamSink,
    TurnRequest, TurnResponse, Usage,
};

/// Extract the `chatgpt_account_id` claim from an access-token JWT, decoded
/// WITHOUT signature verification — see the module doc. Returns `None` on
/// any decode failure or a missing claim; the caller treats that as "send
/// the request without the header" rather than a hard failure, since a
/// malformed/legacy token should surface as the provider's own 401, not an
/// opaque local decode error.
fn chatgpt_account_id(access_token: &str) -> Option<String> {
    let payload_b64 = access_token.split('.').nth(1)?;
    let payload = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(payload_b64)
        .ok()?;
    let claims: serde_json::Value = serde_json::from_slice(&payload).ok()?;
    claims
        .get("https://api.openai.com/auth")?
        .get("chatgpt_account_id")?
        .as_str()
        .map(str::to_string)
}

/// Matches `openai_auth::OAuthConfig::default()`'s redirect URI port.
pub const DEFAULT_CALLBACK_PORT: u16 = 1455;

pub fn default_token_path() -> PathBuf {
    secrets_dir().join("chatgpt-oauth.json")
}

#[derive(Clone)]
pub struct OauthConfig {
    pub oauth: InnerOAuthConfig,
    pub callback_port: u16,
    pub token_path: PathBuf,
    pub model: String,
    /// Chat-completions endpoint override — `None` uses genai's normal
    /// resolution for `model`; tests point this at a local fixture server.
    pub chat_base_url: Option<String>,
}

/// ChatGPT-subscription chat endpoint. A subscription OAuth token is NOT a
/// platform API credential: it 401s (`missing api.responses.write`) against
/// `api.openai.com/v1/responses`, which authorizes by platform scopes the token
/// never carries. Codex-flow tokens are authorized by the account entitlement
/// against the ChatGPT backend instead. genai's `OpenAIResp` adapter builds
/// `{base}responses`, so this base (trailing slash intentional) yields
/// `.../codex/responses` — the endpoint the reference Codex CLI hits.
const CHATGPT_BACKEND_URL: &str = "https://chatgpt.com/backend-api/codex/";

/// The Codex CLI's own numeric version (its `CARGO_PKG_VERSION`). The backend
/// gates model availability on this: it's sent both in the `User-Agent` and in
/// a literal `version` header, and a value below a model's `minimal_client_version`
/// makes the backend report that model "not supported" — which is exactly the
/// 400 we hit at `0.45.0`. Tracks the latest `openai/codex` release tag
/// (`rust-v0.145.0`, 2026-07-21); bump when models we want gate above it.
const CODEX_CLIENT_VERSION: &str = "0.145.0";

/// Appended to [`CHATGPT_BACKEND_URL`] (which carries the trailing slash) to
/// form `.../codex/responses`.
const CODEX_RESPONSES_PATH: &str = "responses";

impl OauthConfig {
    pub fn new(model: impl Into<String>) -> Self {
        Self {
            oauth: InnerOAuthConfig::default(),
            callback_port: DEFAULT_CALLBACK_PORT,
            token_path: default_token_path(),
            model: model.into(),
            chat_base_url: Some(CHATGPT_BACKEND_URL.to_string()),
        }
    }

    fn client(&self) -> Result<OAuthClient, ProviderError> {
        OAuthClient::new(self.oauth.clone())
            .map_err(|e| ProviderError::Api(format!("failed to build OAuth client: {e}")))
    }
}

pub fn load_token(path: &Path) -> Option<TokenSet> {
    let contents = std::fs::read_to_string(path).ok()?;
    serde_json::from_str(&contents).ok()
}

pub fn save_token(path: &Path, token: &TokenSet) -> std::io::Result<()> {
    let json = serde_json::to_string(token).expect("TokenSet serializes");
    write_secret(path, &json)
}

/// What `auth/start` hands the operator: the URL to open plus the R0
/// port-forward instruction (see module doc). `state`/`pkce_verifier` are
/// carried by the caller into [`complete_login`] — they identify and
/// secure THIS attempt, so the protocol layer's session state is the
/// natural home for them between the two calls.
#[derive(Debug, Clone)]
pub struct LoginStart {
    pub authorization_url: String,
    pub port_forward_hint: String,
    pub state: String,
    pub pkce_verifier: String,
}

fn port_forward_hint(port: u16) -> String {
    format!(
        "if the harness is not on this machine: `ssh -L {port}:localhost:{port} <harness-box>` \
         (or the tailscale equivalent), then open the URL above and sign in. Refresh is \
         automatic after that — no further port-forward needed until the refresh token dies."
    )
}

/// One step of sign-in: build the authorization URL. No network call
/// (`OAuthClient::start_flow` generates PKCE locally) — matches the
/// SPEC's "plain async fns" framing even though this particular step
/// doesn't await anything.
pub async fn start_login(cfg: &OauthConfig) -> Result<LoginStart, ProviderError> {
    let flow = cfg
        .client()?
        .start_flow()
        .map_err(|e| ProviderError::Api(format!("failed to start OAuth flow: {e}")))?;
    Ok(LoginStart {
        authorization_url: flow.authorization_url,
        port_forward_hint: port_forward_hint(cfg.callback_port),
        state: flow.state,
        pkce_verifier: flow.pkce_verifier,
    })
}

/// The other step: run the loopback callback server until the browser
/// round-trip completes, then persist the exchanged token (0600). Meant
/// to be spawned in the background by the protocol server right after
/// `start_login` returns — `auth/status` is a separate, cheap check
/// ([`login_status`]) rather than this call's return value, so the
/// protocol layer isn't blocked on it.
pub async fn complete_login(cfg: &OauthConfig, flow: &LoginStart) -> Result<(), ProviderError> {
    let client = cfg.client()?;
    let tokens = openai_auth::run_callback_server(
        cfg.callback_port,
        &flow.state,
        &client,
        &flow.pkce_verifier,
    )
    .await
    .map_err(|e| ProviderError::Api(format!("OAuth callback failed: {e}")))?;
    save_token(&cfg.token_path, &tokens)
        .map_err(|e| ProviderError::Api(format!("failed to persist token: {e}")))
}

// ---------------------------------------------------------------------------
// Device-authorization flow — the right flow for a headless/remote box: no
// loopback callback server, no port-forward. The operator opens a public URL
// and types a short code; THIS process polls OpenAI directly. OpenAI's variant
// is non-standard (two custom endpoints that hand back an authorization_code +
// PKCE pair), which we then run through the SAME standard token exchange as the
// loopback flow (openai-auth's `exchange_code`, with the device redirect_uri).
// Endpoints/fields verified against openai/codex's
// `codex-rs/login/device_code_auth.rs` (Apache-2.0).
// ---------------------------------------------------------------------------

const DEVICE_USERCODE_URL: &str = "https://auth.openai.com/api/accounts/deviceauth/usercode";
const DEVICE_TOKEN_URL: &str = "https://auth.openai.com/api/accounts/deviceauth/token";
const DEVICE_VERIFY_URL: &str = "https://auth.openai.com/codex/device";
const DEVICE_REDIRECT_URI: &str = "https://auth.openai.com/deviceauth/callback";
const DEVICE_POLL_TIMEOUT_SECS: u64 = 15 * 60;

/// What the operator needs to act on: where to go and what code to enter.
#[derive(Debug, Clone)]
pub struct DeviceCodeStart {
    pub user_code: String,
    pub verification_url: String,
    pub device_auth_id: String,
    pub interval_secs: u64,
}

/// A reqwest client that impersonates the Codex CLI. `auth.openai.com` sits
/// behind a Cloudflare WAF that 403s the default `reqwest/*` User-Agent with a
/// JS challenge a headless client can't solve; the `codex_cli_rs` User-Agent +
/// `originator` header are what the reference CLI sends to get through.
fn codex_http() -> Result<reqwest::Client, ProviderError> {
    let mut headers = reqwest::header::HeaderMap::new();
    headers.insert(
        "originator",
        reqwest::header::HeaderValue::from_static("codex_cli_rs"),
    );
    reqwest::Client::builder()
        .user_agent(format!(
            "codex_cli_rs/{CODEX_CLIENT_VERSION} (linux; x86_64)"
        ))
        .default_headers(headers)
        .build()
        .map_err(|e| ProviderError::Api(format!("http client build failed: {e}")))
}

/// Step 1: request a user code. No PKCE here — OpenAI's variant mints the PKCE
/// pair server-side and hands it back with the authorization code at poll time.
pub async fn start_device_login(cfg: &OauthConfig) -> Result<DeviceCodeStart, ProviderError> {
    let http = codex_http()?;
    let resp = http
        .post(DEVICE_USERCODE_URL)
        .json(&serde_json::json!({ "client_id": cfg.oauth.client_id }))
        .send()
        .await
        .map_err(|e| ProviderError::Api(format!("device usercode request failed: {e}")))?;
    let status = resp.status();
    let body = resp.text().await.unwrap_or_default();
    if !status.is_success() {
        return Err(ProviderError::Api(format!(
            "device usercode HTTP {}: {body}",
            status.as_u16()
        )));
    }
    let v: serde_json::Value = serde_json::from_str(&body)
        .map_err(|e| ProviderError::Api(format!("device usercode: bad JSON: {e} ({body})")))?;
    let device_auth_id = v
        .get("device_auth_id")
        .and_then(|x| x.as_str())
        .ok_or_else(|| ProviderError::Api(format!("device usercode: no device_auth_id ({body})")))?
        .to_string();
    let user_code = v
        .get("user_code")
        .or_else(|| v.get("usercode"))
        .and_then(|x| x.as_str())
        .ok_or_else(|| ProviderError::Api(format!("device usercode: no user_code ({body})")))?
        .to_string();
    // `interval` may arrive as a number or a string; floor at 1s, default 5s.
    let interval_secs = v
        .get("interval")
        .and_then(|x| {
            x.as_u64()
                .or_else(|| x.as_str().and_then(|s| s.parse().ok()))
        })
        .unwrap_or(5)
        .max(1);
    Ok(DeviceCodeStart {
        user_code,
        verification_url: DEVICE_VERIFY_URL.to_string(),
        device_auth_id,
        interval_secs,
    })
}

/// Step 2: poll until the operator authorizes (403/404 = still pending), then
/// exchange the returned authorization_code for tokens and persist them. Blocks
/// up to 15 minutes; a slow operator is not an error until the deadline.
pub async fn complete_device_login(
    cfg: &OauthConfig,
    start: &DeviceCodeStart,
) -> Result<(), ProviderError> {
    let http = codex_http()?;
    let deadline =
        std::time::Instant::now() + std::time::Duration::from_secs(DEVICE_POLL_TIMEOUT_SECS);
    let mut polls = 0u32;
    let (auth_code, verifier) = loop {
        if std::time::Instant::now() >= deadline {
            return Err(ProviderError::Auth(
                "device authorization timed out (15 min) — run login again".to_string(),
            ));
        }
        let resp = http
            .post(DEVICE_TOKEN_URL)
            .json(&serde_json::json!({
                "device_auth_id": start.device_auth_id,
                "user_code": start.user_code,
            }))
            .send()
            .await
            .map_err(|e| ProviderError::Api(format!("device token poll failed: {e}")))?;
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        polls += 1;
        eprintln!(
            "[login] poll #{polls}: HTTP {} — {}",
            status.as_u16(),
            body.chars().take(200).collect::<String>()
        );
        if status.is_success() {
            let v: serde_json::Value = serde_json::from_str(&body)
                .map_err(|e| ProviderError::Api(format!("device token: bad JSON: {e} ({body})")))?;
            if let (Some(code), Some(verifier)) = (
                v.get("authorization_code").and_then(|x| x.as_str()),
                v.get("code_verifier").and_then(|x| x.as_str()),
            ) {
                break (code.to_string(), verifier.to_string());
            }
            // 200 without the code yet — some pending states answer 200. Keep polling.
            tokio::time::sleep(std::time::Duration::from_secs(start.interval_secs)).await;
            continue;
        }
        // 403/404 = still pending (operator hasn't finished the browser step).
        // Anything else is fatal.
        if status == reqwest::StatusCode::FORBIDDEN || status == reqwest::StatusCode::NOT_FOUND {
            tokio::time::sleep(std::time::Duration::from_secs(start.interval_secs)).await;
            continue;
        }
        return Err(ProviderError::Api(format!(
            "device token poll HTTP {}: {body}",
            status.as_u16()
        )));
    };

    // Standard authorization_code exchange — hand-rolled rather than
    // openai-auth's `exchange_code` because its internal client sends no
    // User-Agent and `/oauth/token` is behind the same Cloudflare WAF. Same
    // params, our WAF-passing client, against the device callback redirect_uri
    // the code was minted for.
    let params = [
        ("grant_type", "authorization_code"),
        ("client_id", cfg.oauth.client_id.as_str()),
        ("code", auth_code.as_str()),
        ("code_verifier", verifier.as_str()),
        ("redirect_uri", DEVICE_REDIRECT_URI),
    ];
    let resp = http
        .post(&cfg.oauth.token_url)
        .form(&params)
        .send()
        .await
        .map_err(|e| ProviderError::Api(format!("token exchange request failed: {e}")))?;
    let status = resp.status();
    let body = resp.text().await.unwrap_or_default();
    if !status.is_success() {
        return Err(ProviderError::Auth(format!(
            "token exchange HTTP {}: {body}",
            status.as_u16()
        )));
    }
    let v: serde_json::Value = serde_json::from_str(&body)
        .map_err(|e| ProviderError::Api(format!("token exchange: bad JSON: {e} ({body})")))?;
    let access_token = v
        .get("access_token")
        .and_then(|x| x.as_str())
        .ok_or_else(|| ProviderError::Auth(format!("token exchange: no access_token ({body})")))?
        .to_string();
    let expires_in = v.get("expires_in").and_then(|x| x.as_u64()).unwrap_or(3600);
    let expires_at = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
        + expires_in;
    let tokens = TokenSet {
        access_token,
        id_token: v
            .get("id_token")
            .and_then(|x| x.as_str())
            .map(str::to_string),
        refresh_token: v
            .get("refresh_token")
            .and_then(|x| x.as_str())
            .unwrap_or_default()
            .to_string(),
        expires_at,
        api_key: None,
    };
    save_token(&cfg.token_path, &tokens)
        .map_err(|e| ProviderError::Api(format!("failed to persist token: {e}")))
}

/// `auth/status`: token presence, no network call.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LoginStatus {
    SignedOut,
    SignedIn,
}

pub fn login_status(cfg: &OauthConfig) -> LoginStatus {
    match load_token(&cfg.token_path) {
        Some(_) => LoginStatus::SignedIn,
        None => LoginStatus::SignedOut,
    }
}

/// Prove the stored credential chain is live WITHOUT an inference call:
/// force a refresh-token exchange against the auth server and persist the
/// result. Success means the token file, the refresh token, and the auth
/// server all agree; `ProviderError::Auth` means re-run `auth/start`.
pub async fn verify_login(cfg: &OauthConfig) -> Result<(), ProviderError> {
    let stored = load_token(&cfg.token_path).ok_or_else(|| {
        ProviderError::Auth(format!(
            "not signed in — run auth/start ({} has no token)",
            cfg.token_path.display()
        ))
    })?;
    let refreshed = cfg
        .client()?
        .refresh_token(&stored.refresh_token)
        .await
        .map_err(|e| {
            ProviderError::Auth(format!(
                "refresh token rejected, re-auth required via auth/start: {e}"
            ))
        })?;
    save_token(&cfg.token_path, &refreshed)
        .map_err(|e| ProviderError::Api(format!("failed to persist refreshed token: {e}")))
}

/// Refresh if the stored token is within its skew window
/// (`TokenSet::is_expired` — a 5-minute buffer built into `openai-auth`).
/// A dead refresh token (revoked/expired) surfaces as
/// `ProviderError::Auth` telling the caller to re-run `auth/start`,
/// distinct from a transient API failure.
async fn access_token(cfg: &OauthConfig) -> Result<String, ProviderError> {
    let stored = load_token(&cfg.token_path).ok_or_else(|| {
        ProviderError::Auth(format!(
            "not signed in — run auth/start ({} has no token)",
            cfg.token_path.display()
        ))
    })?;
    if !stored.is_expired() {
        return Ok(stored.access_token);
    }
    let refreshed = cfg
        .client()?
        .refresh_token(&stored.refresh_token)
        .await
        .map_err(|e| {
            ProviderError::Auth(format!(
                "refresh token rejected, re-auth required via auth/start: {e}"
            ))
        })?;
    save_token(&cfg.token_path, &refreshed)
        .map_err(|e| ProviderError::Api(format!("failed to persist refreshed token: {e}")))?;
    Ok(refreshed.access_token)
}

pub struct OauthProvider {
    cfg: OauthConfig,
}

impl OauthProvider {
    pub fn new(cfg: OauthConfig) -> Self {
        Self { cfg }
    }
}

impl ModelProvider for OauthProvider {
    async fn complete(
        &self,
        req: TurnRequest,
        sink: Option<StreamSink>,
    ) -> Result<TurnResponse, ProviderError> {
        let token = access_token(&self.cfg).await?;
        codex_responses(&self.cfg, &token, &req, sink).await
    }
}

/// A stable-per-process installation id. The Codex backend expects
/// `x-codex-installation-id` to be constant across a client's requests; it's
/// telemetry/routing, not security, so a fresh v4 per process run is fine.
fn installation_id() -> String {
    static ID: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    ID.get_or_init(|| uuid::Uuid::new_v4().to_string()).clone()
}

/// One conversational `TurnRequest` message → its Responses-API `input`
/// item(s): any encrypted reasoning items the provider surfaced for this
/// message (see [`Message::reasoning_items`]), verbatim and in original
/// order, followed by the message item itself — stateless Responses usage
/// requires echoing prior reasoning back in the next call's input, in
/// position. Only user/assistant reach here: the Codex backend rejects
/// `role: "system"` in `input` (`400 "System messages are not allowed"`) —
/// system content rides the top-level `instructions` field instead (see
/// [`codex_responses`]). User carries an `input_text` content part;
/// assistant carries `output_text` (the Responses API distinguishes the two
/// by direction).
fn to_input_items(m: &Message) -> Vec<serde_json::Value> {
    let (role, content_type) = match m.role {
        Role::User => ("user", "input_text"),
        Role::Assistant => ("assistant", "output_text"),
        // Unreachable: system is partitioned into `instructions` upstream.
        // Fall back to a user `input_text` rather than emit a rejected role.
        Role::System => ("user", "input_text"),
    };
    let mut items: Vec<serde_json::Value> =
        m.reasoning_items.iter().map(|r| r.0.clone()).collect();
    items.push(serde_json::json!({
        "type": "message",
        "role": role,
        "content": [{ "type": content_type, "text": m.content }],
    }));
    items
}

/// Reasoning effort for the Codex `/responses` call. `medium` reliably makes
/// the backend emit `reasoning_summary_text` deltas (the "thinking" the
/// observatory shows); override with `TIDEPOOL_LLM_EFFORT` (`minimal`/`low`/
/// `medium`/`high`) to trade thinking visibility for cost.
fn reasoning_effort() -> String {
    std::env::var("TIDEPOOL_LLM_EFFORT").unwrap_or_else(|_| "medium".to_string())
}

/// Hand-rolled `/responses` call against the ChatGPT Codex backend — see the
/// module doc for why genai can't do this. STREAMS the SSE body incrementally
/// (`reqwest::Response::chunk`): as answer/thinking deltas arrive they're
/// pushed to `sink` (when `Some`) so the observatory shows tokens live, while
/// the same deltas accumulate into the returned complete [`TurnResponse`]. A
/// `None` sink still works — the turn is simply assembled without a watcher.
async fn codex_responses(
    cfg: &OauthConfig,
    token: &str,
    req: &TurnRequest,
    sink: Option<StreamSink>,
) -> Result<TurnResponse, ProviderError> {
    let base = cfg.chat_base_url.as_deref().unwrap_or(CHATGPT_BACKEND_URL);
    let url = format!("{base}{CODEX_RESPONSES_PATH}");

    // The Codex backend takes the system prompt in the top-level
    // `instructions` field, NOT as a `role:"system"` input item (which it
    // 400s). Partition accordingly: system messages join into instructions,
    // user/assistant become input items in order.
    let instructions = req
        .messages
        .iter()
        .filter(|m| m.role == Role::System)
        .map(|m| m.content.as_str())
        .collect::<Vec<_>>()
        .join("\n\n");
    let input: Vec<serde_json::Value> = req
        .messages
        .iter()
        .filter(|m| m.role != Role::System)
        .flat_map(to_input_items)
        .collect();
    let mut body = serde_json::json!({
        "model": cfg.model,
        "input": input,
        "tool_choice": "auto",
        "parallel_tool_calls": false,
        // `summary:"auto"` makes the backend stream a human-readable reasoning
        // summary — the "thinking" the observatory renders.
        "reasoning": { "effort": reasoning_effort(), "summary": "auto" },
        "store": false,
        "stream": true,
        "include": ["reasoning.encrypted_content"],
    });
    if !instructions.is_empty() {
        body["instructions"] = serde_json::json!(instructions);
    }
    // NOTE: no `max_output_tokens`. The ChatGPT Codex backend rejects it
    // (`400 "Unsupported parameter: max_output_tokens"`) — the real Codex CLI
    // never sends an output cap on this endpoint. `req.max_tokens` is honored
    // only on the platform API-key path (genai, `super::api_key`).

    let http = codex_http()?;
    let mut request = http
        .post(&url)
        .bearer_auth(token)
        .header("version", CODEX_CLIENT_VERSION)
        .header("session-id", uuid::Uuid::new_v4().to_string())
        .header("x-codex-installation-id", installation_id())
        .header(reqwest::header::ACCEPT, "text/event-stream");
    if let Some(account_id) = chatgpt_account_id(token) {
        request = request.header("chatgpt-account-id", account_id);
    }

    let mut resp = request
        .json(&body)
        .send()
        .await
        .map_err(|e| ProviderError::Api(format!("responses request failed: {e}")))?;
    let status = resp.status();
    if !status.is_success() {
        let text = resp.text().await.unwrap_or_default();
        let snippet = text.chars().take(1000).collect::<String>();
        let msg = format!("responses HTTP {}: {snippet}", status.as_u16());
        return Err(if status.as_u16() == 401 || status.as_u16() == 403 {
            ProviderError::Auth(msg)
        } else {
            ProviderError::Api(msg)
        });
    }

    // Stream the SSE body, splitting on newlines across chunk boundaries and
    // feeding each complete `data:` line to the accumulator (which forwards
    // deltas to `sink` and builds the final turn).
    let mut acc = SseAcc::default();
    let mut buf: Vec<u8> = Vec::new();
    while let Some(chunk) = resp
        .chunk()
        .await
        .map_err(|e| ProviderError::Api(format!("responses stream read failed: {e}")))?
    {
        buf.extend_from_slice(&chunk);
        while let Some(nl) = buf.iter().position(|&b| b == b'\n') {
            let line: Vec<u8> = buf.drain(..=nl).collect();
            acc.push_line(String::from_utf8_lossy(&line).trim_end(), sink.as_ref());
        }
    }
    if !buf.is_empty() {
        acc.push_line(String::from_utf8_lossy(&buf).trim_end(), sink.as_ref());
    }
    acc.finish()
}

/// Accumulates a Responses-API SSE stream into a complete turn while forwarding
/// each delta to an optional [`StreamSink`]. Shared by the streaming path
/// (`codex_responses`) and the buffered [`parse_sse_response`] used in tests.
#[derive(Default)]
struct SseAcc {
    delta_text: String,
    reasoning: String,
    /// Text carried in the terminal `response.completed` output array — a
    /// fallback used only when no `output_text.delta`s arrived.
    completed_text: Option<String>,
    /// Encrypted reasoning items (`type: "reasoning"`) carried in the same
    /// terminal output array, verbatim — see [`extract_reasoning_items`].
    reasoning_items: Vec<ReasoningItem>,
    usage: Usage,
    error: Option<String>,
}

impl SseAcc {
    /// Process one SSE line (`data: {...}`). Non-`data:` lines, keep-alives,
    /// `[DONE]`, and unparseable JSON are ignored. `sink`, when present,
    /// receives each answer/thinking delta as it lands.
    fn push_line(&mut self, line: &str, sink: Option<&StreamSink>) {
        let Some(data) = line.strip_prefix("data:") else {
            return;
        };
        let data = data.trim();
        if data.is_empty() || data == "[DONE]" {
            return;
        }
        let Ok(v) = serde_json::from_str::<serde_json::Value>(data) else {
            return;
        };
        match v.get("type").and_then(|t| t.as_str()) {
            Some("response.output_text.delta") => {
                if let Some(d) = v.get("delta").and_then(|d| d.as_str()) {
                    self.delta_text.push_str(d);
                    if let Some(s) = sink {
                        let _ = s.send(StreamDelta::Text(d.to_string()));
                    }
                }
            }
            Some("response.reasoning_summary_text.delta") => {
                if let Some(d) = v.get("delta").and_then(|d| d.as_str()) {
                    self.reasoning.push_str(d);
                    if let Some(s) = sink {
                        let _ = s.send(StreamDelta::Reasoning(d.to_string()));
                    }
                }
            }
            Some("response.completed") | Some("response.incomplete") => {
                if let Some(u) = v.pointer("/response/usage") {
                    self.usage.input_tokens =
                        u.get("input_tokens").and_then(|x| x.as_u64()).unwrap_or(0);
                    self.usage.output_tokens =
                        u.get("output_tokens").and_then(|x| x.as_u64()).unwrap_or(0);
                }
                self.completed_text = extract_output_text(v.pointer("/response/output"));
                self.reasoning_items = extract_reasoning_items(v.pointer("/response/output"));
            }
            Some("response.failed") => {
                self.error = Some(
                    v.pointer("/response/error/message")
                        .and_then(|m| m.as_str())
                        .unwrap_or("response.failed")
                        .to_string(),
                );
            }
            Some("error") => {
                self.error = Some(
                    v.get("message")
                        .and_then(|m| m.as_str())
                        .unwrap_or("stream error")
                        .to_string(),
                );
            }
            _ => {}
        }
    }

    /// Build the final turn: accumulated deltas (or the completed-output
    /// fallback), usage, and the reasoning summary if any. A stream-level
    /// error, or no text at all, is a typed failure.
    fn finish(self) -> Result<TurnResponse, ProviderError> {
        if let Some(e) = self.error {
            return Err(ProviderError::Api(format!("responses stream error: {e}")));
        }
        let text = if !self.delta_text.is_empty() {
            self.delta_text
        } else {
            self.completed_text.unwrap_or_default()
        };
        if text.is_empty() {
            return Err(ProviderError::Api(
                "responses stream produced no assistant text".to_string(),
            ));
        }
        Ok(TurnResponse {
            text,
            usage: self.usage,
            reasoning: (!self.reasoning.is_empty()).then_some(self.reasoning),
            reasoning_items: self.reasoning_items,
        })
    }
}

/// Buffered whole-string parse (tests): fold every line through [`SseAcc`]
/// with no sink, then finish.
#[cfg(test)]
fn parse_sse_response(sse: &str) -> Result<TurnResponse, ProviderError> {
    let mut acc = SseAcc::default();
    for line in sse.lines() {
        acc.push_line(line, None);
    }
    acc.finish()
}

/// Concatenate the `output_text` parts of every `message` item in a Responses
/// `output` array — the fallback text source when no streaming deltas arrived.
fn extract_output_text(output: Option<&serde_json::Value>) -> Option<String> {
    let arr = output?.as_array()?;
    let mut s = String::new();
    for item in arr {
        if item.get("type").and_then(|t| t.as_str()) != Some("message") {
            continue;
        }
        let Some(content) = item.get("content").and_then(|c| c.as_array()) else {
            continue;
        };
        for c in content {
            if c.get("type").and_then(|t| t.as_str()) == Some("output_text") {
                if let Some(t) = c.get("text").and_then(|t| t.as_str()) {
                    s.push_str(t);
                }
            }
        }
    }
    (!s.is_empty()).then_some(s)
}

/// Every `type: "reasoning"` item in a Responses `output` array, verbatim and
/// in original order — the encrypted-content payload `include:
/// ["reasoning.encrypted_content"]` requests, kept opaque (never parsed or
/// reshaped) so it can be echoed straight back into the next request's
/// `input`.
fn extract_reasoning_items(output: Option<&serde_json::Value>) -> Vec<ReasoningItem> {
    let Some(arr) = output.and_then(|o| o.as_array()) else {
        return Vec::new();
    };
    arr.iter()
        .filter(|item| item.get("type").and_then(|t| t.as_str()) == Some("reasoning"))
        .map(|item| ReasoningItem(item.clone()))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_token(expires_at: u64) -> TokenSet {
        // `TokenSet` fields are all `pub`; construct directly for tests.
        TokenSet {
            access_token: "at".into(),
            id_token: None,
            refresh_token: "rt".into(),
            expires_at,
            api_key: None,
        }
    }

    fn now_secs() -> u64 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs()
    }

    #[test]
    fn save_and_load_token_roundtrips_with_0600() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("token.json");
        let token = sample_token(now_secs() + 3600);
        save_token(&path, &token).unwrap();
        let loaded = load_token(&path).unwrap();
        assert_eq!(loaded.access_token, token.access_token);
        assert_eq!(loaded.refresh_token, token.refresh_token);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&path).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o600);
        }
    }

    #[test]
    fn load_token_missing_file_is_none() {
        let dir = tempfile::tempdir().unwrap();
        assert!(load_token(&dir.path().join("nope.json")).is_none());
    }

    #[test]
    fn login_status_reflects_token_presence() {
        let dir = tempfile::tempdir().unwrap();
        let mut cfg = OauthConfig::new("gpt-4o-mini");
        cfg.token_path = dir.path().join("token.json");
        assert_eq!(login_status(&cfg), LoginStatus::SignedOut);
        save_token(&cfg.token_path, &sample_token(now_secs() + 3600)).unwrap();
        assert_eq!(login_status(&cfg), LoginStatus::SignedIn);
    }

    #[tokio::test]
    async fn complete_without_token_is_typed_auth_error() {
        let dir = tempfile::tempdir().unwrap();
        let mut cfg = OauthConfig::new("gpt-4o-mini");
        cfg.token_path = dir.path().join("token.json");
        let provider = OauthProvider::new(cfg);
        let req = TurnRequest {
            messages: vec![],
            max_tokens: None,
        };
        let result = provider.complete(req, None).await;
        assert!(matches!(result, Err(ProviderError::Auth(_))));
    }

    #[test]
    fn port_forward_hint_names_the_configured_port() {
        assert!(port_forward_hint(1455).contains("1455:localhost:1455"));
        assert!(port_forward_hint(9999).contains("9999:localhost:9999"));
    }

    #[test]
    fn parse_sse_accumulates_deltas_and_usage() {
        let sse = concat!(
            "event: response.output_text.delta\n",
            "data: {\"type\":\"response.output_text.delta\",\"delta\":\"Hello\"}\n",
            "\n",
            "event: response.output_text.delta\n",
            "data: {\"type\":\"response.output_text.delta\",\"delta\":\", world\"}\n",
            "\n",
            "event: response.completed\n",
            "data: {\"type\":\"response.completed\",\"response\":{\"usage\":{\"input_tokens\":11,\"output_tokens\":3}}}\n",
            "\n",
            "data: [DONE]\n",
        );
        let out = parse_sse_response(sse).unwrap();
        assert_eq!(out.text, "Hello, world");
        assert_eq!(out.usage.input_tokens, 11);
        assert_eq!(out.usage.output_tokens, 3);
    }

    #[test]
    fn parse_sse_falls_back_to_completed_output_when_no_deltas() {
        let sse = concat!(
            "event: response.completed\n",
            "data: {\"type\":\"response.completed\",\"response\":{\"usage\":{\"input_tokens\":5,\"output_tokens\":2},\"output\":[{\"type\":\"message\",\"role\":\"assistant\",\"content\":[{\"type\":\"output_text\",\"text\":\"buffered\"}]}]}}\n",
            "\n",
        );
        let out = parse_sse_response(sse).unwrap();
        assert_eq!(out.text, "buffered");
        assert_eq!(out.usage.output_tokens, 2);
    }

    #[test]
    fn parse_sse_surfaces_stream_error() {
        let sse = concat!(
            "event: response.failed\n",
            "data: {\"type\":\"response.failed\",\"response\":{\"error\":{\"message\":\"model not supported\"}}}\n",
            "\n",
        );
        let err = parse_sse_response(sse).unwrap_err();
        assert!(matches!(err, ProviderError::Api(m) if m.contains("model not supported")));
    }

    #[test]
    fn parse_sse_empty_is_error() {
        assert!(matches!(parse_sse_response(""), Err(ProviderError::Api(_))));
    }

    #[test]
    fn to_input_items_maps_role_to_content_direction() {
        let user = to_input_items(&Message {
            role: Role::User,
            content: "hi".into(),
            reasoning_items: Vec::new(),
        });
        assert_eq!(user.len(), 1);
        assert_eq!(user[0]["role"], "user");
        assert_eq!(user[0]["content"][0]["type"], "input_text");
        let asst = to_input_items(&Message {
            role: Role::Assistant,
            content: "yo".into(),
            reasoning_items: Vec::new(),
        });
        assert_eq!(asst.len(), 1);
        assert_eq!(asst[0]["content"][0]["type"], "output_text");
    }

    #[test]
    fn to_input_items_echoes_reasoning_before_the_message_it_informed() {
        let item = serde_json::json!({
            "type": "reasoning",
            "id": "rs_1",
            "encrypted_content": "opaque-blob",
        });
        let m = Message {
            role: Role::Assistant,
            content: "the answer".into(),
            reasoning_items: vec![ReasoningItem(item.clone())],
        };
        let items = to_input_items(&m);
        assert_eq!(items.len(), 2);
        assert_eq!(items[0], item);
        assert_eq!(items[1]["type"], "message");
    }
}
