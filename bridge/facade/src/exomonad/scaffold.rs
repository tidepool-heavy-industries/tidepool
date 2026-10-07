//! The Exomonad package `exomonad new` writes into a project.
//!
//! Project configuration, the starter agent spec, prompts, and plans are
//! generated from the project template. Generic modules, checks
//! and skills arrive through the workspace submodule. This module is
//! the only writer of `.exomonad/config.toml`.

use std::path::{Path, PathBuf};

use exomonad_worktree::GitCli;

include!(concat!(env!("OUT_DIR"), "/scaffold_package.rs"));

// Keep this in step with this repository's .exomonad/workspace gitlink.
// Both the qualified Git bundle and DEFAULT_WORKSPACE_URL must install the
// commit this release compiled and checked, even if the remote advances.
pub(super) const DEFAULT_WORKSPACE_REV: &str = "266a8982b10aec632d735f3d9f7d5f030f597a1f";

/// The configuration `exomonad new` writes. Its modules, recipes, model aliases
/// and prompt files match the shipped workspace; only repository-specific
/// settings stay out of a new project.
const CONFIG: &str = r#"[defaults]
model = "gpt-6.1-sol"
effort = "medium"

[models]
planner = "gpt-6-astra"
executor = "gpt-6.1-sol"
luna = "gpt-6-luna"

[haskell]
source_roots = [".", "workspace"]
modules = [
  "Exomonad.Contrib.Types", "Exomonad.Contrib.Actors", "Project.Work", "Exomonad.Contrib.Routing", "Project.DecisionAnswers", "Project.Observe",
  "Project.Shell", "Project.Sift", "Project.Lookup", "Project.Reflex", "Project.Evidence",
  "Project.Investigate", "Exomonad.Contrib.Merge", "Project.ReviewPolicy", "Project.Search", "Project.History",
  "Project.FieldNotes", "Project.RebaseRouter", "Project.SupervisionProfiles",
  "Project.Service", "Project.Repository", "Exomonad.Contrib.CheckResults",
  "Exomonad.Contrib.PrepareContinue", "Exomonad.Contrib.RetainedEvidence", "Project.AssumptionWatch",
  "Project.ParallelInvestigate", "Project.SlowCommandWatch", "Project.Interview", "Project.WorkflowExamples",
  "Exomonad.Contrib.CheckPlan", "Project.TestEvidence", "Exomonad.Contrib.ReviewFlow",
  "Project.BaselineIncorporation", "Project.WorkflowReminders", "Project.WorkflowReminderExamples",
]
spec = "AgentSpec.agentSpec"
checks = [
  "Project.RecursiveWorkChecks.nestedRequests",
  "Project.RecursiveWorkChecks.revisedReview",
  "Project.DecisionAnswerChecks.routing",
  "Project.DecisionAnswerChecks.replay",
  "Project.CheckedReviewChecks.published",
  "Project.CheckedReviewChecks.continuation",
  "Project.CheckedReviewChecks.sourceMismatch",
  "Project.AutomationChecks.integration",
  "Project.CheckResultsChecks.completionRouting",
  "Project.RoutingChecks.routing",
  "Project.RoutingChecks.candidateHistory",
  "Project.RoutingChecks.notificationRetention",
  "Project.RoutingChecks.forwardingFailure",
  "Project.RoutingChecks.handlerCall",
  "Project.CollaborationChecks.collaboration",
  "Project.SkillChecks.skills",
  "Project.SkillChecks.reviewProvenance",
  "Project.JevChecks.investigation",
  "Project.JevChecks.reflex",
  "Project.RebaseRouterChecks.agentRef",
  "Project.RebaseRouterChecks.facts",
  "Project.FieldNotesChecks.policy",
  "Project.ReviewFlowChecks.lateCandidate",
  "Project.ReviewFlowChecks.replacement",
  "Project.ReviewFlowChecks.sourcePreflight",
  "Project.ReviewFlowChecks.emptyFindings",
  "Project.CheckedReviewChecks.selectionMismatch",
  "Project.CheckedReviewChecks.duplicateSelection",
  "Project.MergeChecks.greenReceipt",
  "Project.MergeChecks.commandFailure",
  "Project.MergeChecks.checkedHeadChanged",
  "Project.MergeChecks.dirtyAfterSuccess",
  "Project.CheckResultsChecks.preparedCompletion",
  "Project.CheckResultsChecks.managedEvidence",
  "Project.CheckResultsChecks.runningCommandCleanup",
]

