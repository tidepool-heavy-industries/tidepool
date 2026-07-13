//! The operations, built on the raw JSON-RPC client. Each returns a
//! `serde_json::Value` ready to embed in the socket response. All LSP-shaped
//! detail (positions, UTF-16 offsets, `WorkspaceEdit`, hover unions, call
//! hierarchy) is resolved here so the tidepool effect surface never sees it.
//!
//! Addressing: `where` takes a name (the seed). Every other op takes a whole
//! **node** `{name, file, line, char, …}` and re-resolves it to an LSP
//! position by reading the node's exact `pos` (0-based UTF-16 line/char)
//! directly — not a substring search, so there is no wrong-column ambiguity.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use serde_json::{json, Value};

use crate::diff;
use crate::jsonrpc::{path_to_uri, uri_to_path, RaClient};

/// Cache of file contents → lines, so enrichment doesn't re-read.
#[derive(Default)]
struct FileCache {
    files: HashMap<String, Vec<String>>,
}

impl FileCache {
    fn line(&mut self, abs: &str, line0: usize) -> String {
        let lines = self.files.entry(abs.to_string()).or_insert_with(|| {
            match std::fs::read_to_string(abs) {
                Ok(s) => s.lines().map(|l| l.to_string()).collect(),
                Err(_) => Vec::new(),
            }
        });
        lines
            .get(line0)
            .map(|l| l.trim_end().to_string())
            .unwrap_or_default()
    }
}

/// Resolve a node's `file` against the workspace `root`, enforcing containment.
///
/// `file` is untrusted (it arrives on the socket from the effect surface), and
/// `Path::join` on an ABSOLUTE `file` silently replaces the root — so a bare
/// `root.join(file)` let `/etc/x.rs` reach the filesystem. Mirror the Fs
/// handler's sandbox (canonicalize both, then `starts_with`): any path resolving
/// outside the workspace root — via an absolute `file` or `..` escape — is a
/// loud error, not a silent read. Also errors if the path can't be canonicalized
/// (e.g. it doesn't exist).
fn abs_of(root: &Path, file: &str) -> Result<PathBuf, String> {
    let canon_root = root
        .canonicalize()
        .map_err(|e| format!("workspace root {}: {}", root.display(), e))?;
    let abs = canon_root.join(file);
    let canon = abs
        .canonicalize()
        .map_err(|e| format!("resolve {}: {}", abs.display(), e))?;
    if !canon.starts_with(&canon_root) {
        return Err(format!("path {file:?} escapes the workspace root"));
    }
    Ok(canon)
}

fn rel_of(root: &Path, abs: &str) -> String {
    let p = Path::new(abs);
    p.strip_prefix(root)
        .unwrap_or(p)
        .to_string_lossy()
        .to_string()
}

fn symbol_kind(k: u64) -> &'static str {
    match k {
        2 => "module",
        5 => "class",
        6 => "method",
        7 => "property",
        8 => "field",
        9 => "constructor",
        10 => "enum",
        11 => "interface",
        12 => "function",
        13 => "variable",
        14 => "constant",
        22 => "enum-member",
        23 => "struct",
        26 => "type-param",
        _ => "symbol",
    }
}

fn severity(s: u64) -> &'static str {
    match s {
        1 => "error",
        2 => "warning",
        3 => "information",
        4 => "hint",
        _ => "unknown",
    }
}

/// Build a node JSON object (the wire/effect currency). `line1` is 1-based
/// (display); `char0` is the EXACT 0-based UTF-16 column from the LSP response.
/// Together they form the `pos` that re-resolution reads back — no guessing.
fn node(
    name: &str,
    container: &str,
    kind: &str,
    file: &str,
    line1: u64,
    char0: u64,
    text: &str,
) -> Value {
    json!({
        "name": name, "container": container, "kind": kind, "file": file,
        "pos": { "line": line1, "char": char0 }, "text": text,
    })
}

// --- addressing ----------------------------------------------------------

