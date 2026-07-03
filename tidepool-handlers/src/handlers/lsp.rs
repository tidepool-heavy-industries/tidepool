use std::path::PathBuf;
use tidepool_bridge_derive::{CoreRecord, FromCore, ToCore};
use tidepool_effect::dispatch::{EffectContext, EffectHandler};
use tidepool_effect::error::EffectError;
use tidepool_mcp::{CapturedOutput, DescribeEffect, EffectDecl};

// ============================================================================
// Lsp: semantic queries via the tidepool-lsp-daemon sidecar
// ============================================================================

#[derive(FromCore)]
pub enum LspReq {
    #[core(name = "LspWhere")]
    Where(String),
    #[core(name = "LspCallers")]
    Callers(LspNode),
    #[core(name = "LspCallees")]
    Callees(LspNode),
    #[core(name = "LspRefs")]
    Refs(LspNode),
    #[core(name = "LspDef")]
    Def(LspNode),
    #[core(name = "LspHover")]
    Hover(LspNode),
    #[core(name = "LspRename")]
    Rename(LspNode, String),
    #[core(name = "LspDiagnostics")]
    Diagnostics(String),
}

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

    pub fn query(&self, req: serde_json::Value) -> Result<serde_json::Value, EffectError> {
        use std::io::{BufRead, BufReader, Write};
        use std::os::unix::net::UnixStream;

        let stream = UnixStream::connect(&self.sock_path).map_err(|_| {
            EffectError::Handler(format!(
                "no LSP daemon at {} — start `tidepool-lsp-daemon` in the workspace \
                 (it spawns rust-analyzer and stays warm)",
                self.sock_path.display()
            ))
        })?;
        let mut writer = &stream;
        let mut line = serde_json::to_vec(&req).map_err(|e| EffectError::Handler(e.to_string()))?;
        line.push(b'\n');
        writer
            .write_all(&line)
            .and_then(|_| writer.flush())
            .map_err(|e| EffectError::Handler(format!("LSP daemon write failed: {}", e)))?;

        let mut resp = String::new();
        BufReader::new(&stream)
            .read_line(&mut resp)
            .map_err(|e| EffectError::Handler(format!("LSP daemon read failed: {}", e)))?;
        let v: serde_json::Value = serde_json::from_str(resp.trim())
            .map_err(|e| EffectError::Handler(format!("bad LSP daemon response: {}", e)))?;

        if v.get("ok").and_then(serde_json::Value::as_bool) == Some(true) {
            Ok(v.get("result").cloned().unwrap_or(serde_json::Value::Null))
        } else {
            Err(EffectError::Handler(
                v.get("error")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or("LSP daemon error")
                    .to_string(),
            ))
        }
    }
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

impl DescribeEffect for LspHandler {
    fn effect_decl() -> EffectDecl {
        tidepool_mcp::lsp_decl()
    }
}

impl EffectHandler<CapturedOutput> for LspHandler {
    type Request = LspReq;
    fn handle(
        &mut self,
        req: LspReq,
        cx: &EffectContext<'_, CapturedOutput>,
    ) -> Result<tidepool_effect::Response, EffectError> {
        let nodes = |r: &serde_json::Value| -> Vec<LspNode> {
            r.as_array()
                .into_iter()
                .flatten()
                .map(LspNode::from_wire)
                .collect()
        };
        let maybe_nodes = |r: &serde_json::Value| -> Option<Vec<LspNode>> {
            if r.is_null() {
                None
            } else {
                Some(nodes(r))
            }
        };
        match req {
            LspReq::Where(symbol) => {
                let r = self.query(serde_json::json!({ "op": "where", "symbol": symbol }))?;
                cx.respond_list(nodes(&r))
            }
            LspReq::Callers(n) => {
                let r = self.query(serde_json::json!({ "op": "callers", "node": n.to_wire() }))?;
                cx.respond(maybe_nodes(&r))
            }
            LspReq::Callees(n) => {
                let r = self.query(serde_json::json!({ "op": "callees", "node": n.to_wire() }))?;
                cx.respond(maybe_nodes(&r))
            }
            LspReq::Refs(n) => {
                let r =
                    self.query(serde_json::json!({ "op": "references", "node": n.to_wire() }))?;
                cx.respond(maybe_nodes(&r))
            }
            LspReq::Def(n) => {
                let r = self.query(serde_json::json!({ "op": "def", "node": n.to_wire() }))?;
                let opt = if r.is_null() {
                    None
                } else {
                    Some(LspNode::from_wire(&r))
                };
                cx.respond(opt)
            }
            LspReq::Hover(n) => {
                let r = self.query(serde_json::json!({ "op": "hover", "node": n.to_wire() }))?;
                cx.respond(r.as_str().map(str::to_string))
            }
            LspReq::Rename(n, new_name) => {
                let r = self.query(serde_json::json!({
                    "op": "rename", "node": n.to_wire(), "newName": new_name
                }))?;
                cx.respond(r.as_str().map(str::to_string))
            }
            LspReq::Diagnostics(file) => {
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
                cx.respond_list(diags)
            }
        }
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
        assert!(matches!(req, LspReq::Where(ref s) if s == "my_function"));
    }

    #[test]
    fn test_lsp_from_core_diagnostics() {
        let table = full_effect_test_table();
        let con_id = table.get_by_name("LspDiagnostics").unwrap();
        let file = "src/main.rs".to_string().to_value(&table).unwrap();
        let val = Value::Con(con_id, vec![file]);
        let req = LspReq::from_value(&val, &table).unwrap();
        assert!(matches!(req, LspReq::Diagnostics(ref f) if f == "src/main.rs"));
    }
}
