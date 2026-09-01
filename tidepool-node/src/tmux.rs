//! Exact tmux-pane ownership for interactive actor applications.
//!
//! Tmux is a deployment and observability adapter here, never a message
//! transport. Model input goes through the backend's supported push channel.

use std::collections::{BTreeMap, HashSet};
use std::path::PathBuf;

use tokio::process::Command;

/// A tmux session name safe to use as an exact target.
#[derive(Debug, Clone, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct TmuxSessionName(String);

impl TmuxSessionName {
    pub fn parse(value: impl Into<String>) -> Result<Self, TmuxNodeError> {
        let value = value.into();
        if !value.is_empty()
            && value
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
        {
            Ok(Self(value))
        } else {
            Err(TmuxNodeError::InvalidSessionName(value))
        }
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl TryFrom<String> for TmuxSessionName {
    type Error = TmuxNodeError;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        Self::parse(value)
    }
}

impl From<TmuxSessionName> for String {
    fn from(value: TmuxSessionName) -> Self {
        value.0
    }
}

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

/// One interactive actor application launched in a fresh tmux window.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TmuxLaunch {
    pub window_name: String,
    pub cwd: PathBuf,
    pub program: String,
    pub args: Vec<String>,
    /// Values are passed with tmux's `-e`, never interpolated into the shell
    /// command that starts the application.
    pub environment: BTreeMap<String, String>,
}

/// A tmux session selected by name and optional dedicated server socket.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TmuxSession {
    name: TmuxSessionName,
    socket: Option<String>,
}

impl TmuxSession {
    pub fn new(name: impl Into<String>) -> Result<Self, TmuxNodeError> {
        Ok(Self {
            name: TmuxSessionName::parse(name)?,
            socket: None,
        })
    }

    pub fn with_socket(
        name: impl Into<String>,
        socket: impl Into<String>,
    ) -> Result<Self, TmuxNodeError> {
        Ok(Self {
            name: TmuxSessionName::parse(name)?,
            socket: Some(socket.into()),
        })
    }

    #[must_use]
    pub fn name(&self) -> &TmuxSessionName {
        &self.name
    }

    pub async fn exists(&self) -> Result<bool, TmuxNodeError> {
        let output = self
            .command()
            .args(["has-session", "-t", self.name.as_str()])
            .output()
            .await
            .map_err(|source| TmuxNodeError::Io {
                operation: "has-session",
                source,
            })?;
        Ok(output.status.success())
    }

    /// Create the session and run its initial process directly in the first
    /// pane. Existing sessions are an error; callers choose recreation
    /// explicitly rather than racing through an implicit ensure operation.
    pub async fn create(&self, launch: &TmuxLaunch) -> Result<TmuxPaneId, TmuxNodeError> {
        validate_launch(launch)?;
        let output = self
            .create_command(launch)
            .output()
            .await
            .map_err(|source| TmuxNodeError::Io {
                operation: "new-session",
                source,
            })?;
        if !output.status.success() {
            return Err(TmuxNodeError::Command {
                operation: "new-session",
                status: output.status,
                stderr: String::from_utf8_lossy(&output.stderr).trim().to_string(),
            });
        }
        TmuxPaneId::parse(String::from_utf8_lossy(&output.stdout).trim())
    }

    /// Kill this exact session. Absence is already the desired state.
    pub async fn kill(&self) -> Result<(), TmuxNodeError> {
        if !self.exists().await? {
            return Ok(());
        }
        let output = self
            .command()
            .args(["kill-session", "-t", self.name.as_str()])
            .output()
            .await
            .map_err(|source| TmuxNodeError::Io {
                operation: "kill-session",
                source,
            })?;
        if output.status.success() || !self.exists().await? {
            Ok(())
        } else {
            Err(TmuxNodeError::Command {
                operation: "kill-session",
                status: output.status,
                stderr: String::from_utf8_lossy(&output.stderr).trim().to_string(),
            })
        }
    }

