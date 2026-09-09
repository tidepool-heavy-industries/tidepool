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
        .with_overlay_view([root.join("base")], root.join("u0"), root.join("w0"), &view)
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
    spawn_worker(invocation)
}

fn spawn_worker(invocation: ProcessInvocation) -> (Worker, MountNamespace) {
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

#[tokio::test]
#[ignore = "requires a fresh delegated systemd cgroup scope"]
async fn command_oom_releases_writers_for_cow_publication() {
    use tidepool_node::command_resources::{
        CommandResourcePolicy, CommandResourceStatus, CommandResources,
    };
    let owner = CommandResources::delegated(CommandResourcePolicy {
        memory_high_bytes: None,
        memory_max_bytes: 64 * 1024 * 1024,
        swap_max_bytes: 0,
        ..Default::default()
    })
    .unwrap();
    let storage = tempfile::tempdir().unwrap();
    let root = storage.path();
    let (mut worker, namespace) = setup(root);
    let CommandResourceStatus::Admitted { cgroup } = owner.acquire("overlay", "oom").await.unwrap()
    else {
        panic!("expected command admission");
    };
    owner.started("overlay", "oom").unwrap();
    assert_eq!(
        worker.exchange(&format!("oom {}", cgroup.display())),
        "command-failed"
    );
    assert!(matches!(
        owner.status("overlay", "oom").unwrap(),
        CommandResourceStatus::ResourceExhausted
    ));
    let view = root.join("project/target");
    let rotation = OverlayRotation::prepare(
        &view,
        &[root.join("base"), root.join("u0")],
        &root.join("u1"),
        &root.join("w1"),
    )
    .unwrap();
    assert!(matches!(
        namespace.rotate_overlay(rotation),
        OverlayRotationOutcome::Rotated
    ));
    let child = Command::new("bwrap")
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
            "cat \"$1/oom-value\"; printf child >\"$1/oom-value\"",
            "child",
        ])
        .arg(&view)
        .output()
        .unwrap();
    assert!(
        child.status.success(),
        "{}",
        String::from_utf8_lossy(&child.stderr)
    );
    assert_eq!(child.stdout, b"before-oom");
    assert_eq!(
        std::fs::read(root.join("u0/oom-value")).unwrap(),
        b"before-oom"
    );
    assert_eq!(std::fs::read(root.join("uc/oom-value")).unwrap(), b"child");
    assert_eq!(worker.exchange("write"), "wrote");
    assert!(root.join("u1/value").exists());
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

#[test]
fn source_rotation_preserves_live_build_mount_config_and_root_metadata() {
    use std::os::unix::fs::MetadataExt;

    let storage = tempfile::tempdir().unwrap();
    let root = storage.path();
    for name in [
        "project",
        "base",
        "u0",
        "w0",
        "u1",
        "w1",
        "build-base",
        "build-upper",
        "build-work",
        "config",
    ] {
        std::fs::create_dir(root.join(name)).unwrap();
    }
    std::fs::create_dir(root.join("base/target")).unwrap();
    std::fs::write(root.join("base/value"), "source").unwrap();
    std::fs::write(root.join("build-base/artifact"), "warm").unwrap();
    std::fs::write(root.join("config/prompt"), "canonical").unwrap();
    rustix::fs::setxattr(
        root.join("u0"),
        "user.snapshot-contract",
        b"kept",
        rustix::fs::XattrFlags::empty(),
    )
    .unwrap();
    rustix::fs::setxattr(
        root.join("u1"),
        "user.storage-only",
        b"remove",
        rustix::fs::XattrFlags::empty(),
    )
    .unwrap();
    let project = root.join("project");
    let boundary = ProcessMountBoundary::new(&project, [project.clone()], [project.clone()])
        .unwrap()
        .with_read_only_overlay(root, root)
        .unwrap()
        .with_read_only_overlay(root.join("config"), project.join(".shoal"))
        .unwrap()
        .with_overlay_view(
            [root.join("build-base")],
            root.join("build-upper"),
            root.join("build-work"),
            project.join("target"),
        )
        .unwrap()
        .with_overlay_view(
            [root.join("base")],
            root.join("u0"),
            root.join("w0"),
            &project,
        )
        .unwrap();
    let invocation = boundary.wrap(
        "bwrap",
        ProcessInvocation {
            program: "/bin/sh".into(),
            args: vec![
                "-c".into(),
                include_str!("fixtures/overlay_worker.sh").into(),
                "worker".into(),
                project.to_str().unwrap().into(),
            ],
        },
    );
    let (mut worker, namespace) = spawn_worker(invocation);
    let output = namespace
        .host_command(Path::new("/"), "/bin/sh".as_ref())
        .unwrap()
        .args([
            "-c",
            "chmod 751 \"$1\"; touch -d @1234567890 \"$1\"",
            "metadata",
        ])
        .arg(&project)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(worker.exchange("hold_build"), "held");
    let before = std::fs::metadata(root.join("u0")).unwrap();
    // Omitting the current upper omits the .shoal mountpoint that bwrap
    // created there. Failure while assembling nested mounts must roll back.
    let incomplete = OverlayRotation::prepare(
        &project,
        &[root.join("base")],
        &root.join("u1"),
        &root.join("w1"),
    )
    .unwrap()
    .preserving_mounts(&[project.join("target"), project.join(".shoal")])
    .unwrap();
    let failed = namespace.rotate_overlay(incomplete);
    assert!(
        matches!(failed, OverlayRotationOutcome::Restored(_)),
        "{failed:?}"
    );
    assert_eq!(worker.exchange("build_write"), "wrote");

    let rotation = OverlayRotation::prepare(
        &project,
        &[root.join("base"), root.join("u0")],
        &root.join("u1"),
        &root.join("w1"),
    )
    .unwrap()
    .preserving_mounts(&[project.join("target"), project.join(".shoal")])
    .unwrap();
    let outcome = namespace.rotate_overlay(rotation);
    assert!(
        matches!(outcome, OverlayRotationOutcome::Rotated),
        "{outcome:?}"
    );
    let after = std::fs::metadata(root.join("u1")).unwrap();
    assert_eq!((after.uid(), after.gid()), (before.uid(), before.gid()));
    let mut attribute = [0; 16];
    let count = rustix::fs::getxattr(
        root.join("u1"),
        "user.snapshot-contract",
        &mut attribute[..],
    )
    .unwrap();
    assert_eq!(&attribute[..count], b"kept");
    assert_eq!(
        rustix::fs::getxattr(root.join("u1"), "user.storage-only", &mut attribute[..]),
        Err(rustix::io::Errno::NODATA)
    );
    assert_eq!(
        (after.mode(), after.mtime(), after.mtime_nsec()),
        (before.mode(), before.mtime(), before.mtime_nsec())
    );
    let output = namespace
        .host_command(Path::new("/"), "/bin/sh".as_ref())
        .unwrap()
        .args([
            "-c",
            "cat \"$1/target/artifact\" \"$1/.shoal/prompt\"; printf new >\"$1/target/new\"",
            "probe",
        ])
        .arg(&project)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(output.stdout, b"warmcanonical");
    assert_eq!(worker.exchange("build_write"), "wrote");
    assert_eq!(
        std::fs::read_to_string(root.join("build-upper/open")).unwrap(),
        "livelive"
    );
    assert_eq!(
        std::fs::read_to_string(root.join("build-upper/new")).unwrap(),
        "new"
    );
    assert!(!root.join("u1/target/new").exists());
    assert_eq!(worker.exchange("close_build"), "closed");
}
