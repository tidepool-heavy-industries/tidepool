//! Run with `just test-command-resources-delegated`; the target needs an
//! isolated delegated service cgroup with no other resident process.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "integration tests assert on known-good values; .clippy.toml allows this in test code"
)]
use exomonad_node::command_resources::{
    CommandActorIdentity, CommandResourceClient, CommandResourcePolicy, CommandResourceStatus,
    CommandResources, CommandRunIdentity,
};
use std::{path::Path, sync::Arc, time::Duration};
use tokio::process::{Child, Command};
fn spawn_in(path: &Path, script: &str) -> Child {
    use std::os::fd::AsRawFd;
    let join = std::fs::OpenOptions::new()
        .write(true)
        .open(path.join("cgroup.procs"))
        .unwrap();
    #[allow(clippy::disallowed_methods, reason = "test fixture process")]
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
async fn admitted(owner: &Arc<CommandResourceClient>, id: &str) -> std::path::PathBuf {
    owner
        .submit(&CommandActorIdentity::new(0, 1), id, 64 * 1024 * 1024)
        .await
        .unwrap();
    match owner
        .wait(&CommandActorIdentity::new(0, 1), id)
        .await
        .unwrap()
    {
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
    let owner = CommandResourceClient::local(
        owner,
        CommandRunIdentity::new("test-run".into(), 1).unwrap(),
    )
    .unwrap();
    let first = admitted(&owner, "first").await;
    let mut sibling = spawn_in(&first, "import time; time.sleep(30)");
    owner
        .started(&CommandActorIdentity::new(0, 1), "first")
        .await
        .unwrap();
    let second = admitted(&owner, "oom").await;
    let mut child = spawn_in(&second, "a=bytearray(128*1024*1024)");
    owner
        .started(&CommandActorIdentity::new(0, 1), "oom")
        .await
        .unwrap();
    assert!(!child.wait().await.unwrap().success());
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert!(matches!(
        owner
            .status(&CommandActorIdentity::new(0, 1), "oom")
            .await
            .unwrap(),
        CommandResourceStatus::ResourceExhausted
    ));
    assert!(sibling.try_wait().unwrap().is_none());
    let third = admitted(&owner, "third").await;
    let mut command = spawn_in(&third, "import time; time.sleep(30)");
    owner
        .started(&CommandActorIdentity::new(0, 1), "third")
        .await
        .unwrap();
    assert_eq!(
        owner
            .submit(
                &CommandActorIdentity::new(0, 1),
                "retained",
                64 * 1024 * 1024
            )
            .await
            .unwrap(),
        CommandResourceStatus::Queued
    );
    owner
        .cancel(&CommandActorIdentity::new(0, 1), "retained")
        .await
        .unwrap();
    let queued = owner.clone();
    let waiter = tokio::spawn(async move {
        {
            queued
                .submit(
                    &CommandActorIdentity::new(0, 1),
                    "cancelled",
                    64 * 1024 * 1024,
                )
                .await
                .unwrap();
            queued
                .wait(&CommandActorIdentity::new(0, 1), "cancelled")
                .await
                .unwrap()
        }
    });
    tokio::time::sleep(Duration::from_millis(30)).await;
    assert!(matches!(
        owner
            .cancel(&CommandActorIdentity::new(0, 1), "cancelled")
            .await
            .unwrap(),
        CommandResourceStatus::CancelledBeforeStart
    ));
    assert!(matches!(
        waiter.await.unwrap(),
        CommandResourceStatus::CancelledBeforeStart
    ));
    // Cancellation arriving first must prevent a later admission with this identity.
    owner
        .cancel(&CommandActorIdentity::new(0, 1), "cancel-first")
        .await
        .unwrap();
    assert!(matches!(
        owner
            .submit(
                &CommandActorIdentity::new(0, 1),
                "cancel-first",
                64 * 1024 * 1024
            )
            .await
            .unwrap(),
        CommandResourceStatus::CancelledBeforeStart
    ));
    // Losing an observing future does not discard accepted work.
    let abandoned_owner = owner.clone();
    let abandoned = tokio::spawn(async move {
        {
            abandoned_owner
                .submit(
                    &CommandActorIdentity::new(0, 1),
                    "abandoned",
                    64 * 1024 * 1024,
                )
                .await
                .unwrap();
            abandoned_owner
                .wait(&CommandActorIdentity::new(0, 1), "abandoned")
                .await
        }
    });
    tokio::time::sleep(Duration::from_millis(30)).await;
    abandoned.abort();
    assert!(abandoned.await.unwrap_err().is_cancelled());
    assert!(matches!(
        owner
            .status(&CommandActorIdentity::new(0, 1), "abandoned")
            .await
            .unwrap(),
        CommandResourceStatus::Queued
    ));
    owner
        .cancel(&CommandActorIdentity::new(0, 1), "abandoned")
        .await
        .unwrap();
    command.kill().await.unwrap();
    sibling.kill().await.unwrap();
    tokio::time::sleep(Duration::from_millis(200)).await;
    let background_group = admitted(&owner, "background").await;
    let mut background = spawn_in(
        &background_group,
        "import os,time; pid=os.fork(); time.sleep(2) if pid==0 else None",
    );
    owner
        .started(&CommandActorIdentity::new(0, 1), "background")
        .await
        .unwrap();
    assert!(background.wait().await.unwrap().success());
    assert!(
        background_group.exists(),
        "root exit must not retire a live descendant"
    );
    let occupied = admitted(&owner, "occupied").await;
    let mut occupied_child = spawn_in(&occupied, "import time; time.sleep(30)");
    owner
        .started(&CommandActorIdentity::new(0, 1), "occupied")
        .await
        .unwrap();
    assert_eq!(
        owner
            .submit(
                &CommandActorIdentity::new(0, 1),
                "descendant-wait",
                64 * 1024 * 1024
            )
            .await
            .unwrap(),
        CommandResourceStatus::Queued
    );
    owner
        .cancel(&CommandActorIdentity::new(0, 1), "descendant-wait")
        .await
        .unwrap();
    occupied_child.kill().await.unwrap();
    assert_eq!(
        owner
            .cancel(&CommandActorIdentity::new(0, 1), "background")
            .await
            .unwrap(),
        CommandResourceStatus::Running,
        "the resource owner retains custody until the descendant is gone"
    );
    tokio::time::timeout(Duration::from_secs(5), async {
        while !matches!(
            owner
                .status(&CommandActorIdentity::new(0, 1), "background")
                .await
                .unwrap(),
            CommandResourceStatus::Completed
        ) {
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await
    .unwrap();
    assert!(!background_group.exists());
    let final_group = admitted(&owner, "after").await;
    let mut final_command = spawn_in(&final_group, "print('control survived')");
    owner
        .started(&CommandActorIdentity::new(0, 1), "after")
        .await
        .unwrap();
    assert!(final_command.wait().await.unwrap().success());
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert!(matches!(
        owner
            .status(&CommandActorIdentity::new(0, 1), "after")
            .await
            .unwrap(),
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
    let owner = CommandResourceClient::local(
        owner,
        CommandRunIdentity::new("test-run".into(), 1).unwrap(),
    )
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
    use exomonad_node::command_resources::{service, CommandResourceClient};
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
    let first = CommandResourceClient::connect(
        socket.clone(),
        exomonad_node::command_resources::CommandRunIdentity::new("run-a".into(), 1).unwrap(),
        &policy,
    )
    .await
    .unwrap();
    let second = CommandResourceClient::connect(
        socket,
        exomonad_node::command_resources::CommandRunIdentity::new("run-b".into(), 1).unwrap(),
        &policy,
    )
    .await
    .unwrap();
    let status = first
        .submit(
            &exomonad_node::command_resources::CommandActorIdentity::new(0, 1),
            "same-id",
            64 * 1024 * 1024,
        )
        .await
        .unwrap();
    let CommandResourceStatus::Admitted { cgroup } = status else {
        panic!("{status:?}")
    };
    let mut command = spawn_in(&cgroup, "import time; time.sleep(30)");
    first
        .started(
            &exomonad_node::command_resources::CommandActorIdentity::new(0, 1),
            "same-id",
        )
        .await
        .unwrap();
    assert_eq!(
        second
            .submit(
                &exomonad_node::command_resources::CommandActorIdentity::new(0, 1),
                "same-id",
                64 * 1024 * 1024
            )
            .await
            .unwrap(),
        CommandResourceStatus::Queued
    );
    let observer = second.clone();
    let abandoned = tokio::spawn(async move {
        observer
            .wait(
                &exomonad_node::command_resources::CommandActorIdentity::new(0, 1),
                "same-id",
            )
            .await
    });
    tokio::time::sleep(Duration::from_millis(50)).await;
    abandoned.abort();
    // best-effort: aborted tasks resolve to a cancellation error, not a value we act on.
    abandoned.await.ok();
    assert_eq!(
        second
            .status(
                &exomonad_node::command_resources::CommandActorIdentity::new(0, 1),
                "same-id"
            )
            .await
            .unwrap(),
        CommandResourceStatus::Queued
    );
    assert!(second
        .submit(
            &exomonad_node::command_resources::CommandActorIdentity::new(0, 1),
            "same-id",
            32 * 1024 * 1024
        )
        .await
        .is_err());
    command.kill().await.unwrap();
    let result = tokio::time::timeout(
        Duration::from_secs(5),
        second.wait(
            &exomonad_node::command_resources::CommandActorIdentity::new(0, 1),
            "same-id",
        ),
    )
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
    second
        .started(
            &exomonad_node::command_resources::CommandActorIdentity::new(0, 1),
            "same-id",
        )
        .await
        .unwrap();
    assert!(next.wait().await.unwrap().success());
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert_eq!(
        second
            .status(
                &exomonad_node::command_resources::CommandActorIdentity::new(0, 1),
                "same-id"
            )
            .await
            .unwrap(),
        CommandResourceStatus::Completed
    );
    server.abort();
    // best-effort: aborted tasks resolve to a cancellation error, not a value we act on.
    server.await.ok();
}
