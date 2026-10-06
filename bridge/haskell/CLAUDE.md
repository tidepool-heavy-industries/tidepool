# bridge/haskell/ — extractor and eval standard library

## Charter

This directory owns the GHC-to-prepared-STG compiler worker and `lib/Tidepool`, the Haskell
library auto-imported by the MCP surfaces. Toolchain discovery and caching live
in `tidepool-toolchain`; CBOR decoding lives in `tidepool-repr`.

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

`just exomonad-run BUNDLE DESCRIPTOR REPORT ...` and
`just exomonad-init BUNDLE DESCRIPTOR REPORT ...` select that verified artifact
environment. `just doctor BUNDLE DESCRIPTOR` verifies and prints the selection.
`scripts/redeploy.sh` delegates to the same freeze owner and preserves existing
installations and live hosts. Standalone toolchain APIs retain their resolution
contract in `tidepool/toolchain/src/toolchain.rs`; they do not issue deployment
qualification.

## Exact-scope transport

Exact-scope manifests use strict `TPEXACTSCOPE` version 9 with nine fields. The final
fields contain an execution parcel or null and a compiler-purpose authorization
or null. Interface rows have eight fields; their final field declares one
closed artifact role: `["module", certificate path, certificate SHA, optional
Core path, optional Core SHA]`, the same five-field `["native-declaration", ...]`
for native authored originals, `["join"]`, or `["value"]`. Native product
owners require canonical module evidence. Roles never come from module-name spelling.

Canonical module certificates bind the compiler producer, finalized interface
and package bytes, original source digest, exact dependency seals, optional
Core digest, and complete compiler home-unit inventory. The worker checks this
proof against the selected interface closure before hydration. A Core companion
is a separate compiler input; reading a type context does not load it or grant
native execution or lexical imports. Its demanding recovery owner verifies and
decodes the compiler-native payload without replaying source or Template Haskell.

Execution parcels retain `[SHA, absolute graph-file path]` descriptors and exact
original references. Exact-scope graph paths come from the retained immutable
artifact owner; the request keeps its complete parent custody alive while the
worker consumes them. Their sealed paths and digests transport selected bytes;
graph producer and complete original identity checks establish compatibility.
Unchanged `TPEXECUTIONSOURCE` bytes remain independent of the metadata envelope.
Metadata is limited to four MiB. Execution graphs retain their 64 MiB aggregate
and 4096-graph bounds. Authored source is bounded separately at 32 MiB of UTF-8;
other text and metadata keep their existing bounds. Advertised invalid or
oversized parcels are rejected. Resolution evidence retains at most 65,536 rows,
4,096 candidate paths per row and 65,536 candidate paths across all rows; import
and exact-import edges retain their independent budgets.

This is a strict matched worker/frontend migration. Earlier exact-scope versions
2, 4, 6, 7 and 8 are rejected. Deploy both producers and consumers together and
regenerate fixtures through their owning producers.

Candidate offers use strict `TPMCAN` version 10 with seven fields and sixteen-field
module rows. Its execution parcel uses graph-file descriptors beside the candidate
manifest, with the same graph bounds as exact scopes and a separate four MiB
metadata limit. This requires a matched producer/consumer deployment. Each native row retains its exact canonical requirements and sealed
module certificate/Core descriptor. Offers remain cache suggestions: admission
checks them against the request's independently admitted compiler producer and
complete selected interface closure before promoting their durable proof.

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

`TPEXACTCHECK` and `TPEXACTPROGRAM` use version 3; parser receipts use
`TPCELLPLAN2`. The matched Rust/Haskell release rejects older observations and
receipts. Worker fields 34 (`--turn-pin`) and 45 (`--cell-fold-turn`) are retired
and rejected explicitly. Original live inputs use a compiler-issued thin value
interface; execution uses admitted item recipes. Exact scope purposes are
`cell-check4`, `cell-program3` and `checked-item5`. Direct template roots are
separate from their closed support graph; support rows do not authorize new
template imports. Expressions reserve one
capture generation and observation name, with no auxiliary display recipe or
presentation admission. Authored display operations and activation previews
retain their separate compiler and runtime paths.

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

