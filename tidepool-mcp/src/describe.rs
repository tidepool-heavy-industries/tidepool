//! Derive human-facing effect documentation from [`EffectDecl`] — the single
//! source consumed by three tool-description surfaces:
//!
//! 1. the `eval` tool description ([`crate::build_eval_tool_description`]),
//! 2. the repl `session_run` tool description (`tidepool-repl`'s
//!    `build_tool_description`), and
//! 3. the repl `:browse` meta-command (`tidepool-repl`'s `browse_effects`).
//!
//! Before this module each surface hand-maintained (or re-implemented) its own
//! per-effect enumeration and sig/first-sentence parsing — the historical
//! tripled-prose drift class. Now the per-effect enumerations DERIVE from the
//! decls, and the sig/first-sentence parsers live here once (one parser, three
//! consumers). Only the framing prose around the enumeration stays hand-written
//! per surface (it differs: eval points at `tidepool://effect/{name}`, the repl
//! points at `:browse`).

use crate::effect_decls::EffectDecl;

/// The one-line summary of an effect's (often multi-sentence) `description`: the
/// leading sentence, or the whole (trimmed) string when there is no sentence
/// break. This is what `:browse` (bare) and the derived index show — the full
/// multi-sentence `description` is reserved for `:browse <Effect>` and the
/// `tidepool://effect/{name}` resource.
pub fn first_sentence(desc: &str) -> &str {
    let d = desc.trim();
    match d.find(". ") {
        // Keep the period; drop the trailing space + rest.
        Some(i) => d[..=i].trim_end(),
        None => d,
    }
}

/// The signature line of an effect helper. Helper strings are
/// `"[-- comment…\n]sig-line\ndefinition"`; the signature is the first
/// non-comment line that carries a `::` (e.g. `run :: Text -> M Proc`). Falls
/// back to the first non-comment line when no `::` is present.
pub fn helper_sig(helper: &str) -> Option<String> {
    let mut fallback = None;
    for line in helper.lines() {
        let t = line.trim();
        if t.is_empty() || t.starts_with("--") {
            continue;
        }
        if t.contains("::") {
            return Some(t.to_string());
        }
        fallback.get_or_insert_with(|| t.to_string());
    }
    fallback
}

/// The verb NAME of a helper string — the identifier before `::`, e.g.
/// `run :: Text -> M Proc` → `run`. `None` when the helper carries no
/// name-bearing signature (a bare operator section, say).
pub fn helper_name(helper: &str) -> Option<String> {
    let sig = helper_sig(helper)?;
    let head = sig.split_once("::")?.0.trim();
    if head.is_empty() {
        None
    } else {
        Some(head.to_string())
    }
}

/// One derived entry for an effect in a tool-description index: the effect name,
/// its one-line (first-sentence) description, and the names of the helper verbs
/// it exposes — the callable surface, derived from the decl so a new helper
/// auto-appears. Rendered as a header line plus an indented verb list:
///
/// ```text
///   Console: Print text output.
///       verbs: say, sayShow
/// ```
pub fn describe_effect(decl: &EffectDecl) -> String {
    let verbs: Vec<String> = decl.helpers.iter().filter_map(|h| helper_name(h)).collect();
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
/// mechanical per-effect body that used to be hand-written in each server.
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
    fn helper_sig_skips_comments_and_finds_signature() {
        assert_eq!(
            helper_sig("-- | doc line\nrun :: Text -> M Proc\nrun cmd = undefined").as_deref(),
            Some("run :: Text -> M Proc")
        );
        assert_eq!(
            helper_sig("kvSet :: Text -> Value -> M ()\nkvSet k v = undefined").as_deref(),
            Some("kvSet :: Text -> Value -> M ()")
        );
    }

    #[test]
    fn helper_name_is_the_identifier_before_the_arrow() {
        assert_eq!(
            helper_name("-- | doc\nrun :: Text -> M Proc\nrun cmd = undefined").as_deref(),
            Some("run")
        );
        assert_eq!(
            helper_name("grepGlob :: Text -> FilePath -> M [Hit]\ngrepGlob = undefined").as_deref(),
            Some("grepGlob")
        );
    }

    /// Snapshot-guard: the derived index names every effect and lists at least
    /// one of each effect's helper verbs. Adding a new `*_decl()` (with any
    /// helper) therefore auto-appears in both servers' tool descriptions with no
    /// hand-edit — the mechanism that kills the tripled-prose drift class.
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
            // At least one helper verb name for the effect must appear (spot the
            // FIRST helper's name — proof the verb enumeration derives).
            if let Some(name) = d.helpers.iter().find_map(|h| helper_name(h)) {
                assert!(
                    index.contains(&name),
                    "index must list a helper verb ({name}) for effect {}: {index}",
                    d.type_name
                );
            }
        }
    }
}
