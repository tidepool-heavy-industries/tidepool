use std::path::PathBuf;
use tidepool_effect::dispatch::EffectContext;
use tidepool_effect::error::EffectError;
use tidepool_mcp::CapturedOutput;

// ============================================================================
// Tag 2: File I/O (sandboxed to working directory)
// ============================================================================

// FsReq + DescribeEffect + EffectHandler dispatch are generated from the
// single-source definition; only the handler struct and the per-verb method
// bodies below are hand-written.
tidepool_mcp::fs_effect_def!(crate::effect_glue::effect_rust_projection);

/// The record `readGlob` (`FsReadGlob`) yields per matched file (#335): a
/// `path` plus a typed `contents` — `Right text` on a clean UTF-8 read, `Left
/// (FsError)` on a per-file failure. Named record per the records-over-tuples
/// house rule; its Haskell `data FileRead = FileRead { path, contents }` decl is
/// emitted into `Tidepool.Effects` via the Fs `type_defs`. ToCore/FromCore use
/// plain name+arity lookup (unique name), like the bridged records.
#[derive(tidepool_bridge_derive::ToCore, tidepool_bridge_derive::FromCore, Debug)]
struct FileRead {
    path: String,
    contents: Result<String, FsError>,
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

/// Blake3 content hash as a lowercase hex digest — the compare-and-swap token
/// for `FsHash`/`FsWriteCas` (#330). Blake3 matches the cache layer's hash
/// choice (`tidepool-runtime::cache`), so the whole codebase speaks one digest.
pub fn blake3_hex(bytes: &[u8]) -> String {
    blake3::hash(bytes).to_hex().to_string()
}

/// Build the `grepGlob` regex-compile error, always surfacing the underlying
/// regex error and appending the hint that applies to the two common footguns:
/// (a) arg-order — a path glob passed as the (regex, glob) first arg; (b)
/// under-escaping — regex metachars need double-escaping (JSON x Haskell).
/// Loss-less: the real error is always shown, so a heuristic misfire can't hide
/// it. Mirrors the `checked_pattern` diagnose-at-the-boundary precedent.
fn grep_regex_error(regex_str: &str, e: &regex::Error) -> FsError {
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
    FsError::FsBadRegex(msg)
}

/// Expand a glob pattern relative to `root` with sandbox and component filtering.
///
/// Used by [`FsHandler`] (glob/grep) for `**`-normalisation, sandbox check,
/// and hidden-dir filter. Walks via [`ignore::WalkBuilder`] (#343) so
/// gitignored and always-heavy (`target`/`.git`/`node_modules`/
/// `dist-newstyle`) directories are pruned DURING traversal — never
/// descended into — rather than filtered out of the results afterward.
pub fn expand_glob(root: &std::path::Path, pattern: &str) -> Result<Vec<PathBuf>, FsError> {
    // ONE shared empty-glob guard (#328): `glob`/`readGlob`/`grepGlob` all
    // resolve through here, so rejecting `""` in one place covers all three. An
    // empty pattern used to resolve to the sandbox root and expand to `**/*` —
    // matching EVERYTHING (once detonated a readGlob into binaries). It is now a
    // loud typed failure (`FsSandbox`) naming the fix.
    if pattern.is_empty() {
        return Err(FsError::FsSandbox(
            "empty glob pattern matches EVERYTHING — this is a footgun (it once \
             read the whole tree into binaries). Pass an explicit pattern, e.g. \
             \"**/*.hs\" for a filetype or \".\" for the entire tree."
                .to_string(),
        ));
    }
    if pattern.contains("..") {
        return Ok(Vec::new());
    }
    if pattern.starts_with('/') || pattern.starts_with('\\') {
        return Err(FsError::FsSandbox(
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
            normalized = if pattern == "." {
                "**/*".to_string()
            } else {
                format!("{}/**/*", pattern.trim_end_matches('/'))
            };
            normalized.as_str()
        } else if target.exists() {
            pattern
        } else {
            return Err(FsError::FsNotFound(pattern.to_string()));
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
        .map_err(|e| FsError::FsIo(e.to_string()))?;
    let glob_pattern = glob::Pattern::new(&full_pattern)
        .map_err(|e| FsError::FsIo(format!("invalid glob: {}", e)))?;
    // Matches what `glob::glob`/`glob_with` always uses internally
    // (`require_literal_separator` is forced `true` regardless of the
    // options passed to it) — preserves the exact `**`/`*`/`?` semantics
    // callers already rely on.
    let match_options = glob::MatchOptions {
        case_sensitive: true,
        require_literal_separator: true,
        require_literal_leading_dot: false,
    };

    // Prune gitignored + always-heavy dirs DURING the walk (#343): filtering
    // `glob::glob`'s RESULTS can't stop `glob()` from descending into e.g. a
    // multi-hundred-GB `target/` to produce them in the first place — that
    // descent IS the wedge. `ignore::WalkBuilder` walks the tree itself and
    // never recurses past an entry `filter_entry` rejects, so a heavy/
    // gitignored dir is skipped, not merely discarded afterward. Matching
    // against the glob pattern is done separately (`glob::Pattern`) to keep
    // exact pattern semantics; the walk only decides what's traversed.
    let pattern_owned = pattern.to_string();
    let root_owned = root.to_path_buf();
    let mut builder = ignore::WalkBuilder::new(root);
    builder
        // Hidden-dir pruning is handled by `component_filter` below
        // (mention-aware — e.g. `.tidepool/lib/*.hs` still traverses
        // `.tidepool`), so disable the builtin blanket hidden-file skip.
        .hidden(false)
        .git_ignore(true)
        .git_exclude(true)
        .git_global(false)
        // `.gitignore` applies whether or not `root` sits inside a real git
        // checkout (e.g. a temp-dir test, or a sandboxed session root).
        .require_git(false)
        .filter_entry(move |entry| {
            if entry.depth() == 0 {
                return true;
            }
            let rel_path = entry
                .path()
                .strip_prefix(&root_owned)
                .unwrap_or(entry.path());
            component_filter(&pattern_owned, rel_path)
        });

    let paths: Vec<PathBuf> = {
        let mut paths: Vec<PathBuf> = builder
            .build()
            .filter_map(std::result::Result::ok)
            .map(ignore::DirEntry::into_path)
            .filter(|p| {
                p.canonicalize()
                    .map(|cp| cp.starts_with(&canonical_root))
                    .unwrap_or(false)
            })
            .filter(|p| glob_pattern.matches_path_with(p, match_options))
            .collect();
        paths.sort();
        paths
    };
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

    pub fn expand_glob(&self, pattern: &str) -> Result<Vec<PathBuf>, FsError> {
        expand_glob(&self.root, pattern)
    }

    pub fn resolve(&self, path: &str) -> Result<PathBuf, FsError> {
        let resolved = self.root.join(path);
        let canonical_root = self
            .root
            .canonicalize()
            .map_err(|e| FsError::FsIo(e.to_string()))?;
        let check_path = if resolved.exists() {
            resolved
                .canonicalize()
                .map_err(|e| FsError::FsIo(e.to_string()))?
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
                .map_err(|e| FsError::FsIo(e.to_string()))?;
            suffix
                .iter()
                .rev()
                .fold(canonical_ancestor, |p, c| p.join(c))
        };
        if !check_path.starts_with(&canonical_root) {
            return Err(FsError::FsSandbox(format!(
                "path escape: {} is outside sandbox",
                path
            )));
        }
        Ok(check_path)
    }

    fn read_core(&self, path: &str) -> Result<String, FsError> {
        let resolved = self.resolve(path)?;
        std::fs::read_to_string(&resolved).map_err(|e| match e.kind() {
            std::io::ErrorKind::NotFound => FsError::FsNotFound(path.to_string()),
            std::io::ErrorKind::InvalidData => FsError::FsNotUtf8(path.to_string()),
            _ => FsError::FsIo(format!("read '{}' failed: {}", path, e)),
        })
    }
}

/// Human-readable render of a typed Fs failure. The `Left` payload the eval
/// pattern-matches is the primary signal; this Display is what `liftEither`
/// shows on abort and what an untagged verb forwards via [`fs_err_to_effect`].
impl std::fmt::Display for FsError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            FsError::FsNotFound(p) => write!(f, "no such file or directory: {p}"),
            FsError::FsNotUtf8(p) => write!(f, "{p}: not valid UTF-8"),
            FsError::FsSandbox(d) | FsError::FsBadRegex(d) | FsError::FsIo(d) => write!(f, "{d}"),
        }
    }
}

/// Forward a typed `FsError` to the eval-abort channel, for the untagged verbs
/// (`FsMetadata`/`FsReadGlob`/`FsWriteCas`) whose method still returns
/// `Result<Response, EffectError>`. Their shared-helper (`resolve`/`expand_glob`)
/// failures are genuine aborts (a sandbox escape is not per-item data).
fn fs_err_to_effect(e: FsError) -> EffectError {
    EffectError::Handler(e.to_string())
}

impl FsHandler {
    // Errors-tagged verbs: total in `FsError`, no `cx` — the dispatch arm wraps
    // the `Result` via `cx.respond` (Ok→Right, Err→Left). See #335.
    fn fs_read(&mut self, path: String) -> Result<String, FsError> {
        self.read_core(&path)
    }

