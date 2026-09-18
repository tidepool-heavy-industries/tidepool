//! A "see it used here" pointer per callable, derived from the shipped Shoal
//! example workspace — never a hand-maintained second index.
//!
//! `build.rs` globs `examples/shoal-workspace/.shoal/checks/*.hs` (the plan's
//! `examples/shoal-workspace/.shoal/examples/*.hs` does not exist in this
//! tree; `.shoal/checks/*.hs` — worked, compiled Haskell scripts — is the
//! closest shipped analog) and every shipped skill's `SKILL.md`
//! (`examples/shoal-workspace/.shoal/skills/*/SKILL.md`), and embeds their
//! text as `SHOAL_USAGE_SOURCES` (generated into `shoal_usage_sources.rs`
//! under `OUT_DIR`). Adding, removing, or editing a file under either
//! directory changes the index the next time this crate builds; nothing here
//! is a hand-maintained list of names.
//!
//! A qualified-name occurrence (`R.call`, `Cmd.readOutput` — the same shape
//! [`crate::lookup_tool::qualifier_and_identifier`] recognizes in a query) is
//! scanned out of each source and indexed by its bare identifier, first
//! occurrence wins (sources are scanned in a fixed, sorted order, so the
//! index is deterministic across builds).

use std::collections::HashMap;
use std::sync::OnceLock;

include!(concat!(env!("OUT_DIR"), "/shoal_usage_sources.rs"));

/// Maximal runs of identifier-shaped characters (including `.`), so
/// `R.call` scans out of `caller <- R.start …; R.send (callerAsk …` whole,
/// without a regex dependency.
fn raw_tokens(source: &str) -> impl Iterator<Item = &str> {
    let is_word = |c: char| c.is_ascii_alphanumeric() || c == '_' || c == '\'' || c == '.';
    let mut start = None;
    let mut tokens = Vec::new();
    for (index, ch) in source.char_indices() {
        if is_word(ch) {
            start.get_or_insert(index);
        } else if let Some(begin) = start.take() {
            tokens.push(&source[begin..index]);
        }
    }
    if let Some(begin) = start {
        tokens.push(&source[begin..]);
    }
    tokens.into_iter()
}

/// Every qualified-name occurrence in `source`, as `(qualifier, identifier)`
/// pairs — exactly the shape a qualified value or field query has.
fn qualified_tokens(source: &str) -> impl Iterator<Item = (&str, &str)> {
    raw_tokens(source)
        .map(|token| token.trim_matches('.'))
        .filter_map(crate::lookup_tool::qualifier_and_identifier)
}

/// Build the bare-identifier index from an explicit `(locator, source)`
/// list — separated from [`build_index`] so it is testable against synthetic
/// fixtures, independent of the shipped workspace's exact current contents.
fn index_from_sources(sources: &[(&'static str, &'static str)]) -> HashMap<&'static str, &'static str> {
    let mut index = HashMap::new();
    for (locator, source) in sources {
        for (_qualifier, identifier) in qualified_tokens(source) {
            index.entry(identifier).or_insert(*locator);
        }
    }
    index
}

fn build_index() -> HashMap<&'static str, &'static str> {
    index_from_sources(SHOAL_USAGE_SOURCES)
}

static INDEX: OnceLock<HashMap<&'static str, &'static str>> = OnceLock::new();

/// A locator for one shipped worked use of the bare callable `name`, or
/// `None` when no shipped check or skill uses it.
pub(crate) fn pointer_for(name: &str) -> Option<String> {
    let locator = INDEX.get_or_init(build_index).get(name).copied()?;
    present(locator, std::path::Path::new("."))
}

/// A locator as a reader in `workspace` can follow it, or `None` when they
/// cannot. The index is built from the shipped example workspace, and the
/// reader may be anywhere: a skill is loaded by name wherever it is shipped, so
/// it is named; a check file is a path, so it is offered only where that path
/// exists.
fn present(locator: &str, workspace: &std::path::Path) -> Option<String> {
    if let Some(skill) = locator
        .strip_prefix(".shoal/skills/")
        .and_then(|rest| rest.strip_suffix("/SKILL.md"))
    {
        return Some(format!("skill {skill}"));
    }
    workspace.join(locator).is_file().then(|| locator.to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pointer_present_for_a_name_used_in_an_example_and_absent_otherwise() {
        let sources: &[(&str, &str)] = &[(
            ".shoal/checks/handler-call.hs",
            "caller <- R.start callerDefinition\n\
             R.send (callerAsk (R.client caller)) (box, \"exit 3\")\n",
        )];
        let index = index_from_sources(sources);
        assert_eq!(
            index.get("start").copied(),
            Some(".shoal/checks/handler-call.hs")
        );
        assert_eq!(
            index.get("send").copied(),
            Some(".shoal/checks/handler-call.hs")
        );
        assert_eq!(index.get("client").copied(), Some(".shoal/checks/handler-call.hs"));
        // Never scanned: no qualified occurrence of it anywhere in the source.
        assert_eq!(index.get("exitCode"), None);
        // A capitalized-final token (a type/constructor use, e.g. `R.Reply`)
        // is not a callable occurrence and must not seed the index.
        assert_eq!(index.get("Reply"), None);
    }

    /// A pointer is only worth a reader's context if they can follow it from
    /// where they are, which is rarely the workspace the index was built from.
    #[test]
    fn a_pointer_is_offered_only_where_it_can_be_followed() {
        let elsewhere = tempfile::tempdir().unwrap();
        assert_eq!(
            present(".shoal/skills/shoal-command/SKILL.md", elsewhere.path()).as_deref(),
            Some("skill shoal-command")
        );
        assert_eq!(present(".shoal/checks/handler-call.hs", elsewhere.path()), None);
        std::fs::create_dir_all(elsewhere.path().join(".shoal/checks")).unwrap();
        std::fs::write(elsewhere.path().join(".shoal/checks/handler-call.hs"), "").unwrap();
        assert_eq!(
            present(".shoal/checks/handler-call.hs", elsewhere.path()).as_deref(),
            Some(".shoal/checks/handler-call.hs")
        );
    }

    #[test]
    fn first_shipped_occurrence_wins_when_two_sources_use_the_same_bare_name() {
        let sources: &[(&str, &str)] = &[
            (".shoal/checks/a.hs", "R.call boxRun program"),
            (".shoal/skills/shoal-command", "Cmd.call something"),
        ];
        let index = index_from_sources(sources);
        assert_eq!(index.get("call").copied(), Some(".shoal/checks/a.hs"));
    }

    #[test]
    fn qualified_tokens_ignores_prose_and_trailing_punctuation() {
        let tokens: Vec<(&str, &str)> =
            qualified_tokens("See R.call. Also (Cmd.readOutput) and R.Reply.").collect();
        assert!(tokens.contains(&("R", "call")));
        assert!(tokens.contains(&("Cmd", "readOutput")));
        assert!(!tokens.iter().any(|(_, identifier)| *identifier == "Reply"));
    }
}

#[cfg(test)]
mod sanity {
    #[test]
    fn the_real_shipped_index_resolves_at_least_one_known_callable() {
        // A cheap live sanity check on the real generated index (not the
        // synthetic fixtures above): `R.call` is used in
        // `.shoal/checks/handler-call.hs` today.
        // Read from the index rather than through `pointer_for`, which only
        // offers a check file where the reader could open it.
        assert!(super::INDEX
            .get_or_init(super::build_index)
            .contains_key("call"));
    }
}
