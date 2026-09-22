//! Persisted project trust for unattended interactive Codex actors.
//!
//! Codex's TUI requires an exact project path to be trusted in the user config;
//! a `-c projects.<path>.trust_level=...` override does not bypass that prompt.
//! Shoal therefore presents every actor repository at one stable virtual path
//! and performs the same persisted decision as selecting “Yes” for that path
//! in the TUI. Generated repositories never accumulate config entries. This is
//! deliberately limited to interactive actors: headless agent cycles retain
//! their config-isolation contract.

use std::path::Path;
use std::str::FromStr;

use toml_edit::{value, DocumentMut, Item, Table};

use crate::AgentBackendError;

/// Mark one exact, host-selected virtual project root trusted in the
/// operator's Codex config. The update preserves unrelated formatting and
/// comments and is idempotent.
pub fn trust_interactive_project(project_root: &Path) -> Result<(), AgentBackendError> {
    let project_root = project_root.canonicalize().map_err(|error| {
        unavailable(
            format!("resolve interactive project {}", project_root.display()),
            error,
        )
    })?;
    trust_project_in_home(
        &crate::backend::codex::isolation::codex_home(),
        &project_root,
    )
}

fn trust_project_in_home(codex_home: &Path, project_root: &Path) -> Result<(), AgentBackendError> {
    std::fs::create_dir_all(codex_home).map_err(|error| {
        unavailable(format!("create Codex home {}", codex_home.display()), error)
    })?;
    let directory = std::fs::File::open(codex_home)
        .map_err(|error| unavailable(format!("open Codex home {}", codex_home.display()), error))?;
    directory
        .lock()
        .map_err(|error| unavailable("lock Codex config directory", error))?;
    let rules_dir = codex_home.join("rules");
    std::fs::create_dir_all(&rules_dir).map_err(|error| {
        unavailable(
            format!(
                "prepare interactive policy directory {}",
                rules_dir.display()
            ),
            error,
        )
    })?;

    let config_path = codex_home.join("config.toml");
    let source = match std::fs::read_to_string(&config_path) {
        Ok(source) => source,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(error) => {
            return Err(unavailable(
                format!("read {}", config_path.display()),
                error,
            ))
        }
    };
    let mut document =
        DocumentMut::from_str(&source).map_err(|error| AgentBackendError::ProtocolRejected {
            detail: format!(
                "cannot update malformed Codex config {}: {error}",
                config_path.display()
            ),
        })?;
    install_trust(&mut document, project_root)?;
    let rendered = document.to_string();
    if rendered == source {
        return Ok(());
    }
    tidepool_atomic_write::write_durable(&config_path, rendered.as_bytes()).map_err(|error| {
        unavailable(
            format!("persist Codex project trust to {}", config_path.display()),
            error,
        )
    })
}

fn install_trust(document: &mut DocumentMut, project_root: &Path) -> Result<(), AgentBackendError> {
    ensure_table(document.as_table_mut(), "projects")?;
    let projects = document["projects"]
        .as_table_mut()
        .ok_or_else(|| invalid_table("projects"))?;
    let project = project_root.to_string_lossy();
    ensure_table(projects, &project)?;
    let table = projects[project.as_ref()]
        .as_table_mut()
        .ok_or_else(|| invalid_table(&format!("projects.{project:?}")))?;
    table.insert("trust_level", value("trusted"));
    Ok(())
}

fn ensure_table(table: &mut Table, key: &str) -> Result<(), AgentBackendError> {
    match table.get(key) {
        None => {
            table.insert(key, Item::Table(Table::new()));
            Ok(())
        }
        Some(Item::Table(_)) => Ok(()),
        Some(_) => Err(invalid_table(key)),
    }
}

fn invalid_table(key: &str) -> AgentBackendError {
    AgentBackendError::ProtocolRejected {
        detail: format!("Codex config key {key:?} must be a table to record project trust"),
    }
}

fn unavailable(operation: impl Into<String>, error: impl std::fmt::Display) -> AgentBackendError {
    AgentBackendError::BackendUnavailable {
        detail: format!("{}: {error}", operation.into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn trust_is_exact_idempotent_and_preserves_unrelated_text() {
        let home = tempfile::tempdir().unwrap();
        let project = tempfile::tempdir().unwrap();
        let config = home.path().join("config.toml");
        std::fs::write(
            &config,
            "# keep this comment\nmodel = \"gpt-test\"\n\n[projects.\"/already\"]\ntrust_level = \"trusted\"\n",
        )
        .unwrap();

        trust_project_in_home(home.path(), project.path()).unwrap();
        let once = std::fs::read_to_string(&config).unwrap();
        trust_project_in_home(home.path(), project.path()).unwrap();
        let twice = std::fs::read_to_string(&config).unwrap();

        assert_eq!(once, twice);
        assert!(home.path().join("rules").is_dir());
        assert!(once.contains("# keep this comment"));
        assert!(once.contains("model = \"gpt-test\""));
        let parsed = DocumentMut::from_str(&once).unwrap();
        assert_eq!(
            parsed["projects"][project.path().to_string_lossy().as_ref()]["trust_level"].as_str(),
            Some("trusted")
        );
    }

    #[test]
    fn malformed_projects_shape_fails_typed_without_rewriting() {
        let home = tempfile::tempdir().unwrap();
        let project = tempfile::tempdir().unwrap();
        let config = home.path().join("config.toml");
        std::fs::write(&config, "projects = \"not a table\"\n").unwrap();

        assert!(matches!(
            trust_project_in_home(home.path(), project.path()),
            Err(AgentBackendError::ProtocolRejected { .. })
        ));
        assert_eq!(
            std::fs::read_to_string(config).unwrap(),
            "projects = \"not a table\"\n"
        );
    }
}
