//! What `exomonad init` checks and prints before it starts anything: the
//! workspace revision the run will compile, the interactive Codex it will
//! launch, and the compile daemons it will run beside.
//!
//! Observation does the I/O; [`assess`] is the pure decision the tests drive
//! with synthetic observations.

use std::path::{Path, PathBuf};

use exomonad_worktree::GitCli;

use super::scaffold::DEFAULT_WORKSPACE_REV;

const WORKSPACE_MOUNT: &str = ".exomonad/workspace";
const SHORT: usize = 12;

/// How `exomonad init` treats its launch preflight.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum PreflightMode {
    /// Print the checks; warnings pass, failures stop the launch.
    #[default]
    Warn,
    /// Print the checks; warnings stop the launch too.
    Strict,
    /// Run no checks (`--no-preflight`).
    Skip,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Verdict {
    Ok,
    Warn,
    Fail,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PreflightLine {
    pub(crate) check: &'static str,
    pub(crate) verdict: Verdict,
    pub(crate) detail: String,
}

impl std::fmt::Display for PreflightLine {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let verdict = match self.verdict {
            Verdict::Ok => "ok",
            Verdict::Warn => "WARN",
            Verdict::Fail => "FAIL",
        };
        write!(
            f,
            "preflight {:<10} {verdict} {}",
            format!("{}:", self.check),
            self.detail
        )
    }
}

