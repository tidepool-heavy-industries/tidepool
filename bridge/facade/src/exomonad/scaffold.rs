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
// `exomonad new` pulls the source from DEFAULT_WORKSPACE_URL, but must install
// the commit this release compiled and checked, even if the remote advances.
pub(super) const DEFAULT_WORKSPACE_REV: &str = "c488a1559abb7b02f10b68f406c6ee0172816e78";

/// The configuration `exomonad new` writes. Its modules, recipes, model aliases
/// and prompt files match the shipped workspace; only repository-specific
/// settings stay out of a new project.
const CONFIG: &str = r#"[defaults]
model = "gpt-6-sol"
effort = "medium"

[models]
planner = "gpt-6-astra"
executor = "gpt-6-sol"
luna = "gpt-6-luna"

[research]
default_depth = 1
maximum_depth = 8

[haskell]
source_roots = [".", "workspace"]
modules = [
  "Project.Types", "Project.Actors", "Project.Work", "Project.Routing", "Project.Observe",
  "Project.Shell", "Project.Lookup", "Project.Reflex", "Project.Evidence", "Project.Contract",
  "Project.Investigate", "Project.Merge", "Project.Review", "Project.Search", "Project.History",
  "Project.Service", "Project.Repository",
]
spec = "AgentSpec.agentSpec"
checks = [
  "Project.RoutingChecks.routing",
  "Project.RoutingChecks.candidateHistory",
  "Project.RoutingChecks.notificationRetention",
  "Project.RoutingChecks.automaticReview",
  "Project.RoutingChecks.requestRecovery",
  "Project.RoutingChecks.declaredRepair",
  "Project.RoutingChecks.forwardingFailure",
  "Project.RoutingChecks.handlerCall",
  "Project.Checks.workbench",
  "Project.CollaborationChecks.collaboration",
  "Project.SkillChecks.skills",
  "Project.JevChecks.investigation",
  "Project.JevChecks.review",
  "Project.JevChecks.reflex",
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
                write!(formatter, "exomonad new would overwrite files that already exist, and wrote nothing:")?;
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
        #[allow(
            clippy::disallowed_methods,
            reason = "one-shot nix flake lock, like a git one-shot; not a long-lived child"
        )]
        let report = std::process::Command::new(&nix)
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
        git.try_run(workspace, &["init", "--quiet"])?;
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
    #[cfg(test)]
    let source_url = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../.exomonad/workspace")
        .canonicalize()?;
    #[cfg(not(test))]
    let source_url = std::path::PathBuf::from(DEFAULT_WORKSPACE_URL);
    let source_url = source_url.to_string_lossy();
    git.try_run(
        workspace,
        &[
            "-c",
            "protocol.file.allow=always",
            "submodule",
            "add",
            "--name",
            "exomonad-workspace",
            &source_url,
            ".exomonad/workspace",
        ],
    )?;
    git.try_run(
        &workspace.join(".exomonad/workspace"),
        &["checkout", "--detach", DEFAULT_WORKSPACE_REV],
    )?;
    git.try_run(workspace, &["add", "--", ".exomonad/workspace"])?;
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
    use super::DEFAULT_WORKSPACE_REV;
    use exomonad_worktree::GitCli;
    use std::path::Path;

    /// `exomonad new` installs the workspace commit this checkout compiled and
    /// checked: the pin above must be the `.exomonad/workspace` gitlink of the
    /// repository HEAD, or a project gets modules nobody here ran.
    #[test]
    fn default_workspace_rev_is_this_checkouts_workspace_gitlink() {
        let repository = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        let Ok(listing) = GitCli::new().run(&repository, &["ls-tree", "HEAD", ".exomonad/workspace"])
        else {
            eprintln!("skipped: not a git checkout");
            return;
        };
        // `ls-tree` prints mode, type, object, path.
        let gitlink = listing.stdout.split_whitespace().nth(2).unwrap_or_default();
        assert_eq!(
            gitlink, DEFAULT_WORKSPACE_REV,
            "DEFAULT_WORKSPACE_REV must match the .exomonad/workspace gitlink at HEAD"
        );
    }
}
