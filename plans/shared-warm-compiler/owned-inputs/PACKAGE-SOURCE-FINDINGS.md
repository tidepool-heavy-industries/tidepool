# Package source and physical lifetime findings

Read-only investigation at base `5b74c2f1fe010c5c343e0c6f331d54328fbd5090`,
2026-10-19. Investigator: native agent
`/root/warm_compiler_campaign/owned_inputs/source_lifetime`; parent:
`/root/warm_compiler_campaign/owned_inputs`. No descendants, builds, tests,
daemon changes or implementation edits were performed. Source references below
describe this base. This is source evidence and a bounded implementation
recommendation, not executed package mutation or lifetime acceptance.

## Decision

The current compiler deployment API cannot issue a source-backed package
universe. Keep package authentication operation-scoped until a reviewed boot
source owner exists. Do not add a cross-request path/hash cache or treat the
existing producer identity as package-byte authority.

The existing observation fallback also needs a precise limitation: current
resident package EPS/finder/native state is safe only under an external guarantee
that its boot-selected package source remains fixed. Fresh path/hash observations
do not establish that guarantee for arbitrary mutable deployments. If package
source provenance is unknown or changed, reject reuse or boot a clean universe
before accepting user source. Retaining the same EPS and merely rereading files
is insufficient.

Row reuse can proceed independently. Package reread elimination and mutable
package-universe reuse remain unqualified.

## Production source chain

| Owner | Issued evidence and consumption | Missing relationship |
| --- | --- | --- |
| `tidepool/extract-cmd/src/endpoint.rs:87`, `producer_identity` | BLAKE3 of frontend bytes, worker bytes and the encoded libdir **path** | Does not hash package DB contents, interfaces, libraries, unit selection or closure |
| `tidepool/extract-cmd/src/frontend.rs:540`, `PreparedWorker` | Opens worker inode; its command executes `/proc/self/fd/...` and supplies selected `TIDEPOOL_GHC_LIBDIR` | Worker executable is pinned; libdir/package contents have no corresponding retained byte/source owner |
| `tidepool/toolchain/src/toolchain.rs:478`, `CompilerDeploymentAuthority` | Schema 1 has producer/worker identities and absolute frontend/worker/libdir paths | No Nix closure, package DB/order, source retention, native package closure or boot receipt |
| `toolchain.rs:505`, `AdmittedCompilerDeployment`; `CompilerDeploymentConfiguration::admit` | Compares producer and consumed worker; admitted result retains those two digests | Admission drops paths and carries no package-source lifetime; public data is not a private source capability |
| `bridge/haskell/src/Tidepool/GhcPipeline.hs:4851`, `withResidentCompilerScopes` | `getLibdir`, `runGhc`, initial flags, package finder, exact input owner and external interpreter share resident lifetime | Boot does not consume a runtime package-source owner from deployment admission |
| `GhcPipeline.hs:5372`, `extractionDynFlags` | Clears package DBs then selects global DB; explicitly exposes `ghc` | Helpful selection restriction, but path-selected global DB is not immutable by this API |
| `build/package/qualification.py:819`, `pin_nix_closure`; `:1026`, `verify_registered_gc_roots` | Real indirect GC roots, collector registration, complete requisites and NAR hashes | This genuine qualification proof is not consumed as a package-source capability at runtime boot |

The comment in `extractionDynFlags` that producer identity binds the complete
pinned global DB overstates the implementation: the producer hashes the libdir
string. The directory is intended to select a pinned GHC installation; that
intention is not enforced by the general admitted endpoint API.

Frozen qualification is stronger than ordinary deployment admission.
`qualification.py:2005` generates the final-path compiler manifest, checks
frontend/worker/libdir equality, selects GHC and other external Nix roots, and
adds ELF loader/library dependencies. `:2026` roots the requisites. `verify`
checks the exact root set, NAR metadata and registration (`:2088-2120`).
`execution_environment` clears competing configured selectors before applying
the bundle environment. This is an existing source-retention owner worth
extending, not a reason to add a second package cache. However, no current
runtime constructor connects its retained source lifetime and exact package
selection to a booted Haskell universe. Path prefix `/nix/store/` alone cannot
replace checking membership in that acquired closure.

