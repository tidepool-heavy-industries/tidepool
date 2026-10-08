use std::ffi::OsString;
use std::fs;
use tidepool_atomic_write::DirectoryAnchor;

use exomonad_worktree::testing::TestRepo;
use exomonad_worktree::{
    GitCli, WorktreeError, WorktreeManager, WorktreeRecordStatus, WorktreeRegistry, WorktreeSource,
    WorktreeSpec,
};

#[test]
fn generated_preparation_pointer_keeps_current_checkout_clean_but_authored_inputs_do_not() {
    let repo = TestRepo::init().unwrap();
    let git = repo.git();
    for (path, body) in [
        (".exomonad/config.toml", "project configuration\n"),
        (".exomonad/Project/Input.hs", "authored module\n"),
    ] {
        repo.writer()
            .commit_file(path, body, "authored input")
            .unwrap();
    }
    git.ensure_exomonad_local_exclude(repo.path()).unwrap();
    let pointer = repo.path().join(".exomonad/prepared.json");
    // Git admission does not decode this pointer or confer preparation authority.
    fs::write(&pointer, "generated local deployment pointer\n").unwrap();
    assert_eq!(
        git.try_read(repo.path(), &["check-ignore", ".exomonad/prepared.json"])
            .unwrap()
            .trimmed(),
        ".exomonad/prepared.json"
    );
    assert!(git
        .try_read(repo.path(), &["status", "--porcelain"])
        .unwrap()
        .stdout
        .is_empty());
    let storage = tempfile::tempdir().unwrap();
    let anchor = DirectoryAnchor::open_existing(storage.path()).unwrap();
    let manager = WorktreeManager::new(
        GitCli::new(),
        WorktreeRegistry::open(&anchor, "registry").unwrap(),
        storage.path().join("worktrees"),
        repo.path(),
    );
    let child = manager
        .create(&WorktreeSpec::from_current_repository("prepared-child"))
        .unwrap();
    assert!(child.cwd().join(".exomonad/config.toml").is_file());
    assert!(child.cwd().join(".exomonad/Project/Input.hs").is_file());
    assert!(!child.cwd().join(".exomonad/prepared.json").exists());
    for path in [".exomonad/config.toml", ".exomonad/Project/Input.hs"] {
        assert_eq!(
            git.try_read(repo.path(), &["ls-files", "--", path])
                .unwrap()
                .trimmed(),
            path
        );
        let original = fs::read(repo.path().join(path)).unwrap();
        fs::write(repo.path().join(path), "changed authored input\n").unwrap();
        assert!(matches!(
            manager.create(&WorktreeSpec::from_current_repository("dirty-child")),
            Err(WorktreeError::SourceDirty(_))
        ));
        fs::write(repo.path().join(path), original).unwrap();
    }
    fs::write(
        repo.path().join(".exomonad/new-input.toml"),
        "new authored input\n",
    )
    .unwrap();
    assert!(matches!(
        manager.create(&WorktreeSpec::from_current_repository("untracked-child")),
        Err(WorktreeError::SourceDirty(_))
    ));
    assert_eq!(
        fs::read_to_string(pointer).unwrap(),
        "generated local deployment pointer\n"
    );
}

#[test]
fn checkpoint_commits_partial_staging_deletions_and_new_files_on_same_branch() {
    let repo = TestRepo::init().unwrap();
    let git = repo.git();
    repo.writer()
        .commit_file("partial", "old\n", "seed")
        .unwrap();
    repo.writer()
        .commit_file("deleted", "gone\n", "seed 2")
        .unwrap();
    let branch = git
        .try_run(repo.path(), &["symbolic-ref", "--short", "HEAD"])
        .unwrap()
        .trimmed()
        .to_owned();
    fs::write(repo.path().join("partial"), "staged\n").unwrap();
    git.try_run(repo.path(), &["add", "partial"]).unwrap();
    fs::write(repo.path().join("partial"), "staged\nunstaged\n").unwrap();
    fs::remove_file(repo.path().join("deleted")).unwrap();
    fs::write(repo.path().join("new"), "new\n").unwrap();

    let committed = git.checkpoint_source(repo.path(), &[]).unwrap();
    assert_eq!(
        git.try_run(repo.path(), &["show", "HEAD:partial"])
            .unwrap()
            .stdout,
        "staged\nunstaged\n"
    );
    assert_eq!(
        git.try_run(repo.path(), &["show", "HEAD:new"])
            .unwrap()
            .stdout,
        "new\n"
    );
    assert!(git
        .try_run(repo.path(), &["cat-file", "-e", "HEAD:deleted"])
        .is_err());
    assert_eq!(
        git.try_run(repo.path(), &["symbolic-ref", "--short", "HEAD"])
            .unwrap()
            .trimmed(),
        branch
    );
    assert!(git
        .try_run(repo.path(), &["status", "--porcelain"])
        .unwrap()
        .stdout
        .is_empty());
    assert_eq!(git.checkpoint_source(repo.path(), &[]).unwrap(), committed);
}

