//! Configuration syntax is decoded once into the shared inference vocabulary.

use exomonad_actor::{Backend, Model, Reasoning};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentInferenceDefaults {
    pub model: Model,
    pub reasoning: Reasoning,
}

impl AgentInferenceDefaults {
    pub fn deepseek_default() -> Self {
        Self {
            model: Model::DeepSeekFlash,
            reasoning: Reasoning::DeepSeekReasoning(
                exomonad_actor::DeepSeekReasoning::DeepSeekHigh,
            ),
        }
    }

    pub fn validate(&self) -> Result<(), String> {
        if self.model.value().trim().is_empty() {
            return Err("inference model must be nonempty".into());
        }
        if self.model.backend() != self.reasoning.backend() {
            return Err("model and reasoning must select the same backend".into());
        }
        Ok(())
    }
}

/// Bare model strings are the old OpenAI configuration syntax. New selections
/// always carry a backend, including custom model IDs.
#[derive(Debug, Clone, Deserialize)]
#[serde(untagged)]
enum ModelInput {
    LegacyOpenAI(String),
    Qualified(QualifiedModel),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct QualifiedModel {
    backend: String,
    model: String,
}

impl QualifiedModel {
    fn resolve(self) -> Result<Model, String> {
        let backend = Backend::from_config_name(&self.backend)?;
        if self.model.trim().is_empty() {
            return Err("inference model must be nonempty".into());
        }
        Ok(Model::for_backend(backend, self.model.trim()))
    }
}

impl ModelInput {
    fn resolve(self) -> Result<Model, String> {
        match self {
            Self::LegacyOpenAI(model) => QualifiedModel {
                backend: Backend::OpenAIPlan.config_name().into(),
                model,
            }
            .resolve(),
            Self::Qualified(model) => model.resolve(),
        }
    }
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct QualifiedReasoning {
    backend: String,
    effort: String,
}

impl QualifiedReasoning {
    fn resolve(self) -> Result<Reasoning, String> {
        Reasoning::for_backend(Backend::from_config_name(&self.backend)?, &self.effort)
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct DefaultsInput {
    model: ModelInput,
    reasoning: Option<QualifiedReasoning>,
    // Accepted only by the explicit OpenAI legacy reader.
    effort: Option<String>,
}

impl<'de> Deserialize<'de> for AgentInferenceDefaults {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let input = DefaultsInput::deserialize(deserializer)?;
        let legacy = matches!(&input.model, ModelInput::LegacyOpenAI(_));
        let model = input.model.resolve().map_err(serde::de::Error::custom)?;
        let reasoning = match (input.reasoning, input.effort) {
            (Some(reasoning), None) => reasoning.resolve(),
            (None, effort) if legacy => {
                Reasoning::for_backend(Backend::OpenAIPlan, effort.as_deref().unwrap_or("low"))
            }
            (None, None) => Reasoning::for_backend(model.backend(), "high"),
            (None, Some(_)) => Err(
                "legacy effort requires a legacy OpenAI model string; use qualified reasoning"
                    .into(),
            ),
            (Some(_), Some(_)) => {
                Err("select reasoning once; legacy effort and reasoning cannot coexist".into())
            }
        }
        .map_err(serde::de::Error::custom)?;
        let selection = Self { model, reasoning };
        selection.validate().map_err(serde::de::Error::custom)?;
        Ok(selection)
    }
}

impl Serialize for AgentInferenceDefaults {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        #[derive(Serialize)]
        struct Output {
            model: QualifiedModel,
            reasoning: QualifiedReasoning,
        }
        self.validate().map_err(serde::ser::Error::custom)?;
        Output {
            model: QualifiedModel {
                backend: self.model.backend().config_name().into(),
                model: self.model.value().into(),
            },
            reasoning: QualifiedReasoning {
                backend: self.reasoning.backend().config_name().into(),
                effort: self.reasoning.value().into(),
            },
        }
        .serialize(serializer)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn legacy_config_remains_openai_and_new_config_is_explicit() {
        let legacy: AgentInferenceDefaults =
            toml::from_str("model='gpt-6.1-sol'\neffort='medium'").unwrap();
        assert_eq!(legacy.model.backend(), Backend::OpenAIPlan);
        assert_eq!(
            legacy.reasoning,
            Reasoning::for_backend(Backend::OpenAIPlan, "medium").unwrap()
        );
        let default = AgentInferenceDefaults::deepseek_default();
        let text = toml::to_string(&default).unwrap();
        assert!(text.contains("deepseek"));
        assert_eq!(
            toml::from_str::<AgentInferenceDefaults>(&text).unwrap(),
            default
        );
        assert_eq!(
            serde_json::from_str::<AgentInferenceDefaults>(
                &serde_json::to_string(&default).unwrap()
            )
            .unwrap(),
            default
        );
        let old_launch = r#"{"model":"gpt-6.1-sol","effort":"high"}"#;
        let retained: AgentInferenceDefaults = serde_json::from_str(old_launch).unwrap();
        assert_eq!(retained.reasoning.backend(), Backend::OpenAIPlan);
        assert_eq!(retained.reasoning.value(), "high");
    }

