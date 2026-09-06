# Scoped Shoal CLI integration request (root/service owned)

No shared launch file was edited by this lead. Against current
`tidepool/src/bin/shoal.rs`, add a `RunMap` subcommand with these fields:

```rust
RunMap {
    run_dir: PathBuf,
    #[arg(long)] from_unix_ms: Option<u64>,
    #[arg(long)] until_unix_ms: Option<u64>,
    #[arg(long)] json: bool,
},
```

Its existing command dispatch should perform only:

```rust
let report = tidepool::run_map::read_windowed_run(
    &run_dir,
    tidepool::run_map::Limits::default(),
    tidepool::run_map::TimeWindow { from_unix_ms, until_unix_ms },
)?;
if json {
    println!("{}", serde_json::to_string_pretty(&report)?);
} else {
    println!("{}", report.concise());
}
```

Use the binary's existing error return type. Compile that binary and execute a
sanitized fixture for `shoal run-map RUN_DIR --json`, bounded-window flags, and
invalid reversed-window failure. Do not call host initialization, pick a newest
run implicitly, resolve an installed provider, or modify tmux. Window bounds are
UTC Unix milliseconds inclusive start/exclusive end. Untimed events remain
explicitly unclassified; actor inventory itself is not time-filtered. The
representative example already exercises this exact reader and flag mapping.

This increment still reports usage Unknown until the existing backend usage
owner supplies bounded per-response records. Do not advertise full product
acceptance, pricing, review/integration edges, or a new dashboard.

## Owning decoder exposure required before acceptance

Source inspection found the existing version-enforcing status decoder is private:
`tidepool/src/shoal.rs::decode_run_status(bytes)`. The increment currently
projects the existing RunStatus serde shape but cannot call that private owner.
Please expose this function as `pub(crate)` (library-internal, no external API),
then return the baseline so run_map/metadata.rs can replace its
`serde_json::from_slice(&bytes).ok()` with
`crate::shoal::decode_run_status(&bytes).ok()`. Add an unsupported-status-version
negative test then. This is a concrete owning-file aperture, not permission to
copy STATUS_VERSION or introduce another version policy. Until incorporated,
status-version enforcement is a known acceptance gate on this candidate.
