#![cfg(target_os = "linux")]

use std::fs;
use std::os::unix::fs::MetadataExt;
use std::path::Path;
use tidepool_atomic_write::DirectoryAnchor;

use exomonad_worktree::{
    BranchName, GitCli, GitOid, WorktreeId, WorktreeManager, WorktreeOrigin, WorktreeReceipt,
    WorktreeRecordStatus, WorktreeRegistry,
};

#[test]
fn mounted_descriptor_requires_exact_stable_rotation_before_recovery() {
    let storage = tempfile::tempdir().unwrap();
    let storage_anchor = DirectoryAnchor::open_existing(storage.path()).unwrap();
    let root = storage.path();
    let base = root.join("worktrees/.resources/run/root/source/base");
    let old_upper = root.join("worktrees/.resources/run/child/source/upper");
    let next_upper = root.join("worktrees/.resources/run/child/source/next-upper");
    for path in [&base, &old_upper, &next_upper] {
        fs::create_dir_all(path).unwrap();
    }
    fs::write(old_upper.join("dirty"), "preserved").unwrap();
    let registry = WorktreeRegistry::open(&storage_anchor, "registry").unwrap();
    let id = WorktreeId::from_raw("wt-live");
    let cwd = root.join("worktrees/wt-live");
    fs::create_dir_all(&cwd).unwrap();
    let receipt = WorktreeReceipt {
        worktree_id: id.clone(),
        cwd,
        branch: Some(BranchName::from_raw("exomonad/worktree/live")),
        source_head: GitOid::from_raw("a".repeat(40)),
        snapshot_ref: None,
        origin: WorktreeOrigin::CurrentRepository,
        source_repository: root.to_owned(),
        created_at_ms: 1,
        status: WorktreeRecordStatus::Mounted,
    };
    registry.put(&receipt).unwrap();
    let manager = WorktreeManager::new(
        GitCli::new(),
        registry.clone(),
        root.join("worktrees"),
        root,
    );
    assert!(manager.seal_orphaned_mounted_view(&id).is_err());
    assert!(registry.source_layer_references().unwrap().is_none());
    registry
        .put_retained_manifest(&receipt, vec![base.clone(), old_upper.clone()])
        .unwrap();
    registry
        .put_mounted_transition(
            &receipt,
            vec![base.clone(), old_upper.clone()],
            vec![base.clone(), old_upper.clone(), next_upper.clone()],
        )
        .unwrap();
    assert!(manager.seal_orphaned_mounted_view(&id).is_err());
    assert_eq!(
        registry.source_layer_references().unwrap().unwrap().len(),
        3
    );
    // A restart sees the same ambiguous descriptor and cannot infer the
    // winner from directory presence.
    let reopened = WorktreeRegistry::open(&storage_anchor, "registry").unwrap();
    let manager = WorktreeManager::new(
        GitCli::new(),
        reopened.clone(),
        root.join("worktrees"),
        root,
    );
    assert!(manager.seal_orphaned_mounted_view(&id).is_err());
    reopened
        .put_mounted_layers(&receipt, vec![base.clone(), old_upper.clone(), next_upper])
        .unwrap();
    manager.seal_orphaned_mounted_view(&id).unwrap();
    manager.seal_orphaned_mounted_view(&id).unwrap();
    assert_eq!(
        reopened.get(&id).unwrap().unwrap().status,
        WorktreeRecordStatus::Retained
    );
    assert_eq!(
        fs::read_to_string(old_upper.join("dirty")).unwrap(),
        "preserved"
    );
}

fn allocated_bytes(root: &Path) -> u64 {
    let mut pending = vec![root.to_owned()];
    let mut bytes = 0;
    while let Some(path) = pending.pop() {
        let metadata = fs::symlink_metadata(&path).unwrap();
        bytes += metadata.blocks() * 512;
        if metadata.is_dir() {
            for entry in fs::read_dir(path).unwrap() {
                pending.push(entry.unwrap().path());
            }
        }
    }
    bytes
}

