use std::path::PathBuf;
use tidepool_bridge_derive::{CoreRecord, FromCore, ToCore};
use tidepool_effect::dispatch::EffectContext;
use tidepool_effect::error::EffectError;
use tidepool_mcp::CapturedOutput;

// ============================================================================
// Lsp: semantic queries via the tidepool-lsp-daemon sidecar
// ============================================================================

// LspReq + DescribeEffect + EffectHandler dispatch are generated from the
// single-source definition; only the handler struct and the per-verb method
// bodies below are hand-written.
tidepool_mcp::lsp_effect_def!(crate::effect_glue::effect_rust_projection);

#[derive(FromCore, ToCore, Clone, CoreRecord)]
#[core(name = "Position")]
pub struct LspPosition {
    #[core(hs = "posLine")]
    pub line: i64,
    #[core(hs = "posChar")]
    pub character: i64,
}

#[derive(FromCore, ToCore, Clone, CoreRecord)]
#[core(name = "LspNode")]
pub struct LspNode {
    #[core(hs = "nodeName")]
    pub name: String,
    #[core(hs = "nodeContainer")]
    pub container: String,
    #[core(hs = "nodeKind")]
    pub kind: String,
    #[core(hs = "nodeFile")]
    pub file: String,
    #[core(hs = "nodePos", hs_type = "Position")]
    pub pos: LspPosition,
    #[core(hs = "nodeText")]
    pub text: String,
}

impl LspNode {
    pub fn to_wire(&self) -> serde_json::Value {
        serde_json::json!({
            "name": self.name, "container": self.container, "kind": self.kind, "file": self.file,
            "pos": { "line": self.pos.line, "char": self.pos.character }, "text": self.text,
        })
    }

    pub fn from_wire(o: &serde_json::Value) -> LspNode {
        let pos = o.get("pos");
        let line = pos
            .and_then(|p| p.get("line"))
            .and_then(serde_json::Value::as_i64)
            .unwrap_or(0);
        let character = pos
            .and_then(|p| p.get("char"))
            .and_then(serde_json::Value::as_i64)
            .unwrap_or(0);
        LspNode {
            name: json_str(o, "name"),
            container: json_str(o, "container"),
            kind: json_str(o, "kind"),
            file: json_str(o, "file"),
            pos: LspPosition { line, character },
            text: json_str(o, "text"),
        }
    }
}

#[derive(ToCore, CoreRecord)]
#[core(name = "Diag")]
pub struct LspDiag {
    #[core(hs = "diagFile")]
    pub file: String,
    #[core(hs = "diagLine")]
    pub line: i64,
    #[core(hs = "diagSeverity")]
    pub severity: String,
    #[core(hs = "diagMessage")]
    pub message: String,
}

#[derive(Clone)]
pub struct LspHandler {
    sock_path: PathBuf,
}

impl LspHandler {
    pub fn new(root: PathBuf) -> Self {
        let sock_path = std::env::var("TIDEPOOL_LSP_SOCK")
            .map(PathBuf::from)
            .unwrap_or_else(|_| root.join(".tidepool").join("lsp.sock"));
        Self { sock_path }
    }

    /// Talk to the daemon. Every failure mode here (no daemon, write/read
    /// failure, bad response, daemon-reported error) is the SAME typed
    /// failure (#335): the daemon isn't usably present right now.
    pub fn query(&self, req: serde_json::Value) -> Result<serde_json::Value, LspError> {
        use std::io::{BufRead, BufReader, Write};
        use std::os::unix::net::UnixStream;

        let stream = UnixStream::connect(&self.sock_path).map_err(|_| {
            LspError::LspDaemonDown(format!(
                "no LSP daemon at {} — start `tidepool-lsp-daemon` in the workspace \
                 (it spawns rust-analyzer and stays warm)",
                self.sock_path.display()
            ))
        })?;
        let mut writer = &stream;
        let mut line =
            serde_json::to_vec(&req).map_err(|e| LspError::LspDaemonDown(e.to_string()))?;
        line.push(b'\n');
        writer
            .write_all(&line)
            .and_then(|_| writer.flush())
            .map_err(|e| LspError::LspDaemonDown(format!("LSP daemon write failed: {}", e)))?;

        let mut resp = String::new();
        BufReader::new(&stream)
            .read_line(&mut resp)
            .map_err(|e| LspError::LspDaemonDown(format!("LSP daemon read failed: {}", e)))?;
        let v: serde_json::Value = serde_json::from_str(resp.trim())
            .map_err(|e| LspError::LspDaemonDown(format!("bad LSP daemon response: {}", e)))?;

        if v.get("ok").and_then(serde_json::Value::as_bool) == Some(true) {
            Ok(v.get("result").cloned().unwrap_or(serde_json::Value::Null))
        } else {
            Err(LspError::LspDaemonDown(
                v.get("error")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or("LSP daemon error")
                    .to_string(),
            ))
        }
    }
}