    fn fs_write(&mut self, path: String, contents: String) -> Result<(), FsError> {
        let resolved = self.resolve(&path)?;
        if let Some(parent) = resolved.parent() {
            std::fs::create_dir_all(parent).map_err(|e| FsError::FsIo(e.to_string()))?;
        }
        std::fs::write(&resolved, &contents).map_err(|e| FsError::FsIo(e.to_string()))?;
        Ok(())
    }

    fn fs_list_dir(&mut self, path: String) -> Result<Vec<String>, FsError> {
        let resolved = self.resolve(&path)?;
        let mut entries: Vec<String> = std::fs::read_dir(&resolved)
            .map_err(|e| match e.kind() {
                std::io::ErrorKind::NotFound => FsError::FsNotFound(path.clone()),
                _ => FsError::FsIo(e.to_string()),
            })?
            .filter_map(std::result::Result::ok)
            .map(|e| e.file_name().to_string_lossy().to_string())
            .collect();
        entries.sort();
        Ok(entries)
    }

    fn fs_glob(&mut self, pattern: String) -> Result<Vec<String>, FsError> {
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
        Ok(rel_paths)
    }

    fn fs_grep(
        &mut self,
        pattern: String,
        file_glob: String,
    ) -> Result<Vec<(String, i64, String)>, FsError> {
        let re = regex::Regex::new(&pattern).map_err(|e| grep_regex_error(&pattern, &e))?;
        let paths = self.expand_glob(&file_glob)?;
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

        Ok(results)
    }

