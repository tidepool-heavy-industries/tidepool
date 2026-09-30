# SOURCE validation closure repair

Foundation: family candidate `3ae3b7acfeb8d54526cd988bf9eae25a268dd759`
with policy commit `6bbd29f5582ed164947771496fd625def7d4caa6` incorporated as
`69307ab716`. The dynamic plugin input guard remains intact. Only
`HomeProducts.hs` and its existing SOURCE test harness changed; the compiler
admission/candidate hooks and serialized witnesses did not change.

Previously any selected boot declaration caused `HomeProducts` to load every
selected candidate afresh through GHC, including unrelated acyclic products.
The repaired path loads only the closed dependency set required by every
selected boot owner. Other accepted products use the existing immutable
hydration and `checkOldIface` path.

## Closure and provenance proof

The caller still proves the complete current selected graph, source/package
imports and exact ordinary/boot witnesses. `HomeProducts` still checks the
complete interface/summary inventory and every selected ordinary/boot input's
CPP, TH/QQ, preprocessor and dynamic plugin policy before selecting a load.

The narrowing consumes GHC's existing `mgTransDeps`, which retains boot nodes
and current downsweep edges. Both ordinary and boot nodes of every selected
boot owner are roots. The union of their cached dependencies is filtered from
the original node list, preserving its order and representation flags. An
absent ordinary owner, missing reachability entry or dangling home-module edge
refuses reuse. This adds no source resolver or independently reconstructed
import graph. GHC's pinned [module graph implementation](https://raw.githubusercontent.com/ghc/ghc/ghc-9.12.2-release/compiler/GHC/Unit/Module/Graph.hs)
owns those keys and cached dependency relations.

GHC first loads that required closure without replacing its load-produced
interfaces with prepared interfaces. The complete selected graph is restored
before explicit fresh boot typechecking and interface validation. Every
selected ordinary interface is then checked and installed in the unchanged
original dependency order. This preserves SOURCE consumers' original
load-produced/boot interface provenance. The final environment restores the
caller's complete original downsweep graph, including the fresh target.
Existing refusal handling freshens mutable tables before source fallback.

## Measured same-version comparison

The original algorithm plus opt-in measurement was saved as a static executable
before narrowing. Both executables used the repository's pinned GHC 9.12.2 and
Cabal `-O1` profile. Each scenario compiled its original immutable products
once; that cold preparation was outside the measured region. The identical
timed loop replayed three requests in one resident compiler. The first request
was the first candidate replay in that resident session; the next two were
warm resident replays. No build time is included in these request measurements.
Fresh child-process assertions and failure cases run outside the timed loop.

`home_products_source_load_owners` counts actual HMIs in the HPT immediately
after GHC's load. It counts loaded owners whether GHC recompiled them or reused
an existing interface/object; it does not claim to count compiler actions.
Extraction counts are the actual `pprModules` returned by the request. Nested
load timing is emitted under the existing opt-in `TIDEPOOL_TIMING` owner.
Raw samples: [samples.csv](evidence/source-closure-cost/samples.csv).

| Unrelated ordinary modules | Loaded owners before | Loaded owners after | Accepted products, both | Extracted modules, both | Median load ms before → after | Median request ms before → after |
| ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| 1 | 3 | 2 | 3 | 1 | 143 → 136 | 157.285 → 150.703 |
| 10 | 12 | 2 | 12 | 1 | 94 → 88 | 117.804 → 111.771 |
| 100 | 102 | 2 | 102 | 1 | 222 → 136 | 361.667 → 282.137 |

A separate ten-module case makes `Independent1` a real dependency of both the
ordinary and boot declaration. It loads three owners after narrowing, versus
twelve before, and accepts all twelve products while extracting only the
target. Median request timing was 154.857 → 175.554 ms in this case; the three
samples were noisy and do not establish a general latency improvement or
regression. Owner counts are stable and provide the direct structural evidence.
These small, unisolated host measurements support the repair's bounded work
claim; they are not a resident-engine throughput study. Full witness replay
and interface checking still scale with the number of accepted products.

## Executed correctness coverage

The mixed benchmark passed all four graph cases, with 1/10/100 independent
modules and a required acyclic dependency. Each replay retained complete
dependency evidence, negative home lookup witnesses, the exact accepted
product set, and one newly extracted target. Fresh worker processes also
checked those sets. The final harness additionally asserts the fresh child's
exact load-owner count, preventing a return to whole-selected-graph loading.
Those count assertions were separately executed at all three sizes.

The normal SOURCE test target now includes an unrelated acyclic case and the
required ordinary/boot dependency case, so native and Cabal consumers exercise
the narrowing without a separate test implementation. Its existing cold,
resident, warm, target-containing SCC refusal, direct fresh hydration, ABI,
CPP and fresh-worker cases still pass. The small mixed case also rejects a
boot family arity mismatch, recompiles a changed independent product while
continuing to reuse the untouched SCC, and refuses every original candidate
when a formerly absent home `Prelude.hs` changes package selection. Removing
the mutation restores full reuse in each case. The policy suite's dynamic
plugin and foreign plugin-option refusals also executed successfully.

All changed Haskell sources and the owning worker compiled with `-Wall` and
no new warnings. `git diff --check` passed. No provider or compiler daemon was
started or reset. Native Buck has not run in this isolated checkout, which
does not have a provisioned `buck-out` bind mount; the existing
`//bridge/haskell:source_boot_product_reuse_test` and
`//bridge/haskell:source_boot_product_reuse` consume this same test source.

All jobs used `systemd-run --user --slice=tidepool-completion-build.slice`,
working directory `/tmp/tidepool-wave-family-validation`, and
`bash scripts/dev-shell.sh`. The significant inner commands were:

```sh
cd bridge/haskell
cabal build source-boot-product-reuse-test compile-input-policy-test -j8
# Save original static executable; run it with --mixed before narrowing.
cabal list-bin source-boot-product-reuse-test
cabal build source-boot-product-reuse-test compile-input-policy-test tidepool-extract-bin -j8
# Run repaired executable with --mixed for the same measured scenarios.
cabal run source-boot-product-reuse-test --
cabal run compile-input-policy-test --
```

Saved executables, full before/after timing logs, build logs, final count
assertions and failure diagnostics are retained under
`/tmp/tidepool-wave-family-validation/target/completion-evidence/source-closure-cost/`.
The original mixed job including a small harness rebuild took 14.872 s /
1.3 GiB peak. The repaired build, mixed benchmark, original SOURCE suite and
policy suite took 29.501 s / 2.7 GiB peak. The final expanded default suite
including its harness rebuild took 13.961 s / 1.3 GiB peak. All listed executed
checks exited zero; their process totals are not request speedups.
