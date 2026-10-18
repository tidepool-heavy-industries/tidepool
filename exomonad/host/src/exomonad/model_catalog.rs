//! Pure admission of an authored inference catalog. No credentials are read here.
use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;

use exomonad_actor::{Backend, Model, Reasoning};
use serde::{Deserialize, Serialize};

use super::EmbeddedLaunchConfig;
use super::inference_selection::{AgentInferenceDefaults, InferenceLimitOverrides};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct InferenceConfig {
    pub profile: String,
    pub providers: BTreeMap<String, ProviderConfig>,
    pub models: BTreeMap<String, ModelConfig>,
    pub profiles: BTreeMap<String, ProfileConfig>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ProviderConfig {
    #[serde(with = "super::inference_selection::backend_config")]
    pub backend: Backend,
    pub credential_file: Option<PathBuf>,
    pub credential_env: Option<String>,
    pub endpoint: Option<String>,
    #[serde(default)]
    pub inference_limits: InferenceLimitOverrides,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ModelConfig {
    pub provider: String,
    pub model: String,
    pub constructor: String,
    pub effort: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ProfileConfig {
    pub default: String,
    pub allowed: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ResolvedCatalog {
    pub identity: String,
    pub primary: String,
    pub providers: BTreeMap<String, ProviderConfig>,
    pub models: BTreeMap<String, ResolvedModel>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ResolvedModel {
    pub provider: String,
    pub constructor: String,
    pub model: Model,
    pub reasoning: Reasoning,
}

fn constructor_valid(name: &str) -> bool {
    let mut chars = name.chars();
    chars.next().is_some_and(|c| c.is_ascii_uppercase())
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '\'')
        && !matches!(
            name,
            "True" | "False" | "Just" | "Nothing" | "Left" | "Right" | "LT" | "EQ" | "GT"
        )
}

impl ProviderConfig {
    pub(crate) fn validate(&self) -> Result<(), String> {
        if self.credential_file.is_some() == self.credential_env.is_some() {
            return Err(
                "provider credentials must select exactly one file or environment reference".into(),
            );
        }
        if self
            .credential_file
            .as_ref()
            .is_some_and(|p| !p.is_absolute())
        {
            return Err("provider credential_file must be absolute".into());
        }
        if let Some(name) = &self.credential_env {
            let mut chars = name.chars();
            if !chars
                .next()
                .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
                || !chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
            {
                return Err("provider credential_env must be an environment variable name".into());
            }
        }
        // Endpoint authority is enforced by Harness when constructing its provider.
        if self.endpoint.as_ref().is_some_and(|e| e.trim().is_empty()) {
            return Err("provider endpoint must be nonempty".into());
        }
        Ok(())
    }
}

impl InferenceConfig {
    pub(crate) fn resolve(&self) -> Result<ResolvedCatalog, String> {
        for (name, provider) in &self.providers {
            if name.is_empty() {
                return Err("provider name must be nonempty".into());
            }
            provider
                .validate()
                .map_err(|e| format!("provider {name}: {e}"))?;
        }
        let mut resolved = BTreeMap::new();
        let mut constructors = BTreeSet::new();
        for (name, model) in &self.models {
            if name.is_empty() || model.model.trim().is_empty() {
                return Err("model name and ID must be nonempty".into());
            }
            if !constructor_valid(&model.constructor) || !constructors.insert(&model.constructor) {
                return Err(format!(
                    "invalid or colliding model constructor {}",
                    model.constructor
                ));
            }
            let provider = self
                .providers
                .get(&model.provider)
                .ok_or_else(|| format!("model {name}: unknown provider {}", model.provider))?;
            resolved.insert(
                name.clone(),
                ResolvedModel {
                    provider: model.provider.clone(),
                    constructor: model.constructor.clone(),
                    model: Model::for_backend(provider.backend, &model.model),
                    reasoning: Reasoning::for_backend(provider.backend, &model.effort)?,
                },
            );
        }
        for (name, profile) in &self.profiles {
            let mut allowed = BTreeSet::new();
            if name.is_empty() || profile.allowed.is_empty() {
                return Err("profile name and allowed models must be nonempty".into());
            }
            for model in &profile.allowed {
                if !allowed.insert(model) || !resolved.contains_key(model) {
                    return Err(format!(
                        "profile {name}: duplicate or unknown model {model}"
                    ));
                }
            }
            if !allowed.contains(&profile.default) {
                return Err(format!("profile {name}: default must be an allowed model"));
            }
        }
        let profile = self
            .profiles
            .get(&self.profile)
            .ok_or_else(|| format!("unknown inference profile {}", self.profile))?;
        let models: BTreeMap<_, _> = profile
            .allowed
            .iter()
            .map(|name| (name.clone(), resolved[name].clone()))
            .collect();
        let providers = models
            .values()
            .map(|m| (m.provider.clone(), self.providers[&m.provider].clone()))
            .collect();
        let mut catalog = ResolvedCatalog {
            identity: String::new(),
            primary: profile.default.clone(),
            providers,
            models,
        };
        let bytes = serde_json::to_vec(&(
            &self.profile,
            &catalog.primary,
            &catalog.providers,
            &catalog.models,
        ))
        .map_err(|e| format!("serialize admitted inference catalog: {e}"))?;
        catalog.identity = blake3::hash(&bytes).to_hex().to_string();
        Ok(catalog)
    }

    pub(crate) fn synthesize_legacy(
        defaults: &AgentInferenceDefaults,
        aliases: &BTreeMap<String, Model>,
        embedded: &EmbeddedLaunchConfig,
    ) -> Result<Self, String> {
        defaults.validate()?;
        if defaults.model.backend() != embedded.provider {
            return Err("legacy defaults and provider select different backends".into());
        }
        let provider = "legacy".to_string();
        let mut models = BTreeMap::new();
        models.insert(
            "primary".into(),
            ModelConfig {
                provider: provider.clone(),
                model: defaults.model.value().into(),
                constructor: "Primary".into(),
                effort: defaults.reasoning.value().into(),
            },
        );
        for (alias, model) in aliases {
            if matches!(model, Model::CustomModel(..)) {
                return Err(format!(
                    "legacy alias {alias}: custom IDs require an explicit catalog"
                ));
            }
            if model.backend() != embedded.provider {
                return Err(format!(
                    "legacy alias {alias}: backend differs from provider"
                ));
            }
            let constructor = alias
                .split(|c: char| !c.is_ascii_alphanumeric())
                .filter(|s| !s.is_empty())
                .map(|s| {
                    let mut chars = s.chars();
                    chars.next().unwrap().to_ascii_uppercase().to_string() + chars.as_str()
                })
                .collect::<String>();
            if models
                .insert(
                    alias.clone(),
                    ModelConfig {
                        provider: provider.clone(),
                        model: model.value().into(),
                        constructor,
                        effort: defaults.reasoning.value().into(),
                    },
                )
                .is_some()
            {
                return Err(format!("legacy model alias {alias} collides with primary"));
            }
        }
        let config = Self {
            profile: "legacy".into(),
            providers: BTreeMap::from([(
                provider,
                ProviderConfig {
                    backend: embedded.provider,
                    credential_file: (!embedded.credential_file.as_os_str().is_empty())
                        .then(|| embedded.credential_file.clone()),
                    credential_env: if embedded.credential_file.as_os_str().is_empty() {
                        match embedded.provider.config_name() {
                            "deepseek" => Some("DEEPSEEK_API_KEY".into()),
                            "gemini_interactions" => Some("GEMINI_API_KEY".into()),
                            _ => None,
                        }
                    } else {
                        None
                    },
                    endpoint: None,
                    inference_limits: embedded.inference_limits,
                },
            )]),
            profiles: BTreeMap::from([(
                "legacy".into(),
                ProfileConfig {
                    default: "primary".into(),
                    allowed: models.keys().cloned().collect(),
                },
            )]),
            models,
        };
        config.resolve()?;
        Ok(config)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config() -> InferenceConfig {
        InferenceConfig {
            profile: "work".into(),
            providers: BTreeMap::from([(
                "deepseek".into(),
                ProviderConfig {
                    backend: Backend::DeepSeek,
                    credential_file: None,
                    credential_env: Some("DEEPSEEK_API_KEY".into()),
                    endpoint: None,
                    inference_limits: InferenceLimitOverrides::default(),
                },
            )]),
            models: BTreeMap::from([(
                "flash".into(),
                ModelConfig {
                    provider: "deepseek".into(),
                    model: "deepseek-flash".into(),
                    constructor: "Flash".into(),
                    effort: "high".into(),
                },
            )]),
            profiles: BTreeMap::from([(
                "work".into(),
                ProfileConfig {
                    default: "flash".into(),
                    allowed: vec!["flash".into()],
                },
            )]),
        }
    }

    #[test]
    fn exact_selected_catalog_owns_identity() {
        let mut config = config();
        let initial = config.resolve().unwrap();
        config.models.insert(
            "unused".into(),
            ModelConfig {
                constructor: "Unused".into(),
                ..config.models["flash"].clone()
            },
        );
        assert_eq!(initial.identity, config.resolve().unwrap().identity);
        assert_eq!(initial.models.len(), 1);
        assert_eq!(initial.models["flash"].model, Model::DeepSeekFlash);
        config.models.get_mut("flash").unwrap().effort = "low".into();
        assert_ne!(initial.identity, config.resolve().unwrap().identity);
        config.models.get_mut("flash").unwrap().effort = "high".into();
        config.providers.get_mut("deepseek").unwrap().credential_env = Some("OTHER_KEY".into());
        assert_ne!(initial.identity, config.resolve().unwrap().identity);
    }

    #[test]
    fn profiles_refuse_unknown_duplicate_empty_and_missing_primary() {
        for allowed in [vec![], vec!["unknown"], vec!["flash", "flash"]] {
            let mut config = config();
            config.profiles.get_mut("work").unwrap().allowed =
                allowed.into_iter().map(str::to_string).collect();
            assert!(config.resolve().is_err());
        }
        let mut config = config();
        config.profiles.get_mut("work").unwrap().default = "missing".into();
        assert!(config.resolve().is_err());
        config.profile = "missing".into();
        assert!(config.resolve().is_err());
    }

    #[test]
    fn model_admission_refuses_bad_effort_provider_and_constructor() {
        for constructor in ["lower", "9Bad", "A.B", "Just", "True", ""] {
            let mut config = config();
            config.models.get_mut("flash").unwrap().constructor = constructor.into();
            assert!(config.resolve().is_err(), "{constructor}");
        }
        let mut config = config();
        config
            .models
            .insert("other".into(), config.models["flash"].clone());
        assert!(config.resolve().is_err());
        let mut config = config();
        config.models.get_mut("flash").unwrap().effort = "medium".into();
        assert!(config.resolve().is_err());
        config.models.get_mut("flash").unwrap().provider = "unknown".into();
        assert!(config.resolve().is_err());
    }

    #[test]
    fn credentials_are_references_with_exclusive_valid_shape() {
        let mut provider = config().providers.remove("deepseek").unwrap();
        provider.credential_env = Some("KEY-WITH-DASH".into());
        assert!(provider.validate().is_err());
        provider.credential_env = Some("VALID_KEY".into());
        provider.credential_file = Some("relative".into());
        assert!(provider.validate().is_err());
        provider.credential_env = None;
        assert!(provider.validate().is_err());
        provider.credential_file = Some("/nonexistent/not-read".into());
        assert!(provider.validate().is_ok());
        provider.credential_file = None;
        assert!(provider.validate().is_err());
    }
}
