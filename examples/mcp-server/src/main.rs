use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use tidepool_bridge_derive::FromCore;
use tidepool_effect::dispatch::{EffectContext, EffectHandler};
use tidepool_effect::error::EffectError;
use tidepool_eval::value::Value;
use tidepool_mcp::TidepoolMcpServer;

// === Tag 0: Console ===
#[derive(FromCore)]
enum ConsoleReq {
    #[core(name = "Print")]
    Print(String),
}

#[derive(Clone)]
struct ConsoleHandler;

impl EffectHandler<()> for ConsoleHandler {
    type Request = ConsoleReq;
    fn handle(&mut self, req: ConsoleReq, cx: &EffectContext<'_, ()>) -> Result<Value, EffectError> {
        match req {
            ConsoleReq::Print(s) => {
                eprintln!("[console] {}", s);
                cx.respond(())
            }
        }
    }
}

// === Tag 1: KV Store ===
#[derive(FromCore)]
enum KvReq {
    #[core(name = "KvGet")]
    Get(String),
    #[core(name = "KvSet")]
    Set(String, Value),
    #[core(name = "KvDelete")]
    Delete(String),
    #[core(name = "KvKeys")]
    Keys,
}

#[derive(Clone)]
struct KvHandler {
    store: Arc<Mutex<HashMap<String, Value>>>,
}

impl KvHandler {
    fn new() -> Self {
        Self {
            store: Arc::new(Mutex::new(HashMap::new())),
        }
    }
}

impl EffectHandler<()> for KvHandler {
    type Request = KvReq;
    fn handle(&mut self, req: KvReq, cx: &EffectContext<'_, ()>) -> Result<Value, EffectError> {
        let mut store = self.store.lock().unwrap();
        match req {
            KvReq::Get(key) => {
                let val = store.get(&key).cloned();
                cx.respond(val)
            }
            KvReq::Set(key, val) => {
                store.insert(key, val);
                cx.respond(())
            }
            KvReq::Delete(key) => {
                store.remove(&key);
                cx.respond(())
            }
            KvReq::Keys => {
                let keys: Vec<String> = store.keys().cloned().collect();
                cx.respond(keys)
            }
        }
    }
}

// === Tag 2: File I/O (sandboxed to working directory) ===
#[derive(FromCore)]
enum FsReq {
    #[core(name = "FsRead")]
    Read(String),
    #[core(name = "FsWrite")]
    Write(String, String),
}

#[derive(Clone)]
struct FsHandler {
    root: PathBuf,
}

impl FsHandler {
    fn new(root: PathBuf) -> Self {
        Self { root }
    }

    fn resolve(&self, path: &str) -> Result<PathBuf, EffectError> {
        let resolved = self.root.join(path);
        // Sandbox: ensure resolved path is under root
        let canonical_root = self
            .root
            .canonicalize()
            .map_err(|e| EffectError::Handler(e.to_string()))?;
        // For new files that don't exist yet, check parent
        let check_path = if resolved.exists() {
            resolved
                .canonicalize()
                .map_err(|e| EffectError::Handler(e.to_string()))?
        } else {
            let parent = resolved
                .parent()
                .ok_or_else(|| EffectError::Handler("no parent dir".into()))?;
            let canonical_parent = parent
                .canonicalize()
                .map_err(|e| EffectError::Handler(e.to_string()))?;
            canonical_parent.join(
                resolved
                    .file_name()
                    .ok_or_else(|| EffectError::Handler("invalid filename".into()))?,
            )
        };
        if !check_path.starts_with(&canonical_root) {
            return Err(EffectError::Handler(format!(
                "path escape: {} is outside sandbox",
                path
            )));
        }
        Ok(resolved)
    }
}

impl EffectHandler<()> for FsHandler {
    type Request = FsReq;
    fn handle(&mut self, req: FsReq, cx: &EffectContext<'_, ()>) -> Result<Value, EffectError> {
        match req {
            FsReq::Read(path) => {
                let resolved = self.resolve(&path)?;
                let contents = std::fs::read_to_string(&resolved)
                    .map_err(|e| EffectError::Handler(e.to_string()))?;
                cx.respond(contents)
            }
            FsReq::Write(path, contents) => {
                let resolved = self.resolve(&path)?;
                std::fs::write(&resolved, &contents)
                    .map_err(|e| EffectError::Handler(e.to_string()))?;
                cx.respond(())
            }
        }
    }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let cwd = std::env::current_dir()?;
    let handlers = frunk::hlist![ConsoleHandler, KvHandler::new(), FsHandler::new(cwd)];
    let server = TidepoolMcpServer::new(handlers);
    server.serve_stdio().await
}