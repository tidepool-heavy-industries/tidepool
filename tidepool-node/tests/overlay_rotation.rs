#![cfg(target_os = "linux")]

use std::io::{BufRead, BufReader, Write};
use std::path::Path;
use std::process::{Child, ChildStdout, Command, Stdio};

use tidepool_node::{
    MountNamespace, OverlayRotation, OverlayRotationOutcome, ProcessInvocation,
    ProcessMountBoundary,
};

struct Worker {
    child: Child,
    output: BufReader<ChildStdout>,
}

impl Worker {
    fn exchange(&mut self, command: &str) -> String {
        writeln!(self.child.stdin.as_mut().unwrap(), "{command}").unwrap();
        let mut line = String::new();
        self.output.read_line(&mut line).unwrap();
        line.trim().to_owned()
    }
}

impl Drop for Worker {
    fn drop(&mut self) {
        drop(self.child.stdin.take());
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn setup(root: &Path) -> (Worker, MountNamespace) {
    for name in ["project", "base", "u0", "w0", "u1", "w1", "uc", "wc"] {
        std::fs::create_dir(root.join(name)).unwrap();
    }
    std::fs::write(root.join("base/value"), "inherited\n").unwrap();
    let project = root.join("project");
    let view = project.join("target");
    std::fs::create_dir(&view).unwrap();
    let boundary = ProcessMountBoundary::new(&project, [project.clone()], [project.clone()])
        .unwrap()
        .with_read_only_overlay(root, root)
        .unwrap()
        .with_build_overlay([root.join("base")], root.join("u0"), root.join("w0"), &view)
        .unwrap();
    let invocation = boundary.wrap(
        "bwrap",
        ProcessInvocation {
            program: "/bin/sh".into(),
            args: vec![
                "-c".into(),
                include_str!("fixtures/overlay_worker.sh").into(),
                "overlay-worker".into(),
                view.to_str().unwrap().into(),
            ],
        },
    );
    let mut child = Command::new(invocation.program)
        .args(invocation.args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .spawn()
        .unwrap();
    let output = BufReader::new(child.stdout.take().unwrap());
    let mut worker = Worker { child, output };
    let mut pid = String::new();
    worker.output.read_line(&mut pid).unwrap();
    let namespace = MountNamespace::capture(pid.trim().parse().unwrap()).unwrap();
    (worker, namespace)
}

#[test]
fn busy_freeze_preserves_worker_then_publication_allows_independent_continuation() {
    let storage = tempfile::tempdir().unwrap();
    let root = storage.path();
    let (mut worker, namespace) = setup(root);
    let view = root.join("project/target");
    let prepare = || {
        OverlayRotation::prepare(
            &view,
            &[root.join("base"), root.join("u0")],
            &root.join("u1"),
            &root.join("w1"),
        )
        .unwrap()
    };
    assert_eq!(worker.exchange("hold"), "held");
    assert!(matches!(
        namespace.rotate_overlay(prepare()),
        OverlayRotationOutcome::Busy
    ));
    assert_eq!(worker.exchange("write"), "wrote");
    assert_eq!(worker.exchange("close"), "closed");
    assert_eq!(worker.exchange("remember"), "remembered");
    let outcome = namespace.rotate_overlay(prepare());
    assert!(
        matches!(outcome, OverlayRotationOutcome::Rotated),
        "{outcome:?}"
    );
    assert_eq!(worker.exchange("relative"), "readonly");
    assert_eq!(worker.exchange("reenter"), "reentered");
    assert_eq!(worker.exchange("relative"), "wrote");
    let output = namespace
        .host_command(Path::new("/"), "/bin/sh".as_ref())
        .unwrap()
        .args(["-c", "printf forbidden >\"$1/probe\"", "probe"])
        .arg(root.join("u1"))
        .output()
        .unwrap();
    assert!(
        !output.status.success(),
        "the helper must not expose writable backing aliases"
    );
    assert!(!root.join("u1/probe").exists());
    assert_eq!(worker.exchange("write"), "wrote");
    assert!(root.join("u1/value").exists());
    assert_eq!(
        std::fs::read_to_string(root.join("u0/value")).unwrap(),
        "1\n"
    );
    let output = Command::new("bwrap")
        .args(["--bind", "/", "/", "--dev", "/dev", "--overlay-src"])
        .arg(root.join("base"))
        .arg("--overlay-src")
        .arg(root.join("u0"))
        .arg("--overlay")
        .arg(root.join("uc"))
        .arg(root.join("wc"))
        .arg(&view)
        .args([
            "--",
            "/bin/sh",
            "-c",
            "cat \"$1/value\"; printf child >\"$1/value\"",
            "child",
        ])
        .arg(&view)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(output.stdout, b"1\n");
    assert_eq!(
        std::fs::read_to_string(root.join("uc/value")).unwrap(),
        "child"
    );
    assert_eq!(worker.exchange("write"), "wrote");
    assert_eq!(
        std::fs::read_to_string(root.join("u1/value")).unwrap(),
        "3\n"
    );
    assert_eq!(worker.exchange("quit"), "");
    assert!(worker.child.wait().unwrap().success());
}

#[test]
fn failed_replacement_restores_the_original_writable_view() {
    let storage = tempfile::tempdir().unwrap();
    let root = storage.path();
    let (mut worker, namespace) = setup(root);
    let view = root.join("project/target");
    // A prepared backing directory can disappear before mount admission.
    let rotation = OverlayRotation::prepare(
        &view,
        &[root.join("base"), root.join("u0")],
        &root.join("u1"),
        &root.join("w1"),
    )
    .unwrap();
    std::fs::rename(root.join("u1"), root.join("retained-u1")).unwrap();
    let outcome = namespace.rotate_overlay(rotation);
    assert!(
        matches!(outcome, OverlayRotationOutcome::Restored(_)),
        "{outcome:?}"
    );
    assert_eq!(worker.exchange("write"), "wrote");
    assert_eq!(
        std::fs::read_to_string(root.join("u0/value")).unwrap(),
        "1\n"
    );
}

#[test]
fn ordinary_filesystem_is_never_frozen_as_an_overlay() {
    let storage = tempfile::tempdir().unwrap();
    let root = storage.path();
    let (mut worker, namespace) = setup(root);
    let rotation = OverlayRotation::prepare(
        &root.join("project"),
        &[root.join("base"), root.join("u0")],
        &root.join("u1"),
        &root.join("w1"),
    )
    .unwrap();
    let outcome = namespace.rotate_overlay(rotation);
    assert!(
        matches!(outcome, OverlayRotationOutcome::Unchanged(_)),
        "{outcome:?}"
    );
    assert_eq!(worker.exchange("write"), "wrote");
}
