# Engine foundation destination checkpoint

Destination work on 2026-09-29, branch `foundation/destination-checkpoint` in
`/srv/swarm/checkouts/tidepool-foundation`. This supplements the historical
[source transfer](engine-foundation-transfer.md). It is not M1 acceptance,
M2 implementation, deployment approval, or server acceptance.

## Source and dependency order

The original main checkout remains at
`f84bf313d8f0364fff474dcfd883aeca443b291f`. The isolated candidate starts with
`24516ebb5f29a7e9a5f3245c485b578f31f1658f`, merges the main planning record,
then applies missing patches in this order:

1. Transitive binding retention `41583b5eb89737a2dd966908adbfd29bf55452ec`.
   Its equivalent `20a031feb964b8d1437b5a5f8c6537ff1d48550c` was not applied.
2. Skinny module product `fbcfb02027632e2a53199c0a6ef6843f5cc2a577`.
3. Missing adapter patches through
   `e4bb3ddc4b59f01acedcc28b9950377a2299f38a` and handoff documentation
   `bb58540d4f0a2635e5713f3335ec7f82a94ee062`.
4. Destination dependency pins, producer-regenerated artifacts, ABI checking,
   compiler publication proof and reviewed ownership design, followed by
   integration repairs and their checks.

Pinned companion sources:

| Input | Full revision |
| --- | --- |
| Harness | `c485edb9b697ffc671b22c9ef25a73fc84763d76` |
| Codex | `2d58f00c6f139d745e0c123d31dfe6d2f04ff997` |
| Workspace | `5248b927e7b432d1747891df5285d6827eace7d6` |

The Codex candidate is a direct child of transferred
`c09c2b067774be4104fad878ee271a6a14f12690`. Its only change makes Nix read
Rust 1.95.0 from the client's declared toolchain instead of `stable.latest`.
Tidepool retains its own Rust 1.93.0 and GHC 9.12.2 pins. Both submodules are
real Git checkouts. No local URL rewrite or source-box symlink is required.

Pushes are explicitly deferred by the user. The client candidate is preserved
in a verified complete-history bundle with instructions and bounded companion
evidence at `/srv/swarm/checkouts/foundation-transfer/README.md`. The new
candidate commits are local, not advertised remote dependencies.

## Executed destination checks

Unless otherwise stated, commands run from the destination candidate through
`bash scripts/dev-shell.sh`, with logs under `target/transfer-evidence/`.
`scripts/battery.sh` is the repository's Nix-backed nextest wrapper, not bare
Cargo. User systemd scopes bound memory and disable swap within each scope;
no shared compiler or infrastructure daemon was restarted.

| Boundary | Executed result | Log |
| --- | --- | --- |
| Seven embedded artifact producers | Regenerated through their registered Haskell/Rust producers; no byte patching | `embedded-fixtures-update-2.log` |
| Production artifact decoder | 7/7 accepted at execution ABI 8, schema 14 | `embedded-artifact-gate-final.log` |
| Historical ABI 7 artifact | Rejected with exit 1 and explicit expected ABI 8 diagnostic | `stale-abi-rejection-final.log` |
| Repr/codegen libraries | 636 passed, 1 skipped | `joined-native-libs-final.log` |
| Repr schema contract | 7 passed, 49 skipped | `repr-schema-contract.log` |
| Exact artifact linking | 1 passed, 91 skipped | `toolchain-artifact-link.log` |
| Runtime prepared owner and detached capture | 17 passed, 210 skipped | `runtime-owner-tests.log` |
| Actor admission/retirement | 2 passed, 367 skipped | `actor-admission-tests.log` |
| Embedded adapter, frozen tools and real Event retirement | 4 passed, 547 skipped | `joined-facade-adapter.log` |
| Declaration join proof | 1 suite/case passed; six join/consumer scenarios | `declaration-join-proof.log` |
| Module product roundtrip | 1 suite/case passed; skinny interface 2,336 bytes | `module-product-roundtrip.log` |
| Retained compiler scope | 1 suite/case passed | `retained-scope.log` |
| Retained-symbol measurement | 1 passed, explicitly selected ignored measurement | `retained-fingerprint-probe.log` |

Exact Rust selections (each prefixed with `bash scripts/dev-shell.sh`):

```sh
scripts/battery.sh -p tidepool-repr -p tidepool-codegen --lib
scripts/battery.sh -p tidepool-repr --test repr -E 'test(execution_schema_contract)'
scripts/battery.sh -p tidepool-toolchain --lib -E 'test(exact_artifact_parses_and_links_atomically)'
scripts/battery.sh -p tidepool-runtime --lib -E 'test(session::prepared::tests::) | test(detached_capture)'
scripts/battery.sh -p tidepool-runtime --test session
scripts/battery.sh -p exomonad-actor --lib -E 'test(admission_close_waits_for_all_transactions_and_rejects_new_ones) | test(retirement_waits_for_short_host_admission_transaction)'
scripts/battery.sh -p tidepool --lib -E 'test(embedded_harness) | test(issued_tool_snapshot_keeps_old_handler_after_spec_reload) | test(unbounded_repository_event_await_joins_before_actor_retirement)'
```

The measurement uses `TIDEPOOL_TIMING=1` and selects
`-p tidepool-extract-cmd --test retained_fingerprint --run-ignored all
--success-output immediate -E 'test(unrelated_retained_symbols_do_not_scale_g3_interface)'`.
The Cabal checks run from `bridge/haskell` in the same shell:
`cabal test declaration-join-proof --test-show-details=direct`, and
`cabal test prepared-stg-pipeline-test --test-show-details=direct` once with
`--test-options=--module-product-roundtrip` and once with
`--test-options=--retained-scope`.

