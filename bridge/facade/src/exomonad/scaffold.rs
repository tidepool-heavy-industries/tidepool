//! The Exomonad package `exomonad new` writes into a project.
//!
//! Every file in it is embedded from one this repository already maintains for
//! its own use — `bridge/facade/build.rs` generates the table — so a scaffolded
//! workspace and the shipped example cannot drift apart. This module is the
//! only writer of a `.exomonad/config.toml`; every other command reads one.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use exomonad_worktree::GitCli;

include!(concat!(env!("OUT_DIR"), "/scaffold_package.rs"));

/// The configuration `exomonad new` writes: today's agent and research defaults,
/// plus the Haskell package — this workspace's own `.exomonad` as a source root,
/// and the Jev core the project's `flake.nix` pins.
const CONFIG: &str = r#"[defaults]
model = "gpt-6-sol"
effort = "medium"

[research]
default_depth = 1
maximum_depth = 8

[haskell]
source_roots = ["."]
modules = ["Project.Shell", "Project.Lookup"]
spec = "AgentSpec.agentSpec"

# jev-dsl is compiled from the revision `flake.nix` pins, not from a copy in
# this project. Only `core` is named: it is the JSON-polymorphic library, and
# `.exomonad/Jev/Operators.hs` in this package fixes its value type to Tidepool's
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
    let links = skill_links();
    let taken = package
        .iter()
        .map(|(path, _)| path.clone())
        .chain(links.iter().map(|(link, _)| link.clone()))
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
    for (link, destination) in &links {
        let absolute = workspace.join(link);
        if let Some(parent) = absolute.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::os::unix::fs::symlink(destination, &absolute)?;
        written.push(link.clone());
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

/// The links a client loads the workspace skills through, one per skill
/// directory the package carries. Relative, so they survive a copied or
/// renamed checkout.
fn skill_links() -> Vec<(PathBuf, PathBuf)> {
    skill_names()
        .into_iter()
        .map(|name| {
            (
                Path::new(".agents/skills").join(name),
                Path::new("../../.exomonad/skills").join(name),
            )
        })
        .collect()
}

/// The skill directories in the embedded package, derived from it rather than
/// listed beside it.
fn skill_names() -> BTreeSet<&'static str> {
    SCAFFOLD_PACKAGE
        .iter()
        .filter_map(|(path, _)| path.strip_prefix(".exomonad/skills/")?.split('/').next())
        .collect()
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
