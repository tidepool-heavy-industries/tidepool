use std::collections::HashMap;
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
            let _ = std::fs::create_dir_all(parent);
        }
        let _ = std::fs::write(
            &self.path,
            serde_json::to_string_pretty(store).unwrap_or_default(),
        );
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
                if path
                    .file_name()
                    .is_some_and(|n| n.to_string_lossy().starts_with('.'))
                {
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

        let stdout = String::from_utf8_lossy(&output.stdout).to_string();
        let stderr = String::from_utf8_lossy(&output.stderr).to_string();
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
}

#[derive(Clone)]
struct MetaHandler {
    effect_names: Vec<String>,
}

impl MetaHandler {
    fn new(effect_names: Vec<String>) -> Self {
        Self { effect_names }
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
fn ensure_prelude() -> PathBuf {
    if let Some(dir) = std::env::var_os("TIDEPOOL_PRELUDE_DIR") {
        return PathBuf::from(dir);
    }

    // In-repo development: use haskell/lib/ directly if present
    if let Ok(cwd) = std::env::current_dir() {
        let from_root = cwd.join("haskell").join("lib");
        if from_root.join("Tidepool").join("Prelude.hs").exists() {
            return from_root;
        }
        let from_haskell = cwd.join("lib");
        if from_haskell.join("Tidepool").join("Prelude.hs").exists() {
            return from_haskell;
        }
    }

    // Installed mode: write embedded files to ~/.tidepool/prelude/
    let base = dirs::home_dir()
        .expect("could not determine home directory")
        .join(".tidepool")
        .join("prelude");
    let stamp = base.join(".version");
    let version = env!("CARGO_PKG_VERSION");
    if stamp.exists() && std::fs::read_to_string(&stamp).ok().as_deref() == Some(version) {
        return base;
    }

    for (path, content) in EMBEDDED_FILES {
        let full = base.join(path);
        if let Some(parent) = full.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let _ = std::fs::write(&full, content);
    }
    let _ = std::fs::write(&stamp, version);
    base
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
// Main
// ---------------------------------------------------------------------------

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("warn")),
        )
        .with_writer(std::io::stderr)
        .init();

    let prelude_dir = ensure_prelude();

    // If tidepool-extract is not available, serve the degraded setup server.
    if find_tidepool_extract().is_none() {
        tracing::warn!(
            "tidepool-extract not found — serving setup-only MCP server. \
             Install via: nix profile install github:tidepool-heavy-industries/tidepool#tidepool-extract"
        );
        use rmcp::ServiceExt;
        SetupServer
            .serve((tokio::io::stdin(), tokio::io::stdout()))
            .await?
            .waiting()
            .await?;
        return Ok(());
    }

    // Install sigsetjmp/siglongjmp signal handlers early so SIGILL/SIGSEGV
    // from JIT code returns clean errors instead of killing the server.
    tidepool_codegen::signal_safety::install();

    let cwd = std::env::current_dir()?;
    let tidepool_dir = cwd.join(".tidepool");
    let kv_path = tidepool_dir.join("kv.json");
    let effect_names: Vec<String> = tidepool_mcp::standard_decls()
        .iter()
        .map(|d| d.type_name.to_string())
        .collect();
    let handlers = frunk::hlist![
        ConsoleHandler,
        KvHandler::new(kv_path),
        FsHandler::new(cwd.clone()),
        SgHandler::new(cwd.clone()),
        HttpHandler,
        ExecHandler::new(cwd.clone()),
        MetaHandler::new(effect_names)
    ];

    let server = TidepoolMcpServer::new(handlers).with_prelude(prelude_dir);
    server.serve_stdio().await
}
