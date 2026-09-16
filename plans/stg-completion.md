# Finish the STG engine

Implementation status: the [current execution map](handoff/continuation-2026-09-15.md)
records recovered work, active assignments and completion gates. F1/F2 connect
pure prepared notebook turns, pattern binds and cell display through the resident
session. Schema-10 site evidence is in. Production effect suspension now parks
and resumes on both engines for Bool, data/Maybe and byte-backed
Text/Integer/Natural host answers (F4/F5); handle/framed answers, Either/list
wires and ordinary-effect reply sites remain. Bounded residency's machine side
is landed for per-program root blocks, stable ids, the quiescence gate,
liveness mark and retirement (F6 slices 1a/1b); the runtime's receipt drain,
between-turn collection call, and descriptor-arena compaction (slice 1c)
remain. The duplicate `PreparedRuntime` is deleted; `ResidentSession` is the
one mounted engine target, with a known gap where its `close_realm`/
`parked_realm` do not yet forward to the Prepared engine. Step 2 integration
(general typed resume through real owners), step 3 default-routing
eligibility, parity and Core removal remain unfinished. See the execution
map's critical-path table for the current gate-by-gate status. Earlier
evidence is retained in the
[2026-09-15 implementation record](handoff/implementation-2026-09-15.md).

Review baseline: `344ecd59a` (2026-09-14). This is a proposed execution plan,
not an implementation or a fresh gate result. It supersedes the routing
discussion in `stg-wave6.md` as a recommendation; it does not claim that a
production routing change has been made.

Updated 2026-09-15 with the pre-handoff implementer's answers. Their reported
plugin recompilation regression still needs an independent pinned-GHC check;
the required investigation and fix are part of step 1 below.

The accepted destination remains [STG production cutover](stg-production-cutover.md):
one prepared-STG engine, existing Shoal notebook/effect/resident behavior,
Linux x86_64 first, native GHC as the pure-language oracle, and removal of
obsolete execution paths. Other targets reject explicitly. Comparative
performance is not a release gate; emitted fast-path behavior and bounded
resource ownership are contracts. Do not revive the superseded interpreter,
cross-platform, or comparative-baseline requirements of the original design.

## Review conclusions that change the work

The entered-callee refactor and program-declared import leasing are good
structural changes: they move correctness into owners instead of adding
caller checks. Keep that approach for the remaining boundaries.

1. **G1 needs semantic signature identity, not hash equality as proof.**
   `prepared_resolve_call` currently compares only a fingerprint. The proposed
   owner-local collision check is insufficient: caller B can demand a signature
   that collides with A's sole registered signature, without A having any
   internal collision. Use full-signature equality in the resolution owner.
   An interned machine-local signature token is suitable for generated code
   if its construction compares full signatures; hashing is an index only.
   Preserve logical `Void` positions and `NoSuccess`, even when physical
   argument registers happen to agree.
2. **Speculative lookup must not record execution failure.**
   `prepared_program.rs::prepared_resolve_call` calls `set_first_cause` on a
   miss today. Reusing it for the proposed full-demand/prefix search would
   record failure before a later prefix succeeds.
   Native success paths consult that recorded status, so this would turn a
   successfully resolved call into an execution failure, not merely misreport it.
   Separate lookup from terminal failure reporting. A valid callee with no
   matching shape is a clean probe miss; exhausted resolution reports the typed
   reusable error once. Invalid descriptor evidence retains the
   integrity-failure contract.
3. **The proposed demand closure is incomplete.**
   Lifted-result slices cover partial applications. Exact PAP completion also
   needs `arguments[p..] -> original results`, including scalar and multiple
   results. `apply::classify` additionally admits saturation of `NoSuccess`
   functions under other demand result contracts and with excess arguments.
   A lifted-result-only prefix search does not establish that behavior.
   Generate offers from the existing classifier's semantics and explicitly
   handle terminal saturation. Do not let cross-program partitioning change
   an otherwise supported call into `UnresolvedCallee`.
4. **An actor-shaped fixture is not the production actor boundary.**
   `placement_retirement.rs` checks out a `SingleSlot` and calls
   `retire_placement` directly. The production `ActorMachineRegistry` still
   fixes its machine type to `ResidentSession<H, O>`. The test is useful
   retirement evidence, but does not prove prepared execution through that
   registry or exact-incarnation request admission. Connect that owner and
   test its real request/retirement path; private methods can be tested in
   their owning module without widening public APIs.