/// Human-readable render of a typed Lsp failure, for the untagged verbs
/// that still forward to the eval-abort channel.
impl std::fmt::Display for LspError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            LspError::LspDaemonDown(d) => write!(f, "{d}"),
        }
    }
}

/// Forward a typed `LspError` to the eval-abort channel, for the untagged
/// verbs (lspCallers/lspCallees/lspRefs/lspDef/lspHover/lspRename) whose
/// shared `query` helper is now `Result<_, LspError>`. For the plain-list
/// walkers this IS the "structural abort" the round-2 design calls for —
/// a daemon-down failure mid-walk kills the eval, it never surfaces as [].
fn lsp_err_to_effect(e: LspError) -> EffectError {
    EffectError::Handler(e.to_string())
}

pub fn json_str(o: &serde_json::Value, k: &str) -> String {
    o.get(k)
        .and_then(serde_json::Value::as_str)
        .unwrap_or("")
        .to_string()
}

pub fn json_line(o: &serde_json::Value) -> i64 {
    o.get("line")
        .and_then(serde_json::Value::as_i64)
        .unwrap_or(0)
}

/// Decode a daemon reply array into nodes.
fn nodes(r: &serde_json::Value) -> Vec<LspNode> {
    r.as_array()
        .into_iter()
        .flatten()
        .map(LspNode::from_wire)
        .collect()
}

impl LspHandler {
    // Errors-tagged (#335): total in `LspError`, no `cx` — the dispatch arm
    // wraps the `Result` via `cx.respond` (Ok→Right, Err→Left).
    fn lsp_where(&mut self, symbol: String) -> Result<Vec<LspNode>, LspError> {
        let r = self.query(serde_json::json!({ "op": "where", "symbol": symbol }))?;
        Ok(nodes(&r))
    }

    // Untagged, plain [LspNode] (round-2 ergonomics): a daemon-down failure
    // still aborts the eval structurally (via `lsp_err_to_effect`) — you
    // can't meaningfully continue a graph walk with no daemon. A null/empty
    // reply from the daemon (node not analyzable, or analyzable with no
    // results) collapses to the SAME `[]` here — that ambiguity is fine
    // because both mean "nothing to walk from this node", the only
    // distinction a caller composing `concatMapM lspCallers` cares about.
    fn lsp_callers(
        &mut self,
        cx: &EffectContext<'_, CapturedOutput>,
        n: LspNode,
    ) -> Result<tidepool_effect::Response, EffectError> {
        let r = self
            .query(serde_json::json!({ "op": "callers", "node": n.to_wire() }))
            .map_err(lsp_err_to_effect)?;
        cx.respond(nodes(&r))
    }

    fn lsp_callees(
        &mut self,
        cx: &EffectContext<'_, CapturedOutput>,
        n: LspNode,
    ) -> Result<tidepool_effect::Response, EffectError> {
        let r = self
            .query(serde_json::json!({ "op": "callees", "node": n.to_wire() }))
            .map_err(lsp_err_to_effect)?;
        cx.respond(nodes(&r))
    }

    fn lsp_refs(
        &mut self,
        cx: &EffectContext<'_, CapturedOutput>,
        n: LspNode,
    ) -> Result<tidepool_effect::Response, EffectError> {
        let r = self
            .query(serde_json::json!({ "op": "references", "node": n.to_wire() }))
            .map_err(lsp_err_to_effect)?;
        cx.respond(nodes(&r))
    }

    fn lsp_def(
        &mut self,
        cx: &EffectContext<'_, CapturedOutput>,
        n: LspNode,
    ) -> Result<tidepool_effect::Response, EffectError> {
        let r = self
            .query(serde_json::json!({ "op": "def", "node": n.to_wire() }))
            .map_err(lsp_err_to_effect)?;
        let opt = if r.is_null() {
            None
        } else {
            Some(LspNode::from_wire(&r))
        };
        cx.respond(opt)
    }

    fn lsp_hover(
        &mut self,
        cx: &EffectContext<'_, CapturedOutput>,
        n: LspNode,
    ) -> Result<tidepool_effect::Response, EffectError> {
        let r = self
            .query(serde_json::json!({ "op": "hover", "node": n.to_wire() }))
            .map_err(lsp_err_to_effect)?;
        cx.respond(r.as_str().map(str::to_string))
    }