/// Resolve a node to its exact LSP position, read straight from `node.pos`
/// (which the daemon populated from the originating LSP response). No substring
/// search → no wrong-column aborts.
fn node_position(client: &RaClient, n: &Value) -> Result<(String, u64, u64), String> {
    let file = n
        .get("file")
        .and_then(Value::as_str)
        .ok_or("node missing 'file'")?;
    let pos = n.get("pos").ok_or("node missing 'pos'")?;
    let line1 = pos
        .get("line")
        .and_then(Value::as_u64)
        .ok_or("node pos missing 'line'")?;
    let char0 = pos.get("char").and_then(Value::as_u64).unwrap_or(0);
    let abs = abs_of(client.root(), file)?;
    Ok((path_to_uri(&abs), line1.saturating_sub(1), char0))
}

/// `(0-based line, 0-based UTF-16 char)` of a range's start.
fn start_lc(range: &Value) -> (u64, u64) {
    let start = range.get("start");
    let line = start
        .and_then(|s| s.get("line"))
        .and_then(Value::as_u64)
        .unwrap_or(0);
    let ch = start
        .and_then(|s| s.get("character"))
        .and_then(Value::as_u64)
        .unwrap_or(0);
    (line, ch)
}

// --- where (the name seed) ----------------------------------------------

/// All workspace definitions whose name exactly equals `name`.
pub fn where_symbol(client: &RaClient, name: &str) -> Result<Vec<Value>, String> {
    let result = client.request("workspace/symbol", json!({ "query": name }))?;
    let arr = result.as_array().cloned().unwrap_or_default();
    let root = client.root().to_path_buf();
    let mut cache = FileCache::default();
    let mut out = Vec::new();

    for sym in arr {
        if sym.get("name").and_then(Value::as_str) != Some(name) {
            continue;
        }
        let Some(loc) = sym.get("location") else {
            continue;
        };
        let Some((abs, line0, char0)) = location_of_loc(loc) else {
            continue;
        };
        let container = sym
            .get("containerName")
            .and_then(Value::as_str)
            .unwrap_or("");
        let kind = symbol_kind(sym.get("kind").and_then(Value::as_u64).unwrap_or(0));
        out.push(node(
            name,
            container,
            kind,
            &rel_of(&root, &abs),
            line0 + 1,
            char0,
            &cache.line(&abs, line0 as usize),
        ));
    }
    Ok(out)
}

// --- graph edges: callers / callees / def / references -------------------

/// Incoming calls — the functions that call this node. `Ok(None)` when the
/// node isn't callable (no call hierarchy); `Ok(Some(_))` (maybe empty) when it is.
pub fn callers(client: &RaClient, n: &Value) -> Result<Option<Vec<Value>>, String> {
    let Some(item) = prepare_call_item(client, n)? else {
        return Ok(None);
    };
    let result = client.request("callHierarchy/incomingCalls", json!({ "item": item }))?;
    let arr = result.as_array().cloned().unwrap_or_default();
    let root = client.root().to_path_buf();
    let mut cache = FileCache::default();
    Ok(Some(
        arr.iter()
            .filter_map(|c| c.get("from"))
            .map(|item| item_to_node(&root, item, &mut cache))
            .collect(),
    ))
}

/// Outgoing calls — the functions this node calls. `Ok(None)` when not callable.
pub fn callees(client: &RaClient, n: &Value) -> Result<Option<Vec<Value>>, String> {
    let Some(item) = prepare_call_item(client, n)? else {
        return Ok(None);
    };
    let result = client.request("callHierarchy/outgoingCalls", json!({ "item": item }))?;
    let arr = result.as_array().cloned().unwrap_or_default();
    let root = client.root().to_path_buf();
    let mut cache = FileCache::default();
    Ok(Some(
        arr.iter()
            .filter_map(|c| c.get("to"))
            .map(|item| item_to_node(&root, item, &mut cache))
            .collect(),
    ))
}

