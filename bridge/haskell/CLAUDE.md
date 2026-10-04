# bridge/haskell/ — extractor and eval standard library

## Charter

This directory owns the GHC-to-prepared-STG compiler worker and `lib/Tidepool`, the Haskell
library auto-imported by the MCP surfaces. Toolchain discovery and caching live
in `tidepool-toolchain`; CBOR decoding lives in `tidepool-repr`.

## Build the compiler worker

From the repository root, after materializing and configuring pinned tools:

```bash
just build //bridge/haskell:tidepool-extract-bin
just test-native //bridge/haskell:source_boot_product_reuse_test --list-tests
```

The executable is an internal compiler worker, not a user-facing CLI. It accepts
only the versioned request protocol emitted by `tidepool-extract-cmd`, or
`--worker-loop-v2` behind the resident daemon. Native Rust/Haskell test targets
carry matched frontend, worker, deployment, GHC and source resources. The
production worker uses its production toolchain; host Tasty compilation uses
`toolchains//:haskell_tests`. Cabal declarations own component module/package
rosters, from which `scripts/buck2-haskell-tests.py` generates native targets.

## Toolchain resolution and deployment

`tidepool/toolchain/src/toolchain.rs` owns resolution and the startup
fingerprint check.

Frontend precedence:

1. `$TIDEPOOL_EXTRACT`; a set but invalid value is an error.
2. `tidepool-extract` on `PATH`.

Standard-library precedence:

1. `$TIDEPOOL_PRELUDE_DIR`; it must contain `Tidepool/Prelude.hs`.
2. A repository `bridge/haskell/lib` or `lib` found by walking upward from CWD.
3. The library beside a worktree-built extractor's `dist-newstyle`.
4. The library embedded in the server binary (release builds only, when built
   with `TIDEPOOL_EMBED_HASKELL=1`; a dev build embeds nothing and resolves
   from the checkout at step 2 instead).
5. The source tree from which the binary was built.

Use `scripts/redeploy.sh` to deploy the extractor, Rust servers, embedded
library, cache state, and toolchain stamp as one operation. For deliberate
mixed local testing, set `TIDEPOOL_TOOLCHAIN_HANDSHAKE=warn`; do not weaken the
default handshake.

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
original references. The request owner captures graph files beside the manifest;
unchanged `TPEXECUTIONSOURCE` bytes remain independent of the metadata envelope.
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

Cell observations use `TPCELLOBSERVATIONS` version 2: the five-section payload
retains diagnostic types, nominal heads, expression lift/presentation and the
authored prologue. Binder rows have three fields; expression rows have five.
Neither carries imports reconstructed from type presentation. Native checked
signatures supply the exact type authority.

`TPEXACTCHECK` and `TPEXACTPROGRAM` use version 2; parser receipts use
`TPCELLPLAN2`. The matched Rust/Haskell release rejects older observations and
receipts. Worker fields 34 (`--turn-pin`) and 45 (`--cell-fold-turn`) are retired
and rejected explicitly. Whole-cell checking remains the initial admission step
for host inputs; execution uses admitted item recipes.

Canonical `TPFINALMODULE` version 3 certificates have thirteen fields. The final
field is `["source-original", imports]` or `["native-authored-declaration", generation]`;
the source list seals original authored import qualifiers, boot flags, and resolved
home owners separately from type requirements; the native form binds the issuer's
protected native declaration reservation. Scope
roles must match that authenticated origin. The finalization profile remains
`tidepool-ghc-finalized-module-v1`. Prior certificate and scope versions are
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

Host input checking carries that same original witness through the protected
`host-input-check1` admission. The generated `TidepoolActivationInput` type slot
is replaced with the native type before renaming; it is not an imported type
or a new alias. The initial check and preview compilation retain separate
purposes and independently validate the resulting input type.

This is a strict internal migration. Old three-field printed signatures are
rejected. Deploy the Rust consumer and Haskell worker together and regenerate
compiler-produced artifacts through their owning producers.

## Compiler-issued execution recipes

`TPCERT` version 8 retains one closed source-recipe result after its finalized-module
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

Prepared schema 15 / execution ABI 9 carries an exact constructor reply table.
Each entry is `StaticReply TypeNodeId` or `ReplyAtSite`. Static reply graphs come
from the saturated final lifted GHC DataCon result index, independently of the
visible effect row or `KnownEffect` instances. Unresolved replies remain
unconstructible nodes; partial algebraic graphs retain valid fieldless branches.

Only the exact `Tidepool.Internal.RequestSite` TyCon with matching reply index
and a proven first runtime Int field selects `ReplyAtSite`. Its private newtype
constructor and nominal input/result roles prevent authored retagging. The
compiler emits the carrier directly. There are no synthetic constructor sites,
open reply defaults, numeric payload fallbacks, or absent-site sentinels.
Stateful receive has intrinsic `Maybe state` evidence and keeps its separate
checkpoint/reply/continue correlation key.

This is a strict matched producer/runtime migration. Reject schema 14 / ABI 8
artifacts and regenerate corpus and embedded artifacts through their producers.

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
not a second CLI or workflow engine. Compiler-valued caches and recovery graphs belong to one admitted transaction
and are released when it closes. A synchronous failed compilation clears those
graphs before a permitted retry; cancellation terminates the transaction.
Transactions and their requests are serialized and carry their own CWD and
compiler options.

The worker process environment is fixed at startup. Restart the daemon after
changing extractor diagnostic variables, GHC configuration, or its watched
toolchain stamp.

## Eval library

`lib/Tidepool` is the model-facing Haskell surface. Prefer familiar Haskell
APIs and types; novelty here directly costs model fluency.

Key modules:

- `Tidepool.Prelude`: default imports and common helpers;
- `Tidepool.Form`: schema-derived operator forms;
- `Tidepool.Async`: authored concurrency;
- `Tidepool.Worktree`, `Shell`, `Cargo`: typed operational helpers;
- `Tidepool.Agent`: coding-agent delegation;
- generated `Tidepool.Effects`: the effect row and verbs for a compile.

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
  plus a per-incarnation row shim. Persisted contracts normalize `M` to the
  exact concrete row; GHC then enforces compatibility at use sites.
- The resident worker is single-threaded and changes process CWD per request;
  parallel request execution would require a different isolation model.

Add a limit here only when it is present, user-visible, and not already made
unrepresentable by the current API.
