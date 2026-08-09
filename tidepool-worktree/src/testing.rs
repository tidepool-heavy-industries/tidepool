//! Real temporary repositories and the scripted writer that drives them.
//!
//! Every git-behaviour test in this wave runs against a REAL repository — a
//! `TempDir`, `git init`, real commits — driven by [`ScriptedWriter`], plain git
//! commands standing in for a coding agent. There is no mock of git anywhere,
//! deliberately: a mock proves that the mock agrees with the author's model of
//! git, which is precisely the thing in doubt when the code under test exists to
//! observe git honestly.
//!
//! ## Why the writer may rebase when the runtime may not
//!
//! [`ScriptedWriter`] has `rebase_onto`, `amend`, and `reset_hard`. Those are
//! not a crack in PRD 19's boundary — the boundary says the RUNTIME exposes no
//! git workflow verbs, because that work belongs to coding agents. The scripted
//! writer *is* the stand-in coding agent. It exists to produce the repository
//! states the monitor must classify honestly (`Amended`, `Rewritten`,
//! `Rewound`, `Switched`) without needing an LLM in the loop to produce them.
//!
//! Nothing in this module may be re-exported into a runtime surface.

use std::path::{Path, PathBuf};

use crate::error::WorktreeError;
use crate::git::GitCli;
use crate::id::{BranchName, GitOid};

/// A real git repository in a temporary directory, deleted when dropped.
#[derive(Debug)]
pub struct TestRepo {
    dir: tempfile::TempDir,
    git: GitCli,
}

impl TestRepo {
    /// `git init` a repository with deterministic identity and config.
    ///
    /// The identity is pinned rather than inherited so a commit's author does
    /// not depend on whose machine ran the test, and `init.defaultBranch` is
    /// pinned because a test that asserts on `main` and a developer whose git
    /// defaults to `master` is a failure with nothing to learn from it.
    pub fn init() -> Result<Self, WorktreeError> {
        let dir = tempfile::TempDir::new().expect("create temp dir");
        let git = GitCli::new();
        let path = dir.path().to_path_buf();

        git.try_run(&path, &["init", "--initial-branch=main", "-q"])?;
        git.try_run(&path, &["config", "user.name", "Scripted Writer"])?;
        git.try_run(&path, &["config", "user.email", "writer@example.invalid"])?;
        // No signing, no hooks, no gc surprises mid-test.
        git.try_run(&path, &["config", "commit.gpgsign", "false"])?;
        git.try_run(&path, &["config", "gc.auto", "0"])?;

        Ok(Self { dir, git })
    }

    pub fn path(&self) -> &Path {
        self.dir.path()
    }

    pub fn git(&self) -> &GitCli {
        &self.git
    }

    /// A scripted writer against this repository's own working tree.
    pub fn writer(&self) -> ScriptedWriter<'_> {
        ScriptedWriter {
            git: &self.git,
            cwd: self.dir.path().to_path_buf(),
        }
    }

    /// A scripted writer against some other working tree backed by this
    /// repository — a managed worktree, for instance, which is how the monitor
    /// gets a writer to observe.
    pub fn writer_at(&self, cwd: impl Into<PathBuf>) -> ScriptedWriter<'_> {
        ScriptedWriter {
            git: &self.git,
            cwd: cwd.into(),
        }
    }

    /// Keep the directory on disk past drop, returning its path. For debugging
    /// a failing test; never leave a call to this in a committed test.
    pub fn leak(self) -> PathBuf {
        self.dir.keep()
    }
}

/// Plain git commands standing in for a coding agent. See the module docs for
/// why this may do things the runtime surface deliberately cannot.
#[derive(Debug)]
pub struct ScriptedWriter<'a> {
    git: &'a GitCli,
    cwd: PathBuf,
}