/// prepareCallHierarchy at the node's position → the first CallHierarchyItem,
/// or `None` when the symbol has no call hierarchy (not callable — not an error).
fn prepare_call_item(client: &RaClient, n: &Value) -> Result<Option<Value>, String> {
    let (uri, line, ch) = node_position(client, n)?;
    let result = client.request(
        "textDocument/prepareCallHierarchy",
        json!({ "textDocument": { "uri": uri }, "position": { "line": line, "character": ch } }),
    )?;
    Ok(result.as_array().and_then(|a| a.first()).cloned())
}

/// A CallHierarchyItem → a node. `detail` is rust-analyzer's container/signature.
fn item_to_node(root: &Path, item: &Value, cache: &mut FileCache) -> Value {
    let name = item.get("name").and_then(Value::as_str).unwrap_or("");
    let container = item.get("detail").and_then(Value::as_str).unwrap_or("");
    let kind = symbol_kind(item.get("kind").and_then(Value::as_u64).unwrap_or(0));
    let uri = item.get("uri").and_then(Value::as_str).unwrap_or("");
    let abs = uri_to_path(uri);
    // selectionRange points at the identifier; fall back to range.
    let range = item
        .get("selectionRange")
        .or_else(|| item.get("range"))
        .cloned()
        .unwrap_or(Value::Null);
    let (line0, char0) = start_lc(&range);
    node(
        name,
        container,
        kind,
        &rel_of(root, &abs),
        line0 + 1,
        char0,
        &cache.line(&abs, line0 as usize),
    )
}

/// Resolve a node (often a use-site) to its definition node.
pub fn def(client: &RaClient, n: &Value) -> Result<Option<Value>, String> {
    let (uri, line, ch) = node_position(client, n)?;
    let result = client.request(
        "textDocument/definition",
        json!({ "textDocument": { "uri": uri }, "position": { "line": line, "character": ch } }),
    )?;
    // Location | Location[] | LocationLink[].
    let loc = match &result {
        Value::Array(a) => a.first().cloned(),
        Value::Null => None,
        v => Some(v.clone()),
    };
    let Some(loc) = loc else { return Ok(None) };
    // LocationLink uses targetUri/targetSelectionRange; Location uses uri/range.
    let def_uri = loc
        .get("uri")
        .or_else(|| loc.get("targetUri"))
        .and_then(Value::as_str)
        .unwrap_or("");
    let def_range = loc
        .get("range")
        .or_else(|| loc.get("targetSelectionRange"))
        .cloned()
        .unwrap_or(Value::Null);
    let (def_line0, def_char0) = start_lc(&def_range);
    let abs = uri_to_path(def_uri);
    let root = client.root().to_path_buf();
    let mut cache = FileCache::default();

    // Enrich with the symbol's name/kind/container via documentSymbol.
    let (name, kind, container) = symbol_at_line(client, def_uri, def_line0).unwrap_or_else(|| {
        (
            n.get("name")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string(),
            "symbol".to_string(),
            String::new(),
        )
    });
    Ok(Some(node(
        &name,
        &container,
        &kind,
        &rel_of(&root, &abs),
        def_line0 + 1,
        def_char0,
        &cache.line(&abs, def_line0 as usize),
    )))
}

