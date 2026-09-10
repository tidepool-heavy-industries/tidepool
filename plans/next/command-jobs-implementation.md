# Command jobs implementation

Implement the agreed Haskell command workbench on main, with a matched native
Codex revision. Product engine/applications branches and running swarms retain
their existing runtime. This file tracks implementation, not shipped guidance.

## Accepted design

- Inspectable command values, literal `[bash|…|]`, default 256 MiB; the limit is
  also the admission weight. Native shell fallback is fixed at 256 MiB.
- Every accepted Haskell command has a lightweight Rust job actor before
  admission. It survives tool returns, drives the existing native process owner,
  and publishes completion through existing typed actor sources. Native fallback
  retains its existing process-session handle during deferred admission.
- Support batch commands, stdin, PTY, cancellation and bounded output reads.
  `start` accepts immediately; `run` waits up to one second then returns either
  the result or job; `await` explicitly waits. No automatic execution retry.
- A per-user resource service shares 8 GiB general capacity and 512 MiB
  protected small-command capacity across runs. General admission is FIFO;
  requests at most 256 MiB can use the protected capacity. Aggregate swap 1 GiB.
- Queues have no implicit lifetime timeout. Transport loss does not cancel a
  retained job. Grants remain held until all descendants are gone.
- Cap the existing Nix daemon at 8 GiB memory / 1 GiB swap, one build / two cores.
  This contains daemon work in aggregate; it does not attribute it to clients.
- Remove the JavaScript tool wrapper for Shoal, retain direct tools and
  apply_patch. Rewrite prompting/examples and add a focused command skill.
- No engine refactor, transactional editing, output-stream subscription API,
  or automatic broad dogfood launch is included.

## Implementation checkpoints

- [x] Weighted resource admission and retained requests; focused failure tests.
- [ ] Shared service, delegated cgroups, Nix containment and launch integration.
- [x] Rust job actors and native command bridge, including fallback tools.
- [x] Generated Commands effect, Haskell command values and actor completion.
- [x] Native tool exposure, prompts, skills and executable examples.
- [x] Matched model-free native/resident/cgroup acceptance; formatted final diff.
- [ ] Main commits, matched native pin, frozen runner and launch package.

Use the smallest owning checks and compile changed consumers. Acceptance must
exercise real TUI/namespace/cgroup boundaries, delayed admission, process OOM,
cancellation races, descendant custody, completion before subscription, multiple
hosts, argument fidelity and output bounds. No paid inference or full suites.

## Source selection

Initial main: `badb46615322b5f097eee342c4c5b5e88aecd407` in
`/tmp/tidepool-rsi-main-20260908`. Native continuation:
`/tmp/codex-command-jobs-20260910`, branch `shoal-command-jobs-20260910`, starting
from pinned `fe15831c8a22c0d1b8d78d5ce55b7aa5fc3fa666`.
Matched native continuation is committed and pushed at
`7259e93777a0c3a323ce8ad6911b836eb1b73d37`; main's flake pins that revision.

## Current verification

- Real delegated-cgroup OOM, sibling survival, queued cancellation, descendant
  custody and disconnected-observer/shared-client admission checks passed.
- The OOM-to-CoW publication boundary passed with an explicit 64 MiB job weight:
  OOM releases writable handles, publication succeeds and the child writes an
  independent overlay while the original worker remains usable.
- All three resident command checks passed: cancellation before backend attachment,
  early/late exactly-once record actor completion, composed environment overrides,
  argument fidelity, and execution of the command skill's actual code blocks.
- Native deferred cancellation, stdin closure, lossless output and
  root-exit/descendant-pipe lifetime tests passed. The matched native binary built.
- Full-TUI scripted-provider acceptance passed (438.89 s): native OOM and
  subsequent steering, queued admission, protected small commands, stdin/arguments,
  command OOM, cancellation, bounded output, late record-actor completion and
  inherited/resized terminal dimensions. The actual native skill discovery path
  and direct-tool exposure were checked in the provider request. Test binaries
  were retained independently of Cargo output replacement. No paid model calls.
- The shared API guide's executable success/unavailable example passed (619.03 s).
- Generated files and skill format validated. Focused Clippy completed with existing
  warnings outside the new command implementation; final formatting/diff review,
  matched source pins and frozen release package remain.
- Runtime Nix limits verified: MemoryMax=8589934592,
  MemorySwapMax=1073741824, CPUQuotaPerSecUSec=2s. Declarative changes are prepared
  in /etc/nixos/configuration.nix; operator is holding nixos-rebuild until notified
  that current builds are clear. One-build/two-core defaults await activation.
