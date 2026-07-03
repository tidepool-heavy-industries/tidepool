//! Fault-isolating assembly of the `.tidepool/lib` verb-library layer (issue #322).
//!
//! Every eval's preamble does `import Library`, and the `Library` facade
//! re-exports each sibling verb module. So a single broken lib module — from a
//! tool `writeFile`, an out-of-band edit, or an effect-cut regression that
//! leaves a module referencing a deleted constructor — fails `Library`, hence
//! EVERY eval, including the `writeFile`/`writeChecked` that would repair it.
//! Only a host-side edit could un-brick it (this bit us live 2026-07-03; the
//! SG-effect cut left `Explore.hs` referencing the deleted `Match` constructor,
//! fixed host-side in commit 03eeb92).
//!
//! [`isolate_lib_layer`] compile-probes the facade. In the common (healthy)
//! case that is ONE cheap probe and it returns [`LibLayer::default`]. When the
//! facade is broken it probes each re-exported module to find the culprit(s),
//! then materializes a SANITIZED `Library.hs` that re-exports only the modules
//! that currently compile, prepended to the include path so it shadows the real
//! (broken) facade. Healthy verbs stay in scope and an eval can even
//! `writeChecked` a repair.
//!
//! It is re-probed per eval and salted by a content-snapshot hash of the lib
//! dirs, so it is resilient to ANY breakage source — unlike a
//! compile-probe-before-land write gate, which would not have caught the
//! effect-cut regression that motivated the issue. A single-entry in-process
//! memo keyed on that snapshot hash makes the steady state (unchanged lib)
//! essentially free.

use parking_lot::Mutex;
use std::path::{Path, PathBuf};

/// The outcome of fault-isolating the lib layer for one eval.
#[derive(Debug, Default, Clone)]
pub struct LibLayer {
    /// Dirs to PREPEND to the include path (a staging dir holding a sanitized
    /// `Library.hs`). Empty when the facade compiles as-is (the common case).
    pub prepend_include: Vec<PathBuf>,
    /// A human-facing note naming the excluded module(s), surfaced to the eval
    /// caller so a broken verb module is diagnosed rather than silently dropped.
    /// `None` when nothing was excluded.
    pub brick_note: Option<String>,
}

/// Single-entry memo: `(lib-snapshot-hash, layer)`. A snapshot-hash match means
/// no lib file changed since we last probed, so the previous decision stands.
static MEMO: Mutex<Option<(u64, LibLayer)>> = Mutex::new(None);

/// Compile-probe the `.tidepool/lib` facade and, if it is broken, return an
/// include-path prefix + a sanitized `Library.hs` that excludes the broken
/// module(s) so a single bad lib module can't brick every eval.
///
/// * `lib_dirs` — the project/global verb-library dirs (project first).
/// * `include`  — the FULL eval include path (effects, stdlib, the lib dirs),
///   used to resolve each probe's imports.
///
/// Returns [`LibLayer::default`] (no-op) when there is no user `Library`, when
/// the facade compiles cleanly, or when the facade's export list can't be
/// parsed (nothing to reason about — the existing lib-brick diagnostic still
/// fires on the resulting compile error).
pub fn isolate_lib_layer(lib_dirs: &[PathBuf], include: &[PathBuf]) -> LibLayer {
    // No user Library facade → nothing to isolate.
    let Some(lib_src) = read_first_library(lib_dirs) else {
        return LibLayer::default();
    };

    let snapshot = lib_snapshot_hash(lib_dirs);
    if let Some((h, layer)) = MEMO.lock().as_ref() {
        if *h == snapshot {
            return layer.clone();
        }
    }

    let layer = compute_layer(&lib_src, include, snapshot);
    *MEMO.lock() = Some((snapshot, layer.clone()));
    layer
}

fn compute_layer(lib_src: &str, include: &[PathBuf], snapshot: u64) -> LibLayer {
    let salt = format!("libiso-{snapshot:016x}");

    // Fast path: does `import Library` compile? One probe, cached under `salt`.
    if probe_import("Library", include, &salt).is_ok() {
        return LibLayer::default();
    }

    // Facade broken. Parse its intended re-export surface so we can rebuild a
    // clean facade from the subset that compiles.
    let reexports = parse_reexports_ordered(lib_src);
    if reexports.is_empty() {
        // Unparseable export list: can't reason about the intended surface.
        // Leave it to the compile error + the lib-brick diagnostic.
        return LibLayer::default();
    }

    let mut broken: Vec<String> = Vec::new();
    let mut healthy: Vec<String> = Vec::new();
    for m in &reexports {
        if probe_import(m, include, &salt).is_ok() {
            healthy.push(m.clone());
        } else {
            broken.push(m.clone());
        }
    }

    // If every re-export compiles yet `import Library` failed, the fault is in
    // the facade file itself (a typo, or it imports a non-re-exported broken
    // module). Our regenerated facade imports only `healthy` (== reexports
    // here), so it sidesteps that too.
    let excluded: Vec<String> = if broken.is_empty() {
        vec!["Library (the facade file itself)".to_string()]
    } else {
        broken.clone()
    };

    let sanitized = sanitized_library_source(&healthy, &broken);
    match stage_library(&sanitized) {
        Ok(dir) => LibLayer {
            prepend_include: vec![dir],
            brick_note: Some(brick_note(&excluded)),
        },
        Err(e) => {
            eprintln!("[tidepool] lib fault-isolation: failed to stage sanitized Library.hs: {e}");
            LibLayer::default()
        }
    }
}

