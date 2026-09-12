# M1 prepared-STG checkpoint

Operator hold stopped M1 at a coherent, compiled and process-probed boundary.
This is a partial candidate, not M1 completion, schema freeze, or permission for
an application consumer.

## Custody and incorporated source

- Accepted scaffold/task source:
  `7c0354a570878eff14ce11b5e56c45ea397168cc`.
- Branch:
  `shoal/parallel-dogfood-engine/first-implementation-wave/branches/m1-ghc-handoff`.
- Saved M1 source `63677698041d54366ab77dff50de730dec550843`
  was adapted as commits `3169ee22`, `44575306`, `7fdefdf8`,
  `83f319c4`, `2bc1ebf0`, `b5b03dce`, and `04b8d5a1`.
- The checkpoint adds the required native GHC ordering: `hscTidy` produces
  `CgGuts` before CorePrep, and CorePrep/STG use the session's
  `interactiveInScope`. This repairs the real probe's out-of-scope worker
  binder without parsing dumps or changing the frozen worker socket.
- Checked source/evidence checkpoint: `9836dfc998628097cd2db3f455851288bca8c411`.
- No child commits are merely pending or unmerged in this worktree. No merge to
  main was attempted.

The retained untracked `.shoal/` is authoritative runtime state and was not
modified. `haskell/.cabal-m1/` is the candidate-only Cabal build cache. No
build or candidate daemon remains active. The existing live compiler sockets,
other worktrees, TUIs, and caches were not stopped, deleted, or redirected.

## Checked boundary

The inherited toolchain was verified as GHC 9.12.2 and cabal-install 3.16.0.0.
After the environment RSI, Cabal was invoked directly from `haskell/` with
`--builddir=.cabal-m1`; no further `nix develop` was attempted.

- `cabal build tidepool-extract-bin --builddir=.cabal-m1`: passed.
- `cabal test prepared-stg-pipeline-test execution-inventory
  --builddir=.cabal-m1 --test-show-details=direct`: both suites passed (one
  case each). The prepared suite covers direct and resident cold/warm,
  request-local invalid source, and post-failure recovery.
- Candidate process probe using
  `.shoal/build/cargo/debug/tidepool-extract`, the candidate Cabal worker,
  `TIDEPOOL_PREPARED_STG_PROBE=1`, and
  `haskell/test/prepared-stg/PreparedStgProbe.hs`: passed. It compiled three
  modules, retained 81 top-level bindings, and wrote a 12,504-byte structured
  inventory at
  `/tmp/tidepool-m1-direct-final.lC9uPa/prepared-stg.inventory`.
- A prior isolated candidate resident daemon probe (unique temporary socket)
  passed cold and warm prepared requests, returned structured
  `source-failure` for invalid source, recovered on the next request, and
  produced byte-identical cold/warm/recovery inventories. That candidate
  process was terminated by its own cleanup trap; no frozen live socket was
  touched.
- `git diff --check`: passed at the final checkpoint.

Observed failures are preserved rather than recast as success:

- Before the ordering repair, the direct probe failed STG lint because
  `$WCountProbe` was out of scope. The owning GHC 9.12.2 code path tidies
  before CorePrep; following that sequence fixes the probe.
- The probe still reports the existing `embeddedNulString` UTF-8 decode as
  `SKIPPED`; this candidate does not claim support for it.
- An attempted pre-CorePrep typed-site elaborator compiled but its exact
  Tidepool-module test observed zero sites because an OPAQUE surface Id exposes
  no usable interface unfolding. That experiment and failing test were
  reverted before this checkpoint.

## Explicit M1 gaps and next run

M1 remains incomplete. Typed Tidepool suspension sites are still elaborated
only by the legacy flattened-Core translator. Prepared output therefore cannot
yet freeze a site-aware schema or be consumed by applications A7. The prepared
handoff also lacks reviewed support decisions for imported promises, capture
and tag facts, primitive/foreign signatures, constructor layouts, and the
legacy embedded-NUL failure.

The next run should begin from this exact checkpoint and keep the shared
`CgGuts`/inventory contract fixed. One implementation owner should make
typed-site elaboration a pre-CorePrep pass using exact sibling `Id`s collected
from tidied home-module `CgGuts` in dependency order; do not reconstruct names
or depend on OPAQUE unfoldings. A related test owner may independently extend
the adjacent prepared-pipeline fixture with exact Tidepool module/name calls
and direct/resident invalid-source recovery. Join those only after the owner
proves the rewritten call and typed sidecar in the real worker process. A fresh
review owner should then audit identity stability against the legacy site-ID
algorithm and the actual prepared inventory. M3/schema and later waves remain
frozen.
