# Development friction notes

Concrete check and iteration costs observed during corpus, notebook, and STG
integration work. These are follow-up candidates, not evidence that the tests
are nondeterministic.

- **Prepared allocation API was a disconnected sketch.** The descriptor-named
  emitter delegated to the Core allocator (including its poison-pointer return
  convention), and prepared entry parameter zero was used as an output buffer
  despite the ABI declaring VMContext. The existing moving-collection test ran
  only after native return. Following the production call graph exposed this;
  helper names and isolated tests did not. Integration evidence should name
  where collection occurs and which root/heap owner it exercises.
- **Heap-format assumptions were spread through the collector.** Reusing the
  checked frame walk is correct, but the Core body also performs legacy shape
  discovery, external-payload scanning, growth recopy, and verification. Compact
  descriptor objects must dispatch before all four. The active GC state now
  identifies the physical format; shared root capture remains with MachineState.
- **A focused codegen lib test still builds runtime consumers.** Adding a typed
  native runtime error made `cargo test -p tidepool-codegen --lib prepared_`
  stop in `tidepool-runtime`'s exhaustive failure classifier before any tests
  ran. This caught a useful boundary obligation, but the dev-dependency fanout
  makes the cost unlike an isolated backend check. Keep the integration check;
  make the dependency/build scope visible in the focused-check tooling.

- **Flaky gate to revisit: `Project.RoutingChecks.routing`.** The full recipe
  pass failed late on an exact-output assertion; the corrected isolated case
  compiled, but its final result was lost with an interrupted terminal session.
  Treat this recipe as unconfirmed until a clean post-fix run completes. The
  observed failure was assertion brittleness, not demonstrated randomness.
- The full `shoal check --recipes` run reached a late
  `Project.RoutingChecks.routing` assertion after roughly 26 minutes. The CLI
  has no recipe selector, so isolating that case required a temporary workspace
  copy and edits to both its `checks` list and recipe body. Add a targeted
  recipe selector.
- `Tidepool.Check.check` reports the assertion label but not the observed
  value. A failed `ReplyOpen` equality therefore needed an instrumented rerun
  to distinguish changed semantics from a stale display expectation. Let
  checks attach expected and observed values directly.
- A cell with an import followed by an expression renders both the binding
  notice and the expression (`defined at generation 1\nReplyOpen`). Recipe
  assertions comparing the entire output to a constructor are brittle; checks
  should expose expression results separately from binding notices.
- The routing recipe combines several actor lifecycles in one check. A failure
  near its end recompiles and replays earlier cases when rerun. Split
  independent scenarios into separately selectable recipes.
- Even the isolated scenario takes minutes: it starts a fresh extractor
  process for each hosted cell. Reuse a warm extractor across cells in one
  recipe run if its compilation and custody contracts permit it.
- The first temporary extraction of one scenario failed with an ambiguous
  `Member RecipeCheck effects` constraint because the displaced recipe body
  needed its own signature. A supported scenario filter would avoid source
  surgery and this diagnostic detour.
- A published-example assertion expected `Low` while the shipped example
  explicitly selected `Medium`. Keep expected launch policy adjacent to the
  fixture that sets it, or assert the policy from the recipe value.

## STG cutover — 2026-09-12

- **A schema-only test selection compiles the runtime.**
  `bash scripts/dev-shell.sh cargo test -p tidepool-repr --lib execution_schema::validation::tests -- --nocapture`
  compiled bridge, heap, effect, codegen, toolchain, and runtime before any
  selected test ran. It stopped on four stale interpreter `Value` variants in
  `tidepool-runtime/src/render.rs`. The repr crate's `tidepool-testing`
  dev-dependency brings the larger stack into this otherwise local check.
  Follow-up: isolate pure schema tests from execution-harness dependencies;
  keep cross-layer contracts in their own target. This is dependency fan-out,
  not a failing schema test.
  Implemented follow-up: moved the two tests that actually use the high-level
  generators into the existing testing crate and removed repr's dependency on
  that harness. Pure schema checks no longer need that dependency edge; the
  moved scenarios remain integration tests rather than being deleted.
