use super::*;
use crate::{ProcessInvocation, ProcessMountBoundary};
use std::io::{BufRead, BufReader, Write};
use std::process::{Child, ChildStdout, Command};

struct Worker {
    child: Child,
    output: BufReader<ChildStdout>,
}

impl Worker {
    fn exchange(&mut self, command: &str) -> String {
        writeln!(self.child.stdin.as_mut().unwrap(), "{command}").unwrap();
        let mut line = String::new();
        self.output.read_line(&mut line).unwrap();
        line.trim().into()
    }
}

impl Drop for Worker {
    fn drop(&mut self) {
        drop(self.child.stdin.take());
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn fixture(root: &Path) -> (Worker, MountNamespace, OverlayRotation) {
    // Exercise both mountinfo field escapes and option delimiters in real paths.
    let backing = root.join("storage ,colon: and\\slash");
    for name in ["base", "old-upper", "old-work", "new-upper", "new-work"] {
        std::fs::create_dir_all(backing.join(name)).unwrap();
    }
    let project = root.join("project with spaces");
    let target = project.join("target");
    std::fs::create_dir_all(&target).unwrap();
    let boundary = ProcessMountBoundary::new(&project, [project.clone()], [project.clone()])
        .unwrap()
        .with_read_only_overlay(&backing, &backing)
        .unwrap()
        .with_overlay_view(
            [backing.join("base")],
            backing.join("old-upper"),
            backing.join("old-work"),
            &target,
        )
        .unwrap();
    let invocation = boundary.wrap(
        "bwrap",
        ProcessInvocation {
            program: "/bin/sh".into(),
            args: vec![
                "-c".into(),
                include_str!("../../../tests/fixtures/overlay_worker.sh").into(),
                "worker".into(),
                target.to_str().unwrap().into(),
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
    let mut output = BufReader::new(child.stdout.take().unwrap());
    let mut pid = String::new();
    output.read_line(&mut pid).unwrap();
    let namespace = MountNamespace::capture(pid.trim().parse().unwrap()).unwrap();
    let rotation = OverlayRotation::prepare(
        &target,
        &[backing.join("base"), backing.join("old-upper")],
        &backing.join("new-upper"),
        &backing.join("new-work"),
    )
    .unwrap();
    (Worker { child, output }, namespace, rotation)
}

#[test]
fn lost_receipt_after_publication_recovers_the_exact_replacement() {
    let directory = tempfile::tempdir().unwrap();
    let (mut worker, namespace, rotation) = fixture(directory.path());
    assert_eq!(worker.exchange("write"), "wrote");
    let prepared = namespace
        .prepare_overlay_rotation(rotation.clone())
        .unwrap();
    let (recovery, transition) = prepared.apply();
    assert!(
        matches!(transition, OverlayRotationOutcome::Rotated),
        "{transition:?}"
    );
    let installed = namespace.observe_overlay(&rotation.target).unwrap().id;
    let result = recovery.reconcile();
    assert!(
        matches!(result, OverlayRotationOutcome::Rotated),
        "{result:?}"
    );
    assert_eq!(
        namespace.observe_overlay(&rotation.target).unwrap().id,
        installed
    );
    assert!(matches!(
        recovery.reconcile(),
        OverlayRotationOutcome::Rotated
    ));
    assert_eq!(
        namespace.observe_overlay(&rotation.target).unwrap().id,
        installed
    );
    assert_eq!(worker.exchange("write"), "wrote");
    let replacement = Path::new(std::ffi::OsStr::from_bytes(rotation.upper.as_bytes()));
    assert_eq!(
        std::fs::read_to_string(replacement.join("value")).unwrap(),
        "2\n"
    );
}

#[test]
fn interrupted_freeze_restores_only_the_original_mount() {
    let directory = tempfile::tempdir().unwrap();
    let (mut worker, namespace, rotation) = fixture(directory.path());
    let before = namespace.observe_overlay(&rotation.target).unwrap();
    let prepared = namespace
        .prepare_overlay_rotation(rotation.clone())
        .unwrap();
    let recovery = prepared.recovery;
    let target = rotation.target.clone();
    // SAFETY: preconstructed path and mount syscall only after fork.
    let mut freeze = unsafe {
        namespace
            .command_with_setup(
                Path::new("/"),
                "/bin/sh".as_ref(),
                OwnerRequirement::LiveProcess,
                move || {
                    rustix::mount::mount_remount(target.as_c_str(), MountFlags::RDONLY, c"")?;
                    Ok(())
                },
            )
            .unwrap()
    };
    assert!(freeze.args(["-c", ":"]).status().unwrap().success());
    assert!(
        namespace
            .observe_overlay(&rotation.target)
            .unwrap()
            .readonly
    );
    std::fs::remove_dir(Path::new(std::ffi::OsStr::from_bytes(
        rotation.upper.as_bytes(),
    )))
    .unwrap();
    let result = recovery.reconcile();
    assert!(
        matches!(result, OverlayRotationOutcome::RecoveredOriginal),
        "{result:?}"
    );
    assert_eq!(worker.exchange("write"), "wrote");
    // Already-writable recovery must not rotate or allocate another mount.
    let result = recovery.reconcile();
    assert!(matches!(result, OverlayRotationOutcome::RecoveredOriginal));
    assert_eq!(
        namespace.observe_overlay(&rotation.target).unwrap().id,
        before.id
    );
}

#[test]
fn unexpected_replacement_is_not_thawed_or_reported_as_published() {
    let directory = tempfile::tempdir().unwrap();
    let (mut worker, namespace, rotation) = fixture(directory.path());
    let before = namespace.observe_overlay(&rotation.target).unwrap();
    assert!(matches!(
        namespace.rotate_overlay_inner(rotation.clone()).unwrap(),
        OverlayRotationOutcome::Rotated
    ));
    let installed = namespace.observe_overlay(&rotation.target).unwrap().id;
    let mut foreign_recipe = rotation.clone();
    foreign_recipe.upper_option = CString::new("/another-owner/upper").unwrap();
    let result = namespace.settle_rotation(
        &foreign_recipe,
        &before,
        OverlayRotationOutcome::Unconfirmed("uncertain replacement identity".into()),
    );
    assert!(
        matches!(result, OverlayRotationOutcome::Unconfirmed(_)),
        "{result:?}"
    );
    assert_eq!(
        namespace.observe_overlay(&rotation.target).unwrap().id,
        installed
    );
    assert_eq!(worker.exchange("write"), "wrote");
}
