//! Single-source-of-truth guard for the Rust↔Haskell bridged records.
//!
//! Each in-scope wire-record struct derives `CoreRecord`, so its Haskell `data`
//! declaration is GENERATED from the Rust struct. This test:
//!
//!   1. pins each generated decl to its exact expected Haskell text (the
//!      before/after DIFF — a field ORDER or NAME change on either side flips a
//!      probe red; this is the drift the tool is built to catch);
//!   2. ties the Lsp records still declared in `effect_decls` to their
//!      generated decls (they are CoreRecord-derived but their Haskell text
//!      currently lives in-place — the tie is the always-on guard that closes
//!      friction #25's field-level gap without an LSP daemon / extract run);
//!   3. golden-checks the generated `Tidepool.Records.Bridged` module against
//!      the committed stdlib file (regen with `TIDEPOOL_REGEN_BRIDGED=1`).

use tidepool_bridge::CoreRecord;
use tidepool_handlers::{
    bridged_records_module, GitCommit, GitFileDelta, GitStatusEntry, LspDiag, LspNode, LspPosition,
};

/// Strip a trailing `deriving (...)` and collapse whitespace so a hand-written
/// decl (no `deriving`) compares equal to the generated one.
fn normalize(decl: &str) -> String {
    let body = decl.split(" deriving").next().unwrap_or(decl);
    body.split_whitespace().collect::<Vec<_>>().join(" ")
}

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
    // Lsp records (CoreRecord-derived; decls still in effect_decls).
    assert_eq!(
        LspPosition::haskell_decl(),
        "data Position = Position { posLine :: Int, posChar :: Int } deriving (Show, Eq)"
    );
    assert_eq!(
        LspNode::haskell_decl(),
        "data LspNode = LspNode { nodeName :: Text, nodeContainer :: Text, nodeKind :: Text, \
         nodeFile :: Text, nodePos :: Position, nodeText :: Text } deriving (Show, Eq)"
    );
    assert_eq!(
        LspDiag::haskell_decl(),
        "data Diag = Diag { diagFile :: Text, diagLine :: Int, diagSeverity :: Text, \
         diagMessage :: Text } deriving (Show, Eq)"
    );
}

// --- 2. Tie the in-place SG/Lsp decls to the generated ones. -----------------

/// Concatenate every effect decl's `type_defs` into one searchable corpus.
fn effect_type_defs_corpus() -> String {
    let mut corpus = String::new();
    for decl in tidepool_mcp::standard_decls() {
        for td in decl.type_defs {
            corpus.push_str(td);
            corpus.push('\n');
        }
    }
    corpus
}

#[test]
fn in_place_decls_match_generated() {
    let corpus = effect_type_defs_corpus();
    for generated in [
        LspPosition::haskell_decl(),
        LspNode::haskell_decl(),
        LspDiag::haskell_decl(),
    ] {
        let want = normalize(&generated);
        // Find the `data <Name> = ...` line in the effect decls and compare
        // (normalized). The effect_decls copies carry no `deriving`.
        let name = generated.split_whitespace().nth(1).unwrap();
        let needle = format!("data {name} =");
        let found = corpus
            .lines()
            .find(|l| l.trim_start().starts_with(&needle))
            .unwrap_or_else(|| panic!("no `{needle}` decl found in effect type_defs"));
        assert_eq!(
            normalize(found),
            want,
            "effect_decls decl for `{name}` drifted from the Rust struct \
             (CoreRecord). Update the effect_decls type_def OR the struct so they agree."
        );
    }
}

// --- 3. Golden: generated Bridged module == committed stdlib file. -----------

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
