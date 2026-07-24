# Spike spec — the golden path, thin and real (ONE opus agent)

You are building the S1 spike from `plans/harness-r0/TARGET.md` — read
that FIRST (all ⚖ rulings are final), then this. You are the single
agent for the whole spike (D7): coherence over speed. Root reviews at
the seams listed at the bottom. This is CONTRACT-DISCOVERY: everything
you build is keep-path (no stopgaps, no minimal-variants), but THIN —
one thread, nothing wide.

## ANTI-PATTERNS (before anything)

- NO handles/await in the Haskell agent surface (D1). Park-at-fork.
- NO model-elaboration on predicted dialog branches (D6): option-key
  submissions consume mechanically; the model is the exception handler
  for prose/out-of-flowchart only.
- NO full battery in your loop: targeted nextest over touched crates;
  battery ONCE at spike end (freeze gate).
- NO widening: >1 fan, `awaitAll`, uiOf, [form|], heap/meters/trace
  panes, policy ladder are all EXCLUDED (TARGET §3).
- NO reshaping merged contracts (Ui wire, Event enum, registry/resident
  APIs) without stopping to report — freeze candidates change only with
  root sign-off.
- Do NOT touch tidepool-repl, the eval server (tidepool-mcp paths), or
  the GC/rooting internals (segments 20/40 are done; build ON
  `run_child_fragment`/`ResidentSession::run_child`/`checkout_child`).
- The GUI is built RIGHT (D5): real layout/design, chosen libs, no
  placeholder styling "to fix later". Jank is more expensive than care.

## READ FIRST

TARGET.md · tidepool-runtime/src/session/resident.rs (esp. run_child,
reenter, ResidentOutcome) · tidepool-harness/src/{registry,forcing,log,
log/writer,log/reader,tree,ui,provider}.rs · tidepool-web/src/render.rs ·
tidepool-mcp/src/effect_defs.rs (ask_effect_def!) · the extract pass
(haskell/src/Tidepool/Translate.hs returnControl interception — landed
by the time you start) · plans/harness-r0/10-extract-pass/SPEC.md §MECHANISM.

## THE GOLDEN PATH (build exactly this thread)

boot `tidepool-harness` binary → operator signs in (OAuth verbs from
provider/oauth.rs wired to protocol) → observatory pane shell (tree ·
inspector/form · log tail) over SSE → operator creates root node
(thunk) + forces → turn engine drives the calling LLM → its eval calls
`returnControlFork @Verdict "…"` → thunk child node; operator forces →
child conversation = parent transcript forked at checkpoint + hole
card → child answers; ONE deliberate ill-typed attempt exercises the
GHC-verbatim retry → typed answer fills the fork; parent machine
resumes (it was parked at the fork) → program calls the Ui effect with
a small form (one Choice + prose escape) → renders in the form pane →
operator answers via option key → MECHANICAL consumption → program
completes → node_done → **kill -9 the binary** → restart → tree +
suspension state reconstructed from the log → a re-driven run in
record-replay mode reproduces the terminal state with zero live API
calls.

## COMPONENT BRIEFS

1. **Haskell verbs** (effect_defs.rs `ask_effect_def!` + extract):
   `returnControlFork :: Text -> M a` (parks; new AskWith-family
   constructor, same ask_tag) and the Ui effect verb (own effect verb
   family per D2 — one constructor taking a `Ui`-shaped Value, returns
   the submission Value; name it plainly, propose at review). Add
   `returnControlFork` to the extract interception list (same head-swap
   + sidecar mechanism as `returnControl` — the pass is already
   generic over the verb list).
2. **Turn engine** (tidepool-harness): conversation loop per node —
   prompt assembly (transcript prefix + system framing: "one tool:
   eval; answer holes by evaluating `resume expr`"; hole card = prompt
   + `Code` type sig from the sidecar), provider call, extract LAST
   fenced ```haskell block, run via ResidentSession (parent turns) or
   run_child (fork answerers), feed back rendered result or verbatim
   GHC error, loop; per-node turn cap (config, default small); Usage →
   log events.
3. **Transcript store**: turn DELTAS as log events (F2 draft: `turn`
   {node, role, content-delta-ref or inline, seq}); fork records
   {parent node, parent turn position}; reconstruction = fold. Draft
   the exact shapes — they freeze at F2 after you've used them.
4. **Scheduler (thin)**: one parent + one live child. Child evals run
   only while the parent machine is parked (it IS parked — fork parks).
   Registry states (Slot::RunningChild etc.) already exist.
5. **Record-replay provider**: a ModelProvider impl reading `turn`
   events back from a log; live mode logs, replay mode substitutes.
   ONE component — this is also E4's turn-replay. Wire the golden path
   through it as a test.
6. **Crash-replay**: on boot with an existing log: fold events →
   rebuild NodeTree + re-drive sessions to their suspension points
   (eval sources re-run with effect-response substitution from the
   log; the effect req/resp events are already in the schema). Holes
   re-publish. Divergence → demote node to browsable, loudly.
7. **Protocol + pane shell** (tidepool-web): axum; SSE = log tail
   (reader follow mode exists); verbs: force, answer (typed-hole +
   form submission), cancel, auth/start, auth/status; loopback bind.
   Panes server-rendered (maud + render.rs), Datastar-patched, fixed
   layout, real styling — pick a lean CSS approach deliberately
   (vendored, no CDN) and state your choice in the report.
8. **Form submission encoding** (F1 draft): {values: {key: …},
   prose: Text (may be empty)}. Empty prose + known key → mechanical.
   Non-empty prose or unknown shape → route to the calling model as
   elaborator, show-before-consume. Draft, use, refine.

## SEAMS — stop and report to root at each

(a) turn-engine first round-trip (live model writes an eval, it runs);
(b) first fork answered end-to-end incl. the GHC-retry;
(c) pane shell up with the form answer flow;
(d) crash-replay green;
(e) any moment a merged contract needs changing.

## VERIFY (targeted; battery once at the end)

cargo nextest per touched crate as you go; the record-replay golden
path as a CI-shaped test (no live API); GHC-tier resident/nested tests
(`-E 'binary(resident_session)'` with the dev extract per repo
CLAUDE.md); full `scripts/battery.sh` once, at spike end.

## DONE

The golden path runs live with you driving a real model; the operator
(Inanna) can then drive it AD-HOC (D4) — her session, her steering;
kill-9 restore works; record-replay reproduces it in CI; freeze-draft
notes (F1/F2/F3 exact shapes as-used + what you'd change) in your
final report.
