#![cfg(target_os = "linux")]

use std::io::{BufRead, BufReader};
use std::process::{Child, Command, Stdio};
use tidepool_atomic_write::DirectoryAnchor;

use exomonad_node::MountNamespace;
use exomonad_worktree::git::inspect;
use exomonad_worktree::testing::TestRepo;
use exomonad_worktree::InProgressKind;
use exomonad_worktree::{
    BranchName, GitOid, WorktreeId, WorktreeManager, WorktreeOrigin, WorktreeReceipt,
    WorktreeRecordStatus, WorktreeRegistry, WorktreeSpec,
};

struct Owner(Child);

impl Drop for Owner {
    fn drop(&mut self) {
        drop(self.0.stdin.take());
        // best-effort: Drop cannot propagate; the child may already have exited.
        self.0.kill().ok();
        self.0.wait().ok();
    }
}

#[test]
fn completed_checkout_uses_its_launch_view_and_requires_reattachment_on_reopen() {
    let repository = TestRepo::init().unwrap();
    repository
        .writer()
        .commit_file("file", "before\n", "seed")
        .unwrap();
    let storage = tempfile::tempdir().unwrap();
    let storage_anchor = DirectoryAnchor::open_existing(storage.path()).unwrap();
    let root = storage.path();
    let manager = WorktreeManager::new(
        repository.git().clone(),
        WorktreeRegistry::open(&storage_anchor, "registry").unwrap(),
        root.join("managed"),
        repository.path(),
    );
    let prepared = manager
        .prepare_inherited_source(&exomonad_worktree::WorktreeSource::CurrentRepository)
        .unwrap();
    let cwd = prepared.receipt().cwd.clone();
    std::fs::write(cwd.join("file"), "before\n").unwrap();
    let base = root.join("worktrees/.resources/run/source/base");
    std::fs::create_dir_all(&base).unwrap();
    std::fs::copy(cwd.join("file"), base.join("file")).unwrap();
    std::fs::copy(cwd.join(".git"), base.join(".git")).unwrap();
    let upper = base.parent().unwrap().join("upper");
    let work = base.parent().unwrap().join("work");
    std::fs::create_dir(&upper).unwrap();
    std::fs::create_dir(&work).unwrap();
    std::fs::create_dir(root.join("view")).unwrap();
    let view = root.join("view");
    let common = inspect::git_common_dir(repository.git(), repository.path()).unwrap();
    let namespace = exomonad_node::ProcessMountBoundary::new(
        &cwd,
        [repository.path().to_owned(), root.join("managed")],
        [common],
    )
    .unwrap()
    .with_project_root(&view)
    .unwrap()
    .with_overlay_view([base.clone()], &upper, &work, &view)
    .unwrap()
    .prepare_view(
        "bwrap",
        std::time::Instant::now() + std::time::Duration::from_secs(10),
    )
    .unwrap();
    let handle = manager
        .finish_inherited_source(
            prepared,
            namespace.clone(),
            &view,
            vec![base.clone(), upper.clone()],
        )
        .unwrap();
    assert_eq!(handle.receipt().status, WorktreeRecordStatus::Mounted);
    let output = namespace
        .host_command(&view, "/bin/sh".as_ref())
        .unwrap()
        .args([
            "-ec",
            "printf after > file; git add file; git commit -qm changed",
        ])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        std::fs::read_to_string(handle.cwd().join("file")).unwrap(),
        "before\n"
    );
    let observed = manager.observe_submission(&handle).unwrap();
    assert_eq!(observed.committed_paths, ["file"]);
    assert!(observed.working_state.changes.unstaged.is_empty());
    let reopened = WorktreeManager::new(
        repository.git().clone(),
        WorktreeRegistry::open(&storage_anchor, "registry").unwrap(),
        root.join("managed"),
        repository.path(),
    );
    assert!(reopened.lookup(handle.id()).is_err());
    reopened
        .mount_worktree(handle.id(), namespace.clone(), &view)
        .unwrap();
    assert_eq!(reopened.observe_submission(&handle).unwrap(), observed);
    let changed = namespace
        .host_command(&view, "/bin/sh".as_ref())
        .unwrap()
        .args([
            "-ec",
            "printf staged > file; git add file; printf dirty >> file; printf untracked > untracked",
        ])
        .output()
        .unwrap();
    assert!(changed.status.success());
    let before = reopened
        .git()
        .try_run(&cwd, &["status", "--porcelain=v1"])
        .unwrap()
        .trimmed()
        .to_owned();
    let head_before = reopened.worktree_head_by_id(handle.id()).unwrap();
    let branch_before = reopened.worktree_branch_by_id(handle.id()).unwrap();
    assert!(reopened
        .retain_retired_view(
            handle.id(),
            &namespace,
            &view,
            vec![root.join("missing-layer")],
        )
        .is_err());
    assert_eq!(
        reopened
            .registry()
            .get(handle.id())
            .unwrap()
            .unwrap()
            .status,
        WorktreeRecordStatus::Mounted
    );
    reopened
        .retain_retired_view(handle.id(), &namespace, &view, vec![base, upper])
        .unwrap();
    namespace.detach_retired_tree(&view).unwrap();
    assert!(reopened
        .git()
        .try_run(&cwd, &["status", "--porcelain=v1"])
        .is_err());
    assert_eq!(
        reopened.list().unwrap()[0].receipt.status,
        WorktreeRecordStatus::Retained
    );
    assert_eq!(
        reopened.worktree_head_by_id(handle.id()).unwrap(),
        head_before
    );
    assert_eq!(
        reopened.worktree_branch_by_id(handle.id()).unwrap(),
        branch_before
    );
    assert_eq!(
        std::fs::read_to_string(cwd.join("file")).unwrap(),
        "before\n"
    );
    let after_restart = WorktreeManager::new(
        repository.git().clone(),
        WorktreeRegistry::open(&storage_anchor, "registry").unwrap(),
        root.join("managed"),
        repository.path(),
    );
    let restored = after_restart.lookup(handle.id()).unwrap().unwrap();
    assert_eq!(restored.receipt().status, WorktreeRecordStatus::Finalized);
    assert_eq!(
        std::fs::read_to_string(cwd.join("file")).unwrap(),
        "stageddirty"
    );
    assert_eq!(
        std::fs::read_to_string(cwd.join("untracked")).unwrap(),
        "untracked"
    );
    assert_eq!(
        after_restart
            .git()
            .try_run(&cwd, &["status", "--porcelain=v1"])
            .unwrap()
            .trimmed(),
        before
    );
}