- **Shared build admission is manual.** The same focused command first waited
  for Cargo's artifact-directory lock while an agent's build finished. A
  serialized build policy still needs explicit start/finish coordination;
  Cargo's message does not identify the owning agent or command. Follow-up:
  expose the active build owner/command through the existing command runner,
  without duplicating caches or introducing another process supervisor.
- **Projection omissions are discovered too late without a field inventory.**
  The systematic pinned-GHC audit found explicit rejection of `LitRubbish`
  and `LitNullAddr`, and missing tag/capture facts, after consumer work had
  begun. It also corrected apparent gaps that unarisation already resolves.
  `docs/stg-projection-inventory.md` now records the producer field, wire
  mapping, and remaining consumer obligation together. Keep that inventory
  current when either side of the process boundary changes; wire acceptance
  alone is not evidence of native support.
- **Endpoint retirement reaches low-level test compilation.** After the render
  migration was fixed, the focused repr command still selected/executed no
  tests: `tidepool-mcp/src/eval_prep.rs:1093` referenced the removed
  `resources::exclusion_reason`. Deleting an endpoint needs a whole-workspace
  symbol inventory, including test-only callers, before calling the batch
  complete. Retrying the same blocked target before its owner confirms the
  source repair adds no evidence; coordinate that handoff explicitly.
- **Pinned GHC API lookup needs to precede authoring.** The first projection
  compile failed on a guessed `mkVisFunTys` import; GHC 9.12.2 exposes
  `mkScaledFunTys` for that input shape. This was an authoring error, not
  toolchain instability. A discoverable pinned compiler-source location and
  lightweight GHC API inspection recipe would shorten this lookup and avoid
  learning exported names through a larger Cabal compile.
- **Generated fixture ownership was tied to an interpreter being removed.**
  Three prepared tests in toolchain/codegen/runtime included a CBOR file from
  `tidepool-eval/tests/fixtures`. Removing that crate broke unrelated prepared
  consumers. The generator now writes the shared fixture under
  `haskell/test-prepared-stg/fixtures`; the existing broad `*.cbor` ignore rule
  required explicitly force-adding it. Follow-up: make the canonical generator
  outputs and tracked-fixture exceptions discoverable together, and regenerate
  once a coordinated schema/ABI edit is complete rather than mid-batch.
- **Freshness and execution were coupled in `fixtures-check`.** Retirement
  exposed two unrelated blockers after successful regeneration: stale Nextest
  filters, then a hardcoded invocation of the deleted evaluator's semantic
  suite. The command now verifies fingerprint and freshly regenerated bytes;
  it no longer claims semantic verification. The compiled-GHC oracle remains
  pending. The output's `meta.cbor (153 entries)` counts metadata entries,
  not fixtures. Measured inventory is 348 CBOR files, 347 asks JSON files and
  one fingerprint (696 files). Neither number is the older brief's 217
  semantic-test figure. Label counters at the producer to avoid this reporting
  ambiguity; an earlier agent report incorrectly called 153 the corpus count.
- **Generator freshness errors need an actionable rebuild path.** An inherited
  `TIDEPOOL_EXTRACT` pointed at an older Rust wrapper, so `fixtures-update`
  stopped before generation. Rebuilding the wrapper and Haskell worker through
  the dev shell resolved it without `TIDEPOOL_ALLOW_STALE_EXTRACT`. The next
  regeneration changed only the fingerprint, no CBOR/asks bytes. Follow-up:
  have the freshness diagnostic name the owning rebuild command and both
  selected executables, not merely offer a stale-binary override.
- **Prepared tests depend on generated table numbering.** The native fixture
  tests hardcode `ValueId(17)` and signature index 6. After intentional fixture
  regeneration, two native cases compiled but failed with
  `Unsupported("non-parameter local constructor field")`. Their cause has not
  been investigated here, and assertions were not adjusted. Symbol-based
  selection would make these tests readable and less dependent on unrelated
  projection traversal changes.
