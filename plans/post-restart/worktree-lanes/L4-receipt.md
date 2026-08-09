# L4 receipt — authored surface + the `withHandler` interpreter

STATUS: IN PROGRESS. The mechanism decision and the scaffold are landed; the
handler modules and the acceptance harness are in flight in two forked lanes.
Sections marked TODO are filled at convergence.

## 1. The mechanism decision

Full write-up, verification record, and rejected alternatives:
[`L4-mechanism.md`](L4-mechanism.md). Summary:

**Decided: `withHandler` is a scoped INTERPOSITION over the body's freer-simple
structure.** `Eff` is freer-simple's free monad, so the scope walks the
computation it encloses and runs a drain before every effect that computation
performs. The author's handler closure is applied by ordinary Haskell
application, inside the resident's own continuation. No closure crosses to
Rust, none is rooted by Rust, no second continuation is created per invocation,
and **`tidepool-codegen` is untouched by this lane** — so no root notification
for a codegen change was needed, because there is no codegen change.

Both scaffolding findings were verified independently rather than taken on
faith, and both hold:

1. `run_fragment_suspendable_parked` takes a `FuncId` and there is no public
   closure-apply on the parked path — confirmed by enumerating every public run
   entry on `JitEffectMachine`. Additionally, and not previously noted: the
   fragment entries are **nullary**, so even a hand-compiled dispatcher `FuncId`
   could not be handed an observation as an argument. That kills the
   dispatcher-table alternative on a hard fact rather than on preference.
2. No production consumer of the parked path exists — a repo-wide grep finds
   hits in exactly six files, all `tidepool-codegen/tests/realm_*.rs`. This
   lane's acceptance harness is the seventh caller and the first outside
   `tidepool-codegen`.

Rejected: growing `tidepool-codegen` with a closure-application entry point
(pre-authorized, and genuinely not needed — it would add a lifetime-long GC
rooting obligation on the hardest surface in the system to buy nothing);
a top-level dispatcher `FuncId` reading from a closure table (nullary fragments,
plus the same rooting obligation); driver-resumes-the-resident-into-the-handler
(would require every effect's answer type to become a sum carrying a possible
handler invocation — a token threaded through the whole row, which is exactly
the DSL-shrinking move the design stance forbids).

### Verification record

Four probes against the live JIT, run BEFORE anything was built on the answer:

| Probe | Result |
|---|---|
| Walk over a 2-effect body | tick fired exactly twice, body returned `7` — the existential GADT match over `Eff` and the type-aligned `Arrs` re-queue both compile and run |
| Nested scopes, effectful inner tick | `B B A B B B A B` — outer scope interleaves around the inner scope's own effects, i.e. exactly "separate handlers interleave only at suspension points" |
| Failing handler | eval aborted with the handler's error; the statement after `withHandler` never ran |
| Handler containing a real `ask` | genuine suspend, genuine resume, handler continued past its own suspension, body completed |

The fourth is the load-bearing one: "the handler may itself suspend" was the
semantic most likely to force a runtime change, and it costs nothing here
precisely because there is no separate handler continuation to reconcile.

## 2. What landed

Commit `0630b6bd` — scaffold:

- `worktree_effect_def!` and `event_effect_def!` (`tidepool-mcp/src/effect_defs.rs`)
- their decl projections (`tidepool-mcp/src/effect_decls.rs`)
- PRD 19 wire types (`tidepool-bridge-effects/src/lib.rs`)
- `Control.Monad.Freer.Internal` in the generated module's imports
  (`tidepool-mcp/src/eval_prep.rs`)

TODO: handler modules, acceptance harness, per-binary test counts.

## 3. Test name proving each `withHandler` semantic

Every semantic below is a ONE-FAILURE-MODE GATE, so per root's standing receipt
rule each is its own named test with its own pass line — an aggregate count is
not sufficient evidence. The reason is concrete and worth restating where the
evidence lives: a rename, an `#[ignore]`, a `cfg`, or an env-gated early return
leaves an aggregate green while the guard never executed. "N/N passed" cannot
distinguish "the guard ran and held" from "the guard silently stopped
existing"; only a named pass line can.

Pass lines are produced with
`cargo nextest run -E 'test(=name_a) + test(=name_b) + ...'` and reported here
verbatim, together with the BASE COMMIT the run happened on — base proves the
tree, name proves execution, and neither establishes anything alone.

| Semantic | Test | Status |
|---|---|---|
| A subscription never replays rows older than itself | TODO | TODO |
| Events broadcast to all registered handlers | TODO | TODO |
| One handler at a time; later matches queue in observation order | TODO | TODO |
| Lexical drain, then unregister, at body end | TODO | TODO |
| Handler failure fails the enclosing scope | TODO | TODO |
| Bounded-queue overflow fails loudly | TODO | TODO |
| Rooting receipt `stowed_roots_count() == parked_count()` | TODO | TODO |

The rooting receipt gets BOTH treatments deliberately: it is asserted at every
quiescent point inside every acceptance test (cheap, and catches drift wherever
it happens), AND it has one dedicated named test whose whole purpose is that
equality under a deliberate park/resume interleaving. The per-test assertions
alone are exactly the buried-assertion shape this rule cannot certify.

Base commit for the runs: TODO.

## 4. Shared files touched, for the fold's conflict log

