//! Start or join the one per-user command resource service.
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;
use tidepool_node::command_resources::{
    CommandResourceClient, CommandResourcePolicy, CommandResources,
};

type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;
const UNIT: &str = "shoal-command-resources.service";

pub(super) async fn connect(
    policy: CommandResourcePolicy,
    run: &str,
) -> Result<Arc<CommandResourceClient>> {
    policy.validate()?;
    let runtime = std::env::var_os("XDG_RUNTIME_DIR")
        .ok_or("XDG_RUNTIME_DIR is required for shared command resources")?;
    let directory = PathBuf::from(runtime).join("shoal-commands");
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
    if socket.exists() {
        return Ok(CommandResourceClient::connect(socket, run.into(), &policy).await?);
    }
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
                "--property=Delegate=yes",
                "--property=KillMode=process",
                "--property=Restart=no",
                "--property=ExitType=cgroup",
                "--unit",
                UNIT,
            ])
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
    loop {
        if socket.exists() {
            return Ok(CommandResourceClient::connect(socket, run.into(), &policy).await?);
        }
        if tokio::time::Instant::now() >= deadline {
            return Err("shared command resource service did not become ready; existing allocations must be reconciled before restarting it".into());
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

pub async fn serve(socket: PathBuf, policy: PathBuf) -> Result<()> {
    let policy: CommandResourcePolicy = toml::from_str(&std::fs::read_to_string(policy)?)?;
    let owner = CommandResources::delegated(policy)?;
    // Bind exclusively: never unlink a potentially live owner's endpoint.
    let listener = tokio::net::UnixListener::bind(socket)?;
    tidepool_node::command_resources::service::serve(listener, owner).await?;
    Ok(())
}
