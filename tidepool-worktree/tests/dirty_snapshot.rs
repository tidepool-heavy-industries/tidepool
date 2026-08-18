//! The untouched-source proof — LANE L2.
//!
//! Every test here runs against a REAL temporary repository built by
//! [`tidepool_worktree::testing::TestRepo`] and driven by its scripted writer;
//! there is no mock of git.
//!
//! `snapshot_source` calls `git::inspect::dirty_summary` (LANE L1's `pre_status`
//! source) after its refuse-first checks.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use tidepool_worktree::snapshot::snapshot_source;
use tidepool_worktree::testing::{fingerprint, TestRepo};
use tidepool_worktree::{
    GitCli, InProgressKind, WorktreeError, WorktreeId, TIDEPOOL_SNAPSHOT_REF_PREFIX,
};

/// Everything about the source that `allowDirtySnapshot` must leave alone,
/// captured fresh (never cached) so a before/after comparison is honest.
struct SourceState {
    head: String,
    branch: Option<String>,
    index_bytes: Vec<u8>,
    diff_cached: String,
    diff: String,
    fingerprint: BTreeMap<String, (Vec<u8>, u32)>,
    untracked: Vec<String>,
    branch_list: String,
    reflog: String,
}

fn capture_source_state(git: &GitCli, path: &Path) -> SourceState {
    let head = git
        .try_run(path, &["rev-parse", "HEAD"])
        .expect("rev-parse HEAD")
        .trimmed()
        .to_string();
    let branch = git
        .run(path, &["symbolic-ref", "--short", "HEAD"])
        .ok()
        .map(|o| o.trimmed().to_string());
    let index_bytes = std::fs::read(path.join(".git").join("index")).expect("read .git/index");
    let diff_cached = git
        .try_run(path, &["diff", "--cached"])
        .expect("diff --cached")
        .stdout;
    let diff = git.try_run(path, &["diff"]).expect("diff").stdout;
    let status = git
        .try_run(path, &["status", "--porcelain"])
        .expect("status --porcelain")
        .stdout;
    let untracked: Vec<String> = status
        .lines()
        .filter(|l| l.starts_with("??"))
        .map(|l| l.trim_start_matches("??").trim().to_string())
        .collect();
    let branch_list = git
        .try_run(path, &["branch", "--list"])
        .expect("branch --list")
        .stdout;
    let reflog = git
        .try_run(path, &["reflog", "show", "HEAD"])
        .expect("reflog show HEAD")
        .stdout;

    SourceState {
        head,
        branch,
        index_bytes,
        diff_cached,
        diff,
        fingerprint: fingerprint::working_tree(path),
        untracked,
        branch_list,
        reflog,
    }
}

/// Properties 1, 2, 3, 4, 5, 6, and 8 of the untouched-source proof, each
/// asserted separately so a failure names exactly what moved. Property 7
/// (ignored files) needs the snapshot receipt too, so callers assert it
/// alongside this.
fn assert_source_untouched(before: &SourceState, after: &SourceState) {
    // 1. checked-out branch and HEAD.
    assert_eq!(before.branch, after.branch, "checked-out branch moved");
    assert_eq!(before.head, after.head, "HEAD moved");
    // 2. the ordinary index, byte for byte.
    assert_eq!(
        before.index_bytes, after.index_bytes,
        "ordinary .git/index bytes changed"
    );
    // 3. staged content.
    assert_eq!(
        before.diff_cached, after.diff_cached,
        "staged content changed (git diff --cached)"
    );
    // 4. unstaged content.
    assert_eq!(
        before.diff, after.diff,
        "unstaged content changed (git diff)"
    );
    // 5. working-tree file bytes and mode bits.
    assert_eq!(
        before.fingerprint, after.fingerprint,
        "working-tree file bytes or mode bits changed"
    );
    // 6. untracked files: still untracked, still present, unmodified.
    assert_eq!(
        before.untracked, after.untracked,
        "the set of untracked files changed"
    );
    // 8a. no new branch.
    assert_eq!(
        before.branch_list, after.branch_list,
        "a branch appeared or disappeared (git branch --list)"
    );
    // 8b. no new HEAD reflog entry.
    assert_eq!(
        before.reflog, after.reflog,
        "a new HEAD reflog entry appeared"
    );
}

