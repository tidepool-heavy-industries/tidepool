# Contributor workflow

For this crate's ownership boundaries and invariants, see [CLAUDE.md](CLAUDE.md).

Use focused native counted checks, such as
`just test-lib tidepool-runtime --exact FULL_TEST_NAME --expected-count 1` or
`just test-target tidepool-runtime session --exact FULL_TEST_NAME --expected-count 1`. Compile changed
consumers and exercise relevant rejection, uncertainty, cancellation, and stale
settlement paths.
