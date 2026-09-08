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
