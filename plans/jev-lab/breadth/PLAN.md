# Breadth survey: TypeSafe Jev patterns, run live in the Shoal workbench

Written 2026-09-17 for an Opus implementer. Goal: try many TypeSafe-endorsed
patterns and original recompositions of them, each as one or two executable
Haskell cells against real artifacts, and record what each absorbs of an agent's
System 2 work. Breadth over depth. Nothing here becomes a module, a feature or a
framework; the deliverable is runnable cells plus a compact notebook.

## Context

Earlier today one pattern (structure recovery of a failed build, then per-item
Nouls, then a strategy Choice conditioned on stated intent) was taken deep and
became `Project.Investigate`. Its interview with a Sol consumer produced the
strongest lesson so far: the program disagreed with Sol only because it lacked
the assignment. Supplying one sentence of intent moved the strategy choice from
refusal to 0.99 in both directions with no threshold tuning. Every experiment
below carries realistic context in its state for the same reason.

The supervising peer's steer, verbatim: "Prioritize combinations we haven't
tried. Save runnable cells and short observations; most experiments needn't
become modules or features. For each promising combination, identify the
stretch of agent cognition it could absorb, then actually execute it. Surprise
us with applications beyond review and failed-build investigation." And: "Use
TypeSafe's endorsed primitives and patterns as building blocks, then explore
beyond their documented applications. Clearly distinguish documented pattern,
our new composition, and observed result."

## Operating rules (verbatim where security-relevant)

- Tidepool edits go in `~/dev/tidepool-jev` (branch `feat/jev-effect`).
  **Never edit `~/dev/tidepool`.** The toy repo is `~/dev/shoal-evals/tui-test-app`
  on `shoal/source`; do not commit or checkout there during this work.
- Commit by pathspec only: `git commit -F msg -- <paths>`, `git add -N` for new
  files. Never `git add`, `-A`, `--amend`, `reset`, `rebase`, `stash`. Do not push.
- Trailers: `Co-Authored-By: Claude Fable 5.1 <noreply@anthropic.com>` and
  `Claude-Session: https://claude.ai/code/session_012KveR6o1mSvezE7F7kLmi4`.
- The API key is read inline only: `TYPESAFE_API_KEY="$(< ~/.config/typesafe/api-key)"`.
  Never print, echo, log or commit it.
- `nix-shell` does not work in tidepool-jev; use `bash scripts/dev-shell.sh --shoal bash -c '…'`.
- Scratch files go in `$CLAUDE_JOB_DIR/tmp` (`/home/inanna/.claude/jobs/4940a626/tmp`),
  never `/tmp`. Keep the durable copy of every cell in `plans/jev-lab/breadth/`.
- Sonnet/Haiku for any delegated mechanical work; pass `model` on every Agent
  call. This survey needs no subagents.
- Plain language in notes. No coined names.

## Setup (once)

The shoal binary is already built at `~/dev/tidepool-jev/target/debug/shoal`
with the uncommitted engine fixes in the tree (the `Nil` preamble import, the
`pure x` preflight retry, same-cell and cross-cell declaration shadowing, and a
half-finished module-discovery change that compiles). Do not rebuild.

Launch an idle cheap root and drive everything through the proxy:

```bash
cd ~/dev/tidepool-jev
export TIDEPOOL_DEV_FLAKE="git+file://$HOME/dev/tidepool-jev?rev=$(git rev-parse HEAD)"
TYPESAFE_API_KEY="$(< ~/.config/typesafe/api-key)" \
  bash scripts/dev-shell.sh --shoal scripts/shoal-init.sh \
    --workspace "$HOME/dev/shoal-evals/tui-test-app" \
    --session lab5 --no-attach --model gpt-5.6-luna --effort low
# then, per cell:
./target/debug/shoal proxy lab5 "$CLAUDE_JOB_DIR/tmp/lab5/NN-name.hs"
```

Exit 1 means the cell was rejected (read the message; it usually names the
fix). Exit 2 means sent with no answer: never resend blindly, check with
`--actors`. `--fresh` replaces the proxy workbench if it is wedged. The root
pane is tmux window 3 of session `lab5`; leave it idle. Kill with
`tmux kill-session -t lab5` at the end.

Smoke cell first (`00-smoke.hs`), copied from `plans/jev-lab/00-smoke.hs` but
pointing at `plans/jev-lab/fixtures/f726882-check.out`; it proves `Cmd.run`,
`Cmd.stdout` and a `Value` result all work before any Jev call.

