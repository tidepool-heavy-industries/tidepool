//! The stable Haskell home for effect-adjacent `errors` ADTs (and `FileRead`,
//! the one bridged-record FIELD case) — moved out of the per-session
//! generated `Tidepool.Effects` module (an effect definition's
//! `stable_errors true`, effect_defs.rs) because a value meant to survive a
//! session bind cannot mention a type declared inline in `Tidepool.Effects`
//! — that module is fragment-nominal (a fresh one per turn), so a value
//! naming one of its types cannot mean anything once it crosses into a LATER
//! turn's compile (haskell/src/Tidepool/Translate.hs's
//! `typeMentionsEffectMonad`, the session-bind cross-row guard). `FsError`/
//! `FileRead` shipped inline for a long time harmlessly, until a later-landed
//! guard turned the gap into a hard failure the moment someone bound it —
//! this module is the fix and (see `FILE_READ_STABLE_DECL`'s doc) the origin
//! story. `GitError`/`LlmError`/`HttpError` hit the SAME guard for a plainer
//! reason: a verb's own `Either <Err> T` result mentions `<Err>` directly, so
//! binding the WHOLE `Either` (not just a nested record field) already
//! trips it — e.g. `x <- gitLog n` (no `Right x <-` destructuring) reused in
//! a later turn.
//!
//! Mirrors `tidepool_bridge_effects::bridged_records_module` (the stable
//! home the bridged records already have) — same idea, different source.
//! Every `errors` ADT here can't go through that pipeline for the same
//! reason `FsError` couldn't: each Rust enum is itself codegenerated inside
//! `tidepool-handlers` (a TOP crate, from the SAME `errors <Err> [...]` block
//! below) via `effect_rust_projection!`, and `tidepool-bridge-effects` is a
//! LOW crate with no dependency path back to it — so this file hand-carries
//! the SAME variant list each definition's `errors <Err> [...]` block
//! declares (`tidepool-mcp/src/effect_defs.rs`), rather than being generated
//! from a shared Rust struct. Each pair MUST stay in sync; the
//! `stable_records` test (`tidepool-handlers/tests/bridged_records.rs`) pins
//! this file against drift the same way `bridged_records_module_matches_committed_file`
//! pins `Tidepool.Records.Bridged`.

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

/// `GitError`'s Haskell decl + `ToJSON` instance. MUST stay in sync with
/// `git_effect_def!`'s `errors GitError [...]` block (effect_defs.rs).
pub const GIT_ERROR_STABLE_DECL: &str = crate::effect_defs::error_decl_text!(
    GitError,
    { ctor GitBadRevspec, fields { detail: "Text" as String },                    doc "unknown or ambiguous revspec" },
    { ctor GitFailed,     fields { code: "Int" as i64, detail: "Text" as String }, doc "git exited nonzero (or could not be spawned)" },
);

/// `LlmError`'s Haskell decl + `ToJSON` instance. MUST stay in sync with
/// `llm_effect_def!`'s `errors LlmError [...]` block (effect_defs.rs).
pub const LLM_ERROR_STABLE_DECL: &str = crate::effect_defs::error_decl_text!(
    LlmError,
    { ctor LlmApi,     fields { detail: "Text" as String }, doc "API/network call failure" },
    { ctor LlmRefusal, fields { detail: "Text" as String }, doc "the model declined to answer" },
    { ctor LlmBudget,  fields { },                          doc "the per-eval call budget is exhausted" },
);

/// `HttpError`'s Haskell decl + `ToJSON` instance. MUST stay in sync with
/// `http_effect_def!`'s `errors HttpError [...]` block (effect_defs.rs).
pub const HTTP_ERROR_STABLE_DECL: &str = crate::effect_defs::error_decl_text!(
    HttpError,
    { ctor HttpInvalidUrl, fields { detail: "Text" as String },                  doc "the URL is malformed or uses an unsupported scheme" },
    { ctor HttpRestricted, fields { detail: "Text" as String },                  doc "the URL targets a sandboxed/internal address" },
    { ctor HttpNetwork,    fields { detail: "Text" as String },                  doc "a network-level failure (connect/timeout/read)" },
    { ctor HttpStatus,     fields { code: "Int" as i64, body: "Text" as String }, doc "a non-2xx HTTP response" },
    { ctor HttpTooLarge,   fields { nodes: "Int" as i64 },                       doc "the JSON response exceeds the materialization cap (node count); narrow the query" },
);

/// `FileRead`'s Haskell decl + `ToJSON` instance — the per-file result
/// record `readGlob` yields (#328/#335: `path` plus a typed `contents`,
/// `Right text` on a clean UTF-8 read, `Left (FsError)` on a per-file
/// failure). Lived inline in `fs_effect_def!`'s `type_defs` until it needed
/// this stable home (see module doc) — text unchanged from that original.
pub const FILE_READ_STABLE_DECL: &str = "data FileRead = FileRead { path :: Text, contents :: Either FsError Text } deriving (Show, Eq)\ninstance ToJSON FileRead where\n  toJSON (FileRead p c) = object [\"path\" .= p, \"contents\" .= c]";

/// Build the `Tidepool.Records.Stable` Haskell module: the stable,
/// always-available home for effect-adjacent decls that either embed a
/// bridged record's FIELD or are themselves an effect's whole `Either <Err>
/// T` result, so they need to be safe to name from a value that crosses a
/// session bind but can't go through the CoreRecord/`Tidepool.Records.Bridged`
/// pipeline (see module doc for why `FsError`/`FileRead` were the first
/// tenants). Materialized to the committed
/// `haskell/lib/Tidepool/Records/Stable.hs` (kept in sync by the
/// `stable_records` test, mirroring `bridged_records_module`).
pub fn stable_records_module() -> String {
    let mut out = String::new();
    out.push_str(
        "{-# LANGUAGE NoImplicitPrelude, OverloadedStrings, DuplicateRecordFields #-}\n\n",
    );
    out.push_str("-- | The stable home for effect-adjacent decls a bridged record's FIELD\n");
    out.push_str("-- embeds, or an effect's own `errors` ADT (see tidepool-mcp/src/\n");
    out.push_str("-- fs_stable.rs). DO NOT EDIT BY HAND: the Rust side is the single source\n");
    out.push_str("-- of truth, and this file is regenerated + verified by the `stable_records`\n");
    out.push_str(
        "-- test (`TIDEPOOL_REGEN_BRIDGED=1 cargo test -p tidepool-handlers bridged_records`).\n",
    );
    out.push_str("module Tidepool.Records.Stable\n  ( FsError(..), FileRead(..), GitError(..), LlmError(..), HttpError(..) ) where\n\n");
    out.push_str("import Prelude (Either(..), Eq, Int, Show)\n");
    out.push_str("import Data.Text (Text)\n");
    out.push_str("import Tidepool.Aeson.Value (ToJSON(..), object, (.=))\n\n");
    out.push_str(FS_ERROR_STABLE_DECL);
    out.push_str("\n\n");
    out.push_str(GIT_ERROR_STABLE_DECL);
    out.push_str("\n\n");
    out.push_str(LLM_ERROR_STABLE_DECL);
    out.push_str("\n\n");
    out.push_str(HTTP_ERROR_STABLE_DECL);
    out.push_str("\n\n");
    out.push_str(FILE_READ_STABLE_DECL);
    out.push('\n');
    out
}
