# Successor transport lifetime

The reviewed engine repair `cee0baed39059b085f747bad248468669b37adf7`
(received here as `076e8868c5`) removes the append-only predecessor file and
directory frontier. `native_publication_birth` independently reviewed the exact
delta and consumers. This report separates transport lifetime observations from
the still unqualified Haskell cold-restoration boundary.

## Owner trace

- `compiler_world.rs`, `item_successor`, writes one successor descriptor and
  retains only the actual new delta files. The warm `ensure_world` branch exports
  `successor.files`; it never consumes a predecessor's complete descriptor files.
- The complete acquisition branch consumes its own `input_files`. When a real
  successor acquisition reports missing predecessor backing, the existing cold
  branch calls `inputs.clone().prepare_for`, rebuilding the same logical product
  from current primary facts, visible checked rows and captured sources.
- `CheckedValueArtifact::world_type_parts` delegates to its actual
  `RetainedArtifactRow`. That row owns its directory and `OwnedInputSlice`s.
  `WorldColdRows::row` borrows those slices and obtains their exact sealed file
  leases. No predecessor World or descriptor directory supplies that custody.
- Historical native dependency custody remains attached to its own issuers.
  The repair does not change compiler roles, original selection or native graphs.

## Actual control

`artifacts::module_product_tests::real_world_shadow_successors_release_transport_ancestors_before_cold_restore`
uses real compiler output, the existing native issuers, matching transaction
origin/Clean END and production `item_successor`. Two and eight independent
same-name binding statements form separate genuine compilation histories. The
control drops the complete compiled program and all predecessor Worlds, retaining
only the latest World and weak/path observations. It checks old row owners,
arena FDs, descriptor directory owners, and physical row/descriptor paths.

```sh
swarm-build just test-native //tidepool/toolchain:toolchain_world_lifetime_test --output-dir /srv/swarm/evidence/inanna/native-world-lifetime-off-20261020 --compiler-diagnostics off --retain-artifacts
```

The supported diagnostic mode is essential: `capture` intentionally disables
compiler scratch cleanup. `off` retains reports, traces and owned compiler
cleanup evidence while allowing the ordinary physical lifetimes under test.

At engine `63440b49f0` plus the reviewed core repair and control WIP, build
`f5c48e8d-6045-48d1-a2de-f1abc1d173e0` compiled and executed exactly one test.
Both histories reached the drop checks:

| Independent shadow items | Live bindings | Old row owners/FDs | Predecessor directories | Owned arena FDs | Current row arena bytes | Latest descriptor bytes |
| --- | --- | --- | --- | --- | --- | --- |
| 2 | 1 | 0 / 0 | 0 | 1 | 541 | 1805 |
| 8 | 1 | 0 / 0 | 0 | 1 | 541 | 1806 |

Exact observed paths are retained in
`/srv/swarm/evidence/inanna/native-world-lifetime-off-20261020.log`.
These are observations of this fixed-size binding workload, not general memory
or performance bounds.

## Required unresolved boundary

The full control **failed 1/1, exit 1** at its final production `restore_world`:
`ExtractFailed("compiler refused world acquisition: UnsupportedOperation")`,
with Clean END. The Haskell `SuccessorExactInputs` receiver is still explicitly
unsupported. The test requires genuine missing-predecessor acquisition and
same-product cold restoration; it does not reinterpret Unsupported as a miss,
substitute a complete descriptor, or skip the cold gate. No cold restoration
success is claimed.

## Discriminating old-core control

At committed control `fd3c954538`, reversing only the reviewed core patch and
running the same physical-lifetime command confirmed the original retention:
after two shadows, old row owners were zero, but one old arena FD and two
predecessor directory owners survived, leaving two physical owned arena FDs.
The test failed the descriptor-owner release assertion before cold restoration.
This distinguishes obsolete transport custody from historical row authority.
Build `d17d8e36-a94b-4e61-b054-05f63dc92be3` executed exactly one failed test,
exit 1; log and report root are
`/srv/swarm/evidence/inanna/native-world-lifetime-old-core-20261020` (log adds
`.log`). The inverse was confined to this experiment and was fully reapplied.

The final clean `fd3c954538` run rebuilt the repaired code and reproduced the
two/eight-item fixed metrics above. Build
`5fb36473-3498-44b2-843a-fdc3478eb80c` compiled and executed exactly one test,
then failed only at the required Haskell Unsupported cold gate, exit 1. Its log
and report root are
`/srv/swarm/evidence/inanna/native-world-lifetime-fixed-final-20261020`.
Both owned compiler cleanups were confirmed. They used the same Haskell worker
SHA-256 `62d6e018bf107b7401941eff72c5838bd8fe1f9ac75ed2a2b0bb72019af54045`.
The old-core test executable SHA-256 was
`971a98c18ff27e3fabb2ba0d906d4d92f2353d84cfe033e2caad1be37b98b909`;
the repaired executable SHA-256 was
`d60f232f5961ec6bf45bdfb9418866d3942ee982c235340a5711220e7d6ded74`.

The control's small cfg(test) directory getter lends an existing owner for weak
observation and issues no compiler or runtime authority. Successful cold
restoration remains an explicit pending requirement, not a passing or skipped
test.