## Cell rules that cost hours today (read before writing any cell)

1. Lead Jev cells with `{-# LANGUAGE OverloadedLabels, OverloadedRecordDot #-}`.
   `J.` is already qualified in scope; `:=`, `:&`, `Nil` are unqualified.
2. A cell's last unit is a value. `pure x` at the end now works via a retry,
   but plain `x` is still the idiom. Return `Text` or a `Value`; a `String`
   renders as a list of one-character strings.
3. Read answers with record dot on the typed value: `(J.answers r).each`,
   `p.must_change.yes`, `a.strategy.key`. Never `toJSON` an answers packet
   except to display it.
4. A declaration cannot see a `<-` bind from the same cell. Write one
   self-contained function that takes its inputs as arguments. Declarations
   may now be redeclared (shadowing works), so iterate freely.
5. Read files through git, not the filesystem: `git show <oid>:<path>` and
   `git grep -n <term> <oid>`, run with `Cmd.run (Cmd.argv [...])` and
   `Cmd.stdout`. The mount boundary refuses directories outside the toy repo;
   fixtures under `$CLAUDE_JOB_DIR/tmp` are readable via `cat` though.
6. No `read`/`reads`/`T.decimal`. Parse digits with
   `T.foldl (\a c -> a*10 + (fromEnum c - 48)) 0 (T.filter isDigitLike t)`.
7. Bind previews, not whole files (`T.take 2000`); binding a large record
   exhausts the observation budget. Render, then read one field.
8. Copy the packet idiom from `Investigate.hs` lines 764-830 (pool, `eachIn`,
   `askAbout`, one `choice` with `J.alt … J..|`, `J.ask (J.state (object …))`,
   `J.accept`/`J.explain`/`J.selectedKey`). For `score`: `J.score "q" (J.level #a "…" () J..| J.level #b "…" ())`, read `.nearest`, `.expectation`, `.masses`.
9. For each new packet, end the first run by displaying the bound `answer`
   whole so every distribution is visible, then project.

## What TypeSafe says works with this model (the building blocks)

Distilled from `plans/jev-typesafe-patterns.md`, the 18 fetched cookbooks in
`/tmp/typesafe-patterns-2026-09-17/`, and today's 600+ measured calls
(`.shoal/skills/shoal-jev/SKILL.md` playbook). Every experiment must be
expressible in these terms.

- **Literal questions over named state fields.** Name the field in backticks;
  state the deciding fact. Judgment questions ("is this sufficient") sit near
  0.4; literal ones separate. Adding *why* to a question moved 0.42 to 0.72.
- **Options are conditions, not actions, and every set has an exit written as a
  condition.** Without one the model picked a wrong option at 0.94.
- **Noul per item over a pool for membership; Choice only when exactly one wins;
  Score only for an ordered ladder of exclusive concrete situations.**
- **Speculative fan-out.** Ask branch-conditional questions under `J.given` in
  the same request; questions cannot see each other's answers, so new evidence
  is a new stage, never a follow-up in the same packet.
- **Code does everything code can do.** Pre-parse candidate values; the model
  assigns roles; code copies and never invents. Grouping and prefix matching
  are string operations.
- **Composite scoring.** Measure several dimensions once; combine in code;
  combined confidence is a minimum with the weakest part named, never a product.
- **Confidence routing.** Low confidence changes specificity, fetches more
  evidence, or hands back; it never falls through as a decision.
- **Cascade.** Cheap producer, focused semantic verification, selective
  escalation; thresholds swept against recorded outcomes.
- **Consistency.** Examine wording and rivals around an uncertain answer; do
  not repeat until the desired answer wins.
- **Structure recovery.** Pairwise boundary Nouls, block-role Choice with
  speculative companions, code renders with original bytes retained.
- **Entity alignment.** Pairwise identity plus explanatory field judgments,
  three outcomes kept distinct (same / different / undetermined).
- **Two-stage selection.** Broad cheap shortlist, then detailed selection with
  an explicit rejection option.
- **Contradicts vs says nothing** are separate answers; substring prefilter first.
- **Autoresearch.** A model authors the question bundle; outcomes and failures
  drive the revision; the bundle is shared as source.
- **State cap 32k tokens; a whole 30k diff already drops confidence.** Compact.

## Fixtures

All real, none synthetic:

| fixture | where | use |
|---|---|---|
| three failed checks | `plans/jev-lab/fixtures/{f726882,4610b5e,53ad43c}-check.out` | evidence-dependent loops, correction generalization |
| toy repo at those OIDs | `git -C ~/dev/shoal-evals/tui-test-app show/grep` | source reads |
| the PRD | toy repo `TASKS.md` (84 lines: goal bullets, standing rules, sizing rule) | decomposition, assumption tracking |
| run-7 assignments | `~/.claude/jobs/4940a626/tmp/run7-evidence/inboxes/*.jsonl`, `sessionReady.message` field (Sol node 10-1, Luna node 47-1, store leaf 46-1, others) | sibling plan conflicts, steering, pre-parsed extraction |
| run-7 child reports and rollouts | `run7-evidence/rollouts/*.jsonl`, `scripts/*_dump.txt`, `scripts/47_full.txt` | claim checking, prose-to-values |
| run-7 host log and notes | `run7-evidence/host.log`, `dogfood-notes-run7.md`, `tui-brief-7.txt`, toy `.shoal/brief-8.md` | noisy tool output, steering that did not trickle down |
| skills and project modules | toy `.shoal/skills/*/SKILL.md` (10), `.shoal/Project/*.hs` (6), `prompts/shoal/docs/*.md` (12 topics) | discovery by meaning |

Extract fixture text into `$CLAUDE_JOB_DIR/tmp/lab5/fx/` with one small
Python or jq step before the cells (assignments as `assign-10.txt`, etc.) so
each cell reads one flat file with `cat`.

## The experiments

Twelve, in two groups. Each is one or two cells. Budget per experiment: at most
three cell submissions to get it compiling and running, then at most one
rewording pass if the answers show no signal. Then record and move on,
whatever the outcome. Cells are named `NN-name.hs`; the first cell of each
experiment displays the raw `answer`.

For every experiment record: the documented pattern(s) it draws on, what in it
is our new composition, the fixture, the exact questions, the raw
distributions, and one sentence on the stretch of agent cognition it would
absorb (what an agent does by hand today across how many turns).

### Group A: documented patterns we have not run

**A1. Typed action dispatcher against changing state** (function-calling
cookbook + speculative fan-out; "the payload is the continuation"). Cells
`10-dispatch.hs`, `11-dispatch-loop.hs`. State: the `53ad43c` check output plus
an assignment sentence from the store leaf's fixture. Menu of prepared
continuations, each a real `Cmd.run` against the toy repo: read the commit
message (`git log -1 --format=%B <oid>`), show the definition's hunk (`git show
<oid> -- src/store.rs`), grep callers, read TASKS.md's persistence bullet, and
the exit "the state already carries what the next question needs". Branch
arguments asked speculatively in the same packet under `J.given`, e.g. under
"a caller search is next": which term, which path prefix. Loop three steps:
select, run, append the observation to state, ask again. Inspect: does the
menu selection change sensibly as evidence arrives, and does the exit fire
when it should? Cognition absorbed: the agent's "what do I read next" turns.

**A2. Hierarchical source location with several live paths** (hierarchical
classification cookbook; confidence-routing's broader category). Cell
`12-locate.hs`. Question: for each of three PRD bullets ("status line shows
the active filter", "`--tag NAME` parsing like `--file`", "persistence
round-trips tags"), where does the change live? Level 1: a Choice over
top-level areas (`src/app.rs`, `src/store.rs`, `src/main.rs`, `src/panel.rs`,
`src/panels/`), Nouls per area for membership. Level 2 for every area above a
mass floor (not just the winner): a Choice over that area's files or `fn`
headers, pulled by `git grep -n "^\s*\(pub \)\?fn "`. Score paths by geometric
mean in code; report the top two paths and whether the runner-up is close.
Report the broader category (the area) when file-level confidence is low.
Cognition absorbed: the orientation reads a fresh leaf does before touching a
file.

**A3. Two-stage discovery of authored components** (skill-suggestion cookbook).
Cell `13-discover.hs`. Candidates: the 10 skill descriptions (frontmatter
`description:` only), the 6 project modules (header comment, first 12 lines),
the 12 doc topics (first heading). Three task sentences, e.g. "a check failed
and I need to know which files must change", "I have to merge a reviewed
candidate into the integration worktree", "I want to know whether two child
plans collide". Stage 1: one Noul per candidate "would this help with `task`"
over the pool. Stage 2 over survivors above 0.5: one Choice with the exit
"nothing here fits; write it or ask". Inspect whether `Project.Investigate`
surfaces for the first sentence with only its header as evidence. This is the
discovery gap from friction item 10 tried as semantic matching before any
engine change. Cognition absorbed: the lookup and skill-hunting turns at task
start.

**A4. Pre-parsed value extraction from prose reports** (pre-parsed extraction +
citation-check split). Cell `14-extract.hs`. Fixture: a run-7 child's prose
report from a rollout dump. Code finds every 7-40 hex OID, every `src/…rs`
path, every backticked command, every `test …` name. For each candidate a Noul
per role: "is this the submitted candidate OID", "is this a path the report
claims to have changed", "is this the command the report claims to have run".
Then check claims against evidence: for each claimed path, does `git diff
--numstat base..oid` name it? Contradicted vs unsupported vs supported, all
three kept. Cognition absorbed: turning a narrated reply into the typed record
the standing rules demand, and the honesty check.

**A5. Composite scoring with policy varied in code** (composite-scoring
pattern). Cell `15-composite.hs`. Fixture: the six distinct locations from the
`53ad43c` investigation plus the two from `4610b5e`. One packet, four Nouls
per location: on the compiler's reported path, inside a test body, would a
change here alter public API, is it inside the owned paths. Then three
consumers computed in code from the same answers with no new call: a leaf's
edit order (min-based), a reviewer's risk ranking (count of flags), a
context selector's read list (relevance above 0.5). Inspect whether the
orderings differ meaningfully and whether any dimension is degenerate (all
near 0 or 1). Cognition absorbed: re-deriving the same facts for each role.

**A6. Consistency probe around an uncertain answer** (both consistency
cookbooks). Cell `16-consistency.hs`. Take the strategy Choice from
`Investigate` with no intent (the case that sat at 0.35-0.66 today). Ask it
five ways in one packet: the current wording; options reworded as conditions
on `intent` being empty; with `J.given "the commit message says the parameter
was added on purpose"`; with an added rival "the definition and callers should
both change"; with the exit removed. Compare the five distributions. Inspect
which rewording moves mass and which does not. This is diagnosis of wording,
not tuning. Cognition absorbed: none directly; it tells us which of the other
experiments' packets to trust.

