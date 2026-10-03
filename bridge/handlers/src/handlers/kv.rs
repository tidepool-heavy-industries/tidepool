use std::collections::HashMap;
use std::path::{Path, PathBuf};

tidepool_mcp::kv_effect_def!(crate::effect_glue::effect_rust_projection);

#[derive(Clone)]
pub struct KvHandler {
    path: PathBuf,
}

impl KvHandler {
    pub fn new(path: PathBuf) -> Self {
        Self { path }
    }

    fn with_store<T>(
        &self,
        update: impl FnOnce(&mut HashMap<String, serde_json::Value>) -> Result<(T, bool), KvError>,
    ) -> Result<T, KvError> {
        let parent = self
            .path
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
            .unwrap_or_else(|| Path::new("."));
        tidepool_atomic_write::create_dir_all_durable(parent)
            .map_err(|e| KvError::KvIo(e.to_string()))?;
        crate::handlers::fs::with_dir_flock(parent, || {
            let mut store = match std::fs::read(&self.path) {
                Ok(bytes) => serde_json::from_slice(&bytes)
                    .map_err(|e| KvError::KvCorrupt(format!("{}: {e}", self.path.display())))?,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => HashMap::new(),
                Err(error) => {
                    return Err(KvError::KvIo(format!("{}: {error}", self.path.display())));
                }
            };
            let (result, changed) = update(&mut store)?;
            if changed {
                let bytes =
                    serde_json::to_vec_pretty(&store).map_err(|e| KvError::KvIo(e.to_string()))?;
                let staged = tidepool_atomic_write::stage_durable(&self.path, &bytes)
                    .map_err(|e| KvError::KvIo(e.to_string()))?;
                staged.publish().map_err(|error| match error {
                    tidepool_atomic_write::PublishError::BeforeRename(e) => {
                        KvError::KvIo(e.to_string())
                    }
                    tidepool_atomic_write::PublishError::PublishedDurabilityUnconfirmed {
                        source,
                        ..
                    } => KvError::KvDurabilityUnknown(source.to_string()),
                })?;
            }
            Ok(result)
        })
        .map_err(|e| KvError::KvIo(e.to_string()))?
    }
}

impl KvHandler {
    fn kv_get(&mut self, key: String) -> Result<Option<serde_json::Value>, KvError> {
        self.with_store(|store| Ok((store.get(&key).cloned(), false)))
    }

    fn kv_set(&mut self, key: String, value: crate::effect_glue::JsonArg) -> Result<(), KvError> {
        self.with_store(|store| {
            store.insert(key, value.0);
            Ok(((), true))
        })
    }

    fn kv_delete(&mut self, key: String) -> Result<(), KvError> {
        self.with_store(|store| {
            let changed = store.remove(&key).is_some();
            Ok(((), changed))
        })
    }

    fn kv_keys(&mut self) -> Result<Vec<String>, KvError> {
        self.with_store(|store| Ok((store.keys().cloned().collect(), false)))
    }

    fn kv_clear(&mut self, prefix: String) -> Result<i64, KvError> {
        self.with_store(|store| {
            let before = store.len();
            if prefix.is_empty() {
                store.clear();
            } else {
                store.retain(|key, _| !key.starts_with(&prefix));
            }
            let deleted = (before - store.len()) as i64;
            Ok((deleted, deleted != 0))
        })
    }

    fn kv_keys_p(&mut self, prefix: String) -> Result<Vec<String>, KvError> {
        self.with_store(|store| {
            let mut keys = store
                .keys()
                .filter(|key| key.starts_with(&prefix))
                .cloned()
                .collect::<Vec<_>>();
            keys.sort();
            Ok((keys, false))
        })
    }

    fn kv_info(&mut self) -> Result<serde_json::Value, KvError> {
        self.with_store(|store| {
            let count = store.len() as i64;
            let mut sample = store.keys().take(10).cloned().collect::<Vec<_>>();
            sample.sort();
            let file_size = match std::fs::metadata(&self.path) {
                Ok(metadata) => metadata.len() as i64,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => 0,
                Err(error) => return Err(KvError::KvIo(error.to_string())),
            };
            Ok((
                serde_json::json!({"count": count, "sample": sample, "file_size_bytes": file_size}),
                false,
            ))
        })
    }

