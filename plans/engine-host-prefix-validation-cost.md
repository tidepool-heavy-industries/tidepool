# Rust host prefix validation: measurements and bounded repair

The actual prefix profile identifies a repeated full source-key walk inside Rust certification. This parcel replaces that predicate with an index owned by the same inventory. It also captures selected package-interface bytes once per validation stage. Component measurements show substantial savings, but no paired full-engine prefix speedup or historical engine regression is established here.

Production/test source is frozen at `14c32941d06e87fc50136ff31feab2aa6aad47fd`, from main `de31d0e213e055b50789c7aeb248a719d96a2af4`. The dependency-complete chain is `88b9f13587c50003b031a6980a87f3159223355e` → `9fde9b2011c65800b9464c7bd82adda43dfb0471` → `58b79fdca54fbd548a85c9b09765433cbf76c99b` → `14c32941d06e87fc50136ff31feab2aa6aad47fd`. Only `certified_products.rs` and `recovery_artifacts.rs` change production code. Compiler-owned preparation hooks are a separate integration parcel.

## Workflow and evidence limits

A protected item compile derives its offer from the accepted declaration context and preceding checked executions, prepares and materializes its exact scope, invokes the compiler worker, decodes products/receipts, certifies all original source/global ownership, seals its target and checked value output, and then allows runtime/native installation and settlement. Rust performs substantial work before and after GHC. Native settlement, Rust artifact preparation, GHC interface rehydration, and cold Rust target linking are distinct costs.

Historical `acd5c5ee2ccf31e90ef553d46eb2af9f5c80900d` N100 executed one passing native test: 101 prefix compiles took 1,514.209 seconds, with median 14.982 seconds; 100 native bindings took 8.447 seconds. Its service used 1,668.728 CPU seconds and peaked at 4.3 GiB. A retained 30-second process window observed the Rust host spend 22.04 CPU seconds, read 5.926 GB through character I/O, and write 169 MB through character I/O. Near item 96 its cumulative physical write counter was approximately 13.85 GB. These counters do not attribute the entire run to one function, and that source predates later runtime caches.

The newer actual baseline-100 prefix profile is bound to source `7c0f134b860c2d4a2b6d35bb82664f25431c0a3f` and test `session::turn::scaling_tests::protected_growing_prefix_100_baseline_100`. Its 25-second sample retained 1,096 samples with no lost samples; approximately 825 were in the host test thread and 217 in the compiler worker. `certify_inherited_inventory` accounts for 34.43% of sampled cycles inclusively, with descendants in `resolve_receipt_owner` and the full `BTreeMap::keys().any` traversal. SHA-256 also appears prominently. Inclusive percentages cannot be summed or translated directly into full-run wall time.

That packet lives at `target/completion-evidence/final-delivery/protected-prefix-scale-b100/profile-b100-n100-prefix-20260930b.{data,json}` in the main checkout. The separate `cold-0512` profiles failed after about seven seconds, before Ready; nearly all sampled cycles were in GHC. They do not measure successful prefix preparation or cold recovery.

## Implemented owner index

Previously `resolve_receipt_owner` checked every Package import against every full source binder key to prevent downgrading a Home owner to Package. Each key contains unit, module, original ordinal, and the complete symbol identity. With K source keys and P package imports, this predicate costs O(KP) even when the package unit is absent from the home inventory.

`SourceGroupMap` now owns `SourceModuleIndex`, updated by every insertion into that same full-key map. The package predicate uses borrowed unit/module lookups, with no per-query string allocation. Each distinct unit/module name is allocated once. Exact source lookup still uses the original full key and checks the original owner/version. Duplicate binders, ambiguous owners, missing closure, global representations/signatures/evaluation requirements, retained identity/generation, and Home-to-Package refusal keep their existing checks. This index issues no new authority and survives no validation stage.

The implementation is at `certified_products.rs:775` and `resolve_receipt_owner_with_validation` at line 839 in the frozen source.

The cost fixture reads actual complete original module products and the matching certification receipt. It extracts 14,567 complete source binder keys and compares the previous flat predicate with the production module index using the same 1,095 actual package queries. It creates no source-owner capability. Its flat measurement map omits unused owner values; these are isolated key-predicate timings, not full certification timings.

