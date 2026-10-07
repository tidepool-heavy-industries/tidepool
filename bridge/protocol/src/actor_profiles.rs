//! Concrete public actor effect rows. Authority ceilings remain actor-owned.
use std::collections::BTreeSet;

use crate::{gen::GeneratedFile, schema::TypeShape};

#[derive(Clone, Copy)]
enum Row {
    Core,
    ResearchLeaf,
    Research,
    Coding,
    Integration,
    Actor,
}

impl Row {
    fn variant(self) -> &'static str {
        match self {
            Self::Core => "Core",
            Self::ResearchLeaf => "ResearchLeaf",
            Self::Research => "Research",
            Self::Coding => "Coding",
            Self::Integration => "Integration",
            Self::Actor => "Actor",
        }
    }
}

struct RowDefinition {
    row: Row,
    alias: &'static str,
    effects: &'static [&'static str],
}

struct ProfileDefinition {
    variant: &'static str,
    row: Row,
    label: &'static str,
}

fn rows() -> Vec<RowDefinition> {
    vec![
        RowDefinition {
            row: Row::Core,
            alias: "CoreEffects",
            effects: &[
                "Replies",
                "Watches",
                "ActorContext",
                "Notifications",
                "Jev",
                "ModelCall",
                "Commands",
                "Console",
                "Actor",
                "Reflect",
                "Lookup",
            ],
        },
        RowDefinition {
            row: Row::ResearchLeaf,
            alias: "ResearchLeafEffects",
            effects: &[
                "Replies",
                "Watches",
                "ActorContext",
                "BoundWorktree",
                "Notifications",
                "Jev",
                "ModelCall",
                "Commands",
                "Console",
                "Actor",
                "Reflect",
                "Lookup",
            ],
        },
        RowDefinition {
            row: Row::Research,
            alias: "ResearchEffects",
            effects: &[
                "Replies",
                "Watches",
                "Forks",
                "ActorContext",
                "AgentInspection",
                "AgentControl",
                "BoundWorktree",
                "Notifications",
                "Jev",
                "ModelCall",
                "Commands",
                "Console",
                "Actor",
                "Reflect",
                "Lookup",
            ],
        },
        RowDefinition {
            row: Row::Coding,
            alias: "CodingEffects",
            effects: &[
                "Replies",
                "Watches",
                "Forks",
                "ActorContext",
                "AgentInspection",
                "AgentControl",
                "BoundWorktree",
                "WorktreeAllocation",
                "WorktreeIntegration",
                "Notifications",
                "Jev",
                "ModelCall",
                "Commands",
                "Console",
                "Actor",
                "Reflect",
                "Lookup",
                "Source",
            ],
        },
        RowDefinition {
            row: Row::Integration,
            alias: "IntegrationEffects",
            effects: &[
                "Replies",
                "Watches",
                "ActorContext",
                "AgentInspection",
                "BoundWorktree",
                "WorktreeIntegration",
                "Notifications",
                "Jev",
                "ModelCall",
                "Commands",
                "Console",
                "Actor",
                "Reflect",
                "Lookup",
            ],
        },
        RowDefinition {
            row: Row::Actor,
            alias: "ActorEffects",
            effects: &[
                "Replies",
                "Watches",
                "Forks",
                "ActorContext",
                "AgentLaunch",
                "AgentInspection",
                "AgentControl",
                "BoundWorktree",
                "WorktreeRegistry",
                "WorktreeAllocation",
                "WorktreeIntegration",
                "Notifications",
                "Jev",
                "ModelCall",
                "Commands",
                "Console",
                "Actor",
                "Reflect",
                "Lookup",
                "Source",
                "Journal",
                "RepoEvent",
            ],
        },
    ]
}

fn profiles() -> Vec<ProfileDefinition> {
    vec![
        ProfileDefinition {
            variant: "Research",
            row: Row::Research,
            label: "research",
        },
        ProfileDefinition {
            variant: "ResearchLeaf",
            row: Row::ResearchLeaf,
            label: "research-leaf",
        },
        ProfileDefinition {
            variant: "Coding",
            row: Row::Coding,
            label: "coding",
        },
        ProfileDefinition {
            variant: "Scaffolding",
            row: Row::Coding,
            label: "scaffolding",
        },
        ProfileDefinition {
            variant: "Integration",
            row: Row::Integration,
            label: "integration",
        },
    ]
}

fn validate(rows: &[RowDefinition], profiles: &[ProfileDefinition]) -> Result<(), String> {
    let forks = crate::effects::forks::forks();
    let keys = forks
        .type_defs
        .iter()
        .find(|definition| definition.name == "ActorEffectKey")
        .ok_or("ActorEffectKey has no schema owner")?;
    let TypeShape::Sum { variants } = &keys.shape else {
        return Err("ActorEffectKey must be a closed sum".into());
    };
    let keys = variants
        .iter()
        .map(|variant| variant.ctor)
        .collect::<BTreeSet<_>>();
    let mut aliases = BTreeSet::new();
    let mut row_names = BTreeSet::new();
    for definition in rows {
        if !aliases.insert(definition.alias) || !row_names.insert(definition.row.variant()) {
            return Err("duplicate public effect row".into());
        }
        let mut effects = BTreeSet::new();
        for effect in definition.effects {
            if !keys.contains(format!("Effect{effect}").as_str()) {
                return Err(format!(
                    "{} names unknown effect {effect}",
                    definition.alias
                ));
            }
            if !effects.insert(effect) {
                return Err(format!("{} duplicates effect {effect}", definition.alias));
            }
        }
    }
    let mut labels = BTreeSet::new();
    let mut profile_names = BTreeSet::new();
    for profile in profiles {
        if !labels.insert(profile.label) || !profile_names.insert(profile.variant) {
            return Err("duplicate public actor profile".into());
        }
        if !row_names.contains(profile.row.variant()) {
            return Err(format!("{} has no public effect row", profile.label));
        }
    }
    Ok(())
}

