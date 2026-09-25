use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

use parking_lot::Mutex;
use tidepool_bridge::HaskellValue;
use tidepool_effect::dispatch::EffectContext;
use tidepool_effect::error::EffectError;
use tidepool_mcp::CapturedOutput;

// ============================================================================
// Tag 1: KV Store
// ============================================================================

// KvReq + DescribeEffect + EffectHandler dispatch are generated from the
// single-source definition; only the handler struct and the per-verb method
// bodies below are hand-written.
tidepool_mcp::kv_effect_def!(crate::effect_glue::effect_rust_projection);

#[derive(Clone)]
pub struct KvHandler {
    store: Arc<Mutex<HashMap<String, serde_json::Value>>>,
    path: PathBuf,
    // A failed load cannot authorize an empty in-memory store to replace the
    // backing file. This state is fixed for the lifetime of the handler.
    load_failed: bool,
}

impl KvHandler {
    pub fn new(path: PathBuf) -> Self {
        let mut load_failed = false;
        let store = match std::fs::read_to_string(&path) {
            Ok(contents) => match serde_json::from_str(&contents) {
                Ok(map) => map,
                Err(error) => {
                    tracing::warn!(
                        "KV store at {:?} contains invalid JSON ({}); refusing to overwrite it",
                        path,
                        error
                    );
                    load_failed = true;
                    HashMap::new()
                }
            },
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => HashMap::new(),
            Err(error) => {
                tracing::warn!(
                    "KV store at {:?} could not be read ({}); refusing to overwrite it",
                    path,
                    error
                );
                load_failed = true;
                HashMap::new()
            }
        };
        Self {
            store: Arc::new(Mutex::new(store)),
            path,
            load_failed,
        }
    }

    fn flush(&self, store: &HashMap<String, serde_json::Value>) {
        if self.load_failed {
            tracing::warn!(
                "KV flush: refusing to write {:?} — the backing file could not be loaded \
                 at startup; flushing now would overwrite it with an incomplete store",
                self.path
            );
            return;
        }
        if let Some(parent) = self.path.parent() {
            if let Err(e) = std::fs::create_dir_all(parent) {
                tracing::warn!("KV flush: failed to create dir {:?}: {}", parent, e);
                return;
            }
        }
        match serde_json::to_string_pretty(store) {
            Ok(json) => {
                if let Err(e) =
                    tidepool_atomic_write::write_best_effort(&self.path, json.as_bytes())
                {
                    tracing::warn!("KV flush: failed to write {:?}: {}", self.path, e);
                }
            }
            Err(e) => {
                tracing::warn!("KV flush: serialization failed: {}", e);
            }
        }
    }
}