### Group B: original compositions (our recombinations, labelled as such)

**B1. Which assumptions does a new observation undermine.** Cells
`20-assume.hs`, `21-assume-hit.hs`. New composition: RAG-passage
classification (relevance / evidence / contradiction) applied to a
coordinator's own stated assumptions. Code extracts assumptions as the
imperative sentences of a run-7 node assignment (10-1 or 47-1): "the scaffold
is deliberately red", "these ownership sets are disjoint", "review every
candidate with a fresh Luna", etc. Observation: the `4610b5e` check output
(the lint inside a test another leaf wrote), and separately the store leaf's
report. Pool over assumptions, per item two Nouls: "does `observation`
contradict this assumption", "does `observation` say anything about this
assumption at all". Route: contradicted / untouched / supported. Inspect
whether the disjoint-ownership assumption lights up for the cross-file lint.
Cognition absorbed: the re-read of the plan a coordinator does when a child
reports something unexpected.

**B2. Search for the observation that separates competing explanations.**
Cell `22-discriminate.hs`, then `23-discriminate-run.hs`. New composition:
speculative fan-out over (explanation × candidate observation), then A1's
dispatcher runs the winner. Explanations for `53ad43c`: signature change was
intended; signature change was accidental. Candidate observations: the same
menu as A1. Packet: for each observation a Noul per explanation, "if
`explanation` is true, would `observation` show X" with X stated concretely
(commit message mentions a limit; TASKS.md names a limit; the diff of
store.rs:59 adds `limit` and uses it). Pick in code the observation whose two
conditional answers differ most; run it; feed the result into the strategy
Choice from `Investigate` as `intent`-like evidence. Inspect whether the
program recovers on its own the fact Sol had to be told. This extends today's
strongest result directly. Cognition absorbed: the "let me check the commit
message" turn.

**B3. Sibling plans that conflict despite disjoint files.** Cell
`24-siblings.hs`. New composition: entity alignment's pairwise judgment applied
to plans rather than records. Fixture: the run-7 assignments of the UI node
(47-1) and the store leaf (46-1), and any third from the inboxes. Code pairs
them. Per pair: "do both plans change the same public type or variant",
"does plan A read a field, function or file that plan B changes", "do the
plans assume different shapes for the same data (e.g. tags as a field vs a
filter variant)", and the exit "the plans share nothing". Three outcomes kept:
conflict / independent / undetermined. Inspect whether the `Filter::Tag` vs
`Option` redesign from run 7 is visible from the assignment text alone.
Cognition absorbed: the parent's cross-reading of its own children's briefs.

