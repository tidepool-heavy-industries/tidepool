#![cfg(target_os = "linux")]

use std::fs;
use std::os::unix::fs::MetadataExt;
use std::path::Path;

use exomonad_worktree::{
    BranchName, GitOid, WorktreeId, WorktreeOrigin, WorktreeReceipt, WorktreeRecordStatus,
    WorktreeRegistry,
};

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
fn many_tiny_retained_uppers_reference_one_large_base() {
    let storage = tempfile::tempdir().unwrap();
    let root = storage.path();
    let base = root.join("worktrees/.resources/run/root/source/base");
    fs::create_dir_all(&base).unwrap();
    fs::write(base.join("large"), vec![b'x'; 1024 * 1024]).unwrap();
    let registry = WorktreeRegistry::open(root.join("registry")).unwrap();
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
            branch: BranchName::from_raw(format!("exomonad/worktree/tiny-{number}")),
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
}
