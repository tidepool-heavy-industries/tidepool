use std::collections::HashMap;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use ast_grep_config::{DeserializeEnv, SerializableRule};
use ast_grep_core::{Language as _, Pattern};
use ast_grep_language::{LanguageExt, SupportLang};
use rmcp::{model::*, service::RequestContext, ErrorData as McpError, RoleServer, ServerHandler};
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
        tidepool_mcp::console_decl()
    }
}

impl EffectHandler<CapturedOutput> for ConsoleHandler {
    type Request = ConsoleReq;
    fn handle(
        &mut self,
        req: ConsoleReq,
        cx: &EffectContext<'_, CapturedOutput>,
    ) -> Result<Value, EffectError> {
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
    store: Arc<Mutex<HashMap<String, serde_json::Value>>>,
    path: PathBuf,
}

impl KvHandler {
    fn new(path: PathBuf) -> Self {
        let store = if path.exists() {
            match std::fs::read_to_string(&path) {
                Ok(contents) => serde_json::from_str(&contents).unwrap_or_default(),
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
                eprintln!(
                    "[tidepool] KV flush: failed to create dir {:?}: {}",
                    parent, e
                );
                return;
            }
        }
        match serde_json::to_string_pretty(store) {
            Ok(json) => {
                if let Err(e) = std::fs::write(&self.path, json) {
                    eprintln!(
                        "[tidepool] KV flush: failed to write {:?}: {}",
                        self.path, e
                    );
                }
            }
            Err(e) => {
                eprintln!("[tidepool] KV flush: serialization failed: {}", e);
            }
        }
    }
}

impl DescribeEffect for KvHandler {
    fn effect_decl() -> EffectDecl {
        tidepool_mcp::kv_decl()
    }
}

impl EffectHandler<CapturedOutput> for KvHandler {
    type Request = KvReq;
    fn handle(
        &mut self,
        req: KvReq,
        cx: &EffectContext<'_, CapturedOutput>,
    ) -> Result<Value, EffectError> {
        let mut store = self
            .store
            .lock()
            .map_err(|e| EffectError::Handler(format!("Mutex poisoned: {}", e)))?;
        match req {
            KvReq::Get(key) => {
                let val: Option<serde_json::Value> = store.get(&key).cloned();
                cx.respond(val)
            }
            KvReq::Set(key, val) => {
                let json_val = tidepool_runtime::value_to_json(&val, cx.table(), 0);
                store.insert(key, json_val);
                self.flush(&store);
                cx.respond(())
            }
            KvReq::Delete(key) => {
                store.remove(&key);
                self.flush(&store);
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
    #[core(name = "FsListDir")]
    ListDir(String),
    #[core(name = "FsGlob")]
    Glob(String),
    #[core(name = "FsExists")]
    Exists(String),
    #[core(name = "FsMetadata")]
    Metadata(String),
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
        tidepool_mcp::fs_decl()
    }
}

impl EffectHandler<CapturedOutput> for FsHandler {
    type Request = FsReq;
    fn handle(
        &mut self,
        req: FsReq,
        cx: &EffectContext<'_, CapturedOutput>,
    ) -> Result<Value, EffectError> {
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
            FsReq::ListDir(path) => {
                let resolved = self.resolve(&path)?;
                let mut entries: Vec<String> = std::fs::read_dir(&resolved)
                    .map_err(|e| EffectError::Handler(e.to_string()))?
                    .filter_map(|e| e.ok())
                    .map(|e| e.file_name().to_string_lossy().to_string())
                    .collect();
                entries.sort();
                cx.respond(entries)
            }
            FsReq::Glob(pattern) => {
                let full_pattern = self.root.join(&pattern).to_string_lossy().to_string();
                let paths: Vec<String> = glob::glob(&full_pattern)
                    .map_err(|e| EffectError::Handler(format!("invalid glob: {}", e)))?
                    .filter_map(|e| e.ok())
                    .filter_map(|p| {
                        p.strip_prefix(&self.root)
                            .ok()
                            .map(|r| r.to_string_lossy().to_string())
                    })
                    .collect();
                cx.respond(paths)
            }
            FsReq::Exists(path) => {
                let resolved = self.resolve(&path)?;
                cx.respond(resolved.exists())
            }
            FsReq::Metadata(path) => {
                let resolved = self.resolve(&path)?;
                let meta = std::fs::metadata(&resolved)
                    .map_err(|e| EffectError::Handler(e.to_string()))?;
                cx.respond((meta.len() as i64, meta.is_file(), meta.is_dir()))
            }
        }
    }
}

// === Tag 3: Structural Grep (ast-grep) ===

#[derive(Clone, Copy, FromCore)]
enum Lang {
    #[core(name = "Rust")]
    Rust,
    #[core(name = "Python")]
    Python,
    #[core(name = "TypeScript")]
    TypeScript,
    #[core(name = "JavaScript")]
    JavaScript,
    #[core(name = "Go")]
    Go,
    #[core(name = "Java")]
    Java,
    #[core(name = "C")]
    C,
    #[core(name = "Cpp")]
    Cpp,
    #[core(name = "Haskell")]
    Haskell,
    #[core(name = "Nix")]
    Nix,
    #[core(name = "Html")]
    Html,
    #[core(name = "Css")]
    Css,
    #[core(name = "Json")]
    Json,
    #[core(name = "Yaml")]
    Yaml,
    #[core(name = "Toml")]
    Toml,
}

impl Lang {
    fn to_support_lang(self) -> Result<SupportLang, EffectError> {
        match self {
            Lang::Rust => Ok(SupportLang::Rust),
            Lang::Python => Ok(SupportLang::Python),
            Lang::TypeScript => Ok(SupportLang::TypeScript),
            Lang::JavaScript => Ok(SupportLang::JavaScript),
            Lang::Go => Ok(SupportLang::Go),
            Lang::Java => Ok(SupportLang::Java),
            Lang::C => Ok(SupportLang::C),
            Lang::Cpp => Ok(SupportLang::Cpp),
            Lang::Haskell => Ok(SupportLang::Haskell),
            Lang::Nix => Ok(SupportLang::Nix),
            Lang::Html => Ok(SupportLang::Html),
            Lang::Css => Ok(SupportLang::Css),
            Lang::Json => Ok(SupportLang::Json),
            Lang::Yaml => Ok(SupportLang::Yaml),
            Lang::Toml => Err(EffectError::Handler(
                "Toml is not supported by ast-grep".into(),
            )),
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
    #[core(name = "SgRuleFind")]
    RuleFind(Lang, Value, Vec<String>),
    #[core(name = "SgRuleReplace")]
    RuleReplace(Lang, Value, String, Vec<String>),
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

#[derive(Clone)]
struct SgHandler {
    root: PathBuf,
}

impl SgHandler {
    fn new(root: PathBuf) -> Self {
        Self { root }
    }

    fn collect_files(
        &self,
        lang: SupportLang,
        paths: &[String],
    ) -> Result<Vec<PathBuf>, EffectError> {
        let mut files = Vec::new();
        if paths.is_empty() {
            self.walk_dir(&self.root, lang, &mut files)?;
        } else {
            for p in paths {
                let full = self.root.join(p);
                if full.is_file() {
                    files.push(full);
                } else if full.is_dir() {
                    self.walk_dir(&full, lang, &mut files)?;
                }
            }
        }
        Ok(files)
    }

    fn walk_dir(
        &self,
        dir: &std::path::Path,
        lang: SupportLang,
        files: &mut Vec<PathBuf>,
    ) -> Result<(), EffectError> {
        let entries = std::fs::read_dir(dir)
            .map_err(|e| EffectError::Handler(format!("read_dir {}: {}", dir.display(), e)))?;
        for entry in entries {
            let entry = entry.map_err(|e| EffectError::Handler(e.to_string()))?;
            let path = entry.path();
            if path.is_dir() {
                let name = path
                    .file_name()
                    .map(|n| n.to_string_lossy())
                    .unwrap_or_default();
                if name.starts_with('.') || matches!(name.as_ref(), "target" | "node_modules") {
                    continue;
                }
                self.walk_dir(&path, lang, files)?;
            } else if SupportLang::from_path(&path) == Some(lang) {
                files.push(path);
            }
        }
        Ok(())
    }

    fn run_find(
        &self,
        lang: Lang,
        pattern: &str,
        paths: &[String],
        rewrite: Option<&str>,
    ) -> Result<Vec<SgMatch>, EffectError> {
        let sl = lang.to_support_lang()?;
        let pat = Pattern::try_new(pattern, sl)
            .map_err(|e| EffectError::Handler(format!("invalid pattern: {}", e)))?;
        let files = self.collect_files(sl, paths)?;
        let mut results = Vec::new();

        for file_path in files {
            let source = std::fs::read_to_string(&file_path)
                .map_err(|e| EffectError::Handler(e.to_string()))?;
            let grep = sl.ast_grep(&source);
            let relative = file_path
                .strip_prefix(&self.root)
                .unwrap_or(&file_path)
                .to_string_lossy()
                .to_string();

            for m in grep.root().find_all(&pat) {
                let text = m.text().to_string();
                let line = m.start_pos().line() as i64 + 1;
                let env: HashMap<String, String> = m.get_env().clone().into();
                let mut vars: Vec<(String, String)> = env.into_iter().collect();
                vars.sort_by(|a, b| a.0.cmp(&b.0));

                let replacement = if let Some(rw) = rewrite {
                    let edit = m.replace_by(rw);
                    String::from_utf8_lossy(&edit.inserted_text).to_string()
                } else {
                    String::new()
                };

                results.push(SgMatch {
                    text,
                    file: relative.clone(),
                    line,
                    vars,
                    replacement,
                });
            }
        }
        Ok(results)
    }

    fn run_replace(
        &self,
        lang: Lang,
        pattern: &str,
        rewrite: &str,
        paths: &[String],
    ) -> Result<i64, EffectError> {
        let sl = lang.to_support_lang()?;
        let files = self.collect_files(sl, paths)?;
        let mut total = 0i64;

        for file_path in files {
            let source = std::fs::read_to_string(&file_path)
                .map_err(|e| EffectError::Handler(e.to_string()))?;
            let mut grep = sl.ast_grep(&source);
            let mut file_count = 0i64;

            loop {
                let pat = Pattern::try_new(pattern, sl)
                    .map_err(|e| EffectError::Handler(format!("invalid pattern: {}", e)))?;
                match grep.replace(pat, rewrite) {
                    Ok(true) => file_count += 1,
                    Ok(false) => break,
                    Err(e) => return Err(EffectError::Handler(e)),
                }
            }

            if file_count > 0 {
                let modified = grep.generate();
                std::fs::write(&file_path, &modified)
                    .map_err(|e| EffectError::Handler(e.to_string()))?;
                total += file_count;
            }
        }
        Ok(total)
    }

    fn deserialize_rule(
        &self,
        lang: Lang,
        rule_json: &Value,
        table: &tidepool_repr::DataConTable,
    ) -> Result<(SupportLang, ast_grep_config::Rule), EffectError> {
        let sl = lang.to_support_lang()?;
        let json_val = tidepool_runtime::value_to_json(rule_json, table, 0);
        let serializable: SerializableRule = serde_json::from_value(json_val)
            .map_err(|e| EffectError::Handler(format!("invalid rule JSON: {}", e)))?;
        let env = DeserializeEnv::new(sl);
        let rule = env
            .deserialize_rule(serializable)
            .map_err(|e| EffectError::Handler(format!("invalid rule: {}", e)))?;
        Ok((sl, rule))
    }

    fn run_rule_find(
        &self,
        lang: Lang,
        rule_json: &Value,
        paths: &[String],
        rewrite: Option<&str>,
        table: &tidepool_repr::DataConTable,
    ) -> Result<Vec<SgMatch>, EffectError> {
        let (sl, rule) = self.deserialize_rule(lang, rule_json, table)?;
        let files = self.collect_files(sl, paths)?;
        let mut results = Vec::new();

        for file_path in files {
            let source = std::fs::read_to_string(&file_path)
                .map_err(|e| EffectError::Handler(e.to_string()))?;
            let grep = sl.ast_grep(&source);
            let relative = file_path
                .strip_prefix(&self.root)
                .unwrap_or(&file_path)
                .to_string_lossy()
                .to_string();

            for m in grep.root().find_all(&rule) {
                let text = m.text().to_string();
                let line = m.start_pos().line() as i64 + 1;
                let env: HashMap<String, String> = m.get_env().clone().into();
                let mut vars: Vec<(String, String)> = env.into_iter().collect();
                vars.sort_by(|a, b| a.0.cmp(&b.0));

                let replacement = if let Some(rw) = rewrite {
                    let edit = m.replace_by(rw);
                    String::from_utf8_lossy(&edit.inserted_text).to_string()
                } else {
                    String::new()
                };

                results.push(SgMatch {
                    text,
                    file: relative.clone(),
                    line,
                    vars,
                    replacement,
                });
            }
        }
        Ok(results)
    }

    fn run_rule_replace(
        &self,
        lang: Lang,
        rule_json: &Value,
        rewrite: &str,
        paths: &[String],
        table: &tidepool_repr::DataConTable,
    ) -> Result<i64, EffectError> {
        let (sl, rule) = self.deserialize_rule(lang, rule_json, table)?;
        let files = self.collect_files(sl, paths)?;
        let mut total = 0i64;

        for file_path in files {
            let source = std::fs::read_to_string(&file_path)
                .map_err(|e| EffectError::Handler(e.to_string()))?;
            let grep = sl.ast_grep(&source);
            let edits = grep.root().replace_all(&rule, rewrite);

            if !edits.is_empty() {
                total += edits.len() as i64;
                let mut new_source = source.clone();
                // Apply edits in reverse order to preserve positions
                let mut sorted_edits = edits;
                sorted_edits.sort_by(|a, b| b.position.cmp(&a.position));
                for edit in sorted_edits {
                    let start = edit.position;
                    let end = start + edit.deleted_length;
                    let replacement = String::from_utf8_lossy(&edit.inserted_text);
                    new_source.replace_range(start..end, &replacement);
                }
                std::fs::write(&file_path, &new_source)
                    .map_err(|e| EffectError::Handler(e.to_string()))?;
            }
        }
        Ok(total)
    }
}

impl DescribeEffect for SgHandler {
    fn effect_decl() -> EffectDecl {
        tidepool_mcp::sg_decl()
    }
}

impl EffectHandler<CapturedOutput> for SgHandler {
    type Request = SgReq;
    fn handle(
        &mut self,
        req: SgReq,
        cx: &EffectContext<'_, CapturedOutput>,
    ) -> Result<Value, EffectError> {
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
            SgReq::RuleFind(lang, rule_json, paths) => {
                let matches = self.run_rule_find(lang, &rule_json, &paths, None, cx.table())?;
                cx.respond(matches)
            }
            SgReq::RuleReplace(lang, rule_json, rewrite, paths) => {
                let count =
                    self.run_rule_replace(lang, &rule_json, &rewrite, &paths, cx.table())?;
                cx.respond(count)
            }
        }
    }
}

// === Tag 4: Http ===

#[derive(FromCore)]
enum HttpReq {
    #[core(name = "HttpGet")]
    Get(String),
    #[core(name = "HttpPost")]
    Post(String, Value),
    #[core(name = "HttpRequest")]
    Request(String, String, Vec<(String, String)>, String),
}

#[derive(Clone)]
struct HttpHandler;

impl HttpHandler {
    fn validate_url(url_str: &str) -> Result<url::Url, EffectError> {
        let url = url::Url::parse(url_str)
            .map_err(|e| EffectError::Handler(format!("Invalid URL '{}': {}", url_str, e)))?;

        if url.scheme() != "http" && url.scheme() != "https" {
            return Err(EffectError::Handler(format!(
                "Unsupported protocol '{}'. Only http/https allowed.",
                url.scheme()
            )));
        }

        if let Some(host) = url.host() {
            match host {
                url::Host::Ipv4(ip) => {
                    if ip.is_loopback() || ip.is_private() || ip.is_link_local() {
                        return Err(EffectError::Handler(format!(
                            "Access to internal IP '{}' is restricted.",
                            ip
                        )));
                    }
                }
                url::Host::Ipv6(ip) => {
                    if ip.is_loopback() || ip.is_unspecified() {
                        return Err(EffectError::Handler(format!(
                            "Access to internal IP '{}' is restricted.",
                            ip
                        )));
                    }
                }
                url::Host::Domain(domain) => {
                    if domain == "localhost" {
                        return Err(EffectError::Handler(
                            "Access to 'localhost' is restricted.".into(),
                        ));
                    }
                }
            }
        }

        Ok(url)
    }

    fn parse_response(_url_str: &str, body: &str) -> Result<serde_json::Value, EffectError> {
        serde_json::from_str(body).or_else(|_| {
            // If not valid JSON, wrap as string
            Ok(serde_json::Value::String(body.to_string()))
        })
    }
}

impl DescribeEffect for HttpHandler {
    fn effect_decl() -> EffectDecl {
        tidepool_mcp::http_decl()
    }
}

impl EffectHandler<CapturedOutput> for HttpHandler {
    type Request = HttpReq;
    fn handle(
        &mut self,
        req: HttpReq,
        cx: &EffectContext<'_, CapturedOutput>,
    ) -> Result<Value, EffectError> {
        match req {
            HttpReq::Get(url_str) => {
                let url = Self::validate_url(&url_str)?;
                let resp = ureq::get(url.as_str())
                    .timeout(std::time::Duration::from_secs(30))
                    .call()
                    .map_err(|e| {
                        EffectError::Handler(format!("HTTP GET '{}' failed: {}", url_str, e))
                    })?;
                let body = resp.into_string().map_err(|e| {
                    EffectError::Handler(format!("Read body from '{}' failed: {}", url_str, e))
                })?;
                let json = Self::parse_response(&url_str, &body)?;
                cx.respond(json)
            }
            HttpReq::Post(url_str, body_val) => {
                let url = Self::validate_url(&url_str)?;
                let json_body = tidepool_runtime::value_to_json(&body_val, cx.table(), 0);
                let resp = ureq::post(url.as_str())
                    .timeout(std::time::Duration::from_secs(30))
                    .send_json(&json_body)
                    .map_err(|e| {
                        EffectError::Handler(format!("HTTP POST '{}' failed: {}", url_str, e))
                    })?;
                let body = resp.into_string().map_err(|e| {
                    EffectError::Handler(format!("Read body from '{}' failed: {}", url_str, e))
                })?;
                let json = Self::parse_response(&url_str, &body)?;
                cx.respond(json)
            }
            HttpReq::Request(method, url_str, headers, body_str) => {
                let url = Self::validate_url(&url_str)?;
                let mut req = ureq::request(&method, url.as_str())
                    .timeout(std::time::Duration::from_secs(30));
                for (k, v) in &headers {
                    req = req.set(k, v);
                }
                let resp = if body_str.is_empty() {
                    req.call()
                } else {
                    req.set("Content-Type", "application/json")
                        .send_string(&body_str)
                }
                .map_err(|e| {
                    EffectError::Handler(format!("HTTP {} '{}' failed: {}", method, url_str, e))
                })?;
                let body = resp.into_string().map_err(|e| {
                    EffectError::Handler(format!("Read body from '{}' failed: {}", url_str, e))
                })?;
                let json = Self::parse_response(&url_str, &body)?;
                cx.respond(json)
            }
        }
    }
}

// === Tag 5: Exec (shell commands) ===

#[derive(FromCore)]
enum ExecReq {
    #[core(name = "Run")]
    Run(String),
    #[core(name = "RunIn")]
    RunIn(String, String),
    #[core(name = "RunJson")]
    RunJson(String),
}

#[derive(Clone)]
struct ExecHandler {
    root: PathBuf,
}

impl ExecHandler {
    fn new(root: PathBuf) -> Self {
        Self { root }
    }

    /// Maximum stdout size for RunJson (512 KB). Large JSON creates tens of
    /// thousands of Value nodes that can crash the JIT.
    const MAX_JSON_OUTPUT_BYTES: usize = 512 * 1024;
    /// Maximum stdout/stderr size for Run/RunIn (2 MB). Prevents OOM from
    /// commands that produce unbounded output (e.g. `find /`, `yes`).
    const MAX_EXEC_OUTPUT_BYTES: usize = 2 * 1024 * 1024;

    fn run_command(
        &self,
        cmd: &str,
        dir: &std::path::Path,
    ) -> Result<(i64, String, String), EffectError> {
        let output = std::process::Command::new("sh")
            .arg("-c")
            .arg(cmd)
            .current_dir(dir)
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .output()
            .map_err(|e| EffectError::Handler(format!("exec failed: {}", e)))?;

        let mut stdout = String::from_utf8_lossy(&output.stdout).to_string();
        let mut stderr = String::from_utf8_lossy(&output.stderr).to_string();
        if stdout.len() > Self::MAX_EXEC_OUTPUT_BYTES {
            let mut end = Self::MAX_EXEC_OUTPUT_BYTES;
            while !stdout.is_char_boundary(end) {
                end -= 1;
            }
            stdout.truncate(end);
            stdout.push_str("\n...[truncated at 2MB]");
        }
        if stderr.len() > Self::MAX_EXEC_OUTPUT_BYTES {
            let mut end = Self::MAX_EXEC_OUTPUT_BYTES;
            while !stderr.is_char_boundary(end) {
                end -= 1;
            }
            stderr.truncate(end);
            stderr.push_str("\n...[truncated at 2MB]");
        }
        let code = output.status.code().unwrap_or(-1) as i64;
        Ok((code, stdout, stderr))
    }
}

impl DescribeEffect for ExecHandler {
    fn effect_decl() -> EffectDecl {
        tidepool_mcp::exec_decl()
    }
}

impl EffectHandler<CapturedOutput> for ExecHandler {
    type Request = ExecReq;
    fn handle(
        &mut self,
        req: ExecReq,
        cx: &EffectContext<'_, CapturedOutput>,
    ) -> Result<Value, EffectError> {
        match req {
            ExecReq::Run(cmd) => {
                let (code, stdout, stderr) = self.run_command(&cmd, &self.root)?;
                cx.respond((code, stdout, stderr))
            }
            ExecReq::RunJson(cmd) => {
                let (code, stdout, stderr) = self.run_command(&cmd, &self.root)?;
                if code != 0 {
                    return Err(EffectError::Handler(format!(
                        "command failed (exit {}): {}\n{}",
                        code, cmd, stderr
                    )));
                }
                if stdout.len() > Self::MAX_JSON_OUTPUT_BYTES {
                    return Err(EffectError::Handler(format!(
                        "runJson: stdout too large ({} bytes, limit {}). Large JSON creates \
                         too many Value nodes for the JIT. Pipe through jq or use flags like \
                         --no-deps to reduce output.",
                        stdout.len(),
                        Self::MAX_JSON_OUTPUT_BYTES,
                    )));
                }
                let json_val: serde_json::Value = serde_json::from_str(&stdout).map_err(|e| {
                    EffectError::Handler(format!("runJson: invalid JSON from '{}': {}", cmd, e))
                })?;
                cx.respond(json_val)
            }
            ExecReq::RunIn(dir, cmd) => {
                let target = self.root.join(&dir);
                if !target.is_dir() {
                    return Err(EffectError::Handler(format!(
                        "directory '{}' does not exist",
                        dir
                    )));
                }
                let (code, stdout, stderr) = self.run_command(&cmd, &target)?;
                cx.respond((code, stdout, stderr))
            }
        }
    }
}

// === Tag 7: Git (repository access) ===

#[derive(FromCore)]
enum GitReq {
    #[core(name = "GitLog")]
    Log(String, i64),
    #[core(name = "GitShow")]
    Show(String),
    #[core(name = "GitDiff")]
    Diff(String),
    #[core(name = "GitBlame")]
    Blame(String, i64, i64),
    #[core(name = "GitTree")]
    Tree(String, String),
    #[core(name = "GitBranches")]
    Branches,
}

#[derive(Clone)]
struct GitHandler {
    root: PathBuf,
}

impl GitHandler {
    fn new(root: PathBuf) -> Self {
        Self { root }
    }

    fn open_repo(&self) -> Result<git2::Repository, EffectError> {
        git2::Repository::open(&self.root)
            .map_err(|e| EffectError::Handler(format!("git: failed to open repo: {}", e)))
    }
}

impl DescribeEffect for GitHandler {
    fn effect_decl() -> EffectDecl {
        tidepool_mcp::git_decl()
    }
}

impl EffectHandler<CapturedOutput> for GitHandler {
    type Request = GitReq;
    fn handle(
        &mut self,
        req: GitReq,
        cx: &EffectContext<'_, CapturedOutput>,
    ) -> Result<Value, EffectError> {
        let repo = self.open_repo()?;
        match req {
            GitReq::Log(refspec, count) => {
                let obj = repo.revparse_single(&refspec).map_err(|e| {
                    EffectError::Handler(format!("git log: bad ref '{}': {}", refspec, e))
                })?;
                let mut revwalk = repo
                    .revwalk()
                    .map_err(|e| EffectError::Handler(format!("git log: revwalk: {}", e)))?;
                revwalk
                    .push(obj.id())
                    .map_err(|e| EffectError::Handler(format!("git log: push: {}", e)))?;
                revwalk.set_sorting(git2::Sort::TIME).ok();
                let mut entries = Vec::new();
                for (i, oid_result) in revwalk.enumerate() {
                    if i >= count.max(0) as usize {
                        break;
                    }
                    let oid = oid_result
                        .map_err(|e| EffectError::Handler(format!("git log: walk: {}", e)))?;
                    let commit = repo.find_commit(oid).map_err(|e| {
                        EffectError::Handler(format!("git log: find commit: {}", e))
                    })?;
                    entries.push(serde_json::json!({
                        "hash": oid.to_string(),
                        "subject": commit.summary().unwrap_or(""),
                        "author": commit.author().name().unwrap_or(""),
                        "date": commit.time().seconds(),
                    }));
                }
                cx.respond(entries)
            }
            GitReq::Show(hash) => {
                let oid = repo
                    .revparse_single(&hash)
                    .map_err(|e| {
                        EffectError::Handler(format!("git show: bad ref '{}': {}", hash, e))
                    })?
                    .id();
                let commit = repo
                    .find_commit(oid)
                    .map_err(|e| EffectError::Handler(format!("git show: {}", e)))?;
                let parents: Vec<String> = commit.parent_ids().map(|id| id.to_string()).collect();
                let result = serde_json::json!({
                    "hash": oid.to_string(),
                    "subject": commit.summary().unwrap_or(""),
                    "author": commit.author().name().unwrap_or(""),
                    "date": commit.time().seconds(),
                    "body": commit.body().unwrap_or(""),
                    "parents": parents,
                });
                cx.respond(result)
            }
            GitReq::Diff(hash) => {
                let oid = repo
                    .revparse_single(&hash)
                    .map_err(|e| {
                        EffectError::Handler(format!("git diff: bad ref '{}': {}", hash, e))
                    })?
                    .id();
                let commit = repo
                    .find_commit(oid)
                    .map_err(|e| EffectError::Handler(format!("git diff: {}", e)))?;
                let tree = commit
                    .tree()
                    .map_err(|e| EffectError::Handler(format!("git diff: tree: {}", e)))?;
                let parent_tree = if commit.parent_count() > 0 {
                    Some(
                        commit
                            .parent(0)
                            .map_err(|e| EffectError::Handler(format!("git diff: parent: {}", e)))?
                            .tree()
                            .map_err(|e| {
                                EffectError::Handler(format!("git diff: parent tree: {}", e))
                            })?,
                    )
                } else {
                    None
                };
                let diff = repo
                    .diff_tree_to_tree(parent_tree.as_ref(), Some(&tree), None)
                    .map_err(|e| EffectError::Handler(format!("git diff: diff: {}", e)))?;
                let mut files = Vec::new();
                for delta in diff.deltas() {
                    let status_char = match delta.status() {
                        git2::Delta::Added => "A",
                        git2::Delta::Deleted => "D",
                        git2::Delta::Modified => "M",
                        git2::Delta::Renamed => "R",
                        git2::Delta::Copied => "C",
                        _ => "?",
                    };
                    let path = delta
                        .new_file()
                        .path()
                        .or_else(|| delta.old_file().path())
                        .map(|p| p.to_string_lossy().to_string())
                        .unwrap_or_default();
                    files.push(serde_json::json!({
                        "path": path,
                        "status": status_char,
                    }));
                }
                cx.respond(files)
            }
            GitReq::Blame(file, start, end) => {
                let mut opts = git2::BlameOptions::new();
                if start > 0 && end > 0 {
                    opts.min_line(start as usize);
                    opts.max_line(end as usize);
                }
                let blame = repo
                    .blame_file(std::path::Path::new(&file), Some(&mut opts))
                    .map_err(|e| EffectError::Handler(format!("git blame '{}': {}", file, e)))?;
                // Read file content to get line text
                let file_path = self.root.join(&file);
                let content = std::fs::read_to_string(&file_path).unwrap_or_default();
                let lines: Vec<&str> = content.lines().collect();
                let mut hunks = Vec::new();
                for hunk_idx in 0..blame.len() {
                    let Some(hunk) = blame.get_index(hunk_idx) else {
                        continue;
                    };
                    let sig = hunk.final_signature();
                    let line_start = hunk.final_start_line();
                    let line_count = hunk.lines_in_hunk();
                    for offset in 0..line_count {
                        let line_no = line_start + offset;
                        let line_text = if line_no > 0 && line_no <= lines.len() {
                            lines[line_no - 1]
                        } else {
                            ""
                        };
                        hunks.push(serde_json::json!({
                            "commit": hunk.final_commit_id().to_string(),
                            "author": sig.name().unwrap_or(""),
                            "line": line_no,
                            "content": line_text,
                        }));
                    }
                }
                cx.respond(hunks)
            }
            GitReq::Tree(commitish, path) => {
                let obj = repo.revparse_single(&commitish).map_err(|e| {
                    EffectError::Handler(format!("git tree: bad ref '{}': {}", commitish, e))
                })?;
                let commit = obj
                    .peel_to_commit()
                    .map_err(|e| EffectError::Handler(format!("git tree: peel: {}", e)))?;
                let tree = commit
                    .tree()
                    .map_err(|e| EffectError::Handler(format!("git tree: {}", e)))?;
                let target_tree = if path == "." || path.is_empty() {
                    tree
                } else {
                    let entry = tree.get_path(std::path::Path::new(&path)).map_err(|e| {
                        EffectError::Handler(format!("git tree: path '{}': {}", path, e))
                    })?;
                    repo.find_tree(entry.id())
                        .map_err(|e| EffectError::Handler(format!("git tree: subtree: {}", e)))?
                };
                let mut entries = Vec::new();
                for entry in target_tree.iter() {
                    let kind = match entry.kind() {
                        Some(git2::ObjectType::Blob) => "blob",
                        Some(git2::ObjectType::Tree) => "tree",
                        _ => "other",
                    };
                    entries.push(serde_json::json!({
                        "name": entry.name().unwrap_or(""),
                        "type": kind,
                        "oid": entry.id().to_string(),
                    }));
                }
                cx.respond(entries)
            }
            GitReq::Branches => {
                let branches = repo
                    .branches(None)
                    .map_err(|e| EffectError::Handler(format!("git branches: {}", e)))?;
                let mut result = Vec::new();
                for branch_result in branches {
                    let (branch, _branch_type) = branch_result
                        .map_err(|e| EffectError::Handler(format!("git branches: iter: {}", e)))?;
                    let name = branch.name().ok().flatten().unwrap_or("").to_string();
                    let is_head = branch.is_head();
                    let commit_hash = branch
                        .get()
                        .peel_to_commit()
                        .map(|c| c.id().to_string())
                        .unwrap_or_default();
                    result.push(serde_json::json!({
                        "name": name,
                        "is_head": is_head,
                        "commit": commit_hash,
                    }));
                }
                cx.respond(result)
            }
        }
    }
}

// === Tag 8: Llm (LLM calls via Anthropic API) ===

#[derive(FromCore)]
enum LlmReq {
    #[core(name = "LlmChat")]
    Chat(String),
    #[core(name = "LlmStructured")]
    Structured(String, Value), // Value is the Schema ADT, decoded in handler
}

#[derive(Clone)]
struct LlmHandler {
    api_key: String,
    call_count: std::sync::Arc<std::sync::atomic::AtomicU32>,
}

const LLM_MODEL: &str = "claude-haiku-4-5-20251001";
const LLM_MAX_CALLS: u32 = 200;

impl LlmHandler {
    fn new() -> Self {
        let api_key = std::env::var("ANTHROPIC_API_KEY")
            .ok()
            .filter(|s| !s.is_empty())
            .or_else(|| Self::read_key_file())
            .unwrap_or_default();
        Self {
            api_key,
            call_count: std::sync::Arc::new(std::sync::atomic::AtomicU32::new(0)),
        }
    }

    /// Read API key from `.tidepool/anthropic.key` (cwd or ancestors).
    fn read_key_file() -> Option<String> {
        let mut dir = std::env::current_dir().ok()?;
        loop {
            let key_path = dir.join(".tidepool").join("anthropic.key");
            if key_path.is_file() {
                let contents = std::fs::read_to_string(&key_path).ok()?;
                let trimmed = contents.trim().to_string();
                if !trimmed.is_empty() {
                    return Some(trimmed);
                }
            }
            if !dir.pop() {
                break;
            }
        }
        None
    }

    fn check_rate_limit(&self) -> Result<(), EffectError> {
        let count = self
            .call_count
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        if count >= LLM_MAX_CALLS {
            Err(EffectError::Handler(format!(
                "LLM call limit exceeded ({} calls max per eval)",
                LLM_MAX_CALLS
            )))
        } else {
            Ok(())
        }
    }

    fn check_api_key(&self) -> Result<(), EffectError> {
        if self.api_key.is_empty() {
            Err(EffectError::Handler(
                "ANTHROPIC_API_KEY not set and no .tidepool/anthropic.key found".to_string(),
            ))
        } else {
            Ok(())
        }
    }

    fn call_api(
        &self,
        messages: &serde_json::Value,
        tools: Option<&serde_json::Value>,
    ) -> Result<serde_json::Value, EffectError> {
        let mut body = serde_json::json!({
            "model": LLM_MODEL,
            "max_tokens": 4096,
            "messages": messages,
        });
        if let Some(tools_val) = tools {
            body["tools"] = tools_val.clone();
            body["tool_choice"] = serde_json::json!({"type": "any"});
        }
        let resp = ureq::post("https://api.anthropic.com/v1/messages")
            .timeout(std::time::Duration::from_secs(60))
            .set("x-api-key", &self.api_key)
            .set("anthropic-version", "2023-06-01")
            .set("Content-Type", "application/json")
            .send_json(&body)
            .map_err(|e| EffectError::Handler(format!("LLM API call failed: {}", e)))?;
        let body_str = resp
            .into_string()
            .map_err(|e| EffectError::Handler(format!("LLM API response read failed: {}", e)))?;
        serde_json::from_str(&body_str)
            .map_err(|e| EffectError::Handler(format!("LLM API response parse failed: {}", e)))
    }
}

impl DescribeEffect for LlmHandler {
    fn effect_decl() -> EffectDecl {
        tidepool_mcp::llm_decl()
    }
}

impl EffectHandler<CapturedOutput> for LlmHandler {
    type Request = LlmReq;
    fn handle(
        &mut self,
        req: LlmReq,
        cx: &EffectContext<'_, CapturedOutput>,
    ) -> Result<Value, EffectError> {
        self.check_api_key()?;
        self.check_rate_limit()?;
        match req {
            LlmReq::Chat(prompt) => {
                let messages = serde_json::json!([{"role": "user", "content": prompt}]);
                let resp = self.call_api(&messages, None)?;
                let text = resp["content"][0]["text"]
                    .as_str()
                    .unwrap_or("")
                    .to_string();
                cx.respond(text)
            }
            LlmReq::Structured(prompt, schema_val) => {
                let schema_json = tidepool_runtime::value_to_json(&schema_val, cx.table(), 0);
                let tools = serde_json::json!([{
                    "name": "structured_output",
                    "description": "Return structured data matching the schema.",
                    "input_schema": schema_json,
                }]);
                let messages = serde_json::json!([{"role": "user", "content": prompt}]);
                let resp = self.call_api(&messages, Some(&tools))?;
                // Extract tool use input from response
                let input = resp["content"]
                    .as_array()
                    .and_then(|arr| {
                        arr.iter().find(|block| block["type"] == "tool_use")
                    })
                    .and_then(|block| block.get("input"))
                    .cloned()
                    .unwrap_or(serde_json::Value::Null);
                eprintln!("[llm-structured] API response input: {}", serde_json::to_string(&input).unwrap_or_default());
                let result = cx.respond(input);
                eprintln!("[llm-structured] cx.respond result: {}", result.is_ok());
                result
            }
        }
    }
}

// === Tag 6: Meta (runtime introspection) ===

#[derive(FromCore)]
enum MetaReq {
    #[core(name = "MetaConstructors")]
    Constructors,
    #[core(name = "MetaLookupCon")]
    LookupCon(String),
    #[core(name = "MetaPrimOps")]
    PrimOps,
    #[core(name = "MetaEffects")]
    Effects,
    #[core(name = "MetaDiagnostics")]
    Diagnostics,
    #[core(name = "MetaVersion")]
    Version,
    #[core(name = "MetaHelp")]
    Help,
}

#[derive(Clone)]
struct MetaHandler {
    effect_names: Vec<String>,
    helper_sigs: Vec<String>,
}

impl MetaHandler {
    fn new(effect_names: Vec<String>, helper_sigs: Vec<String>) -> Self {
        Self {
            effect_names,
            helper_sigs,
        }
    }
}

impl DescribeEffect for MetaHandler {
    fn effect_decl() -> EffectDecl {
        tidepool_mcp::meta_decl()
    }
}

impl EffectHandler<CapturedOutput> for MetaHandler {
    type Request = MetaReq;
    fn handle(
        &mut self,
        req: MetaReq,
        cx: &EffectContext<'_, CapturedOutput>,
    ) -> Result<Value, EffectError> {
        match req {
            MetaReq::Constructors => {
                let mut pairs: Vec<(String, i64)> = cx
                    .table()
                    .iter()
                    .map(|dc| (dc.name.clone(), dc.rep_arity as i64))
                    .collect();
                pairs.sort_by(|a, b| a.0.cmp(&b.0));
                cx.respond(pairs)
            }
            MetaReq::LookupCon(name) => {
                let result: Option<(i64, i64)> = cx.table().get_by_name(&name).and_then(|id| {
                    cx.table()
                        .get(id)
                        .map(|dc| (dc.tag as i64, dc.rep_arity as i64))
                });
                cx.respond(result)
            }
            MetaReq::PrimOps => {
                let primops: Vec<String> = vec![
                    "+#",
                    "-#",
                    "*#",
                    "negateInt#",
                    "==#",
                    "/=#",
                    "<#",
                    "<=#",
                    ">#",
                    ">=#",
                    "quotInt#",
                    "remInt#",
                    "andI#",
                    "orI#",
                    "xorI#",
                    "notI#",
                    "uncheckedIShiftL#",
                    "uncheckedIShiftRA#",
                    "uncheckedIShiftRL#",
                    "int2Double#",
                    "double2Int#",
                    "+##",
                    "-##",
                    "*##",
                    "/##",
                    "negateDouble#",
                    "==##",
                    "/=##",
                    "<##",
                    "<=##",
                    ">##",
                    ">=##",
                    "sqrtDouble#",
                    "sinDouble#",
                    "cosDouble#",
                    "expDouble#",
                    "logDouble#",
                    "**##",
                    "fabsDouble#",
                    "chr#",
                    "ord#",
                    "newMutVar#",
                    "readMutVar#",
                    "writeMutVar#",
                    "seq#",
                    "tagToEnum#",
                ]
                .into_iter()
                .map(String::from)
                .collect();
                cx.respond(primops)
            }
            MetaReq::Effects => cx.respond(self.effect_names.clone()),
            MetaReq::Diagnostics => {
                let diags = tidepool_runtime::drain_diagnostics();
                cx.respond(diags)
            }
            MetaReq::Version => cx.respond(env!("CARGO_PKG_VERSION").to_string()),
            MetaReq::Help => cx.respond(self.helper_sigs.clone()),
        }
    }
}

// ---------------------------------------------------------------------------
// Embedded Haskell stdlib — written to ~/.tidepool/prelude/ on startup
// ---------------------------------------------------------------------------

const PRELUDE_HS: &str = include_str!("../../haskell/lib/Tidepool/Prelude.hs");
const TEXT_HS: &str = include_str!("../../haskell/lib/Tidepool/Text.hs");
const TABLE_HS: &str = include_str!("../../haskell/lib/Tidepool/Table.hs");
const AESON_HS: &str = include_str!("../../haskell/lib/Tidepool/Aeson.hs");
const AESON_VALUE_HS: &str = include_str!("../../haskell/lib/Tidepool/Aeson/Value.hs");
const AESON_KEYMAP_HS: &str = include_str!("../../haskell/lib/Tidepool/Aeson/KeyMap.hs");
const AESON_LENS_HS: &str = include_str!("../../haskell/lib/Tidepool/Aeson/Lens.hs");

const EMBEDDED_FILES: &[(&str, &str)] = &[
    ("Tidepool/Prelude.hs", PRELUDE_HS),
    ("Tidepool/Text.hs", TEXT_HS),
    ("Tidepool/Table.hs", TABLE_HS),
    ("Tidepool/Aeson.hs", AESON_HS),
    ("Tidepool/Aeson/Value.hs", AESON_VALUE_HS),
    ("Tidepool/Aeson/KeyMap.hs", AESON_KEYMAP_HS),
    ("Tidepool/Aeson/Lens.hs", AESON_LENS_HS),
];

/// Ensure embedded Haskell stdlib is written to ~/.tidepool/prelude/.
/// Returns the prelude directory path. Respects TIDEPOOL_PRELUDE_DIR override.
fn ensure_prelude() -> Result<PathBuf, Box<dyn std::error::Error>> {
    if let Some(dir) = std::env::var_os("TIDEPOOL_PRELUDE_DIR") {
        return Ok(PathBuf::from(dir));
    }

    // In-repo development: use haskell/lib/ directly if present
    if let Ok(cwd) = std::env::current_dir() {
        let from_root = cwd.join("haskell").join("lib");
        if from_root.join("Tidepool").join("Prelude.hs").exists() {
            return Ok(from_root);
        }
        let from_haskell = cwd.join("lib");
        if from_haskell.join("Tidepool").join("Prelude.hs").exists() {
            return Ok(from_haskell);
        }
    }

    // Installed mode: write embedded files to ~/.tidepool/prelude/
    let base = dirs::home_dir()
        .ok_or("could not determine home directory")?
        .join(".tidepool")
        .join("prelude");
    let stamp = base.join(".version");
    let version = env!("CARGO_PKG_VERSION");
    if stamp.exists() && std::fs::read_to_string(&stamp).ok().as_deref() == Some(version) {
        return Ok(base);
    }

    for (path, content) in EMBEDDED_FILES {
        let full = base.join(path);
        if let Some(parent) = full.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let _ = std::fs::write(&full, content);
    }
    let _ = std::fs::write(&stamp, version);
    Ok(base)
}

/// Check if tidepool-extract is available.
fn find_tidepool_extract() -> Option<PathBuf> {
    // 1. TIDEPOOL_EXTRACT env var
    if let Ok(p) = std::env::var("TIDEPOOL_EXTRACT") {
        let path = PathBuf::from(&p);
        if path.exists() {
            return Some(path);
        }
    }
    // 2. On PATH
    which::which("tidepool-extract").ok()
}

// ---------------------------------------------------------------------------
// Degraded MCP server — served when tidepool-extract is missing
// ---------------------------------------------------------------------------

#[derive(Clone)]
struct SetupServer;

const INSTALL_INSTRUCTIONS: &str = "\
Tidepool MCP server is running but the GHC toolchain is not installed.
The Haskell compiler is needed to evaluate code.

Install it with Nix:

  1. Install Nix (if needed):
     curl --proto '=https' --tlsv1.2 -sSf -L https://install.determinate.systems/nix | sh -s -- install

  2. Install the tidepool GHC toolchain:
     nix profile install github:tidepool-heavy-industries/tidepool#tidepool-extract

  3. Restart this MCP server.

Alternatively, set TIDEPOOL_EXTRACT to point to an existing tidepool-extract binary.";

impl ServerHandler for SetupServer {
    fn get_info(&self) -> ServerInfo {
        ServerInfo {
            instructions: Some(
                "Tidepool MCP server (setup mode). The GHC toolchain is not installed. \
                 Call the install_instructions tool for setup steps."
                    .into(),
            ),
            capabilities: ServerCapabilities::builder().enable_tools().build(),
            ..Default::default()
        }
    }

    async fn list_tools(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListToolsResult, McpError> {
        let schema = serde_json::json!({
            "type": "object",
            "properties": {},
        });
        let input_schema = match schema {
            serde_json::Value::Object(o) => Arc::new(o),
            _ => Arc::new(serde_json::Map::new()),
        };
        Ok(ListToolsResult {
            tools: vec![Tool {
                name: "install_instructions".into(),
                title: None,
                description: Some(
                    "Get instructions for installing the GHC toolchain required by Tidepool."
                        .into(),
                ),
                input_schema,
                output_schema: None,
                annotations: None,
                icons: None,
                meta: None,
                execution: None,
            }],
            next_cursor: None,
            meta: None,
        })
    }

    async fn call_tool(
        &self,
        request: CallToolRequestParams,
        _context: RequestContext<RoleServer>,
    ) -> Result<CallToolResult, McpError> {
        match request.name.as_ref() {
            "install_instructions" => Ok(CallToolResult {
                content: vec![Content::text(INSTALL_INSTRUCTIONS)],
                structured_content: None,
                is_error: Some(false),
                meta: None,
            }),
            _ => Err(McpError {
                code: ErrorCode::METHOD_NOT_FOUND,
                message: format!("Tool not found: {}", request.name).into(),
                data: None,
            }),
        }
    }
}

// ---------------------------------------------------------------------------
// CLI
// ---------------------------------------------------------------------------

#[derive(clap::Parser)]
#[command(name = "tidepool", about = "Tidepool MCP server")]
struct Args {
    /// Serve over streamable HTTP instead of stdio. Example: --http 0.0.0.0:8080
    #[arg(long, conflicts_with = "port")]
    http: Option<SocketAddr>,

    /// Serve over HTTP on 0.0.0.0:<PORT>. Shorthand for --http 0.0.0.0:<PORT>
    #[arg(long, conflicts_with = "http")]
    port: Option<u16>,
}

// ---------------------------------------------------------------------------
// Main
// ---------------------------------------------------------------------------

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Install panic hook that writes crash dumps to ~/.tidepool/crash.log
    std::panic::set_hook(Box::new(|info| {
        let msg = format!("{}\n{:?}\n", info, std::backtrace::Backtrace::capture());
        let path = dirs::home_dir()
            .unwrap_or_default()
            .join(".tidepool/crash.log");
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let _ = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
            .and_then(|mut f| std::io::Write::write_all(&mut f, msg.as_bytes()));
        eprintln!("[tidepool] PANIC — see {}", path.display());
    }));

    use clap::Parser;
    let args = Args::parse();
    let http_addr = args
        .http
        .or(args.port.map(|p| SocketAddr::from(([0, 0, 0, 0], p))));

    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("warn")),
        )
        .with_writer(std::io::stderr)
        .init();