impl KvHandler {
    /// Lock the store (shared prelude of every verb).
    fn locked(
        &self,
    ) -> parking_lot::MutexGuard<'_, std::collections::HashMap<String, serde_json::Value>> {
        self.store.lock()
    }

    fn kv_get(
        &mut self,
        cx: &EffectContext<'_, CapturedOutput>,
        key: String,
    ) -> Result<tidepool_effect::Response, EffectError> {
        let store = self.locked();
        let val: Option<serde_json::Value> = store.get(&key).cloned();
        cx.respond(val)
    }

    fn kv_set(
        &mut self,
        cx: &EffectContext<'_, CapturedOutput>,
        key: String,
        val: HaskellValue,
    ) -> Result<tidepool_effect::Response, EffectError> {
        let json_val = tidepool_runtime::value_to_json(&val, cx.table(), 0);
        let mut store = self.locked();
        store.insert(key, json_val);
        self.flush(&store);
        cx.respond(())
    }

    fn kv_delete(
        &mut self,
        cx: &EffectContext<'_, CapturedOutput>,
        key: String,
    ) -> Result<tidepool_effect::Response, EffectError> {
        let mut store = self.locked();
        store.remove(&key);
        self.flush(&store);
        cx.respond(())
    }

    fn kv_keys(
        &mut self,
        cx: &EffectContext<'_, CapturedOutput>,
    ) -> Result<tidepool_effect::Response, EffectError> {
        let store = self.locked();
        let keys: Vec<String> = store.keys().cloned().collect();
        cx.respond(keys)
    }

    fn kv_clear(
        &mut self,
        cx: &EffectContext<'_, CapturedOutput>,
        prefix: String,
    ) -> Result<tidepool_effect::Response, EffectError> {
        let mut store = self.locked();
        let before = store.len();
        if prefix.is_empty() {
            // Empty prefix clears the ENTIRE store. This is intentional and loud
            // in the docstring — callers that want to clear a namespace should pass
            // a non-empty prefix (e.g. "agent/" rather than "").
            store.clear();
        } else {
            store.retain(|k, _| !k.starts_with(prefix.as_str()));
        }
        let deleted = (before - store.len()) as i64;
        self.flush(&store);
        cx.respond(deleted)
    }

    fn kv_keys_p(
        &mut self,
        cx: &EffectContext<'_, CapturedOutput>,
        prefix: String,
    ) -> Result<tidepool_effect::Response, EffectError> {
        let store = self.locked();
        let mut keys: Vec<String> = store
            .keys()
            .filter(|k| k.starts_with(prefix.as_str()))
            .cloned()
            .collect();
        keys.sort();
        cx.respond(keys)
    }

    fn kv_cas(
        &mut self,
        cx: &EffectContext<'_, CapturedOutput>,
        key: String,
        expected: Option<HaskellValue>,
        new: HaskellValue,
    ) -> Result<tidepool_effect::Response, EffectError> {
        // Cross-process compare-and-swap (#330-for-KV). The resident worker is
        // single-threaded, so the only real race is between separate agent
        // processes sharing this backing file — so the compare-and-write runs
        // under an flock on the file's parent dir, and RE-READS the file from
        // disk (authoritative), not the possibly-stale in-memory copy another
        // process may have superseded. On success both disk and the in-memory
        // store are updated; on a mismatch nothing is written and the ACTUAL
        // current value comes back as `Left actual` (conflicts-as-data).
        let expected_json: Option<serde_json::Value> =
            expected.map(|v| tidepool_runtime::value_to_json(&v, cx.table(), 0));
        let new_json = tidepool_runtime::value_to_json(&new, cx.table(), 0);

        let path = self.path.clone();
        let parent = path
            .parent()
            .map(std::path::Path::to_path_buf)
            .unwrap_or_else(|| std::path::PathBuf::from("."));
        std::fs::create_dir_all(&parent).map_err(|e| EffectError::Handler(e.to_string()))?;

        // Under the lock: read disk (authoritative), compare, write if match.
        // Returns (fresh disk map, CAS outcome) — the map is used to refresh the
        // in-memory store on BOTH outcomes, so a caller retrying after a
        // conflict reads the up-to-date value (and can tell absent from null,
        // which the `Left` wire value alone cannot).
        type Outcome = Result<(), Option<serde_json::Value>>;
        let (disk, outcome): (HashMap<String, serde_json::Value>, Outcome) =
            crate::handlers::fs::with_dir_flock(
                &parent,
                || -> Result<(HashMap<String, serde_json::Value>, Outcome), EffectError> {
                    let mut disk: HashMap<String, serde_json::Value> = match std::fs::read(&path) {
                        Ok(bytes) => serde_json::from_slice(&bytes).map_err(|e| {
                            // Exists but unparseable — refuse to clobber it
                            // (matches `flush`'s read-failed refusal).
                            EffectError::Handler(format!(
                                "KV CAS: backing file {path:?} is unreadable JSON ({e}); \
                                 refusing to overwrite"
                            ))
                        })?,
                        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                            HashMap::new()
                        }
                        Err(error) => {
                            return Err(EffectError::Handler(format!(
                                "KV CAS: backing file {path:?} could not be read ({error}); \
                                 refusing to overwrite"
                            )))
                        }
                    };
                    let actual = disk.get(&key).cloned();
                    if actual == expected_json {
                        disk.insert(key.clone(), new_json.clone());
                        let json = serde_json::to_string_pretty(&disk)
                            .map_err(|e| EffectError::Handler(e.to_string()))?;
                        tidepool_atomic_write::write_best_effort(&path, json.as_bytes())
                            .map_err(|e| EffectError::Handler(e.to_string()))?;
                        Ok((disk, Ok(())))
                    } else {
                        Ok((disk, Err(actual)))
                    }
                },
            )
            .map_err(|e| EffectError::Handler(e.to_string()))??;

        // Refresh the in-memory store to the disk state we read under the lock,
        // on success AND conflict, so subsequent in-process reads (incl. a
        // caller's retry `kvGet`) see the committed value.
        *self.locked() = disk;

        match outcome {
            Ok(()) => cx.respond(Ok::<(), serde_json::Value>(())),
            Err(actual) => cx.respond(Err::<(), serde_json::Value>(
                actual.unwrap_or(serde_json::Value::Null),
            )),
        }
    }

    fn kv_info(
        &mut self,
        cx: &EffectContext<'_, CapturedOutput>,
    ) -> Result<tidepool_effect::Response, EffectError> {
        let store = self.locked();
        let count = store.len() as i64;
        let mut sample: Vec<String> = store.keys().take(10).cloned().collect();
        sample.sort();
        let file_size = std::fs::metadata(&self.path)
            .map(|m| m.len() as i64)
            .unwrap_or(0);
        let info = serde_json::json!({
            "count": count,
            "sample": sample,
            "file_size_bytes": file_size
        });
        cx.respond(info)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::*;
    use tidepool_bridge::HaskellValue;
    use tidepool_bridge::ToHaskell;
    use tidepool_effect::dispatch::{DispatchEffect, EffectContext};
    use tidepool_repr::DataConTable;

    #[test]
    fn test_kv_dispatch_roundtrip_keys() {
        let table = full_effect_test_table();
        let captured = CapturedOutput::new();
        let cx = EffectContext::with_user(&table, &captured);
        let tmp = std::env::temp_dir().join("tidepool_test_kv.json");
        let mut handlers = frunk::hlist![KvHandler::new(tmp)];
        let con_id = table.get_by_name("KvKeys").unwrap();
        let request = HaskellValue::Con(con_id, vec![]);
        let result = response_value(expect_handled(handlers.dispatch(&request, &cx)), &table);
        assert_is_haskell_list(&result, &table);
    }

    /// Two `KvHandler` instances backed by different paths must not share keys.
    /// This is the unit-level guard for per-session KV isolation (no live extract needed).
    #[test]
    fn kv_handlers_with_different_paths_are_isolated() {
        let table = full_effect_test_table();
        let captured = CapturedOutput::new();
        let cx = EffectContext::with_user(&table, &captured);

        let pid = std::process::id();
        let path_a = std::env::temp_dir().join(format!("tidepool_kv_iso_a_{pid}.json"));
        let path_b = std::env::temp_dir().join(format!("tidepool_kv_iso_b_{pid}.json"));
        std::fs::remove_file(&path_a).ok();
        std::fs::remove_file(&path_b).ok();

        let mut ha = frunk::hlist![KvHandler::new(path_a.clone())];
        let mut hb = frunk::hlist![KvHandler::new(path_b.clone())];

        // Set a key in handler A (session A).
        let set_id = table.get_by_name("KvSet").unwrap();
        let keys_id = table.get_by_name("KvKeys").unwrap();
        let key_val = "isolation-test-key".to_string().to_value(&table).unwrap();
        let lit_val = HaskellValue::Lit(tidepool_repr::Literal::LitInt(99));
        let set_req = HaskellValue::Con(set_id, vec![key_val, lit_val]);
        expect_handled(ha.dispatch(&set_req, &cx));

        // kvKeys on handler B (session B) must be empty — no bleed from A.
        let keys_req = HaskellValue::Con(keys_id, vec![]);
        let keys_b = response_value(expect_handled(hb.dispatch(&keys_req, &cx)), &table);
        match &keys_b {
            HaskellValue::Con(id, args) => {
                let name = table.name_of(*id).unwrap();
                assert_eq!(
                    name, "[]",
                    "session B should have no keys after session A kvSet; got fields: {args:?}"
                );
            }
            other => panic!("expected empty list (\"[]\"), got {:?}", other),
        }

        std::fs::remove_file(&path_a).ok();
        std::fs::remove_file(&path_b).ok();
    }

    /// kvClear with a prefix deletes only keys under that prefix; the other
    /// namespace survives intact, and the returned count is exact.
    #[test]
    fn kv_clear_prefix_deletes_only_matching_keys() {
        let table = full_effect_test_table();
        let captured = CapturedOutput::new();
        let cx = EffectContext::with_user(&table, &captured);

        let pid = std::process::id();
        let path = std::env::temp_dir().join(format!("tidepool_kv_clear_{pid}.json"));
        std::fs::remove_file(&path).ok();
        let mut h = frunk::hlist![KvHandler::new(path.clone())];

        let set_id = table.get_by_name("KvSet").unwrap();
        let clear_id = table.get_by_name("KvClear").unwrap();
        let keys_id = table.get_by_name("KvKeys").unwrap();

        // Insert two keys under "ns1/" and one under "ns2/".
        for key in &["ns1/alpha", "ns1/beta", "ns2/gamma"] {
            let k = key.to_string().to_value(&table).unwrap();
            let v = HaskellValue::Lit(tidepool_repr::Literal::LitInt(1));
            expect_handled(h.dispatch(&HaskellValue::Con(set_id, vec![k, v]), &cx));
        }

        // Clear "ns1/" — expect count = 2.
        // i64 is bridged as I#(LitInt(n)); unbox via the shared shape decoder.
        let prefix = "ns1/".to_string().to_value(&table).unwrap();
        let clear_req = HaskellValue::Con(clear_id, vec![prefix]);
        let clear_result = response_value(expect_handled(h.dispatch(&clear_req, &cx)), &table);
        let deleted_count = tidepool_bridge::shapes::unbox_int(&clear_result, &table)
            .unwrap_or_else(|| {
                panic!(
                    "expected I#(LitInt) count from kvClear, got {:?}",
                    clear_result
                )
            });
        assert_eq!(
            deleted_count, 2,
            "kvClear \"ns1/\" should have deleted 2 keys, got {deleted_count}"
        );

        // "ns2/gamma" must still be present.
        let all_keys = response_value(
            expect_handled(h.dispatch(&HaskellValue::Con(keys_id, vec![]), &cx)),
            &table,
        );
        let mut surviving: Vec<String> = Vec::new();
        fn collect_list(v: &HaskellValue, table: &DataConTable, out: &mut Vec<String>) {
            use tidepool_bridge::FromHaskell;
            if let HaskellValue::Con(id, fields) = v {
                let name = table.name_of(*id).unwrap();
                if name == ":" {
                    if let Ok(s) = String::from_value(&fields[0], table) {
                        out.push(s);
                    }
                    collect_list(&fields[1], table, out);
                }
            }
        }
        collect_list(&all_keys, &table, &mut surviving);
        assert_eq!(
            surviving,
            vec!["ns2/gamma".to_string()],
            "only ns2/gamma should survive after clearing ns1/; got {:?}",
            surviving
        );

        std::fs::remove_file(&path).ok();
    }

    /// kvKeysP filters keys by prefix and returns them sorted.
    #[test]
    fn kv_keys_p_filters_by_prefix() {
        let table = full_effect_test_table();
        let captured = CapturedOutput::new();
        let cx = EffectContext::with_user(&table, &captured);

        let pid = std::process::id();
        let path = std::env::temp_dir().join(format!("tidepool_kv_keysp_{pid}.json"));
        std::fs::remove_file(&path).ok();
        let mut h = frunk::hlist![KvHandler::new(path.clone())];

        let set_id = table.get_by_name("KvSet").unwrap();
        let keysp_id = table.get_by_name("KvKeysP").unwrap();

        for key in &["ns1/b", "ns1/a", "ns2/c"] {
            let k = key.to_string().to_value(&table).unwrap();
            let v = HaskellValue::Lit(tidepool_repr::Literal::LitInt(0));
            expect_handled(h.dispatch(&HaskellValue::Con(set_id, vec![k, v]), &cx));
        }

        let prefix = "ns1/".to_string().to_value(&table).unwrap();
        let result = response_value(
            expect_handled(h.dispatch(&HaskellValue::Con(keysp_id, vec![prefix]), &cx)),
            &table,
        );

        // Collect the list into a Vec<String> and verify sorted order.
        let mut keys: Vec<String> = Vec::new();
        fn collect_strs(v: &HaskellValue, table: &DataConTable, out: &mut Vec<String>) {
            use tidepool_bridge::FromHaskell;
            if let HaskellValue::Con(id, fields) = v {
                let name = table.name_of(*id).unwrap();
                if name == ":" {
                    if let Ok(s) = String::from_value(&fields[0], table) {
                        out.push(s);
                    }
                    collect_strs(&fields[1], table, out);
                }
            }
        }
        collect_strs(&result, &table, &mut keys);
        assert_eq!(
            keys,
            vec!["ns1/a".to_string(), "ns1/b".to_string()],
            "kvKeysP \"ns1/\" should return [\"ns1/a\", \"ns1/b\"] sorted; got {:?}",
            keys
        );

        std::fs::remove_file(&path).ok();
    }

    /// A backing file that EXISTS but can't be read at startup (transient
    /// EACCES) must not have its contents wiped by the next flush: `new`
    /// starts fresh in-memory (as before) but `flush` must refuse to write
    /// while unreadable, so the on-disk data survives for a later restart.
    #[cfg(unix)]
    #[test]
    fn kv_refuses_to_flush_over_a_file_it_could_not_read() {
        use std::os::unix::fs::PermissionsExt;

        let table = full_effect_test_table();
        let captured = CapturedOutput::new();
        let cx = EffectContext::with_user(&table, &captured);

        let pid = std::process::id();
        let path = std::env::temp_dir().join(format!("tidepool_kv_unreadable_{pid}.json"));
        std::fs::remove_file(&path).ok();
        let original = r#"{"marker":"original"}"#;
        std::fs::write(&path, original).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o000)).unwrap();

        // Skip in environments (e.g. root) where permission bits don't gate reads.
        if std::fs::read_to_string(&path).is_ok() {
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
            std::fs::remove_file(&path).ok();
            eprintln!("skipping: read succeeded despite 0o000 (likely running as root)");
            return;
        }

        let mut h = frunk::hlist![KvHandler::new(path.clone())];
        // Restore read/write so we can inspect the file afterward; flush must
        // still refuse to write to it (the refusal is sticky for this handler).
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();

        let set_id = table.get_by_name("KvSet").unwrap();
        let k = "x".to_string().to_value(&table).unwrap();
        let v = HaskellValue::Lit(tidepool_repr::Literal::LitInt(1));
        expect_handled(h.dispatch(&HaskellValue::Con(set_id, vec![k, v]), &cx));

        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            original,
            "flush must refuse to overwrite a file that failed to read at startup"
        );

        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn kv_refuses_to_replace_malformed_json_with_an_empty_startup_store() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("kv.json");
        let original = b"{ incomplete";
        std::fs::write(&path, original).unwrap();

        let handler = KvHandler::new(path.clone());
        handler.flush(&HashMap::from([("new".into(), serde_json::json!(1))]));
        handler.clone().flush(&HashMap::new());

        assert_eq!(std::fs::read(path).unwrap(), original);
    }

    #[test]
    fn kv_cas_refuses_a_backing_path_that_cannot_be_read() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("kv.json");
        std::fs::create_dir(&path).unwrap();
        let table = full_effect_test_table();
        let captured = CapturedOutput::new();
        let cx = EffectContext::with_user(&table, &captured);

        let mut handler = KvHandler::new(path.clone());
        let result = handler.kv_cas(
            &cx,
            "key".into(),
            None,
            HaskellValue::Lit(tidepool_repr::Literal::LitInt(1)),
        );

        let error = match result {
            Err(error) => error,
            Ok(_) => panic!("a directory is not a readable KV file"),
        };
        assert!(error.to_string().contains("could not be read"), "{error}");
        assert!(path.is_dir());
    }

    /// Cross-process compare-and-swap: two handlers over the SAME backing file
    /// (standing in for two agent processes) both CAS the same key from
    /// "expected absent". The first commits; the second, re-reading disk under
    /// the flock, sees the now-present value and gets `Left actual` — no lost
    /// update, the conflict comes back as data. This is the KV analogue of the
    /// FsWriteCas #330 guarantee the kvIncr/kvModify/kvAppend loops rely on.
    #[test]
    fn kv_cas_two_handlers_same_file_no_lost_update() {
        let table = full_effect_test_table();
        let captured = CapturedOutput::new();
        let cx = EffectContext::with_user(&table, &captured);

        let pid = std::process::id();
        let path = std::env::temp_dir().join(format!("tidepool_kv_cas_{pid}.json"));
        std::fs::remove_file(&path).ok();

        let mut ha = frunk::hlist![KvHandler::new(path.clone())];
        let mut hb = frunk::hlist![KvHandler::new(path.clone())];

        let cas_id = table.get_by_name("KvCas").unwrap();
        let nothing = HaskellValue::Con(table.get_by_name("Nothing").unwrap(), vec![]);
        let key = "shared/counter".to_string().to_value(&table).unwrap();

        // Handler A: CAS from absent -> 1. Commits (Right ()).
        let a_new = HaskellValue::Lit(tidepool_repr::Literal::LitInt(1));
        let a_req = HaskellValue::Con(cas_id, vec![key.clone(), nothing.clone(), a_new]);
        let a_res = response_value(expect_handled(ha.dispatch(&a_req, &cx)), &table);
        let a_name = match &a_res {
            HaskellValue::Con(id, _) => table.name_of(*id).unwrap(),
            other => panic!("expected Con from kvCas, got {other:?}"),
        };
        assert_eq!(a_name, "Right", "handler A's CAS from absent should commit");

        // Handler B: CAS from absent -> 2, but the key now exists (A wrote 1).
        // Must be Left (conflict), NOT a silent clobber.
        let b_new = HaskellValue::Lit(tidepool_repr::Literal::LitInt(2));
        let b_req = HaskellValue::Con(cas_id, vec![key, nothing, b_new]);
        let b_res = response_value(expect_handled(hb.dispatch(&b_req, &cx)), &table);
        let b_name = match &b_res {
            HaskellValue::Con(id, _) => table.name_of(*id).unwrap(),
            other => panic!("expected Con from kvCas, got {other:?}"),
        };
        assert_eq!(
            b_name, "Left",
            "handler B's stale CAS must conflict (Left), not clobber A's write"
        );

        // The file must hold A's value (1), not B's (2).
        let on_disk: std::collections::HashMap<String, serde_json::Value> =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(
            on_disk.get("shared/counter"),
            Some(&serde_json::json!(1)),
            "the committed value must survive; B must not have clobbered it"
        );

        std::fs::remove_file(&path).ok();
    }

    /// kvInfo returns a JSON object with count, sample, and file_size_bytes fields.
    #[test]
    fn kv_info_returns_expected_shape() {
        let table = full_effect_test_table();
        let captured = CapturedOutput::new();
        let cx = EffectContext::with_user(&table, &captured);

        let pid = std::process::id();
        let path = std::env::temp_dir().join(format!("tidepool_kv_info_{pid}.json"));
        std::fs::remove_file(&path).ok();
        let mut h = frunk::hlist![KvHandler::new(path.clone())];

        let set_id = table.get_by_name("KvSet").unwrap();
        let info_id = table.get_by_name("KvInfo").unwrap();

        for key in &["a", "b", "c"] {
            let k = key.to_string().to_value(&table).unwrap();
            let v = HaskellValue::Lit(tidepool_repr::Literal::LitInt(1));
            expect_handled(h.dispatch(&HaskellValue::Con(set_id, vec![k, v]), &cx));
        }

        // kvInfo must not error; the response is a non-null HaskellValue.
        let result = response_value(
            expect_handled(h.dispatch(&HaskellValue::Con(info_id, vec![]), &cx)),
            &table,
        );
        // The response is a Haskell-encoded JSON HaskellValue. Just verify it's not Null
        // (i.e. the Object constructor was selected, not Null).
        let name = match &result {
            HaskellValue::Con(id, _) => table.name_of(*id).unwrap().to_string(),
            other => panic!("expected Con from kvInfo, got {:?}", other),
        };
        assert_ne!(name, "Null", "kvInfo should return an Object, not Null");
        assert_eq!(
            name, "Object",
            "kvInfo should return an Object; got constructor {name:?}"
        );

        std::fs::remove_file(&path).ok();
    }
}