    #[test]
    fn qualified_config_refuses_cross_backend_and_legacy_effort_ambiguity() {
        for input in [
            "model={backend='deepseek',model='deepseek-flash'}\nreasoning={backend='chatgpt_plan',effort='high'}",
            "model={backend='deepseek',model='deepseek-flash'}\neffort='medium'",
            "model={backend='deepseek',model='deepseek-flash'}\nreasoning={backend='deepseek',effort='medium'}",
            "model='gpt-6.1-sol'\neffort='low'\nreasoning={backend='chatgpt_plan',effort='high'}",
        ] {
            assert!(toml::from_str::<AgentInferenceDefaults>(input).is_err(), "{input}");
        }
        let explicit: AgentInferenceDefaults =
            toml::from_str("model={backend='deepseek',model='deepseek-flash'}").unwrap();
        assert_eq!(explicit, AgentInferenceDefaults::deepseek_default());
        let legacy: AgentInferenceDefaults = toml::from_str("model='deepseek-flash'").unwrap();
        assert_eq!(legacy.model.backend(), Backend::OpenAIPlan);
    }
}

pub(super) mod backend_config {
    use super::*;
    pub fn serialize<S: serde::Serializer>(
        backend: &Backend,
        serializer: S,
    ) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(backend.config_name())
    }
    pub fn deserialize<'de, D: serde::Deserializer<'de>>(
        deserializer: D,
    ) -> Result<Backend, D::Error> {
        Backend::from_config_name(&String::deserialize(deserializer)?)
            .map_err(serde::de::Error::custom)
    }
}

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct InferenceLimitOverrides {
    pub max_output_tokens: Option<u32>,
    pub connect_timeout_secs: Option<u64>,
    pub total_timeout_secs: Option<u64>,
    pub max_inflight: Option<usize>,
}

impl InferenceLimitOverrides {
    pub fn resolve(self, backend: Backend) -> harness::transport::InferenceLimits {
        let defaults = match backend {
            Backend::DeepSeek => harness::transport::InferenceLimits::deepseek_default(),
            Backend::OpenAIPlan => harness::transport::InferenceLimits::chatgpt_default(),
        };
        harness::transport::InferenceLimits {
            max_output_tokens: self.max_output_tokens.or(defaults.max_output_tokens),
            connect_timeout_secs: self
                .connect_timeout_secs
                .unwrap_or(defaults.connect_timeout_secs),
            total_timeout_secs: self
                .total_timeout_secs
                .unwrap_or(defaults.total_timeout_secs),
            max_inflight: self.max_inflight.unwrap_or(defaults.max_inflight),
        }
    }
}

pub(super) mod models_config {
    use super::*;
    use std::collections::BTreeMap;
    pub fn deserialize<'de, D: serde::Deserializer<'de>>(
        deserializer: D,
    ) -> Result<BTreeMap<String, Model>, D::Error> {
        BTreeMap::<String, ModelInput>::deserialize(deserializer)?
            .into_iter()
            .map(|(name, selection)| {
                selection
                    .resolve()
                    .map(|model| (name, model))
                    .map_err(serde::de::Error::custom)
            })
            .collect()
    }
    pub fn serialize<S: serde::Serializer>(
        models: &BTreeMap<String, Model>,
        serializer: S,
    ) -> Result<S::Ok, S::Error> {
        models
            .iter()
            .map(|(name, model)| {
                (
                    name,
                    QualifiedModel {
                        backend: model.backend().config_name().into(),
                        model: model.value().into(),
                    },
                )
            })
            .collect::<BTreeMap<_, _>>()
            .serialize(serializer)
    }
}
