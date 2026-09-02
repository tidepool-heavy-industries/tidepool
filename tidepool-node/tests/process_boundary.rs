#![cfg(target_os = "linux")]

use std::process::Command;

use tidepool_node::{ProcessInvocation, ProcessMountBoundary};

#[test]
fn actor_repository_is_writable_while_source_and_siblings_are_read_only() {
    let root = tempfile::tempdir().expect("temp root");
    let source = root.path().join("source");
    let workers = root.path().join("workers");
    let actor = workers.join("actor");
    let sibling = workers.join("sibling");
    std::fs::create_dir_all(&source).expect("create source");
    git(root.path(), ["init", source.to_str().unwrap()]);
    git(&source, ["config", "user.name", "Source"]);
    git(&source, ["config", "user.email", "source@example.invalid"]);
    std::fs::write(source.join("base.txt"), "base\n").expect("write base");
    git(&source, ["add", "base.txt"]);
    git(&source, ["commit", "-m", "base"]);
    std::fs::create_dir_all(&workers).expect("create worker root");
    git(
        &workers,
        [
            "clone",
            "--shared",
            source.to_str().unwrap(),
            actor.to_str().unwrap(),
        ],
    );
    git(
        &workers,
        [
            "clone",
            "--shared",
            source.to_str().unwrap(),
            sibling.to_str().unwrap(),
        ],
    );

    let boundary =
        ProcessMountBoundary::new(&actor, [source.clone(), workers.clone()], [actor.clone()])
            .expect("boundary");
    let script = format!(
        "git config user.name Actor && \
         git config user.email actor@example.invalid && \
         printf 'candidate\\n' > candidate.txt && \
         git add candidate.txt && git commit -m candidate && \
         ! git -C {source} config tidepool.escaped yes && \
         ! touch {sibling}/escaped",
        source = source.display(),
        sibling = sibling.display(),
    );
    let invocation = boundary.wrap(
        "bwrap",
        ProcessInvocation {
            program: "/bin/sh".into(),
            args: vec!["-c".into(), script],
        },
    );
    let output = Command::new(&invocation.program)
        .args(&invocation.args)
        .output()
        .expect("run bubblewrap");

    assert!(
        output.status.success(),
        "bubblewrap probe failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(actor.join("candidate.txt").exists());
    assert_eq!(
        Command::new("git")
            .args(["-C", actor.to_str().unwrap(), "log", "-1", "--format=%s"])
            .output()
            .unwrap()
            .stdout,
        b"candidate\n"
    );
    assert!(!Command::new("git")
        .args([
            "-C",
            source.to_str().unwrap(),
            "config",
            "--get",
            "tidepool.escaped"
        ])
        .status()
        .unwrap()
        .success());
    assert!(!sibling.join("escaped").exists());
}

fn git<const N: usize>(cwd: &std::path::Path, args: [&str; N]) {
    let output = Command::new("git")
        .current_dir(cwd)
        .args(args)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "git failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}