/// References (use sites) of the node's symbol. Tagged `kind:"reference"`.
/// `Ok(None)` when the position isn't a symbol (RA returns null).
///
/// `textDocument/references` alone undercounts: rust-analyzer drops sites
/// where the symbol is coerced to a value rather than called or imported
/// (e.g. `f as *const u8` fn-pointer registration — #348), even though
/// `textDocument/rename` DOES rewrite those same sites. So the reference set
/// here is unioned with a rename probe's touch points, deduped by
/// `(file, line, char)` — the two verbs must agree on what counts as a
/// reference, since `lspRefs` is the blast-radius query and `lspRename` is
/// its ground truth for "does this site depend on the symbol".
pub fn references(client: &RaClient, n: &Value) -> Result<Option<Vec<Value>>, String> {
    let (uri, line, ch) = node_position(client, n)?;
    let name = n.get("name").and_then(Value::as_str).unwrap_or("");
    let result = client.request(
        "textDocument/references",
        json!({
            "textDocument": { "uri": uri },
            "position": { "line": line, "character": ch },
            "context": { "includeDeclaration": true }
        }),
    )?;
    if result.is_null() {
        return Ok(None);
    }
    let arr = result.as_array().cloned().unwrap_or_default();
    let root = client.root().to_path_buf();
    let mut cache = FileCache::default();
    let mut seen: HashSet<(String, u64, u64)> = HashSet::new();
    let mut out = Vec::new();
    for loc in arr {
        let Some((abs, line0, char0)) = location_of_loc(&loc) else {
            continue;
        };
        seen.insert((abs.clone(), line0, char0));
        out.push(node(
            name,
            "",
            "reference",
            &rel_of(&root, &abs),
            line0 + 1,
            char0,
            &cache.line(&abs, line0 as usize),
        ));
    }

    // Scratch-name rename probe: never applied, read-only (`request_rename_edit`
    // only returns the WorkspaceEdit; nothing touches disk). Best-effort — a
    // probe failure (e.g. the symbol genuinely can't be renamed, such as an
    // external-crate item) just means no supplement, not an error.
    if !name.is_empty() {
        let probe_name = format!("{name}__tidepool_ref_probe");
        if let Ok(Some(edit)) = request_rename_edit(client, n, &probe_name) {
            for (edit_uri, line0, char0) in edit_touch_points(&edit) {
                let abs = uri_to_path(&edit_uri);
                if seen.insert((abs.clone(), line0, char0)) {
                    out.push(node(
                        name,
                        "",
                        "reference",
                        &rel_of(&root, &abs),
                        line0 + 1,
                        char0,
                        &cache.line(&abs, line0 as usize),
                    ));
                }
            }
        }
    }

    out.sort_by_key(node_sort_key);
    Ok(Some(out))
}

// --- hover / rename / diagnostics ----------------------------------------

/// Hover (type / signature / docs) for a node, flattened to plain text.
pub fn hover(client: &RaClient, n: &Value) -> Result<Option<String>, String> {
    let (uri, line, ch) = node_position(client, n)?;
    let result = client.request(
        "textDocument/hover",
        json!({ "textDocument": { "uri": uri }, "position": { "line": line, "character": ch } }),
    )?;
    if result.is_null() {
        return Ok(None);
    }
    Ok(flatten_hover(result.get("contents")))
}

/// Raw `textDocument/rename` WorkspaceEdit, or `None` when RA can't rename
/// here. Read-only — the edit is never applied to disk; callers either diff
/// it in memory (`rename`) or just harvest touch points (`references`).
fn request_rename_edit(
    client: &RaClient,
    n: &Value,
    new_name: &str,
) -> Result<Option<Value>, String> {
    let (uri, line, ch) = node_position(client, n)?;
    let result = client.request(
        "textDocument/rename",
        json!({
            "textDocument": { "uri": uri },
            "position": { "line": line, "character": ch },
            "newName": new_name
        }),
    )?;
    Ok(if result.is_null() { None } else { Some(result) })
}

/// Every edit's `(uri, 0-based line, 0-based UTF-16 char)` start position in
/// a WorkspaceEdit — the set of sites rename would touch.
fn edit_touch_points(edit: &Value) -> Vec<(String, u64, u64)> {
    let mut out = Vec::new();
    if let Some(changes) = edit.get("changes").and_then(Value::as_object) {
        for (uri, edits) in changes {
            for e in edits.as_array().into_iter().flatten() {
                if let Some(range) = e.get("range") {
                    let (l, c) = start_lc(range);
                    out.push((uri.clone(), l, c));
                }
            }
        }
    }
    if let Some(docs) = edit.get("documentChanges").and_then(Value::as_array) {
        for doc in docs {
            let Some(uri) = doc
                .get("textDocument")
                .and_then(|t| t.get("uri"))
                .and_then(Value::as_str)
            else {
                continue;
            };
            for e in doc
                .get("edits")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
            {
                if let Some(range) = e.get("range") {
                    let (l, c) = start_lc(range);
                    out.push((uri.to_string(), l, c));
                }
            }
        }
    }
    out
}