/// Compile-probe a single module by importing it into a throwaway module. This
/// forces GHC to build `module` (and its transitive deps) via the include path;
/// a broken module fails the extract. Salting by the lib snapshot hash means an
/// on-disk edit busts the (otherwise source-identical) probe's cache entry.
fn probe_import(
    module: &str,
    include: &[PathBuf],
    salt: &str,
) -> Result<(), tidepool_runtime::CompileError> {
    let src = format!(
        "{pragmas}\nmodule LibProbe where\nimport {module}\n__libProbe__ :: ()\n__libProbe__ = ()\n",
        pragmas = crate::EVAL_PRAGMAS,
    );
    let refs: Vec<&Path> = include.iter().map(PathBuf::as_path).collect();
    tidepool_runtime::compile_haskell_salted(&src, "__libProbe__", &refs, Some(salt)).map(|_| ())
}

/// Read the first `Library.hs` found across `lib_dirs` (project shadows global,
/// mirroring GHC's first-match-wins include search).
fn read_first_library(lib_dirs: &[PathBuf]) -> Option<String> {
    lib_dirs
        .iter()
        .find_map(|d| std::fs::read_to_string(d.join("Library.hs")).ok())
}

/// Ordered parse of a `module Library ( module A, module B, … ) where` header
/// into the re-exported module stems, preserving source order (so a regenerated
/// facade keeps the original layout). Returns empty if the header is absent /
/// the export list can't be found.
fn parse_reexports_ordered(src: &str) -> Vec<String> {
    let Some((_, after)) = src.split_once("module Library") else {
        return Vec::new();
    };
    let list = after.split_once(')').map_or(after, |(l, _)| l);
    let mut out = Vec::new();
    for raw in list.split(',') {
        let entry = raw.trim().trim_start_matches('(').trim();
        if let Some(rest) = entry.strip_prefix("module ") {
            if let Some(name) = rest.split_whitespace().next() {
                out.push(name.to_string());
            }
        }
    }
    out
}

/// Render a sanitized `Library.hs` re-exporting only `healthy` (order
/// preserved). `broken` is listed in a header comment for provenance. When
/// `healthy` is empty the facade is a valid, importable, empty module — a
/// degraded-but-not-bricked fallback.
fn sanitized_library_source(healthy: &[String], broken: &[String]) -> String {
    let mut s = String::new();
    s.push_str("-- GENERATED by tidepool fault-isolation (issue #322).\n");
    if !broken.is_empty() {
        s.push_str(&format!(
            "-- Excluded (failed to compile): {}\n",
            broken.join(", ")
        ));
    }
    s.push_str("-- Re-exports only the .tidepool/lib modules that currently compile,\n");
    s.push_str("-- so a broken verb module cannot brick every eval.\n");
    if healthy.is_empty() {
        s.push_str("module Library () where\n");
        return s;
    }
    s.push_str("module Library\n  ( ");
    s.push_str(
        &healthy
            .iter()
            .map(|m| format!("module {m}"))
            .collect::<Vec<_>>()
            .join("\n  , "),
    );
    s.push_str("\n  ) where\n");
    for m in healthy {
        s.push_str(&format!("import {m}\n"));
    }
    s
}

/// The caller-facing note surfaced when a broken module was contained.
fn brick_note(excluded: &[String]) -> String {
    format!(
        "[lib-brick contained] a project library module failed to compile and was \
         EXCLUDED from the auto-imported `Library` facade so healthy verbs still work: \
         {}. Fix or remove the module — you can `writeChecked` a repair now, the facade \
         is no longer bricked; run `vocab` to see what stayed in scope.",
        excluded.join(", ")
    )
}

/// Write the sanitized facade into a content-addressed staging dir under the
/// stable cache root and return that dir (an include root). Idempotent: the
/// path is keyed on the source, so identical facades reuse the same dir.
fn stage_library(src: &str) -> std::io::Result<PathBuf> {
    let hash = crate::fnv1a_hash(src.as_bytes());
    let root = tidepool_runtime::paths::effects_dir().join(format!("tidepool-libiso-{hash:016x}"));
    crate::write_module_file(&root, "Library.hs", src)?;
    Ok(root)
}

