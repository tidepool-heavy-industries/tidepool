//! Minimal JSON-RPC client for a language server over stdio.
//!
//! Owns the spawned server process, frames messages with `Content-Length`
//! headers, and runs a background reader thread that routes responses back to
//! waiting callers by id, tracks indexing progress (the readiness gate), caches
//! pushed diagnostics, and auto-replies to server→client requests.
//!
//! Everything is `serde_json::Value` — no `lsp-types` dependency. The LSP
//! payload shapes this client touches (`uri`, `range.start.{line,character}`,
//! hover `contents`, `WorkspaceEdit`) are union-typed in the spec and easier to
//! hand-parse than to thread through evolving typed structs.

use std::collections::HashMap;
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use crossbeam_channel::{bounded, Sender};
use serde_json::{json, Value};

/// Per-request timeout. Kept below the tidepool 30s eval timeout so the daemon
/// returns a clean error before the calling eval gives up.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(25);

/// Map of in-flight request id → the channel awaiting its response.
type Pending = Arc<Mutex<HashMap<i64, Sender<Result<Value, String>>>>>;

/// Readiness state, updated by the reader thread from `$/progress` traffic.
#[derive(Default)]
struct Ready {
    /// True once indexing/cache-priming has reported completion.
    ready: bool,
    /// Human-readable progress note, e.g. "indexing (37%)".
    message: String,
}

/// A live connection to a language server subprocess.
pub struct RaClient {
    stdin: Arc<Mutex<ChildStdin>>,
    next_id: AtomicI64,
    pending: Pending,
    ready: Arc<Mutex<Ready>>,
    /// uri string -> latest published diagnostics array.
    diagnostics: Arc<Mutex<HashMap<String, Value>>>,
    /// Files we've sent `didOpen` for (avoid re-opening).
    opened: Arc<Mutex<HashMap<String, i64>>>,
    root: PathBuf,
    _child: Child,
}

