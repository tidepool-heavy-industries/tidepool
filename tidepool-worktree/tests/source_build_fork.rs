#![cfg(target_os = "linux")]

use std::io::{BufRead, BufReader};
use std::path::Path;
use std::process::{Child, Command, Stdio};

use tidepool_node::{
    MountNamespace, OverlayRotation, OverlayRotationOutcome, ProcessInvocation,
    ProcessMountBoundary,
};
use tidepool_repr::ActorPath;
use tidepool_worktree::{
    git::inspect, testing::TestRepo, WorktreeManager, WorktreeRecordStatus, WorktreeRegistry,
};

struct Owner(Child);

impl Drop for Owner {
    fn drop(&mut self) {
        drop(self.0.stdin.take());
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn launch(boundary: ProcessMountBoundary) -> (Owner, MountNamespace) {
    let command = boundary.wrap(
        "bwrap",
        ProcessInvocation {
            program: "/bin/sh".into(),
            args: vec!["-c".into(), "echo $$; read finished".into()],
        },
    );
    let mut owner = Owner(
        Command::new(command.program)
            .args(command.args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .unwrap(),
    );
    let mut line = String::new();
    BufReader::new(owner.0.stdout.take().unwrap())
        .read_line(&mut line)
        .unwrap();
    let namespace = MountNamespace::capture(line.trim().parse().unwrap()).unwrap();
    (owner, namespace)
}

fn shell(namespace: &MountNamespace, cwd: &Path, script: &str) -> String {
    let output = namespace
        .host_command(cwd, "/bin/sh".as_ref())
        .unwrap()
        .args(["-eu", "-c", script])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap()
}

fn build(namespace: &MountNamespace, view: &Path) -> Vec<(String, bool)> {
    let output = namespace
        .host_command(view, "cargo".as_ref())
        .unwrap()
        .args(["build", "--offline", "--message-format=json"])
        .env("CARGO_TARGET_DIR", view.join("target"))
        .env_remove("RUSTC_WRAPPER")
        .env_remove("RUSTC_WORKSPACE_WRAPPER")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout)
        .unwrap()
        .lines()
        .filter_map(|line| {
            let value: serde_json::Value = serde_json::from_str(line).unwrap();
            (value["reason"] == "compiler-artifact").then(|| {
                (
                    value["target"]["name"].as_str().unwrap().to_owned(),
                    value["fresh"].as_bool().unwrap(),
                )
            })
        })
        .collect()
}

/// Composes the production mount/Git primitives without a provider or TUI.
/// Native admission and ordinary `unfold` allocation need separate acceptance.
#[test]
fn source_and_build_fork_preserves_git_state_and_cargo_freshness() {
    let repository = TestRepo::init().unwrap();
    let git = repository.git();
    for (path, bytes) in [
        ("Cargo.toml", "[package]\nname = 'snapshot-probe'\nversion = '0.1.0'\nedition = '2021'\n"),
        ("build.rs", "fn main() { println!(\"cargo:rerun-if-changed=input\"); println!(\"cargo:rustc-env=VALUE={}\", std::fs::read_to_string(\"input\").unwrap().trim()); }\n"),
        ("input", "before\n"),
        ("tracked", "committed\n"),
        ("deleted", "remove in parent\n"),
        (".gitignore", "target/\nignored\n"),
    ] {
        std::fs::write(repository.path().join(path), bytes).unwrap();
    }
    std::fs::create_dir(repository.path().join("src")).unwrap();
    std::fs::write(
        repository.path().join("src/main.rs"),
        "fn main() { println!(\"{}\", env!(\"VALUE\")); }\n",
    )
    .unwrap();
    git.try_run(repository.path(), &["add", "."]).unwrap();
    git.try_run(repository.path(), &["commit", "-qm", "seed"])
        .unwrap();
    let storage = tempfile::tempdir().unwrap();
    let root = storage.path();
    let base = root.join("base");
    git.try_run(
        repository.path(),
        &[
            "worktree",
            "add",
            "-qb",
            "parent",
            base.to_str().unwrap(),
            "HEAD",
        ],
    )
    .unwrap();
    for name in [
        "view",
        "source-u0",
        "source-w0",
        "source-u1",
        "source-w1",
        "source-uc",
        "source-wc",
        "build-base",
        "build-u0",
        "build-w0",
        "build-u1",
        "build-w1",
        "build-uc",
        "build-wc",
    ] {
        std::fs::create_dir(root.join(name)).unwrap();
    }
    std::fs::create_dir(base.join("target")).unwrap();
    let view = root.join("view");
    let target = view.join("target");
    std::fs::create_dir(&target).unwrap();
    let boundary = |source_layers: Vec<std::path::PathBuf>,
                    source_upper: &str,
                    source_work: &str,
                    build_layers: Vec<std::path::PathBuf>,
                    build_upper: &str,
                    build_work: &str| {
        ProcessMountBoundary::new(&view, [view.clone()], [view.clone()])
            .unwrap()
            .with_overlay_view(
                source_layers,
                root.join(source_upper),
                root.join(source_work),
                &view,
            )
            .unwrap()
            .with_overlay_view(
                build_layers,
                root.join(build_upper),
                root.join(build_work),
                &target,
            )
            .unwrap()
    };
    let (_parent, parent) = launch(boundary(
        vec![base.clone()],
        "source-u0",
        "source-w0",
        vec![root.join("build-base")],
        "build-u0",
        "build-w0",
    ));
    shell(&parent, &view, "printf staged > tracked; git add tracked; printf unstaged > tracked; printf ignored > ignored; printf untracked > untracked; rm deleted; printf intent > intent; git add -N intent; git update-index --assume-unchanged Cargo.toml; git update-index --split-index");
    let parent_git = git.with_mount_namespace(parent.clone());
    let staged = parent_git
        .try_run(&view, &["diff", "--cached"])
        .unwrap()
        .stdout;
    let unstaged = parent_git.try_run(&view, &["diff"]).unwrap().stdout;
    let stamp = shell(&parent, &view, "stat -c '%y' src/main.rs");
    let cold = build(&parent, &view);
    assert!(
        cold.iter()
            .any(|(name, fresh)| name == "snapshot-probe" && !fresh),
        "{cold:?}"
    );
    assert_eq!(
        shell(&parent, &view, "target/debug/snapshot-probe"),
        "before\n"
    );

    let build_layers = vec![root.join("build-base"), root.join("build-u0")];
    let outcome = parent.rotate_overlay(
        OverlayRotation::prepare(
            &target,
            &build_layers,
            &root.join("build-u1"),
            &root.join("build-w1"),
        )
        .unwrap(),
    );
    assert!(
        matches!(outcome, OverlayRotationOutcome::Rotated),
        "{outcome:?}"
    );
    let source_layers = vec![base, root.join("source-u0")];
    assert!(matches!(
        parent.rotate_overlay(
            OverlayRotation::prepare(
                &view,
                &source_layers,
                &root.join("source-u1"),
                &root.join("source-w1")
            )
            .unwrap()
            .preserving_mounts(std::slice::from_ref(&target))
            .unwrap()
        ),
        OverlayRotationOutcome::Rotated
    ));

    // Working files inherit their layers; only private Git administration is copied.
    let parent_admin = inspect::git_dir(&parent_git, &view).unwrap();
    let original_index = std::fs::read(parent_admin.join("index")).unwrap();
    let manager = WorktreeManager::new(
        parent_git.clone(),
        WorktreeRegistry::open(root.join("registry")).unwrap(),
        root.join("managed"),
        view.clone(),
    );
    let prepared = manager
        .prepare_inherited_source(&ActorPath::parse("root/child").unwrap())
        .unwrap();
    assert_eq!(prepared.receipt().status, WorktreeRecordStatus::Provisional);
    assert!(matches!(
        manager.lookup(&prepared.receipt().worktree_id),
        Err(tidepool_worktree::WorktreeError::WorktreeAuthorityDenied(_))
    ));
    assert_eq!(
        manager
            .registry()
            .get(&prepared.receipt().worktree_id)
            .unwrap()
            .unwrap()
            .status,
        WorktreeRecordStatus::Provisional
    );
    assert!(!prepared.receipt().cwd.join("src").exists());
    assert_eq!(
        std::fs::read(parent_admin.join("index")).unwrap(),
        original_index
    );
    std::fs::copy(prepared.git_file(), root.join("source-uc/.git")).unwrap();
    let (_child, child) = launch(boundary(
        source_layers,
        "source-uc",
        "source-wc",
        build_layers,
        "build-uc",
        "build-wc",
    ));
    let child_git = git.with_mount_namespace(child.clone());
    assert_eq!(
        child_git
            .try_run(&view, &["ls-files", "-v"])
            .unwrap()
            .stdout,
        parent_git
            .try_run(&view, &["ls-files", "-v"])
            .unwrap()
            .stdout
    );
    assert_eq!(
        child_git
            .try_run(&view, &["diff", "--cached"])
            .unwrap()
            .stdout,
        staged
    );
    assert_eq!(
        child_git.try_run(&view, &["diff"]).unwrap().stdout,
        unstaged
    );
    assert_eq!(shell(&child, &view, "stat -c '%y' src/main.rs"), stamp);
    assert_eq!(
        shell(&child, &view, "test ! -e deleted; cat ignored untracked"),
        "ignoreduntracked"
    );
    let warm = build(&child, &view);
    assert!(
        !warm.is_empty() && warm.iter().all(|(_, fresh)| *fresh),
        "{warm:?}"
    );
    shell(
        &child,
        &view,
        "printf after > input; git add input; git commit -qm child",
    );
    let changed = build(&child, &view);
    assert!(
        changed
            .iter()
            .any(|(name, fresh)| name == "snapshot-probe" && !fresh),
        "{changed:?}"
    );
    assert_eq!(
        shell(&child, &view, "target/debug/snapshot-probe"),
        "after\n"
    );
    assert_eq!(
        shell(&parent, &view, "target/debug/snapshot-probe; cat input"),
        "before\nbefore\n"
    );
    shell(
        &child,
        &view,
        r#"printf 'fn main() { println!("local edit"); }' > src/main.rs"#,
    );
    let local_edit = build(&child, &view);
    assert!(
        local_edit
            .iter()
            .any(|(name, fresh)| name == "snapshot-probe" && !fresh),
        "{local_edit:?}"
    );
    assert_eq!(
        shell(&child, &view, "target/debug/snapshot-probe"),
        "local edit\n"
    );
    assert_eq!(
        shell(&parent, &view, "target/debug/snapshot-probe"),
        "before\n"
    );
    assert_ne!(
        child_git
            .try_run(&view, &["rev-parse", "HEAD"])
            .unwrap()
            .stdout,
        parent_git
            .try_run(&view, &["rev-parse", "HEAD"])
            .unwrap()
            .stdout
    );
    assert_eq!(
        parent_git
            .try_run(&view, &["diff", "--cached"])
            .unwrap()
            .stdout,
        staged
    );
}
