# Spec: rename cabal package `tidepool-harness` → `tidepool-extract`

Frees the `tidepool-harness` name for the new Rust product crate. The cabal
package is internal extract machinery; its flake output and binary are
ALREADY named `tidepool-extract`/`tidepool-extract-bin`, so this rename
makes the Haskell side self-consistent.

## ANTI-PATTERNS (read first)

- DO NOT rename the binary (`tidepool-extract-bin` stays) or the flake
  output (`packages.tidepool-extract` stays). Only the cabal PACKAGE name
  and the file name change.
- DO NOT touch `dist-newstyle/` or commit anything from it.
- DO NOT `git add -A` (repo rule; `tmp/` is protected scratch).
- DO NOT change any Haskell source semantics — this is a pure rename.

## READ FIRST

- `haskell/tidepool-harness.cabal` (the file being renamed)
- `flake.nix` lines ~109–149 (`packages.tidepool-extract` — check how the
  cabal package is referenced; the haskell-packages override may key on the
  package name)
- `haskell/CLAUDE.md`, `scripts/battery.sh`, `scripts/redeploy.sh` — grep
  for `tidepool-harness`
- `haskell/cabal.project`

## STEPS

1. `git mv haskell/tidepool-harness.cabal haskell/tidepool-extract.cabal`;
   change the `name:` field inside to `tidepool-extract`.
2. Grep the whole repo for `tidepool-harness` (excluding plans/ and git
   history): update flake.nix package references, cabal.project if it names
   the package, scripts, docs. Each hit is either updated or justified in
   the PR description.
3. Rebuild: `cd haskell && cabal build tidepool-extract-bin`.

## VERIFY

- `cd haskell && cabal build tidepool-extract-bin` succeeds; `cabal
  list-bin tidepool-extract-bin` resolves.
- `nix build .#tidepool-extract` succeeds (flake builds only TRACKED files
  — `git add` before building).
- `scripts/battery.sh` green.

## DONE

Repo-wide grep for `tidepool-harness` hits only plans/ docs and the (not
yet created) Rust crate. Battery green. One commit.