## What the validators establish

`tidepool/toolchain/src/recovery_artifacts.rs:1181`,
`PackageInterfaceValidation`, is deliberately operation-local. It keeps a
bounded map from absolute path to captured bytes and SHA-256. `verify_with_budget`
opens a regular file, bounds and charges the read, authenticates its digest, and
distinguishes missing from unreadable. A repeated path within that owner can
reuse its observation; conflicting expected seals refuse. A new validation
owner reobserves the file. Retained byte copies are host observations; GHC does
not load package interfaces from these copies.

`bridge/haskell/src/Tidepool/PackageWitness.hs` separates sidecar facts, current
selection and current byte observations:

- `extendAdmittedPackageImportsWithFacts` authenticates previously unseen
  package roots and merges exact unit/module/path/seal witnesses. Reused decoded
  sidecar facts do not claim current package bytes.
- `validateAdmittedPackageSelection` uses GHC finder/unit environment and checks
  selected path against the witness.
- `revalidateAdmittedPackageImports` observes selected files at terminal proof.
- `PackageRootObservations` shares physical observations within a read-only
  operation; its contract explicitly ends before interface loading/effects.
- `packageInputClosure` follows selected installed interface dependencies with
  before/after file validation. Its independent reads do not certify the
  provenance of an already loaded EPS or native library.

`ExactScope.readExactScopeWithOwner` reuses immutable acquired home-original
contents and pure decoded sidecar facts through `ExactInputOwner`. It still
authenticates installed package bytes and performs receiving selection checks.
Neither the exact original-content cache nor `AdmittedPackageImports` is a
package-source lease. Preserve current home-versus-package selection checks,
including negative package resolution when a new zero-group home owner appears;
neither optional native roots nor historical custody defines that census.

## Why old EPS cannot be blessed by a new observation

`ExactHydration.hs:723`, `PackageFinderFacts`, retains one package finder for a
compiler universe, including unsuccessful searches. Home finder state is fresh.
`forkExactContextWithPackageFacts` (`:751`) intentionally retains the same
`ue_eps`. Its comment explains the actual reason: lazy `HomeModInfo` details
capture their original `HscEnv`; copying EPS would separate later package
declarations from the instances visible to the selected attempt. The fork only
removes negative home PIT rows and refuses successful home interfaces in EPS.
It does not invalidate packages when their source files change.

`GhcPipeline.hs:1240`, `CompilerUniverse`, retains the environment, completed
source/original versions, recovery caches, package finder, exact original inputs
and interpreter. `residentCompileOne` borrows that package state, and completed
versions retain their issuing environment. Successful helper operations also
merge observed EPS/finder cells back into the universe. Failure cleanup preserves
demand-loaded package facts on the same lazy owner. Native package state can
outlive Haskell object changes; the existing interpreter epoch/reaping owner
(`retireCompilerInterpreter`, `:5272`) must participate in source replacement.

Source-derived counterexample, not an executed reproduction:

1. Configure an absolute mutable libdir with global package DB. Boot a resident
   worker and load package module P version A, leaving a lazy dependency or
   instance demand unforced; optionally load native package code into iserv.
2. Replace P's interface/library at the same path with B without changing the
   worker/frontend or libdir string. Endpoint producer identity is unchanged.
3. A fresh host validation and GHC package witness can observe B at the selected
   path. Finder path equality still holds. Existing EPS/lazy/native state may
   remain A. Alternatively, restoring A after an intervening B load yields a
   matching fresh A seal while retained package objects remain B.
4. Neither fresh observation can attach its bytes to those previously loaded
   objects. Ordinary artifact sidecar mismatch may reject particular histories,
   but there is no universe-wide issuer preventing this stale-state association.

This is an unproven-source boundary in the existing design, not a demonstrated
regression of the proposed row change. Tests must use isolated copied package
trees, never mutate installed Nix/global inputs.

## Bounded implementation choices

Preferred source-backed path, requiring the deployment/worker owners together:

1. Extend the qualification/deployment acquisition owner to return a private
   retained source reference covering the actual compiler/libdir/global DB,
   selected units/ABI and native package closure. Verify registered retention
   and exact closure membership there; do not teach a package validator to
   manufacture the proof from a pathname or digest.
2. Boot a **new** worker universe with that source selection before any package
   load, including bootstrap flags, plugins and external interpreter. Bind its
   worker-instance source identity to that boot result. Do not upgrade an
   existing unknown EPS by adding a token.
3. Validate actual finder selections and native library selections against that
   source; package roots outside it remain observation-only or refuse at the
   relevant boundary. Retain the source through lazy environments, registered
   realizations and interpreter use. Exact receiving selection remains fresh.
4. Only then reuse authenticated package byte/decoded facts across requests.
   Switching source issues a new universe and retires old loader state through
   the existing owner; source loss after acceptance follows settlement and never
   compile replay.

Explicit fallback when that work is outside this parcel:

- Retain existing operation-local Rust/Haskell observations and their counters.
  Pure immutable sidecar decoding may still be shared by its acquired home
  input owner. Do not claim package reread elimination.
- Source-free acquisition can retain portable immutable home parts; it cannot
  promote unknown package-backed hydrated GHC objects into reusable authority.
- For an unproven/mutable source, use the existing worker replacement/direct
  finite-worker owner to obtain a fresh `runGhc`/finder/EPS/iserv universe before
  user-source acceptance, or refuse that reusable mode. A fresh home fork or
  `freshExactContext` alone does not reset all interpreter/lazy owners. This
  requires warm-workers coordination; no independent routing table is proposed.
- Even clean boot does not make a mutable filesystem transactionally immutable.
  Keep current byte observations, terminal refusal and non-replay restrictions;
  arbitrary concurrent mutation remains outside source-backed proof. Preserve
  the unknown/changed-source distinction in the admission result.

No small safe observation-only edit found here closes the missing provenance
relationship. Removing terminal reads, caching path hashes across operations or
sharing an observation across interface loading would weaken the contract.

## Physical backing extent and sibling lifetime

At this base, three different reference counts must not be confused:

| Issued handle | Actual retained backing | Drop implication |
| --- | --- | --- |
| `CheckedValueArtifact` (`checked_cell.rs:1288`) | Interface bytes, selected support/templates/prototype, plus `Arc<ValueInterfaceDirectory>` | `capture_output` clones the full producing `CheckedValueInputs` directory, which contains baseline copies and sibling outputs. A tiny escaped output keeps all files until the last directory borrower drops. |
| `RetainedArtifactRow` (`declaration_context.rs:4384`) | Exact row evidence, slices, canonical custody, shared materialization TempDir | Metadata detachment preserves selected row handle identity but still pins the shared directory containing sibling files. |
| `OwnedInputSlice` (`owned_input_arena.rs:70`) | `Arc<OwnedInputArena>` with one descriptor and the **whole sealed extent** | Slice length is selected bytes, not retained backing. Every surviving slice pins all pages/parts in that arena, including unrelated sibling rows/graphs. |

`materialize_retained_artifacts` (`declaration_context.rs:9705`) creates one
builder for all fresh entries, appends each interface/package/certificate/Core/
native/census part and new execution graphs, then seals one arena. Every fresh
row clones that materialization's TempDir. `select_custody_inputs` and
`MaterializationFacts::detach` correctly avoid retaining broad parent metadata
for residual selections, but cannot release sibling physical bytes sharing
these owners. `input_transport_files` also walks selected rows/parts and graph
paths before deduplicating file owners; it is an acquisition path, not an O(1)
resident-hit probe.

The bounded repair belongs to the assigned row/materialization owner: seal each
independently movable new value's own parts in a leaf owner, share exact
dependencies by their existing row references, and copy new output from mutable
staging at most once if needed. Avoid indefinite whole-staging retention. For
other artifacts, decide per-row versus explicitly bounded chunk packing with
measured FD/inode and stranded-backing costs. Selecting one tiny row from a
shared large arena is not retention-complete merely because sibling row handles
drop. Report selected bytes, unique part bytes, union of arena extents, all
retained directory files, descriptors and producer/drop cleanup separately.

