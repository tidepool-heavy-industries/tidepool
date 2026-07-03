use std::path::PathBuf;

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
pub(crate) fn ensure_prelude() -> Result<PathBuf, Box<dyn std::error::Error>> {
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
