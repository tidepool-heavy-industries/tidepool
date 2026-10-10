# bridge/haskell/ — extractor and eval standard library

## Charter

This directory owns the GHC-to-prepared-STG compiler worker and `lib/Tidepool`, the Haskell
library auto-imported by the MCP surfaces. Toolchain discovery and caching live
in `tidepool-toolchain`; CBOR decoding lives in `tidepool-repr`.

Compiler templates distinguish the generic preamble import slot from the
protected primitive default recipe. Installer templates may carry only the
import marker. `TurnSource` captures the default recipe from its separate
marker before authored syntax is inserted, then carries that typed fact through
rendering. Do not infer defaulting authority from an import insertion marker or
recapture an already qualified protected recipe.

`GhcPipeline` alone issues completed pipeline results. Their read accessors keep
compiler environments, prepared bodies, dependencies and admitted candidates
paired; consumers cannot construct or update the issued records. Refusal tests
change raw inputs at their admission owner, and projection tests change the
projection inputs without reissuing compiler results.

## Build the compiler worker

From the repository root, after materializing and configuring pinned tools:

```bash
just build //bridge/haskell:tidepool_extract_bin
just test-native //bridge/haskell:source_boot_product_reuse_test --list-tests
```

The executable is an internal compiler worker, not a user-facing CLI. It accepts
only the versioned request protocol emitted by `tidepool-extract-cmd`, or
`--worker-loop-v2` behind the resident daemon. Native Rust/Haskell test targets
carry matched frontend, worker, deployment, GHC and source resources. The
production worker uses its production toolchain; host Tasty compilation uses
`toolchains//:haskell_tests`. The Cabal package owns production components and
26 Tasty suites. The pinned Cabal metadata producer finalizes their source,
package and compiler-option declarations; `scripts/buck2-haskell-components.py`
projects them into the single `components.bzl` native graph. Listing a suite
is discovery, not test execution.

## Toolchain resolution and deployment

`tidepool/toolchain/src/toolchain.rs` owns compiler resolution, deployment
identity and source selection. Native compiler and test actions declare the
frontend, worker, GHC, stdlib sources and generated effects they consume.
Test-only packages stay in the host test toolchain.

For a hosted run, build `//build/package:native_runtime_bundle`, freeze its
products at the final deployment path, and use its qualification descriptor.
`build/package/qualification.py` supplies that bundle's compiler deployment
manifest, stdlib, runtime libraries and browser assets to both acceptance and
actual execution. The first native delivery uses source-backed stdlib inputs
through the normal compiler; canonical catalog and durable module-cache
acceptance remain separate obligations. See
[the package guide](../../build/package/README.md) for freeze and qualification.

`just exomonad-run DESCRIPTOR REPORT ...` and
`just exomonad-init DESCRIPTOR REPORT ...` select the verified artifact
environment from the descriptor's sibling owner. `just doctor DESCRIPTOR`
verifies and prints the selection.
`scripts/redeploy.sh` delegates to the same freeze owner and preserves existing
installations and live hosts. Standalone toolchain APIs retain their resolution
contract in `tidepool/toolchain/src/toolchain.rs`; they do not issue deployment
qualification.

## Exact-scope transport

Exact-scope manifests use strict `TPEXACTSCOPE` version 13 with eleven fields. The final
fields contain an execution parcel or null, a compiler-purpose authorization
or null, the exact published source selection roots, and an input acquisition.
Acquisition is `["fresh-files"]` or `["continue-originals", arenaDescriptors, images]`. Each arena descriptor binds a canonical absolute `/proc/<owner-pid>/fd/<fd>` endpoint and sealed extent; parts select the table index and bounded offset, with their existing positive length. Cold reads verify all four memfd seals, extent, range and SHA on the same opened FD. The host retains the actual files through compiler close; a replacement worker can capture them independently. Durable recovery and exported fixtures use authenticated files, never process endpoints. Each image
binds the compiler producer, unit/module and ordered kind/SHA/length facts;
receiving logical paths and authenticated issuing origins are provenance bound
by the receiving envelope, independently of content identity. Their later drift
does not revoke captured bytes. Closed part kinds
are interface, packages, certificate, Core, native, census and graph. Interface rows have eight fields; their final field declares one
closed artifact role: `["module", certificate path, certificate SHA, optional
Core path, optional Core SHA]`, the same five-field `["native-declaration", ...]`
for native authored originals, `["join"]`, or `["value"]`. Native product
owners require canonical module evidence. Roles never come from module-name spelling.

