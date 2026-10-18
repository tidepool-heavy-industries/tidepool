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
            "True"
                | "False"
                | "Just"
                | "Nothing"
                | "Left"
                | "Right"
                | "LT"
                | "EQ"
                | "GT"
                | "ModelSelectionEnvelope"
                | "ArgonOptions"
                | "DeepSeekOptions"
                | "OpenAIOptions"
                | "EffortLow"
                | "EffortMedium"
                | "EffortHigh"
                | "DeepSeekNone"
                | "DeepSeekLow"
                | "DeepSeekHigh"
                | "DeepSeekMax"
                | "OpenAILow"
                | "OpenAIMedium"
                | "OpenAIHigh"
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
            if provider.backend == Backend::GeminiInteractions && model.model != "gemini-4-argon" {
                return Err(format!(
                    "model {name}: Gemini Interactions requires gemini-4-argon"
                ));
            }
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
        let providers: BTreeMap<_, _> = models
            .values()
            .map(|m| (m.provider.clone(), self.providers[&m.provider].clone()))
            .collect();
        // OpenAI account selection belongs to the run, not a provider key.
        // Several models may share that account owner through one provider.
        let openai_providers = providers
            .iter()
            .filter(|(_, provider)| provider.backend == Backend::OpenAIPlan)
            .map(|(name, _)| name.as_str())
            .collect::<Vec<_>>();
        if openai_providers.len() > 1 {
            return Err(format!(
                "profile {} selects multiple OpenAI providers ({}); OpenAI account selection is shared by the run, so reference one shared provider key from all OpenAI models",
                self.profile,
                openai_providers.join(", ")
            ));
        }
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

