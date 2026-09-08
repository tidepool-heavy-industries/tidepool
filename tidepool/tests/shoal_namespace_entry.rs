//! Exercise the actual pre-runtime entry executable without a provider or TUI.
use std::io::{BufRead, BufReader};
use std::process::{Child, Command, Stdio};

use tidepool_node::{MountNamespace, ProcessInvocation, ProcessMountBoundary};

struct Bootstrap(Child);
impl Drop for Bootstrap {
    fn drop(&mut self) {
        drop(self.0.stdin.take());
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

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
    let invocation = boundary.wrap(
        "bwrap",
        ProcessInvocation {
            program: "/bin/sh".into(),
            args: vec![
                "-c".into(),
                "printf '%s\n' \"$$\"; read line || exit 0".into(),
            ],
        },
    );
    let mut bootstrap = Bootstrap(
        Command::new(invocation.program)
            .args(invocation.args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .unwrap(),
    );
    let mut pid = String::new();
    BufReader::new(bootstrap.0.stdout.take().unwrap())
        .read_line(&mut pid)
        .unwrap();
    let namespace = MountNamespace::capture(pid.trim().parse().unwrap()).unwrap();
    let entry = namespace.entry().unwrap();
    drop(bootstrap.0.stdin.take());
    assert!(bootstrap.0.wait().unwrap().success());
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
