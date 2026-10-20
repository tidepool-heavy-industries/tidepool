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

The initial field-growth finding was source-confirmed. The repaired owner
frontier is physically observed above; an old-core physical counterfactual is
still pending. The control's small cfg(test) directory getter lends an existing
owner for weak observation and issues no compiler or runtime authority.
