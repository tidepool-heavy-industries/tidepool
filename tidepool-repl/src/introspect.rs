//! `:i <Name>` resolution for stdlib/preamble types — the source-scan lane.
//!
//! The repl's `:i` used to see only (1) session value bindings, (2) built-in
//! effect decl `type_defs`, and (3) session-declared types — so the types the
//! preamble puts in scope (`Proc`, `Hit`, `Schema`, … from
//! `haskell/lib/Tidepool/*.hs`, re-exported by `Tidepool.Prelude`, plus
//! project/global `.tidepool/lib` verb-module types) answered
//! `not a bound value or known type` — a dead end exactly where a caller is
//! trying to repair a type error.
//!
//! The extract binary owns GHC-side type knowledge but has no info-dump flag
//! (and is outside this crate's boundary), so this lane scans the SOURCES the
//! session compiles against: every `.hs` file under the session's
//! `base_include` dirs (generated `Tidepool.Effects`, the stdlib, project +
//! global verb libs — the exact GHC include path, so scan hits are by
//! construction in scope).

use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};

/// Resolve a type/class/constructor name against the include-dir sources.
///
/// Returns `Some` JSON on a hit:
/// - Type/class head (`data|newtype|type|class <name>` at line start, module
///   scope): `{"name", "shape": <the full declaration text, continuation
///   lines included, `deriving`/`where` clause and all>, "module": <the
///   module name from the file header>, "file": <path>, "source": "stdlib"}`.
///   The shape for `Proc` must read
///   `data Proc = Proc { exitCode :: Int, stdout :: Text, stderr :: Text } …`
///   — fields WITH types, verbatim from the source.
/// - Constructor that is not also a type head (e.g. a variant of a sum type):
///   the ENCLOSING data declaration, same shape, plus `"constructor": <name>`.
/// - Ambiguous: more than one include dir has a DISTINCT-file hit for `name`
///   (e.g. stdlib's `Tidepool/Patch.hs` and a project `.tidepool/lib/Edit.hs`
///   both declare `data Edit`). Rather than silently returning the first
///   dir's hit (masking the collision), returns
///   `{"name", "ambiguous": [{"module","file","source":"stdlib"}, …]}` in
///   `include_dirs` order.
///
/// `None` on a miss (the caller falls through to its total-miss error).
///
/// Scan rules:
/// - Walk each dir in `include_dirs` recursively for `*.hs` (these dirs are
///   small — stdlib + verb libs; no depth/size guard needed beyond symlink
///   sanity).
/// - A declaration ends where the next line is blank or starts at column 0
///   (standard layout — continuation lines are indented).
/// - ALL dirs are scanned (not first-hit-wins): a single hit renders the
///   usual shape; two or more DISTINCT-file hits render the ambiguity
///   response instead, in `include_dirs` order (matches GHC include-path
///   precedence: effects dir, stdlib, project lib, global lib).
pub fn stdlib_info(include_dirs: &[PathBuf], name: &str) -> Option<serde_json::Value> {
    if name.is_empty() || !name.starts_with(|c: char| c.is_uppercase()) {
        return None; // types/classes/constructors are always uppercase
    }
    let mut visited = HashSet::new();
    walk_and_classify(include_dirs, name, |dir, hits, seen_files| {
        if let Some(hit) = scan_dir(dir, name, &mut visited) {
            if let Some(file) = hit.get("file").and_then(|f| f.as_str()) {
                if seen_files.insert(file.to_string()) {
                    hits.push(hit);
                }
            }
        }
    })
}

/// Resolve a lowercase VALUE/function name (`findDef`, not a type/class/
/// constructor) against the include-dir sources' top-level signatures —
/// the `:i findDef` lane. Reuses `tidepool_mcp::extract_sigs`, the same
/// lowercase-signature scanner `:vocab` already relies on
/// (`tidepool-mcp/src/preamble.rs`), so a name `:vocab` lists is guaranteed
/// resolvable here too.
///
/// Returns the same `{"name","shape","module","file","source":"stdlib"}`
/// shape as a type hit (`shape` is the joined signature text, e.g.
/// `findDef :: Text -> Text -> M LspNode`). `None` on a miss.
///
/// Like `stdlib_info`, scans every dir (not first-hit-wins) and reports an
/// ambiguity if the same name has a signature in more than one distinct
/// file: `{"name","ambiguous":[{...}, ...]}`.
pub fn stdlib_value_info(include_dirs: &[PathBuf], name: &str) -> Option<serde_json::Value> {
    if name.is_empty() || !name.starts_with(|c: char| c.is_lowercase() || c == '_') {
        return None; // values/functions are always lowercase-leading
    }
    walk_and_classify(include_dirs, name, |dir, hits, seen_files| {
        collect_value_hits(dir, name, hits, seen_files);
    })
}