    fn fs_exists(&mut self, path: String) -> Result<bool, FsError> {
        let resolved = self.resolve(&path)?;
        Ok(resolved.exists())
    }

    fn fs_metadata(
        &mut self,
        cx: &EffectContext<'_, CapturedOutput>,
        path: String,
    ) -> Result<tidepool_effect::Response, EffectError> {
        let resolved = self.resolve(&path).map_err(fs_err_to_effect)?;
        match std::fs::metadata(&resolved) {
            Ok(meta) => cx.respond(serde_json::json!({
                "size": meta.len() as i64,
                "is_file": meta.is_file(),
                "is_dir": meta.is_dir(),
            })),
            Err(_) => cx.respond(serde_json::Value::Null),
        }
    }

    fn fs_read_glob(
        &mut self,
        cx: &EffectContext<'_, CapturedOutput>,
        pattern: String,
    ) -> Result<tidepool_effect::Response, EffectError> {
        // Per-file failure isolation (#328): a mixed glob (readable
        // text + a binary/non-UTF-8 file) yields one entry per file —
        // `Right content` on a clean UTF-8 read, `Left err` on failure —
        // instead of failing the whole batch. The empty-glob guard in
        // `expand_glob` covers `""` here too. Contract mirrors #335's
        // per-item typed-failure surface: `[(path, Either err text)]`.
        let paths = self.expand_glob(&pattern).map_err(fs_err_to_effect)?;
        let results: Vec<FileRead> = paths
            .into_iter()
            .filter(|p| p.is_file())
            .map(|p| {
                let rel = p
                    .strip_prefix(&self.root)
                    .unwrap_or(&p)
                    .to_string_lossy()
                    .to_string();
                let contents = std::fs::read_to_string(&p).map_err(|e| match e.kind() {
                    std::io::ErrorKind::InvalidData => FsError::FsNotUtf8(rel.clone()),
                    _ => FsError::FsIo(format!("{rel} failed: {e}")),
                });
                FileRead {
                    path: rel,
                    contents,
                }
            })
            .collect();
        cx.respond_list(results)
    }

