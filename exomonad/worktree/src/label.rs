//! Sanitization for caller-supplied labels. Labels are decoration, never paths
//! or identities. Agent labels reject `/`; managed branch labels preserve it.

/// Sanitized into the tail of an `AgentRef` (`agent-<id>-<label>`). Only
/// `[A-Za-z0-9._-]` survives; everything else becomes `-`, runs of `-`
/// collapse, and the ends are trimmed of `-`/`.`. An all-punctuation label
/// falls back to `"worker"` rather than yielding a ref ending in a bare `-`.
pub fn sanitize_agent_label(raw: &str) -> String {
    sanitize(raw, false, "worker")
}

/// Sanitized into the tail of a managed branch name
/// (`tidepool/worktree/<label>-<id>`). `[A-Za-z0-9._-/]` survives; everything
/// else becomes `-`, runs of `-`/`/` collapse together, and the ends are
/// trimmed of `-`/`/`/`.`. An all-punctuation label falls back to
/// `"worktree"` rather than a branch name ending in the bare prefix.
pub fn sanitize_branch_label(raw: &str) -> String {
    sanitize(raw, true, "worktree")
}

/// Shared implementation for the two slash policies.
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

    /// `(input, agent_expected, branch_expected)`. These outputs participate
    /// in durable identifiers and are compatibility-sensitive.
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
                sanitize_agent_label(input),
                *expected,
                "sanitize_agent_label({input:?})"
            );
        }
    }

    #[test]
    fn branch_label_matches_pinned_corpus() {
        for (input, _, expected) in CORPUS {
            assert_eq!(
                sanitize_branch_label(input),
                *expected,
                "sanitize_branch_label({input:?})"
            );
        }
    }

    #[test]
    fn policies_genuinely_diverge_on_slash_bearing_labels() {
        let raw = "dev-tree/root";
        assert_ne!(sanitize_agent_label(raw), sanitize_branch_label(raw));
    }
}