# jev-dsl is compiled from the revision `flake.nix` pins, not from a copy in
# this project. Only `core` is named: it is the JSON-polymorphic library, and
# `.exomonad/workspace/Jev/Operators.hs` fixes its value type to Tidepool's
# own. The input's `src` holds the same front over aeson, which Tidepool does
# not have; naming it here would put a second `Jev.Operators` on the search
# path that cannot compile.
#
# `Jev.Operators` is deliberately absent from `[haskell] modules`: that list is
# imported unqualified into every cell, and this surface reaches a cell as `J`
# through the workbench, which offers it wherever this package supplies the
# module.
[haskell.flake_sources]
jev-dsl = ["core"]

[prompts.files]
coordinator = "prompts/coordinator.md"
planner = "prompts/planner.md"
lead = "prompts/lead.md"
specialist = "prompts/specialist.md"
task = "prompts/task.md"
review = "prompts/review.md"
repair = "prompts/repair.md"
incorporate = "prompts/incorporate.md"
rsi = "prompts/rsi.md"
"#;

/// Why `exomonad new` will not scaffold a path. Each variant leaves the path
/// exactly as it found it.
#[derive(Debug)]
pub enum NewRefusal {
    /// The path already carries an Exomonad workspace.
    AlreadyAWorkspace(PathBuf),
    /// A non-empty path that is not the root of a Git work tree.
    NotARepositoryRoot(PathBuf),
    /// Files the package would write already exist.
    Occupied(Vec<PathBuf>),
}

impl std::fmt::Display for NewRefusal {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::AlreadyAWorkspace(config) => write!(
                formatter,
                "{} already exists, so this is already an Exomonad workspace; exomonad new will not overwrite one",
                config.display()
            ),
            Self::NotARepositoryRoot(path) => write!(
                formatter,
                "exomonad new takes an empty directory or the root of a Git repository, and {} is neither",
                path.display()
            ),
            Self::Occupied(paths) => {
                write!(
                    formatter,
                    "exomonad new would overwrite files that already exist, and wrote nothing:"
                )?;
                for path in paths {
                    write!(formatter, "\n  {}", path.display())?;
                }
                Ok(())
            }
        }
    }
}

impl std::error::Error for NewRefusal {}

/// What `exomonad new` found at its target path.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Target {
    /// Nothing, or an empty directory: `exomonad new` creates the repository and
    /// commits the package.
    Fresh,
    /// The root of a Git work tree that carries no Exomonad package: the package
    /// is written and staged, and the project owns the commit.
    Repository,
}

/// Produces `flake.lock` for the `flake.nix` `exomonad new` writes.
///
/// Locking reaches the network through `nix`. It is a separate step so that a
/// machine with neither still ends up with a complete package it can lock
/// later, and so the scaffolding can be exercised without either.
pub trait FlakeLock {
    fn lock(&self, workspace: &Path) -> Result<(), Box<dyn std::error::Error>>;
}

/// `nix flake lock`, through the same `nix` a run fetches its pinned inputs
/// with.
pub struct NixLock;

