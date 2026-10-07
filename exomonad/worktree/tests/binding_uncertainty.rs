#![cfg(target_os = "linux")]
use exomonad_worktree::{
    AgentRef, BindingTable, EventJournal, WorktreeError, WorktreeId, WorktreeRegistry,
    WorkspaceAccess,
};
use std::{fs, path::PathBuf, process::Command};
use tidepool_atomic_write::DirectoryAnchor;

#[test]
fn binding_fault_child() {
    let Ok(root) = std::env::var("BIND_ROOT") else {
        return;
    };
    let root = PathBuf::from(root);
    let storage = PathBuf::from(std::env::var("BIND_STORAGE_ROOT").unwrap());
    let anchor = DirectoryAnchor::open_existing(&storage).unwrap();
    let relative = root.strip_prefix(&storage).unwrap();
    let arm = PathBuf::from(std::env::var("BIND_FAULT_ARM").unwrap());
    let operation = std::env::var("BIND_OPERATION").unwrap();
    let mut table = BindingTable::open(&anchor, relative).unwrap();
    let id = WorktreeId::from_raw("wt-one");
    let other = WorktreeId::from_raw("wt-other");
    let agent = AgentRef::from_raw("agent-one");
    if operation == "reopen" {
        let lease = table.bind(&id, &agent, WorkspaceAccess::ReadWrite, 1).unwrap();
        drop(table);
        fs::write(&arm, "armed").unwrap();
        BindingTable::open(&anchor, relative)
            .expect_err("loaded row must sync before authorizing custody");
        fs::remove_file(&arm).unwrap();
        let mut reopened = BindingTable::open(&anchor, relative).unwrap();
        assert!(reopened.membership(&id, &agent).is_none());
        assert_eq!(reopened.participants(&id).unwrap().count(), 1);
        lease
            .release(&mut reopened)
            .expect_err("pre-reopen generation cannot settle loaded row");
        assert!(matches!(
            reopened.bind(&id, &agent, WorkspaceAccess::ReadWrite, 2),
            Err(WorktreeError::WorktreeAuthorityDenied(_))
        ));
        return;
    }
    let other_agent = AgentRef::from_raw("agent-other");
    let prior = table.bind(&other, &other_agent, WorkspaceAccess::ReadWrite, 1).unwrap();
    let mut transferred = None;
    if operation == "bind" {
        fs::write(&arm, "armed").unwrap();
        table
            .bind(&id, &agent, WorkspaceAccess::ReadWrite, 2)
            .expect_err("uncertain bind returns no lease");
    } else if operation == "transfer" {
        let mut lease = table.bind(&id, &agent, WorkspaceAccess::ReadWrite, 2).unwrap();
        fs::write(&arm, "armed").unwrap();
        table
            .transfer(&mut lease, &AgentRef::from_raw("agent-two"), 3)
            .expect_err("uncertain transfer retains custody without granting authority");
        transferred = Some(lease);
    } else {
        let lease = table.bind(&id, &agent, WorkspaceAccess::ReadWrite, 2).unwrap();
        fs::write(&arm, "armed").unwrap();
        lease
            .release(&mut table)
            .expect_err("uncertain release consumes lease");
    }
    fs::remove_file(&arm).unwrap();
    let path = root.join("wt-one.json");
    let before = fs::read(&path).unwrap();
    let rows: serde_json::Value = serde_json::from_slice(&before).unwrap();
    assert_eq!(
        rows[0]["state"],
        if operation == "bind" {
            "Active"
        } else {
            "Released"
        }
    );
    assert!(
        table.membership(&id, &agent).is_none(),
        "uncertainty cannot grant custody through current"
    );
    assert!(table.membership(&other, &other_agent).is_none(), "whole table is fenced");
    assert!(table.participants(&id).is_err(), "uncertainty refuses diagnostics too");
    assert!(table.active_for_agent(&agent).is_none());
    if let Some(lease) = &mut transferred {
        assert_eq!(rows[1]["state"], "Active");
        assert_eq!(rows[1]["agent"], "agent-two");
        assert_eq!(rows[1]["predecessor"], "agent-one");
        assert!(table
            .active_for_agent(&AgentRef::from_raw("agent-two"))
            .is_none());
        table
            .transfer(lease, &agent, 4)
            .expect_err("uncertain transfer cannot be retried");
    }
    assert!(matches!(
        table.bind(&id, &agent, WorkspaceAccess::ReadWrite, 3),
        Err(WorktreeError::StorageFailure { .. })
    ));
    assert!(matches!(
        prior.complete(&mut table),
        Err(WorktreeError::StorageFailure { .. })
    ));
    assert_eq!(fs::read(&path).unwrap(), before);
    assert!(
        BindingTable::open(&anchor, relative).is_err(),
        "poison retains exclusive lock"
    );
    drop(table);
    let mut table = BindingTable::open(&anchor, relative).unwrap();
    if operation == "bind" || operation == "transfer" {
        assert!(table.membership(&id, &agent).is_none());
        assert_eq!(table.participants(&id).unwrap().count(), 1);
        let holder = if operation == "transfer" {
            AgentRef::from_raw("agent-two")
        } else {
            agent.clone()
        };
        assert!(matches!(
            table.bind(&id, &holder, WorkspaceAccess::ReadWrite, 4),
            Err(WorktreeError::WorktreeAuthorityDenied(_))
        ));
        let wrong_predecessor = AgentRef::from_raw("unrelated-peer");
        assert!(table
            .recover_active(&id, &wrong_predecessor, &holder, 4)
            .is_err());
        table
            .recover_active(&id, &agent, &holder, 4)
            .unwrap()
            .release(&mut table)
            .unwrap();
    } else {
        assert!(table.membership(&id, &agent).is_none());
        let lease = table.bind(&id, &agent, WorkspaceAccess::ReadWrite, 4).unwrap();
        lease.complete(&mut table).unwrap();
        let lease = table.bind(&id, &agent, WorkspaceAccess::ReadWrite, 5).unwrap();
        lease.release(&mut table).unwrap();
    }
    if let Some(lease) = transferred {
        assert_eq!(table.participants(&id).unwrap().count(), 0);
        lease
            .release(&mut table)
            .expect_err("reopen cannot revive a transfer receipt");
    }
    // Another previously active row is retained; no lease is manufactured on reopen.
    assert!(matches!(
        table.bind(&other, &other_agent, WorkspaceAccess::ReadWrite, 6),
        Err(WorktreeError::WorktreeAuthorityDenied(_))
    ));
}

