//! Start or join the one per-user command resource service.
use exomonad_node::command_resources::{
    CommandResourceClient, CommandResourcePolicy, CommandResources,
};
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;
const UNIT: &str = "exomonad-command-resources.service";

pub(super) async fn connect(
    policy: CommandResourcePolicy,
    run: &str,
    slice: &exomonad_node::systemd_slice::SystemdSlice,
) -> Result<Arc<CommandResourceClient>> {
    policy.validate()?;
    let runtime = std::env::var_os("XDG_RUNTIME_DIR")
        .ok_or("XDG_RUNTIME_DIR is required for shared command resources")?;
    let directory = PathBuf::from(runtime).join("exomonad-commands");
    std::fs::create_dir_all(&directory)?;
    std::fs::set_permissions(&directory, std::fs::Permissions::from_mode(0o700))?;
    let lock = std::fs::File::options()
        .create(true)
        .truncate(false)
        .write(true)
        .open(directory.join("startup.lock"))?;
    // One startup owner; this guard is not held during command admission.
    lock.lock()?;
    let socket = directory.join("resources.sock");
    let active = tokio::process::Command::new("systemctl")
        .args(["--user", "is-active", "--quiet", UNIT])
        .status()
        .await?;
    if !active.success() {
        let policy_file = directory.join("policy.toml");
        tidepool_atomic_write::write_best_effort(
            &policy_file,
            toml::to_string(&policy)?.as_bytes(),
        )?;
        let status = tokio::process::Command::new("systemd-run")
            .args([
                "--user",
                "--quiet",
                "--service-type=exec",
                "--expand-environment=no",
                "--property=Delegate=yes",
                "--property=KillMode=process",
                "--property=Restart=on-failure",
                "--property=RestartSec=2s",
                "--property=StartLimitIntervalSec=60s",
                "--property=StartLimitBurst=5",
                "--unit",
                UNIT,
            ])
            .arg(format!("--slice={}", slice.as_str()))
            .arg(std::env::current_exe()?)
            .args(["in-slice", "--slice", slice.as_str(), "--"])
            .arg(std::env::current_exe()?)
            .arg("command-resources")
            .arg("--socket")
            .arg(&socket)
            .arg("--policy")
            .arg(&policy_file)
            .status()
            .await?;
        if !status.success() {
            return Err(
                "shared command resource service did not start; inspect its systemd journal".into(),
            );
        }
    }
    let deadline = tokio::time::Instant::now() + Duration::from_secs(15);
    let mut last_error = None;
    loop {
        if socket.exists() {
            verify_service_slice(slice).await?;
            match CommandResourceClient::connect(socket.clone(), run.into(), &policy).await {
                Ok(client) => return Ok(client),
                Err(error) => last_error = Some(error),
            }
        }
        if tokio::time::Instant::now() >= deadline {
            let detail = last_error.map_or_else(
                || "endpoint was not published".into(),
                |error| error.to_string(),
            );
            return Err(format!("shared command resource service did not become ready after reconciliation: {detail}").into());
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

async fn verify_service_slice(slice: &exomonad_node::systemd_slice::SystemdSlice) -> Result<()> {
    let output = tokio::process::Command::new("systemctl")
        .args(["--user", "show", UNIT, "--property=ControlGroup", "--value"])
        .output()
        .await?;
    let group = String::from_utf8_lossy(&output.stdout);
    if !output.status.success() || !slice.contains(std::path::Path::new(group.trim())) {
        return Err("shared command service is outside the selected slice; stop its runs before restarting it".into());
    }
    Ok(())
}

pub async fn serve(socket: PathBuf, policy: PathBuf) -> Result<()> {
    let directory = socket
        .parent()
        .ok_or("resource socket has no parent directory")?;
    let ownership = std::fs::File::options()
        .create(true)
        .truncate(false)
        .write(true)
        .open(directory.join("service.lock"))?;
    ownership.try_lock().map_err(|error| {
        std::io::Error::other(format!("another command resource owner is active: {error}"))
    })?;
    let policy: CommandResourcePolicy = toml::from_str(&std::fs::read_to_string(policy)?)?;
    let journal = directory.join("ownership.v1.jsonl");
    let owner = CommandResources::delegated_with_journal(policy, journal)?;
    clear_stale_resource_socket(&socket)?;
    let listener = tokio::net::UnixListener::bind(socket)?;
    exomonad_node::command_resources::service::serve(listener, owner).await?;
    drop(ownership);
    Ok(())
}

/// Called only after journal/cgroup reconciliation and while holding the
/// service ownership lock, so a refused endpoint is known to be stale.
fn clear_stale_resource_socket(socket: &std::path::Path) -> Result<()> {
    use std::os::unix::fs::FileTypeExt as _;

    if socket.exists() {
        if !std::fs::symlink_metadata(socket)?.file_type().is_socket() {
            return Err("refusing to replace a non-socket resource endpoint".into());
        }
        match std::os::unix::net::UnixStream::connect(socket) {
            Ok(_) => return Err("refusing to replace a live resource endpoint".into()),
            Err(error)
                if matches!(
                    error.kind(),
                    std::io::ErrorKind::ConnectionRefused | std::io::ErrorKind::NotFound
                ) =>
            {
                std::fs::remove_file(socket)?;
            }
            Err(error) => return Err(error.into()),
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn restart_removes_only_a_stale_socket() {
        let directory = tempfile::tempdir().unwrap();
        let socket = directory.path().join("resources.sock");
        let listener = std::os::unix::net::UnixListener::bind(&socket).unwrap();
        assert!(clear_stale_resource_socket(&socket).is_err());
        drop(listener);
        clear_stale_resource_socket(&socket).unwrap();
        assert!(!socket.exists());
    }

    #[test]
    fn restart_refuses_to_replace_non_socket_evidence() {
        let directory = tempfile::tempdir().unwrap();
        let socket = directory.path().join("resources.sock");
        std::fs::write(&socket, b"ownership evidence").unwrap();
        assert!(clear_stale_resource_socket(&socket).is_err());
        assert_eq!(std::fs::read(socket).unwrap(), b"ownership evidence");
    }
}