    fn lsp_rename(
        &mut self,
        cx: &EffectContext<'_, CapturedOutput>,
        n: LspNode,
        new_name: String,
    ) -> Result<tidepool_effect::Response, EffectError> {
        let r = self
            .query(serde_json::json!({
                "op": "rename", "node": n.to_wire(), "newName": new_name
            }))
            .map_err(lsp_err_to_effect)?;
        cx.respond(r.as_str().map(str::to_string))
    }

    // Errors-tagged (#335): plain list, no Maybe already in play.
    fn lsp_diagnostics(&mut self, file: String) -> Result<Vec<LspDiag>, LspError> {
        let r = self.query(serde_json::json!({ "op": "diagnostics", "file": file }))?;
        let diags: Vec<LspDiag> = r
            .as_array()
            .into_iter()
            .flatten()
            .map(|o| LspDiag {
                file: json_str(o, "file"),
                line: json_line(o),
                severity: json_str(o, "severity"),
                message: json_str(o, "message"),
            })
            .collect();
        Ok(diags)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::*;
    use tidepool_bridge::{FromCore, ToCore};
    use tidepool_eval::value::Value;

    #[test]
    fn test_lsp_from_core_where() {
        let table = full_effect_test_table();
        let con_id = table.get_by_name("LspWhere").unwrap();
        let sym = "my_function".to_string().to_value(&table).unwrap();
        let val = Value::Con(con_id, vec![sym]);
        let req = LspReq::from_value(&val, &table).unwrap();
        assert!(matches!(req, LspReq::LspWhere(ref s) if s == "my_function"));
    }

    #[test]
    fn test_lsp_from_core_diagnostics() {
        let table = full_effect_test_table();
        let con_id = table.get_by_name("LspDiagnostics").unwrap();
        let file = "src/main.rs".to_string().to_value(&table).unwrap();
        let val = Value::Con(con_id, vec![file]);
        let req = LspReq::from_value(&val, &table).unwrap();
        assert!(matches!(req, LspReq::LspDiagnostics(ref f) if f == "src/main.rs"));
    }

    fn extract_available() -> bool {
        let bin =
            std::env::var("TIDEPOOL_EXTRACT").unwrap_or_else(|_| "tidepool-extract".to_string());
        std::process::Command::new(&bin)
            .arg("--help")
            .output()
            .is_ok()
    }

    /// #335 end-to-end acceptance: with no `tidepool-lsp-daemon` reachable, the
    /// socket connect fails immediately (cheap, no live dependency), so
    /// `lspWhere` is a typed `Left (LspDaemonDown _)` the eval pattern-matches
    /// — never an abort. Skips cleanly when TIDEPOOL_EXTRACT is unavailable.
    /// The Lsp handler gets an ISOLATED cwd (no `.tidepool/lsp.sock`) so a dev
    /// daemon legitimately running at the repo root can't turn this into a
    /// live query.
    #[tokio::test]
    async fn lsp_where_no_daemon_is_typed_left_lspdaemondown() {
        if !extract_available() {
            eprintln!("skipping: tidepool-extract not available (set TIDEPOOL_EXTRACT)");
            return;
        }
        let decls = tidepool_mcp::standard_decls();
        let source = jit_test_source(&[
            "r <- lspWhere \"definitely_not_a_real_symbol_335\"",
            "pure (case r of { Left (LspDaemonDown _) -> (\"daemondown\" :: Text); Left _ -> \"other\"; Right _ -> \"ok\" })",
        ]);
        let include = prelude_include();
        let effects_dir = tidepool_mcp::ensure_effects_module(&decls).unwrap();
        let include_paths: Vec<&std::path::Path> = vec![include.as_path(), effects_dir.as_path()];
        let kv_path = std::env::temp_dir().join("tidepool_lsp_jit_kv_nodaemon.json");
        let cwd = repo_root();
        let lsp_cwd = std::env::temp_dir().join("tidepool_lsp_nodaemon_cwd");
        std::fs::create_dir_all(&lsp_cwd).unwrap();
        let captured = CapturedOutput::new();
        let mut handlers = frunk::hlist![
            crate::ConsoleHandler,
            crate::KvHandler::new(kv_path),
            crate::FsHandler::new(cwd.clone()),
            crate::HttpHandler,
            crate::ExecHandler::new(cwd.clone()),
            LspHandler::new(lsp_cwd),
            crate::LlmHandler::new("ollama:llama3.2".to_string()),
            crate::GitHandler::new(cwd.clone()),
            crate::TimeHandler,
        ];
        let result = tidepool_runtime::compile_and_run(
            &source,
            "result",
            &include_paths,
            &mut handlers,
            &captured,
        );
        match result {
            Ok(v) => assert_eq!(v.to_json(), serde_json::json!("daemondown")),
            Err(e) => panic!("JIT lspWhere eval failed: {:?}", e),
        }
    }
}