impl FlakeLock for NixLock {
    fn lock(&self, workspace: &Path) -> Result<(), Box<dyn std::error::Error>> {
        let nix = super::workspace::nix_bin();
        let report = super::workspace::nix_command()
            .arg("--extra-experimental-features")
            .arg("nix-command flakes")
            .args(["flake", "lock"])
            .arg(workspace)
            .output()
            .map_err(|error| {
                format!(
                    "cannot start {} to lock the project's flake: {error}",
                    nix.display()
                )
            })?;
        if report.status.success() {
            return Ok(());
        }
        Err(format!(
            "{} flake lock failed ({}): {}",
            nix.display(),
            report.status,
            String::from_utf8_lossy(&report.stderr).trim()
        )
        .into())
    }
}

/// What the scaffolding could do about the pinned Jev source.
pub(super) enum JevPin {
    /// `flake.nix` was written and locked: the next run compiles Jev.
    Locked,
    /// `flake.nix` was written; locking it did not succeed.
    Unlocked(Box<dyn std::error::Error>),
    /// The project brought its own `flake.nix`, which `exomonad new` never edits.
    ProjectFlake,
}

/// One scaffolded package.
pub(super) struct Scaffolded {
    pub(super) target: Target,
    /// Workspace-relative paths written and staged, in write order.
    pub(super) written: Vec<PathBuf>,
    pub(super) jev: JevPin,
}

/// Write the package into `workspace`, stage it, and pin Jev.
///
/// Nothing is written until every target has been checked, so a refusal leaves
/// the project untouched. Staging precedes locking because `nix` reads a Git
/// tree's tracked files: an unstaged `flake.nix` is a file the lock step cannot
/// see.
pub(super) fn scaffold(
    workspace: &Path,
    lock: &dyn FlakeLock,
) -> Result<Scaffolded, Box<dyn std::error::Error>> {
    let git = GitCli::new();
    let target = classify(&git, workspace)?;
    let package = package_files();
    let taken = package
        .iter()
        .map(|(path, _)| path.clone())
        .chain([
            PathBuf::from(".agents/skills"),
            PathBuf::from(".exomonad/workspace"),
        ])
        .filter(|path| workspace.join(path).symlink_metadata().is_ok())
        .collect::<Vec<_>>();
    if !taken.is_empty() {
        return Err(NewRefusal::Occupied(taken).into());
    }

    if target == Target::Fresh {
        std::fs::create_dir_all(workspace)?;
        git.init_repository(workspace, &["--quiet"])?;
    }
    // Runtime state is excluded through Git metadata, so the authored package
    // beside it stays an ordinary tracked directory.
    git.ensure_exomonad_local_exclude(workspace)?;
    let state = workspace.join(".exomonad");
    std::fs::create_dir_all(state.join("logs"))?;
    std::fs::create_dir_all(state.join("sessions"))?;

    let mut written = Vec::new();
    for (path, contents) in &package {
        let absolute = workspace.join(path);
        if let Some(parent) = absolute.parent() {
            std::fs::create_dir_all(parent)?;
        }
        tidepool_atomic_write::write_durable(&absolute, contents.as_bytes())?;
        written.push(path.clone());
    }
    add_default_workspace(&git, workspace)?;
    written.push(PathBuf::from(".gitmodules"));
    written.push(PathBuf::from(".exomonad/workspace"));

    for (link, destination) in skill_links(workspace)? {
        let absolute = workspace.join(&link);
        if let Some(parent) = absolute.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::os::unix::fs::symlink(destination, &absolute)?;
        written.push(link);
    }

    let flake = PathBuf::from("flake.nix");
    let brings_own_flake = workspace.join(&flake).symlink_metadata().is_ok();
    if !brings_own_flake {
        tidepool_atomic_write::write_durable(&workspace.join(&flake), flake_nix().as_bytes())?;
        written.push(flake);
    }
    stage(&git, workspace, &written)?;

    let jev = if brings_own_flake {
        JevPin::ProjectFlake
    } else {
        match lock.lock(workspace) {
            Ok(()) => {
                let lock_file = PathBuf::from("flake.lock");
                if workspace.join(&lock_file).is_file() {
                    stage(&git, workspace, std::slice::from_ref(&lock_file))?;
                    written.push(lock_file);
                }
                JevPin::Locked
            }
            Err(error) => JevPin::Unlocked(error),
        }
    };

    if target == Target::Fresh {
        git.try_run(
            workspace,
            &[
                "-c",
                "user.name=Exomonad",
                "-c",
                "user.email=exomonad@localhost",
                "commit",
                "--quiet",
                "-m",
                "Initialize Exomonad workspace",
            ],
        )?;
    }

    Ok(Scaffolded {
        target,
        written,
        jev,
    })
}

