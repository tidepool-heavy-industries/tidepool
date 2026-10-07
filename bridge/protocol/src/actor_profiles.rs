//! One general actor effect list; resource authority is issued by runtime owners.
use crate::{gen::GeneratedFile, schema::TypeShape};
use std::collections::BTreeSet;

const EFFECTS: &[&str] = &[
    "Replies",
    "Watches",
    "ResourceScopes",
    "Forks",
    "ActorContext",
    "AgentLaunch",
    "AgentInspection",
    "AgentControl",
    "BoundWorktree",
    "WorktreeRegistry",
    "WorktreeAllocation",
    "WorktreeIntegration",
    "Sleep",
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
];

fn validate(effects: &[&str]) -> Result<(), String> {
    let schema = crate::effects::forks::forks();
    let definition = schema
        .type_defs
        .iter()
        .find(|definition| definition.name == "ActorEffectKey")
        .ok_or("ActorEffectKey has no schema owner")?;
    let TypeShape::Sum { variants } = &definition.shape else {
        return Err("ActorEffectKey must be a closed sum".into());
    };
    let keys = variants
        .iter()
        .map(|variant| variant.ctor)
        .collect::<BTreeSet<_>>();
    let mut seen = BTreeSet::new();
    for effect in effects {
        if !keys.contains(format!("Effect{effect}").as_str()) {
            return Err(format!("unknown actor effect {effect}"));
        }
        if !seen.insert(effect) {
            return Err(format!("duplicate actor effect {effect}"));
        }
    }
    Ok(())
}

/// Haskell alias and native defaults come from the same declared list.
#[must_use]
pub fn generated_files() -> Vec<GeneratedFile> {
    validate(EFFECTS).unwrap_or_else(|error| panic!("invalid default actor effects: {error}"));
    let mut rust = crate::gen::header("//! ", "Default actor effects");
    rust.push_str(
        "\nuse crate::ActorEffectKey;\n\npub const DEFAULT_ACTOR_EFFECTS: &[ActorEffectKey] = &[\n",
    );
    for effect in EFFECTS {
        rust.push_str(&format!("    ActorEffectKey::{effect},\n"));
    }
    rust.push_str("];\n");
    let mut hs = "{-# LANGUAGE DataKinds #-}\n".to_owned();
    hs.push_str(&crate::gen::header("-- ", "Default actor effects"));
    hs.push_str("module Tidepool.Internal.ActorProfiles (ActorEffects) where\n\nimport Tidepool.Agent.Reply (Replies)\nimport Tidepool.Agent.Watch (Watches)\nimport Tidepool.Effects.Core\n  ( ");
    hs.push_str(
        &EFFECTS
            .iter()
            .filter(|effect| !matches!(**effect, "Replies" | "Watches"))
            .copied()
            .collect::<Vec<_>>()
            .join("\n  , "),
    );
    hs.push_str("\n  )\n\ntype ActorEffects =\n  '[");
    hs.push_str(&EFFECTS.join(", "));
    hs.push_str("]\n");
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
    fn default_actor_effects_reject_duplicates_and_unknown_members() {
        assert!(validate(EFFECTS).is_ok());
        assert!(validate(&["Replies", "Replies"]).is_err());
        assert!(validate(&["NotAnEffect"]).is_err());
    }
}