    let prelude_dir = ensure_prelude()?;

    // If tidepool-extract is not available, serve the degraded setup server.
    if find_tidepool_extract().is_none() {
        tracing::warn!(
            "tidepool-extract not found — serving setup-only MCP server. \
             Install via: nix profile install github:tidepool-heavy-industries/tidepool#tidepool-extract"
        );
        use rmcp::ServiceExt;
        if let Some(addr) = http_addr {
            use rmcp::transport::streamable_http_server::{
                session::local::LocalSessionManager, StreamableHttpServerConfig,
                StreamableHttpService,
            };
            let config = StreamableHttpServerConfig::default();
            let cancel = config.cancellation_token.clone();
            let service = StreamableHttpService::new(
                || Ok(SetupServer),
                Arc::new(LocalSessionManager::default()),
                config,
            );
            async fn health() -> axum::Json<serde_json::Value> {
                axum::Json(serde_json::json!({"status": "ok"}))
            }
            let router = axum::Router::new()
                .route("/health", axum::routing::get(health))
                .nest_service("/mcp", service);
            let listener = tokio::net::TcpListener::bind(addr).await?;
            eprintln!(
                "Tidepool MCP v{} listening on http://{}/mcp (setup mode)",
                env!("CARGO_PKG_VERSION"),
                addr,
            );
            axum::serve(listener, router)
                .with_graceful_shutdown(async move {
                    tokio::signal::ctrl_c().await.ok();
                    cancel.cancel();
                })
                .await?;
        } else {
            SetupServer
                .serve((tokio::io::stdin(), tokio::io::stdout()))
                .await?
                .waiting()
                .await?;
        }
        return Ok(());
    }

