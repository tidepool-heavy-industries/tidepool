use super::*;
use exomonad_actor::{ForkWorkspaceAdmission, ForkWorkspacePolicy};
use exomonad_agent::interactive::*;
use exomonad_agent::{AgentBackendError, BackendThreadId};
use std::io::{BufRead, BufReader};
use std::os::unix::fs::MetadataExt;
use std::process::{Child, Stdio};

#[test]
fn helper_draft_fork_is_an_independent_file_snapshot() {
    let root = tempfile::tempdir().unwrap();
    let parent = root.path().join("helpers/drafts/parent");
    let child = root.path().join("helpers/drafts/child");
    std::fs::create_dir_all(parent.join("SessionHelpers")).unwrap();
    std::fs::write(
        parent.join("SessionHelpers.hs"),
        "module SessionHelpers where\nvalue = 1\n",
    )
    .unwrap();
    std::fs::write(
        parent.join("SessionHelpers/Child.hs"),
        "module SessionHelpers.Child where\nchild = 1\n",
    )
    .unwrap();

    copy_helper_draft(&parent, &child).unwrap();
    std::fs::write(
        parent.join("SessionHelpers.hs"),
        "module SessionHelpers where\nvalue = 2\n",
    )
    .unwrap();
    assert!(std::fs::read_to_string(child.join("SessionHelpers.hs"))
        .unwrap()
        .contains("value = 1"));
    assert_eq!(
        std::fs::read_to_string(child.join("SessionHelpers/Child.hs")).unwrap(),
        "module SessionHelpers.Child where\nchild = 1\n"
    );
    assert!(copy_helper_draft(&parent, &child).is_err());
}

#[test]
fn helper_fork_keeps_invalid_draft_separate_from_last_active_revision() {
    let root = tempfile::tempdir().unwrap();
    let helper_root = root.path().join("helpers");
    let parent_draft = helper_root.join("drafts/parent");
    let child_draft = helper_root.join("drafts/child");
    std::fs::create_dir_all(&parent_draft).unwrap();
    std::fs::write(
        parent_draft.join("SessionHelpers.hs"),
        "module SessionHelpers where\nvalue = 1\n",
    )
    .unwrap();
    let domain = "helper-fork-test";
    let parent_layer = crate::exomonad::source::SourceLayer::helpers(&helper_root, "parent");
    let active = parent_layer
        .ensure_active_from(domain, std::slice::from_ref(&parent_draft))
        .unwrap();

    std::fs::write(
        parent_draft.join("SessionHelpers.hs"),
        "module SessionHelpers where\nvalue = (\n",
    )
    .unwrap();
    copy_helper_draft(&parent_draft, &child_draft).unwrap();
    let child_layer = crate::exomonad::source::SourceLayer::helpers(&helper_root, "child");
    let inherited = child_layer
        .inherit_active_from(&parent_layer, std::slice::from_ref(&child_draft))
        .unwrap();

    assert_eq!(inherited.identity, active.identity);
    assert!(
        std::fs::read_to_string(child_draft.join("SessionHelpers.hs"))
            .unwrap()
            .contains("value = (")
    );
    assert!(std::fs::read_to_string(
        child_layer.active_include_paths().unwrap()[0].join("SessionHelpers.hs")
    )
    .unwrap()
    .contains("value = 1"));
}

#[test]
fn prepared_fork_inherits_the_published_helper_revision() {
    let repo = exomonad_worktree::testing::TestRepo::init().unwrap();
    let runtime = tempfile::tempdir().unwrap();
    let (manager, _) = actor_worktree_resources_at(runtime.path(), repo.path()).unwrap();
    let layout = WorkspaceLayout {
        run_namespace: "published-helper-fork".into(),
        source_root: repo.path().into(),
        source_exclude: Vec::new(),
        root_imports: Arc::default(),
        worktrees: manager,
        base_prompt: FrozenBasePrompt::materialize(runtime.path()).unwrap(),
        backend: Arc::new(Backend::default()),
    };
    let parent_draft = layout.helper_draft("run");
    std::fs::create_dir_all(parent_draft.join("SessionHelpers")).unwrap();
    std::fs::write(
        parent_draft.join("SessionHelpers/TestEvidence.hs"),
        "module SessionHelpers.TestEvidence where\nvalue = 1\n",
    )
    .unwrap();
    let parent_layer = crate::exomonad::source::SourceLayer::helpers(&layout.helper_root(), "run");
    let published = parent_layer
        .ensure_active_from("published-helper-fork", std::slice::from_ref(&parent_draft))
        .unwrap();

    std::fs::write(
        parent_draft.join("SessionHelpers/TestEvidence.hs"),
        "module SessionHelpers.TestEvidence where\nvalue = (\n",
    )
    .unwrap();
    layout
        .inherit_helper_branch(&parent_draft, "reviewer")
        .unwrap();

    let child_layer =
        crate::exomonad::source::SourceLayer::helpers(&layout.helper_root(), "reviewer");
    assert_eq!(
        std::fs::read_link(layout.helper_root().join("layers/reviewer/active")).unwrap(),
        PathBuf::from("revisions").join(&published.identity)
    );
    assert_eq!(
        std::fs::read_to_string(
            child_layer.active_include_paths().unwrap()[0].join("SessionHelpers/TestEvidence.hs")
        )
        .unwrap(),
        "module SessionHelpers.TestEvidence where\nvalue = 1\n"
    );
    assert!(std::fs::read_to_string(
        layout
            .helper_draft("reviewer")
            .join("SessionHelpers/TestEvidence.hs")
    )
    .unwrap()
    .contains("value = ("));
}

#[test]
fn helper_draft_fork_rejects_symlink_entries() {
    use std::os::unix::fs::symlink;

    let root = tempfile::tempdir().unwrap();
    let parent = root.path().join("parent");
    let child = root.path().join("child");
    std::fs::create_dir_all(&parent).unwrap();
    let outside = root.path().join("outside.hs");
    std::fs::write(&outside, "module SessionHelpers where").unwrap();
    symlink(&outside, parent.join("SessionHelpers.hs")).unwrap();
    assert!(copy_helper_draft(&parent, &child).is_err());
    assert!(!child.exists());
}

#[derive(Default)]
struct Backend {
    identity: Mutex<Option<PublicationIdentity>>,
    peer_pid: Mutex<Option<u32>>,
    busy: Mutex<bool>,
    calls: Mutex<Vec<(u64, PublicationOperation)>>,
    lose_finish: Mutex<bool>,
    lose_begin: Mutex<bool>,
    unavailable: Mutex<bool>,
    begin_pause: Mutex<
        Option<(
            tokio::sync::oneshot::Sender<()>,
            tokio::sync::oneshot::Receiver<()>,
        )>,
    >,
}

impl InteractiveAgentBackend for Backend {
    fn prepare_native_tool_policy(
        &self,
        _: InteractiveNativeToolPolicy,
        _: &Path,
    ) -> Result<Vec<InteractivePolicyMount>, AgentBackendError> {
        Ok(vec![])
    }
    fn render(
        &self,
        _: &InteractiveAgentSpec,
    ) -> Result<InteractiveAgentCommand, AgentBackendError> {
        unreachable!()
    }
    fn push<'a>(
        &'a self,
        _: &'a str,
        _: &'a QueueReadyThread,
        _: &'a str,
    ) -> InteractiveFuture<'a, ()> {
        unreachable!()
    }
    fn archive<'a>(&'a self, _: &'a str, _: &'a QueueReadyThread) -> InteractiveFuture<'a, ()> {
        unreachable!()
    }
    fn workspace_publication<'a>(
        &'a self,
        _: &'a QueueReadyThread,
        sequence: std::num::NonZeroU64,
        operation: PublicationOperation,
    ) -> InteractiveFuture<'a, PublicationReply> {
        Box::pin(async move {
            self.calls.lock().push((sequence.get(), operation));
            match operation {
                PublicationOperation::Begin { expected } => {
                    let pause = self.begin_pause.lock().take();
                    if let Some((entered, release)) = pause {
                        entered.send(()).unwrap();
                        release.await.unwrap();
                    }
                    if *self.unavailable.lock() {
                        return Ok(PublicationReply::Unavailable {
                            detail: "test native unavailable".into(),
                        });
                    }
                    if std::mem::take(&mut *self.lose_begin.lock()) {
                        return Err(AgentBackendError::BackendUnavailable {
                            detail: "lost begin reply".into(),
                        });
                    }
                    if *self.busy.lock() {
                        return Ok(PublicationReply::Busy);
                    }
                    let identity = self.identity.lock().unwrap();
                    assert!(expected.is_none_or(|expected| expected == identity));
                    Ok(PublicationReply::Ready {
                        peer_pid: self.peer_pid.lock().unwrap_or(identity.pid),
                        pid: identity.pid,
                        start_ticks: identity.start_ticks,
                        mount_namespace_inode: identity.mount_namespace_inode,
                        cgroup_path: "/test/writers".into(),
                    })
                }
                PublicationOperation::Finish { expected } => {
                    assert_eq!(Some(expected), *self.identity.lock());
                    if std::mem::take(&mut *self.lose_finish.lock()) {
                        Err(AgentBackendError::BackendUnavailable {
                            detail: "lost finish reply".into(),
                        })
                    } else {
                        Ok(PublicationReply::Settled)
                    }
                }
            }
        })
    }
}

