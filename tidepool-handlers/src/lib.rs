//! Concrete effect handlers for the Tidepool eval server.
//!
//! Provides the 8 base handlers (Console, KV, Fs, SG, Http, Exec, Lsp, Llm),
//! the debug-only MetaHandler, and the [`build_base_stack`] / [`base_decls_with_ask`]
//! convenience functions for assembling a fully-wired eval server.
//!
//! ## Stack assembly
//!
//! ```no_run
//! # use std::path::PathBuf;
//! # use tidepool_handlers::{HandlerConfig, build_base_stack, base_decls_with_ask};
//! # use tidepool_mcp::TidepoolMcpServer;
//! // Must be called inside a tokio runtime (LlmHandler captures Handle::current()).
//! let cfg = HandlerConfig {
//!     cwd: PathBuf::from("."),
//!     kv_path: PathBuf::from(".tidepool/kv.json"),
//!     llm_model: "gpt-4o-mini".into(),
//! };
//! let stack = build_base_stack(&cfg);
//! let server = TidepoolMcpServer::new(stack);
//! ```

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use ast_grep_config::{DeserializeEnv, SerializableRule};
use ast_grep_core::{Language as _, Pattern};
use ast_grep_language::{LanguageExt, SupportLang};
use tidepool_bridge_derive::{FromCore, ToCore};
use tidepool_effect::dispatch::{EffectContext, EffectHandler};
use tidepool_effect::error::EffectError;
use tidepool_eval::value::Value;
use tidepool_mcp::{CapturedOutput, CollectEffectDecls, DescribeEffect, EffectDecl};

// ============================================================================
// Tag 0: Console
// ============================================================================

#[derive(FromCore)]
pub enum ConsoleReq {
    #[core(name = "Print")]
    Print(String),
}

#[derive(Clone)]
pub struct ConsoleHandler;

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

// ============================================================================
// Tag 1: KV Store
// ============================================================================