#[test]
fn host_git_observes_and_commits_the_actual_mounted_worktree() {
    let repository = TestRepo::init().unwrap();
    let git = repository.git();
    std::fs::write(repository.path().join("file"), "inherited\n").unwrap();
    git.try_run(repository.path(), &["add", "file"]).unwrap();
    git.try_run(repository.path(), &["commit", "-qm", "seed"])
        .unwrap();
    let original_head = git
        .try_run(repository.path(), &["rev-parse", "HEAD"])
        .unwrap();
    let storage = tempfile::tempdir().unwrap();
    let storage_anchor = DirectoryAnchor::open_existing(storage.path()).unwrap();
    for name in ["base", "upper", "work"] {
        std::fs::create_dir(storage.path().join(name)).unwrap();
    }
    let base = storage.path().join("base");
    let upper = storage.path().join("upper");
    let work = storage.path().join("work");
    let view = storage.path().join("view");
    std::fs::copy(repository.path().join("file"), base.join("file")).unwrap();
    git.try_run(
        repository.path(),
        &[
            "worktree",
            "add",
            "--no-checkout",
            "-b",
            "child",
            view.to_str().unwrap(),
            "HEAD",
        ],
    )
    .unwrap();
    std::fs::copy(view.join(".git"), upper.join(".git")).unwrap();
    let git_dir = inspect::git_dir(git, &view).unwrap();
    std::fs::remove_file(view.join(".git")).unwrap();
    let registry = WorktreeRegistry::open(&storage_anchor, "registry").unwrap();
    let id = WorktreeId::from_raw("mounted-child");
    registry
        .put(&WorktreeReceipt {
            worktree_id: id.clone(),
            cwd: view.clone(),
            branch: Some(BranchName::from_raw("child")),
            source_head: GitOid::from_raw(original_head.trimmed()),
            snapshot_ref: None,
            origin: WorktreeOrigin::CurrentRepository,
            source_repository: repository.path().into(),
            created_at_ms: 0,
            status: WorktreeRecordStatus::Finalized,
        })
        .unwrap();
    let private_admin = storage.path().join("git-admin");
    #[allow(clippy::disallowed_methods, reason = "one-shot cp test fixture")]
    let cp = Command::new("cp")
        .arg("-a")
        .arg(&git_dir)
        .arg(&private_admin)
        .status()
        .unwrap();
    assert!(cp.success());
    #[allow(clippy::disallowed_methods, reason = "test fixture process")]
    let mut owner = Owner(
        Command::new("bwrap")
            .args([
                "--unshare-user",
                "--bind",
                "/",
                "/",
                "--dev",
                "/dev",
                "--overlay-src",
            ])
            .arg(&base)
            .arg("--overlay")
            .arg(&upper)
            .arg(&work)
            .arg(&view)
            .arg("--bind")
            .arg(&private_admin)
            .arg(&git_dir)
            .args([
                "--die-with-parent",
                "--",
                "/bin/sh",
                "-c",
                "printf '%s\\n' \"$$\"; read line || exit 0",
            ])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .unwrap(),
    );
    let mut line = String::new();
    BufReader::new(owner.0.stdout.take().unwrap())
        .read_line(&mut line)
        .unwrap();
    let namespace = MountNamespace::capture(line.trim().parse().unwrap()).unwrap();
    let mut probe = namespace
        .host_command(&view, std::ffi::OsStr::new("/bin/sh"))
        .unwrap();
    let output = probe
        .args(["-c", "cat /proc/self/status"])
        .output()
        .unwrap();
    assert!(output.status.success());
    let status = String::from_utf8(output.stdout).unwrap();
    for field in ["CapInh:", "CapPrm:", "CapEff:", "CapAmb:"] {
        let value = status
            .lines()
            .find_map(|line| line.strip_prefix(field))
            .unwrap();
        assert_eq!(value.trim(), "0000000000000000", "{field}");
    }
    assert!(status.lines().any(|line| line == "NoNewPrivs:\t1"));
    let mounted_git = git.with_mount_namespace(namespace.clone());
    let manager = WorktreeManager::new(
        mounted_git.clone(),
        registry,
        storage.path(),
        repository.path(),
    );
    let handle = manager.lookup(&id).unwrap().unwrap();
    assert!(manager.list().unwrap()[0].present);
    assert!(namespace.try_exists(&view.join("file")).unwrap());
    assert!(!namespace.try_exists(&view.join("missing")).unwrap());
    std::fs::write(private_admin.join("MERGE_HEAD"), original_head.trimmed()).unwrap();
    assert!(!git_dir.join("MERGE_HEAD").exists());
    assert_eq!(
        inspect::in_progress(&mounted_git, &view).unwrap(),
        Some(InProgressKind::Merge)
    );
    std::fs::remove_file(private_admin.join("MERGE_HEAD")).unwrap();
    assert_eq!(inspect::in_progress(&mounted_git, &view).unwrap(), None);
    mounted_git.try_run(&view, &["read-tree", "HEAD"]).unwrap();
    let observed = manager.observe_submission(&handle).unwrap();
    assert!(observed.working_state.changes.staged.is_empty());
    assert!(observed.working_state.changes.unstaged.is_empty());
    assert_eq!(
        mounted_git
            .try_run(&view, &["status", "--porcelain"])
            .unwrap()
            .trimmed(),
        ""
    );
    assert!(
        !view.join("file").exists(),
        "the host's underlying directory is deliberately not the mounted view"
    );
    mounted_git
        .try_run(&view, &["mv", "file", "renamed"])
        .unwrap();
    mounted_git
        .try_run(&view, &["commit", "-qm", "child change"])
        .unwrap();
    assert_eq!(
        git.try_run(repository.path(), &["show", "child:renamed"])
            .unwrap()
            .trimmed(),
        "inherited"
    );
    assert_eq!(
        git.try_run(repository.path(), &["rev-parse", "HEAD"])
            .unwrap(),
        original_head
    );
    assert_eq!(
        std::fs::read_to_string(repository.path().join("file")).unwrap(),
        "inherited\n"
    );
    let serialized = serde_json::to_vec(&namespace.entry().unwrap()).unwrap();
    let entry: exomonad_node::NamespaceEntry = serde_json::from_slice(&serialized).unwrap();
    let mut prepared = entry
        .command(&view, std::ffi::OsStr::new("/bin/sh"))
        .unwrap();
    prepared.args(["-c", "exit 0"]);
    let next_upper = storage.path().join("next-upper");
    let next_work = storage.path().join("next-work");
    std::fs::create_dir(&next_upper).unwrap();
    std::fs::create_dir(&next_work).unwrap();
    let rotation = exomonad_node::OverlayRotation::prepare(
        &view,
        &[base.clone(), upper.clone()],
        &next_upper,
        &next_work,
    )
    .unwrap();
    let publication = namespace
        .prepare_overlay_rotation(rotation.clone())
        .unwrap();
    drop(owner.0.stdin.take());
    assert!(owner.0.wait().unwrap().success());
    assert!(
        prepared.output().unwrap().status.success(),
        "retained filesystem access survives the captured process"
    );
    assert!(
        mounted_git
            .try_run(&view, &["status", "--porcelain"])
            .unwrap()
            .trimmed()
            .is_empty(),
        "retained access must still inspect the mounted checkout"
    );
    assert_eq!(
        namespace.require_live_owner().unwrap_err().kind(),
        std::io::ErrorKind::NotFound
    );
    assert!(namespace.prepare_overlay_rotation(rotation).is_err());
    assert!(matches!(
        publication.apply().1,
        exomonad_node::OverlayRotationOutcome::Unconfirmed(_)
    ));
    assert!(std::fs::read_dir(&next_upper).unwrap().next().is_none());
    let mut invalid: serde_json::Value = serde_json::from_slice(&serialized).unwrap();
    invalid["start_ticks"] = 0.into();
    let invalid: exomonad_node::NamespaceEntry = serde_json::from_value(invalid).unwrap();
    assert!(invalid.command(&view, "/bin/sh".as_ref()).is_err());
    let mut invalid: serde_json::Value = serde_json::from_slice(&serialized).unwrap();
    invalid["identity"]["root_mount"] = 0.into();
    let invalid: exomonad_node::NamespaceEntry = serde_json::from_value(invalid).unwrap();
    assert!(invalid.command(&view, "/bin/sh".as_ref()).is_err());
    assert!(manager.list().unwrap()[0].present);
    assert!(namespace.try_exists(&view.join("renamed")).unwrap());
    assert!(!view.join("renamed").exists());
    mounted_git
        .try_run(&view, &["mv", "renamed", "after-exit"])
        .unwrap();
    mounted_git
        .try_run(&view, &["commit", "-qm", "retained view change"])
        .unwrap();
    assert_eq!(
        git.try_run(repository.path(), &["show", "child:after-exit"])
            .unwrap()
            .trimmed(),
        "inherited"
    );
}

