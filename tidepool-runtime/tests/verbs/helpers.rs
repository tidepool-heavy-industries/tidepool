//! Shared setup for the verbs/ suite: an in-memory filesystem answering the Fs
//! effect, counting writes so each verb file's atomic/no-partial-mutation
//! claims are checkable.
use std::collections::HashMap;
use tidepool_bridge::FromCore;
use tidepool_bridge_effects::FileMeta;
use tidepool_effect::DispatchEffect;
use tidepool_eval::value::Value;

#[derive(Default)]
pub struct FsDispatcher {
    pub files: HashMap<String, String>,
    pub writes: usize,
}

impl DispatchEffect<()> for FsDispatcher {
    fn dispatch(
        &mut self,
        _tag: u64,
        request: &Value,
        cx: &tidepool_effect::EffectContext<'_, ()>,
    ) -> Result<tidepool_effect::Response, tidepool_effect::error::EffectError> {
        let table = cx.table();
        if let Value::Con(con_id, fields) = request {
            match table.name_of(*con_id) {
                Some("FsMetadata") => {
                    let path = String::from_value(&fields[0], table).unwrap();
                    let meta = self.files.get(&path).map(|c| FileMeta {
                        size: c.len() as i64,
                        is_file: true,
                        is_dir: false,
                    });
                    return cx.respond(meta);
                }
                Some("FsRead") => {
                    let path = String::from_value(&fields[0], table).unwrap();
                    let content = self.files.get(&path).cloned().unwrap_or_default();
                    return cx.respond(Ok::<String, String>(content));
                }
                Some("FsWrite") => {
                    let path = String::from_value(&fields[0], table).unwrap();
                    let content = String::from_value(&fields[1], table).unwrap();
                    self.files.insert(path, content);
                    self.writes += 1;
                    return cx.respond(Ok::<(), String>(()));
                }
                _ => {}
            }
        }
        cx.respond(())
    }
}

pub fn preload(pairs: &[(&str, &str)]) -> FsDispatcher {
    let mut d = FsDispatcher::default();
    for (k, v) in pairs {
        d.files.insert((*k).to_string(), (*v).to_string());
    }
    d
}
