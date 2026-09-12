use super::*;
use std::io::{BufRead, BufReader};
use std::os::unix::fs::MetadataExt;
use std::process::{Child, Stdio};
use tidepool_actor::{ForkWorkspaceAdmission, ForkWorkspacePolicy};
use tidepool_agent::interactive::*;
use tidepool_agent::{AgentBackendError, BackendThreadId};

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
        let _ = self.0.wait();
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
    let output = shell(workspace, "CARGO_HOME=\"$PWD/.shoal/build/cargo/home\" CARGO_TARGET_DIR=\"$PWD/.shoal/build/cargo\" RUSTC_WRAPPER= cargo build --offline --message-format=json");
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
                view: tidepool_node::MountNamespace::capture(native.0.id()).unwrap(),
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
    native_tools: tidepool_actor::NativeToolClass::Coding,
    workspace: tidepool_actor::WorkspaceAccess::WritableBound,
};

#[test]
fn source_exclusions_keep_tracked_files_and_untagged_directories() {
    let repo = tidepool_worktree::testing::TestRepo::init().unwrap();
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
    let excluded = layout.source_exclusions(repo.path()).unwrap();
    assert!(excluded.contains(&"node_modules".into()));
    assert!(!excluded.contains(&"tracked".into()));
    assert!(!excluded.contains(&"ordinary".into()));
    layout.source_exclude.push("ordinary".into());
    assert!(layout
        .source_exclusions(repo.path())
        .unwrap()
        .contains(&"ordinary".into()));
    layout.source_exclude.push("tracked".into());
    assert!(layout.source_exclusions(repo.path()).is_err());
    repo.git()
        .try_run(repo.path(), &["rm", "--cached", "--", "tracked/source"])
        .unwrap();
    assert!(
        layout.source_exclusions(repo.path()).is_err(),
        "HEAD must still protect a staged deletion"
    );
    let mut config = crate::shoal::LaunchConfig::default();
    for invalid in ["../outside", "", ".git", "build*"] {
        config.source_exclude = vec![invalid.into()];
        assert!(config.validate().is_err(), "{invalid:?}");
    }
}

#[test]
fn root_import_reuse_requires_matching_content_and_exclusions() {
    let repo = tidepool_worktree::testing::TestRepo::init().unwrap();
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
    let excluded = layout.source_exclusions(repo.path()).unwrap();
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
    let changed_exclusions = layout.source_exclusions(repo.path()).unwrap();
    assert!(layout
        .reusable_import(repo.path(), &changed_exclusions)
        .is_none());
}

#[test]
fn root_workspace_resources_are_isolated_between_runs() {
    let repo = tidepool_worktree::testing::TestRepo::init().unwrap();
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
        )
        .unwrap();
    shell(&first, "echo first > .shoal/build/cargo/marker");
    let collision = layout
        .prepare(
            repo.path().into(),
            None,
            "actor-0-1",
            true,
            CODING,
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
        )
        .unwrap();
    shell(
        &second,
        "test ! -e .shoal/build/cargo/marker; echo second > .shoal/build/cargo/marker",
    );
    assert_eq!(shell(&first, "cat .shoal/build/cargo/marker"), "first\n");
    assert_eq!(shell(&second, "cat .shoal/build/cargo/marker"), "second\n");
    assert_eq!(
        std::fs::read_to_string(legacy.join("retained")).unwrap(),
        "old run"
    );
}

