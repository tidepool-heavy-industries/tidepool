# Runtime Haskell source isolation

## Observed failure

Wave 13's running compiler rejected `Tidepool.Command` after the checkout's
Haskell library changed: its old generated `Tidepool.Effects.Core` did not
declare `CommandQueueWait`. The integration was reverted without restarting
the live compiler. The incident and retained evidence are in [actions.md](actions.md).

This is a source selection failure for a live development run. It does not by
itself show that the compiler daemon changed version. A run must use a Haskell
library captured for its own compiled runtime, even while a newer checkout is
built for another run.

## Current production path

1. `bridge/facade/build.rs` emits empty stdlib and actor bundles when
   `TIDEPOOL_EMBED_HASKELL` is unset. It deliberately does not watch Haskell
   source files in that mode, so the dev binary carries neither their bytes nor
   a build time source identity. Release/install builds embed both trees.
2. `bridge/facade/src/haskell_sources.rs::ensure_embedded_stdlib` resolves the
   mutable checkout stdlib for a dev binary. `ensure_exomonad_haskell` likewise
   resolves the mutable actor library. `source_identity` hashes their current
   source trees, but a dev binary has no earlier identity to compare with.
3. `bridge/facade/src/exomonad/workspace.rs::FrozenWorkspace::load` records
   `library_identity` and captures project source roots. It neither captures
   the Tidepool stdlib nor the actor library. On host load it compares the
   recorded identity to the *current* checkout; a live host performs no such
   check before later compiles.
4. `bridge/facade/src/actor_host.rs::driver_sources` calls
   `ensure_embedded_stdlib` for each driver compile. The recipe check in
   `bridge/facade/src/actor_host/recipe_checks.rs` does the same. Candidate
   source checks also pass through `compile_driver`. These consumers can read
   edited checkout files while the host and compiler remain running.
5. `tidepool-mcp::ensure_effects_module` generates Core and the row shim from
   declarations compiled into the running Rust binary. Its
   `write_module_dir` selects a source-hashed directory, so old and new Core
   sources have different paths and can coexist. The old host can therefore
   pair its old Core with a newly edited, checkout-backed stdlib.

The general toolchain's `TIDEPOOL_PRELUDE_DIR` precedence and deploy stamp in
`tidepool/toolchain/src/toolchain.rs` govern compiler/stdlib discovery and
installation compatibility. They do not freeze a live run's include roots.
The resident compiler endpoint and its producer identity remain a separate
compatibility boundary; a stdlib snapshot alone does not prove that a newly
started or replaced dev compiler matches an old host.

## Proposed owner change

1. Give dev Exomonad binaries a build-bound identity for the complete stdlib
   and actor Haskell source trees. `build.rs` must rerun when those sources
   change and emit the identity even when it does not embed their contents.
   This intentionally adds a facade rebuild/relink after a Haskell library
   edit; otherwise an old binary launched *after* the edit could accept the
   wrong library as a fresh snapshot. Release builds already bind the bytes by
   embedding them.
2. At run admission, compare the selected source trees with that build-bound
   identity. Reject a mismatch before creating or loading a run. Capture both
   trees through the existing `FrozenWorkspace` source capture owner, verify
   the captured files against the same identity, then publish the selection
   manifest. A capture interrupted by an edit must fail or retry, never
   publish mixed bytes. Record the capture format version and decide explicitly
   how pre-snapshot run manifests fail or migrate on recovery.
3. Pass the run's captured stdlib and actor include roots to driver, reload,
   recipe check, and child machine compiles. Keep the generated Core/shim
   paths from the running binary's source-hashed materializer. Do not alter
   the general toolchain's operator override for unrelated Tidepool commands.
4. Keep extractor/worker producer validation as its own gate. A host recovery
   with a different binary or compiler must reject an incompatible run even
   when the run's Haskell source snapshot remains intact.

The existing content-addressed bundle materializer could instead hold a
runtime copy shared by runs. That reduces duplicate disk use, but needs a
retention rule so cache cleanup cannot remove a live run's include root. A
run-owned capture uses the existing manifest and file verification with no new
global cache owner. Embedding all Haskell sources in dev binaries is simpler
for matching, but forces a larger relink and retains the full trees in every
binary. Merely copying checkout files at host startup does not establish a
match between a dev binary and those files at admission.

## Required focused checks before integration

- Build an old dev binary, edit a library module, and verify a new admission
  fails until that binary is rebuilt. Rebuild and verify its new identity is
  accepted. Check both stdlib and actor library edits.
- Start a run, then edit the checkout libraries. Compile a driver and a
  candidate reload against the run capture; both must still read the original
  bytes. The recipe check and a child machine must use the same include roots.
- Change an effect declaration between builds and verify the two generated
  Core paths differ while each run's stdlib remains paired with its own Core.
- Interrupt or mutate a source capture and verify no selection manifest is
  published. Verify a changed captured file fails the manifest check on host
  recovery. Exercise the chosen old-manifest migration behavior.
- Compile the facade, source reload, recipe check, and generated effects
  consumers with focused Nix-backed checks. No live daemon restart is needed
  for these fixtures.

No runtime implementation or live service was changed for this investigation.