    fn fs_hash(&mut self, path: String) -> Result<Option<String>, FsError> {
        // Current blake3 digest, or Nothing if the file is absent — the
        // read half of the CAS loop (#330). Read the hash, compute new
        // content, then `FsWriteCas` back with this as the expectation.
        let resolved = self.resolve(&path)?;
        let hash: Option<String> = match std::fs::read(&resolved) {
            Ok(bytes) => Some(blake3_hex(&bytes)),
            Err(_) => None,
        };
        Ok(hash)
    }

    fn fs_write_cas(
        &mut self,
        cx: &EffectContext<'_, CapturedOutput>,
        path: String,
        expected: Option<String>,
        contents: String,
    ) -> Result<tidepool_effect::Response, EffectError> {
        // Compare-and-swap write (#330): snapshot the current content
        // hash (None = absent), and write ONLY if it equals `expected`
        // (None expected = require the file absent, i.e. create-only).
        // The compare-and-write is one handler call, so the lost-update
        // race between parallel agents shrinks from an agent's
        // think-time to a few syscalls. On a precondition miss nothing
        // is written and the ACTUAL hash comes back as `Left actual`
        // (conflicts-as-data, matching Diff/Edit philosophy + #335).
        let resolved = self.resolve(&path).map_err(fs_err_to_effect)?;
        let actual: Option<String> = match std::fs::read(&resolved) {
            Ok(bytes) => Some(blake3_hex(&bytes)),
            Err(_) => None,
        };
        if actual == expected {
            if let Some(parent) = resolved.parent() {
                std::fs::create_dir_all(parent).map_err(|e| EffectError::Handler(e.to_string()))?;
            }
            std::fs::write(&resolved, &contents)
                .map_err(|e| EffectError::Handler(e.to_string()))?;
            cx.respond(Ok::<(), Option<String>>(()))
        } else {
            cx.respond(Err::<(), Option<String>>(actual))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::*;
    use tidepool_bridge::{FromCore, ToCore};
    use tidepool_effect::dispatch::EffectHandler;
    use tidepool_effect::dispatch::{DispatchEffect, EffectContext};
    use tidepool_eval::value::Value;
    use tidepool_repr::DataConTable;

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

    /// #343: a broad glob must not walk gitignored scratch dirs (a real
    /// tester's `**/*.rs` picked up a gitignored copy that `rg` skipped) NOR
    /// the always-heavy dirs, while still returning real source files.
    #[test]
    fn test_expand_glob_respects_gitignore_and_heavy_dirs() {
        use tempfile::tempdir;
        let dir = tempdir().unwrap();
        let root = dir.path().to_path_buf();

        std::fs::write(root.join(".gitignore"), "scratch/\n").unwrap();

        std::fs::create_dir_all(root.join("src")).unwrap();
        std::fs::write(root.join("src/lib.rs"), "fn main() {}").unwrap();

        std::fs::create_dir_all(root.join("scratch")).unwrap();
        std::fs::write(root.join("scratch/junk.rs"), "junk").unwrap();

        std::fs::create_dir_all(root.join("target/debug")).unwrap();
        std::fs::write(root.join("target/debug/build.rs"), "junk").unwrap();

        let handler = FsHandler::new(root.clone());
        let names: Vec<String> = handler
            .expand_glob("**/*.rs")
            .unwrap()
            .iter()
            .filter_map(|p| p.strip_prefix(&root).ok())
            .map(|p| p.to_string_lossy().to_string())
            .collect();

        assert!(names.iter().any(|n| n.ends_with("lib.rs")), "{names:?}");
        assert!(!names.iter().any(|n| n.contains("scratch")), "{names:?}");
        assert!(!names.iter().any(|n| n.contains("target")), "{names:?}");
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
    fn test_tryreadglob_mixed_binary_isolates_per_file() {
        use tempfile::tempdir;
        let dir = tempdir().unwrap();
        let root = dir.path().to_path_buf();
        std::fs::write(root.join("good.txt"), "hello\nworld").unwrap();
        // Invalid UTF-8 — the mixed-glob case #328 is about: one binary swept up
        // by a wide glob must not fail the whole batch.
        std::fs::write(root.join("bad.bin"), vec![0xff, 0xfe, 0x00, 0x01]).unwrap();

        let mut handler = FsHandler::new(root.clone());
        let table = full_effect_test_table();
        let captured = CapturedOutput::new();
        let cx = EffectContext::with_user(&table, &captured);

        let req = FsReq::FsReadGlob("*".to_string());
        let res = response_value(handler.handle(req, &cx).unwrap(), &table);
        let mut results: Vec<FileRead> = FromCore::from_value(&res, &table).unwrap();
        results.sort_by(|a, b| a.path.cmp(&b.path));
        assert_eq!(results.len(), 2, "{results:?}");
        // bad.bin -> contents Left err (isolated), good.txt -> Right (survives).
        assert_eq!(results[0].path, "bad.bin");
        assert!(
            results[0].contents.is_err(),
            "binary must be Left: {results:?}"
        );
        assert_eq!(results[1].path, "good.txt");
        assert_eq!(results[1].contents.as_deref(), Ok("hello\nworld"));
    }

    #[test]
    fn test_empty_glob_is_loud_on_all_four_verbs() {
        use tempfile::tempdir;
        let dir = tempdir().unwrap();
        let root = dir.path().to_path_buf();
        std::fs::write(root.join("a.txt"), "x").unwrap();

        let mut handler = FsHandler::new(root.clone());
        let table = full_effect_test_table();
        let captured = CapturedOutput::new();
        let cx = EffectContext::with_user(&table, &captured);

        // ONE shared guard in expand_glob → every glob-resolving verb inherits
        // it. Empty pattern used to expand to the whole tree.
        let err = handler.expand_glob("").unwrap_err();
        assert!(format!("{err}").contains("matches EVERYTHING"), "{err}");

        // The errors-tagged glob verbs now return the guard as DATA — a
        // `Left (FsSandbox _)` — not an abort (#335). FsReadGlob is untagged, so
        // its verb-level guard still aborts (per-item Eithers ride the list).
        for req in [
            FsReq::FsGlob(String::new()),
            FsReq::FsGrep("x".to_string(), String::new()),
        ] {
            let res = response_value(handler.handle(req, &cx).unwrap(), &table);
            let decoded: Result<Value, FsError> = FromCore::from_value(&res, &table).unwrap();
            match decoded {
                Err(FsError::FsSandbox(d)) => assert!(
                    d.contains("matches EVERYTHING"),
                    "empty glob should be loud, got: {d}"
                ),
                other => panic!("expected Left (FsSandbox _), got {other:?}"),
            }
        }

        let e = handler
            .handle(FsReq::FsReadGlob(String::new()), &cx)
            .unwrap_err();
        assert!(
            format!("{e}").contains("matches EVERYTHING"),
            "empty readGlob should still abort loudly, got: {e}"
        );
    }

    #[test]
    fn test_write_cas_hit_miss_and_hash_getter() {
        use tempfile::tempdir;
        let dir = tempdir().unwrap();
        let root = dir.path().to_path_buf();
        let mut handler = FsHandler::new(root.clone());
        let table = full_effect_test_table();
        let captured = CapturedOutput::new();
        let cx = EffectContext::with_user(&table, &captured);

        let decode =
            |r: tidepool_effect::Response, t: &DataConTable| -> Result<(), Option<String>> {
                FromCore::from_value(&response_value(r, t), t).unwrap()
            };

        // create-only (expected = None): file absent → writes.
        let req = FsReq::FsWriteCas("f.txt".to_string(), None, "v1".to_string());
        assert_eq!(decode(handler.handle(req, &cx).unwrap(), &table), Ok(()));
        assert_eq!(std::fs::read_to_string(root.join("f.txt")).unwrap(), "v1");

        // fileHash (FsHash): current digest of an existing file. Now errors-
        // tagged, so the digest arrives as `Right (Just hash)`.
        let req = FsReq::FsHash("f.txt".to_string());
        let res = response_value(handler.handle(req, &cx).unwrap(), &table);
        let h: Result<Option<String>, FsError> = FromCore::from_value(&res, &table).unwrap();
        let h = h.unwrap().expect("hash of an existing file");
        assert_eq!(h, blake3_hex(b"v1"));

        // FsHash on an absent file → Right Nothing (absence is data, not error).
        let req = FsReq::FsHash("missing.txt".to_string());
        let res = response_value(handler.handle(req, &cx).unwrap(), &table);
        let none: Result<Option<String>, FsError> = FromCore::from_value(&res, &table).unwrap();
        assert_eq!(none.unwrap(), None);

        // CAS HIT: expected == current hash → writes v2.
        let req = FsReq::FsWriteCas("f.txt".to_string(), Some(h.clone()), "v2".to_string());
        assert_eq!(decode(handler.handle(req, &cx).unwrap(), &table), Ok(()));
        assert_eq!(std::fs::read_to_string(root.join("f.txt")).unwrap(), "v2");

        // CAS MISS: stale expected hash (of v1) → Left(actual = hash of v2), no write.
        let req = FsReq::FsWriteCas("f.txt".to_string(), Some(h), "v3".to_string());
        assert_eq!(
            decode(handler.handle(req, &cx).unwrap(), &table),
            Err(Some(blake3_hex(b"v2"))),
            "conflict must carry the ACTUAL hash"
        );
        assert_eq!(
            std::fs::read_to_string(root.join("f.txt")).unwrap(),
            "v2",
            "a failed CAS must write NOTHING"
        );

        // create-only MISS: expected None but file exists → Left(actual).
        let req = FsReq::FsWriteCas("f.txt".to_string(), None, "v4".to_string());
        assert_eq!(
            decode(handler.handle(req, &cx).unwrap(), &table),
            Err(Some(blake3_hex(b"v2")))
        );
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

        let req = FsReq::FsGrep("hello".to_string(), "**/*.txt".to_string());
        let res = response_value(handler.handle(req, &cx).unwrap(), &table);
        let decoded: Result<Vec<(String, i64, String)>, FsError> =
            FromCore::from_value(&res, &table).unwrap();
        let results = decoded.unwrap();
        assert_eq!(results.len(), 2);
        assert_eq!(
            results[0],
            ("test.txt".to_string(), 1, "hello world".to_string())
        );
        assert_eq!(
            results[1],
            ("test.txt".to_string(), 3, "hello rust".to_string())
        );

        let req = FsReq::FsGrep("hello".to_string(), "**/*".to_string());
        let res = response_value(handler.handle(req, &cx).unwrap(), &table);
        let decoded: Result<Vec<(String, i64, String)>, FsError> =
            FromCore::from_value(&res, &table).unwrap();
        assert_eq!(decoded.unwrap().len(), 2);
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

        let req = FsReq::FsGrep("match".to_string(), "large.txt".to_string());
        let res = response_value(handler.handle(req, &cx).unwrap(), &table);
        let decoded: Result<Vec<(String, i64, String)>, FsError> =
            FromCore::from_value(&res, &table).unwrap();
        let results = decoded.unwrap();

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
    fn test_fs_from_core_exists() {
        let table = full_effect_test_table();
        let con_id = table.get_by_name("FsExists").unwrap();
        let path = "Cargo.toml".to_string().to_value(&table).unwrap();
        let val = Value::Con(con_id, vec![path]);
        let req = FsReq::from_value(&val, &table).unwrap();
        assert!(matches!(req, FsReq::FsExists(ref p) if p == "Cargo.toml"));
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
        // FsExists is errors-tagged: `Right True` for an existing path.
        let decoded: Result<bool, FsError> = FromCore::from_value(&result, &table).unwrap();
        assert_eq!(decoded, Ok(true), "Cargo.toml should exist");
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
        // FsListDir is errors-tagged: `Right [entries]`.
        let decoded: Result<Vec<String>, FsError> = FromCore::from_value(&result, &table).unwrap();
        assert!(
            !decoded.unwrap().is_empty(),
            "repo root should have entries"
        );
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
        let req = FsReq::FsWrite("a/b/c.txt".into(), "hello mkdir-p".into());
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
        // Attempt to escape via `..` into a sibling directory that doesn't exist.
        // FsWrite is errors-tagged: the escape now comes back as `Left
        // (FsSandbox _)` DATA, not an abort (#335).
        let req = FsReq::FsWrite("../../escape/evil.txt".into(), "bad".into());
        let res = response_value(handler.handle(req, &cx).unwrap(), &table);
        let decoded: Result<(), FsError> = FromCore::from_value(&res, &table).unwrap();
        match decoded {
            Err(FsError::FsSandbox(msg)) => {
                assert!(
                    msg.contains("outside sandbox") || msg.contains("escape"),
                    "{msg}"
                );
            }
            other => panic!("expected Left (FsSandbox _), got {other:?}"),
        }
    }

    /// #335 end-to-end through the REAL pipeline (extract → generated
    /// `Tidepool.Effects` with `data FsError` → JIT → Either): a missing-file
    /// `readFile` is a typed `Left (FsNotFound _)` the eval pattern-matches in
    /// Haskell — never an abort. This is the acceptance proof that the whole
    /// errors-block mechanism composes.
    #[tokio::test]
    async fn fs_read_missing_file_is_typed_left_fsnotfound() {
        let v = jit_eval(&[
            "r <- readFile \"definitely-not-a-real-file-xyz-335.txt\"",
            "pure (case r of { Left (FsNotFound _) -> (\"notfound\" :: Text); Left _ -> \"other\"; Right _ -> \"ok\" })",
        ]);
        assert_eq!(v, serde_json::json!("notfound"));
    }

    /// The happy path still threads through the Either: an existing read is a
    /// `Right _`, so `readFile p >>= liftEither` (the natural unwrap) yields the
    /// content.
    #[tokio::test]
    async fn fs_read_existing_file_is_right() {
        let v = jit_eval(&[
            "src <- readFile \"Cargo.toml\" >>= liftEither",
            "pure (T.length src > 0)",
        ]);
        assert_eq!(v, serde_json::json!(true));
    }

    /// #328/#335 acceptance: `readGlob` over a mixed glob (one clean UTF-8 text
    /// file, one invalid-UTF-8 binary file) through the REAL extract → JIT
    /// pipeline, rooted at a temp dir (not the repo root, so `jit_eval` doesn't
    /// fit — `EvalHarness` is). `partitionEithers (map (.contents) rs)` must
    /// split the per-file outcomes: the binary isolates as `Left`, the text
    /// file survives as `Right`, and neither poisons the batch.
    #[tokio::test]
    async fn fs_read_glob_mixed_binary_partitions_via_partition_eithers() {
        use tempfile::tempdir;
        use tidepool_testing::eval_harness::EvalHarness;

        let dir = tempdir().unwrap();
        let root = dir.path().to_path_buf();
        std::fs::write(root.join("good.txt"), "hello").unwrap();
        std::fs::write(root.join("bad.bin"), vec![0xff, 0xfe, 0x00, 0x01]).unwrap();

        let decls = tidepool_mcp::standard_decls();
        let preamble = tidepool_mcp::build_preamble(&decls, false);
        let stack = tidepool_mcp::build_effect_stack_type(&decls);
        let code = tidepool_mcp::wrap_do(concat!(
            "rs <- readGlob \"*\"\n",
            "let (bad, good) = partitionEithers (map (.contents) rs)\n",
            "pure (object [\"goodCount\" .= length good, \"badCount\" .= length bad, \"goodText\" .= good])",
        ));
        let source = tidepool_mcp::template_haskell(&preamble, &stack, &code, "", "", None, None);

        // Only Fs is exercised (readGlob), so the handler HList only needs to
        // cover tags 0..2 (Console, KV, Fs) — dispatch never recurses past Fs.
        let kv_path = std::env::temp_dir().join("tidepool_fs_readglob_partition_test_kv.json");
        let handlers = frunk::hlist![
            crate::ConsoleHandler,
            crate::KvHandler::new(kv_path),
            FsHandler::new(root),
        ];

        let harness = EvalHarness::new().with_stdlib().with_effects_module();
        let out = harness.run_with(&source, "result", handlers, CapturedOutput::new());
        assert_eq!(
            out.json(),
            serde_json::json!({"goodCount": 1, "badCount": 1, "goodText": ["hello"]}),
            "{:?}",
            out.err()
        );
    }
}