- **A shared fixture no longer exercises a specific import contract.** The
  toolchain mismatch test requires a known callable import, but the regenerated
  M3 fixture has none: unknown imported entry evidence now remains unknown.
  The test fails at its explicit fixture prerequisite. Use scenario-specific
  producer fixtures for required ABI evidence rather than assuming arbitrary
  package imports carry it. Linker unit tests separately cover the contract;
  that does not make the toolchain test pass.
- **Requested and executed check scope can diverge in delegation.** The final
  toolchain request named `prepared_artifact::tests`, but the agent ran all 89
  lib tests. This was not a workspace battery, but it exceeded the requested
  selection. Handbacks must report the actual command and counts; passing a
  filter as a structured task field would reduce this coordination error.

## STG review follow-through

- Replaced prepared fixture numeric binding/signature selectors with symbol
  lookup. The two formerly red native cases now pass without changing their
  behavioral assertions. Added an actual `Data.List.reverse` call to the
  producer fixture; the callable-import prerequisite and all three focused
  artifact tests now pass using GHC-derived evidence.
- Moving the pure CBOR/VarId tests out of `tidepool-repr` removes its harness
  dependency fanout while retaining the scenarios in `tidepool-testing`.
  `cargo test -p tidepool-testing --test proptest_cbor` exposed a red
  `literal_round_trip`: minimal `LitFloat(4294967296)` is rejected as exceeding
  u32 bits. Source inspection identifies this as pre-existing; no main baseline
  run was used and it is distinct from the separate historical float tests. No
  assertion was weakened. The moved VarId tests pass (6/6); restored
  bridge-value effect routing passes (3/3).
- Compile-only integration found a remaining `EffectRoster` caller after MCP
  endpoint deletion. Shared handler declaration composition now remains in
  the shared owner, without restoring the removed server.
- Semispace growth can reuse a former allocation's numeric address. Pointer
  inequality cannot establish whether collection occurred: the runtime now
  records completed copying explicitly for cursor publication and generation
  invalidation, including growth failure after a successful first copy.
- Ambient root registries can contain the same slot more than once. The new
  collector reuses sorted/deduplicated root-slot scratch before mutation;
  accepting arbitrary destination pointers would hide invalid initial roots.
- A full-suite test can expose a missing migration that focused library tests
  miss. After removing the obsolete expression-depth limit, the repr library
  passed but `execution_schema_codec` still constructed `DecodeLimits` with
  `max_depth`. Keep wire-shape integration tests in the schema change's focused
  compile selection; the corrected 13-field codec suite now passes.
- A deep-value test exposed a dormant Drop-queue borrow bug. The `while let`
  condition held a `RefMut` across `drop(value)`, and dropping a constructor
  re-entered the same queue, panicking and then overflowing during unwind.
  Extracting the next item in a shorter borrow scope makes the existing
  iterative Drop work at 30,000 levels on a 64 KiB stack.
- GHC local `Id` reuse across separate top-level RHS scopes invalidated the
  projection's program-wide `Id`-to-`ValueId` memoization: parameters of
  `importedReverse` and `Box` both became `ValueId(41)` in the M3 fixture.
  The schema-v4 global-binder check caught it before native compilation.
  Projection now allocates lexical binder IDs with scoped lookup, with a
  producer regression; a Rust-side duplicate exception would hide the alias.
  The regenerated fixture also moved numeric IDs, exposing one retained-session
  test still selecting entries by number. It now resolves Box and entry by
  their symbolic identities.
- Exact-symbol closure keys remove a second GHC-unique assumption at the
  projection's top-level boundary. Wave 3's structured binding traversal now
  makes target selection complete for the M3 case: targeting
  `polymorphicIdentityResult` retains its local `polymorphicIdentity`
  dependency while excluding unrelated tops, and the producer regression
  passes. The earlier exploratory omission is retained as history, not a
  current execution blocker.
