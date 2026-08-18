//! Config isolation checker for the operator's Codex home directory.
//!
//! PRD 18 acceptance criterion 11: no normal worker run mutates the
//! operator's global Codex configuration. `$CODEX_HOME` (`~/.codex` when
//! unset) also holds live sqlite databases the operator's own Codex sessions
//! write to continuously (`logs_2.sqlite`, `goals_1.sqlite`,
//! `memories_1.sqlite`, plus their `-wal`/`-shm` siblings) — diffing the
//! whole directory against that is pure noise and reports false positives.
//! This checker scopes the assertion to configuration and credentials only:
//! `config.toml`, `auth.json`, `installation_id`, and the top-level file
//! listing (to catch a new file appearing without caring what the live
//! databases do to their own bytes).
//!
//! A new `projects.<path>` entry in `config.toml` — Codex's project-trust
//! write on a workspace-write thread start — surfaces here as a
//! `config.toml` hash mismatch, which is exactly the failure this checker
//! exists to catch.

use std::collections::BTreeSet;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};

/// Resolve the Codex home directory the same way the CLI does: `$CODEX_HOME`
/// if set, else `~/.codex`.
pub fn codex_home() -> PathBuf {
    if let Some(dir) = std::env::var_os("CODEX_HOME") {
        return PathBuf::from(dir);
    }
    let home = std::env::var_os("HOME").expect("HOME must be set to locate ~/.codex");
    PathBuf::from(home).join(".codex")
}

/// A point-in-time snapshot of the config/credential surface under a Codex
/// home directory.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConfigSnapshot {
    codex_home: PathBuf,
    config_toml_sha256: Option<String>,
    auth_json_sha256: Option<String>,
    installation_id: Option<String>,
    top_level_names: BTreeSet<String>,
}

impl ConfigSnapshot {
    /// Capture the current state of `codex_home`. A missing file is recorded
    /// as `None` rather than erroring — a fresh `CODEX_HOME` has none of
    /// these yet, and that absence is itself part of the snapshot.
    pub fn capture(codex_home: impl Into<PathBuf>) -> io::Result<Self> {
        let codex_home = codex_home.into();
        let config_toml_sha256 = hash_file(&codex_home.join("config.toml"))?;
        let auth_json_sha256 = hash_file(&codex_home.join("auth.json"))?;
        let installation_id = read_trimmed(&codex_home.join("installation_id"))?;
        let top_level_names = list_top_level(&codex_home)?;
        Ok(Self {
            codex_home,
            config_toml_sha256,
            auth_json_sha256,
            installation_id,
            top_level_names,
        })
    }

    /// Compare this snapshot (the "before") against a later one (the
    /// "after") of the same Codex home.
    ///
    /// # Panics
    ///
    /// Panics if the two snapshots were captured against different
    /// directories — comparing them would not answer "did this run mutate
    /// the operator's config".
    pub fn compare(&self, after: &ConfigSnapshot) -> IsolationReport {
        assert_eq!(
            self.codex_home, after.codex_home,
            "compared snapshots of different codex homes: {:?} vs {:?}",
            self.codex_home, after.codex_home,
        );
        IsolationReport {
            config_toml: FieldVerdict {
                field: "config.toml",
                before: self.config_toml_sha256.clone(),
                after: after.config_toml_sha256.clone(),
            },
            auth_json: FieldVerdict {
                field: "auth.json",
                before: self.auth_json_sha256.clone(),
                after: after.auth_json_sha256.clone(),
            },
            installation_id: FieldVerdict {
                field: "installation_id",
                before: self.installation_id.clone(),
                after: after.installation_id.clone(),
            },
            new_top_level_files: after
                .top_level_names
                .difference(&self.top_level_names)
                .filter(|name| !is_benign_wal_sidecar(name, &self.top_level_names))
                .cloned()
                .collect(),
        }
    }
}

/// Whether `name` is a `-wal`/`-shm` sidecar SQLite creates on first open in
/// WAL mode for a database that already existed before the run.
///
/// Confirmed empirically: starting `codex app-server` against a real
/// `~/.codex` creates `-shm`/`-wal` siblings for `goals_1.sqlite`,
/// `memories_1.sqlite`, and `state_5.sqlite` even though their *content*
/// hash is untouched — this is SQLite's WAL bookkeeping on open, not a new
/// state store, and not the project-trust write this checker cares about. A
/// sidecar for a `.sqlite` file that did NOT already exist is still flagged:
/// that would mean a new database was created this run, which is
/// surprising.
fn is_benign_wal_sidecar(name: &str, before: &BTreeSet<String>) -> bool {
    let Some(base) = name
        .strip_suffix("-wal")
        .or_else(|| name.strip_suffix("-shm"))
    else {
        return false;
    };
    before.contains(base)
}