fn stage(
    git: &GitCli,
    workspace: &Path,
    paths: &[PathBuf],
) -> Result<(), Box<dyn std::error::Error>> {
    let mut arguments = vec![std::ffi::OsString::from("add"), "--".into()];
    arguments.extend(paths.iter().map(std::ffi::OsString::from));
    git.try_run(workspace, &arguments)?;
    Ok(())
}

/// Decide what `exomonad new` is being pointed at, refusing anything it would
/// have to overwrite or guess about.
fn classify(git: &GitCli, workspace: &Path) -> Result<Target, NewRefusal> {
    let config = workspace.join(super::EXOMONAD_CONFIG);
    if config.symlink_metadata().is_ok() {
        return Err(NewRefusal::AlreadyAWorkspace(config));
    }
    let empty = match std::fs::read_dir(workspace) {
        Ok(mut entries) => entries.next().is_none(),
        // A path with no listable directory holds nothing to preserve.
        // Creating it reports whatever else is wrong with it.
        Err(_) => return Ok(Target::Fresh),
    };
    if empty {
        return Ok(Target::Fresh);
    }
    let root = exomonad_worktree::git::inspect::work_tree(git, workspace)
        .map_err(|_| NewRefusal::NotARepositoryRoot(workspace.to_path_buf()))?;
    let same = std::fs::canonicalize(&root).ok() == std::fs::canonicalize(workspace).ok();
    same.then_some(Target::Repository)
        .ok_or_else(|| NewRefusal::NotARepositoryRoot(workspace.to_path_buf()))
}

/// The package's files, workspace-relative, in write order.
fn package_files() -> Vec<(PathBuf, &'static str)> {
    std::iter::once((PathBuf::from(super::EXOMONAD_CONFIG), CONFIG))
        .chain(
            SCAFFOLD_PACKAGE
                .iter()
                .map(|(path, contents)| (PathBuf::from(path), *contents)),
        )
        .collect()
}

/// Client skill links into the default workspace submodule. Relative links
/// survive a copied or renamed checkout.
fn skill_links(workspace: &Path) -> Result<Vec<(PathBuf, PathBuf)>, Box<dyn std::error::Error>> {
    let skills = workspace.join(".exomonad/workspace/skills");
    let mut links = Vec::new();
    for entry in std::fs::read_dir(skills)? {
        let entry = entry?;
        if entry.file_type()?.is_dir() {
            let name = entry.file_name();
            links.push((
                Path::new(".agents/skills").join(&name),
                Path::new("../../.exomonad/workspace/skills").join(name),
            ));
        }
    }
    links.sort();
    Ok(links)
}

fn add_default_workspace(git: &GitCli, workspace: &Path) -> Result<(), Box<dyn std::error::Error>> {
    let bundle = std::env::var_os("EXOMONAD_WORKSPACE_GIT_BUNDLE").map(PathBuf::from);
    let source_url = workspace_source_url(git, workspace, bundle.as_deref())?;
    let mut arguments = Vec::new();
    if bundle.is_some() {
        // File transport is admitted only for the verified declared bundle.
        arguments.extend(["-c", "protocol.file.allow=always"]);
    }
    arguments.extend([
        "submodule",
        "add",
        "--name",
        "exomonad-workspace",
        &source_url,
        ".exomonad/workspace",
    ]);
    git.try_run(workspace, &arguments)?;
    git.try_run(
        &workspace.join(".exomonad/workspace"),
        &["checkout", "--detach", DEFAULT_WORKSPACE_REV],
    )?;
    if bundle.is_some() {
        // New projects retain the public upstream URL after the offline clone.
        git.try_run(
            workspace,
            &[
                "config",
                "-f",
                ".gitmodules",
                "submodule.exomonad-workspace.url",
                DEFAULT_WORKSPACE_URL,
            ],
        )?;
        git.try_run(
            workspace,
            &["submodule", "sync", "--", ".exomonad/workspace"],
        )?;
    }
    git.try_run(workspace, &["add", "--", ".exomonad/workspace"])?;
    Ok(())
}

