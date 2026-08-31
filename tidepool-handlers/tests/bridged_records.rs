//! Single-source-of-truth guard for the Rust↔Haskell bridged records.
//!
//! Each in-scope wire-record struct derives `CoreRecord`, so its Haskell `data`
//! declaration is GENERATED from the Rust struct. This test:
//!
//!   1. pins each generated decl to its exact expected Haskell text (the
//!      before/after DIFF — a field ORDER or NAME change on either side flips a
//!      probe red; this is the drift the tool is built to catch);
//!   2. golden-checks the generated `Tidepool.Records.Bridged` module against
//!      the committed stdlib file (regen with `TIDEPOOL_REGEN_BRIDGED=1`).
//!
//! A third, separate guard (bottom of this file) covers `Tidepool.Records.
//! Stable` — the stable home for `FsError`/`FileRead`/`GitError`/`LlmError`/
//! `HttpError`, which can't go through the `CoreRecord` pipeline above (see
//! `tidepool-mcp/src/fs_stable.rs`) but needs the exact same drift protection.

use tidepool_bridge::CoreRecord;
use tidepool_handlers::{
    bridged_records_module, GitCommit, GitCommitDeltas, GitFileDelta, GitStatusEntry,
};

// --- 1. Exact generated decls (the before/after diff). ----------------------

#[test]
fn generated_decls_match_expected_exactly() {
    // Git records (fully migrated into Tidepool.Records.Bridged).
    assert_eq!(
        GitCommit::haskell_decl(),
        "data Commit = Commit { sha :: Text, subject :: Text, author :: Text, \
         date :: Text, files :: [Text] } deriving (Show, Eq)"
    );
    assert_eq!(
        GitStatusEntry::haskell_decl(),
        "data StatusEntry = StatusEntry { path :: Text, state :: Text } deriving (Show, Eq)"
    );
    assert_eq!(
        GitFileDelta::haskell_decl(),
        "data FileDelta = FileDelta { path :: Text, adds :: Int, dels :: Int, \
         binary :: Bool } deriving (Show, Eq)"
    );
    assert_eq!(
        GitCommitDeltas::haskell_decl(),
        "data CommitDeltas = CommitDeltas { commit :: Commit, deltas :: [FileDelta] } deriving (Show, Eq)"
    );
}

// --- 2. Golden: generated Bridged module == committed stdlib file. -----------

fn bridged_path() -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../haskell/lib/Tidepool/Records/Bridged.hs")
}

#[test]
fn bridged_records_module_matches_committed_file() {
    let generated = bridged_records_module();
    let path = bridged_path();
    let regen = std::env::var_os("TIDEPOOL_REGEN_BRIDGED").is_some();
    let current = std::fs::read_to_string(&path).ok();
    if regen || current.is_none() {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        std::fs::write(&path, &generated).unwrap();
        if current.as_deref() != Some(generated.as_str()) && !regen {
            panic!("wrote missing/updated {} — re-run the test", path.display());
        }
        return;
    }
    assert_eq!(
        current.unwrap(),
        generated,
        "committed {} is stale vs the generated module — regenerate with \
         TIDEPOOL_REGEN_BRIDGED=1 cargo test -p tidepool-handlers bridged_records",
        path.display()
    );
}

// --- 3. `Tidepool.Records.Stable` — the stable home for `errors` ADTs. -----
//
// Not `CoreRecord`-derived (see `tidepool-mcp/src/fs_stable.rs` for why:
// each Rust enum is codegenerated inside THIS crate, so
// `tidepool-bridge-effects`, a LOW crate, has no path back to it). Each
// constant there hand-carries the SAME variant list its effect def's
// `errors <Err> [...]` block declares (effect_defs.rs) — this test is what
// catches the two drifting apart.

fn stable_path() -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../haskell/lib/Tidepool/Records/Stable.hs")
}

