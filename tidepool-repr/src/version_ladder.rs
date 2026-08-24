//! The one shared migration-ladder mechanism for durable, non-reproducible
//! persistence artifacts (harness log, worktree/handlers journals, the
//! selfharness transcript, `Checkpoint`) — as opposed to `serial/`'s CBOR
//! wire format, whose artifacts (`.cbor` fixtures) are reproducible build
//! outputs and so can just be regenerated on a breaking bump. See
//! `plans/persistence-versioning-design.md` for the design this implements;
//! this module is Section 3's "the ladder itself."
//!
//! **Per-kind, not per-repo.** Every artifact kind (the harness log's
//! `Event`, a worktree journal row, a handlers-journal segment, the
//! selfharness transcript, the `Checkpoint` envelope, a harness's own
//! `State` blob) owns its OWN `floor`/`current`/migration table — mirroring
//! how `serial::VERSION_MAJOR`/`VERSION_MINOR` are scoped to one wire
//! format, not the whole repo. This module supplies only the fold; each
//! consumer supplies its own bounds and migration functions and does its
//! own typed, path-carrying rejection on top of [`LadderError`] (a
//! `PersistenceError::BelowFloor`, a `WorktreeError::JournalFutureVersion`,
//! …) — see this module's own doc on why the bound-check/typed-rejection
//! split is drawn there and not here.
//!
//! **The unstamped-file convention.** An artifact written before this
//! scheme existed carries no `"version"` key at all. [`found_version`]
//! reads that as `0`, never as the kind's current version — so a caller's
//! `floor` starts at `0` (accepting the pre-stamp shape) and its migration
//! table's first entry is the `0 -> 1` step that turns an unstamped payload
//! into an explicitly-versioned one. This is what lets a real operator's
//! on-disk checkpoint/log/journal, written before this design landed, keep
//! loading — the entire point of building a mechanism instead of leaving
//! the wire frozen by social discipline.

use serde_json::Value;

/// One migration step: transform a payload one version forward. A
/// conforming migration also advances the payload's own `"version"` key
/// (via [`set_version`]) to the version it produces — every step leaves the
/// value looking like a payload genuinely written at its target version,
/// not merely reshaped.
///
/// A plain `fn` pointer, not a closure: migrations are named, in-source,
/// individually testable steps (the `N` versions cost `N-1` steps idiom),
/// never constructed dynamically.
pub type Migration = fn(Value) -> Result<Value, MigrationError>;

/// Why one migration step failed. Kept as a plain message — the ladder
/// itself is schema-agnostic (it never knows what a given kind's payload
/// means), so a step's own code is the only place that can explain its
/// failure in domain terms.
#[derive(Debug, Clone, thiserror::Error)]
#[error("{0}")]
pub struct MigrationError(pub String);

/// Why [`migrate_to_current`] refused to produce a current-shape payload.
/// Carries no path — each caller's own error enum adds that (and an
/// operator-legible remedy) when it converts this into its own typed
/// variant; see `persistence-versioning-design.md` §6 for the floor-policy
/// message shape this feeds.
#[derive(Debug, Clone, thiserror::Error)]
pub enum LadderError {
    /// `found` is older than the oldest version this build still carries a
    /// migration path from. Never a silent reset — the artifact must be
    /// archived/deleted and restarted fresh, or read with an older build.
    #[error("version {found} is below the floor this build still supports ({floor})")]
    BelowFloor { found: u32, floor: u32 },
    /// `found` is newer than this build knows how to read — a build too old
    /// for a payload a newer build wrote.
    #[error("version {found} is newer than this build supports (current {current})")]
    UnsupportedVersion { found: u32, current: u32 },
    /// A migration step itself failed (domain-level surgery gone wrong, not
    /// a bounds violation).
    #[error("migration from version {from} failed: {source}")]
    Migration { from: u32, source: MigrationError },
}

/// Read a payload's `"version"` top-level key, defaulting to `0` — the
/// convention for an artifact written before this scheme existed (see the
/// module doc). Not `u64`: no artifact kind here needs more than `u32`
/// worth of migrations, and keeping the bound narrow makes an absurd value
/// (a corrupt or foreign payload) fail the same way an out-of-range version
/// legitimately would, rather than wrapping.
pub fn found_version(value: &Value) -> u32 {
    value
        .get("version")
        .and_then(Value::as_u64)
        .and_then(|v| u32::try_from(v).ok())
        .unwrap_or(0)
}