Pure activation previews use the thirteen-field `host-activation-preview3`
authorization. It seals the admission, preview generation and budget, protected
template digest, mounted input generation and complete binder metadata, original
native signature and canonical witness, mounted value interface/package seals,
complete original instance interface graph, its original target owner, and ordered include roots. Protected
target/fingerprint edges expose that graph only to the preview's instance
traversal; they grant no authored imports or lexical names. The target's canonical
interface alone supplies its original orphan visibility census, including its
own orphan identity. A scoped renamer callback restores that census before the
preview's declarations are typechecked. The worker compiles only
`Input -> Eff '[] (Text, Bool)`: its single `TidepoolActivationInput` slot is
replaced with the original native type before renaming. An opaque probe solves
the exact `WorkbenchDisplay` constraint; a missing instance permits opaque
output. Missing code for a selected display dependency has a separate unavailable
result. The final input argument must match the original canonical witness.

Successful preview compilation emits an expression turn and the eight-field
`TPEXACTACTIVATIONPREVIEW1` receipt. If projection of the selected preview finds
`UnavailableOriginalHomeDependencies`, the worker instead emits only the
seven-field `TPEXACTACTIVATIONPREVIEWUNAVAILABLE1` result. It binds the original
request, admission, generation, template digest and exact input witness bytes;
no turn or executable packet is emitted. Other projection, source-loading,
authority, and infrastructure failures remain ordinary failures. Neither
preview result creates an input value, value interface, or authored checked
completion. The retired `host-input-check1` and
`host-activation-input2` purposes and activation-input receipts are rejected.
Qualified imports from a protected template retain their exact alias and
interface graph; changing or duplicating an import cannot inherit that authority.
The prior `host-activation-preview1` and `host-activation-preview2` authorizations are rejected; deploy the
matched frontend and worker together.

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

`TPCERT` version 9 retains one closed source-recipe result after its finalized-module
envelope: `["ordinary"]`, `["exact-unavailable", reason]`, or
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

The durable `TPHOMEOWNERS` version 4 preserves those interface seals and an
explicit optional execution-source digest. Inventory admission checks the
required interface bytes and compiler producer before adding retention edges.
Native witness reuse retains this same proof; cold recovery must preserve every
certified interface edge. Earlier product and Home certificate versions are
rejected. Deploy the matched worker and frontend and regenerate artifacts
through their owning producers.

## Native constructor replies

Prepared schema 16 / execution ABI 9 carries the constructor reply table and
one finite reply-type graph. Earlier prepared schemas are rejected; registered
prepared artifacts must be regenerated through their original compiler producers.
The graph stores scoped expressions, nominal declarations, original constructor
field templates and ordered typed edges. Newtype RHS templates retain their
eta-prefix scope. Recursive and nonregular recursive fields link declarations
without instantiating an expanding field tree.

An intrinsic static reply uses the complete source `DataCon` binder telescope;
only the exact compiler-issued request-site carrier selects `AtSite`. Open
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

- **Read-only questions and typed child groups**
  (`bridge/haskell/actors/Tidepool/Actors/Unfold.hs`). `errand` starts a small
  invocation-owned inspection leaf without a fork group or worktree; await its
  watch in the same invocation. `unfold`/`child` admit typed applicative groups
  immediately from `selected` or `fromCheckpoint` context, so their continuation
  can await results before returning. `unfoldDeferred` instead publishes after
  the enclosing call's real result and requires explicit persistent lifetime;
  never await its children before that call returns. Use `ActorOwned` branches
  or `withAgentLifetime ActorOwned` on a direct `AgentLaunchSpec` for work
  intentionally spanning invocations. Returning a handle does not extend lifetime.
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
