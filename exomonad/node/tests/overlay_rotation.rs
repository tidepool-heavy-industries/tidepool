#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "integration tests assert on known-good values; .clippy.toml allows this in test code"
)]
#![cfg(target_os = "linux")]

use std::io::{BufRead, BufReader, Write};
use std::path::Path;
use std::process::{Child, ChildStdout, Command, Stdio};

use exomonad_node::{
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
        // best-effort: Drop cannot propagate; the worker may already have exited.
        self.child.kill().ok();
        self.child.wait().ok();
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
    #[allow(clippy::disallowed_methods, reason = "test fixture process")]
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

/// Exercises the production helper with byte arguments and environment values.
/// Build `exomonad-view-helper` beside this target before executing this test.
#[test]
fn view_helper_preserves_raw_arguments_environment_cwd_and_streams() {
    use std::ffi::{OsStr, OsString};
    use std::os::unix::ffi::OsStringExt;

    let storage = tempfile::tempdir().unwrap();
    let (_worker, namespace) = setup(storage.path());
    let directory = storage.path().join("project/target");
    let raw = OsString::from_vec(vec![b'a', 0xff, b' ', b'\n']);
    let output = exomonad_node::view_command::output_in_view(
        &namespace,
        &directory,
        OsStr::new("/bin/sh"),
        &[
            "-c".into(),
            include_str!("fixtures/view_command_contract.sh").into(),
            "view-contract".into(),
            raw.clone(),
        ],
        &[
            ("VIEW_CONTRACT_VALUE".into(), Some(raw)),
            ("HOME".into(), None),
        ],
    )
    .unwrap();
    assert!(output.status.success(), "{output:?}");
    let mut expected = directory.as_os_str().as_encoded_bytes().to_vec();
    expected.push(0);
    expected.extend_from_slice(b"a\xff \n\0a\xff \n\0unset\0eof\0");
    assert_eq!(output.stdout, expected);
    assert_eq!(output.stderr, b"separate stderr\n");
}

#[test]
fn view_helper_distinguishes_setup_failure_from_payload_exit_and_signal() {
    use std::ffi::OsStr;
    use std::os::unix::process::ExitStatusExt;

    let storage = tempfile::tempdir().unwrap();
    let (_worker, namespace) = setup(storage.path());
    let run = |directory: &Path, program: &str, args: &[std::ffi::OsString]| {
        exomonad_node::view_command::output_in_view(
            &namespace,
            directory,
            OsStr::new(program),
            args,
            &[],
        )
    };
    assert!(run(Path::new("/"), "/missing-view-contract-program", &[]).is_err());
    assert!(run(&storage.path().join("absent"), "/bin/sh", &[]).is_err());
    let exited = run(Path::new("/"), "/bin/sh", &["-c".into(), "exit 127".into()]).unwrap();
    assert_eq!(exited.status.code(), Some(127));
    let signalled = run(
        Path::new("/"),
        "/bin/sh",
        &["-c".into(), "kill -TERM $$".into()],
    )
    .unwrap();
    assert_eq!(signalled.status.signal(), Some(libc::SIGTERM));
    // Failed setup and a signalled payload must not poison the retained view.
    let recovered = run(
        Path::new("/"),
        "/bin/sh",
        &["-c".into(), "printf alive".into()],
    )
    .unwrap();
    assert!(recovered.status.success());
    assert_eq!(recovered.stdout, b"alive");
}

#[test]
fn view_helper_acquisition_retains_its_view_and_rejects_expired_descriptors() {
    use std::ffi::OsStr;

    let storage = tempfile::tempdir().unwrap();
    let (worker, namespace) = setup(storage.path());
    let entry = namespace.entry().unwrap();
    // The captured descriptors own the view even after its original process
    // has exited and been reaped.
    drop(worker);
    let acquired = entry.acquire().unwrap();
    drop(namespace);
    assert!(
        entry.acquire().is_err(),
        "expired descriptor references must fail closed"
    );
    let output = exomonad_node::view_command::output_in_view(
        &acquired,
        &storage.path().join("project/target"),
        OsStr::new("/bin/sh"),
        &["-c".into(), "cat value".into()],
        &[],
    )
    .unwrap();
    assert!(output.status.success(), "{output:?}");
    assert_eq!(output.stdout, b"inherited\n");
}

/// Manual paired measurement. Build `exomonad-view-helper` first, then run
/// this ignored test once without tracing for latency and once under `strace`
/// for clone/exec evidence. `VIEW_SPAWN_BENCH_RSS_MIB` adds touched host RSS.
#[test]
#[ignore = "manual retained-view spawn measurement"]
fn compare_host_fork_with_view_helper_spawn() {
    use std::ffi::OsStr;
    use std::time::Instant;

    let storage = tempfile::tempdir().unwrap();
    let (_worker, namespace) = setup(storage.path());
    let rss_mib: usize = std::env::var("VIEW_SPAWN_BENCH_RSS_MIB")
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(0);
    let pairs: usize = std::env::var("VIEW_SPAWN_BENCH_PAIRS")
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(100);
    assert!(pairs > 0);
    let mut host_memory = vec![0_u8; rss_mib * 1024 * 1024];
    for page in host_memory.chunks_mut(4096) {
        page[0] = 1;
    }
    std::hint::black_box(&host_memory);

    let run = |helper: bool| {
        let started = Instant::now();
        let output = if helper {
            exomonad_node::view_command::output_in_view(
                &namespace,
                Path::new("/"),
                OsStr::new("/bin/sh"),
                &["-c".into(), ":".into()],
                &[],
            )
            .unwrap()
        } else {
            namespace
                .host_command(Path::new("/"), OsStr::new("/bin/sh"))
                .unwrap()
                .args(["-c", ":"])
                .output()
                .unwrap()
        };
        assert!(output.status.success(), "{output:?}");
        started.elapsed().as_nanos()
    };
    let warmups = if pairs >= 20 { 20 } else { 0 };
    for index in 0..warmups {
        run(index % 2 == 0);
        run(index % 2 != 0);
    }
    let mut legacy = Vec::with_capacity(pairs);
    let mut helper = Vec::with_capacity(pairs);
    for index in 0..pairs {
        if index % 2 == 0 {
            legacy.push(run(false));
            helper.push(run(true));
        } else {
            helper.push(run(true));
            legacy.push(run(false));
        }
    }
    let summarize = |mut samples: Vec<u128>| {
        samples.sort_unstable();
        (
            samples[(samples.len() - 1) / 2],
            samples[(samples.len() - 1) * 95 / 100],
        )
    };
    eprintln!(
        "view spawn rss_mib={rss_mib} pairs={pairs} legacy_p50_p95_ns={:?} helper_p50_p95_ns={:?}",
        summarize(legacy),
        summarize(helper)
    );
    std::hint::black_box(host_memory);
}

#[tokio::test]
#[ignore = "requires a fresh delegated systemd cgroup scope"]
async fn command_oom_releases_writers_for_cow_publication() {
    use exomonad_node::command_resources::{
        CommandResourcePolicy, CommandResourceStatus, CommandResources,
    };
    let owner = CommandResources::delegated(CommandResourcePolicy {
        general_bytes: 512 * 1024 * 1024,
        protected_bytes: 0,
        swap_max_bytes: 0,
        ..Default::default()
    })
    .unwrap();
    let storage = tempfile::tempdir().unwrap();
    let root = storage.path();
    let (mut worker, namespace) = setup(root);
    owner.submit("overlay", "oom", 64 * 1024 * 1024).unwrap();
    let CommandResourceStatus::Admitted { cgroup } = owner.wait("overlay", "oom").await.unwrap()
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
    #[allow(clippy::disallowed_methods, reason = "test fixture process")]
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
    #[allow(clippy::disallowed_methods, reason = "test fixture process")]
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
        .with_read_only_overlay(root.join("config"), project.join(".exomonad"))
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
    // Omitting the current upper omits the .exomonad mountpoint that bwrap
    // created there. Failure while assembling nested mounts must roll back.
    let incomplete = OverlayRotation::prepare(
        &project,
        &[root.join("base")],
        &root.join("u1"),
        &root.join("w1"),
    )
    .unwrap()
    .preserving_mounts(&[project.join("target"), project.join(".exomonad")])
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
    .preserving_mounts(&[project.join("target"), project.join(".exomonad")])
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
            "cat \"$1/target/artifact\" \"$1/.exomonad/prompt\"; printf new >\"$1/target/new\"",
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