#[tokio::test]
async fn ordinary_admission_captures_root_before_startup_and_busy_uses_head() {
    let repo = tidepool_worktree::testing::TestRepo::init().unwrap();
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
        .commit_file(".gitignore", "ignored\n.shoal/\n", "ignore")
        .unwrap();
    std::fs::create_dir(repo.path().join(".shoal")).unwrap();
    std::fs::write(repo.path().join(".shoal/config"), "canonical").unwrap();
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
    let index = std::fs::read(repo.path().join(".git/index")).unwrap();
    let head = std::fs::read(repo.path().join(".git/HEAD")).unwrap();
    let runtime = tempfile::tempdir().unwrap();
    let (manager, bindings) = actor_worktree_resources_at(runtime.path(), repo.path()).unwrap();
    let bindings = Arc::new(Mutex::new(bindings));
    let authority = ActorWorktreeAuthority::new("workspace-test", bindings.clone());
    let root = ActorRef::first(tidepool_actor::ActorId(1));
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
        .prepare(repo.path().into(), None, "root", true, CODING, None, None)
        .unwrap();
    assert!(build(&workspace).iter().any(|fresh| !fresh));
    let _native = NativeProcess::start(&workspace, &backend);

    let binding = runtime.path().join("binding.json");
    tidepool_agent::accept_interactive_session_binding(
        &binding,
        tidepool_agent::HOST_DYNAMIC_TOOLS_PROTOCOL_VERSION,
        BackendThreadId(uuid::Uuid::new_v4().to_string()),
        None,
    )
    .await
    .unwrap();
    let thread = tidepool_agent::read_interactive_binding(&binding)
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
        .install(ActorRef::first(tidepool_actor::ActorId(2)))
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
        shell(child, ".shoal/build/cargo/debug/workspace-fork-fixture"),
        "second\n"
    );

    assert_eq!(
        shell(
            child,
            "cat file; git show :file; cat untracked ignored .shoal/config"
        ),
        "workingstageduntrackedignoredcanonical"
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
    assert_eq!(
        std::fs::read(repo.path().join(".git/index")).unwrap(),
        index
    );
    assert_eq!(std::fs::read(repo.path().join(".git/HEAD")).unwrap(), head);
    std::fs::write(repo.path().join("file"), "later").unwrap();
    assert_eq!(shell(child, "cat file"), "working");
    shell(child, "printf child > file");
    assert_eq!(
        std::fs::read_to_string(repo.path().join("file")).unwrap(),
        "later"
    );
    std::fs::write(
        repo.path().join("target/CACHEDIR.TAG"),
        "Signature: 8a477f597d28d172789f06886806bc55\n",
    )
    .unwrap();
    let tagged = admission
        .admit(root, "root/tagged-cache".into(), seed(), CODING)
        .await
        .unwrap()
        .install(ActorRef::first(tidepool_actor::ActorId(30)))
        .unwrap();
    let tagged = (tagged.as_ref() as &dyn std::any::Any)
        .downcast_ref::<ActorWorkspaceCustody>()
        .unwrap();
    assert!(tagged.inheritance_notice.is_none());
    shell(tagged.workspace.as_ref().unwrap(), "test ! -e target");
    let calls_before_siblings = backend.calls.lock().len();
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
            .install(ActorRef::first(tidepool_actor::ActorId(actor)))
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
    *backend.busy.lock() = true;
    let fallback = admission
        .admit(root, "root/busy".into(), seed(), CODING)
        .await
        .unwrap()
        .install(ActorRef::first(tidepool_actor::ActorId(3)))
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
            "cat file; test ! -e untracked; test ! -e ignored"
        ),
        "committed"
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
    let calls = backend.calls.lock();
    let retries = &calls[calls.len() - 3..];
    assert!(retries
        .iter()
        .all(|(sequence, _)| *sequence == retries[0].0));
    drop(calls);
    let _child_native = NativeProcess::start(child, &backend);
    let child_actor = ActorRef::first(tidepool_actor::ActorId(2));
    admission.native.as_ref().unwrap().owners.lock().insert(
        child_actor,
        owner(
            child.clone(),
            tidepool_agent::read_interactive_binding(&binding)
                .await
                .unwrap(),
            &_child_native,
        ),
    );
    shell(child, "printf staged-child > file; git add file; printf dirty-child > file; printf warm > .shoal/build/cargo/artifact");
    let grandchild = admission
        .admit(
            child_actor,
            "root/child/grandchild".into(),
            ForkWorkspaceSeed::BoundHead(tidepool_bridge_effects::WtDirtyPolicy::RequireClean),
            CODING,
        )
        .await
        .unwrap()
        .install(ActorRef::first(tidepool_actor::ActorId(4)))
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
            "cat file; git show :file; cat .shoal/build/cargo/artifact .shoal/config"
        ),
        "dirty-childstaged-childwarmcanonical"
    );
    shell(
        child,
        "printf later-child > file; printf later-build > .shoal/build/cargo/artifact",
    );
    assert_eq!(
        shell(grandchild, "cat file .shoal/build/cargo/artifact"),
        "dirty-childwarm"
    );
    shell(grandchild, "git commit -qm grandchild");
    assert_eq!(shell(child, "git show HEAD:file"), "committed");

    let inspection = admission
        .admit(
            child_actor,
            "root/child/inspection".into(),
            ForkWorkspaceSeed::BoundHead(tidepool_bridge_effects::WtDirtyPolicy::RequireClean),
            ForkWorkspacePolicy {
                native_tools: tidepool_actor::NativeToolClass::InspectionOnly,
                workspace: tidepool_actor::WorkspaceAccess::InspectOnly,
            },
        )
        .await
        .unwrap()
        .install(ActorRef::first(tidepool_actor::ActorId(5)))
        .unwrap();
    let inspection = (inspection.as_ref() as &dyn std::any::Any)
        .downcast_ref::<ActorWorkspaceCustody>()
        .unwrap();
    let inspection = inspection.workspace.as_ref().unwrap();
    assert!(inspection.build.is_none());
    assert_eq!(
        shell(
            inspection,
            "cat file; if touch forbidden 2>/dev/null; then exit 1; fi"
        ),
        "later-child"
    );
    shell(
        child,
        "git rev-parse HEAD > \"$(git rev-parse --git-path MERGE_HEAD)\"",
    );
    let fallback = admission
        .admit(
            child_actor,
            "root/child/in-progress".into(),
            ForkWorkspaceSeed::BoundHead(tidepool_bridge_effects::WtDirtyPolicy::RequireClean),
            CODING,
        )
        .await
        .unwrap()
        .install(ActorRef::first(tidepool_actor::ActorId(6)))
        .unwrap();
    let fallback = (fallback.as_ref() as &dyn std::any::Any)
        .downcast_ref::<ActorWorkspaceCustody>()
        .unwrap();
    assert!(fallback
        .inheritance_notice
        .as_ref()
        .unwrap()
        .contains("Git operation in progress"));
    assert_eq!(
        shell(fallback.workspace.as_ref().unwrap(), "cat file"),
        "committed"
    );
    shell(child, "rm -- \"$(git rev-parse --git-path MERGE_HEAD)\"");
    *backend.lose_begin.lock() = true;
    let failed = admission
        .admit(
            child_actor,
            "root/child/lost-begin".into(),
            ForkWorkspaceSeed::BoundHead(tidepool_bridge_effects::WtDirtyPolicy::RequireClean),
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
        .install(ActorRef::first(tidepool_actor::ActorId(7)))
        .unwrap();
    let foreign = (foreign.as_ref() as &dyn std::any::Any)
        .downcast_ref::<ActorWorkspaceCustody>()
        .unwrap();
    let foreign = foreign.workspace.as_ref().unwrap();
    assert_eq!(
        shell(
            foreign,
            "cat input.txt; .shoal/build/cargo/debug/workspace-fork-fixture"
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
        .install(ActorRef::first(tidepool_actor::ActorId(8)))
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
        shell(
            explicit.workspace.as_ref().unwrap(),
            "cat file; test ! -e untracked"
        ),
        "committed"
    );
    *backend.unavailable.lock() = true;
    let unavailable = admission
        .admit(
            child_actor,
            "root/child/unavailable".into(),
            ForkWorkspaceSeed::BoundHead(tidepool_bridge_effects::WtDirtyPolicy::RequireClean),
            CODING,
        )
        .await
        .unwrap()
        .install(ActorRef::first(tidepool_actor::ActorId(9)))
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
        "committed"
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
                ForkWorkspaceSeed::BoundHead(tidepool_bridge_effects::WtDirtyPolicy::RequireClean),
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
                    && summary.receipt.status == tidepool_worktree::WorktreeRecordStatus::Mounted
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
    shell(child, "printf preserved > retirement-untracked; printf '*.ignored\\n' > .gitignore; printf ignored > retirement.ignored; rm -f input.txt");
    let status = shell(child, "git status --porcelain=v1 -- . ':!.shoal'");
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
    for (arguments, expected) in [
        (
            vec!["status", "--porcelain=v1", "--", ".", ":!.shoal"],
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
    assert_eq!(shell(grandchild, "cat .shoal/build/cargo/artifact"), "warm");
}
