# Notebook cell compile-count reduction (design, not implemented)

Branch `engine/stg-production-cutover`. Read-only design. Line numbers refer
to the current working tree.

## 1. Where the compiles come from today

`ResidentActorKernel` cell path (`tidepool-actor/src/resident_actor.rs` ~4494, ~4614):

| Phase | Owner | Spawns |
|---|---|---|
| Whole-cell check | `prepare_cell` -> `check_cell` (`resident_workbench.rs:2002`, `turn.rs:1068`) | 1. The worker may run 1-3 GHC passes internally (`Main.hs` `runCellMode`: display-instance contexts, then fields), but it counts as one invocation. |
| Per non-decl item | `prepare_cell_in_session` -> `compile_block_in_view` -> `run_turn[_pinned]` (`:5810`, `:6063`) | 1 per `Bind`/`Expr` item. All of them are compiled **before any item runs**, against staged ifaces. |
| Per expression, after it completes | `settle_fragment` -> `render_cell_observation` (`:2570`, `:2779`) | (a) page bind `__tidepoolPage{g} <- pure (displayPageWithout [keys] budget (obs ()))`, plus (a') an `opaque` retry compile if (a) is rejected. (b) `inspect_rendered_value` for `(text, pageHasMore, pageUnavailable)`. (c) `cellDisplay <- pure __tidepoolPage{g}` alias. Total: 3, or 4 on a rejection. |

For 14 lets and 6 expressions: 1 + 20 + 6x3 = **39**, or up to 45 with
rejections. Every render compile embeds runtime data in its source: `budget`
comes from `present_output` (`:241`), and `presented` job keys come from
`present_command` (`:250`). No two render modules are byte-identical, so
nothing in them is reusable.

Why the render step is separate today: the budget and presented keys are
known only after the expression's effects and their output have run. Also,
`command_jobs_tests.rs::failed_command_display_retains_result_without_reexecution`
requires that a **runtime** bottom inside `displayTree` yields "Display failed
... Value remains bound as observationN". That means the observation must be
committed before rendering is forced.

## 2. Design: one turn module per expression

### 2.1 Module shape (template owner: `tidepool-runtime::session::turn`)

Add `ExpressionResult::DisplayedObservation` next to `Observation` in
`assemble_expression_module_with_result`. Expose it as
`assemble_displayed_observation_module(preamble, target, effect_stack, expr, lift, page: PageRendering)`,
where `PageRendering` is `Rendered` or `Opaque`. Body (effectful lift shown):

```haskell
__result = __tidepoolInEffectRow $ do {
  __value <- __workbenchValue ;                                  -- authored effects, exactly once
  (__keys, __budget) <- TidepoolInspection.observationCaptured (\() -> __value) ;   -- request #1
  let { __page = TidepoolInspection.displayPageWithout __keys __budget __value } ;  -- or pageWithContinuation … "<opaque value>"
  TidepoolInspection.presentPage __page ;                        -- request #2: strict (text, hasMore, unavailable) payload
  pure (\() -> __value, __page, __page) }
```

- **One spawn, four variants.** `compile_block_in_view` (`:6009`) supplies an
  ordered `TemplateSelector::Bind` list: Effectful+Rendered, Pure+Rendered,
  Effectful+Opaque, Pure+Opaque. The extractor already retries variants inside
  one invocation (`Main.hs` `compileVariants`). This replaces both today's
  `[Effectful, Pure]` observation pair and the page-then-opaque retry. The
  effectful flag becomes `variant ∈ {0, 2}`.
- **Three binders from one compile.** The verdict binders become
  `[observation{g}, __tidepoolPage{g}, cellDisplay]`. `mkBoundBinders`
  (`SessionArtifacts.hs:26`) already splits a tuple result into per-name
  binders with exact types and stable var ids in one `Val.G<g>` iface. No
  extractor change is needed.
- **Runtime inputs cross as request answers, not source text.** The module
  text is independent of budget and keys, which makes it more stable for the
  build-products directory.
- `presentPage` forces its `(Text, Bool, Bool)` payload in Haskell before
  suspending. A bottom in `displayTree` is then a runtime error **after**
  request #1 committed the observation. That preserves the current "Display
  failed" semantics exactly.
- Put the two request constructors in the existing workbench-internal effect
  definitions (owner `tidepool-protocol`; unmigrated ones live in
  `tidepool-mcp/src/effect_defs.rs`). `AgentToolsInputWith` in `begin_tool`
  (`:1817`) is the precedent for an internal row request. Do not add a new
  union member.

### 2.2 Execution (owners: `tidepool-actor::resident_workbench`, `tidepool-runtime::session::resident`)

- `begin_ready_block` (`:2450`) runs the item with a new
  `ResidentSession::run_displayed_observation_with_sites`. It is the sibling of
  `run_observation_with_sites` (`resident.rs:1958`): the same dependency
  preservation, but a plain hole, because completion materializes nothing. The
  three `BoundBinder`s and the generation travel in
  `WorkbenchDisplay::Observation` in place of `source`/`type_modules`. That
  field then no longer needs to carry a compile source.
- `settle_fragment`'s `Suspended` arm (`:2611`) intercepts the two internal
  requests before they can become `Running`. They are therefore never kernel
  operations, never `unit_operations`, and never cancellation points.
  - **#1 `observationCaptured obs`:** mount the payload root as `binders[0]` at
    `g`. Reuse the payload-custody capture used by `capture_kernel_value`
    (`:3978`) and `mount_compiled_binding_in` (`:1906`). Answer
    `(fragment.presented, budget - output chars)`. This is the same
    computation `settle_fragment` does at `:2572` today.
  - **#2 `presentPage page` (text, more, unavailable):** mount the page as
    `binders[1]`, lease it, then call `publish_captured_alias_in` with
    `binders[2]` (`resident.rs:855`) exactly as `:2912` does now. Answer `()`.
  - **Completion:** produce the receipt text from the #2 payload plus the
    `[display continues: cellDisplay.more]` and unavailable suffixes.
  - A runtime error after #1 and before #2 gives "Display failed: … Value
    remains bound as observation{g}". A runtime error before #1 gives
    `render_runtime_rejection`, as today.
- `render_cell_observation` and its `inspect_rendered_value` use are deleted
  from the cell path. `inspect_rendered_value` stays for
  `render_activation_observation`.

### 2.3 Invariants checked against the design

- **Whole-cell typecheck and rejection.** Unchanged. `check_cell` still gates
  everything. Per-item prep rejection still yields `PreparedCell::Rejected{index}`.
  When all four variants fail, the extractor reports the last variant
  (Pure+Opaque). That adds no constraints beyond today's Pure observation
  error, but pin it with a test.
- **Lexical `cellDisplay` within a cell**
  (`notebook_display_keeps_previous_cell_display_lexical_and_publishes_prefix`).
  In `prepare_cell_in_session` (`:5829`), `with_staged_values` and
  `staged_names` must add **only** `binders[0]`. Later items in the same cell
  must keep resolving the pre-cell `cellDisplay`. The alias is still published
  at run time, per successful display, so it is part of the committed prefix,
  including under cancellation.
- **Binding installation order.** Observation, then page, then alias, all
  within one item, as today. Across items nothing changes.
- **Cancellation at item boundaries.** Unchanged. The internal requests
  resolve synchronously inside `settle_item`.
- **Generations.** Today one expression consumes g, g+1, g+2. After the change
  it consumes one g, and `observation{g}` numbering shifts. No tests hard-code
  `observation<N>` or `__tidepoolPage<N>` (grepped), but model-facing
  transcripts will differ.
- **Uncommitted iface names.** If #1 commits but #2 never does, `Val.G<g>`
  exports `__tidepoolPage{g}` and `cellDisplay` without materialized bindings.
  The `reserve_value_generations_through` comment (`:6054`) already tolerates
  a thin iface whose names never materialize, because resolution goes through
  the binding table. Verify that `compile_view` imports never name an
  unmaterialized binder, and add that assertion to the test.

## 3. Consecutive `let` items

**Worth doing only as a second phase.** Sharing a compile while keeping
per-item execution needs item k+1's entry to reference item k's session
binding, not recompute it. Inside one GHC module that is not expressible: the
staged-iface chain exists precisely for this. That leaves two options:

- **(a) Group run: one module, one projected bind.** Rust fans out per-item
  receipts. A maximal run of `let` items qualifies only if GHC's verdict (not
  Rust syntax) marks each as a lazy `let`: no bang patterns, no repeated binder
  names (same-name binders in one `Val.G<g>` collide in `stableVarId`), and
  every pin present. Group pins concatenate `pins_for_item`. Cost of the
  guarantee: binds force WHNF at materialization
  (`notebook_display_prefix_failure.hs`: `boom <- pure (error …)` is a runtime
  rejection). A bottom in item k fails the whole atomic group. The fallback on
  runtime failure is to recompile and run that group per item, which restores
  the committed prefix and attribution at today's cost plus one. Risk:
  `Debug.Trace` output from forced RHSs appears twice. Cancellation
  granularity inside a pure group is unobservable except for receipt timing.