**B4. A correction becomes questions that catch the same mistake elsewhere.**
Cells `25-generalize.hs`, `26-generalize-run.hs`. New composition: structure
recovery (find candidate sites in code) plus a correction carried as state.
Correction fixtures: (a) the `4610b5e` lint "field assignment outside of
initializer for an instance created with `Default::default()`": code greps
the tree at that OID for `Default::default()` and takes 6 lines after each;
(b) the `f726882` non-exhaustive match: code greps for `match .*panel` /
`ActivePanel` and slices each match block. Per site Noul: "does this site
have the same defect as `correction_site`, namely …" with the defect named
concretely, plus "is this site already the corrected form". Inspect precision
against the compiler's own list (fixture a has exactly one, fixture b has
five plus two test lines). Cognition absorbed: "are there other places like
this" after a reviewer finding.

**B5. A tool view that adapts to the agent's current question.** Cell
`27-view.hs`. New composition: autoformat's block roles plus a relevance Noul
against a stated uncertainty. Fixture: the first 400 lines of
`run7-evidence/host.log` or the `f726882` check output. Code splits into
blocks on blank lines and timestamp changes and keeps byte offsets. Two
states, run as two packets: "I am trying to find out why the child never
started" and "I am trying to find out whether the merge happened". Per block:
a role Choice (diagnostic / command echo / progress / prose / repetition of an
earlier block) and a Noul "does this block bear on `current_question`". Code
renders: relevant blocks verbatim, others as one index line with offset and
role. Inspect that the two renderings differ and that nothing is lost (index
covers every omitted block). Cognition absorbed: scrolling.

