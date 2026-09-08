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

fn launch(boundary: ProcessMountBoundary) -> (Owner, MountNamespace, u32) {
    let command = boundary.wrap(
        "bwrap",
        ProcessInvocation {
            program: "/bin/sh".into(),
            args: vec!["-c".into(), "echo $$; read finished".into()],
        },
    );
    let mut process = Command::new(command.program);
    process.args(command.args);
    capture_worker(process)
}

fn capture_worker(mut process: Command) -> (Owner, MountNamespace, u32) {
    let mut owner = Owner(
        process
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .unwrap(),
    );
    let mut line = String::new();
    BufReader::new(owner.0.stdout.take().unwrap())
        .read_line(&mut line)
        .unwrap();
    let pid = line.trim().parse().unwrap();
    let namespace = MountNamespace::capture(pid).unwrap();
    (owner, namespace, pid)
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
            .with_read_only_overlay(root, root)
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
    let (_parent, parent, _) = launch(boundary(
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
    let wrong_view = manager
        .prepare_inherited_source(&ActorPath::parse("root/wrong-view").unwrap())
        .unwrap();
    let wrong_id = wrong_view.receipt().worktree_id.clone();
    assert!(matches!(
        manager.finish_inherited_source(wrong_view, parent.clone(), &view),
        Err(tidepool_worktree::WorktreeError::WorktreeAuthorityDenied(_))
    ));
    assert_eq!(
        manager.registry().get(&wrong_id).unwrap().unwrap().status,
        WorktreeRecordStatus::Provisional
    );
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
    let child = boundary(
        source_layers,
        "source-uc",
        "source-wc",
        build_layers,
        "build-uc",
        "build-wc",
    )
    .prepare_view(
        "bwrap",
        std::time::Instant::now() + std::time::Duration::from_secs(10),
    )
    .unwrap();
    assert!(child.require_live_owner().is_err());
    let host_manager = WorktreeManager::new(
        tidepool_worktree::GitCli::new(),
        manager.registry().clone(),
        root.join("managed"),
        repository.path(),
    );
    let handle = manager
        .finish_inherited_source(prepared, child.clone(), &view)
        .unwrap();
    assert_eq!(handle.receipt().status, WorktreeRecordStatus::Mounted);
    assert!(host_manager.lookup(handle.id()).unwrap().is_some());
    assert!(host_manager
        .registry()
        .list()
        .unwrap()
        .iter()
        .any(|row| row.receipt.worktree_id == *handle.id() && row.present));
    let reopened = WorktreeManager::new(
        tidepool_worktree::GitCli::new(),
        WorktreeRegistry::open(root.join("registry")).unwrap(),
        root.join("managed"),
        repository.path(),
    );
    assert!(reopened
        .git()
        .try_run(handle.cwd(), &["status", "--porcelain"])
        .is_err());
    assert!(
        matches!(
            reopened.lookup(handle.id()),
            Err(tidepool_worktree::WorktreeError::StorageFailure { .. })
        ),
        "lost view descriptors cannot expose the Git-only host directory"
    );
    // Bootstrap inspection/finalization precedes any continuing worker. Its
    // command acquires the same view, without remounting the writable overlay.
    let mut process = child
        .entry()
        .unwrap()
        .command(&view, "/bin/sh".as_ref())
        .unwrap();
    process.args(["-c", "echo $$; read finished"]);
    let (_child, live_child, child_pid) = capture_worker(process);
    assert!(child.same_view_as(&live_child).unwrap());
    host_manager
        .restore_mounted_source(handle.id(), live_child.clone(), &view)
        .unwrap();
    let child = live_child;
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
    assert!(matches!(
        reopened.restore_mounted_source(handle.id(), parent.clone(), &view),
        Err(tidepool_worktree::WorktreeError::WorktreeAuthorityDenied(_))
    ));
    assert!(reopened.lookup(handle.id()).is_err());
    let recaptured = MountNamespace::capture(child_pid).unwrap();
    assert!(child.same_view_as(&recaptured).unwrap());
    assert!(!child.same_view_as(&parent).unwrap());
    let restored = reopened
        .restore_mounted_source(handle.id(), recaptured, &view)
        .unwrap();
    assert_eq!(restored, handle);
    assert_eq!(
        reopened
            .restore_mounted_source(handle.id(), child.clone(), &view)
            .unwrap(),
        handle
    );
    assert_eq!(
        reopened.worktree_head(&restored).unwrap(),
        host_manager.worktree_head(&handle).unwrap()
    );
    assert_eq!(
        reopened
            .git()
            .try_run(restored.cwd(), &["diff"])
            .unwrap()
            .stdout,
        child_git.try_run(&view, &["diff"]).unwrap().stdout
    );
    // Sharing Git administration alone must not replace an already retained
    // filesystem view with a different namespace's empty working directory.
    let (_other, other, _) = launch(
        ProcessMountBoundary::new(
            handle.cwd(),
            [handle.cwd().to_owned()],
            [handle.cwd().to_owned()],
        )
        .unwrap()
        .with_project_root(&view)
        .unwrap(),
    );
    assert!(reopened
        .restore_mounted_source(handle.id(), other, &view)
        .is_err());
    assert_eq!(
        reopened
            .git()
            .try_run(restored.cwd(), &["diff"])
            .unwrap()
            .stdout,
        child_git.try_run(&view, &["diff"]).unwrap().stdout
    );
    assert_eq!(
        host_manager.worktree_head(&handle).unwrap().as_str(),
        child_git
            .try_run(&view, &["rev-parse", "HEAD"])
            .unwrap()
            .trimmed()
    );
    assert_eq!(
        host_manager
            .git()
            .try_run(handle.cwd(), &["diff"])
            .unwrap()
            .stdout,
        child_git.try_run(&view, &["diff"]).unwrap().stdout
    );
    assert!(host_manager.observe_submission(&handle).is_ok());
    assert_eq!(
        parent_git
            .try_run(&view, &["diff", "--cached"])
            .unwrap()
            .stdout,
        staged
    );
}
