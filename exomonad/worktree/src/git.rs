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

use std::collections::{BTreeMap, BTreeSet};
use std::ffi::{OsStr, OsString};
use std::fs::File;
use std::io;
use std::os::unix::ffi::OsStringExt;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;
use std::time::Duration;

use crate::admission::{GitAccess, GitBacking, GitCandidate, GitPermit};
use crate::error::{GitFailureReceipt, InProgressKind, WorktreeError};
use crate::id::GitOid;

/// The repository-local exclusions Exomonad installs, in the order written. Only
/// the runtime state directories and generated preparation pointer are excluded:
/// a project's `.exomonad/Project` modules, skills, and configuration are authored
/// source and stay tracked.
/// This is the single list; [`GitCli::ensure_exomonad_local_exclude`] is the
/// single writer.
pub const EXOMONAD_LOCAL_EXCLUDES: &[&str] = &[
    "/.exomonad/logs/",
    "/.exomonad/sessions/",
    "/.exomonad/runtime/",
    "/.exomonad/build/",
    "/.exomonad/prepared.json",
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

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SourceWorkingPath {
    pub path: PathBuf,
    pub tracked: bool,
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

std::thread_local! {
    static ACTIVE_BACKINGS: std::cell::RefCell<Vec<ScopeBacking>> = const { std::cell::RefCell::new(Vec::new()) };
    static SCOPED_IDENTITIES: std::cell::RefCell<Vec<ScopedIdentity>> = const { std::cell::RefCell::new(Vec::new()) };
}

#[derive(Clone)]
struct ScopeBacking {
    owner: Arc<()>,
    backing: GitBacking,
    permit: Arc<GitPermit>,
    access: GitAccess,
}

#[derive(Clone)]
struct ScopedIdentity {
    owner: Arc<()>,
    requested: PathBuf,
    backing: GitBacking,
    worktree: (u64, u64),
    git_dir: (u64, u64),
    view: GitExecutionView,
}

pub struct GitScopeGuard {
    owner: Arc<()>,
    backing_count: usize,
    identity_count: usize,
    _thread_bound: std::marker::PhantomData<std::rc::Rc<()>>,
}

impl Drop for GitScopeGuard {
    fn drop(&mut self) {
        ACTIVE_BACKINGS.with(|active| {
            let mut active = active.borrow_mut();
            let before = active.len();
            active.retain(|scope| !Arc::ptr_eq(&scope.owner, &self.owner));
            assert_eq!(
                before - active.len(),
                self.backing_count,
                "Git scope backing changed"
            );
        });
        SCOPED_IDENTITIES.with(|active| {
            let mut active = active.borrow_mut();
            let before = active.len();
            active.retain(|scope| !Arc::ptr_eq(&scope.owner, &self.owner));
            assert_eq!(
                before - active.len(),
                self.identity_count,
                "Git scope identity changed"
            );
        });
    }
}

pub type GitCaptureGuard = GitScopeGuard;
pub type GitWriteGuard = GitScopeGuard;

#[derive(Clone, Copy)]
enum ScopePurpose {
    Transaction,
    Command,
}

#[derive(Clone)]
enum GitExecutionView {
    Host(PathBuf),
    #[cfg(target_os = "linux")]
    Mounted {
        namespace: exomonad_node::MountNamespace,
        cwd: PathBuf,
    },
}

impl GitExecutionView {
    fn directory(&self) -> &Path {
        match self {
            Self::Host(cwd) => cwd,
            #[cfg(target_os = "linux")]
            Self::Mounted { cwd, .. } => cwd,
        }
    }

    fn worktree_identity(&self) -> io::Result<(u64, u64)> {
        let metadata = self.open_directory(self.directory())?.metadata()?;
        Ok((metadata.dev(), metadata.ino()))
    }

    fn same_view_as(&self, other: &Self) -> io::Result<bool> {
        match (self, other) {
            (Self::Host(_), Self::Host(_)) => Ok(true),
            #[cfg(target_os = "linux")]
            (
                Self::Mounted {
                    namespace: left, ..
                },
                Self::Mounted {
                    namespace: right, ..
                },
            ) => left.same_view_as(right),
            #[cfg(target_os = "linux")]
            _ => Ok(false),
        }
    }

    fn output<S: AsRef<OsStr>>(
        &self,
        args: &[S],
        environment: &[(OsString, Option<OsString>)],
    ) -> io::Result<std::process::Output> {
        match self {
            Self::Host(cwd) => {
                #[allow(clippy::disallowed_methods, reason = "the one Git CLI owner")]
                let mut command = Command::new("git");
                command.current_dir(cwd).args(args);
                for (key, value) in environment {
                    match value {
                        Some(value) => {
                            command.env(key, value);
                        }
                        None => {
                            command.env_remove(key);
                        }
                    }
                }
                command.output()
            }
            #[cfg(target_os = "linux")]
            Self::Mounted { namespace, cwd } => {
                let arguments = args
                    .iter()
                    .map(|arg| arg.as_ref().to_owned())
                    .collect::<Vec<_>>();
                exomonad_node::view_command::output_in_view(
                    namespace,
                    cwd,
                    OsStr::new("git"),
                    &arguments,
                    environment,
                )
            }
        }
    }

    fn open_directory(&self, path: &Path) -> io::Result<File> {
        match self {
            Self::Host(_) => File::open(path),
            #[cfg(target_os = "linux")]
            Self::Mounted { namespace, .. } => namespace.open_view_directory(path),
        }
    }
}

#[derive(Clone, Debug, Default)]
pub struct GitCli {
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

    /// Keep a multi-command observation coherent with cooperating writers.
    /// Nested reads reuse this permit; a read scope cannot be promoted to a
    /// write or capture scope while another reader may hold the backing.
    pub fn read_scope(&self, cwd: &Path) -> Result<GitScopeGuard, WorktreeError> {
        self.scope_within(cwd, GitAccess::Read, Duration::MAX)?
            .ok_or_else(|| crate::storage::storage_failure(cwd, "Git read admission timed out"))
    }

    /// Create a repository before a common Git directory exists to admit.
    /// The caller owns the new directory's allocation; subsequent commands
    /// enter the ordinary backing admission protocol.
    pub fn init_repository<S: AsRef<OsStr>>(
        &self,
        cwd: &Path,
        options: &[S],
    ) -> Result<GitOutput, WorktreeError> {
        let mut args = vec![OsString::from("init")];
        args.extend(options.iter().map(|option| option.as_ref().to_owned()));
        let view = self
            .execution_view(cwd)
            .map_err(|error| crate::storage::storage_failure(cwd, error))?;
        let output = view
            .output(&args, &self.environment())
            .map_err(|error| crate::storage::storage_failure(cwd, error))?;
        if !output.status.success() {
            return Err(WorktreeError::GitFailure(GitFailureReceipt {
                args: args
                    .iter()
                    .map(|arg| arg.to_string_lossy().into_owned())
                    .collect(),
                cwd: cwd.to_owned(),
                exit_code: output.status.code(),
                stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
                stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
            }));
        }
        Ok(GitOutput {
            stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
        })
    }

    /// Exclude cooperating Git operations on this repository while capturing
    /// source and private metadata. The capture thread may read its own scope.
    /// Native writers require their separate process admission and sampling.
    pub fn try_capture(&self, cwd: &Path) -> Result<Option<GitCaptureGuard>, WorktreeError> {
        self.capture_within(cwd, Duration::ZERO)
    }

    /// [`Self::try_capture`], waiting up to `timeout`. The permit covers the
    /// whole capture transaction, not one Git invocation inside it.
    pub fn capture_within(
        &self,
        cwd: &Path,
        timeout: Duration,
    ) -> Result<Option<GitCaptureGuard>, WorktreeError> {
        self.scope_within(cwd, GitAccess::Capture, timeout)
    }

    /// Hold one repository's cooperative write authority through a complete
    /// operation, including its checks, Git calls, and filesystem updates.
    pub fn write_within(
        &self,
        cwd: &Path,
        timeout: Duration,
    ) -> Result<Option<GitWriteGuard>, WorktreeError> {
        self.scope_within(cwd, GitAccess::Write, timeout)
    }

    pub fn write_scope(&self, cwd: &Path) -> Result<GitWriteGuard, WorktreeError> {
        self.write_within(cwd, Duration::MAX)?
            .ok_or_else(|| crate::storage::storage_failure(cwd, "Git write admission timed out"))
    }

    /// Acquire all existing repository backings in kernel-identity order. A
    /// merge can mutate its superproject and initialized workspace repository;
    /// sorting prevents opposite source/target merges from deadlocking.
    pub fn write_scope_many(&self, paths: &[&Path]) -> Result<GitWriteGuard, WorktreeError> {
        if paths.is_empty() {
            return Err(crate::storage::storage_failure(
                Path::new("/"),
                "Git write admission requires a repository",
            ));
        }
        self.scope_many_within(paths, GitAccess::Write, Duration::MAX)?
            .ok_or_else(|| {
                crate::storage::storage_failure(
                    paths.first().copied().unwrap_or(Path::new("/")),
                    "Git write admission timed out",
                )
            })
    }

    fn scope_within(
        &self,
        cwd: &Path,
        access: GitAccess,
        timeout: Duration,
    ) -> Result<Option<GitScopeGuard>, WorktreeError> {
        self.scope_many_within(&[cwd], access, timeout)
    }

    fn scope_many_within(
        &self,
        paths: &[&Path],
        access: GitAccess,
        timeout: Duration,
    ) -> Result<Option<GitScopeGuard>, WorktreeError> {
        self.admit_scope(paths, access, timeout, ScopePurpose::Transaction)
    }

    fn admit_scope(
        &self,
        paths: &[&Path],
        access: GitAccess,
        timeout: Duration,
        purpose: ScopePurpose,
    ) -> Result<Option<GitScopeGuard>, WorktreeError> {
        let started = std::time::Instant::now();
        let owner = Arc::new(());
        let mut common_by_backing = BTreeMap::new();
        let mut scoped = Vec::new();
        for cwd in paths {
            let view = self
                .execution_view(cwd)
                .map_err(|error| crate::storage::storage_failure(cwd, error))?;
            let common = self
                .common_directory(cwd, &view)
                .map_err(WorktreeError::GitFailure)?;
            let candidate = GitCandidate::open(&common)
                .map_err(|error| crate::storage::storage_failure(cwd, error))?;
            let backing = candidate.backing;
            let git_dir = self
                .git_directory(cwd, &view)
                .map_err(WorktreeError::GitFailure)?
                .metadata()
                .map_err(|error| crate::storage::storage_failure(cwd, error))?;
            scoped.push(ScopedIdentity {
                owner: owner.clone(),
                requested: cwd.to_path_buf(),
                backing,
                worktree: view
                    .worktree_identity()
                    .map_err(|error| crate::storage::storage_failure(cwd, error))?,
                git_dir: (git_dir.dev(), git_dir.ino()),
                view,
            });
            common_by_backing
                .entry(backing)
                .or_insert((candidate, *cwd));
        }
        let existing = SCOPED_IDENTITIES.with(|active| active.borrow().clone());
        for scope in &scoped {
            if let Some(previous) = existing
                .iter()
                .rev()
                .find(|previous| previous.requested == scope.requested)
            {
                let same_view = previous
                    .view
                    .same_view_as(&scope.view)
                    .map_err(|error| crate::storage::storage_failure(&scope.requested, error))?;
                if !same_view
                    || previous.backing != scope.backing
                    || previous.worktree != scope.worktree
                    || previous.git_dir != scope.git_dir
                {
                    return Err(crate::storage::storage_failure(
                        &scope.requested,
                        "Git backing or worktree view changed during admitted operation",
                    ));
                }
            }
        }
        let active = ACTIVE_BACKINGS.with(|active| active.borrow().clone());
        if matches!(purpose, ScopePurpose::Command) {
            for scope in &scoped {
                if active.iter().any(|held| held.backing == scope.backing)
                    && !existing
                        .iter()
                        .any(|held| held.requested == scope.requested)
                {
                    return Err(crate::storage::storage_failure(
                        &scope.requested,
                        "Git worktree was not included in the active admission scope",
                    ));
                }
            }
        }
        if let Some(highest) = active.iter().map(|scope| scope.backing).max() {
            if common_by_backing.keys().any(|backing| {
                !active.iter().any(|scope| scope.backing == *backing) && backing < &highest
            }) {
                return Err(crate::storage::storage_failure(
                    paths[0],
                    "nested Git admission would reverse backing order",
                ));
            }
        }
        #[cfg(test)]
        admission_tests::before_acquire();
        let mut backings = Vec::new();
        for (backing, (candidate, cwd)) in common_by_backing {
            let (permit, held_access) = if let Some(existing) =
                active.iter().rev().find(|scope| scope.backing == backing)
            {
                if !existing.access.permits(access) {
                    return Err(crate::storage::storage_failure(
                        cwd,
                        "cannot promote a Git read admission to write or capture",
                    ));
                }
                (existing.permit.clone(), existing.access)
            } else {
                match GitPermit::acquire(
                    candidate,
                    access,
                    timeout.saturating_sub(started.elapsed()),
                ) {
                    Ok(permit) => (Arc::new(permit), access),
                    Err(error) if error.kind() == io::ErrorKind::WouldBlock => return Ok(None),
                    Err(error) => return Err(crate::storage::storage_failure(cwd, error)),
                }
            };
            backings.push(ScopeBacking {
                owner: owner.clone(),
                backing,
                permit,
                access: held_access,
            });
        }
        // A view or Git pointer can move while flock waits. Recheck every
        // selected identity before publishing the scope to the caller, whose
        // transaction may inspect files directly as well as invoke Git.
        for expected in &scoped {
            let cwd = &expected.requested;
            let view = self
                .execution_view(cwd)
                .map_err(|error| crate::storage::storage_failure(cwd, error))?;
            let same_view = expected
                .view
                .same_view_as(&view)
                .map_err(|error| crate::storage::storage_failure(cwd, error))?;
            let worktree = view
                .worktree_identity()
                .map_err(|error| crate::storage::storage_failure(cwd, error))?;
            let common = self
                .common_directory(cwd, &view)
                .map_err(WorktreeError::GitFailure)?;
            let backing = GitCandidate::open(&common)
                .map_err(|error| crate::storage::storage_failure(cwd, error))?
                .backing;
            let git_dir = self
                .git_directory(cwd, &view)
                .map_err(WorktreeError::GitFailure)?
                .metadata()
                .map_err(|error| crate::storage::storage_failure(cwd, error))?;
            if !same_view
                || expected.worktree != worktree
                || expected.backing != backing
                || expected.git_dir != (git_dir.dev(), git_dir.ino())
            {
                return Err(crate::storage::storage_failure(
                    cwd,
                    "Git backing or worktree view changed while acquiring admission",
                ));
            }
        }
        ACTIVE_BACKINGS.with(|active| active.borrow_mut().extend(backings.iter().cloned()));
        SCOPED_IDENTITIES.with(|active| active.borrow_mut().extend(scoped.iter().cloned()));
        Ok(Some(GitScopeGuard {
            owner,
            backing_count: backings.len(),
            identity_count: scoped.len(),
            _thread_bound: std::marker::PhantomData,
        }))
    }

    fn execution_view(&self, cwd: &Path) -> io::Result<GitExecutionView> {
        #[cfg(target_os = "linux")]
        {
            if let Some(view) = self.views.resolve(cwd)? {
                return Ok(GitExecutionView::Mounted {
                    namespace: view.namespace,
                    cwd: view.root,
                });
            }
            if let Some(namespace) = &self.namespace {
                return Ok(GitExecutionView::Mounted {
                    namespace: namespace.clone(),
                    cwd: cwd.to_owned(),
                });
            }
        }
        Ok(GitExecutionView::Host(cwd.to_owned()))
    }

    fn environment(&self) -> Vec<(OsString, Option<OsString>)> {
        let mut scrubbed: BTreeSet<OsString> = [
            "GIT_DIR",
            "GIT_INDEX_FILE",
            "GIT_WORK_TREE",
            "GIT_OBJECT_DIRECTORY",
            "GIT_COMMON_DIR",
            "GIT_ALTERNATE_OBJECT_DIRECTORIES",
            "GIT_CONFIG",
            "GIT_CONFIG_COUNT",
            "GIT_CONFIG_PARAMETERS",
            "GIT_CONFIG_SYSTEM",
            "GIT_CONFIG_GLOBAL",
            "GIT_CONFIG_NOSYSTEM",
            "GIT_CONFIG_KEY_",
            "GIT_CONFIG_VALUE_",
        ]
        .into_iter()
        .map(OsString::from)
        .collect();
        for (key, _) in std::env::vars_os() {
            let key_text = key.to_string_lossy();
            if ["GIT_CONFIG_KEY_", "GIT_CONFIG_VALUE_"]
                .iter()
                .any(|prefix| {
                    key_text
                        .strip_prefix(prefix)
                        .is_some_and(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()))
                })
            {
                scrubbed.insert(key);
            }
        }
        let mut environment: Vec<_> = scrubbed.into_iter().map(|key| (key, None)).collect();
        environment.extend([
            (
                OsString::from("GIT_TERMINAL_PROMPT"),
                Some(OsString::from("0")),
            ),
            (
                OsString::from("GIT_OPTIONAL_LOCKS"),
                Some(OsString::from("0")),
            ),
            (OsString::from("LC_ALL"), Some(OsString::from("C"))),
        ]);
        environment.extend(
            self.env
                .iter()
                .map(|(key, value)| (OsString::from(key), Some(OsString::from(value)))),
        );
        environment
    }

    fn common_directory(
        &self,
        cwd: &Path,
        view: &GitExecutionView,
    ) -> Result<File, GitFailureReceipt> {
        self.probed_directory(
            cwd,
            view,
            &["rev-parse", "--path-format=absolute", "--git-common-dir"],
        )
    }

    fn git_directory(
        &self,
        cwd: &Path,
        view: &GitExecutionView,
    ) -> Result<File, GitFailureReceipt> {
        self.probed_directory(cwd, view, &["rev-parse", "--absolute-git-dir"])
    }

    fn probed_directory(
        &self,
        cwd: &Path,
        view: &GitExecutionView,
        arguments: &[&str],
    ) -> Result<File, GitFailureReceipt> {
        let output = view
            .output(arguments, &self.environment())
            .map_err(|error| GitFailureReceipt {
                args: arguments.iter().map(|arg| (*arg).into()).collect(),
                cwd: cwd.to_owned(),
                exit_code: None,
                stdout: String::new(),
                stderr: format!("Git directory identity probe failed: {error}"),
            })?;
        if !output.status.success() {
            return Err(GitFailureReceipt {
                args: arguments.iter().map(|arg| (*arg).into()).collect(),
                cwd: cwd.to_owned(),
                exit_code: output.status.code(),
                stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
                stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
            });
        }
        let path = output.stdout.strip_suffix(b"\n").unwrap_or(&output.stdout);
        let path = PathBuf::from(OsString::from_vec(path.to_vec()));
        view.open_directory(&path)
            .map_err(|error| GitFailureReceipt {
                args: arguments.iter().map(|arg| (*arg).into()).collect(),
                cwd: cwd.to_owned(),
                exit_code: None,
                stdout: String::new(),
                stderr: format!("Git directory unavailable: {error}"),
            })
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

    /// Run git in `cwd` under the declared backing. Nonzero exits, launch
    /// failures, admission failures, and backing drift carry a receipt; a
    /// successful command with output on stderr remains `Ok`.
    fn run_output<S: AsRef<OsStr>>(
        &self,
        cwd: &Path,
        args: &[S],
        access: GitAccess,
    ) -> Result<std::process::Output, GitFailureReceipt> {
        let arg_strings: Vec<String> = args
            .iter()
            .map(|a| a.as_ref().to_string_lossy().into_owned())
            .collect();

        let receipt = |exit_code, stdout: String, stderr: String| GitFailureReceipt {
            args: arg_strings.clone(),
            cwd: cwd.to_path_buf(),
            exit_code,
            stdout,
            stderr,
        };

        let guard = self
            .admit_scope(&[cwd], access, Duration::MAX, ScopePurpose::Command)
            .map_err(|error| match error {
                WorktreeError::GitFailure(failure) => failure,
                error => receipt(None, String::new(), error.to_string()),
            })?
            .ok_or_else(|| receipt(None, String::new(), "Git admission timed out".to_owned()))?;
        let view = SCOPED_IDENTITIES.with(|scoped| {
            scoped
                .borrow()
                .iter()
                .rev()
                .find(|scope| Arc::ptr_eq(&scope.owner, &guard.owner) && scope.requested == cwd)
                .expect("admission publishes the requested view")
                .view
                .clone()
        });
        let out = view
            .output(args, &self.environment())
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
        let out = self.run_output(cwd, args, GitAccess::Write)?;
        Ok(GitOutput {
            stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
        })
    }

    /// Execute a command declared to inspect Git state without writing it.
    /// The caller chooses this typed authority; argument text is never used
    /// to infer whether a command can mutate repository state.
    pub fn read<S: AsRef<OsStr>>(
        &self,
        cwd: &Path,
        args: &[S],
    ) -> Result<GitOutput, GitFailureReceipt> {
        let out = self.run_output(cwd, args, GitAccess::Read)?;
        Ok(GitOutput {
            stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
        })
    }

    pub fn try_read<S: AsRef<OsStr>>(
        &self,
        cwd: &Path,
        args: &[S],
    ) -> Result<GitOutput, WorktreeError> {
        self.read(cwd, args).map_err(WorktreeError::GitFailure)
    }

    /// Run a read-only Git command and preserve stdout bytes for protocols
    /// whose paths may not be UTF-8 (for example porcelain and numstat).
    pub fn stdout_bytes<S: AsRef<OsStr>>(
        &self,
        cwd: &Path,
        args: &[S],
    ) -> Result<Vec<u8>, WorktreeError> {
        self.run_output(cwd, args, GitAccess::Read)
            .map(|output| output.stdout)
            .map_err(WorktreeError::GitFailure)
    }

    /// Working files eligible for a source snapshot: indexed paths, including
    /// tracked files matched by ignore rules, and ordinary untracked paths.
    /// Keep Git's NUL-delimited path bytes so unusual names remain exact.
    pub fn source_working_paths(
        &self,
        repo: &Path,
    ) -> Result<Vec<SourceWorkingPath>, WorktreeError> {
        use std::os::unix::ffi::OsStringExt;
        let mut selected = std::collections::BTreeMap::new();
        for (tracked, arguments) in [
            (true, &["ls-files", "--cached", "-z"][..]),
            (
                false,
                &["ls-files", "--others", "--exclude-standard", "-z"][..],
            ),
        ] {
            let bytes = self.stdout_bytes(repo, arguments)?;
            for path in bytes
                .split(|byte| *byte == 0)
                .filter(|path| !path.is_empty())
            {
                let path = PathBuf::from(OsString::from_vec(path.to_vec()));
                selected
                    .entry(path)
                    .and_modify(|old| *old |= tracked)
                    .or_insert(tracked);
            }
        }
        Ok(selected
            .into_iter()
            .map(|(path, tracked)| SourceWorkingPath { path, tracked })
            .collect())
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
        let _admission = self.write_scope(repo)?;
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
        let _admission = self.write_scope(repo)?;
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
            .read(cwd, &["rev-parse", "--absolute-git-dir"])
            .map_err(|_| WorktreeError::NotARepository(cwd.to_path_buf()))?;
        Ok(PathBuf::from(out.trimmed()))
    }

    /// Absolute path of the working tree root for `cwd`.
    ///
    /// A genuine non-repository is [`WorktreeError::NotARepository`] — a
    /// caller like the nesting guard reads that as "nothing to nest inside,
    /// safe to proceed". Any OTHER git failure (a corrupt `.git`, a
    /// permission error, anything `rev-parse` chokes on for a reason other
    /// than "there is no repository here") stays [`WorktreeError::GitFailure`]
    /// so it cannot be misread the same way — see [`is_not_a_repository`].
    pub fn work_tree(git: &GitCli, cwd: &Path) -> Result<PathBuf, WorktreeError> {
        let out = git
            .read(cwd, &["rev-parse", "--show-toplevel"])
            .map_err(|failure| {
                if is_not_a_repository(&failure) {
                    WorktreeError::NotARepository(cwd.to_path_buf())
                } else {
                    WorktreeError::GitFailure(failure)
                }
            })?;
        Ok(PathBuf::from(out.trimmed()))
    }

    /// Whether a failed `rev-parse` reports "no repository here" rather than
    /// some other git failure.
    ///
    /// Classified by git's own stable message text, never by exit code alone:
    /// a corrupt `.git` (e.g. an invalid gitfile pointer, or a `.git` that is
    /// a plain file with garbage content) also exits 128, and reading that as
    /// "not a repository" is exactly the fail-open bug this exists to close —
    /// it would make the never-dirty-the-source nesting guard skip itself on
    /// any git malfunction, not just a genuine absence of a repository. The
    /// message is locale-stable because [`GitCli::run_output`] pins
    /// `LC_ALL=C` on every invocation.
    fn is_not_a_repository(failure: &GitFailureReceipt) -> bool {
        failure.stderr.contains("fatal: not a git repository")
    }

    /// Absolute path of the shared Git metadata used by `cwd`.
    ///
    /// A linked worktree's `.git` is only a pointer file. Callers granting a
    /// process enough filesystem authority to commit must grant the resolved
    /// common directory, not assume that `<worktree>/.git` is a directory.
    pub fn git_common_dir(git: &GitCli, cwd: &Path) -> Result<PathBuf, WorktreeError> {
        let out = git
            .read(
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

        let out = git.try_read(
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

        let ignored_out = git.try_read(
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
    thread_local! {
        static BEFORE_ACQUIRE: std::cell::RefCell<Option<Box<dyn FnOnce()>>> =
            std::cell::RefCell::new(None);
    }

    pub(super) fn before_acquire() {
        let hook = BEFORE_ACQUIRE.with(|hook| hook.borrow_mut().take());
        if let Some(hook) = hook {
            hook();
        }
    }

    #[test]
    fn command_revalidates_repository_after_waiting_for_admission() {
        let first = crate::testing::TestRepo::init().unwrap();
        let second = crate::testing::TestRepo::init().unwrap();
        let root = tempfile::tempdir().unwrap();
        let alias = root.path().join("source");
        std::os::unix::fs::symlink(first.path(), &alias).unwrap();
        let git = first.git().clone();
        let writer = git.write_scope(&alias).unwrap();
        let (ready_tx, ready_rx) = std::sync::mpsc::channel();
        let (resume_tx, resume_rx) = std::sync::mpsc::channel();
        let reader_path = alias.clone();
        let reader = std::thread::spawn(move || {
            BEFORE_ACQUIRE.with(|hook| {
                *hook.borrow_mut() = Some(Box::new(move || {
                    ready_tx.send(()).unwrap();
                    resume_rx
                        .recv_timeout(super::Duration::from_secs(5))
                        .unwrap();
                }));
            });
            git.read(&reader_path, &["rev-parse", "--git-common-dir"])
        });
        ready_rx
            .recv_timeout(super::Duration::from_secs(5))
            .unwrap();
        let replacement = root.path().join("replacement");
        std::os::unix::fs::symlink(second.path(), &replacement).unwrap();
        std::fs::rename(&replacement, &alias).unwrap();
        drop(writer);
        resume_tx.send(()).unwrap();
        let failure = reader.join().unwrap().unwrap_err();
        assert!(failure.stderr.contains("changed while acquiring admission"));
    }

    #[test]
    fn cloned_clients_admit_concurrent_reads() {
        let repo = crate::testing::TestRepo::init().unwrap();
        let git = repo.git().clone();
        let _read = git.read_scope(repo.path()).unwrap();
        let reader = git.clone();
        let path = repo.path().to_owned();
        std::thread::spawn(move || {
            let _scope = reader
                .scope_within(&path, super::GitAccess::Read, super::Duration::ZERO)
                .unwrap()
                .expect("compatible read cannot wait on another client clone");
            reader
                .read(&path, &["rev-parse", "--git-common-dir"])
                .unwrap();
        })
        .join()
        .unwrap();
    }

    #[test]
    fn cloned_clients_do_not_exclude_unrelated_repositories() {
        let first = crate::testing::TestRepo::init().unwrap();
        let second = crate::testing::TestRepo::init().unwrap();
        let git = first.git().clone();
        let _writer = git.write_scope(first.path()).unwrap();
        let other = git.clone();
        let path = second.path().to_owned();
        std::thread::spawn(move || {
            let _scope = other
                .write_within(&path, super::Duration::ZERO)
                .unwrap()
                .expect("an unrelated backing has its own admission");
            other
                .try_run(&path, &["config", "test.admitted", "true"])
                .unwrap();
        })
        .join()
        .unwrap();
    }

    #[test]
    fn read_scope_refuses_write_promotion_but_retains_inherited_write_authority() {
        let repo = crate::testing::TestRepo::init().unwrap();
        let git = repo.git();
        let read = git.read_scope(repo.path()).unwrap();
        assert!(git
            .write_within(repo.path(), super::Duration::ZERO)
            .is_err());
        assert!(git.try_capture(repo.path()).is_err());
        assert!(git
            .try_run(repo.path(), &["config", "test.promoted", "true"])
            .is_err());
        git.read(repo.path(), &["rev-parse", "--git-common-dir"])
            .unwrap();
        drop(read);
        let _write = git.write_scope(repo.path()).unwrap();
        let _nested_read = git.read_scope(repo.path()).unwrap();
        git.try_run(repo.path(), &["config", "test.admitted", "true"])
            .unwrap();
    }

    #[test]
    fn cooperating_processes_share_repository_admission() {
        const CHILD_PATH: &str = "EXOMONAD_ADMISSION_TEST_PATH";
        const CHILD_EXPECTED: &str = "EXOMONAD_ADMISSION_TEST_EXPECTED";
        if let Some(path) = std::env::var_os(CHILD_PATH) {
            let git = super::GitCli::new();
            let path = std::path::PathBuf::from(path);
            let actual = [
                super::GitAccess::Read,
                super::GitAccess::Write,
                super::GitAccess::Capture,
            ]
            .map(|access| {
                let permit = git
                    .scope_within(&path, access, super::Duration::ZERO)
                    .unwrap();
                if permit.is_some() {
                    git.read(&path, &["rev-parse", "HEAD"]).unwrap();
                }
                permit.is_some()
            });
            let expected = match std::env::var(CHILD_EXPECTED).unwrap().as_str() {
                "open" => [true, true, true],
                "read-only" => [true, false, false],
                "blocked" => [false, false, false],
                other => panic!("unknown child expectation {other}"),
            };
            assert_eq!(actual, expected);
            return;
        }
        let repo = crate::testing::TestRepo::init().unwrap();
        repo.writer().commit_file("file", "seed", "seed").unwrap();
        let sibling_root = tempfile::tempdir().unwrap();
        let sibling = sibling_root.path().join("sibling");
        repo.git()
            .try_run(
                repo.path(),
                &[
                    "worktree",
                    "add",
                    "-q",
                    "-b",
                    "sibling",
                    sibling.to_str().unwrap(),
                ],
            )
            .unwrap();
        let probe = |expected| {
            let output = std::process::Command::new(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    "git::admission_tests::cooperating_processes_share_repository_admission",
                    "--nocapture",
                ])
                .env(CHILD_PATH, &sibling)
                .env(CHILD_EXPECTED, expected)
                .output()
                .unwrap();
            assert!(
                output.status.success(),
                "child failed: {}\n{}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
            assert!(
                String::from_utf8_lossy(&output.stdout).contains("1 passed"),
                "child selected no test"
            );
        };
        probe("open");
        let read = repo.git().read_scope(repo.path()).unwrap();
        probe("read-only");
        drop(read);
        let write = repo.git().write_scope(repo.path()).unwrap();
        probe("blocked");
        drop(write);
        let capture = repo.git().try_capture(repo.path()).unwrap().unwrap();
        probe("blocked");
        drop(capture);
        probe("open");
    }

    #[test]
    fn opposite_repository_orders_acquire_without_deadlock() {
        let first = crate::testing::TestRepo::init().unwrap();
        let second = crate::testing::TestRepo::init().unwrap();
        let start = std::sync::Arc::new(std::sync::Barrier::new(2));
        let threads = [false, true].map(|reverse| {
            let mut paths = [first.path().to_owned(), second.path().to_owned()];
            if reverse {
                paths.reverse();
            }
            let start = start.clone();
            std::thread::spawn(move || {
                let git = super::GitCli::new();
                start.wait();
                let _scope = git
                    .scope_many_within(
                        &[&paths[0], &paths[1]],
                        super::GitAccess::Write,
                        super::Duration::from_secs(5),
                    )
                    .unwrap()
                    .expect("opposite caller ordering cannot deadlock");
                for path in &paths {
                    git.try_run(path, &["config", "test.admitted", "true"])
                        .unwrap();
                }
            })
        });
        for thread in threads {
            thread.join().unwrap();
        }
    }

    #[test]
    fn nested_scope_retains_flock_when_outer_guard_drops_first() {
        let repo = crate::testing::TestRepo::init().unwrap();
        let git = super::GitCli::new();
        let outer = git.write_scope(repo.path()).unwrap();
        let inner = git.write_scope(repo.path()).unwrap();
        drop(outer);
        git.read(repo.path(), &["rev-parse", "--git-common-dir"])
            .unwrap();
        let path = repo.path().to_path_buf();
        assert!(std::thread::spawn(move || super::GitCli::new()
            .try_capture(&path)
            .unwrap()
            .is_none())
        .join()
        .unwrap());
        drop(inner);
        assert!(git.try_capture(repo.path()).unwrap().is_some());
    }

    #[test]
    fn active_capture_does_not_implicitly_admit_a_linked_worktree() {
        let repo = crate::testing::TestRepo::init().unwrap();
        repo.writer().commit_file("file", "seed", "seed").unwrap();
        let root = tempfile::tempdir().unwrap();
        let sibling = root.path().join("sibling");
        repo.git()
            .try_run(
                repo.path(),
                &[
                    "worktree",
                    "add",
                    "-q",
                    "-b",
                    "sibling",
                    sibling.to_str().unwrap(),
                ],
            )
            .unwrap();
        let git = super::GitCli::new();
        let capture = git.try_capture(repo.path()).unwrap().unwrap();
        let failure = git.read(&sibling, &["rev-parse", "HEAD"]).unwrap_err();
        assert!(failure
            .stderr
            .contains("not included in the active admission scope"));
        drop(capture);
        git.read(&sibling, &["rev-parse", "HEAD"]).unwrap();
    }

    #[test]
    fn active_write_scope_rejects_a_borrowed_gitfile_in_another_worktree() {
        let repo = crate::testing::TestRepo::init().unwrap();
        repo.writer().commit_file("file", "seed", "seed").unwrap();
        let other = tempfile::tempdir().unwrap();
        std::fs::write(
            other.path().join(".git"),
            format!("gitdir: {}\n", repo.path().join(".git").display()),
        )
        .unwrap();
        let git = super::GitCli::new();
        let _write = git.write_scope(repo.path()).unwrap();
        let failure = git
            .read(other.path(), &["rev-parse", "--show-toplevel"])
            .unwrap_err();
        assert!(failure
            .stderr
            .contains("not included in the active admission scope"));
    }

    #[test]
    fn scoped_repository_refuses_a_rewritten_linked_gitfile() {
        let repo = crate::testing::TestRepo::init().unwrap();
        repo.writer().commit_file("file", "seed", "seed").unwrap();
        let root = tempfile::tempdir().unwrap();
        let first = root.path().join("first");
        let second = root.path().join("second");
        for (branch, path) in [("first", &first), ("second", &second)] {
            repo.git()
                .try_run(
                    repo.path(),
                    &[
                        "worktree",
                        "add",
                        "-q",
                        "-b",
                        branch,
                        path.to_str().unwrap(),
                    ],
                )
                .unwrap();
        }
        let git = super::GitCli::new();
        let capture = git.try_capture(&first).unwrap().unwrap();
        let original = std::fs::read(first.join(".git")).unwrap();
        std::fs::write(
            first.join(".git"),
            std::fs::read(second.join(".git")).unwrap(),
        )
        .unwrap();
        let failure = git.read(&first, &["rev-parse", "HEAD"]).unwrap_err();
        assert!(failure
            .stderr
            .contains("Git backing or worktree view changed"));
        std::fs::write(first.join(".git"), original).unwrap();
        drop(capture);
    }

    #[test]
    fn scoped_repository_refuses_another_worktree_on_the_same_backing() {
        let repo = crate::testing::TestRepo::init().unwrap();
        repo.writer().commit_file("file", "seed", "seed").unwrap();
        let root = tempfile::tempdir().unwrap();
        let sibling = root.path().join("sibling");
        repo.git()
            .try_run(
                repo.path(),
                &[
                    "worktree",
                    "add",
                    "-q",
                    "-b",
                    "sibling",
                    sibling.to_str().unwrap(),
                ],
            )
            .unwrap();
        let alias = root.path().join("source");
        std::os::unix::fs::symlink(repo.path(), &alias).unwrap();
        let git = super::GitCli::new();
        let capture = git.try_capture(&alias).unwrap().unwrap();
        let replacement = root.path().join("replacement");
        std::os::unix::fs::symlink(&sibling, &replacement).unwrap();
        std::fs::rename(&replacement, &alias).unwrap();
        let failure = git.read(&alias, &["rev-parse", "HEAD"]).unwrap_err();
        assert!(failure
            .stderr
            .contains("Git backing or worktree view changed"));
        drop(capture);
    }

    #[test]
    fn scoped_repository_refuses_a_changed_lock_inode() {
        let repo = crate::testing::TestRepo::init().unwrap();
        let git = super::GitCli::new();
        let capture = git.try_capture(repo.path()).unwrap().unwrap();
        let directory = repo.path().join(".git/exomonad-admission");
        let replacement = directory.join("replacement");
        let file = std::fs::File::create(&replacement).unwrap();
        use std::os::unix::fs::PermissionsExt;
        file.set_permissions(std::fs::Permissions::from_mode(0o600))
            .unwrap();
        std::fs::rename(&replacement, directory.join("lock")).unwrap();
        let failure = git.read(repo.path(), &["rev-parse", "HEAD"]).unwrap_err();
        assert!(failure
            .stderr
            .contains("Git backing or worktree view changed"));
        drop(capture);
    }

    #[test]
    fn scoped_repository_refuses_a_retargeted_path() {
        let first = crate::testing::TestRepo::init().unwrap();
        let second = crate::testing::TestRepo::init().unwrap();
        let root = tempfile::tempdir().unwrap();
        let alias = root.path().join("source");
        std::os::unix::fs::symlink(first.path(), &alias).unwrap();
        let git = super::GitCli::new();
        let capture = git.try_capture(&alias).unwrap().unwrap();
        let replacement = root.path().join("replacement");
        std::os::unix::fs::symlink(second.path(), &replacement).unwrap();
        std::fs::rename(&replacement, &alias).unwrap();
        let failure = git
            .read(&alias, &["rev-parse", "--git-common-dir"])
            .unwrap_err();
        assert!(failure
            .stderr
            .contains("Git backing or worktree view changed"));
        drop(capture);
        git.read(&alias, &["rev-parse", "--git-common-dir"])
            .unwrap();
    }

    #[test]
    fn independent_clients_share_linked_worktree_backing() {
        let repo = crate::testing::TestRepo::init().unwrap();
        repo.writer().commit_file("file", "seed", "seed").unwrap();
        let sibling_root = tempfile::tempdir().unwrap();
        let sibling = sibling_root.path().join("sibling");
        repo.git()
            .try_run(
                repo.path(),
                &[
                    "worktree",
                    "add",
                    "-q",
                    "-b",
                    "sibling",
                    sibling.to_str().unwrap(),
                ],
            )
            .unwrap();
        let separate = super::GitCli::new();
        let capture = repo.git().try_capture(repo.path()).unwrap().unwrap();
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            std::fs::metadata(repo.path().join(".git/exomonad-admission"))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o700,
        );
        assert_eq!(
            std::fs::metadata(repo.path().join(".git/exomonad-admission/lock"))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o600,
        );
        let (observed, receiver) = std::sync::mpsc::channel();
        let (released, proceed) = std::sync::mpsc::channel();
        let worker = std::thread::spawn(move || {
            observed
                .send(separate.try_capture(&sibling).unwrap().is_none())
                .unwrap();
            proceed.recv().unwrap();
            separate.try_capture(&sibling).unwrap().is_some()
        });
        assert!(receiver.recv().unwrap());
        drop(capture);
        released.send(()).unwrap();
        assert!(worker.join().unwrap());
    }

    #[test]
    fn capture_within_waits_for_a_running_capture() {
        let repo = crate::testing::TestRepo::init().unwrap();
        let git = repo.git().clone();
        let holder = git.clone();
        let path = repo.path().to_owned();
        let (held, ready) = std::sync::mpsc::channel();
        let thread = std::thread::spawn(move || {
            let _capture = holder.try_capture(&path).unwrap().unwrap();
            held.send(()).unwrap();
            #[allow(clippy::disallowed_methods, reason = "sync test thread, not async")]
            std::thread::sleep(std::time::Duration::from_millis(200));
        });
        ready.recv().unwrap();
        assert!(git.try_capture(repo.path()).unwrap().is_none());
        assert!(git
            .capture_within(repo.path(), std::time::Duration::from_secs(10))
            .unwrap()
            .is_some());
        thread.join().unwrap();
    }

    #[test]
    fn capture_within_returns_none_when_its_finite_deadline_expires() {
        let repo = crate::testing::TestRepo::init().unwrap();
        let git = repo.git().clone();
        let capture = git.try_capture(repo.path()).unwrap().unwrap();
        let path = repo.path().to_owned();
        let timeout = std::time::Duration::from_millis(180);
        let outer_deadline = std::time::Duration::from_secs(5);
        let (started_tx, started_rx) = std::sync::mpsc::channel();
        let (finished_tx, finished_rx) = std::sync::mpsc::channel();
        let waiter_git = git.clone();
        let waiter_path = path.clone();
        let waiter = std::thread::spawn(move || {
            let started = std::time::Instant::now();
            started_tx.send(()).unwrap();
            let permit = waiter_git.capture_within(&waiter_path, timeout).unwrap();
            finished_tx
                .send((started.elapsed(), permit.is_some()))
                .unwrap();
        });

        started_rx.recv_timeout(outer_deadline).unwrap();
        let (elapsed, acquired) = finished_rx.recv_timeout(outer_deadline).unwrap();
        waiter.join().unwrap();
        assert!(!acquired, "a held capture must make the waiter time out");
        assert!(
            elapsed >= timeout,
            "finite admission returned before its deadline: {elapsed:?}"
        );

        drop(capture);
        assert!(git.try_capture(&path).unwrap().is_some());
    }

    #[test]
    fn source_capture_excludes_host_git_mutation_and_allows_its_own_reads() {
        let repo = crate::testing::TestRepo::init().unwrap();
        repo.writer().commit_file("file", "seed", "seed").unwrap();
        std::fs::write(repo.path().join("file"), "changed").unwrap();
        let git = repo.git().clone();
        let capture = git.try_capture(repo.path()).unwrap().unwrap();
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
            assert!(writer.try_capture(&path).unwrap().is_none());
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