#[cfg(unix)]
fn set_executable(path: &Path) {
    use std::os::unix::fs::PermissionsExt;
    let mut perms = std::fs::metadata(path).expect("stat").permissions();
    perms.set_mode(0o755);
    std::fs::set_permissions(path, perms).expect("chmod +x");
}

/// A deliberately messy source: a staged edit, an unstaged edit to a
/// different tracked file, an untracked file, an ignored file, a tracked file
/// whose executable bit was flipped without a content change, and a CLEAN
/// submodule whose checked-out commit has moved past what the superproject
/// recorded (a legitimate gitlink bump, not dirtiness).
fn build_messy_repo() -> TestRepo {
    let inner = TestRepo::init().expect("init inner (submodule source)");
    inner
        .writer()
        .commit_file("inner.txt", "one\n", "inner init")
        .expect("inner init commit");

    let repo = TestRepo::init().expect("init outer");
    let w = repo.writer();
    w.commit_file("tracked.txt", "original\n", "init")
        .expect("c1");
    w.commit_file("other.txt", "original other\n", "c2")
        .expect("c2");
    w.write_file(".gitignore", "ignored.txt\n")
        .expect("write .gitignore");
    w.stage(".gitignore").expect("stage .gitignore");
    repo.git()
        .try_run(repo.path(), &["commit", "-q", "-m", "gitignore"])
        .expect("commit .gitignore");
    w.commit_file("exec.txt", "#!/bin/sh\necho hi\n", "add exec.txt")
        .expect("commit exec.txt");

    repo.git()
        .try_run(
            repo.path(),
            &[
                "-c",
                "protocol.file.allow=always",
                "submodule",
                "add",
                inner.path().to_str().expect("inner path is utf8"),
                "sub",
            ],
        )
        .expect("submodule add");
    repo.git()
        .try_run(repo.path(), &["commit", "-q", "-m", "add submodule"])
        .expect("commit submodule addition");

    // Advance the submodule's OWN checkout to a new, still-clean commit — a
    // clean gitlink bump. The submodule's working tree and index end clean
    // because `commit_file` writes, stages, and commits in one go.
    repo.writer_at(repo.path().join("sub"))
        .commit_file("inner.txt", "one\ntwo\n", "inner advance")
        .expect("advance submodule checkout");

    // Now make the source messy on top of that.
    w.write_file("tracked.txt", "staged edit\n")
        .expect("write tracked.txt");
    w.stage("tracked.txt").expect("stage tracked.txt");
    w.write_file("other.txt", "original other\nunstaged edit\n")
        .expect("write other.txt");
    w.write_file("untracked.txt", "brand new\n")
        .expect("write untracked.txt");
    w.write_file("ignored.txt", "must never be captured\n")
        .expect("write ignored.txt");
    #[cfg(unix)]
    set_executable(&repo.path().join("exec.txt"));

    // `inner`'s objects were cloned into `sub` at `submodule add` time; it is
    // not needed after that.
    drop(inner);
    repo
}

#[test]
fn dirty_source_untouched_after_snapshot() {
    let repo = build_messy_repo();
    let before = capture_source_state(repo.git(), repo.path());

    let temp_index_dir = tempfile::TempDir::new().expect("temp index dir");
    let worktree_id = WorktreeId::from_raw("w-messy");
    let receipt = snapshot_source(repo.git(), repo.path(), &worktree_id, temp_index_dir.path())
        .expect("snapshot succeeds against a dirty, non-conflicted, submodule-clean source");

    let after = capture_source_state(repo.git(), repo.path());
    assert_source_untouched(&before, &after);

    // 7. ignored files: still ignored, still present, NOT captured.
    assert!(
        repo.path().join("ignored.txt").exists(),
        "ignored.txt must still be present in the source"
    );
    assert!(
        repo.git()
            .run(repo.path(), &["check-ignore", "ignored.txt"])
            .is_ok(),
        "ignored.txt must still be ignored by git"
    );
    assert!(
        !receipt.captured_paths.iter().any(|p| p == "ignored.txt"),
        "ignored.txt must not appear in captured_paths: {:?}",
        receipt.captured_paths
    );

    for expected in [
        "tracked.txt",
        "other.txt",
        "untracked.txt",
        "exec.txt",
        "sub",
    ] {
        assert!(
            receipt.captured_paths.iter().any(|p| p == expected),
            "expected {expected} to be captured, got {:?}",
            receipt.captured_paths
        );
    }
    assert_eq!(
        receipt.captured_paths,
        {
            let mut sorted = receipt.captured_paths.clone();
            sorted.sort();
            sorted.dedup();
            sorted
        },
        "captured_paths must be sorted and deduplicated"
    );
}

