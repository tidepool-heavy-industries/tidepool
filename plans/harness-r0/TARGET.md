# TARGET — the full harness buildout (calendar phases dissolved)

Replaces the R0/R1 split. Scope-denominated per the 2026-07-23 replan:
spike-then-freeze-then-widen. This document is the full design; decision
points are marked `⚖ Dn` with the operator's ruling recorded inline once
made. Mechanism claims cite what is merged on main.

## 1. The system (end-state)

A calling LLM drives a resident tidepool session. Its programs can, via
ONE suspension mechanism (the Ask/hole machinery, merged) with THREE
routings:

- `returnControl @T prompt` — suspend; the calling LLM answers **in its
  live context** by evaluating `resume expr :: T`.
- `returnControlFork @T prompt` (⚖ D1 RULED 2026-07-23) — **parks**: the
  harness registers a THUNK child node (consent gates apply), forks the
  caller's TRANSCRIPT at the checkpoint into a fresh context window, and
  the program suspends until the fork's typed answer arrives. A batch
  form (`returnControlFanout @T [prompts] → M [T]`, name at freeze)
  parks once over N parallel forks — parallelism is N inference streams,
  not N heaps; child EVALS serialize on the parent's machine (GC-rooted
  nested runs, merged). RULING: handles/await are NOT the agent API —
  they may exist at the impl level, but the agent-facing surface is
  park-at-fork plus, in widen, RECURSION-SCHEME combinators (parallel
  cata/ana whose per-node judgment is a forked answerer — the LspGraph
  idiom: scheme traverses, forks judge) built over the same machinery.
  Applicative fanout = later speed optimization, no hard need.
- The operator dialog surface (⚖ D2 RULED) is its OWN effect, NOT part
  of the return family: `Ui`-valued elicitation as a normal effect verb
  (name at freeze; mechanically it is still a hole with operator
  routing). Interception stays symmetric: the operator may answer any
  hole.

Answer validation is GHC end-to-end: ill-typed/bottomed answers do not
consume the continuation (merged semantics + segment 40's NF-force); the
compiler error is the retry prompt, verbatim.

Haskell holds opaque ids and typed values ONLY. Conversations, forking,
scheduling, budgets, consent live harness-side. Fork is a
conversation-plane operation; that is why it is cheap.

## 2. Component inventory

| layer | component | state |
|---|---|---|
| engine | ResidentSession / SessionRegistry / fragment-suspend | **merged** |
| engine | GC-rooted nested child runs + NF-force | **merged** |
| extract | typed-yield site capture | in flight; fork verbs join the same interception list post-merge |
| haskell | `Ui` eDSL module | **merged** |
| haskell | fork/await/dialogAsk verbs | unbuilt (extend Ask effect — no new union slot; root-decided §6) |
| harness | event log writer/reader | **merged** |
| harness | NodeTree/forcing/consent | **merged** |
| harness | provider clients (genai + openai-auth) | **merged** |
| harness | **turn engine** (conversation driver: prompt assembly, eval-block extraction, GHC-error retry, budget caps) | unbuilt — spike |
| harness | **transcript store** (conversation tree as log-folded turn deltas; fork = ref to parent position) (⚖ D3) | unbuilt — spike |
| harness | **scheduler** (N conversations ↔ one machine) | unbuilt — spike-thin, widen-hard |
| harness | record-replay provider = E4 replay (one component: turns are log events; CI reads them back) | unbuilt — spike |
| web | protocol (SSE + verbs incl. auth/start) | unbuilt — spike-thin |
| web | pane shell (⚖ D5 RULED: fixed layout, server-side layout state; GUI built RIGHT — proper libs/design system, no half-assing; jank is more expensive to debug than to avoid) | unbuilt — spike-thin (3 panes), widen-full |
| web | `Ui`→Datastar renderer | **merged** |
| web | forms full path (`uiOf` server-derived, `[form|]`, mechanical answers) (⚖ D6) | widen |
| accept | golden path in CI (record-replay), full battery at freeze gates | widen |

## 3. Spike (S1) — the golden path, thin and real (⚖ D4/D7 RULED: full
path scope confirmed; validation is OPERATOR-DRIVEN AD-HOC use, not a
rigid script — record-replay captures ride along opportunistically; ONE
opus agent builds it, root reviews at the seams)

One thread through final components, nothing wide:

boot binary → sign in → operator creates root node (thunk) → forces it →
turn engine drives the calling LLM → its eval `fork @Verdict` (n=1) →
thunk child appears, operator forces → child = transcript-fork answers;
one deliberate ill-typed attempt proves the GHC-retry loop → parent
`await` resumes with the value → parent `dialogAsk` with a small `Ui` →
operator answers in the pane form → program completes → **kill -9** →
restart → tree restored from log, replayed turns → same terminal state.

Every event logged; the record-replay provider re-drives the whole path
in CI with zero live API calls. Spike explicitly EXCLUDES: >1 fan,
`awaitAll`, uiOf, [form|], mechanical form answers, heap/meters/trace
panes, policy ladder.

## 4. Freeze gates (S2) — the only expensive artifacts

Frozen immediately after the spike touches reality; each freeze is an
operator+root review (opus pre-brief allowed):

- **F1 UI wire contract**: the `Ui` JSON (draft merged:
  `wire_shape_is_stable`) + the ANSWER encoding (widget values keyed by
  option/field + always-present prose channel) + pane-fragment envelope.
- **F2 log schema**: current Event enum + turn events (message DELTAS;
  transcripts reconstruct by folding; fork references parent position) +
  reserved kinds: `turn_spliced` (operator interjection into a child
  conversation), memo reservation. Header pins unchanged.
- **F3 fork surface**: the three verbs + `Branch` + fork-site type
  capture + child capability rule (§6) + the effect-row filtering
  wrapper mechanism (cheap DispatchEffect allowlist — designed, ships
  when policy needs it).

## 5. Form answers (operator → typed value) — ⚖ D6 RULED (inverted)

MECHANICAL-FIRST. The authoring model writes the dialog anticipating a
flowchart of likely branches: structured selections on predicted paths
consume MECHANICALLY (option key → value, handled in the program's own
Haskell continuation or by direct construction — zero model turns). The
model is invoked ONLY for unexpected/less-expected branches: prose
answers, out-of-flowchart submissions — that is the fallback
interpreter's whole job, not the default path. Prose-primacy still
holds when prose IS present (it wins over a conflicting widget, shown
before consume). F1 freezes the submission encoding (widget values +
always-present prose channel); the elaboration path is the exception
handler, not the pipeline.

## 6. Root-decided (recorded; flag to reopen)

- Fork verbs EXTEND the Ask effect (new constructors, same ask_tag
  interception, no new union slot).
- Child answerers inherit the parent's full effect row (same trust
  domain — the fork IS the calling agent); narrowing = the F3 filtering
  wrapper, config-side, later.
- `uiOf` derives forms SERVER-side from the DataConTable (flLabel is
  captured) — no Haskell Generic machinery, no fluency tax.
- Scheduler contention rule: child evals run only while the machine is
  parked or between parent turns; inference-bound fan sizes make
  starvation a non-issue, revisit with meters data.
- ApplicativeDo/FreeAp: NOT built. Handles+await already deliver
  heterogeneous parallel fan with monadic syntax; applicative sugar is
  an F3-time addition only if spike experience shows the fork/await
  spelling is a fluency tax.

## 7. Held — by constraint class, not calendar

- Parallel DIVERGENT fork (heap copy): verification-bound. Gate = GC
  design earning adversarial review. Sequential-isolated is the shipped
  semantics.
- Policy-ladder rungs, prompt tuning, calibration: experience-bound.
  Config surface ships (autoForce=never IS the t0 design); rungs get set
  by dogfood data. Instrument from session one: usage, retry counts,
  forcing latencies, hole-open durations — all already log events.
- Memos: operator deferral; log-schema reservation only.

## 8. Staffing & economics

Spike: ONE opus agent (coherence beats speed inside contract-discovery)
(⚖ D7), root reviews at the seams. Freezes: operator+root judgment — the
scarce resource, spent deliberately. Widen: sonnet fan-out behind frozen
contracts (≈8–12 leaves: panes, forms, uiOf, [form|], scheduler
hardening, elaboration flow, meters/trace/heap, acceptance, protocol
polish, run-by-hand deployment glue). GHC suite: merge-gate +
freeze-gate, never inner-loop. TESTING POLICY (operator, 2026-07-23):
leaf verify sections use TARGETED nextest filters over the touched
crates/tests — never full battery per subagent; full battery runs at
freeze gates and root-level engine merges only.
