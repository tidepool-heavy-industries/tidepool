//! Durable source-only declaration recovery for a resident session.
//!
//! The manifest deliberately contains no evaluated values, closures, effect
//! handles, or heap identities. A successor session may replay ordinary root
//! declaration source through GHC; everything resident stays tied to the
//! machine incarnation that created it.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::Value;
use tidepool_repr::version_ladder;

use super::SessionError;

const FLOOR: u32 = 1;
const CURRENT: u32 = 1;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub(crate) struct RecoveryManifest {
    pub version: u32,
    pub source_session: u64,
    pub turns: Vec<RecoveryTurn>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub(crate) struct RecoveryTurn {
    pub origin_session: u64,
    pub generation: u64,
    pub sources: Vec<String>,
    pub retracts: Vec<String>,
    pub replayable: bool,
    pub source_hash: String,
}

impl RecoveryTurn {
    pub(crate) fn new(
        origin_session: u64,
        generation: u64,
        sources: Vec<String>,
        retracts: Vec<String>,
        replayable: bool,
    ) -> Self {
        let source_hash = turn_hash(origin_session, generation, &sources, &retracts, replayable);
        Self {
            origin_session,
            generation,
            sources,
            retracts,
            replayable,
            source_hash,
        }
    }

    fn has_valid_hash(&self) -> bool {
        self.source_hash
            == turn_hash(
                self.origin_session,
                self.generation,
                &self.sources,
                &self.retracts,
                self.replayable,
            )
    }
}

/// One declaration turn successfully reconstructed in a successor session.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReplayedDeclaration {
    pub origin_session: u64,
    pub source_generation: u64,
    pub successor_generation: u64,
    pub source_hash: String,
}

/// Source that could not be reconstructed without the lost resident machine.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LostDeclaration {
    pub origin_session: u64,
    pub source_generation: u64,
    pub source_hash: String,
    pub sources: Vec<String>,
    pub reason: String,
}

/// Exact declaration-plane facts established while starting a successor.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DeclarationRecoveryReport {
    pub source_session: Option<u64>,
    pub successor_session: u64,
    pub replayed: Vec<ReplayedDeclaration>,
    pub lost: Vec<LostDeclaration>,
}

pub(crate) fn read(path: &Path) -> Result<Option<RecoveryManifest>, SessionError> {
    let bytes = match std::fs::read(path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => {
            return Err(manifest_error(
                path,
                format!("could not read manifest: {error}"),
            ))
        }
    };
    let raw: Value = serde_json::from_slice(&bytes)
        .map_err(|error| manifest_error(path, format!("invalid JSON: {error}")))?;
    let found = version_ladder::found_version(&raw);
    let current = version_ladder::migrate_to_current(raw, found, FLOOR, CURRENT, &[])
        .map_err(|error| manifest_error(path, error.to_string()))?;
    let manifest: RecoveryManifest = serde_json::from_value(current)
        .map_err(|error| manifest_error(path, format!("invalid manifest shape: {error}")))?;
    for turn in &manifest.turns {
        if !turn.has_valid_hash() {
            return Err(manifest_error(
                path,
                format!(
                    "source hash mismatch at recorded generation {}",
                    turn.generation
                ),
            ));
        }
    }
    Ok(Some(manifest))
}

pub(crate) fn write(
    path: &Path,
    source_session: u64,
    turns: &[RecoveryTurn],
) -> Result<(), SessionError> {
    if let Some(parent) = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
    {
        std::fs::create_dir_all(parent).map_err(|error| {
            manifest_error(
                parent,
                format!("could not create manifest directory: {error}"),
            )
        })?;
    }
    let bytes = serde_json::to_vec_pretty(&RecoveryManifest {
        version: CURRENT,
        source_session,
        turns: turns.to_vec(),
    })
    .map_err(|error| manifest_error(path, format!("could not encode manifest: {error}")))?;
    tidepool_atomic_write::write_durable(path, &bytes).map_err(|error| {
        manifest_error(
            &error.path,
            format!("could not durably write manifest: {}", error.source),
        )
    })
}

fn turn_hash(
    origin_session: u64,
    generation: u64,
    sources: &[String],
    retracts: &[String],
    replayable: bool,
) -> String {
    let mut hasher = blake3::Hasher::new();
    hasher.update(&origin_session.to_le_bytes());
    hasher.update(&generation.to_le_bytes());
    hasher.update(&[u8::from(replayable)]);
    hash_strings(&mut hasher, sources);
    hash_strings(&mut hasher, retracts);
    hasher.finalize().to_hex().to_string()
}

fn hash_strings(hasher: &mut blake3::Hasher, values: &[String]) {
    hasher.update(&(values.len() as u64).to_le_bytes());
    for value in values {
        hasher.update(&(value.len() as u64).to_le_bytes());
        hasher.update(value.as_bytes());
    }
}

fn manifest_error(path: &Path, detail: String) -> SessionError {
    SessionError::RecoveryManifest {
        path: PathBuf::from(path),
        detail,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn manifest_round_trips_and_rejects_modified_source() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("root-declarations.json");
        let turn = RecoveryTurn::new(
            41,
            1,
            vec!["data Decision = Accept | Reject".into()],
            Vec::new(),
            true,
        );
        write(&path, 41, std::slice::from_ref(&turn)).unwrap();
        let loaded = read(&path).unwrap().unwrap();
        assert_eq!(loaded.source_session, 41);
        assert_eq!(loaded.turns[0].source_hash, turn.source_hash);

        let mut raw: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        raw["turns"][0]["sources"][0] = Value::String("data Decision = Maybe".into());
        std::fs::write(&path, serde_json::to_vec(&raw).unwrap()).unwrap();
        assert!(matches!(
            read(&path),
            Err(SessionError::RecoveryManifest { detail, .. })
                if detail.contains("source hash mismatch")
        ));
    }

    #[test]
    fn future_manifest_version_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("root-declarations.json");
        std::fs::write(&path, br#"{"version":2,"source_session":1,"turns":[]}"#).unwrap();
        assert!(matches!(
            read(&path),
            Err(SessionError::RecoveryManifest { detail, .. })
                if detail.contains("newer than this build")
        ));
    }
}