    fn kv_cas(
        &mut self,
        key: String,
        expected: Option<crate::effect_glue::JsonArg>,
        new: crate::effect_glue::JsonArg,
    ) -> Result<Result<(), Option<serde_json::Value>>, KvError> {
        self.with_store(|store| {
            let actual = store.get(&key).cloned();
            if actual == expected.map(|value| value.0) {
                store.insert(key, new.0);
                Ok((Ok(()), true))
            } else {
                Ok((Err(actual), false))
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::full_effect_test_table;
    use tempfile::tempdir;
    use tidepool_bridge::HaskellValue;
    use tidepool_effect::dispatch::{EffectContext, EffectHandler};
    use tidepool_mcp::CapturedOutput;

    #[test]
    fn every_handler_uses_the_current_disk_state() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("kv.json");
        let mut first = KvHandler::new(path.clone());
        let mut second = KvHandler::new(path.clone());
        first
            .kv_set(
                "a".into(),
                crate::effect_glue::JsonArg(serde_json::json!(1)),
            )
            .unwrap();
        second
            .kv_set(
                "b".into(),
                crate::effect_glue::JsonArg(serde_json::json!(2)),
            )
            .unwrap();
        assert_eq!(first.kv_keys().unwrap().len(), 2);
        assert_eq!(
            first.kv_get("b".into()).unwrap(),
            Some(serde_json::json!(2))
        );
        let mut isolated = KvHandler::new(dir.path().join("isolated.json"));
        assert!(isolated.kv_get("a".into()).unwrap().is_none());
        assert_eq!(
            first
                .kv_cas(
                    "missing".into(),
                    Some(crate::effect_glue::JsonArg(serde_json::Value::Null)),
                    crate::effect_glue::JsonArg(serde_json::json!(3))
                )
                .unwrap(),
            Err(None)
        );
        first
            .kv_set(
                "null".into(),
                crate::effect_glue::JsonArg(serde_json::Value::Null),
            )
            .unwrap();
        assert_eq!(
            first
                .kv_cas(
                    "null".into(),
                    None,
                    crate::effect_glue::JsonArg(serde_json::json!(4))
                )
                .unwrap(),
            Err(Some(serde_json::Value::Null))
        );
    }

    #[test]
    fn malformed_store_is_a_typed_error_and_is_preserved() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("kv.json");
        std::fs::write(&path, b"{").unwrap();
        let mut handler = KvHandler::new(path.clone());
        assert!(matches!(handler.kv_keys(), Err(KvError::KvCorrupt(_))));
        assert_eq!(std::fs::read(path).unwrap(), b"{");
    }

    #[test]
    fn mutation_publishes_a_complete_atomic_json_file() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("kv.json");
        let mut handler = KvHandler::new(path.clone());
        handler
            .kv_set(
                "key".into(),
                crate::effect_glue::JsonArg(serde_json::json!({"value": 7})),
            )
            .unwrap();
        let stored: HashMap<String, serde_json::Value> =
            serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        assert_eq!(stored.get("key"), Some(&serde_json::json!({"value": 7})));
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 1);
    }