fn workspace_source_url(
    git: &GitCli,
    workspace: &Path,
    bundle: Option<&Path>,
) -> Result<String, Box<dyn std::error::Error>> {
    match bundle {
        Some(bundle) => {
            let bundle = bundle.canonicalize()?;
            verify_workspace_bundle(git, workspace, &bundle, DEFAULT_WORKSPACE_REV)?;
            Ok(bundle.to_string_lossy().into_owned())
        }
        None => Ok(DEFAULT_WORKSPACE_URL.to_owned()),
    }
}

fn verify_workspace_bundle(
    git: &GitCli,
    workspace: &Path,
    bundle: &Path,
    expected_revision: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    let bundle = bundle.to_string_lossy();
    let heads = git.try_read(workspace, &["bundle", "list-heads", &bundle])?;
    if heads.trimmed()
        != format!("{expected_revision} HEAD\n{expected_revision} refs/heads/workspace")
    {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "declared workspace Git bundle differs from the release's pinned commit",
        )
        .into());
    }
    git.try_read(workspace, &["bundle", "verify", &bundle])?;
    Ok(())
}

fn flake_nix() -> String {
    format!(
        r#"{{
  description = "Haskell this workspace compiles but does not carry: jev-dsl, pinned.";

  inputs.jev-dsl = {{
    url = "{JEV_DSL_URL}";
    flake = false;
  }};

  # Nothing is built from here. `[haskell.flake_sources]` in `.exomonad/config.toml`
  # names the directories inside the input that hold modules, and Exomonad captures
  # them into the run as ordinary source roots.
  outputs = {{ ... }}: {{ }};
}}
"#
    )
}

/// What a project is missing when the flake `exomonad new` wrote could not be
/// locked. The pin stays on disk, so finishing it is one command.
pub(super) fn unlocked_message(workspace: &Path, error: &dyn std::error::Error) -> String {
    format!(
        "flake.nix is written but not locked: {error}\nJev is unavailable in this workspace \
         until `nix flake lock {}` succeeds.\n",
        workspace.display()
    )
}

/// What to add to a project that brought its own `flake.nix`, which
/// `exomonad new` does not edit.
pub(super) fn project_flake_hint(workspace: &Path) -> String {
    format!(
        "This project has its own flake.nix, which exomonad new does not edit. Jev is \
         unavailable in this workspace until you add the pinned input\n\n  \
         inputs.jev-dsl = {{ url = \"{JEV_DSL_URL}\"; flake = false; }};\n\nand lock it:\n\n  \
         nix flake lock {}\n",
        workspace.display()
    )
}

#[cfg(test)]
mod tests {
    use super::{
        add_default_workspace, verify_workspace_bundle, workspace_source_url,
        DEFAULT_WORKSPACE_REV, DEFAULT_WORKSPACE_URL,
    };
    use exomonad_worktree::GitCli;

