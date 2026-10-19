# Wave 1: exact worker realizations

Status: proposed. Supervisor: [warm workers](README.md). Base: parent integrated contract and implementation revision descended from `70a92c65a02a591f76590daf4e5bfa3e6fcbf723`. This wave makes source-free exact input realization reusable; it does not assume cached `TcGblEnv` is safe or eliminate ordinary generated imports.

## Assignment, inputs and outputs

Implement in the existing toolchain/Haskell compiler owners one opaque worker realization of an **already host-issued exact root**. Inputs are the logical root, its selected compiler roles/explicit Original decisions/exact outgoing edges, immutable owned row parts and optional proven package-universe owner. Output is a worker-instance-bound reference to completed selected metadata, with a request-local hold and a typed acquisition result. The host remains the authority for selection and publication. Root registration observes no user source and issues no executable/native permission.

Dependencies: host-inputs supplies the actual current/candidate root and checked deltas. Consume the accepted owned-inputs/01 row-ownership slice and existing transport together with owned-inputs/03 Wave A lease/granularity contract; those provide independently retained portable row/arena owners and fresh output directories. The complete owned-inputs/03 transport Wave B joins after this protocol is reviewed, so it is not a circular prerequisite. Source-backed package proof is optional; sound operation-scoped observation mode is a valid initial capability. Freeze the shared contract before branching protocol implementation. A raw root digest, inventory list or mutable package path is not an eligible substitute.

Owned implementation paths:

- `tidepool/toolchain/src/declaration_context.rs`, `exact_context.rs`, `artifacts.rs`: root-to-physical-offer consumer and receiving evidence; coordinate root type ownership with host-inputs.
- `bridge/haskell/src/Tidepool/GhcPipeline.hs`: `CompilerUniverse`, `withResidentCompilerScopes`, `residentCompileOne`; realization issuance/selection, shared facts and fresh attempts.
- `ExactScope.hs`: `readExactScopeWithOwner`; `OwnedInputTransport.hs`; `ExactHydration.hs`: `forkExactContextWithPackageFacts`, `RequestIfaceDecoder`, `hydrateOriginalInterfaces`.
- `CompilerRequest.hs`, `ExtractRequest.hs`, `WorkerServer.hs`, `app/Main.hs`: source-free dispatch and current-purpose binding.
- `tidepool/extract-cmd/src/request.rs`, `endpoint.rs`, `daemon.rs`: matched encoding/transport and exact worker issuer, **not** a new semantic cache/selection registry. Shared wire edits have one owner in this wave.

## Minimal protocol and state transitions

Use the supervisor's proposed `EnsureExactInputs` source-free exchange inside the existing pinned compiler transaction. Its input is a host exact-root reference plus a tagged acquisition: `Resident(realization_ref)` or `Owned(selected_parts_or_checked_delta)`. Its closed result is:

| Result | Contract |
| --- | --- |
| `Ready(realization_ref)` | This transaction holds a complete realization of precisely that logical root under the actual worker/name/package owner. It authorizes only the subsequent request's input borrow. |
| `NeedsAcquisition(root)` | No user-source operation has been accepted or executed for this exchange. Request complete portable acquisition of this same selected root; an unknown/evicted realization need not know its part inventory. The caller may submit that Owned source-free exchange in the same transaction. |
| `Rejected(cause)` | Invalid root/issuer/selection/delta, unsupported compiler profile, inaccessible or corrupt input, exhausted budget, or other typed refusal. Never choose a different implementation to recover. |

A checked delta is accepted only against its authenticated retained base. A missing/evicted delta base returns NeedsAcquisition for the complete target root; do not enumerate unknown missing parts, fetch broad custody, or add a per-part negotiation framework. On a confirmed miss the host sends the complete portable selected root once; another inability to establish it is an explicit acquisition/refusal result, not a compile retry loop.

After `Ready`, the normal versioned compile/query request carries the host root/reference, realization reference, new target inputs and fresh current purpose. It must verify the same root and hold before entering source execution. Receiving an accepted compile response without confirmed outer END remains insufficient for publication. Lost/uncertain source-free or compile responses retain their exact physical ownership and uncertainty; do not replay them. A **confirmed successful** NeedsAcquisition response is a normal protocol continuation, not failure retry or unsubmission of BEGIN.