    /// Enter the session from either a plain terminal or an existing tmux
    /// client. This avoids tmux's nested-session refusal.
    pub async fn attach_or_switch(&self) -> Result<(), TmuxNodeError> {
        let operation = if std::env::var_os("TMUX").is_some() {
            "switch-client"
        } else {
            "attach-session"
        };
        let status = self
            .command()
            .args([operation, "-t", self.name.as_str()])
            .status()
            .await
            .map_err(|source| TmuxNodeError::Io { operation, source })?;
        if status.success() {
            Ok(())
        } else {
            Err(TmuxNodeError::Command {
                operation,
                status,
                stderr: "tmux did not attach or switch the client".into(),
            })
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

    /// Select the window containing an exact pane owned by this session.
    ///
    /// A detached session retains this selection, so a later attach lands on
    /// the actor application rather than its background host window.
    pub async fn select_window_for_pane(&self, pane: &TmuxPaneId) -> Result<(), TmuxNodeError> {
        if !self.contains_pane(pane).await? {
            return Err(TmuxNodeError::PaneNotOwned(pane.as_str().into()));
        }
        let output = self
            .command()
            .args(["select-window", "-t", pane.as_str()])
            .output()
            .await
            .map_err(|source| TmuxNodeError::Io {
                operation: "select-window",
                source,
            })?;
        if output.status.success() {
            Ok(())
        } else {
            Err(TmuxNodeError::Command {
                operation: "select-window",
                status: output.status,
                stderr: String::from_utf8_lossy(&output.stderr).trim().to_string(),
            })
        }
    }

    pub async fn kill_pane(&self, pane: &TmuxPaneId) -> Result<(), TmuxNodeError> {
        // Pane ids are server-global. Prove that this exact pane is still in
        // the owned session before issuing a destructive tmux command.
        if !self.contains_pane(pane).await? {
            return Ok(());
        }
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
            // Exact-pane teardown is idempotent: a process that exits between
            // the membership check and kill may remove its pane first.
            if !self.contains_pane(pane).await? {
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
            .args([
                "list-panes",
                "-s",
                "-t",
                self.name.as_str(),
                "-F",
                "#{pane_id}",
            ])
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

    async fn contains_pane(&self, pane: &TmuxPaneId) -> Result<bool, TmuxNodeError> {
        if !self.exists().await? {
            return Ok(false);
        }
        match self.list_panes().await {
            Ok(panes) => Ok(panes.contains(pane)),
            Err(error) => {
                if self.exists().await? {
                    Err(error)
                } else {
                    Ok(false)
                }
            }
        }
    }

    fn command(&self) -> Command {
        let mut command = Command::new("tmux");
        command.kill_on_drop(true);
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
            .arg(self.name.as_str())
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

    fn create_command(&self, launch: &TmuxLaunch) -> Command {
        let mut command = self.command();
        command
            .arg("new-session")
            .arg("-d")
            .arg("-s")
            .arg(self.name.as_str())
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
        if !valid_environment_name(name) {
            return Err(TmuxNodeError::InvalidEnvironmentName(name.clone()));
        }
    }
    Ok(())
}

fn valid_environment_name(name: &str) -> bool {
    !name.is_empty()
        && name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
        && !name.as_bytes()[0].is_ascii_digit()
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
    #[error("invalid tmux session name {0:?}")]
    InvalidSessionName(String),
    #[error("invalid tmux pane id {0:?}")]
    InvalidPaneId(String),
    #[error("tmux pane {0:?} does not belong to the owned session")]
    PaneNotOwned(String),
    #[error("tmux launch program is empty or contains NUL")]
    InvalidProgram,
    #[error("invalid tmux launch environment name {0:?}")]
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
        let session = TmuxSession::with_socket("swarm", "sock").unwrap();
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

    #[test]
    fn session_names_are_exact_validated_targets() {
        assert_eq!(
            TmuxSessionName::parse("shoal-tidepool_2").unwrap().as_str(),
            "shoal-tidepool_2"
        );
        for invalid in ["", "has:target", "has.dot", "has space", "🐟"] {
            assert!(TmuxSessionName::parse(invalid).is_err(), "{invalid:?}");
        }
    }

    #[tokio::test]
    async fn dedicated_socket_session_has_exact_create_and_kill_lifecycle() {
        let suffix = uuid::Uuid::new_v4().simple().to_string();
        let session = TmuxSession::with_socket(
            format!("shoal_test_{}", &suffix[..8]),
            format!("shoal-test-{}", &suffix[..8]),
        )
        .unwrap();
        let neighbor = TmuxSession::with_socket(
            format!("shoal_neighbor_{}", &suffix[..8]),
            format!("shoal-test-{}", &suffix[..8]),
        )
        .unwrap();
        let launch = TmuxLaunch {
            window_name: "Host".into(),
            cwd: std::env::temp_dir(),
            program: "sleep".into(),
            args: vec!["60".into()],
            environment: BTreeMap::new(),
        };

        let pane = session.create(&launch).await.unwrap();
        assert!(session.exists().await.unwrap());
        assert!(session.list_panes().await.unwrap().contains(&pane));
        assert!(session.create(&launch).await.is_err());

        let mut actor_launch = launch.clone();
        actor_launch.window_name = "RootActor".into();
        let actor_pane = session.spawn_window(&actor_launch).await.unwrap();
        session.select_window_for_pane(&actor_pane).await.unwrap();
        let selected = session
            .command()
            .args([
                "display-message",
                "-p",
                "-t",
                session.name.as_str(),
                "#{window_name}",
            ])
            .output()
            .await
            .unwrap();
        assert!(selected.status.success());
        assert_eq!(
            String::from_utf8_lossy(&selected.stdout).trim(),
            "RootActor"
        );

        let foreign_pane = neighbor.create(&launch).await.unwrap();
        assert!(matches!(
            session.select_window_for_pane(&foreign_pane).await,
            Err(TmuxNodeError::PaneNotOwned(_))
        ));
        session.kill_pane(&foreign_pane).await.unwrap();
        assert!(neighbor.list_panes().await.unwrap().contains(&foreign_pane));

        session.kill().await.unwrap();
        assert!(!session.exists().await.unwrap());
        session.kill().await.unwrap();
        neighbor.kill().await.unwrap();
    }
}