5. **Releasing a lease does not establish session reclamation.**
   Prepared installs append programs, descriptors and static owners, register
   persistent top/import roots, and consume a fixed top table. The runtime
   reserves 4096 slots. The prepared machine does not run a major collection
   pass. Realm cleanup can therefore pass while repeated turns retain data
   and eventually exhaust slots. Close this lifecycle before default routing;
   raising the slot limit is not the structural fix.
   Wave 6's no-uninstall decision was limited to that wave. Its cited Core
   fragment-count rotation is not connected to prepared execution and cannot
   preserve arbitrary live closures. Neither rotation nor the slot limit is
   an accepted lifetime design for the completed engine. The existing heap
   and Core JIT lifetime plans do not supply prepared-program reclamation.
6. **Compiler reuse needs a correctness contract before measurement.**
   The withholding plugin is always installed and overrides only
   `installCoreToDos` on `defaultPlugin`. The implementer reports that the
   pinned GHC default forces home-module recompilation, defeating reuse.
   `GutsMemo` retained-set checks alone do not establish interface validity.
   Separately, daemon memo sanitation removes every `Tidepool.Session.*`
   module, including the prepared turn modules since G2; their lost memo reuse
   is correct under the current policy, but its cost is unmeasured. Fix plugin
   recompilation semantics first, then measure the separate sanitation cost.

## Execution sequence

### 1. Close cross-program application and Wave 6

Owners: `prepared_program::{plan,apply,resolve,machine}`, `MachineState`, and
`Tidepool.{RetainedUnfoldings,GhcPipeline}` for compiler validity.

- First verify how pinned GHC 9.12.2 includes static plugins in recompilation
  decisions and interface fingerprints. Give the withholding plugin a
  recompilation fingerprint derived deterministically from the full retained
  identity set read from the same request cell as the pass. Include the pass's
  semantic version in the validity contract. Do not replace forced
  recompilation with an unconditional reuse declaration.
- If GHC does not consume that fingerprint for static plugins, connect the
  retained-set dependency to its actual interface invalidation path in the
  existing pipeline owner before enabling reuse. Do not assume that changing
  `pluginRecompile` or validating `GutsMemo` covers the interface cache.
- Test unchanged source with retained sets empty/A/A/B/A/empty through the
  real resident compiler, including warm home-module interfaces and memo
  entries. Establish same-set reuse and changed-set semantic equivalence to
  fresh compilation. Use named retained imports/unfoldings as observations,
  not timing alone; cover one-shot and daemon outputs as appropriate.

- Keep application with the callee's owner: it already knows PAP fields,
  descriptors, rooting and flattening. Do not introduce host argument buffers
  or a second runtime PAP representation.
- Establish full-signature resolution and non-failing probes first. Share
  semantic call classification between local emission and foreign offers.
- Emit the complete finite set of exact remaining signatures and partial
  demands for functions/PAPs, plus an explicit terminal-saturation route.
  Close caller demand sets over required excess suffixes. Every successful
  excess step consumes logical arguments; propagate failure before applying
  a suffix and preserve live roots across both calls.
- Start with complete owner dispatchers. Measure declaration count, emitted
  bytes and compilation time on a real prepared artifact. If header chains
  dominate, specialize owner adapters for known headers while reusing the
  same application emitter. Do not trade valid-call coverage for code size
  or add escape analysis merely to justify incomplete coverage.
- Cover foreign exact PAP completion to lifted, scalar and multiple results;
  partial-then-complete and partial-an-existing-PAP; over-application through
  another owner's returned closure; logical `Void`; `NoSuccess`; mismatched
  signatures; and an imported thunk returning a captured closure.
- Force collection during foreign execution with caller roots live, including
  the intermediate excess result. Verify a successful prefix probe leaves
  no recorded failure. Verify invalid calls leave a sound machine reusable.

Exit: plugin recompilation and interface-cache validity under retained-set
changes are demonstrated, the application cases pass, fixtures remain consistent,
and the relevant Wave 6 integration gate runs on one recorded revision. Classify known actor
timeouts separately; earlier passes and a baseline reproduction do not make
the current broad gate green.

Step 1 evidence at `413f8cea4` (G1 committed; recorded 2026-09-15):