#[test]
fn snapshot_commit_content_matches_captured_working_tree() {
    let repo = build_messy_repo();
    let temp_index_dir = tempfile::TempDir::new().expect("temp index dir");
    let worktree_id = WorktreeId::from_raw("w-content");
    let receipt = snapshot_source(repo.git(), repo.path(), &worktree_id, temp_index_dir.path())
        .expect("snapshot succeeds");

    let scratch = tempfile::TempDir::new().expect("scratch dir");
    repo.git()
        .try_run(
            repo.path(),
            &[
                "worktree",
                "add",
                "--detach",
                scratch.path().to_str().expect("scratch path is utf8"),
                receipt.snapshot_commit.as_str(),
            ],
        )
        .expect("check out the snapshot commit into a scratch worktree");

    let source_fp = fingerprint::working_tree(repo.path());
    let scratch_fp = fingerprint::working_tree(scratch.path());

    // `sub` is a gitlink, not a regular file — the fingerprint only walks
    // regular files, so its content is verified separately below.
    for path in receipt
        .captured_paths
        .iter()
        .filter(|p| p.as_str() != "sub")
    {
        let source_entry = source_fp.get(path);
        assert!(
            source_entry.is_some(),
            "captured path {path} should be a real file in the source"
        );
        assert_eq!(
            source_entry,
            scratch_fp.get(path),
            "captured path {path} differs in bytes or mode between the source and the snapshot checkout"
        );
    }

    let submodule_head = repo
        .git()
        .try_run(&repo.path().join("sub"), &["rev-parse", "HEAD"])
        .expect("submodule HEAD")
        .trimmed()
        .to_string();
    let ls_tree = repo
        .git()
        .try_run(
            repo.path(),
            &["ls-tree", receipt.snapshot_commit.as_str(), "--", "sub"],
        )
        .expect("ls-tree sub")
        .stdout;
    assert!(
        ls_tree.contains(&submodule_head),
        "the snapshot's gitlink for sub must point at the submodule's checked-out commit \
         {submodule_head}, got: {ls_tree}"
    );

    assert!(
        !scratch_fp.contains_key("ignored.txt"),
        "ignored.txt must be absent from the snapshot checkout"
    );
}

#[test]
fn snapshot_ref_lives_outside_refs_heads_and_never_in_branch_list() {
    let repo = build_messy_repo();
    let temp_index_dir = tempfile::TempDir::new().expect("temp index dir");
    let worktree_id = WorktreeId::from_raw("w-ref-namespace");
    let receipt = snapshot_source(repo.git(), repo.path(), &worktree_id, temp_index_dir.path())
        .expect("snapshot succeeds");

    assert!(
        receipt
            .snapshot_ref
            .as_str()
            .starts_with(TIDEPOOL_SNAPSHOT_REF_PREFIX),
        "snapshot ref must live under {TIDEPOOL_SNAPSHOT_REF_PREFIX}: {}",
        receipt.snapshot_ref.as_str()
    );
    assert!(
        !receipt.snapshot_ref.as_str().starts_with("refs/heads/"),
        "snapshot ref must not be a branch ref: {}",
        receipt.snapshot_ref.as_str()
    );

    let branch_list = repo
        .git()
        .try_run(repo.path(), &["branch", "--list"])
        .expect("branch --list")
        .stdout;
    assert!(
        !branch_list.contains(worktree_id.as_str()),
        "the snapshot must never appear in `git branch --list`: {branch_list}"
    );

    let reachable_from_heads = repo
        .git()
        .try_run(
            repo.path(),
            &[
                "branch",
                "--list",
                "--contains",
                receipt.snapshot_commit.as_str(),
            ],
        )
        .expect("branch --contains");
    assert!(
        reachable_from_heads.stdout.trim().is_empty(),
        "the snapshot commit must not be reachable from any branch: {}",
        reachable_from_heads.stdout
    );
}

