//! Version stamp for [`super::journal::JournalHandler`]'s per-segment wire
//! contract, on `tidepool_repr::version_ladder` (the one migration-ladder
//! mechanism for durable, non-reproducible persistence artifacts). Stamped
//! per segment because each segment has an independent append-only wire
//! format. Legacy migrations are retained as compatibility fixtures.

/// This build's current per-segment journal version.
pub const CURRENT: u32 = 2;
#[cfg(test)]
mod legacy {
    use serde_json::Value;
    use tidepool_repr::version_ladder::{set_version, Migration, MigrationError};

    /// The oldest segment version retained in the migration table. Version `0`
    /// has no header line and predates the version-stamp scheme.
    pub const FLOOR: u32 = 0;

    fn entry_v0_to_v1(v: Value) -> Result<Value, MigrationError> {
        // Purely additive: versions `0` and `1` retain the same entry fields, this
        // step only makes the version explicit.
        Ok(set_version(v, 1))
    }

    fn entry_v1_to_v2(mut v: Value) -> Result<Value, MigrationError> {
        // Entries written before provenance timestamps existed have no honest
        // wall-clock value to recover; `0` records an unknown timestamp.
        if let Value::Object(map) = &mut v {
            map.entry("ts".to_string())
                .or_insert_with(|| Value::from(0));
        }
        Ok(set_version(v, 2))
    }

    /// Indexed from [`FLOOR`].
    pub const MIGRATIONS: &[Migration] = &[entry_v0_to_v1, entry_v1_to_v2];
}

#[cfg(test)]
mod tests {
    use super::{legacy::*, CURRENT};
    use serde_json::json;

    #[test]
    fn legacy_entry_migrations_preserve_fields_and_add_unknown_timestamp() {
        let legacy = json!({"seq": 3, "kind": "split", "key": "branch/a", "payload": 1});
        let migrated = tidepool_repr::version_ladder::migrate_to_current(
            legacy, 0, FLOOR, CURRENT, MIGRATIONS,
        )
        .unwrap();
        assert_eq!(migrated["seq"], 3);
        assert_eq!(migrated["kind"], "split");
        assert_eq!(migrated["key"], "branch/a");
        assert_eq!(migrated["payload"], 1);
        assert_eq!(migrated["ts"], 0);
        assert_eq!(migrated["version"], 2);
    }
}