#[test]
fn stable_records_decl_matches_fs_effect_def() {
    // `FS_ERROR_STABLE_DECL` hand-carries the SAME 5 variants as
    // `fs_effect_def!`'s `errors FsError [...]` block (effect_defs.rs) — a
    // renamed/added/removed variant on either side must show up here.
    // (The Rust `FsError` enum, generated from that SAME block via
    // `effect_rust_projection!`, is checked independently by every
    // `FsError::<Ctor>(...)` construction already in `handlers/fs.rs` —
    // rename a variant there and the crate fails to compile.)
    for ctor in [
        "FsNotFound",
        "FsNotUtf8",
        "FsSandbox",
        "FsBadRegex",
        "FsIo",
        "FsNonUtf8Path",
    ] {
        assert!(
            tidepool_mcp::FS_ERROR_STABLE_DECL.contains(ctor),
            "stable FsError decl is missing constructor `{ctor}`"
        );
    }
    assert!(tidepool_mcp::FS_ERROR_STABLE_DECL.starts_with("data FsError = "));
    assert!(tidepool_mcp::FILE_READ_STABLE_DECL.starts_with("data FileRead = "));
    // `fs_decl()` reuses the nominal types exported by Records/Prelude rather
    // than declaring a duplicate copy in Core.
    assert!(tidepool_mcp::fs_read_decl().type_defs.is_empty());
    assert!(tidepool_mcp::fs_write_decl().type_defs.is_empty());
}

#[test]
fn stable_records_decl_matches_git_llm_http_effect_defs() {
    // `GIT_ERROR_STABLE_DECL` hand-carries the SAME 2 variants as
    // `git_effect_def!`'s `errors GitError [...]` block.
    for ctor in ["GitBadRevspec", "GitFailed"] {
        assert!(
            tidepool_mcp::GIT_ERROR_STABLE_DECL.contains(ctor),
            "stable GitError decl is missing constructor `{ctor}`"
        );
    }
    assert!(tidepool_mcp::GIT_ERROR_STABLE_DECL.starts_with("data GitError = "));

    // `LLM_ERROR_STABLE_DECL` hand-carries the SAME 3 variants as
    // `llm_effect_def!`'s `errors LlmError [...]` block.
    for ctor in ["LlmApi", "LlmRefusal", "LlmBudget"] {
        assert!(
            tidepool_mcp::LLM_ERROR_STABLE_DECL.contains(ctor),
            "stable LlmError decl is missing constructor `{ctor}`"
        );
    }
    assert!(tidepool_mcp::LLM_ERROR_STABLE_DECL.starts_with("data LlmError = "));

    // `HTTP_ERROR_STABLE_DECL` hand-carries the SAME 5 variants as
    // `http_effect_def!`'s `errors HttpError [...]` block.
    for ctor in [
        "HttpInvalidUrl",
        "HttpRestricted",
        "HttpNetwork",
        "HttpStatus",
        "HttpTooLarge",
    ] {
        assert!(
            tidepool_mcp::HTTP_ERROR_STABLE_DECL.contains(ctor),
            "stable HttpError decl is missing constructor `{ctor}`"
        );
    }
    assert!(tidepool_mcp::HTTP_ERROR_STABLE_DECL.starts_with("data HttpError = "));

    // These effects reuse the nominal types exported by Records/Prelude.
    assert!(tidepool_mcp::git_decl().type_defs.is_empty());
    assert!(tidepool_mcp::llm_decl().type_defs.is_empty());
    assert!(tidepool_mcp::http_decl().type_defs.is_empty());
}

#[test]
fn stable_records_module_matches_committed_file() {
    let generated = tidepool_mcp::stable_records_module();
    let path = stable_path();
    let regen = std::env::var_os("TIDEPOOL_REGEN_BRIDGED").is_some();
    let current = std::fs::read_to_string(&path).ok();
    if regen || current.is_none() {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        std::fs::write(&path, &generated).unwrap();
        if current.as_deref() != Some(generated.as_str()) && !regen {
            panic!("wrote missing/updated {} — re-run the test", path.display());
        }
        return;
    }
    assert_eq!(
        current.unwrap(),
        generated,
        "committed {} is stale vs the generated module — regenerate with \
         TIDEPOOL_REGEN_BRIDGED=1 cargo test -p tidepool-handlers bridged_records",
        path.display()
    );
}