| File | How |
|---|---|
| `tidepool-mcp/src/effect_defs.rs` | ADDITIVE — two new `*_effect_def!` macros appended before the `#[cfg(test)]` module. No existing macro touched. Agent-wave may also be appending effect definitions here; per the conflict experiment this was not pre-partitioned. |
| `tidepool-mcp/src/effect_decls.rs` | ADDITIVE — two `*_effect_def!(effect_decl_projection)` lines appended at end. |
| `tidepool-bridge-effects/src/lib.rs` | ADDITIVE — a PRD 19 section appended before `bridged_records_module()`. The existing six records and that function are untouched. |
| `tidepool-mcp/src/eval_prep.rs` | **The one edit INSIDE an existing function** (`effects_module_source_at`): one added import line, `Control.Monad.Freer.Internal (Eff(..), qApp, tsingleton)`. `Eff` is the same type already re-exported by `Control.Monad.Freer`, so this adds constructors and the queue operations and shadows nothing. Flagged here because it is the only non-append in the lane. |
| `tidepool-handlers/src/handlers/mod.rs` | ADDITIVE — two `pub mod` + two `pub use` lines, from the two forked lanes. |

Deliberately NOT touched: `tidepool-codegen/**` (the mechanism needs no change
there, and `resident.rs` pending/`ChildSuspended` is a hard hold),
`tidepool-harness` observability/error files, `tidepool-worktree/**`,
`harness-dogfooding/dev-tree/Harness.hs`. The Worktree and RepoEvent effects
were deliberately NOT added to `build_base_stack`/`base_effects!`/`handler_for!`
— they are out of the default server row until the dogfood lands, so no
positional union-tag slot was claimed.

## 5. HOLD lines, and one I came close to

**Came close: the `(<|>)` collision — held the DSL, did not shrink it.**
`Tidepool.Prelude` already exports `(<|>)` from `Control.Applicative`, and the
frozen `Tidepool.Event` requires `Tidepool.Effects` to export its own
`Event`-merge `(<|>)`. Author code importing both hits an ambiguous occurrence,
and GHC cannot disambiguate by type. The tempting fixes were all DSL edits —
rename the operator, or fake an `Applicative`/`Alternative` instance for `Event`
so the Prelude's operator covers it. I took neither: `Tidepool.Event` is frozen
and the PRD says the operator "need not fake a general `Applicative` instance".
So the operator is implemented as written and the collision is reported rather
than papered over. **If this bites the dev-tree dogfood it is a root
escalation**, not something for a lane to settle quietly.

**Not crossed:** `workspaceOf`, any `Workspace` type, and any coupling to an
agent handle remain absent. `harness-dogfooding/dev-tree/Harness.hs` is
unedited. The dev-tree dogfood compile (PRD acceptance 9) was not attempted.

**Naming divergence, deliberate and worth a reviewer's eye.**
`WorktreeReceipt`'s id field is `treeId`, not the PRD snippet's `worktreeId`.
The PRD's own public surface also pins `worktreeId :: WorktreeHandle ->
WorktreeId` as a standalone function, and a record selector plus a top-level
function of the same name is an ambiguous occurrence at the export. The
signature the PRD pins won; the illustrative field name yielded. Access is
`r.treeId`, per the record-dot rule.

## 6. Corrections absorbed from root mid-flight

- **Poke semantics — my spec's version was stale and is now reversed.** I was
  told `dev-tree/Harness.hs` diverged because it "treats a poke as if delivery
  were guaranteed where the PRD has since made pokes fire-and-forget". That is
  backwards. PRD 18's revision makes `pokeAgent` a DURABLE PER-AGENT QUEUE: a
  poke is accepted, stays queued until deliverable, is never silently discarded,
  and delivery to an idle agent starts or queues a follow-up turn. So a
  `headChanged` handler that pokes and returns is CORRECT, and needs no
  error-handling choreography for unsteerable agents. Harness.hs's shape is
  closer to right than my spec said. It does still diverge, differently:
  `sendMessage` and `followupTask` no longer exist as separate operations (both
  are `pokeAgent`), so its `pokeAgent = sendMessage` is wrong for that reason
  instead. The `Observed`/`payload`-vs-`value` divergence stands. The file
  remains read-only to this lane either way. **Nothing in this lane's code
  depended on the stale fact** — pokes are PRD 18 surface, not L4's.

- **Cycle shape — reinforces the constraint, changes the surrounding picture.**
  Agents may now continue running between resident cycles, and their identities
  and the resident's plan are ordinary checkpointed data. What still never
  crosses a cycle boundary is unchanged: an attached Haskell handle, a parked
  Haskell continuation, or an event subscription. A subscription is per-cycle,
  full stop. Nothing in this design leans on a subscription outliving its cycle
  or on the unfold/fold completing inside one — `withHandler` is lexically
  scoped and its registry lives and dies with the handler stack for the cycle.

  The change does put weight on one distinction that would be easy to implement
  away, so it is recorded as a design constraint rather than left implicit:
  **"no replay" is a rule about JOURNAL ROWS, not about observations.** Because
  re-registering from `State` each cycle is now a real repeated path rather than
  a recovery story, there is a real window between cycles with running agents
  and no subscribers. If the monitor's baseline were process memory, the first
  reconcile of a new cycle would conclude nothing moved and every commit in that
  window would vanish — breaking "commits are never silently dropped" in the
  harder-to-notice direction. The monitor's own contract already says the right
  thing (`tidepool-worktree/src/monitor.rs`: the restart baseline is the last
  JOURNALLED observation, never an in-memory one). With that, a gap movement
  surfaces in the new cycle as a genuinely new observation rather than a
  replayed row, and both rules hold at once for the same reason: the journal is
  the durable baseline, the queue is per-subscription. The event lane was told
  to state explicitly which baseline its adapter uses.