#[test]
fn checkpoint_refuses_uncommitted_submodule_files_before_mutation() {
    let workspace = TestRepo::init().expect("init workspace");
    workspace
        .writer()
        .commit_file(
            "Project/Initial.hs",
            "module Initial where\n",
            "initial module",
        )
        .expect("commit workspace");
    let repo = TestRepo::init().expect("init project");
    let git = repo.git();
    git.try_run(
        repo.path(),
        &[
            "-c",
            "protocol.file.allow=always",
            "submodule",
            "add",
            workspace.path().to_str().expect("utf8 workspace path"),
            ".exomonad/workspace",
        ],
    )
    .expect("add workspace");
    git.try_run(repo.path(), &["commit", "-q", "-m", "record workspace"])
        .expect("commit gitlink");
    let head = git.try_run(repo.path(), &["rev-parse", "HEAD"]).unwrap();
    repo.writer_at(repo.path().join(".exomonad/workspace"))
        .write_file("Project/Draft.hs", "module Draft where\n")
        .expect("write uncommitted nested source");
    repo.writer()
        .write_file("main.py", "print('pending')\n")
        .unwrap();

    assert!(matches!(
        git.checkpoint_source(repo.path(), &[]),
        Err(WorktreeError::DirtySubmoduleUnsupported(path))
            if path == std::path::Path::new(".exomonad/workspace")
    ));
    assert_eq!(
        git.try_run(repo.path(), &["rev-parse", "HEAD"]).unwrap(),
        head
    );
    assert!(git
        .try_run(repo.path(), &["cat-file", "-e", "HEAD:main.py"])
        .is_err());
    assert!(repo.path().join("main.py").exists());
}

