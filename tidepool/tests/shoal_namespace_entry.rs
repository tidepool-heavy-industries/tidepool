//! Exercise the actual pre-runtime entry executable without a provider or TUI.
use std::os::unix::fs::PermissionsExt;
use std::process::Command;
use std::time::{Duration, Instant};

use tidepool_node::ProcessMountBoundary;

#[test]
fn executable_enters_retained_view_without_runtime_or_mount_privileges() {
    let storage = tempfile::tempdir().unwrap();
    let root = storage.path();
    for name in ["base", "upper", "work", "view"] {
        std::fs::create_dir(root.join(name)).unwrap();
    }
    std::fs::write(root.join("base/marker"), "inherited\n").unwrap();
    let view = root.join("view");
    let boundary = ProcessMountBoundary::new(&view, [root.to_owned()], [view.clone()])
        .unwrap()
        .with_overlay_view(
            [root.join("base")],
            root.join("upper"),
            root.join("work"),
            &view,
        )
        .unwrap();
    let namespace = boundary
        .prepare_view("bwrap", Instant::now() + Duration::from_secs(10))
        .unwrap();
    assert!(namespace.require_live_owner().is_err());
    assert_eq!(
        boundary
            .prepare_view("false", Instant::now() + Duration::from_secs(10))
            .unwrap_err()
            .kind(),
        std::io::ErrorKind::UnexpectedEof
    );
    assert_eq!(
        boundary
            .prepare_view("/no-bootstrap-should-spawn", Instant::now())
            .unwrap_err()
            .kind(),
        std::io::ErrorKind::TimedOut
    );
    let wrong_owner = root.join("wrong-owner");
    std::fs::write(
        &wrong_owner,
        "#!/bin/sh\nprintf '%s\\n' \"$PPID\"; read release\n",
    )
    .unwrap();
    std::fs::set_permissions(&wrong_owner, std::fs::Permissions::from_mode(0o700)).unwrap();
    assert_eq!(
        boundary
            .prepare_view(
                wrong_owner.to_string_lossy().into_owned(),
                Instant::now() + Duration::from_secs(10)
            )
            .unwrap_err()
            .to_string(),
        "view bootstrap is not the owned monitor's child"
    );
    let entry = namespace.entry().unwrap();
    let run = |entry: &str, script: &str| {
        Command::new(env!("CARGO_BIN_EXE_shoal"))
            .args(["enter-view", "--view", entry, "--cwd"])
            .arg(&view)
            .args(["--", "/bin/sh", "-c", script])
            .output()
            .unwrap()
    };
    let output = run(
        &serde_json::to_string(&entry).unwrap(),
        "cat marker; printf child > child; cat /proc/self/status",
    );
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let output = String::from_utf8(output.stdout).unwrap();
    assert!(output.starts_with("inherited\n"));
    for field in ["CapInh:", "CapPrm:", "CapEff:", "CapAmb:"] {
        let value = output
            .lines()
            .find_map(|line| line.strip_prefix(field))
            .unwrap();
        assert_eq!(value.trim(), "0000000000000000", "{field}");
    }
    assert!(output.lines().any(|line| line == "NoNewPrivs:\t1"));
    assert_eq!(std::fs::read(root.join("upper/child")).unwrap(), b"child");
    assert!(!view.join("child").exists());
    let mut expired = serde_json::to_value(entry).unwrap();
    expired["start_ticks"] = 0.into();
    assert!(!run(&expired.to_string(), "touch forbidden")
        .status
        .success());
    assert!(!namespace.try_exists(&view.join("forbidden")).unwrap());
}

#[test]
fn executable_rejects_uncontained_payload_before_execution() {
    let marker = tempfile::tempdir().unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_shoal"))
        .args([
            "in-slice",
            "--slice",
            "shoal-unconfigured-test.slice",
            "--",
            "/bin/sh",
            "-c",
            "touch \"$1\"",
            "test",
        ])
        .arg(marker.path().join("must-not-exist"))
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("outside required slice"));
    assert!(!marker.path().join("must-not-exist").exists());
}

#[tokio::test]
#[ignore = "requires a configured user swarm.slice and tmux"]
async fn executable_enters_slice_through_outside_tmux_server() {
    use tidepool_node::systemd_slice::SystemdSlice;
    use tidepool_node::{ProcessInvocation, TmuxLaunch, TmuxSession};
    let slice = SystemdSlice::default();
    let limits = slice.inspect().await.unwrap();
    assert!(limits.memory_max > 0);
    // An isolated server starts outside the selected slice, just like an existing
    // operator server. Both scoped windows must explicitly enter the budget.
    assert!(slice.current_membership().is_err());
    let storage = tempfile::tempdir().unwrap();
    let socket = format!("shoal-slice-test-{}", uuid::Uuid::new_v4().simple());
    struct Server(String);
    impl Drop for Server {
        fn drop(&mut self) {
            let _ = Command::new("tmux")
                .args(["-L", &self.0, "kill-server"])
                .output();
        }
    }
    let _server = Server(socket.clone());
    let tmux = TmuxSession::with_socket("slice-acceptance", socket).unwrap();
    let seed = tmux
        .create(&TmuxLaunch {
            window_name: "outside".into(),
            cwd: storage.path().into(),
            program: "/bin/sh".into(),
            args: vec!["-c".into(), "exec sleep 120".into()],
            environment: Default::default(),
            unset_environment: Default::default(),
        })
        .await
        .unwrap();
    let executable = std::path::Path::new(env!("CARGO_BIN_EXE_shoal"));
    for name in ["first", "second"] {
        let launch = slice.scope(slice.verified_command(
            executable,
            ProcessInvocation {
                program: "/bin/sh".into(),
                args: vec![
                    "-c".into(),
                    "cat /proc/self/cgroup > \"$1\"; printf '%s' \"$2\" > \"$1.args\"".into(),
                    "test".into(),
                    storage.path().join(name).display().to_string(),
                    "$HOME; literal value".into(),
                ],
            },
        ));
        tmux.spawn_window(&TmuxLaunch {
            window_name: name.into(),
            cwd: storage.path().into(),
            program: launch.program,
            args: launch.args,
            environment: Default::default(),
            unset_environment: Default::default(),
        })
        .await
        .unwrap();
    }
    let observed = tokio::time::timeout(Duration::from_secs(20), async {
        loop {
            if ["first", "second"].iter().all(|name| {
                std::fs::read_to_string(storage.path().join(format!("{name}.args")))
                    .is_ok_and(|value| value == "$HOME; literal value")
            }) {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await;
    let outside = tmux.pane_status(&seed).await;
    tmux.kill().await.unwrap();
    observed.expect("both independently scoped payloads must finish");
    assert!(outside.unwrap().is_some_and(|status| !status.dead));
    for name in ["first", "second"] {
        let group = std::fs::read_to_string(storage.path().join(name)).unwrap();
        let path = group.trim().strip_prefix("0::").unwrap();
        assert!(slice.contains(std::path::Path::new(path)), "{group}");
        assert_eq!(
            std::fs::read_to_string(storage.path().join(format!("{name}.args"))).unwrap(),
            "$HOME; literal value"
        );
    }
}
