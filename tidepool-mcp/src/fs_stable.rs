//! The stable Haskell home for `FsError` and `FileRead` — moved out of the
//! per-session generated `Tidepool.Effects` module (`fs_effect_def!`'s
//! `stable_errors true`, effect_defs.rs) because `FileRead.contents ::
//! Either FsError Text` embeds `FsError` as a FIELD, and a record meant to
//! cross session-bind fragments cannot mention a type declared inline in
//! `Tidepool.Effects` — that module is fragment-nominal (a fresh one per
//! turn), so a value naming one of its types cannot mean anything once it
//! crosses into a LATER turn's compile (haskell/src/Tidepool/Translate.hs's
//! `typeMentionsEffectMonad`, the session-bind cross-row guard).
//!
//! Mirrors `tidepool_bridge_effects::bridged_records_module` (the stable
//! home the six `CoreRecord`-derived bridged records already have) — same
//! idea, different source. Those six can't cover `FsError`/`FileRead`:
//! `FsError`'s Rust enum is itself codegenerated inside `tidepool-handlers`
//! (a TOP crate, from the SAME `errors FsError [...]` block below) via
//! `effect_rust_projection!`, and `tidepool-bridge-effects` is a LOW crate
//! with no dependency path back to it — so this file hand-carries the SAME
//! variant list `fs_effect_def!`'s `errors FsError [...]` block declares
//! (tidepool-mcp/src/effect_defs.rs), rather than being generated from a
//! shared Rust struct. The two lists MUST stay in sync; the `stable_records`
//! test (tidepool-handlers/tests/bridged_records.rs) pins this file against
//! drift the same way `bridged_records_module_matches_committed_file` pins
//! `Tidepool.Records.Bridged`.

/// `FsError`'s Haskell decl + `ToJSON` instance. MUST stay in sync with
/// `fs_effect_def!`'s `errors FsError [...]` block (effect_defs.rs) — the
/// `stable_records` test catches drift between them.
pub const FS_ERROR_STABLE_DECL: &str = crate::effect_defs::error_decl_text!(
    FsError,
    { ctor FsNotFound, fields { path: "Text" as String },   doc "path does not exist" },
    { ctor FsNotUtf8,  fields { path: "Text" as String },   doc "file is not valid UTF-8" },
    { ctor FsSandbox,  fields { detail: "Text" as String }, doc "path escapes the sandbox, or the glob pattern is not allowed" },
    { ctor FsBadRegex, fields { detail: "Text" as String }, doc "grep regex failed to compile" },
    { ctor FsIo,       fields { detail: "Text" as String }, doc "other I/O failure" },
    { ctor FsNonUtf8Path, fields { path: "Text" as String }, doc "path is not valid UTF-8 (lossy rendering shown for diagnostics)" },
);

/// `FileRead`'s Haskell decl + `ToJSON` instance — the per-file result
/// record `readGlob` yields (#328/#335: `path` plus a typed `contents`,
/// `Right text` on a clean UTF-8 read, `Left (FsError)` on a per-file
/// failure). Lived inline in `fs_effect_def!`'s `type_defs` until it needed
/// this stable home (see module doc) — text unchanged from that original.
pub const FILE_READ_STABLE_DECL: &str = "data FileRead = FileRead { path :: Text, contents :: Either FsError Text } deriving (Show, Eq)\ninstance ToJSON FileRead where\n  toJSON (FileRead p c) = object [\"path\" .= p, \"contents\" .= c]";

/// Build the `Tidepool.Records.Stable` Haskell module: the stable,
/// always-available home for effect-adjacent decls that a bridged record's
/// FIELD embeds, so they need to be safe to name from a value that crosses a
/// session bind but can't go through the CoreRecord/`Tidepool.Records.Bridged`
/// pipeline (see module doc for why `FsError`/`FileRead` are the first
/// tenants). Materialized to the committed
/// `haskell/lib/Tidepool/Records/Stable.hs` (kept in sync by the
/// `stable_records` test, mirroring `bridged_records_module`).
pub fn stable_records_module() -> String {
    let mut out = String::new();
    out.push_str(
        "{-# LANGUAGE NoImplicitPrelude, OverloadedStrings, DuplicateRecordFields #-}\n\n",
    );
    out.push_str("-- | The stable home for effect-adjacent decls a bridged record's FIELD\n");
    out.push_str("-- embeds (see tidepool-mcp/src/fs_stable.rs). DO NOT EDIT BY HAND: the\n");
    out.push_str("-- Rust side is the single source of truth, and this file is regenerated +\n");
    out.push_str("-- verified by the `stable_records` test\n");
    out.push_str(
        "-- (`TIDEPOOL_REGEN_BRIDGED=1 cargo test -p tidepool-handlers bridged_records`).\n",
    );
    out.push_str("module Tidepool.Records.Stable\n  ( FsError(..), FileRead(..) ) where\n\n");
    out.push_str("import Prelude (Either(..), Eq, Show)\n");
    out.push_str("import Data.Text (Text)\n");
    out.push_str("import Tidepool.Aeson.Value (ToJSON(..), object, (.=))\n\n");
    out.push_str(FS_ERROR_STABLE_DECL);
    out.push_str("\n\n");
    out.push_str(FILE_READ_STABLE_DECL);
    out.push('\n');
    out
}