- The historical workspace warm-up report blamed unchanged
  `tidepool-macro/src/expand.rs:32`; source inspection instead attributes that
  missing `InlineInput` code to `e1a4b9145`, not unchanged `main`. In the final
  fold, the workspace compile reached all crates and failed in
  `tidepool-actor/tests/resident_local_actor.rs` because `sibling_server` is
  undefined at lines 321 and 329. This is outside F's repair scope.

## Wave 3 fold evidence — 2026-09-12

- F regeneration and the owning prepared checks are complete at working-tree
  revision `204604dc19ce526dde3d717f3c8e0078714a5002`; the tree remains dirty.
- A reports 49 final schema tests passed; B's corrected target-closure
  projection regression is 1/1; C's lock repair leaves 2 additions and 339
  deletions with no retained upgrades, and the `tidepool` library check passes.
- D reports 4 tag, 10 descriptor, 7 descriptor-bridge, 1 prepared-native,
  and 1 ABI-rejection test passed; the regenerated-fixture fold is recorded
  below.
- E's correction passed 49 heap and 3 prepared-GC tests; the forwarded
  incoming-tag regression passes after the root-order fixture fix,
  `descriptor_at` uses raw pointer/local dereference, and capacity growth
  passes. G's residual helper migration is complete; regenerated reruns are
  generics 0/11 and repro-339 0/1, with concrete prepared representation,
  duplicate-`sat`, and Word(32)/Word(64) signature failures.
- Retirement dropped the differential/proptest scenarios while preserving
  explicit JIT assertions. Trial rounds are A 4, B 3, C 2 plus Terra 1, D 1,
  E 3 plus Terra repair, and G 3. These counts distinguish worker/tool turns
  from lead assignment/review/correction interventions; complete lead totals
  and token usage are unavailable. Brief-size insufficiency remains part of
  the record; E's sub-item escalation is resolved.

## Wave 4 integration friction — 2026-09-12

- Shared-tree tests encountered temporarily undefined emitter helpers and ran
  zero tests. A build-slot lease prevents simultaneous builds, but does not
  freeze the source being compiled. A worker-ready revision acknowledgment is
  needed before integrated checks; a quiet build queue alone is not readiness.
- Flat-wire handwritten fixtures introduced forward/self child references while
  adding Case and Let coverage. Validate fixture construction before debugging
  generated execution; keep these failures distinct from emitter failures.
- Allocator emission changes the active native block. Scheduling a continuation
  with the pre-allocation block is invalid even when its Rust types compile.
  The worklist boundary must use the allocator's continuation block explicitly.
- Several emitter handbacks ended at partial progress or awaiting a build slot,
  requiring parent follow-up merely to resume the same assigned obligation.
  Typed worker states should distinguish ready-for-build from task-complete;
  a slot grant should resume the existing obligation without a new assignment.
- The corpus runner and recipe handoff disagreed whether prepared output was a
  file or a directory. The artifact path contract must be explicit at the
  runner boundary before corpus aggregation is treated as evidence.
- A duplicate inline/file `tests` module in prepared codegen blocked the
  focused test compile until the inline slow-entry tests were renamed; keep
  module ownership explicit when extracting test files.
- A runtime wire failure was first attributed to a stale fixture, but canonical
  byte normalization rejected that explanation. Preserve producer byte-order
  evidence before changing fixture expectations.
- Current focused evidence is repr 2/2, runtime 6/6, comparator 7/7, and
  runner 6/6. Prepared engine evidence is 26/27; the remaining nested-function
  fixture fails with `InvalidScope("value ValueId(1) is out of scope")`. This
  is not a corpus or workspace gate result.

## Wave 4 identity/corpus follow-up — 2026-09-13

- Binder reachability and wire identity must use different scopes. The
  inventory walk retains GHC binder `Unique` values; only the complete-module
  `VarEnv` assigns stable wire symbols. Missing home tops and unknown internal
  names are typed projection failures, not opportunities to synthesize local
  imports; genuine external package imports remain admissible.
