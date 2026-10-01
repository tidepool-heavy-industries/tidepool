//! Systemd placement shared by launchers; resource limits belong to system policy.
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::ProcessInvocation;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct SystemdSlice(String);

/// Optional filesystem hiding for a single supervised host service.
/// Paths are validated before they become systemd's whitespace-separated list.
#[derive(Clone, Debug, Default, PartialEq, Eq, Deserialize)]
#[serde(try_from = "HostFilesystemPaths")]
pub struct HostFilesystemPolicy {
    inaccessible_paths: Vec<PathBuf>,
}

#[derive(Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct HostFilesystemPaths {
    inaccessible_paths: Vec<PathBuf>,
}

impl TryFrom<HostFilesystemPaths> for HostFilesystemPolicy {
    type Error = String;

    fn try_from(paths: HostFilesystemPaths) -> Result<Self, Self::Error> {
        Self::try_new(paths.inaccessible_paths)
    }
}

impl HostFilesystemPolicy {
    pub fn try_new(inaccessible_paths: Vec<PathBuf>) -> Result<Self, String> {
        for path in &inaccessible_paths {
            let text = path
                .to_str()
                .ok_or("host inaccessible path must be UTF-8")?;
            if !path.is_absolute()
                || path
                    .components()
                    .any(|part| matches!(part, std::path::Component::ParentDir))
                || text.bytes().any(|byte| {
                    byte.is_ascii_whitespace()
                        || byte.is_ascii_control()
                        || matches!(byte, b'\\' | b'%' | b'\"' | b'\'')
                })
            {
                return Err("host inaccessible paths require absolute paths without parent components, whitespace, controls, quotes, backslashes, or systemd specifiers".into());
            }
        }
        Ok(Self { inaccessible_paths })
    }

    fn service_properties(&self) -> Vec<String> {
        if self.inaccessible_paths.is_empty() {
            return Vec::new();
        }
        vec![
            "--property=PrivateUsers=yes".into(),
            "--property=PrivateMounts=yes".into(),
            format!(
                "--property=InaccessiblePaths={}",
                self.inaccessible_paths
                    .iter()
                    .map(|path| path.to_str().expect("validated UTF-8 path"))
                    .collect::<Vec<_>>()
                    .join(" ")
            ),
        ]
    }
}

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
        #[allow(
            clippy::disallowed_methods,
            reason = "short synchronous probe of systemd slice properties, not a long-lived child"
        )]
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

    /// Run a host as a restart-bounded per-run service. `--wait` keeps the
    /// tmux diagnostics window attached to the unit through automatic restarts;
    /// an intentional `systemctl stop` is successful and does not restart.
    pub fn supervised_service(
        &self,
        unit: &str,
        command: ProcessInvocation,
        environment: &BTreeMap<String, String>,
        filesystem: &HostFilesystemPolicy,
    ) -> ProcessInvocation {
        let mut args = vec![
            "--user".into(),
            "--quiet".into(),
            "--wait".into(),
            "--collect".into(),
            "--service-type=exec".into(),
            "--expand-environment=no".into(),
            format!("--slice={}", self.as_str()),
            "--property=Delegate=yes".into(),
            "--property=KillMode=process".into(),
            "--property=Restart=on-failure".into(),
            "--property=RestartSec=2s".into(),
            "--property=StartLimitIntervalSec=60s".into(),
            "--property=StartLimitBurst=5".into(),
            format!("--unit={unit}"),
        ];
        args.extend(filesystem.service_properties());
        args.extend(environment.keys().map(|name| format!("--setenv={name}")));
        args.extend(["--".into(), command.program]);
        args.extend(command.args);
        ProcessInvocation {
            program: "systemd-run".into(),
            args,
        }
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
                "configure the swarm slice before launching Exomonad",
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

    #[test]
    fn supervised_service_has_bounded_failure_restart_and_literal_arguments() {
        let environment = BTreeMap::from([("PATH".into(), "/bin".into())]);
        let wrapped = SystemdSlice::default().supervised_service(
            "exomonad-host-run-1",
            ProcessInvocation {
                program: "/a path/exomonad".into(),
                args: vec!["host".into(), "$HOME;literal".into()],
            },
            &environment,
            &HostFilesystemPolicy::default(),
        );
        assert_eq!(wrapped.program, "systemd-run");
        assert!(wrapped
            .args
            .contains(&"--property=Restart=on-failure".into()));
        assert!(wrapped
            .args
            .contains(&"--property=StartLimitBurst=5".into()));
        assert!(wrapped.args.contains(&"--wait".into()));
        assert!(wrapped.args.contains(&"--setenv=PATH".into()));
        assert_eq!(wrapped.args.last().unwrap(), "$HOME;literal");
    }
    #[test]
    fn host_filesystem_policy_rejects_systemd_list_and_specifier_syntax() {
        for path in [
            "relative", "/a/../b", "/a b", "/a\t", "/a\n", "/a\\b", "/a%b", "/a\"b", "/a'b",
        ] {
            assert!(
                HostFilesystemPolicy::try_new(vec![PathBuf::from(path)]).is_err(),
                "{path:?}"
            );
        }
        assert!(serde_json::from_str::<HostFilesystemPolicy>(
            r#"{"inaccessible_paths":["relative"]}"#
        )
        .is_err());
        assert!(serde_json::from_str::<HostFilesystemPolicy>(r#"{"unknown":[]}"#).is_err());
    }

    #[test]
    fn supervised_host_filesystem_policy_is_per_unit_and_default_empty() {
        let policy = HostFilesystemPolicy::try_new(vec![
            PathBuf::from("/srv/swarm/checkouts"),
            PathBuf::from("/srv/build"),
        ])
        .unwrap();
        let service = |policy: &HostFilesystemPolicy| {
            SystemdSlice::default().supervised_service(
                "host-one",
                ProcessInvocation {
                    program: "/nix/store/host".into(),
                    args: vec!["host".into()],
                },
                &BTreeMap::new(),
                policy,
            )
        };
        let default_policy = HostFilesystemPolicy::default();
        let baseline = service(&default_policy);
        let isolated = service(&policy);
        let boundary = isolated.args.iter().position(|arg| arg == "--").unwrap();
        assert_eq!(
            &isolated.args[boundary..],
            &["--", "/nix/store/host", "host"]
        );
        for property in policy.service_properties() {
            assert!(isolated.args[..boundary].contains(&property));
            assert!(!baseline.args.contains(&property));
        }
        assert!(isolated.args.contains(&"--unit=host-one".into()));
        assert_eq!(isolated.args.len(), baseline.args.len() + 3);
    }
}