struct NativeProcess(Child);
impl NativeProcess {
    fn start(workspace: &PreparedWorkspace, backend: &Backend) -> Self {
        let mut child = workspace
            .view
            .host_command(
                Path::new(ACTOR_PROJECT_ROOT),
                std::ffi::OsStr::new("/bin/sh"),
            )
            .unwrap()
            .args(["-c", "echo $$; read -r ignored"])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .unwrap();
        let mut line = String::new();
        BufReader::new(child.stdout.take().unwrap())
            .read_line(&mut line)
            .unwrap();
        let pid: u32 = line.trim().parse().unwrap();
        let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).unwrap();
        let start_ticks = stat
            .rsplit_once(')')
            .unwrap()
            .1
            .split_whitespace()
            .nth(19)
            .unwrap()
            .parse()
            .unwrap();
        let mount_namespace_inode = std::fs::metadata(format!("/proc/{pid}/ns/mnt"))
            .unwrap()
            .ino();
        *backend.peer_pid.lock() = Some(pid);
        *backend.identity.lock() = Some(PublicationIdentity {
            pid: 2,
            start_ticks,
            mount_namespace_inode,
        });
        Self(child)
    }
}
impl Drop for NativeProcess {
    fn drop(&mut self) {
        drop(self.0.stdin.take());
        // best-effort: teardown of a child process this test started.
        self.0.wait().ok();
    }
}
fn shell(workspace: &PreparedWorkspace, script: &str) -> String {
    let output = workspace
        .view
        .host_command(
            Path::new(ACTOR_PROJECT_ROOT),
            std::ffi::OsStr::new("/bin/sh"),
        )
        .unwrap()
        .args(["-ec", script])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "script {script:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap()
}
fn build(workspace: &PreparedWorkspace) -> Vec<bool> {
    let output = shell(workspace, "CARGO_HOME=\"$PWD/.exomonad/build/cargo/home\" CARGO_TARGET_DIR=\"$PWD/.exomonad/build/cargo\" RUSTC_WRAPPER= cargo build --offline --message-format=json");
    let artifacts: Vec<bool> = output
        .lines()
        .filter_map(|line| {
            let value: serde_json::Value = serde_json::from_str(line).unwrap();
            (value["reason"] == "compiler-artifact").then(|| value["fresh"].as_bool().unwrap())
        })
        .collect();
    assert!(!artifacts.is_empty(), "Cargo reported no artifacts");
    artifacts
}

fn owner(
    workspace: Arc<PreparedWorkspace>,
    thread: QueueReadyThread,
    native: &NativeProcess,
) -> InteractiveApplicationOwner {
    InteractiveApplicationOwner {
        supervisor: None,
        creator_workspace: Some(BoundWorkspace {
            workspace: Arc::new(ActiveWorkspace {
                view: exomonad_node::MountNamespace::capture(native.0.id()).unwrap(),
                prepared: workspace,
            }),
            thread,
        }),
        cancel: None,
        native_retirement: Default::default(),
        pane: Arc::new(Mutex::new(None)),
        fork_gate: None,
        custody: None,
        scoped_retention: None,
        hosted: Arc::new(Mutex::new(None)),
        launch: HostLaunchState::Published,
        pending_activations: Vec::new(),
        terminal: None,
        retirement: Arc::new(Mutex::new(None)),
    }
}
const CODING: ForkWorkspacePolicy = ForkWorkspacePolicy {
    native_tools: exomonad_actor::NativeToolClass::Coding,
    workspace: exomonad_actor::WorkspaceAccess::WritableBound,
};

#[test]
fn source_exclusions_keep_tracked_files_and_untagged_directories() {
    let repo = exomonad_worktree::testing::TestRepo::init().unwrap();
    repo.writer()
        .commit_file("tracked/source", "source", "seed")
        .unwrap();
    for name in ["tracked", "node_modules", "ordinary"] {
        std::fs::create_dir_all(repo.path().join(name)).unwrap();
        std::fs::write(repo.path().join(name).join("asset"), name).unwrap();
    }
    for name in ["tracked", "node_modules"] {
        std::fs::write(
            repo.path().join(name).join("CACHEDIR.TAG"),
            "Signature: 8a477f597d28d172789f06886806bc55\n",
        )
        .unwrap();
    }
    let runtime = tempfile::tempdir().unwrap();
    let (manager, _) = actor_worktree_resources_at(runtime.path(), repo.path()).unwrap();
    let mut layout = WorkspaceLayout {
        run_namespace: "source-exclusion-test".into(),
        source_root: repo.path().into(),
        source_exclude: Vec::new(),
        root_imports: Arc::default(),
        worktrees: manager,
        base_prompt: FrozenBasePrompt::materialize(runtime.path()).unwrap(),
        backend: Arc::new(Backend::default()),
    };
    let excluded = layout.source_exclusions(repo.path(), repo.path()).unwrap();
    assert!(excluded.contains(&"node_modules".into()));
    assert!(!excluded.contains(&"tracked".into()));
    assert!(!excluded.contains(&"ordinary".into()));
    layout.source_exclude.push("ordinary".into());
    assert!(layout
        .source_exclusions(repo.path(), repo.path())
        .unwrap()
        .contains(&"ordinary".into()));
    layout.source_exclude.push("tracked".into());
    assert!(layout.source_exclusions(repo.path(), repo.path()).is_err());
    repo.git()
        .try_run(repo.path(), &["rm", "--cached", "--", "tracked/source"])
        .unwrap();
    assert!(
        layout.source_exclusions(repo.path(), repo.path()).is_err(),
        "HEAD must still protect a staged deletion"
    );
    let mut config = crate::exomonad::LaunchConfig::default();
    for invalid in ["../outside", "", ".git", "build*"] {
        config.source_exclude = vec![invalid.into()];
        assert!(config.validate().is_err(), "{invalid:?}");
    }
}

#[test]
fn root_import_reuse_requires_matching_content_and_exclusions() {
    let repo = exomonad_worktree::testing::TestRepo::init().unwrap();
    repo.writer().commit_file("file", "first", "seed").unwrap();
    std::fs::write(repo.path().join("dirty"), "dirty").unwrap();
    std::fs::hard_link(repo.path().join("file"), repo.path().join("linked")).unwrap();
    std::os::unix::fs::symlink("file", repo.path().join("symlink")).unwrap();
    let runtime = tempfile::tempdir().unwrap();
    let (manager, _) = actor_worktree_resources_at(runtime.path(), repo.path()).unwrap();
    let mut layout = WorkspaceLayout {
        run_namespace: "root-import-test".into(),
        source_root: repo.path().into(),
        source_exclude: Vec::new(),
        root_imports: Arc::default(),
        worktrees: manager,
        base_prompt: FrozenBasePrompt::materialize(runtime.path()).unwrap(),
        backend: Arc::new(Backend::default()),
    };
    let excluded = layout.source_exclusions(repo.path(), repo.path()).unwrap();
    let source =
        OverlayResourceLease::allocate_path(layout.resource_root("first").join("source"), None)
            .unwrap();
    let excluded_refs = excluded
        .iter()
        .map(std::ffi::OsString::as_os_str)
        .collect::<Vec<_>>();
    source.import_source(repo.path(), &excluded_refs).unwrap();
    layout
        .remember_import(repo.path(), &excluded, &source)
        .unwrap();
    drop(source);
    assert!(layout.reusable_import(repo.path(), &excluded).is_some());

    let original = layout.root_imports.lock().get(repo.path()).unwrap().clone();
    std::fs::write(repo.path().join("file"), "later").unwrap();
    assert!(layout.reusable_import(repo.path(), &excluded).is_none());
    // Simulate identical inventory stamps, including a ctime collision. The
    // content manifest must still reject the changed bytes.
    layout.root_imports.lock().insert(
        repo.path().to_path_buf(),
        Arc::new(RootImport {
            inventory: source_inventory(repo.path(), &excluded_refs).unwrap(),
            exclusions: original.exclusions.clone(),
            manifest: original.manifest.clone(),
            snapshot: original.snapshot.clone(),
        }),
    );
    assert!(layout.reusable_import(repo.path(), &excluded).is_none());
    layout.source_exclude.push("scratch".into());
    let changed_exclusions = layout.source_exclusions(repo.path(), repo.path()).unwrap();
    assert!(layout
        .reusable_import(repo.path(), &changed_exclusions)
        .is_none());
}

