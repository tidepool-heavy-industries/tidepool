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

mod config;
use config::Config;

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
    ) -> Result<tidepool_effect::Response, EffectError> {
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
                Ok(contents) => match serde_json::from_str(&contents) {
                    Ok(map) => map,
                    Err(e) => {
                        tracing::warn!(
                            "KV store at {:?} contains invalid JSON ({}), starting fresh",
                            path,
                            e
                        );
                        HashMap::new()
                    }
                },
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
                tracing::warn!("KV flush: failed to create dir {:?}: {}", parent, e);
                return;
            }
        }
        match serde_json::to_string_pretty(store) {
            Ok(json) => {
                if let Err(e) = std::fs::write(&self.path, json) {
                    tracing::warn!("KV flush: failed to write {:?}: {}", self.path, e);
                }
            }
            Err(e) => {
                tracing::warn!("KV flush: serialization failed: {}", e);
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
    ) -> Result<tidepool_effect::Response, EffectError> {
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
    #[core(name = "FsGrep")]
    Grep(String, String),
    #[core(name = "FsExists")]
    Exists(String),
    #[core(name = "FsMetadata")]
    Metadata(String),
    #[core(name = "TryFsRead")]
    TryRead(String),
}

const DEFAULT_IGNORE_DIRS: &[&str] = &["target", ".git", "node_modules", "dist-newstyle"];

fn pattern_mentions(pattern: &str, dir: &str) -> bool {
    pattern.split(['/', '\\']).any(|c| c == dir)
}

/// Ripgrep-style default exclusions: skip paths containing a default-ignored
/// directory (build artifacts) or any HIDDEN component (dot-prefixed — VCS
/// stores, tool worktrees, caches), unless the glob pattern explicitly names
/// that component (e.g. `.tidepool/lib/*.hs` still traverses `.tidepool`).
fn component_filter(pattern: &str, rel_path: &std::path::Path) -> bool {
    for component in rel_path.components() {
        if let std::path::Component::Normal(name) = component {
            let name_str = name.to_string_lossy();
            let excluded =
                DEFAULT_IGNORE_DIRS.contains(&name_str.as_ref()) || name_str.starts_with('.');
            if excluded && !pattern_mentions(pattern, &name_str) {
                return false;
            }
        }
    }
    true
}

#[derive(Clone)]
struct FsHandler {
    root: PathBuf,
}

impl FsHandler {
    fn new(root: PathBuf) -> Self {
        Self { root }
    }

    fn expand_glob(&self, pattern: &str) -> Result<Vec<PathBuf>, EffectError> {
        if pattern.contains("..") {
            return Ok(Vec::new());
        }
        if pattern.starts_with('/') || pattern.starts_with('\\') {
            return Err(EffectError::Handler(
                "absolute glob patterns not allowed".to_string(),
            ));
        }
        // The glob crate's `**` matches DIRECTORIES only, so a bare trailing
        // `/**` silently returns dirs and no files — never what the caller
        // meant. Normalize to `/**/*` (everything, any depth).
        let normalized;
        let pattern = if pattern == "**" || pattern.ends_with("/**") {
            normalized = format!("{}/*", pattern);
            normalized.as_str()
        } else {
            pattern
        };
        let full_pattern = self.root.join(pattern).to_string_lossy().to_string();
        let canonical_root = self
            .root
            .canonicalize()
            .map_err(|e| EffectError::Handler(e.to_string()))?;

        let paths: Vec<PathBuf> = glob::glob(&full_pattern)
            .map_err(|e| EffectError::Handler(format!("invalid glob: {}", e)))?
            .filter_map(std::result::Result::ok)
            .filter(|p| {
                p.canonicalize()
                    .map(|cp| cp.starts_with(&canonical_root))
                    .unwrap_or(false)
            })
            .filter(|p| {
                let rel_path = p.strip_prefix(&self.root).unwrap_or(p);
                component_filter(pattern, rel_path)
            })
            .collect();
        Ok(paths)
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

    /// Fallible core for reading a file: shared by the plain
    /// (`?`-propagating) and isolating (`respond_caught`) handler arms. The
    /// path is folded into the error so an isolated `Left` carries operation
    /// context. See `respond_caught` for the policy + block-level-`try` seam.
    fn read_core(&self, path: &str) -> Result<String, EffectError> {
        let resolved = self.resolve(path)?;
        std::fs::read_to_string(&resolved)
            .map_err(|e| EffectError::Handler(format!("read '{}' failed: {}", path, e)))
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
    ) -> Result<tidepool_effect::Response, EffectError> {
        match req {
            FsReq::Read(path) => cx.respond(self.read_core(&path)?),
            // Isolating: a read error (missing file, permission, non-UTF-8)
            // becomes Left instead of aborting the eval.
            FsReq::TryRead(path) => cx.respond_caught(self.read_core(&path)),
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
                    .filter_map(std::result::Result::ok)
                    .map(|e| e.file_name().to_string_lossy().to_string())
                    .collect();
                entries.sort();
                cx.respond_list(entries)
            }
            FsReq::Glob(pattern) => {
                let paths = self.expand_glob(&pattern)?;
                let rel_paths: Vec<String> = paths
                    .into_iter()
                    .filter_map(|p| {
                        p.strip_prefix(&self.root)
                            .ok()
                            .map(|r| r.to_string_lossy().to_string())
                    })
                    .collect();
                cx.respond_list(rel_paths)
            }
            FsReq::Grep(regex_str, pattern) => {
                let re = regex::Regex::new(&regex_str)
                    .map_err(|e| EffectError::Handler(format!("invalid regex: {}", e)))?;
                let paths = self.expand_glob(&pattern)?;
                let mut results: Vec<(String, i64, String)> = Vec::new();
                let mut more_matches = 0;
                let cap = 2000;

                for path in paths {
                    if !path.is_file() {
                        continue;
                    }
                    let content = match std::fs::read(&path) {
                        Ok(b) => b,
                        Err(_) => continue,
                    };
                    if content.contains(&0) {
                        continue;
                    }
                    let text = match String::from_utf8(content) {
                        Ok(s) => s,
                        Err(_) => continue,
                    };

                    let rel_path = path
                        .strip_prefix(&self.root)
                        .unwrap_or(&path)
                        .to_string_lossy()
                        .to_string();

                    for (i, line) in text.lines().enumerate() {
                        if re.is_match(line) {
                            if results.len() >= cap {
                                more_matches += 1;
                                continue;
                            }
                            results.push((rel_path.clone(), (i + 1) as i64, line.to_string()));
                        }
                    }
                }

                if more_matches > 0 {
                    results.push((
                        "...".to_string(),
                        0,
                        format!("truncated: {} more matches", more_matches),
                    ));
                }

                cx.respond_list(results)
            }
            FsReq::Exists(path) => {
                let resolved = self.resolve(&path)?;
                cx.respond(resolved.exists())
            }
            FsReq::Metadata(path) => {
                let resolved = self.resolve(&path)?;
                // Value-native: a JSON object on success, Null for a missing/
                // unreadable path (so it doubles as an existence check — no throw).
                match std::fs::metadata(&resolved) {
                    Ok(meta) => cx.respond(serde_json::json!({
                        "size": meta.len() as i64,
                        "is_file": meta.is_file(),
                        "is_dir": meta.is_dir(),
                    })),
                    Err(_) => cx.respond(serde_json::Value::Null),
                }
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
    #[core(name = "SgRuleFind")]
    RuleFind(Lang, Value, Vec<String>),
    #[core(name = "SgPlan")]
    Plan(Lang, String, String, Vec<String>),
    #[core(name = "SgApply")]
    Apply(Lang, String, String, Vec<String>),
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
        let canonical_root = self
            .root
            .canonicalize()
            .map_err(|e| EffectError::Handler(e.to_string()))?;
        if paths.is_empty() {
            self.walk_dir(&canonical_root, lang, &mut files)?;
        } else {
            for p in paths {
                let full = self.root.join(p);
                let canonical = full
                    .canonicalize()
                    .map_err(|e| EffectError::Handler(format!("Bad path {}: {}", p, e)))?;
                if !canonical.starts_with(&canonical_root) {
                    return Err(EffectError::Handler(format!("Path escapes sandbox: {}", p)));
                }
                if canonical.is_file() {
                    files.push(canonical);
                } else if canonical.is_dir() {
                    self.walk_dir(&canonical, lang, &mut files)?;
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

    /// Build a pattern, rejecting ones that parse with syntax errors —
    /// those "succeed" and then silently match nothing (the classic
    /// `pub fn $NAME($$ARGS)` footgun: a signature without a body is not
    /// a valid parse).
    fn checked_pattern(pattern: &str, sl: SupportLang) -> Result<Pattern, EffectError> {
        let pat = Pattern::try_new(pattern, sl)
            .map_err(|e| EffectError::Handler(format!("invalid pattern: {}", e)))?;
        if pat.has_error() {
            return Err(EffectError::Handler(format!(
                "pattern `{}` parses with syntax errors as {:?} and would likely match \
                 nothing. Patterns must be valid code fragments — e.g. a bare fn \
                 signature needs a body (`{} {{ $$$BODY }}`). For definition lookup, \
                 the rsFn/hsDef recipes (kind + name regex) are the robust path.",
                pattern, sl, pattern
            )));
        }
        // The OTHER footgun parses cleanly: a bare Rust fn signature is a
        // valid `function_signature_item` (trait/extern item) — which never
        // occurs in normal code, so the pattern silently matches nothing.
        if matches!(sl, SupportLang::Rust) {
            use ast_grep_core::Matcher as _;
            let sig_kind = sl.kind_to_id("function_signature_item");
            if sig_kind != 0 {
                if let Some(kinds) = pat.potential_kinds() {
                    if kinds.contains(sig_kind as usize) {
                        return Err(EffectError::Handler(format!(
                            "pattern `{}` parses as a fn SIGNATURE (trait/extern item) and \
                             will not match function definitions. Append a body — \
                             `{} {{ $$$BODY }}` — or use the rsFn recipe.",
                            pattern, pattern
                        )));
                    }
                }
            }
        }
        Ok(pat)
    }

    fn run_find(
        &self,
        lang: Lang,
        pattern: &str,
        paths: &[String],
        rewrite: Option<&str>,
    ) -> Result<Vec<SgMatch>, EffectError> {
        let sl = lang.to_support_lang()?;
        let pat = Self::checked_pattern(pattern, sl)?;
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

    /// Apply a pattern rewrite in place across matching files; returns the
    /// number of edits. The library-level `rewrite` verb gates this behind
    /// a continuation approval carrying the SgPlan diff — prefer that flow.
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
                // Rebuilt per iteration: `replace` consumes the pattern.
                let pat = Self::checked_pattern(pattern, sl)?;
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
    ) -> Result<tidepool_effect::Response, EffectError> {
        match req {
            SgReq::Find(lang, pattern, paths) => {
                let matches = self.run_find(lang, &pattern, &paths, None)?;
                cx.respond_list(matches)
            }
            SgReq::RuleFind(lang, rule_json, paths) => {
                let matches = self.run_rule_find(lang, &rule_json, &paths, None, cx.table())?;
                cx.respond_list(matches)
            }
            SgReq::Plan(lang, pattern, rewrite, paths) => {
                // Dry run: matches with the replacement field filled, no writes.
                let matches = self.run_find(lang, &pattern, &paths, Some(&rewrite))?;
                cx.respond_list(matches)
            }
            SgReq::Apply(lang, pattern, rewrite, paths) => {
                let n = self.run_replace(lang, &pattern, &rewrite, &paths)?;
                cx.respond(n)
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
    #[core(name = "TryHttpGet")]
    TryGet(String),
    #[core(name = "TryHttpPost")]
    TryPost(String, Value),
    #[core(name = "ParseJson")]
    ParseJson(String),
    #[core(name = "TryParseJson")]
    TryParseJson(String),
}

/// Parse a JSON string into a `serde_json::Value` (the bridge then marshals it to
/// the eval's `Value`, same path as `HttpGet`). Spec-compliant via serde_json.
fn parse_json_str(s: &str) -> Result<serde_json::Value, EffectError> {
    serde_json::from_str(s).map_err(|e| EffectError::Handler(format!("invalid JSON: {e}")))
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

    /// Fallible core for GET: shared by the plain (`?`-propagating) and the
    /// isolating (`respond_caught`) handler arms. See `respond_caught` for the
    /// catch-vs-propagate policy and the block-level-`try` seam note.
    fn get(&self, url_str: &str) -> Result<serde_json::Value, EffectError> {
        let url = Self::validate_url(url_str)?;
        let resp = ureq::get(url.as_str())
            .timeout(std::time::Duration::from_secs(30))
            .call()
            .map_err(|e| EffectError::Handler(format!("HTTP GET '{}' failed: {}", url_str, e)))?;
        let body = resp.into_string().map_err(|e| {
            EffectError::Handler(format!("Read body from '{}' failed: {}", url_str, e))
        })?;
        Self::parse_response(url_str, &body)
    }

    /// Fallible core for POST (body pre-converted to JSON by the caller, which
    /// holds the `EffectContext` needed for `value_to_json`).
    fn post(
        &self,
        url_str: &str,
        json_body: &serde_json::Value,
    ) -> Result<serde_json::Value, EffectError> {
        let url = Self::validate_url(url_str)?;
        let resp = ureq::post(url.as_str())
            .timeout(std::time::Duration::from_secs(30))
            .send_json(json_body)
            .map_err(|e| EffectError::Handler(format!("HTTP POST '{}' failed: {}", url_str, e)))?;
        let body = resp.into_string().map_err(|e| {
            EffectError::Handler(format!("Read body from '{}' failed: {}", url_str, e))
        })?;
        Self::parse_response(url_str, &body)
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
    ) -> Result<tidepool_effect::Response, EffectError> {
        match req {
            HttpReq::Get(url_str) => cx.respond(self.get(&url_str)?),
            HttpReq::Post(url_str, body_val) => {
                let json_body = tidepool_runtime::value_to_json(&body_val, cx.table(), 0);
                cx.respond(self.post(&url_str, &json_body)?)
            }
            // Isolating: a network error or non-2xx status becomes Left.
            HttpReq::TryGet(url_str) => cx.respond_caught(self.get(&url_str)),
            HttpReq::TryPost(url_str, body_val) => {
                let json_body = tidepool_runtime::value_to_json(&body_val, cx.table(), 0);
                cx.respond_caught(self.post(&url_str, &json_body))
            }
            // Parse a JSON string Rust-side (serde_json); ParseJson raises on
            // invalid JSON, TryParseJson isolates it to Left.
            HttpReq::ParseJson(s) => cx.respond(parse_json_str(&s)?),
            HttpReq::TryParseJson(s) => cx.respond_caught(parse_json_str(&s)),
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
    #[core(name = "TryRun")]
    TryRun(String),
    #[core(name = "TryRunIn")]
    TryRunIn(String, String),
}

#[derive(Clone)]
struct ExecHandler {
    root: PathBuf,
}

impl ExecHandler {
    fn new(root: PathBuf) -> Self {
        Self { root }
    }

    /// Maximum stdout/stderr size for Run/RunIn (2 MB). Prevents OOM from
    /// commands that produce unbounded output (e.g. `find /`, `yes`).
    const MAX_EXEC_OUTPUT_BYTES: usize = 2 * 1024 * 1024;

    fn resolve_dir(&self, rel: &str) -> Result<PathBuf, EffectError> {
        let target = self.root.join(rel);
        let canonical_root = self
            .root
            .canonicalize()
            .map_err(|e| EffectError::Handler(e.to_string()))?;
        let canonical = target
            .canonicalize()
            .map_err(|e| EffectError::Handler(format!("Cannot resolve directory: {}", e)))?;
        if !canonical.starts_with(&canonical_root) {
            return Err(EffectError::Handler(format!(
                "Path escapes sandbox: {}",
                rel
            )));
        }
        Ok(canonical)
    }

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
    ) -> Result<tidepool_effect::Response, EffectError> {
        match req {
            ExecReq::Run(cmd) => {
                let (code, stdout, stderr) = self.run_command(&cmd, &self.root)?;
                cx.respond((code, stdout, stderr))
            }
            ExecReq::RunIn(dir, cmd) => {
                let target = self.resolve_dir(&dir)?;
                let (code, stdout, stderr) = self.run_command(&cmd, &target)?;
                cx.respond((code, stdout, stderr))
            }
            // Isolating: a SPAWN failure (sandbox escape, exec error) becomes
            // Left. A nonzero exit is NOT a failure — run_command returns it in
            // the Ok tuple, so it arrives as Right (code, out, err).
            ExecReq::TryRun(cmd) => cx.respond_caught(self.run_command(&cmd, &self.root)),
            ExecReq::TryRunIn(dir, cmd) => cx.respond_caught(
                self.resolve_dir(&dir)
                    .and_then(|target| self.run_command(&cmd, &target)),
            ),
        }
    }
}

// (GitHandler removed — use `run "git ..."` instead)

// === Tag 8: Llm (LLM calls via genai — multi-provider, model-name routed) ===

#[derive(FromCore)]
enum LlmReq {
    #[core(name = "LlmStructured")]
    Structured(String, Value), // Value is the Schema ADT, decoded in handler
    #[core(name = "TryLlmStructured")]
    TryStructured(String, Value),
}

/// Default OpenAI model when TIDEPOOL_LLM_PROVIDER=openai (deprecated alias)
/// is set without an OpenAI-shaped model name.
const DEFAULT_OPENAI_MODEL: &str = "gpt-4o-mini";

/// All LLM traffic goes through genai (the known-good multi-provider crate);
/// the adapter is routed from the model name (gpt-* → OpenAI, claude-* →
/// Anthropic, gemini-* → Gemini, unknown → Ollama; `ns::model` forces a
/// namespace). API keys come from the standard env vars (OPENAI_API_KEY,
/// ANTHROPIC_API_KEY, ...), which `load_secrets` can fill from
/// `.tidepool/secrets/` at startup.
struct LlmHandler {
    client: genai::Client,
    model: String,
    rt: tokio::runtime::Handle,
    call_count: std::sync::Arc<std::sync::atomic::AtomicU32>,
}

/// The server clones the handler stack once per eval, so Clone is the
/// per-eval budget boundary: a FRESH call counter per clone. (A derived
/// Clone shared the Arc — making LLM_MAX_CALLS a server-LIFETIME cap that
/// silently starved the Llm effect partway through long sessions; found
/// 2026-06-11 while auditing orchestration limits.)
impl Clone for LlmHandler {
    fn clone(&self) -> Self {
        Self {
            client: self.client.clone(),
            model: self.model.clone(),
            rt: self.rt.clone(),
            call_count: std::sync::Arc::new(std::sync::atomic::AtomicU32::new(0)),
        }
    }
}

const LLM_MAX_CALLS: u32 = 200;

impl LlmHandler {
    /// TIDEPOOL_LLM_PROVIDER=openai is a deprecated alias from before the
    /// genai consolidation: coerce the model into OpenAI's namespace
    /// (strip a legacy `openai:` prefix; keep a bare model name; replace a
    /// foreign-provider name with the default OpenAI model). Without the
    /// alias the model passes through untouched and genai routes by name.
    fn effective_model(model: String) -> String {
        let provider = std::env::var("TIDEPOOL_LLM_PROVIDER").unwrap_or_default();
        if !provider.eq_ignore_ascii_case("openai") {
            return model;
        }
        let coerced = if let Some(m) = model.strip_prefix("openai:") {
            m.to_string()
        } else if !model.is_empty() && !model.contains(':') {
            model
        } else {
            DEFAULT_OPENAI_MODEL.to_string()
        };
        tracing::warn!(
            "TIDEPOOL_LLM_PROVIDER=openai is deprecated; genai routes by model name \
             (set TIDEPOOL_LLM_MODEL={} instead)",
            coerced
        );
        coerced
    }

    /// Legacy single-colon provider prefixes ("ollama:llama3.2") predate
    /// the genai consolidation and were passed to providers VERBATIM
    /// (ollama 404s on a model literally named "ollama:llama3.2", found
    /// live 2026-06-11). genai's namespace syntax is `ns::model` — the
    /// namespace is stripped before the request — so rewrite a single
    /// colon to a double one, but ONLY for known provider prefixes:
    /// ollama's own tag syntax uses a bare colon ("qwen2.5:7b") and must
    /// pass through untouched.
    fn normalize_model(model: String) -> String {
        const PROVIDERS: &[&str] = &[
            "ollama",
            "openai",
            "anthropic",
            "gemini",
            "groq",
            "cohere",
            "deepseek",
            "xai",
            "fireworks",
            "together",
        ];
        if let Some(idx) = model.find(':') {
            let is_legacy_prefix =
                model.as_bytes().get(idx + 1) != Some(&b':') && PROVIDERS.contains(&&model[..idx]);
            if is_legacy_prefix {
                let fixed = format!("{}::{}", &model[..idx], &model[idx + 1..]);
                tracing::info!("normalized legacy model name {model:?} -> {fixed:?}");
                return fixed;
            }
        }
        model
    }

    fn new(model: String) -> Self {
        Self {
            client: genai::Client::default(),
            model: Self::normalize_model(Self::effective_model(model)),
            rt: tokio::runtime::Handle::current(),
            call_count: std::sync::Arc::new(std::sync::atomic::AtomicU32::new(0)),
        }
    }

    /// Does the configured model route to an OpenAI-family adapter?
    /// (Decides whether the structured-output schema needs strictifying.)
    fn targets_openai(&self) -> bool {
        matches!(
            genai::adapter::AdapterKind::from_model(&self.model),
            Ok(genai::adapter::AdapterKind::OpenAI | genai::adapter::AdapterKind::OpenAIResp)
        )
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

    /// Fallible core for a structured (JSON-schema) call. `schema_json` is the
    /// already-`value_to_json`'d Schema (the caller holds the `EffectContext`);
    /// strictification for OpenAI-family models happens here.
    fn structured_core(
        &self,
        prompt: String,
        mut schema_json: serde_json::Value,
    ) -> Result<serde_json::Value, EffectError> {
        // Providers (OpenAI especially) require a top-level `type: "object"`
        // structured-output schema. If the caller passed a non-object schema
        // (e.g. `llm (SEnum …)` / `llm SStr …`), wrap it as `{value: <schema>}`
        // and unwrap the result so the bare value rides back transparently.
        let wrapped = schema_json.get("type").and_then(|t| t.as_str()) != Some("object");
        if wrapped {
            schema_json = serde_json::json!({
                "type": "object",
                "properties": { "value": schema_json },
                "required": ["value"],
            });
        }
        if self.targets_openai() {
            strictify(&mut schema_json);
        }
        let json_spec = genai::chat::JsonSpec::new("structured_output", schema_json);
        let opts = genai::chat::ChatOptions::default()
            .with_response_format(genai::chat::ChatResponseFormat::JsonSpec(json_spec));
        let req = genai::chat::ChatRequest::from_user(format!(
            "{}\n\nRespond with ONLY valid JSON matching the provided schema. No markdown, no explanation.",
            prompt
        ));
        let resp = self
            .rt
            .block_on(self.client.exec_chat(&self.model, req, Some(&opts)))
            .map_err(|e| EffectError::Handler(format!("LLM structured call failed: {}", e)))?;
        let text = resp.first_text().unwrap_or("null");
        let mut out = serde_json::from_str(text).unwrap_or(serde_json::Value::Null);
        if wrapped {
            out = out
                .get_mut("value")
                .map(serde_json::Value::take)
                .unwrap_or(serde_json::Value::Null);
        }
        Ok(out)
    }
}

/// OpenAI strict structured outputs require EVERY property to appear in
/// `required` (optionality is expressed as a null-union instead), and genai's
/// OpenAI adapter hardcodes `strict: true` + `additionalProperties: false`.
/// Our Haskell `schemaToValue` expresses optional (SOpt) fields by omitting
/// them from `required`. Bridge the dialects: walk every object schema,
/// wrap originally-optional properties in `{"anyOf": [orig, {"type":"null"}]}`,
/// and list every property in `required`. The result is a schema OpenAI
/// ENFORCES (an upgrade over the old hand-rolled strict:false steering).
fn strictify(schema: &mut serde_json::Value) {
    let Some(obj) = schema.as_object_mut() else {
        return;
    };
    match obj.get("type").and_then(|t| t.as_str()).unwrap_or("") {
        "object" => {
            let required: std::collections::HashSet<String> = obj
                .get("required")
                .and_then(|r| r.as_array())
                .map(|a| {
                    a.iter()
                        .filter_map(|v| v.as_str().map(String::from))
                        .collect()
                })
                .unwrap_or_default();
            let mut all_keys: Vec<serde_json::Value> = Vec::new();
            if let Some(props) = obj.get_mut("properties").and_then(|p| p.as_object_mut()) {
                for (k, v) in props.iter_mut() {
                    strictify(v);
                    if !required.contains(k.as_str()) {
                        let orig = v.take();
                        *v = serde_json::json!({"anyOf": [orig, {"type": "null"}]});
                    }
                    all_keys.push(serde_json::Value::String(k.clone()));
                }
            }
            if obj.contains_key("properties") {
                obj.insert("required".into(), serde_json::Value::Array(all_keys));
            }
        }
        "array" => {
            if let Some(items) = obj.get_mut("items") {
                strictify(items);
            }
        }
        _ => {}
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
    ) -> Result<tidepool_effect::Response, EffectError> {
        // check_rate_limit stays a hard `?` for ALL arms (including the try*
        // ones): budget exhaustion is a control limit that aborts the eval, it
        // is NOT an external probe failure to be isolated into a `Left`.
        self.check_rate_limit()?;
        match req {
            LlmReq::Structured(prompt, schema_val) => {
                let schema_json = tidepool_runtime::value_to_json(&schema_val, cx.table(), 0);
                cx.respond(self.structured_core(prompt, schema_json)?)
            }
            // Isolating: an API/network error or refusal becomes Left.
            LlmReq::TryStructured(prompt, schema_val) => {
                let schema_json = tidepool_runtime::value_to_json(&schema_val, cx.table(), 0);
                cx.respond_caught(self.structured_core(prompt, schema_json))
            }
        }
    }
}

/// Load API keys from a secrets directory into the environment. Each file is
/// named for the env var it fills (e.g. `.tidepool/secrets/OPENAI_API_KEY`)
/// and contains the bare key — drop a key in, restart the server, done.
/// Only names ending in `_API_KEY` are honored (this is a key dropbox, not a
/// general env injector), and a variable already set in the environment
/// wins. Key VALUES are never logged.
fn load_secrets_from(dir: &std::path::Path) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return; // no secrets dir — nothing to do
    };
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        let valid_name = name.ends_with("_API_KEY")
            && name
                .chars()
                .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_');
        if !valid_name {
            tracing::warn!(
                "{}/{}: ignored (filename must be an env-var name ending in _API_KEY)",
                dir.display(),
                name
            );
            continue;
        }
        if std::env::var_os(&name).is_some_and(|v| !v.is_empty()) {
            tracing::info!("{name} already set in environment; secrets file ignored");
            continue;
        }
        match std::fs::read_to_string(entry.path()) {
            Ok(contents) => {
                let key = contents.trim();
                if key.is_empty() {
                    tracing::warn!("{}/{}: empty; ignored", dir.display(), name);
                } else {
                    std::env::set_var(&name, key);
                    tracing::info!("loaded {name} from {}", dir.display());
                }
            }
            Err(e) => tracing::warn!("{}/{}: unreadable: {e}", dir.display(), name),
        }
    }
}

fn load_secrets() {
    // Project-local first (walk up from CWD for `.tidepool/secrets`), then
    // user-global (`~/.config/tidepool/secrets`, legacy `~/.tidepool/secrets`).
    // `load_secrets_from` lets an already-set var win, so the FIRST source to
    // provide a key takes precedence — project overrides global.
    if let Ok(cwd) = std::env::current_dir() {
        if let Some(root) = tidepool_runtime::paths::find_project_root(&cwd) {
            load_secrets_from(&root.join(".tidepool").join("secrets"));
        }
    }
    for dir in tidepool_runtime::paths::global_secrets_dirs() {
        load_secrets_from(&dir);
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
    ) -> Result<tidepool_effect::Response, EffectError> {
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
// Bundled Haskell stdlib — embedded at build time (build.rs walks the whole
// haskell/lib/Tidepool tree), materialized to a content-addressed cache dir.
// ---------------------------------------------------------------------------

// `EMBEDDED_STDLIB: &[(&str, &str)]` — (Tidepool/<rel>, contents) for every
// `.hs` module in the tree (Internal/ + Prelude_cbor excluded). @generated.
include!(concat!(env!("OUT_DIR"), "/embedded_stdlib.rs"));

/// Deterministic hash of the embedded stdlib content. Stable across runs
/// (`DefaultHasher` has fixed keys) so a given binary always maps to the same
/// cache dir; a changed binary maps to a fresh one.
fn stdlib_content_hash() -> String {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    for (rel, content) in EMBEDDED_STDLIB {
        rel.hash(&mut h);
        content.hash(&mut h);
    }
    format!("{:016x}", h.finish())
}

/// Resolve the directory holding the Tidepool stdlib (an include root for GHC).
/// Precedence: `TIDEPOOL_PRELUDE_DIR` → in-repo `haskell/lib` → materialized
/// bundle in the content-addressed cache dir. The bundle is the COMPLETE tree
/// and is keyed on content, so it can't go stale across binary versions (the
/// old `.version` stamp froze it) and can't drift from a hand-maintained subset.
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

    // Installed mode: materialize the bundled stdlib to a content-addressed dir.
    let hash = stdlib_content_hash();
    let base = tidepool_runtime::paths::stdlib_dir(&hash);
    // Sentinel marks a COMPLETE write — guards against serving a half-written dir
    // (e.g. a crash mid-materialization) and against the macOS cache reaper.
    let sentinel = base.join(".complete");
    if !sentinel.exists() {
        for (rel, content) in EMBEDDED_STDLIB {
            let full = base.join(rel);
            if let Some(parent) = full.parent() {
                std::fs::create_dir_all(parent)?;
            }
            std::fs::write(&full, content)?;
        }
        std::fs::write(&sentinel, hash.as_bytes())?;
    }
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

    /// Enable debug effects (Meta introspection)
    #[arg(long)]
    debug: bool,

    /// Advertise the `help` tool — reference docs (the same content as the
    /// `tidepool://…` MCP resources) via a plain tool call. Enable for clients
    /// that don't support MCP resources; resource-capable clients don't need it.
    #[arg(long)]
    help_tool: bool,

    /// LLM model for the Llm effect. genai routes the provider from the
    /// name: gpt-4o-mini → OpenAI, claude-haiku-4-5 → Anthropic, gemini-*
    /// → Gemini, unknown names → Ollama (e.g. qwen2.5:7b); `ns::model`
    /// forces a namespace. API keys come from the standard env vars
    /// (OPENAI_API_KEY, ...) or from `.tidepool/secrets/<ENV_VAR_NAME>`
    /// files.
    /// (Unset → falls back to `config.toml` `llm_model`, then the built-in default.)
    #[arg(long, env = "TIDEPOOL_LLM_MODEL")]
    llm: Option<String>,
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
        tracing::debug!("PANIC — see {}", path.display());
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

    // Initialize the `log`-crate diagnostic logging for the JIT subsystems
    // (tidepool::calls / scope / heap / effects / fp). Routed to stderr via
    // env_logger; honors RUST_LOG plus the legacy TIDEPOOL_TRACE* env vars.
    // Independent of the tracing subscriber above (which owns the `tracing::`
    // macros at the MCP layer); env_logger owns the `log::` global logger.
    tidepool_codegen::debug::init_logging();

    // Fill missing *_API_KEY env vars from .tidepool/secrets/ (drop a key
    // file in, restart, done). Must run before any handler reads the env.
    load_secrets();

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
            tracing::debug!(
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
    let project_root = tidepool_runtime::paths::find_project_root(&cwd);

    // Layered config: defaults < global config.toml < project config.toml < env.
    let cfg = Config::load(project_root.as_deref());
    // Model precedence: --llm / TIDEPOOL_LLM_MODEL (args.llm) > config > default.
    let model = args
        .llm
        .clone()
        .or(cfg.llm_model)
        .unwrap_or_else(|| DEFAULT_OPENAI_MODEL.to_string());
    // Bridge the configured default eval timeout into the env knob the server
    // reads, unless it's already set explicitly (env stays authoritative).
    if std::env::var_os("TIDEPOOL_EVAL_TIMEOUT_SECS").is_none() {
        if let Some(t) = cfg.eval_timeout_secs {
            std::env::set_var("TIDEPOOL_EVAL_TIMEOUT_SECS", t.to_string());
        }
    }

    // KV persists in the project's `.tidepool/` if we're inside one (walk up from
    // CWD), so it's found from any subdir; otherwise a global store under the
    // cache dir, so state survives even when launched outside any project.
    let kv_path = match &project_root {
        Some(root) => root.join(".tidepool").join("kv.json"),
        None => tidepool_runtime::paths::cache_dir().join("kv.json"),
    };
    if args.debug {
        // Build Meta's effect_names/helper_sigs from full decls (standard + meta)
        let mut decls = tidepool_mcp::standard_decls();
        decls.insert(decls.len() - 2, tidepool_mcp::meta_decl()); // before Llm, Ask
        let effect_names: Vec<String> = decls.iter().map(|d| d.type_name.to_string()).collect();
        let mut helper_sigs: Vec<String> = Vec::new();
        helper_sigs.push("putStrLn :: Text -> M ()".into());
        helper_sigs.push("showI :: Int -> Text".into());
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
            LlmHandler::new(model.clone())
        ];
        let server = TidepoolMcpServer::new(handlers)
            .with_prelude(prelude_dir)
            .with_help_tool(args.help_tool);
        if let Some(addr) = http_addr {
            server.serve_http(addr).await
        } else {
            server.serve_stdio().await
        }
    } else {
        let handlers = frunk::hlist![
            ConsoleHandler,
            KvHandler::new(kv_path),
            FsHandler::new(cwd.clone()),
            SgHandler::new(cwd.clone()),
            HttpHandler,
            ExecHandler::new(cwd.clone()),
            LlmHandler::new(model.clone())
        ];
        let server = TidepoolMcpServer::new(handlers)
            .with_prelude(prelude_dir)
            .with_help_tool(args.help_tool);
        if let Some(addr) = http_addr {
            server.serve_http(addr).await
        } else {
            server.serve_stdio().await
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Unwrap a handler Response for inspection: Complete passes through;
    /// Stream drains into the equivalent cons-list Value (iteratively).
    fn response_value(
        r: tidepool_effect::Response,
        table: &tidepool_repr::DataConTable,
    ) -> tidepool_eval::value::Value {
        use tidepool_eval::value::Value;
        match r {
            tidepool_effect::Response::Complete(v) => v,
            tidepool_effect::Response::Stream(s) => {
                let (mut source, cons_id, nil_id) = s.into_parts();
                let mut items = Vec::new();
                while let Some(i) = source.next_value(table) {
                    items.push(i.expect("stream element conversion"));
                }
                let mut acc = Value::Con(nil_id, vec![]);
                for i in items.into_iter().rev() {
                    acc = Value::Con(cons_id, vec![i, acc]);
                }
                acc
            }
        }
    }

    #[test]
    fn test_pattern_mentions() {
        assert!(!pattern_mentions("**/*.rs", "target"));
        assert!(pattern_mentions("target/**/*.rs", "target"));
        assert!(pattern_mentions("foo/target/bar", "target"));
        assert!(!pattern_mentions("retarget/foo", "target"));
    }

    #[test]
    fn test_component_filter_hidden_dirs() {
        use std::path::Path;
        // Hidden dirs skipped by default (worktrees, VCS stores, caches).
        assert!(!component_filter(
            "**/*.rs",
            Path::new(".exo/w1/src/lib.rs")
        ));
        assert!(!component_filter("**/*.rs", Path::new(".jj/store/x.rs")));
        assert!(!component_filter("**/*.rs", Path::new("a/.cache/x.rs")));
        // Explicit mention re-enables traversal.
        assert!(component_filter(
            ".tidepool/lib/*.hs",
            Path::new(".tidepool/lib/Std.hs")
        ));
        assert!(component_filter(
            ".exo/**/*.rs",
            Path::new(".exo/w1/src/lib.rs")
        ));
        // Default ignore dirs still excluded; mention escapes.
        assert!(!component_filter("**/*.rs", Path::new("target/debug/x.rs")));
        assert!(component_filter(
            "target/**/*.rs",
            Path::new("target/debug/x.rs")
        ));
        // Plain paths pass.
        assert!(component_filter(
            "**/*.rs",
            Path::new("tidepool-repr/src/lib.rs")
        ));
        // Hidden FILE (not just dir) also skipped unless mentioned.
        assert!(!component_filter("**/*", Path::new("src/.hidden")));
        assert!(component_filter(".gitignore", Path::new(".gitignore")));
    }

    #[test]
    fn test_glob_trailing_doublestar_finds_files() {
        use tempfile::tempdir;
        let dir = tempdir().unwrap();
        let root = dir.path().to_path_buf();
        std::fs::create_dir_all(root.join("a/b")).unwrap();
        std::fs::write(root.join("a/top.txt"), "x").unwrap();
        std::fs::write(root.join("a/b/deep.txt"), "y").unwrap();

        let handler = FsHandler::new(root.clone());
        // Bare trailing /**: the raw glob crate would return dirs only.
        let paths = handler.expand_glob("a/**").unwrap();
        let names: Vec<String> = paths
            .iter()
            .filter_map(|p| p.strip_prefix(&root).ok())
            .map(|p| p.to_string_lossy().to_string())
            .collect();
        assert!(names.iter().any(|n| n.ends_with("top.txt")), "{names:?}");
        assert!(names.iter().any(|n| n.ends_with("deep.txt")), "{names:?}");
    }

    #[test]
    fn test_sg_bare_signature_pattern_rejected() {
        use tempfile::tempdir;
        let dir = tempdir().unwrap();
        let root = dir.path().to_path_buf();
        std::fs::write(root.join("t.rs"), "pub fn run_find(x: i64) -> i64 { x }\n").unwrap();

        let mut handler = SgHandler::new(root);
        let table = full_effect_test_table();
        let captured = CapturedOutput::new();
        let cx = EffectContext::with_user(&table, &captured);

        // The classic footgun: a fn signature without a body. Previously
        // "succeeded" with zero matches; now a guided error.
        let req = SgReq::Find(
            Lang::Rust,
            "pub fn $NAME($$ARGS)".into(),
            vec!["t.rs".into()],
        );
        let err = handler
            .handle(req, &cx)
            .expect_err("bare signature must be rejected");
        let msg = format!("{err}");
        assert!(msg.contains("SIGNATURE"), "{msg}");
        assert!(msg.contains("rsFn"), "{msg}");

        // With a body the same intent WORKS.
        let req = SgReq::Find(
            Lang::Rust,
            "pub fn $NAME($$ARGS) -> $RET { $$$BODY }".into(),
            vec!["t.rs".into()],
        );
        let res = response_value(handler.handle(req, &cx).unwrap(), &table);
        let n = res.node_count();
        assert!(n > 1, "expected a match, got {res:?}");
    }

    #[test]
    fn test_sg_plan_apply() {
        use tempfile::tempdir;
        let dir = tempdir().unwrap();
        let root = dir.path().to_path_buf();
        let f = root.join("t.rs");
        std::fs::write(&f, "fn main() { foo(1); foo(2); }\n").unwrap();

        let mut handler = SgHandler::new(root.clone());
        let table = full_effect_test_table();
        let captured = CapturedOutput::new();
        let cx = EffectContext::with_user(&table, &captured);

        // Plan: replacements computed, NOTHING written.
        let req = SgReq::Plan(
            Lang::Rust,
            "foo($A)".into(),
            "bar($A)".into(),
            vec!["t.rs".into()],
        );
        let res = response_value(handler.handle(req, &cx).unwrap(), &table);
        let _ = res; // plan returns Matches with replacements; key check below:
        assert!(
            std::fs::read_to_string(&f).unwrap().contains("foo(1)"),
            "plan must not write"
        );

        // Apply: both call sites rewritten.
        let req = SgReq::Apply(
            Lang::Rust,
            "foo($A)".into(),
            "bar($A)".into(),
            vec!["t.rs".into()],
        );
        let res = response_value(handler.handle(req, &cx).unwrap(), &table);
        // Int responds as the boxed worker repr: Con(I#, [LitInt n]).
        let n = match &res {
            tidepool_eval::value::Value::Con(_, fields) => match fields.as_slice() {
                [tidepool_eval::value::Value::Lit(tidepool_repr::Literal::LitInt(n))] => *n,
                other => panic!("expected boxed Int, got {:?}", other),
            },
            tidepool_eval::value::Value::Lit(tidepool_repr::Literal::LitInt(n)) => *n,
            other => panic!("expected Int count, got {:?}", other),
        };
        assert_eq!(n, 2);
        let after = std::fs::read_to_string(&f).unwrap();
        assert!(after.contains("bar(1)") && after.contains("bar(2)"));
    }

    #[test]
    fn test_grep_handler() {
        use tempfile::tempdir;
        let dir = tempdir().unwrap();
        let root = dir.path().to_path_buf();
        let file_path = root.join("test.txt");
        std::fs::write(&file_path, "hello world\nrust is great\nhello rust").unwrap();

        // Binary file to skip
        let bin_path = root.join("test.bin");
        std::fs::write(&bin_path, vec![0, 1, 2, 3]).unwrap();

        // Ignored dir
        let target_dir = root.join("target");
        std::fs::create_dir(&target_dir).unwrap();
        std::fs::write(target_dir.join("ignored.txt"), "hello").unwrap();

        let mut handler = FsHandler::new(root.clone());
        let table = full_effect_test_table();
        let captured = CapturedOutput::new();
        let cx = EffectContext::with_user(&table, &captured);

        // Test normal grep
        let req = FsReq::Grep("hello".to_string(), "**/*.txt".to_string());
        let res = response_value(handler.handle(req, &cx).unwrap(), &table);
        use tidepool_bridge::FromCore;
        let results: Vec<(String, i64, String)> = FromCore::from_value(&res, &table).unwrap();
        assert_eq!(results.len(), 2);
        assert_eq!(
            results[0],
            ("test.txt".to_string(), 1, "hello world".to_string())
        );
        assert_eq!(
            results[1],
            ("test.txt".to_string(), 3, "hello rust".to_string())
        );

        // Test skip binary and ignored
        let req = FsReq::Grep("hello".to_string(), "**/*".to_string());
        let res = response_value(handler.handle(req, &cx).unwrap(), &table);
        let results: Vec<(String, i64, String)> = FromCore::from_value(&res, &table).unwrap();
        // Should only find test.txt matches, skipping target/ignored.txt and test.bin
        assert_eq!(results.len(), 2);
    }

    #[test]
    fn test_grep_truncation() {
        use tempfile::tempdir;
        let dir = tempdir().unwrap();
        let root = dir.path().to_path_buf();
        let file_path = root.join("large.txt");
        let mut content = String::new();
        for _ in 0..2005 {
            content.push_str("match\n");
        }
        std::fs::write(&file_path, content).unwrap();

        let mut handler = FsHandler::new(root.clone());
        let table = full_effect_test_table();
        let captured = CapturedOutput::new();
        let cx = EffectContext::with_user(&table, &captured);

        let req = FsReq::Grep("match".to_string(), "large.txt".to_string());
        let res = response_value(handler.handle(req, &cx).unwrap(), &table);
        use tidepool_bridge::FromCore;
        let results: Vec<(String, i64, String)> = FromCore::from_value(&res, &table).unwrap();

        assert_eq!(results.len(), 2001); // 2000 matches + 1 sentinel
        assert_eq!(results[2000].0, "...");
        assert_eq!(results[2000].1, 0);
        assert_eq!(results[2000].2, "truncated: 5 more matches");
    }

    #[test]
    fn test_glob_ignore_filter() {
        let pattern = "**/*.rs";
        let ignored_but_mentioned: Vec<&str> = DEFAULT_IGNORE_DIRS
            .iter()
            .filter(|&&dir| pattern_mentions(pattern, dir))
            .copied()
            .collect();

        let filter = |path_str: &str| {
            let rel_path = std::path::Path::new(path_str);
            for component in rel_path.components() {
                if let std::path::Component::Normal(name) = component {
                    let name_str = name.to_string_lossy();
                    if DEFAULT_IGNORE_DIRS.contains(&name_str.as_ref())
                        && !ignored_but_mentioned.contains(&name_str.as_ref())
                    {
                        return false;
                    }
                }
            }
            true
        };

        assert!(!filter("target/debug/foo.rs"));
        assert!(!filter(".git/config"));
        assert!(filter("src/lib.rs"));
        assert!(filter("retarget/foo.rs"));
        assert!(filter("src/target_file.rs"));

        let pattern2 = "target/**/*.rs";
        let ignored_but_mentioned2: Vec<&str> = DEFAULT_IGNORE_DIRS
            .iter()
            .filter(|&&dir| pattern_mentions(pattern2, dir))
            .copied()
            .collect();

        let filter2 = |path_str: &str| {
            let rel_path = std::path::Path::new(path_str);
            for component in rel_path.components() {
                if let std::path::Component::Normal(name) = component {
                    let name_str = name.to_string_lossy();
                    if DEFAULT_IGNORE_DIRS.contains(&name_str.as_ref())
                        && !ignored_but_mentioned2.contains(&name_str.as_ref())
                    {
                        return false;
                    }
                }
            }
            true
        };

        assert!(filter2("target/debug/foo.rs"));
    }

    use tidepool_bridge::{FromCore, ToCore};
    use tidepool_effect::dispatch::DispatchEffect;
    use tidepool_repr::{DataCon, DataConId, DataConTable};

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
        // Tests pass statement sequences; the expression-first contract
        // requires an explicit do-block for sequencing.
        let code_str = tidepool_mcp::wrap_do(&code.join("\n"));
        tidepool_mcp::template_haskell(&preamble, &stack, &code_str, "", "", None, None)
    }

    /// Compile and run a Haskell snippet through the JIT with the full handler stack.
    /// Returns the result as serde_json::Value, or panics on error.
    fn jit_eval(code: &[&str]) -> serde_json::Value {
        let source = jit_test_source(code);
        let include = prelude_include();
        let effects_dir =
            tidepool_mcp::ensure_effects_module(&tidepool_mcp::standard_decls()).unwrap();
        let include_paths: Vec<&std::path::Path> = vec![include.as_path(), effects_dir.as_path()];
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
            LlmHandler::new("ollama:llama3.2".to_string())
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

    /// Build a DataConTable with standard types + all effect constructors + response types.
    /// Auto-generated from `standard_decls()` so it never goes stale.
    fn full_effect_test_table() -> DataConTable {
        let mut t = tidepool_testing::gen::datacon_table::standard_datacon_table();
        let mut decls = tidepool_mcp::standard_decls();
        // Include Meta for unit tests even though it's not in the default stack
        decls.push(tidepool_mcp::meta_decl());
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

    // === Request FromCore tests ===

    // === Response structure tests ===

    // === Full dispatch round-trip tests ===

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

    const EFFECTS_WITH_ROUNDTRIP_TESTS: &[&str] =
        &["Console", "KV", "Fs", "SG", "Http", "Exec", "Llm", "Ask"];

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
        // HList order from main(): Console(0), KV(1), Fs(2), SG(3), Http(4), Exec(5), Llm(6)
        // Ask(7) is handled by MCP server, not in main HList
        let expected = ["Console", "KV", "Fs", "SG", "Http", "Exec", "Llm"];
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
        let result = response_value(handlers.dispatch(0, &request, &cx).unwrap(), &table);
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
        let result = response_value(handlers.dispatch(0, &request, &cx).unwrap(), &table);
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
        let result = response_value(handlers.dispatch(0, &request, &cx).unwrap(), &table);
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
        let result = response_value(handlers.dispatch(0, &request, &cx).unwrap(), &table);
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
        let result = response_value(handlers.dispatch(0, &request, &cx).unwrap(), &table);
        // Returns [Text] — list of strings (each string is a cons-list of chars)
        assert_is_cons_list(&result, &table);
    }

    // === Ask construction test (Ask is handled by MCP AskDispatcher, no FromCore type) ===

    #[test]
    fn test_ask_constructor_in_table() {
        let table = full_effect_test_table();
        // Bare `Ask` was reaped with the structured-Ask collapse; `ask` suspends
        // via AskWith (prompt + schema-carrying meta Value).
        let con_id = table.get_by_name("AskWith").unwrap();
        let dc = table.get(con_id).unwrap();
        // AskWith :: Text -> Value -> Ask Value  →  arity 2
        assert_eq!(dc.rep_arity, 2, "AskWith should have arity 2");
        // Verify we can construct a well-formed AskWith request Value
        let prompt = "What is your name?".to_string().to_value(&table).unwrap();
        let meta = "{}".to_string().to_value(&table).unwrap();
        let val = Value::Con(con_id, vec![prompt, meta]);
        match &val {
            Value::Con(id, fields) => {
                assert_eq!(table.name_of(*id).unwrap(), "AskWith");
                assert_eq!(fields.len(), 2);
            }
            _ => panic!("Expected Con"),
        }
    }

    // === JIT-level roundtrip tests ===
    // These compile Haskell through GHC + Cranelift JIT with the full handler stack.
    // They catch case trap (SIGILL) bugs that tree-walking eval misses.

    #[tokio::test]
    async fn test_jit_console_roundtrip() {
        let result = jit_eval(&["putStrLn \"hello from JIT\"", "pure (toJSON True)"]);
        assert_eq!(result, serde_json::json!(true));
    }

    #[tokio::test]
    async fn test_jit_kv_roundtrip() {
        let result = jit_eval(&[
            "kvSet \"jit_test\" (toJSON (42 :: Int))",
            "v <- kvGet \"jit_test\"",
            "pure (toJSON v)",
        ]);
        assert_eq!(result, serde_json::json!(42));
    }

    #[tokio::test]
    async fn test_jit_fs_exists_roundtrip() {
        let result = jit_eval(&["b <- doesFileExist \"Cargo.toml\"", "pure (toJSON b)"]);
        assert_eq!(result, serde_json::json!(true));
    }

    #[tokio::test]
    async fn test_jit_fs_listdir_roundtrip() {
        let result = jit_eval(&[
            "entries <- listDirectory \".\"",
            "pure (toJSON (length entries > 0))",
        ]);
        assert_eq!(result, serde_json::json!(true));
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
        ) -> Result<tidepool_effect::Response, EffectError> {
            match req {
                LlmReq::Structured(_, _) => cx.respond(self.response.clone()),
                LlmReq::TryStructured(_, _) => {
                    cx.respond_caught(Ok::<serde_json::Value, EffectError>(self.response.clone()))
                }
            }
        }
    }

    fn jit_eval_with_mock_llm(
        code: &[&str],
        mock_response: serde_json::Value,
    ) -> serde_json::Value {
        let source = jit_test_source(code);
        let include = prelude_include();
        let effects_dir =
            tidepool_mcp::ensure_effects_module(&tidepool_mcp::standard_decls()).unwrap();
        let include_paths: Vec<&std::path::Path> = vec![include.as_path(), effects_dir.as_path()];
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
            MockLlmHandler {
                response: mock_response
            }
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
        let result = jit_eval_with_mock_llm(&["llm (SObj [(\"greeting\", SStr)]) \"test\""], mock);
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
            &["llm (SObj [(\"languages\", SArr (SObj [(\"name\", SStr), (\"year\", SNum)]))]) \"test\""],
            mock,
        );
        let langs = result["languages"]
            .as_array()
            .expect("languages should be array");
        assert_eq!(langs.len(), 3);
        assert_eq!(langs[0]["name"], "Haskell");
    }

    #[test]
    fn test_llm_structured_encode_roundtrip() {
        // Roundtrip: llm → extract field → return via JSON bridge
        let mock = serde_json::json!({"greeting": "hello"});
        let result = jit_eval_with_mock_llm(
            &[
                "r <- llm (SObj [(\"greeting\", SStr)]) \"test\"",
                "pure (object [\"result\" .= r, \"field\" .= (r ?. \"greeting\")])",
            ],
            mock,
        );
        assert_eq!(result["result"]["greeting"], "hello");
        assert_eq!(result["field"], "hello");
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
                "r <- llm (SObj [(\"languages\", SArr (SObj [(\"name\", SStr), (\"year\", SNum)]))]) \"test\"",
                "pure r",
            ],
            mock,
        );
        let langs = result["languages"]
            .as_array()
            .expect("languages should be array");
        assert_eq!(langs.len(), 2);
    }

    #[test]
    fn test_llm_structured_empty_object() {
        let mock = serde_json::json!({});
        let result = jit_eval_with_mock_llm(&["llm (SObj []) \"test\""], mock);
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
            &["llm (SObj [(\"name\", SStr), (\"count\", SNum), (\"active\", SBool)]) \"test\""],
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
                "r <- llm (SObj [(\"name\", SStr), (\"count\", SNum), (\"active\", SBool)]) \"test\"",
                "pure r",
            ],
            mock,
        );
        assert_eq!(result["name"], "test");
        assert_eq!(result["count"], 42);
        assert_eq!(result["active"], true);
    }

    #[tokio::test]
    async fn test_llm_effective_model() {
        // No provider alias -> model passes through untouched
        std::env::remove_var("TIDEPOOL_LLM_PROVIDER");
        assert_eq!(
            LlmHandler::effective_model("ollama:llama3.2".into()),
            "ollama:llama3.2"
        );
        assert_eq!(LlmHandler::effective_model("gpt-4o".into()), "gpt-4o");

        // Deprecated TIDEPOOL_LLM_PROVIDER=openai alias coerces the model
        std::env::set_var("TIDEPOOL_LLM_PROVIDER", "openai");
        assert_eq!(
            LlmHandler::effective_model("openai:gpt-4o".into()),
            "gpt-4o"
        );
        assert_eq!(LlmHandler::effective_model("gpt-4o".into()), "gpt-4o");
        assert_eq!(
            LlmHandler::effective_model("ollama:llama3.2".into()),
            DEFAULT_OPENAI_MODEL
        );
        assert_eq!(
            LlmHandler::effective_model(String::new()),
            DEFAULT_OPENAI_MODEL
        );
        std::env::remove_var("TIDEPOOL_LLM_PROVIDER");
    }

    #[test]
    fn test_normalize_model() {
        // legacy single-colon prefix -> genai double-colon namespace
        assert_eq!(
            LlmHandler::normalize_model("ollama:llama3.2".into()),
            "ollama::llama3.2"
        );
        assert_eq!(
            LlmHandler::normalize_model("anthropic:claude-haiku-4-5".into()),
            "anthropic::claude-haiku-4-5"
        );
        // already-correct forms untouched
        assert_eq!(
            LlmHandler::normalize_model("ollama::llama3.2".into()),
            "ollama::llama3.2"
        );
        assert_eq!(
            LlmHandler::normalize_model("gpt-4o-mini".into()),
            "gpt-4o-mini"
        );
        // ollama's native model:tag syntax is NOT a provider prefix
        assert_eq!(
            LlmHandler::normalize_model("qwen2.5:7b".into()),
            "qwen2.5:7b"
        );
        assert_eq!(
            LlmHandler::normalize_model("tinyllama:latest".into()),
            "tinyllama:latest"
        );
    }

    #[tokio::test]
    async fn test_llm_call_budget_resets_per_clone() {
        let handler = LlmHandler::new("gpt-4o-mini".into());
        handler
            .call_count
            .store(LLM_MAX_CALLS, std::sync::atomic::Ordering::Relaxed);
        assert!(handler.check_rate_limit().is_err());
        // a clone (= a new eval) gets a fresh budget
        let fresh = handler.clone();
        assert!(fresh.check_rate_limit().is_ok());
        // and the original's exhaustion is untouched by the clone
        assert!(handler.check_rate_limit().is_err());
    }

    #[test]
    fn test_strictify_optional_fields() {
        // schemaToValue output for SObj [("a", SStr), ("b", SOpt SNum)]:
        // "b" omitted from required = optional.
        let mut schema = serde_json::json!({
            "type": "object",
            "properties": {
                "a": {"type": "string"},
                "b": {"type": "number"}
            },
            "required": ["a"]
        });
        strictify(&mut schema);
        // every property is now required...
        let req: Vec<&str> = schema["required"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_str().unwrap())
            .collect();
        assert!(req.contains(&"a") && req.contains(&"b"));
        // ...required field untouched, optional field null-unioned
        assert_eq!(
            schema["properties"]["a"],
            serde_json::json!({"type": "string"})
        );
        assert_eq!(
            schema["properties"]["b"],
            serde_json::json!({"anyOf": [{"type": "number"}, {"type": "null"}]})
        );
    }

    #[test]
    fn test_strictify_nested_and_arrays() {
        let mut schema = serde_json::json!({
            "type": "object",
            "properties": {
                "items": {
                    "type": "array",
                    "items": {
                        "type": "object",
                        "properties": {"opt": {"type": "string"}},
                        "required": []
                    }
                }
            },
            "required": ["items"]
        });
        strictify(&mut schema);
        // nested object inside array items also strictified
        let inner = &schema["properties"]["items"]["items"];
        assert_eq!(inner["required"], serde_json::json!(["opt"]));
        assert_eq!(
            inner["properties"]["opt"],
            serde_json::json!({"anyOf": [{"type": "string"}, {"type": "null"}]})
        );
    }

    #[test]
    fn test_strictify_all_required_unchanged() {
        // h_aug-style schema (everything required) passes through unchanged
        let mut schema = serde_json::json!({
            "type": "object",
            "properties": {"answer": {"type": "boolean"}},
            "required": ["answer"]
        });
        let before = schema.clone();
        strictify(&mut schema);
        assert_eq!(schema, before);
    }

    #[test]
    fn test_load_secrets_from() {
        let dir =
            std::env::temp_dir().join(format!("tidepool-secrets-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();

        // Valid key file, var unset -> loaded (trimmed)
        std::env::remove_var("TIDEPOOL_TEST_DUMMY_API_KEY");
        std::fs::write(dir.join("TIDEPOOL_TEST_DUMMY_API_KEY"), "sk-test-123\n").unwrap();
        // Invalid names / non-key files -> ignored
        std::fs::write(dir.join("notes.txt"), "not a key").unwrap();
        std::fs::write(dir.join("lower_api_key"), "nope").unwrap();
        // Env already set -> file does NOT override
        std::env::set_var("TIDEPOOL_TEST_PRESET_API_KEY", "from-env");
        std::fs::write(dir.join("TIDEPOOL_TEST_PRESET_API_KEY"), "from-file").unwrap();

        load_secrets_from(&dir);

        assert_eq!(
            std::env::var("TIDEPOOL_TEST_DUMMY_API_KEY").unwrap(),
            "sk-test-123"
        );
        assert_eq!(
            std::env::var("TIDEPOOL_TEST_PRESET_API_KEY").unwrap(),
            "from-env"
        );
        assert!(std::env::var("notes.txt").is_err());

        std::env::remove_var("TIDEPOOL_TEST_DUMMY_API_KEY");
        std::env::remove_var("TIDEPOOL_TEST_PRESET_API_KEY");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Live smoke against the real OpenAI API — runs only when
    /// OPENAI_API_KEY is set (e.g. after dropping a key into
    /// .tidepool/secrets and exporting it). Exercises the strictified
    /// JsonSpec path end to end:
    /// `OPENAI_API_KEY=$(cat .tidepool/secrets/OPENAI_API_KEY) cargo test -p tidepool live_smoke_openai -- --nocapture`
    #[tokio::test(flavor = "multi_thread")]
    async fn live_smoke_openai() {
        if std::env::var("OPENAI_API_KEY")
            .map(|k| k.trim().is_empty())
            .unwrap_or(true)
        {
            eprintln!("skipping live_smoke_openai: OPENAI_API_KEY not set");
            return;
        }
        let client = genai::Client::default();
        let model = "gpt-4o-mini";

        // chat
        let resp = client
            .exec_chat(
                model,
                genai::chat::ChatRequest::from_user("Reply with the single word: pong"),
                None,
            )
            .await
            .expect("chat");
        assert!(!resp.first_text().unwrap_or("").is_empty());

        // structured with an SOpt-shaped (optional-field) schema — the
        // exact case genai's hardcoded strict:true would reject unshimmed
        let mut schema = serde_json::json!({
            "type": "object",
            "properties": {
                "n": {"type": "number"},
                "note": {"type": "string"}
            },
            "required": ["n"]
        });
        strictify(&mut schema);
        let spec = genai::chat::JsonSpec::new("structured_output", schema);
        let opts = genai::chat::ChatOptions::default()
            .with_response_format(genai::chat::ChatResponseFormat::JsonSpec(spec));
        let resp = client
            .exec_chat(
                model,
                genai::chat::ChatRequest::from_user(
                    "Return JSON with field n set to 7. Omit or null the note field.\n\n\
                     Respond with ONLY valid JSON matching the provided schema.",
                ),
                Some(&opts),
            )
            .await
            .expect("structured");
        let parsed: serde_json::Value =
            serde_json::from_str(resp.first_text().unwrap_or("null")).unwrap();
        assert_eq!(parsed["n"], serde_json::json!(7));
    }
}
