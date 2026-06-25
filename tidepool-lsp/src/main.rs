//! `tidepool-lsp-daemon` — a persistent language-server sidecar for the tidepool
//! LSP effect.
//!
//! Spawns rust-analyzer once, keeps it warm/indexed, and serves a tiny
//! newline-delimited-JSON protocol over a Unix socket. The protocol speaks only
//! symbol names and file paths — never positions — so the tidepool `LspHandler`
//! that connects here stays a thin socket client.
//!
//! Usage: `tidepool-lsp-daemon [--root DIR] [--socket PATH]`
//!   --root    workspace root (default: current directory)
//!   --socket  socket path (default: $TIDEPOOL_LSP_SOCK or <root>/.tidepool/lsp.sock)

mod diff;
mod jsonrpc;
mod registry;
mod resolve;

use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use serde_json::{json, Value};

use jsonrpc::RaClient;

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let root = arg(&args, "--root")
        .map(PathBuf::from)
        .unwrap_or_else(|| std::env::current_dir().expect("cwd"));
    let root = root.canonicalize().unwrap_or(root);

    let sock_path = arg(&args, "--socket")
        .map(PathBuf::from)
        .or_else(|| std::env::var("TIDEPOOL_LSP_SOCK").ok().map(PathBuf::from))
        .unwrap_or_else(|| root.join(".tidepool").join("lsp.sock"));

    // v1 manages rust-analyzer for the whole workspace.
    let server_cmd = "rust-analyzer";
    eprintln!(
        "[tidepool-lsp] starting {} for {}",
        server_cmd,
        root.display()
    );

    let client = match RaClient::spawn(server_cmd, &root) {
        Ok(c) => Arc::new(c),
        Err(e) => {
            eprintln!("[tidepool-lsp] fatal: {}", e);
            std::process::exit(1);
        }
    };

    // Announce readiness in the background so the operator knows when warm.
    {
        let client = Arc::clone(&client);
        std::thread::spawn(move || {
            if client.wait_ready(Duration::from_secs(600)) {
                eprintln!("[tidepool-lsp] ready — workspace indexed");
            } else {
                eprintln!("[tidepool-lsp] still indexing after 10min; serving anyway");
            }
        });
    }

    // Bind the socket (clear any stale one first).
    if let Some(parent) = sock_path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let _ = std::fs::remove_file(&sock_path);
    let listener = match UnixListener::bind(&sock_path) {
        Ok(l) => l,
        Err(e) => {
            eprintln!("[tidepool-lsp] fatal: bind {}: {}", sock_path.display(), e);
            std::process::exit(1);
        }
    };
    eprintln!("[tidepool-lsp] listening on {}", sock_path.display());

    for stream in listener.incoming() {
        match stream {
            Ok(stream) => {
                let client = Arc::clone(&client);
                std::thread::spawn(move || handle_conn(stream, &client));
            }
            Err(e) => eprintln!("[tidepool-lsp] accept error: {}", e),
        }
    }
}

/// One connection: read request lines, write a response line each.
fn handle_conn(stream: UnixStream, client: &RaClient) {
    let reader = BufReader::new(match stream.try_clone() {
        Ok(s) => s,
        Err(_) => return,
    });
    let mut writer = stream;
    for line in reader.lines() {
        let Ok(line) = line else { break };
        if line.trim().is_empty() {
            continue;
        }
        let response = dispatch(&line, client);
        let mut bytes = serde_json::to_vec(&response).unwrap_or_else(|_| b"{}".to_vec());
        bytes.push(b'\n');
        if writer.write_all(&bytes).is_err() || writer.flush().is_err() {
            break;
        }
    }
}

/// Parse and route one request line.
fn dispatch(line: &str, client: &RaClient) -> Value {
    let req: Value = match serde_json::from_str(line) {
        Ok(v) => v,
        Err(e) => return err(format!("invalid request JSON: {}", e)),
    };
    let op = req.get("op").and_then(Value::as_str).unwrap_or("");

    // Status is always answerable.
    if op == "status" {
        return json!({ "ok": true, "result": {
            "ready": client.is_ready(), "message": client.status_message()
        }});
    }

    // Everything else needs the workspace indexed.
    if !client.is_ready() {
        return err(format!(
            "rust-analyzer not ready: {}",
            client.status_message()
        ));
    }

    let file = req.get("file").and_then(Value::as_str);
    let symbol = req.get("symbol").and_then(Value::as_str);
    let node = req.get("node");

    // Reject files we have no server for (forward-compat with the registry).
    let node_file = node.and_then(|n| n.get("file")).and_then(Value::as_str);
    for f in [file, node_file].into_iter().flatten() {
        if registry::server_for(f).is_none() {
            return err(format!("no language server configured for {}", f));
        }
    }

    // The node-addressed ops share one resolution path.
    let on_node = |f: &dyn Fn(&Value) -> Result<Value, String>| match node {
        Some(n) => f(n),
        None => Err(format!("'{}' needs a node", op)),
    };

    let result = match op {
        "where" => match symbol {
            Some(s) => resolve::where_symbol(client, s).map(Value::from),
            None => Err("'where' needs a symbol".into()),
        },
        "callers" => on_node(&|n| resolve::callers(client, n).map(Value::from)),
        "callees" => on_node(&|n| resolve::callees(client, n).map(Value::from)),
        "references" => on_node(&|n| resolve::references(client, n).map(Value::from)),
        "def" => on_node(&|n| resolve::def(client, n).map(|o| o.unwrap_or(Value::Null))),
        "hover" => on_node(&|n| {
            resolve::hover(client, n).map(|o| o.map(Value::String).unwrap_or(Value::Null))
        }),
        "rename" => {
            let new_name = req.get("newName").and_then(Value::as_str);
            match new_name {
                Some(nn) => on_node(&|n| resolve::rename(client, n, nn).map(Value::String)),
                None => Err("'rename' needs newName".into()),
            }
        }
        "diagnostics" => match file {
            Some(f) => resolve::diagnostics(client, f).map(Value::from),
            None => Err("'diagnostics' needs a file".into()),
        },
        other => Err(format!("unknown op '{}'", other)),
    };

    match result {
        Ok(v) => json!({ "ok": true, "result": v }),
        Err(e) => err(e),
    }
}

fn err(msg: impl Into<String>) -> Value {
    json!({ "ok": false, "error": msg.into() })
}

/// Read a `--flag value` argument.
fn arg(args: &[String], flag: &str) -> Option<String> {
    args.iter()
        .position(|a| a == flag)
        .and_then(|i| args.get(i + 1).cloned())
}