#[test]
fn root_workspace_resources_are_isolated_between_runs() {
    let repo = exomonad_worktree::testing::TestRepo::init().unwrap();
    repo.writer().commit_file("file", "source", "seed").unwrap();
    let runtime = tempfile::tempdir().unwrap();
    let (manager, _) = actor_worktree_resources_at(runtime.path(), repo.path()).unwrap();
    let legacy = manager.managed_root().join(".resources/actor-0-1/build");
    std::fs::create_dir_all(&legacy).unwrap();
    std::fs::write(legacy.join("retained"), "old run").unwrap();
    let mut layout = WorkspaceLayout {
        run_namespace: "first-run".into(),
        source_root: repo.path().into(),
        source_exclude: Vec::new(),
        root_imports: Arc::default(),
        worktrees: manager,
        base_prompt: FrozenBasePrompt::materialize(runtime.path()).unwrap(),
        backend: Arc::new(Backend::default()),
    };
    let first = layout
        .prepare(
            repo.path().into(),
            None,
            "actor-0-1",
            true,
            CODING,
            None,
            None,
            None,
        )
        .unwrap();
    shell(&first, "echo first > .exomonad/build/cargo/marker");
    let collision = layout
        .prepare(
            repo.path().into(),
            None,
            "actor-0-1",
            true,
            CODING,
            None,
            None,
            None,
        )
        .err()
        .unwrap();
    assert_eq!(collision.kind(), std::io::ErrorKind::AlreadyExists);

    layout.run_namespace = "second-run".into();
    let second = layout
        .prepare(
            repo.path().into(),
            None,
            "actor-0-1",
            true,
            CODING,
            None,
            None,
            None,
        )
        .unwrap();
    shell(
        &second,
        "test ! -e .exomonad/build/cargo/marker; echo second > .exomonad/build/cargo/marker",
    );
    assert_eq!(shell(&first, "cat .exomonad/build/cargo/marker"), "first\n");
    assert_eq!(
        shell(&second, "cat .exomonad/build/cargo/marker"),
        "second\n"
    );
    assert_eq!(
        std::fs::read_to_string(legacy.join("retained")).unwrap(),
        "old run"
    );
}

#[test]
fn root_mount_starts_with_the_pinned_workspace_helper_seed() {
    let repo = exomonad_worktree::testing::TestRepo::init().unwrap();
    repo.writer()
        .commit_file(
            ".exomonad/workspace/seeds/helpers/SessionHelpers.hs",
            "module SessionHelpers where\nseeded :: Int\nseeded = 1\n",
            "add helper seed",
        )
        .unwrap();
    let runtime = tempfile::tempdir().unwrap();
    let (manager, _) = actor_worktree_resources_at(runtime.path(), repo.path()).unwrap();
    let layout = WorkspaceLayout {
        run_namespace: "helper-seed-test".into(),
        source_root: repo.path().into(),
        source_exclude: Vec::new(),
        root_imports: Arc::default(),
        worktrees: manager,
        base_prompt: FrozenBasePrompt::materialize(runtime.path()).unwrap(),
        backend: Arc::new(Backend::default()),
    };
    let root = layout
        .prepare(
            repo.path().into(),
            None,
            "root",
            true,
            CODING,
            None,
            None,
            None,
        )
        .unwrap();
    assert_eq!(
        shell(&root, "cat .exomonad/helpers/SessionHelpers.hs"),
        "module SessionHelpers where\nseeded :: Int\nseeded = 1\n"
    );
}

#[test]
fn root_mount_keeps_authored_helpers_clean_and_preserves_host_bytes() {
    let repo = exomonad_worktree::testing::TestRepo::init().unwrap();
    repo.writer()
        .commit_file(
            ".exomonad/workspace/seeds/helpers/SessionHelpers/BrowserChecks.hs",
            "module SessionHelpers.BrowserChecks where\n",
            "add generic helpers",
        )
        .unwrap();
    for (path, body) in [
        (
            ".exomonad/helpers/README.md",
            "project helper documentation\n",
        ),
        (
            ".exomonad/helpers/SessionHelpers.hs",
            "module SessionHelpers (custom) where\ncustom = 1\n",
        ),
        (
            ".exomonad/helpers/SessionHelpers/Custom.hs",
            "module SessionHelpers.Custom where\n",
        ),
    ] {
        repo.writer()
            .commit_file(path, body, "author project helpers")
            .unwrap();
    }
    let original_git = std::fs::read(repo.path().join(".git/HEAD")).unwrap();
    let original_helpers =
        std::fs::read(repo.path().join(".exomonad/helpers/SessionHelpers.hs")).unwrap();
    let runtime = tempfile::tempdir().unwrap();
    let (manager, _) = actor_worktree_resources_at(runtime.path(), repo.path()).unwrap();
    let layout = WorkspaceLayout {
        run_namespace: "authored-helper-test".into(),
        source_root: repo.path().into(),
        source_exclude: Vec::new(),
        root_imports: Arc::default(),
        worktrees: manager,
        base_prompt: FrozenBasePrompt::materialize(runtime.path()).unwrap(),
        backend: Arc::new(Backend::default()),
    };
    let root = layout
        .prepare(
            repo.path().into(),
            None,
            "root",
            true,
            CODING,
            None,
            None,
            None,
        )
        .unwrap();
    assert_eq!(
        shell(&root, "cat .exomonad/helpers/SessionHelpers.hs"),
        String::from_utf8(original_helpers.clone()).unwrap()
    );
    assert_eq!(
        shell(&root, "cat .exomonad/helpers/README.md"),
        "project helper documentation\n"
    );
    assert_eq!(shell(&root, "test -f .exomonad/helpers/SessionHelpers/Custom.hs; test ! -e .exomonad/helpers/SessionHelpers/BrowserChecks.hs; git status --porcelain"), "");
    assert_eq!(
        std::fs::read(repo.path().join(".git/HEAD")).unwrap(),
        original_git
    );
    assert_eq!(
        std::fs::read(repo.path().join(".exomonad/helpers/SessionHelpers.hs")).unwrap(),
        original_helpers
    );
}

fn repo_with_nested_workspace_submodule() -> (
    exomonad_worktree::testing::TestRepo,
    exomonad_worktree::testing::TestRepo,
    exomonad_worktree::testing::TestRepo,
) {
    let repo = exomonad_worktree::testing::TestRepo::init().unwrap();
    let workspace = exomonad_worktree::testing::TestRepo::init().unwrap();
    let nested = exomonad_worktree::testing::TestRepo::init().unwrap();
    nested
        .writer()
        .commit_file("Nested.hs", "module Nested where\n", "nested module")
        .unwrap();
    workspace
        .writer()
        .commit_file("Project.hs", "module Project where\n", "workspace module")
        .unwrap();
    workspace
        .git()
        .try_run(
            workspace.path(),
            &[
                "-c",
                "protocol.file.allow=always",
                "submodule",
                "add",
                nested.path().to_str().unwrap(),
                "nested",
            ],
        )
        .unwrap();
    workspace
        .git()
        .try_run(
            workspace.path(),
            &["commit", "-qm", "record nested submodule"],
        )
        .unwrap();
    repo.writer()
        .commit_file("README.md", "project\n", "project")
        .unwrap();
    repo.git()
        .try_run(
            repo.path(),
            &[
                "-c",
                "protocol.file.allow=always",
                "submodule",
                "add",
                workspace.path().to_str().unwrap(),
                ".exomonad/workspace",
            ],
        )
        .unwrap();
    repo.git()
        .try_run(
            repo.path(),
            &["commit", "-qm", "record workspace submodule"],
        )
        .unwrap();
    repo.git()
        .try_run(
            repo.path(),
            &[
                "-c",
                "protocol.file.allow=always",
                "submodule",
                "update",
                "--init",
                "--recursive",
            ],
        )
        .unwrap();
    (repo, workspace, nested)
}

