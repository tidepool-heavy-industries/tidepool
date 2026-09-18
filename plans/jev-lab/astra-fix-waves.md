# Fix waves from the parked Astra flight

## Context

Astra (gpt-6-astra, medium) drove a two-hour Shoal session in `~/dev/tidepool-astra`
on 2026-09-17/18. The session is parked. Evidence: the root Codex rollout
(95 tool calls), six Codex-native subagent rollouts, the host and compiler logs,
Astra's `.shoal/discoveries/` notes, its uncommitted patch, and four interview
rounds. Astra's own summary: *the computational model is ahead of the everyday
affordances — fewer tiny mysteries, not fewer capabilities.*

What the evidence shows:

- Two real harness faults: the observation budget rejected committed work
  (45 operations, then `reflect 3`), and `lookup` returned `no match` for
  qualified type names (`Cmd.CommandResult`, `Cmd.RunResult`, twice).
- 44 direct shell calls against 33 cells; outputs then re-fetched inside Haskell.
- Six delegations to Codex-native subagents against one Shoal child.
- Four GHC rejections, all "tiny mysteries": same-cell scope (x2), `String` vs
  `Text` for `Cmd.argv`, `Cmd.stdout` applied to a completion event.
- The host log is 350 lines for two hours. No spans exist in `tidepool-actor`,
  `tidepool-runtime`, `tidepool-codegen`, `tidepool-handlers`. Cells, receipts and
  GHC diagnostics exist only in the provider's rollout; the compiler log holds no
  diagnostic text; child task text is encrypted.
- 233 compiles for 33 cells; one 26 s compile right after the routine worker
  rotation at 256 requests; about 75 s for the first two small cells.

Decisions already taken by the user (do not reopen):

| Topic | Decision |
|---|---|
| Executor | Opus sessions, one wave each; Sonnet for mechanical parcels; no large Opus fan-out |
| Ambition | Repairs first, then marked bets each with a kill criterion |
| Primitives to own | whole-stream read, guarded file replace, `Text`-returning show |
| Tracing | `tracing` spans from the start; add `tracing-appender`; content ON in a run-local file (trusted dev box) |
| Direct tools | every direct shell call binds a retained job in Haskell scope |
| Cell scope | make source order work (not merely a better error) |
| Delegation | make Shoal children as cheap as native subagents |
| Oversized bare expression | bind it and show a bounded view |
| Astra's lookup patch | we adopt and finish it |
| Active source | status view only |
| Acceptance | 3-6 named tests per wave plus one live cell through the proxy |
| jev-dsl | workspace `flake_sources`; upstream front made JSON-generic; docs rewritten from upstream's authoring guide; parallel track |
| Deferred | edit-a-draft-cell; loading model-written Haskell from a `.shoal` dir mid-session |

Where model-written Haskell belongs, per Astra: `.shoal/discoveries/` holds
experimental source, examples and observations; a battery two consumers genuinely
share is promoted to one ordinary module under `.shoal/Project/` with a concrete
purpose and explicit inputs. Discoveries must not become a competing library
location. Inherited live bindings are snapshots, never the distribution
mechanism. That promotion step is what the deferred load-from-a-directory idea
should serve when it arrives.

Guiding rule from Astra, adopted: a correction that can live in an error, a type,
a discovery result or an executable example lives there, not in a new guidance
paragraph. Items Astra filed as its own ordinary code (receipt type,
gather-before-drafting, stopping policy, `runWithFollowup`) are NOT shipped
(primitives-not-helpers).

Work tree: `~/dev/tidepool-jev`. Never touch `~/dev/tidepool` or the parked
`~/dev/tidepool-astra`. Commit by pathspec. Focused tests only; no `just verify`
until the end.

---

## Wave 0 — finish what is already in flight (Fable, this session)

1. `tidepool/src/actor_host/observation_budget_tests.rs:120` — follow-up cell
   becomes `either (const 0) length editorialContext` (`reflect` returns `Either`).
2. Vacuity check: revert the `resident.rs` Bind/Project arms, confirm both tests
   fail, restore.
3. Commit fix + regression + `plans/jev-lab/observation-limit/` + `EVAL-RUNS.md`.

## Wave 1 — observation and receipts tell the truth