#[test]
fn checkpoint_leaves_staged_exclusions_and_disables_hooks_and_signing() {
    let repo = TestRepo::init().unwrap();
    let git = repo.git();
    repo.writer()
        .commit_file("included", "old", "seed")
        .unwrap();
    fs::create_dir_all(repo.path().join(".exomonad")).unwrap();
    fs::write(repo.path().join(".exomonad/runtime"), "private").unwrap();
    git.try_run(repo.path(), &["add", "-f", ".exomonad/runtime"])
        .unwrap();
    fs::write(
        repo.path().join(".exomonad/untracked-private"),
        "unique excluded content that must never enter Git objects",
    )
    .unwrap();
    let excluded_blob = git
        .try_run(
            repo.path(),
            &["hash-object", "--no-filters", ".exomonad/untracked-private"],
        )
        .unwrap()
        .trimmed()
        .to_owned();
    fs::create_dir_all(repo.path().join("config")).unwrap();
    fs::write(repo.path().join("config/private"), "secret").unwrap();
    git.try_run(repo.path(), &["add", "config/private"])
        .unwrap();
    fs::write(
        repo.path().join("configuration"),
        "included by exact exclusion",
    )
    .unwrap();
    fs::write(repo.path().join("included"), "new").unwrap();
    fs::write(repo.path().join(".gitignore"), "forced-ignored\n").unwrap();
    fs::write(repo.path().join("forced-ignored"), "explicitly staged").unwrap();
    git.try_run(repo.path(), &["add", "-f", "forced-ignored"])
        .unwrap();
    git.try_run(repo.path(), &["config", "commit.gpgsign", "true"])
        .unwrap();
    let hook = repo.path().join(".git/hooks/pre-commit");
    fs::write(&hook, "#!/bin/sh\nexit 17\n").unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&hook, fs::Permissions::from_mode(0o755)).unwrap();
    }

    fs::write(
        repo.path().join(".git/info/exclude"),
        b"# local\r\n/custom/\n/.exomonad/\r\n",
    )
    .unwrap();
    git.ensure_exomonad_local_exclude(repo.path()).unwrap();
    let first_exclude = fs::read(repo.path().join(".git/info/exclude")).unwrap();
    assert!(first_exclude.starts_with(b"# local\r\n/custom/\n"));
    let installed = String::from_utf8(first_exclude.clone()).unwrap();
    // The whole-directory line hides a project's authored `.exomonad` source, so
    // the writer removes it and installs only the runtime state directories.
    assert!(!installed.lines().any(|line| line.trim() == "/.exomonad/"));
    for exclusion in exomonad_worktree::git::EXOMONAD_LOCAL_EXCLUDES {
        assert!(installed.lines().any(|line| line == *exclusion));
    }
    git.ensure_exomonad_local_exclude(repo.path()).unwrap();
    assert_eq!(
        fs::read(repo.path().join(".git/info/exclude")).unwrap(),
        first_exclude
    );
    let committed = git
        .checkpoint_source(
            repo.path(),
            &[OsString::from(".exomonad"), OsString::from("config")],
        )
        .unwrap();
    assert_eq!(
        git.try_run(repo.path(), &["show", "HEAD:included"])
            .unwrap()
            .stdout,
        "new"
    );
    assert!(git
        .try_run(repo.path(), &["cat-file", "-e", "HEAD:.exomonad/runtime"])
        .is_err());
    assert!(git
        .try_run(repo.path(), &["cat-file", "-e", "HEAD:config/private"])
        .is_err());
    assert!(git
        .try_run(repo.path(), &["cat-file", "-e", &excluded_blob])
        .is_err());
    assert_eq!(
        git.try_run(repo.path(), &["show", "HEAD:forced-ignored"])
            .unwrap()
            .stdout,
        "explicitly staged"
    );
    assert_eq!(
        git.try_run(repo.path(), &["show", "HEAD:configuration"])
            .unwrap()
            .stdout,
        "included by exact exclusion"
    );
    assert_eq!(
        git.try_run(repo.path(), &["diff", "--cached", "--name-only"])
            .unwrap()
            .trimmed(),
        ".exomonad/runtime\nconfig/private"
    );
    assert_eq!(
        git.checkpoint_source(
            repo.path(),
            &[OsString::from(".exomonad"), OsString::from("config")]
        )
        .unwrap(),
        committed
    );
}

#[test]
fn checkpoint_refuses_detached_and_in_progress_repositories_without_mutation() {
    let repo = TestRepo::init().unwrap();
    let git = GitCli::new();
    let head = repo.writer().commit_file("file", "old", "seed").unwrap();
    fs::write(repo.path().join("file"), "new").unwrap();
    git.try_run(repo.path(), &["checkout", "--detach", "-q"])
        .unwrap();
    assert!(matches!(
        git.checkpoint_source(repo.path(), &[]),
        Err(WorktreeError::GitFailure(_))
    ));
    assert_eq!(
        git.try_run(repo.path(), &["rev-parse", "HEAD"])
            .unwrap()
            .trimmed(),
        head.as_str()
    );
    git.try_run(repo.path(), &["checkout", "-q", "main"])
        .unwrap();
    fs::write(repo.path().join(".git/MERGE_HEAD"), head.as_str()).unwrap();
    assert!(matches!(
        git.checkpoint_source(repo.path(), &[]),
        Err(WorktreeError::SourceOperationInProgress(_))
    ));
    assert_eq!(
        git.try_run(repo.path(), &["rev-parse", "HEAD"])
            .unwrap()
            .trimmed(),
        head.as_str()
    );
}

