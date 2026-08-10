//! The one place this crate shells out to git.
//!
//! Every lane funnels through [`GitCli`] so that the environment scrubbing, the
//! failure receipt shape, and the "never inherit the caller's index/config"
//! discipline exist once. A lane that spawns `Command::new("git")` itself has
//! bypassed all three.
//!
//! Why the git CLI rather than a libgit2 binding: the thing being observed is a
//! repository that real coding agents are mutating with the real `git` binary,
//! including operations (rebase, cherry-pick, worktree) whose on-disk state
//! libgit2 models incompletely. Reconciled inspection through the same tool the
//! writers use is the honest observer.

use std::collections::BTreeMap;
use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::process::Command;

use crate::error::{GitFailureReceipt, InProgressKind, WorktreeError};

/// A successful git invocation.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GitOutput {
    pub stdout: String,
    pub stderr: String,
}

impl GitOutput {
    /// stdout with the single trailing newline git appends removed. Use for
    /// single-value queries (`rev-parse`, `symbolic-ref`).
    pub fn trimmed(&self) -> &str {
        self.stdout.trim_end_matches('\n')
    }

    /// stdout split on newlines with the trailing empty element dropped.
    pub fn lines(&self) -> Vec<&str> {
        let t = self.trimmed();
        if t.is_empty() {
            Vec::new()
        } else {
            t.split('\n').collect()
        }
    }

    /// stdout split on NUL, for `-z` porcelain. Empty trailing field dropped.
    pub fn nul_fields(&self) -> Vec<&str> {
        self.stdout.split('\0').filter(|s| !s.is_empty()).collect()
    }
}

/// Runs git commands with a scrubbed environment.
#[derive(Clone, Debug, Default)]
pub struct GitCli {
    /// Extra environment applied to every invocation (the snapshot lane sets
    /// `GIT_INDEX_FILE` here; the monitor sets nothing).
    env: BTreeMap<String, String>,
}

impl GitCli {
    pub fn new() -> Self {
        Self::default()
    }

    /// Return a clone with one environment variable overridden for its
    /// invocations. Used by the dirty-snapshot lane to point at a TEMPORARY
    /// index — the whole no-touch proof rests on that variable never leaking
    /// onto an invocation that was supposed to see the real index, which is why
    /// this returns a new value instead of mutating a shared one.
    #[must_use]
    pub fn with_env(&self, key: impl Into<String>, value: impl Into<String>) -> Self {
        let mut next = self.clone();
        next.env.insert(key.into(), value.into());
        next
    }

    /// Run git in `cwd`. `Err` only for a nonzero exit or a spawn failure; a
    /// command that succeeds with output on stderr is still `Ok`.
    pub fn run<S: AsRef<OsStr>>(
        &self,
        cwd: &Path,
        args: &[S],
    ) -> Result<GitOutput, GitFailureReceipt> {
        let arg_strings: Vec<String> = args
            .iter()
            .map(|a| a.as_ref().to_string_lossy().into_owned())
            .collect();

        let mut cmd = Command::new("git");
        cmd.current_dir(cwd);
        cmd.args(args);

        // Scrub the ambient git environment. Inheriting GIT_DIR/GIT_INDEX_FILE/
        // GIT_WORK_TREE from whatever spawned Tidepool would silently retarget
        // every invocation here at another repository — the exact failure the
        // "never dirty the source" invariant cannot detect after the fact.
        for leaked in [
            "GIT_DIR",
            "GIT_INDEX_FILE",
            "GIT_WORK_TREE",
            "GIT_OBJECT_DIRECTORY",
            "GIT_COMMON_DIR",
            "GIT_ALTERNATE_OBJECT_DIRECTORIES",
        ] {
            cmd.env_remove(leaked);
        }
        // Deterministic, non-interactive, locale-stable output.
        cmd.env("GIT_TERMINAL_PROMPT", "0");
        cmd.env("GIT_OPTIONAL_LOCKS", "0");
        cmd.env("LC_ALL", "C");
        for (k, v) in &self.env {
            cmd.env(k, v);
        }

        let receipt = |exit_code, stdout: String, stderr: String| GitFailureReceipt {
            args: arg_strings.clone(),
            cwd: cwd.to_path_buf(),
            exit_code,
            stdout,
            stderr,
        };

        let out = cmd
            .output()
            .map_err(|e| receipt(None, String::new(), format!("spawn failed: {e}")))?;

        let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
        let stderr = String::from_utf8_lossy(&out.stderr).into_owned();

        if out.status.success() {
            Ok(GitOutput { stdout, stderr })
        } else {
            Err(receipt(out.status.code(), stdout, stderr))
        }
    }

    /// [`Self::run`], with the failure already lifted into [`WorktreeError`].
    pub fn try_run<S: AsRef<OsStr>>(
        &self,
        cwd: &Path,
        args: &[S],
    ) -> Result<GitOutput, WorktreeError> {
        self.run(cwd, args).map_err(WorktreeError::GitFailure)
    }
}

/// Facts about a repository, read fresh. Nothing here is cached: reconciled
/// inspection is the source of truth, and a cache is how an observer starts
/// reporting a past it no longer has.
pub mod inspect {
    use super::*;

    /// Absolute path of the repository's `.git` directory for `cwd` — the
    /// per-worktree one, not the common dir. In-progress-operation markers live
    /// here, so a rebase in one worktree does not look like a rebase in another.
    pub fn git_dir(git: &GitCli, cwd: &Path) -> Result<PathBuf, WorktreeError> {
        if !cwd.exists() {
            return Err(WorktreeError::NotARepository(cwd.to_path_buf()));
        }
        let out = git
            .run(cwd, &["rev-parse", "--absolute-git-dir"])
            .map_err(|_| WorktreeError::NotARepository(cwd.to_path_buf()))?;
        Ok(PathBuf::from(out.trimmed()))
    }