impl RaClient {
    /// Spawn `command` rooted at `root`, perform the LSP initialize handshake,
    /// and start the background reader. Returns once `initialized` is sent;
    /// indexing continues asynchronously (gate on [`RaClient::is_ready`]).
    pub fn spawn(command: &str, root: &Path) -> Result<Self, String> {
        let mut child = Command::new(command)
            .current_dir(root)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|e| format!("failed to spawn '{}': {}", command, e))?;

        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| "no stdin on language server".to_string())?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| "no stdout on language server".to_string())?;

        let stdin = Arc::new(Mutex::new(stdin));
        let pending: Pending = Arc::new(Mutex::new(HashMap::new()));
        let ready = Arc::new(Mutex::new(Ready::default()));
        let diagnostics = Arc::new(Mutex::new(HashMap::new()));

        // Reader thread: frame, route responses, handle notifications/requests.
        {
            let pending = Arc::clone(&pending);
            let ready = Arc::clone(&ready);
            let diagnostics = Arc::clone(&diagnostics);
            let stdin = Arc::clone(&stdin);
            thread::spawn(move || {
                let mut reader = BufReader::new(stdout);
                loop {
                    match read_message(&mut reader) {
                        Ok(Some(msg)) => {
                            handle_message(msg, &pending, &ready, &diagnostics, &stdin);
                        }
                        Ok(None) => break, // EOF: server exited
                        Err(_) => break,
                    }
                }
                // On exit, fail any in-flight requests so callers don't hang.
                let mut p = pending.lock().unwrap();
                for (_, tx) in p.drain() {
                    let _ = tx.send(Err("language server exited".to_string()));
                }
            });
        }

        let client = RaClient {
            stdin,
            next_id: AtomicI64::new(1),
            pending,
            ready,
            diagnostics,
            opened: Arc::new(Mutex::new(HashMap::new())),
            root: root.to_path_buf(),
            _child: child,
        };

        client.handshake()?;
        Ok(client)
    }

    fn handshake(&self) -> Result<(), String> {
        let root_uri = path_to_uri(&self.root);
        let params = json!({
            "processId": std::process::id(),
            "rootUri": root_uri,
            // Widen symbol search: include all symbol kinds (not only types) and
            // raise the result cap so exact-name matches for common short names
            // (dispatch, handle, new) aren't buried past the default ~128 limit.
            "initializationOptions": {
                "workspace": { "symbol": { "search": {
                    "kind": "all_symbols", "scope": "workspace", "limit": 2048
                }}}
            },
            "capabilities": {
                "workspace": { "symbol": { "dynamicRegistration": false } },
                "textDocument": {
                    "references": {},
                    "hover": { "contentFormat": ["plaintext", "markdown"] },
                    "rename": {},
                    "definition": {},
                    "publishDiagnostics": {},
                    "diagnostic": {}
                },
                "window": { "workDoneProgress": true }
            },
            "workspaceFolders": [ { "uri": root_uri, "name": "root" } ]
        });
        self.request("initialize", params)?;
        self.notify("initialized", json!({}));
        Ok(())
    }

    /// Send a request and block for its response (up to [`REQUEST_TIMEOUT`]).
    pub fn request(&self, method: &str, params: Value) -> Result<Value, String> {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let (tx, rx) = bounded(1);
        self.pending.lock().unwrap().insert(id, tx);

        let msg = json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params });
        if let Err(e) = self.write_message(&msg) {
            self.pending.lock().unwrap().remove(&id);
            return Err(e);
        }

        match rx.recv_timeout(REQUEST_TIMEOUT) {
            Ok(result) => result,
            Err(_) => {
                self.pending.lock().unwrap().remove(&id);
                Err(format!("language server timed out on {}", method))
            }
        }
    }

    /// Fire-and-forget notification.
    pub fn notify(&self, method: &str, params: Value) {
        let msg = json!({ "jsonrpc": "2.0", "method": method, "params": params });
        let _ = self.write_message(&msg);
    }

    fn write_message(&self, msg: &Value) -> Result<(), String> {
        let body = serde_json::to_vec(msg).map_err(|e| e.to_string())?;
        let mut w = self.stdin.lock().unwrap();
        write!(w, "Content-Length: {}\r\n\r\n", body.len()).map_err(|e| e.to_string())?;
        w.write_all(&body).map_err(|e| e.to_string())?;
        w.flush().map_err(|e| e.to_string())
    }

    /// True once indexing has reported completion.
    pub fn is_ready(&self) -> bool {
        self.ready.lock().unwrap().ready
    }

    /// Current progress note (e.g. "indexing (37%)").
    pub fn status_message(&self) -> String {
        let r = self.ready.lock().unwrap();
        if r.ready {
            "ready".to_string()
        } else if r.message.is_empty() {
            "starting".to_string()
        } else {
            r.message.clone()
        }
    }

    /// Block until ready or `timeout` elapses; returns whether it became ready.
    pub fn wait_ready(&self, timeout: Duration) -> bool {
        let start = std::time::Instant::now();
        while start.elapsed() < timeout {
            if self.is_ready() {
                return true;
            }
            thread::sleep(Duration::from_millis(100));
        }
        self.is_ready()
    }

    /// Ensure the server has `didOpen` for `abs_path` (needed for diagnostics).
    pub fn ensure_open(&self, abs_path: &Path) -> Result<(), String> {
        let uri = path_to_uri(abs_path);
        if self.opened.lock().unwrap().contains_key(&uri) {
            return Ok(());
        }
        let text = std::fs::read_to_string(abs_path)
            .map_err(|e| format!("read {}: {}", abs_path.display(), e))?;
        let lang_id = language_id(abs_path);
        self.notify(
            "textDocument/didOpen",
            json!({ "textDocument": {
                "uri": uri, "languageId": lang_id, "version": 1, "text": text
            }}),
        );
        self.opened.lock().unwrap().insert(uri, 1);
        Ok(())
    }

    /// Latest pushed diagnostics for `uri`, if any.
    pub fn cached_diagnostics(&self, uri: &str) -> Option<Value> {
        self.diagnostics.lock().unwrap().get(uri).cloned()
    }

    pub fn root(&self) -> &Path {
        &self.root
    }
}

/// Route one parsed message: response → waiting caller; notification → state;
/// server→client request → default reply.
fn handle_message(
    msg: Value,
    pending: &Pending,
    ready: &Arc<Mutex<Ready>>,
    diagnostics: &Arc<Mutex<HashMap<String, Value>>>,
    stdin: &Arc<Mutex<ChildStdin>>,
) {
    // Response to one of our requests.
    if let Some(id) = msg.get("id").and_then(Value::as_i64) {
        if msg.get("method").is_none() {
            let result = if let Some(err) = msg.get("error") {
                Err(err
                    .get("message")
                    .and_then(Value::as_str)
                    .unwrap_or("language server error")
                    .to_string())
            } else {
                Ok(msg.get("result").cloned().unwrap_or(Value::Null))
            };
            if let Some(tx) = pending.lock().unwrap().remove(&id) {
                let _ = tx.send(result);
            }
            return;
        }
        // Has both id and method → server→client request: reply with a default.
        reply_to_server_request(id, &msg, stdin);
        return;
    }

    // Notification.
    if let Some(method) = msg.get("method").and_then(Value::as_str) {
        match method {
            "$/progress" => update_progress(msg.get("params"), ready),
            "textDocument/publishDiagnostics" => {
                if let Some(params) = msg.get("params") {
                    if let Some(uri) = params.get("uri").and_then(Value::as_str) {
                        let diags = params.get("diagnostics").cloned().unwrap_or(json!([]));
                        diagnostics.lock().unwrap().insert(uri.to_string(), diags);
                    }
                }
            }
            _ => {}
        }
    }
}