Native product rows retain exact selected ordinals and a logical path/SHA
descriptor for the existing `TPHOMEOWNERS` version 5 certificate. The worker
admits and indexes its full native census once against the original owner, native
bytes and canonical certificate. Selected projections borrow indexed groups;
private availability explicitly selects the complete certified ordinal set. Availability and selected roots
remain separate: later checked demand selects groups from the same stored native
carrier, without promoting that owner's Core or emitting another native product.

Canonical module certificates bind the compiler producer, finalized interface
and package bytes, original source digest, exact dependency seals, optional
Core digest, and complete compiler home-unit inventory. The worker checks this
proof against the selected interface closure before hydration. A Core companion
is a separate compiler input: admission captures its encoded bytes without
loading defining Core or granting native execution or lexical imports. Its
recovery owner decodes those bytes only on demand, without replaying source or
Template Haskell.

Encoded artifacts use opaque `ArtifactBytes`, joining strict bytes, their SHA
and length. A receiving request separately owns paths, allowances and byte
budgets. Canonical and local finalization admissions retain their selected
bodies. Materialization writes held bodies into fresh output views; a path alone
does not own bytes. Decoded interfaces and Core retain their existing NameCache
and compiler-epoch owners.

A physical request owns immutable captured home originals, checked interfaces,
certificates, Core and graphs. Scope generations extend that opaque owner; they
cannot replace an admitted owner or expose a partially assembled closure. The
encoded byte budget defaults to four GiB and is configured by the trusted host's
`TIDEPOOL_REQUEST_CAPTURE_BYTES` positive integer. Decoded GHC data is accounted
separately. The compiler universe retains path-free original content and decoded
certificate, sidecar, census and graph facts. Each continuation creates a fresh
receiving allowance, selected paths and current-source observations; no earlier
manifest or scratch observation is inherited. Worker misses read only the
offered sealed arena ranges. Fresh acquisition always reads its offered files.
Inactive content uses union-unique byte accounting, at most 4096 images, and
`TIDEPOOL_RETAINED_ORIGINAL_INPUT_BYTES` (nonnegative bytes; experimental default
128 MiB, clamped to the request allowance). Eviction prunes associated decoded
facts; live scopes retain their own immutable inputs. Existing worker RSS
admission and rotation bound total decoded GHC residency. Timing counters expose
retained/evicted/receiving bytes, decoded fact counts and content hit/miss events.
Interface decoding uses a disposable captured-file adapter and a
universe-owned memo tied to the GHC NameCache's mutable intern-table cell and
unique issuer character, retaining only its current cache epoch. Identity of a reboxed GHC record is not owner identity.
Neither a retained cache nor an EPS lazy closure may retain a temporary path;
executable GHC make views are separate disposable materializations.

All source downsweeps use `depanalSourceModules`. Exact interface summaries have
no source path and belong to admitted instance/linker graphs; the source boundary
preserves their home interfaces. CPP summaries require fresh preprocessing so
changed include files cannot survive in a retained source summary.

Installed packages belong to the matched pinned immutable compiler universe.
Their exact resolution is checked in each consuming environment, with fresh
interface seal observations at admission and terminal publication. Package
objects and shared libraries are not request snapshots. Current source selection,
source/dependency and negative-candidate checks remain fresh. Prepared candidate
results retain the issuer's opaque admission through certification and program
retention. Captured originals remain available from their immutable byte owner;
terminal publication does not reopen their origin or materialization paths.
Retention and checked receipt publication capture source evidence before one
terminal proof over their required scopes. That proof shares current scope,
package/import and newly materialized output observations; no observations
survive into publication, cancellation recovery or further compiler work.
`CertifiedOriginalProducts` retains the final fresh dependency evidence after
native output readiness is established. Ordinary, staged and retained publication
issue that evidence directly; the earlier prepared interface inventory cannot
reconstruct the completed product facts. Source selection remains an independent
proof and does not itself establish native readiness.