/// Set (or insert) `value`'s top-level `"version"` key to `to`. A no-op on a
/// non-object `value` — every real artifact kind this module serves is a
/// JSON object at the top level, so this only ever matters for the
/// identity/bootstrap migrations exercising it directly.
pub fn set_version(mut value: Value, to: u32) -> Value {
    if let Value::Object(map) = &mut value {
        map.insert("version".to_string(), Value::from(to));
    }
    value
}

/// Fold `value` forward from its `found` version to `current`, through
/// `migrations` (indexed from `floor`: `migrations[0]` is the
/// `floor -> floor + 1` step, `migrations[1]` is `floor + 1 -> floor + 2`,
/// …). Rejects loud and typed outside `[floor, current]` — see
/// `persistence-versioning-design.md` §6. Never silently regenerates or
/// resets; the caller decodes the returned value into its own current typed
/// shape.
///
/// Panics if `migrations` is shorter than the ladder needs to climb from
/// `found` to `current` — a caller registering a `current` its own table
/// cannot actually reach is a programming error in that caller, not a
/// runtime condition an operator can hit.
pub fn migrate_to_current(
    mut value: Value,
    found: u32,
    floor: u32,
    current: u32,
    migrations: &[Migration],
) -> Result<Value, LadderError> {
    if found < floor {
        return Err(LadderError::BelowFloor { found, floor });
    }
    if found > current {
        return Err(LadderError::UnsupportedVersion { found, current });
    }
    let mut v = found;
    while v < current {
        let idx = (v - floor) as usize;
        let step = migrations.get(idx).unwrap_or_else(|| {
            panic!("no migration registered for version {v} -> {} (floor {floor}, current {current}, table len {})", v + 1, migrations.len())
        });
        value = step(value).map_err(|source| LadderError::Migration { from: v, source })?;
        v += 1;
    }
    Ok(value)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn bump_to_1(v: Value) -> Result<Value, MigrationError> {
        Ok(set_version(v, 1))
    }

    #[test]
    fn found_version_defaults_to_zero_when_absent() {
        assert_eq!(found_version(&json!({"a": 1})), 0);
    }

    #[test]
    fn found_version_reads_present_key() {
        assert_eq!(found_version(&json!({"version": 7, "a": 1})), 7);
    }

    #[test]
    fn unstamped_payload_migrates_through_the_identity_step() {
        let v = json!({"a": 1});
        let migrated =
            migrate_to_current(v, found_version(&json!({"a":1})), 0, 1, &[bump_to_1]).unwrap();
        assert_eq!(migrated, json!({"a": 1, "version": 1}));
    }

    #[test]
    fn already_current_skips_the_ladder_entirely() {
        let v = json!({"a": 1, "version": 1});
        let migrated = migrate_to_current(v.clone(), 1, 0, 1, &[bump_to_1]).unwrap();
        assert_eq!(migrated, v, "no migration step should have run");
    }

    #[test]
    fn below_floor_is_a_typed_rejection() {
        let err = migrate_to_current(json!({"version": 0}), 0, 1, 2, &[bump_to_1]).unwrap_err();
        assert!(matches!(
            err,
            LadderError::BelowFloor { found: 0, floor: 1 }
        ));
    }

    #[test]
    fn future_version_is_a_typed_rejection() {
        let err = migrate_to_current(json!({"version": 9}), 9, 0, 1, &[bump_to_1]).unwrap_err();
        assert!(matches!(
            err,
            LadderError::UnsupportedVersion {
                found: 9,
                current: 1
            }
        ));
    }

    #[test]
    fn multi_step_ladder_applies_every_step_in_order() {
        fn bump_to_2(v: Value) -> Result<Value, MigrationError> {
            Ok(set_version(v, 2))
        }
        fn bump_to_3(v: Value) -> Result<Value, MigrationError> {
            Ok(set_version(v, 3))
        }
        let migrated =
            migrate_to_current(json!({}), 0, 0, 3, &[bump_to_1, bump_to_2, bump_to_3]).unwrap();
        assert_eq!(migrated, json!({"version": 3}));
    }

    #[test]
    fn failing_migration_step_surfaces_typed_and_names_the_source_version() {
        fn always_fails(_v: Value) -> Result<Value, MigrationError> {
            Err(MigrationError("boom".to_string()))
        }
        let err = migrate_to_current(json!({}), 0, 0, 1, &[always_fails]).unwrap_err();
        match err {
            LadderError::Migration { from, source } => {
                assert_eq!(from, 0);
                assert_eq!(source.0, "boom");
            }
            other => panic!("expected Migration, got {other:?}"),
        }
    }
}
