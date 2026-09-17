# Jev dogfood notes (2026-09-17)

Sol (low effort) roots Shoal in `~/dev/jev-dsl` and exercises the `Jev`
effect. Each run ends with an operator interview; the run's own log is
`~/dev/jev-dsl/.shoal/dogfood-notes.md`. This file records what was fixed and
what remains open.

## Run 1 (f1371f51)

Fixed after the run:

- Children spawned through the Haskell role plans (`ResearchEffects`,
  `CodingEffects`, ...) lacked `Jev`; the rows in
  `haskell/actors/Tidepool/Actors/Role.hs` now carry it.
- A notebook binding whose inferred type mentions jev-dsl internals failed to
  retain (`Not in scope: type constructor Jev.Core.Schema.Offer`). The
  workbench now imports the `Jev.Core*` modules qualified.
- `:=` and `:&` are in scope unqualified; `doc jev` says which names need `J.`.
- `exec_command`/`write_stdin`/`read_output`/`cancel_command` with an
  out-of-range `yield_time_ms` or `max_output_bytes` now answer with a text
  rejection instead of failing the cell with a bare runtime error. A refused
  command start (a research actor) says so in text.
- Socket directories are released once the process and hosted work are
  confirmed settled. The workspace retire error names its failing step, and
  the worktree-binding receipt no longer blames tmux.

Open:

- `nix develop` in an actor worktree failed with "Path 'flake.nix' ... is not
  tracked by Git" although the file is committed; `nix-shell` worked.
- A `String` cell result was shown as a list of characters once
  (`['L','e','f','t',...]`); not reproduced yet.
- Jev usage was not visible to the model without calling `J.usage`.

## Run 2 (634a580a)

Jev worked in research and coding children; the coding child's docs branch
passed `check.sh`.

Fixed after the run:

- `error` messages reach the notebook on the prepared route. After a call
  fails by raising, the exception is forced under a bounded budget and the
  failure becomes `RaisedExceptionMessage` with its text
  (`forcing::describe_raised_exception`); the corpus oracle treats it as a
  raise.
- Child retirement no longer fails `BuildResource` with ENOENT: the mount
  helper re-execs this binary by descriptor (`execveat`), because the
  retired view's `/proc/self` does not resolve.
- Research (inspection-only) actors can start commands. Their project view
  is mounted read-only; before, every command start was refused and they
  had no way to read files.
- `doc jev` has a complete pooled-packet cell (pool, `eachIn`/`askAbout`,
  `given`, `accept` projected with `selectedKey`), compiled by
  `doc_jev_pool_example_sends_one_request`, and explains the `let` layout
  rule behind run 1's `:&` parse error.

Open:

- `stopAgent` answers `StoppedNow` before cleanup settles; degraded cleanup
  arrives later as a notice with no handle to await or inspect.
- Evidence transcription (cells, commands, errors, Jev scores) into the log
  was the third-largest time cost; a notebook helper that appends a cell's
  source and result to a log would remove it.
- `spawnWatched` takes one child; launching a pair meant switching to
  `unfold` plus an applicative `watch`.
- Jev reported mass/confidence 1.0 for a subjective documentation choice.
- `documentation_tests::workspace_lead_repairs_locally_...` and
  `candidate_workspace_runs_its_own_model_free_recipes` fail on recipe type
  drift (`GitOid` vs `GitRef`, the `coding` signature); unrelated to this
  work and not yet checked against `main`.

## Run 3 (3ad454f7)

A research child triaged a test gap with Jev and read files through shell
commands; the root gated the hand-off with `J.accept`; a coding child added
`refKey`/`refPayload` coverage on a branch with `check.sh` green. A typed
`error` cell showed its message. The research child's cleanup settled.

Fixed after the run:

- Retirement failed with "Git operation active during retirement" when the
  root read the child's branch at the same moment; the capture now waits up
  to 60 s for running host Git commands (`GitCli::capture_within`).
