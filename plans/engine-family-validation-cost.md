# Full family validation cost repair

Baseline: `761a2ba78caedd882eecb0a52f147d7e407352a5`, with the focused test
added but `DeclarationJoin.hs` unchanged. Both executables used the repository's
same Nix GHC 9.12.2, Cabal `-O1` profile and the identical benchmark source.
This is a measured comparison of the original and repaired full-family
validator, not a before/after measurement of the whole resident engine.

The public `validateRetainedFamilyInstances` API and production admission hooks
remain unchanged. A stable GHC Unique set now deduplicates exact axioms in
retained validation and interface inventory. GHC's coercion axiom equality is
Unique equality, so the same first original representative is preserved.
Injectivity lookup now consumes the existing complete package/home environment
pair, instead of filtering and rebuilding a whole environment for every
instance. GHC's branch comparison accepts self comparisons, including
polymorphic branches; package/home visibility and conflict detection remain
complete. See the pinned [CoAxiom equality implementation](https://raw.githubusercontent.com/ghc/ghc/ghc-9.12.2-release/compiler/GHC/Core/Coercion/Axiom.hs)
and [family lookup and injective branch comparison](https://raw.githubusercontent.com/ghc/ghc/ghc-9.12.2-release/compiler/GHC/Core/FamInstEnv.hs).

## Measurement method

Each executable compiled five individually legal original GHC fixture modules
once. They shared the original family Names in one GHC NameCache; mutable
home/package state was freshened between producers. Their immutable `FamInst`
graphs were then reused. Compatible and incompatible producers never imported
each other's equations. Generated producers included independent plain and
injective families, associated type/data equations, polymorphic injective
equations, and one injective family with many distinct equations.

Fixture compilation, GHC startup and first lazy payload forcing were outside
the measurements. Each of five samples measured five complete validation calls
through an IORef, using monotonic elapsed time and RTS `allocated_bytes` deltas.
Explicit GCs at sample boundaries made allocation snapshots complete; those
boundary GCs were outside elapsed time. Validation-triggered GCs remain inside
elapsed time. Rejected-case diagnostics were fully forced. Tables show median
cost per validation; allocations were stable across the five samples. Background
host activity was not isolated, so elapsed timings are supporting evidence;
the deterministic allocation reduction is the stronger result. Raw samples:
[before](evidence/family-validation-cost/before.csv),
[after](evidence/family-validation-cost/after.csv).

## Results

Independent injective families, one original equation per family:

| Families | Original ms | Repaired ms | Original MiB allocated | Repaired MiB allocated |
| ---: | ---: | ---: | ---: | ---: |
| 16 | 0.035 | 0.023 | 0.152 | 0.079 |
| 64 | 0.318 | 0.085 | 2.019 | 0.320 |
| 256 | 4.400 | 0.349 | 33.741 | 1.309 |
| 1,024 | 83.860 | 1.337 | 602.941 | 5.336 |

At 1,024 independent families:

| Scenario | Retained axioms | Original ms | Repaired ms | Original MiB | Repaired MiB |
| --- | ---: | ---: | ---: | ---: | ---: |
| Plain originals | 1,024 | 4.220 | 0.952 | 3.437 | 3.725 |
| Same exact plain originals repeated | 2,048 | 7.556 | 0.903 | 3.437 | 3.725 |
| Compatible plain pair | 2,048 | 18.047 | 1.982 | 6.943 | 7.572 |
| Conflicting plain pair | 2,049 | 17.768 | 1.903 | 7.048 | 7.678 |
| Injective originals | 1,024 | 83.860 | 1.337 | 602.941 | 5.336 |
| Compatible injective pair | 2,048 | 423.915 | 3.247 | 2,727.350 | 12.527 |
| Conflicting injective pair | 1,025 | 82.101 | 1.409 | 603.765 | 5.380 |

The Unique set adds approximately 8–9% allocation for plain-only validation,
while removing quadratic exact-axiom comparison CPU. Independent injective
families now scale approximately with the closure size rather than with the
number of repeated whole-closure environment rebuilds.

One shared injective family remains a meaningful limit. At 1,024 equations,
validation changed from 321.368 ms / 1,684.567 MiB to 197.277 ms / 740.826 MiB.
GHC still compares each equation with the other equations in that same family
to prove injectivity. This required all-pairs semantic work is distinct from
the eliminated repeated global environment construction. Avoiding it would
require an independently proved RHS discriminator or incrementally certified
pair obligations; this repair introduces neither another cache nor a weaker
full-closure check.

## Verification and reproduction

Both original and repaired executables passed 12 focused cases: original
closure, exact duplicates, compatible originals, shared package/home copies,
hidden and package-slot family conflicts, associated type/data conflicts,
injective result collisions, and polymorphic injectivity in both home and
package slots. Package-slot cases use actual compiled original axioms in
`FamInstEnv`, not a synthetic installed package. The repaired existing
`declaration-join-test` also executed and passed its persisted interface,
source-hidden fresh consumer, replacement/retraction, real dictionary
execution and four retained-family failure paths. The shared internal library,
worker, focused family test and declaration join test all compiled. Fixture
sources' intentional missing-associated-default warning is preexisting.

All jobs ran through `systemd-run --user --slice=tidepool-completion-build.slice`
with this checkout as working directory and `bash scripts/dev-shell.sh` as the
pinned environment entry point. The significant inner commands were:

```sh
cd bridge/haskell
cabal build family-consistency-test -j8
cabal run family-consistency-test --
# Save this original executable before changing DeclarationJoin.hs.
cabal list-bin family-consistency-test
# Run the saved executable with --benchmark to produce before.csv.
cabal build family-consistency-test declaration-join-test tidepool-extract-bin -j8
cabal run family-consistency-test --
cabal run declaration-join-test --
# Run the repaired executable with --benchmark to produce after.csv.
```

Scratch diagnostics and both saved static test executables remain under
`/tmp/tidepool-wave-family-validation/target/completion-evidence/family-validation-cost/`.
The original cold build took 54.073 s / 1.7 GiB peak. Its benchmark process took
46.883 s / 425.9 MiB peak including excluded GHC fixture compilation. The first
repaired build took 26.185 s / 3.1 GiB peak; the final combined incremental
build, semantic suites and benchmark took 48.380 s / 4 GiB peak. These process
times are not reported as validation speedups. Executed commands exited zero.

Native Buck parity targets are registered as `//bridge/haskell:family_consistency_test`
and `//bridge/haskell:family_consistency`; they have not been built or executed
in this isolated checkout, which has no provisioned `buck-out` bind mount.
Cabal compiled and executed the same source through the repository's pinned
toolchain. `git diff --check` passed. No provider or compiler daemon was started
or reset, and no prepared-STG translation or wire format changed.