/// Shared walk-and-filter tail of [`stdlib_info`] / [`stdlib_value_info`]:
/// runs `scan_one_dir` over every dir in `include_dirs` (accumulating hits +
/// the file-path dedup set it threads through), then classifies the result —
/// `None` (no hits), `Some(hit)` (exactly one), or the ambiguity-report shape
/// (two or more distinct-file hits, in `include_dirs` order — matches GHC
/// include-path precedence). `scan_one_dir` owns the per-dir scan STRATEGY
/// (`stdlib_info`'s `scan_dir` returns at most one hit per dir tree;
/// `stdlib_value_info`'s `collect_value_hits` collects every file's hit) — only
/// the accumulate-then-classify shell is shared.
fn walk_and_classify(
    include_dirs: &[PathBuf],
    name: &str,
    mut scan_one_dir: impl FnMut(&Path, &mut Vec<serde_json::Value>, &mut HashSet<String>),
) -> Option<serde_json::Value> {
    let mut hits: Vec<serde_json::Value> = Vec::new();
    let mut seen_files = HashSet::new();
    for dir in include_dirs {
        scan_one_dir(dir, &mut hits, &mut seen_files);
    }
    match hits.len() {
        0 => None,
        1 => hits.pop(),
        _ => Some(serde_json::json!({
            "name": name,
            "ambiguous": hits
                .into_iter()
                .map(|h| serde_json::json!({
                    "module": h.get("module").cloned().unwrap_or(serde_json::Value::Null),
                    "file": h.get("file").cloned().unwrap_or(serde_json::Value::Null),
                    "source": h.get("source").cloned().unwrap_or(serde_json::Value::Null),
                }))
                .collect::<Vec<_>>(),
        })),
    }
}

/// Recursively walk `dir` for `*.hs` files, appending a value-signature hit
/// for `name` (if any, first match within a file wins) to `hits`, deduped by
/// canonical file path via `seen_files`.
fn collect_value_hits(
    dir: &Path,
    name: &str,
    hits: &mut Vec<serde_json::Value>,
    seen_files: &mut HashSet<String>,
) {
    let Ok(rd) = fs::read_dir(dir) else { return };
    let mut entries: Vec<PathBuf> = rd.flatten().map(|e| e.path()).collect();
    entries.sort();
    for p in &entries {
        if p.is_file() && p.extension().and_then(|e| e.to_str()) == Some("hs") {
            if let Some(hit) = scan_file_for_value(p, name) {
                if let Some(file) = hit.get("file").and_then(|f| f.as_str()) {
                    if seen_files.insert(file.to_string()) {
                        hits.push(hit);
                    }
                }
            }
        }
    }
    for p in &entries {
        if p.is_dir() {
            collect_value_hits(p, name, hits, seen_files);
        }
    }
}

/// Scan one source file's top-level signatures (via `extract_sigs`) for a
/// signature whose head identifier matches `name`.
fn scan_file_for_value(path: &Path, name: &str) -> Option<serde_json::Value> {
    let src = fs::read_to_string(path).ok()?;
    let sigs = tidepool_mcp::extract_sigs(&src);
    let sig = sigs.into_iter().find(|s| {
        s.split_once("::")
            .map(|(head, _)| head.trim() == name)
            .unwrap_or(false)
    })?;
    Some(serde_json::json!({
        "name": name,
        "shape": sig,
        "module": module_name(&src),
        "file": path.display().to_string(),
        "source": "stdlib",
    }))
}

