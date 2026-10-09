use std::io;
use std::path::Path;

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct WorkspaceGitlink {
    schema: u32,
    path: String,
    mode: String,
    revision: String,
}

pub(super) fn read_workspace_revision(path: &Path) -> io::Result<String> {
    let bytes = std::fs::read(path)?;
    let record: WorkspaceGitlink = serde_json::from_slice(&bytes)
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
    if record.schema != 1
        || record.path != ".exomonad/workspace"
        || record.mode != "160000"
        || record.revision.len() != 40
        || !record
            .revision
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "workspace Gitlink must describe one exact recorded submodule revision",
        ));
    }
    Ok(record.revision)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn record(revision: &str) -> serde_json::Value {
        serde_json::json!({
            "schema": 1, "path": ".exomonad/workspace", "mode": "160000",
            "revision": revision,
        })
    }

    #[test]
    fn workspace_revision_follows_the_declared_input() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("gitlink.json");
        for revision in ["1".repeat(40), "abcdef0123".repeat(4)] {
            std::fs::write(&path, serde_json::to_vec(&record(&revision)).unwrap()).unwrap();
            assert_eq!(read_workspace_revision(&path).unwrap(), revision);
        }
    }

    #[test]
    fn workspace_revision_refuses_missing_input() {
        let directory = tempfile::tempdir().unwrap();
        assert_eq!(
            read_workspace_revision(&directory.path().join("missing.json"))
                .unwrap_err()
                .kind(),
            io::ErrorKind::NotFound,
        );
    }

    #[test]
    fn workspace_revision_refuses_malformed_input() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("gitlink.json");
        let valid = record(&"a".repeat(40));
        for (field, value) in [
            ("schema", serde_json::json!(2)),
            ("path", serde_json::json!("other/workspace")),
            ("mode", serde_json::json!("100644")),
            ("revision", serde_json::json!("a".repeat(39))),
            ("revision", serde_json::json!("g".repeat(40))),
            ("revision", serde_json::json!("A".repeat(40))),
            ("unknown", serde_json::json!(true)),
        ] {
            let mut invalid = valid.clone();
            invalid[field] = value;
            std::fs::write(&path, serde_json::to_vec(&invalid).unwrap()).unwrap();
            assert_eq!(
                read_workspace_revision(&path).unwrap_err().kind(),
                io::ErrorKind::InvalidData,
                "invalid field {field}",
            );
        }
        std::fs::write(&path, b"not JSON").unwrap();
        assert_eq!(
            read_workspace_revision(&path).unwrap_err().kind(),
            io::ErrorKind::InvalidData,
        );
    }
}