- Focused checks on the committed tree: codegen library 486 passed, 1 skipped
  (the prior 484, minus three deleted fingerprint unit tests, plus five
  foreign-application tests); runtime library plus `prepared_execution` and
  `prepared_resident_composite` 188 passed; `placement_retirement` 1 passed;
  the ignored `prepared_turn` 1 passed with the extractor resolved; Haskell
  `execution-schema-projection` and `prepared-stg-pipeline-test` passed;
  canonical `just fixtures-check` passed.
- The peer's broad `just changed HEAD` run was stopped at 230/2966 with 28
  failures, all in `tidepool` actor-host tests; that crate does not reference
  the prepared engine. Triage reproduced a representative sample at the
  pre-session commit `067adcf18` (with its own extractor) and again on the G1
  tree, both run quietly:
  - `actor_host::tests::actor_workspace_recipes_distinguish_orchestrators_from_coding_workers`
    and `overlay_resource::tests::lost_descendant_custody_does_not_authorize_parent_reclamation`
    fail identically at baseline: pre-existing assertion failures.
  - `research_policy_tests::research_admission_obeys_configured_width_and_consumes_depth`
    fails with the same assertion (`research_policy_tests.rs:110`) at baseline
    and on the G1 tree: pre-existing.
  - `custody_tests::custody_install_failure_prevents_provider_publication`
    passes at baseline and on the G1 tree (about 117 s each); its failure in
    the broad run was a timeout under load.
  - Seven `command_jobs_tests` match the classification in `067adcf18`.
  - Quiet rerun on the G1 tree (two concurrent tests): three more custody
    tests pass (`custody_actor_cancellation_after_binding_prevents_provider_publication`,
    `custody_actor_cancellation_during_delayed_install_releases_exact_binding`,
    `custody_haskell_bootstrap_failure_after_install_releases_binding`), so
    their broad-run failures were load timeouts.
  - Six fail quietly on the G1 tree and identically, same panic sites, at
    `067adcf18`: pre-existing. `hosted_tools_tests::frozen_tools_dispatch_raw_and_structured_inputs_without_workbench_bindings`
    (`hosted_tools_tests.rs:26`), `tests::active_update_keeps_original_request_and_fences_terminal_delivery`
    (`actor_host.rs:5391`), `tests::forest_operator_survives_model_root_recovery`
    (`actor_host.rs:5160`), `tests::haskell_actor_sends_normal_steering_without_a_native_session`
    (`actor_host.rs:7606`), `tests::notification_admission_and_poll_preserve_typed_request_bindings`
    (`actor_host.rs:7056`), and `custody_tests::custody_precedes_first_bootstrap_worktree_use_for_two_siblings`
    (`custody_tests.rs:583`). Four of them are notebook cells rejected by GHC
    type errors against current stdlib signatures.
  Failures the broad run reported after test 116 of 2,966 were not captured
  by name; the run's artifact directory was removed by
  `scripts/lib-extract.sh`'s exit cleanup when the run was stopped.
- Prepared corpus on the G1 tree (run by `just fixtures-check` through
  `scripts/prepared-corpus.sh`): every contract cohort passed all six stages;
  suite projection, validation, admission and compilation 812/812; execution
  628 passed, 184 failed; comparison 216 passed, 0 failed, 595 missing
  expectations. Execution and comparison equal the script's floors exactly,
  so nothing regressed and there is no headroom.
- A pre-existing clippy error (`clone` on the `Copy` type `RuntimeRep`,
  `tidepool-toolchain/src/prepared_artifact.rs:100`) stops that crate's test
  build under the broad gate.

- Dispatcher cost, one debug-build observation per artifact (the ignored
  `foreign_dispatch_cost_on_freer_artifact` with `TIDEPOOL_COST_ARTIFACT`; the
  pre-G1 column is a throwaway probe of `CompiledProgram::compile` at `344ecd59a`):

  | Artifact | Pre-G1 compile | G1 compile | G1 dispatchers | G1 dispatcher bytes | G1 dispatcher emission | Exports pre-G1 / offers G1 |
  |---|---|---|---|---|---|---|
  | freer-resume fixture | 434 ms | 486 ms | 96 | 509 KB | 172 ms | 158 / 2,968 |
  | text contract `3.prepared.cbor` (306 KB) | 533 ms | 623 ms | 124 | 699 KB | 227 ms | 167 / 3,983 |
  | suite `502.prepared.cbor` (297 KB) | 762 ms | 922 ms | 236 | 1.35 MB | 441 ms | 245 / 6,522 |

  G1 adds 12-21% compile time; dispatcher emission is 35-48% of the total,
  so header chains do not dominate compilation. Emitted dispatcher code is
  large relative to the artifact and has no pre-G1 byte baseline. Owner
  adapters specialized per known header are not a prerequisite for any step.
  Measure representative session growth first (code bytes and compile time per
  turn across a long notebook session); specialize only if that shows
  resource pressure.