- **Oversized bare expression** (`SettlePlan::Observe`,
  `tidepool-runtime/src/session/resident.rs` ~851): on `BudgetExceeded`, keep the
  handle, bind it under the next `itN`-style name, and render a bounded view
  through the existing `render_cell_observation` / `cellDisplay.more` paging.
  Needs a budget-bounded partial materializer in
  `tidepool-codegen/src/heap_bridge.rs` (the mechanism-index home for
  heap-to-`Value`), emitting `OVERSIZE_SENTINEL` at the cut.
- **Receipt attribution** — diagnosed exactly, do not re-investigate. Every
  non-command arm of `resolve_effect` fuses producing the response with resuming
  the machine past it: the Reflect arm at
  `tidepool-actor/src/resident_actor.rs:2074-2094` reads the conversation and then
  calls `runner.resume_value(...)` in the same future. So the value `resolve_effect`
  returns reflects everything downstream, not the effect. At
  `resident_actor.rs:4488-4529` the `Err` arm records
  `command_disposition.unwrap_or(Unknown)`, so any downstream failure is
  attributed to the last effect. That is how a successful `reflect 3` became
  `Unknown (reflect)` when the later bind blew the display budget, contradicting
  the documented meaning at `workbench.rs:265-268`. An effect whose response was
  delivered is `Committed`; `Unknown` stays for failure before or during delivery.
  The command path at `resident_actor.rs:4467-4476` computes its own disposition
  and must keep winning.
- **Failure layer as data**: one enum — compile, effect, observation — carried on
  the item receipt and rendered in the tool text ("effects committed; observing
  the result failed; the value is bound as …"). Same field feeds Wave 2 spans.
- Tests: extend `observation_budget_tests.rs` (bare-expression case, receipt text,
  `reflect` disposition); live cell: Astra's preserved audit cell.

## Wave 2 — tracing (can start alongside Wave 1)

Base: `tidepool/src/shoal.rs:1254-1299` already builds a layered registry.

- Add a JSON layer (`fmt::layer().json()`, span list, `FmtSpan::CLOSE`) writing
  `<run_id>.jsonl` through `tracing-appender` non-blocking; keep the guard alive
  for the host's lifetime.
- Span tree, using identifiers that already exist: run → actor{path,incarnation}
  → tool_call{call_id,tool} → cell{execution} → unit{index,kind} →
  effect{ordinal,name}; compile requests as child spans carrying
  `compile_request`. Record status, disposition, failure layer, byte sizes on
  close. `.instrument()` every spawned future so children inherit context.
- Content target (`shoal::content`): cell source, receipts, lookup queries and
  results, GHC diagnostic text, child assignment text. ON by default in the
  run-local file. The existing privacy assertion (request-update text,
  `actor_host.rs:5790`) stays true: that text is never emitted.
- Pass run id and request id to the extractor daemon
  (`tidepool-extract-cmd/src/daemon.rs:70-112`) so both logs join.
- Tests assert on span fields via the capture-writer pattern already in
  `daemon.rs:935`. Live check: reconstruct one cell end to end from the JSONL
  alone, without the Codex rollout.
- First use: explain the 75 s first cells and the 26 s post-rotation compile.
  Candidate repair (only if spans confirm): pre-warm the replacement worker
  before retiring the old one.

## Wave 3 — discovery returns a usable starting point

- **Adopt Astra's lookup patch** from `~/dev/tidepool-astra` (read-only source;
  apply the functional hunks only): `lookup_module_fallbacks`
  (`resident_actor.rs`), the hosted test, the helper test. Fix review findings:
  update `LOOKUP_DESCRIPTION` (`lookup_tool.rs:19-21`); a failed fallback round
  must not discard round-one results; avoid the doubled round trip by sending
  Info and Browse for a dotted capitalized query in ONE `lookup_inspections`
  batch and choosing per result; add tests for a missing qualified name through
  the hosted path and for a name that is both type and constructor. Drop the
  rustfmt-only hunks. Also adopt the `shoal-command/SKILL.md` hunk, adding the
  `Cmd.readOutput`/`Cmd.next` route for a failed command's streams.
- **One usage pointer per callable**: `LookupResponse::render_text` gains a single
  link to a worked topic (skill section or `.shoal/examples/*.hs`). Source of
  truth is an index derived from the shipped examples at build time — no second
  hand-maintained index.