/// FNV-1a over every `.hs` file (sorted path + content) across `lib_dirs`. Any
/// edit/add/remove to any lib module changes this, busting both the in-process
/// memo and every probe's compile cache so the layer is recomputed.
fn lib_snapshot_hash(lib_dirs: &[PathBuf]) -> u64 {
    let mut entries: Vec<(String, Vec<u8>)> = Vec::new();
    for dir in lib_dirs {
        if let Ok(rd) = std::fs::read_dir(dir) {
            for e in rd.flatten() {
                let p = e.path();
                if p.extension().is_some_and(|x| x == "hs") {
                    if let Ok(bytes) = std::fs::read(&p) {
                        entries.push((p.to_string_lossy().into_owned(), bytes));
                    }
                }
            }
        }
    }
    entries.sort_by(|a, b| a.0.cmp(&b.0));
    let mut combined: Vec<u8> = Vec::new();
    for (path, bytes) in entries {
        combined.extend_from_slice(path.as_bytes());
        combined.push(0);
        combined.extend_from_slice(&bytes);
        combined.push(0);
    }
    crate::fnv1a_hash(&combined)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    #[test]
    fn parse_reexports_is_ordered_and_strips_paren() {
        let src = "\
module Library
  ( module Schemes
  , module Explore
  , module Lsp
  ) where
import Schemes
import Explore
import Lsp
";
        assert_eq!(
            parse_reexports_ordered(src),
            vec![
                "Schemes".to_string(),
                "Explore".to_string(),
                "Lsp".to_string()
            ]
        );
    }

    #[test]
    fn parse_reexports_absent_header_is_empty() {
        assert!(parse_reexports_ordered("module Schemes where\nfoo :: Int\n").is_empty());
    }

    #[test]
    fn sanitized_library_excludes_broken_and_keeps_order() {
        let healthy = vec!["Schemes".to_string(), "Lsp".to_string()];
        let broken = vec!["Explore".to_string()];
        let out = sanitized_library_source(&healthy, &broken);
        // The broken module is gone from the export + import list.
        assert!(
            !out.contains("module Explore"),
            "must not re-export broken: {out}"
        );
        assert!(
            !out.contains("import Explore"),
            "must not import broken: {out}"
        );
        // Healthy modules survive, in order.
        assert!(out.contains("module Schemes"));
        assert!(out.contains("module Lsp"));
        assert!(out.contains("import Schemes"));
        assert!(out.contains("import Lsp"));
        let schemes = out.find("Schemes").unwrap();
        let lsp = out.find("Lsp").unwrap();
        assert!(schemes < lsp, "order preserved: {out}");
        // Provenance comment names the excluded module.
        assert!(
            out.contains("Excluded (failed to compile): Explore"),
            "{out}"
        );
        // It is a compilable module header.
        assert!(out.contains("module Library\n  ( "), "{out}");
    }

    #[test]
    fn sanitized_library_all_broken_is_empty_but_importable() {
        let out = sanitized_library_source(&[], &["A".to_string(), "B".to_string()]);
        assert!(out.contains("module Library () where"), "{out}");
        assert!(!out.contains("import A"));
    }

    #[test]
    fn brick_note_names_culprits() {
        let note = brick_note(&["Explore".to_string(), "Dev".to_string()]);
        assert!(note.contains("Explore"));
        assert!(note.contains("Dev"));
        assert!(note.contains("EXCLUDED"));
    }

    #[test]
    fn snapshot_hash_changes_when_a_lib_file_changes() {
        let dir = std::env::temp_dir().join(format!(
            "tidepool-libiso-snap-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("A.hs"), "module A where\na = 1\n").unwrap();
        let dirs = vec![dir.clone()];
        let h1 = lib_snapshot_hash(&dirs);
        // A no-op re-read is stable.
        assert_eq!(h1, lib_snapshot_hash(&dirs));
        // Editing a module changes the snapshot.
        std::fs::write(dir.join("A.hs"), "module A where\na = 2\n").unwrap();
        assert_ne!(h1, lib_snapshot_hash(&dirs), "edit must bust the snapshot");
        // Adding a module changes the snapshot.
        let h2 = lib_snapshot_hash(&dirs);
        std::fs::write(dir.join("B.hs"), "module B where\nb = 1\n").unwrap();
        assert_ne!(h2, lib_snapshot_hash(&dirs), "add must bust the snapshot");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn healthy_set_is_reexports_minus_broken() {
        let reexports: Vec<String> = ["Schemes", "Explore", "Lsp"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        let broken: HashSet<String> = ["Explore".to_string()].into_iter().collect();
        let healthy: Vec<String> = reexports
            .iter()
            .filter(|m| !broken.contains(*m))
            .cloned()
            .collect();
        assert_eq!(healthy, vec!["Schemes".to_string(), "Lsp".to_string()]);
    }
}