#[test]
fn binding_public_paths_fence_uncertain_custody_until_reopen() {
    let temp = tempfile::tempdir().unwrap();
    let library = std::env::var_os("EXOMONAD_BINDING_DIRECTORY_FAULT_LIBRARY")
        .map(PathBuf::from)
        .expect("native test runner must supply the declared binding fault library");
    for operation in ["bind", "settle", "transfer", "reopen"] {
        for kind in ["open", "sync"] {
            let root = temp.path().join(format!("{operation}-{kind}/new/deep"));
            let log = temp.path().join(format!("{operation}-{kind}.hits"));
            let arm = temp.path().join(format!("{operation}-{kind}.arm"));
            let target = if operation == "reopen" {
                root.join("wt-one.json")
            } else {
                root.clone()
            };
            #[allow(
                clippy::disallowed_methods,
                reason = "short synchronous test-fixture spawn"
            )]
            let output = Command::new(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    "binding_uncertainty::binding_fault_child",
                    "--nocapture",
                ])
                .env("LD_PRELOAD", &library)
                .env("BIND_ROOT", &root)
                .env("BIND_STORAGE_ROOT", temp.path())
                .env("BIND_FAULT_PATH", &target)
                .env("BIND_FAULT_LOG", &log)
                .env("BIND_FAULT_ARM", &arm)
                .env("BIND_OPERATION", operation)
                .env("BIND_FAULT_KIND", kind)
                .output()
                .unwrap();
            assert!(
                output.status.success(),
                "{operation}/{kind}: {} {}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
            assert_eq!(
                fs::read_to_string(&log).unwrap(),
                "hit\n",
                "exactly one injected directory failure"
            );
        }
    }
}