Bind the reference to a fresh worker-instance issuer and the actual NameCache cell/issuer, producer/profile, package-owner mode and selected-root identity. Daemon epoch+slot or PID alone is invalid because worker replacement reuses slots. Treat a stale/evicted reference as NeedsAcquisition before source entry; distinguish wrong claimed root/owner from a genuine unavailable realization. Direct transport uses the same exchange and fresh transaction owner, with no promise of reuse after process exit.

The Haskell compiler universe owns immutable realized facts. The current transaction takes a hold through END. A completed world can remain in the bounded worker store after END, but no actor transaction/CPU grant remains. The host root owns portable input bytes independently; eviction/replacement can lose acceleration without losing authority or requiring source/effect replay. Cache publication is atomic only after all source-free input validation/hydration completes; partial knots, failed reads or cancelled registration cannot produce Ready. An unreferenced complete realization may be evicted normally.

## Exact selected world, not retained inventory

Derive roots once from the host issuer's compiler role map. Consume `RequestNativeAvailability::for_compiler_roles`; do not reproduce filtering in registration. Carry the selected map and exact edge identity into realization validity. Dormant equal-interface alternatives may share byte/decoded custody but are not optional roots and must not enter consumed namespace or source visibility. Exact required historical children survive unchanged even if they are not direct selected roots. A genuine required child contradicting an explicit Original selection still refuses at `validate_consumed_native_namespace`.

Keep canonical/source selection, acquired bytes, native availability and executable demand typed separately. Registration does not promote bounded groups to Full, infer absence from missing observations, select all bodies with an equal canonical ABI, or rebind historical edges through a latest-module map. New purpose/private availability remains request-local. An issued candidate root may serve explicitly admitted private/preparation operations and ordered private prefixes under its issuer and current purpose before publication. Only successful host publication advances the actor's publicly selected root; Ready does not do so.

## GHC realization layout

Retain immutable selected interface/detail facts and their actual dependency/package owners. Share common completed nodes through existing module/recovery caches. A realization records a selected persistent HPT/graph/import projection, not an entire per-actor ancestor chain of mutable HscEnv values. Audit what each retained closure pins; live selected rows must not retain cumulative request/output trees unnecessarily.

Preserve `forkExactContextWithPackageFacts` restrictions: CompManager, one definite home unit, shared fixed package EPS, private home/file finder, cleared attempt knots/IC/preview plugins. Keep the `fixIO` hydration knot closed over the exact selected versions. Decoded `ModIface` can reuse `(NameCache owner, unit,module,SHA)`; decoded `ModDetails`, solver/instance environments and lazy closures require their own exact dependency-world ownership. Name/Unique/`eqType` equality cannot validate a cross-world issuer. A new NameCache cannot borrow old decoded EPS/HPT.

Create fresh mutable checking cells, target hooks/diagnostics, output root and current-purpose executor for each source operation. Do not retain raw `TcGblEnv` as a reusable checking engine in this wave. Sharing completed closed worlds is serial per worker; the outer Ghc session/CWD/output capture remains single-owner. Separate metadata from `activateCompilerInterpreter`: compatible code may persist, incompatible native symbols rotate the interpreter, and realization metadata must never turn stale bytecode/ForeignHValues into current execution authority.

A package-universe acceleration is available only if owned-inputs proves that host resolution, GHC finder/EPS lazy loads and native loader consume the same retained immutable compiler/package closure. Without that proof, expose the capability as unavailable and preserve operation-scoped package observations/checks. Do not claim that a path/hash or shared EPS alone eliminated package freshness work. Existing TH/dependent-file/plugin no-reuse gates remain.

## Implementation sequence

1. Define the matched typed exchange, worker-instance issuer and pre-source results with the parent contract. Add malformed/missing/foreign-field protocol controls before a production consumer uses the reference.
2. Extend CompilerUniverse with a completed realization owner and explicit request hold; reuse existing captured input/decode/package owners. Add a full cold acquire path through the production source-free front door.
3. Add Resident acquisition and current-root/purpose binding, then same-transaction confirmed miss-to-Owned continuation. Stage valid output construction before Ready. Verify host root retirement/reload fences with reload tranche.
4. Route one production metadata inspection through the exchange while retaining its ordinary CheckedEnvironment leaf frontend. Then route the cell input path. Check early joins before adding broad reuse.
5. Remove duplicated normalized-baseline assembly only where the new owner proves identical selected facts. Keep fresh target/output production, receiving allowances and physical error distinctions. Update owning docs and version compatibility together.