impl ScriptedWriter<'_> {
    pub fn cwd(&self) -> &Path {
        &self.cwd
    }

    /// Write a file (creating parents) without staging or committing it.
    pub fn write_file(&self, rel: &str, contents: &str) -> Result<(), WorktreeError> {
        let path = self.cwd.join(rel);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).expect("create parent dir");
        }
        std::fs::write(&path, contents).expect("write file");
        Ok(())
    }

    pub fn stage(&self, rel: &str) -> Result<(), WorktreeError> {
        self.git.try_run(&self.cwd, &["add", "--", rel])?;
        Ok(())
    }

    /// Write, stage, and commit one file. Returns the new commit.
    pub fn commit_file(
        &self,
        rel: &str,
        contents: &str,
        message: &str,
    ) -> Result<GitOid, WorktreeError> {
        self.write_file(rel, contents)?;
        self.stage(rel)?;
        self.git
            .try_run(&self.cwd, &["commit", "-q", "-m", message])?;
        self.head()
    }

    /// Commit with no file change (`--allow-empty`). Useful for producing head
    /// movement without touching the tree.
    pub fn commit_empty(&self, message: &str) -> Result<GitOid, WorktreeError> {
        self.git
            .try_run(&self.cwd, &["commit", "-q", "--allow-empty", "-m", message])?;
        self.head()
    }

    /// Replace the tip with a same-parent commit — the `Amended` case.
    pub fn amend(&self, message: &str) -> Result<GitOid, WorktreeError> {
        self.git
            .try_run(&self.cwd, &["commit", "-q", "--amend", "-m", message])?;
        self.head()
    }

    /// Move the tip backwards — the `Rewound` case.
    pub fn reset_hard(&self, target: &str) -> Result<GitOid, WorktreeError> {
        self.git
            .try_run(&self.cwd, &["reset", "--hard", "-q", target])?;
        self.head()
    }

    /// Create and check out a branch — the `Switched` case.
    pub fn checkout_new_branch(&self, name: &str) -> Result<(), WorktreeError> {
        self.git
            .try_run(&self.cwd, &["checkout", "-q", "-b", name])?;
        Ok(())
    }

    pub fn checkout(&self, name: &str) -> Result<(), WorktreeError> {
        self.git.try_run(&self.cwd, &["checkout", "-q", name])?;
        Ok(())
    }

    /// Rewrite history onto a new base — the `Rewritten` case. This is the
    /// stand-in for a coding agent honouring a `RebaseWhenSafe` poke.
    pub fn rebase_onto(&self, upstream: &str) -> Result<GitOid, WorktreeError> {
        self.git.try_run(&self.cwd, &["rebase", "-q", upstream])?;
        self.head()
    }

    pub fn head(&self) -> Result<GitOid, WorktreeError> {
        let out = self.git.try_run(&self.cwd, &["rev-parse", "HEAD"])?;
        Ok(GitOid::from_raw(out.trimmed()))
    }

    /// Current branch, or `None` on a detached HEAD.
    pub fn current_branch(&self) -> Result<Option<BranchName>, WorktreeError> {
        match self
            .git
            .run(&self.cwd, &["symbolic-ref", "--short", "HEAD"])
        {
            Ok(out) => Ok(Some(BranchName::from_raw(out.trimmed()))),
            Err(_) => Ok(None),
        }
    }
}

/// A byte-level fingerprint of a working tree, for the untouched-source proof.
///
/// Compares what a diff of git state cannot: file bytes, mode bits, and the
/// presence of untracked and ignored files. A snapshot that left `git status`
/// looking identical while rewriting a working file would pass a status
/// comparison and fail this one, which is the point.
pub mod fingerprint {
    use std::collections::BTreeMap;
    use std::path::Path;

    /// Every file under `root` (excluding `.git`), mapped to
    /// `(bytes, unix_mode)`. Sorted, so two fingerprints compare directly and
    /// a failure names the differing path.
    pub fn working_tree(root: &Path) -> BTreeMap<String, (Vec<u8>, u32)> {
        let mut out = BTreeMap::new();
        collect(root, root, &mut out);
        out
    }

    fn collect(root: &Path, dir: &Path, out: &mut BTreeMap<String, (Vec<u8>, u32)>) {
        let entries = match std::fs::read_dir(dir) {
            Ok(e) => e,
            Err(_) => return,
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.file_name().is_some_and(|n| n == ".git") {
                continue;
            }
            if path.is_dir() {
                collect(root, &path, out);
            } else if let Ok(bytes) = std::fs::read(&path) {
                let mode = mode_of(&path);
                let rel = path
                    .strip_prefix(root)
                    .unwrap_or(&path)
                    .to_string_lossy()
                    .into_owned();
                out.insert(rel, (bytes, mode));
            }
        }
    }

    #[cfg(unix)]
    fn mode_of(path: &Path) -> u32 {
        use std::os::unix::fs::PermissionsExt;
        std::fs::metadata(path)
            .map(|m| m.permissions().mode())
            .unwrap_or(0)
    }

    #[cfg(not(unix))]
    fn mode_of(_path: &Path) -> u32 {
        0
    }
}
