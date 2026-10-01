# Release preparation handoff

Approved scope: define the deployment boundary for the bounded model-turns
release-preparation parcel. This is an ownership handoff, not an implementation
or activation proposal. It adds no release registry, manifest format, selector,
or second deployment path before the package and run owners expose the inputs
those mechanisms must consume.

## Current owners and evidence

- The current Nix producer in `flake.nix` exposes `.#exomonad`, which wraps the
  `exomonad-unwrapped` binary with the extractor and embedded web assets, and
  `.#tidepool-extract`. These are the matched package inputs described by the
  approved plan.
- `scripts/redeploy.sh` owns today's mutable install path: it builds or installs
  the extractor and server binaries into user profiles, then writes the
  toolchain deploy stamp. Staged selection must replace or extend this owner;
  a parallel script that independently owns a `current` pointer would create
  two deployment authorities.
- `bridge/facade/src/exomonad.rs` resolves the executable and compiler at run
  launch, copies `exomonad`, `tidepool-extract`, and the compiler worker into
  `runs/<id>/bin`, and records hashes beside those copies. `RunStatus` currently
  has no release identity or closure reference. The copies preserve executable
  bytes, but there is no durable run-to-release index or release GC root.
- Durable state has multiple owners. Actor journal recovery rejects unsupported
  versions with an explicit-migration error in
  `exomonad/actor/src/recovery.rs`; inbox state and migrations belong to
  `exomonad/node/src/inbox.rs`; durable JSONL versioning belongs to
  `tidepool/repr/src/{jsonl,version_ladder}.rs`. Release compatibility must be
  supplied by those format owners, not inferred from an executable version.
- The native Buck migration branch inspected read-only at
  `f381cf6c9ec192426df8df0664efdc47ee0bc8f7` generates build rules but does not
  yet publish a matched distributable release package. When that becomes the
  producer, the handoff must use its package interface rather than adding
  another Nix packaging owner.

## Required producer contract

The engine/package owner must publish one matched immutable release input that
identifies at least:

- the source revision and package producer identity;
- the Exomonad package closure and the exact extractor/compiler-worker pair it
  expects, plus embedded assets;
- content identities for the binaries and assets, and the immutable store paths
  needed to execute and resume them;
- a state compatibility declaration from the owners of every durable format
  the release reads or writes, including whether it can read existing state
  and whether an explicit forward migration is required.

The package owner owns package construction and identity. The deployment owner
consumes that contract; it must not reconstruct package identity from version
strings, ambient `PATH`, mutable profiles, or separately selected binaries.

## Required deployment and run contract

The owner extending `scripts/redeploy.sh` must make staging and selection one
deployment mechanism with these boundaries:

1. Stage packages by immutable identity. Validate the producer match, required
   executables/assets, hashes, and state compatibility before making a release
   selectable. A failed or interrupted stage leaves the selected release
   unchanged.
2. Retain each staged closure with a Nix GC root for as long as the release is
   selected or any retained run references it. Do not infer run liveness from
   process-manager state alone; use a durable run record and its existing
   lifecycle/cleanup owner.
3. Change selection atomically. New runs resolve the selected release exactly
   once, pin its release identity and immutable paths in their durable run
   record, and launch from that release. Resuming a run uses its recorded
   release regardless of the current selection.
4. Retain run-referenced closures until the run owner has authoritatively
   retired the run and released its reference. A crash or uncertain cleanup
   keeps the reference. GC-root release follows the durable reference update.
5. Rollback changes selection for future runs only. It must validate that the
   selected older release can read the existing durable state. It never
   restores an older database snapshot, runs a reverse migration, or silently
   downgrades a database. If compatibility cannot be proved, selection fails
   and the operator needs a separately owned forward migration or recovery
   decision.

The run owner is `bridge/facade/src/exomonad.rs` and its durable `RunStatus` /
run lifecycle. The release selector must not add a competing run registry. The
durable state owners named above must declare compatibility and retain their
existing migration entry points. The deployment owner coordinates the atomic
selector, run pin, and GC-root reference updates across those interfaces.

## Handoff sequence

1. Package owner publishes the matched immutable package contract and a
   disposable package fixture; do not change the active package producer from
   the deployment parcel.
2. Run owner adds a durable release pin at launch and uses it on resume, with
   authoritative release-reference retention and cleanup.
3. State owners define read compatibility and explicit forward migration
   behavior for their formats. No release selector may substitute for these
   declarations.
4. Deployment owner stages, validates, roots, selects, and rolls back the
   package contract atomically, with fixture coverage for stage failure,
   interrupted selection, run retention, cleanup, resume pinning, incompatible
   rollback refusal, and no database downgrade.
5. Integrate only after the package, run, state, and deployment interfaces are
   reviewed together. This handoff does not authorize production activation,
   daemon restart, or changes to the active release.

Until these interfaces exist, use the existing `just exomonad-build` / matched
package producer for builds and keep `scripts/redeploy.sh` as the sole current
deployment owner. Do not add a mock selector or standalone manifest format
whose only consumer is its own test.
