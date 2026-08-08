//! API-key `ModelProvider` impl — co-equal with the OAuth impl in
//! [`super::oauth`]. Key resolution: env var first (an already-set var
//! wins, matching `tidepool_runtime::paths::load_secrets`'s precedence),
//! then the config-dir secrets file of the same name. The chat call itself
//! is `genai`'s job (see `super::http`) — any provider genai supports, not
//! just OpenAI, so `model`/`env_var` are the caller's choice.

use crate::provider::http::{
    build_client, chat_options, map_genai_err, to_chat_request, to_turn_response,
};
use crate::provider::paths::secrets_dir;
use crate::provider::{ModelProvider, ProviderError, TurnRequest, TurnResponse};

#[derive(Debug, Clone)]
pub struct ApiKeyConfig {
    /// Env var / secrets-file name the key is resolved from, e.g.
    /// `"TIDEPOOL_HARNESS_API_KEY"`.
    pub env_var: String,
    pub model: String,
    /// Endpoint override — `None` uses genai's normal resolution for
    /// `model`; tests point this at a local fixture server.
    pub base_url: Option<String>,
}

impl ApiKeyConfig {
    pub fn new(env_var: impl Into<String>, model: impl Into<String>) -> Self {
        Self {
            env_var: env_var.into(),
            model: model.into(),
            base_url: None,
        }
    }

    /// Env var (if set and non-empty) first, else the secrets-dir file of
    /// the same name (trimmed). `None` if neither source has a key.
    pub fn resolve_key(&self) -> Option<String> {
        if let Ok(v) = std::env::var(&self.env_var) {
            if !v.trim().is_empty() {
                return Some(v.trim().to_string());
            }
        }
        let path = secrets_dir().join(&self.env_var);
        std::fs::read_to_string(&path)
            .ok()
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
    }
}

pub struct ApiKeyProvider {
    cfg: ApiKeyConfig,
}

impl ApiKeyProvider {
    pub fn new(cfg: ApiKeyConfig) -> Self {
        Self { cfg }
    }
}

impl ModelProvider for ApiKeyProvider {
    // The API-key path is buffered (genai chat/completions); it doesn't stream
    // to `sink`. Passing `None` from the harness yields the same result, so the
    // observatory simply shows the turn on completion for this provider.
    async fn complete(
        &self,
        req: TurnRequest,
        _sink: Option<crate::provider::StreamSink>,
    ) -> Result<TurnResponse, ProviderError> {
        let key = self.cfg.resolve_key().ok_or_else(|| {
            ProviderError::Auth(format!(
                "no API key found: set {} or write {}",
                self.cfg.env_var,
                secrets_dir().join(&self.cfg.env_var).display()
            ))
        })?;
        let client = build_client(self.cfg.base_url.clone(), key);
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

    fn with_env<F: FnOnce()>(var: &str, val: Option<&str>, f: F) {
        let prior = std::env::var(var).ok();
        match val {
            Some(v) => std::env::set_var(var, v),
            None => std::env::remove_var(var),
        }
        f();
        match prior {
            Some(v) => std::env::set_var(var, v),
            None => std::env::remove_var(var),
        }
    }

    #[test]
    fn resolve_key_prefers_env_over_file() {
        let dir = tempfile::tempdir().unwrap();
        std::env::set_var("TIDEPOOL_CONFIG_DIR", dir.path());
        std::fs::create_dir_all(secrets_dir()).unwrap();
        std::fs::write(secrets_dir().join("TEST_APIKEY_ENV_WINS"), "from-file").unwrap();

        with_env("TEST_APIKEY_ENV_WINS", Some("from-env"), || {
            let cfg = ApiKeyConfig::new("TEST_APIKEY_ENV_WINS", "m");
            assert_eq!(cfg.resolve_key(), Some("from-env".to_string()));
        });

        std::env::remove_var("TIDEPOOL_CONFIG_DIR");
    }

    #[test]
    fn resolve_key_falls_back_to_secrets_file() {
        let dir = tempfile::tempdir().unwrap();
        std::env::set_var("TIDEPOOL_CONFIG_DIR", dir.path());
        std::fs::create_dir_all(secrets_dir()).unwrap();
        std::fs::write(
            secrets_dir().join("TEST_APIKEY_FILE_ONLY"),
            "sk-from-file\n",
        )
        .unwrap();

        with_env("TEST_APIKEY_FILE_ONLY", None, || {
            let cfg = ApiKeyConfig::new("TEST_APIKEY_FILE_ONLY", "m");
            assert_eq!(cfg.resolve_key(), Some("sk-from-file".to_string()));
        });

        std::env::remove_var("TIDEPOOL_CONFIG_DIR");
    }

    #[test]
    fn resolve_key_none_when_absent() {
        let dir = tempfile::tempdir().unwrap();
        std::env::set_var("TIDEPOOL_CONFIG_DIR", dir.path());

        with_env("TEST_APIKEY_ABSENT", None, || {
            let cfg = ApiKeyConfig::new("TEST_APIKEY_ABSENT", "m");
            assert_eq!(cfg.resolve_key(), None);
        });

        std::env::remove_var("TIDEPOOL_CONFIG_DIR");
    }

    #[tokio::test]
    async fn complete_without_key_is_typed_auth_error() {
        let dir = tempfile::tempdir().unwrap();
        std::env::set_var("TIDEPOOL_CONFIG_DIR", dir.path());
        with_env("TEST_APIKEY_MISSING_FOR_COMPLETE", None, || {});

        let cfg = ApiKeyConfig::new("TEST_APIKEY_MISSING_FOR_COMPLETE", "gpt-4o-mini");
        let provider = ApiKeyProvider::new(cfg);
        let req = TurnRequest {
            messages: vec![],
            max_tokens: None,
        };
        let result = provider.complete(req, None).await;
        assert!(matches!(result, Err(ProviderError::Auth(_))));

        std::env::remove_var("TIDEPOOL_CONFIG_DIR");
    }
}
