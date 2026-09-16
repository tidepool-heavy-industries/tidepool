# Jev integration laboratory

A focused **member of Tidepool's Cargo workspace** for TypeSafe API experiments.
It has no Tidepool engine dependencies. The eventual Haskell effect remains a
single operation; this executable is a research consumer, not a production SDK.

Run commands from this worktree's repository root. Cargo uses the workspace
lockfile and this worktree's `target/` directory. Build only this package:

```sh
bash scripts/dev-shell.sh cargo test -p jev-integration -- --test-threads=1
bash scripts/dev-shell.sh cargo run -p jev-integration -- list
bash scripts/dev-shell.sh cargo run -p jev-integration -- show structured
```

`list` and `show` are offline. `run` performs one authenticated attempt with no
automatic retry or redirect following. Requests are synthetic and deliberately
include invalid/disputed forms. Do not use these raw JSON builders as the future
validity-preserving DSL. Each invocation selects one of 82 probes; there is no
automatic matrix run or batching scheduler. One discovery probe names an
undocumented, account-disabled `bounding_box` discriminator; it is retained as
evidence but excluded from the current text/glue DSL scope.

The 14 `shoal-*` probes exercise routing, attention, investigation expansion and
folding, evidence selection, question relationships, repair routing, and experiment
selection. See [expectations and live results](SHOAL-EXPERIMENTS.md).

Five `world-*` probes explore structured agent graphs, contract-sensitive delegation,
and joint versus per-piece evidence selection; see [results](WORLD-EXPERIMENTS.md).

`simulate <scenario> --output-dir <new-directory>` runs bounded dependent Jev
calls over synthetic tool observations. Selections choose the next fixture; no
shell commands execute. The initial four are documented in
[linked simulation results](NOTEBOOK-SIMULATIONS.md). Ten Bash/LSP scenarios with
paired absence cases are documented in [Bash/LSP experiments](BASH-LSP-EXPERIMENTS.md).

`frontier --output-dir <new-directory>` probes Jev's useful decision boundary:
wide conjunctive selection, semantic graph paths, temporal exceptions,
overlapping judgments, and exact pointer traversal. The observed result is a
sharp distinction between strong semantic width and unreliable deterministic
depth; see [frontier experiments](FRONTIER-EXPERIMENTS.md).
Use `--only <case-name-fragment>` for focused repetitions. Later experiments
showed 640-question fan-out and a seven-answer Shoal microprogram succeeding in
one call, while also locating shared-state and duplicate-Choice-label limits.

## Live experiments

Supply `TYPESAFE_API_KEY` through the process environment, not a CLI argument or
tracked file. The CLI does not load dotenv files or print the key. Then:

```sh
mkdir -p jev-integration/evidence
bash scripts/dev-shell.sh cargo run -p jev-integration -- run structured \
  --model jev-latest --output jev-integration/evidence/structured-001.json
```

Default timeout is 15 seconds for the attempt, including body reading. Override
with `--timeout-ms`. Response capture is bounded to 2 MiB. Start with `structured`
as the positive control, then run individual cases from
[the contract matrix](CONTRACT.md). A 401/429/529 or transport failure does not
resolve a schema question. Repeat with a new evidence filename when appropriate.

The evidence file is exclusively created **before** sending the request; existing
files are never overwritten. Its parent directory must already exist. Unix file
mode is 0600. `evidence/` is git-ignored. Evidence contains:

- Request JSON and endpoint, timestamp, source/lockfile BLAKE3 fingerprint.
- HTTP status, elapsed time, response bytes, and typed transport failure if any.
- Parsed response JSON and a separate provisional typed interpretation.
- A flag if literal or JSON-escaped credential echoes were redacted from the body.

`exchange.body` is an array of bytes to preserve non-UTF-8 and non-JSON responses.
For a body limit or read failure it is a prefix, not a complete response. Outgoing
headers and raw transport error strings are not captured. Credential-echo
redaction is defensive, not a general-purpose sensitive-data scrubber.

Exit codes: 0 for successful HTTP and no interpretation findings; 2 for captured
HTTP/transport failure or interpretation findings; 1 for setup/storage failure.
Interpretation findings are research diagnostics, not final validity rules.
Distribution-sum and Score-mean tolerances are provisionally 0.01. Full raw JSON
retains unknown fields even when the typed projection does not know them.
Interrupted/killed processes may leave an incomplete evidence file; these are not
completed observations. Service-side completion after a timeout is unknown.

Public OpenAPI retrieval requires no key and performs no inference:

```sh
bash scripts/dev-shell.sh cargo run -p jev-integration -- snapshot \
  --output jev-integration/evidence/openapi-next.json
```

## Haskell feasibility sketches

```sh
bash scripts/dev-shell.sh bash jev-integration/haskell/check.sh
```

This compiles and runs a small mode-interpreted record example and verifies four
expected compiler rejections. The sketch supports structured description records,
heterogeneous local payloads, recursive keyed question collections, and
result-owned existential candidate identity. It uses an internal synthetic result;
there is no generic wire codec, Jev effect, or resident execution in this sketch.

`matchChoice` consumes an exhaustive handler record without a caller-visible
scope. `withChoice` exposes paired candidate/distribution evidence only when an
operation needs it. Nominal role annotations prevent bypassing scope separation
with `coerce`. Ordinary Haskell missing record fields remain partial unless
`-Werror=missing-fields` is enabled: the final DSL must settle that authoring
contract or use a total builder. Positive tests use `-Wall -Werror`.

See [the design handoff](../plans/jev-dsl.md) for user decisions and remaining
questions. The TypeSafe skill's live-doc workflow informed the probe matrix; its
batching suggestions are not part of this integration's design.

The publisher's TypeSafe skill is vendored under
[`vendor/typesafe-ai`](vendor/typesafe-ai/UPSTREAM.md) at a recorded upstream
commit, including its MIT license. It is a research input and agent guide, not
runtime code. Its repeated recommendation is the same boundary found in live
tests: code owns exact work and execution; Jev contributes semantic judgments.