/// `(file, line, char)` sort key for a reference/def node, for deterministic ordering.
fn node_sort_key(v: &Value) -> (String, u64, u64) {
    let file = v
        .get("file")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    let pos = v.get("pos");
    let line = pos
        .and_then(|p| p.get("line"))
        .and_then(Value::as_u64)
        .unwrap_or(0);
    let char = pos
        .and_then(|p| p.get("char"))
        .and_then(Value::as_u64)
        .unwrap_or(0);
    (file, line, char)
}

/// Rename the node's symbol to `new_name`; returns a unified diff (not applied).
/// `Ok(None)` when the symbol can't be renamed (RA returns null).
pub fn rename(client: &RaClient, n: &Value, new_name: &str) -> Result<Option<String>, String> {
    let Some(result) = request_rename_edit(client, n, new_name)? else {
        return Ok(None);
    };

    // WorkspaceEdit: either `changes: {uri: [edit]}` or `documentChanges: [...]`.
    let mut per_file: Vec<(String, Vec<Value>)> = Vec::new();
    if let Some(changes) = result.get("changes").and_then(Value::as_object) {
        for (uri, edits) in changes {
            per_file.push((uri.clone(), edits.as_array().cloned().unwrap_or_default()));
        }
    }
    if let Some(docs) = result.get("documentChanges").and_then(Value::as_array) {
        for doc in docs {
            let Some(uri) = doc
                .get("textDocument")
                .and_then(|t| t.get("uri"))
                .and_then(Value::as_str)
            else {
                continue;
            };
            let edits = doc
                .get("edits")
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default();
            per_file.push((uri.to_string(), edits));
        }
    }

    let root = client.root().to_path_buf();
    let mut diff_out = String::new();
    for (uri, edits) in per_file {
        let abs = uri_to_path(&uri);
        let old = std::fs::read_to_string(&abs).map_err(|e| format!("read {}: {}", abs, e))?;
        let new = apply_edits(&old, &edits);
        let rel = rel_of(&root, &abs);
        diff_out.push_str(&diff::unified(&rel, &old, &new));
    }
    Ok(Some(diff_out))
}

/// Sleep for RA to push its own diagnostics, then read the last-pushed cache
/// (empty if none). Shared fallback for both the pull-failure and
/// pull-succeeded-but-shapeless branches of `diagnostics`.
fn fallback_diagnostics(client: &RaClient, uri: &str) -> Vec<Value> {
    std::thread::sleep(std::time::Duration::from_millis(400));
    client
        .cached_diagnostics(uri)
        .and_then(|d| d.as_array().cloned())
        .unwrap_or_default()
}

/// Diagnostics for `file` (pull request, falling back to pushed cache).
pub fn diagnostics(client: &RaClient, file: &str) -> Result<Vec<Value>, String> {
    let root = client.root().to_path_buf();
    let abs = abs_of(&root, file)?;
    client.ensure_open(&abs)?;
    let uri = path_to_uri(&abs);

    let items = match client.request(
        "textDocument/diagnostic",
        json!({ "textDocument": { "uri": uri } }),
    ) {
        Ok(r) => r
            .get("items")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_else(|| fallback_diagnostics(client, &uri)),
        Err(_) => fallback_diagnostics(client, &uri),
    };

    let mut out = Vec::new();
    for d in items {
        let line = d
            .get("range")
            .and_then(|r| r.get("start"))
            .and_then(|s| s.get("line"))
            .and_then(Value::as_u64)
            .unwrap_or(0);
        let sev = d.get("severity").and_then(Value::as_u64).unwrap_or(0);
        let msg = d
            .get("message")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string();
        out.push(json!({
            "file": file,
            "line": line + 1,
            "severity": severity(sev),
            "message": msg,
        }));
    }
    Ok(out)
}

