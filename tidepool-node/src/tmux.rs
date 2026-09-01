//! Exact tmux-pane ownership for interactive actor nodes.
//!
//! Tmux is a deployment and observability adapter here, never a message
//! transport. Model input goes through the backend's supported push channel.

use std::collections::{BTreeMap, HashSet};
use std::path::PathBuf;

use tokio::process::Command;

/// Stable tmux pane identity, immune to window names and base-index changes.
#[derive(Debug, Clone, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct TmuxPaneId(String);

impl TmuxPaneId {
    pub fn parse(value: impl Into<String>) -> Result<Self, TmuxNodeError> {
        let value = value.into();
        let suffix = value.strip_prefix('%').unwrap_or_default();
        if !suffix.is_empty() && suffix.bytes().all(|byte| byte.is_ascii_digit()) {
            Ok(Self(value))
        } else {
            Err(TmuxNodeError::InvalidPaneId(value))
        }
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl TryFrom<String> for TmuxPaneId {
    type Error = TmuxNodeError;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        Self::parse(value)
    }
}

impl From<TmuxPaneId> for String {
    fn from(value: TmuxPaneId) -> Self {
        value.0
    }
}

/// One actor-node process launched in a fresh tmux window.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TmuxLaunch {
    pub window_name: String,
    pub cwd: PathBuf,
    pub program: String,
    pub args: Vec<String>,
    /// Values are passed with tmux's `-e`, never interpolated into the shell
    /// command that starts the node host.
    pub environment: BTreeMap<String, String>,
}

/// A tmux session selected by name and optional dedicated server socket.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TmuxSession {
    name: String,
    socket: Option<String>,
}

impl TmuxSession {
    #[must_use]
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            socket: None,
        }
    }

    #[must_use]
    pub fn with_socket(name: impl Into<String>, socket: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            socket: Some(socket.into()),
        }
    }

    /// Ensure the named tmux session exists before actor windows are added.
    pub async fn ensure(&self) -> Result<(), TmuxNodeError> {
        let status = self
            .command()
            .args(["has-session", "-t", &self.name])
            .status()
            .await
            .map_err(|source| TmuxNodeError::Io {
                operation: "has-session",
                source,
            })?;
        if status.success() {
            return Ok(());
        }
        let output = self
            .command()
            .args(["new-session", "-d", "-s", &self.name])
            .output()
            .await
            .map_err(|source| TmuxNodeError::Io {
                operation: "new-session",
                source,
            })?;
        if output.status.success() {
            Ok(())
        } else {
            // Another creator may have won the race between `has-session`
            // and `new-session`; recheck before reporting failure.
            let raced = self
                .command()
                .args(["has-session", "-t", &self.name])
                .status()
                .await
                .map_err(|source| TmuxNodeError::Io {
                    operation: "has-session",
                    source,
                })?;
            if raced.success() {
                Ok(())
            } else {
                Err(TmuxNodeError::Command {
                    operation: "new-session",
                    status: output.status,
                    stderr: String::from_utf8_lossy(&output.stderr).trim().to_string(),
                })
            }
        }
    }

    pub async fn spawn_window(&self, launch: &TmuxLaunch) -> Result<TmuxPaneId, TmuxNodeError> {
        validate_launch(launch)?;
        let output = self
            .spawn_window_command(launch)
            .output()
            .await
            .map_err(|source| TmuxNodeError::Io {
                operation: "new-window",
                source,
            })?;
        if !output.status.success() {
            return Err(TmuxNodeError::Command {
                operation: "new-window",
                status: output.status,
                stderr: String::from_utf8_lossy(&output.stderr).trim().to_string(),
            });
        }
        TmuxPaneId::parse(String::from_utf8_lossy(&output.stdout).trim())
    }

    pub async fn kill_pane(&self, pane: &TmuxPaneId) -> Result<(), TmuxNodeError> {
        let output = self
            .command()
            .args(["kill-pane", "-t", pane.as_str()])
            .output()
            .await
            .map_err(|source| TmuxNodeError::Io {
                operation: "kill-pane",
                source,
            })?;
        if output.status.success() {
            Ok(())
        } else {
            // Exact-pane teardown is idempotent: a process that already
            // exited may have removed its pane before the owner reaps it.
            if !self.list_panes().await?.contains(pane) {
                Ok(())
            } else {
                Err(TmuxNodeError::Command {
                    operation: "kill-pane",
                    status: output.status,
                    stderr: String::from_utf8_lossy(&output.stderr).trim().to_string(),
                })
            }
        }
    }

    pub async fn list_panes(&self) -> Result<HashSet<TmuxPaneId>, TmuxNodeError> {
        let output = self
            .command()
            .args(["list-panes", "-a", "-F", "#{pane_id}"])
            .output()
            .await
            .map_err(|source| TmuxNodeError::Io {
                operation: "list-panes",
                source,
            })?;
        if !output.status.success() {
            return Err(TmuxNodeError::Command {
                operation: "list-panes",
                status: output.status,
                stderr: String::from_utf8_lossy(&output.stderr).trim().to_string(),
            });
        }
        String::from_utf8_lossy(&output.stdout)
            .lines()
            .filter(|line| !line.trim().is_empty())
            .map(|line| TmuxPaneId::parse(line.trim()))
            .collect()
    }

    fn command(&self) -> Command {
        let mut command = Command::new("tmux");
        if let Some(socket) = &self.socket {
            command.arg("-L").arg(socket);
        }
        command
    }

    fn spawn_window_command(&self, launch: &TmuxLaunch) -> Command {
        let mut command = self.command();
        command
            .arg("new-window")
            .arg("-d")
            .arg("-t")
            .arg(&self.name)
            .arg("-n")
            .arg(&launch.window_name)
            .arg("-c")
            .arg(&launch.cwd);
        for (name, value) in &launch.environment {
            command.arg("-e").arg(format!("{name}={value}"));
        }
        command
            .arg("-P")
            .arg("-F")
            .arg("#{pane_id}")
            .arg(render_shell_command(&launch.program, &launch.args));
        command
    }
}