- **Near-match on `no match`**: `Cmd.resultOf`, `Cmd.exitCode`, `R.await`,
  `R.lift` all missed; suggest the closest exported names in that qualifier.
- **`replace` is untested, not undiscovered.** `haskell/lib/Tidepool/Actor/Record.hs:361`
  exports `replace :: ActorHandle api -> ActorSpec api effects -> Eff parent
  (ActorHandle api)`. Astra saw it and passed it over deliberately: its two
  collectors were different command attempts, not a policy change on one
  continuing workload. What it lacked was a worked account of event delivery,
  pending work and old endpoints. So the deliverable is one TESTED example plus a
  written contract, and the lookup pointer to it.

  Astra's desired semantics, to be checked against actual behaviour — document
  every divergence rather than quietly matching the list:
  - a check still running delivers its eventual completion to the new handler;
  - a handler already executing finishes under its original policy, neither
    interrupted mid-effects nor replayed;
  - accepted but unstarted events run under the new handler, in order;
  - retained state survives replacement;
  - reattaching subscriptions does not reprocess an already-consumed completion;
  - outstanding commands and requests stay owned and observable, never orphaned
    or implicitly cancelled;
  - receipts identify which policy revision handled an event;
  - invalidated endpoints are explicit, never silently retargeted.
- **Reflex table follow-through** — ours to finish; Astra has paused its side to
  avoid parallel edits. Its change removed the automatic add-import step and
  routed unresolved-name diagnostics to inspection first, because a diagnostic
  code alone does not establish that importing is the repair. Three artifacts
  still describe the deleted step: `tableVocabulary` in the Reflex table, the
  upstream `plans/jev/reflex_table.json`, and
  `plans/jev/addendum-A-B-2026-09-17.md`. Remove the contradictions only. Do NOT
  broaden this into an import-repair system, and before editing, check whether any
  of the three is a live mechanism rather than documentation; if one is, inspect
  that distinction instead of replacing words.
- Live cell: `lookup ["Cmd.CommandResult","Cmd.RunResult","Tidepool.Command"]`.

## Wave 4 — a few harness-specific errors name the next valid operation

Recognized cases only, chosen from transcript counts; rendered beside the GHC
text by `render_cell_compile_error` (`tidepool-runtime::session`).

- `Cmd.stdout`/`Cmd.stderr` applied to `Cmd.CommandResult` → name the pattern:
  capture the job, `Cmd.readStdout job`, handle `Left`.
- `[Char]` vs `Text` at an argument of a stdlib function → say the stdlib is
  `Text`-first and string literals already are `Text`.
- **Source order works**: split a cell into units in source order so a
  declaration after a statement sees its bindings
  (`tidepool-runtime::session::workbench` owns sequencing and classification).
  Stage it: first land the pre-GHC detection (a declaration's free names
  intersect a same-cell statement binder → targeted rejection), which also
  catches silent mis-resolution such as `previews` binding to Control.Lens's;
  then the source-order execution as the marked experiment. Kill criterion: more
  than one extra compile per cell on the median, measured by Wave 2 spans.
- Pinned by `tidepool-mcp/tests/pragma_set_consistency.rs` and the
  `DECL_TEMPLATE_SOURCE` tests; run those.

## Wave 5 — primitives (Haskell stdlib + handlers)

- `Cmd.readStderr` beside `Cmd.readStdout` (`haskell/lib/Tidepool/Command.hs:235`),
  and one call returning both complete streams with outcome and cleanup, valid
  for failed commands — replacing the `captureStream` recursion in
  `~/dev/tidepool-astra/.shoal/discoveries/run-ahead.hs`.

  Astra's constraint on its result type, which decides the design: *"A convenient
  call must not turn incomplete capture into apparently complete strings."* The
  value must still express a stream-specific capture failure or retention gap,
  carry the originating job, keep outcome and cleanup separate from any semantic
  judgment, and distinguish complete evidence from a display excerpt. So the
  result is not a pair of `Text`; the paging protocol's loss signals
  (`outputLostBytes`, `outputLossy`, `outputFinished`) must survive into it.
