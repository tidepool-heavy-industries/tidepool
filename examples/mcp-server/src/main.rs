use std::collections::HashMap;
use std::path::PathBuf;
use std::process::Command;
use std::sync::{Arc, Mutex};
use tidepool_bridge_derive::{FromCore, ToCore};
use tidepool_effect::dispatch::{EffectContext, EffectHandler};
use tidepool_effect::error::EffectError;
use tidepool_eval::value::Value;
use tidepool_mcp::{CapturedOutput, DescribeEffect, EffectDecl, TidepoolMcpServer};

// === Tag 0: Console ===

#[derive(FromCore)]
enum ConsoleReq {
    #[core(name = "Print")]
    Print(String),
}

#[derive(Clone)]
struct ConsoleHandler;

impl DescribeEffect for ConsoleHandler {
    fn effect_decl() -> EffectDecl {
        EffectDecl {
            type_name: "Console",
            description: "Print text output.",
            constructors: &["Print :: Text -> Console ()"],
            type_defs: &[],
        }
    }
}

impl EffectHandler<CapturedOutput> for ConsoleHandler {
    type Request = ConsoleReq;
    fn handle(&mut self, req: ConsoleReq, cx: &EffectContext<'_, CapturedOutput>) -> Result<Value, EffectError> {
        match req {
            ConsoleReq::Print(s) => {
                cx.user().push(s);
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

impl DescribeEffect for KvHandler {
    fn effect_decl() -> EffectDecl {
        EffectDecl {
            type_name: "KV",
            description: "Persistent key-value store. State survives across calls within one server session.",
            constructors: &[
                "KvGet :: Text -> KV (Maybe Text)",
                "KvSet :: Text -> Text -> KV ()",
                "KvDelete :: Text -> KV ()",
                "KvKeys :: KV [Text]",
            ],
            type_defs: &[],
        }
    }
}

impl EffectHandler<CapturedOutput> for KvHandler {
    type Request = KvReq;
    fn handle(&mut self, req: KvReq, cx: &EffectContext<'_, CapturedOutput>) -> Result<Value, EffectError> {
        let mut store = self
            .store
            .lock()
            .map_err(|e| EffectError::Handler(format!("Mutex poisoned: {}", e)))?;
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
        let canonical_root = self
            .root
            .canonicalize()
            .map_err(|e| EffectError::Handler(e.to_string()))?;
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
        Ok(check_path)
    }
}

impl DescribeEffect for FsHandler {
    fn effect_decl() -> EffectDecl {
        EffectDecl {
            type_name: "Fs",
            description: "Read and write files (sandboxed to server working directory).",
            constructors: &[
                "FsRead :: Text -> Fs Text",
                "FsWrite :: Text -> Text -> Fs ()",
            ],
            type_defs: &[],
        }
    }
}

impl EffectHandler<CapturedOutput> for FsHandler {
    type Request = FsReq;
    fn handle(&mut self, req: FsReq, cx: &EffectContext<'_, CapturedOutput>) -> Result<Value, EffectError> {
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

// === Tag 3: Structural Grep (ast-grep) ===

#[derive(Clone, Copy, FromCore)]
enum Lang {
    #[core(name = "Rust")]       Rust,
    #[core(name = "Python")]     Python,
    #[core(name = "TypeScript")] TypeScript,
    #[core(name = "JavaScript")] JavaScript,
    #[core(name = "Go")]         Go,
    #[core(name = "Java")]       Java,
    #[core(name = "C")]          C,
    #[core(name = "Cpp")]        Cpp,
    #[core(name = "Haskell")]    Haskell,
    #[core(name = "Nix")]        Nix,
    #[core(name = "Html")]       Html,
    #[core(name = "Css")]        Css,
    #[core(name = "Json")]       Json,
    #[core(name = "Yaml")]       Yaml,
    #[core(name = "Toml")]       Toml,
}

impl Lang {
    fn as_str(&self) -> &'static str {
        match self {
            Lang::Rust => "rust",
            Lang::Python => "python",
            Lang::TypeScript => "typescript",
            Lang::JavaScript => "javascript",
            Lang::Go => "go",
            Lang::Java => "java",
            Lang::C => "c",
            Lang::Cpp => "cpp",
            Lang::Haskell => "haskell",
            Lang::Nix => "nix",
            Lang::Html => "html",
            Lang::Css => "css",
            Lang::Json => "json",
            Lang::Yaml => "yaml",
            Lang::Toml => "toml",
        }
    }
}

#[derive(FromCore)]
enum SgReq {
    #[core(name = "SgFind")]
    Find(Lang, String, Vec<String>),
    #[core(name = "SgPreview")]
    Preview(Lang, String, String, Vec<String>),
    #[core(name = "SgReplace")]
    Replace(Lang, String, String, Vec<String>),
}

/// Rust-side Match value returned to Haskell.
/// Field order must match the Haskell data constructor:
///   Match { mText, mFile, mLine, mVars, mReplacement }
#[derive(ToCore)]
#[core(name = "Match")]
struct SgMatch {
    text: String,
    file: String,
    line: i64,
    vars: Vec<(String, String)>,
    replacement: String,
}

// ast-grep JSON output structures

#[derive(serde::Deserialize)]
struct AstGrepMatch {
    text: String,
    file: String,
    range: AstGrepRange,
    #[serde(default)]
    replacement: Option<String>,
    #[serde(default, rename = "metaVariables")]
    meta_variables: AstGrepMetaVars,
}

#[derive(serde::Deserialize, Default)]
struct AstGrepMetaVars {
    #[serde(default)]
    single: HashMap<String, AstGrepCapture>,
    #[serde(default)]
    multi: HashMap<String, Vec<AstGrepCapture>>,
}

#[derive(serde::Deserialize)]
struct AstGrepCapture {
    text: String,
}

#[derive(serde::Deserialize)]
struct AstGrepRange {
    start: AstGrepPos,
}

#[derive(serde::Deserialize)]
struct AstGrepPos {
    line: i64,
}

#[derive(Clone)]
struct SgHandler {
    root: PathBuf,
}

impl SgHandler {
    fn new(root: PathBuf) -> Self {
        Self { root }
    }

    fn run_find(
        &self,
        lang: Lang,
        pattern: &str,
        paths: &[String],
        rewrite: Option<&str>,
    ) -> Result<Vec<SgMatch>, EffectError> {
        let mut cmd = Command::new("ast-grep");
        cmd.arg("run")
            .arg("--pattern").arg(pattern)
            .arg("--lang").arg(lang.as_str())
            .arg("--json=compact");
        if let Some(rw) = rewrite {
            cmd.arg("--rewrite").arg(rw);
        }
        for p in paths {
            cmd.arg(self.root.join(p));
        }
        cmd.current_dir(&self.root);

        let output = cmd.output()
            .map_err(|e| EffectError::Handler(format!("ast-grep exec failed: {}", e)))?;

        // exit code 1 = no matches
        if !output.status.success() && output.status.code() != Some(1) {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(EffectError::Handler(format!("ast-grep error: {}", stderr)));
        }

        let stdout = String::from_utf8_lossy(&output.stdout);
        if stdout.trim().is_empty() {
            return Ok(Vec::new());
        }

        let raw: Vec<AstGrepMatch> = serde_json::from_str(&stdout)
            .map_err(|e| EffectError::Handler(format!("ast-grep JSON parse: {}", e)))?;

        Ok(raw.into_iter().map(|m| {
            let mut vars: Vec<(String, String)> = m.meta_variables.single.into_iter()
                .map(|(k, v)| (k, v.text))
                .collect();
            // Include multi-captures ($$$ vars) as joined text
            for (k, captures) in m.meta_variables.multi {
                let joined: String = captures.into_iter().map(|c| c.text).collect::<Vec<_>>().join("\n");
                vars.push((k, joined));
            }
            vars.sort_by(|a, b| a.0.cmp(&b.0));
            // Make file path relative to root
            let file = m.file.strip_prefix(self.root.to_str().unwrap_or(""))
                .unwrap_or(&m.file)
                .trim_start_matches('/')
                .to_string();
            SgMatch {
                text: m.text,
                file,
                line: m.range.start.line + 1, // ast-grep is 0-indexed, Haskell convention is 1-indexed
                vars,
                replacement: m.replacement.unwrap_or_default(),
            }
        }).collect())
    }

    fn run_replace(
        &self,
        lang: Lang,
        pattern: &str,
        rewrite: &str,
        paths: &[String],
    ) -> Result<i64, EffectError> {
        // First do a find to count matches
        let matches = self.run_find(lang.clone(), pattern, paths, Some(rewrite))?;
        let count = matches.len() as i64;
        if count == 0 {
            return Ok(0);
        }

        // Now apply with --update-all
        let mut cmd = Command::new("ast-grep");
        cmd.arg("run")
            .arg("--pattern").arg(pattern)
            .arg("--rewrite").arg(rewrite)
            .arg("--lang").arg(lang.as_str())
            .arg("--update-all");
        for p in paths {
            cmd.arg(self.root.join(p));
        }
        cmd.current_dir(&self.root);

        let output = cmd.output()
            .map_err(|e| EffectError::Handler(format!("ast-grep exec failed: {}", e)))?;

        if !output.status.success() && output.status.code() != Some(1) {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(EffectError::Handler(format!("ast-grep replace error: {}", stderr)));
        }

        Ok(count)
    }
}

impl DescribeEffect for SgHandler {
    fn effect_decl() -> EffectDecl {
        EffectDecl {
            type_name: "SG",
            description: concat!(
                "Structural code search and rewrite via ast-grep. ",
                "Use patterns with $VAR for single-node captures and $$$VAR for multi-node. ",
                "Paths are relative to server working directory.",
            ),
            type_defs: &[
                "data Lang = Rust | Python | TypeScript | JavaScript | Go | Java | C | Cpp | Haskell | Nix | Html | Css | Json | Yaml | Toml",
                "data Match = Match { mText :: Text, mFile :: Text, mLine :: Int, mVars :: [(Text, Text)], mReplacement :: Text }",
                "var :: Match -> Text -> Text",
                "var (Match _ _ _ vs _) k = case [v | (k', v) <- vs, k' == k] of { (x:_) -> x; _ -> \"\" }",
            ],
            constructors: &[
                "SgFind :: Lang -> Text -> [Text] -> SG [Match]",
                "SgPreview :: Lang -> Text -> Text -> [Text] -> SG [Match]",
                "SgReplace :: Lang -> Text -> Text -> [Text] -> SG Int",
            ],
        }
    }
}

impl EffectHandler<CapturedOutput> for SgHandler {
    type Request = SgReq;
    fn handle(&mut self, req: SgReq, cx: &EffectContext<'_, CapturedOutput>) -> Result<Value, EffectError> {
        match req {
            SgReq::Find(lang, pattern, paths) => {
                let matches = self.run_find(lang, &pattern, &paths, None)?;
                cx.respond(matches)
            }
            SgReq::Preview(lang, pattern, rewrite, paths) => {
                let matches = self.run_find(lang, &pattern, &paths, Some(&rewrite))?;
                cx.respond(matches)
            }
            SgReq::Replace(lang, pattern, rewrite, paths) => {
                let count = self.run_replace(lang, &pattern, &rewrite, &paths)?;
                cx.respond(count)
            }
        }
    }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("warn")),
        )
        .with_writer(std::io::stderr)
        .init();

    let cwd = std::env::current_dir()?;
    let handlers = frunk::hlist![ConsoleHandler, KvHandler::new(), FsHandler::new(cwd.clone()), SgHandler::new(cwd.clone())];

    // Prelude lives at haskell/lib/ relative to repo root, or via TIDEPOOL_PRELUDE_DIR.
    // Try haskell/lib first (running from repo root), fall back to lib/ (running from haskell/).
    let prelude_fallback = {
        let from_root = cwd.join("haskell").join("lib");
        if from_root.join("Tidepool").join("Prelude.hs").exists() {
            from_root
        } else {
            cwd.join("lib")
        }
    };
    let server = TidepoolMcpServer::new(handlers).with_prelude(prelude_fallback);
    server.serve_stdio().await
}
