//! Pure admission of an authored inference catalog. No credentials are read here.
use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;

use exomonad_actor::{Backend, Model, Reasoning};
use serde::{Deserialize, Serialize};

use super::inference_selection::{AgentInferenceDefaults, InferenceLimitOverrides};
use super::EmbeddedLaunchConfig;

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
    pub effort: String,
    #[serde(default)]
    pub allowed_efforts: Vec<String>,
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
    pub model: Model,
    pub reasoning: Reasoning,
    pub allowed_efforts: Vec<Reasoning>,
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
        for (name, model) in &self.models {
            if name.is_empty() || model.model.trim().is_empty() {
                return Err("model name and ID must be nonempty".into());
            }
            let provider = self
                .providers
                .get(&model.provider)
                .ok_or_else(|| format!("model {name}: unknown provider {}", model.provider))?;
            if provider.backend == Backend::GeminiInteractions && model.model != "gemini-4-argon" {
                return Err(format!(
                    "model {name}: Gemini Interactions requires gemini-4-argon"
                ));
            }
            resolved.insert(
                name.clone(),
                ResolvedModel {
                    provider: model.provider.clone(),
                    model: Model::for_backend(provider.backend, &model.model),
                    reasoning: Reasoning::for_backend(provider.backend, &model.effort)?,
                    allowed_efforts: {
                        let initial = Reasoning::for_backend(provider.backend, &model.effort)?;
                        let mut efforts = if model.allowed_efforts.is_empty() {
                            vec![initial]
                        } else {
                            model
                                .allowed_efforts
                                .iter()
                                .map(|effort| Reasoning::for_backend(provider.backend, effort))
                                .collect::<Result<Vec<_>, _>>()?
                        };
                        if !efforts.contains(&initial) {
                            return Err(format!(
                                "model {name}: configured effort must be permitted"
                            ));
                        }
                        let mut unique = Vec::new();
                        for effort in efforts.drain(..) {
                            if !unique.contains(&effort) {
                                unique.push(effort);
                            }
                        }
                        unique
                    },
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
                effort: defaults.reasoning.value().into(),
                allowed_efforts: Vec::new(),
            },
        );
        for (alias, model) in aliases {
            if model.backend() != embedded.provider {
                return Err(format!(
                    "legacy alias {alias}: backend differs from provider"
                ));
            }
            if models
                .insert(
                    alias.clone(),
                    ModelConfig {
                        provider: provider.clone(),
                        model: model.value().into(),
                        effort: defaults.reasoning.value().into(),
                        allowed_efforts: Vec::new(),
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

impl ResolvedCatalog {
    pub(crate) fn root_capabilities(
        &self,
        mut ordinary: Vec<exomonad_tool::ActorEffectKey>,
    ) -> Result<exomonad_actor::ActorCapabilities, String> {
        use exomonad_actor::{
            ActorCapabilities, AdmittedModelIdentity, ConversationPermission, ModelGrant,
            ModelPermissions,
        };
        use exomonad_tool::{ModelOperation, ModelPermission};
        let primary = self
            .models
            .get(&self.primary)
            .ok_or("catalog primary model is missing")?;
        let mut grants = Vec::new();
        let mut own = None;
        for (key, selected) in &self.models {
            let identity = AdmittedModelIdentity::new(
                self.identity.clone(),
                key.clone(),
                selected.provider.clone(),
                selected.model.clone(),
            )
            .map_err(|error| error.to_string())?;
            if key == &self.primary {
                own = Some(
                    ConversationPermission::new(identity.clone(), selected.allowed_efforts.clone())
                        .map_err(|error| error.to_string())?,
                );
            }
            for operation in [
                ModelOperation::SpawnAgent,
                ModelOperation::ForkAgent,
                ModelOperation::ModelCall,
            ] {
                if operation == ModelOperation::ForkAgent
                    && (selected.provider != primary.provider
                        || selected.model.backend() != primary.model.backend())
                {
                    continue;
                }
                grants.push(
                    ModelGrant::new(
                        ModelPermission {
                            operation,
                            family: identity.family(),
                        },
                        identity.clone(),
                        selected.allowed_efforts.clone(),
                    )
                    .map_err(|error| error.to_string())?,
                );
            }
        }
        let permissions = ModelPermissions::new(grants).map_err(|error| error.to_string())?;
        ordinary.retain(|key| key.model_permission().is_none());
        ordinary.extend(permissions.effect_keys());
        ActorCapabilities::admit(ordinary, permissions, own).map_err(|error| error.to_string())
    }

    pub(crate) fn render_haskell_models(&self) -> Result<String, String> {
        let selected = self
            .models
            .get(&self.primary)
            .ok_or("catalog primary model is missing")?;
        let binding = self.render_binding(
            "PrimaryModelFamily",
            "primaryModel",
            &self.primary,
            selected.reasoning,
        )?;
        Ok(format!(
            r#"{{-# LANGUAGE OverloadedStrings #-}}
module Exomonad.Models
  ( catalogIdentity
  , PrimaryModelFamily
  , primaryModel
  , module Tidepool.Model.Selection
  ) where

import Data.Text (Text)
import Tidepool.Model.Selection

catalogIdentity :: Text
catalogIdentity = {identity}

{binding}"#,
            identity = haskell_literal(&self.identity)
        ))
    }

    /// Render a binding from the exact catalog key; provider IDs never become model selectors.
    fn render_binding(
        &self,
        alias: &str,
        name: &str,
        key: &str,
        reasoning: Reasoning,
    ) -> Result<String, String> {
        let selected = self
            .models
            .get(key)
            .ok_or("actor model is not in the admitted catalog")?;
        if !selected.allowed_efforts.contains(&reasoning) {
            return Err("actor reasoning is not in the admitted model efforts".into());
        }
        let (family, constructor, options, prefix) = match selected.model.backend() {
            Backend::GeminiInteractions => ("Argon", "argon", "ArgonOptions", "Effort"),
            Backend::DeepSeek => ("DeepSeek", "deepSeek", "DeepSeekOptions", "DeepSeek"),
            Backend::OpenAIPlan => ("OpenAI", "openAI", "OpenAIOptions", "OpenAI"),
        };
        let effort = match reasoning {
            Reasoning::OpenAIReasoning(exomonad_actor::OpenAIReasoning::OpenAILow)
            | Reasoning::ArgonReasoning(exomonad_actor::ArgonEffort::EffortLow)
            | Reasoning::DeepSeekReasoning(exomonad_actor::DeepSeekReasoning::DeepSeekLow) => "Low",
            Reasoning::OpenAIReasoning(exomonad_actor::OpenAIReasoning::OpenAIMedium)
            | Reasoning::ArgonReasoning(exomonad_actor::ArgonEffort::EffortMedium) => "Medium",
            Reasoning::OpenAIReasoning(exomonad_actor::OpenAIReasoning::OpenAIHigh)
            | Reasoning::ArgonReasoning(exomonad_actor::ArgonEffort::EffortHigh)
            | Reasoning::DeepSeekReasoning(exomonad_actor::DeepSeekReasoning::DeepSeekHigh) => {
                "High"
            }
            Reasoning::DeepSeekReasoning(exomonad_actor::DeepSeekReasoning::DeepSeekNone) => "None",
            Reasoning::DeepSeekReasoning(exomonad_actor::DeepSeekReasoning::DeepSeekMax) => "Max",
        };
        Ok(format!(
            r#"type {alias} = {family}

{name} :: Model {alias}
{name} = {constructor} {identity} {key} ({options} {prefix}{effort})
"#,
            identity = haskell_literal(&self.identity),
            key = haskell_literal(key)
        ))
    }
}

fn haskell_literal(value: &str) -> String {
    format!(
        "\"{}\"",
        tidepool_runtime::session::escape_workbench_haskell_string(value)
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture_config() -> InferenceConfig {
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
                    effort: "high".into(),
                    allowed_efforts: Vec::new(),
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
        let mut config = fixture_config();
        let initial = config.resolve().unwrap();
        config.models.insert(
            "unused".into(),
            ModelConfig {
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
    fn root_grants_preserve_exact_catalog_provider_effort_and_capture_binding() {
        use exomonad_actor::AdmittedModelIdentity;
        use exomonad_tool::ModelOperation;
        let mut config = fixture_config();
        config.models.get_mut("flash").unwrap().allowed_efforts = vec!["low".into(), "high".into()];
        let mut twin = config.models["flash"].clone();
        twin.provider = "other".into();
        config.models.insert("twin".into(), twin);
        config
            .providers
            .insert("other".into(), config.providers["deepseek"].clone());
        config
            .profiles
            .get_mut("work")
            .unwrap()
            .allowed
            .push("twin".into());
        let catalog = config.resolve().unwrap();
        let caps = catalog
            .root_capabilities(vec![exomonad_tool::ActorEffectKey::Console])
            .unwrap();
        let primary = &catalog.models["flash"];
        let own = AdmittedModelIdentity::new(
            catalog.identity.clone(),
            "flash".into(),
            primary.provider.clone(),
            primary.model.clone(),
        )
        .unwrap();
        let twin = AdmittedModelIdentity::new(
            catalog.identity.clone(),
            "twin".into(),
            "other".into(),
            primary.model.clone(),
        )
        .unwrap();
        let low = Reasoning::for_backend(Backend::DeepSeek, "low").unwrap();
        let max = Reasoning::for_backend(Backend::DeepSeek, "max").unwrap();
        assert!(caps
            .model_permissions()
            .permits(ModelOperation::ForkAgent, &own, low));
        assert!(!caps
            .model_permissions()
            .permits(ModelOperation::ForkAgent, &twin, low));
        for op in [ModelOperation::ModelCall, ModelOperation::SpawnAgent] {
            assert!(caps.model_permissions().permits(op, &twin, low));
            assert!(!caps.model_permissions().permits(op, &twin, max));
        }
        let conversation = caps.conversation_permission().unwrap();
        assert!(conversation.permits(&own, low));
        assert!(!conversation.permits(&twin, low));
        assert!(!conversation.permits(&own, max));
    }

    #[test]
    fn profiles_refuse_unknown_duplicate_empty_and_missing_primary() {
        for allowed in [vec![], vec!["unknown"], vec!["flash", "flash"]] {
            let mut config = fixture_config();
            config.profiles.get_mut("work").unwrap().allowed =
                allowed.into_iter().map(str::to_string).collect();
            assert!(config.resolve().is_err());
        }
        let mut config = fixture_config();
        config.profiles.get_mut("work").unwrap().default = "missing".into();
        assert!(config.resolve().is_err());
        config.profile = "missing".into();
        assert!(config.resolve().is_err());
    }

    #[test]
    fn model_admission_refuses_bad_effort_and_provider() {
        let mut config = fixture_config();
        config.models.get_mut("flash").unwrap().effort = "medium".into();
        assert!(config.resolve().is_err());
        config.models.get_mut("flash").unwrap().provider = "unknown".into();
        assert!(config.resolve().is_err());
        let mut config = fixture_config();
        config.models.get_mut("flash").unwrap().allowed_efforts = vec!["low".into()];
        assert!(config.resolve().is_err());
        config
            .models
            .get_mut("flash")
            .unwrap()
            .allowed_efforts
            .push("medium".into());
        assert!(config.resolve().is_err());
    }

    #[test]
    fn credentials_are_references_with_exclusive_valid_shape() {
        let mut provider = fixture_config().providers.remove("deepseek").unwrap();
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

    #[test]
    fn gemini_catalog_admits_only_argon() {
        let mut config = fixture_config();
        config.providers.get_mut("deepseek").unwrap().backend = Backend::GeminiInteractions;
        assert!(config.resolve().is_err());
        config.models.get_mut("flash").unwrap().model = "gemini-4-argon".into();
        let resolved = config.resolve().unwrap();
        assert_eq!(resolved.models["flash"].model, Model::GeminiArgon);
        config.models.get_mut("flash").unwrap().model = "gemini-other".into();
        assert!(config.resolve().is_err());
    }

    #[test]
    fn primary_model_renderer_uses_explicit_family_and_effort() {
        let mut config = fixture_config();
        config.models.get_mut("flash").unwrap().allowed_efforts = vec!["low".into(), "high".into()];
        let catalog = config.resolve().unwrap();
        let source = catalog
            .render_binding(
                "PrimaryModelFamily",
                "primaryModel",
                "flash",
                Reasoning::for_backend(Backend::DeepSeek, "low").unwrap(),
            )
            .unwrap();
        assert!(source.contains("type PrimaryModelFamily = DeepSeek"));
        assert!(source.contains("primaryModel :: Model PrimaryModelFamily"));
        assert!(source.contains("primaryModel = deepSeek"));
        assert!(source.contains("(DeepSeekOptions DeepSeekLow)"));
        assert!(!source.contains("data Model"));
        assert!(!source.contains("ModelChoice"));
        assert!(catalog
            .render_binding(
                "PrimaryModelFamily",
                "primaryModel",
                "unknown",
                catalog.models["flash"].reasoning
            )
            .is_err());
        assert!(catalog
            .render_binding(
                "PrimaryModelFamily",
                "primaryModel",
                "flash",
                Reasoning::for_backend(Backend::OpenAIPlan, "low").unwrap()
            )
            .is_err());
    }

    #[test]
    fn renderer_escapes_catalog_keys_and_identity_with_existing_haskell_owner() {
        let mut catalog = fixture_config().resolve().unwrap();
        let model = catalog.models.remove("flash").unwrap();
        catalog.primary = "key\"\n\\".into();
        catalog.models.insert(catalog.primary.clone(), model);
        catalog.identity = "identity\"\n".into();
        let source = catalog.render_haskell_models().unwrap();
        assert!(source.contains(&haskell_literal(&catalog.identity)));
        assert!(source.contains(&haskell_literal(&catalog.primary)));
        assert!(!source.contains("identity\"\n"));
    }

    #[test]
    fn legacy_registered_custom_alias_preserves_distinct_catalog_keys() {
        let embedded: EmbeddedLaunchConfig = toml::from_str(
            "listen='127.0.0.1:0'\ncontext_capacity_tokens=4096\nprovider='deepseek'",
        )
        .unwrap();
        let defaults = AgentInferenceDefaults::deepseek_default();
        let mut aliases = BTreeMap::from([(
            "local-fast".into(),
            Model::for_backend(Backend::DeepSeek, "registered-custom"),
        )]);
        let catalog = InferenceConfig::synthesize_legacy(&defaults, &aliases, &embedded)
            .unwrap()
            .resolve()
            .unwrap();
        assert_eq!(
            catalog.models["local-fast"].model.value(),
            "registered-custom"
        );
        aliases.insert("local_fast".into(), Model::DeepSeekFlash);
        let distinct = InferenceConfig::synthesize_legacy(&defaults, &aliases, &embedded)
            .unwrap()
            .resolve()
            .unwrap();
        assert_ne!(
            distinct.models["local-fast"].model,
            distinct.models["local_fast"].model
        );
    }

    fn persona_argon_fixture_config() -> super::super::ExomonadConfig {
        toml::from_str(include_str!(
            "../../../examples/persona/.exomonad/config.argon.toml"
        ))
        .expect("the shipped Argon profile must use the production config schema")
    }

    #[test]
    fn shipped_argon_profile_applies_and_renders_exact_admitted_choices() {
        use exomonad_actor::{ArgonEffort, DeepSeekReasoning};
        let mut config = persona_argon_fixture_config();
        config.launch.embedded = Some(
            toml::from_str(
                "listen='127.0.0.1:0'\ncontext_capacity_tokens=4096\nprovider='deepseek'",
            )
            .unwrap(),
        );
        config.apply_inference().unwrap();
        let embedded = config.launch.embedded.as_ref().unwrap();
        assert_eq!(embedded.provider, Backend::GeminiInteractions);
        assert!(embedded.credential_file.as_os_str().is_empty());
        assert_eq!(config.defaults.model, Model::GeminiArgon);
        assert_eq!(
            config.defaults.reasoning,
            Reasoning::ArgonReasoning(ArgonEffort::EffortHigh)
        );
        let catalog = config.resolved_catalog().unwrap().unwrap();
        assert_eq!(catalog.primary, "argon");
        assert_eq!(
            catalog
                .models
                .keys()
                .map(String::as_str)
                .collect::<Vec<_>>(),
            ["argon", "flash", "pro"]
        );
        assert_eq!(catalog.providers.len(), 2);
        assert_eq!(catalog.models["flash"].model, Model::DeepSeekFlash);
        assert_eq!(catalog.models["pro"].model, Model::DeepSeekPro);
        let source = catalog.render_haskell_models().unwrap();
        assert!(source.contains("type PrimaryModelFamily = Argon"));
        assert!(source.contains("primaryModel :: Model PrimaryModelFamily"));
        assert!(source.contains("(ArgonOptions EffortHigh)"));
        assert_eq!(
            catalog.models["flash"].reasoning,
            Reasoning::DeepSeekReasoning(DeepSeekReasoning::DeepSeekHigh)
        );
        assert_eq!(
            catalog.models["pro"].reasoning,
            Reasoning::DeepSeekReasoning(DeepSeekReasoning::DeepSeekHigh)
        );
        assert!(source.contains(&haskell_literal(&catalog.primary)));
    }

    #[test]
    fn shipped_argon_profile_rejects_invalid_effort_before_applying_defaults() {
        let mut config = persona_argon_fixture_config();
        let original = config.defaults.clone();
        config
            .inference
            .as_mut()
            .unwrap()
            .models
            .get_mut("argon")
            .unwrap()
            .effort = "max".into();
        assert!(config.apply_inference().is_err());
        assert_eq!(config.defaults, original);
        let mut config = persona_argon_fixture_config();
        config
            .inference
            .as_mut()
            .unwrap()
            .models
            .get_mut("flash")
            .unwrap()
            .effort = "medium".into();
        assert!(config.apply_inference().is_err());
    }

    #[test]
    fn selected_catalog_identity_binds_primary_keys_providers_options_and_limits() {
        let base = persona_argon_fixture_config().inference.unwrap();
        let identity = base.resolve().unwrap().identity;
        let mut variants = Vec::new();

        let mut primary = base.clone();
        primary.profiles.get_mut("argon").unwrap().default = "flash".into();
        variants.push(primary);

        let mut key = base.clone();
        let model = key.models.remove("argon").unwrap();
        key.models.insert("renamed".into(), model);
        let profile = key.profiles.get_mut("argon").unwrap();
        profile.default = "renamed".into();
        profile.allowed[0] = "renamed".into();
        variants.push(key);

        let mut binding = base.clone();
        binding
            .providers
            .insert("gemini-copy".into(), binding.providers["gemini"].clone());
        binding.models.get_mut("argon").unwrap().provider = "gemini-copy".into();
        variants.push(binding);

        let mut options = base.clone();
        options.models.get_mut("argon").unwrap().effort = "medium".into();
        variants.push(options);

        let mut limits = base.clone();
        limits
            .providers
            .get_mut("gemini")
            .unwrap()
            .inference_limits
            .max_output_tokens = Some(1234);
        variants.push(limits);

        let mut efforts = base.clone();
        efforts.models.get_mut("argon").unwrap().allowed_efforts = vec!["high".into()];
        variants.push(efforts);

        let mut membership = base.clone();
        membership
            .profiles
            .get_mut("argon")
            .unwrap()
            .allowed
            .retain(|key| key != "pro");
        variants.push(membership);

        for variant in variants {
            assert_ne!(variant.resolve().unwrap().identity, identity);
        }
        let mut unselected = base;
        unselected.profiles.insert(
            "another-profile".into(),
            ProfileConfig {
                default: "flash".into(),
                allowed: vec!["flash".into()],
            },
        );
        assert_eq!(unselected.resolve().unwrap().identity, identity);
    }
}