Execution parcels retain `[SHA, absolute graph-file path]` descriptors and exact
original references. Exact-scope graph paths come from the retained immutable
artifact owner; the request keeps its complete parent custody alive while the
worker consumes them. Their logical paths and digests bind selected identity; owned acquisition transports
the captured graph bytes through arena ranges. Graph producer and complete original
identity checks establish compatibility.
Unchanged `TPEXECUTIONSOURCE` bytes remain independent of the metadata envelope.
Metadata is limited to four MiB. Execution graphs retain their 64 MiB aggregate
and 4096-graph bounds. Authored source is bounded separately at 32 MiB of UTF-8;
other text and metadata keep their existing bounds. Advertised invalid or
oversized parcels are rejected. Resolution evidence retains at most 65,536 rows,
4,096 candidate paths per row and 65,536 candidate paths across all rows; import
and exact-import edges retain their independent budgets.

This is a strict matched worker/frontend migration. Earlier exact-scope versions
before 12 are rejected. Deploy both producers and consumers together and
regenerate fixtures through their owning producers.

Candidate offers use strict `TPMCAN` version 10 with seven fields and sixteen-field
module rows. Its execution parcel uses absolute authenticated graph-file
descriptors; acquired artifact owners may retain them outside the request directory, with the same graph bounds as exact scopes and a separate four MiB
metadata limit. This requires a matched producer/consumer deployment. Each native row retains its exact canonical requirements and sealed
module certificate/Core descriptor. Optional proof validation retains metadata
and permits absent Core. Admission captures selected interfaces, certificates,
package sidecars, native products and graphs under the request byte budget;
executable promotion also requires Core. Hydration and scope extension consume
that shared owner. Offers remain cache suggestions, checked against the request's
independently admitted compiler producer and complete selected interface closure
before promotion.

## Checked inspection inputs

Resident lookup captures reachable checked value interfaces with its immutable
actor view while owning the session checkout. The `inspection1` exact-scope
purpose seals their module inventory, original bytes and ordered include roots.
It authorizes inspection only; cell checking, prepared execution, certification,
declaration joining and display cannot consume it. Successful answers still
require exact consumed source receipts. Mutable session files and legacy
interfaces do not replace the retained checked certificates.

This is an internal matched worker/frontend migration: previous workers reject
`inspection1`. Existing graph and metadata versions and budgets are unchanged.

## Current source selection of retained originals

`TPEXACTCOMPILE` version 3 retains the fresh source receipt fields and a
separate current source-selection section. Each selected row names the exact
unit/module, canonical certificate SHA, interface SHA and source SHA. Current
source evidence records bytes, authored import adjacency and resolution
witnesses. This proof needs no native product or execution-source graph and
never grants native execution or Core-loading authority. Version 2 receipts
are rejected by the matched Rust consumer.

Only actual current imports under a checked request's complete sealed search
order can issue this proof. The worker validates the admitted canonical source
origin, current source and GHC exact-interface compatibility before installing
its source-import graph. Both consumers compare current adjacency with original
imports sealed by canonical issuance. Retained canonical type requirements
alone grant no lexical selection. Fresh dependencies remain fresh; import shapes
do not retain an original child or add compiler execution obligations. Session
implementation anchors use their existing independent checked-value and lexical authorities, never ordinary source-selection rows.
Fresh-source and retained exact imports form one canonical set-valued lexical
row through `preparedHomeRequirements`. Their provenance partition and authored
order do not change adjacency. Ordered source search roots remain a separate
selection contract.
The current GHC session's Finder roots own that search order. Reused module
summaries may keep preprocessing flags from an earlier request; their historical
include paths do not grant current lookup authority or invalidate its sealed roots.
Checking publishes its exact receipt at checked completion. Prepared results
retain the typed `ExactCompilation` and current source proof; product certification
publishes their receipt. Observe each operation through its own result and
publication owner rather than assuming they expose identical artifacts.

