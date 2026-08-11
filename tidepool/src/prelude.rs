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

/// Materialize the bundled stdlib into its content-addressed cache dir and
/// return it. Idempotent: a `.complete` sentinel marks a finished write, so
/// repeat startups do one `exists()` check. The sentinel also guards against
/// serving a half-written dir (a crash mid-materialization) and against the
/// macOS cache reaper.
///
/// The bundle is the COMPLETE tree and is keyed on content, so it can't go
/// stale across binary versions and can't drift from a hand-maintained
/// subset.
fn materialize_bundle() -> Result<PathBuf, Box<dyn std::error::Error>> {
    let hash = stdlib_content_hash();
    let base = tidepool_runtime::paths::stdlib_dir(&hash);
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

/// Resolve the directory holding the Tidepool stdlib (an include root for GHC).
///
/// Precedence lives in ONE place — [`tidepool_runtime::toolchain::locate_stdlib`],
/// whose module docs carry the table. This binary contributes step 4 (the
/// stdlib embedded at build time, materialized above); it needs no step 5,
/// since the bundle always ships with it.
pub(crate) fn ensure_prelude() -> Result<PathBuf, Box<dyn std::error::Error>> {
    let fallbacks = tidepool_runtime::toolchain::StdlibFallbacks {
        bundle: Some(materialize_bundle()?),
        build_tree: None,
    };
    Ok(tidepool_runtime::toolchain::locate_stdlib(&fallbacks)?.dir)
}
