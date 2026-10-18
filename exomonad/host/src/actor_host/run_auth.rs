//! The run fixes one admitted provider; clones share its transport allowance.

use harness::transport::{
    auth::{
        chatgpt::{RunAccountAuth, SavedAccounts},
        DeepSeekAuth, GeminiAuth,
    },
    Auth, CredentialReference, NativeTransportAdmission, TransportError,
};
use std::sync::Arc;

#[derive(Clone)]
pub(crate) enum RunAuth {
    OpenAIPlan(RunAccountAuth),
    DeepSeek(DeepSeekAuth),
    Gemini(GeminiAuth),
}

impl RunAuth {
    pub(super) fn from_provider(
        provider: &crate::exomonad::model_catalog::ProviderConfig,
        store: Arc<harness::store::Store>, run: String,
    ) -> Result<Self, String> {
        use harness::transport::{NativeTransportContract, ResponsesRoute, ResponsesProtocol};
        use exomonad_actor::Backend;
        provider.validate()?;
        let credential = match (&provider.credential_file, &provider.credential_env) {
            (Some(path), None) => CredentialReference::File { path: path.clone() },
            (None, Some(name)) => CredentialReference::Env { name: name.clone() },
            _ => return Err("provider must select one credential source".into()),
        };
        let (route, protocol, endpoint) = match provider.backend {
            Backend::OpenAIPlan => (ResponsesRoute::ChatGptPlan, ResponsesProtocol::Standard, harness::transport::client::CHATGPT_PLAN_ENDPOINT),
            Backend::DeepSeek => (ResponsesRoute::DeepSeek, ResponsesProtocol::Standard, harness::transport::client::DEEPSEEK_ENDPOINT),
            Backend::GeminiInteractions => (ResponsesRoute::GeminiInteractions, ResponsesProtocol::Interactions, harness::transport::client::GEMINI_INTERACTIONS_ENDPOINT),
        };
        let limits = provider.inference_limits.resolve(provider.backend);
        let admission = NativeTransportAdmission::rehydrate(NativeTransportContract {
            route, protocol, endpoint: provider.endpoint.clone().unwrap_or_else(|| endpoint.into()), credential, limits,
        }).map_err(|e| e.to_string())?;
        match provider.backend {
            Backend::DeepSeek => DeepSeekAuth::from_admission(admission).map(Self::DeepSeek).map_err(|e| e.to_string()),
            Backend::GeminiInteractions => GeminiAuth::from_admission(admission).map(Self::Gemini).map_err(|e| e.to_string()),
            Backend::OpenAIPlan => {
                let path = provider.credential_file.clone().ok_or("OpenAI plan requires a credential file")?;
                let accounts = SavedAccounts::beside(path).map_err(|e| e.to_string())?;
                RunAccountAuth::new_with_limits(store, run, accounts, limits).map(Self::OpenAIPlan).map_err(|e| e.to_string())
            }
        }
    }

    pub(super) fn from_settings(
        settings: &crate::exomonad::EmbeddedLaunchConfig,
        store: Arc<harness::store::Store>,
        run: String,
    ) -> Result<Self, String> {
        let limits = settings.inference_limits.resolve(settings.provider);
        match settings.provider {
            exomonad_actor::Backend::OpenAIPlan => {
                let accounts = SavedAccounts::beside(settings.credential_file.clone())
                    .map_err(|error| error.to_string())?;
                RunAccountAuth::new_with_limits(store, run, accounts, limits)
                    .map(Self::OpenAIPlan)
                    .map_err(|error| error.to_string())
            }
            exomonad_actor::Backend::DeepSeek => {
                let reference = if settings.credential_file.as_os_str().is_empty() {
                    CredentialReference::Env {
                        name: "DEEPSEEK_API_KEY".into(),
                    }
                } else {
                    CredentialReference::File {
                        path: settings.credential_file.clone(),
                    }
                };
                DeepSeekAuth::from_reference(reference, limits)
                    .map(Self::DeepSeek)
                    .map_err(|error| error.to_string())
            }
            exomonad_actor::Backend::GeminiInteractions => {
                let reference = if settings.credential_file.as_os_str().is_empty() {
                    CredentialReference::Env {
                        name: "GEMINI_API_KEY".into(),
                    }
                } else {
                    CredentialReference::File {
                        path: settings.credential_file.clone(),
                    }
                };
                GeminiAuth::from_reference(reference, limits)
                    .map(Self::Gemini)
                    .map_err(|error| error.to_string())
            }
        }
    }

    pub(super) fn validate_for(
        &self,
        settings: &crate::exomonad::EmbeddedLaunchConfig,
    ) -> Result<(), String> {
        if self.backend() != settings.provider {
            return Err("preadmitted transport differs from configured run backend".into());
        }
        let admission = self.admission().map_err(|error| error.to_string())?;
        if admission.contract().limits != settings.inference_limits.resolve(settings.provider) {
            return Err("preadmitted transport differs from configured inference limits".into());
        }
        Ok(())
    }

    pub(super) fn backend(&self) -> exomonad_actor::Backend {
        match self {
            Self::OpenAIPlan(_) => exomonad_actor::Backend::OpenAIPlan,
            Self::DeepSeek(_) => exomonad_actor::Backend::DeepSeek,
            Self::Gemini(_) => exomonad_actor::Backend::GeminiInteractions,
        }
    }

    pub(super) fn account(&self) -> Option<&RunAccountAuth> {
        match self {
            Self::OpenAIPlan(auth) => Some(auth),
            Self::DeepSeek(_) | Self::Gemini(_) => None,
        }
    }
}

impl Auth for RunAuth {
    fn admission(&self) -> Result<NativeTransportAdmission, TransportError> {
        match self {
            Self::OpenAIPlan(auth) => auth.admission(),
            Self::DeepSeek(auth) => auth.admission(),
            Self::Gemini(auth) => auth.admission(),
        }
    }
    fn access(&self) -> Result<String, TransportError> {
        match self {
            Self::OpenAIPlan(auth) => auth.access(),
            Self::DeepSeek(auth) => auth.access(),
            Self::Gemini(auth) => auth.access(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn injected_contract_refuses_backend_and_limits_before_startup_work() {
        let mut settings: crate::exomonad::EmbeddedLaunchConfig = toml::from_str(
            "provider='deepseek'\nlisten='127.0.0.1:0'\nasset_root='/unused'\ncontext_capacity_tokens=4096\n",
        ).unwrap();
        let auth = RunAuth::DeepSeek(
            DeepSeekAuth::from_reference(
                CredentialReference::Env {
                    name: "DEEPSEEK_TEST_REFERENCE_NOT_ACCESSED".into(),
                },
                settings.inference_limits.resolve(settings.provider),
            )
            .unwrap(),
        );
        // The production constructor and admission check do not retrieve the key.
        auth.validate_for(&settings).unwrap();
        settings.inference_limits.max_inflight = Some(5);
        assert!(auth.validate_for(&settings).is_err());
        settings.inference_limits.max_inflight = None;
        auth.validate_for(&settings).unwrap();
        settings.provider = exomonad_actor::Backend::OpenAIPlan;
        assert!(auth.validate_for(&settings).is_err());
    }
}
