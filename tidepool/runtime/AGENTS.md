# Contributor workflow

For this crate's ownership boundaries and invariants, see [CLAUDE.md](CLAUDE.md).

Use focused Nix-backed checks, such as
`just test-lib tidepool-runtime 'test(<name>)'` or
`just test-target tidepool-runtime session 'test(<name>)'`. Compile changed
consumers and exercise relevant rejection, uncertainty, cancellation, and stale
settlement paths.
