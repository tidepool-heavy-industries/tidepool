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
    owner.submit("test", id, 64 * 1024 * 1024).unwrap();
    match owner.wait("test", id).await.unwrap() {
        CommandResourceStatus::Admitted { cgroup } => cgroup,
        other => panic!("unexpected {other:?}"),
    }
}
#[tokio::test]
#[ignore = "requires a fresh delegated systemd cgroup scope"]
async fn command_oom_and_queue_preserve_the_control_process() {
    let policy = CommandResourcePolicy {
        general_bytes: 128 * 1024 * 1024,
        protected_bytes: 0,
        swap_max_bytes: 0,
        actor_start_timeout_seconds: 1,
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
    assert_eq!(
        owner.submit("test", "retained", 64 * 1024 * 1024).unwrap(),
        CommandResourceStatus::Queued
    );
    owner.cancel("test", "retained").unwrap();
    let queued = owner.clone();
    let waiter = tokio::spawn(async move {
        {
            queued
                .submit("test", "cancelled", 64 * 1024 * 1024)
                .unwrap();
            queued.wait("test", "cancelled").await.unwrap()
        }
    });
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
        owner
            .submit("test", "cancel-first", 64 * 1024 * 1024)
            .unwrap(),
        CommandResourceStatus::CancelledBeforeStart
    ));
    // Losing an observing future does not discard accepted work.
    let abandoned_owner = owner.clone();
    let abandoned = tokio::spawn(async move {
        {
            abandoned_owner
                .submit("test", "abandoned", 64 * 1024 * 1024)
                .unwrap();
            abandoned_owner.wait("test", "abandoned").await
        }
    });
    tokio::time::sleep(Duration::from_millis(30)).await;
    abandoned.abort();
    assert!(abandoned.await.unwrap_err().is_cancelled());
    assert!(matches!(
        owner.status("test", "abandoned").unwrap(),
        CommandResourceStatus::Queued
    ));
    owner.cancel("test", "abandoned").unwrap();
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
    assert_eq!(
        owner
            .submit("test", "descendant-wait", 64 * 1024 * 1024)
            .unwrap(),
        CommandResourceStatus::Queued
    );
    owner.cancel("test", "descendant-wait").unwrap();
    occupied_child.kill().await.unwrap();
    tokio::time::timeout(Duration::from_secs(5), async {
        while !matches!(
            owner.status("test", "background").unwrap(),
            CommandResourceStatus::Completed
        ) {
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await
    .unwrap();
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
        actor_start_timeout_seconds: 1,
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

#[tokio::test]
#[ignore = "requires a fresh delegated systemd cgroup scope"]
async fn shared_clients_retain_queued_work_after_observer_disconnect() {
    use tidepool_node::command_resources::{service, CommandResourceClient};
    let policy = CommandResourcePolicy {
        general_bytes: 64 * 1024 * 1024,
        protected_bytes: 0,
        swap_max_bytes: 0,
        ..Default::default()
    };
    let owner = CommandResources::delegated(policy.clone()).unwrap();
    let directory = tempfile::tempdir().unwrap();
    let socket = directory.path().join("resources.sock");
    let listener = tokio::net::UnixListener::bind(&socket).unwrap();
    let server = tokio::spawn(service::serve(listener, owner));
    let first = CommandResourceClient::connect(socket.clone(), "run-a".into(), &policy)
        .await
        .unwrap();
    let second = CommandResourceClient::connect(socket, "run-b".into(), &policy)
        .await
        .unwrap();
    let status = first
        .submit("0-1", "same-id", 64 * 1024 * 1024)
        .await
        .unwrap();
    let CommandResourceStatus::Admitted { cgroup } = status else {
        panic!("{status:?}")
    };
    let mut command = spawn_in(&cgroup, "import time; time.sleep(30)");
    first.started("0-1", "same-id").await.unwrap();
    assert_eq!(
        second
            .submit("0-1", "same-id", 64 * 1024 * 1024)
            .await
            .unwrap(),
        CommandResourceStatus::Queued
    );
    let observer = second.clone();
    let abandoned = tokio::spawn(async move { observer.wait("0-1", "same-id").await });
    tokio::time::sleep(Duration::from_millis(50)).await;
    abandoned.abort();
    let _ = abandoned.await;
    assert_eq!(
        second.status("0-1", "same-id").await.unwrap(),
        CommandResourceStatus::Queued
    );
    assert!(second
        .submit("0-1", "same-id", 32 * 1024 * 1024)
        .await
        .is_err());
    command.kill().await.unwrap();
    let result = tokio::time::timeout(Duration::from_secs(5), second.wait("0-1", "same-id"))
        .await
        .unwrap()
        .unwrap();
    let CommandResourceStatus::Admitted {
        cgroup: second_group,
    } = result
    else {
        panic!("{result:?}")
    };
    assert_ne!(second_group, cgroup);
    let mut next = spawn_in(&second_group, "print('admitted without a model retry')");
    second.started("0-1", "same-id").await.unwrap();
    assert!(next.wait().await.unwrap().success());
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert_eq!(
        second.status("0-1", "same-id").await.unwrap(),
        CommandResourceStatus::Completed
    );
    server.abort();
    let _ = server.await;
}