## Frontdoor controls and independent oracle

Compare acquired exact inputs and final products against the existing complete cold path, plus independent expected roots/edges. The old differential helper shares selection policy and cannot independently establish optional-root correctness. Use independently issued artifact histories and assert direct roots, exact required children, unrelated child exclusion and byte custody separately.

Required histories:

- Both possible explicit Original winners with equal canonical interface; keep the loser in custody. Later native and notebook lookup and a pure cell succeed. A required historical child keeps its exact edge; a real selected conflict refuses.
- A -> divergent B -> A with lazy original package instances forced **after** the switch; private home file hashes/finder choices remain distinct. Same module spelling/different layout must not be combined by Name/equality alone.
- Fork, shadow, old retained closure/type/display context, dropped sibling, source-root/gitlink differences, package-to-home negative dependency and recovered saved root.
- Cancel before registration, during hydration, after Ready and during source; fail END; kill/rotate worker between transactions; evict an inactive realization. No partial Ready, no stale-source execution, no accepted-uncertainty replay or lost input leases.
- Malformed manifest/row/ref, inaccessible versus missing FD/path, wrong worker/name/package owner, wrong root, stale current generation, unsupported HUG/OneShot/Backpack profile and budget refusal all keep typed causes.

Compile changed Rust/Haskell owners and their direct test consumers through the pinned admitted setup. Use `just test-native //bridge/haskell:source_boot_product_reuse_test --pattern PATTERN` for maintained native controls after discovering exact names; `just test-lib tidepool-toolchain --exact ... --expected-count 1` for selected-root controls from the handoff; `just test-lib tidepool-extract-cmd --exact ... --expected-count N` for protocol/END controls. Add fixtures to owning Cabal/native input mappings. Discover counts before choosing N; linking/listing is not an executed pass. The parent qualification tranche owns the joined packaged actor history and bundle receipts.

## Structural/performance evidence and deletion

Instrument source-free full acquire/hit/miss/hydration, selected nodes/edges, unique decoded/backing bytes, requested/received part bytes and cache holds. On a matching Resident hit require **zero baseline row/graph serialization, zero baseline hydration and zero baseline part/FD enumeration or export**. Audit `ExactCompilationRequest::input_transport_files` and its `artifacts.rs` callers before encoding/sending: constructing a full FD/lease vector and then discarding it is still O(scope) work. The transaction holds the already acquired realization; only a cold Owned acquisition or checked delta enumerates the selected transferred parts. If a receiving check truly requires fresh baseline physical bytes, classify that request as acquisition and charge it, rather than calling it a constant-input hit. Independently intercept the production encoder, FD/lease traversal and wire paths; a memo hit counter beside a full baseline manifest is a failed acceptance. Report ordinary generated import count and initialHomes fold separately: they are not removed by this wave.

Retain timing ancestry, wall/current-thread or process CPU where meaningful, allocation/GC and process-tree residency. Registration cost and cold reconstruction count against amortized workload cost. Compare fixed semantic histories under the same producer/profile/package/input owner; report failures separately.

Delete superseded per-request full-baseline serialization/assembly routes once all their consumers enter the new typed owner. Do not leave a new hash-keyed semantic cache alongside the old root issuer. Retain explicit cold reconstruction for process loss and one independent recomputation oracle. Remove temporary protocol compatibility branches after matched deployment migration; cold fallback must not hide corrupt or inaccessible inputs.

## Integration/review gate

An independent exact-candidate review reads the protocol, root selection, lazy owner and production metadata/cell consumers together. Fold only after affected test targets compile, nonzero controls execute, selected root/edge expectations pass and hit wire evidence proves the baseline is absent. Run the small joined startup/cell/lookup/later-cell history immediately on that fold. Record unresolved package-owner or GHC eligibility boundaries as unavailable capabilities. Wave 2 may then consume the reference; wave 3 remains a separate experiment.
