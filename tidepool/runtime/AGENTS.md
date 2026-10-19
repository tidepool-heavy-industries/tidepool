# Contributor workflow

For this crate's ownership boundaries and invariants, see [CLAUDE.md](CLAUDE.md).

Use focused native counted checks, such as
`just test-lib tidepool-runtime --exact FULL_TEST_NAME --expected-count 1` or
`just test-target tidepool-runtime session --exact FULL_TEST_NAME --expected-count 1`. Compile changed
consumers and exercise relevant rejection, uncertainty, cancellation, and stale
settlement paths.

Recovery controls must persist the interface closure from the issued compiler
context. Native products and their own canonical companions do not enumerate
interface-only dependencies. When narrowing a recovery fixture, pair a successful
complete-closure round trip with a typed refusal after omitting a required
interface; do not infer closure completeness from native owner counts.