- **(b) Batch prep request: one worker invocation compiles the non-decl items
  sequentially in one GHC session.** Each item's iface is written before the
  next compiles, and per-item `TurnOut`, pins, and diagnostics are kept. This
  keeps every semantic and attribution unchanged. It removes per-invocation
  overhead, not per-module GHC typechecking. Owners: `tidepool-extract-cmd`
  typed request and `run_turn` in `turn.rs`. Measure invocation overhead
  against module work under the daemon before choosing (b) over (a).

## 4. Expected counts (14 lets, 6 expressions, no decls)

| Design | Spawns |
|---|---|
| Today | 39 (up to 45) |
| §2 only | 1 + 20 = **21** |
| §2 + §3(a), lets in *r* maximal runs (1 ≤ r ≤ 7) | 1 + 6 + r = **8-14**; an interleaved let/expr cell (r = 6) gives 13 |
| §2 + §3(b) | 1 check + 1 batch = **2** invocations (GHC module work ≈ 20 modules) |

Paging (`cellDisplay.more`) stays at 1 check + 1 item.

## 5. Owners to change

- `tidepool-runtime::session::turn`: `DisplayedObservation` result,
  `PageRendering`, and the four-variant template list builder (for example
  `resident_observation_templates` in `session::workbench`, next to
  `resident_workbench_templates`).
