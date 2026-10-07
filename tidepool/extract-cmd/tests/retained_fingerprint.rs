//! Manual matched probe of production prepared-interface construction.
//!
//! Run in the repository Nix shell with this checkout's `tidepool-extract`
//! frontend and Haskell worker selected, and `TIDEPOOL_TIMING=1`.
//! The typed ExtractCmd builder owns request encoding and producer binding.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "manual measurement fixture asserts its prepared environment"
)]

use std::fs;
use std::path::{Path, PathBuf};

use tidepool_extract_cmd::{ExtractCmd, SymbolIdentity};

fn fixture_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/retained_fingerprint")
}

fn stdlib_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .join("bridge/haskell/lib")
}

fn run(
    cmd: &ExtractCmd,
    label: &str,
    producer: &mut Option<[u8; 32]>,
) -> tidepool_extract_cmd::ExtractRun {
    let endpoint = cmd.bind_direct().unwrap();
    let selected = *endpoint.identity().producer_bytes();
    if let Some(expected) = producer {
        assert_eq!(
            *expected, selected,
            "compiler producer changed during probe"
        );
    } else {
        *producer = Some(selected);
    }
    let output = endpoint.execute(cmd).unwrap();
    assert!(
        output.success(),
        "{label} failed:\n{}",
        output.stderr_lossy()
    );
    output
}

fn detail(stderr: &str) -> (u64, u64) {
    let matching = stderr
        .lines()
        .filter(|line| {
            line.starts_with("tidepool-timing-module-detail ")
                && line.contains("module=Tidepool.Session.Val.G3 ")
                && line.contains("phase=make_iface ")
        })
        .collect::<Vec<_>>();
    assert_eq!(matching.len(), 1, "expected one G3 make_iface timing line");
    let field = |key: &str| {
        matching[0]
            .split_whitespace()
            .find_map(|field| field.strip_prefix(key))
            .unwrap_or_else(|| panic!("missing {key} in G3 timing"))
            .parse::<u64>()
            .unwrap()
    };
    (field("wall_ns="), field("allocated_bytes="))
}

#[test]
#[ignore = "manual GHC measurement; requires the matched local extractor and worker"]
fn unrelated_retained_symbols_do_not_scale_g3_interface() {
    assert_eq!(
        std::env::var("TIDEPOOL_TIMING").as_deref(),
        Ok("1"),
        "set TIDEPOOL_TIMING=1 to emit per-module timings"
    );
    assert!(std::env::var_os("TIDEPOOL_EXTRACT_WORKER").is_some());
    assert!(std::env::var_os("TIDEPOOL_GHC_LIBDIR").is_some());
    let base = ExtractCmd::new().unwrap();
    let mut producer = None;
    let scratch = tempfile::tempdir().unwrap();
    let session = scratch.path().join("session");
    let val = session.join("Tidepool/Session/Val");
    let wave = scratch.path().join("Wave22FullCore");
    let command = scratch.path().join("Tidepool/Command");
    fs::create_dir_all(&val).unwrap();
    fs::create_dir_all(&wave).unwrap();
    fs::create_dir_all(&command).unwrap();
    fs::copy(fixture_root().join("G3.hs"), val.join("G3.hs")).unwrap();
    fs::copy(
        fixture_root().join("CommandTypes.hs"),
        command.join("Types.hs"),
    )
    .unwrap();
    let input = wave.join("W1.hs");
    fs::copy(fixture_root().join("W1.hs"), &input).unwrap();
    let stdlib = stdlib_root();
    assert!(stdlib.is_dir(), "matched runtime source library is missing");

    // Produce the thin G2 iface through the same worker that will consume it.
    // The historical G2 source is unavailable; this binder is only imported
    // by W1 and is not used by the G3 interface under measurement.
    let bind = scratch.path().join("bind.txt");
    fs::write(&bind, "__tidepoolJobCarrier <- pure (0 :: Int)\n").unwrap();
    let mut bootstrap = base.clone();
    bootstrap
        .input(&bind)
        .turn()
        .turn_template("bind", &fixture_root().join("Bind.hs"))
        .turn_verdict("bind:__tidepoolJobCarrier")
        .turn_out(scratch.path().join("bootstrap-turn.cbor"))
        .output_dir(scratch.path().join("bootstrap-out"))
        .session_root(&session)
        .bind_gen(2)
        .include(scratch.path())
        .include(&stdlib);
    run(&bootstrap, "G2 bootstrap", &mut producer);
    assert!(val.join("G2.hi").is_file());

    for count in [0, 1_000, 10_000] {
        let mut cmd = base.clone();
        cmd.input(&input)
            .target("result")
            .output_dir(scratch.path().join(format!("out-{count}")))
            .session_root(&session)
            .inject_val("Tidepool.Session.Val.G2")
            .include(scratch.path())
            .include(&stdlib)
            .include(&session);
        for index in 0..count {
            cmd.retained_generation(
                SymbolIdentity {
                    unit: "main".into(),
                    module: "Wave22Probe.Unused".into(),
                    namespace: "value".into(),
                    occurrence: format!("probe{index:05}"),
                    record_parent: None,
                },
                1,
            );
        }
        let output = run(&cmd, &format!("{count} retained symbols"), &mut producer);
        let (wall_ns, allocated_bytes) = detail(&output.stderr_lossy());
        eprintln!(
            "retained_fingerprint count={count} g3_make_iface_wall_ns={wall_ns} g3_make_iface_allocated_bytes={allocated_bytes} request_elapsed_ns={}",
            output.elapsed.as_nanos()
        );
    }
}