// --- shared helpers ------------------------------------------------------

/// `(abs path, 0-based line, 0-based UTF-16 char)` from a `Location`.
fn location_of_loc(loc: &Value) -> Option<(String, u64, u64)> {
    let uri = loc.get("uri").and_then(Value::as_str)?;
    let range = loc.get("range")?;
    let (line0, char0) = start_lc(range);
    Some((uri_to_path(uri), line0, char0))
}

/// Find the innermost `DocumentSymbol` covering `line0`; returns
/// `(name, kind, container)` where container is the enclosing symbol's name.
fn symbol_at_line(client: &RaClient, uri: &str, line0: u64) -> Option<(String, String, String)> {
    let result = client
        .request(
            "textDocument/documentSymbol",
            json!({ "textDocument": { "uri": uri } }),
        )
        .ok()?;
    let syms = result.as_array()?;
    fn walk(syms: &[Value], line0: u64, parent: &str) -> Option<(String, String, String)> {
        for s in syms {
            let range = s.get("range")?;
            let start = range.get("start")?.get("line")?.as_u64()?;
            let end = range.get("end")?.get("line")?.as_u64()?;
            if line0 >= start && line0 <= end {
                let name = s.get("name").and_then(Value::as_str).unwrap_or("");
                if let Some(children) = s.get("children").and_then(Value::as_array) {
                    if let Some(deep) = walk(children, line0, name) {
                        return Some(deep);
                    }
                }
                let kind = symbol_kind(s.get("kind").and_then(Value::as_u64).unwrap_or(0));
                return Some((name.to_string(), kind.to_string(), parent.to_string()));
            }
        }
        None
    }
    walk(syms, line0, "")
}

/// Flatten an LSP hover `contents` (string | MarkedString | MarkupContent |
/// array of those) to plain text.
fn flatten_hover(contents: Option<&Value>) -> Option<String> {
    let c = contents?;
    let text = match c {
        Value::String(s) => s.clone(),
        Value::Object(_) => c
            .get("value")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string(),
        Value::Array(items) => items
            .iter()
            .filter_map(|i| match i {
                Value::String(s) => Some(s.clone()),
                Value::Object(_) => i.get("value").and_then(Value::as_str).map(str::to_string),
                _ => None,
            })
            .collect::<Vec<_>>()
            .join("\n"),
        _ => return None,
    };
    let trimmed = text.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_string())
    }
}

/// Apply a list of LSP `TextEdit`s to `content`, returning the new text.
/// Ranges are (line, UTF-16 char); edits are applied right-to-left.
fn apply_edits(content: &str, edits: &[Value]) -> String {
    let mut line_starts = vec![0usize];
    for (i, b) in content.bytes().enumerate() {
        if b == b'\n' {
            line_starts.push(i + 1);
        }
    }
    let line_str = |ln: usize| -> &str {
        let start = *line_starts.get(ln).unwrap_or(&content.len());
        let end = line_starts.get(ln + 1).copied().unwrap_or(content.len());
        &content[start..end]
    };
    let to_byte = |ln: u64, ch: u64| -> usize {
        let ln = ln as usize;
        let base = *line_starts.get(ln).unwrap_or(&content.len());
        base + utf16_to_byte(line_str(ln), ch as usize)
    };

    let mut spans: Vec<(usize, usize, String)> = edits
        .iter()
        .filter_map(|e| {
            let range = e.get("range")?;
            let s = range.get("start")?;
            let en = range.get("end")?;
            let sb = to_byte(s.get("line")?.as_u64()?, s.get("character")?.as_u64()?);
            let eb = to_byte(en.get("line")?.as_u64()?, en.get("character")?.as_u64()?);
            let txt = e
                .get("newText")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string();
            Some((sb, eb, txt))
        })
        .collect();
    spans.sort_by(|a, b| b.0.cmp(&a.0));

    let mut out = content.to_string();
    for (sb, eb, txt) in spans {
        if sb <= eb && eb <= out.len() {
            out.replace_range(sb..eb, &txt);
        }
    }
    out
}