    #[test]
    fn declared_workspace_bundle_scaffolds_the_original_pin_and_public_upstream() {
        assert!(
            std::env::var_os("EXOMONAD_WORKSPACE_GIT_BUNDLE").is_some(),
            "native scaffold proof requires its declared bundle"
        );
        let workspace = tempfile::tempdir().expect("temporary workspace");
        let git = GitCli::default();
        git.init_repository(workspace.path(), &["--quiet"])
            .expect("initialize workspace");
        add_default_workspace(&git, workspace.path())
            .expect("scaffold from original pinned objects");
        let source = workspace.path().join(".exomonad/workspace");
        assert_eq!(
            git.try_read(&source, &["rev-parse", "HEAD"])
                .expect("workspace commit")
                .trimmed(),
            DEFAULT_WORKSPACE_REV
        );
        assert_eq!(
            git.try_read(
                workspace.path(),
                &[
                    "config",
                    "-f",
                    ".gitmodules",
                    "submodule.exomonad-workspace.url"
                ]
            )
            .expect("public source URL")
            .trimmed(),
            DEFAULT_WORKSPACE_URL
        );
        assert_eq!(
            git.try_read(&source, &["remote", "get-url", "origin"])
                .expect("public origin")
                .trimmed(),
            DEFAULT_WORKSPACE_URL
        );
    }

    #[test]
    fn missing_declared_workspace_bundle_refuses_instead_of_using_remote() {
        let workspace = tempfile::tempdir().expect("temporary workspace");
        let error = workspace_source_url(
            &GitCli::default(),
            workspace.path(),
            Some(&workspace.path().join("missing.bundle")),
        )
        .expect_err("a declared resource must exist");
        assert_eq!(
            error
                .downcast_ref::<std::io::Error>()
                .expect("missing file error")
                .kind(),
            std::io::ErrorKind::NotFound
        );
        assert!(!workspace.path().join(".gitmodules").exists());
    }

    #[test]
    fn declared_workspace_bundle_refuses_a_different_gitlink() {
        let workspace = tempfile::tempdir().expect("temporary workspace");
        let git = GitCli::default();
        git.init_repository(workspace.path(), &["--quiet"])
            .expect("initialize workspace");
        let bundle = std::env::var_os("EXOMONAD_WORKSPACE_GIT_BUNDLE")
            .map(std::path::PathBuf::from)
            .expect("declared native workspace bundle");
        verify_workspace_bundle(&git, workspace.path(), &bundle, DEFAULT_WORKSPACE_REV)
            .expect("the declared resource contains the real pinned commit");
        let error = verify_workspace_bundle(&git, workspace.path(), &bundle, &"0".repeat(40))
            .expect_err("a different pin must refuse");
        assert_eq!(
            error
                .downcast_ref::<std::io::Error>()
                .expect("pin mismatch error")
                .kind(),
            std::io::ErrorKind::InvalidData
        );
        assert!(!workspace.path().join(".gitmodules").exists());
    }

    /// The source generator verifies this declared descriptor against the
    /// source index. Test execution does not require a Git checkout.
    #[test]
    fn default_workspace_rev_is_this_checkouts_workspace_gitlink() {
        #[derive(serde::Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Gitlink {
            schema: u32,
            path: String,
            mode: String,
            revision: String,
        }

        let descriptor = std::env::var_os("EXOMONAD_WORKSPACE_GITLINK")
            .map(std::path::PathBuf::from)
            .expect(
                "workspace gitlink proof requires its declared EXOMONAD_WORKSPACE_GITLINK input",
            );
        let gitlink: Gitlink = serde_json::from_slice(
            &std::fs::read(descriptor).expect("read the declared workspace gitlink descriptor"),
        )
        .expect("decode the declared workspace gitlink descriptor");
        assert_eq!(gitlink.schema, 1);
        assert_eq!(gitlink.path, ".exomonad/workspace");
        assert_eq!(gitlink.mode, "160000");
        assert_eq!(gitlink.revision.len(), 40);
        assert!(gitlink
            .revision
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit()));
        assert_eq!(
            gitlink.revision, DEFAULT_WORKSPACE_REV,
            "DEFAULT_WORKSPACE_REV must match the declared .exomonad/workspace gitlink"
        );
    }
}