    // Install sigsetjmp/siglongjmp signal handlers early so SIGILL/SIGSEGV
    // from JIT code returns clean errors instead of killing the server.
    tidepool_codegen::signal_safety::install();

    let cwd = std::env::current_dir()?;
    let tidepool_dir = cwd.join(".tidepool");
    let kv_path = tidepool_dir.join("kv.json");
    let decls = tidepool_mcp::standard_decls();
    let effect_names: Vec<String> = decls.iter().map(|d| d.type_name.to_string()).collect();
    let mut helper_sigs: Vec<String> = Vec::new();
    // Preamble-generated helpers
    helper_sigs.push("say :: Text -> M ()".into());
    helper_sigs.push("showI :: Int -> Text".into());
    // Effect-declared helpers
    for decl in &decls {
        for h in decl.helpers {
            if let Some(sig) = h.lines().next() {
                helper_sigs.push(sig.to_string());
            }
        }
    }
    let handlers = frunk::hlist![
        ConsoleHandler,
        KvHandler::new(kv_path),
        FsHandler::new(cwd.clone()),
        SgHandler::new(cwd.clone()),
        HttpHandler,
        ExecHandler::new(cwd.clone()),
        MetaHandler::new(effect_names, helper_sigs),
        GitHandler::new(cwd.clone()),
        LlmHandler::new()
    ];

