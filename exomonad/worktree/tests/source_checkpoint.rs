use std::ffi::OsString;
use std::fs;

use exomonad_worktree::testing::TestRepo;
use exomonad_worktree::{GitCli, WorktreeError};

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
