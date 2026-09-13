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
  u32 bits. No baseline was checked and no assertion was weakened. The moved
  VarId tests pass (6/6); restored bridge-value effect routing passes (3/3).
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
  projection's top-level boundary, but do not make target selection complete.
  An exploratory M3 target check for `polymorphicIdentityResult` retained only
  that top binding even though the full projection contains a Local call to
  `polymorphicIdentity`: the prepared `pmBindings` free-variable evidence omits
  that intra-module dependency. The exploratory assertion was not added to
  the passing suite. Connected execution must repair the dependency source
  before treating target-only projection as a complete program.
- The workspace compile-only warm-up is blocked before the prepared targets by
  unchanged `tidepool-macro/src/expand.rs:32`: it calls
  `syn::parse2::<InlineInput>` but this file defines no `InlineInput` type.
  `nix develop --command cargo build --workspace --tests` reproduces E0425;
  `git show HEAD:tidepool-macro/src/expand.rs` confirms the missing type is
  already in committed HEAD. A HEAD-only build was not run. Focused
  changed-target tests compile and run, but this is not a completed workspace
  build.
