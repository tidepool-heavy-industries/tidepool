# Contributing

Start with the [contributor guide](AGENTS.md). It owns repository workflow,
verification commands, architecture boundaries, and subsystem ownership.

## Build and dependency notes

The lockfile graph measured with `cargo tree --locked -e normal` points to
replacing `ureq` in `tidepool-handlers` with the `reqwest` already used by the
workspace as the next HTTP/TLS optimization target; aligning the two `reqwest`
major versions saves only one crate compile. Revisit the source change when
that dependency work is in scope.