## Existing END and source-free acquisition owners

`tidepool/extract-cmd/src/endpoint.rs:318`, `CompilerInputCustody`, already owns
deduplicated original `Arc<File>` descriptors and `Arc<TempDir>` directories.
`retain_compiler_input_file` / `retain_compiler_input_directory` attach them to
the existing transaction. Confirmed END releases request custody; unconfirmed
close retains it in `CompilerTransactionCloseEvidence`, including earlier
uncertainty. Explicit unscoped transactions must retain inputs through `finish`.
A response or cancelled waiter is not release evidence. Extend this owner for
actual transfers; do not add a second transport table/supervisor.

Home-original acquisition already uses `OriginalInputArenaTable` and the matched
`continue-originals` format in Rust `declaration_context/original_inputs.rs` and
Haskell `OwnedInputTransport.hs`. The receiver validates kinds, owners,
seals/lengths and bounded ranges, then `ExactScope` captures bytes into
`RequestOriginalInputs` and optionally retains immutable captured content. This
is portable byte ownership, distinct from GHC package/EPS authority. There is
no completed standalone `EnsureExactInputs` registration protocol at this base;
that new source-free request belongs to warm-workers in the existing versioned
request/transaction owner. `CompilerScope` is guarded and transaction-local;
it is not an idle cache lease. A cached realization must own independent
captured bytes or an explicit host lease until acknowledged release/confirmed
worker retirement. Actual END and unknown cleanup remain with the existing
endpoint owner.

## Acceptance still required

Use the maintained observation mutation/refusal controls in
`recovery_artifacts.rs` and `test-source-boot/SourceBootCases.hs`, then add the
isolated stale-EPS/lazy/native history above through actual worker consumers.
For source-backed mode, prove host and GHC actual selected/loaded body identity
and final source lease release. For rows, retain one tiny output/row after
dropping producer and siblings, compile through the survivor, then drop the last
borrower and observe the actual arena/directory/FD owner. Include END uncertainty
and failed registration. Preserve the mandatory optional-root versus required
historical-child regressions from `HLS-REWRITE-HANDOFF.md`.

No checks executed: disk headroom was reported by the parent as approximately
102 GiB with a 100 GiB reserve; this investigation did not request a build lane
or bypass admission. The source trace establishes the missing ownership
relationship and concrete retainers; runtime outcomes, growth measurements and
joined package/row behavior remain pending at an exact implementation candidate.

## In-progress row repair inspection

Separately inspected uncommitted `feature/shared-warm/owned-rows` WIP in
`/srv/swarm/checkouts/tidepool-shared-warm-owned-rows-20261019` at the same base.
This is steering for the implementation owner, not an exact-candidate review.
The initial `RetainedArtifactMaterialization::capture_checked_value` WIP had two
concrete problems; the parent accepted directed repair and exact-candidate
verification remains pending:

1. It stores an incomplete value-only materialization in
   `support.artifacts().retain_materialization`. Existing
   `ViewLease::collect_materializations` (`artifact_inventory.rs:5646`) skips
   inherited/fixed owners when the current cache is nonempty: completed entries
   are assumed to cover their inherited physical closure. For value V with
   required declaration A, inserting only V can make a later projected
   descendant miss inherited A's physical row and rematerialize it. Meanwhile
   the value's original support still owns A's fixed materialization, so its
   backing remains retained. Do not install a singleton as a complete compiler
   materialization. The existing owner must attach an issued leaf without
   invalidating complete-closure cache semantics, or store an entry that also
   carries exact required dependency physical owners. The latter still needs
   backing retention measurement.
2. The inherited-row shortcut checks only that selected facts contain V, then
   detaches the row handle. It does not prove that row's directory/arena is a
   value-sized leaf. If V was previously materialized beside large unrelated S,
   this shortcut preserves V's broad arena and directory, keeping S alive.
   Reuse must recognize actual leaf ownership through the issuing owner; row
   count, digest or a singleton selected map cannot prove backing granularity.