/// Recursively scan one dir for `.hs` files, files before subdirs, both in
/// name order (deterministic). Recursion is keyed on the canonical path so a
/// symlink cycle terminates instead of looping.
fn scan_dir(dir: &Path, name: &str, visited: &mut HashSet<PathBuf>) -> Option<serde_json::Value> {
    let canon = fs::canonicalize(dir).ok()?;
    if !visited.insert(canon) {
        return None;
    }
    let mut entries: Vec<PathBuf> = fs::read_dir(dir)
        .ok()?
        .flatten()
        .map(|e| e.path())
        .collect();
    entries.sort();
    entries
        .iter()
        .filter(|p| p.is_file() && p.extension().and_then(|e| e.to_str()) == Some("hs"))
        .find_map(|p| scan_file(p, name))
        .or_else(|| {
            entries
                .iter()
                .filter(|p| p.is_dir())
                .find_map(|p| scan_dir(p, name, visited))
        })
}

/// Scan one source file. A decl-head match (`data|newtype|type|class <name>`)
/// wins over a constructor-only match in the same file; the constructor hit is
/// kept as the fallback (first enclosing block in line order).
fn scan_file(path: &Path, name: &str) -> Option<serde_json::Value> {
    let src = fs::read_to_string(path).ok()?;
    let lines: Vec<&str> = src.lines().collect();
    let mut constructor_hit: Option<serde_json::Value> = None;
    let mut i = 0;
    while i < lines.len() {
        let Some((kw, head)) = decl_head(lines[i]) else {
            i += 1;
            continue;
        };
        let end = block_end(&lines, i);
        let block = &lines[i..end];
        if head == name {
            return Some(render_hit(&src, path, name, block, false));
        }
        if constructor_hit.is_none()
            && matches!(kw, "data" | "newtype")
            && block_has_constructor(block, name)
        {
            constructor_hit = Some(render_hit(&src, path, name, block, true));
        }
        i = end;
    }
    constructor_hit
}

/// End index (exclusive) of the declaration block opened at `start`: continues
/// through indented continuation lines, stops at a blank line or the next
/// column-0 line.
fn block_end(lines: &[&str], start: usize) -> usize {
    let mut end = start + 1;
    while end < lines.len() {
        let l = lines[end];
        if l.trim().is_empty() || !l.starts_with([' ', '\t']) {
            break;
        }
        end += 1;
    }
    end
}

/// Assemble the hit JSON per the module-doc contract.
fn render_hit(
    src: &str,
    path: &Path,
    name: &str,
    block: &[&str],
    constructor: bool,
) -> serde_json::Value {
    let mut v = serde_json::json!({
        "name": name,
        "shape": block.join("\n").trim_end(),
        "module": module_name(src),
        "file": path.display().to_string(),
        "source": "stdlib",
    });
    if constructor {
        v["constructor"] = serde_json::json!(name);
    }
    v
}

/// If a column-0 `line` opens a type/class declaration, return
/// `(keyword, head_name)`. Handles contexts (`class Eq a => Ord a where` —
/// the name follows the last `=>`), `type family F`, and GADT heads
/// (`data T where`); `data instance`/`type instance` reuse an existing head
/// and are skipped.
fn decl_head(line: &str) -> Option<(&'static str, &str)> {
    let (kw, rest) = ["data", "newtype", "type", "class"]
        .into_iter()
        .find_map(|kw| {
            let r = line.strip_prefix(kw)?;
            r.starts_with([' ', '\t']).then_some((kw, r))
        })?;
    // The head is everything before the RHS `=` (standalone, not `=>`/`==`).
    let head = &rest[..standalone_sym_pos(rest.as_bytes(), b'=').unwrap_or(rest.len())];
    let head = head.rsplit("=>").next().unwrap_or(head);
    let mut toks = head.split_whitespace();
    let mut tok = toks.next()?;
    if tok == "family" {
        tok = toks.next()?;
    }
    if tok == "instance" {
        return None;
    }
    let ident = ident_prefix(tok);
    (ident.len() == tok.len() && ident.starts_with(|c: char| c.is_uppercase()))
        .then_some((kw, ident))
}

/// Whether `name` occurs as a constructor inside a data/newtype block: the
/// identifier directly after a standalone `=` or `|` (Haskell-98 variants), or
/// on a GADT-style `C1, C2 :: …` continuation line.
fn block_has_constructor(block: &[&str], name: &str) -> bool {
    for (li, line) in block.iter().enumerate() {
        let bytes = line.as_bytes();
        for i in 0..bytes.len() {
            if (bytes[i] == b'=' || bytes[i] == b'|')
                && is_standalone_at(bytes, i)
                && ident_prefix(line[i + 1..].trim_start()) == name
            {
                return true;
            }
        }
        // GADT constructor signature lines (indented, so li > 0).
        if li > 0 {
            let t = line.trim_start();
            if let Some(pos) = t.find("::") {
                if t[..pos].split(',').map(str::trim).any(|c| c == name) {
                    return true;
                }
            }
        }
    }
    false
}