#[test]
fn directory_admission_fault_child() {
    let Ok(storage) = std::env::var("BIND_STORAGE_ROOT") else {
        return;
    };
    let storage = PathBuf::from(storage);
    let anchor = DirectoryAnchor::open_existing(&storage).unwrap();
    let relative = "new/deep";
    let root = storage.join(relative);
    let arm = PathBuf::from(std::env::var("BIND_FAULT_ARM").unwrap());
    let fault = PathBuf::from(std::env::var("BIND_FAULT_PATH").unwrap());
    let store = std::env::var("BIND_STORE").unwrap();
    fs::write(&arm, "armed").unwrap();
    for _ in 0..2 {
        let error = match store.as_str() {
            "registry" => WorktreeRegistry::open(&anchor, relative).unwrap_err(),
            "binding" => BindingTable::open(&anchor, relative).unwrap_err(),
            "journal" => EventJournal::open(&anchor, "new/deep/events.jsonl").unwrap_err(),
            _ => panic!("unknown store fixture"),
        };
        assert!(matches!(error, WorktreeError::StorageFailure { path, .. } if path == fault));
        assert!(
            root.is_dir(),
            "failed confirmation leaves a visible directory"
        );
        assert!(
            !root.join("records").exists(),
            "registry cannot derive child stores"
        );
        assert!(
            !root.join(".owner.lock").exists(),
            "binding admission cannot acquire its lock"
        );
        assert!(
            !root.join("events.owner.lock").exists(),
            "journal admission cannot acquire its lock"
        );
        assert!(
            !root.join("events.jsonl").exists(),
            "journal initialization cannot publish"
        );
    }
    fs::remove_file(&arm).unwrap();
    match store.as_str() {
        "registry" => {
            let registry = WorktreeRegistry::open(&anchor, relative).unwrap();
            assert_eq!(registry.root(), root);
            assert!(root.join("records").is_dir());
            assert!(root.join("retained-views").is_dir());
        }
        "binding" => {
            let mut table = BindingTable::open(&anchor, relative).unwrap();
            let id = WorktreeId::from_raw("wt-admitted");
            let lease = table.bind(&id, &AgentRef::from_raw("agent"), WorkspaceAccess::ReadWrite, 1).unwrap();
            lease.complete(&mut table).unwrap();
        }
        "journal" => {
            let mut journal = EventJournal::open(&anchor, "new/deep/events.jsonl").unwrap();
            assert_eq!(
                journal.append(&[], exomonad_worktree::EventId(1)).unwrap(),
                1
            );
        }
        _ => panic!("unknown store fixture"),
    }
}

#[test]
fn store_admission_retries_the_original_scope_before_granting_authority() {
    let temp = tempfile::tempdir().unwrap();
    let storage_anchor = DirectoryAnchor::open_existing(temp.path()).unwrap();
    let library = std::env::var_os("EXOMONAD_BINDING_DIRECTORY_FAULT_LIBRARY")
        .map(PathBuf::from)
        .expect("native test runner must supply the declared binding fault library");
    for store in ["registry", "binding", "journal"] {
        for kind in ["open", "sync"] {
            let storage = temp.path().join(format!("{store}-{kind}"));
            storage_anchor.child(format!("{store}-{kind}")).unwrap();
            let log = storage.join("hits");
            #[allow(
                clippy::disallowed_methods,
                reason = "short synchronous test-fixture spawn"
            )]
            let output = Command::new(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    "binding_uncertainty::directory_admission_fault_child",
                    "--nocapture",
                ])
                .env("LD_PRELOAD", &library)
                .env("BIND_STORAGE_ROOT", &storage)
                .env("BIND_FAULT_PATH", storage.join("new"))
                .env("BIND_FAULT_LOG", &log)
                .env("BIND_FAULT_ARM", storage.join("armed"))
                .env("BIND_FAULT_KIND", kind)
                .env("BIND_STORE", store)
                .output()
                .unwrap();
            assert!(
                output.status.success(),
                "{store}/{kind}: {} {}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
            assert_eq!(
                fs::read_to_string(log).unwrap(),
                "hit\nhit\n",
                "visible ancestry must be reconfirmed on each retry"
            );
        }
    }
}