/// Byte offset within `line` of the given UTF-16 code-unit offset.
fn utf16_to_byte(line: &str, utf16_off: usize) -> usize {
    let mut units = 0usize;
    for (byte_idx, ch) in line.char_indices() {
        if units >= utf16_off {
            return byte_idx;
        }
        units += ch.len_utf16();
    }
    line.len()
}

#[cfg(test)]
mod tests {
    use super::*;

    // --- abs_of: sandbox containment ---------------------------------

    #[test]
    fn abs_of_rejects_absolute_path_outside_root() {
        let root = tempfile::tempdir().unwrap();
        let err = abs_of(root.path(), "/etc/passwd").unwrap_err();
        assert!(
            err.contains("escapes the workspace root"),
            "expected escape error, got: {err}"
        );
    }

    #[test]
    fn abs_of_rejects_dotdot_escape() {
        let root = tempfile::tempdir().unwrap();
        // A sibling file outside root, reached via `../`.
        let outside = root
            .path()
            .parent()
            .unwrap()
            .join("tidepool_lsp_test_escape.rs");
        std::fs::write(&outside, "").unwrap();
        let err = abs_of(root.path(), "../tidepool_lsp_test_escape.rs").unwrap_err();
        std::fs::remove_file(&outside).ok();
        assert!(
            err.contains("escapes the workspace root"),
            "expected escape error, got: {err}"
        );
    }

    #[test]
    fn abs_of_accepts_in_root_relative_path() {
        let root = tempfile::tempdir().unwrap();
        std::fs::write(root.path().join("lib.rs"), "fn main() {}").unwrap();
        let resolved = abs_of(root.path(), "lib.rs").unwrap();
        assert_eq!(resolved, root.path().canonicalize().unwrap().join("lib.rs"));
    }

    #[test]
    fn abs_of_accepts_nested_in_root_relative_path() {
        let root = tempfile::tempdir().unwrap();
        std::fs::create_dir(root.path().join("src")).unwrap();
        std::fs::write(root.path().join("src/lib.rs"), "fn main() {}").unwrap();
        let resolved = abs_of(root.path(), "src/lib.rs").unwrap();
        assert_eq!(
            resolved,
            root.path().canonicalize().unwrap().join("src/lib.rs")
        );
    }

    #[test]
    fn abs_of_errors_on_nonexistent_path() {
        let root = tempfile::tempdir().unwrap();
        let err = abs_of(root.path(), "missing.rs").unwrap_err();
        assert!(
            err.contains("resolve"),
            "expected resolve error, got: {err}"
        );
    }

    // --- utf16_to_byte: multibyte / surrogate-pair conversion ---------

    #[test]
    fn utf16_to_byte_ascii() {
        let line = "hello world";
        // Offset 5 lands right after "hello".
        assert_eq!(utf16_to_byte(line, 5), 5);
    }

    #[test]
    fn utf16_to_byte_two_byte_char() {
        // 'é' is U+00E9: 1 UTF-16 code unit, 2 UTF-8 bytes.
        let line = "caf\u{e9}_x";
        // Offset 3 is right before 'é' — byte offset 3 (c,a,f are 1 byte each).
        assert_eq!(utf16_to_byte(line, 3), 3);
        // Offset 4 is right after 'é' — byte offset 5 (3 + 2 bytes for é).
        assert_eq!(utf16_to_byte(line, 4), 5);
        // Offset 5 is right after '_' — byte offset 6.
        assert_eq!(utf16_to_byte(line, 5), 6);
    }