/// Project the same concrete rows into compiler-checked Haskell aliases and
/// transport-neutral Rust metadata. No launch or runtime authority is issued.
#[must_use]
pub fn generated_files() -> Vec<GeneratedFile> {
    let rows = rows();
    let profiles = profiles();
    validate(&rows, &profiles).unwrap_or_else(|error| panic!("invalid actor profiles: {error}"));
    let derive = "#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]\n#[serde(rename_all = \"kebab-case\")]\n";
    let mut rust = crate::gen::header("//! ", "Concrete public actor profiles");
    rust.push_str("\nuse crate::ActorEffectKey;\n\n");
    rust.push_str(derive);
    rust.push_str("pub enum PublicActorEffectRow {\n");
    for row in &rows {
        rust.push_str(&format!("    {},\n", row.row.variant()));
    }
    rust.push_str("}\n\nimpl PublicActorEffectRow {\n");
    rust.push_str(&format!("    pub const ALL: [Self; {}] = [\n", rows.len()));
    for row in &rows {
        rust.push_str(&format!("        Self::{},\n", row.row.variant()));
    }
    rust.push_str("    ];\n\n    #[must_use]\n    pub const fn haskell_alias(self) -> &'static str {\n        match self {\n");
    for row in &rows {
        rust.push_str(&format!(
            "            Self::{} => \"{}\",\n",
            row.row.variant(),
            row.alias
        ));
    }
    rust.push_str("        }\n    }\n\n    #[must_use]\n    pub const fn effect_keys(self) -> &'static [ActorEffectKey] {\n        match self {\n");
    for row in &rows {
        rust.push_str(&format!("            Self::{} => &[\n", row.row.variant()));
        for effect in row.effects {
            rust.push_str(&format!("                ActorEffectKey::{effect},\n"));
        }
        rust.push_str("            ],\n");
    }
    rust.push_str("        }\n    }\n}\n\n");
    rust.push_str(derive);
    rust.push_str("pub enum PublicActorProfile {\n");
    for profile in &profiles {
        rust.push_str(&format!("    {},\n", profile.variant));
    }
    rust.push_str("}\n\nimpl PublicActorProfile {\n");
    rust.push_str(&format!(
        "    pub const ALL: [Self; {}] = [\n",
        profiles.len()
    ));
    for profile in &profiles {
        rust.push_str(&format!("        Self::{},\n", profile.variant));
    }
    rust.push_str("    ];\n\n    #[must_use]\n    pub const fn label(self) -> &'static str {\n        match self {\n");
    for profile in &profiles {
        rust.push_str(&format!(
            "            Self::{} => \"{}\",\n",
            profile.variant, profile.label
        ));
    }
    rust.push_str("        }\n    }\n\n    #[must_use]\n    pub const fn effect_row(self) -> PublicActorEffectRow {\n        match self {\n");
    for profile in &profiles {
        rust.push_str(&format!(
            "            Self::{} => PublicActorEffectRow::{},\n",
            profile.variant,
            profile.row.variant()
        ));
    }
    rust.push_str("        }\n    }\n\n    #[must_use]\n    pub const fn effect_keys(self) -> &'static [ActorEffectKey] {\n        self.effect_row().effect_keys()\n    }\n}\n");

    let mut hs = "{-# LANGUAGE DataKinds #-}\n".to_owned();
    hs.push_str(&crate::gen::header(
        "-- ",
        "Concrete public actor effect rows",
    ));
    hs.push_str("module Tidepool.Internal.ActorProfiles\n  ( ");
    hs.push_str(
        &rows
            .iter()
            .map(|row| row.alias)
            .collect::<Vec<_>>()
            .join("\n  , "),
    );
    hs.push_str("\n  ) where\n\nimport Tidepool.Agent.Reply (Replies)\nimport Tidepool.Agent.Watch (Watches)\nimport Tidepool.Effects.Core\n  ( ");
    let core = rows
        .iter()
        .flat_map(|row| row.effects.iter().copied())
        .filter(|effect| !matches!(*effect, "Replies" | "Watches"))
        .collect::<BTreeSet<_>>();
    hs.push_str(&core.into_iter().collect::<Vec<_>>().join("\n  , "));
    hs.push_str("\n  )\n\n");
    for row in &rows {
        hs.push_str(&format!(
            "type {} =\n  '[{}]\n\n",
            row.alias,
            row.effects.join(", ")
        ));
    }
    vec![
        GeneratedFile {
            path: "exomonad/tool/src/generated/public_profiles.rs".into(),
            contents: rust,
        },
        GeneratedFile {
            path: "bridge/haskell/lib/Tidepool/Internal/ActorProfiles.hs".into(),
            contents: hs,
        },
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn public_profile_schema_rejects_duplicate_and_unknown_effects() {
        let profiles = profiles();
        for effects in [&["Replies", "Replies"][..], &["NotAnEffect"][..]] {
            let mut rows = rows();
            rows[0].effects = effects;
            assert!(validate(&rows, &profiles).is_err());
        }
    }
}
