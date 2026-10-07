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
/// (`exomonad/worktree/<label>-<id>`). `[A-Za-z0-9._-/]` survives; everything
/// else becomes `-`, runs of `-`/`/` collapse together, and the ends are
/// trimmed of `-`/`/`/`.`. An all-punctuation label falls back to
/// `"worktree"` rather than a branch name ending in the bare prefix.
/// Components lose leading dots, interior dot runs become `-`, and
/// intermediate `.lock` suffixes become `-lock` so the generated Git ref is
/// valid. A final `.lock` remains valid before the generated `-<id>` suffix.
/// The normalized label is capped at 200 ASCII bytes, leaving room for the
/// owned namespace and a minted worktree id in a filesystem-backed ref.
pub fn sanitize_branch_label(raw: &str) -> String {
    let label = sanitize(raw, true, "worktree");
    let mut components = label.split('/').peekable();
    let mut out = String::with_capacity(label.len());
    while let Some(component) = components.next() {
        if !out.is_empty() {
            out.push('/');
        }
        let component = component.trim_start_matches('.');
        if component.is_empty() {
            out.push_str("worktree");
            continue;
        }

        let mut normalized = String::with_capacity(component.len());
        let mut chars = component.chars().peekable();
        while let Some(mut c) = chars.next() {
            if c == '.' && chars.peek() == Some(&'.') {
                while chars.peek() == Some(&'.') {
                    chars.next();
                }
                c = '-';
            }
            if c != '-' || !normalized.ends_with('-') {
                normalized.push(c);
            }
        }
        if components.peek().is_some() {
            if let Some(stem) = normalized.strip_suffix(".lock") {
                out.push_str(stem);
                out.push_str("-lock");
                continue;
            }
        }
        out.push_str(&normalized);
    }
    const MAX_LABEL_BYTES: usize = 200;
    if out.len() > MAX_LABEL_BYTES {
        // The sanitizer emits ASCII only, so this byte boundary is a char boundary.
        out.truncate(MAX_LABEL_BYTES);
        while out.ends_with('-') || out.ends_with('/') || out.ends_with('.') {
            out.pop();
        }
    }
    if out.is_empty() {
        "worktree".to_string()
    } else {
        out
    }
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
    use std::cell::RefCell;

    use proptest::prelude::*;
    use proptest::test_runner::{Config, FileFailurePersistence, TestRunner};
    use tidepool_atomic_write::DirectoryAnchor;

    use crate::create::EXOMONAD_BRANCH_PREFIX;
    use crate::{GitCli, WorktreeRegistry};

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
    fn branch_labels_repair_invalid_git_ref_components() {
        for (input, agent_expected, branch_expected) in [
            ("a..b", "a..b", "a-b"),
            ("a...b", "a...b", "a-b"),
            ("a-..-b", "a-..-b", "a-b"),
            ("a/.b", "a-.b", "a/b"),
            ("a/../b", "a-..-b", "a/worktree/b"),
            ("a/.../b", "a-...-b", "a/worktree/b"),
            ("a.lock/b", "a.lock-b", "a-lock/b"),
            ("a/.lock/b", "a-.lock-b", "a/lock/b"),
            ("a.lock.lock/b", "a.lock.lock-b", "a.lock-lock/b"),
            ("a.lock", "a.lock", "a.lock"),
            ("a/b.lock", "a-b.lock", "a/b.lock"),
            ("a./b", "a.-b", "a./b"),
        ] {
            assert_eq!(sanitize_agent_label(input), agent_expected, "{input:?}");
            assert_eq!(sanitize_branch_label(input), branch_expected, "{input:?}");
        }
    }

    #[test]
    fn branch_labels_are_bounded_without_trailing_ref_separators() {
        for input in [
            "x".repeat(300),
            "x/".repeat(120),
            format!("{}.", "x".repeat(300)),
        ] {
            let label = sanitize_branch_label(&input);
            assert!(label.len() <= 200, "label length {}", label.len());
            assert!(
                !label.ends_with('-') && !label.ends_with('/') && !label.ends_with('.'),
                "{label:?}"
            );
            assert!(label.split('/').all(|component| !component.is_empty()));
        }
    }

    #[test]
    fn policies_genuinely_diverge_on_slash_bearing_labels() {
        let raw = "dev-tree/root";
        assert_ne!(sanitize_agent_label(raw), sanitize_branch_label(raw));
    }

    fn branch_label_inputs() -> impl Strategy<Value = String> {
        let token = prop_oneof![
            Just("word".to_owned()),
            Just("/".to_owned()),
            Just(".".to_owned()),
            Just("..".to_owned()),
            Just(".lock".to_owned()),
            Just("---".to_owned()),
            Just("///".to_owned()),
            Just(" ".to_owned()),
            Just("🎉".to_owned()),
            Just("é".to_owned()),
            Just("!@#$%^&*()".to_owned()),
        ];
        prop_oneof![
            3 => collection::vec(any::<char>(), 0..260)
                .prop_map(|chars| chars.into_iter().collect()),
            2 => collection::vec(token, 0..80).prop_map(|parts| parts.concat()),
        ]
    }

    #[derive(Default, Debug)]
    struct GeneratedCoverage {
        callbacks: usize,
        slash_inputs: usize,
        slash_outputs: usize,
        unicode_inputs: usize,
        capped_outputs: usize,
        lock_components: usize,
        long_inputs: usize,
    }

    impl GeneratedCoverage {
        fn observe(&mut self, raw: &str, label: &str) {
            self.callbacks += 1;
            self.slash_inputs += usize::from(raw.contains('/'));
            self.slash_outputs += usize::from(label.contains('/'));
            self.unicode_inputs += usize::from(!raw.is_ascii());
            self.capped_outputs += usize::from(label.len() == 200);
            self.lock_components += usize::from(raw.contains(".lock"));
            self.long_inputs += usize::from(raw.len() > 200);
        }
    }

    fn property_config(name: &'static str) -> Config {
        let mut config = Config {
            test_name: Some(name),
            ..Config::default()
        };
        if std::env::var_os("PROPTEST_CASES").is_none() {
            config.cases = 64;
        }
        if std::env::var_os("PROPTEST_MAX_SHRINK_ITERS").is_none() {
            config.max_shrink_iters = 1_024;
        }
        if let Some(path) = option_env!("TIDEPOOL_PROPTEST_REGRESSIONS") {
            config.failure_persistence = Some(Box::new(FileFailurePersistence::Direct(path)));
            eprintln!("worktree label seed persistence: {path}");
        }
        config
    }

    #[test]
    fn generated_managed_branch_refs_are_valid_git_refs_and_keep_minted_identity() {
        let git = GitCli::new();
        let repository = tempfile::tempdir().expect("temporary repository");
        git.init_repository(repository.path(), &["--quiet"])
            .expect("initialize Git oracle repository");

        let storage = tempfile::tempdir().expect("temporary registry storage");
        let anchor = DirectoryAnchor::open_existing(storage.path()).expect("storage anchor");
        let registry = WorktreeRegistry::open(&anchor, "registry").expect("open registry");
        let id = registry.mint_id().expect("mint worktree identity");

        let coverage = RefCell::new(GeneratedCoverage::default());
        let config = property_config(concat!(
            module_path!(),
            "::generated_managed_branch_refs_are_valid_git_refs_and_keep_minted_identity"
        ));
        let configured_fresh_cases = config.cases;
        let result = TestRunner::new(config).run(&branch_label_inputs(), |raw| {
            let label = sanitize_branch_label(&raw);
            coverage.borrow_mut().observe(&raw, &label);

            prop_assert!(label.is_ascii(), "sanitizer output must be ASCII");
            prop_assert!(!label.is_empty(), "fallback must keep the label nonempty");
            prop_assert!(label.len() <= 200, "label exceeded its byte cap: {}", label.len());

            let branch = format!("{EXOMONAD_BRANCH_PREFIX}/{label}-{}", id.as_str());
            prop_assert!(
                branch.ends_with(&format!("-{}", id.as_str())),
                "managed branch must retain its minted worktree identity"
            );
            let full_ref = format!("refs/heads/{branch}");
            let checked = git.read(repository.path(), &["check-ref-format", &full_ref]);
            prop_assert!(
                checked.is_ok(),
                "Git rejected generated managed branch {branch:?}: {checked:?}"
            );
            Ok(())
        });
        eprintln!(
            "worktree label configured fresh cases: {configured_fresh_cases}; generated callback coverage: {:#?}",
            coverage.borrow()
        );
        result.unwrap();
    }
}