The unrestricted `scripts/fixtures.sh check` passed at clean source
`7881626da56c23732e27ed7af216614ac303a8b1`, with all 12 reported cohorts,
metadata checks and seven embedded artifacts accepted. The Suite cohort has
799 STG tops, 261 mapped source tops and 234 successful comparisons; the other
cohorts add 48 successful comparisons. Refused, unobserved and helper tops are
not counted as passing executions. Exact stage accounting is retained in
`target/prepared-corpus/latest-success.json`; log `fixtures-check-final.log`.

The first corpus attempt stopped at an obsolete oracle fingerprint. Native GHC
regeneration changed only that fingerprint: all 237 expectations, 26 refusals
and 261 source tops remained identical, with unchanged payload digest
`35ea9188d22b81bb8703f2cac36831935afc114d55acf1176009552d08565a98`.
The changed input was the flake; the oracle was not edited by hand.

The full native battery initially exposed two stale test assumptions after the
code/installation split. The repaired tests inspect the exact invocation's
descriptor and expect the current three-load installation environment path.
The subsequent 636-test run includes the three transferred retention
regressions and the final ABI diagnostic regression.

Runtime session integration initially ran 32 tests: 31 passed, one failed,
one skipped. The failed cross-session parcel regression revealed a carried
static installation ownership problem. Repair and final verification are
pending; `joined-runtime-session.log` retains the original failure.

The matched local host build succeeded with
`bash scripts/dev-shell.sh bash exomonad/scripts/exomonad-build.sh`:
frontend, Haskell worker, endpoint validation, `exomonad` and
`exomonad-view-helper`. This uses the declared default build environment;
it does not realize the private Codex package or launch a live session.
The adapter still has unused production composition surface, reported by
compiler warnings; a successful build is not full M1 owner composition.

## Companion and Nix evidence

Independent focused review found no defect in the completed-owner reply or
short snapshot-admission paths. Harness executed its owner race 1/1 and its
embedded-host selection 5/5. The latter requires generated browser assets;
the initial fresh-checkout 404 disappeared after the declared web build.

The client reran the exact source-box selection under its own Nix Rust 1.95.0:

```sh
cd codex-rs
just test -p codex-tui --lib \
  -E 'test(app::tests::host_input::) | test(host_dynamic_tools::cancellation::tests)'
```

Result: 13 passed, 5,488 skipped, one binary. Other repaired client modules
compiled but were not selected. An earlier positional-filter invocation
selected zero tests and is only compile evidence. The source-box historical
log did not print rustc; current source-box and destination verification both
report 1.95.0. Companion logs and commands are retained with the local bundle.

Both Tidepool CargoLock consumers use the actual harness vendor-source hash
`sha256-gfk2stabmTx7F6rjQUndSmaBCbYmLz0xyWfk9hIojQs=`. The initial Git
`fetchTree` NAR hash was not the Cargo vendoring hash and failed realization.
The corrected shared vendor derivation was built successfully:

- Derivation: `/nix/store/yail7j5r0f30zw2rmav740aqgqkaaisc-cargo-vendor-dir.drv`
- Output: `/nix/store/jzyfmljhm8yxcngbnn86sw5blywx8fk6-cargo-vendor-dir`
- Output NAR: `sha256-NqXxqYBAugCOY4kJSDjZRZeWsEOHIWM3Sm6wPCw59oA=`

At `37cb590c0c1ffa8be0264ca947a01773ff36399f`, both the extractor frontend
and unwrapped Exomonad evaluate to that same built vendor closure. No release
package was built. A local immutable Codex override to `2d58f00...` also
evaluated the Exomonad dev shell successfully, without writing a lockfile.
Current remote source capture remains blocked on publishing that client pin;
the local override is expression compatibility evidence only.

## Design result and remaining boundaries

[The reviewed M2 design](harness-integration-runtime.md) keeps lifecycle and
shared coordination actor-owned while giving each execution its own private
scope, continuation, writes and receipts. Publication stages fallible work,
revalidates generation, and orders a successful visibility swap against
cancellation through one short decision lock. A stale success or rejection
retries staging without replaying effects. Captures take independent strong
leases on the admitted source and private lexical state.

The compiler proof demonstrates that an import-wrapper typecheck alone does
not reject duplicate ordinary instances: use of the constraint does. Explicit
combined-instance validation therefore remains required. Hidden-name policy,
exact declaration provenance, replacements/retractions and instance visibility
are still compiler-join obligations, not solved by a name-only delta.

No M2 concurrency, durable neutral-group codec/cache consumer, or demand
compilation was implemented in this checkpoint. Full M1 owner composition,
broad client tests, release package realization and remote source verification
remain follow-on gates. Canonical Codex HTTP still does not pin handlers at
model-request time. Admission leases must remain short: retirement waits for
them before computing its shutdown budget.

The retained-symbol probe measured G3 interface construction at approximately
0.917 / 0.214 / 0.221 ms for 0 / 1,000 / 10,000 unrelated retained symbols;
whole requests were 160 / 183 / 539 ms. The first sample included allocation
activity absent from the later samples. These are current-worker measurements,
not an exact historical before/after comparison or a speedup claim.
Source-box raw logs and `/tmp/tidepool-wave22-fullcore` have not been transferred
or deleted by this work. Historical G2 and private W2 dependencies remain
unavailable; preserving those logs does not make the old run reproducible.