- Lane V follow-up checks on the G1 tree: full codegen suite 849 passed, 3
  skipped; `tidepool-testing` 32 passed; two new G1 gap tests pass (an owned
  non-callable callee is a reusable `UnresolvedCallee`; cancellation at every
  function-entry, allocation and thunk-entry poll of a cross-program
  over-application is reusable and retries). A header no installed program
  owns cannot reach a dispatcher through the host API, so that branch is
  documented rather than tested.
- `just suite tidepool-runtime` stops at its first shard: 17 of 34
  `properties` tests (`proptest_cache_layer`) fail with
  `MissingOutput(<key>.prepared.cbor)`. They fail identically at `067adcf18`:
  the cache has required a prepared artifact since `b217095e5` while the
  property harness's fake extractor still writes only Core output.
  Pre-existing; the remaining 16 runtime shards have not run.
- S5 was incomplete, found by a new daemon retained-set transition check:
  with `producerValue`/`producerFn` retained, the consumer's projection still
  recovered the floated `producerValue1..5` tops, in-process and through the
  binary. `selectPreparedTarget` walked retained bodies for reachability
  while recovery skipped them; both now use `skippedFromRecovery`, and the
  Haskell test requires that no producer top is recovered.

- Broad gate at `ff73dfde0` (`just verify`, independent steps, nothing else
  running): lint, suite registration and fixtures passed, and every prepared
  corpus stage held its floor. Default tier: 2970 run, 2913 passed, 55 failed,
  2 timed out, 69 skipped (list: the gate log's `FAIL`/`TIMEOUT` lines).
  Triage by shared cause, not by test:
  - Stale contract outputs, fixed and verified: a hand-edited generated
    decoder (`818eb8b5d`) and stale schema pins (`373f19abc`).
  - Core engine integrity failure (the raw `bash` actor tests): compiled
    `String#`/`ByteArray#` literals pointed at module data the external-storage
    ledger never registered, so a tenure's fixup collection reported
    `Untracked` and latched `BadPointer`. Not Unicode: the ASCII variant
    failed identically. Fix: literal payloads are ledger-owned byte arrays;
    the new codegen regression test failed before and passes after, and both
    raw `bash` tests pass.
  - Prepared projection rejected text's `_hs_text_measure_off` (five MCP
    print/capture tests); an unused polymorphic typed-site helper rejected
    whole modules (two facade tests); `Tidepool.Actor.Record` exports a
    signature naming the unexported `Message` (two command-job tests).
  - Loaded-gate hang, not a defect: `stack_safety::test_deep_add_small_stack`
    passes in the full codegen suite (856 passed) after `76718e6c7`.
  - Remaining failures, grouped by panic text (read-only triage):
    likely lifecycle defects after `drainActor`/`awaitExit` (machine
    `BadPointer` or a zeroed closure in three actor-source/drain tests, an
    owned child's exit not visible after `replaceActor`, and the hang in
    `stateful_handler_failure_after_effect_pauses_without_replay_or_closing_mailbox`);
    stale Haskell fixtures after `request` took an `Assignment` (four tests)
    and `sendMessage` took an `AgentRef` (one); stale contracts for designed
    behavior changes (seven tests, including `failed_command_display_*` and
    `command_presentation_*`, which are not engine bugs); load timeouts
    hiding two real assertions (`custody_precedes_first_bootstrap_worktree_use_for_two_siblings`,
    `research_admission_obeys_configured_width_and_consumes_depth`); an
    ungated live-PTY test. The two command-job timeouts lean load; one needs
    a solo timed rerun.