- Manifest v2 separates actual prepared coverage from historical provenance.
  The Suite producer enumerated 812 actual tops, with 802 projection passes
  and 802 validation passes; 10 rows were rejected at projection. The old
  347-name ledger mapped 255 names and left 92 explicitly unmapped. These
  denominators must not be combined.
- Reached comparison evidence was 56 passed and 0 failed, but 67 rows lacked
  expectations. Admission and execution remain bounded by the connected
  native subset, so this is not a full-corpus success claim.
- Exact oracle identity is deliberately narrow: only an external occurrence
  gets a key, while local and rejected rows do not. Suffix aliases are rejected
  by consumer validation. Ambiguous exact external legacy matches fail the
  producer, and an all-tops source or identity-enumeration failure aborts
  rather than emitting a falsely successful empty manifest.
- A preset stale `TIDEPOOL_EXTRACT_WORKER` caused the first
  `just fixtures-update` attempt to fail. Unsetting both extractor paths let
  the recipe resolve and rebuild a matched worker; update and check then
  succeeded. This should be automated at the recipe boundary so routine
  source changes do not require callers to diagnose stale shell state.

## Wave 5 exact recovery and lazy entry

- GHC's source loader can rebuild imported fixture modules with the session's
  flags, replacing an explicitly generated fat interface with a thin one.
  The exact-lookup fixture must preserve fat flags during loading, then rebuild
  the deliberately thin case. The repaired focused test passes.
- GHC's package-interface cache strips declarations needed by recovered-body
  preparation. Reading the exact finder-selected interface before typechecking
  it avoids the PIT's `No mi_decls` panic; module identity remains checked.
- A passing source-only worker report hid a scope omission: local thunks were
  still rejected to preserve an earlier admission test. Contract briefs need
  both top and local examples, not just a CAF example and a broad mechanism name.
- Shared-tree worker completion and a free harness thread are not always
  simultaneous. Several fresh spawns reported `agent thread limit reached`
  until the previous worker's completion event arrived. This is coordination
  overhead for the proposed typed worker/build-state owner, not coding work.
- Parent review caught host-side scalar initialization writing a machine word
  for narrow packed fields. Shared typed artifact builders prevent wire drift
  but do not replace mixed-width layout interaction tests.
- IR contract tests selected calls by argument count; adding a two-argument
  cancellation poll silently changed what they measured. Resolve the callee's
  declared identity instead. Both repaired allocation/entry contracts pass.
- Pure primop ports repeatedly confused GHC constructor labels with emitted
  occurrence names. Worker briefs must point at an executable table query or
  the authoritative occurrence mapping, not merely the old emitter enum.
- A deep observation fixture compiled one function per data node, making an
  iterative traversal test impractically expensive. Build a validated static
  image of the large data behind a tiny compiled program instead. The real
  20,000-node forcing observer then passes on a 256 KiB thread stack.
- A shared-tree test failure was reported as "pre-existing" without revision
  evidence. It was a sibling's concurrent edit. Reports should distinguish
  baseline failures, concurrent parcel failures and unknown provenance.
- Serializing builds does not freeze their inputs: a worker can edit a shared
  dependency halfway through another worker's compilation. Verification needs
  a revision/input snapshot as well as a build lease. Ephemeral worktrees solve
  the source race; a fold still needs explicit dependency-ready handshakes.
  Typed coordinator messages should carry producer parcel/revision and required
  consumer edits, rather than asking the lead to relay import/signature fixes.
- A fresh worker review accepted a cached Box-derived admission pointer across
  later exclusive borrows. Focused tests also passed. Rust's Box alias contract
  still forbids that use; a scoped owner borrow and automatic pointer cleanup
  make the invariant explicit. Unsafe ownership/provenance needs lead review,
  not just a quick worker spot check or a claim of stable allocation.
- A corpus probe omitted the real generated Effects.Core include and therefore
  reported a missing-module failure before reaching engine admission. Reuse
  the production generator's directory, never the similarly named fixture stub.