An explicit import in a submitted cell prologue requests current source
selection. The original GHC parser carries its module and package qualifier
through checking and prepared compilation; aliases and import lists do not
change that demand. Compiler-generated template/program imports retain their
existing exact owners. Using an already captured lexical name, qualified or
unqualified, retains that original identity even if its old source changes or
is absent. This demand check runs before frontend compilation; it is not a
general guarantee about arbitrary Template Haskell or plugin execution.

## Matched cell observation migration

Cell observations use `TPCELLOBSERVATIONS` version 3: the five-section payload
retains diagnostic types, nominal heads, expression lifting and the authored
prologue. Binder rows have three fields; expression rows have four.
Neither carries imports reconstructed from type presentation. Native checked
signatures supply the exact type authority.

`TPEXACTCHECK` uses version 3; planned-cell `TPEXACTPROGRAM` uses version 4 and
planned-item `TPEXACTITEM` uses version 2. Parser receipts use `TPCELLPLAN3`,
including the parser-owned action/let/recursive binding form. The matched
Rust/Haskell release rejects older observations and receipts. Worker fields
34 (`--turn-pin`) and 45 (`--cell-fold-turn`) are retired
and rejected explicitly. Original live inputs use a compiler-issued thin value
interface; execution uses admitted item recipes. Exact scope purposes are
`cell-check4`, `cell-program3` and `checked-item5`. Direct template roots are
separate from their closed support graph; support rows do not authorize new
template imports. Expressions reserve one
capture generation and observation name, with no auxiliary display recipe or
presentation admission. Authored display operations and activation previews
retain their separate compiler and runtime paths.

Each executable inference segment has one successful authored frontend and
one canonical finalized module with all its kept entry roots. Descriptors retain
actual GHC capture Ids, their types and Name-keyed fixities; Session's existing
decoder supplies the global Ids substituted into later roots before simplification.
Planned receipts seal the complete ordered segment plan and each entry's actual
source owner and compiler-issued original group ordinal. Generalized lets retain
their real GHC sigma types. Unresolved action-bound captures require a concrete
same-cell use or annotation and are refused before execution. Bare observations
retain a lazy result thunk, with effectful observations running their action once.
Per-item product projection and certification remain separate serialization work;
they do not run another authored frontend.

Canonical `TPFINALMODULE` version 3 certificates have thirteen fields. The final
field is `["source-original", imports]` or `["native-authored-declaration", generation]`;
the source list seals original authored import qualifiers, boot flags, and resolved
home owners separately from type requirements; the native form binds the issuer's
protected native declaration reservation. Scope
roles must match that authenticated origin. Canonical certificates keep profile `tidepool-ghc-finalized-module-v1`.
The compiler finalization envelope uses `tidepool-ghc-finalized-module-v2`
with separate source rows and captured selected value-interface rows. Value rows
retain the exact injected interface, package sidecar and nominal requirements;
they grant type closure only, never source, native groups or live bindings. Prior certificate and scope versions are
rejected; deploy the matched issuer and worker and regenerate evidence through
its producers. Canonical origin alone grants neither lexical import authority
nor native execution. Request-local native originals retain their protected
planned declaration and exact finalized owner/source association.

## Checked type signatures

`TPCHECKEDSIGNATURE2` carries a compiler-produced GHC interface declaration,
its exact external Name inventory, and separate presentation text. Generated
annotations use a placeholder which is replaced with `XHsType` before renaming;
the presentation text is never parsed to reconstruct the type. GHC supplies the
interface codec and type hydration, including binder kinds and coercions.

