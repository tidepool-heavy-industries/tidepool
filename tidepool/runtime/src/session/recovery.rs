//! Durable declaration recovery for resident sessions. Legacy v1 records can
//! replay only when their whole source surface passes the explicit safe policy;
//! v2 records retain exact artifacts and declaration graph metadata.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::Value;
use tidepool_repr::{version_ladder, Generation};

use super::SessionError;

#[path = "newrecovery_v2.rs"]
mod newrecovery_v2;
pub use newrecovery_v2::RecoveryPublicOwner;
pub(crate) use newrecovery_v2::*;

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

#[derive(Clone, Debug)]
pub(crate) struct LegacyV1Migration {
    pub high_water: Generation,
    pub replay_safe: Vec<RecoveryTurn>,
    pub lost: Vec<LostDeclaration>,
}

/// Convert the old source-only format explicitly. It has no exact export or
/// artifact identities, so non-replayable turns and head-only retractions are
/// reported lost and cannot erase or reveal graph winners by guesswork.
pub(crate) fn migrate_v1(manifest: &RecoveryManifest) -> LegacyV1Migration {
    let high_water = manifest
        .turns
        .iter()
        .map(|turn| turn.generation)
        .max()
        .unwrap_or(0);
    let mut migration = LegacyV1Migration {
        high_water: Generation(high_water),
        replay_safe: Vec::new(),
        lost: Vec::new(),
    };
    let mut prior_generation = 0;
    let mut manifest_reason = None;
    for turn in &manifest.turns {
        let reason = if turn.origin_session != manifest.source_session {
            Some("legacy turn belongs to a different source session")
        } else if !turn.has_valid_hash() {
            Some("legacy turn checksum is invalid")
        } else if turn.generation <= prior_generation {
            Some("legacy generations are not strictly increasing")
        } else if !turn.replayable {
            Some("legacy turn depended on unavailable resident state")
        } else if !turn.retracts.is_empty() {
            Some("legacy retraction lacks exact GHC export identities")
        } else if turn.sources.is_empty() {
            Some("legacy turn has no source to migrate")
        } else {
            None
        };
        if manifest_reason.is_none() {
            manifest_reason = reason;
        }
        prior_generation = prior_generation.max(turn.generation);
    }
    if manifest_reason.is_none() {
        migration.replay_safe.extend(manifest.turns.iter().cloned());
        return migration;
    }
    for turn in &manifest.turns {
        let reason = if turn.origin_session != manifest.source_session {
            "legacy turn belongs to a different source session"
        } else if !turn.has_valid_hash() {
            "legacy turn checksum is invalid"
        } else if !turn.replayable {
            "legacy turn depended on unavailable resident state"
        } else if !turn.retracts.is_empty() {
            "legacy retraction lacks exact GHC export identities"
        } else if turn.sources.is_empty() {
            "legacy turn has no source to migrate"
        } else {
            "the v1 manifest contains another ambiguous turn; preserve the whole surface as lost"
        };
        migration.lost.push(LostDeclaration {
            origin_session: turn.origin_session,
            source_generation: turn.generation,
            source_hash: turn.source_hash.clone(),
            sources: turn.sources.clone(),
            reason: reason.into(),
        });
    }
    migration
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

/// Exact declaration-environment facts established while starting a successor.
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

    #[test]
    fn v1_migration_requires_the_complete_surface_to_be_unambiguous() {
        let manifest = RecoveryManifest {
            version: 1,
            source_session: 41,
            turns: vec![
                RecoveryTurn::new(41, 1, vec!["data A = A".into()], vec![], true),
                RecoveryTurn::new(41, 2, vec![], vec!["A".into()], true),
                RecoveryTurn::new(41, 3, vec!["x = 1".into()], vec![], false),
            ],
        };
        let migrated = migrate_v1(&manifest);
        assert_eq!(migrated.high_water, Generation(3));
        assert!(migrated.replay_safe.is_empty());
        assert_eq!(migrated.lost.len(), 3);
        assert!(migrated
            .lost
            .iter()
            .any(|lost| lost.reason.contains("exact GHC export identities")));

        let safe = RecoveryManifest {
            version: 1,
            source_session: 41,
            turns: vec![
                RecoveryTurn::new(41, 1, vec!["data A = A".into()], vec![], true),
                RecoveryTurn::new(41, 2, vec!["x = A".into()], vec![], true),
            ],
        };
        let migrated = migrate_v1(&safe);
        assert_eq!(migrated.replay_safe.len(), 2);
        assert!(migrated.lost.is_empty());
    }
}