#[test]
fn root_mount_resolves_relative_workspace_and_nested_gitfiles() {
    let (repo, _workspace, _nested) = repo_with_nested_workspace_submodule();
    let workspace_gitfile = repo.path().join(".exomonad/workspace/.git");
    let nested_gitfile = repo.path().join(".exomonad/workspace/nested/.git");
    let original_workspace = std::fs::read(&workspace_gitfile).unwrap();
    let original_nested = std::fs::read(&nested_gitfile).unwrap();
    assert!(original_workspace.starts_with(b"gitdir: ../"));
    assert!(original_nested.starts_with(b"gitdir: ../"));
    let workspace_config = exomonad_worktree::git::inspect::git_dir(
        repo.git(),
        &repo.path().join(".exomonad/workspace"),
    )
    .unwrap()
    .join("config");
    let nested_config = exomonad_worktree::git::inspect::git_dir(
        repo.git(),
        &repo.path().join(".exomonad/workspace/nested"),
    )
    .unwrap()
    .join("config");
    let original_workspace_config = std::fs::read(&workspace_config).unwrap();
    let original_nested_config = std::fs::read(&nested_config).unwrap();
    let runtime = tempfile::tempdir().unwrap();
    let (manager, _) = actor_worktree_resources_at(runtime.path(), repo.path()).unwrap();
    let layout = WorkspaceLayout {
        run_namespace: "nested-gitfile-test".into(),
        source_root: repo.path().into(),
        source_exclude: Vec::new(),
        root_imports: Arc::default(),
        worktrees: manager,
        base_prompt: FrozenBasePrompt::materialize(runtime.path()).unwrap(),
        backend: Arc::new(Backend::default()),
    };
    let root = layout
        .prepare(
            repo.path().into(),
            None,
            "root",
            true,
            CODING,
            None,
            None,
            None,
        )
        .unwrap();
    assert_eq!(shell(&root, "git -C .exomonad/workspace status --porcelain; git -C .exomonad/workspace/nested status --porcelain; git status --porcelain"), "");
    assert_eq!(
        shell(
            &root,
            "git -C .exomonad/workspace rev-parse --show-toplevel"
        ),
        format!("{ACTOR_PROJECT_ROOT}/.exomonad/workspace\n")
    );
    assert_eq!(
        shell(
            &root,
            "git -C .exomonad/workspace/nested rev-parse --show-toplevel"
        ),
        format!("{ACTOR_PROJECT_ROOT}/.exomonad/workspace/nested\n")
    );
    assert!(shell(&root, "cat .exomonad/workspace/.git").starts_with("gitdir: /"));
    assert!(shell(&root, "cat .exomonad/workspace/nested/.git").starts_with("gitdir: /"));
    assert_eq!(
        std::fs::read(&workspace_gitfile).unwrap(),
        original_workspace
    );
    assert_eq!(std::fs::read(&nested_gitfile).unwrap(), original_nested);
    assert_eq!(
        std::fs::read(&workspace_config).unwrap(),
        original_workspace_config
    );
    assert_eq!(
        std::fs::read(&nested_config).unwrap(),
        original_nested_config
    );
}

#[test]
fn root_mount_refuses_broken_workspace_gitfile_before_launch() {
    let (repo, _workspace, _nested) = repo_with_nested_workspace_submodule();
    let gitfile = repo.path().join(".exomonad/workspace/.git");
    std::fs::write(&gitfile, "gitdir: ../missing-admin\n").unwrap();
    let original = std::fs::read(&gitfile).unwrap();
    let runtime = tempfile::tempdir().unwrap();
    let (manager, _) = actor_worktree_resources_at(runtime.path(), repo.path()).unwrap();
    let layout = WorkspaceLayout {
        run_namespace: "broken-gitfile-test".into(),
        source_root: repo.path().into(),
        source_exclude: Vec::new(),
        root_imports: Arc::default(),
        worktrees: manager,
        base_prompt: FrozenBasePrompt::materialize(runtime.path()).unwrap(),
        backend: Arc::new(Backend::default()),
    };
    assert!(layout
        .prepare(
            repo.path().into(),
            None,
            "root",
            true,
            CODING,
            None,
            None,
            None
        )
        .is_err());
    assert_eq!(std::fs::read(&gitfile).unwrap(), original);
}

#[test]
fn actors_sharing_a_checkout_mount_distinct_helper_drafts() {
    let repo = exomonad_worktree::testing::TestRepo::init().unwrap();
    repo.writer().commit_file("file", "source", "seed").unwrap();
    let runtime = tempfile::tempdir().unwrap();
    let (manager, _) = actor_worktree_resources_at(runtime.path(), repo.path()).unwrap();
    let layout = WorkspaceLayout {
        run_namespace: "helper-mount-test".into(),
        source_root: repo.path().into(),
        source_exclude: Vec::new(),
        root_imports: Arc::default(),
        worktrees: manager,
        base_prompt: FrozenBasePrompt::materialize(runtime.path()).unwrap(),
        backend: Arc::new(Backend::default()),
    };
    let first = layout
        .prepare(
            repo.path().into(),
            None,
            "first-actor",
            true,
            CODING,
            None,
            None,
            Some("first-helpers".into()),
        )
        .unwrap();
    let second = layout
        .prepare(
            repo.path().into(),
            None,
            "second-actor",
            true,
            CODING,
            None,
            None,
            Some("second-helpers".into()),
        )
        .unwrap();
    shell(
        &first,
        "echo 'module SessionHelpers where' > .exomonad/helpers/SessionHelpers.hs",
    );
    assert_eq!(
        shell(&first, "cat .exomonad/helpers/SessionHelpers.hs"),
        "module SessionHelpers where\n"
    );
    assert_eq!(
        shell(&second, "test ! -e .exomonad/helpers/SessionHelpers.hs"),
        ""
    );
    assert_ne!(first.helper_draft, second.helper_draft);
}

#[tokio::test]
async fn live_capture_inherits_dirty_source_after_host_git_activity() {
    let repo = exomonad_worktree::testing::TestRepo::init().unwrap();
    repo.writer()
        .commit_file("file", "committed", "seed")
        .unwrap();
    repo.writer()
        .commit_file(".exomonad/config", "test", "workspace configuration")
        .unwrap();
    let runtime = tempfile::tempdir().unwrap();
    let (manager, bindings) = actor_worktree_resources_at(runtime.path(), repo.path()).unwrap();
    let bindings = Arc::new(Mutex::new(bindings));
    let authority = ActorWorktreeAuthority::new("workspace-test", bindings.clone());
    let root = ActorRef::first(exomonad_actor::ActorId(1));
    authority.install_grant(
        root.into(),
        tidepool_handlers::handlers::worktree::ActorWorktreeGrant::Repository,
    );
    let backend = Arc::new(Backend::default());
    let layout = WorkspaceLayout {
        run_namespace: "queued-capture-test".into(),
        source_root: repo.path().into(),
        source_exclude: Vec::new(),
        root_imports: Arc::default(),
        worktrees: manager.clone(),
        base_prompt: FrozenBasePrompt::materialize(runtime.path()).unwrap(),
        backend: backend.clone(),
    };
    let workspace = layout
        .prepare(
            repo.path().into(),
            None,
            "root",
            true,
            CODING,
            None,
            None,
            None,
        )
        .unwrap();
    let native = NativeProcess::start(&workspace, &backend);
    let binding = runtime.path().join("binding.json");
    exomonad_agent::accept_interactive_session_binding(
        &binding,
        exomonad_agent::HOST_DYNAMIC_TOOLS_PROTOCOL_VERSION,
        BackendThreadId(uuid::Uuid::new_v4().to_string()),
        None,
    )
    .await
    .unwrap();
    let thread = exomonad_agent::read_interactive_binding(&binding)
        .await
        .unwrap();
    let owners = Arc::new(Mutex::new(std::collections::HashMap::from([(
        root,
        owner(workspace.clone(), thread, &native),
    )])));
    let admission = fork_workspace_admission(
        manager,
        authority,
        bindings,
        "workspace-test".into(),
        Some(NativeForkAdmission {
            owners,
            backend: backend.clone(),
            layout: Some(layout),
        }),
    );
    let previous_head = shell(&workspace, "git rev-parse HEAD");
    std::fs::write(repo.path().join("file"), "live-dirty").unwrap();

    let (entered, ready) = tokio::sync::oneshot::channel();
    let (release_begin, resumed) = tokio::sync::oneshot::channel();
    *backend.begin_pause.lock() = Some((entered, resumed));
    let queued_admission = admission.clone();
    let queued = tokio::spawn(async move {
        queued_admission
            .admit(
                root,
                "root/git-queued".into(),
                ForkWorkspaceSeed::Explicit(tidepool_bridge_effects::WtWorktreeSpec {
                    spec_source: tidepool_bridge_effects::WtWorktreeSource::SourceCurrentRepository,
                    spec_label: "queued".into(),
                    spec_dirty_policy: tidepool_bridge_effects::WtDirtyPolicy::RequireClean,
                }),
                CODING,
            )
            .await
    });
    ready.await.unwrap();
    let git = admission.manager.git().clone();
    let (held_sender, held_receiver) = std::sync::mpsc::channel();
    let (release_git_sender, release_git_receiver) = std::sync::mpsc::channel();
    let holder = std::thread::spawn(move || {
        let _gate = git.try_capture().expect("test Git lane must be free");
        held_sender.send(()).unwrap();
        // best-effort: the sending side may already be gone once the main thread proceeds.
        release_git_receiver.recv().ok();
    });
    held_receiver.recv_timeout(Duration::from_secs(5)).unwrap();
    // The Git lane is held when the backend admits the fork. The mutex unit
    // test checks waiting itself; this test checks the resulting live source.
    release_begin.send(()).unwrap();
    release_git_sender.send(()).unwrap();
    holder.join().unwrap();
    let queued = tokio::time::timeout(Duration::from_secs(20), queued)
        .await
        .unwrap()
        .unwrap()
        .unwrap()
        .install(ActorRef::first(exomonad_actor::ActorId(2)))
        .unwrap();
    let queued = (queued.as_ref() as &dyn std::any::Any)
        .downcast_ref::<ActorWorkspaceCustody>()
        .unwrap();
    assert!(
        queued.inheritance_notice.is_none(),
        "queued capture unexpectedly fell back: {:?}",
        queued.inheritance_notice
    );
    assert_eq!(
        shell(queued.workspace.as_ref().unwrap(), "cat file"),
        "live-dirty"
    );
    assert_ne!(shell(&workspace, "git rev-parse HEAD"), previous_head);
    assert_eq!(
        shell(queued.workspace.as_ref().unwrap(), "git rev-parse HEAD"),
        shell(&workspace, "git rev-parse HEAD")
    );
}