The enclosing checked receipt pins producer identity and the complete payload.
Only those admitted bytes reach the native GHC decoder. Before hydration, the
worker verifies the signature declaration identity and its complete Name census
against the receipt; home Names must already exist in the admitted environment.
The canonical activation witness remains a separate semantic equality contract:
GHC binary bytes are not a canonical type fingerprint.

Pure activation renderers use the nine-field `host-activation-renderer1`
authorization. It seals the original execution-context identity, budget,
protected template digest, original native signature and canonical witness,
complete original instance interface graph, its original target owner, and
ordered include roots. It contains no mounted value interface, binder or child
generation. Protected target/fingerprint edges expose that graph only to the
renderer instance traversal; they grant no authored imports or lexical names.
The target's canonical interface supplies its original orphan visibility census,
including its own orphan identity. A scoped renamer callback restores that census
before typechecking. The worker compiles only
`Input -> Eff '[] (Text, Bool)`: its single `TidepoolActivationInput` slot is
replaced with the original native type before renaming. The probe solves the
exact `WorkbenchDisplay` constraint; a missing instance permits opaque output.
Missing code for a selected dependency remains unavailable. The final input
argument must match the original canonical witness.

Successful compilation emits an expression turn and the seven-field
`TPEXACTACTIVATIONRENDERER1` receipt. Missing selected original home dependencies
instead emit the six-field `TPEXACTACTIVATIONRENDERERUNAVAILABLE1` result with no
turn or executable packet. Both bind the issuer's original context and input
witness. Other source, authority and infrastructure failures remain failures.
Renderer evidence is retained by the runtime's original program owner; each
child receives its own fresh value interface, mount and affine invocation
admission. No rendered value is cached. Earlier child-bound preview purposes and
receipts are rejected by this matched worker/frontend migration.

Qualified imports from a protected template retain their exact alias and
interface graph; changing or duplicating an import cannot inherit that authority.

This is a strict internal migration. Old three-field printed signatures are
rejected. Deploy the Rust consumer and Haskell worker together and regenerate
compiler-produced artifacts through their owning producers.

## Thin binding interfaces

`TPHOSTBINDINGINTERFACE` version 2 adds a closed purpose after the original
nine fields: `["host-built"]` or `["original-live-input", canonical witness bytes]`.
`TPHOSTBINDINGINTERFACERECEIPT` version 2 adds the matching result after its
original eleven fields. Version 1, unknown purposes, wrong arity, noncanonical
CBOR and trailing bytes are rejected. Witness bytes retain their four MiB bound.

Host-built issuance requires the existing authenticated representation. Original
live-input issuance resolves the original native signature only through the
admitted exact interface environment, independently captures and seals its type,
and compares canonical structure and original owner seals with the offered
witness. The offered native signature must match the supplied original signature
exactly; freshly captured GHC binary signatures are not canonical fingerprints.
The receipt returns the independent sealed witness. Original live-input binders
have no host-builder authority, including Text and JSON input types. Both paths
share the native thin writer and emit no prepared products or source compilation.

## Compiler-issued execution recipes

`TPCERT` version 10 is a closed sum of ordinary products, segment originals,
and segment items. `CurrentOriginalInventory` emits immutable modules, native
groups, package availability, captured finalization and execution recipes once
at the segment root. Item receipts carry complete target global witnesses and
only target package additions. Captured future value types remain private; exact
checked-item selection determines their visibility. Older versions are refused.
Ordinary and segment-original variants retain one closed source-recipe result
after their finalized-module envelope: `["ordinary"]`, `["exact-unavailable", reason]`, or
`["exact-available", SHA]`. An available result binds the immutable
`execution-source.cbor` in the owning compiler output directory. The descriptor
contains no worker-selected path. Metadata keeps its four MiB bound and the
existing graph keeps its independent 64 MiB bound.

The exact compiler issues this graph once. The same object supplies later worker
passes and the receipt sidecar. Frontend admission authenticates its original
source, producer, semantic request, complete native owner inventory, exact
imports and per-original source-import package closure before retaining its
original bytes. Native-global package witnesses are separate evidence and cannot
replace the source-import closure. An advertised missing, corrupt or mismatched
graph is a refusal; exact requests never fall back to ordinary graph issuance.
Ordinary requests have no intra-request graph consumer and retain their existing
single frontend issuer. Unavailable reasons are `no-fresh-originals`,
`incomplete-source-evidence`, `unsupported-source-recipe` and
`unavailable-source-root`.

