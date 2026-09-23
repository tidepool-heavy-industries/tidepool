//! The one place this crate shells out to git.
//!
//! Every call site funnels through [`GitCli`] so that the environment scrubbing, the
//! failure receipt shape, and the "never inherit the caller's index/config"
//! discipline exist once. A call site that spawns `Command::new("git")` itself has
//! bypassed all three.
//!
//! Why the git CLI rather than a libgit2 binding: the thing being observed is a
//! repository that real coding agents are mutating with the real `git` binary,
//! including operations (rebase, cherry-pick, worktree) whose on-disk state
//! libgit2 models incompletely. Reconciled inspection through the same tool the
//! writers use is the honest observer.

use parking_lot::{RawFairMutex, RawThreadId};
use std::collections::{BTreeMap, BTreeSet};
use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};
use std::process::Command;

use crate::error::{GitFailureReceipt, InProgressKind, WorktreeError};
use crate::id::GitOid;

/// The repository-local exclusions Exomonad installs, in the order written. Only
/// the runtime state directories are excluded: a project's `.exomonad/Project`
/// modules, skills, and configuration are authored source and stay tracked.
/// This is the single list; [`GitCli::ensure_exomonad_local_exclude`] is the
/// single writer.
pub const EXOMONAD_LOCAL_EXCLUDES: &[&str] = &[
    "/.exomonad/logs/",
    "/.exomonad/sessions/",
    "/.exomonad/runtime/",
    "/.exomonad/build/",
];

/// One `info/exclude` line without the carriage return of a CRLF file.
fn exclude_line(line: &[u8]) -> &[u8] {
    line.strip_suffix(b"\r").unwrap_or(line)
}

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
type AdmissionMutex = parking_lot::lock_api::ReentrantMutex<RawFairMutex, RawThreadId, ()>;
pub type GitCaptureGuard<'a> =
    parking_lot::lock_api::ReentrantMutexGuard<'a, RawFairMutex, RawThreadId, ()>;

#[derive(Clone, Debug, Default)]
pub struct GitCli {
    // Fair handoff keeps frequent short Git reads from starving a queued
    // source checkpoint. Recursive calls on the capture thread remain valid.
    admission: std::sync::Arc<AdmissionMutex>,
    /// Extra environment applied to every invocation (the snapshot lane sets
    /// `GIT_INDEX_FILE` here; the monitor sets nothing).
    env: BTreeMap<String, String>,
    #[cfg(target_os = "linux")]
    namespace: Option<exomonad_node::MountNamespace>,
    #[cfg(target_os = "linux")]
    views: crate::view::WorktreeViews,
}

impl GitCli {
    pub fn new() -> Self {
        Self::default()
    }