    let server = TidepoolMcpServer::new(handlers).with_prelude(prelude_dir);
    if let Some(addr) = http_addr {
        server.serve_http(addr).await
    } else {
        server.serve_stdio().await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tidepool_bridge::{FromCore, ToCore};
    use tidepool_effect::dispatch::DispatchEffect;
    use tidepool_repr::{DataCon, DataConId, DataConTable, Literal};

    /// Prelude include path for JIT tests.
    fn prelude_include() -> PathBuf {
        let mut dir = repo_root();
        dir.push("haskell");
        dir.push("lib");
        dir
    }

    /// Build full Haskell source for a JIT effect test.
    fn jit_test_source(code: &[&str]) -> String {
        let decls = tidepool_mcp::standard_decls();
        let preamble = tidepool_mcp::build_preamble(&decls, false);
        let stack = tidepool_mcp::build_effect_stack_type(&decls);
        let code_str = code.join("\n");
        tidepool_mcp::template_haskell(&preamble, &stack, &code_str, "", "", None, None)
    }

    /// Compile and run a Haskell snippet through the JIT with the full handler stack.
    /// Returns the result as serde_json::Value, or panics on error.
    fn jit_eval(code: &[&str]) -> serde_json::Value {
        let source = jit_test_source(code);
        let include = prelude_include();
        let include_paths: Vec<&std::path::Path> = vec![include.as_path()];
        let kv_path = std::env::temp_dir().join("tidepool_jit_test_kv.json");
        let cwd = repo_root();
        let captured = CapturedOutput::new();
        let mut handlers = frunk::hlist![
            ConsoleHandler,
            KvHandler::new(kv_path),
            FsHandler::new(cwd.clone()),
            SgHandler::new(cwd.clone()),
            HttpHandler,
            ExecHandler::new(cwd.clone()),
            MetaHandler::new(vec![], vec![]),
            GitHandler::new(cwd),
            LlmHandler::new()
        ];
        let result = tidepool_runtime::compile_and_run(
            &source,
            "result",
            &include_paths,
            &mut handlers,
            &captured,
        );
        match result {
            Ok(eval_result) => eval_result.to_json(),
            Err(e) => panic!("JIT eval failed: {:?}", e),
        }
    }

    /// Helper: find repo root.
    fn repo_root() -> PathBuf {
        let mut dir = std::env::current_dir().unwrap();
        loop {
            if dir.join(".git").exists() {
                return dir;
            }
            if !dir.pop() {
                panic!("not inside a git repo");
            }
        }
    }

    /// Helper: open the repo at the workspace root and run a GitHandler operation.
    fn git_handler() -> (GitHandler, git2::Repository) {
        let dir = repo_root();
        let repo = git2::Repository::open(&dir).unwrap();
        (GitHandler::new(dir), repo)
    }

    /// Build a DataConTable with standard types + all effect constructors + response types.
    /// Auto-generated from `standard_decls()` so it never goes stale.
    fn full_effect_test_table() -> DataConTable {
        let mut t = tidepool_testing::gen::datacon_table::standard_datacon_table();
        let decls = tidepool_mcp::standard_decls();
        let mut next_id = 100u64;

        // Auto-add all effect constructors from declarations
        for decl in &decls {
            for con_str in decl.constructors {
                let parsed = tidepool_mcp::parse_constructor(con_str)
                    .unwrap_or_else(|e| panic!("bad constructor decl: {e}"));
                if t.get_by_name(&parsed.name).is_some() {
                    continue;
                }
                t.insert(DataCon {
                    id: DataConId(next_id),
                    name: parsed.name,
                    tag: 1,
                    rep_arity: parsed.arity,
                    field_bangs: vec![],
                    qualified_name: None,
                });
                next_id += 1;
            }
        }

        // Response-type constructors used by handlers
        let response_extras: &[(&str, u32)] = &[
            ("Object", 1),
            ("Array", 1),
            ("String", 1),
            ("Number", 1),
            ("Bool", 1),
            ("Null", 0),
            ("Bin", 5),
            ("Tip", 0),
            ("()", 0),
            ("(,,)", 3),
            // SG types
            ("Match", 5),
            ("Rust", 0),
            ("Python", 0),
            ("TypeScript", 0),
            ("JavaScript", 0),
            ("Go", 0),
            ("Java", 0),
            ("C", 0),
            ("Cpp", 0),
            ("Haskell", 0),
            ("Nix", 0),
            ("Html", 0),
            ("Css", 0),
            ("Json", 0),
            ("Yaml", 0),
            ("Toml", 0),
        ];
        for &(name, arity) in response_extras {
            if t.get_by_name(name).is_some() {
                continue;
            }
            t.insert(DataCon {
                id: DataConId(next_id),
                name: name.into(),
                tag: 1,
                rep_arity: arity,
                field_bangs: vec![],
                qualified_name: None,
            });
            next_id += 1;
        }
        t
    }

    /// Walk a Value and assert it's a valid Haskell cons-list ([] or : _ _).
    fn assert_is_haskell_list(val: &Value, table: &DataConTable) {
        match val {
            Value::Con(id, fields) => {
                let name = table.name_of(*id).unwrap();
                match name {
                    "[]" => assert!(fields.is_empty()),
                    ":" => {
                        assert_eq!(fields.len(), 2, "cons cell should have 2 fields");
                        assert_is_json_value(&fields[0], table);
                        assert_is_haskell_list(&fields[1], table);
                    }
                    other => panic!("Expected list constructor, got {}", other),
                }
            }
            other => panic!("Expected Con (list), got {:?}", other),
        }
    }

    /// Assert a Value is a valid JSON Value constructor.
    fn assert_is_json_value(val: &Value, table: &DataConTable) {
        match val {
            Value::Con(id, _) => {
                let name = table.name_of(*id).unwrap();
                assert!(
                    ["Object", "Array", "String", "Number", "Bool", "Null"].contains(&name),
                    "Expected JSON Value constructor, got {}",
                    name
                );
            }
            _ => panic!("Expected Con (JSON Value), got {:?}", val),
        }
    }

    /// Get HEAD commit hash for tests that need a valid oid.
    fn head_hash() -> String {
        let repo = git2::Repository::open(repo_root()).unwrap();
        let head = repo.head().unwrap().peel_to_commit().unwrap();
        head.id().to_string()
    }

    // === Request FromCore tests ===

    #[test]
    fn test_git_req_from_core_branches() {
        let table = full_effect_test_table();
        let con_id = table.get_by_name("GitBranches").unwrap();
        let val = Value::Con(con_id, vec![]);
        let req = GitReq::from_value(&val, &table).unwrap();
        assert!(matches!(req, GitReq::Branches));
    }

    #[test]
    fn test_git_req_from_core_log() {
        let table = full_effect_test_table();
        let con_id = table.get_by_name("GitLog").unwrap();
        let ref_val = "HEAD".to_string().to_value(&table).unwrap();
        let count_val = Value::Lit(Literal::LitInt(5));
        let val = Value::Con(con_id, vec![ref_val, count_val]);
        let req = GitReq::from_value(&val, &table).unwrap();
        assert!(matches!(req, GitReq::Log(ref s, 5) if s == "HEAD"));
    }

    #[test]
    fn test_git_req_from_core_show() {
        let table = full_effect_test_table();
        let con_id = table.get_by_name("GitShow").unwrap();
        let hash = head_hash();
        let hash_val = hash.clone().to_value(&table).unwrap();
        let val = Value::Con(con_id, vec![hash_val]);
        let req = GitReq::from_value(&val, &table).unwrap();
        assert!(matches!(req, GitReq::Show(ref s) if s == &hash));
    }

    #[test]
    fn test_git_req_from_core_diff() {
        let table = full_effect_test_table();
        let con_id = table.get_by_name("GitDiff").unwrap();
        let hash = head_hash();
        let hash_val = hash.clone().to_value(&table).unwrap();
        let val = Value::Con(con_id, vec![hash_val]);
        let req = GitReq::from_value(&val, &table).unwrap();
        assert!(matches!(req, GitReq::Diff(ref s) if s == &hash));
    }

    #[test]
    fn test_git_req_from_core_blame() {
        let table = full_effect_test_table();
        let con_id = table.get_by_name("GitBlame").unwrap();
        let file_val = "Cargo.toml".to_string().to_value(&table).unwrap();
        let start_val = Value::Lit(Literal::LitInt(1));
        let end_val = Value::Lit(Literal::LitInt(5));
        let val = Value::Con(con_id, vec![file_val, start_val, end_val]);
        let req = GitReq::from_value(&val, &table).unwrap();
        assert!(matches!(req, GitReq::Blame(ref f, 1, 5) if f == "Cargo.toml"));
    }

    #[test]
    fn test_git_req_from_core_tree() {
        let table = full_effect_test_table();
        let con_id = table.get_by_name("GitTree").unwrap();
        let ref_val = "HEAD".to_string().to_value(&table).unwrap();
        let path_val = ".".to_string().to_value(&table).unwrap();
        let val = Value::Con(con_id, vec![ref_val, path_val]);
        let req = GitReq::from_value(&val, &table).unwrap();
        assert!(matches!(req, GitReq::Tree(ref r, ref p) if r == "HEAD" && p == "."));
    }

    // === Response structure tests ===

    #[test]
    fn test_git_response_branches_structure() {
        let table = full_effect_test_table();
        let captured = CapturedOutput::new();
        let cx = EffectContext::with_user(&table, &captured);
        let (mut handler, _) = git_handler();
        let result = handler.handle(GitReq::Branches, &cx).unwrap();
        assert_is_haskell_list(&result, &table);
    }

    #[test]
    fn test_git_response_log_structure() {
        let table = full_effect_test_table();
        let captured = CapturedOutput::new();
        let cx = EffectContext::with_user(&table, &captured);
        let (mut handler, _) = git_handler();
        let result = handler.handle(GitReq::Log("HEAD".into(), 3), &cx).unwrap();
        assert_is_haskell_list(&result, &table);
    }

    #[test]
    fn test_git_response_show_structure() {
        let table = full_effect_test_table();
        let captured = CapturedOutput::new();
        let cx = EffectContext::with_user(&table, &captured);
        let (mut handler, _) = git_handler();
        let hash = head_hash();
        let result = handler.handle(GitReq::Show(hash), &cx).unwrap();
        // Show returns a single JSON Object, not a list
        assert_is_json_value(&result, &table);
    }

    #[test]
    fn test_git_response_diff_structure() {
        let table = full_effect_test_table();
        let captured = CapturedOutput::new();
        let cx = EffectContext::with_user(&table, &captured);
        let (mut handler, _) = git_handler();
        let hash = head_hash();
        let result = handler.handle(GitReq::Diff(hash), &cx).unwrap();
        assert_is_haskell_list(&result, &table);
    }

    #[test]
    fn test_git_response_tree_structure() {
        let table = full_effect_test_table();
        let captured = CapturedOutput::new();
        let cx = EffectContext::with_user(&table, &captured);
        let (mut handler, _) = git_handler();
        let result = handler
            .handle(GitReq::Tree("HEAD".into(), ".".into()), &cx)
            .unwrap();
        assert_is_haskell_list(&result, &table);
    }

    #[test]
    fn test_git_response_blame_structure() {
        let table = full_effect_test_table();
        let captured = CapturedOutput::new();
        let cx = EffectContext::with_user(&table, &captured);
        let (mut handler, _) = git_handler();
        let result = handler
            .handle(GitReq::Blame("Cargo.toml".into(), 1, 5), &cx)
            .unwrap();
        assert_is_haskell_list(&result, &table);
    }

    // === Full dispatch round-trip tests ===

    #[test]
    fn test_git_dispatch_roundtrip_branches() {
        let table = full_effect_test_table();
        let captured = CapturedOutput::new();
        let cx = EffectContext::with_user(&table, &captured);
        let mut handlers = frunk::hlist![GitHandler::new(repo_root())];
        let con_id = table.get_by_name("GitBranches").unwrap();
        let request = Value::Con(con_id, vec![]);
        let result = handlers.dispatch(0, &request, &cx).unwrap();
        assert_is_haskell_list(&result, &table);
    }

    #[test]
    fn test_git_dispatch_roundtrip_log() {
        let table = full_effect_test_table();
        let captured = CapturedOutput::new();
        let cx = EffectContext::with_user(&table, &captured);
        let mut handlers = frunk::hlist![GitHandler::new(repo_root())];
        let con_id = table.get_by_name("GitLog").unwrap();
        let ref_val = "HEAD".to_string().to_value(&table).unwrap();
        let count_val = Value::Lit(Literal::LitInt(3));
        let request = Value::Con(con_id, vec![ref_val, count_val]);
        let result = handlers.dispatch(0, &request, &cx).unwrap();
        assert_is_haskell_list(&result, &table);
    }

    #[test]
    fn test_git_dispatch_roundtrip_show() {
        let table = full_effect_test_table();
        let captured = CapturedOutput::new();
        let cx = EffectContext::with_user(&table, &captured);
        let mut handlers = frunk::hlist![GitHandler::new(repo_root())];
        let con_id = table.get_by_name("GitShow").unwrap();
        let hash = head_hash();
        let hash_val = hash.to_value(&table).unwrap();
        let request = Value::Con(con_id, vec![hash_val]);
        let result = handlers.dispatch(0, &request, &cx).unwrap();
        assert_is_json_value(&result, &table);
    }

    #[test]
    fn test_git_dispatch_roundtrip_diff() {
        let table = full_effect_test_table();
        let captured = CapturedOutput::new();
        let cx = EffectContext::with_user(&table, &captured);
        let mut handlers = frunk::hlist![GitHandler::new(repo_root())];
        let con_id = table.get_by_name("GitDiff").unwrap();
        let hash = head_hash();
        let hash_val = hash.to_value(&table).unwrap();
        let request = Value::Con(con_id, vec![hash_val]);
        let result = handlers.dispatch(0, &request, &cx).unwrap();
        assert_is_haskell_list(&result, &table);
    }

    #[test]
    fn test_git_dispatch_roundtrip_tree() {
        let table = full_effect_test_table();
        let captured = CapturedOutput::new();
        let cx = EffectContext::with_user(&table, &captured);
        let mut handlers = frunk::hlist![GitHandler::new(repo_root())];
        let con_id = table.get_by_name("GitTree").unwrap();
        let ref_val = "HEAD".to_string().to_value(&table).unwrap();
        let path_val = ".".to_string().to_value(&table).unwrap();
        let request = Value::Con(con_id, vec![ref_val, path_val]);
        let result = handlers.dispatch(0, &request, &cx).unwrap();
        assert_is_haskell_list(&result, &table);
    }

    #[test]
    fn test_git_dispatch_roundtrip_blame() {
        let table = full_effect_test_table();
        let captured = CapturedOutput::new();
        let cx = EffectContext::with_user(&table, &captured);
        let mut handlers = frunk::hlist![GitHandler::new(repo_root())];
        let con_id = table.get_by_name("GitBlame").unwrap();
        let file_val = "Cargo.toml".to_string().to_value(&table).unwrap();
        let start_val = Value::Lit(Literal::LitInt(1));
        let end_val = Value::Lit(Literal::LitInt(5));
        let request = Value::Con(con_id, vec![file_val, start_val, end_val]);
        let result = handlers.dispatch(0, &request, &cx).unwrap();
        assert_is_haskell_list(&result, &table);
    }

    // === Structural guard tests ===

    #[test]
    fn all_effect_constructors_in_table() {
        let table = full_effect_test_table();
        for decl in &tidepool_mcp::standard_decls() {
            for con_str in decl.constructors {
                let parsed = tidepool_mcp::parse_constructor(con_str).unwrap();
                let id = table.get_by_name(&parsed.name).unwrap_or_else(|| {
                    panic!(
                        "Constructor '{}' from effect '{}' missing from test DataConTable",
                        parsed.name, decl.type_name
                    )
                });
                let dc = table.get(id).unwrap();
                assert_eq!(
                    dc.rep_arity, parsed.arity,
                    "Arity mismatch for '{}': decl says {} but table has {}",
                    parsed.name, parsed.arity, dc.rep_arity
                );
            }
        }
    }

    const EFFECTS_WITH_ROUNDTRIP_TESTS: &[&str] = &[
        "Console", "KV", "Fs", "SG", "Http", "Exec", "Meta", "Git", "Llm", "Ask",
    ];

    #[test]
    fn all_effects_have_roundtrip_coverage() {
        let declared: Vec<&str> = tidepool_mcp::standard_decls()
            .iter()
            .map(|d| d.type_name)
            .collect();
        let missing: Vec<&&str> = declared
            .iter()
            .filter(|name| !EFFECTS_WITH_ROUNDTRIP_TESTS.contains(name))
            .collect();
        assert!(
            missing.is_empty(),
            "Effects in standard_decls() without roundtrip tests: {:?}\n\
             Add roundtrip tests and update EFFECTS_WITH_ROUNDTRIP_TESTS.",
            missing
        );
    }

    #[test]
    fn handler_order_matches_standard_decls() {
        let decls = tidepool_mcp::standard_decls();
        // HList order from main(): Console(0), KV(1), Fs(2), SG(3), Http(4), Exec(5), Meta(6), Git(7), Llm(8)
        // Ask(9) is handled by MCP server, not in main HList
        let expected = ["Console", "KV", "Fs", "SG", "Http", "Exec", "Meta", "Git", "Llm"];
        for (i, name) in expected.iter().enumerate() {
            assert_eq!(
                decls[i].type_name, *name,
                "Tag {} should be '{}' but standard_decls has '{}'",
                i, name, decls[i].type_name
            );
        }
    }

    // === Console roundtrip tests ===

    #[test]
    fn test_console_from_core_print() {
        let table = full_effect_test_table();
        let con_id = table.get_by_name("Print").unwrap();
        let msg = "hello".to_string().to_value(&table).unwrap();
        let val = Value::Con(con_id, vec![msg]);
        let req = ConsoleReq::from_value(&val, &table).unwrap();
        assert!(matches!(req, ConsoleReq::Print(ref s) if s == "hello"));
    }

    #[test]
    fn test_console_dispatch_roundtrip() {
        let table = full_effect_test_table();
        let captured = CapturedOutput::new();
        let cx = EffectContext::with_user(&table, &captured);
        let mut handlers = frunk::hlist![ConsoleHandler];
        let con_id = table.get_by_name("Print").unwrap();
        let msg = "test output".to_string().to_value(&table).unwrap();
        let request = Value::Con(con_id, vec![msg]);
        let _result = handlers.dispatch(0, &request, &cx).unwrap();
        assert_eq!(captured.drain(), vec!["test output".to_string()]);
    }

    // === KV roundtrip tests ===

    #[test]
    fn test_kv_from_core_keys() {
        let table = full_effect_test_table();
        let con_id = table.get_by_name("KvKeys").unwrap();
        let val = Value::Con(con_id, vec![]);
        let req = KvReq::from_value(&val, &table).unwrap();
        assert!(matches!(req, KvReq::Keys));
    }

    #[test]
    fn test_kv_from_core_get() {
        let table = full_effect_test_table();
        let con_id = table.get_by_name("KvGet").unwrap();
        let key = "mykey".to_string().to_value(&table).unwrap();
        let val = Value::Con(con_id, vec![key]);
        let req = KvReq::from_value(&val, &table).unwrap();
        assert!(matches!(req, KvReq::Get(ref k) if k == "mykey"));
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
        let result = handlers.dispatch(0, &request, &cx).unwrap();
        assert_is_haskell_list(&result, &table);
    }

    // === Fs roundtrip tests ===

    #[test]
    fn test_fs_from_core_exists() {
        let table = full_effect_test_table();
        let con_id = table.get_by_name("FsExists").unwrap();
        let path = "Cargo.toml".to_string().to_value(&table).unwrap();
        let val = Value::Con(con_id, vec![path]);
        let req = FsReq::from_value(&val, &table).unwrap();
        assert!(matches!(req, FsReq::Exists(ref p) if p == "Cargo.toml"));
    }

    #[test]
    fn test_fs_dispatch_roundtrip_exists() {
        let table = full_effect_test_table();
        let captured = CapturedOutput::new();
        let cx = EffectContext::with_user(&table, &captured);
        let mut handlers = frunk::hlist![FsHandler::new(repo_root())];
        let con_id = table.get_by_name("FsExists").unwrap();
        let path = "Cargo.toml".to_string().to_value(&table).unwrap();
        let request = Value::Con(con_id, vec![path]);
        let result = handlers.dispatch(0, &request, &cx).unwrap();
        // Should return True constructor
        match &result {
            Value::Con(id, _) => {
                let name = table.name_of(*id).unwrap();
                assert_eq!(name, "True", "Cargo.toml should exist");
            }
            other => panic!("Expected Con (Bool), got {:?}", other),
        }
    }

    #[test]
    fn test_fs_dispatch_roundtrip_listdir() {
        let table = full_effect_test_table();
        let captured = CapturedOutput::new();
        let cx = EffectContext::with_user(&table, &captured);
        let mut handlers = frunk::hlist![FsHandler::new(repo_root())];
        let con_id = table.get_by_name("FsListDir").unwrap();
        let path = ".".to_string().to_value(&table).unwrap();
        let request = Value::Con(con_id, vec![path]);
        let result = handlers.dispatch(0, &request, &cx).unwrap();
        // Returns [Text] — a cons-list of Text values
        assert_is_cons_list(&result, &table);
    }

    // === SG FromCore tests (side-effectful, no dispatch) ===

    #[test]
    fn test_sg_from_core_find() {
        let table = full_effect_test_table();
        let con_id = table.get_by_name("SgFind").unwrap();
        let lang_id = table.get_by_name("Rust").unwrap();
        let lang = Value::Con(lang_id, vec![]);
        let pattern = "fn $NAME".to_string().to_value(&table).unwrap();
        // Empty file list
        let nil_id = table.get_by_name("[]").unwrap();
        let files = Value::Con(nil_id, vec![]);
        let val = Value::Con(con_id, vec![lang, pattern, files]);
        let req = SgReq::from_value(&val, &table).unwrap();
        assert!(matches!(req, SgReq::Find(_, ref p, ref f) if p == "fn $NAME" && f.is_empty()));
    }

    // === Http FromCore tests (network side effects, no dispatch) ===

    #[test]
    fn test_http_from_core_get() {
        let table = full_effect_test_table();
        let con_id = table.get_by_name("HttpGet").unwrap();
        let url = "https://example.com".to_string().to_value(&table).unwrap();
        let val = Value::Con(con_id, vec![url]);
        let req = HttpReq::from_value(&val, &table).unwrap();
        assert!(matches!(req, HttpReq::Get(ref u) if u == "https://example.com"));
    }

    #[test]
    fn test_http_from_core_post() {
        let table = full_effect_test_table();
        let con_id = table.get_by_name("HttpPost").unwrap();
        let url = "https://example.com/api"
            .to_string()
            .to_value(&table)
            .unwrap();
        // Use a Null JSON value as body
        let null_id = table.get_by_name("Null").unwrap();
        let body = Value::Con(null_id, vec![]);
        let val = Value::Con(con_id, vec![url, body]);
        let req = HttpReq::from_value(&val, &table).unwrap();
        assert!(matches!(req, HttpReq::Post(ref u, _) if u == "https://example.com/api"));
    }

    // === Exec FromCore tests (shell side effects, no dispatch) ===

    #[test]
    fn test_exec_from_core_run() {
        let table = full_effect_test_table();
        let con_id = table.get_by_name("Run").unwrap();
        let cmd = "echo hello".to_string().to_value(&table).unwrap();
        let val = Value::Con(con_id, vec![cmd]);
        let req = ExecReq::from_value(&val, &table).unwrap();
        assert!(matches!(req, ExecReq::Run(ref c) if c == "echo hello"));
    }

    #[test]
    fn test_exec_from_core_run_in() {
        let table = full_effect_test_table();
        let con_id = table.get_by_name("RunIn").unwrap();
        let dir = "/tmp".to_string().to_value(&table).unwrap();
        let cmd = "ls".to_string().to_value(&table).unwrap();
        let val = Value::Con(con_id, vec![dir, cmd]);
        let req = ExecReq::from_value(&val, &table).unwrap();
        assert!(matches!(req, ExecReq::RunIn(ref d, ref c) if d == "/tmp" && c == "ls"));
    }

    // === Meta roundtrip tests ===

    #[test]
    fn test_meta_from_core_version() {
        let table = full_effect_test_table();
        let con_id = table.get_by_name("MetaVersion").unwrap();
        let val = Value::Con(con_id, vec![]);
        let req = MetaReq::from_value(&val, &table).unwrap();
        assert!(matches!(req, MetaReq::Version));
    }

    #[test]
    fn test_meta_from_core_constructors() {
        let table = full_effect_test_table();
        let con_id = table.get_by_name("MetaConstructors").unwrap();
        let val = Value::Con(con_id, vec![]);
        let req = MetaReq::from_value(&val, &table).unwrap();
        assert!(matches!(req, MetaReq::Constructors));
    }

    /// Assert a Value is a cons-list ([] or : _ _) without checking element types.
    fn assert_is_cons_list(val: &Value, table: &DataConTable) {
        match val {
            Value::Con(id, fields) => {
                let name = table.name_of(*id).unwrap();
                match name {
                    "[]" => assert!(fields.is_empty()),
                    ":" => {
                        assert_eq!(fields.len(), 2, "cons cell should have 2 fields");
                        assert_is_cons_list(&fields[1], table);
                    }
                    other => panic!("Expected list constructor, got {}", other),
                }
            }
            other => panic!("Expected Con (list), got {:?}", other),
        }
    }

    #[test]
    fn test_meta_dispatch_roundtrip_version() {
        let table = full_effect_test_table();
        let captured = CapturedOutput::new();
        let cx = EffectContext::with_user(&table, &captured);
        let mut handlers = frunk::hlist![MetaHandler::new(vec![], vec![])];
        let con_id = table.get_by_name("MetaVersion").unwrap();
        let request = Value::Con(con_id, vec![]);
        let result = handlers.dispatch(0, &request, &cx).unwrap();
        // MetaVersion returns a Text value
        match &result {
            Value::Con(id, _) => {
                let name = table.name_of(*id).unwrap();
                assert_eq!(name, "Text", "MetaVersion should return a Text");
            }
            _ => panic!("Expected Con (Text), got {:?}", result),
        }
    }

    #[test]
    fn test_meta_dispatch_roundtrip_primops() {
        let table = full_effect_test_table();
        let captured = CapturedOutput::new();
        let cx = EffectContext::with_user(&table, &captured);
        let mut handlers = frunk::hlist![MetaHandler::new(vec![], vec![])];
        let con_id = table.get_by_name("MetaPrimOps").unwrap();
        let request = Value::Con(con_id, vec![]);
        let result = handlers.dispatch(0, &request, &cx).unwrap();
        // Returns [Text] — list of strings (each string is a cons-list of chars)
        assert_is_cons_list(&result, &table);
    }

    // === Ask construction test (Ask is handled by MCP AskDispatcher, no FromCore type) ===

    #[test]
    fn test_ask_constructor_in_table() {
        let table = full_effect_test_table();
        let con_id = table.get_by_name("Ask").unwrap();
        let dc = table.get(con_id).unwrap();
        // Ask :: Text -> Ask Value  →  arity 1
        assert_eq!(dc.rep_arity, 1, "Ask should have arity 1");
        // Verify we can construct a well-formed Ask request Value
        let prompt = "What is your name?".to_string().to_value(&table).unwrap();
        let val = Value::Con(con_id, vec![prompt]);
        match &val {
            Value::Con(id, fields) => {
                assert_eq!(table.name_of(*id).unwrap(), "Ask");
                assert_eq!(fields.len(), 1);
            }
            _ => panic!("Expected Con"),
        }
    }

    // === JIT-level roundtrip tests ===
    // These compile Haskell through GHC + Cranelift JIT with the full handler stack.
    // They catch case trap (SIGILL) bugs that tree-walking eval misses.

    #[test]
    fn test_jit_console_roundtrip() {
        let result = jit_eval(&["say \"hello from JIT\"", "pure (toJSON True)"]);
        assert_eq!(result, serde_json::json!(true));
    }

    #[test]
    fn test_jit_kv_roundtrip() {
        let result = jit_eval(&[
            "kvSet \"jit_test\" (toJSON (42 :: Int))",
            "v <- kvGet \"jit_test\"",
            "pure (toJSON v)",
        ]);
        assert_eq!(result, serde_json::json!(42));
    }

    #[test]
    fn test_jit_fs_exists_roundtrip() {
        let result = jit_eval(&["b <- fsExists \"Cargo.toml\"", "pure (toJSON b)"]);
        assert_eq!(result, serde_json::json!(true));
    }

    #[test]
    fn test_jit_fs_listdir_roundtrip() {
        let result = jit_eval(&[
            "entries <- fsListDir \".\"",
            "pure (toJSON (length entries > 0))",
        ]);
        assert_eq!(result, serde_json::json!(true));
    }

    #[test]
    fn test_jit_meta_version_roundtrip() {
        let result = jit_eval(&["v <- metaVersion", "pure (toJSON v)"]);
        assert!(
            result.is_string(),
            "metaVersion should return a string, got {:?}",
            result
        );
    }

    #[test]
    fn test_jit_meta_primops_roundtrip() {
        let result = jit_eval(&["ops <- metaPrimOps", "pure (toJSON (length ops > 0))"]);
        assert_eq!(result, serde_json::json!(true));
    }

    #[test]
    fn test_jit_git_log_roundtrip() {
        let result = jit_eval(&[
            "commits <- gitLog \"HEAD\" 3",
            "pure (toJSON (length commits))",
        ]);
        let n = result.as_i64().unwrap();
        assert!(
            n > 0 && n <= 3,
            "gitLog should return 1-3 commits, got {}",
            n
        );
    }

    #[test]
    fn test_jit_git_show_roundtrip() {
        let result = jit_eval(&[
            "c <- gitShow \"HEAD\"",
            "pure (toJSON (c ^? key \"subject\" . _String))",
        ]);
        assert!(
            result.is_string(),
            "gitShow HEAD subject should be a string, got {:?}",
            result
        );
    }

    #[test]
    fn test_jit_git_diff_roundtrip() {
        let result = jit_eval(&["diffs <- gitDiff \"HEAD\"", "pure (toJSON (length diffs))"]);
        assert!(
            result.is_number(),
            "gitDiff should return a count, got {:?}",
            result
        );
    }

    #[test]
    fn test_jit_git_branches_roundtrip() {
        let result = jit_eval(&["bs <- gitBranches", "pure (toJSON (length bs > 0))"]);
        assert_eq!(result, serde_json::json!(true));
    }

    #[test]
    fn test_jit_git_tree_roundtrip() {
        let result = jit_eval(&[
            "entries <- gitTree \"HEAD\" \".\"",
            "pure (toJSON (length entries > 0))",
        ]);
        assert_eq!(result, serde_json::json!(true));
    }

    #[test]
    fn test_jit_git_blame_roundtrip() {
        let result = jit_eval(&[
            "hunks <- gitBlame \"Cargo.toml\" 1 3",
            "pure (toJSON (length hunks))",
        ]);
        let n = result.as_i64().unwrap();
        assert!(n > 0, "gitBlame should return at least 1 hunk, got {}", n);
    }

    // === Raw git2 tests ===

    #[test]
    fn test_git_open_repo() {
        let (handler, _) = git_handler();
        handler.open_repo().expect("should open repo");
    }

    #[test]
    fn test_git_branches() {
        let (_, repo) = git_handler();
        let branches = repo.branches(None).unwrap();
        let names: Vec<String> = branches
            .filter_map(|b| b.ok())
            .filter_map(|(b, _)| b.name().ok().flatten().map(String::from))
            .collect();
        assert!(!names.is_empty(), "should have at least one branch");
        assert!(
            names.iter().any(|n| n == "main" || n == "master"),
            "should have main or master branch, got: {:?}",
            names
        );
    }

    #[test]
    fn test_git_log() {
        let (_, repo) = git_handler();
        let obj = repo.revparse_single("HEAD").unwrap();
        let mut revwalk = repo.revwalk().unwrap();
        revwalk.push(obj.id()).unwrap();
        revwalk.set_sorting(git2::Sort::TIME).ok();
        let commits: Vec<git2::Oid> = revwalk.take(3).filter_map(|r| r.ok()).collect();
        assert!(!commits.is_empty(), "should have at least one commit");
        let commit = repo.find_commit(commits[0]).unwrap();
        assert!(
            commit.summary().is_some(),
            "HEAD commit should have a summary"
        );
    }

    #[test]
    fn test_git_show() {
        let (_, repo) = git_handler();
        let head = repo.head().unwrap().peel_to_commit().unwrap();
        let oid = head.id();
        let commit = repo.find_commit(oid).unwrap();
        assert!(commit.summary().is_some());
        assert!(commit.author().name().is_some());
    }

    #[test]
    fn test_git_diff() {
        let (_, repo) = git_handler();
        let head = repo.head().unwrap().peel_to_commit().unwrap();
        let tree = head.tree().unwrap();
        if head.parent_count() > 0 {
            let parent_tree = head.parent(0).unwrap().tree().unwrap();
            let diff = repo
                .diff_tree_to_tree(Some(&parent_tree), Some(&tree), None)
                .unwrap();
            // Just verify it doesn't panic — diff may be empty for merge commits
            let _count = diff.deltas().count();
        }
    }

    #[test]
    fn test_git_tree() {
        let (_, repo) = git_handler();
        let head = repo.head().unwrap().peel_to_commit().unwrap();
        let tree = head.tree().unwrap();
        let entries: Vec<String> = tree
            .iter()
            .filter_map(|e| e.name().map(String::from))
            .collect();
        assert!(!entries.is_empty(), "root tree should have entries");
        assert!(
            entries
                .iter()
                .any(|e| e == "Cargo.toml" || e == "CLAUDE.md"),
            "root tree should contain known files, got: {:?}",
            entries
        );
    }

    #[test]
    fn test_git_blame() {
        let (handler, repo) = git_handler();
        let mut opts = git2::BlameOptions::new();
        opts.min_line(1);
        opts.max_line(5);
        let blame = repo
            .blame_file(std::path::Path::new("Cargo.toml"), Some(&mut opts))
            .unwrap();
        assert!(blame.len() > 0, "blame should have at least one hunk");
        let hunk = blame.get_index(0).unwrap();
        assert!(
            hunk.final_signature().name().is_some(),
            "blame hunk should have an author"
        );
        // Verify file content reading works
        let content = std::fs::read_to_string(handler.root.join("Cargo.toml")).unwrap();
        assert!(!content.is_empty());
    }

    // === LLM structured JSON round-trip through JIT ===

    /// Mock LLM handler that returns a fixed JSON response instead of calling the API.
    #[derive(Clone)]
    struct MockLlmHandler {
        response: serde_json::Value,
    }

    impl DescribeEffect for MockLlmHandler {
        fn effect_decl() -> EffectDecl {
            tidepool_mcp::llm_decl()
        }
    }

    impl EffectHandler<CapturedOutput> for MockLlmHandler {
        type Request = LlmReq;
        fn handle(
            &mut self,
            req: LlmReq,
            cx: &EffectContext<'_, CapturedOutput>,
        ) -> Result<Value, EffectError> {
            match req {
                LlmReq::Chat(_) => cx.respond("mock response".to_string()),
                LlmReq::Structured(_, _) => cx.respond(self.response.clone()),
            }
        }
    }

    fn jit_eval_with_mock_llm(code: &[&str], mock_response: serde_json::Value) -> serde_json::Value {
        let source = jit_test_source(code);
        let include = prelude_include();
        let include_paths: Vec<&std::path::Path> = vec![include.as_path()];
        let kv_path = std::env::temp_dir().join("tidepool_mock_llm_kv.json");
        let cwd = repo_root();
        let captured = CapturedOutput::new();
        let mut handlers = frunk::hlist![
            ConsoleHandler,
            KvHandler::new(kv_path),
            FsHandler::new(cwd.clone()),
            SgHandler::new(cwd.clone()),
            HttpHandler,
            ExecHandler::new(cwd.clone()),
            MetaHandler::new(vec![], vec![]),
            GitHandler::new(cwd),
            MockLlmHandler { response: mock_response }
        ];
        let result = tidepool_runtime::compile_and_run(
            &source,
            "result",
            &include_paths,
            &mut handlers,
            &captured,
        );
        match result {
            Ok(eval_result) => eval_result.to_json(),
            Err(e) => panic!("JIT eval failed: {:?}", e),
        }
    }

    #[test]
    fn test_llm_structured_simple_object() {
        let mock = serde_json::json!({"greeting": "hello"});
        let result = jit_eval_with_mock_llm(
            &["llmJson \"test\" (SObj [(\"greeting\", SStr)])"],
            mock,
        );
        assert_eq!(result["greeting"], "hello");
    }

    #[test]
    fn test_llm_structured_nested_object() {
        let mock = serde_json::json!({
            "languages": [
                {"name": "Haskell", "year": 1990},
                {"name": "Rust", "year": 2010},
                {"name": "Python", "year": 1991}
            ]
        });
        let result = jit_eval_with_mock_llm(
            &["llmJson \"test\" (SObj [(\"languages\", SArr (SObj [(\"name\", SStr), (\"year\", SNum)]))])"],
            mock,
        );
        let langs = result["languages"].as_array().expect("languages should be array");
        assert_eq!(langs.len(), 3);
        assert_eq!(langs[0]["name"], "Haskell");
    }

    #[test]
    fn test_llm_structured_encode_roundtrip() {
        // Roundtrip: llmJson → show result
        let mock = serde_json::json!({"greeting": "hello"});
        let result = jit_eval_with_mock_llm(
            &[
                "r <- llmJson \"test\" (SObj [(\"greeting\", SStr)])",
                "say (show r)",
                "pure r",
            ],
            mock,
        );
        assert_eq!(result["greeting"], "hello");
    }

    #[test]
    fn test_llm_structured_nested_encode_roundtrip() {
        let mock = serde_json::json!({
            "languages": [
                {"name": "Haskell", "year": 1990},
                {"name": "Rust", "year": 2010}
            ]
        });
        let result = jit_eval_with_mock_llm(
            &[
                "r <- llmJson \"test\" (SObj [(\"languages\", SArr (SObj [(\"name\", SStr), (\"year\", SNum)]))])",
                "say (show r)",
                "pure r",
            ],
            mock,
        );
        let langs = result["languages"].as_array().expect("languages should be array");
        assert_eq!(langs.len(), 2);
    }

    #[test]
    fn test_llm_structured_empty_object() {
        let mock = serde_json::json!({});
        let result = jit_eval_with_mock_llm(
            &["llmJson \"test\" (SObj [])"],
            mock,
        );
        assert!(result.is_object());
        assert_eq!(result.as_object().unwrap().len(), 0);
    }

    #[test]
    fn test_llm_structured_mixed_types() {
        let mock = serde_json::json!({
            "name": "test",
            "count": 42,
            "active": true
        });
        let result = jit_eval_with_mock_llm(
            &["llmJson \"test\" (SObj [(\"name\", SStr), (\"count\", SNum), (\"active\", SBool)])"],
            mock,
        );
        assert_eq!(result["name"], "test");
        assert_eq!(result["count"], 42);
        assert_eq!(result["active"], true);
    }

    #[test]
    fn test_llm_structured_mixed_encode_roundtrip() {
        let mock = serde_json::json!({
            "name": "test",
            "count": 42,
            "active": true
        });
        let result = jit_eval_with_mock_llm(
            &[
                "r <- llmJson \"test\" (SObj [(\"name\", SStr), (\"count\", SNum), (\"active\", SBool)])",
                "say (show r)",
                "pure r",
            ],
            mock,
        );
        assert_eq!(result["name"], "test");
        assert_eq!(result["count"], 42);
        assert_eq!(result["active"], true);
    }

}