| Package queries | Flat source-key walk | Indexed lookup | Index setup plus lookup |
| ---: | ---: | ---: | ---: |
| 1 | 1.346 ms | 0.0056 ms | 7.526 ms |
| 10 | 13.279 ms | 0.0125 ms | 7.533 ms |
| 100 | 110.980 ms | 0.0789 ms | 7.600 ms |
| 1,095 | 1,216.409 ms | 0.7578 ms | 8.279 ms |

The index has one unit and 72 modules. Setup costs 7.521 ms in this run and is included explicitly. At one query, setup outweighs the lookup saving. Actual insertion maintains the index while constructing the existing full-key inventory; the fixture separately walks the captured keys, so its setup includes that extra walk. No allocation count or production prefix latency was measured for this component.

## Implemented package validation frame

`PackageInterfaceValidation` is opaque and crate-private in `recovery_artifacts.rs:243`. The first verification of a selected absolute path opens the actual file, verifies regular-file/32 MiB bounds on that descriptor, reads bounded full bytes, computes SHA-256, and retains those bytes and digest for the stage. Every subsequent expected digest is compared against that captured input; a conflicting expected digest refuses. No global, process, mtime, inode, or hash-only authority cache is introduced.

Retained interface bytes are capped at 64 MiB per frame. Above that budget, verification falls back to the previous independent read/check rather than rejecting a valid larger closure. The input may therefore be read repeatedly in that fallback. Frame memory is an explicit tradeoff; the measured actual package inventory totals 15,044,882 bytes and fits the budget.

Existing public entrypoints create fresh frames. Internal certification loops share one frame. Compiler preparation can thread the same frame through the new crate-private `*_with_validation` methods for materialization, recovery verification, and inherited certification. Mutable product, skinny-interface, sidecar, source-evidence, and negative-witness reads remain independent. The frame must not become a filesystem witness for a later compile, installation, transfer, or recovery stage. The endpoint-consumed path must still match the captured input at its owning boundary; later stages start fresh verification.

Tests demonstrate consistent first capture within a stage, refusal of conflicting digests, changed bytes detected by a new stage, path/inode replacement and symlink-target changes detected by new stages, fresh-read fallback when retention is exhausted, and regular-file/size/path bounds. Existing target binding, package downgrade, original source cycles, exact closure, artifact confinement, wrong owner, and checksum negatives also pass.

## Actual retained packet and package measurements

The packet came from a successful original declaration compilation that was subsequently refused by whole checking because of an exact owner collision. It was not adopted or published into a running session. Its executed source was `1c22fcf75417b72c43f214f44a1d6534cb4a8822`, corresponding to tree `ee9bd2e24918c1f97f36207c597057a54b64db24`; worker source was `f8b5ffee7f4472da67230a2b0b0e5c3050aee4c8` with SHA-256 `c7eae66e9b52d44b70b815289da4af2f5f5da176e31bc4df5e59f4fad251810b`.

- `module-products.cbor`: 46,437,044 bytes, SHA-256 `ee8f36e7e654474f5d39856c7337c771f97af6f251ba24a928ab879d63a0b93c`.
- `certified-products.cbor`: 2,024,593 bytes, SHA-256 `622956bbb087d3970fbf1c73f9f4fb340d619daf0c9c69865c82850e53ee428b`.

The fixture checks all actual module/group/global pairs before timing. There are 72 modules, 23,233 globals, 1,095 Package globals, and 73 selected package-interface paths. It then measures only production Package owner resolution, excluding decoding and source/retained owner resolution. Its source map is empty after independently confirming these package owners are not home modules; this isolates package I/O rather than conflating it with the owner-index measurement.

| Actual Package globals | Baseline time | Final frame time | Baseline thread rchar | Frame thread rchar |
| ---: | ---: | ---: | ---: | ---: |
| 1 | 0.571 ms | 0.446 ms | 566,216 | 566,210 |
| 10 | 6.239 ms | 9.586 ms | 8,374,357 | 2,991,087 |
| 100 | 30.871 ms | 6.645 ms | 45,557,838 | 7,027,050 |
| 1,095 | 405.774 ms | 15.620 ms | 634,988,184 | 15,044,984 |