#[test]
fn interrupted_admission_keeps_provisional_descriptor_layers() {
    let storage = tempfile::tempdir().unwrap();
    let storage_anchor = DirectoryAnchor::open_existing(storage.path()).unwrap();
    let root = storage.path();
    let base = root.join("worktrees/.resources/run/root/source/base");
    let upper = root.join("worktrees/.resources/run/child/source/upper");
    fs::create_dir_all(&base).unwrap();
    fs::create_dir_all(&upper).unwrap();
    let registry = WorktreeRegistry::open(&storage_anchor, "registry").unwrap();
    let receipt = WorktreeReceipt {
        worktree_id: WorktreeId::from_raw("wt-admission"),
        cwd: root.join("worktrees/wt-admission"),
        branch: Some(BranchName::from_raw("exomonad/worktree/admission")),
        source_head: GitOid::from_raw("a".repeat(40)),
        snapshot_ref: None,
        origin: WorktreeOrigin::CurrentRepository,
        source_repository: root.to_owned(),
        created_at_ms: 1,
        status: WorktreeRecordStatus::Provisional,
    };
    fs::create_dir_all(&receipt.cwd).unwrap();
    registry.put(&receipt).unwrap();
    registry
        .put_retained_manifest(&receipt, vec![base.clone(), upper.clone()])
        .unwrap();
    let reopened = WorktreeRegistry::open(&storage_anchor, "registry").unwrap();
    assert_eq!(
        reopened.source_layer_references().unwrap().unwrap(),
        [base, upper].into_iter().collect()
    );
    assert_eq!(
        reopened.get(&receipt.worktree_id).unwrap().unwrap().status,
        WorktreeRecordStatus::Provisional
    );
}

#[test]
fn many_tiny_retained_uppers_reference_one_large_base() {
    let storage = tempfile::tempdir().unwrap();
    let storage_anchor = DirectoryAnchor::open_existing(storage.path()).unwrap();
    let root = storage.path();
    let base = root.join("worktrees/.resources/run/root/source/base");
    fs::create_dir_all(&base).unwrap();
    fs::write(base.join("large"), vec![b'x'; 1024 * 1024]).unwrap();
    let registry = WorktreeRegistry::open(&storage_anchor, "registry").unwrap();
    let mut receipts = Vec::new();
    for number in 0..16 {
        let id = WorktreeId::from_raw(format!("wt-tiny-{number}"));
        let upper = root.join(format!("worktrees/.resources/run/{id}/source/upper"));
        fs::create_dir_all(&upper).unwrap();
        fs::write(upper.join("tiny"), [number as u8]).unwrap();
        let cwd = root.join("worktrees").join(id.as_str());
        fs::create_dir_all(&cwd).unwrap();
        let receipt = WorktreeReceipt {
            worktree_id: id,
            cwd,
            branch: Some(BranchName::from_raw(format!("exomonad/worktree/tiny-{number}"))),
            source_head: GitOid::from_raw("a".repeat(40)),
            snapshot_ref: None,
            origin: WorktreeOrigin::CurrentRepository,
            source_repository: root.to_owned(),
            created_at_ms: number,
            status: WorktreeRecordStatus::Retained,
        };
        registry
            .put_retained_manifest(&receipt, vec![base.clone(), upper])
            .unwrap();
        registry.put(&receipt).unwrap();
        receipts.push(receipt);
    }
    assert_eq!(registry.list().unwrap().len(), 16);
    assert!(registry
        .list()
        .unwrap()
        .iter()
        .all(|summary| summary.present));
    let references = registry.source_layer_references().unwrap().unwrap();
    assert_eq!(references.len(), 17);
    assert!(allocated_bytes(root) < 4 * 1024 * 1024);

    // Simulate interruption after each Finalized receipt is durable but
    // before its obsolete manifest is unlinked. A restart must release the
    // references even if the layer files have since been reclaimed.
    for receipt in &mut receipts {
        receipt.status = WorktreeRecordStatus::Finalized;
        registry.put(receipt).unwrap();
    }
    fs::remove_dir_all(&base).unwrap();
    let reopened = WorktreeRegistry::open(&storage_anchor, "registry").unwrap();
    assert!(reopened
        .source_layer_references()
        .unwrap()
        .unwrap()
        .is_empty());
    reopened.release_finalized_manifest(&receipts[0]).unwrap();
    assert!(!root
        .join(format!(
            "registry/retained-views/{}.json",
            receipts[0].worktree_id
        ))
        .exists());
}
