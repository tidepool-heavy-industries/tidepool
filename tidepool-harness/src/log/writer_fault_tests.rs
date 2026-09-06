use super::*;
use std::{fs, process::Command};

fn header() -> LogHeader {
    LogHeader {
        prelude_hash: "fault".into(),
        extract_fingerprint: "fault".into(),
        harness_version: "fault".into(),
    }
}
fn event() -> Event {
    Event::TurnStart {
        node: crate::NodeId(1),
        source: "fault".into(),
        input: None,
    }
}

#[test]
fn process_child() {
    let Ok(root) = std::env::var("LOG_FAULT_ROOT") else {
        return;
    };
    let root = Path::new(&root);
    let path = root.join("nested/deep/log.jsonl");
    match std::env::var("LOG_FAULT_CASE").unwrap().as_str() {
        "ancestry" => {
            assert!(LogWriter::create(&path, &header()).is_err());
            assert!(path.parent().unwrap().is_dir());
            assert!(!path.exists());
            LogWriter::create(&path, &header()).unwrap();
        }
        "publish" => {
            assert!(LogWriter::create(&path, &header()).is_err());
            let bytes = fs::read(&path).unwrap();
            assert_eq!(bytes.iter().filter(|&&c| c == b'\n').count(), 1);
            assert!(LogWriter::create(&path, &header()).is_err());
            assert_eq!(fs::read(&path).unwrap(), bytes);
        }
        "append" => {
            let mut writer = LogWriter::create(&path, &header()).unwrap();
            assert!(writer.append(event()).is_err());
            let bytes = fs::read(&path).unwrap();
            assert_eq!(bytes.iter().filter(|&&c| c == b'\n').count(), 2);
            assert!(matches!(writer.append(event()), Err(WriteError::Uncertain)));
            assert_eq!(fs::read(&path).unwrap(), bytes);
        }
        _ => panic!("unknown case"),
    }
}

#[test]
fn public_writer_faults_retain_publication_and_fence_sequence() {
    let tmp = tempfile::tempdir().unwrap();
    let source = tmp.path().join("fault.c");
    fs::write(&source, include_str!("fixtures/writer_fault.c")).unwrap();
    let lib = tmp.path().join("fault.so");
    assert!(Command::new("cc")
        .args(["-shared", "-fPIC", "-Wall", "-Werror"])
        .arg(source)
        .arg("-o")
        .arg(&lib)
        .arg("-ldl")
        .status()
        .unwrap()
        .success());
    for (i, (case, kind, suffix, nth)) in [
        ("ancestry", "sync", "", "1"),
        ("publish", "open", "nested/deep", "2"),
        ("publish", "sync", "nested/deep", "2"),
        ("append", "sync", "nested/deep/log.jsonl", "2"),
    ]
    .into_iter()
    .enumerate()
    {
        let root = tmp.path().join(i.to_string());
        fs::create_dir(&root).unwrap();
        let hits = tmp.path().join(format!("hits-{i}"));
        let result = Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "log::writer::fault_tests::process_child",
                "--nocapture",
            ])
            .env("LD_PRELOAD", &lib)
            .env("LOG_FAULT_ROOT", &root)
            .env(
                "LOG_FAULT_PATH",
                if suffix.is_empty() {
                    root.clone()
                } else {
                    root.join(suffix)
                },
            )
            .env("LOG_FAULT_NTH", nth)
            .env("LOG_FAULT_CASE", case)
            .env("LOG_FAULT_KIND", kind)
            .env("LOG_FAULT_HITS", &hits)
            .output()
            .unwrap();
        assert!(
            result.status.success(),
            "{case}/{kind}: {} {}",
            String::from_utf8_lossy(&result.stdout),
            String::from_utf8_lossy(&result.stderr)
        );
        assert_eq!(fs::read_to_string(hits).unwrap(), "hit\n");
    }
    let path = tmp.path().join("normal/new/log.jsonl");
    let mut writer = LogWriter::create(&path, &header()).unwrap();
    assert_eq!(writer.append(event()).unwrap(), 0);
    assert_eq!(writer.append(event()).unwrap(), 1);
    drop(writer);
    assert!(LogWriter::create(&path, &header()).is_err());
}