The helper also edits `RetainedArtifactMetadata` maps after deriving a complete
snapshot while leaving dependency tuples and ambiguous-owner metadata intact.
This makes a compiler-metadata value internally incomplete solely to select
physical storage. Prefer a physical-row issuance boundary under the existing
owner over synthetic semantic metadata. The checked certificate comparison is
useful; it does not repair the cache-completeness or backing-size invariants.
No concrete per-view absence bypass was established in this bounded pass.

The subsequent WIP uses typed `Compilation` versus `CheckedValueLeaf` cache
keys, validates namespace/acquired payload before a leaf hit, and makes leaves
additive. Review must check both own and inherited cache collection: a leaf-only
cache must preserve fixed/parent owners; ancestors must not be appended after a
new leaf and override its physical row through last-wins collection. Existing
`ViewReadCache.inherited_materializations` observations must not leave the
published next-offer path blind to a newly issued leaf. Required controls are
V requiring A (same original A row, no copy), plus V and huge unrelated S
(value-sized V survives and S backing releases), including overlapping/diamond
sources.

A second WIP issue was the custom `CheckedValueArtifact` equality dropping
original path/physical ownership while runtime `BindingIndex` uses equality as
issued authority. The parent directed retaining all logical fields, path and
exact row reference; source now includes them. The independent bounded analogue
search found no additional confirmed changed-helper bypass. Interface-fact
equality in `CheckedValueImportAuthority::matches_interface` is an intentional
compatibility observation for a thin value's instance-free scaffold; it grants
no replacement binding or native permission.

Strict equality then exposed duplicate promotion of one output: ordinary and
prepared capture use the same `ProducedValueTypeInterface` object/certificate,
staging path and P6 receipt but derive separate support views. Per-view row
caches promoted that same output twice. The parent approved the existing private
produced-output owner retaining one successful `OnceLock<Arc<Row>>` promotion,
then attaching that physical row under each support's independently checked
exact binding. Preserve the original ordinary/prepared authority equality
assertion; do not substitute comparison of receipt bytes alone. Separate output
issuers keep independent rows even if their bytes match. No digest registry is
needed.

Read campaign amendment `e2bd2611102566085d0c573568d820a50ed62fb3` directly.
Package negative dependencies consume the authenticated **selected module**
census from canonical/value/lexical row issuers. The broader type-classification
`home_units`, dormant custody and executable demand remain distinct; physical
row presence cannot reconstruct that census.

## Fresh-scope fallback refinement

Warm-workers' bounded proposal moves `withResidentCompilerScopes` inside each
complete user-operation handler. Fresh `runGhc` creates fresh package EPS,
finder and NameCache. The existing outer cleanup must retire/reap the external
iserv before a response or next operation; uncertain cleanup poisons/replaces
the worker. Keep multi-stage compilation inside one complete operation so
intermediate compiler state remains available to its own consumer.

`ExactInputOwner` is not purely raw: it also retains `RequestIfaceDecoder`
decoded `ModIface` packets keyed by the actual NameCache and pure decoded
certificate/package/census/graph facts. The NameCache guard prevents old decoded
interface reuse after a genuinely fresh cache, but retaining the whole owner
still retains those old Names. Split/clear the decoder at the new scope boundary
if claiming raw-only retention; keep authenticated captured bytes and pure wire
facts under their existing owner. No `HscEnv`, `HomeModInfo`, `TcGblEnv`, derived
package object or loader state may escape into the portable realization.

Fresh `runGhc` plus iserv cleanup alone does not establish absence of
process-global plugin/dlopen state. For arbitrary unproven package/native/plugin
sources, full worker-process replacement is the robust fallback unless the
owning GHC path and an executed control establish external-only loading. This
is an implementable fallback condition, not a license to infer freshness from
hashes. Transaction integration currently asserts cross-request module reuse;
the explicit fallback mode must replace that expectation honestly and report
decoded warm reuse unavailable. Low-level tests that explicitly open one shared
`withResidentCompilerScopes` continue testing its fixed-source precondition,
not production fallback freshness.