/// rust-analyzer issues a few requests (configuration, progress-create,
/// capability registration). Reply with a benign default so it doesn't block.
fn reply_to_server_request(id: i64, msg: &Value, stdin: &Arc<Mutex<ChildStdin>>) {
    let method = msg.get("method").and_then(Value::as_str).unwrap_or("");
    let result = match method {
        // One config object per requested item; null = "use defaults".
        "workspace/configuration" => {
            let n = msg
                .get("params")
                .and_then(|p| p.get("items"))
                .and_then(Value::as_array)
                .map(|a| a.len())
                .unwrap_or(1);
            Value::Array(vec![Value::Null; n.max(1)])
        }
        _ => Value::Null,
    };
    let reply = json!({ "jsonrpc": "2.0", "id": id, "result": result });
    if let Ok(body) = serde_json::to_vec(&reply) {
        if let Ok(mut w) = stdin.lock() {
            let _ = write!(w, "Content-Length: {}\r\n\r\n", body.len());
            let _ = w.write_all(&body);
            let _ = w.flush();
        }
    }
}

/// Track indexing/cache-priming progress to drive the readiness gate.
fn update_progress(params: Option<&Value>, ready: &Arc<Mutex<Ready>>) {
    let Some(params) = params else { return };
    let token = params
        .get("token")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    let Some(value) = params.get("value") else {
        return;
    };
    let kind = value.get("kind").and_then(Value::as_str).unwrap_or("");

    // Tokens that indicate the workspace is becoming queryable.
    let is_index_token = token.contains("Indexing")
        || token.contains("cachePriming")
        || token.contains("Roots Scanned")
        || token.contains("Building");

    let mut r = ready.lock().unwrap();
    match kind {
        "begin" | "report" => {
            if is_index_token {
                let pct = value.get("percentage").and_then(Value::as_u64);
                let title = value
                    .get("title")
                    .and_then(Value::as_str)
                    .unwrap_or("indexing");
                r.message = match pct {
                    Some(p) => format!("{} ({}%)", title, p),
                    None => title.to_string(),
                };
            }
        }
        "end" => {
            if is_index_token {
                r.ready = true;
                r.message = "ready".to_string();
            }
        }
        _ => {}
    }
}

/// Read one `Content-Length`-framed JSON message. `Ok(None)` on clean EOF.
fn read_message<R: BufRead>(reader: &mut R) -> std::io::Result<Option<Value>> {
    let mut content_length: Option<usize> = None;
    loop {
        let mut line = String::new();
        let n = reader.read_line(&mut line)?;
        if n == 0 {
            return Ok(None); // EOF
        }
        let trimmed = line.trim_end();
        if trimmed.is_empty() {
            break; // end of headers
        }
        if let Some(rest) = trimmed.strip_prefix("Content-Length:") {
            content_length = rest.trim().parse().ok();
        }
    }
    let len = match content_length {
        Some(n) => n,
        None => return Ok(None),
    };
    let mut buf = vec![0u8; len];
    reader.read_exact(&mut buf)?;
    match serde_json::from_slice(&buf) {
        Ok(v) => Ok(Some(v)),
        Err(_) => Ok(Some(Value::Null)), // skip unparseable frames
    }
}

/// `file:///abs/path` URI for a filesystem path (no percent-encoding of the
/// path beyond what rust-analyzer needs; paths here are workspace-local).
pub fn path_to_uri(path: &Path) -> String {
    let s = path.to_string_lossy();
    if s.starts_with('/') {
        format!("file://{}", s)
    } else {
        format!("file:///{}", s)
    }
}

/// Strip a `file://` URI back to a path string.
pub fn uri_to_path(uri: &str) -> String {
    uri.strip_prefix("file://").unwrap_or(uri).to_string()
}

fn language_id(path: &Path) -> &'static str {
    match path.extension().and_then(|e| e.to_str()) {
        Some("rs") => "rust",
        Some("hs") => "haskell",
        Some("py") => "python",
        Some("ts") => "typescript",
        Some("js") => "javascript",
        Some("go") => "go",
        _ => "plaintext",
    }
}