**B6. Interpretations first, then investigation** (the peer's provocation;
needs a generating model). No `Llm` effect is in the workbench row, so the
generator is a Luna child started with `unfold` at low effort and asked for
three one-sentence readings of an ambiguous PRD bullet ("clearing returns to
the current unfiltered view"), replying as text. Cell `28-interpret.hs`
starts it and waits; `29-interpret-judge.hs` asks, per interpretation, Nouls
against the actual source at HEAD (the `f` filter cycle code in `app.rs`,
found by grep): "is this reading consistent with what `source` does for the
`f` key", "would a test for this reading differ from a test for the others".
If the child costs more than one turn or the unfold path fights back, record
it as "needs a missing capability: a cheap generation call in a cell" and move
on; do not build the capability. Cognition absorbed: the disambiguation a
leaf does silently before writing a test.

### Optional if time remains

**C1. Autoresearch light.** Take B1's assumption-check bundle, run it against
three observations with known outcomes (which assumption each actually broke
in run 7, from `dogfood-notes-run7.md`), list the misses, rewrite the
questions once, rerun. Record both bundles and the change. This is the loop
the pattern describes with the implementer as the proposing model.

**C2. Two thresholds, not one** (guardrails). On A4's contradicted claims,
route with two independent floors: act automatically (record as a defect) vs
notify a person, with a severity Score (claim about a test passing that
failed > claim about a path). Report which claims land where.

## More ideas to run down (the long list)

Groups A and B are the scheduled core. Everything below is a candidate for the
same treatment: one or two cells, a real fixture, raw distributions recorded,
one sentence on the cognition absorbed. Pick by curiosity and by what the core
results suggest; a starred item is one I would run first. Each entry names the
TypeSafe basis, what is new in the composition, the fixture, and the stretch of
agent work it would absorb. Items marked *outside the swarm* are applications
beyond today's workflow, which the peer asked for explicitly.

### Attention, inbox and steering

- **D1 ★ Steering fan-out that actually forwards.** Run 7's root received
  operator steering and forwarded nothing; "steering does not trickle down".
  Basis: function-calling dispatcher + speculative fan-out. New: the pool is
  the live children, the payload is a real `sendMessage`. Per child, Noul
  "does `steering` change what `assignment` asks this child to do" and a
  Choice among forward verbatim / forward with the one relevant sentence /
  not affected / needs `updateRequest` because the child is mid-turn on the
  affected request. Fixture: the run-7 stand-down text from
  `dogfood-notes-run7.md` against the three assignments in the inboxes; run
  live against two idle Luna children in `lab5` so the forward is real.
  Absorbs: the coordinator turn that decides who needs to hear this.
- **D2. Wake or batch.** A record actor sits on a parent's inbox; per message
  a Score ladder of concrete situations ("nothing depends on this yet" →
  "a decision the parent is about to make is wrong without it") decides wake
  now / batch into the next turn / drop, and writes a receipt the parent can
  read. Basis: Score as ordered ladder; confidence routing. Fixture: the 8
  run-7 inboxes replayed in order. Absorbs: the interruption tax on every
  `watchChanged`.
- **D3. Novel-or-repeat on progress notes.** Successive progress notes from
  one child; pairwise Noul "does `note_n` say anything `note_{n-1}` did not";
  the parent sees only novel notes. Basis: entity alignment. Fixture: run-7
  rollouts' progress payloads. Absorbs: rereading the same status five times.

### Plans, contracts and assignments

- **D4 ★ Ownership gaps before launch.** Given each child's owned path set and
  the PRD bullets assigned to it, per (bullet, child) Noul "can `bullet` be
  implemented by editing only `owned_paths`, given `tree` (the `git ls-files`
  listing and the `pub` item index)". Flags the run-7 `ActivePanel::Tags`
  outside-ownership problem before any child is admitted. Basis: pool Nouls
  with code-read evidence. Fixture: `TASKS.md` bullets against the run-7
  ownership sets. Absorbs: the contract gap found two minutes after a
  reviewer accepted.
- **D5. PRD to dependency order.** Per bullet: subsystem Choice with a broader
  fallback; "introduces a public type or variant other bullets use" Noul;
  pairwise "does `b1` need `b2` to exist first". Code emits the "shared
  contract first" partition and a topological order. Basis: hierarchical
  classification + composite scoring. Fixture: `TASKS.md`. Absorbs: the
  root's first hour of decomposition.
- **D6. Checklist clause lint.** Per acceptance-checklist item: "checkable
  from a diff and test output alone", "names a concrete test", "names the
  empty, zero or duplicate case". Returns which clauses a gate cannot check
  and which lack the likely-miss condition. Basis: the playbook's own
  likely-miss lesson turned into questions. Fixture: run-7 contracts from
  the inbox assignments. Absorbs: contract review.
- **D7. Sizing by ladder.** Score with rungs written as task shapes ("one file,
  named tests, no new public item" … "needs a design decision that spans
  files") selects Luna low / Luna medium / Sol per bullet. The sizing rule in
  `TASKS.md` mechanised. Basis: Score. Fixture: `TASKS.md` bullets plus the
  actual run-7 decomposition as the answer key. Absorbs: sizing judgment.
- **D8. Prose assignment vs typed contract drift.** Per contract field, Noul
  "does `assignment_text` state this field's value" and "does it contradict
  it". Run-7 finding: every assignment was hand-typed prose beside a typed
  contract. Basis: citation check. Fixture: inbox assignments and the
  `Contract` values in the run-7 rollouts. Absorbs: the parent's own
  consistency check.

### Source, diffs and evidence

- **D9 ★ Diff to requirement coverage matrix.** Code splits a candidate diff
  into hunks; per hunk a Choice of role (implements clause k / tests clause k
  / incidental / unrelated) with clauses enumerated as options and an exit;
  code builds clause × hunk and lists uncovered clauses and unrelated hunks.
  Basis: autoformat block roles + composite scoring. Fixture: a run-7
  candidate diff (`run7-evidence/diff-e8547e7..620a56d.patch`) against its
  contract. Absorbs: the reviewer's "did it do everything and only that".
- **D10. Test name to clause alignment.** Required test names vs the test
  functions actually in the diff; pairwise "does `test_fn` assert `clause`".
  Catches a renamed test and a test that exists but asserts nothing. Basis:
  entity alignment, three outcomes kept. Fixture: same diff. Absorbs: the
  reviewer's test read.
- **D11. Typed change record without generation.** Per hunk Nouls: changes a
  public signature, changes the persisted file format, changes key handling,
  changes rendering. Code emits a change record for the closure check and for
  D12. Basis: parallel questions. Fixture: same diff. Absorbs: the summary
  the child writes in prose.
- **D12. Semantic blast radius.** After a definition changes, code greps the
  callers; per caller Noul "does this call site depend on the changed
  behaviour, not only the signature". Prunes a call graph by meaning. Basis:
  pool Nouls over code-fetched sites. Fixture: `store::load` at `53ad43c`, and
  the `f` filter cycle at HEAD. Absorbs: the reads before a refactor.
- **D13. Commit message vs diff.** Per sentence of the message, contradicted /
  unsupported / supported by the diff; per hunk "is this mentioned at all".
  Basis: citation check both directions. Fixture: any run-7 candidate commit.
  Absorbs: part of review, and a lie detector for child reports.
- **D14. Stale comment finder.** *Outside the swarm.* Code extracts each
  comment with the following 8 lines; Noul "does the code still do what the
  comment says". Fixture: the toy repo, and one tidepool crate. Absorbs:
  nothing an agent does today, which is the point.
- **D15 ★ Bisect by meaning.** Over a commit range, per commit message plus
  `--stat` a Noul "could this commit have introduced `symptom`"; run
  `check.sh` only on the top candidates in ranked order. Basis: reranking +
  a real tool between calls. Fixture: the toy repo branches that end in
  `f726882`, `4610b5e`, `53ad43c` (known red commits, known symptoms).
  Absorbs: a bisect session. Also the first experiment where Jev saves
  compute, not turns.

### Review, verification and analysis of our own runs

- **D16 ★ Did the review add anything.** Run 7's reviews mostly restated the
  implementer's reply. Per reviewer sentence: "is this a restatement of
  `implementer_report`" vs "does it cite a line of `diff` or `test_output`
  not in the report". Code scores novelty; a review under a floor is not
  counted as a review. Basis: citation check. Fixture: the run-7 reviewer
  replies in the rollouts. Absorbs: reading reviews.
- **D17 ★ Rollout step classification replaces the Sonnet analyses.** Per
  rollout step (tool call or message) a Choice: reading / editing / running
  the check / hand-rolling the review loop / waiting / steering. Code then
  computes the run-8 comparison metrics the wave-4 plan lists (turns a node
  spent on the loop, hand-typed assignments) from a cell instead of five
  Sonnet reports. Basis: structure recovery + Choice with exit. Fixture: the
  nine run-7 rollouts via `dump_luna.py`. Absorbs: our own post-run analysis.
  *Outside the swarm*, in the sense that it improves the method, not a run.
- **D18. Interview answer coding.** Per sentence of an interview transcript, a
  Choice: defect / missing information / convenience / disagreement /
  praise, with an exit. Fixture: `interview-pane-{3,4,5}.txt`,
  `interview-46-answers.txt`. Absorbs: the summarising pass.
- **D19. Flake or failure.** Per failing test, Noul "does the message name a
  timing, ordering or environment condition"; route rerun vs repair. Basis:
  confidence routing. Fixture: any failing check output; plant one
  timing-shaped message. Absorbs: the retry decision.
- **D20. Cascade over a Luna candidate.** Per requirement Noul against the
  diff; only unresolved requirements go to a fresh reviewer with a brief
  naming only those. Basis: extraction cascade. Fixture: a run-7 candidate;
  measure how many requirements a reviewer would still have to read.

### Model-facing text: Jev linting our own tool

- **D21 ★ Error message lint.** *Outside the swarm.* `rg` every `bail!`,
  `anyhow!` and refusal string in `tidepool-actor` and `tidepool/src/shoal`;
  per message three Nouls from the standard the mount-boundary refusal set:
  names what was refused, names what would have worked, names the state that
  caused it. Report the messages that fail all three. Absorbs: a UX review
  we would otherwise do by hand or not at all.
- **D22. Skill example lint.** Per fenced example in the ten skills, Noul
  "uses only names the skill's shipped list contains" with the list in state.
  Cheap, and it found one stale example today by hand.
- **D23 ★ Jev lints Jev questions.** Before sending a packet, a meta-packet:
  per question text, "names a field of `state` in backticks", "states the
  deciding fact rather than asking for a judgment", "the options describe
  conditions, not actions". Test whether it predicts which questions return
  0.4s by running it on the vague and narrow wordings in `wording-ab.md`.
  Absorbs: the wording pass an author does by trial.

### Calibration experiments (cheap, and every other packet depends on them)

- **D24 ★ Position bias.** One Choice, five option orders, same state. If
  mass moves with position, every dispatcher above needs shuffling or a
  rule. Fixture: the strategy Choice from `Investigate`.
- **D25. Pool size and anchor drift.** The same two known-answer anchors in
  pools of 5, 15 and 30 real items. Does the pool's size or an item's
  position move the anchors? Derive the batch-size rule from the numbers.
- **D26. Description length.** One-line vs three-line option descriptions on
  the same Choice; one-line vs three-line item previews on the same pool.
- **D27. Compaction limit.** Per-requirement Nouls on a diff given as stat
  only / hunks only / first 12 lines per hunk / whole. Find where the answers
  change and write the compaction rule that follows. The state cap is 32k;
  nobody has measured where evidence stops helping.
- **D28. Field order.** Does moving `intent` first or last in the state
  object change the strategy answer? Cheap, and it decides how D1 to D13
  should lay out their state.

### Memory across runs

- **D29 ★ Lesson retrieval by meaning.** Given a new failure text, per
  paragraph of `dogfood-notes-run5..7.md` a Noul "does this paragraph describe
  the same failure". Surfaces "we hit this in run 5" without embeddings or a
  registry. Basis: reranking. Fixture: the `4610b5e` lint against all three
  notes files. Absorbs: the part of a handoff brief nobody rereads.
- **D30. Friction dedup.** New friction entry vs existing `friction.md`
  items, pairwise "same defect". Basis: entity alignment. Keeps our own log
  honest.
- **D31. Handoff brief to action list.** Per paragraph of `brief-8.md`, Noul
  "states something the next root must do before launching" vs background;
  code emits the checklist. Absorbs: reading the handoff.

### Loops that act between calls

- **D32. Speculative pre-fetch while a child works.** Given the contract, ask
  which files and tests the review will need; read them into state now so
  the review is one packet when the reply lands. Absorbs latency, not turns;
  record whether that is worth anything.
- **D33. Narrowest follow-up to a child.** When a claim is unsupported (D13
  or A4), a Choice over prepared follow-up requests (send the failing test
  name / ask for the OID / ask for the command output) picks the one to send
  first by consequence; the payload is a real `sendMessage` to an idle Luna
  in `lab5`. Basis: dispatcher. Absorbs: composing the nag.

### Rules of engagement for this list

Same as the core: at most three compile attempts and one rewording pass each;
record before moving on; nothing becomes a module. Prefer items that run a real
command or send a real message between calls, because those are the ones the
cookbooks do not demonstrate. When one of these surprises you, follow it for
one more cell and write down what changed, then return to breadth.

## Recording

One notebook, `plans/jev-lab/breadth/NOTEBOOK.md`, updated after every
experiment, never at the end:

```
| # | pattern(s) drawn on | our composition | fixture | outcome | one-line observation |
```

`outcome` is exactly one of: **worked**, **interesting failure**, **needs a
missing capability**, **worth combining with**. Below the table, one short
section per experiment: the questions verbatim, the raw distributions (numbers
in a small table), the cognition absorbed, and what would change next. Keep
every cell in `plans/jev-lab/breadth/NN-name.hs` exactly as last run. Append
new friction to `plans/jev-lab/friction.md` with the exact message.

Commit at least twice during the session and once at the end, by pathspec:

```bash
cd ~/dev/tidepool-jev && git add -N plans/jev-lab/breadth && \
git commit -F "$CLAUDE_JOB_DIR/tmp/msg" -- plans/jev-lab/breadth plans/jev-lab/friction.md
```

Finish with a short `plans/jev-lab/breadth/SUMMARY.md` (under 60 lines): the
three most surprising results, the compositions worth a second look, the
patterns that did not fit this substrate and why, and the missing capabilities
found. Distinguish documented pattern, new composition and observed result in
every sentence that makes a claim.

## Stop rules

- A cell that fails to compile three times is recorded as friction with its
  message and the experiment is skipped or simplified, not debugged further.
- A packet whose answers cluster at 0.35-0.65 gets exactly one rewording pass
  following the "name the deciding fact" rule; then it is recorded as is.
- No experiment becomes a `Project.*` module today. No engine change today
  beyond appending to `friction.md`. No threshold tuning.
- Do not start Group B before at least three of Group A have run; do not spend
  more than a third of the session on any single experiment.
- The Sol interview lesson is a constraint: every state object carries an
  `assignment` or `intent` field with realistic text from the fixtures, and
  every experiment that has an intent-dependent answer is run once with it and
  once without.

## Verification

- `NOTEBOOK.md` has a row for every experiment attempted, each with a raw
  distribution table.
- Every `NN-name.hs` in `plans/jev-lab/breadth/` reruns through
  `shoal proxy lab5` with exit 0 (spot-check three at the end).
- `git -C ~/dev/tidepool-jev status --short plans/jev-lab` is clean after the
  final commit; nothing outside `plans/jev-lab` changed.
- `tmux ls` shows no `lab5` session.