/// One tracked field's before/after value. `before == after` is the pass
/// condition; the values themselves (sha256 hex digests, or the
/// installation id string) are the report, not a boolean.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FieldVerdict {
    pub field: &'static str,
    pub before: Option<String>,
    pub after: Option<String>,
}

impl FieldVerdict {
    pub fn identical(&self) -> bool {
        self.before == self.after
    }
}

/// The result of comparing two [`ConfigSnapshot`]s: every field individually,
/// not just an aggregate verdict, so a failure shows which value changed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IsolationReport {
    pub config_toml: FieldVerdict,
    pub auth_json: FieldVerdict,
    pub installation_id: FieldVerdict,
    pub new_top_level_files: BTreeSet<String>,
}

impl IsolationReport {
    pub fn passed(&self) -> bool {
        self.config_toml.identical()
            && self.auth_json.identical()
            && self.installation_id.identical()
            && self.new_top_level_files.is_empty()
    }

    /// Render as per-file comparison lines, suitable for quoting verbatim in
    /// a report.
    pub fn render(&self) -> String {
        let mut out = String::new();
        for v in [&self.config_toml, &self.auth_json, &self.installation_id] {
            out.push_str(&format!(
                "{}: before={:?} after={:?} identical={}\n",
                v.field,
                v.before,
                v.after,
                v.identical()
            ));
        }
        if self.new_top_level_files.is_empty() {
            out.push_str("new top-level files: none\n");
        } else {
            out.push_str(&format!(
                "new top-level files: {:?}\n",
                self.new_top_level_files
            ));
        }
        out
    }
}

fn hash_file(path: &Path) -> io::Result<Option<String>> {
    match fs::read(path) {
        Ok(bytes) => {
            let mut hasher = Sha256::new();
            hasher.update(&bytes);
            let digest = hasher.finalize();
            Ok(Some(
                digest
                    .iter()
                    .map(|b| format!("{b:02x}"))
                    .collect::<String>(),
            ))
        }
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e),
    }
}

fn read_trimmed(path: &Path) -> io::Result<Option<String>> {
    match fs::read_to_string(path) {
        Ok(s) => Ok(Some(s.trim().to_string())),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e),
    }
}

