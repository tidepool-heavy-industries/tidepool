//! Derive human-facing effect documentation from [`EffectDecl`] — the single
//! source consumed by three tool-description surfaces:
//!
//! 1. the `eval` tool description ([`crate::build_eval_tool_description`]),
//! 2. the repl `session_run` tool description (`tidepool-repl`'s
//!    `build_tool_description`), and
//! 3. the repl `:browse` meta-command (`tidepool-repl`'s `browse_effects`).
//!
//! HAZARD: do not hand-roll a fourth per-effect enumeration or sig parser in
//! a new surface — the three consumers above previously each maintained (or
//! reimplemented) their own and drifted from each other. The per-effect
//! enumerations DERIVE from the decls, and the sig/first-sentence parsers
//! live here once. Only the framing prose around the enumeration stays
//! hand-written per surface (it differs: eval points at
//! `tidepool://effect/{name}`, the repl points at `:browse`).

use crate::effect_decls::EffectDecl;
use crate::effect_defs::substrate_marker;

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

/// Is `helper`'s rendered text tagged substrate — an implementation detail
/// the extract layer needs (schema-building internals, the `*Sited`
/// call-site-id plumbing behind `runLLMTurn`/`fork`/`forkAll`, Green's raw
/// thread primitives, Subagent's raw wire helpers) rather than a verb a
/// model should reach for directly?
///
/// Structural, not a hand-maintained (effect, helper name) allowlist: a
/// helper is substrate iff `effect_defs.rs`'s `helper_text!` macro rendered
/// it via the `raw substrate [...]` form, which prepends
/// [`substrate_marker!`] as the helper's first line — the single choke
/// point every effect definition already routes through, so a newly added
/// substrate helper is tagged at its own definition site instead of
/// requiring a second edit here. This checks the rendered TEXT rather than
/// adding a visibility field to [`EffectDecl`] itself: that struct is a
/// plain `Copy` type constructed as a bare struct literal in several crates
/// this lane does not own (e.g. `tidepool-harness`'s
/// `delegate_branches_decl`) — a new field would force an edit in every one
/// of them (none of which use `..Default::default()`), which the macro-side
/// convention avoids entirely. Consulted only by the derived INDEX below;
/// the full per-effect resource (`tidepool://effect/{name}`,
/// `:browse <Effect>`) still lists every helper verbatim — a model that
/// asks for that depth gets it.
fn helper_is_substrate(helper: &str) -> bool {
    helper.trim_start().starts_with(substrate_marker!())
}

/// One derived entry for an effect in a tool-description index: the effect name,
/// its one-line (first-sentence) description, and the names of the PUBLIC helper
/// verbs it exposes — the recommended callable surface, derived from the decl so
/// a new public helper auto-appears (substrate helpers, see
/// [`is_substrate_helper`], are excluded here but still fully documented at
/// `tidepool://effect/{name}`). Rendered as a header line plus an indented verb
/// list:
///
/// ```text
///   Console: Print text output.
///       verbs: say, sayShow
/// ```
pub fn describe_effect(decl: &EffectDecl) -> String {
    let verbs: Vec<String> = decl
        .helpers
        .iter()
        .filter(|h| !helper_is_substrate(h))
        .filter_map(|h| helper_name(h))
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
    /// one of each effect's PUBLIC helper verbs (an effect whose only declared
    /// helpers are substrate, e.g. `Fork`'s `forkSited`/`forkAllSited`, is
    /// exempt — its model-facing verbs live in the Haskell stdlib instead, not
    /// in `EffectDecl::helpers`). Adding a new public helper to a `*_decl()`
    /// therefore auto-appears in both servers' tool descriptions with no
    /// hand-edit; a substrate helper never does.
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
            // At least one PUBLIC helper verb name for the effect must appear
            // (spot the first PUBLIC helper's name — proof the verb
            // enumeration derives and substrate is excluded).
            let public_name = d
                .helpers
                .iter()
                .filter(|h| !helper_is_substrate(h))
                .find_map(|h| helper_name(h));
            if let Some(name) = public_name {
                assert!(
                    index.contains(&name),
                    "index must list a helper verb ({name}) for effect {}: {index}",
                    d.type_name
                );
            }
        }
    }

    /// The recommended-surface index must not leak the extract-layer
    /// substrate: `ask`'s schema-building internals, `RunLLMTurn`'s
    /// call-site-id plumbing, and `Fork`'s — the report finding this closes
    /// (D's #3): the generated index advertised `isOpt`, `innerSchema`,
    /// `schemaToValue`, every `*Sited` variant, and only `forkSited`/
    /// `forkAllSited` for `Fork`, none of which a model should call directly.
    #[test]
    fn derived_index_excludes_substrate_helpers() {
        let decls = standard_decls();
        let index = describe_effects_index(&decls);
        for name in [
            "isOpt",
            "innerSchema",
            "schemaToValue",
            "forkSited",
            "forkAllSited",
            "runLLMTurnSited",
            "runLLMTurnForkSited",
            "runLLMTurnFanoutSited",
            "runLLMTurnBranchSited",
            "runLLMTurnBranchLabeledSited",
            "runLLMTurnBranchFanoutSited",
        ] {
            assert!(
                !index.contains(name),
                "index must not advertise substrate helper {name}: {index}"
            );
        }
        // The model-facing verbs stay listed.
        assert!(index.contains("ask"), "index must still list ask: {index}");
        assert!(
            index.contains("runLLMTurn"),
            "index must still list runLLMTurn: {index}"
        );
    }

    /// Green and Subagent are not in `standard_decls()` (neither is part of
    /// the base stack), so the tests above never exercise them — but they
    /// are exactly the dup-survey's items 2/4: raw substrate leaking beside
    /// a safer authored wrapper. Assert the marker mechanism reaches them
    /// too, without a second hand-maintained allowlist entry.
    #[test]
    fn derived_index_excludes_green_and_subagent_raw_helpers() {
        let green = crate::green_decl();
        let green_index = describe_effect(&green);
        for name in [
            "asyncSpawn",
            "asyncJoinAny",
            "asyncStatus",
            "asyncResult",
            "asyncCancel",
        ] {
            assert!(
                !green_index.contains(name),
                "Green index must not advertise raw substrate helper {name}: {green_index}"
            );
        }

        let subagent = crate::subagent_decl();
        let subagent_index = describe_effect(&subagent);
        for name in [
            "spawnAgentRaw",
            "agentBeginRaw",
            "agentResumeRaw",
            "agentSpawnAsyncRaw",
            "agentAwaitRaw",
            "agentCancelRaw",
        ] {
            assert!(
                !subagent_index.contains(name),
                "Subagent index must not advertise raw substrate helper {name}: {subagent_index}"
            );
        }
        // The genuinely public pure helpers stay listed.
        for name in ["spawnSpec", "spawnSpecIn", "renderBackendFailure"] {
            assert!(
                subagent_index.contains(name),
                "Subagent index must still list public helper {name}: {subagent_index}"
            );
        }
    }
}