#[test]
fn refuses_when_source_is_mid_merge_and_leaves_it_untouched() {
    let repo = TestRepo::init().expect("init");
    let w = repo.writer();
    w.commit_file("f.txt", "base\n", "base")
        .expect("base commit");
    w.checkout_new_branch("feature").expect("branch feature");
    w.commit_file("f.txt", "feature change\n", "feature commit")
        .expect("feature commit");
    w.checkout("main").expect("checkout main");
    w.commit_file("f.txt", "main change\n", "main commit")
        .expect("main commit");
    let _ = repo
        .git()
        .run(repo.path(), &["merge", "--no-edit", "feature"]);
    assert!(
        repo.path().join(".git").join("MERGE_HEAD").exists(),
        "test setup must actually produce a conflicted, in-progress merge"
    );

    let before = capture_source_state(repo.git(), repo.path());
    let temp_index_dir = tempfile::TempDir::new().expect("temp index dir");
    let worktree_id = WorktreeId::from_raw("w-merge");
    let err = snapshot_source(repo.git(), repo.path(), &worktree_id, temp_index_dir.path())
        .expect_err("a mid-merge source must be refused");
    assert_eq!(
        err,
        WorktreeError::SourceOperationInProgress(InProgressKind::Merge)
    );

    let after = capture_source_state(repo.git(), repo.path());
    assert_source_untouched(&before, &after);
}

#[test]
fn refuses_when_source_is_mid_rebase_and_leaves_it_untouched() {
    let repo = TestRepo::init().expect("init");
    let w = repo.writer();
    w.commit_file("f.txt", "base\n", "base")
        .expect("base commit");
    w.checkout_new_branch("feature").expect("branch feature");
    w.commit_file("f.txt", "feature change\n", "feature commit")
        .expect("feature commit");
    w.checkout("main").expect("checkout main");
    w.commit_file("f.txt", "main change\n", "main commit")
        .expect("main commit");
    w.checkout("feature").expect("checkout feature");
    let _ = w.rebase_onto("main");
    assert!(
        repo.path().join(".git").join("rebase-apply").exists()
            || repo.path().join(".git").join("rebase-merge").exists(),
        "test setup must actually produce a conflicted, in-progress rebase"
    );

    let before = capture_source_state(repo.git(), repo.path());
    let temp_index_dir = tempfile::TempDir::new().expect("temp index dir");
    let worktree_id = WorktreeId::from_raw("w-rebase");
    let err = snapshot_source(repo.git(), repo.path(), &worktree_id, temp_index_dir.path())
        .expect_err("a mid-rebase source must be refused");
    assert_eq!(
        err,
        WorktreeError::SourceOperationInProgress(InProgressKind::Rebase)
    );

    let after = capture_source_state(repo.git(), repo.path());
    assert_source_untouched(&before, &after);
}

#[test]
fn refuses_dirty_submodule_and_leaves_source_untouched() {
    let repo = build_messy_repo();
    // Dirty the submodule's OWN working tree, beyond its already-clean
    // gitlink bump — uncommitted content inside the submodule itself.
    std::fs::write(
        repo.path().join("sub").join("inner.txt"),
        "dirtied in place\n",
    )
    .expect("dirty submodule content");

    let before = capture_source_state(repo.git(), repo.path());
    let temp_index_dir = tempfile::TempDir::new().expect("temp index dir");
    let worktree_id = WorktreeId::from_raw("w-dirty-submodule");
    let err = snapshot_source(repo.git(), repo.path(), &worktree_id, temp_index_dir.path())
        .expect_err("a dirty submodule must be refused");
    match err {
        WorktreeError::DirtySubmoduleUnsupported(path) => {
            assert_eq!(path, PathBuf::from("sub"));
        }
        other => panic!("expected DirtySubmoduleUnsupported, got {other:?}"),
    }

    let after = capture_source_state(repo.git(), repo.path());
    assert_source_untouched(&before, &after);
}

