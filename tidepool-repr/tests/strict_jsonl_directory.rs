#![cfg(target_os = "linux")]
use std::{fs, path::Path, process::Command};
use tidepool_repr::jsonl::{append_new_line, SyncPolicy};

#[test]
fn jsonl_fault_child() {
    let Ok(root) = std::env::var("FAULT_ROOT") else {
        return;
    };
    let policy = match std::env::var("FAULT_POLICY").unwrap().as_str() {
        "all" => SyncPolicy::All,
        "data" => SyncPolicy::Data,
        "none" => SyncPolicy::None,
        _ => panic!(),
    };
    let path = Path::new(&root).join("rows");
    let result = append_new_line(&path, "1", policy);
    assert_eq!(result.is_ok(), policy == SyncPolicy::None, "{result:?}");
    assert!(fs::read_to_string(path).unwrap().ends_with("1\n"));
}

#[test]
fn strict_jsonl_append_syncs_new_and_existing_directory_entries() {
    let root = std::env::temp_dir().join(format!(
        "jsonl-directory-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    fs::create_dir(&root).unwrap();
    struct Remove(std::path::PathBuf);
    impl Drop for Remove {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }
    let _remove = Remove(root.clone());
    let source = root.join("fault.c");
    fs::write(
        &source,
        include_str!("../../tidepool-atomic-write/tests/fixtures/directory_fault.c"),
    )
    .unwrap();
    let library = root.join("fault.so");
    assert!(Command::new("cc")
        .args(["-shared", "-fPIC", "-Wall", "-Werror"])
        .arg(source)
        .arg("-o")
        .arg(&library)
        .arg("-ldl")
        .status()
        .unwrap()
        .success());
    for policy in ["all", "data", "none"] {
        for existing in [false, true] {
            let dir = root.join(format!("{policy}-{existing}"));
            fs::create_dir(&dir).unwrap();
            if existing {
                fs::write(dir.join("rows"), "0\n").unwrap();
            }
            let log = root.join(format!("hit-{policy}-{existing}"));
            let output = Command::new(std::env::current_exe().unwrap())
                .args(["--exact", "jsonl_fault_child", "--nocapture"])
                .env("LD_PRELOAD", &library)
                .env("FAULT_ROOT", &dir)
                .env("FAULT_PATH", &dir)
                .env("FAULT_LOG", &log)
                .env("FAULT_KIND", "sync")
                .env("FAULT_POLICY", policy)
                .output()
                .unwrap();
            assert!(
                output.status.success(),
                "{policy}/{existing}: {} {}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
            assert_eq!(log.exists(), policy != "none", "directory fault reached");
        }
    }
    assert!(append_new_line(&root.join("missing/rows"), "1", SyncPolicy::All).is_err());
    assert!(!root.join("missing").exists());
}
