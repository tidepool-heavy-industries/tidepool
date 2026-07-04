use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use tidepool_effect::dispatch::EffectContext;
use tidepool_effect::error::EffectError;
use tidepool_eval::value::Value;
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
}

impl KvHandler {
    pub fn new(path: PathBuf) -> Self {
        let store = if path.exists() {
            match std::fs::read_to_string(&path) {
                Ok(contents) => match serde_json::from_str(&contents) {
                    Ok(map) => map,
                    Err(e) => {
                        tracing::warn!(
                            "KV store at {:?} contains invalid JSON ({}), starting fresh",
                            path,
                            e
                        );
                        HashMap::new()
                    }
                },
                Err(_) => HashMap::new(),
            }
        } else {
            HashMap::new()
        };
        Self {
            store: Arc::new(Mutex::new(store)),
            path,
        }
    }

    fn flush(&self, store: &HashMap<String, serde_json::Value>) {
        if let Some(parent) = self.path.parent() {
            if let Err(e) = std::fs::create_dir_all(parent) {
                tracing::warn!("KV flush: failed to create dir {:?}: {}", parent, e);
                return;
            }
        }
        match serde_json::to_string_pretty(store) {
            Ok(json) => {
                if let Err(e) = std::fs::write(&self.path, json) {
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
    ) -> Result<std::sync::MutexGuard<'_, std::collections::HashMap<String, serde_json::Value>>, EffectError>
    {
        self.store
            .lock()
            .map_err(|e| EffectError::Handler(format!("Mutex poisoned: {}", e)))
    }

    fn kv_get(
        &mut self,
        cx: &EffectContext<'_, CapturedOutput>,
        key: String,
    ) -> Result<tidepool_effect::Response, EffectError> {
        let store = self.locked()?;
        let val: Option<serde_json::Value> = store.get(&key).cloned();
        cx.respond(val)
    }

    fn kv_set(
        &mut self,
        cx: &EffectContext<'_, CapturedOutput>,
        key: String,
        val: Value,
    ) -> Result<tidepool_effect::Response, EffectError> {
        let json_val = tidepool_runtime::value_to_json(&val, cx.table(), 0);
        let mut store = self.locked()?;
        store.insert(key, json_val);
        self.flush(&store);
        cx.respond(())
    }

    fn kv_delete(
        &mut self,
        cx: &EffectContext<'_, CapturedOutput>,
        key: String,
    ) -> Result<tidepool_effect::Response, EffectError> {
        let mut store = self.locked()?;
        store.remove(&key);
        self.flush(&store);
        cx.respond(())
    }

    fn kv_keys(
        &mut self,
        cx: &EffectContext<'_, CapturedOutput>,
    ) -> Result<tidepool_effect::Response, EffectError> {
        let store = self.locked()?;
        let keys: Vec<String> = store.keys().cloned().collect();
        cx.respond(keys)
    }

    fn kv_clear(
        &mut self,
        cx: &EffectContext<'_, CapturedOutput>,
        prefix: String,
    ) -> Result<tidepool_effect::Response, EffectError> {
        let mut store = self.locked()?;
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
        let store = self.locked()?;
        let mut keys: Vec<String> = store
            .keys()
            .filter(|k| k.starts_with(prefix.as_str()))
            .cloned()
            .collect();
        keys.sort();
        cx.respond(keys)
    }

    fn kv_info(
        &mut self,
        cx: &EffectContext<'_, CapturedOutput>,
    ) -> Result<tidepool_effect::Response, EffectError> {
        let store = self.locked()?;
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
    use tidepool_bridge::{FromCore, ToCore};
    use tidepool_effect::dispatch::{DispatchEffect, EffectContext};
    use tidepool_eval::value::Value;
    use tidepool_repr::DataConTable;

    #[test]
    fn test_kv_from_core_keys() {
        let table = full_effect_test_table();
        let con_id = table.get_by_name("KvKeys").unwrap();
        let val = Value::Con(con_id, vec![]);
        let req = KvReq::from_value(&val, &table).unwrap();
        assert!(matches!(req, KvReq::KvKeys()));
    }

    #[test]
    fn test_kv_from_core_get() {
        let table = full_effect_test_table();
        let con_id = table.get_by_name("KvGet").unwrap();
        let key = "mykey".to_string().to_value(&table).unwrap();
        let val = Value::Con(con_id, vec![key]);
        let req = KvReq::from_value(&val, &table).unwrap();
        assert!(matches!(req, KvReq::KvGet(ref k) if k == "mykey"));
    }

    #[test]
    fn test_kv_dispatch_roundtrip_keys() {
        let table = full_effect_test_table();
        let captured = CapturedOutput::new();
        let cx = EffectContext::with_user(&table, &captured);
        let tmp = std::env::temp_dir().join("tidepool_test_kv.json");
        let mut handlers = frunk::hlist![KvHandler::new(tmp)];
        let con_id = table.get_by_name("KvKeys").unwrap();
        let request = Value::Con(con_id, vec![]);
        let result = response_value(handlers.dispatch(0, &request, &cx).unwrap(), &table);
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
        let _ = std::fs::remove_file(&path_a);
        let _ = std::fs::remove_file(&path_b);

        let mut ha = frunk::hlist![KvHandler::new(path_a.clone())];
        let mut hb = frunk::hlist![KvHandler::new(path_b.clone())];

        // Set a key in handler A (session A).
        let set_id = table.get_by_name("KvSet").unwrap();
        let keys_id = table.get_by_name("KvKeys").unwrap();
        let key_val = "isolation-test-key".to_string().to_value(&table).unwrap();
        let lit_val = Value::Lit(tidepool_repr::Literal::LitInt(99));
        let set_req = Value::Con(set_id, vec![key_val, lit_val]);
        ha.dispatch(0, &set_req, &cx).unwrap();

        // kvKeys on handler B (session B) must be empty — no bleed from A.
        let keys_req = Value::Con(keys_id, vec![]);
        let keys_b = response_value(hb.dispatch(0, &keys_req, &cx).unwrap(), &table);
        match &keys_b {
            Value::Con(id, args) => {
                let name = table.name_of(*id).unwrap();
                assert_eq!(
                    name, "[]",
                    "session B should have no keys after session A kvSet; got fields: {args:?}"
                );
            }
            other => panic!("expected empty list (\"[]\"), got {:?}", other),
        }

        let _ = std::fs::remove_file(&path_a);
        let _ = std::fs::remove_file(&path_b);
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
        let _ = std::fs::remove_file(&path);
        let mut h = frunk::hlist![KvHandler::new(path.clone())];

        let set_id = table.get_by_name("KvSet").unwrap();
        let clear_id = table.get_by_name("KvClear").unwrap();
        let keys_id = table.get_by_name("KvKeys").unwrap();

        // Insert two keys under "ns1/" and one under "ns2/".
        for key in &["ns1/alpha", "ns1/beta", "ns2/gamma"] {
            let k = key.to_string().to_value(&table).unwrap();
            let v = Value::Lit(tidepool_repr::Literal::LitInt(1));
            h.dispatch(0, &Value::Con(set_id, vec![k, v]), &cx).unwrap();
        }

        // Clear "ns1/" — expect count = 2.
        // i64 is bridged as I#(LitInt(n)); unbox via the shared shape decoder.
        let prefix = "ns1/".to_string().to_value(&table).unwrap();
        let clear_req = Value::Con(clear_id, vec![prefix]);
        let clear_result = response_value(h.dispatch(0, &clear_req, &cx).unwrap(), &table);
        let deleted_count = tidepool_eval::shapes::unbox_int(&clear_result, &table)
            .unwrap_or_else(|| panic!("expected I#(LitInt) count from kvClear, got {:?}", clear_result));
        assert_eq!(
            deleted_count, 2,
            "kvClear \"ns1/\" should have deleted 2 keys, got {deleted_count}"
        );

        // "ns2/gamma" must still be present.
        let all_keys = response_value(
            h.dispatch(0, &Value::Con(keys_id, vec![]), &cx).unwrap(),
            &table,
        );
        let mut surviving: Vec<String> = Vec::new();
        fn collect_list(v: &Value, table: &DataConTable, out: &mut Vec<String>) {
            use tidepool_bridge::FromCore;
            match v {
                Value::Con(id, fields) => {
                    let name = table.name_of(*id).unwrap();
                    if name == ":" {
                        if let Ok(s) = String::from_value(&fields[0], table) {
                            out.push(s);
                        }
                        collect_list(&fields[1], table, out);
                    }
                }
                _ => {}
            }
        }
        collect_list(&all_keys, &table, &mut surviving);
        assert_eq!(
            surviving,
            vec!["ns2/gamma".to_string()],
            "only ns2/gamma should survive after clearing ns1/; got {:?}",
            surviving
        );

        let _ = std::fs::remove_file(&path);
    }

    /// kvKeysP filters keys by prefix and returns them sorted.
    #[test]
    fn kv_keys_p_filters_by_prefix() {
        let table = full_effect_test_table();
        let captured = CapturedOutput::new();
        let cx = EffectContext::with_user(&table, &captured);

        let pid = std::process::id();
        let path = std::env::temp_dir().join(format!("tidepool_kv_keysp_{pid}.json"));
        let _ = std::fs::remove_file(&path);
        let mut h = frunk::hlist![KvHandler::new(path.clone())];

        let set_id = table.get_by_name("KvSet").unwrap();
        let keysp_id = table.get_by_name("KvKeysP").unwrap();

        for key in &["ns1/b", "ns1/a", "ns2/c"] {
            let k = key.to_string().to_value(&table).unwrap();
            let v = Value::Lit(tidepool_repr::Literal::LitInt(0));
            h.dispatch(0, &Value::Con(set_id, vec![k, v]), &cx).unwrap();
        }

        let prefix = "ns1/".to_string().to_value(&table).unwrap();
        let result = response_value(
            h.dispatch(0, &Value::Con(keysp_id, vec![prefix]), &cx)
                .unwrap(),
            &table,
        );

        // Collect the list into a Vec<String> and verify sorted order.
        let mut keys: Vec<String> = Vec::new();
        fn collect_strs(v: &Value, table: &DataConTable, out: &mut Vec<String>) {
            use tidepool_bridge::FromCore;
            match v {
                Value::Con(id, fields) => {
                    let name = table.name_of(*id).unwrap();
                    if name == ":" {
                        if let Ok(s) = String::from_value(&fields[0], table) {
                            out.push(s);
                        }
                        collect_strs(&fields[1], table, out);
                    }
                }
                _ => {}
            }
        }
        collect_strs(&result, &table, &mut keys);
        assert_eq!(
            keys,
            vec!["ns1/a".to_string(), "ns1/b".to_string()],
            "kvKeysP \"ns1/\" should return [\"ns1/a\", \"ns1/b\"] sorted; got {:?}",
            keys
        );

        let _ = std::fs::remove_file(&path);
    }

    /// kvInfo returns a JSON object with count, sample, and file_size_bytes fields.
    #[test]
    fn kv_info_returns_expected_shape() {
        let table = full_effect_test_table();
        let captured = CapturedOutput::new();
        let cx = EffectContext::with_user(&table, &captured);

        let pid = std::process::id();
        let path = std::env::temp_dir().join(format!("tidepool_kv_info_{pid}.json"));
        let _ = std::fs::remove_file(&path);
        let mut h = frunk::hlist![KvHandler::new(path.clone())];

        let set_id = table.get_by_name("KvSet").unwrap();
        let info_id = table.get_by_name("KvInfo").unwrap();

        for key in &["a", "b", "c"] {
            let k = key.to_string().to_value(&table).unwrap();
            let v = Value::Lit(tidepool_repr::Literal::LitInt(1));
            h.dispatch(0, &Value::Con(set_id, vec![k, v]), &cx).unwrap();
        }

        // kvInfo must not error; the response is a non-null Value.
        let result = response_value(
            h.dispatch(0, &Value::Con(info_id, vec![]), &cx).unwrap(),
            &table,
        );
        // The response is a Haskell-encoded JSON Value. Just verify it's not Null
        // (i.e. the Object constructor was selected, not Null).
        let name = match &result {
            Value::Con(id, _) => table.name_of(*id).unwrap().to_string(),
            other => panic!("expected Con from kvInfo, got {:?}", other),
        };
        assert_ne!(name, "Null", "kvInfo should return an Object, not Null");
        assert_eq!(
            name, "Object",
            "kvInfo should return an Object; got constructor {name:?}"
        );

        let _ = std::fs::remove_file(&path);
    }
}