- `tidepool-runtime::session::resident`: `run_displayed_observation_with_sites`,
  and mounting a parked-request payload as a named binding.
- `tidepool-protocol` (or `tidepool-mcp` `effect_defs.rs` if unmigrated): the
  `observationCaptured` and `presentPage` requests. Regenerate the Haskell
  surface. `haskell/lib/Tidepool/Inspection.hs` gains the two verbs; the strict
  summary lives next to `pageHasMore`/`pageUnavailable`.
- `tidepool-actor::resident_workbench`: `compile_block_in_view` templates and
  verdict, `prepare_cell_in_session` staged-name filter, `begin_ready_block`,
  `settle_fragment` internal-request arm; delete `render_cell_observation`.
  The `presented` and budget state already lives on `ResidentWorkbenchFragment`.
- Extractor: none for §2. §3(b) needs a batch turn request in
  `tidepool-extract-cmd` and `Main.hs`.
- Docs: `haskell/CLAUDE.md` deploy steps for the stdlib change, and
  `just fixtures-check` if the effect schema changes.

## 6. Risks

- **Error attribution.** Four variants per item: confirm the reported
  diagnostic is the opaque-Pure one, and that `render_turn_compile_error`'s
  `attempted_source` span remap still points into `<cell item N>`. §3(a) group
  compile errors must remap to the originating item. Keep the per-item
  recompile as the attribution path.
- **Memo and cache keys.** Turn spawns bypass the `tidepool-toolchain::cache`
  memo (`turn.rs` `extract_cmd` doc). Only the build-products directory
  matters, and moving runtime data into answers makes module text stable.
  Nothing request-local enters a reusable key. §3(b) must keep each module's
  `--bind-gen` and inject set exact per item.
- **Suspension cost.** Two host round-trips per expression replace three
  compiles. Negligible, but the `settle_item` loop must not publish posture or
  operations for them.
- **Custody.** The #1 payload mount must check `parked_realm`, as
  `capture_kernel_value` does. It must also be released on abort between #1
  and #2 without unrooting the committed observation.

## 7. Test (compile-count as an observability contract)

Add it in `tidepool/src/actor_host/documentation_tests.rs`, beside the
`notebook_display_*` tests (the crate already depends on
`tidepool-extract-cmd`; nextest process isolation keeps the global counter
private):

1. Start `TestCampaign`. Commit a warm-up cell so startup compiles are behind
   us. Then `tidepool_extract_cmd::reset_extract_spawn_count()`.
2. Dispatch a fixture cell: 1 decl, 3 lets, 1 displayable expression large
   enough to page, 1 function-valued expression (the opaque variant), and a
   `cellDisplay.text` item. Assert
   `extract_spawn_count() == 1 + non_decl_items` (= 6). Under §3(a) use
   `1 + exprs + let_runs`. Also assert the receipts, the
   `[display continues: cellDisplay.more]` marker, the opaque output, and the
   in-cell `cellDisplay.text` still returning the previous page.
3. Reset, run `cellDisplay.more`, and assert 2.
4. Reset, run `command_display_failure.hs`, and assert "Display failed" and
   "Value remains bound", with count `1 + 2` (let + expression). Then
   `observation{g}` must still be usable.

Assert exact equality, not `<=`, so a regression back to render-time compiles
fails loudly.

## 8. Engine-neutral design (notebook slice)

The notebook slice is step 2 of the STG completion sequence; step 5 is the
Core deletion that must not inherit this work as debt.

- Do **not** implement the render as hand-built Core (for example Rust
  constructing `App(Var render, keys, budget)` against a compiled table). It
  is Core-only and dies in step 5. The request protocol above uses the freer
  suspension both engines share.
- The prepared route must support, and step-2 acceptance should list:
  - a structured host answer `([Text], Int)`: list and Text, per the
    typed-resume "Host-built answers" decision;
  - mounting a parked request's live payload as a named binding with realm
    check: the typed-resume "Live answers" and handle custody decision;
  - multi-binder metadata from one turn. `prepared_turn_module` is currently
    single-declaration, so it needs a tuple-projected or three-top form.
- Keep the four-variant list and the staged-name filter in the shared
  template and prep owners, so the prepared notebook turn reuses them rather
  than growing a second display path.
- §3(b)'s batch prep request should be designed against the prepared
  compile-view, not Core `TurnOut` alone, if it is pursued.
