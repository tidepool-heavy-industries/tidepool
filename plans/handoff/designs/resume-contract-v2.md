#### Typed-resume contract (amended, for review before implementation)

Replaces "Typed-resume contract (proposed…)" and its appended review list.
Every source fact below was re-checked on `engine/stg-production-cutover`.

Source facts:
- **Site evidence is dropped, and it is the wrong evidence.**
  `PreparedSites.buildYieldSite` records `SiteType { rendered, modules, heads }`.
  `TypePolicy.nominalHeadsOfType` returns `sort . nub` of every TyCon in the
  type, so `Either Int Text` and `Either Text Int` give the same evidence.
  `ExecutionEncode` has no site table, and `SCHEMA_VERSION = 8` on both sides
  (`ExecutionSchema.hs:24`, `execution_schema.rs:9`). Only
  `test-prepared-stg/Main.hs` reads `pmYieldSites`.
- **A site's recorded answer is often not what the continuation receives.**
  `EffectSchema.sitedVerbs` records the answer as the verb's first type
  argument, or one it selects. The actual inputs are:
  - `runLLMTurnFork` resumes with `Either InvocationExit T`, and
    `runLLMTurnFanout` with `[Either InvocationExit T]`
    (`tidepool-harness/src/engine.rs` `build_child_answer_value`, `ForkSource`).
    `fork` resumes with bare `T`. `forkAll`, `forkMap` and `forkCata` resume
    with `[T]`.
  - `request*` resumes with the result of `submitRequest`. The `result`
    travels as `ResponseResult result` in an `ExitCell` inside `Response`,
    filled by `fillResponse` (`Agent/Reply/Internal.hs:77,272-281`,
    `Internal/ExitCell.hs`).
  - `childSited = BranchU` (`Actors/Unfold.hs:475`) is a constructor, not a
    `send`.
  - `receive` parks with its handler as a live payload
    (`resident_workbench.rs` `capture_receiver_boundary`). It resumes with
    `next`, a value that compiled code produced.
  - `finalize` is never resumed. The harness reads the value out of the
    request and terminates the node (`harness.rs:2595-2612`).
  - The harness answers `runLLMTurn` in context with `resume expr :: T`, which
    delivers a rooted handle (`resident.rs` `resume_handle*`). The MCP resume
    tool bridges JSON through the `DataConTable` to `ResumeInput::Answer`
    (`session/engine.rs:1002`).
- **Freer shape** (freer-simple 1.2.1.2 from the flake):
  `data Eff effs a = Val a | forall b. E (Union effs b) (Arrs effs b a)`,
  `Union :: {-# UNPACK #-} !Word -> t a -> Union r a` (the payload is lazy),
  `Arrs = FTCQueue (Eff effs)` built from `Leaf`/`Node`, and
  `qApp :: Arrs effs b w -> b -> Eff effs w`. `tidepool_repr::freer_names`
  already names `Val`, `E`, `Union`, `Leaf` and `Node`.
  `prepared_resident_composite.rs` drives the loop with three tops per type
  (`askArgument`, `valResult`, `resumeInt`). They exist only because the
  payload and the `Val` field reach `inspect_outer` as thunks
  (`FreerResume.hs:12-25,47-70`).
- **Constructors are interned only when used.**
  `ExecutionProjection.internConstructor` runs from `Construct`,
  `StgRhsCon` and `DataAlt` patterns, so a constructor that is never built or
  matched has no `ConstructorDecl`. `DescriptorInterner` is keyed by
  `SymbolIdentity` and has no `host_id` index (`interner.rs`). The
  per-artifact `host_id` uniqueness check lives in `validation.rs:1294`.
  Algebraic dispatch compares header words with the compiling program's
  descriptor addresses. A mismatch traps with an integrity failure and latches
  the machine (`machine.rs` S3/X1 doc, around line 4095).
