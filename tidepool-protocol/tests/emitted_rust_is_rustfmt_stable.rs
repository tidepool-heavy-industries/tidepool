//! Every emitted `.rs` file must be a FIXED POINT of rustfmt.
//!
//! This is an acceptance property, not a nicety. Generated `.rs` is subject to
//! `cargo fmt --all -- --check` like any
//! other source, so if the emitter's output is not already formatted, the format
//! gate and the golden gate fight each other: `cargo fmt` rewrites the file,
//! `generated_files_are_current` then declares it stale, and regenerating it
//! puts the fight back. There is no stable state.
//!
//! It is a TEST rather than a manual check for the reason every guard in this
//! program follows: a check nothing runs is not a check. `tidepool-protocol`
//! is outside `.config/nextest.toml`'s `default-filter` exclusion set and needs
//! no GHC, so a bare `cargo nextest run` reaches this.
//!
//! Scope is every emitted `.rs` for every DESCRIBED effect, not just the
//! migrated ones — `effects::all_described()` includes Worktree, whose wire and
//! adapter modules are the largest emitted files in the crate and are not on
//! disk yet. Waiting for the flip to discover a formatting drift in them would
//! discover it at exactly the wrong moment.

use std::io::Write;
use std::process::{Command, Stdio};

/// Format `src` with rustfmt at the workspace's edition, returning its output.
///
/// # Panics
/// Panics when `rustfmt` is absent or rejects the input. Both are hard failures
/// by design: `cargo fmt --all -- --check` is in this crate's verify list, so a
/// toolchain that cannot format is a broken toolchain, and source the formatter
/// REJECTS is source that does not parse — which is a much more serious finding
/// than a formatting difference.
fn rustfmt(src: &str) -> String {
    let mut child = Command::new("rustfmt")
        .args(["--emit", "stdout", "--edition", "2021", "--quiet"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect(
            "rustfmt must be on PATH — `cargo fmt --all -- --check` is part of this \
             crate's verify, so a toolchain without it cannot check the generated files",
        );
    child
        .stdin
        .as_mut()
        .expect("stdin was piped")
        .write_all(src.as_bytes())
        .expect("rustfmt accepted the source on stdin");
    let out = child.wait_with_output().expect("rustfmt ran to completion");
    assert!(
        out.status.success(),
        "rustfmt REJECTED generated source — it does not parse as Rust:\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8(out.stdout).expect("rustfmt emitted UTF-8")
}

#[test]
fn every_emitted_rust_file_is_a_rustfmt_fixed_point() {
    let effects = tidepool_protocol::effects::all_described();
    let files: Vec<_> = tidepool_protocol::gen::all_files(&effects)
        .into_iter()
        .filter(|f| f.path.ends_with(".rs"))
        .collect();

    // Guard the guard: a filter that silently matched nothing would make this
    // test vacuously green, which is the failure mode of every "assert over a
    // collection" test.
    assert!(
        files.len() >= 8,
        "expected at least the decl/handler/wire/adapter files plus three indexes, \
         got {}: {:?}",
        files.len(),
        files.iter().map(|f| &f.path).collect::<Vec<_>>()
    );

    let mut drifted = Vec::new();
    for f in &files {
        let formatted = rustfmt(&f.contents);
        if formatted != f.contents {
            drifted.push(f.path.clone());
        }
    }

    assert!(
        drifted.is_empty(),
        "these emitted files are NOT rustfmt fixed points, so `cargo fmt --all -- --check` \
         and `generated_files_are_current` would fight over them:\n  {}\n\
         Fix the EMITTER, not the file — the file is regenerated from it.",
        drifted.join("\n  ")
    );
}

/// As [`every_emitted_rust_file_is_a_rustfmt_fixed_point`], for the
/// suspension-decode roster ([`tidepool_protocol::effects::suspension_roster`])
/// MINUS `Ask` — a separate sweep because
/// [`tidepool_protocol::harness_generated_files`] is a disjoint file set from
/// [`tidepool_protocol::gen::all_files`] (see that function's doc), not
/// covered by the sweep above. `Ask` is covered by
/// [`every_runtime_decode_file_is_a_rustfmt_fixed_point`] instead.
#[test]
fn every_harness_decode_file_is_a_rustfmt_fixed_point() {
    let files = tidepool_protocol::harness_generated_files();

    assert!(
        files.len() >= 9,
        "expected at least one file per suspension-roster effect plus the mod index, got {}: {:?}",
        files.len(),
        files.iter().map(|f| &f.path).collect::<Vec<_>>()
    );

    let mut drifted = Vec::new();
    for f in &files {
        let formatted = rustfmt(&f.contents);
        if formatted != f.contents {
            drifted.push(f.path.clone());
        }
    }

    assert!(
        drifted.is_empty(),
        "these emitted harness decode files are NOT rustfmt fixed points:\n  {}\n\
         Fix the EMITTER (gen::suspension_req_rs), not the file.",
        drifted.join("\n  ")
    );
}

/// As [`every_harness_decode_file_is_a_rustfmt_fixed_point`], for the `Ask`
/// member emitted into `tidepool-runtime` instead.
#[test]
fn every_runtime_decode_file_is_a_rustfmt_fixed_point() {
    let files = tidepool_protocol::runtime_generated_files();

    assert!(
        files.len() >= 2,
        "expected at least the Ask decode file plus the mod index, got {}: {:?}",
        files.len(),
        files.iter().map(|f| &f.path).collect::<Vec<_>>()
    );

    let mut drifted = Vec::new();
    for f in &files {
        let formatted = rustfmt(&f.contents);
        if formatted != f.contents {
            drifted.push(f.path.clone());
        }
    }

    assert!(
        drifted.is_empty(),
        "these emitted runtime decode files are NOT rustfmt fixed points:\n  {}\n\
         Fix the EMITTER (gen::suspension_req_rs), not the file.",
        drifted.join("\n  ")
    );
}

/// The actor kernel owns its own generated decoder and therefore needs the
/// same fixed-point guard as the harness and runtime targets.
#[test]
fn every_actor_decode_file_is_a_rustfmt_fixed_point() {
    let files = tidepool_protocol::actor_generated_files();

    assert!(
        files.len() >= 2,
        "expected actor-owned decode files plus their mod index, got {}",
        files.len()
    );
    for f in files {
        assert_eq!(
            rustfmt(&f.contents),
            f.contents,
            "{} is not a rustfmt fixed point",
            f.path
        );
    }
}

/// The Worktree wire and adapter modules specifically, named rather than left to
/// the sweep above.
///
/// They are the two Worktree files, not yet on disk (Worktree is
/// deliberately absent from `effects::all()`), and they are the ones carrying
/// hand-shaped emitter output — a `match` body, a wrapped `use` list, a
/// multi-clause boundary-constructor condition. If the sweep above ever stops
/// covering them, this fails and says so.
#[test]
fn the_worktree_wire_and_adapter_modules_are_rustfmt_fixed_points() {
    let wt = tidepool_protocol::effects::worktree::worktree();
    for f in [
        tidepool_protocol::gen::wire_rs::file(&wt),
        tidepool_protocol::gen::adapter_rs::file(&wt),
    ] {
        assert_eq!(
            rustfmt(&f.contents),
            f.contents,
            "{} is not a rustfmt fixed point",
            f.path
        );
    }
}