- `stopAgent` and each cleanup stop step wait for the host's release receipt
  (`ad4028072`). `StoppedNow` is final; `StoppedRetaining`/`StoppedReleasing`
  say otherwise, and only the latter is followed by a notice. The receipt
  separates "actor is stopped" from the retained components.
- `planCleanupFor` plans from a `Response`; the `doc cleanup` example had the
  exact type error the root hit. `doc unfold` shows two children with `<*>`
  and one watch over both.
- The display-failure notice no longer prints `()` as the value's type.
- `doc jev`: Cost (Jev is cheap, be Jev-dense), Calibration (a 1.0 mass with
  one contender means an under-specified pool) and a two-cell Jev-dense
  example compiled by a test.

Decided, not fixed:

- A bare `error "..."` cell is fully polymorphic; GHC cannot classify it as
  pure or effectful and defaulting does not apply to a custom class. `doc
  workbench` says to annotate it. Only `error`/`undefined` have that type.
- A cumulative Jev usage counter: dropped. Jev is cheap enough that the
  model should not meter it; `J.usage` per response remains.
- `J.explain` (policy rationale): a DSL addition, so it is a run-4 task for
  Sol, who owns the DSL design.

Open:

- Jev reports mass and confidence 1.0 for subjective prioritisation
  (provider-side; the doc pattern mitigates).
- Wall-clock is dominated by children's full `nix-shell --run ./check.sh`.
- `typed_reply_settles_response_and_wakes_registered_watch` fails on
  `EchoReport cache` vs `EchoReport "cache"`: a Text inside a Show'd record
  renders unquoted. Pre-existing; likely the same cause as run 1's
  character-list display.

## Run 4 (4ee4280b)

Sol low root, Luna children (`withModel (Literal "gpt-5.6-luna")`). Task:
review-and-merge the three branches from runs 1-3 into `shoal/integration`
through fresh Luna reviewers, a `J.accept` gate and `tryMerge`; the run was
stopped after that task to save quota (the polish and DSL tasks were the
wrong tier for Sol). Log: `~/dev/jev-dsl/.shoal/dogfood-notes-run4.md`;
observer notes: `~/.claude/jobs/4940a626/tmp/observer-run4.md`.

Verified: six stops, all `AgentStoppedNow`, no later release notice, final
roster root-only; `planCleanupFor`/`executeCleanup` on three groups; three
children admitted in one applicative `unfold` with one `watch`; `tryMerge`
fast-forward and two merge commits; a Luna checker's `check.sh` green on the
integrated revision. The Jev gate refused the reviewers' compact summaries
(confidence 0.26 to 0.34) and accepted the evidence packets (0.67 to 0.89):
gate on the artifact, not the narration.

Open, by time cost:

- The root's `boundWorktree` returned `Left (StorageFailure ".../worktrees/
  wt-02a87edd-.../.git" "mounted worktree ... requires filesystem
  recovery")`. That worktree id is run 3's coding child, whose retirement
  had failed; a stale retained worktree from an earlier run reached the
  next run's root binding. Investigate custody carry-over across runs.