- **Construction parts exist, but no builder uses them.**
  `descriptor_bridge::marshal_descriptor_object` validates every field before
  it writes the header. `ProgramPlan` mints its own `bytes_array`
  `External(Bytes)` descriptor for each program (`plan.rs:442`). External
  payloads come from `MachineState::allocate_external_storage` and are freed
  by `release_external_storage` (`machine_state.rs:1962,2462`). A prepared
  collection with a reserve is `prepared_gc_trigger(vmctx, reserve)`
  (`invocation.rs:326`). Generated references carry the descriptor tag
  (`byte_arrays.rs` `bor_imm(object, descriptor.tag())`).
- **The one-shot ledger exists, but only for Core.** `ResourceLedger`
  provides `park`, `continuation` (peek), `take_continuation` and
  never-reused `ContinuationId`s. Its `ContinuationFrame` holds a
  `table: Arc<DataConTable>` (`resource_ledger.rs:21-31,141-160`). The
  `PreparedMachine` embeds the same ledger, but its continuation map is always
  empty (`machine.rs:127-132`). Core resume peeks, NF-validates, then takes,
  so a rejected answer stays parked (`jit_machine.rs:2576-2644`).
  `PreparedHandle` is `Copy`. A thunk consumed under `SingleEntry` keeps
  `Evaluating` (`freer_boundary_tests.rs:20-26`), so entering `k` twice is
  unsound, not merely a policy violation.
- **Handle facts.** `inspect_outer` filters handles by realm
  (`machine.rs:831`). `handle_is_evaluated` answers only whether a handle is
  WHNF (`machine.rs:1179`). Core validates only bridged `Value`s for bottom
  and never validates handles (`jit_machine.rs:2604-2608`).
- **The Core contract to preserve**: `ResumeInput::{Answer, Handle,
  FramedHandle, Abort}`, rejection before consumption, and the workbench's
  resume by handle and framed custody.

Decisions:

1. **Delivery mode per site.** `VerbSpec` gains `vsDelivery`, and each
   `YieldSite` records it with the exact wire type the continuation receives:
   - `HostAnswer wire`. For `runLLMTurn` the wire type is `T`; for
     `runLLMTurnFork` it is `Either InvocationExit T`; for `runLLMTurnFanout`
     it is `[Either InvocationExit T]`; for `fork` it is `T`; for `forkAll`,
     `forkMap` and `forkCata` it is `[T]`. The site accepts `Answer`,
     `Handle` or `FramedHandle`, each validated against `wire`.
   - `LiveReentry next` (`receive`, `serve`) accepts `Handle` only.
   - `ExitCellFill cell` (`request*`, `child*`). The resume at this site
     carries no host-built `result`. `ResponseResult result` is recorded so
     the target's compile view and the waiter can check it; the host never
     constructs it.
   - `TerminalCapture value` (`finalize`) is never resumed.
   The `wire` type is derived in the extractor from the verb's `ret` type,
   not rebuilt in Rust. The harness's name lookup
   `get_resilient(table, "Right", 1)` is replaced by the recorded wire
   evidence.
2. **Structural type evidence in the artifact.** Schema version 9 adds:
   - `types: [TypeNode]`. A node is either a constructor application
     `Data { family: SymbolIdentity, constructors: [CtorRow] }`, with
     `CtorRow { constructor: ConstructorId, fields: [TypeNodeId] }`, or a
     leaf: `Text`, `Integer`, `Natural`, or
     `HostUnconstructible { reason, rendered }`.
   - Nodes are hash-consed instantiated applications, so recursive types such
     as lists form cycles of node ids. Each site's root is a `TypeNodeId`.
   - Normalization runs in `Tidepool.TypePolicy`, next to
     `stabilizeEffectRows`: expand synonyms (`coreView`), normalize families,
     and erase newtypes to their representation type while keeping the
     newtype name only as presentation.
   - The following are marked `HostUnconstructible`: functions, `Eff`,
     existential or constraint-carrying constructors, unlifted fields other
     than the `Text`/`Integer`/`Natural` leaves, `ExitCell`, `Map`, `Set`,
     and anything left unnormalized.
   - Owners: producer `TypePolicy` plus `PreparedSites`; wire
     `ExecutionSchema.hs` and `ExecutionEncode.hs`; decoder and validation in
     `tidepool-repr::execution_schema::{codec, validation}`. Validation checks
     that node ids and constructor ids resolve and that each row's field count
     and representations match its `ConstructorDecl`.
   - Rendered type, modules and nominal heads stay for source rendering only.
     Fixtures regenerate through `just fixtures-update`. The Core `asks.json`
     sidecar is unchanged.
