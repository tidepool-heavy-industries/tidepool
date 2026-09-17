# Initial scaffold verification — 2026-09-16

Worktree `research/jev-integration`, based on `00f8f3332`. All commands ran from
`/home/inanna/dev/tidepool-jev-integration`. Toolchain: repository development
shell, Rust 1.93.0, GHC 9.12.2. Builds used this worktree's `target/` directory.

| Command | Observed result |
| --- | --- |
| `bash scripts/dev-shell.sh cargo test -p jev-integration -- --test-threads=1` | Passed: 12 tests, zero failures/ignored/filtered tests. Includes synthetic response checks, credential handling, exclusive evidence creation, local HTTP errors/redirects, timeout, and bounded response capture. |
| `bash scripts/dev-shell.sh cargo clippy -p jev-integration --all-targets -- -D warnings` | Passed; binary and test targets checked without warnings. |
| `bash scripts/dev-shell.sh cargo fmt -p jev-integration` | Applied Rust formatting. |
| `bash scripts/dev-shell.sh bash jev-integration/haskell/check.sh` | Positive fixture compiled and executed successfully. Four negative fixtures failed as expected at their intended sites; diagnostics retained in `target/jev-haskell/`. |
| `bash scripts/dev-shell.sh cargo run -p jev-integration -- snapshot --output jev-integration/evidence/openapi-initial.json` | Binary compiled and executed; public endpoint returned HTTP 200, schema version 0.2.0, no transport failure. |
| `target/debug/jev-integration list` | Executed successfully; 34 named probes. |
| `target/debug/jev-integration show escaped-keys` | Executed successfully; rendered the Unicode/separator/quoted-key request offline. |
| `bash scripts/dev-shell.sh cargo run -p jev-integration -- show structured` | Binary rebuilt and executed offline. |
| `git diff --check` | Passed. |

The public snapshot's harness fingerprint is
`6a05262eb9aad1c432ac38c698dc1a9dbd122643aad883970c87ebd87ee887be`.
That capture precedes the final Clippy correction and additional body-limit test;
it is evidence of public connectivity, not an exact-final-source live check.

The negative fixtures establish distinct scope rejection, wrong payload rejection,
nominal-role rejection of `coerce`, and rejection of missing record handlers with
`-Werror=missing-fields`. Compiler diagnostics were inspected; these were not
failures caused by missing dependencies. No Tidepool JIT behavior was tested.

Subsequently, all 34 authenticated probes ran once using the compiled harness:
21 HTTP 200, three HTTP 400, ten HTTP 422, no transport failures. All successful
responses passed provisional interpretation checks. See [CONTRACT.md](CONTRACT.md)
for results and capture provenance. No broad workspace batteries ran and no
production engine, protocol, handler, or actor behavior was changed.

A second constructor-shape matrix then ran 29 probes: 16 HTTP 200, four HTTP
400, nine HTTP 422, and no transport failures. This covered required request
fields, empty values and identifiers, recursive description forms, discriminator
validation, mixed-request atomicity, and 255/256-question maps. Both large maps
returned the exact number of corresponding answers. An undocumented
`bounding_box` tag was recognized but account-disabled and is excluded from the
current DSL scope. After extending the harness, the focused Rust test suite still
passed all 12 tests.
