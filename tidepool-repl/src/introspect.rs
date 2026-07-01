//! `:i <Name>` resolution for stdlib/preamble types — the source-scan lane.
//!
//! The repl's `:i` used to see only (1) session value bindings, (2) built-in
//! effect decl `type_defs`, and (3) session-declared types — so the types the
//! preamble puts in scope (`Proc`, `Hit`, `Doc`, `Schema`, … from
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

use std::path::PathBuf;

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
///
/// `None` on a miss (the caller falls through to its total-miss error).
///
/// Scan rules:
/// - Walk each dir in `include_dirs` recursively for `*.hs` (these dirs are
///   small — stdlib + verb libs; no depth/size guard needed beyond symlink
///   sanity).
/// - A declaration ends where the next line is blank or starts at column 0
///   (standard layout — continuation lines are indented).
/// - First hit wins in `include_dirs` order (matches GHC include-path
///   precedence: effects dir, stdlib, project lib, global lib).
pub fn stdlib_info(include_dirs: &[PathBuf], name: &str) -> Option<serde_json::Value> {
    // TODO(child repl-info): implement per the doc above.
    let _ = (include_dirs, name);
    None
}