/// Whether the symbol at byte `i` is standalone — not part of a multi-char
/// operator (`==`, `=>`, `<=`, `||`) or a quasiquote bracket (`[|`, `|]`).
fn is_standalone_at(bytes: &[u8], i: usize) -> bool {
    const OP: &[u8] = b"!#$%&*+./<=>?@\\^|-~:";
    let prev_ok = i == 0 || (!OP.contains(&bytes[i - 1]) && bytes[i - 1] != b'[');
    let next_ok = i + 1 >= bytes.len() || (!OP.contains(&bytes[i + 1]) && bytes[i + 1] != b']');
    prev_ok && next_ok
}

/// Position of the first standalone `sym` in `bytes` (see [`is_standalone_at`]).
fn standalone_sym_pos(bytes: &[u8], sym: u8) -> Option<usize> {
    (0..bytes.len()).find(|&i| bytes[i] == sym && is_standalone_at(bytes, i))
}

/// Leading Haskell-identifier prefix of `s` (alphanumeric / `_` / `'`).
fn ident_prefix(s: &str) -> &str {
    let end = s
        .find(|c: char| !(c.is_alphanumeric() || c == '_' || c == '\''))
        .unwrap_or(s.len());
    &s[..end]
}

/// The module name from the file's `module X.Y.Z` header line (column 0;
/// tolerates both `module X.Y (…) where` and a multi-line export list).
fn module_name(src: &str) -> Option<String> {
    src.lines().find_map(|l| {
        let rest = l.strip_prefix("module")?;
        if !rest.starts_with([' ', '\t']) {
            return None;
        }
        let rest = rest.trim_start();
        let end = rest
            .find(|c: char| !(c.is_alphanumeric() || c == '.' || c == '_' || c == '\''))
            .unwrap_or(rest.len());
        (end > 0).then(|| rest[..end].to_string())
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A temp dir populated with the given (relative path, content) files.
    fn dir_with(files: &[(&str, &str)]) -> tempfile::TempDir {
        let d = tempfile::tempdir().unwrap();
        for (rel, content) in files {
            let p = d.path().join(rel);
            if let Some(parent) = p.parent() {
                fs::create_dir_all(parent).unwrap();
            }
            fs::write(p, content).unwrap();
        }
        d
    }

    fn lookup(dirs: &[&tempfile::TempDir], name: &str) -> Option<serde_json::Value> {
        let dirs: Vec<PathBuf> = dirs.iter().map(|d| d.path().to_path_buf()).collect();
        stdlib_info(&dirs, name)
    }

    /// A synthetic stdlib module exercising every decl shape the scanner must
    /// handle: single-line record, multi-line sum with deriving on its own
    /// line, type alias, class-with-where block, GADT-style data.
    const RECORDS: &str = "\
{-# LANGUAGE NoImplicitPrelude #-}
module Fake.Records
  ( Proc(..)
  ) where

import Prelude (Int, Eq, Show)

-- | A finished subprocess.
data Proc = Proc { exitCode :: Int, stdout :: Text, stderr :: Text } deriving (Show, Eq)

data Outcome
  = Rejected { reason :: Text }
  | NoChange
  deriving (Show, Eq)

type Name = Text

class Pack a where
  pack :: a -> Text
  unpack' :: Text -> a

class Show a => Pretty a where
  pretty :: a -> Text

data Gadt where
  MkGadt :: Int -> Gadt
";

    #[test]
    fn single_line_record_hit() {
        let d = dir_with(&[("Fake/Records.hs", RECORDS)]);
        let v = lookup(&[&d], "Proc").expect("Proc is a head hit");
        assert_eq!(v["name"], "Proc");
        assert_eq!(
            v["shape"],
            "data Proc = Proc { exitCode :: Int, stdout :: Text, stderr :: Text } deriving (Show, Eq)"
        );
        assert_eq!(v["module"], "Fake.Records");
        assert_eq!(v["source"], "stdlib");
        assert!(v.get("constructor").is_none(), "head hit, not constructor");
        assert!(v["file"].as_str().unwrap().ends_with("Fake/Records.hs"));
    }

    #[test]
    fn multi_line_data_with_deriving_on_own_line() {
        let d = dir_with(&[("Fake/Records.hs", RECORDS)]);
        let v = lookup(&[&d], "Outcome").expect("Outcome is a head hit");
        assert_eq!(
            v["shape"],
            "data Outcome\n  = Rejected { reason :: Text }\n  | NoChange\n  deriving (Show, Eq)"
        );
    }

    #[test]
    fn type_alias_hit() {
        let d = dir_with(&[("Fake/Records.hs", RECORDS)]);
        let v = lookup(&[&d], "Name").expect("Name is a type alias hit");
        assert_eq!(v["shape"], "type Name = Text");
    }

    #[test]
    fn class_where_block_hit() {
        let d = dir_with(&[("Fake/Records.hs", RECORDS)]);
        let v = lookup(&[&d], "Pack").expect("Pack is a class hit");
        assert_eq!(
            v["shape"],
            "class Pack a where\n  pack :: a -> Text\n  unpack' :: Text -> a"
        );
        // A superclass context does not confuse the head-name extraction.
        let v = lookup(&[&d], "Pretty").expect("Pretty is a class hit");
        assert_eq!(
            v["shape"],
            "class Show a => Pretty a where\n  pretty :: a -> Text"
        );
    }

    #[test]
    fn constructor_only_hit_returns_enclosing_decl() {
        let d = dir_with(&[("Fake/Records.hs", RECORDS)]);
        // `| NoChange` sum variant.
        let v = lookup(&[&d], "NoChange").expect("NoChange is a constructor hit");
        assert_eq!(v["name"], "NoChange");
        assert_eq!(v["constructor"], "NoChange");
        assert!(
            v["shape"].as_str().unwrap().starts_with("data Outcome"),
            "shape is the enclosing decl: {v}"
        );
        // `= Rejected {…}` first variant.
        let v = lookup(&[&d], "Rejected").expect("Rejected is a constructor hit");
        assert_eq!(v["constructor"], "Rejected");
        // GADT-style `MkGadt :: Int -> Gadt`.
        let v = lookup(&[&d], "MkGadt").expect("MkGadt is a GADT constructor hit");
        assert_eq!(v["constructor"], "MkGadt");
        assert_eq!(v["shape"], "data Gadt where\n  MkGadt :: Int -> Gadt");
    }

    #[test]
    fn miss_returns_none() {
        let d = dir_with(&[("Fake/Records.hs", RECORDS)]);
        assert_eq!(lookup(&[&d], "Nonexistent"), None);
        // Lowercase names (record fields, functions) are never type hits.
        assert_eq!(lookup(&[&d], "exitCode"), None);
        // Word boundary: a prefix of a real head is a miss.
        assert_eq!(lookup(&[&d], "Pro"), None);
        assert_eq!(lookup(&[&d], "Out"), None);
    }

    #[test]
    fn name_is_word_bounded_not_prefix_matched() {
        let d = dir_with(&[(
            "A.hs",
            "module A where\n\ndata ProcSpec = ProcSpec { cmd :: Text }\n",
        )]);
        assert_eq!(lookup(&[&d], "Proc"), None, "Proc must not match ProcSpec");
    }

    #[test]
    fn include_dir_collision_reports_ambiguous_in_dir_order() {
        // Two distinct files across two include dirs both declare `Foo` —
        // this used to silently return the first dir's hit (masking the
        // collision); it must now report ambiguity, in `include_dirs` order.
        let d1 = dir_with(&[("A.hs", "module A where\n\ndata Foo = FooA Int\n")]);
        let d2 = dir_with(&[("B.hs", "module B where\n\ndata Foo = FooB Int\n")]);
        let v = lookup(&[&d1, &d2], "Foo").unwrap();
        assert!(
            v.get("ambiguous").is_some(),
            "collision must be reported: {v}"
        );
        let ambiguous = v["ambiguous"].as_array().unwrap();
        assert_eq!(ambiguous.len(), 2);
        assert_eq!(ambiguous[0]["module"], "A", "dir order preserved");
        assert_eq!(ambiguous[1]["module"], "B");

        let v = lookup(&[&d2, &d1], "Foo").unwrap();
        let ambiguous = v["ambiguous"].as_array().unwrap();
        assert_eq!(ambiguous[0]["module"], "B", "order reversed → B first");
        assert_eq!(ambiguous[1]["module"], "A");
    }

    #[test]
    fn single_dir_hit_is_unaffected_by_ambiguity_handling() {
        // The non-colliding case (only one dir has the name) must still
        // return the plain single-hit shape, not an ambiguity wrapper.
        let d1 = dir_with(&[("A.hs", "module A where\n\ndata Foo = FooA Int\n")]);
        let d2 = dir_with(&[("B.hs", "module B where\n\ndata Bar = BarB Int\n")]);
        let v = lookup(&[&d1, &d2], "Foo").unwrap();
        assert_eq!(v["module"], "A");
        assert!(v.get("ambiguous").is_none());
    }

    #[test]
    fn adjacent_column_zero_decl_ends_block() {
        let d = dir_with(&[("A.hs", "module A where\n\ndata A = A Int\ndata B = B Int\n")]);
        let v = lookup(&[&d], "A").unwrap();
        assert_eq!(v["shape"], "data A = A Int", "next col-0 line ends block");
    }

    #[test]
    fn non_hs_files_skipped() {
        let d = dir_with(&[("notes.txt", "module N where\n\ndata Zed = Zed\n")]);
        assert_eq!(lookup(&[&d], "Zed"), None);
    }

    #[test]
    fn instance_and_function_lines_are_not_heads() {
        let d = dir_with(&[(
            "A.hs",
            "module A where\n\ndata instance Fam Int = FamInt\ntype instance F Int = Int\n\nclassify :: Int -> Int\nclassify x = x\n",
        )]);
        assert_eq!(lookup(&[&d], "Fam"), None, "data instance is not a head");
        // `classify` starts with the letters of `class` but not the keyword.
        assert_eq!(lookup(&[&d], "Int"), None);
    }

    #[test]
    fn value_signature_hit() {
        let d = dir_with(&[(
            "Lsp.hs",
            "module Lsp where\n\nfindDef :: Text -> Text -> M LspNode\nfindDef a b = undefined\n",
        )]);
        let dirs: Vec<PathBuf> = vec![d.path().to_path_buf()];
        let v = stdlib_value_info(&dirs, "findDef").expect("findDef is a value hit");
        assert_eq!(v["name"], "findDef");
        assert_eq!(v["shape"], "findDef :: Text -> Text -> M LspNode");
        assert_eq!(v["module"], "Lsp");
        assert_eq!(v["source"], "stdlib");
    }

    #[test]
    fn value_signature_miss_and_uppercase_rejected() {
        let d = dir_with(&[(
            "Lsp.hs",
            "module Lsp where\n\nfindDef :: Text -> Text -> M LspNode\n",
        )]);
        let dirs: Vec<PathBuf> = vec![d.path().to_path_buf()];
        assert_eq!(stdlib_value_info(&dirs, "nonexistent"), None);
        // Uppercase names are never value hits (that's stdlib_info's job).
        assert_eq!(stdlib_value_info(&dirs, "FindDef"), None);
    }

    #[test]
    fn value_signature_collision_reports_ambiguous() {
        let d1 = dir_with(&[("A.hs", "module A where\n\nfoo :: Int -> Int\n")]);
        let d2 = dir_with(&[("B.hs", "module B where\n\nfoo :: Text -> Text\n")]);
        let dirs: Vec<PathBuf> = vec![d1.path().to_path_buf(), d2.path().to_path_buf()];
        let v = stdlib_value_info(&dirs, "foo").unwrap();
        let ambiguous = v["ambiguous"].as_array().expect("ambiguous array");
        assert_eq!(ambiguous.len(), 2);
        assert_eq!(ambiguous[0]["module"], "A");
        assert_eq!(ambiguous[1]["module"], "B");
    }

    #[cfg(unix)]
    #[test]
    fn symlink_cycle_terminates() {
        let d = dir_with(&[("A.hs", "module A where\n\ndata Foo = Foo Int\n")]);
        std::os::unix::fs::symlink(d.path(), d.path().join("loop")).unwrap();
        let v = lookup(&[&d], "Foo").expect("hit despite the cycle");
        assert_eq!(v["module"], "A");
        assert_eq!(lookup(&[&d], "Nope"), None, "miss terminates too");
    }
}