- A worktree the root allocated with `createWorktree` is read-only to the
  root's own commands (`dist-newstyle: createDirectory: permission denied
  (Read-only file system)`), so the root could not run `check.sh` on the
  integrated revision and spent 13 minutes on a Luna checker instead.
  Decide whether root-allocated integration worktrees should be writable.
- Inferred selector declarations (`firstReview = \(a,_,_) -> a` without a
  signature) fail in the generated wrapper with `Not in scope: type
  constructor or class GHC.Types.ZonkAny`. Same family as run 1's
  `Jev.Core.Schema` import gap: an inferred type the wrapper cannot name.
  Either import it or reject the declaration with a message that says to
  add a signature.
- A cell that bound six 220-line file previews failed with `observation
  budget 100000 exhausted` before its Jev call. `doc jev` now says to bind
  short previews; consider not observing bindings the cell never displays.
- `listAgents` renders a full roster record per actor, usage observations
  included; five actors fill a screen. A compact default view.
- A watch forgotten by cleanup still delivered a `Pending → Ready` notice
  afterwards; `pollWatch` then answered `WatchUnavailable (WatchRejected
  ReplyStale)`. Suppress notices for forgotten watches.
- Sol's asks: `tryMerge` targeting a named branch directly; typed tuple
  selectors for multi-child responses; an `explain` for `J.accept`
  (handed to the DSL owner); a `reviewGate` helper bundling policy, packet
  and rationale.

Next run moves to `~/dev/shoal-evals/tui-test-app` (small Rust TUI, five
stub modules, seconds-fast `check.sh`): the orchestration loop is the object
of study, not the code.

## Run 5 (04c058ee, tui-test-app)

Sol low root, Luna children, three waves over `TASKS.md`. The whole loop
ran end to end in about 17 minutes: five implementers admitted in one
`unfold` (settled in 4m50s, four of five clean on the first pass), five
fresh reviewers (all Accept in 75 s), Jev risk-ranked `tryMerge`s into
`shoal/integration`, `check.sh` green on the integrated revision (verified
by the operator: 18 of 18 tests), ten `AgentStoppedNow` stops with complete
receipts. `main` untouched. Log: `~/dev/shoal-evals/tui-test-app/.shoal/
dogfood-notes-run5.md`; observer: `~/.claude/jobs/4940a626/tmp/observer-run5.md`.

Jev, 23 calls, all from the root:

- One packet classified all five children's test outputs (saved four model
  turns by Sol's count).
- The reflex over the integrated `check.sh` output classified `pass` at 0.98.
- `sufficient diff testOutput` as a bare Noul scored 0.47 to 0.54 on every
  candidate: a vague judgment question, uninformative by construction. The
  measured rule applies: ask condition-descriptive questions over named
  fields, not "is this sufficient".
- The artifact-only `J.accept` merge gate (0.70 / 0.40 / 0.85) accepted two
  candidates and returned `Unconfident` for list 0.75, help 0.82, store
  0.44. Sol inspected those diffs itself and merged on the reviewers'
  verdicts; nothing broke. The two near the floor argue for `spawning`-level
  confidence on a reviewed, test-passing diff; store at 0.44 is the case the
  gate exists for and deserved a repair request or a second reviewer.
- The risk `Score` for store came back "medium" with confidence 0.0; check
  how a Score's confidence is derived when the mass is flat.

Friction, new:

- The root cannot read children's worktrees: `cargo test` from the
  `WorktreeReceipt.cwd` paths failed with `could not find Cargo.toml`. By
  construction (each actor sees its own mount); the docs should say that
  artifacts travel in typed replies, which is what worked.
- `Cmd.withArguments [commit, "--", file] [bash|git show --format=|]`: the
  arguments became shell positionals, not git arguments; `"$1" -- "$2"` in
  the quasiquote fixed it. A `doc` line on how `withArguments` binds.
- Inferred `Text` where `Label` was required in the multi-child cell;
  `Ambiguous type variable parent0 ... Contains Replies parent0` when
  wrapping `child` in a helper; `Overlapping instances for Render (f0
  Double)` on an ambiguous numeric in a displayed tuple (twice); a
  `Cmd.OutputIssue -> String` vs `Text` slip. All "unresolved type variable
  meets the wrapper" or String/Text: candidates for one `doc workbench`
  paragraph and for defaulting in the cell templates.
- The child-side `respond` signature took one child three tries (an extra
  first argument, then a `Reply` wrapper).
- No `doc` topic for record actors, so that pattern was skipped: the
  skills gap from wave 2.
- Sol wrote its log in three batches instead of incrementally; the
  operator survey is the reliable record.

## Run 6 (31b38b4f, tui-test-app, after wave 2)

Sol low root, Luna children, three waves over five new tasks on top of
run 5's `shoal/integration`. Every task merged; `check.sh` green on the
integrated revision (33 tests); `main` untouched at the scaffold; fourteen
`AgentStoppedNow` stops with complete receipts. Wave 1 ended at +17m11s
(Task 1 alone, one repair round, then four children in one `unfold`),
wave 2 at +35m43s (five fresh reviewers, two repairs, two re-reviews, four
`tryMerge`s), wave 3 under a minute. Log:
`~/dev/shoal-evals/tui-test-app/.shoal/dogfood-notes-run6.md` (with the
survey and the interview appended); observer:
`~/.claude/jobs/4940a626/tmp/observer-run6.md`; pane transcript
`~/.claude/jobs/4940a626/tmp/pane-run6-full.txt`.

What the wave-2 primitives bought, in Sol's own scoring:

- Parent-derived artifacts: `responseWorktree` OID, then `git show` /
  `git diff --stat` / `git diff --numstat` in the root's checkout, with the
  coverage check (every stat file has a hunk, counts equal) done in awk
  before every packet. Saved model turns; used for all five candidates and
  all three repairs; never touched a child `cwd`.
- Root-allocated integration worktree: `createWorktree (fromRef (GitRef
  "shoal/integration") …)`, `tryMerge` into it, `git merge --ff-only`,
  `./check.sh` run by the root itself three times. Saved model turns. Sol
  wants `tryMerge` to advance the named branch too (the extra `--ff-only`
  is a turn).
- Pre-wave "most likely missed condition" per task, carried into the gate
  checklists and the reviewer briefs: two of the three repairs (Task 3
  bare `--file` swallowing the next flag, Task 4 empty-list style overlap)
  were exactly the predicted misses.
- Reflex before classification: zero of eight Jev calls looked at a
  `check.sh` output. Predicted from run 5's data, confirmed live.
- Incremental logging held: the log was current through every wave.
- Skills loaded via `.agents/skills` in one `cat`; the only `doc` fallback
  (`doc worktree`) returned "unknown topic".

Jev, eight calls, all from the root, all the playbook gate (`J.choice`,
four options including Sol's own `insufficient_evidence`, `J.accept
J.merging`, `J.explain` printed):

- Task 1 first candidate refused at 0.63; the reviewer found the timestamp
  test cheating (`created_at - 1`, `>=`) and a prose `implOutput`. Repair
  accepted at 0.92.
- Task 3 refused at 0.61; the reviewer confirmed the predicted miss. The
  repaired candidate was refused again at 0.74 and the fresh reviewer
  accepted it: one conservative doubt on a good candidate.
- Task 4 accepted at 0.90; the reviewer found the empty-list case untested.
  A confident false accept with complete evidence: the checklist item
  "distinct styles for zero/all/partial" did not name the empty list, so
  the gate had nothing to check it against. The lesson is in the options,
  not the policy: the pre-wave miss list must be written into the
  `item_missing` conditions verbatim.
- Tasks 2 and 5 accepted at 0.95 and 0.94; reviewers agreed.
- Sol on trust: it would merge unseen only after a labeled corpus covers
  every checklist dimension with planted misses, `insufficient_evidence`
  reliably wins when hunks are removed, and the gate names the violated
  item structurally. "Confidence can reflect textual plausibility rather
  than contract coverage."

Friction, by time cost (observer and survey agree):

- Reusable gate helper: `universalChecklist`/`mkGateState` took four
  attempts across ~8 minutes. "this declaration's type is ambiguous; add a
  signature (e.g. :: Text)" fired at least four times in different cells;
  splitting a signature from its equation inside `let` lost the argument
  scope ("Variable not in scope: specific"); top-level signature plus
  equation worked. Bare literals under `ToJSON` needed `:: Text`. The
  diagnostic must name the binding and show where the signature goes.
- `shoal-define-actors`: `coordinationActor` is not in scope in a fresh
  project (it lives in `examples/shoal-workspace`'s `Project.Routing`);
  `import Project.Work`/`Project.Routing` fail. The skill has to stand on
  the shipped surface (`R.start`, `R.client`, `R.on`, `R.settlement`).
- `atRef "shoal/integration"`: `No instance for IsString GitRef`; wants
  either the instance or an error that shows `atRef (GitRef "…")`.
- `readFile` from a cell: `Ambiguous type variable 'effs0' … FindElem
  FsRead` plus `No instance for Pack (Either FsError Text)`; Sol shelled
  out with `cat` for every file read afterwards. Bash won all repository
  evidence work: "short, familiar pipelines with transparent failure
  semantics" versus quoting, `Either OutputIssue Text`, effect-row
  inference and display budgets.
- Multi-statement `case` extraction cells over `J.answers`/`J.explain` ran
  30 to 107 s each, six times.
- Combined reply displays overflowed into `cellDisplay.more` pages.
- No settlement timestamp on replies (known; `responseSettledAt` still
  needs a clock effect); watch notices' "+Xm since actor launch" served.
- Task 1's new shared API tripped `-D warnings` dead-code until the child
  added `#[allow(dead_code)]`.

