# lab8 — Scale and ground truth

Session `lab8`, workspace clone `/home/inanna/.claude/jobs/4940a626/tmp/ws8`
(the shared `~/dev/shoal-evals/tui-test-app` binding root was held by another
concurrent worker session for the whole launch window; the coordinator
redirected me to a dedicated clone — see friction note at the end).

## 60. Map-reduce semantic evaluation over real history

**Documented**: the published TypeSafe Jev pattern claims that, at this
price, subjective/semantic evaluation can run over millions of rows, one
cheap call at a time.

**Ours**: batching real commit-subject classification into fixed-size pools
(25 real items + 2 fixed canaries per request), looped in Haskell, with the
canaries' own answers used as an inline stability gauge across the whole run
rather than a one-off sanity check.

**Fixture**: `git -C ~/dev/tidepool-jev log --format='%h|%s' -n 300`, gathered
with a plain Bash call into
`/home/inanna/.claude/jobs/4940a626/tmp/lab8/commits-300.txt` (real commit
history of tidepool-jev, not a toy repo). Read inside the cell with
`sh ["cat", "<abspath>"]`.

**Questions** (verbatim, asked as two `J.askAbout` nouls per pooled item):
- "Does this commit subject describe a change a user of the system would
  notice, rather than an internal refactor, test change or documentation
  edit?"
- "Does this commit subject name a specific component, file or subsystem?"

Canaries, carried verbatim in every one of the 12 batches:
- user-visible + names a component: `60e8e8050` — "fix(shoal): a refused
  command and a refused worktree say what was refused"
- pure internal, names nothing user-facing: `25fe148d7` — "style(runtime): run
  cargo fmt on prepared_execution.rs"

**Numbers**. 300 real commit subjects classified in **12 requests** (25 real
items per batch, chunked in Haskell; 300/25 = 12 exactly). Each request's
pool held 27 items (25 real + 2 canaries) and asked 2 nouls each — 54 answers
per request, 648 answers total for this experiment.

Anchor stability table (`user_visible` / `names_component` probability, per
batch):

| batch | anchor_user.visible | anchor_user.names_component | anchor_internal.visible | anchor_internal.names_component |
|---|---|---|---|---|
| 1 | 0.84 | 0.91 | 0.09 | 0.97 |
| 2 | 0.85 | 0.93 | 0.08 | 0.96 |
| 3 | 0.81 | 0.93 | 0.05 | 0.95 |
| 4 | 0.82 | 0.93 | 0.06 | 0.95 |
| 5 | 0.83 | 0.94 | 0.08 | 0.94 |
| 6 | 0.83 | 0.88 | 0.05 | 0.95 |
| 7 | 0.85 | 0.91 | 0.05 | 0.97 |
| 8 | 0.76 | 0.92 | 0.07 | 0.96 |
| 9 | 0.79 | 0.90 | 0.07 | 0.96 |
| 10 | 0.77 | 0.90 | 0.06 | 0.95 |
| 11 | 0.80 | 0.93 | 0.06 | 0.95 |
| 12 | 0.83 | 0.90 | 0.06 | 0.96 |

Ranges across the 12 batches: anchor_user.visible 0.76–0.85 (spread **0.09**,
just under the 0.10 flag line — batch 8 is the low point, then it recovers);
anchor_user.names_component 0.88–0.94 (spread 0.06); anchor_internal.visible
0.05–0.09 (spread 0.04); anchor_internal.names_component 0.94–0.97 (spread
0.03). **No anchor crossed the 0.10 drift line I was told to flag**, but
anchor_user.visible came within 0.01 of it, entirely from one dip at batch 8
that recovered by batch 11–12 rather than a monotonic trend.