- Guarded text replace effect (compare-and-swap on prior content). Check
  `Tidepool.Patch` / `Tidepool.QQ.Patch` first; extend rather than add.
- `Text`-returning show in `Tidepool.Prelude` (check for an existing name first).
- Extractor/stdlib change: follow `haskell/CLAUDE.md`; `just fixtures-check`.

## Wave 6 — the cheap path crosses into a program for free

- Every direct shell tool call leaves a retained `jobN` in Haskell scope, named in
  the tool result; make the existing partial behaviour uniform
  (`tidepool/src/host_dynamic_tools.rs`, command jobs in `tidepool-actor`).
- Notices carry attempt identity and the retained handle; mark a job superseded
  only where ownership data already knows it.
- **What-is-live view** in the `status` tool (`tidepool-actor/src/status_tool.rs`):
  collectors → jobs they watch → finished or not; bindings with defining cell
  execution id; binary build revision vs worktree head and dirty files; frozen
  workspace modules that differ from disk. A view over actor lineage, command
  jobs and workbench records — no new registry.
- **Binding source as a value** (bet): exact source text, execution id, and
  unresolved same-session dependencies for a named binding, from the cell source
  the workbench already keeps (`workbench.rs:140-186`). Kill criterion: not used
  unprompted in the next flight.
- **Compact default rendering**: do not print a result and an identical final
  state twice; bounded by default with obvious expansion.

## Wave 7 — Shoal children as cheap as native subagents

Investigate first with Wave 2 spans: measure a read-only child from `R.start` to
first useful tool call (the one Shoal child lived 7m45s; native subagents started
in 5-8 s). Then cut ceremony for the read-only errand: one call that takes a task
and returns a reply, with no definition, client, state query or retirement to
write. Kill criterion: Astra still prefers native subagents in the next flight.

## Parallel track — jev-dsl at `f16f136` via `flake_sources` (own worktree)

1. **Upstream** — DONE as a proposal, awaiting review. Branch
   `proposal/generic-front` in `~/dev/jev-dsl`, worktree `~/dev/jev-dsl-generic`,
   commits `b33ed2c` and `d29f9e9`. Not merged; `main` untouched. Design is Sol's
   territory, so this is a proposal, not a landing.

   The shape, proven rather than argued:
   - `core/Jev/Front.hs` (new, in `jev-core`) is the operator front with the JSON
     type as a parameter. It must live in `jev-core` because that is the only
     library a consumer without aeson can depend on. It adds no dependency.
   - `src/Jev/Operators.hs` becomes pure specialisation — type aliases fixing the
     parameter to aeson's `Value`, plus `name = Front.name` under the signature it
     already had. No logic left in it.

   Measured cost, zero on all three criteria: `examples/Guard.hs`, `app/Main.hs`
   and every existing test are byte-identical, not one character changed; all 19
   rejection fixtures produce byte-identical GHC output; `:browse Jev.Operators`
   is unchanged modulo the module qualifier GHCi prints.

   **The finding that decides our facade's shape: a re-export-only facade does not
   work.** Aliasing the types while leaving the functions class-constrained leaves
   an unannotated top-level binding monomorphised with an unsolvable constraint
   (`Ambiguous type variable 'v0' ... prevents the constraint '(JsonValue v0)'`).
   It does not fire for `let`/`where` bindings, only top-level ones — which is
   exactly how a model writes in a notebook. `NoMonomorphismRestriction` does not
   fix it. The cure is to name each value-carrying verb at the fixed type: 9 type
   aliases plus 24 one-line specialisations, being exactly the exports whose type
   mentions the JSON parameter. Everything else re-exports unchanged.
2. **Tidepool**: delete `scripts/sync-jev-dsl.sh`, `haskell/lib/Jev/VENDORED`,
   the copied `Jev/Core*`, and the hand-ported `Jev/Operators.hs`. Keep
   `Jev.Tidepool` (the `JsonValue` instance) and `Jev.Host` (effect-backed
   session), and add the specialisation facade described in step 1, shipped as
   workspace source beside the pinned input.

   Size it honestly: the facade is roughly 80 lines, not three — the upstream
   proposal built and tested exactly this facade for a second JSON type to prove
   it. But they are 80 lines of signature and delegation, where drift surfaces as
   a compile error on the exact line, replacing a 335-line hand-port of real logic
   that can drift silently. That is the whole of the win and it should not be
   oversold as more.

   Two smaller things the proposal surfaced: the facade needs a second import of
   `Jev.Core` for constraints the generic front does not re-export, which widening
   that export list would fix but is a design call for the owner; and this step is
   blocked until the upstream proposal is reviewed and lands, since we must not
   pin a branch.