/// Order of the project's workspace revision relative to the binary's pin.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Recency {
    Same,
    PinnedNewer,
    ProjectNewer,
    Diverged,
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum WorkspaceRevision {
    /// The project has no `.exomonad/workspace`.
    Absent,
    /// `source` is `checkout` (the submodule HEAD, which the run compiles) or
    /// `gitlink` (recorded in the project, not checked out).
    Found {
        source: &'static str,
        revision: String,
        recency: Recency,
    },
    Unreadable(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum PersistentDaemon {
    Absent,
    /// A socket file with no daemon answering its preflight.
    Unreachable,
    Live {
        producer: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct LaunchObservation {
    pub(crate) pinned_workspace: String,
    pub(crate) workspace: WorkspaceRevision,
    /// Resolved executable and version, or the resolution error.
    pub(crate) codex: Result<(PathBuf, String), String>,
    /// The extractor the run's own daemon executes and its producer identity.
    pub(crate) extractor: Result<(PathBuf, String), String>,
    pub(crate) persistent_socket: PathBuf,
    pub(crate) persistent: PersistentDaemon,
}

impl LaunchObservation {
    pub(crate) fn observe(
        workspace: &Path,
        codex: Result<&exomonad_agent::InteractiveAgentInstallation, String>,
    ) -> Self {
        let persistent_socket = tidepool_toolchain::paths::persistent_compile_daemon_socket();
        let persistent = if !persistent_socket.exists() {
            PersistentDaemon::Absent
        } else {
            match tidepool_extract_cmd::preflight_compiler_daemon(&persistent_socket) {
                Ok(identity) => PersistentDaemon::Live {
                    producer: identity.producer_hex(),
                },
                Err(_) => PersistentDaemon::Unreachable,
            }
        };
        let extractor = tidepool_toolchain::toolchain::locate_extract()
            .map_err(|error| error.to_string())
            .and_then(|location| {
                tidepool_extract_cmd::ExtractCmd::with_bin(
                    tidepool_extract_cmd::ResolvedExtractBin::assume_resolved(&location.path),
                )
                .bind_direct()
                .map(|endpoint| (location.path, endpoint.identity().producer_hex()))
                .map_err(|error| error.to_string())
            });
        Self {
            pinned_workspace: DEFAULT_WORKSPACE_REV.to_owned(),
            workspace: observe_workspace(workspace, DEFAULT_WORKSPACE_REV),
            codex: codex.map(|agent| (agent.executable().to_owned(), agent.version().to_owned())),
            extractor,
            persistent_socket,
            persistent,
        }
    }
}

fn observe_workspace(project: &Path, pinned: &str) -> WorkspaceRevision {
    let git = GitCli::new();
    let mount = project.join(WORKSPACE_MOUNT);
    if !mount.exists() {
        return WorkspaceRevision::Absent;
    }
    // A submodule checkout carries its own `.git`; without one, `rev-parse`
    // would answer with the enclosing project's HEAD.
    let (source, revision) = if mount.join(".git").exists() {
        match git.run(&mount, &["rev-parse", "HEAD"]) {
            Ok(output) => ("checkout", output.trimmed().to_owned()),
            Err(error) => return WorkspaceRevision::Unreadable(error.to_string()),
        }
    } else {
        match git.run(project, &["ls-tree", "HEAD", WORKSPACE_MOUNT]) {
            Ok(output) => match output.stdout.split_whitespace().collect::<Vec<_>>()[..] {
                [_, "commit", revision, ..] => ("gitlink", revision.to_owned()),
                _ => return WorkspaceRevision::Absent,
            },
            Err(error) => return WorkspaceRevision::Unreadable(error.to_string()),
        }
    };
    let recency = if revision == pinned {
        Recency::Same
    } else if source != "checkout" {
        Recency::Unknown
    } else {
        let known = |rev: &str| {
            git.run(&mount, &["cat-file", "-e", &format!("{rev}^{{commit}}")])
                .is_ok()
        };
        let ancestor = |older: &str, newer: &str| {
            git.run(&mount, &["merge-base", "--is-ancestor", older, newer])
                .is_ok()
        };
        if !known(pinned) {
            Recency::Unknown
        } else if ancestor(pinned, &revision) {
            Recency::ProjectNewer
        } else if ancestor(&revision, pinned) {
            Recency::PinnedNewer
        } else {
            Recency::Diverged
        }
    };
    WorkspaceRevision::Found {
        source,
        revision,
        recency,
    }
}

fn short(revision: &str) -> &str {
    revision.get(..SHORT).unwrap_or(revision)
}

/// The launch decision: one line per check, warnings promoted to failures
/// under [`PreflightMode::Strict`].
pub(crate) fn assess(observation: &LaunchObservation, mode: PreflightMode) -> Vec<PreflightLine> {
    let pinned = short(&observation.pinned_workspace);
    let workspace = match &observation.workspace {
        WorkspaceRevision::Absent => (
            Verdict::Ok,
            format!("no {WORKSPACE_MOUNT}; nothing to compare with pinned {pinned}"),
        ),
        WorkspaceRevision::Unreadable(error) => (
            Verdict::Warn,
            format!("cannot read the {WORKSPACE_MOUNT} revision: {error}"),
        ),
        WorkspaceRevision::Found {
            source,
            revision,
            recency: Recency::Same,
        } => (
            Verdict::Ok,
            format!("{source} {} matches pinned {pinned}", short(revision)),
        ),
        WorkspaceRevision::Found {
            source,
            revision,
            recency,
        } => {
            let order = match recency {
                Recency::PinnedNewer => "; pinned is newer",
                Recency::ProjectNewer => "; project is newer",
                Recency::Diverged => "; they have diverged",
                Recency::Unknown | Recency::Same => "; order unknown",
            };
            (
                Verdict::Warn,
                format!(
                    "{source} {} differs from this binary's pinned {pinned}{order}; the run compiles the project's {WORKSPACE_MOUNT}",
                    short(revision)
                ),
            )
        }
    };
    let codex = match &observation.codex {
        Ok((executable, version)) => (
            Verdict::Ok,
            format!(
                "{} ({version}) from EXOMONAD_INTERACTIVE_CODEX_BIN",
                executable.display()
            ),
        ),
        Err(error) => (Verdict::Fail, error.clone()),
    };
    let socket = observation.persistent_socket.display();
    let compiler = match &observation.extractor {
        Err(error) => (Verdict::Fail, format!("no extractor for the run: {error}")),
        Ok((extractor, producer)) => {
            let run = format!(
                "per-run daemon from {} (producer {})",
                extractor.display(),
                short(producer)
            );
            match &observation.persistent {
                PersistentDaemon::Absent | PersistentDaemon::Unreachable => (
                    Verdict::Ok,
                    format!("{run}; no persistent daemon at {socket}"),
                ),
                PersistentDaemon::Live { producer: live } if live == producer => (
                    Verdict::Ok,
                    format!("{run}; persistent daemon at {socket} has the same producer"),
                ),
                PersistentDaemon::Live { producer: live } => (
                    Verdict::Warn,
                    format!(
                        "{run}; persistent daemon at {socket} is stale (producer {}) and keeps running beside it; `just daemon-stop` retires it",
                        short(live)
                    ),
                ),
            }
        }
    };
    [
        ("workspace", workspace),
        ("codex", codex),
        ("compiler", compiler),
    ]
    .into_iter()
    .map(|(check, (verdict, detail))| PreflightLine {
        check,
        verdict: match (verdict, mode) {
            (Verdict::Warn, PreflightMode::Strict) => Verdict::Fail,
            (verdict, _) => verdict,
        },
        detail,
    })
    .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    const PINNED: &str = "46de15cfe528de3e70ff7d65e1cba7654ecafd75";
    const PRODUCER: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";

    fn healthy() -> LaunchObservation {
        LaunchObservation {
            pinned_workspace: PINNED.into(),
            workspace: WorkspaceRevision::Found {
                source: "checkout",
                revision: PINNED.into(),
                recency: Recency::Same,
            },
            codex: Ok(("/nix/store/codex/bin/codex".into(), "codex-cli 1.0".into())),
            extractor: Ok(("/run/tidepool-extract".into(), PRODUCER.into())),
            persistent_socket: "/cache/battery-daemon/extract.sock".into(),
            persistent: PersistentDaemon::Live {
                producer: PRODUCER.into(),
            },
        }
    }

    fn verdicts(lines: &[PreflightLine]) -> Vec<Verdict> {
        lines.iter().map(|line| line.verdict).collect()
    }

    #[test]
    fn healthy_launch_passes_every_check() {
        let lines = assess(&healthy(), PreflightMode::Strict);
        assert_eq!(verdicts(&lines), [Verdict::Ok; 3]);
        assert_eq!(
            lines[0].to_string(),
            "preflight workspace: ok checkout 46de15cfe528 matches pinned 46de15cfe528"
        );
    }

    #[test]
    fn workspace_and_daemon_mismatches_warn_and_name_the_newer_side() {
        let mut observation = healthy();
        observation.workspace = WorkspaceRevision::Found {
            source: "checkout",
            revision: "a5bbb3b000000000000000000000000000000000".into(),
            recency: Recency::PinnedNewer,
        };
        observation.persistent = PersistentDaemon::Live {
            producer: "b".repeat(64),
        };
        let lines = assess(&observation, PreflightMode::Warn);
        assert_eq!(
            verdicts(&lines),
            [Verdict::Warn, Verdict::Ok, Verdict::Warn]
        );
        assert!(lines[0].detail.contains("pinned is newer"), "{}", lines[0]);
        assert!(lines[0].detail.contains("a5bbb3b00000"), "{}", lines[0]);
        assert!(lines[2].detail.contains("stale"), "{}", lines[2]);
    }

    #[test]
    fn missing_codex_or_extractor_fails_in_every_mode() {
        let mut observation = healthy();
        observation.codex =
            Err("EXOMONAD_INTERACTIVE_CODEX_BIN must explicitly name the pinned interactive Codex executable".into());
        observation.extractor = Err("tidepool-extract not found".into());
        let lines = assess(&observation, PreflightMode::Warn);
        assert_eq!(
            verdicts(&lines),
            [Verdict::Ok, Verdict::Fail, Verdict::Fail]
        );
        assert!(lines[1]
            .to_string()
            .starts_with("preflight codex:     FAIL"));
    }

    #[test]
    fn strict_mode_turns_warnings_into_failures() {
        let mut observation = healthy();
        observation.workspace = WorkspaceRevision::Found {
            source: "gitlink",
            revision: "a5bbb3b000000000000000000000000000000000".into(),
            recency: Recency::Unknown,
        };
        assert_eq!(
            verdicts(&assess(&observation, PreflightMode::Warn)),
            [Verdict::Warn, Verdict::Ok, Verdict::Ok]
        );
        assert_eq!(
            verdicts(&assess(&observation, PreflightMode::Strict)),
            [Verdict::Fail, Verdict::Ok, Verdict::Ok]
        );
    }

    #[test]
    fn absent_workspace_and_persistent_daemon_are_healthy() {
        let mut observation = healthy();
        observation.workspace = WorkspaceRevision::Absent;
        observation.persistent = PersistentDaemon::Unreachable;
        assert_eq!(
            verdicts(&assess(&observation, PreflightMode::Strict)),
            [Verdict::Ok; 3]
        );
    }

    /// The observation reads the checkout, not the enclosing project, and
    /// orders the two revisions with the checkout's own history.
    #[test]
    fn workspace_observation_orders_checkout_against_pin() {
        let root = tempfile::tempdir().unwrap();
        let project = root.path();
        let mount = project.join(WORKSPACE_MOUNT);
        std::fs::create_dir_all(&mount).unwrap();
        let git = GitCli::new();
        let commit = |message: &str| {
            git.try_run(
                &mount,
                &[
                    "-c",
                    "user.name=t",
                    "-c",
                    "user.email=t@t",
                    "commit",
                    "--allow-empty",
                    "-qm",
                    message,
                ],
            )
            .unwrap();
            git.try_run(&mount, &["rev-parse", "HEAD"])
                .unwrap()
                .trimmed()
                .to_owned()
        };
        git.try_run(&mount, &["init", "-q"]).unwrap();
        let old = commit("old");
        let new = commit("new");
        git.try_run(&mount, &["checkout", "-q", "--detach", &old])
            .unwrap();
        assert!(matches!(
            observe_workspace(project, &new),
            WorkspaceRevision::Found { source: "checkout", recency: Recency::PinnedNewer, revision } if revision == old
        ));
        assert!(matches!(
            observe_workspace(project, &old),
            WorkspaceRevision::Found {
                recency: Recency::Same,
                ..
            }
        ));
        assert!(matches!(
            observe_workspace(project, PINNED),
            WorkspaceRevision::Found {
                recency: Recency::Unknown,
                ..
            }
        ));
        assert_eq!(
            observe_workspace(&project.join("elsewhere"), PINNED),
            WorkspaceRevision::Absent
        );
    }
}