impl ResolvedCatalog {
    /// Emit the workspace's closed choices from its admitted catalog, never disk inventory.
    pub(crate) fn render_haskell_models(&self) -> Result<String, String> {
        let primary = self
            .models
            .get(&self.primary)
            .ok_or_else(|| "catalog primary model is missing".to_string())?;
        let mut declarations = Vec::new();
        let mut cases = Vec::new();
        let mut allowed = Vec::new();
        let mut constructors = BTreeSet::new();
        for (key, model) in &self.models {
            if !constructor_valid(&model.constructor) || !constructors.insert(&model.constructor) {
                return Err(format!(
                    "invalid or colliding model constructor {}",
                    model.constructor
                ));
            }
            if model.model.backend() != model.reasoning.backend() {
                return Err(format!("model {key}: model and reasoning backends differ"));
            }
            let shape = option_shape(model.model.backend());
            declarations.push(format!("{} {}", model.constructor, shape.options));
            cases.push(format!(
                "  modelSelection ({} options) = ModelSelectionEnvelope {} {} (Core.{} ({} options))",
                model.constructor,
                haskell_literal(&self.identity),
                haskell_literal(key),
                shape.reasoning,
                shape.field,
            ));
            allowed.push(format!(
                "({}, {})",
                haskell_literal(key),
                default_choice(model)?
            ));
        }
        let shape = option_shape(primary.model.backend());
        Ok(format!(
            r#"{{-# LANGUAGE FlexibleContexts #-}}
{{-# LANGUAGE OverloadedStrings #-}}

module Exomonad.Models
  ( Model (..)
  , primaryModel
  , allowedModels
  , setNextEffort
  , module Tidepool.Model.Selection
  ) where

import Prelude
import Control.Monad.Freer (Eff, Member)
import Data.Text (Text)
import qualified Tidepool.Agent.Context as Context
import qualified Tidepool.Effects.Core as Core
import Tidepool.Model.Selection

data Model = {}
  deriving (Eq, Show)

instance ModelChoice Model where
{}

primaryModel :: Model
primaryModel = {}

allowedModels :: [(Text, Model)]
allowedModels = [{}]

setNextEffort :: Member Core.ContextReadWrite effects => {} -> Eff effects ()
setNextEffort = Context.setNextReasoning . Core.{}
"#,
            declarations.join(" | "),
            cases.join("\n"),
            default_choice(primary)?,
            allowed.join(", "),
            shape.effort,
            shape.reasoning,
        ))
    }
}

struct OptionShape {
    options: &'static str,
    effort: &'static str,
    field: &'static str,
    reasoning: &'static str,
    prefix: &'static str,
}

fn option_shape(backend: Backend) -> OptionShape {
    match backend {
        Backend::GeminiInteractions => OptionShape {
            options: "ArgonOptions",
            effort: "ArgonEffort",
            field: "argonEffort",
            reasoning: "ArgonReasoning",
            prefix: "Effort",
        },
        Backend::DeepSeek => OptionShape {
            options: "DeepSeekOptions",
            effort: "DeepSeekReasoning",
            field: "deepSeekEffort",
            reasoning: "DeepSeekReasoning",
            prefix: "DeepSeek",
        },
        Backend::OpenAIPlan => OptionShape {
            options: "OpenAIOptions",
            effort: "OpenAIReasoning",
            field: "openAIEffort",
            reasoning: "OpenAIReasoning",
            prefix: "OpenAI",
        },
    }
}

fn default_choice(model: &ResolvedModel) -> Result<String, String> {
    let shape = option_shape(model.model.backend());
    let effort = match model.reasoning.value() {
        "none" => "None",
        "low" => "Low",
        "medium" => "Medium",
        "high" => "High",
        "max" => "Max",
        effort => return Err(format!("unsupported reasoning setting {effort}")),
    };
    Ok(format!(
        "{} ({} {}{})",
        model.constructor, shape.options, shape.prefix, effort
    ))
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
        let mut config = fixture_config();
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

    fn add_openai_model(config: &mut InferenceConfig, key: &str, provider: &str, path: &str) {
        config.providers.insert(
            provider.into(),
            ProviderConfig {
                backend: Backend::OpenAIPlan,
                credential_file: Some(path.into()),
                credential_env: None,
                endpoint: None,
                inference_limits: InferenceLimitOverrides::default(),
            },
        );
        config.models.insert(
            key.into(),
            ModelConfig {
                provider: provider.into(),
                model: format!("gpt-{key}"),
                constructor: format!("OpenAI{key}"),
                effort: "low".into(),
            },
        );
    }

    #[test]
    fn selected_profile_refuses_multiple_openai_providers_regardless_of_credential_location() {
        for (first, second) in [
            ("/not-read/accounts/one.json", "/not-read/accounts/two.json"),
            ("/not-read/one/account.json", "/not-read/two/account.json"),
            (
                "/not-read/accounts/same.json",
                "/not-read/accounts/same.json",
            ),
        ] {
            let mut config = fixture_config();
            add_openai_model(&mut config, "One", "openai-one", first);
            add_openai_model(&mut config, "Two", "openai-two", second);
            config
                .profiles
                .get_mut("work")
                .unwrap()
                .allowed
                .extend(["One".into(), "Two".into()]);
            assert_eq!(
                config.resolve().unwrap_err(),
                "profile work selects multiple OpenAI providers (openai-one, openai-two); OpenAI account selection is shared by the run, so reference one shared provider key from all OpenAI models"
            );
        }
    }

    #[test]
    fn selected_profile_admits_multiple_openai_models_sharing_one_provider() {
        let mut config = fixture_config();
        add_openai_model(
            &mut config,
            "One",
            "openai",
            "/not-read/accounts/openai.json",
        );
        let mut second = config.models["One"].clone();
        second.model = "gpt-two".into();
        second.constructor = "OpenAITwo".into();
        config.models.insert("Two".into(), second);
        let profile = config.profiles.get_mut("work").unwrap();
        profile.default = "One".into();
        profile.allowed.extend(["One".into(), "Two".into()]);
        let catalog = config.resolve().unwrap();
        assert_eq!(catalog.primary, "One");
        assert_eq!(catalog.providers.len(), 2);
        assert_eq!(catalog.models["One"].provider, "openai");
        assert_eq!(catalog.models["Two"].provider, "openai");
        assert_eq!(catalog.models["One"].model.value(), "gpt-One");
        assert_eq!(catalog.models["Two"].model.value(), "gpt-two");
    }

    #[test]
    fn unselected_openai_providers_do_not_block_the_selected_profile() {
        let mut config = fixture_config();
        let original = config.resolve().unwrap();
        add_openai_model(
            &mut config,
            "One",
            "openai-one",
            "/not-read/one/account.json",
        );
        add_openai_model(
            &mut config,
            "Two",
            "openai-two",
            "/not-read/two/account.json",
        );
        config.profiles.insert(
            "other".into(),
            ProfileConfig {
                default: "One".into(),
                allowed: vec!["One".into(), "Two".into()],
            },
        );
        let catalog = config.resolve().unwrap();
        assert_eq!(catalog.identity, original.identity);
        assert_eq!(
            catalog
                .providers
                .keys()
                .map(String::as_str)
                .collect::<Vec<_>>(),
            vec!["deepseek"]
        );
        config
            .profiles
            .get_mut("work")
            .unwrap()
            .allowed
            .push("One".into());
        let catalog = config.resolve().unwrap();
        assert!(catalog.providers.contains_key("openai-one"));
        assert!(!catalog.providers.contains_key("openai-two"));
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
    fn model_admission_refuses_bad_effort_provider_and_constructor() {
        for constructor in ["lower", "9Bad", "A.B", "Just", "True", ""] {
            let mut config = fixture_config();
            config.models.get_mut("flash").unwrap().constructor = constructor.into();
            assert!(config.resolve().is_err(), "{constructor}");
        }
        let mut config = fixture_config();
        config
            .models
            .insert("other".into(), config.models["flash"].clone());
        assert!(config.resolve().is_err());
        let mut config = fixture_config();
        config.models.get_mut("flash").unwrap().effort = "medium".into();
        assert!(config.resolve().is_err());
        config.models.get_mut("flash").unwrap().provider = "unknown".into();
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
    fn renderer_emits_provider_specific_closed_choices() {
        let mut config = fixture_config();
        for (key, backend, model, constructor, effort) in [
            (
                "argon",
                Backend::GeminiInteractions,
                "gemini-4-argon",
                "Argon",
                "medium",
            ),
            ("openai", Backend::OpenAIPlan, "gpt-6.1-sol", "Sol", "low"),
        ] {
            let mut provider = config.providers["deepseek"].clone();
            provider.backend = backend;
            config.providers.insert(key.into(), provider);
            config.models.insert(
                key.into(),
                ModelConfig {
                    provider: key.into(),
                    model: model.into(),
                    constructor: constructor.into(),
                    effort: effort.into(),
                },
            );
            config
                .profiles
                .get_mut("work")
                .unwrap()
                .allowed
                .push(key.into());
        }
        let catalog = config.resolve().unwrap();
        let source = catalog.render_haskell_models().unwrap();
        assert!(source.contains(
            "data Model = Argon ArgonOptions | Flash DeepSeekOptions | Sol OpenAIOptions"
        ));
        assert!(source.contains("modelSelection (Argon options) = ModelSelection"));
        assert!(source.contains("(Core.ArgonReasoning (argonEffort options))"));
        assert!(source.contains("(Core.DeepSeekReasoning (deepSeekEffort options))"));
        assert!(source.contains("(Core.OpenAIReasoning (openAIEffort options))"));
        assert!(source.contains("primaryModel = Flash (DeepSeekOptions DeepSeekHigh)"));
        assert!(source.contains("(\"argon\", Argon (ArgonOptions EffortMedium))"));
        assert!(source.contains("(\"openai\", Sol (OpenAIOptions OpenAILow))"));
        assert!(source.contains(&format!(
            "ModelSelectionEnvelope \"{}\" \"argon\"",
            catalog.identity
        )));
        assert!(source.contains("setNextEffort :: Member Core.ContextReadWrite effects => DeepSeekReasoning -> Eff effects ()"));
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
    fn renderer_rejects_option_constructor_collisions() {
        for constructor in [
            "ArgonOptions",
            "DeepSeekOptions",
            "OpenAIOptions",
            "EffortLow",
            "EffortMedium",
            "EffortHigh",
            "ModelSelectionEnvelope",
        ] {
            let mut config = fixture_config();
            config.models.get_mut("flash").unwrap().constructor = constructor.into();
            assert!(config.resolve().is_err(), "{constructor}");
        }
        let mut catalog = fixture_config().resolve().unwrap();
        catalog.models.get_mut("flash").unwrap().constructor = "ArgonOptions".into();
        assert!(catalog.render_haskell_models().is_err());
    }

    #[test]
    fn legacy_registered_custom_alias_is_admitted_and_constructor_collision_refused() {
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
        assert_eq!(catalog.models["local-fast"].constructor, "LocalFast");
        assert_eq!(
            catalog.models["local-fast"].model.value(),
            "registered-custom"
        );
        aliases.insert("local_fast".into(), Model::DeepSeekFlash);
        assert!(InferenceConfig::synthesize_legacy(&defaults, &aliases, &embedded).is_err());
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
        assert!(source.contains(
            "data Model = Argon ArgonOptions | Flash DeepSeekOptions | Pro DeepSeekOptions"
        ));
        assert!(source.contains("primaryModel = Argon (ArgonOptions EffortHigh)"));
        assert!(source.contains("setNextEffort :: Member Core.ContextReadWrite effects => ArgonEffort -> Eff effects ()"));
        assert_eq!(
            catalog.models["flash"].reasoning,
            Reasoning::DeepSeekReasoning(DeepSeekReasoning::DeepSeekHigh)
        );
        assert_eq!(
            catalog.models["pro"].reasoning,
            Reasoning::DeepSeekReasoning(DeepSeekReasoning::DeepSeekHigh)
        );
        for key in ["argon", "flash", "pro"] {
            assert!(source.contains(&format!(
                "ModelSelectionEnvelope \"{}\" \"{key}\"",
                catalog.identity
            )));
        }
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

        let mut constructor = base.clone();
        constructor.models.get_mut("argon").unwrap().constructor = "SelectedArgon".into();
        variants.push(constructor);

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