3. `examples/shoal-workspace`: `flake.nix` input `jev-dsl` (`flake = false`),
   `[haskell.flake_sources] jev-dsl = ["core","src"]`, modules list; mechanism
   already exists in `tidepool/src/shoal/workspace.rs:282-390`.
3a. **Port two real programs and fix what makes the ports awkward** — Astra's own
   alpha priority, and worth more than a showcase written to flatter the API.
   Starting points, corrected by Astra:
   - contextual output selection: `contextualViews` in
     `~/dev/tidepool-astra/.shoal/discoveries/attention-pilot.hs` together with
     its completion-handler consumer. NOT `select-useful-example.hs`, which is the
     older single-question selector; `example-for-use.hs` is its bundled successor
     if an example-selection port is also wanted.
   - run-ahead: `readAhead` in
     `~/dev/tidepool-astra/.shoal/discoveries/run-ahead.hs`, the independent
     relevance version. Do NOT port the older `Choice`-based `runAheadReads` as
     the recommended pattern. The complete check-event wrapper was live-session
     code and is not in the saved file.
3b. **Version identity, or agents will mix the surfaces.** The running Shoal
   surface and this revision differ materially, so ship an identified alpha
   version, a concise old-to-new migration table upstream beside the changelog,
   examples checked against that version, and a statement of which Shoal revision
   supports it. Our skills name the exact revision they were checked against.
4. Rewrite `prompts/shoal/docs/jev.md` and `shoal-jev/SKILL.md` from upstream
   `docs/authoring.md` and `examples/Guard.hs`; migrate about 38 old-API uses
   (`accept`, `onMany`, `Nil`, `pool`, `ref`) in the guide, skills, worked cells
   and `tidepool/src/actor_host/jev_tests.rs`. Every example is a tested cell.
   Carry Astra's two cautions verbatim in substance: a receipt is not a reason for
   strict; rules in Haskell, judgments in Jev.
5. Merge conflicts with Waves 3-4 land in the skills and `jev.md`; this track
   rebases last.

## The acceptance experiment (after Waves 1, 3 and 5)

Astra's smallest program that would survive one real failure and one real repair.
Astra writes it; we only have to make it possible. Run it on the failure that
actually happened in the flight — the focused lookup check exiting 101 with five
rustc errors.

    focused check fails
    → preserve both streams, outcome and cleanup
    → extract the relevant diagnostic blocks
    → fetch the enclosing test, its imports and a neighbouring invocation
    → recognize a supported local API mismatch
    → apply a guarded prepared correction
    → rerun that focused check once
    → retain the checked result, or return a precise unresolved receipt

The concrete correction is narrow: the new test used `dispatch` where the
surrounding policy uses `dispatch_boxed`, and invented a `tool_json` wrapper.
Jev's job is to establish that the neighbouring example expresses the same
intended invocation and result flow, not that the compiler mentioned a similar
name. Code owns exact preimages, permitted edit locations, execution and
exit-status checks. To exercise revision as well: start with a helper that
gathers evidence and hands the case back, then add that one recognized repair
branch and rerun on the same task without redesigning the controller.

What it would establish: one complete interruption absorbed, with a real failure,
a consequential action and verification. Not general repair ability, and not net
productivity.

Each wave is judged by whether it removes a step from this program that Astra
would otherwise hand-roll.

## Verification

Per wave: the named tests above via `just test-lib CRATE 'test(name)'` /
`just test-target CRATE SUITE 'test(name)'`, then one live cell through a Shoal
session driven by the proxy, reproducing the original failing cell from
`plans/jev-lab/observation-limit/` or the transcript. After Wave 2, every later
wave's live check is read back from the JSONL trace. One `just verify` at the
very end, on the user's word.

## Proposals for Astra to read

See the message accompanying this plan; the same text is to be saved as
`plans/jev-lab/astra-proposals-2026-09-18.md` once editing is allowed.