/// `GIT_INDEX_FILE` routes git's own read/write to `temp_index_path` — a
/// non-UTF-8 `temp_index_dir` (raw bytes, unix-only) must be refused as a
/// typed `StorageFailure` here, at the point the path is about to become
/// text handed to git, rather than silently mangled into pointing git at
/// the wrong (or no) file.
#[cfg(unix)]
#[test]
fn refuses_non_utf8_temp_index_dir_and_leaves_source_untouched() {
    use std::ffi::OsString;
    use std::os::unix::ffi::OsStringExt;

    let repo = TestRepo::init().expect("init");
    repo.writer()
        .commit_file("f.txt", "base\n", "base commit")
        .expect("base commit");

    let before = capture_source_state(repo.git(), repo.path());
    let parent = tempfile::TempDir::new().expect("temp parent dir");
    // `0x80` alone is not a valid UTF-8 lead byte.
    let bad_name = OsString::from_vec(vec![b'i', b'd', 0x80, b'x']);
    let temp_index_dir = parent.path().join(PathBuf::from(bad_name));

    let worktree_id = WorktreeId::from_raw("w-nonutf8");
    let err = snapshot_source(repo.git(), repo.path(), &worktree_id, &temp_index_dir)
        .expect_err("a non-UTF-8 temp index dir must be refused, not silently mangled");
    match err {
        WorktreeError::StorageFailure { detail, .. } => {
            assert!(detail.contains("UTF-8"), "{detail}");
        }
        other => panic!("expected StorageFailure, got {other:?}"),
    }

    let after = capture_source_state(repo.git(), repo.path());
    assert_source_untouched(&before, &after);
}

/// Dirty capture through the PRODUCTION path (`WorktreeManager::create` with
/// `allowDirtySnapshot`), not `snapshot_source` handed a pre-made temp dir.
/// The manager derives its temp-index dir under `worktree_root` and nothing
/// pre-creates it — this test is red if `snapshot_source` assumes the dir
/// exists (git creates the index FILE, never its parent directories).
#[test]
fn manager_level_dirty_create_captures_through_a_nonexistent_index_dir() {
    use tidepool_worktree::{
        DirtyPolicy, WorktreeManager, WorktreeRegistry, WorktreeSource, WorktreeSpec,
    };

    let repo = TestRepo::init().expect("init");
    repo.writer()
        .commit_file("tracked.txt", "committed\n", "first")
        .expect("commit");
    std::fs::write(repo.path().join("tracked.txt"), "dirtied\n").expect("dirty the tree");

    let base = tempfile::TempDir::new().expect("tempdir");
    let registry = WorktreeRegistry::open(base.path().join("registry")).expect("open registry");
    let manager = WorktreeManager::new(
        GitCli::new(),
        registry,
        base.path().join("worktrees"),
        repo.path(),
    );

    let before = capture_source_state(repo.git(), repo.path());
    let handle = manager
        .create(&WorktreeSpec {
            source: WorktreeSource::CurrentRepository,
            label: "dirty-capture".to_string(),
            dirty_policy: DirtyPolicy::AllowDirtySnapshot,
        })
        .expect("dirty create must succeed through the manager-owned index dir");
    assert!(
        handle.receipt().snapshot_ref.is_some(),
        "a dirty source must yield a snapshot ref"
    );
    // NOT `assert_source_untouched`: the manager path legitimately adds a
    // managed `tidepool/worktree/…` branch to the source repo (that branch IS
    // the mechanism). What must be untouched: bytes, HEAD, checked-out
    // branch, index, and the dirty state itself.
    let after = capture_source_state(repo.git(), repo.path());
    assert_eq!(before.head, after.head, "source HEAD moved");
    assert_eq!(before.branch, after.branch, "checked-out branch changed");
    assert_eq!(before.index_bytes, after.index_bytes, ".git/index changed");
    assert_eq!(before.diff, after.diff, "unstaged diff changed");
    assert_eq!(before.diff_cached, after.diff_cached, "staged diff changed");
    assert_eq!(
        std::fs::read(repo.path().join("tracked.txt")).expect("read back"),
        b"dirtied\n",
        "working-tree bytes changed"
    );
}