    /// Absolute path of the working tree root for `cwd`.
    pub fn work_tree(git: &GitCli, cwd: &Path) -> Result<PathBuf, WorktreeError> {
        let out = git
            .run(cwd, &["rev-parse", "--show-toplevel"])
            .map_err(|_| WorktreeError::NotARepository(cwd.to_path_buf()))?;
        Ok(PathBuf::from(out.trimmed()))
    }

    /// One parsed `git status --porcelain=v1 -z` entry. `x`/`y` are the index
    /// and worktree status columns; `path` is repository-relative.
    struct StatusEntry<'a> {
        x: char,
        y: char,
        path: &'a str,
        /// The pre-rename/copy path (`R`/`C` entries only). Load-bearing for
        /// the snapshot: staging only the NEW path into the temp index leaves
        /// the OLD path present from `read-tree HEAD`, so the synthetic tree
        /// would resurrect a file the rename removed.
        orig_path: Option<&'a str>,
    }

    /// Parse `-z` porcelain v1 output into entries, consuming the extra
    /// `orig_path` field a rename/copy (`R`/`C` in either column) appends —
    /// otherwise every entry after the first rename would misalign.
    fn parse_porcelain_z(stdout: &str) -> Vec<StatusEntry<'_>> {
        let mut parts: Vec<&str> = stdout.split('\0').collect();
        if parts.last() == Some(&"") {
            parts.pop();
        }
        let mut out = Vec::new();
        let mut i = 0;
        while i < parts.len() {
            let entry = parts[i];
            i += 1;
            if entry.len() < 3 {
                continue;
            }
            let bytes = entry.as_bytes();
            let x = bytes[0] as char;
            let y = bytes[1] as char;
            let path = &entry[3..];
            let orig_path = if x == 'R' || x == 'C' || y == 'R' || y == 'C' {
                // Rename/copy entries carry an extra orig_path field.
                let orig = parts.get(i).copied();
                i += 1;
                orig
            } else {
                None
            };
            out.push(StatusEntry {
                x,
                y,
                path,
                orig_path,
            });
        }
        out
    }

    /// The source's dirty state, read without changing anything.
    ///
    /// Lives here rather than in [`crate::snapshot`] because BOTH the clean
    /// path (to produce [`WorktreeError::SourceDirty`]) and the snapshot path
    /// (to record `pre_status`) need it, and two notions of "dirty" that drift
    /// apart would let a source be refused by one and captured differently by
    /// the other.
    ///
    /// Two reads: the first (no `--ignored`) classifies staged/unstaged/
    /// untracked; the second (`--ignored=matching`) counts ignored paths
    /// without asking the first read to carry a flag it does not need.
    pub fn dirty_summary(
        git: &GitCli,
        source: &Path,
    ) -> Result<crate::error::DirtySummary, WorktreeError> {
        use std::collections::BTreeSet;

        let out = git.try_run(
            source,
            &["status", "--porcelain=v1", "-z", "--untracked-files=normal"],
        )?;

        let mut staged = BTreeSet::new();
        let mut unstaged = BTreeSet::new();
        let mut untracked = BTreeSet::new();
        for e in parse_porcelain_z(&out.stdout) {
            if e.x == '?' && e.y == '?' {
                untracked.insert(e.path.to_string());
                continue;
            }
            if e.x == '!' && e.y == '!' {
                continue;
            }
            if e.x != ' ' {
                staged.insert(e.path.to_string());
                if let Some(orig) = e.orig_path {
                    // A rename's OLD path is part of the same change: staging
                    // it records the deletion in the snapshot's temp index.
                    staged.insert(orig.to_string());
                }
            }
            if e.y != ' ' {
                unstaged.insert(e.path.to_string());
                if let Some(orig) = e.orig_path {
                    unstaged.insert(orig.to_string());
                }
            }
        }

        let ignored_out = git.try_run(
            source,
            &[
                "status",
                "--porcelain=v1",
                "-z",
                "--untracked-files=normal",
                "--ignored=matching",
            ],
        )?;
        let ignored_excluded = parse_porcelain_z(&ignored_out.stdout)
            .into_iter()
            .filter(|e| e.x == '!' && e.y == '!')
            .count();

        Ok(crate::error::DirtySummary {
            staged: staged.into_iter().collect(),
            unstaged: unstaged.into_iter().collect(),
            untracked: untracked.into_iter().collect(),
            ignored_excluded,
        })
    }

    /// Which in-progress operation, if any, the worktree at `cwd` is inside.
    ///
    /// Checked by marker path rather than by parsing `git status`, because the
    /// markers are what git itself keys on and they stay meaningful when the
    /// status output format changes.
    pub fn in_progress(git: &GitCli, cwd: &Path) -> Result<Option<InProgressKind>, WorktreeError> {
        let dir = git_dir(git, cwd)?;
        let has = |name: &str| dir.join(name).exists();
        Ok(if has("MERGE_HEAD") {
            Some(InProgressKind::Merge)
        } else if has("rebase-merge") || has("rebase-apply") || has("REBASE_HEAD") {
            Some(InProgressKind::Rebase)
        } else if has("CHERRY_PICK_HEAD") {
            Some(InProgressKind::CherryPick)
        } else if has("REVERT_HEAD") {
            Some(InProgressKind::Revert)
        } else if has("BISECT_LOG") {
            Some(InProgressKind::Bisect)
        } else {
            None
        })
    }
}