    #[test]
    fn prefix_listing_clearing_and_info_use_the_persisted_map() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("kv.json");
        let mut handler = KvHandler::new(path);
        for key in ["ns/a", "ns/b", "other"] {
            handler
                .kv_set(
                    key.into(),
                    crate::effect_glue::JsonArg(serde_json::json!(key)),
                )
                .unwrap();
        }
        assert_eq!(
            handler.kv_keys_p("ns/".into()).unwrap(),
            vec!["ns/a".to_owned(), "ns/b".to_owned()]
        );
        let info = handler.kv_info().unwrap();
        assert_eq!(info["count"], 3);
        assert_eq!(info["sample"], serde_json::json!(["ns/a", "ns/b", "other"]));
        assert!(info["file_size_bytes"].as_i64().unwrap() > 0);
        assert_eq!(handler.kv_clear("ns/".into()).unwrap(), 2);
        assert_eq!(handler.kv_keys().unwrap(), vec!["other".to_owned()]);
        assert_eq!(handler.kv_delete("other".into()).unwrap(), ());
        assert_eq!(handler.kv_info().unwrap()["count"], 0);
    }

    #[test]
    fn typed_storage_failure_is_dispatched_as_left() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("kv.json");
        std::fs::write(&path, b"{").unwrap();
        let table = full_effect_test_table();
        let captured = CapturedOutput::new();
        let cx = EffectContext::with_user(&table, &captured);
        let mut handler = KvHandler::new(path);
        let response =
            EffectHandler::handle(&mut handler, KvReq::KvGet("key".into()), &cx).unwrap();
        let value = response.to_value(&table).unwrap();
        let HaskellValue::Con(id, _) = value else {
            panic!("expected Either.Left, got {value:?}")
        };
        assert_eq!(table.get(id).unwrap().name, "Left");
    }

    #[test]
    fn successful_read_is_dispatched_as_right() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("kv.json");
        std::fs::write(&path, b"{}").unwrap();
        let table = full_effect_test_table();
        let captured = CapturedOutput::new();
        let cx = EffectContext::with_user(&table, &captured);
        let mut handler = KvHandler::new(path);
        let response = EffectHandler::handle(&mut handler, KvReq::KvKeys(), &cx).unwrap();
        let value = response.to_value(&table).unwrap();
        let HaskellValue::Con(id, _) = value else {
            panic!("expected Either.Right, got {value:?}")
        };
        assert_eq!(table.get(id).unwrap().name, "Right");
    }

    #[test]
    fn missing_file_is_empty_and_directory_failure_is_typed() {
        let dir = tempdir().unwrap();
        let mut handler = KvHandler::new(dir.path().join("missing/kv.json"));
        assert!(handler.kv_get("x".into()).unwrap().is_none());
        let blocking_file = dir.path().join("file");
        std::fs::write(&blocking_file, b"x").unwrap();
        let mut bad = KvHandler::new(blocking_file.join("kv.json"));
        assert!(matches!(
            bad.kv_set(
                "x".into(),
                crate::effect_glue::JsonArg(serde_json::json!(1))
            ),
            Err(KvError::KvIo(_))
        ));
        assert!(matches!(
            bad.kv_cas(
                "x".into(),
                None,
                crate::effect_glue::JsonArg(serde_json::json!(1))
            ),
            Err(KvError::KvIo(_))
        ));

        let directory = dir.path().join("directory");
        std::fs::create_dir(&directory).unwrap();
        let mut unreadable = KvHandler::new(directory);
        assert!(matches!(
            unreadable.kv_get("x".into()),
            Err(KvError::KvIo(_))
        ));
    }

    #[test]
    fn concurrent_cas_and_set_delete_preserve_fresh_updates() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("kv.json");
        let mut handler = KvHandler::new(path.clone());
        handler
            .kv_set(
                "x".into(),
                crate::effect_glue::JsonArg(serde_json::json!(0)),
            )
            .unwrap();
        let mut threads = (0..6)
            .map(|_| {
                let path = path.clone();
                std::thread::spawn(move || {
                    let mut handler = KvHandler::new(path);
                    for _ in 0..15 {
                        loop {
                            let old = handler.kv_get("x".into()).unwrap();
                            let value = old
                                .as_ref()
                                .and_then(serde_json::Value::as_i64)
                                .unwrap_or_default()
                                + 1;
                            match handler
                                .kv_cas(
                                    "x".into(),
                                    old.map(crate::effect_glue::JsonArg),
                                    crate::effect_glue::JsonArg(serde_json::json!(value)),
                                )
                                .unwrap()
                            {
                                Ok(()) => break,
                                Err(_) => continue,
                            }
                        }
                    }
                })
            })
            .collect::<Vec<_>>();
        let mutation_path = path.clone();
        threads.push(std::thread::spawn(move || {
            let mut handler = KvHandler::new(mutation_path);
            for n in 0..30 {
                handler
                    .kv_set(
                        "other".into(),
                        crate::effect_glue::JsonArg(serde_json::json!(n)),
                    )
                    .unwrap();
                handler.kv_delete("other".into()).unwrap();
            }
        }));
        for thread in threads {
            thread.join().unwrap();
        }
        assert_eq!(
            handler.kv_get("x".into()).unwrap(),
            Some(serde_json::json!(90))
        );
        let bytes = std::fs::read(path).unwrap();
        let _: HashMap<String, serde_json::Value> = serde_json::from_slice(&bytes).unwrap();
    }
}
