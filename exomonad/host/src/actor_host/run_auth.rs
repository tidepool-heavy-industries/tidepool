//! The run fixes one admitted provider; clones share its transport allowance.

use harness::transport::{
    auth::{
        chatgpt::{RunAccountAuth, SavedAccounts},
        DeepSeekAuth,
    },
    Auth, CredentialReference, NativeTransportAdmission, TransportError,
};
use std::sync::Arc;

#[derive(Clone)]
pub(crate) enum RunAuth {
    OpenAIPlan(RunAccountAuth),
    DeepSeek(DeepSeekAuth),
}

impl RunAuth {
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
        }
    }

    pub(super) fn account(&self) -> Option<&RunAccountAuth> {
        match self {
            Self::OpenAIPlan(auth) => Some(auth),
            Self::DeepSeek(_) => None,
        }
    }
}

impl Auth for RunAuth {
    fn admission(&self) -> Result<NativeTransportAdmission, TransportError> {
        match self {
            Self::OpenAIPlan(auth) => auth.admission(),
            Self::DeepSeek(auth) => auth.admission(),
        }
    }
    fn access(&self) -> Result<String, TransportError> {
        match self {
            Self::OpenAIPlan(auth) => auth.access(),
            Self::DeepSeek(auth) => auth.access(),
        }
    }
}
