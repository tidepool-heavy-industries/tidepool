#![cfg(target_os = "linux")]
use std::{fs, path::Path, process::Command};

#[test]
fn fault_child() {
    let Ok(root) = std::env::var("FAULT_ROOT") else {
        return;
    };
    let root = Path::new(&root);
    let operation = std::env::var("FAULT_OPERATION").unwrap();
    let result = match operation.as_str() {
        "durable" => tidepool_atomic_write::write_durable(&root.join("value"), b"new"),
        "best" => tidepool_atomic_write::write_best_effort(&root.join("value"), b"new"),
        "mkdir" => tidepool_atomic_write::create_dir_all_durable(&root.join("new/deep")),
        _ => panic!("unknown operation"),
    };
    assert_eq!(
        result.is_ok(),
        std::env::var("FAULT_EXPECT_OK").unwrap() == "yes",
        "{result:?}"
    );
    if operation == "mkdir" {
        assert!(root.join("new/deep").is_dir());
    } else {
        assert_eq!(fs::read(root.join("value")).unwrap(), b"new");
    }
}

#[test]
fn strict_directory_faults_are_reported_after_visible_publication() {
    let temp = tempfile::tempdir().unwrap();
    let library = temp.path().join("fault.so");
    let source = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/directory_fault.c");
    assert!(Command::new("cc")
        .args(["-shared", "-fPIC", "-Wall", "-Werror"])
        .arg(source)
        .args(["-o"])
        .arg(&library)
        .arg("-ldl")
        .status()
        .unwrap()
        .success());
    for (index, (operation, kind, ok, hits)) in [
        ("durable", "open", false, true),
        ("durable", "sync", false, true),
        ("best", "sync", true, false),
        ("mkdir", "sync", false, true),
        ("mkdir", "trace", true, true),
    ]
    .into_iter()
    .enumerate()
    {
        let root = temp.path().join(index.to_string());
        fs::create_dir(&root).unwrap();
        let log = temp.path().join(format!("hits-{index}"));
        let output = Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "fault_child", "--nocapture"])
            .env("LD_PRELOAD", &library)
            .env("FAULT_PATH", &root)
            .env("FAULT_ROOT", &root)
            .env("FAULT_LOG", &log)
            .env("FAULT_OPERATION", operation)
            .env("FAULT_KIND", kind)
            .env("FAULT_EXPECT_OK", if ok { "yes" } else { "no" })
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{operation}/{kind}: {} {}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        assert_eq!(log.exists(), hits, "injection coverage {operation}/{kind}");
        if hits {
            assert!(!fs::read(&log).unwrap().is_empty());
        }
        if operation == "mkdir" {
            tidepool_atomic_write::create_dir_all_durable(&root.join("new/deep")).unwrap();
        }
    }
    assert!(tidepool_atomic_write::write_durable(&temp.path().join("absent/value"), b"x").is_err());
    assert!(!temp.path().join("absent").exists());
}