This is a strict matched worker/frontend migration. Earlier `TPCERT` versions
are rejected. Regenerate actual source-boot candidate receipts through their
Haskell producers and run the structural prepared corpus; existing prepared
wire artifacts and execution-graph versions do not change.

Native product origins are `fresh`, `cached`, or `retained-core`. The last is
exact-request-only: actual projected home globals demand complete native
preparation from the original admitted canonical interface/Core pair. It keeps
that original certificate, source identity, package imports and dependency
seals; it grants neither current source selection nor a source execution recipe.
Its module row seals the original certificate in the dependency-witness field
and the newly emitted native aggregate in the product field. Advertised corrupt
inputs are refusals. Missing defining capability remains native unavailability.
Fresh and retained complete originals share site elaboration and preparation;
package body subsets retain their separate coverage and cannot issue original
home products.

## Original interface requirements

`TPCERT` records each original module's interface-only dependencies as
sorted exact unit/module/SHA-256 rows. Both GHC home usage forms contribute;
self usages are excluded. Every seal must match a fresh same-transaction
interface or the admitted exact interface closure. Authored import adjacency
and executable group/global requirements remain separate evidence.

The durable `TPHOMEOWNERS` version 5 preserves those interface seals and an
explicit optional execution-source digest. Inventory admission checks the
required interface bytes and compiler producer before adding retention edges.
Native witness reuse retains this same proof; cold recovery must preserve every
certified interface edge. Earlier product and Home certificate versions are
rejected. Deploy the matched worker and frontend and regenerate artifacts
through their owning producers.

## Native constructor replies

Prepared schema 17 / execution ABI 9 carries the constructor reply table and
one finite reply-type graph. Earlier prepared schemas are rejected; registered
prepared artifacts must be regenerated through their original compiler producers.
The graph stores scoped expressions, nominal declarations, original constructor
field templates and ordered typed edges. Newtype RHS templates retain their
eta-prefix scope. Recursive and nonregular recursive fields link declarations
without instantiating an expanding field tree.

An intrinsic static reply uses the complete source `DataCon` binder telescope;
only the exact compiler-issued request-site carrier selects `AtSite`. A non-leading
carrier separately seals its original site and retained payload field while
preserving the closed reply type. Capture authority requires the payload type
to equal the carrier's final closed input. Open
parameters, functions and opaque families retain structural identity without
acquiring host construction authority. Original field kinds and worker layouts
must agree with the existing physical constructor inventory. Fieldless branches
remain available when another branch requires an unsupported payload.

Rust validates and freezes one `Arc<TypeGraph>` backed by `petgraph`, with
read-only graph access. Construction resolves a scoped expression and shared
argument environment on demand. Reachable reply compatibility ignores local
node IDs and diagnostic rendering. Whole ordered site evidence commitment is a
separate contract. Exact wire/content identity still includes diagnostics;
rendered type text never authorizes duplicate reply admission. Closed activation
type bytes and original interface seals retain their separate existing format.

## Native request types

Request-site result types use `TPREQUESTTYPESIGNATURES1` version 1, containing
one native `request-reply` signature and an optional `request-progress`
signature. The complete bundle is limited to four MiB and belongs to the
original site's metadata seal. Progress signatures issue only for the known
progress request/child verbs with their declared input and answer positions;
canonical input witnesses remain separate.

This is a strict sidecar migration: inline typed-site rows now have ten fields,
and JSON rows must include `request_type_signatures` (null for nonrequest or
constructor replies). Seven-, eight- and nine-field inline rows and JSON rows missing
the new field are rejected. Deploy matched worker/frontend binaries and
regenerate retained compiler artifacts through their producers. Request
authority consumers must additionally require original-site native signatures;
decoding an observation alone does not grant that authority.