#[test]
fn activation_replaces_only_the_expected_preparation_view() {
    let repository = TestRepo::init().unwrap();
    repository
        .writer()
        .commit_file("file", "seed", "seed")
        .unwrap();
    let storage = tempfile::tempdir().unwrap();
    let storage_anchor = DirectoryAnchor::open_existing(storage.path()).unwrap();
    let manager = WorktreeManager::new(
        repository.git().clone(),
        WorktreeRegistry::open(&storage_anchor, "registry").unwrap(),
        storage.path().join("managed"),
        repository.path(),
    );
    let handle = manager
        .create(&WorktreeSpec::from_current_repository("activation"))
        .unwrap();
    let visible = storage.path().join("visible");
    std::fs::create_dir(&visible).unwrap();
    let boundary = exomonad_node::ProcessMountBoundary::new(
        handle.cwd(),
        [repository.path().to_owned(), storage.path().join("managed")],
        [inspect::git_common_dir(repository.git(), repository.path()).unwrap()],
    )
    .unwrap()
    .with_project_root(&visible)
    .unwrap();
    let prepare = || {
        boundary
            .prepare_view(
                "bwrap",
                std::time::Instant::now() + std::time::Duration::from_secs(10),
            )
            .unwrap()
    };
    let prepared = prepare();
    let active = prepare();
    assert!(!prepared.same_view_as(&active).unwrap());
    assert!(manager
        .activate_worktree(handle.id(), &prepared, active.clone(), &visible)
        .is_err());
    manager
        .mount_worktree(handle.id(), prepared.clone(), &visible)
        .unwrap();
    assert!(manager
        .activate_worktree(handle.id(), &active, active.clone(), &visible)
        .is_err());
    manager
        .activate_worktree(handle.id(), &prepared, active.clone(), &visible)
        .unwrap();
    assert!(manager
        .activate_worktree(handle.id(), &prepared, prepared.clone(), &visible)
        .is_err());
    assert!(manager
        .mount_worktree(handle.id(), prepared, &visible)
        .is_err());
    manager
        .mount_worktree(handle.id(), active, &visible)
        .unwrap();
    manager.observe_submission(&handle).unwrap();
}