#[test]
fn failed_commit_preserves_head_and_real_index() {
    let repo = TestRepo::init().unwrap();
    let git = repo.git();
    let head = repo.writer().commit_file("file", "old", "seed").unwrap();
    fs::write(repo.path().join("file"), "new").unwrap();
    git.try_run(repo.path(), &["add", "file"]).unwrap();
    git.try_run(repo.path(), &["config", "user.name", ""])
        .unwrap();
    git.try_run(repo.path(), &["config", "user.email", ""])
        .unwrap();
    assert!(matches!(
        git.checkpoint_source(repo.path(), &[]),
        Err(WorktreeError::GitFailure(_))
    ));
    assert_eq!(
        git.try_run(repo.path(), &["rev-parse", "HEAD"])
            .unwrap()
            .trimmed(),
        head.as_str()
    );
    assert_eq!(
        git.try_run(repo.path(), &["show", ":file"]).unwrap().stdout,
        "new"
    );
    assert_eq!(
        git.try_run(repo.path(), &["status", "--porcelain"])
            .unwrap()
            .trimmed(),
        "M  file"
    );
}

#[test]
fn failed_workspace_initialization_retains_a_provisional_checkout() {
    let workspace = TestRepo::init().expect("init workspace repository");
    let workspace_commit = workspace
        .writer()
        .commit_file("module.txt", "workspace\n", "workspace commit")
        .expect("commit workspace object");
    let repo = TestRepo::init().expect("init project repository");
    let git = repo.git();
    repo.writer()
        .commit_file("README.md", "project\n", "project seed")
        .expect("project seed");
    let missing_url = repo.path().join("unreachable-workspace.git");
    assert!(!missing_url.exists());
    repo.writer()
        .write_file(
            ".gitmodules",
            &format!(
                "[submodule \"exomonad-workspace\"]\n\tpath = .exomonad/workspace\n\turl = {}\n",
                missing_url.display()
            ),
        )
        .expect("write submodule URL");
    git.try_run(repo.path(), &["add", ".gitmodules"])
        .expect("stage submodule configuration");
    git.try_run(
        repo.path(),
        &[
            "update-index",
            "--add",
            "--cacheinfo",
            &format!("160000,{},.exomonad/workspace", workspace_commit.as_str()),
        ],
    )
    .expect("stage workspace gitlink");
    git.try_run(
        repo.path(),
        &["commit", "-q", "-m", "record workspace gitlink"],
    )
    .expect("commit project gitlink");
    let source_head = git.try_run(repo.path(), &["rev-parse", "HEAD"]).unwrap();
    let source_index = git.try_run(repo.path(), &["ls-files", "--stage"]).unwrap();

    let storage = tempfile::tempdir().expect("storage directory");
    let storage_anchor = DirectoryAnchor::open_existing(storage.path()).unwrap();
    let manager = WorktreeManager::new(
        GitCli::new(),
        WorktreeRegistry::open(&storage_anchor, "registry").expect("registry"),
        storage.path().join("worktrees"),
        repo.path(),
    );
    let error = manager
        .prepare_inherited_source(&WorktreeSource::CurrentRepository)
        .expect_err("unreachable workspace URL must fail initialization");
    let WorktreeError::GitFailure(receipt) = error else {
        panic!("expected a Git failure from submodule initialization: {error:?}");
    };
    assert!(receipt.args.iter().any(|arg| arg == "submodule"));
    assert!(receipt.cwd.starts_with(storage.path().join("worktrees")));

    let listed = manager.list().expect("retained registry listing");
    let [summary] = listed.as_slice() else {
        panic!("expected one retained checkout: {listed:?}");
    };
    assert_eq!(summary.receipt.status, WorktreeRecordStatus::Provisional);
    assert_eq!(summary.receipt.cwd, receipt.cwd);
    assert!(summary.present, "failed checkout remains on disk");
    assert!(summary.receipt.cwd.join(".git").exists());
    assert_eq!(summary.receipt.source_head.as_str(), source_head.trimmed());
    assert_eq!(
        git.try_run(
            repo.path(),
            &[
                "rev-parse",
                &format!(
                    "refs/heads/{}",
                    summary
                        .receipt
                        .branch
                        .as_ref()
                        .expect("managed branch")
                        .as_str()
                )
            ]
        )
        .expect("retained branch")
        .trimmed(),
        source_head.trimmed()
    );
    assert!(matches!(
        manager.lookup(&summary.receipt.worktree_id),
        Err(WorktreeError::WorktreeAuthorityDenied(_))
    ));
    assert_eq!(
        git.try_run(repo.path(), &["rev-parse", "HEAD"]).unwrap(),
        source_head
    );
    assert_eq!(
        git.try_run(repo.path(), &["ls-files", "--stage"]).unwrap(),
        source_index
    );
}