- Recovering a body successfully was obscured by a later generic ExitFailure
  residual. The saved GHC lint diagnostic distinguished missing subset scope
  from a binder/body type mismatch. Preserve stage-specific diagnostic evidence
  so corpus accounting does not send the next repair to the wrong owner.
- Primitive porting exposed a producer/consumer lifetime gap: the compilation
  plan pinned scalar string literals, but the compiled owner retained only
  top-level strings. Code-address inventories must include every embedded
  allocation, and primitive strings need GHC's implicit terminal NUL in backing
  storage without changing their logical wire bytes.
- A GHC panic message named `mkSeqs` even though the fault was our later facts
  walker forcing an intentionally undefined annotation. Reproducing the panic
  identified the affected body but did not establish its mechanism; checking
  the pinned constructor definition and every consumer avoided a workaround
  that would have disabled a valid GHC pass. Exception text alone is not an
  ownership boundary.
- A build lease stayed held while an interactive Cabal repl was idle. Lease
  handback should report whether a compiler/test process is actually running;
  a live shell session is not evidence of useful build work.
- Flat-wire fixture migrations produced four focused failures from stale tags
  and hand-maintained expression indices. Helpers should allocate child nodes
  before parents and return their indices; a schema change needs a search for
  handwritten envelope fragments, not only struct constructors.
- Similar primitive names do not establish a shared mutation contract:
  shrink returns State# while resize returns a replacement handle. Inventory
  exact pinned-GHC signatures before designing alias settlement; otherwise a
  worker can faithfully implement a contract that invalidates the caller's
  only usable handle.
- A completed worker did not process follow-up corrections sent as ordinary
  messages; it needed an explicit resumed task. Build routing then waited on
  work that was not running. A coordinator should distinguish queued messages
  from an active repair task and require an acknowledgement of the assigned
  revision before marking that dependency in progress.
- Moving an error vocabulary across the heap/runtime boundary exposed raw
  diagnostic pointers that made the enclosing runtime error non-Send. Carry
  numeric addresses in errors, not ownership-shaped pointers; compilation
  caught this before any unsafe Send workaround was introduced.
- New GC scratch growth was initially fallible after forwarding began. The
  existing source-header walk provides an exact upper bound on external
  handles, so reserve traversal storage there. Allocation timing is part of
  the failure contract, not merely a performance detail.
- The array inventory and lead seed copied source State# result positions into
  wire results. The validator caught the resulting Void Case before execution.
  The producer uses GHC's physical `typePrimRep_maybe` for results (no Void
  constructor), but deliberately injects Void for zero-width arguments. Check
  both producer conversion functions before transcribing source signatures.
  This was a lead contract defect, not a worker implementation defect; neither
  the schema nor the validator needed loosening.
- Rust-only verification through `just` can be blocked by a concurrently edited
  Haskell worker's freshness check. The focused Nix Cargo run still used the
  pinned toolchain and checked the owning Rust target, but did not establish
  extractor freshness. Report those as separate checks rather than rebuilding
  the worker for every independent Rust edit.
- A corpus child has a watchdog through the shared testing helper, not an
  inline timer in the runner. Two source investigations missed that indirection.
  Follow the owning helper before diagnosing a hang or killing the parent.
- A rebuilt test binary does not refresh a separately built projection probe.
  Record the actual executable identity when checking a producer correction;
  stale probes can make a fixed typed diagnostic appear absent.
- The corpus script reuses build-directory executable paths throughout a run.
  Current coordination freezes Cargo builds for that run; copying runner/probe
  binaries into the result directory would make provenance independent of the
  build lease and permit unrelated compilation safely.
- Focused scalar compilation hit another worker's half-written byte-array
  expression despite disjoint ownership. A shared tree isolates files, not
  compilation revisions; ephemeral worktrees would remove this interruption.
- A direct build-lease handoff stalled with both workers waiting for the other,
  after an intervening verification parcel changed the queue. A typed lease
  owner plus acknowledged grant/return events would remove this ambiguity; a
  prose statement that the slot is "free" is not an acknowledged assignment.