3. **Constructor closure and the `host_id` index.** For every site root
   reachable from a `HostAnswer` or `LiveReentry` node, the extractor calls
   `internConstructor` on every `tyConDataCons` of every `Data` node, so the
   artifact declares `Left` even when no code matches it.
   `DescriptorInterner` gains `by_host: BTreeMap<DataConId, SymbolIdentity>`.
   `intern` and `absorb` refuse a `host_id` that already maps to another
   identity, and absorb stays all-or-nothing. The consuming program compiles
   against the interner (`compile_for_install`), so its dispatch addresses
   include every constructor the builder can produce.
4. **One resume entry and one settlement entry, in shared freer modules.**
   - `Tidepool.Internal.Resume` (deployed stdlib) is the only module that
     imports `Control.Monad.Freer.Internal (Eff(..), Arrs, qApp)` and
     `Data.OpenUnion.Internal (Union(..))` for engine use. It exports
     `resumeLifted :: Arrs effs b a -> b -> Eff effs a`
     (`resumeLifted = qApp`, `NOINLINE`), which replaces `resumeInt`.
   - The effect-surface generator (`tidepool-mcp`, next to the `*Sited`
     helpers) emits one `settleEff :: Eff Row a -> Eff Row a` per compiled
     row. On `E (Union tag p) q` it dispatches on `tag` and fully forces
     every request field except fields whose binding is
     `RustBinding::CoreValue`. Those are forced only to WHNF and kept live
     under the existing `LivePayloadPolicy`. It returns the value unchanged.
     `Val` is returned untouched, and the completion policy forces it.
   - The prepared turn template (`session::turn`) references both entries, so
     both are admitted tops of every turn artifact. No Rust code walks freer
     data or decodes a request.
5. **Settlement is one routine.** Initial and resumed runs both call
   `settleEff` and then `inspect_outer` the result.
   - On `Val`: apply the turn's completion policy, as with `ParkKind`.
   - On `E`: observe the `Union` payload through the existing `observe`
     path. Read the site id from the observed request, and look up the site
     row in the artifact of the program that produced it. Root `q` and any
     live payload, then park.
6. **Continuation ledger with peek → validate → build → take → enter.**
   - Prepared parking uses the machine's existing `ResourceLedger::park`.
     `ContinuationFrame.table` becomes
     `evidence: FrameEvidence::{Core(Arc<DataConTable>),
     Prepared { program, site }}`. `cell` is the persistent root of `q`.
   - `resume(id, input)`:
     1. Peek the frame and check its realm.
     2. Validate the input against the site's delivery mode and type node.
     3. Build (decision 7) into rooted temporary storage.
     4. `take_continuation`, then deregister `q`'s root.
     5. Enter `resumeLifted q x` through `run_entry_retained`.
     6. Settle.
   - Every failure before step 4 leaves the frame parked and rooted. That is
     what "still resumable" means.
   - A failure after step 4 is a run failure, as in Core. `Abort` takes the
     frame without entering it.
   - No `PreparedHandle` is ever the continuation token.
