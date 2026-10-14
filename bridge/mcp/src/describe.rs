//! Derive authored vocabulary documentation from [`EffectDecl`] metadata.

use crate::effect_decls::{EffectDecl, HelperDecl, HelperVisibility};

#[must_use]
pub fn has_curated_authored_surface(type_name: &str) -> bool {
    crate::generated::CURATED_EFFECTS.contains(&type_name)
}

#[must_use]
pub fn authored_name_is_hidden(effect: &str, name: &str) -> bool {
    crate::generated::AUTHORED_HIDDEN_BY_EFFECT
        .iter()
        .find(|(owner, _)| *owner == effect)
        .is_some_and(|(_, hidden)| hidden.contains(&name))
}

fn constructor_name(constructor: &str) -> Option<&str> {
    constructor.split_once("::").map(|(name, _)| name.trim())
}

fn type_definition_name(definition: &str) -> Option<&str> {
    definition.split_whitespace().nth(1)
}

/// The canonical model-visible constructor slice for one effect.
#[must_use]
pub fn authored_constructors(decl: &EffectDecl) -> Vec<&str> {
    decl.constructors
        .iter()
        .copied()
        .filter(|constructor| {
            constructor_name(constructor)
                .is_none_or(|name| !authored_name_is_hidden(decl.type_name, name))
        })
        .collect()
}

/// The canonical model-visible supporting-type slice for one effect.
#[must_use]
pub fn authored_type_definitions(decl: &EffectDecl) -> Vec<&str> {
    decl.type_defs
        .iter()
        .copied()
        .filter(|definition| {
            type_definition_name(definition)
                .is_none_or(|name| !authored_name_is_hidden(decl.type_name, name))
        })
        .collect()
}

/// The canonical model-visible helper-source slice for one effect.
#[must_use]
pub fn authored_helpers(decl: &EffectDecl) -> Vec<HelperDecl> {
    decl.helpers
        .iter()
        .copied()
        .filter(|helper| {
            helper
                .name
                .is_none_or(|name| !authored_name_is_hidden(decl.type_name, name))
        })
        .collect()
}

/// The leading sentence used by compact indexes; the declaration retains
/// the complete description for detailed inspection.
pub fn first_sentence(desc: &str) -> &str {
    let d = desc.trim();
    match d.find(". ") {
        // Keep the period; drop the trailing space + rest.
        Some(i) => d[..=i].trim_end(),
        None => d,
    }
}

/// An effect's summary and named public helpers. Substrate and unnamed
/// fragments remain available as source in the declaration.
pub fn describe_effect(decl: &EffectDecl) -> String {
    let verbs: Vec<&str> = authored_helpers(decl)
        .into_iter()
        .filter(|h| h.visibility == HelperVisibility::Public)
        .filter_map(|helper| helper.name)
        .collect();
    let mut s = format!("  {}: {}", decl.type_name, first_sentence(decl.description));
    if !verbs.is_empty() {
        s.push_str("\n      verbs: ");
        s.push_str(&verbs.join(", "));
    }
    s
}

/// The derived effects-enumeration block for a tool description: one
/// [`describe_effect`] entry per decl, newline-joined (trailing newline
/// included). Callers supply their own framing header — this is only the
/// mechanical per-effect body.
pub fn describe_effects_index(decls: &[EffectDecl]) -> String {
    let mut s = String::new();
    for d in decls {
        s.push_str(&describe_effect(d));
        s.push('\n');
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::standard_decls;

    #[test]
    fn first_sentence_takes_leading_sentence() {
        assert_eq!(
            first_sentence("Run shell commands. And capture output."),
            "Run shell commands."
        );
        // No sentence break → whole (trimmed) string.
        assert_eq!(first_sentence("  just one clause  "), "just one clause");
    }

    #[test]
    fn authored_effect_view_applies_one_curation_policy_to_every_name_class() {
        let actor = crate::actor_decl();
        assert!(authored_constructors(&actor).is_empty());
        assert!(authored_type_definitions(&actor).is_empty());
        assert!(authored_helpers(&actor).is_empty());

        let console = crate::console_decl();
        assert_eq!(authored_constructors(&console), console.constructors);
        assert_eq!(authored_type_definitions(&console), console.type_defs);
        assert_eq!(authored_helpers(&console), console.helpers);
    }

    /// Every declaration and its first named public helper appears in the index.
    #[test]
    fn derived_index_covers_every_decl_and_a_helper_verb() {
        let decls = standard_decls();
        let index = describe_effects_index(&decls);
        for d in &decls {
            assert!(
                index.contains(&format!("  {}:", d.type_name)),
                "index must name effect {}: {index}",
                d.type_name
            );
            let public_name = d
                .helpers
                .iter()
                .filter(|h| h.visibility == HelperVisibility::Public)
                .find_map(|h| h.name);
            if let Some(name) = public_name {
                assert!(
                    index.contains(name),
                    "index must list a helper verb ({name}) for effect {}: {index}",
                    d.type_name
                );
            }
        }
    }

    #[test]
    fn helper_metadata_drives_index_without_hiding_detailed_source() {
        let console = crate::console_decl();
        let entropy = crate::entropy_decl();
        for (decl, substrate) in [(console, "displayViewRaw"), (entropy, "entropySeed")] {
            let index = describe_effect(&decl);
            assert!(!index.contains(substrate));
            let detailed = authored_helpers(&decl);
            let helper = detailed
                .iter()
                .find(|helper| helper.name == Some(substrate))
                .unwrap();
            assert_eq!(helper.visibility, HelperVisibility::Substrate);
            assert!(helper.source.contains(substrate));
        }
        assert!(describe_effect(&console).contains("verbs: say"));
        const UNNAMED: HelperDecl = crate::effect_defs::helper_decl!({ unnamed raw ["apparentName :: Int", "apparentName = 1"] });
        let decl = EffectDecl {
            helpers: &[UNNAMED],
            ..console
        };
        assert!(!describe_effect(&decl).contains("apparentName"));
        assert_eq!(authored_helpers(&decl)[0].source, UNNAMED.source);
    }

    /// Opt-in Green declarations must also hide raw substrate helpers.
    #[test]
    fn derived_index_excludes_green_raw_helpers() {
        let green = crate::green_decl();
        let green_index = describe_effect(&green);
        for name in ["asyncSpawn", "asyncJoinAny", "asyncStatus", "asyncCancel"] {
            assert!(
                !green_index.contains(name),
                "Green index must not advertise raw substrate helper {name}: {green_index}"
            );
        }
    }
}
