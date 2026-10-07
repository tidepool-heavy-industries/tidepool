use super::*;
use exomonad_actor::{WorkspaceAdmission, WorkspaceSelection};
use exomonad_worktree::WorkspaceAccess;

fn fixture() -> (
    exomonad_worktree::testing::TestRepo,
    tempfile::TempDir,
    Arc<ActorWorkspaceAdmission>,
) {
    let repository = exomonad_worktree::testing::TestRepo::init().unwrap();
    repository
        .writer()
        .commit_file("shared.txt", "seed", "seed")
        .unwrap();
    let storage = tempfile::tempdir().unwrap();
    let anchor = tidepool_atomic_write::DirectoryAnchor::open_existing(storage.path()).unwrap();
    let manager = WorktreeManager::new(
        exomonad_worktree::GitCli::new(),
        exomonad_worktree::WorktreeRegistry::open(&anchor, "registry").unwrap(),
        storage.path().join("workspaces"),
        repository.path(),
    );
    let bindings = Arc::new(Mutex::new(
        BindingTable::open(&anchor, "memberships").unwrap(),
    ));
    let authority = ActorWorktreeAuthority::new("workspace-tests", bindings.clone());
    let admission =
        fork_workspace_admission(manager, authority, bindings, "workspace-tests".into());
    (repository, storage, admission)
}

#[tokio::test(flavor = "current_thread")]
async fn shared_directory_attachments_have_independent_custody_and_live_files() {
    let (repository, _storage, admission) = fixture();
    let parent = ActorRef::first(exomonad_actor::ActorId(1));
    admission
        .authority
        .install_grant(parent.into(), ActorWorktreeGrant::Repository);
    // Identity adoption accepts dirt without staging or resetting it.
    repository
        .writer()
        .write_file("shared.txt", "parent dirt")
        .unwrap();
    let one = admission
        .prepare(parent, WorkspaceSelection::SameDirectory, None)
        .await
        .unwrap();
    let two = admission
        .prepare(parent, WorkspaceSelection::SameDirectory, None)
        .await
        .unwrap();
    assert_eq!(
        one.handle().handle_receipt.tree_id,
        two.handle().handle_receipt.tree_id
    );
    let tree = WorktreeId::from_raw(one.handle().handle_receipt.tree_id.raw.clone());
    let first = ActorRef::first(exomonad_actor::ActorId(2));
    let second = ActorRef::first(exomonad_actor::ActorId(3));
    let first_custody = one.install(first).unwrap();
    let second_custody = two.install(second).unwrap();
    let first_roots = resident_command_roots(
        &admission.authority,
        &admission.manager,
        repository.path(),
        first,
    )
    .unwrap();
    let second_roots = resident_command_roots(
        &admission.authority,
        &admission.manager,
        repository.path(),
        second,
    )
    .unwrap();
    assert_eq!(first_roots.directory, second_roots.directory);
    assert_eq!(
        first_roots.directory,
        repository.path().canonicalize().unwrap()
    );
    assert!(first_roots.writable.contains(&first_roots.directory));
    std::fs::write(first_roots.directory.join("shared.txt"), "first writes").unwrap();
    assert_eq!(
        std::fs::read_to_string(second_roots.directory.join("shared.txt")).unwrap(),
        "first writes"
    );
    drop(first_custody);
    assert!(admission.authority.bound_worktree(first.into()).is_none());
    assert_eq!(
        admission.authority.bound_worktree(second.into()),
        Some(tree.clone())
    );
    assert_eq!(
        admission
            .bindings
            .lock()
            .participants(&tree)
            .unwrap()
            .count(),
        1
    );
    assert!(repository.path().join("shared.txt").exists());
    drop(second_custody);
    assert_eq!(
        admission
            .bindings
            .lock()
            .participants(&tree)
            .unwrap()
            .count(),
        0
    );
    assert!(repository.path().join("shared.txt").exists());
}

#[tokio::test(flavor = "current_thread")]
async fn opaque_workspace_attachment_inherits_access_and_outlives_issuer() {
    let (repository, _storage, admission) = fixture();
    let parent = ActorRef::first(exomonad_actor::ActorId(1));
    admission
        .authority
        .install_grant(parent.into(), ActorWorktreeGrant::Repository);
    let attachment = admission
        .prepare(
            parent,
            WorkspaceSelection::SameDirectory,
            Some(WorkspaceAccess::ReadOnly),
        )
        .await
        .unwrap();
    let tree = WorktreeId::from_raw(attachment.handle().handle_receipt.tree_id.raw.clone());
    let issuer = ActorRef::first(exomonad_actor::ActorId(2));
    let issuer_custody = attachment.install(issuer).unwrap();
    let token = admission
        .authority
        .issue_workspace(issuer.into(), &tree, WorkspaceAccess::ReadOnly)
        .unwrap();
    let selection =
        WorkspaceSelection::ExistingDirectory(tidepool_bridge_effects::WtWorkspaceHandle {
            raw: token.clone(),
        });
    assert!(admission
        .prepare(issuer, selection.clone(), Some(WorkspaceAccess::ReadWrite))
        .await
        .is_err());
    assert!(admission
        .prepare(
            issuer,
            WorkspaceSelection::ExistingDirectory(tidepool_bridge_effects::WtWorkspaceHandle {
                raw: tree.as_str().into()
            }),
            None
        )
        .await
        .is_err());
    let preparation = admission.prepare(issuer, selection, None).await.unwrap();
    drop(issuer_custody);
    let peer = ActorRef::first(exomonad_actor::ActorId(3));
    let custody = preparation.install(peer).unwrap();
    assert_eq!(
        admission.authority.workspace_access(peer.into(), &tree),
        Some(WorkspaceAccess::ReadOnly)
    );
    let roots = resident_command_roots(
        &admission.authority,
        &admission.manager,
        repository.path(),
        peer,
    )
    .unwrap();
    assert!(roots.writable.is_empty());
    assert_eq!(roots.directory, repository.path().canonicalize().unwrap());
    drop(custody);
    assert!(admission.manager.lookup(&tree).unwrap().is_some());
}