For all Package globals, read syscalls fall from 2,194 to 362. One-package calls use more read syscalls with the bounded captured reader (12 versus 6). `rchar` includes the small `/proc/thread-self/io` observation read and represents bytes returned by read-like calls, not physical storage reads. The N10 final timing is slower than baseline; an earlier candidate run measured 2.855 ms. These single-run timings are noisy, so no blanket latency improvement or statistical confidence is claimed. The complete subset consistently reduces the repeated byte reads. The full baseline fixture took 4.05 seconds, including untimed decoding/checking; the final two cost fixtures took 8.65 seconds, including a second complete decode and the flat-key comparison.

The package-I/O component took approximately 0.406 seconds before repair. This does not establish that it caused the historical 15-second prefix compile. The sampled source-key traversal is independent evidence for the owner-index repair.

## Verification and provenance

All executions used the repository-pinned dev shell, Rust 1.93.0, GHC 9.12.2 requirements, one dedicated Cargo target, at most six build jobs, and `tidepool-completion-build.slice`. No compiler worker, provider, or daemon was started by these tests.

Baseline source `88b9f13587c50003b031a6980a87f3159223355e` executed exactly one ignored cost test, with 144 filtered. Final source `14c32941d06e87fc50136ff31feab2aa6aad47fd` executed 12 certified-product tests (3 ignored, 136 filtered), 11 recovery tests (140 filtered), and exactly two ignored cost tests (149 filtered). The final service succeeded in 14.170 seconds, consumed 16.188 CPU seconds including compilation, and peaked at 624.9 MiB. Its profile is unoptimized Rust test code; no optimized release claim follows. A prior intermediate compile failed on two wrapper signature mistakes; its full log is retained, and final tests recompiled the repaired code.

Canonical evidence is at `/srv/swarm/checkouts/tidepool/target/completion-evidence/final-delivery/package-validation-frame/`: `manifest.json` records source/input/log hashes and exact commands, `rows.json` preserves every row from baseline and candidate runs, `baseline01.log` and `final05.log` retain full unit/test outcomes, and `packet/` contains the bounded original inputs. The fixture adaptation between baseline and candidate threads the new frame across the same Package rows; fixture source bytes are not claimed identical. Both packet hashes are identical in every run.

Focused commands are:

```sh
cargo test -p tidepool-toolchain --lib certified_products::tests:: -- --nocapture --test-threads=1
cargo test -p tidepool-toolchain --lib recovery_artifacts::tests:: -- --nocapture --test-threads=1
cargo test -p tidepool-toolchain --lib certified_products::tests::retained_original_ -- --ignored --nocapture --test-threads=1
```

The retained manifest supplies the `TIDEPOOL_PACKAGE_VALIDATION_PACKET` path, pinned wrapper, unit, target, and job limit. Formatting and `git diff --check` passed. Full protected prefix, actor/facade, Buck integration, and recovery acceptance remain separate gates.

## Remaining source-grounded work

1. **Repeated writes of an already-owned exact artifact.** `recovery_artifacts::durable_copy` unconditionally calls `atomic_write::write_durable_new`. That primitive writes and fsyncs a full temporary file before learning at `persist_noclobber` that the final name exists. Preparing an exact context materializes its full product closure, then `inherited_groups` materializes it again. Therefore valid repeated materialization still writes the full original payload and rereads/checks it. Compiler-owned request-local reuse can remove the second materialization. A separate owning existing-file validated/durable path can avoid failed-claim temporary writes without a filesystem cache. This is a concrete source cause of write amplification; the full historical 13.85 GB has not been attributed to it by phase measurements. A bounded real-payload I/O fixture and wrong-byte/symlink negatives are the next authorized parcel.
2. **Repeated full immutable decoding and ownership inventory reconstruction.** `certify_inherited_products` reparses every retained per-module product, and output certification parses fresh products again. Exact context preparation also rechecks already materialized original bytes. A request-owned verified product/parse result could remove repeated immutable work while retaining independent endpoint filesystem checks, package/source contracts, and negative resolution witnesses. Measure actual decode/parse/certification phases before extending that scope.
3. **Prefix metadata serialization and sealed value inventory scans.** Checked prefix preparation rebuilds full baseline/completed value maps and authorizations. Existing persistent interface-byte sharing and scoped runtime view caching do not remove every host inventory walk or serialization. Measure bytes and row counts at N1/N10/N100 with fixed original closure, and preserve all exact completed-prefix/native winner checks before changing representation.

The native installation and binding costs, original SOURCE compiler hydration, compiler worker interface work, and Buck build granularity have other owners. Buck remains a build tool; it is not runtime scheduling.