Direct answer to the coordinator's question about sharpening vs. noise: with
batch **size held fixed** at ~27 items per request (25 real + 2 canaries, per
the brief's explicit instruction), I did not vary batch size, so this run
cannot speak to size-scaling the way the coordinator's pruning finding does.
What it can say: across 12 *sequential* batches of the same size, the
canaries show no directional trend (no monotonic sharpening, no monotonic
drift) — just bounded noise inside a ~0.09 band. I did not observe the
canaries getting sharper as more of the corpus was processed.

Top 10 by `user_visible` (pooled across all 12 batches) — **caveat**: because
both canaries are repeated in every batch, `60e8e8050` (the user-visible
canary) appears 6 of the 10 slots here purely from being asked 12 times at a
consistently high score. This is a bug in my own aggregation, not a Jev
finding — a real top-N report needs canaries excluded from cross-batch
ranking. The 4 distinct non-canary commits that made the true top ranks:

| hash | subject | user_visible | names_component |
|---|---|---|---|
| 5635ad9d8 | feat(shoal): spawnWatched, awaitAnySettled, findAgentsByLabel, delegation guide | 0.86 | 0.93 |
| 9b3aea5e2 | Record the workbench lab: what a cell can do, and where it stops | 0.85 | 0.89 |
| 9bfa8f71f | fix(shoal): four refusals that did not say what the caller did wrong | 0.83 | 0.89 |
| 92bbec3ef | feat(shoal): wave 3 primitives — mergeAdvance, IsString refs, exclude fix, R.withWorktree, coding allocation, shoal proxy | 0.83 | 0.88 |

Bottom 10 by `user_visible` (no canary contamination here — the internal
canary never scored low enough to make this list):

| hash | subject | user_visible | names_component |
|---|---|---|---|
| 496ca2e32 | docs(stg): completion plan at 344ecd59a and G1 implementation evidence | 0.04 | 0.50 |
| aca28b7bb | docs(stg): close the review's blocking items with their commits | 0.04 | 0.55 |
| 29602fe8c | test(runtime): fake extractor writes a prepared-STG artifact for the cache proptest suite | 0.04 | 0.87 |
| e4ea15996 | test(stg): one ask helper and one refusal-invariance helper in the notebook tests | 0.04 | 0.70 |
| 873e0abcd | docs: record Jev research session handoff | 0.04 | 0.16 |
| 44b7d17cf | docs: remove trailing whitespace from Jev evidence notes | 0.04 | 0.35 |
| 6c9b00d64 | docs(inventory): describe admitted imports and the remaining per-program dispatch tables | 0.05 | 0.75 |
| dfa75b27f | docs(wave5): record how Wave 6B's gate resolved the three open W1 items | 0.05 | 0.51 |
| 19890675e | test(runtime): pin rung 3 across two installed programs (C0) | 0.05 | 0.83 |
| 5190f628d | docs(wave6): record the accurate final verification sweep | 0.05 | 0.57 |

Read those twenty subjects myself: the bottom 10 look right without exception
— every one is a docs/test/chore commit, correctly scored near-zero on user
visibility regardless of whether it names a component (it can name one, e.g.
"the cache proptest suite", and still correctly score 0.04 on visibility,
which is exactly the separation the two questions are supposed to give). The
top 4 (excluding the canary-duplicated rows) are three real `fix`/`feat`
commits that do look user-visible and do name a component — reasonable. But
**one entry is a real miscalibration**: `9b3aea5e2` ("Record the workbench
lab: what a cell can do, and where it stops") is a pure research-notes
commit — no different in kind from the "docs:" commits sitting at 0.04 in the
bottom 10 — yet it scored 0.85 on a question that explicitly excludes
documentation edits. This is a genuine classification error, not a matter of
taste: the subject describes recording notes, not a change a user would
notice, and the question says so in as many words.

**Outcome**: worked, with one real classification error found and one
aggregation bug found (both reported above, not smoothed over).

**Stretch of agent work absorbed**: skimming and triaging 300 real commit
subjects for "is this worth surfacing in a changelog and does it name what
changed" — the kind of judgment a root actor currently either skips or burns
a full model turn per handful of commits on — collapsed into 12 cheap calls
sharing one canary-checked packet shape.

## 61. Shadow testing against ground truth

**Documented**: the published guidance says not to trust a vendor's
confidence thresholds — run the model beside a known-good workflow, map its
distribution against what actually happened, and read the threshold off that
mapping rather than off a marketing number.

**Ours**: using a *real* prior multi-agent run's evidence (run 7's dogfood
notes and three actor-transcript reports) as the known-good workflow, with
ground truth recorded in a file the Jev calls never saw
(`/home/inanna/.claude/jobs/4940a626/tmp/lab8/exp61-ground-truth.md`), and
mapping the observed confidence/mass/margin triples onto the exact numeric
floors of the three named policies actually shipped in
`haskell/lib/Jev/Operators.hs` (read from source, not guessed):
`routing = Policy 0.40 0.08 0.50`, `spawning = Policy 0.55 0.20 0.70`,
`merging = Policy 0.70 0.40 0.85` (mass, margin, confidence floors, in that
field order).

**Fixture**: `~/.claude/jobs/4940a626/tmp/run7-evidence/dogfood-notes-run7.md`
and `reports/{leaves-and-reviewers,root,sol-node}.md` — a real Shoal dogfood
run's actor transcripts, read with the Read tool (not in a cell). 15 labelled
situations were built from this evidence: 11 sent through the mechanical gate
(each is a `{owned_paths, acceptance_checklist, base, candidate, diff_stat,
hunks, test_output}` state built from real OIDs, real diff stats, real
`check.sh` output, and real test names quoted in the reports), plus 4
narrative-only situations (a review that only restated the implementer's own
report, a candidate that was never actually reviewed because of a descendant
ceiling, a repair request that silently added scope after acceptance and
mislabeled it as a "repair," and a stray post-reply session-id confusion)
that don't fit the diff-shaped gate at all and are reported qualitatively
only — the mechanical `{diff_stat, hunks, test_output}` shape has no field
for "was this reviewed" or "did the request's own framing match what a
reviewer actually said," and that gap is itself a finding, not a
non-finding. Every situation was given the same amount of concrete state
(real OIDs, real hunks, real check output) regardless of which side of the
question it was expected to land on, so none is judged on a short label.

**Questions** (verbatim, from SKILL.md item 2 in the Playbook section of
`~/dev/shoal-evals/tui-test-app/.shoal/skills/shoal-jev/SKILL.md`), one
`J.ask` per situation (not pooled — item 2's gate is inherently one-candidate-
per-state, and pooling eleven different diffs into one packet would have
mixed their evidence):

- Choice: "Which statement describes the candidate?" with alternatives
  `#all_present` ("Every item of the checklist holds: <items joined by
  `; `>."), `#item_missing` ("At least one item does not hold: a changed file
  outside the owned file, a failing or missing owned test, a deleted or
  weakened test, a remaining todo!(), or an implementation that does not
  match the goal."), `#conflicting` ("The items are all present but
  contradict each other, for example the report claims a test passes that
  the test output shows failing."), and the exit `#insufficient_evidence`
  ("The state does not carry what the checklist needs to be decided: a file
  named in `diff_stat` has no hunk, or `test_output` names none of the
  required tests.").
- Tripwire noul, in the same packet: "Does `hunks` contain a hunk for every
  file named in `diff_stat`?"

**Numbers**. Raw per-situation results (11 real Jev calls, one per
situation; `key` is the winning alternative, `covered_yes` is the tripwire
noul's yes-probability):

| id | ground truth | key | correct? | confidence | mass | margin | covered_yes |
|---|---|---|---|---|---|---|---|
| S1_good_app_round1 | all_present | all_present | yes | 0.92 | 0.94 | 0.89 | 0.90 |
| S2_good_store | all_present | all_present | yes | 0.90 | 0.92 | 0.87 | 0.93 |
| S3_missing_focus_defect (miss condition NOT named in checklist) | item_missing | all_present | **no** | 0.60 | 0.70 | 0.53 | 0.91 |
| S10_missing_focus_defect_named_miss (same diff, miss condition named) | item_missing | item_missing | yes | 0.99 | 1.00 | 1.00 | 0.90 |
| S4a_deliberate_red_no_exception | item_missing | item_missing | yes | 0.98 | 0.99 | 0.98 | 0.88 |
| S4b_deliberate_red_with_exception (same diff, checklist names the exception) | all_present | all_present | yes | 0.45 | 0.58 | 0.39 | 0.90 |
| S5_readonly_check_failure | insufficient_evidence | insufficient_evidence | yes | 0.46 | 0.59 | 0.19 | 0.87 |
| S6_gate_never_started | insufficient_evidence | item_missing | **no** | 0.88 | 0.91 | 0.82 | **0.48** |
| S7_compressed_not_false_report | all_present | all_present | yes | 0.82 | 0.86 | 0.77 | 0.93 |
| S8_store_tags_fmt_only_failure | item_missing | item_missing | yes | 0.97 | 0.98 | 0.96 | 0.93 |
| S9_filter_enum_consumer_break | item_missing | item_missing | yes | 0.98 | 0.99 | 0.98 | 0.90 |

9 of 11 correct. Both errors are named, not hidden:

1. **S3 vs S10 is a controlled pair, and it replicates SKILL.md 2b exactly.**
   Identical diff, identical test output, identical everything except one
   checklist sentence naming the exact condition the task was likely to
   miss. Without that sentence (S3), Jev picked `all_present` at 0.60
   confidence — wrong, a real reviewer (actor 39, in the real run) caught
   this defect and the checklist as literally written never named it.
   *With* that sentence added verbatim (S10), Jev picked `item_missing`
   correctly at 0.99 confidence. This is exactly the mechanism 2b describes
   ("a condition the option never names is a condition the gate has nothing
   to check against"), reproduced as a paired experiment rather than
   asserted.
2. **S6 is a confidently wrong answer that clears the real merging policy.**
   The state carries no diff and no test output at all (the gate itself
   never started in the real run) — the correct answer is
   `insufficient_evidence`. Jev said `item_missing` at confidence 0.88, mass
   0.91, margin 0.82. Checked against the actual shipped policy
   (`merging = Policy 0.70 0.40 0.85`): mass 0.91 ≥ 0.70 ✓, margin 0.82 ≥
   0.40 ✓, confidence 0.88 ≥ 0.85 ✓ — **all three floors clear**, so
   `J.accept J.merging` on this state returns `Right item_missing`, wrong,
   not a doubt. The one place the model's own uncertainty showed up at all
   was the tripwire noul: `covered_yes` was 0.48 for S6, against 0.87–0.93
   everywhere else in the table — a real, available signal that the state
   was incomplete, that the main gate choice did not act on.

**Threshold sweep**, confidence-only, exactly the five floors asked for
(accept if confidence ≥ floor, else hold/doubt; reporting false accepts and
false doubts separately per the coordinator's note, never a single accuracy
number). n = 11 for every row below — a frontier sketch, not a calibration:

| floor | correct accepts | **false accepts** | correct doubts | **false doubts** |
|---|---|---|---|---|
| 0.50 | 7 | 2 (S3, S6) | 0 | 2 (S4b, S5) |
| 0.60 | 7 | 2 (S3, S6) | 0 | 2 (S4b, S5) |
| 0.70 | 7 | 1 (S6) | 1 (S3) | 2 (S4b, S5) |
| 0.80 | 7 | 1 (S6) | 1 (S3) | 2 (S4b, S5) |
| 0.90 | 6 | 0 | 2 (S3, S6) | 3 (S4b, S5, S7) |

Same sweep using the **actual three named policies** (all three floors —
mass, margin, confidence — checked together, values from `Jev/Operators.hs`
above, not the confidence-only proxy):

| policy (floor) | correct accepts | **false accepts** | correct doubts | **false doubts** |
|---|---|---|---|---|
| `routing` (0.40/0.08/0.50) | 7 | 2 (S3, S6) | 0 | 2 (S4b, S5) |
| `spawning` (0.55/0.20/0.70) | 7 | 1 (S6) | 1 (S3) | 2 (S4b, S5) |
| `merging` (0.70/0.40/0.85) | 6 | **1 (S6)** | 1 (S3) | 3 (S4b, S5, S7) |

The confidence-only sweep and the real 3-part policies agree almost exactly
in this sample (mass and margin moved with confidence throughout, so they
add little discriminating power here) — the one place they diverge is that
`merging`'s real confidence floor (0.85) is *below* my 0.90 sweep point, and
S6 (confidence 0.88) sits in that gap: **a floor at or above 0.90 would have
excluded S6; the shipped `merging` floor of 0.85 does not.** That is the one
concrete, data-backed suggestion this frontier sketch supports — raise
`merging`'s confidence floor toward 0.90, or add the tripwire noul as a
second required check (`covered_yes ≥ 0.6`, say) before trusting a gate
`accept`. I did not tune anything to produce this number; it is one wrong
case in eleven, and n = 11 means this is a direction to test further, not a
threshold to ship.

**Outcome**: worked — and this is the "interesting failure" the survey
asked about: a real, minimal case (S6) where the actual production policy
(`merging`) accepts a wrong answer with every one of its three floors
cleared, alongside a real, controlled confirmation (S3/S10) of the skill
file's own stated mechanism for why a checklist gate misses things.

**Stretch of agent work absorbed**: this replaces the entire "did the child's
candidate actually satisfy its contract" judgment call a Sol/Luna node makes
by hand for every reviewed candidate (node 10 in run 7 hand-rolled exactly
this check three separate times after `gateFor` itself broke) — with the
caveat, now measured rather than assumed, that the mechanical gate alone is
not yet safe to trust unattended at its shipped `merging` floor.

## Friction

The workspace lock (`another process already owns this binding root`) is
shared per literal workspace path, not per session name — three concurrent
launch attempts against `~/dev/shoal-evals/tui-test-app` (mine included)
failed while another lab session held it. The coordinator redirected me to a
dedicated clone at `/home/inanna/.claude/jobs/4940a626/tmp/ws8`, which
launched cleanly on the first attempt once the workspace was free of that
lock. Also: `shoal proxy` truncates a large cell result mid-value with
`[display continues: cellDisplay.more]`; submitting a follow-up cell whose
entire content is `cellDisplay.more` reads the retained remainder without
re-running the original effect — needed once, for experiment 60's larger
JSON payload, not needed for experiment 61 once I trimmed the final `pure`
to only the small aggregates.
