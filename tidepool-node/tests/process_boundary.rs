#![cfg(target_os = "linux")]

use std::process::Command;

use tidepool_node::{ProcessInvocation, ProcessMountBoundary};

#[test]
fn linked_actor_worktree_commits_into_shared_git_namespace() {
    let root = tempfile::tempdir().expect("temp root");
    let source = root.path().join("source");
    let workers = root.path().join("workers");
    let actor = workers.join("actor");
    let sibling = workers.join("sibling");
    let project_root = root.path().join("actor-project");
    std::fs::create_dir_all(&source).expect("create source");
    git(root.path(), ["init", source.to_str().unwrap()]);
    git(&source, ["config", "user.name", "Source"]);
    git(&source, ["config", "user.email", "source@example.invalid"]);
    std::fs::write(source.join("base.txt"), "base\n").expect("write base");
    git(&source, ["add", "base.txt"]);
    git(&source, ["commit", "-m", "base"]);
    std::fs::create_dir_all(&workers).expect("create worker root");
    std::fs::create_dir_all(&project_root).expect("create project root");
    git(
        &source,
        ["worktree", "add", "-b", "actor", actor.to_str().unwrap()],
    );
    git(
        &source,
        [
            "worktree",
            "add",
            "-b",
            "sibling",
            sibling.to_str().unwrap(),
        ],
    );
    let common_git = source.join(".git");

    let boundary = ProcessMountBoundary::new(
        &actor,
        [source.clone(), workers.clone(), common_git.clone()],
        [actor.clone(), common_git],
    )
    .expect("boundary")
    .with_project_root(&project_root)
    .expect("stable project root");
    let script = format!(
        "test \"$(pwd)\" = {project_root} && \
         git config user.name Actor && \
         git config user.email actor@example.invalid && \
         printf 'candidate\\n' > candidate.txt && \
         git add candidate.txt && git commit -m candidate && \
         git -C {source} show-ref --verify refs/heads/actor && \
         ! touch {sibling}/escaped",
        source = source.display(),
        sibling = sibling.display(),
        project_root = project_root.display(),
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
    assert!(Command::new("git")
        .args([
            "-C",
            source.to_str().unwrap(),
            "cat-file",
            "-e",
            "actor^{commit}"
        ])
        .status()
        .unwrap()
        .success());
    assert!(!source.join("candidate.txt").exists());
    assert!(!sibling.join("escaped").exists());
}

#[test]
fn writable_overlay_keeps_build_artifacts_outside_the_checkout() {
    let root = tempfile::tempdir().expect("temp root");
    let workspace = root.path().join("workspace");
    let project_root = root.path().join("actor-project");
    let resource = root.path().join("build-resource");
    let relative_target = std::path::Path::new(".shoal/build/cargo");
    for path in [
        workspace.join(relative_target),
        project_root.join(relative_target),
        resource.clone(),
    ] {
        std::fs::create_dir_all(path).expect("create boundary path");
    }

    let boundary = ProcessMountBoundary::new(&workspace, [workspace.clone()], [workspace.clone()])
        .expect("boundary")
        .with_project_root(&project_root)
        .expect("stable project root")
        .with_writable_overlay(&resource, project_root.join(relative_target))
        .expect("build overlay");
    let invocation = boundary.wrap(
        "bwrap",
        ProcessInvocation {
            program: "/bin/sh".into(),
            args: vec![
                "-c".into(),
                "printf artifact > .shoal/build/cargo/probe".into(),
            ],
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
    assert_eq!(
        std::fs::read(resource.join("probe")).expect("overlay artifact"),
        b"artifact"
    );
    assert!(!workspace.join(relative_target).join("probe").exists());
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

#[test]
fn build_overlay_shares_layers_and_isolates_writes() {
    let root = tempfile::tempdir().unwrap();
    for name in [
        "workspace",
        "base",
        "newer",
        "upper-a",
        "work-a",
        "upper-b",
        "work-b",
    ] {
        std::fs::create_dir(root.path().join(name)).unwrap();
    }
    let workspace = root.path().join("workspace");
    let target = workspace.join("target");
    std::fs::create_dir(&target).unwrap();
    let base = root.path().join("base");
    let newer = root.path().join("newer");
    std::fs::write(base.join("artifact"), "old").unwrap();
    std::fs::write(newer.join("artifact"), "warm").unwrap();
    std::fs::write(base.join("untouched"), vec![42; 1024 * 1024]).unwrap();
    for suffix in ["a", "b"] {
        let upper = root.path().join(format!("upper-{suffix}"));
        let boundary =
            ProcessMountBoundary::new(&workspace, [workspace.clone()], [workspace.clone()])
                .unwrap()
                .with_overlay_view(
                    [base.clone(), newer.clone()],
                    &upper,
                    root.path().join(format!("work-{suffix}")),
                    &target,
                )
                .unwrap();
        let invocation = boundary.wrap("bwrap", ProcessInvocation {
            program: "/bin/sh".into(),
            args: vec!["-c".into(),
                "test \"$(cat target/artifact)\" = warm && printf '%s' \"$1\" > target/artifact && ! touch \"$2/escape\" && ! touch \"$3/escape\"".into(),
                "probe".into(), suffix.into(), newer.to_string_lossy().into_owned(), upper.to_string_lossy().into_owned()],
        });
        let output = Command::new(invocation.program)
            .args(invocation.args)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert_eq!(
            std::fs::read_to_string(upper.join("artifact")).unwrap(),
            suffix
        );
        assert!(!upper.join("untouched").exists());
    }
    assert_eq!(
        std::fs::read_to_string(newer.join("artifact")).unwrap(),
        "warm"
    );
    assert_eq!(
        std::fs::read_to_string(base.join("artifact")).unwrap(),
        "old"
    );
}

#[test]
fn build_overlay_rejects_escape_and_overlapping_backing_directories() {
    let root = tempfile::tempdir().unwrap();
    for name in ["workspace", "base", "upper", "work"] {
        std::fs::create_dir(root.path().join(name)).unwrap();
    }
    let workspace = root.path().join("workspace");
    let boundary =
        ProcessMountBoundary::new(&workspace, [workspace.clone()], [workspace.clone()]).unwrap();
    for target in [root.path().join("outside"), workspace.join("../escape")] {
        assert!(matches!(
            boundary.clone().with_overlay_view(
                [root.path().join("base")],
                root.path().join("upper"),
                root.path().join("work"),
                target,
            ),
            Err(tidepool_node::ProcessBoundaryError::OverlayOutsideProjectRoot { .. })
        ));
    }
    assert!(matches!(
        boundary.with_overlay_view(
            [root.path().join("base")],
            root.path().join("upper"),
            root.path().join("upper"),
            workspace.join("target"),
        ),
        Err(tidepool_node::ProcessBoundaryError::InvalidOverlayView)
    ));
}