#[derive(FromCore)]
pub enum KvReq {
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
pub struct KvHandler {
    store: Arc<Mutex<HashMap<String, serde_json::Value>>>,
    path: PathBuf,
}

impl KvHandler {
    pub fn new(path: PathBuf) -> Self {
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

// ============================================================================
// Tag 2: File I/O (sandboxed to working directory)
// ============================================================================

#[derive(FromCore)]
pub enum FsReq {
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

pub const DEFAULT_IGNORE_DIRS: &[&str] = &["target", ".git", "node_modules", "dist-newstyle"];

pub fn pattern_mentions(pattern: &str, dir: &str) -> bool {
    pattern.split(['/', '\\']).any(|c| c == dir)
}

/// Ripgrep-style default exclusions: skip paths containing a default-ignored
/// directory (build artifacts) or any HIDDEN component (dot-prefixed — VCS
/// stores, tool worktrees, caches), unless the glob pattern explicitly names
/// that component (e.g. `.tidepool/lib/*.hs` still traverses `.tidepool`).
pub fn component_filter(pattern: &str, rel_path: &std::path::Path) -> bool {
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

/// Returns true if `p` contains glob metacharacters (`*`, `?`, `[`).
pub fn is_glob(p: &str) -> bool {
    p.contains('*') || p.contains('?') || p.contains('[')
}

/// Build the `grepGlob` regex-compile error, always surfacing the underlying
/// regex error and appending the hint that applies to the two common footguns:
/// (a) arg-order — a path glob passed as the (regex, glob) first arg; (b)
/// under-escaping — regex metachars need double-escaping (JSON x Haskell).
/// Loss-less: the real error is always shown, so a heuristic misfire can't hide
/// it. Mirrors the `checked_pattern` diagnose-at-the-boundary precedent.
fn grep_regex_error(regex_str: &str, e: &regex::Error) -> EffectError {
    let mut msg = format!("invalid regex {:?}: {}", regex_str, e);
    // (a) Looks like a path glob in arg 1. Gate on path-shape so a regex
    // char-class like `[abc]` doesn't misfire; the real error shows regardless.
    if is_glob(regex_str)
        && (regex_str.starts_with('*')
            || regex_str.contains("*/")
            || regex_str.contains("*.")
            || regex_str.contains('/'))
    {
        msg.push_str(
            "\nhint: grepGlob is (regex, glob) — this looks like a path glob; \
             pass it as the SECOND argument, e.g. grepGlob \"fn \" \"**/*.rs\".",
        );
    } else {
        // (b) Most other compile failures here are under-escaped metachars.
        msg.push_str(
            "\nhint: regex backslashes are escaped twice on the way here \
             (JSON x Haskell) — write a literal dot as four backslashes then a dot.",
        );
    }
    EffectError::Handler(msg)
}

/// Expand a glob pattern relative to `root` with sandbox and component filtering.
///
/// Shared by [`FsHandler`] and [`SgHandler`] so both benefit from the same
/// `**`-normalisation, sandbox check, and hidden-dir filter.
pub fn expand_glob(root: &std::path::Path, pattern: &str) -> Result<Vec<PathBuf>, EffectError> {
    if pattern.contains("..") {
        return Ok(Vec::new());
    }
    if pattern.starts_with('/') || pattern.starts_with('\\') {
        return Err(EffectError::Handler(
            "absolute glob patterns not allowed".to_string(),
        ));
    }
    // rg-style root semantics (friction #20): a metachar-free pattern names a
    // concrete path, not a glob. If it's a directory, recurse into it
    // (`dir/**/*`) — a bare dir silently returned `[]` before. If it names
    // nothing, that's a LOUD error, not an empty result (a typo'd root looked
    // identical to a clean no-match). An existing file passes through as-is.
    let normalized;
    let pattern = if !is_glob(pattern) {
        let target = root.join(pattern);
        if target.is_dir() {
            normalized = if pattern.is_empty() || pattern == "." {
                "**/*".to_string()
            } else {
                format!("{}/**/*", pattern.trim_end_matches('/'))
            };
            normalized.as_str()
        } else if target.exists() {
            pattern
        } else {
            return Err(EffectError::Handler(format!(
                "no such file or directory: {pattern:?} (search root does not exist; \
                 a zero-match glob returns [] — a missing root is an error)"
            )));
        }
    } else if pattern == "**" || pattern.ends_with("/**") {
        // The glob crate's `**` matches DIRECTORIES only, so a bare trailing
        // `/**` silently returns dirs and no files — normalize to `/**/*`.
        normalized = format!("{}/*", pattern);
        normalized.as_str()
    } else {
        pattern
    };
    let full_pattern = root.join(pattern).to_string_lossy().to_string();
    let canonical_root = root
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
            let rel_path = p.strip_prefix(root).unwrap_or(p);
            component_filter(pattern, rel_path)
        })
        .collect();
    Ok(paths)
}

#[derive(Clone)]
pub struct FsHandler {
    root: PathBuf,
}

impl FsHandler {
    pub fn new(root: PathBuf) -> Self {
        Self { root }
    }

    pub fn expand_glob(&self, pattern: &str) -> Result<Vec<PathBuf>, EffectError> {
        expand_glob(&self.root, pattern)
    }

    pub fn resolve(&self, path: &str) -> Result<PathBuf, EffectError> {
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
            // Walk up to the deepest existing ancestor, canonicalize it, then
            // reconstruct. Handles paths like "a/b/c.txt" when "a/b/" doesn't
            // exist yet while still catching `..`-based escapes via the
            // canonicalize call on the existing prefix.
            let mut suffix: Vec<std::ffi::OsString> = Vec::new();
            let mut cur = resolved.as_path();
            while let Some(parent) = cur.parent() {
                if let Some(name) = cur.file_name() {
                    suffix.push(name.to_owned());
                }
                cur = parent;
                if cur.exists() {
                    break;
                }
            }
            let canonical_ancestor = cur
                .canonicalize()
                .map_err(|e| EffectError::Handler(e.to_string()))?;
            suffix
                .iter()
                .rev()
                .fold(canonical_ancestor, |p, c| p.join(c))
        };
        if !check_path.starts_with(&canonical_root) {
            return Err(EffectError::Handler(format!(
                "path escape: {} is outside sandbox",
                path
            )));
        }
        Ok(check_path)
    }

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
            FsReq::TryRead(path) => cx.respond_caught(self.read_core(&path)),
            FsReq::Write(path, contents) => {
                let resolved = self.resolve(&path)?;
                if let Some(parent) = resolved.parent() {
                    std::fs::create_dir_all(parent)
                        .map_err(|e| EffectError::Handler(e.to_string()))?;
                }
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
                // Files only: `glob` feeds `readGlob`/`grepGlob`, which read
                // file contents — a matched DIRECTORY (from a `dir/**/*`
                // recursion or a `*/`-shaped pattern) would make `readGlob`
                // die "Is a directory" (friction #20). Use `listDir` for dirs.
                let paths = self.expand_glob(&pattern)?;
                let rel_paths: Vec<String> = paths
                    .into_iter()
                    .filter(|p| p.is_file())
                    .filter_map(|p| {
                        p.strip_prefix(&self.root)
                            .ok()
                            .map(|r| r.to_string_lossy().to_string())
                    })
                    .collect();
                cx.respond_list(rel_paths)
            }
            FsReq::Grep(regex_str, pattern) => {
                let re =
                    regex::Regex::new(&regex_str).map_err(|e| grep_regex_error(&regex_str, &e))?;
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

// ============================================================================
// Tag 3: Structural Grep (ast-grep)
// ============================================================================

#[derive(Clone, Copy, FromCore)]
pub enum Lang {
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
    pub fn to_support_lang(self) -> Result<SupportLang, EffectError> {
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
pub enum SgReq {
    #[core(name = "SgFind")]
    Find(Lang, String, Vec<String>),
    #[core(name = "SgRuleFind")]
    RuleFind(Lang, Value, Vec<String>),
    #[core(name = "SgPlan")]
    Plan(Lang, String, String, Vec<String>),
    #[core(name = "SgApply")]
    Apply(Lang, String, String, Vec<String>),
}

const MATCH_TEXT_LIMIT: usize = 500;

/// Clamp match text to [`MATCH_TEXT_LIMIT`] chars so a single `[Match]` list
/// doesn't overflow the eval context with whole-function bodies.
fn truncate_match_text(text: String) -> String {
    if text.len() <= MATCH_TEXT_LIMIT {
        return text;
    }
    let mut end = MATCH_TEXT_LIMIT;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}…", &text[..end])
}

/// Rust-side Match value returned to Haskell.
/// Field order must match the Haskell data constructor:
///   Match { mText, mFile, mLine, mVars, mReplacement }
#[derive(ToCore)]
#[core(name = "Match")]
pub struct SgMatch {
    pub text: String,
    pub file: String,
    pub line: i64,
    pub vars: Vec<(String, String)>,
    pub replacement: String,
}

#[derive(Clone)]
pub struct SgHandler {
    root: PathBuf,
}

impl SgHandler {
    pub fn new(root: PathBuf) -> Self {
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
                if is_glob(p) {
                    // Glob path: expand then filter to language-matching files.
                    for expanded in expand_glob(&self.root, p)? {
                        if expanded.is_file() {
                            if SupportLang::from_path(&expanded) == Some(lang) {
                                files.push(expanded);
                            }
                        } else if expanded.is_dir() {
                            self.walk_dir(&expanded, lang, &mut files)?;
                        }
                    }
                } else {
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
    pub fn checked_pattern(pattern: &str, sl: SupportLang) -> Result<Pattern, EffectError> {
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

    pub fn run_find(
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
                let text = truncate_match_text(m.text().to_string());
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
                let text = truncate_match_text(m.text().to_string());
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

#[derive(FromCore, ToCore, Clone)]
#[core(name = "Position")]
pub struct LspPosition {
    pub line: i64,
    pub character: i64,
}

#[derive(FromCore, ToCore, Clone)]
#[core(name = "LspNode")]
pub struct LspNode {
    pub name: String,
    pub container: String,
    pub kind: String,
    pub file: String,
    pub pos: LspPosition,
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

#[derive(ToCore)]
#[core(name = "Diag")]
pub struct LspDiag {
    pub file: String,
    pub line: i64,
    pub severity: String,
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

// ============================================================================
// Tag 4: Http
// ============================================================================

#[derive(FromCore)]
pub enum HttpReq {
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

pub fn parse_json_str(s: &str) -> Result<serde_json::Value, EffectError> {
    serde_json::from_str(s).map_err(|e| EffectError::Handler(format!("invalid JSON: {e}")))
}

#[derive(Clone)]
pub struct HttpHandler;

impl HttpHandler {
    pub fn validate_url(url_str: &str) -> Result<url::Url, EffectError> {
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
        serde_json::from_str(body).or_else(|_| Ok(serde_json::Value::String(body.to_string())))
    }

    pub fn get(&self, url_str: &str) -> Result<serde_json::Value, EffectError> {
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

    pub fn post(
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
            HttpReq::TryGet(url_str) => cx.respond_caught(self.get(&url_str)),
            HttpReq::TryPost(url_str, body_val) => {
                let json_body = tidepool_runtime::value_to_json(&body_val, cx.table(), 0);
                cx.respond_caught(self.post(&url_str, &json_body))
            }
            HttpReq::ParseJson(s) => cx.respond(parse_json_str(&s)?),
            HttpReq::TryParseJson(s) => cx.respond_caught(parse_json_str(&s)),
        }
    }
}

// ============================================================================
// Tag 5: Exec (shell commands)
// ============================================================================

#[derive(FromCore)]
pub enum ExecReq {
    #[core(name = "Run")]
    Run(String),
    #[core(name = "RunIn")]
    RunIn(String, String),
    #[core(name = "TryRun")]
    TryRun(String),
    #[core(name = "TryRunIn")]
    TryRunIn(String, String),
    #[core(name = "RunArgv")]
    RunArgv(Vec<String>),
}

#[derive(Clone)]
pub struct ExecHandler {
    root: PathBuf,
}

impl ExecHandler {
    pub fn new(root: PathBuf) -> Self {
        Self { root }
    }

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
            ExecReq::TryRun(cmd) => cx.respond_caught(self.run_command(&cmd, &self.root)),
            ExecReq::TryRunIn(dir, cmd) => cx.respond_caught(
                self.resolve_dir(&dir)
                    .and_then(|target| self.run_command(&cmd, &target)),
            ),
            ExecReq::RunArgv(argv) => {
                if argv.is_empty() {
                    return Err(EffectError::Handler("runArgv: empty argv".to_string()));
                }
                let output = std::process::Command::new(&argv[0])
                    .args(&argv[1..])
                    .current_dir(&self.root)
                    .stdout(std::process::Stdio::piped())
                    .stderr(std::process::Stdio::piped())
                    .output()
                    .map_err(|e| EffectError::Handler(format!("runArgv exec failed: {}", e)))?;
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
                cx.respond((code, stdout, stderr))
            }
        }
    }
}

// ============================================================================
// Tag 7 (base stack position 7): Llm
// ============================================================================

#[derive(FromCore)]
pub enum LlmReq {
    #[core(name = "LlmStructured")]
    Structured(String, Value),
    #[core(name = "TryLlmStructured")]
    TryStructured(String, Value),
}

pub const DEFAULT_OPENAI_MODEL: &str = "gpt-4o-mini";

pub struct LlmHandler {
    client: genai::Client,
    model: String,
    rt: tokio::runtime::Handle,
    call_count: std::sync::Arc<std::sync::atomic::AtomicU32>,
}

/// Fresh call counter per clone — see comment in original source for rationale.
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

pub const LLM_MAX_CALLS: u32 = 200;

impl LlmHandler {
    pub fn effective_model(model: String) -> String {
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

    pub fn normalize_model(model: String) -> String {
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

    /// Must be called inside a tokio runtime (captures Handle::current()).
    pub fn new(model: String) -> Self {
        Self {
            client: genai::Client::default(),
            model: Self::normalize_model(Self::effective_model(model)),
            rt: tokio::runtime::Handle::current(),
            call_count: std::sync::Arc::new(std::sync::atomic::AtomicU32::new(0)),
        }
    }

    pub fn targets_openai(&self) -> bool {
        matches!(
            genai::adapter::AdapterKind::from_model(&self.model),
            Ok(genai::adapter::AdapterKind::OpenAI | genai::adapter::AdapterKind::OpenAIResp)
        )
    }

    pub fn check_rate_limit(&self) -> Result<(), EffectError> {
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

    pub fn structured_core(
        &self,
        prompt: String,
        mut schema_json: serde_json::Value,
    ) -> Result<serde_json::Value, EffectError> {
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

/// OpenAI strict structured outputs require EVERY property in `required`
/// (optionality = null-union). Walks every object schema and wraps originally-
/// optional properties in `{"anyOf": [orig, {"type":"null"}]}`.
pub fn strictify(schema: &mut serde_json::Value) {
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
        self.check_rate_limit()?;
        match req {
            LlmReq::Structured(prompt, schema_val) => {
                let schema_json = tidepool_runtime::value_to_json(&schema_val, cx.table(), 0);
                cx.respond(self.structured_core(prompt, schema_json)?)
            }
            LlmReq::TryStructured(prompt, schema_val) => {
                let schema_json = tidepool_runtime::value_to_json(&schema_val, cx.table(), 0);
                cx.respond_caught(self.structured_core(prompt, schema_json))
            }
        }
    }
}

// ============================================================================
// Meta handler (debug path only — --debug flag in the eval server)
// ============================================================================

#[derive(FromCore)]
pub enum MetaReq {
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
pub struct MetaHandler {
    effect_names: Vec<String>,
    helper_sigs: Vec<String>,
}

impl MetaHandler {
    pub fn new(effect_names: Vec<String>, helper_sigs: Vec<String>) -> Self {
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

// ============================================================================
// Tag 8: Git (read-only repository queries)
// ============================================================================

#[derive(FromCore)]
pub enum GitReq {
    #[core(name = "GitLog")]
    Log(i64),
    #[core(name = "GitStatus")]
    Status,
    #[core(name = "GitDiffStat")]
    DiffStat(String),
    #[core(name = "GitShow")]
    Show(String),
}

/// Haskell `Commit` record: sha / subject / author / date / files.
#[derive(ToCore, Clone)]
#[core(name = "Commit")]
pub struct GitCommit {
    pub sha: String,
    pub subject: String,
    pub author: String,
    pub date: String,
    pub files: Vec<String>,
}

/// Haskell `StatusEntry` record: path / state (2-char XY code).
#[derive(ToCore, Clone)]
#[core(name = "StatusEntry")]
pub struct GitStatusEntry {
    pub path: String,
    pub state: String,
}

/// Haskell `FileDelta` record: path / adds / dels / binary.
#[derive(ToCore, Clone)]
#[core(name = "FileDelta")]
pub struct GitFileDelta {
    pub path: String,
    pub adds: i64,
    pub dels: i64,
    pub binary: bool,
}

#[derive(Clone)]
pub struct GitHandler {
    root: PathBuf,
}

impl GitHandler {
    pub fn new(root: PathBuf) -> Self {
        Self { root }
    }

    /// Run a git command in the sandbox root; propagate non-zero exit as
    /// `EffectError::Handler` (clean eval failure, no panic).
    fn run_git(&self, args: &[&str]) -> Result<String, EffectError> {
        let output = std::process::Command::new("git")
            .args(args)
            .current_dir(&self.root)
            // Read-only ops; suppress optional index locks.
            .env("GIT_OPTIONAL_LOCKS", "0")
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .output()
            .map_err(|e| EffectError::Handler(format!("git exec failed: {}", e)))?;
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(EffectError::Handler(format!("git: {}", stderr.trim())));
        }
        Ok(String::from_utf8_lossy(&output.stdout).into_owned())
    }

    /// Parse `git log --format="%H%x00%s%x00%an%x00%cI" --name-only` output.
    ///
    /// Each commit block is separated by a blank line (`\n\n`).  The first
    /// line of each block is NUL-delimited metadata; subsequent non-empty
    /// lines are changed files.
    fn parse_log_output(output: &str) -> Vec<GitCommit> {
        // git log --format=... --name-only produces:
        //   sha\x00subject\x00author\x00date\n
        //   \n                           <- blank separator between header and files
        //   file1\n
        //   file2\n
        //   sha2\x00...                 <- next header follows files directly (no blank)
        //
        // Scan line by line: lines with 4+ NUL-separated fields are headers; blank
        // lines are separators (skip); all other non-empty lines are file paths.
        let mut commits = Vec::new();
        let mut current: Option<GitCommit> = None;

        for line in output.lines() {
            if line.is_empty() {
                continue;
            }
            let parts: Vec<&str> = line.splitn(4, '\x00').collect();
            if parts.len() >= 4 {
                // Header line: sha\x00subject\x00author\x00date
                if let Some(commit) = current.take() {
                    commits.push(commit);
                }
                current = Some(GitCommit {
                    sha: parts[0].to_string(),
                    subject: parts[1].to_string(),
                    author: parts[2].to_string(),
                    date: parts[3].to_string(),
                    files: Vec::new(),
                });
            } else if let Some(ref mut commit) = current {
                commit.files.push(line.to_string());
            }
        }
        if let Some(commit) = current {
            commits.push(commit);
        }
        commits
    }

    /// Parse one commit from log output; error if missing.
    fn parse_single_commit(output: &str, revspec: &str) -> Result<GitCommit, EffectError> {
        let commits = Self::parse_log_output(output);
        commits.into_iter().next().ok_or_else(|| {
            EffectError::Handler(format!("gitShow: no commit found for '{}'", revspec))
        })
    }

    /// Parse `git status --porcelain=v1` output.
    fn parse_status_output(output: &str) -> Vec<GitStatusEntry> {
        output
            .lines()
            .filter(|l| l.len() >= 3)
            .map(|line| {
                let state = line[..2].to_string();
                let rest = line[3..].trim();
                // For renames "XY old -> new", take the destination path.
                let path = if let Some(idx) = rest.find(" -> ") {
                    rest[idx + 4..].to_string()
                } else {
                    rest.to_string()
                };
                GitStatusEntry { path, state }
            })
            .collect()
    }

    /// Parse `git diff --numstat <rev>` output.
    fn parse_numstat_output(output: &str) -> Vec<GitFileDelta> {
        output
            .lines()
            .filter(|l| !l.is_empty())
            .filter_map(|line| {
                let parts: Vec<&str> = line.splitn(3, '\t').collect();
                if parts.len() < 3 {
                    return None;
                }
                let (adds_s, dels_s, path) = (parts[0], parts[1], parts[2].trim());
                // Binary files show "-" instead of counts.
                let binary = adds_s == "-" || dels_s == "-";
                let adds = if binary {
                    0
                } else {
                    adds_s.parse::<i64>().unwrap_or(0)
                };
                let dels = if binary {
                    0
                } else {
                    dels_s.parse::<i64>().unwrap_or(0)
                };
                Some(GitFileDelta {
                    path: path.to_string(),
                    adds,
                    dels,
                    binary,
                })
            })
            .collect()
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
    ) -> Result<tidepool_effect::Response, EffectError> {
        match req {
            GitReq::Log(n) => {
                let n_str = n.to_string();
                let output = self.run_git(&[
                    "log",
                    "-n",
                    &n_str,
                    "--format=%H%x00%s%x00%an%x00%cI",
                    "--name-only",
                ])?;
                cx.respond_list(Self::parse_log_output(&output))
            }
            GitReq::Status => {
                let output = self.run_git(&["status", "--porcelain=v1"])?;
                cx.respond_list(Self::parse_status_output(&output))
            }
            GitReq::DiffStat(rev) => {
                let output = self.run_git(&["diff", "--numstat", &rev])?;
                cx.respond_list(Self::parse_numstat_output(&output))
            }
            GitReq::Show(rev) => {
                let output = self.run_git(&[
                    "log",
                    "-n",
                    "1",
                    &rev,
                    "--format=%H%x00%s%x00%an%x00%cI",
                    "--name-only",
                ])?;
                let commit = Self::parse_single_commit(&output, &rev)?;
                cx.respond(commit)
            }
        }
    }
}

// ============================================================================
// Tag 9: Time (UTC wall clock)
// ============================================================================

#[derive(FromCore)]
pub enum TimeReq {
    #[core(name = "TimeNow")]
    Now,
}

#[derive(Clone)]
pub struct TimeHandler;

impl DescribeEffect for TimeHandler {
    fn effect_decl() -> EffectDecl {
        tidepool_mcp::time_decl()
    }
}

impl EffectHandler<CapturedOutput> for TimeHandler {
    type Request = TimeReq;

    fn handle(
        &mut self,
        req: TimeReq,
        cx: &EffectContext<'_, CapturedOutput>,
    ) -> Result<tidepool_effect::Response, EffectError> {
        match req {
            TimeReq::Now => {
                let millis = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map_err(|e| EffectError::Handler(format!("system time error: {}", e)))?
                    .as_millis() as i64;
                cx.respond(millis)
            }
        }
    }
}

// ============================================================================
// Stack assembly
// ============================================================================

/// Configuration for building the base effect handler stack.
pub struct HandlerConfig {
    /// Working directory (sandbox root for Fs, Sg, Exec, Lsp, Git).
    pub cwd: PathBuf,
    /// Path for the KV store's JSON backing file.
    pub kv_path: PathBuf,
    /// LLM model name (routed by genai: gpt-* → OpenAI, claude-* → Anthropic, etc.).
    pub llm_model: String,
}

/// Build the base effect stack (tags 0–9: Console, KV, Fs, SG, Http, Exec, Lsp, Llm, Git, Time).
///
/// **Must be called inside a tokio runtime** — `LlmHandler` captures
/// `tokio::runtime::Handle::current()` at construction time.
///
/// Ask (tag 10) is **not** included here; it is interposed by each server's
/// `AskDispatcher` wrapper (see `TidepoolMcpServer::new`).
pub fn build_base_stack(
    cfg: &HandlerConfig,
) -> impl tidepool_effect::dispatch::DispatchEffect<CapturedOutput>
       + CollectEffectDecls
       + Clone
       + Send
       + Sync
       + 'static {
    frunk::hlist![
        ConsoleHandler,
        KvHandler::new(cfg.kv_path.clone()),
        FsHandler::new(cfg.cwd.clone()),
        SgHandler::new(cfg.cwd.clone()),
        HttpHandler,
        ExecHandler::new(cfg.cwd.clone()),
        LspHandler::new(cfg.cwd.clone()),
        LlmHandler::new(cfg.llm_model.clone()),
        GitHandler::new(cfg.cwd.clone()),
        TimeHandler,
    ]
}

/// Build the MINIMAL effect stack (tag 0: Console only).
///
/// For cheap-startup sessions and tests that exercise the session mechanism
/// rather than the effects — it avoids constructing the heavier handlers (Llm's
/// genai client, the cwd-bound Fs/SG/Exec/Lsp). Ask (the next tag) is interposed
/// by each server's `AskDispatcher` wrapper, as with [`build_base_stack`]. Pair
/// with [`base_decls_with_ask`] (which is generic over any `CollectEffectDecls`
/// stack) to derive `(decls, ask_tag)`.
pub fn build_minimal_stack() -> impl tidepool_effect::dispatch::DispatchEffect<CapturedOutput>
       + CollectEffectDecls
       + Clone
       + Send
       + Sync
       + 'static {
    frunk::hlist![ConsoleHandler]
}

/// Collect effect declarations from a base stack and append the Ask effect.
///
/// Returns `(decls, ask_tag)` where `ask_tag` is the index of the Ask effect
/// in `decls`. Mirrors `TidepoolMcpServer::new`'s internal logic so Wave B
/// servers can build the same declaration list without constructing a full
/// `TidepoolMcpServer`.
pub fn base_decls_with_ask<H: CollectEffectDecls>(_stack: &H) -> (Vec<EffectDecl>, u64) {
    let mut decls = H::collect_decls();
    let ask_tag = decls.len() as u64;
    decls.push(tidepool_mcp::ask_decl());
    (decls, ask_tag)
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use tidepool_bridge::{FromCore, ToCore};
    use tidepool_effect::dispatch::DispatchEffect;
    use tidepool_repr::{DataCon, DataConId, DataConTable};

    /// Unwrap a handler Response: Complete passes through; Stream drains into
    /// the equivalent cons-list Value.
    fn response_value(r: tidepool_effect::Response, table: &DataConTable) -> Value {
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

    fn repo_root() -> std::path::PathBuf {
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

    fn prelude_include() -> std::path::PathBuf {
        let mut dir = repo_root();
        dir.push("haskell");
        dir.push("lib");
        dir
    }

    fn jit_test_source(code: &[&str]) -> String {
        let decls = tidepool_mcp::standard_decls();
        let preamble = tidepool_mcp::build_preamble(&decls, false);
        let stack = tidepool_mcp::build_effect_stack_type(&decls);
        let code_str = tidepool_mcp::wrap_do(&code.join("\n"));
        tidepool_mcp::template_haskell(&preamble, &stack, &code_str, "", "", None, None)
    }

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

    /// Build a DataConTable with standard types + all effect constructors.
    fn full_effect_test_table() -> DataConTable {
        let mut t = tidepool_testing::gen::datacon_table::standard_datacon_table();
        let mut decls = tidepool_mcp::standard_decls();
        decls.push(tidepool_mcp::meta_decl());
        let mut next_id = 100u64;

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
            // Git effect response types
            ("Commit", 5),
            ("StatusEntry", 2),
            ("FileDelta", 4),
            // Time effect response types
            ("UTCTime", 1),
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

    // === SgHandler glob + truncation tests ===

    #[test]
    fn test_sg_collect_files_glob() {
        use tempfile::tempdir;
        let dir = tempdir().unwrap();
        let root = dir.path().to_path_buf();
        std::fs::create_dir_all(root.join("sub")).unwrap();
        std::fs::write(root.join("a.rs"), "fn a() {}").unwrap();
        std::fs::write(root.join("sub/b.rs"), "fn b() {}").unwrap();
        std::fs::write(root.join("ignore.hs"), "x = 1").unwrap();

        let handler = SgHandler::new(root.clone());
        let files = handler
            .collect_files(SupportLang::Rust, &["**/*.rs".to_string()])
            .unwrap();
        assert!(
            files.len() >= 2,
            "expected .rs files from glob, got {files:?}"
        );
        assert!(
            files
                .iter()
                .all(|f| f.extension().map(|e| e == "rs").unwrap_or(false)),
            "non-.rs file slipped through: {files:?}"
        );
    }

    #[test]
    fn test_truncate_match_text_short() {
        let s = "hello".to_string();
        assert_eq!(truncate_match_text(s.clone()), s);
    }

    #[test]
    fn test_truncate_match_text_long() {
        let long = "x".repeat(600);
        let result = truncate_match_text(long);
        assert!(
            result.len() <= MATCH_TEXT_LIMIT + 4,
            "too long: {}",
            result.len()
        );
        assert!(result.ends_with('…'));
    }

    // === Structural guard tests ===

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
        assert!(!component_filter(
            "**/*.rs",
            Path::new(".exo/w1/src/lib.rs")
        ));
        assert!(!component_filter("**/*.rs", Path::new(".jj/store/x.rs")));
        assert!(!component_filter("**/*.rs", Path::new("a/.cache/x.rs")));
        assert!(component_filter(
            ".tidepool/lib/*.hs",
            Path::new(".tidepool/lib/Std.hs")
        ));
        assert!(component_filter(
            ".exo/**/*.rs",
            Path::new(".exo/w1/src/lib.rs")
        ));
        assert!(!component_filter("**/*.rs", Path::new("target/debug/x.rs")));
        assert!(component_filter(
            "target/**/*.rs",
            Path::new("target/debug/x.rs")
        ));
        assert!(component_filter(
            "**/*.rs",
            Path::new("tidepool-repr/src/lib.rs")
        ));
        assert!(!component_filter("**/*", Path::new("src/.hidden")));
        assert!(component_filter(".gitignore", Path::new(".gitignore")));
    }

    #[test]
    fn test_glob_bare_dir_recurses_and_missing_root_errors() {
        use tempfile::tempdir;
        let dir = tempdir().unwrap();
        let root = dir.path().to_path_buf();
        std::fs::create_dir_all(root.join("src/inner")).unwrap();
        std::fs::write(root.join("src/a.rs"), "x").unwrap();
        std::fs::write(root.join("src/inner/b.rs"), "y").unwrap();
        let handler = FsHandler::new(root.clone());

        // rg-style: a bare directory (no metachars) recurses (friction #20 —
        // previously returned []).
        let names: Vec<String> = handler
            .expand_glob("src")
            .unwrap()
            .iter()
            .filter(|p| p.is_file())
            .filter_map(|p| p.strip_prefix(&root).ok())
            .map(|p| p.to_string_lossy().to_string())
            .collect();
        assert!(names.iter().any(|n| n.ends_with("a.rs")), "{names:?}");
        assert!(names.iter().any(|n| n.ends_with("b.rs")), "{names:?}");

        // A nonexistent root is a LOUD error, not a silent empty result.
        let err = handler.expand_glob("does/not/exist").unwrap_err();
        assert!(
            format!("{err}").contains("no such file or directory"),
            "{err}"
        );

        // A concrete existing file passes through.
        let one = handler.expand_glob("src/a.rs").unwrap();
        assert_eq!(one.len(), 1);
        assert!(one[0].ends_with("a.rs"));

        // A real glob that matches nothing still returns [] (not an error).
        assert!(handler.expand_glob("src/*.nope").unwrap().is_empty());
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

        let req = SgReq::Plan(
            Lang::Rust,
            "foo($A)".into(),
            "bar($A)".into(),
            vec!["t.rs".into()],
        );
        let res = response_value(handler.handle(req, &cx).unwrap(), &table);
        let _ = res;
        assert!(
            std::fs::read_to_string(&f).unwrap().contains("foo(1)"),
            "plan must not write"
        );

        let req = SgReq::Apply(
            Lang::Rust,
            "foo($A)".into(),
            "bar($A)".into(),
            vec!["t.rs".into()],
        );
        let res = response_value(handler.handle(req, &cx).unwrap(), &table);
        let n = match &res {
            Value::Con(_, fields) => match fields.as_slice() {
                [Value::Lit(tidepool_repr::Literal::LitInt(n))] => *n,
                other => panic!("expected boxed Int, got {:?}", other),
            },
            Value::Lit(tidepool_repr::Literal::LitInt(n)) => *n,
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

        let bin_path = root.join("test.bin");
        std::fs::write(&bin_path, vec![0, 1, 2, 3]).unwrap();

        let target_dir = root.join("target");
        std::fs::create_dir(&target_dir).unwrap();
        std::fs::write(target_dir.join("ignored.txt"), "hello").unwrap();

        let mut handler = FsHandler::new(root.clone());
        let table = full_effect_test_table();
        let captured = CapturedOutput::new();
        let cx = EffectContext::with_user(&table, &captured);

        let req = FsReq::Grep("hello".to_string(), "**/*.txt".to_string());
        let res = response_value(handler.handle(req, &cx).unwrap(), &table);
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

        let req = FsReq::Grep("hello".to_string(), "**/*".to_string());
        let res = response_value(handler.handle(req, &cx).unwrap(), &table);
        let results: Vec<(String, i64, String)> = FromCore::from_value(&res, &table).unwrap();
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
        let results: Vec<(String, i64, String)> = FromCore::from_value(&res, &table).unwrap();

        assert_eq!(results.len(), 2001);
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
        "Console", "KV", "Fs", "SG", "Http", "Exec", "Lsp", "Llm", "Git", "Time", "Ask",
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

    /// Two `KvHandler` instances backed by different paths must not share keys.
    /// This is the unit-level guard for per-session KV isolation (no live extract needed).
    #[test]
    fn kv_handlers_with_different_paths_are_isolated() {
        let table = full_effect_test_table();
        let captured = CapturedOutput::new();
        let cx = EffectContext::with_user(&table, &captured);

        let pid = std::process::id();
        let path_a = std::env::temp_dir().join(format!("tidepool_kv_iso_a_{pid}.json"));
        let path_b = std::env::temp_dir().join(format!("tidepool_kv_iso_b_{pid}.json"));
        let _ = std::fs::remove_file(&path_a);
        let _ = std::fs::remove_file(&path_b);

        let mut ha = frunk::hlist![KvHandler::new(path_a.clone())];
        let mut hb = frunk::hlist![KvHandler::new(path_b.clone())];

        // Set a key in handler A (session A).
        let set_id = table.get_by_name("KvSet").unwrap();
        let keys_id = table.get_by_name("KvKeys").unwrap();
        let key_val = "isolation-test-key".to_string().to_value(&table).unwrap();
        let lit_val = Value::Lit(tidepool_repr::Literal::LitInt(99));
        let set_req = Value::Con(set_id, vec![key_val, lit_val]);
        ha.dispatch(0, &set_req, &cx).unwrap();

        // kvKeys on handler B (session B) must be empty — no bleed from A.
        let keys_req = Value::Con(keys_id, vec![]);
        let keys_b = response_value(hb.dispatch(0, &keys_req, &cx).unwrap(), &table);
        match &keys_b {
            Value::Con(id, args) => {
                let name = table.name_of(*id).unwrap();
                assert_eq!(
                    name, "[]",
                    "session B should have no keys after session A kvSet; got fields: {args:?}"
                );
            }
            other => panic!("expected empty list (\"[]\"), got {:?}", other),
        }

        let _ = std::fs::remove_file(&path_a);
        let _ = std::fs::remove_file(&path_b);
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
        assert_is_cons_list(&result, &table);
    }

    #[test]
    fn test_fs_write_creates_parent_dirs() {
        use tempfile::tempdir;
        let dir = tempdir().unwrap();
        let root = dir.path().to_path_buf();
        // a/b/ does not exist — write must create it
        let mut handler = FsHandler::new(root.clone());
        let table = full_effect_test_table();
        let captured = CapturedOutput::new();
        let cx = EffectContext::with_user(&table, &captured);
        let req = FsReq::Write("a/b/c.txt".into(), "hello mkdir-p".into());
        handler
            .handle(req, &cx)
            .expect("write into missing subtree must succeed");
        let actual = std::fs::read_to_string(root.join("a/b/c.txt")).unwrap();
        assert_eq!(actual, "hello mkdir-p");
    }

    #[test]
    fn test_fs_write_sandbox_escape_with_missing_parents() {
        use tempfile::tempdir;
        let dir = tempdir().unwrap();
        let root = dir.path().to_path_buf();
        let mut handler = FsHandler::new(root.clone());
        let table = full_effect_test_table();
        let captured = CapturedOutput::new();
        let cx = EffectContext::with_user(&table, &captured);
        // Attempt to escape via `..` into a sibling directory that doesn't exist
        let req = FsReq::Write("../../escape/evil.txt".into(), "bad".into());
        let err = handler
            .handle(req, &cx)
            .expect_err("sandbox escape must be rejected");
        let msg = format!("{err}");
        assert!(
            msg.contains("outside sandbox") || msg.contains("escape"),
            "{msg}"
        );
    }

    // === SG FromCore tests ===

    #[test]
    fn test_sg_from_core_find() {
        let table = full_effect_test_table();
        let con_id = table.get_by_name("SgFind").unwrap();
        let lang_id = table.get_by_name("Rust").unwrap();
        let lang = Value::Con(lang_id, vec![]);
        let pattern = "fn $NAME".to_string().to_value(&table).unwrap();
        let nil_id = table.get_by_name("[]").unwrap();
        let files = Value::Con(nil_id, vec![]);
        let val = Value::Con(con_id, vec![lang, pattern, files]);
        let req = SgReq::from_value(&val, &table).unwrap();
        assert!(matches!(req, SgReq::Find(_, ref p, ref f) if p == "fn $NAME" && f.is_empty()));
    }

    // === Http FromCore tests ===

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
        let null_id = table.get_by_name("Null").unwrap();
        let body = Value::Con(null_id, vec![]);
        let val = Value::Con(con_id, vec![url, body]);
        let req = HttpReq::from_value(&val, &table).unwrap();
        assert!(matches!(req, HttpReq::Post(ref u, _) if u == "https://example.com/api"));
    }

    // === Exec FromCore tests ===

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

    // === Lsp FromCore tests ===

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

    #[test]
    fn test_meta_dispatch_roundtrip_version() {
        let table = full_effect_test_table();
        let captured = CapturedOutput::new();
        let cx = EffectContext::with_user(&table, &captured);
        let mut handlers = frunk::hlist![MetaHandler::new(vec![], vec![])];
        let con_id = table.get_by_name("MetaVersion").unwrap();
        let request = Value::Con(con_id, vec![]);
        let result = response_value(handlers.dispatch(0, &request, &cx).unwrap(), &table);
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
        assert_is_cons_list(&result, &table);
    }

    // === Ask construction test ===

    #[test]
    fn test_ask_constructor_in_table() {
        let table = full_effect_test_table();
        let con_id = table.get_by_name("AskWith").unwrap();
        let dc = table.get(con_id).unwrap();
        assert_eq!(dc.rep_arity, 2, "AskWith should have arity 2");
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

    // === Mock LLM handler for JIT structured-output tests ===

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
            LspHandler::new(cwd.clone()),
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
        std::env::remove_var("TIDEPOOL_LLM_PROVIDER");
        assert_eq!(
            LlmHandler::effective_model("ollama:llama3.2".into()),
            "ollama:llama3.2"
        );
        assert_eq!(LlmHandler::effective_model("gpt-4o".into()), "gpt-4o");

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
        assert_eq!(
            LlmHandler::normalize_model("ollama:llama3.2".into()),
            "ollama::llama3.2"
        );
        assert_eq!(
            LlmHandler::normalize_model("anthropic:claude-haiku-4-5".into()),
            "anthropic::claude-haiku-4-5"
        );
        assert_eq!(
            LlmHandler::normalize_model("ollama::llama3.2".into()),
            "ollama::llama3.2"
        );
        assert_eq!(
            LlmHandler::normalize_model("gpt-4o-mini".into()),
            "gpt-4o-mini"
        );
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
        let fresh = handler.clone();
        assert!(fresh.check_rate_limit().is_ok());
        assert!(handler.check_rate_limit().is_err());
    }

    #[test]
    fn test_strictify_optional_fields() {
        let mut schema = serde_json::json!({
            "type": "object",
            "properties": {
                "a": {"type": "string"},
                "b": {"type": "number"}
            },
            "required": ["a"]
        });
        strictify(&mut schema);
        let req: Vec<&str> = schema["required"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_str().unwrap())
            .collect();
        assert!(req.contains(&"a") && req.contains(&"b"));
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
        let inner = &schema["properties"]["items"]["items"];
        assert_eq!(inner["required"], serde_json::json!(["opt"]));
        assert_eq!(
            inner["properties"]["opt"],
            serde_json::json!({"anyOf": [{"type": "string"}, {"type": "null"}]})
        );
    }

    #[test]
    fn test_strictify_all_required_unchanged() {
        let mut schema = serde_json::json!({
            "type": "object",
            "properties": {"answer": {"type": "boolean"}},
            "required": ["answer"]
        });
        let before = schema.clone();
        strictify(&mut schema);
        assert_eq!(schema, before);
    }

    /// Live smoke against the real OpenAI API — runs only when OPENAI_API_KEY is set.
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

        let resp = client
            .exec_chat(
                model,
                genai::chat::ChatRequest::from_user("Reply with the single word: pong"),
                None,
            )
            .await
            .expect("chat");
        assert!(!resp.first_text().unwrap_or("").is_empty());

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

    // === Git FromCore roundtrip tests ===

    #[test]
    fn test_git_from_core_log() {
        let table = full_effect_test_table();
        let con_id = table.get_by_name("GitLog").unwrap();
        let n = (5i64).to_value(&table).unwrap();
        let val = Value::Con(con_id, vec![n]);
        let req = GitReq::from_value(&val, &table).unwrap();
        assert!(matches!(req, GitReq::Log(5)));
    }

    #[test]
    fn test_git_from_core_status() {
        let table = full_effect_test_table();
        let con_id = table.get_by_name("GitStatus").unwrap();
        let val = Value::Con(con_id, vec![]);
        let req = GitReq::from_value(&val, &table).unwrap();
        assert!(matches!(req, GitReq::Status));
    }

    #[test]
    fn test_git_from_core_diffstat() {
        let table = full_effect_test_table();
        let con_id = table.get_by_name("GitDiffStat").unwrap();
        let rev = "HEAD~1".to_string().to_value(&table).unwrap();
        let val = Value::Con(con_id, vec![rev]);
        let req = GitReq::from_value(&val, &table).unwrap();
        assert!(matches!(req, GitReq::DiffStat(ref r) if r == "HEAD~1"));
    }

    #[test]
    fn test_git_from_core_show() {
        let table = full_effect_test_table();
        let con_id = table.get_by_name("GitShow").unwrap();
        let rev = "HEAD".to_string().to_value(&table).unwrap();
        let val = Value::Con(con_id, vec![rev]);
        let req = GitReq::from_value(&val, &table).unwrap();
        assert!(matches!(req, GitReq::Show(ref r) if r == "HEAD"));
    }

    // =========================================================================
    // Git handler tests — unit (parse functions) + integration (scratch repo)
    // =========================================================================

    #[test]
    fn test_git_parse_log_output_two_commits() {
        // Simulate `git log -n 2 --format="%H%x00%s%x00%an%x00%cI" --name-only`
        let output = "\
abc123\x00First commit\x00Alice\x002024-01-01T00:00:00+00:00\n\
\n\
file_a.txt\n\
file_b.rs\n\
\n\
def456\x00Second commit\x00Bob\x002024-01-02T00:00:00+00:00\n\
\n\
file_c.txt\n\
\n";
        let commits = GitHandler::parse_log_output(output);
        assert_eq!(commits.len(), 2);
        assert_eq!(commits[0].sha, "abc123");
        assert_eq!(commits[0].subject, "First commit");
        assert_eq!(commits[0].author, "Alice");
        assert_eq!(commits[0].date, "2024-01-01T00:00:00+00:00");
        assert_eq!(commits[0].files, vec!["file_a.txt", "file_b.rs"]);
        assert_eq!(commits[1].sha, "def456");
        assert_eq!(commits[1].subject, "Second commit");
        assert_eq!(commits[1].files, vec!["file_c.txt"]);
    }

    #[test]
    fn test_git_parse_status_renames() {
        let output = "M  src/lib.rs\n?? untracked.txt\nR  old.rs -> new.rs\n";
        let entries = GitHandler::parse_status_output(output);
        assert_eq!(entries.len(), 3);
        assert_eq!(entries[0].state, "M ");
        assert_eq!(entries[0].path, "src/lib.rs");
        assert_eq!(entries[1].state, "??");
        assert_eq!(entries[1].path, "untracked.txt");
        // Rename: destination path only
        assert_eq!(entries[2].path, "new.rs");
    }

    #[test]
    fn test_git_parse_numstat_with_binary() {
        let output = "10\t5\tsrc/lib.rs\n-\t-\timage.png\n3\t0\tdocs/README.md\n";
        let deltas = GitHandler::parse_numstat_output(output);
        assert_eq!(deltas.len(), 3);
        assert_eq!(deltas[0].path, "src/lib.rs");
        assert_eq!(deltas[0].adds, 10);
        assert_eq!(deltas[0].dels, 5);
        assert!(!deltas[0].binary);
        assert_eq!(deltas[1].path, "image.png");
        assert!(deltas[1].binary);
        assert_eq!(deltas[1].adds, 0);
        assert_eq!(deltas[2].path, "docs/README.md");
        assert_eq!(deltas[2].adds, 3);
        assert_eq!(deltas[2].dels, 0);
    }

    // Build a scratch git repo with 2 commits, a staged file, and an untracked file.
    fn make_scratch_repo() -> tempfile::TempDir {
        use std::fs;
        use std::process::Command;
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path();

        let run = |args: &[&str]| {
            Command::new(args[0])
                .args(&args[1..])
                .current_dir(p)
                .env("GIT_AUTHOR_NAME", "Test")
                .env("GIT_AUTHOR_EMAIL", "test@test.com")
                .env("GIT_COMMITTER_NAME", "Test")
                .env("GIT_COMMITTER_EMAIL", "test@test.com")
                .env("GIT_AUTHOR_DATE", "2024-01-01T00:00:00+00:00")
                .env("GIT_COMMITTER_DATE", "2024-01-01T00:00:00+00:00")
                .output()
                .expect("git command failed")
        };

        run(&["git", "init", "--initial-branch=main"]);
        run(&["git", "config", "user.email", "test@test.com"]);
        run(&["git", "config", "user.name", "Test"]);

        // First commit
        fs::write(p.join("alpha.txt"), "alpha content").unwrap();
        run(&["git", "add", "alpha.txt"]);
        run(&["git", "commit", "-m", "first: add alpha"]);

        // Second commit
        fs::write(p.join("beta.txt"), "beta content").unwrap();
        run(&["git", "add", "beta.txt"]);
        run(&["git", "commit", "-m", "second: add beta"]);

        // Staged change
        fs::write(p.join("alpha.txt"), "alpha modified").unwrap();
        run(&["git", "add", "alpha.txt"]);

        // Untracked file
        fs::write(p.join("untracked.txt"), "untracked").unwrap();

        dir
    }

    #[test]
    fn test_git_handler_log_two_commits_newest_first() {
        let dir = make_scratch_repo();
        let table = full_effect_test_table();
        let captured = CapturedOutput::new();
        let cx = EffectContext::with_user(&table, &captured);
        let mut handler = GitHandler::new(dir.path().to_path_buf());

        let n = (2i64).to_value(&table).unwrap();
        let con_id = table.get_by_name("GitLog").unwrap();
        let request = Value::Con(con_id, vec![n]);
        let result = response_value(handler.handle(GitReq::Log(2), &cx).unwrap(), &table);

        // Should be a cons list with 2 Commit cells
        let mut node = &result;
        let mut count = 0;
        loop {
            match node {
                Value::Con(id, fields) => {
                    let name = table.name_of(*id).unwrap();
                    match name {
                        "[]" => break,
                        ":" => {
                            assert_eq!(fields.len(), 2);
                            // head is a Commit (5 fields)
                            match &fields[0] {
                                Value::Con(cid, cfields) => {
                                    assert_eq!(table.name_of(*cid).unwrap(), "Commit");
                                    assert_eq!(cfields.len(), 5, "Commit must have 5 fields");
                                }
                                other => panic!("expected Commit Con, got {:?}", other),
                            }
                            count += 1;
                            node = &fields[1];
                        }
                        other => panic!("unexpected list constructor: {}", other),
                    }
                }
                other => panic!("expected Con, got {:?}", other),
            }
        }
        assert_eq!(count, 2, "gitLog 2 should return exactly 2 commits");
        // Suppress unused warning from the FromCore round-trip test above
        let _ = request;
    }

    #[test]
    fn test_git_handler_status_sees_staged_and_untracked() {
        let dir = make_scratch_repo();
        let table = full_effect_test_table();
        let captured = CapturedOutput::new();
        let cx = EffectContext::with_user(&table, &captured);
        let mut handler = GitHandler::new(dir.path().to_path_buf());

        let result = response_value(handler.handle(GitReq::Status, &cx).unwrap(), &table);

        // Collect all StatusEntry names from the cons list
        let mut paths_and_states: Vec<(String, String)> = Vec::new();
        let mut node = &result;
        loop {
            match node {
                Value::Con(id, fields) if table.name_of(*id).unwrap() == ":" => {
                    if let Value::Con(eid, efields) = &fields[0] {
                        assert_eq!(table.name_of(*eid).unwrap(), "StatusEntry");
                        assert_eq!(efields.len(), 2);
                        // path is efields[0], state is efields[1]
                        // Extract Text from Con("Text", [ByteArray, off, len])
                        paths_and_states.push(("?".into(), "?".into()));
                    }
                    node = &fields[1];
                }
                Value::Con(id, _) if table.name_of(*id).unwrap() == "[]" => break,
                other => panic!("unexpected: {:?}", other),
            }
        }
        // At minimum we should see 2 entries (staged alpha.txt + untracked.txt)
        assert!(
            paths_and_states.len() >= 2,
            "gitStatus should return at least 2 entries, got {}",
            paths_and_states.len()
        );
    }

    #[test]
    fn test_git_handler_diffstat_head_tilde1() {
        let dir = make_scratch_repo();
        let table = full_effect_test_table();
        let captured = CapturedOutput::new();
        let cx = EffectContext::with_user(&table, &captured);
        let mut handler = GitHandler::new(dir.path().to_path_buf());

        let result = response_value(
            handler
                .handle(GitReq::DiffStat("HEAD~1".to_string()), &cx)
                .unwrap(),
            &table,
        );
        // HEAD~1 introduces beta.txt; should return at least 1 FileDelta
        let mut count = 0;
        let mut node = &result;
        loop {
            match node {
                Value::Con(id, fields) if table.name_of(*id).unwrap() == ":" => {
                    match &fields[0] {
                        Value::Con(did, dfields) => {
                            assert_eq!(table.name_of(*did).unwrap(), "FileDelta");
                            assert_eq!(dfields.len(), 4, "FileDelta must have 4 fields");
                        }
                        other => panic!("expected FileDelta Con, got {:?}", other),
                    }
                    count += 1;
                    node = &fields[1];
                }
                Value::Con(id, _) if table.name_of(*id).unwrap() == "[]" => break,
                other => panic!("unexpected: {:?}", other),
            }
        }
        assert!(
            count >= 1,
            "gitDiffStat HEAD~1 should return at least 1 delta"
        );
    }

    #[test]
    fn test_git_handler_show_head() {
        let dir = make_scratch_repo();
        let table = full_effect_test_table();
        let captured = CapturedOutput::new();
        let cx = EffectContext::with_user(&table, &captured);
        let mut handler = GitHandler::new(dir.path().to_path_buf());

        let result = response_value(
            handler
                .handle(GitReq::Show("HEAD".to_string()), &cx)
                .unwrap(),
            &table,
        );
        match &result {
            Value::Con(id, fields) => {
                assert_eq!(table.name_of(*id).unwrap(), "Commit");
                assert_eq!(
                    fields.len(),
                    5,
                    "Commit must have 5 fields (sha/subject/author/date/files)"
                );
            }
            other => panic!("gitShow HEAD should return a Commit, got {:?}", other),
        }
    }

    #[test]
    fn test_git_handler_show_bad_revspec_errors() {
        let dir = make_scratch_repo();
        let table = full_effect_test_table();
        let captured = CapturedOutput::new();
        let cx = EffectContext::with_user(&table, &captured);
        let mut handler = GitHandler::new(dir.path().to_path_buf());

        let result = handler.handle(GitReq::Show("notaref_zzzzzz".to_string()), &cx);
        assert!(result.is_err(), "gitShow with bad revspec should error");
    }

    fn extract_available() -> bool {
        let bin =
            std::env::var("TIDEPOOL_EXTRACT").unwrap_or_else(|_| "tidepool-extract".to_string());
        std::process::Command::new(&bin)
            .arg("--help")
            .output()
            .is_ok()
    }

    /// Full JIT end-to-end: `gitLog 1` on the real repo returns a Commit record
    /// with a 40-character sha field, exercising the generated Tidepool.Effects
    /// wiring + Records visibility + con-name/arity agreement through the JIT.
    /// Skips cleanly when TIDEPOOL_EXTRACT is unavailable.
    #[tokio::test]
    async fn test_jit_git_log_returns_commit() {
        if !extract_available() {
            eprintln!("skipping: tidepool-extract not available (set TIDEPOOL_EXTRACT)");
            return;
        }
        let decls = tidepool_mcp::standard_decls();
        // Return the observed values (not a collapsed Bool) so a failure names
        // which invariant broke and what we actually saw.
        let source = jit_test_source(&[
            "commits <- gitLog 1",
            "let n = length commits",
            "let shaLen = case commits of { (c:_) -> T.length c.sha; _ -> 0 }",
            "pure (toJSON [n, shaLen])",
        ]);
        let include = prelude_include();
        let effects_dir = tidepool_mcp::ensure_effects_module(&decls).unwrap();
        let include_paths: Vec<&std::path::Path> = vec![include.as_path(), effects_dir.as_path()];
        let kv_path = std::env::temp_dir().join("tidepool_git_jit_kv.json");
        let cwd = repo_root();
        let captured = CapturedOutput::new();
        let mut handlers = frunk::hlist![
            ConsoleHandler,
            KvHandler::new(kv_path),
            FsHandler::new(cwd.clone()),
            SgHandler::new(cwd.clone()),
            HttpHandler,
            ExecHandler::new(cwd.clone()),
            LspHandler::new(cwd.clone()),
            LlmHandler::new("ollama:llama3.2".to_string()),
            GitHandler::new(cwd.clone()),
            TimeHandler,
        ];
        let result = tidepool_runtime::compile_and_run(
            &source,
            "result",
            &include_paths,
            &mut handlers,
            &captured,
        );
        match result {
            Ok(v) => assert_eq!(
                v.to_json(),
                serde_json::json!([1, 40]),
                "gitLog 1 should return exactly 1 Commit ([n, shaLen] observed)"
            ),
            Err(e) => panic!("JIT gitLog eval failed: {:?}", e),
        }
    }

    // === Time FromCore roundtrip tests ===

    #[test]
    fn test_time_from_core_now() {
        let table = full_effect_test_table();
        let con_id = table.get_by_name("TimeNow").unwrap();
        let val = Value::Con(con_id, vec![]);
        let req = TimeReq::from_value(&val, &table).unwrap();
        assert!(matches!(req, TimeReq::Now));
    }

    #[test]
    fn test_time_dispatch_now() {
        let table = full_effect_test_table();
        let captured = CapturedOutput::new();
        let cx = EffectContext::with_user(&table, &captured);
        let mut handlers = frunk::hlist![TimeHandler];
        let con_id = table.get_by_name("TimeNow").unwrap();
        let request = Value::Con(con_id, vec![]);
        let result = response_value(handlers.dispatch(0, &request, &cx).unwrap(), &table);
        // i64::to_value boxes as Con(I#, [LitInt(n)]).
        let ms = match &result {
            Value::Con(_, fields) if fields.len() == 1 => match &fields[0] {
                Value::Lit(tidepool_repr::Literal::LitInt(n)) => *n,
                _ => panic!("Expected Con(_, [LitInt]), got {:?}", result),
            },
            _ => panic!("Expected Con(I#, [LitInt(ms)]), got {:?}", result),
        };
        assert!(ms > 0, "epoch millis should be positive, got {}", ms);
    }

    // === Time JIT e2e tests ===

    fn time_jit_handlers(
        cwd: std::path::PathBuf,
        kv_path: std::path::PathBuf,
    ) -> impl tidepool_effect::dispatch::DispatchEffect<CapturedOutput> {
        frunk::hlist![
            ConsoleHandler,
            KvHandler::new(kv_path),
            FsHandler::new(cwd.clone()),
            SgHandler::new(cwd.clone()),
            HttpHandler,
            ExecHandler::new(cwd.clone()),
            LspHandler::new(cwd.clone()),
            LlmHandler::new("ollama:llama3.2".to_string()),
            GitHandler::new(cwd.clone()),
            TimeHandler,
        ]
    }

    #[tokio::test]
    async fn test_jit_time_format_golden() {
        if !extract_available() {
            eprintln!("skipping: tidepool-extract not available (set TIDEPOOL_EXTRACT)");
            return;
        }
        let decls = tidepool_mcp::standard_decls();
        // Pure computation — no Time effect dispatch; tests civil_from_days + formatting.
        let source = jit_test_source(&[
            "let t0 = UTCTime 0",
            "let t1 = UTCTime 1709164800000",
            "let s0 = formatISO8601 t0",
            "let s1 = formatISO8601 t1",
            "pure (toJSON [s0, s1])",
        ]);
        let include = prelude_include();
        let effects_dir = tidepool_mcp::ensure_effects_module(&decls).unwrap();
        let include_paths: Vec<&std::path::Path> = vec![include.as_path(), effects_dir.as_path()];
        let kv_path = std::env::temp_dir().join("tidepool_time_golden_kv.json");
        let cwd = repo_root();
        let captured = CapturedOutput::new();
        let mut handlers = time_jit_handlers(cwd, kv_path);
        let result = tidepool_runtime::compile_and_run(
            &source,
            "result",
            &include_paths,
            &mut handlers,
            &captured,
        );
        match result {
            Ok(v) => assert_eq!(
                v.to_json(),
                serde_json::json!(["1970-01-01T00:00:00Z", "2024-02-29T00:00:00Z"]),
                "golden ISO-8601 timestamps ([epoch, leap-day] observed)"
            ),
            Err(e) => panic!("JIT time golden eval failed: {:?}", e),
        }
    }

    #[tokio::test]
    async fn test_jit_time_now_e2e() {
        if !extract_available() {
            eprintln!("skipping: tidepool-extract not available (set TIDEPOOL_EXTRACT)");
            return;
        }
        let decls = tidepool_mcp::standard_decls();
        let source = jit_test_source(&["t <- getCurrentTime", "pure (toJSON (epochMillis t))"]);
        let include = prelude_include();
        let effects_dir = tidepool_mcp::ensure_effects_module(&decls).unwrap();
        let include_paths: Vec<&std::path::Path> = vec![include.as_path(), effects_dir.as_path()];
        let kv_path = std::env::temp_dir().join("tidepool_time_now_kv.json");
        let cwd = repo_root();
        let captured = CapturedOutput::new();
        let mut handlers = time_jit_handlers(cwd, kv_path);
        let rust_before = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_millis() as i64;
        let result = tidepool_runtime::compile_and_run(
            &source,
            "result",
            &include_paths,
            &mut handlers,
            &captured,
        );
        let rust_after = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_millis() as i64;
        match result {
            Ok(v) => {
                // Return observed [ms, rust_before, rust_after] on failure so we can see
                // the actual values rather than just a collapsed bool.
                let ms = v
                    .to_json()
                    .as_i64()
                    .unwrap_or_else(|| panic!("expected i64 epoch millis, got {:?}", v.to_json()));
                let five_min_ms = 5 * 60 * 1000_i64;
                assert!(
                    ms >= rust_before - five_min_ms && ms <= rust_after + five_min_ms,
                    "getCurrentTime returned {} ms; expected in [{}, {}] (±5 min of Rust wall clock)",
                    ms,
                    rust_before - five_min_ms,
                    rust_after + five_min_ms
                );
            }
            Err(e) => panic!("JIT getCurrentTime eval failed: {:?}", e),
        }
    }
}