## Generated fixtures

Recovered package executable bodies come from their defining interface's
original fat Core. Optimizer unfoldings remain frontend metadata; reconstructing
them can change exported allocation and entry contracts. Missing original Core,
unreadable interfaces and incompatible body types retain distinct refusals.
Exact recursive groups and defining group order stay intact. Selected groups
include the transitive private top scope from that same decoded interface;
external sibling dependencies retain the normal demand loop. Final selected emission still checks
the canonical entry signature and evaluatedness.

Recovery retains a component ledger under one target owner. Each canonical
unit carries its exact original version and the retained declaring-context
issuer; equal interface bytes do not prove equal dependency contexts. Cache
copy, owner selection and eviction retain that issuer. Pure units and one
constructor-worker arena survive demand growth. A complete site batch replaces
its previous arena, advances the site epoch and rebuilds target reachability.
New units issue dependency facts once; the existing bounded executor has one
completion pump for ready units and newly discovered CorePrep references.
At quiescence, component projection supplies actual exact original-package
demand to the same ledger. Final candidate emission checks entry ABI and
conflicting evidence before flattening the stable closure.

After changing translation or serialization, run `just fixtures-check`.
Each native corpus producer compiles its declared module/targets and emits
constructor metadata and prepared programs from that same graph. The native
validator checks all requested programs against an independently compiled
GHC oracle. Fixed prepared fixtures and generated effects are declared output
resources consumed at runtime, rather than committed prepared blobs.

Change the owning source or target roster to change a fixture. Buck rebuilds
immutable outputs from complete declared inputs; no source refresh/updater,
ambient compiler cache, or committed metadata fingerprint is the acceptance
owner. Retain actual execution counts and reports separately from compilation.

The shared Haskell `Tidepool.Test.FixturePacket` transport consumes the Rust
`module_candidates::fixture_packets` completion owner. A successful libtest exit
alone does not establish fixture issuance. Completion is one-shot and binds the
specific producer, consumed request bytes, private packet and output hashes.
Genuine compiler captures and structural codec requests retain separate entries;
completion grants no compiler authority. Candidate manifests are delivered from
a verified packet snapshot; their work-owned graph companions retain the original
publication identity and pass independent graph and candidate admission checks.

Mutation tests first admit a genuine envelope through the production parser and
use `CodecFixtureSupport` to retain fields outside the mutation. A file-only
fixture proves fresh acquisition; it cannot prove continuation reuse. Reuse tests
retain live sealed arenas through the complete read sequence, preserve the
issuer-selected semantic rows, and read the adapted envelope through the same
production parser. Logical path placement does not replace producer, owner or
content admission.

## Extractor diagnostics

Diagnostics are opt-in:

| Variable | Purpose |
|---|---|
| `TIDEPOOL_TIMING=1` | phase timings, nested breakdowns, and counts, correlated by compiler request |
| `TIDEPOOL_VARID_AUDIT=1` | report VarId collisions |
| `TIDEPOOL_VARID_AUDIT=<hex>,...` | resolve selected VarIds to names |
| `TIDEPOOL_DANGLING_DEBUG=1` | show unresolved references before allowed session refs are removed |
| `TIDEPOOL_IFACE_DEBUG=1` | trace fat-interface loading |

`TIDEPOOL_TEST_DROP_DC` and `TIDEPOOL_TEST_FORCE_VALIDATION_ONLY` are
fault-injection controls for extractor tests, not debugging defaults.

For a separate symbol-rich worker and bounded phase-aligned CPU/RSS captures,
see [compiler profiling](../../docs/compiler-profiling.md).

## Worker lifetime

