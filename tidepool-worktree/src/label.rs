//! Caller-supplied label sanitization — one validated source, two named
//! policies.
//!
//! A caller-supplied label is decoration, never a path or an identity. Two
//! independent call sites used to sanitize the SAME kind of input under
//! silently different rules: `tidepool-agent`'s spawn saga folded a label
//! into the tail of an [`AgentRef`](crate::AgentRef), and
//! [`crate::create`]'s worktree creation folded one into the tail of a
//! managed branch name. The two are not interchangeable — a branch name may
//! contain `/` as a path separator; an `AgentRef` tail never does — so they
//! stay two named types, [`AgentLabel`] and [`BranchLabel`], rather than
//! collapsing into one. What collapses is the IMPLEMENTATION: both are the
//! same `sanitize` sweep, parameterized on whether `/` survives and on the
//! empty-result fallback, because that parameterization is enough to
//! reproduce each call site's ORIGINAL behavior byte-for-byte (pinned by
//! `label_compat` below) — this module changes nothing about what either
//! caller sees, only where the logic lives.

/// Sanitized into the tail of an `AgentRef` (`agent-<id>-<label>`). Only
/// `[A-Za-z0-9._-]` survives; everything else becomes `-`, runs of `-`
/// collapse, and the ends are trimmed of `-`/`.`. An all-punctuation label
/// falls back to `"worker"` rather than yielding a ref ending in a bare `-`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentLabel(String);

impl AgentLabel {
    pub fn new(raw: &str) -> Self {
        Self(sanitize(raw, false, "worker"))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Sanitized into the tail of a managed branch name
/// (`tidepool/worktree/<label>-<id>`). `[A-Za-z0-9._-/]` survives; everything
/// else becomes `-`, runs of `-`/`/` collapse together, and the ends are
/// trimmed of `-`/`/`/`.`. An all-punctuation label falls back to
/// `"worktree"` rather than a branch name ending in the bare prefix.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BranchLabel(String);

impl BranchLabel {
    pub fn new(raw: &str) -> Self {
        Self(sanitize(raw, true, "worktree"))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// The shared sweep. `allow_slash` is the one axis the two policies differ
/// on: whether `/` is a surviving character (and therefore also a member of
/// the "separator" class that collapses and trims) or an ordinary character
/// this sweep rejects to `-` like any other punctuation.
fn sanitize(label: &str, allow_slash: bool, fallback: &str) -> String {
    let mut out = String::with_capacity(label.len());
    let mut last_was_sep = false;
    for c in label.chars() {
        let c = if c.is_ascii_alphanumeric()
            || matches!(c, '-' | '_' | '.')
            || (allow_slash && c == '/')
        {
            c
        } else {
            '-'
        };
        let is_sep = c == '-' || (allow_slash && c == '/');
        if is_sep && last_was_sep {
            continue;
        }
        last_was_sep = is_sep;
        out.push(c);
    }
    let trimmed = if allow_slash {
        out.trim_matches(|c| c == '-' || c == '/' || c == '.')
    } else {
        out.trim_matches(|c| c == '-' || c == '.')
    };
    if trimmed.is_empty() {
        fallback.to_string()
    } else {
        trimmed.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Compatibility corpus, generated against the two ORIGINAL
    /// implementations (agent's `spawn.rs::sanitize_label` and worktree's
    /// pre-consolidation `create.rs::sanitize_label`) before either was
    /// touched. Each row is `(input, agent_expected, branch_expected)`. A
    /// change to either policy that breaks a row here is a durable-identifier
    /// compatibility break, not a refactor — see this crate's `CLAUDE.md` on
    /// worktree branch/path names being durable registry identifiers.
    const CORPUS: &[(&str, &str, &str)] = &[
        ("", "worker", "worktree"),
        ("!!!", "worker", "worktree"),
        ("___", "___", "___"),
        ("...", "worker", "worktree"),
        ("héllo", "h-llo", "h-llo"),
        ("日本語", "worker", "worktree"),
        ("café🎉", "caf", "caf"),
        ("a/b/c", "a-b-c", "a/b/c"),
        ("/leading/", "leading", "leading"),
        ("trailing/", "trailing", "trailing"),
        ("a.b.c", "a.b.c", "a.b.c"),
        ("a--b", "a-b", "a-b"),
        ("a//b", "a-b", "a/b"),
        ("a-/b", "a-b", "a-b"),
        ("a/-b", "a-b", "a/b"),
        ("-abc-", "abc", "abc"),
        ("/abc/", "abc", "abc"),
        (".abc.", "abc", "abc"),
        ("MyLabel_123", "MyLabel_123", "MyLabel_123"),
        ("dev-tree/root", "dev-tree-root", "dev-tree/root"),
        ("a_b_c", "a_b_c", "a_b_c"),
        (" a b ", "a-b", "a-b"),
        ("a\tb\nc", "a-b-c", "a-b-c"),
        ("-", "worker", "worktree"),
        ("/", "worker", "worktree"),
        (".", "worker", "worktree"),
        ("_", "_", "_"),
        ("root", "root", "root"),
        ("worker", "worker", "worker"),
        ("worktree", "worktree", "worktree"),
        ("----", "worker", "worktree"),
        ("////", "worker", "worktree"),
        ("a---/---b", "a-b", "a-b"),
        ("🔥", "worker", "worktree"),
        ("  ", "worker", "worktree"),
        ("a.", "a", "a"),
        (".a", "a", "a"),
        ("a-", "a", "a"),
        ("-a", "a", "a"),
        ("a/", "a", "a"),
        ("/a", "a", "a"),
    ];

    #[test]
    fn agent_label_matches_pinned_corpus() {
        for (input, expected, _) in CORPUS {
            assert_eq!(
                AgentLabel::new(input).as_str(),
                *expected,
                "AgentLabel::new({input:?})"
            );
        }
    }

    #[test]
    fn branch_label_matches_pinned_corpus() {
        for (input, _, expected) in CORPUS {
            assert_eq!(
                BranchLabel::new(input).as_str(),
                *expected,
                "BranchLabel::new({input:?})"
            );
        }
    }

    /// The divergence the review named: the same raw label produces
    /// DIFFERENT sanitized output under the two policies whenever it
    /// contains a `/` that survives worktree's policy but not agent's.
    #[test]
    fn policies_genuinely_diverge_on_slash_bearing_labels() {
        let raw = "dev-tree/root";
        assert_ne!(
            AgentLabel::new(raw).as_str(),
            BranchLabel::new(raw).as_str()
        );
    }
}