7. **Host construction** (`prepared_program/answer.rs`, built on
   `marshal_descriptor_object`). A bridge `Value` is an owned tree, so no
   sharing memo is needed.
   1. **Size:** walk the value iteratively against the type node. Each
      constructor resolves through `by_host` and must belong to the node's
      `CtorRow` set. Sum `descriptor.allocation_extent()` and the external
      byte lengths. Reject bottom, which a `Value` cannot hold, and every
      `HostUnconstructible` node.
   2. **Reserve:** call `prepared_gc_trigger` once for the total, before any
      allocation. The frame and handle roots are already registered, so the
      collection moves them safely.
   3. **Bytes:** allocate every external payload through
      `allocate_external_storage` and record each one in a builder-owned
      rollback list. `External(Bytes)` wrappers use one machine-wide bytes
      descriptor, hoisted from `ProgramPlan.bytes_array` into the interner's
      custody.
      - `Text` uses the `Data.Text.Internal.Text` constructor's own declared
        reps (text-2: bytes, offset, length). The payload is UTF-8 with offset
        0 and length equal to the byte length.
      - `Integer` is built as `IS` when it fits in 64 bits. Otherwise it is
        `IP` or `IN` over normalized little-endian limbs with no zero high
        limb; `Natural` likewise uses `NS`/`NB`. Take the constructor
        identities from the `ghc-internal` declarations in the closure.
   4. **Build:** allocate bottom-up in the reserved nursery span. The build
      makes no allocating calls, so intermediates need no roots. Tag
      references as generated code does.
   5. **Publish:** register the root as a handle in the frame's realm.
   6. **On any failure:** reset the bump cursor to its pre-build value and
      release the external payloads in reverse order. Nothing becomes
      reachable.
8. **Handle policy.** `Handle` and `FramedHandle` inputs require:
   - the handle is known and belongs to the frame's realm;
   - the target is not `Evaluating` (`resolves_to_whnf_value` grows into a
     three-state WHNF/thunk/evaluating answer);
   - `FramedHandle` values used in strict fields are WHNF;
   - the handle's provenance evidence (the `TypeNodeId` it was retained at,
     carried with `RootedValueRef` provenance) is structurally equal to the
     delivery node. Absent evidence is a refusal.
   Handles are not bottom-checked, matching Core. `Map` and `Set` answers
   arrive only as handles produced by compiled code, for example
   `resume (Map.fromList …)`.

Acceptance:
- Scalar, record, `Maybe`, `Either A B` with A ≠ B, list, `Text`, `Integer`
  and handle answers all resume through `resumeLifted`.
- Every `sitedVerbs` row has a delivery mode, and every `HostAnswer` wire type
  passes a round-trip test.
- These are rejected with the frame still parked, the handle and root counts
  unchanged, and the machine latch clear:
  - a wrong-family answer;
  - swapped `Either` arguments;
  - a constructor that is not in the site's closure;
  - a host answer to a `LiveReentry` site;
  - a handle from another realm, or one that is `Evaluating`;
  - an injected byte-allocation failure midway through a build.
- A second resume of a consumed id is `UnknownContinuation`.

First slice: `runLLMTurn @Bool`, answered by the host.
- **Program:** a prepared notebook turn `b <- runLLMTurn @Bool "q"; pure (not b)`
  parks, the session resume API supplies JSON `true`, and the turn completes
  with `false`.
- **What it exercises:**
  - a schema-9 site row with `HostAnswer Bool`;
  - closure emission, so `False` and `True` are both declared although only
    one is matched;
  - the `by_host` index;
  - `settleEff` over the real `RunLLMTurnWith` request, whose site sits inside
    its JSON payload and whose `Text` and `Value` fields are forced;
  - the ledger's peek → validate → build → take → enter path;
  - a nullary-constructor build;
  - rejection of `"yes"` and of JSON `1` while the frame stays resumable.
- **Removes:** `resumeInt`, `askArgument` and `valResult` from the resume path
  (the fixture keeps them as oracles).
- **Defers:**
  - `Text`/`Integer` byte construction (slice 2: `runLLMTurn @Text`);
  - `Either InvocationExit T` and fanout lists (slice 3);
  - handle and framed answers, and harness `resume expr` routing (slice 4);
  - `LiveReentry`, `ExitCellFill` and `TerminalCapture` wiring;
  - `Map`/`Set`;
  - receipts and committed-prefix recovery;
  - stale-incarnation admission;
  - retirement of parked frames (lifetime contract).