fn validate_launch(launch: &TmuxLaunch) -> Result<(), TmuxNodeError> {
    if launch.program.is_empty() || launch.program.as_bytes().contains(&0) {
        return Err(TmuxNodeError::InvalidProgram);
    }
    for name in launch.environment.keys() {
        if name.is_empty()
            || !name
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
            || name.as_bytes()[0].is_ascii_digit()
        {
            return Err(TmuxNodeError::InvalidEnvironmentName(name.clone()));
        }
    }
    Ok(())
}

fn render_shell_command(program: &str, args: &[String]) -> String {
    std::iter::once(program)
        .chain(args.iter().map(String::as_str))
        .map(quote_shell_word)
        .collect::<Vec<_>>()
        .join(" ")
}

fn quote_shell_word(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}

#[derive(Debug, thiserror::Error)]
pub enum TmuxNodeError {
    #[error("invalid tmux pane id {0:?}")]
    InvalidPaneId(String),
    #[error("actor-node program is empty or contains NUL")]
    InvalidProgram,
    #[error("invalid actor-node environment name {0:?}")]
    InvalidEnvironmentName(String),
    #[error("tmux {operation} could not run: {source}")]
    Io {
        operation: &'static str,
        source: std::io::Error,
    },
    #[error("tmux {operation} failed ({status}): {stderr}")]
    Command {
        operation: &'static str,
        status: std::process::ExitStatus,
        stderr: String,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pane_identity_is_exact_and_serde_checked() {
        let pane = TmuxPaneId::parse("%42").expect("valid pane");
        assert_eq!(serde_json::to_string(&pane).unwrap(), r#""%42""#);
        assert_eq!(
            serde_json::from_str::<TmuxPaneId>(r#""%7""#)
                .unwrap()
                .as_str(),
            "%7"
        );
        assert!(TmuxPaneId::parse("42").is_err());
        assert!(serde_json::from_str::<TmuxPaneId>(r#""%bad""#).is_err());
    }

    #[test]
    fn launch_uses_tmux_environment_and_quotes_only_the_constant_command() {
        let launch = TmuxLaunch {
            window_name: "🤖 actor.1".into(),
            cwd: PathBuf::from("/tmp/work tree"),
            program: "/tmp/tidepool node".into(),
            args: vec!["host".into(), "apostrophe's".into()],
            environment: BTreeMap::from([
                ("TIDEPOOL_ACTOR".into(), "1:2 with spaces".into()),
                ("TOKEN".into(), "not shell-expanded: $HOME".into()),
            ]),
        };
        validate_launch(&launch).unwrap();
        let session = TmuxSession::with_socket("swarm", "sock");
        let command = session.spawn_window_command(&launch);
        let args = command
            .as_std()
            .get_args()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect::<Vec<_>>();
        assert_eq!(&args[..2], ["-L", "sock"]);
        assert!(args
            .windows(2)
            .any(|pair| pair == ["-e", "TIDEPOOL_ACTOR=1:2 with spaces"]));
        assert!(args
            .windows(2)
            .any(|pair| pair == ["-e", "TOKEN=not shell-expanded: $HOME"]));
        assert_eq!(
            args.last().unwrap(),
            "'/tmp/tidepool node' 'host' 'apostrophe'\\''s'"
        );
    }

    #[test]
    fn invalid_environment_names_fail_before_tmux() {
        let launch = TmuxLaunch {
            window_name: "actor".into(),
            cwd: PathBuf::from("/tmp"),
            program: "node".into(),
            args: Vec::new(),
            environment: BTreeMap::from([("BAD-NAME".into(), "x".into())]),
        };
        assert!(matches!(
            validate_launch(&launch),
            Err(TmuxNodeError::InvalidEnvironmentName(_))
        ));
    }
}
