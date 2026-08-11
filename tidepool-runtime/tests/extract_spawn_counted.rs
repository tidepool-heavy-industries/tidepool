//! The undercount that motivated `tidepool-extract-cmd`: a `run_turn` compile
//! spawns `tidepool-extract`, and the process-global spawn counter — the
//! extract-wave plan's done-criterion — could not see it, because the counter
//! lived in `tidepool-harness::compile` and only that crate's `compile_turn`
//! bumped it (`plans/post-restart/extract-manifest.md`, D-B).
//!
//! Its own test binary: [`tidepool_extract_cmd::extract_spawn_count`] is
//! PROCESS-GLOBAL, so a test asserting on it must not share a process with
//! anything else that compiles.
//!
//! Needs NO GHC and no extract toolchain — the spawn is a two-line shell
//! script that exits non-zero. The compile is allowed to fail; only whether
//! the spawn was COUNTED matters here. Run it with:
//! `cargo nextest run --ignore-default-filter -p tidepool-runtime -E
//! 'binary(extract_spawn_counted)'` (tidepool-runtime is excluded from the
//! quick tier wholesale — see `.config/nextest.toml`).

use std::os::unix::fs::PermissionsExt;

use tempfile::TempDir;
use tidepool_extract_cmd::{extract_spawn_count, reset_extract_spawn_count};
use tidepool_runtime::session::{
    run_turn, TemplateSelector, TurnClassification, TurnKind, TurnRequest, TurnTemplate,
};

#[test]
fn run_turn_increments_the_process_global_spawn_count() {
    let dir = TempDir::new().unwrap();
    let fake = dir.path().join("fake-extract");
    std::fs::write(&fake, "#!/bin/sh\nexit 1\n").unwrap();
    std::fs::set_permissions(&fake, std::fs::Permissions::from_mode(0o755)).unwrap();
    // Env mutation is safe: nextest runs each test in its own process.
    std::env::set_var("TIDEPOOL_EXTRACT", &fake);

    let session_root = dir.path().join("session");
    std::fs::create_dir_all(&session_root).unwrap();
    let templates = vec![TurnTemplate {
        kind: TemplateSelector::Expr,
        source: "module M where\nresult = {{TURN}}\n".to_string(),
    }];

    reset_extract_spawn_count();
    assert_eq!(extract_spawn_count(), 0);

    let req = TurnRequest {
        turn_text: "1 + 1",
        templates: &templates,
        include: &[],
        session_root: &session_root,
        inject_modules: &[],
        gen: 0,
        verdict: Some(TurnClassification {
            kind: TurnKind::Expr,
            binders: Vec::new(),
        }),
        target: None,
    };
    // The compile fails (the fake extract writes no output and exits 1) —
    // irrelevant: it RAN, so it cost a real spawn and must be counted.
    let _ = run_turn(req);

    assert_eq!(
        extract_spawn_count(),
        1,
        "a run_turn spawn must be visible to the process-global counter — \
         this invisibility is exactly the defect tidepool-extract-cmd fixes"
    );
}

/// A spawn that never launched paid no extract cost, so it must not count —
/// the same rule `tidepool-harness::compile` has always applied to its own
/// site, now enforced in one place for every site.
#[test]
fn a_failed_spawn_is_not_counted() {
    let dir = TempDir::new().unwrap();
    // A readable file (so binary RESOLUTION succeeds) that is not executable
    // (so the SPAWN fails) — the process never launched, so no extract cost
    // was paid.
    let unlaunchable = dir.path().join("not-executable");
    std::fs::write(&unlaunchable, "#!/bin/sh\nexit 0\n").unwrap();
    std::fs::set_permissions(&unlaunchable, std::fs::Permissions::from_mode(0o644)).unwrap();
    std::env::set_var("TIDEPOOL_EXTRACT", &unlaunchable);

    let session_root = dir.path().join("session");
    std::fs::create_dir_all(&session_root).unwrap();
    reset_extract_spawn_count();

    let req = TurnRequest {
        turn_text: "1 + 1",
        templates: &[],
        include: &[],
        session_root: &session_root,
        inject_modules: &[],
        gen: 0,
        verdict: None,
        target: None,
    };
    let err = run_turn(req).unwrap_err();
    assert!(
        matches!(err, tidepool_runtime::CompileError::Io(_)),
        "a spawn failure is an environment problem, got {err:?}"
    );

    assert_eq!(extract_spawn_count(), 0);
}
