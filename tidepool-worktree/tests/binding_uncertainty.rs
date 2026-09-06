#![cfg(target_os = "linux")]
use std::{fs, path::PathBuf, process::Command};
use tidepool_worktree::{AgentRef, BindingTable, WorktreeError, WorktreeId};

#[test]
fn binding_fault_child() {
    let Ok(root) = std::env::var("BIND_ROOT") else {
        return;
    };
    let root = PathBuf::from(root);
    let arm = PathBuf::from(std::env::var("BIND_FAULT_ARM").unwrap());
    let operation = std::env::var("BIND_OPERATION").unwrap();
    let mut table = BindingTable::open(&root).unwrap();
    let id = WorktreeId::from_raw("wt-one");
    let other = WorktreeId::from_raw("wt-other");
    let agent = AgentRef::from_raw("agent-one");
    if operation == "reopen" {
        let lease = table.bind(&id, &agent, 1).unwrap();
        drop(table);
        fs::write(&arm, "armed").unwrap();
        BindingTable::open(&root).expect_err("loaded row must sync before authorizing custody");
        fs::remove_file(&arm).unwrap();
        let mut reopened = BindingTable::open(&root).unwrap();
        assert!(reopened.current(&id).is_some());
        lease
            .release(&mut reopened)
            .expect_err("pre-reopen generation cannot settle loaded row");
        assert!(matches!(
            reopened.bind(&id, &agent, 2),
            Err(WorktreeError::WorktreeBusy { .. })
        ));
        return;
    }
    let prior = table.bind(&other, &agent, 1).unwrap();
    if operation == "bind" {
        fs::write(&arm, "armed").unwrap();
        table
            .bind(&id, &agent, 2)
            .expect_err("uncertain bind returns no lease");
    } else {
        let lease = table.bind(&id, &agent, 2).unwrap();
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
        table.current(&id).is_none(),
        "uncertainty cannot grant custody through current"
    );
    assert!(table.current(&other).is_none(), "whole table is fenced");
    assert!(table.active_for_agent(&agent).is_none());
    assert!(matches!(
        table.bind(&id, &agent, 3),
        Err(WorktreeError::StorageFailure { .. })
    ));
    assert!(matches!(
        prior.complete(&mut table),
        Err(WorktreeError::StorageFailure { .. })
    ));
    assert_eq!(fs::read(&path).unwrap(), before);
    assert!(
        BindingTable::open(&root).is_err(),
        "poison retains exclusive lock"
    );
    drop(table);
    let mut table = BindingTable::open(&root).unwrap();
    if operation == "bind" {
        assert!(table.current(&id).is_some());
        assert!(matches!(
            table.bind(&id, &agent, 4),
            Err(WorktreeError::WorktreeBusy { .. })
        ));
    } else {
        assert!(table.current(&id).is_none());
        let lease = table.bind(&id, &agent, 4).unwrap();
        lease.complete(&mut table).unwrap();
        let lease = table.bind(&id, &agent, 5).unwrap();
        lease.release(&mut table).unwrap();
    }
    // Another previously active row is retained; no lease is manufactured on reopen.
    assert!(matches!(
        table.bind(&other, &agent, 6),
        Err(WorktreeError::WorktreeBusy { .. })
    ));
}

#[test]
fn binding_public_paths_fence_uncertain_custody_until_reopen() {
    let temp = tempfile::tempdir().unwrap();
    let library = temp.path().join("binding-fault.so");
    assert!(Command::new("cc")
        .args(["-shared", "-fPIC", "-Wall", "-Werror"])
        .arg(
            PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join("tests/fixtures/binding_directory_fault.c")
        )
        .arg("-o")
        .arg(&library)
        .arg("-ldl")
        .status()
        .unwrap()
        .success());
    for operation in ["bind", "settle", "reopen"] {
        for kind in ["open", "sync"] {
            let root = temp.path().join(format!("{operation}-{kind}/new/deep"));
            let log = temp.path().join(format!("{operation}-{kind}.hits"));
            let arm = temp.path().join(format!("{operation}-{kind}.arm"));
            let target = if operation == "reopen" {
                root.join("wt-one.json")
            } else {
                root.clone()
            };
            let output = Command::new(std::env::current_exe().unwrap())
                .args(["--exact", "binding_fault_child", "--nocapture"])
                .env("LD_PRELOAD", &library)
                .env("BIND_ROOT", &root)
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
