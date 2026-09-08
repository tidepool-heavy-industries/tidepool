#![cfg(target_os = "linux")]

use std::io::{BufRead, BufReader};
use std::process::{Child, Command, Stdio};

use tidepool_node::MountNamespace;
use tidepool_worktree::git::inspect;
use tidepool_worktree::testing::TestRepo;
use tidepool_worktree::InProgressKind;
use tidepool_worktree::{
    BranchName, GitOid, WorktreeId, WorktreeManager, WorktreeOrigin, WorktreeReceipt,
    WorktreeRecordStatus, WorktreeRegistry, WorktreeSpec,
};

struct Owner(Child);

impl Drop for Owner {
    fn drop(&mut self) {
        drop(self.0.stdin.take());
        let _ = self.0.kill();
        let _ = self.0.wait();
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
    let root = storage.path();
    let manager = WorktreeManager::new(
        repository.git().clone(),
        WorktreeRegistry::open(root.join("registry")).unwrap(),
        root.join("managed"),
        repository.path(),
    );
    let handle = manager
        .create(&WorktreeSpec::from_current_repository("child"))
        .unwrap();
    for name in ["upper", "work", "view"] {
        std::fs::create_dir(root.join(name)).unwrap();
    }
    let view = root.join("view");
    let common = inspect::git_common_dir(repository.git(), repository.path()).unwrap();
    let namespace = tidepool_node::ProcessMountBoundary::new(
        handle.cwd(),
        [repository.path().to_owned(), root.join("managed")],
        [common],
    )
    .unwrap()
    .with_project_root(&view)
    .unwrap()
    .with_overlay_view(
        [handle.cwd().to_owned()],
        root.join("upper"),
        root.join("work"),
        &view,
    )
    .unwrap()
    .prepare_view(
        "bwrap",
        std::time::Instant::now() + std::time::Duration::from_secs(10),
    )
    .unwrap();
    let mounted = manager
        .mount_worktree(handle.id(), namespace.clone(), &view)
        .unwrap();
    assert_eq!(mounted.receipt().status, WorktreeRecordStatus::Mounted);
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
        WorktreeRegistry::open(root.join("registry")).unwrap(),
        root.join("managed"),
        repository.path(),
    );
    assert!(reopened.lookup(handle.id()).is_err());
    reopened
        .mount_worktree(handle.id(), namespace, &view)
        .unwrap();
    assert_eq!(reopened.observe_submission(&handle).unwrap(), observed);
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
    let registry = WorktreeRegistry::open(storage.path().join("registry")).unwrap();
    let id = WorktreeId::from_raw("mounted-child");
    registry
        .put(&WorktreeReceipt {
            worktree_id: id.clone(),
            cwd: view.clone(),
            branch: BranchName::from_raw("child"),
            source_head: GitOid::from_raw(original_head.trimmed()),
            snapshot_ref: None,
            origin: WorktreeOrigin::CurrentRepository,
            source_repository: repository.path().into(),
            created_at_ms: 0,
            status: WorktreeRecordStatus::Finalized,
        })
        .unwrap();
    let private_admin = storage.path().join("git-admin");
    assert!(Command::new("cp")
        .arg("-a")
        .arg(&git_dir)
        .arg(&private_admin)
        .status()
        .unwrap()
        .success());
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
    let entry: tidepool_node::NamespaceEntry = serde_json::from_slice(&serialized).unwrap();
    let mut prepared = entry
        .command(&view, std::ffi::OsStr::new("/bin/sh"))
        .unwrap();
    prepared.args(["-c", "exit 0"]);
    let next_upper = storage.path().join("next-upper");
    let next_work = storage.path().join("next-work");
    std::fs::create_dir(&next_upper).unwrap();
    std::fs::create_dir(&next_work).unwrap();
    let rotation = tidepool_node::OverlayRotation::prepare(
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
        tidepool_node::OverlayRotationOutcome::Unconfirmed(_)
    ));
    assert!(std::fs::read_dir(&next_upper).unwrap().next().is_none());
    let mut invalid: serde_json::Value = serde_json::from_slice(&serialized).unwrap();
    invalid["start_ticks"] = 0.into();
    let invalid: tidepool_node::NamespaceEntry = serde_json::from_value(invalid).unwrap();
    assert!(invalid.command(&view, "/bin/sh".as_ref()).is_err());
    let mut invalid: serde_json::Value = serde_json::from_slice(&serialized).unwrap();
    invalid["identity"]["root_mount"] = 0.into();
    let invalid: tidepool_node::NamespaceEntry = serde_json::from_value(invalid).unwrap();
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