#[tokio::test]
async fn ordinary_admission_captures_root_before_startup_and_busy_uses_head() {
    let repo = exomonad_worktree::testing::TestRepo::init().unwrap();
    repo.writer().commit_file("Cargo.toml", "[package]\nname = \"workspace-fork-fixture\"\nversion = \"0.1.0\"\nedition = \"2021\"\n", "crate").unwrap();
    repo.writer()
        .commit_file(
            "src/main.rs",
            "fn main() { println!(\"{}\", env!(\"VALUE\")); }",
            "source",
        )
        .unwrap();
    repo.writer().commit_file("build.rs", "fn main() { println!(\"cargo:rerun-if-changed=input.txt\"); println!(\"cargo:rustc-env=VALUE={}\", std::fs::read_to_string(\"input.txt\").unwrap()); }", "build script").unwrap();
    repo.writer()
        .commit_file("input.txt", "first", "input")
        .unwrap();
    repo.writer()
        .commit_file("file", "committed", "seed")
        .unwrap();
    repo.writer()
        .commit_file(".gitignore", "ignored\n", "ignore")
        .unwrap();
    repo.writer()
        .commit_file(".exomonad/config", "canonical", "workspace configuration")
        .unwrap();
    let workspace_repo = exomonad_worktree::testing::TestRepo::init().unwrap();
    workspace_repo
        .writer()
        .commit_file("module.txt", "base", "workspace base")
        .unwrap();
    repo.git()
        .try_run(
            repo.path(),
            &[
                "-c",
                "protocol.file.allow=always",
                "submodule",
                "add",
                "--name",
                "exomonad-workspace",
                workspace_repo.path().to_str().unwrap(),
                ".exomonad/workspace",
            ],
        )
        .unwrap();
    repo.git()
        .try_run(repo.path(), &["commit", "-qm", "record workspace"])
        .unwrap();
    let root_workspace = repo.path().join(".exomonad/workspace");
    repo.git()
        .try_run(&root_workspace, &["config", "user.name", "Workspace Test"])
        .unwrap();
    repo.git()
        .try_run(
            &root_workspace,
            &["config", "user.email", "workspace@example.invalid"],
        )
        .unwrap();
    std::fs::write(root_workspace.join("module.txt"), "unpushed-root").unwrap();
    repo.git()
        .try_run(
            &root_workspace,
            &["commit", "-qam", "local workspace revision"],
        )
        .unwrap();
    let root_workspace_head = repo
        .git()
        .try_run(&root_workspace, &["rev-parse", "HEAD"])
        .unwrap()
        .trimmed()
        .to_owned();
    for name in ["logs", "sessions", "runtime"] {
        std::fs::create_dir_all(repo.path().join(".exomonad").join(name)).unwrap();
        std::fs::write(
            repo.path().join(".exomonad").join(name).join("root-only"),
            "runtime",
        )
        .unwrap();
    }
    std::fs::write(repo.path().join("file"), "staged").unwrap();
    repo.git().try_run(repo.path(), &["add", "file"]).unwrap();
    std::fs::write(repo.path().join("file"), "working").unwrap();
    std::fs::write(repo.path().join("untracked"), "untracked").unwrap();
    std::fs::write(repo.path().join("ignored"), "ignored").unwrap();
    std::fs::create_dir(repo.path().join("target")).unwrap();
    std::fs::write(repo.path().join("target/source"), "ordinary source").unwrap();
    let source_modified = std::fs::metadata(repo.path().join("file"))
        .unwrap()
        .modified()
        .unwrap();
    let head = std::fs::read(repo.path().join(".git/HEAD")).unwrap();
    let runtime = tempfile::tempdir().unwrap();
    let (manager, bindings) = actor_worktree_resources_at(runtime.path(), repo.path()).unwrap();
    let bindings = Arc::new(Mutex::new(bindings));
    let authority = ActorWorktreeAuthority::new("workspace-test", bindings.clone());
    let root = ActorRef::first(exomonad_actor::ActorId(1));
    authority.install_grant(
        root.into(),
        tidepool_handlers::handlers::worktree::ActorWorktreeGrant::Repository,
    );
    let backend = Arc::new(Backend::default());
    let layout = WorkspaceLayout {
        run_namespace: "workspace-test".into(),
        source_root: repo.path().into(),
        source_exclude: Vec::new(),
        root_imports: Arc::default(),
        worktrees: manager.clone(),
        base_prompt: FrozenBasePrompt::materialize(runtime.path()).unwrap(),
        backend: backend.clone(),
    };
    let workspace = layout
        .prepare(
            repo.path().into(),
            None,
            "root",
            true,
            CODING,
            None,
            None,
            None,
        )
        .unwrap();
    assert!(build(&workspace).iter().any(|fresh| !fresh));
    let _native = NativeProcess::start(&workspace, &backend);

    let binding = runtime.path().join("binding.json");
    exomonad_agent::accept_interactive_session_binding(
        &binding,
        exomonad_agent::HOST_DYNAMIC_TOOLS_PROTOCOL_VERSION,
        BackendThreadId(uuid::Uuid::new_v4().to_string()),
        None,
    )
    .await
    .unwrap();
    let thread = exomonad_agent::read_interactive_binding(&binding)
        .await
        .unwrap();
    let owners = Arc::new(Mutex::new(std::collections::HashMap::from([(
        root,
        owner(workspace.clone(), thread.clone(), &_native),
    )])));
    let admission = fork_workspace_admission(
        manager,
        authority,
        bindings,
        "workspace-test".into(),
        Some(NativeForkAdmission {
            owners,
            backend: backend.clone(),
            layout: Some(layout),
        }),
    );
    let seed = || {
        ForkWorkspaceSeed::Explicit(tidepool_bridge_effects::WtWorktreeSpec {
            spec_source: tidepool_bridge_effects::WtWorktreeSource::SourceCurrentRepository,
            spec_label: "child".into(),
            spec_dirty_policy: tidepool_bridge_effects::WtDirtyPolicy::RequireClean,
        })
    };
    let prepared = admission
        .admit(root, "root/child".into(), seed(), CODING)
        .await
        .unwrap();
    let custody = prepared
        .install(ActorRef::first(exomonad_actor::ActorId(2)))
        .unwrap();
    let child = (custody.as_ref() as &dyn std::any::Any)
        .downcast_ref::<ActorWorkspaceCustody>()
        .unwrap();
    assert!(
        child.inheritance_notice.is_none(),
        "{:?}",
        child.inheritance_notice
    );
    let child = child.workspace.as_ref().unwrap();
    assert_eq!(
        shell(child, "git -C .exomonad/workspace rev-parse HEAD").trim(),
        root_workspace_head
    );
    shell(child, "git -C .exomonad/workspace status --porcelain; test ! -e .exomonad/logs/root-only; test ! -e .exomonad/sessions/root-only; test ! -e .exomonad/runtime/root-only");
    assert!(shell(child, "cat .exomonad/workspace/.git").starts_with("gitdir: /"));
    shell(child, "printf child-config > .exomonad/config");
    assert_eq!(
        std::fs::read_to_string(repo.path().join(".exomonad/config")).unwrap(),
        "canonical"
    );
    {
        let source = child.source.as_ref().unwrap().publication.lock().await;
        let source = source.as_ref().unwrap();
        assert!(source.unchanged_snapshot().unwrap().is_some());
    }
    assert!(child
        .build
        .as_ref()
        .unwrap()
        .publication
        .lock()
        .await
        .as_ref()
        .unwrap()
        .unchanged_snapshot()
        .unwrap()
        .is_some());
    assert_eq!(shell(child, "cat target/source"), "ordinary source");
    let layout = admission.native.as_ref().unwrap().layout.as_ref().unwrap();
    let allocated = |path: PathBuf| {
        let mut paths = vec![path];
        let mut inodes = std::collections::HashSet::new();
        let mut bytes = 0;
        while let Some(path) = paths.pop() {
            let metadata = std::fs::symlink_metadata(&path).unwrap();
            if inodes.insert((metadata.dev(), metadata.ino())) {
                bytes += metadata.blocks() * 512;
            }
            if metadata.is_dir() {
                match std::fs::read_dir(&path) {
                    Ok(entries) => paths.extend(entries.map(|entry| entry.unwrap().path())),
                    Err(error) if error.kind() == std::io::ErrorKind::PermissionDenied => {
                        assert_eq!(
                            metadata.mode() & 0o777,
                            0,
                            "only opaque kernel scratch may be excluded"
                        );
                    }
                    Err(error) => panic!("{}: {error}", path.display()),
                }
            }
        }
        bytes
    };
    let root_bytes = allocated(layout.resource_root("root").join("build"));
    let child_bytes = allocated(
        layout
            .resource_root(child.worktree.as_ref().unwrap().as_str())
            .join("build"),
    );
    assert!(
        child_bytes < root_bytes / 10,
        "new child should allocate private metadata, not copy the build tree"
    );
    eprintln!("build backing storage (excluding opaque kernel scratch): root={root_bytes} bytes; new child={child_bytes} bytes");
    assert!(
        build(child).iter().all(|fresh| *fresh),
        "unchanged source must reuse the root build"
    );
    shell(child, "printf '\n// changed locally\n' >> src/main.rs");
    assert!(
        build(child).iter().any(|fresh| !fresh),
        "changed Rust source must rebuild"
    );
    shell(child, "printf second > input.txt");
    assert!(
        build(child).iter().any(|fresh| !fresh),
        "changed build-script input must rebuild"
    );
    assert_eq!(
        shell(child, ".exomonad/build/cargo/debug/workspace-fork-fixture"),
        "second\n"
    );

    assert_eq!(
        shell(
            child,
            "cat file; git show :file; cat untracked ignored .exomonad/config"
        ),
        "workingworkinguntrackedignoredchild-config"
    );
    assert_eq!(
        shell(child, "stat -c '%y' file"),
        shell(&workspace, "stat -c '%y' file")
    );
    assert_eq!(
        std::fs::metadata(repo.path().join("file"))
            .unwrap()
            .modified()
            .unwrap(),
        source_modified
    );
    assert_eq!(shell(&workspace, "git status --porcelain"), "");
    assert_eq!(
        shell(child, "git rev-parse HEAD"),
        shell(&workspace, "git rev-parse HEAD")
    );
    assert_eq!(shell(child, "git show HEAD:file"), "working");
    assert_eq!(std::fs::read(repo.path().join(".git/HEAD")).unwrap(), head);
    std::fs::write(repo.path().join("file"), "later").unwrap();
    assert_eq!(shell(child, "cat file"), "working");
    shell(child, "printf child > file");
    assert_eq!(
        std::fs::read_to_string(repo.path().join("file")).unwrap(),
        "later"
    );
    repo.git()
        .try_run(repo.path(), &["rm", "--cached", "target/source"])
        .unwrap();
    repo.git()
        .try_run(
            repo.path(),
            &["commit", "-qm", "remove source from cache directory"],
        )
        .unwrap();
    std::fs::write(
        repo.path().join("target/CACHEDIR.TAG"),
        "Signature: 8a477f597d28d172789f06886806bc55\n",
    )
    .unwrap();
    let tagged = admission
        .admit(root, "root/tagged-cache".into(), seed(), CODING)
        .await
        .unwrap()
        .install(ActorRef::first(exomonad_actor::ActorId(30)))
        .unwrap();
    let tagged = (tagged.as_ref() as &dyn std::any::Any)
        .downcast_ref::<ActorWorkspaceCustody>()
        .unwrap();
    assert!(tagged.inheritance_notice.is_none());
    shell(tagged.workspace.as_ref().unwrap(), "test ! -e target");
    let calls_before_siblings = backend.calls.lock().len();
    let sibling_head = shell(&workspace, "git rev-parse HEAD");
    let (entered, ready) = tokio::sync::oneshot::channel();
    let (release, resumed) = tokio::sync::oneshot::channel();
    *backend.begin_pause.lock() = Some((entered, resumed));
    let first_admission = admission.clone();
    let first = tokio::spawn(async move {
        first_admission
            .admit(root, "root/queued-first".into(), seed(), CODING)
            .await
    });
    ready.await.unwrap();
    let cancelled_admission = admission.clone();
    let cancelled = tokio::spawn(async move {
        cancelled_admission
            .admit(root, "root/queued-cancelled".into(), seed(), CODING)
            .await
    });
    tokio::task::yield_now().await;
    cancelled.abort();
    assert!(matches!(cancelled.await, Err(error) if error.is_cancelled()));
    let next_admission = admission.clone();
    let next = tokio::spawn(async move {
        next_admission
            .admit(root, "root/queued-next".into(), seed(), CODING)
            .await
    });
    tokio::task::yield_now().await;
    assert_eq!(
        backend.calls.lock().len(),
        calls_before_siblings + 1,
        "a sibling must wait for publication, not begin another operation"
    );
    release.send(()).unwrap();
    let first = tokio::time::timeout(Duration::from_secs(30), first)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    let next = tokio::time::timeout(Duration::from_secs(30), next)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    for (actor, prepared) in [(31, first), (32, next)] {
        let custody = prepared
            .install(ActorRef::first(exomonad_actor::ActorId(actor)))
            .unwrap();
        let child = (custody.as_ref() as &dyn std::any::Any)
            .downcast_ref::<ActorWorkspaceCustody>()
            .unwrap();
        assert!(
            child.inheritance_notice.is_none(),
            "unexpected sibling fallback: {:?}",
            child.inheritance_notice
        );
        assert_eq!(
            shell(child.workspace.as_ref().unwrap(), "cat file untracked"),
            "lateruntracked",
            "siblings must inherit the same dirty source before either starts"
        );
        let workspace = child.workspace.as_ref().unwrap();
        assert_eq!(shell(workspace, "git rev-parse HEAD"), sibling_head);
        assert!(
            !layout
                .resource_root(workspace.worktree.as_ref().unwrap().as_str())
                .join("source/base")
                .exists(),
            "unchanged root siblings should reuse the imported base"
        );
    }
    assert_eq!(
        backend.calls.lock()[calls_before_siblings..]
            .iter()
            .filter(|(_, operation)| matches!(operation, PublicationOperation::Begin { .. }))
            .count(),
        2,
        "the cancelled waiter must not begin publication"
    );
    assert_eq!(shell(&workspace, "git rev-parse HEAD"), sibling_head);
    std::fs::write(repo.path().join("file"), "unpublished-busy").unwrap();
    *backend.busy.lock() = true;
    let fallback = admission
        .admit(root, "root/busy".into(), seed(), CODING)
        .await
        .unwrap()
        .install(ActorRef::first(exomonad_actor::ActorId(3)))
        .unwrap();
    let fallback = (fallback.as_ref() as &dyn std::any::Any)
        .downcast_ref::<ActorWorkspaceCustody>()
        .unwrap();
    assert!(fallback
        .inheritance_notice
        .as_ref()
        .unwrap()
        .contains("source is busy"));
    assert_eq!(
        shell(
            fallback.workspace.as_ref().unwrap(),
            "cat file untracked; test ! -e ignored"
        ),
        "lateruntracked"
    );
    assert_eq!(
        shell(
            fallback.workspace.as_ref().unwrap(),
            "stat -c '%y' src/main.rs"
        ),
        shell(&workspace, "stat -c '%y' src/main.rs"),
        "matching tracked source should retain the donor mtime"
    );
    assert_ne!(
        shell(fallback.workspace.as_ref().unwrap(), "stat -c '%y' file"),
        shell(&workspace, "stat -c '%y' file"),
        "different working bytes must keep the committed checkout's fresh mtime"
    );
    assert!(
        build(fallback.workspace.as_ref().unwrap())
            .iter()
            .all(|fresh| *fresh),
        "committed fallback should reuse its inherited build artifacts"
    );
    *backend.busy.lock() = false;

    // A ready native source with a busy host Git lane cannot skip its source
    // checkpoint and silently fork the previous HEAD.
    std::fs::write(repo.path().join("file"), "git-busy-dirty").unwrap();
    let before_git_busy = shell(&workspace, "git rev-parse HEAD");
    let (entered, ready) = tokio::sync::oneshot::channel();
    let (release_begin, resumed) = tokio::sync::oneshot::channel();
    *backend.begin_pause.lock() = Some((entered, resumed));
    let blocked_admission = admission.clone();
    let blocked = tokio::spawn(async move {
        blocked_admission
            .admit(root, "root/git-busy".into(), seed(), CODING)
            .await
    });
    ready.await.unwrap();
    let git = admission.manager.git().clone();
    let (held_sender, held_receiver) = std::sync::mpsc::channel();
    let (release_git_sender, release_git_receiver) = std::sync::mpsc::channel();
    let holder = std::thread::spawn(move || {
        let _gate = git.try_capture().expect("test Git lane must be free");
        held_sender.send(()).unwrap();
        release_git_receiver.recv().unwrap();
    });
    held_receiver.recv_timeout(Duration::from_secs(5)).unwrap();
    release_begin.send(()).unwrap();
    let failed = tokio::time::timeout(Duration::from_secs(40), blocked).await;
    release_git_sender.send(()).unwrap();
    holder.join().unwrap();
    let failed = failed.unwrap().unwrap();
    assert!(
        failed
            .as_ref()
            .is_err_and(|error| error.to_string().contains("not checkpointed")),
        "Git-busy live fork must fail visibly"
    );
    assert!(!workspace.publication.lock().await.is_pending());
    assert_eq!(shell(&workspace, "git rev-parse HEAD"), before_git_busy);
    assert_eq!(
        std::fs::read_to_string(repo.path().join("file")).unwrap(),
        "git-busy-dirty"
    );

    *backend.lose_finish.lock() = true;
    let mut publication = workspace.publication.lock().await;
    assert!(matches!(
        publication.begin(backend.as_ref(), &thread).await.unwrap(),
        Admission::Ready(_)
    ));
    assert!(publication.finish(backend.as_ref(), &thread).await.is_err());
    assert!(publication.is_pending());
    BoundWorkspace {
        workspace: Arc::new(ActiveWorkspace {
            view: workspace.view.clone(),
            prepared: workspace.clone(),
        }),
        thread,
    }
    .settle_publication(&mut publication, backend.as_ref())
    .await
    .unwrap();
    assert!(!publication.is_pending());
    drop(publication);
    {
        let calls = backend.calls.lock();
        let retries = &calls[calls.len() - 3..];
        assert!(retries
            .iter()
            .all(|(sequence, _)| *sequence == retries[0].0));
    }
    let _child_native = NativeProcess::start(child, &backend);
    let child_actor = ActorRef::first(exomonad_actor::ActorId(2));
    let child_binding = exomonad_agent::read_interactive_binding(&binding)
        .await
        .unwrap();
    admission.native.as_ref().unwrap().owners.lock().insert(
        child_actor,
        owner(child.clone(), child_binding, &_child_native),
    );
    shell(
        child,
        "printf dirty-module > .exomonad/workspace/module.txt",
    );
    let before_dirty_fork = shell(child, "git rev-parse HEAD");
    let dirty_fork = admission
        .admit(
            child_actor,
            "root/child/dirty-workspace".into(),
            ForkWorkspaceSeed::CurrentCheckout(
                tidepool_bridge_effects::WtDirtyPolicy::RequireClean,
            ),
            CODING,
        )
        .await;
    assert!(
        dirty_fork.is_err(),
        "uncommitted submodule edits must not be silently omitted"
    );
    assert_eq!(shell(child, "git rev-parse HEAD"), before_dirty_fork);
    assert!(!child.publication.lock().await.is_pending());
    shell(child, "printf staged-child > file; git add file; printf dirty-child > file; printf warm > .exomonad/build/cargo/artifact");
    shell(child, "git -C .exomonad/workspace config user.name 'Workspace Test'; git -C .exomonad/workspace config user.email workspace@example.invalid; printf child-module > .exomonad/workspace/module.txt; git -C .exomonad/workspace commit -qam child-module");
    let child_workspace_head = shell(child, "git -C .exomonad/workspace rev-parse HEAD");
    let grandchild = admission
        .admit(
            child_actor,
            "root/child/grandchild".into(),
            ForkWorkspaceSeed::CurrentCheckout(
                tidepool_bridge_effects::WtDirtyPolicy::RequireClean,
            ),
            CODING,
        )
        .await
        .unwrap()
        .install(ActorRef::first(exomonad_actor::ActorId(4)))
        .unwrap();
    let grandchild = (grandchild.as_ref() as &dyn std::any::Any)
        .downcast_ref::<ActorWorkspaceCustody>()
        .unwrap();
    assert!(
        grandchild.inheritance_notice.is_none(),
        "{:?}",
        grandchild.inheritance_notice
    );
    let grandchild = grandchild.workspace.as_ref().unwrap();
    assert_eq!(
        shell(
            grandchild,
            "cat file; git show :file; cat .exomonad/build/cargo/artifact .exomonad/config"
        ),
        "dirty-childdirty-childwarmchild-config"
    );
    assert_eq!(
        shell(grandchild, "git -C .exomonad/workspace rev-parse HEAD"),
        child_workspace_head
    );
    assert_eq!(
        shell(grandchild, "cat .exomonad/workspace/module.txt"),
        "child-module"
    );
    shell(grandchild, "printf private-grandchild > .exomonad/config");
    assert_eq!(shell(child, "cat .exomonad/config"), "child-config");
    shell(
        child,
        "printf later-child > file; printf later-build > .exomonad/build/cargo/artifact",
    );
    assert_eq!(
        shell(grandchild, "cat file .exomonad/build/cargo/artifact"),
        "dirty-childwarm"
    );
    shell(
        grandchild,
        "printf grandchild > file; git add file; git commit -qm grandchild",
    );
    assert_eq!(shell(child, "git show HEAD:file"), "dirty-child");

    let inspection = admission
        .admit(
            child_actor,
            "root/child/inspection".into(),
            ForkWorkspaceSeed::CurrentCheckout(
                tidepool_bridge_effects::WtDirtyPolicy::RequireClean,
            ),
            ForkWorkspacePolicy {
                native_tools: exomonad_actor::NativeToolClass::InspectionOnly,
                workspace: exomonad_actor::WorkspaceAccess::InspectOnly,
            },
        )
        .await
        .unwrap()
        .install(ActorRef::first(exomonad_actor::ActorId(5)))
        .unwrap();
    let inspection = (inspection.as_ref() as &dyn std::any::Any)
        .downcast_ref::<ActorWorkspaceCustody>()
        .unwrap();
    let inspection = inspection.workspace.as_ref().unwrap();
    assert!(inspection.build.is_none());
    assert_eq!(
        shell(
            inspection,
            "cat file; if touch forbidden 2>/dev/null; then exit 1; fi; if touch .exomonad/forbidden 2>/dev/null; then exit 1; fi; if touch .exomonad/workspace/forbidden 2>/dev/null; then exit 1; fi"
        ),
        "later-child"
    );
    shell(
        child,
        "git rev-parse HEAD > \"$(git rev-parse --git-path MERGE_HEAD)\"",
    );
    let failed = admission
        .admit(
            child_actor,
            "root/child/in-progress".into(),
            ForkWorkspaceSeed::CurrentCheckout(
                tidepool_bridge_effects::WtDirtyPolicy::RequireClean,
            ),
            CODING,
        )
        .await;
    assert!(failed.is_err(), "a failed checkpoint must stop this fork");
    assert!(!child.publication.lock().await.is_pending());
    assert_eq!(shell(child, "cat file"), "later-child");
    shell(child, "rm -- \"$(git rev-parse --git-path MERGE_HEAD)\"");
    *backend.lose_begin.lock() = true;
    let failed = admission
        .admit(
            child_actor,
            "root/child/lost-begin".into(),
            ForkWorkspaceSeed::CurrentCheckout(
                tidepool_bridge_effects::WtDirtyPolicy::RequireClean,
            ),
            CODING,
        )
        .await;
    assert!(failed.is_err());
    assert!(
        !child.publication.lock().await.is_pending(),
        "lost begin must settle the same operation"
    );
    assert_eq!(shell(child, "cat file"), "later-child");
    let foreign = admission
        .admit(
            root,
            "root/foreign-source".into(),
            ForkWorkspaceSeed::Explicit(tidepool_bridge_effects::WtWorktreeSpec {
                spec_source: tidepool_bridge_effects::WtWorktreeSource::SourceWorktree(
                    tidepool_bridge_effects::WtWorktreeId {
                        raw: child.worktree.as_ref().unwrap().as_str().into(),
                    },
                ),
                spec_label: "foreign-source".into(),
                spec_dirty_policy: tidepool_bridge_effects::WtDirtyPolicy::RequireClean,
            }),
            CODING,
        )
        .await
        .unwrap()
        .install(ActorRef::first(exomonad_actor::ActorId(7)))
        .unwrap();
    let foreign = (foreign.as_ref() as &dyn std::any::Any)
        .downcast_ref::<ActorWorkspaceCustody>()
        .unwrap();
    let foreign = foreign.workspace.as_ref().unwrap();
    assert_eq!(
        shell(
            foreign,
            "cat input.txt; .exomonad/build/cargo/debug/workspace-fork-fixture"
        ),
        "secondfirst\n",
        "source follows the selected child; cache follows the root creator"
    );
    let calls = backend.calls.lock().len();
    let explicit = admission
        .admit(
            root,
            "root/explicit-ref".into(),
            ForkWorkspaceSeed::Explicit(tidepool_bridge_effects::WtWorktreeSpec {
                spec_source: tidepool_bridge_effects::WtWorktreeSource::SourceRef(
                    tidepool_bridge_effects::WtGitRef { raw: "HEAD".into() },
                ),
                spec_label: "explicit-ref".into(),
                spec_dirty_policy: tidepool_bridge_effects::WtDirtyPolicy::RequireClean,
            }),
            CODING,
        )
        .await
        .unwrap()
        .install(ActorRef::first(exomonad_actor::ActorId(8)))
        .unwrap();
    assert_eq!(
        backend.calls.lock().len(),
        calls,
        "explicit refs require no live capture"
    );
    let explicit = (explicit.as_ref() as &dyn std::any::Any)
        .downcast_ref::<ActorWorkspaceCustody>()
        .unwrap();
    assert!(explicit.inheritance_notice.is_none());
    assert_eq!(
        shell(explicit.workspace.as_ref().unwrap(), "cat file untracked"),
        "lateruntracked"
    );
    *backend.unavailable.lock() = true;
    let unavailable = admission
        .admit(
            child_actor,
            "root/child/unavailable".into(),
            ForkWorkspaceSeed::CurrentCheckout(
                tidepool_bridge_effects::WtDirtyPolicy::RequireClean,
            ),
            CODING,
        )
        .await
        .unwrap()
        .install(ActorRef::first(exomonad_actor::ActorId(9)))
        .unwrap();
    let unavailable = (unavailable.as_ref() as &dyn std::any::Any)
        .downcast_ref::<ActorWorkspaceCustody>()
        .unwrap();
    assert!(unavailable
        .inheritance_notice
        .as_ref()
        .unwrap()
        .contains("test native unavailable"));
    assert_eq!(
        shell(unavailable.workspace.as_ref().unwrap(), "cat file"),
        "later-child"
    );
    *backend.unavailable.lock() = false;
    let (entered, ready) = tokio::sync::oneshot::channel();
    let (release, resumed) = tokio::sync::oneshot::channel();
    *backend.begin_pause.lock() = Some((entered, resumed));
    let caller_admission = admission.clone();
    let caller = tokio::spawn(async move {
        caller_admission
            .admit(
                child_actor,
                "root/child/cancelled".into(),
                ForkWorkspaceSeed::CurrentCheckout(
                    tidepool_bridge_effects::WtDirtyPolicy::RequireClean,
                ),
                CODING,
            )
            .await
    });
    ready.await.unwrap();
    caller.abort();
    assert!(matches!(caller.await, Err(error) if error.is_cancelled()));
    release.send(()).unwrap();
    let publication = tokio::time::timeout(Duration::from_secs(10), child.publication.lock())
        .await
        .unwrap();
    assert!(
        !publication.is_pending(),
        "abandoning the caller must not abandon native admission"
    );
    drop(publication);
    assert!(matches!(
        backend.calls.lock().last().unwrap().1,
        PublicationOperation::Finish { .. }
    ));
    let branch = tidepool_repr::ActorPath::parse("root/child/cancelled")
        .unwrap()
        .git_branch();
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            let complete = admission.manager.list().unwrap().iter().any(|summary| {
                summary.receipt.branch.as_str() == branch
                    && summary.receipt.status == exomonad_worktree::WorktreeRecordStatus::Mounted
            });
            if complete {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    assert_eq!(shell(child, "cat file"), "later-child");
    // Exercise retirement through the same composed admission fixture: dirty
    // source and the index survive, while descendants keep their warm layers.
    shell(child, "printf preserved > retirement-untracked; printf '*.ignored\\n' > .gitignore; printf ignored > retirement.ignored; rm -f input.txt; printf retained-config > .exomonad/config; printf retained-module > .exomonad/workspace/module.txt");
    let status = shell(child, "git status --porcelain=v1 -- . ':!.exomonad'");
    let index = shell(child, "git show :file");
    let child_head = shell(child, "git rev-parse HEAD");
    drop(_child_native);
    child.retire(&child.view).await.unwrap();
    child.retire(&child.view).await.unwrap();
    let receipt = admission
        .manager
        .registry()
        .get(child.worktree.as_ref().unwrap())
        .unwrap()
        .unwrap();
    assert_eq!(
        std::fs::read_to_string(receipt.cwd.join("file")).unwrap(),
        "later-child"
    );
    assert_eq!(
        std::fs::read_to_string(receipt.cwd.join("retirement-untracked")).unwrap(),
        "preserved"
    );
    assert_eq!(
        std::fs::read_to_string(receipt.cwd.join("retirement.ignored")).unwrap(),
        "ignored"
    );
    assert!(!receipt.cwd.join("input.txt").exists());
    assert_eq!(
        std::fs::read_to_string(receipt.cwd.join(".exomonad/config")).unwrap(),
        "retained-config"
    );
    assert_eq!(
        std::fs::read_to_string(receipt.cwd.join(".exomonad/workspace/module.txt")).unwrap(),
        "retained-module"
    );
    assert_eq!(
        admission
            .manager
            .git()
            .try_run(
                &receipt.cwd.join(".exomonad/workspace"),
                &["rev-parse", "HEAD"]
            )
            .unwrap()
            .trimmed(),
        child_workspace_head.trim()
    );
    for (arguments, expected) in [
        (
            vec!["status", "--porcelain=v1", "--", ".", ":!.exomonad"],
            status,
        ),
        (vec!["show", ":file"], index),
        (vec!["rev-parse", "HEAD"], child_head),
    ] {
        assert_eq!(
            admission
                .manager
                .git()
                .run(&receipt.cwd, &arguments)
                .unwrap()
                .trimmed(),
            expected.trim_end()
        );
    }
    assert_eq!(
        shell(grandchild, "cat .exomonad/build/cargo/artifact"),
        "warm"
    );
}
