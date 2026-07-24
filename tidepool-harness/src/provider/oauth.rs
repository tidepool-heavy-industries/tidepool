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

use std::path::{Path, PathBuf};

use openai_auth::{OAuthClient, OAuthConfig as InnerOAuthConfig, TokenSet};

use crate::provider::http::{
    build_client, chat_options, map_genai_err, to_chat_request, to_turn_response,
};
use crate::provider::paths::{secrets_dir, write_secret};
use crate::provider::{ModelProvider, ProviderError, TurnRequest, TurnResponse};

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

impl OauthConfig {
    pub fn new(model: impl Into<String>) -> Self {
        Self {
            oauth: InnerOAuthConfig::default(),
            callback_port: DEFAULT_CALLBACK_PORT,
            token_path: default_token_path(),
            model: model.into(),
            chat_base_url: None,
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
    async fn complete(&self, req: TurnRequest) -> Result<TurnResponse, ProviderError> {
        let token = access_token(&self.cfg).await?;
        let client = build_client(self.cfg.chat_base_url.clone(), token);
        let resp = client
            .exec_chat(
                &self.cfg.model,
                to_chat_request(&req),
                chat_options(&req).as_ref(),
            )
            .await
            .map_err(map_genai_err)?;
        to_turn_response(resp)
    }
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
        let result = provider.complete(req).await;
        assert!(matches!(result, Err(ProviderError::Auth(_))));
    }

    #[test]
    fn port_forward_hint_names_the_configured_port() {
        assert!(port_forward_hint(1455).contains("1455:localhost:1455"));
        assert!(port_forward_hint(9999).contains("9999:localhost:9999"));
    }
}