    /// Exclude this repository owner's host Git commands while capturing source
    /// and private Git state. The capture thread can issue nested Git commands;
    /// other threads wait at the normal invocation entry point. Native writers
    /// require their separate admission boundary.
    pub fn try_capture(&self) -> Option<GitCaptureGuard<'_>> {
        self.admission.try_lock()
    }

    /// [`Self::try_capture`], waiting up to `timeout` for running host Git
    /// commands to finish.
    pub fn capture_within(&self, timeout: std::time::Duration) -> Option<GitCaptureGuard<'_>> {
        self.admission.try_lock_for(timeout)
    }

    /// Bind host Git operations to the same mounted filesystem as its owner.
    /// Environment scrubbing and failure receipts remain at this entry point.
    #[cfg(target_os = "linux")]
    pub fn with_mount_namespace(&self, namespace: exomonad_node::MountNamespace) -> Self {
        let mut next = self.clone();
        next.namespace = Some(namespace);
        next
    }

    #[cfg(target_os = "linux")]
    pub(crate) fn with_worktree_views(mut self, views: crate::view::WorktreeViews) -> Self {
        self.views = views;
        self
    }

    /// Managed checkout allocation writes host-owned Git administration and
    /// storage, even when source inspection runs through a read-only actor view.
    pub(crate) fn on_host(&self) -> Self {
        let mut host = self.clone();
        #[cfg(target_os = "linux")]
        {
            host.namespace = None;
            host.views = crate::view::WorktreeViews::default();
        }
        host
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

    fn command(&self, cwd: &Path) -> std::io::Result<Command> {
        #[cfg(target_os = "linux")]
        if let Some(view) = self.views.resolve(cwd)? {
            return view.namespace.host_command(&view.root, OsStr::new("git"));
        }
        #[cfg(target_os = "linux")]
        if let Some(namespace) = &self.namespace {
            return namespace.host_command(cwd, OsStr::new("git"));
        }
        let mut command = Command::new("git");
        command.current_dir(cwd);
        Ok(command)
    }

    /// Filesystem inspection uses the same view as Git, never its host alias.
    pub(crate) fn try_exists(&self, path: &Path) -> Result<bool, WorktreeError> {
        #[cfg(target_os = "linux")]
        if let Some(view) = self
            .views
            .resolve(path)
            .map_err(|error| crate::storage::storage_failure(path, error))?
        {
            return view
                .namespace
                .try_exists(&view.root)
                .map_err(|error| crate::storage::storage_failure(path, error));
        }
        #[cfg(target_os = "linux")]
        let result = match &self.namespace {
            Some(namespace) => namespace.try_exists(path),
            None => path.try_exists(),
        };
        #[cfg(not(target_os = "linux"))]
        let result = path.try_exists();
        result.map_err(|error| crate::storage::storage_failure(path, error))
    }

    fn try_exists_in_repo(&self, repo: &Path, path: &Path) -> Result<bool, WorktreeError> {
        #[cfg(target_os = "linux")]
        if let Some(view) = self
            .views
            .resolve(repo)
            .map_err(|error| crate::storage::storage_failure(repo, error))?
        {
            return view
                .namespace
                .try_exists(path)
                .map_err(|error| crate::storage::storage_failure(path, error));
        }
        self.try_exists(path)
    }

    /// Copy private Git state out of the same filesystem view used for commands.
    /// The destination belongs to the host; it need not be writable in that view.
    pub(crate) fn copy_file_to_host(
        &self,
        source: &Path,
        destination: &Path,
    ) -> Result<(), WorktreeError> {
        let copy = || -> std::io::Result<()> {
            #[cfg(target_os = "linux")]
            if let Some(namespace) = &self.namespace {
                let output = namespace
                    .host_command(Path::new("/"), OsStr::new("cat"))?
                    .arg("--")
                    .arg(source)
                    .stdout(std::fs::File::create(destination)?)
                    .output()?;
                if !output.status.success() {
                    return Err(std::io::Error::other(
                        String::from_utf8_lossy(&output.stderr).into_owned(),
                    ));
                }
                return Ok(());
            }
            std::fs::copy(source, destination)?;
            Ok(())
        };
        copy().map_err(|error| crate::storage::storage_failure(source, error))
    }

    /// Run git in `cwd`. `Err` only for a nonzero exit or a spawn failure; a
    /// command that succeeds with output on stderr is still `Ok`.
    fn run_output<S: AsRef<OsStr>>(
        &self,
        cwd: &Path,
        args: &[S],
    ) -> Result<std::process::Output, GitFailureReceipt> {
        let _admission = self.admission.lock();
        let arg_strings: Vec<String> = args
            .iter()
            .map(|a| a.as_ref().to_string_lossy().into_owned())
            .collect();

        let mut cmd = self.command(cwd).map_err(|error| GitFailureReceipt {
            args: arg_strings.clone(),
            cwd: cwd.to_path_buf(),
            exit_code: None,
            stdout: String::new(),
            stderr: error.to_string(),
        })?;
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

        if !out.status.success() {
            Err(receipt(
                out.status.code(),
                String::from_utf8_lossy(&out.stdout).into_owned(),
                String::from_utf8_lossy(&out.stderr).into_owned(),
            ))
        } else {
            Ok(out)
        }
    }

    pub fn run<S: AsRef<OsStr>>(
        &self,
        cwd: &Path,
        args: &[S],
    ) -> Result<GitOutput, GitFailureReceipt> {
        let out = self.run_output(cwd, args)?;
        Ok(GitOutput {
            stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
        })
    }

    fn stdout_bytes<S: AsRef<OsStr>>(
        &self,
        cwd: &Path,
        args: &[S],
    ) -> Result<Vec<u8>, WorktreeError> {
        self.run_output(cwd, args)
            .map(|output| output.stdout)
            .map_err(WorktreeError::GitFailure)
    }

    /// [`Self::run`], with the failure already lifted into [`WorktreeError`].
    pub fn try_run<S: AsRef<OsStr>>(
        &self,
        cwd: &Path,
        args: &[S],
    ) -> Result<GitOutput, WorktreeError> {
        self.run(cwd, args).map_err(WorktreeError::GitFailure)
    }

    /// Add repository-local exclusions for Exomonad's runtime state. This leaves
    /// project ignore files and existing `info/exclude` bytes intact, and
    /// removes a whole-directory `/.exomonad/` line, which would hide a project's
    /// authored modules, skills, and configuration from Git.
    pub fn ensure_exomonad_local_exclude(&self, repo: &Path) -> Result<(), WorktreeError> {
        let _admission = self.admission.lock();
        let path = inspect::git_common_dir(self, repo)?.join("info/exclude");
        let parent = path.parent().ok_or_else(|| WorktreeError::StorageFailure {
            path: path.clone(),
            detail: "Git exclude path has no parent".to_owned(),
        })?;
        std::fs::create_dir_all(parent)
            .map_err(|error| crate::storage::storage_failure(parent, error))?;
        let existing = match std::fs::read(&path) {
            Ok(contents) => contents,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Vec::new(),
            Err(error) => return Err(crate::storage::storage_failure(&path, error)),
        };
        let mut lines: Vec<&[u8]> = existing.split(|byte| *byte == b'\n').collect();
        if lines.last().is_some_and(|line| line.is_empty()) {
            lines.pop();
        }
        let mut contents = Vec::with_capacity(existing.len());
        for line in &lines {
            if exclude_line(line) == b"/.exomonad/" {
                continue;
            }
            contents.extend_from_slice(line);
            contents.push(b'\n');
        }
        for exclusion in EXOMONAD_LOCAL_EXCLUDES {
            if !lines
                .iter()
                .any(|line| exclude_line(line) == exclusion.as_bytes())
            {
                contents.extend_from_slice(exclusion.as_bytes());
                contents.push(b'\n');
            }
        }
        if contents == existing {
            return Ok(());
        }
        tidepool_atomic_write::write_best_effort(&path, &contents)
            .map_err(|error| crate::storage::storage_failure(&error.path, error.source))
    }

    /// Commit all eligible source working-tree changes before a fork. The
    /// temporary index starts at HEAD, so a pre-staged excluded path cannot
    /// enter the commit or be unstaged by the post-commit real-index update.
    /// The caller holds the source capture gate against native writers.
    pub fn checkpoint_source(
        &self,
        repo: &Path,
        excluded: &[OsString],
    ) -> Result<GitOid, WorktreeError> {
        let _admission = self.admission.lock();
        inspect::work_tree(self, repo)?;
        if let Some(kind) = inspect::in_progress(self, repo)? {
            return Err(WorktreeError::SourceOperationInProgress(kind));
        }
        // A superproject commit can record only a submodule's HEAD. Refuse
        // uncommitted nested working files before any checkpoint mutation.
        crate::snapshot::refuse_dirty_submodules(self, repo)?;
        self.try_run(repo, &["symbolic-ref", "HEAD"])?;
        let head = GitOid::from_raw(
            self.try_run(repo, &["rev-parse", "--verify", "HEAD^{commit}"])?
                .trimmed()
                .to_owned(),
        );

        // Paths are repository-relative; callers may exclude a root entry or
        // nested runtime directory. Literal pathspecs prevent Git metacharacters
        // from changing the exclusion's meaning.
        let mut paths = vec![OsString::from(":(top)")];
        for name in excluded {
            let mut path = OsString::from(":(top,exclude,literal)");
            path.push(name);
            paths.push(path);
        }
        let common = inspect::git_common_dir(&self.on_host(), repo)?;
        let temporary = tempfile::Builder::new()
            .prefix("exomonad-checkpoint-")
            .tempdir_in(&common)
            .map_err(|error| crate::storage::storage_failure(&common, error))?;
        let index = temporary.path().join("index");
        let index =
            camino::Utf8Path::from_path(&index).ok_or_else(|| WorktreeError::StorageFailure {
                path: index.clone(),
                detail: "temporary index path is not valid UTF-8".to_owned(),
            })?;
        let temp_git = self.with_env("GIT_INDEX_FILE", index.as_str());
        temp_git.try_run(repo, &["read-tree", "HEAD"])?;

        // Enumerate eligible files before asking Git to create any blobs.
        // The HEAD-backed temporary index covers tracked deletions and ordinary
        // untracked files; the real index adds explicitly staged ignored files.
        let mut list = vec![
            OsString::from("ls-files"),
            OsString::from("--full-name"),
            OsString::from("-z"),
            OsString::from("--cached"),
            OsString::from("--others"),
            OsString::from("--exclude-standard"),
            OsString::from("--deduplicate"),
            OsString::from("--"),
        ];
        list.extend(paths.iter().cloned());
        let split = |output: Vec<u8>| -> BTreeSet<Vec<u8>> {
            output
                .split(|byte| *byte == 0)
                .filter(|path| !path.is_empty())
                .map(<[u8]>::to_vec)
                .collect()
        };
        let mut eligible = split(temp_git.stdout_bytes(repo, &list)?);
        let staged = split(self.stdout_bytes(repo, &list)?);
        let mut deleted_args = vec![
            OsString::from("ls-files"),
            OsString::from("--full-name"),
            OsString::from("-z"),
            OsString::from("--deleted"),
            OsString::from("--"),
        ];
        deleted_args.extend(paths.iter().cloned());
        let deleted = split(self.stdout_bytes(repo, &deleted_args)?);
        eligible.extend(staged.into_iter().filter(|path| !deleted.contains(path)));
        if eligible.is_empty() {
            return Ok(head);
        }

        let pathspec_file = temporary.path().join("paths");
        let mut pathspecs = Vec::new();
        for path in eligible {
            pathspecs.extend_from_slice(b":(top,literal)");
            pathspecs.extend_from_slice(&path);
            pathspecs.push(0);
        }
        std::fs::write(&pathspec_file, pathspecs)
            .map_err(|error| crate::storage::storage_failure(&pathspec_file, error))?;
        let mut add = vec![
            OsString::from("add"),
            OsString::from("-A"),
            OsString::from("-f"),
            OsString::from("--pathspec-file-nul"),
        ];
        let mut file_arg = OsString::from("--pathspec-from-file=");
        file_arg.push(&pathspec_file);
        add.push(file_arg);
        temp_git.try_run(repo, &add)?;

        let changed = match temp_git.run(repo, &["diff", "--cached", "--quiet", "--exit-code"]) {
            Ok(_) => false,
            Err(failure) if failure.exit_code == Some(1) => true,
            Err(failure) => return Err(WorktreeError::GitFailure(failure)),
        };
        if !changed {
            return Ok(head);
        }
        temp_git.try_run(
            repo,
            &[
                "-c",
                "core.hooksPath=/dev/null",
                "-c",
                "commit.gpgsign=false",
                "commit",
                "--no-verify",
                "--no-gpg-sign",
                "-m",
                "chore: checkpoint source before Exomonad fork\n\nAutogenerated source checkpoint. Make meaningful commits when ready. No checks ran.",
            ],
        )?;
        let committed = GitOid::from_raw(
            self.try_run(repo, &["rev-parse", "--verify", "HEAD^{commit}"])?
                .trimmed()
                .to_owned(),
        );
        let mut reset = vec![
            OsString::from("reset"),
            OsString::from("--quiet"),
            OsString::from("HEAD"),
            OsString::from("--"),
        ];
        reset.extend(paths);
        self.try_run(repo, &reset)?;
        Ok(committed)
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

    /// Absolute path of the shared Git metadata used by `cwd`.
    ///
    /// A linked worktree's `.git` is only a pointer file. Callers granting a
    /// process enough filesystem authority to commit must grant the resolved
    /// common directory, not assume that `<worktree>/.git` is a directory.
    pub fn git_common_dir(git: &GitCli, cwd: &Path) -> Result<PathBuf, WorktreeError> {
        let out = git
            .run(
                cwd,
                &["rev-parse", "--path-format=absolute", "--git-common-dir"],
            )
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
        let has = |name: &str| git.try_exists_in_repo(cwd, &dir.join(name));
        Ok(if has("MERGE_HEAD")? {
            Some(InProgressKind::Merge)
        } else if has("rebase-merge")? || has("rebase-apply")? || has("REBASE_HEAD")? {
            Some(InProgressKind::Rebase)
        } else if has("CHERRY_PICK_HEAD")? {
            Some(InProgressKind::CherryPick)
        } else if has("REVERT_HEAD")? {
            Some(InProgressKind::Revert)
        } else if has("BISECT_LOG")? {
            Some(InProgressKind::Bisect)
        } else {
            None
        })
    }
}

#[cfg(test)]
mod admission_tests {

    #[test]
    fn capture_within_waits_for_a_running_capture() {
        let repo = crate::testing::TestRepo::init().unwrap();
        let git = repo.git().clone();
        let holder = git.clone();
        let (held, ready) = std::sync::mpsc::channel();
        let thread = std::thread::spawn(move || {
            let _capture = holder.try_capture().unwrap();
            held.send(()).unwrap();
            std::thread::sleep(std::time::Duration::from_millis(200));
        });
        ready.recv().unwrap();
        assert!(git.try_capture().is_none());
        assert!(git
            .capture_within(std::time::Duration::from_secs(10))
            .is_some());
        thread.join().unwrap();
    }

    #[test]
    fn source_capture_excludes_host_git_mutation_and_allows_its_own_reads() {
        let repo = crate::testing::TestRepo::init().unwrap();
        repo.writer().commit_file("file", "seed", "seed").unwrap();
        std::fs::write(repo.path().join("file"), "changed").unwrap();
        let git = repo.git().clone();
        let capture = git.try_capture().unwrap();
        assert_eq!(
            git.try_run(repo.path(), &["show", ":file"])
                .unwrap()
                .trimmed(),
            "seed"
        );
        let writer = git.clone();
        let path = repo.path().to_owned();
        let (started, ready) = std::sync::mpsc::channel();
        let (finished, done) = std::sync::mpsc::channel();
        let thread = std::thread::spawn(move || {
            assert!(writer.try_capture().is_none());
            started.send(()).unwrap();
            writer.try_run(&path, &["add", "file"]).unwrap();
            finished.send(()).unwrap();
        });
        ready.recv().unwrap();
        assert!(done.try_recv().is_err());
        assert_eq!(
            git.try_run(repo.path(), &["show", ":file"])
                .unwrap()
                .trimmed(),
            "seed"
        );
        drop(capture);
        done.recv_timeout(std::time::Duration::from_secs(10))
            .unwrap();
        thread.join().unwrap();
        assert_eq!(
            git.try_run(repo.path(), &["show", ":file"])
                .unwrap()
                .trimmed(),
            "changed"
        );
    }
}