The Rust frontend owns CLI parsing, Unix sockets, daemon configuration, and
process lifecycle. It either starts this worker for one typed request or keeps
one worker alive with `--worker-loop-v2`. `Tidepool.WorkerServer` owns only the
framed stdin/stdout loop; `Tidepool.GhcPipeline` owns the resident compiler
state. `Main` decodes a typed request and dispatches compiler operations; it is
not a second CLI or workflow engine. Completed compiler products belong to the
resident compiler universe. Each request validates the selected source, policy,
dependency and interface versions before reusing them across scoped requests.
An attempt publishes additions only after the whole compiler operation succeeds;
a synchronous refusal discards partial additions while preserving previously
completed versions. Cancellation terminates the transaction. Transactions and
their requests are serialized and carry their own CWD and compiler options.

The worker process environment and trusted RTS options are fixed at startup.
The daemon supplies `-I0` for resident workers, retaining allocation-driven GC;
direct finite invocations keep the normal idle-GC default. Idle collections no
longer trigger finalizers or GHC idle deadlock detection between requests; normal
collections still run finalizers. The frontend's request deadlines, cancellation
and process reaping own resident-request liveness.
Restart the daemon after changing extractor diagnostic variables, GHC
configuration, or its watched toolchain stamp.

## Eval library

`lib/Tidepool` is the model-facing Haskell surface. Prefer familiar Haskell
APIs and types; novelty here directly costs model fluency.

Key modules:

- `Tidepool.Prelude`: default imports and common helpers;
- `Tidepool.Form`: schema-derived operator forms;
- `Tidepool.Async`: authored concurrency;
- `Tidepool.Worktree`, `Shell`, `Cargo`: typed operational helpers;
- `Tidepool.Agent`: coding-agent delegation;
- generated `Tidepool.Effects`: stable effect types and authored verbs.

Pure reusable helpers belong in named library modules. Effect constructors and
wire records belong in `tidepool-protocol`/`tidepool-mcp`, not handwritten
copies in the library.

`ask` and `llm` share the `Schema` vocabulary. `ask` suspends for a caller
reply, which the caller checks; `llm` requests schema-constrained output from
the model provider and parses its JSON response.

## Adding Prelude or library functions

1. Put the implementation in the narrowest appropriate module.
2. Export it explicitly.
3. Add it to auto-imports only when it belongs in the default model-facing
   vocabulary.
4. Extend an existing bundled surface test where possible; do not create a new
   extractor compile for a one-line assertion.
5. Rebuild the extractor if the module is part of the deployed library.

## Recorded design rationale

- **Diagnostic structure loss** (`bridge/haskell/src/Tidepool/DiagJson.hs`,
  `bridge/haskell/src/Tidepool/Introspection.hs`). GHC's `diagnosticCode` (the
  `[GHC-NNNNN]` code) is never extracted as a field by `envelopeToDiag`; it
  reaches Rust only if GHC's own rendering happens to embed it in the message
  text. `InspectionRejected String` discards span and severity even earlier,
  before any wire encoding, unlike the cell-compile path's `Diag` JSON report.
- **Why `matchQuality` splits the sigma type first** (`Introspection.hs`,
  `matchQuality`/`matchesEitherDirection`). Full-sigma `tcMatchTy`/`tcUnifyTy`
  only succeeds on alpha-equivalent polymorphic types and is useless for "is
  this candidate compatible" search, so matching splits off the type body
  (`tcSplitSigmaTy`) and matches bodies symmetrically; predicates gate to
  full-type equality whenever either side has constraints, because body-only
  matching would silently erase predicate evidence.

## Current limits

- Topological recovery can only recover bindings whose dependencies are
  available through prepared recovery or accepted session modules.
- Session-generated modules require the universal Authored module pair
  and stable effect vocabulary. Helpers use `Member` constraints and `Eff`
  rows; no implicit `M` alias is generated. Authors may define explicit aliases.
  Persisted contracts expand effect-row aliases, and GHC enforces compatibility
  at use sites.
- Each resident worker serves one request at a time and changes process CWD
  under that request owner. An admitted job grant bounds GHC module make and
  independent prepared lowering, projection and recovery within the request;
  module tasks consume acquired typed inputs and do not mutate its live Session.
  Concurrent requests in one process still require a different CWD model.

Add a limit here only when it is present, user-visible, and not already made
unrepresentable by the current API.
