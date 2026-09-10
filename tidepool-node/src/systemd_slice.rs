//! Systemd placement shared by launchers; resource limits belong to system policy.
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::ProcessInvocation;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct SystemdSlice(String);

impl Default for SystemdSlice {
    fn default() -> Self {
        Self("swarm.slice".into())
    }
}

impl TryFrom<String> for SystemdSlice {
    type Error = String;

    fn try_from(name: String) -> Result<Self, Self::Error> {
        let Some(stem) = name.strip_suffix(".slice") else {
            return Err("systemd slice must end in .slice".into());
        };
        if stem.is_empty()
            || stem == "-"
            || !stem
                .bytes()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, b'-' | b'_'))
        {
            return Err("invalid systemd slice name".into());
        }
        Ok(Self(name))
    }
}

impl From<SystemdSlice> for String {
    fn from(slice: SystemdSlice) -> Self {
        slice.0
    }
}

impl SystemdSlice {
    pub fn as_str(&self) -> &str {
        &self.0
    }

    pub fn verified_command(
        &self,
        executable: &Path,
        command: ProcessInvocation,
    ) -> ProcessInvocation {
        let mut args = vec![
            "in-slice".into(),
            "--slice".into(),
            self.0.clone(),
            "--".into(),
            command.program,
        ];
        args.extend(command.args);
        ProcessInvocation {
            program: executable.display().to_string(),
            args,
        }
    }

    /// Require explicit machine configuration rather than an unlimited implicit slice.
    pub async fn inspect(&self) -> std::io::Result<SliceLimits> {
        let output = tokio::process::Command::new("systemctl")
            .args([
                "--user",
                "show",
                self.as_str(),
                "--property=LoadState,ControlGroup,MemoryHigh,MemoryMax,MemorySwapMax",
            ])
            .output()
            .await?;
        if !output.status.success() {
            return Err(std::io::Error::other(
                String::from_utf8_lossy(&output.stderr).into_owned(),
            ));
        }
        SliceLimits::parse(&String::from_utf8_lossy(&output.stdout))
    }

    /// Keep arguments separate until the existing tmux launcher quotes them.
    pub fn scope(&self, command: ProcessInvocation) -> ProcessInvocation {
        self.wrap(command, Vec::new())
    }

    pub fn delegated_scope(&self, unit: &str, command: ProcessInvocation) -> ProcessInvocation {
        self.wrap(
            command,
            vec!["--property=Delegate=yes".into(), format!("--unit={unit}")],
        )
    }

    fn wrap(&self, command: ProcessInvocation, properties: Vec<String>) -> ProcessInvocation {
        let mut args = vec![
            "--user".into(),
            "--scope".into(),
            "--quiet".into(),
            "--expand-environment=no".into(),
            format!("--slice={}", self.as_str()),
        ];
        args.extend(properties);
        args.extend(["--".into(), command.program]);
        args.extend(command.args);
        ProcessInvocation {
            program: "systemd-run".into(),
            args,
        }
    }

    pub fn contains(&self, cgroup: &Path) -> bool {
        cgroup
            .components()
            .any(|component| component.as_os_str() == self.as_str())
    }

    pub fn current_membership(&self) -> std::io::Result<PathBuf> {
        let membership = std::fs::read_to_string("/proc/self/cgroup")?;
        let path = membership
            .lines()
            .find_map(|line| line.strip_prefix("0::"))
            .map(PathBuf::from)
            .ok_or_else(|| std::io::Error::other("cgroup v2 membership unavailable"))?;
        if !self.contains(&path) {
            return Err(std::io::Error::other(format!(
                "process is outside required slice {}: {}",
                self.as_str(),
                path.display()
            )));
        }
        Ok(path)
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct SliceLimits {
    pub cgroup: PathBuf,
    pub memory_high: u64,
    pub memory_max: u64,
    pub swap_max: u64,
}

impl SliceLimits {
    fn parse(text: &str) -> std::io::Result<Self> {
        let fields: std::collections::BTreeMap<_, _> = text
            .lines()
            .filter_map(|line| line.split_once('='))
            .collect();
        if fields.get("LoadState") != Some(&"loaded") {
            return Err(std::io::Error::other(
                "configure the swarm slice before launching Shoal",
            ));
        }
        let limit = |name| -> std::io::Result<u64> {
            let value = fields
                .get(name)
                .and_then(|value| value.parse::<u64>().ok())
                .filter(|value| *value != u64::MAX)
                .ok_or_else(|| std::io::Error::other(format!("slice requires a finite {name}")))?;
            Ok(value)
        };
        let result = Self {
            cgroup: PathBuf::from(fields.get("ControlGroup").copied().unwrap_or_default()),
            memory_high: limit("MemoryHigh")?,
            memory_max: limit("MemoryMax")?,
            swap_max: limit("MemorySwapMax")?,
        };
        if result.memory_max == 0
            || result.memory_high == 0
            || result.memory_high > result.memory_max
        {
            return Err(std::io::Error::other(
                "slice requires 0 < MemoryHigh <= MemoryMax",
            ));
        }
        Ok(result)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_implicit_or_unlimited_configuration() {
        assert!(SliceLimits::parse("LoadState=not-found\n").is_err());
        assert!(SliceLimits::parse(
            "LoadState=loaded\nMemoryHigh=16\nMemoryMax=infinity\nMemorySwapMax=24\n"
        )
        .is_err());
        assert_eq!(SliceLimits::parse("LoadState=loaded\nMemoryHigh=16\nMemoryMax=18\nMemorySwapMax=24\nControlGroup=/user.slice/swarm.slice\n").unwrap().memory_max, 18);
    }

    #[test]
    fn membership_matches_a_whole_component() {
        let slice = SystemdSlice::default();
        assert!(slice.contains(Path::new("/user.slice/swarm.slice/run.scope/control")));
        assert!(!slice.contains(Path::new("/user.slice/not-swarm.slice/run.scope")));
    }

    #[test]
    fn scope_preserves_literal_arguments() {
        let wrapped = SystemdSlice::default().scope(ProcessInvocation {
            program: "/a path/tool".into(),
            args: vec!["$HOME;literal".into()],
        });
        assert_eq!(wrapped.args.last().unwrap(), "$HOME;literal");
        assert_eq!(wrapped.args[6], "/a path/tool");
    }
}