- STG defects found while triaging, not yet fixed:
  - Typed-site elaboration: both elaborators place the site literal by
    counting value arguments from the right, so an eta-reduced
    `Member`-constrained verb gets the literal in its dictionary slot (untyped
    garbage on the Core path, ill-typed Core on the prepared path), and the
    prepared elaborator does not strip GHC's `nospec` wrapper. Recognized
    verbs should be rewritten or rejected, never passed through; one shared
    classifier should place the literal from the verb's type.
  - Projection has no result contract for a function whose result
    representation is chosen by its caller (a pattern-synonym matcher,
    `$mActorDefinition`, reached from `startActor`/`startActorFork`); it needs
    a caller-supplied return contract in the schema and machine, not a guessed
    representation.
  - `Address` values cannot reach the host (`observe.rs`, `inspect_outer`,
    `PreparedResult`), which is the one engine gap in the 184 corpus execution
    failures; the other 183 are harness-driven (function-typed tops run with no
    arguments, cyclic values, unobserved dictionaries). No corpus result
    disagreed with its oracle, but 595 executable programs have no expectation.
  - Cell pins: `Tidepool.Actor.Record` indexes protocols by
    `Schema api = Rep (api Shape)`, which renders as an unexported synonym or
    as the generic representation; exporting `Message` fixes only the
    constructor. Index by the record type (`Message api`).
  - Core engine heap corruption after `drainActor`/`awaitExit` survives
    `76718e6c7`: the two actor-source/drain tests still fail with case traps
    (one on a zeroed closure) and a `BadPointer` latch. Under investigation.

Proposed fixes for the defects above (designs from read-only investigation,
for review before implementation):
- Typed-site classifier: one pure classifier shared by both elaborators
  decides rewrite or reject for every recognized verb occurrence. Rewrite when
  all type arguments and dictionaries are present, placing the site literal at
  the index derived from the verb's type (foralls plus constraints), whatever
  the value count; reject (deferred to reachability) when a type argument or
  dictionary is missing or a type is open. Strip `nospec` first on both
  paths. `vsMisShapeIsError` goes away. Defence in depth: the `runLLMTurn*`
  and `finalize` stubs pass a bottom site instead of 0, and the runtime
  rejects a site with no recorded type.