/// Pins the currently-unpinned tolerance flagged by an external type review on
/// 2026-08-17 (see §11.11 of `plans/self-iterating-harness/22-p1-protocol-scaffold.md`):
/// `WorktreeSource::Ref` resolves a named ref via `rev-parse` and never
/// consults `spec.dirty_policy` at all. This is correct as far as it goes — a
/// named ref is already-committed content, so there is nothing uncommitted to
/// snapshot — but the TYPE nevertheless permits the pair
/// `(Ref, AllowDirtySnapshot)`, so a caller can ask for a dirty-snapshot
/// opt-in against a ref source and silently not get one. This test pins
/// TODAY's behavior (the pair is accepted and produces identical results
/// under both policies); the follow-on lane that makes the pair
/// unrepresentable is expected to CHANGE this test in the same commit as the
/// fix, not to leave it standing.
///
/// Contrast: the same dirty repository through
/// `WorktreeSource::CurrentRepository` + `AllowDirtySnapshot` DOES produce
/// `snapshot_ref: Some(..)` — see
/// `manager_level_dirty_create_captures_through_a_nonexistent_index_dir`
/// above.
#[test]
fn ref_source_ignores_dirty_policy_and_never_snapshots() {
    use tidepool_worktree::{
        DirtyPolicy, GitRef, WorktreeManager, WorktreeRegistry, WorktreeSource, WorktreeSpec,
    };

    let repo = TestRepo::init().expect("init");
    repo.writer()
        .commit_file("tracked.txt", "committed\n", "first")
        .expect("commit");
    let seed_commit = repo
        .git()
        .try_run(repo.path(), &["rev-parse", "HEAD"])
        .expect("rev-parse HEAD")
        .trimmed()
        .to_string();
    // Dirty the working tree AFTER recording the seed commit, so the ref
    // being resolved is unambiguously already-committed content distinct
    // from the uncommitted mess sitting on top of it.
    std::fs::write(repo.path().join("tracked.txt"), "dirtied\n").expect("dirty the tree");

    let run_with_policy = |policy: DirtyPolicy, label: &str| {
        let base = tempfile::TempDir::new().expect("tempdir");
        let registry = WorktreeRegistry::open(base.path().join("registry")).expect("open registry");
        let manager = WorktreeManager::new(
            GitCli::new(),
            registry,
            base.path().join("worktrees"),
            repo.path(),
        );
        // The returned handle owns a plain-data receipt (already copied out
        // of the registry), so `base` dropping here and reclaiming the
        // worktree directory does not affect anything asserted below.
        manager
            .create(&WorktreeSpec {
                source: WorktreeSource::Ref(GitRef::from_raw("main")),
                label: label.to_string(),
                dirty_policy: policy,
            })
            .expect("ref-source create must succeed regardless of dirty_policy")
    };

    let under_require_clean = run_with_policy(DirtyPolicy::RequireClean, "ref-require-clean");
    let under_allow_dirty = run_with_policy(DirtyPolicy::AllowDirtySnapshot, "ref-allow-dirty");

    for (label, handle) in [
        ("RequireClean", &under_require_clean),
        ("AllowDirtySnapshot", &under_allow_dirty),
    ] {
        assert_eq!(
            handle.receipt().source_head.as_str(),
            seed_commit,
            "{label}: a ref source must seed at the ref's commit"
        );
        // The load-bearing assertion: despite AllowDirtySnapshot being
        // requested, a ref source never takes a snapshot, because a named
        // ref has nothing uncommitted to snapshot in the first place.
        assert_eq!(
            handle.receipt().snapshot_ref,
            None,
            "{label}: a ref source must never record a snapshot_ref"
        );
    }
}

/// A staged rename (`git mv`) must not resurrect the OLD path in the snapshot:
/// `read-tree HEAD` seeds the temp index with the old path, and only staging
/// the rename's BOTH sides records the deletion. Pre-fix, the porcelain parser
/// consumed the orig_path field for alignment but discarded it, so the
/// synthetic tree contained the renamed-away file alongside the new one.
#[test]
fn snapshot_of_a_staged_rename_drops_the_old_path() {
    let repo = TestRepo::init().expect("init");
    let w = repo.writer();
    w.commit_file("old_name.txt", "content\n", "init")
        .expect("c1");
    repo.git()
        .try_run(repo.path(), &["mv", "old_name.txt", "new_name.txt"])
        .expect("git mv");

    let temp_index_dir = tempfile::TempDir::new().expect("temp index dir");
    let receipt = snapshot_source(
        repo.git(),
        repo.path(),
        &WorktreeId::from_raw("w-rename"),
        temp_index_dir.path(),
    )
    .expect("snapshot succeeds");

    let tree = repo
        .git()
        .try_run(
            repo.path(),
            &[
                "ls-tree",
                "-r",
                "--name-only",
                receipt.snapshot_commit.as_str(),
            ],
        )
        .expect("ls-tree")
        .stdout;
    let names: Vec<&str> = tree.lines().collect();
    assert!(
        names.contains(&"new_name.txt"),
        "the rename's new path must be captured: {names:?}"
    );
    assert!(
        !names.contains(&"old_name.txt"),
        "the rename's old path must NOT survive into the snapshot: {names:?}"
    );
}
