# Native Buck build migration

The Buck graph is being introduced alongside the existing `just` workflows.
Cargo metadata and `Cargo.lock` remain authoritative for Rust package versions
and dependency edges. `scripts/buck2-first-party.py --package NAME` generates a
bounded set of native Rust targets; `scripts/buck2-reindeer.sh` generates the
locked third-party crate graph. Their `--check` modes fail when the committed
graph is stale. Do not wrap Cargo or Cabal workspace builds in opaque Buck
`genrule` targets.

Regenerate or verify the initial slice in the pinned development shell:

```sh
bash scripts/dev-shell.sh scripts/buck2-first-party.py \
  --package tidepool-atomic-write --package tidepool-repr
scripts/buck2-reindeer.sh
```

Add `--check` to either command to verify generated inputs without retaining
regenerated output.

The initial native slice is `tidepool-atomic-write` and `tidepool-repr`:

- `//bridge/atomic-write:tidepool_atomic_write_unit_tests`
- `//bridge/atomic-write:strict_directory`
- `//tidepool/repr:tidepool_repr_unit_tests`
- `//tidepool/repr:repr`

These are native Rust rules with crate/test sources and compile-time fixtures
as declared inputs. The `repr` fixture filegroups are owned by their source
packages. A Buck test result is evidence only for its selected target; all
other packages retain their existing `just` checks until migrated and accepted.

## Toolchain and output setup

The flake pins Buck2, Reindeer, Rust, GHC, C/C++, and the action support tools.
Materialize the declared action closure before configuring the checkout:

```sh
nix build --no-link .#buck-toolchain-closure
scripts/buck2-configure.sh
export PATH="$(awk -F ' = ' '$1 == "action_path" {print $2}' .buckconfig.local):/run/current-system/sw/bin"
```

`buck-out` must be a bind mount of a per-checkout directory on `/srv/build`; a
symlink is not supported. Check `findmnt --mountpoint "$PWD/buck-out"` before
any Buck invocation. Do not run Buck metadata queries or builds from an
unadmitted agent shell. For initial validation use the admitted build slice and
keep execution local while NativeLink gates remain pending:

```sh
buck2 build --local-only -c remote.enabled=false //tidepool/repr:tidepool_repr
buck2 test --local-only -c remote.enabled=false \
  //bridge/atomic-write:tidepool_atomic_write_unit_tests \
  //bridge/atomic-write:strict_directory \
  //tidepool/repr:tidepool_repr_unit_tests \
  //tidepool/repr:repr
```

Report the source OID, exact command, selected/executed test count, exit status,
and retained log. Building a test binary is not running it. Re-run once to
record a real action-cache hit, then change one selected source or fixture and
verify only its affected dependency closure rebuilds.

The inherited migration's Haskell rule groups worker sources under one library
target. Inspect the pinned Prelude's actual action graph before making any
module-granularity claim; a target count alone does not establish cache
granularity.
Generated worker artifacts, test fixtures, embedded browser assets, and web
`dist` outputs must remain separate declared actions with explicit source,
resource, and toolchain inputs before those surfaces move from their existing
Cabal/Nix workflows. Remote execution remains disabled until its independent
closure, isolation, reuse, and cancellation checks pass.
