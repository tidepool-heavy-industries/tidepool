//! Run this target inside a fresh `systemd-run --user --scope -p Delegate=yes` scope.
use std::{path::Path, sync::Arc, time::Duration};
use tidepool_node::command_resources::{
    CommandResourcePolicy, CommandResourceStatus, CommandResources,
};
use tokio::process::{Child, Command};
fn spawn_in(path: &Path, script: &str) -> Child {
    use std::os::fd::AsRawFd;
    let join = std::fs::OpenOptions::new()
        .write(true)
        .open(path.join("cgroup.procs"))
        .unwrap();
    let mut command = Command::new("python3");
    command.args(["-c", script]);
    // SAFETY: only a retained fd and async-signal-safe write are used before exec.
    unsafe {
        command.pre_exec(move || {
            if libc::write(join.as_raw_fd(), b"0".as_ptr().cast(), 1) != 1 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
    command.spawn().unwrap()
}
async fn admitted(owner: &Arc<CommandResources>, id: &str) -> std::path::PathBuf {
    match owner.acquire("test", id).await.unwrap() {
        CommandResourceStatus::Admitted { cgroup } => cgroup,
        other => panic!("unexpected {other:?}"),
    }
}
#[tokio::test]
#[ignore = "requires a fresh delegated systemd cgroup scope"]
async fn command_oom_and_queue_preserve_the_control_process() {
    let policy = CommandResourcePolicy {
        concurrency: 2,
        memory_high_bytes: None,
        memory_max_bytes: 64 * 1024 * 1024,
        swap_max_bytes: 0,
        queue_timeout_seconds: 1,
        ..Default::default()
    };
    let owner = CommandResources::delegated(policy).unwrap();
    let first = admitted(&owner, "first").await;
    let mut sibling = spawn_in(&first, "import time; time.sleep(30)");
    owner.started("test", "first").unwrap();
    let second = admitted(&owner, "oom").await;
    let mut child = spawn_in(&second, "a=bytearray(128*1024*1024)");
    owner.started("test", "oom").unwrap();
    assert!(!child.wait().await.unwrap().success());
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert!(matches!(
        owner.status("test", "oom").unwrap(),
        CommandResourceStatus::ResourceExhausted
    ));
    assert!(sibling.try_wait().unwrap().is_none());
    let third = admitted(&owner, "third").await;
    let mut command = spawn_in(&third, "import time; time.sleep(30)");
    owner.started("test", "third").unwrap();
    assert!(matches!(
        owner.acquire("test", "timeout").await.unwrap(),
        CommandResourceStatus::AdmissionTimedOut
    ));
    let queued = owner.clone();
    let waiter = tokio::spawn(async move { queued.acquire("test", "cancelled").await.unwrap() });
    tokio::time::sleep(Duration::from_millis(30)).await;
    assert!(matches!(
        owner.cancel("test", "cancelled").unwrap(),
        CommandResourceStatus::CancelledBeforeStart
    ));
    assert!(matches!(
        waiter.await.unwrap(),
        CommandResourceStatus::CancelledBeforeStart
    ));
    // Cancellation arriving first must prevent a later admission with this identity.
    owner.cancel("test", "cancel-first").unwrap();
    assert!(matches!(
        owner.acquire("test", "cancel-first").await.unwrap(),
        CommandResourceStatus::CancelledBeforeStart
    ));
    // An abandoned request cannot linger in the queue and launch after capacity frees.
    let abandoned_owner = owner.clone();
    let abandoned = tokio::spawn(async move { abandoned_owner.acquire("test", "abandoned").await });
    tokio::time::sleep(Duration::from_millis(30)).await;
    abandoned.abort();
    assert!(abandoned.await.unwrap_err().is_cancelled());
    assert!(matches!(
        owner.status("test", "abandoned").unwrap(),
        CommandResourceStatus::CancelledBeforeStart
    ));
    command.kill().await.unwrap();
    sibling.kill().await.unwrap();
    tokio::time::sleep(Duration::from_millis(200)).await;
    let background_group = admitted(&owner, "background").await;
    let mut background = spawn_in(
        &background_group,
        "import os,time; pid=os.fork(); time.sleep(2) if pid==0 else None",
    );
    owner.started("test", "background").unwrap();
    assert!(background.wait().await.unwrap().success());
    assert!(
        background_group.exists(),
        "root exit must not retire a live descendant"
    );
    let occupied = admitted(&owner, "occupied").await;
    let mut occupied_child = spawn_in(&occupied, "import time; time.sleep(30)");
    owner.started("test", "occupied").unwrap();
    assert!(matches!(
        owner.acquire("test", "descendant-wait").await.unwrap(),
        CommandResourceStatus::AdmissionTimedOut
    ));
    occupied_child.kill().await.unwrap();
    tokio::time::sleep(Duration::from_millis(1200)).await;
    assert!(matches!(
        owner.status("test", "background").unwrap(),
        CommandResourceStatus::Completed
    ));
    let final_group = admitted(&owner, "after").await;
    let mut final_command = spawn_in(&final_group, "print('control survived')");
    owner.started("test", "after").unwrap();
    assert!(final_command.wait().await.unwrap().success());
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert!(matches!(
        owner.status("test", "after").unwrap(),
        CommandResourceStatus::Completed
    ));
}

#[tokio::test]
#[ignore = "requires a fresh delegated systemd cgroup scope"]
async fn actor_admission_times_out_without_starting() {
    let owner = CommandResources::delegated(CommandResourcePolicy {
        machine_headroom_bytes: 1 << 60,
        queue_timeout_seconds: 1,
        ..Default::default()
    })
    .unwrap();
    let attempted = owner.clone();
    let start = tokio::spawn(async move { attempted.admit_actor().await.map(|_| ()) });
    assert!(start
        .await
        .unwrap()
        .unwrap_err()
        .to_string()
        .contains("actor not started"));
    let attempted = owner.clone();
    let cancelled = tokio::spawn(async move { attempted.admit_actor().await });
    tokio::time::sleep(Duration::from_millis(30)).await;
    cancelled.abort();
    assert!(matches!(cancelled.await, Err(error) if error.is_cancelled()));
}