fn list_top_level(dir: &Path) -> io::Result<BTreeSet<String>> {
    match fs::read_dir(dir) {
        Ok(entries) => entries
            .map(|entry| entry.map(|e| e.file_name().to_string_lossy().into_owned()))
            .collect(),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(BTreeSet::new()),
        Err(e) => Err(e),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write(dir: &Path, name: &str, contents: &str) {
        fs::write(dir.join(name), contents).unwrap();
    }

    #[test]
    fn identical_snapshot_passes() {
        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), "config.toml", "model = \"gpt-5.6-terra\"\n");
        write(dir.path(), "auth.json", "{\"token\":\"redacted\"}");
        write(dir.path(), "installation_id", "abc-123\n");

        let before = ConfigSnapshot::capture(dir.path()).unwrap();
        let after = ConfigSnapshot::capture(dir.path()).unwrap();
        let report = before.compare(&after);

        assert!(report.passed(), "{}", report.render());
        assert!(report.config_toml.identical());
        assert!(report.auth_json.identical());
        assert!(report.installation_id.identical());
        assert!(report.new_top_level_files.is_empty());
    }

    #[test]
    fn config_toml_mutation_is_detected() {
        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), "config.toml", "model = \"gpt-5.6-terra\"\n");

        let before = ConfigSnapshot::capture(dir.path()).unwrap();
        write(
            dir.path(),
            "config.toml",
            "model = \"gpt-5.6-terra\"\n[projects.\"/tmp/worker\"]\ntrusted = true\n",
        );
        let after = ConfigSnapshot::capture(dir.path()).unwrap();
        let report = before.compare(&after);

        assert!(!report.passed());
        assert!(!report.config_toml.identical());
        assert_ne!(report.config_toml.before, report.config_toml.after);
    }

    #[test]
    fn auth_json_mutation_is_detected() {
        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), "auth.json", "{\"token\":\"a\"}");

        let before = ConfigSnapshot::capture(dir.path()).unwrap();
        write(dir.path(), "auth.json", "{\"token\":\"b\"}");
        let after = ConfigSnapshot::capture(dir.path()).unwrap();
        let report = before.compare(&after);

        assert!(!report.passed());
        assert!(!report.auth_json.identical());
    }

    #[test]
    fn new_top_level_file_is_detected() {
        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), "config.toml", "model = \"gpt-5.6-terra\"\n");

        let before = ConfigSnapshot::capture(dir.path()).unwrap();
        write(dir.path(), "unexpected.lock", "");
        let after = ConfigSnapshot::capture(dir.path()).unwrap();
        let report = before.compare(&after);

        assert!(!report.passed());
        assert!(report.new_top_level_files.contains("unexpected.lock"));
    }

    #[test]
    fn live_database_churn_is_ignored() {
        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), "config.toml", "model = \"gpt-5.6-terra\"\n");
        write(dir.path(), "logs_2.sqlite", "initial-bytes");
        write(dir.path(), "logs_2.sqlite-wal", "wal-bytes-1");

        let before = ConfigSnapshot::capture(dir.path()).unwrap();
        write(
            dir.path(),
            "logs_2.sqlite",
            "mutated-bytes-from-a-live-session",
        );
        write(dir.path(), "logs_2.sqlite-wal", "wal-bytes-2-longer-now");
        let after = ConfigSnapshot::capture(dir.path()).unwrap();
        let report = before.compare(&after);
        assert!(report.passed(), "{}", report.render());
    }

    #[test]
    fn wal_sidecar_appearing_for_a_preexisting_database_is_ignored() {
        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), "config.toml", "model = \"gpt-5.6-terra\"\n");
        write(dir.path(), "goals_1.sqlite", "pre-existing-bytes");

        let before = ConfigSnapshot::capture(dir.path()).unwrap();
        write(dir.path(), "goals_1.sqlite-wal", "");
        write(dir.path(), "goals_1.sqlite-shm", "");
        let after = ConfigSnapshot::capture(dir.path()).unwrap();
        let report = before.compare(&after);

        assert!(report.passed(), "{}", report.render());
    }

    #[test]
    fn wal_sidecar_for_a_brand_new_database_is_still_flagged() {
        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), "config.toml", "model = \"gpt-5.6-terra\"\n");

        let before = ConfigSnapshot::capture(dir.path()).unwrap();
        write(dir.path(), "new_store.sqlite", "");
        write(dir.path(), "new_store.sqlite-wal", "");
        let after = ConfigSnapshot::capture(dir.path()).unwrap();
        let report = before.compare(&after);

        assert!(!report.passed());
        assert!(report.new_top_level_files.contains("new_store.sqlite"));
        assert!(report.new_top_level_files.contains("new_store.sqlite-wal"));
    }

    #[test]
    fn installation_id_mutation_is_detected() {
        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), "installation_id", "abc-123\n");

        let before = ConfigSnapshot::capture(dir.path()).unwrap();
        write(dir.path(), "installation_id", "different-id\n");
        let after = ConfigSnapshot::capture(dir.path()).unwrap();
        let report = before.compare(&after);

        assert!(!report.passed());
        assert!(!report.installation_id.identical());
    }

    #[test]
    fn missing_files_capture_as_none_not_an_error() {
        let dir = tempfile::tempdir().unwrap();
        let snap = ConfigSnapshot::capture(dir.path()).unwrap();
        assert_eq!(snap.config_toml_sha256, None);
        assert_eq!(snap.auth_json_sha256, None);
        assert_eq!(snap.installation_id, None);
    }

    #[test]
    fn codex_home_respects_env_override() {
        // SAFETY: single-threaded set/restore of a process-global env var,
        // scoped tightly around the read it's testing.
        let saved = std::env::var_os("CODEX_HOME");
        unsafe { std::env::set_var("CODEX_HOME", "/tmp/example-codex-home") };
        assert_eq!(codex_home(), PathBuf::from("/tmp/example-codex-home"));
        match saved {
            Some(v) => unsafe { std::env::set_var("CODEX_HOME", v) },
            None => unsafe { std::env::remove_var("CODEX_HOME") },
        }
    }
}