    #[test]
    fn utf16_to_byte_surrogate_pair_emoji() {
        // U+1F600 (grinning face) is 2 UTF-16 code units (surrogate pair), 4 UTF-8 bytes.
        let line = "x\u{1F600}y";
        // Offset 1 is right before the emoji.
        assert_eq!(utf16_to_byte(line, 1), 1);
        // Offset 3 is right after the emoji (1 unit for 'x' + 2 units for the emoji).
        assert_eq!(utf16_to_byte(line, 3), 5);
        // Offset 4 is right after 'y'.
        assert_eq!(utf16_to_byte(line, 4), 6);
    }

    #[test]
    fn utf16_to_byte_at_eol() {
        let line = "abc";
        assert_eq!(utf16_to_byte(line, 3), 3);
    }

    #[test]
    fn utf16_to_byte_past_eof_clamps_to_line_len() {
        let line = "abc";
        // Offset beyond the line's length falls through to `line.len()`.
        assert_eq!(utf16_to_byte(line, 100), 3);
    }

    // --- apply_edits: multi-edit, out-of-order application ------------

    #[test]
    fn apply_edits_out_of_order_multi_edit() {
        let content = "line one\nline two\nline three\n";
        // Edits deliberately listed out of source order (line 2 before line 0).
        let edits = vec![
            json!({
                "range": {
                    "start": { "line": 2, "character": 5 },
                    "end": { "line": 2, "character": 10 }
                },
                "newText": "3"
            }),
            json!({
                "range": {
                    "start": { "line": 0, "character": 5 },
                    "end": { "line": 0, "character": 8 }
                },
                "newText": "1"
            }),
        ];
        let out = apply_edits(content, &edits);
        assert_eq!(out, "line 1\nline two\nline 3\n");
    }

    #[test]
    fn apply_edits_no_edits_is_identity() {
        let content = "unchanged\ntext\n";
        assert_eq!(apply_edits(content, &[]), content);
    }

    #[test]
    fn apply_edits_multibyte_line() {
        // Replace the accented character on a line with an ASCII one.
        let content = "caf\u{e9}_x\nsecond\n";
        let edits = vec![json!({
            "range": {
                "start": { "line": 0, "character": 3 },
                "end": { "line": 0, "character": 4 }
            },
            "newText": "e"
        })];
        let out = apply_edits(content, &edits);
        assert_eq!(out, "cafe_x\nsecond\n");
    }

    // --- node_sort_key / edit_touch_points: deterministic ordering ----

    #[test]
    fn node_sort_key_orders_by_file_then_line_then_char() {
        let a = node("f", "", "reference", "b.rs", 1, 0, "");
        let b = node("f", "", "reference", "a.rs", 5, 2, "");
        let c = node("f", "", "reference", "a.rs", 1, 9, "");
        let mut nodes = vec![a.clone(), b.clone(), c.clone()];
        nodes.sort_by_key(node_sort_key);
        assert_eq!(nodes, vec![c, b, a]);
    }

    #[test]
    fn edit_touch_points_from_changes_map() {
        let edit = json!({
            "changes": {
                "file:///a.rs": [
                    { "range": { "start": { "line": 2, "character": 4 }, "end": { "line": 2, "character": 4 } } }
                ]
            }
        });
        let points = edit_touch_points(&edit);
        assert_eq!(points, vec![("file:///a.rs".to_string(), 2, 4)]);
    }

    #[test]
    fn edit_touch_points_from_document_changes() {
        let edit = json!({
            "documentChanges": [
                {
                    "textDocument": { "uri": "file:///b.rs" },
                    "edits": [
                        { "range": { "start": { "line": 7, "character": 1 }, "end": { "line": 7, "character": 1 } } }
                    ]
                }
            ]
        });
        let points = edit_touch_points(&edit);
        assert_eq!(points, vec![("file:///b.rs".to_string(), 7, 1)]);
    }
}