Sol's asks, read through "primitives, not helpers": `collectSettlements`,
`artifactBundle`, `checklistGate`, `mergeIntoNamedBranch`. Each is a
`collectArtifacts`-shaped helper the model writes itself once the
primitives are there: a record-actor effect row that admits Jev, Commands
and Forks (so a gate actor is definable in `.shoal`), `GitRef` from a
literal, a `readFile` that types, `tryMerge` optionally advancing a
branch. The first non-LLM actor Sol described unprompted is the wave-3
gate actor: subscribe to settlements, derive stat and hunks in parent
custody, prove coverage, run the reflex, wake the root only with a ready
bundle, a concrete mechanical repair list, or an unclassified failure.

Interview question 17 ("where did you still have to say the obvious next
thing") is the wave-3 specification. Sol listed ten predetermined
transitions that each cost a root turn: implementation settled → collect
artifacts, start review; review asked repair → send the findings back;
repair settled → collect, re-review; re-review accepted → merge, run the
integrated check; four children settled → collect, start four reviews;
mixed verdicts → merge the accepted, request the repairs; repairs settled
→ re-review; re-reviews accepted → merge, final check; check green → plan
cleanup; plans clear → execute. Plus "pending" turns after registering
watches. The root should wake only when mechanical evidence fails
unexpectedly, Jev doubts or finds conflict, a reviewer asks for a
contract-changing repair, a merge conflicts, the integrated check fails,
cleanup retains resources, or the outcome is ready. Sol also wants
policy-based escalation (deterministic checks, then Jev, Luna only on
doubt or high risk) instead of every candidate seeing both, and capability
search over the installed surface ("collect responses of the same result
type", "settlement timestamp") that distinguishes documented, installed and
callable here.

Where the 30 to 107 s cells went (measured from the run-6 compiler log,
620 daemon requests): each cell item costs one classification probe (~0 s),
one extractor request (median 1.4 s, p90 2.2 s: GHC typecheck plus Core
extraction of the item's module; the runtime's own "compile summary" of
~110 ms is only the JIT step after that) and ~1.3 s of execution and
observation before the next item starts. About 3.3 s per item, in series.
A nine-item `case` extraction cell is therefore ~30 s by construction, and
the whole run spent 859 s inside the extractor. Two consequences: fewer
model turns do not shorten the loop unless cells get shorter too, and a
gate actor compiled once from `.shoal/Project` pays this cost once per
run instead of once per handler invocation. Engine follow-up (perf
backlog): compile a multi-item cell as one module when no item depends on
an observed value of an earlier one, while preserving the notebook's
failure semantics explicitly: earlier committed items stay committed when a
later item fails (Astra: the dependency condition establishes the batching
opportunity, not equivalent preparation and failure behaviour).

## Wave 3 candidates (from Astra's review, 2026-09-17)

- Ambiguous-type diagnostic: name the ambiguous binding or expression,
  distinguish authored ambiguity from a generated-wrapper failure, and show
  the annotation at the smallest useful location when derivable. "Add a
  signature" alone still leaves the model guessing; a model dropping the
  field is the signal the repair is dearer than abandoning it.
- Cleanup: one operation that plans and executes and returns the typed
  receipt, keeping `planCleanupFor`/`executeCleanup` for preview or
  selective execution; it earns its place only if it owns target validation
  and the plan-to-execute delta.
- Gate evidence provenance: the parent's own `git diff <base>..<oid>` is
  the evidence; the child's file list is a claim to check. Truncated hunks
  or omitted files must be explicit in the state (E13/E14 on the laptop
  measure this).
- The strongest finding to preserve: a structured `Doubt` elicited useful
  recovery from Sol without a prescribed workflow. Make missing evidence
  similarly explicit.
- E13/E14 (laptop, `plans/jev/addendum-E13-E14-provenance-2026-09-17.md`):
  the gate's evidence must be the parent's own `git diff <base>..<oid>`;
  a child-reported state passed a planted omitted-file case at 0.95 that
  the parent-derived state failed at 1.00. With every hunk removed the gate
  still accepted at 0.62 to 0.82: confidence measures the options in view,
  not what is missing. Coverage is a code check before the packet is sent.
- Ideas from the laptop worth a wave: field ablation as a gate diagnostic
  (drop each state field, see what moves; a field that moves nothing is
  unused or ignored); anchors (one known-good, one known-bad item) in every
  judgment pool; replay the ledger against `jev-preview` before the alias
  moves; a question linter (paraphrase delta over 0.1 marks a judgment
  question); a code-keyed reflex over the notebook's own GHC errors (Label
  vs Text, Render ambiguity, ZonkAny, String vs Text) with a suggested
  rewrite attached to the cell error; contradiction "does A contradict B"
  over commit message vs diff, brief vs implementation, doc vs code.

## Wave 2 landed (after run 5)

Five edit-only parcels, one compile: root allocations land in a root-owned
`worktrees/root/` directory that is writable to the root at launch, and
`register_source_checkout` scans registry records without deriving liveness
(the run-4 stale-worktree fault; the binding key already carried the run id);
ambiguous-type GHC diagnostics (`ZonkAny`, `Render (f0 …)`, bare `error`) are
rewritten to "this declaration's type is ambiguous; add a signature"
(defaulting cannot reach any of the three); no notice for a watch cleanup
forgot; `listAgents` returns a ten-field `AgentSummary`, `listAgentsFull`
the record; the child's activation message shows the concrete `respond`
call; four skills (`shoal-jev`, `shoal-cleanup`, `shoal-unfold`,
`shoal-workbench`) plus `doc actors`, `doc topics` lists skills; jev-dsl
answers are record-dot fields with `Show`/`ToJSON`, `ask`/`ask1` primary,
published at github.com/inanna-malick/jev-dsl; the vendored copy resynced.

Not done: `responseSettledAt` (no clock in the `Replies` GADT and no Rust
projection of `ResponseResult`; needs a small effect). Spot tests: 47 of 49
pass; `typed_reply_settles…` (pre-existing Text quoting) and
`activation_presents_prose…` (a long-Text display truncation assertion, same
family, not verified against main) fail.
- Run 6, live: the `shoal-define-actors` skill's example depends on
  `coordinationActor` from `Project.Routing`, which exists only in
  `examples/shoal-workspace`, not in a fresh project's `.shoal`; Sol's
  `lookup`, `rg` and imports all failed. Skills must be self-contained over
  the shipped surface (`R.start`, `R.client`, …) or ship the Project modules
  they cite. Otherwise run 6 so far: the checklist gate refused a 0.63
  candidate, the reviewer found two real defects, the repair went back to
  the same child, a fresh reviewer accepted, the root ran `check.sh` in its
  own worktree and merged.

## Wave 3 target (agreed 2026-09-17, after run 6 observation)

Efficacy means more of the loop as code, with Jev inside the coordination
machinery so the root stops spending turns on routing and Luna boilerplate;
deeper trees follow from that. Shape (Astra, agreed): Sol supplies task,
acceptance conditions, allowed roles and escalation policy once; Haskell in
the project's `.shoal` watches typed replies and gathers evidence tied to
their revisions; deterministic checks handle known outcomes first; Jev
classifies the residue into explicit conditions, always with a "cannot
determine from this evidence" exit; code performs already-authorized
actions (request missing evidence, route a candidate to review, return a
concrete repair request); Sol receives unresolved cases and completion
summaries. Start with the child outcomes whose next action is already
determined: missing evidence → request it; candidate ready → dispatch
review; conflicting reviews or unclear failure → escalate. For the deeper
tree, give an intermediate actor a coherent responsibility (implement and
repair one component through its own children) so the level absorbs work.
Settle before implementing: what proceeds without waking the parent and
what bounds it (spawning authority, repair-attempt limit, repeated identical
failures, merge); existing authority and budgets govern, Jev chooses within
them. Primitive to confirm first: a record actor's `EffectProfile` must
admit `Jev`, read-only `Commands` (git diff) and reviewer admission.
Run-7 interview question: "Where did Sol still have to say the obvious next
thing?"

## Wave 3 landed (2026-09-17, before run 7)

What shipped: `mergeAdvance` on `MergeRequest`; `IsString GitRef/BranchName`;
one exclude writer (`ensure_shoal_local_exclude` no longer hides `.shoal/`);
ambiguity advice that names the binding and the cell form; `R.withWorktree`;
the coding role may allocate worktrees; a Rust/Haskell row parity test; the
`shoal-orchestrate` skill; `shoal proxy`. The toy project's `.shoal/Project`
carries `Gate.hs` (eight Jev seams, per-item Nouls, typed `J.handle` routing,
bounded packets with truncation metadata, typed evidence failures) and
`Reflex.hs` (the reflex table as data).

Finding that reshaped the gate: integrate authority follows worktree custody
(`ActorWorktreeAuthority::owns`), custody is exclusive, and a record actor
started without a worktree resolves to the research role, whose ceiling has no
`WorktreeIntegration`. A gate that merges by itself is refused at start. So
merging is its own actor: the parent creates the integration worktree unbound,
starts one `Integrator` holding it with `R.withWorktree`, and gates `R.call`
it; the mailbox serialises merge-and-check. The integrator checks before it
publishes (merge with `mergeAdvance = Nothing`, run `check.sh`, then
`update-ref` on green; `reset --hard` on red), refuses on publication drift or
when the publication branch is checked out elsewhere, and stays blocked until
`reconcile`. Consequence for the source checkout: it must not sit on the
publication branch during a run (`update-ref` would leave its index stale).

Run-7 shape change: the root receives the feature as a goal plus standing rules
and a two-stage sizing rule (Sol nodes, then Luna nodes and leaves) and writes
its own contracts. Budgets are bounded depth and per-node width (`child_budget`
clamps to parent depth minus one and inherited width); a gate without a
worktree spends the research policy's one generation on its reviewers, so the
tree recurses through nodes that hold worktrees, never through gates. Whether
a node appears is an observation, not a pass condition. The root also writes
`brief-8.md`, the first handoff artifact toward swarm-to-swarm iteration.

Open: the extractor daemon left by a long recipes run and the embedded stdlib
in a stale `shoal` binary both masked a `haskell/lib` edit once (`Not in scope:
R.withWorktree`); `check-tui.sh` now rebuilds `shoal` and sets
`TIDEPOOL_PRELUDE_DIR`. Several `documentation_tests` failed while `haskell/lib`
was mid-edit; rerun after the commit.