- Caller-chosen result representation: `ResultContract::CallerResult`
  (schema version bump), allowed only for functions, joins, call demands and
  global entry signatures; validation rejects it in thunks, operations, case
  scrutinees, enters and the program entry, so only tail positions produce
  it. Projection emits it from one helper shared by functions, joins and
  imported entries. Codegen keeps one descriptor per function and emits one
  monomorphic native entry per result representation actually demanded
  (closed over the program's own demands plus a lifted instance); a
  cross-program demand at an uninstantiated representation is a typed
  reusable miss. First slice fixes `startActor`/`startActorFork`.
- `Address` host values: an observed address that resolves inside an
  installed program's pinned literal bytes observes as `LitString` of the
  literal's suffix from that offset (no NUL scan); a byte-array address, null,
  or anything unauthenticated is a typed `ObservationFailure::Address` refusal
  with its origin, never dereferenced. First slice is observation only
  (pinned-bytes literal length index, pool union in `ObservationHeap`, the
  `expand` arm) and clears the 74 corpus Address rows; a second slice adds
  machine-minted address tokens to `PreparedResult`/`PreparedInput` and stops
  accepting a raw scalar for an `Address` parameter.

This is not a green broad gate; the exit criterion above still requires one.

### 2. Connect one complete notebook turn through the production owners

Owners: `session::{workbench,turn,prepared,registry,supervisor}`, extractor
pipeline, and `tidepool-actor::{mount,resident_workbench,request}`.

Starting gaps, confirmed by the implementer: no prepared-path experiments
have exercised general typed answers, actor compile views or committed-prefix
recovery. Resume uses handwritten answer-type wrappers, with only the fixture's
Int wrapper implemented. The runtime has no effect handlers or receipt/recovery
implementation. Prepared turn modules omit notebook pragmas, preamble and actor
compile-view imports; they use implicit standard Prelude and plain GHC defaults.
That source is not dialect-neutral: defaulting, language extensions and visible
names differ from the notebook contract.

- Select prepared execution at session construction for a temporary explicit
  migration route. Never fall back to Core after a turn starts or fails.
  Preserve existing machines until their deliberate retirement.
- Reuse whole-cell preparation, `TurnKind`, templates and ordered cursors.
  Support expressions, declarations and effectful binds; preserve lookup,
  type/defaulting behavior, shadowing and exact `ActorCompileView` imports.
  Render the notebook's actual pragmas, preamble and exact imports through
  the shared template owner. Test dialect-sensitive expressions and shadowing
  through the production compile view against the pinned notebook oracle.
  Fold `SessionTurns`' useful projection work into these owners rather than
  growing its caller-supplied `TurnForm` into another notebook frontend.
- Carry typed effect requests and answers through the existing schema,
  interpreters and authority checks. Share initial/resumed completion.
  Preserve receipts, committed prefixes and recovery observations.
  First design the general answer-to-continuation contract: which existing
  typed-site evidence determines answer representation, who compiles any
  required adapter, and how managed answer roots and failed validation are
  owned. Derive adapters from authoritative types in the existing compiler/
  bridge owners; do not grow a handwritten wrapper per answer type. Cover
  representative scalar, structured and managed answers and all production
  verb answer types, including rejection before continuation consumption.
- Mount the prepared machine in the production actor registry, including its
  retirement contract. Remove ambient-scope placeholders: prepared bindings
  currently use `ScopeId::ROOT`, and stored actor policies are not interpreted.
- Use the production daemon and toolchain path. Check cold/warm compilation
  across source and compile-view changes after step 1 establishes retained-set
  validity. Measure home-module recompilation, memo reuse and request-module
  sanitation separately. Keep request-local state out of reusable memo entries;
  use the existing owner if evidence justifies a narrower reuse contract.

#### Typed-resume contract (proposed, for review before implementation)

Source facts:
- Site evidence is computed and then dropped. `Tidepool.PreparedSites`
  yields `YieldSite { site, origin, ordinal, answer, inputs, replyDeclaration }`,
  whose `SiteType` carries rendered type, defining modules and nominal heads,
  into `PreparedModule.pmYieldSites`. Nothing encodes it into
  `.prepared.cbor`; only the Core `asks.json` sidecar carries sites.
- Answers are typed Haskell values. `Replies` returns polymorphic payloads
  (`ObserveProgressWith :: Int -> Replies (ProgressState progress)`), so
  answer construction must follow the site's type evidence.
- The continuation's representation is uniform. `qApp :: Arrs effs a b -> a -> Eff effs b`
  receives a lifted `a` for every answer type. A single compiled resume entry
  therefore serves every site. Validation happens before that call and
  never relies on native signatures, since distinct types share
  representations.
- Host construction has its parts. The machine-wide `DescriptorInterner`
  maps a constructor's `SymbolIdentity` to its declaration and descriptor,
  and the declaration's `host_id` is the bridge `DataConId`.
  `descriptor_bridge::marshal_descriptor_object` validates every field
  before writing a header. No production path builds prepared objects from
  the host yet.
- The Core contract to preserve is `ResumeInput::{Answer, Handle, FramedHandle, Abort}`,
  bottom rejection before consumption, and the workbench's resume by handle
  and framed custody.

Decisions:
1. **Evidence in the artifact.** The prepared schema gains a site table:
   each site's answer and input nominal heads, plus each constructor's
   normalized qualified name from `Tidepool.Identity.qualifiedName`, so the
   module alias table stays in one place. The encoder, decoder and schema
   version change together, and fixtures regenerate through the canonical
   owner.
2. **One resume entry.** A library top applies `qApp` to the retained
   continuation and the answer. It replaces the per-type `resumeInt` wrapper.
3. **Host-built answers.** A prepared value builder resolves each bridge
   `Value` constructor by `host_id` through the interner. It checks the
   constructor's family against the site's nominal heads and builds
   iteratively for deep and list answers. On failure it publishes nothing.
   The same builder supplies the prepared `DataConTable` the workbench's
   encode and decode sites need.
4. **Live answers.** An existing handle or framed handle, closures included,
   passes as a managed argument after realm custody is checked. The
   continuation is not consumed until construction and custody both succeed.
5. **One settlement routine.** Initial and resumed completion share one
   routine: `Val` completes, `E` parks with its site and request.

Acceptance: scalar, record, `Maybe`, `Either`, list and managed answers; every
production verb answer type; a wrong-family answer and a bottom answer are both
rejected with the continuation still resumable.

Review against source (adversarial, read-only) found the one `qApp` entry
sound and validate-then-marshal the right primitive, but the evidence model
unsafe as written. Amendments required before implementation:
- A site's recorded type is often not the continuation's input:
  `runLLMTurnFork`/`Fanout` resume with `Either InvocationExit T` / lists,
  `request`/`child` deliver `result` through an exit cell, `receive`/`serve`
  resume with live values. Record a delivery mode per site (host answer with
  its exact wire type, live handle, exit-cell fill).
- Nominal heads are a sorted set: too weak (`Either Int Text` equals
  `Either Text Int`) and too strict (field-type families, erased newtypes,
  synonyms). Carry closed structural type evidence with a per-family
  constructor field table; validation is a type-directed walk.
- Constructors are interned only when used, so a `Left` never matched is
  undeclared and a late descriptor makes `Case` trap (machine latch). The
  extractor must emit the constructor closure of every host-answer type; the
  interner gains a `host_id` index and refuses divergent descriptors.
- Byte payloads (`Text`, `Integer`) need machine-wide bytes descriptors and a
  builder-owned ledger allocation with rollback; `Map`/`Set` go through a
  compiled adapter or are refused.
- Construction sizes the graph first (memoizing shared nodes), reserves once,
  and unwinds the ledger on failure; handles are checked for realm, rejected
  when `Evaluating`, and must be WHNF in strict fields.
- One-shot consumption needs a continuation ledger (peek, validate, build,
  take, enter), not a `Copy` handle; "still resumable" means rejected before
  entry.
- Settlement needs a compiled request-observation entry that forces the
  `Union` payload and site literal. The first slice is the
`FreerResume` fixture resumed through the single entry, then a structured
answer, then a handle answer.

Exit: a real notebook declares a function, retains and applies a PAP on a later
turn, performs an authorized effect, parks, allows sibling work, resumes, and
looks up its result. Repeat through cancellation, rejection after a committed
effect, stale-incarnation admission, and retirement with a surviving sibling.
Do not build captured native stacks: freer suspension by return already
provides the required serialized evaluator model.

### 3. Complete retirement and collection

Owners: prepared machine, existing root ledger/collector, external-storage
owner, and code lifetime owner. Design this before broadening session routing;
it can be implemented alongside step 2 after the lifetime contract is fixed.

- Define which roots keep values, programs and globals live. An escaped closure
  or PAP must retain its code/descriptors and required globals after its
  producer's actor or lexical binding retires.
- Make unreachable installed programs release their top/import roots and
  resolution entries, then reclaim code/descriptors only when safe. Treat
  mutually referring programs as a reachability problem, not naive reference
  counting. Code-to-global edges are part of the collector contract.
- Connect full old-space/external-payload reclamation. Choose stable slot
  storage/reuse compatible with generated top accesses; reuse a slot only
  after no live code or root can address its previous generation.
- Keep failure cleanup metadata-driven after an integrity failure; retain
  native owners until execution has unwound.

#### Lifetime contract (proposed, designed alongside the typed-resume contract)

Source facts:
- `PreparedMachine::install` bump-claims a contiguous slot range from
  `claimed_slots` and never returns it. Generated code addresses tops and
  imports through those slot immediates.
- Each install pushes its instantiated `StaticRegion` onto `statics` and
  its descriptors onto `descriptors`. It absorbs constructors into the
  machine-wide `DescriptorInterner` and extends `descriptor_registry` and
  the stack-map chain.
- `MachineState.prepared_callables` maps a header and signature to a code
  pointer, and `prepared_enters` maps a header to a code pointer. Both are
  machine-wide, record no owning program, and are cleared only when the
  machine drops.
- `ProgramCustody` keeps each compiled pipeline alive for the machine's
  whole life. `release` and `close_realm` deregister handle roots only.

Decisions:
1. **Ownership is recorded, not merged.** Every machine-wide entry an
   install creates carries its `ProgramId`: resolution entries, enter
   headers, descriptor registry rows, statics, stack-map links and slot
   ranges. Merged views are indexes over owned rows, so retiring a program
   removes exactly its rows.
2. **Roots come from outside.** External roots are bindings, handles,
   parked continuations and actor placements. From a root, tracing reaches
   heap objects. An object's descriptor header names its owning program,
   and that program's code references its imported globals. A program is
   live only if some live object or live frame reaches it.
3. **Globals are conditional edges.** A program's top and import slots stay
   roots only while the program is live. Unreachable program cycles are
   therefore collectable; reference counting would keep them alive.
4. **Reclamation at quiescence only.** With no native frame inside the
   machine, a major collection marks live programs, then retires the
   unreachable ones in this order: resolution entries, enter headers,
   descriptor rows and interner entries no live program shares, stack-map
   links, slot ranges, statics, and finally code.
5. **Slot reuse by generation.** A freed slot range returns to a free list,
   and a later install of the same width reuses it. Generated code for a
   retired program can no longer run, so the old addresses are unreachable.

Acceptance: escaped closures and cross-program PAPs still run after their
producer's binding and actor retire; repeated install, run, retire and collect
cycles beyond 4096 cumulative slots keep live residency bounded; counters for
handles, import and top roots, slots, old and external bytes, and installed
code owners are reported separately.

Review against source (adversarial, read-only) confirmed that generated code
reads only its own slots, import graphs cannot cycle (value graphs can), and a
between-installs quiescence notion exists. Holes requiring amendment:
- Liveness edges missed by "header names owning program": constructor
  descriptors are interned machine-wide with no owner; the collector never
  visits static-region or old-space objects (no prepared mark phase exists);
  raw `Address` fields point into a program's byte storage with no header edge
  (use-after-free); parked sites reference their compiling program; programs
  need explicit pins between install and first bind, and the first program
  permanently.
- `ProgramId` is a vector index; retirement needs a never-reused issuer.
- Shared index rows (`prepared_enters`, `descriptor_registry`, the collector's
  descriptor map) hold one value per header from the last writer, so "remove
  exactly its rows" breaks: rows need owner sets plus a live-header census.
- Slot reuse by same width fragments and races compile against install;
  prefer one fixed-address root block per program, freed with it.
- Reclamation order must start by deregistering slot roots and purging
  remembered slots; quiescence must be enforced (depth zero, no borrows,
  never allocation-triggered, machine reusable) and must release per-program
  leases through a runtime callback.
- Nothing is reclaimable until shadowed notebook bindings retire, since root
  bindings and handles pin every program.

Exit: repeated install/run/retire/full-collect cycles beyond the present slot
budget have bounded live residency. Escaped closures and cross-program PAPs
still run; unreachable old graphs and external cycles are reclaimed. Measure
handles, import/top roots, slots, old/external bytes and installed code owners
separately so a falling lease count cannot conceal retained programs.

### 4. Establish production parity, then make STG the default

Owners: the existing corpus harness and Shoal integration tests.

- Run the pinned-GHC oracle under the actual notebook dialect. Retain the four
  original semantic regressions and observe functions/lazy values through
  application and finite demand contexts.
- Run the declared production corpus through projection, validation, compile,
  execution and comparison with explicit denominators. Wave 5's reported
  216/217 expected values is useful prior evidence, not whole-product parity.
- Exercise the real source-to-effect-to-resumed-result route and failure
  receipts from step 2. Include long-lived sessions from step 3.
- Compile supported production targets and run the retained broad gates.
  Resolve release-blocking failures, including baseline failures that obstruct
  the acceptance path; report unrelated exclusions explicitly.
- Make prepared execution the default for fresh sessions. Drain or explicitly
  terminate old sessions; never migrate live closures or replay effects.

Exit: ordinary Shoal use takes the prepared route, required production forms
work, and unsupported platforms/facilities have explicit tested diagnostics.

### 5. Delete the old engine and temporary migration surface

- Follow production consumers through the original deletion ledger. Remove
  Core JIT execution and obsolete translation/normalization/lowering repairs;
  retain Core work still needed for GHC preparation or introspection.
- Remove the temporary engine selector, duplicate turn/session adapters and
  obsolete tests while preserving useful scenarios against the new owners.
- Coordinate artifact/profile/ABI/daemon compatibility changes. Reject stale
  artifacts explicitly and regenerate through the canonical fixture owner.
- Verify generated allocation fast paths through emitted IR or counters;
  report maintained production code and mechanism reduction. Use focused
  measurements for actual regressions, without inventing a historical
  comparative-performance gate.
- Reconcile the support inventory with source; remove stale claims that
  imports are not callable or that no session retention exists. Move lasting
  contracts to owning source and retire superseded plan histories.

Exit: one production engine and one notebook execution route, no hidden Core
fallback, no unexplained session growth, and the final production gates pass.

## Verification discipline

Use focused Nix-backed `just test-lib` / `just test-target` selections while
implementing, compile every changed target, format changed languages and run
`git diff --check`. Run `just fixtures-check` after projection/serialization
changes and the relevant broad gates at the integration exits above. Record
what executed, what only compiled, and what failed or was excluded.

The two reported actor lifecycle timeouts have only commit-message evidence
of matching baseline failures. Recover their exact names, commands, baseline
revision and logs into the integration result when running that gate.

This review read source, prior evidence and implementer answers; it did not
rerun engine tests or certify the reported pass counts. The plan's gates are
future obligations.
