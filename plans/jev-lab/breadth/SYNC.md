# Sync: breadth survey of Jev patterns, 2026-09-17

Seventeen experiments, run live against real artifacts in four parallel Shoal
sessions. Every number came from a real call; raw distributions are in
`NOTEBOOK.md` and `NOTES-lab{6,7,8}.md`, and every cell reruns.

**Read the audit section first.** Two of my own negative results turned out to
be defects in how I wrote the questions, not findings about the patterns, and
they are retracted below. Every remaining negative carries an explicit verdict:
whether it is evidence the pattern does not work, or evidence that we did it
wrong.

## The single most useful finding

**Every alternative in a choice must describe the same kind of thing, and that
thing must be something a reader can check against the state.** Measured three
times, in three unrelated experiments.

Scope it carefully: uniform content-shaped alternatives repaired **this**
routing program on **these three fixtures**. That is a demonstrated repair, not
a law about the model. Missing evidence is a separate and independent failure
mode, and the audit section shows it was operating in this same notebook at the
same time. Two distinct causes were in play; fixing one does not retire the
other.

| case | odd-one-out wording | uniform wording |
|---|---|---|
| a completion option inside a menu of fetch options | 0.01 | 0.39 (also evidence-starved; see audit) |
| routing an ambiguous build failure | **wrong** at mass 0.86 | **right** at mass 1.00 |
| our own past question wordings, scored by a lint packet | 0.20 | 0.45 |

The cost of breaking this rule is not a weak signal you notice and investigate.
It is a confident wrong answer that clears every policy floor we ship. Both of
my retracted negatives were this defect, and I did not see it in my own code
until I went looking a second time.

## Results

**1. Typed semantic action selection drove a multi-step investigation.** A
dispatcher whose alternatives carry real `git` commands as payloads fetched the
commit message, then the definition's diff, then every call site, in three
steps with no model turn between them. It never repeated an action: each
alternative named what `observations` did not yet contain, so satisfying one
removed its own reason to be chosen, and the commit-message option fell
0.59 → 0.29 → 0.05 as its text arrived.

**2. Three-way branching works on our workload.** Three real failed builds,
correct destination known independently beforehand.

| fixture | what it is | key | mass | confidence |
|---|---|---|---|---|
| `4610b5e` | a lint promoted by `-D warnings` | `mechanical` | 0.96 | 0.95 |
| `f726882` | five uncovered match arms | `needs_writing` | 0.86 | 0.80 |
| `53ad43c` | two repairs genuinely compete | `needs_intent` | **1.00** | **1.00** |

The case that needs a person is identified from the diagnostics alone, at mass
and confidence 1.00. Each branch then runs a different real program.

**3. Evidence a program fetches itself is not a substitute for stated intent.**
Same repair-strategy choice, three states, nothing else changed:

| state | key | mass | confidence | under `merging` |
|---|---|---|---|---|
| nothing | `insufficient_evidence` | 1.00 | 0.99 | refuses, correctly |
| 1519 bytes it fetched itself | `callers_catch_up` | 0.75 | 0.63 | held back |
| one sentence of intent | `callers_catch_up` | 0.97 | 0.94 | accepted |

Gathering evidence turns a refusal into a correct but unconfident answer.
Stating intent turns it into an actionable one.

**4. A branch is judged on the text you put in front of it.** A traversal
choosing between five source files by filename went to the wrong file at 0.51
against 0.47. The same traversal with each file's real first forty lines as its
description landed exactly right, and the former winner fell to 0.18. Reached
independently by the calibration worker from the other side: widening a pool
preview from 200 to 1200 characters moved answers by 0.25 to 0.32, because the
deciding sentence sat just past the cut.

**5. A packet can lint its own questions before they are sent.** Against our own
measured A/B of wordings, "states the deciding fact" averaged 0.45 for
measured-strong questions against 0.20 for measured-weak. It independently
flagged both live failures from my runs. The backtick-naming axis showed no
separation and should be dropped.

**6. Shadow testing found a real misrouting, and a signal we ship and ignore.**
Over 11 labelled situations from a real past run, 9 were classified correctly.
The miss: a candidate carrying no diff at all was called `item_missing` at mass
0.91, margin 0.82, confidence 0.88, clearing every floor of `merging`.

I first wrote this up as a false accept. **That overstated it, and our own skill
file says why**: `Right` from `J.accept` means the winning key cleared the
floors, never that a candidate was approved, and both `item_missing` and the
correct `insufficient_evidence` are non-merge outcomes. No bad code merges
under either. The real cost is a wasted round trip: `item_missing` sends a
repair request to the child, while `insufficient_evidence` should have sent the
parent to fetch the diff it failed to collect. A child gets blamed for the
parent's omission.

What survives unchanged, and is the more useful half: the tripwire noul read
0.48 there against 0.87 to 0.93 elsewhere. The warning was present and unused.

**7. The gate's own documented weakness, reproduced under control.** Our skill
file claims a checklist gate misses a condition no alternative names. A paired
test on an identical diff, changing only that sentence, moved the gate from
0.60 and wrong to 0.99 and right. The claim previously rested on one field
observation.

## Measurements against published claims

| claim | ours |
|---|---|
| 13 questions in one packet are 10.0x faster than sequential | **6.98x**: 317 ms against 2213 ms. Answers agreed between the two runs, so the speed was not bought with worse answers. |
| pooled answers degrade as the batch grows | **not in our range.** Two ground-truth anchors held at 0.82/0.79/0.81 and 0.04/0.04/0.04 across pools of 5, 15 and 30, max spread 0.03. Over 300 real commit subjects in 12 requests, anchors moved at most 0.09. |
| do not trust vendor thresholds; shadow-test locally | **done, and it paid.** See result 6. n = 11, a frontier sketch, not a calibration. |

## Audit: pattern failure, or our failure?

### Retracted, and now positive results

**Three-way routing was confidently wrong on the ambiguous failure.** *Our
failure.* My `needs_intent` alternative read "reports a conflict that two
different repairs would both resolve", describing a property of the repair
space, while its three siblings described the contents of the output. Rewritten
to describe contents, the same fixture goes from wrong at 0.86 to right at
1.00. I had measured this exact defect two experiments earlier and still wrote
it. **The earlier conclusion, that a confidence floor cannot detect ambiguity
absent from the evidence, is withdrawn.**

**A structural two-repairs test found nothing.** *Our failure.* Both questions
asked about a counterfactual ("would reverting fix this?") rather than about
what the text contains. The same error in a second form: asking about an
outcome the state cannot witness. **The conclusion that ambiguity is a property
of the change rather than of the diagnostics is withdrawn**; uniform content
wording answers it from diagnostics alone at 1.00.

### Still negative, verdict stated

**Autonomous termination was never demonstrated.** *Our failure, and not the
one I first recorded. Not yet tested.* I logged this as a wording defect. On
audit it is an evidence defect, and a self-inflicted one. `dispatch` returned
`T.take 700` of each fetch and the loop appended exactly that to
`observations`; the record of the definition's diff ends mid-line at
`-pub fn load(path: impl AsR`, before the line adding `limit: usize` and before
the line using it. **The decisive evidence never entered the state.** A
completion option describing evidence that is absent, answered low, is answered
correctly. The same holds for the follow-up probe, whose state was cut at 900
characters and whose completion condition also required a commit message
"naming the new parameter" when the real message names the behaviour.

So the honest position is that we have not yet asked a loop to stop while
holding what it needed to stop. That experiment is: untruncated observations,
and a completion condition that is true of the state once it is reached. This
is result 4 of this survey occurring inside the survey's own first experiment.

**Pruning satisfied alternatives made the judgment worse (0.39 to 0.15).**
*Confounded three ways; not evidence about pruning.* The option that won the
pruned menu was a fetch that was genuinely still unsatisfied, so its winning
may simply be correct; the completion option carried a conjunct that was false
of the state; and, per the audit above, the evidence it described had been
truncated away before the question was asked. Treat the pruning claim as
unmeasured. The
hypothesis worth one clean experiment is that alternatives scoring near zero
are themselves evidence about the state.

**Position bias measured zero spread.** *Our failure, and the fix is obvious.*
The fixture was unambiguous, every rotation landed the same key at mass 1.0,
and a saturated distribution has no headroom for position to move. Rerun on a
genuinely close pair.

**The skill-example lint could not separate clean from stale examples.**
*Probably our failure, untested.* The question asked, per fenced block, whether
it uses only shipped names, and it never fell below 0.68 even on blocks that
use only shipped verbs, because it cannot tell a qualified library call from an
ordinary local binding. Ask per extracted name against the shipped list, rather
than per block.

**A research-notes commit scored 0.85 on "would a user notice this".** *Our
failure.* The question excluded "an internal refactor, test change or
documentation edit", and research notes are arguably none of those three. The
exclusion list did not cover the case.

**The shadow-test false accept.** *Genuine, and it names a concrete fix.* Not a
wording slip of ours in the experiment, but quite possibly one in what we ship.
Our `insufficient_evidence` exit is written as "a file named in `diff_stat` has
no hunk", which does not describe a `diff_stat` that is empty. The situation
that fooled the gate had no diff at all, so the exit condition was literally
inapplicable and the gate fell through to `item_missing`. That is the same
class of defect as everything above, in production text rather than in an
experiment. Two candidate fixes: extend the exit to name the empty case, and
require the tripwire noul alongside the choice. The second is cheaper and the
data supports it.

**Fan-out measured 6.98x rather than the published 10.0x.** *Neither; a
different workload.* Thirteen cheap literal nouls over one small file, where
per-call overhead is a larger share of the total. Not a refutation.

## The real lesson

Not that wording is fragile. That **these small semantic programs are
debuggable, and a small repair unlocks substantially better behaviour**. One
sentence rewritten moved a routing decision from wrong at 0.86 to right at
1.00. The distributions told us where to look every time: a near tie pointed at
missing evidence per branch, an option stuck at 0.01 pointed at a vocabulary
mismatch, a flat answer pointed at a question about something absent from the
state. None of these needed a threshold change.

## What I would put in front of a real task

The dispatcher and the content-carrying traversal, composed: fetch the diff
with the dispatcher, then route with uniform content-shaped alternatives, then
ask the strategy question. Results 1, 2, 3 and 4 support that sequence.

I would not spend another wave on wording by hand. Result 5 gets most of that
benefit for one extra request, and this survey is itself evidence that authors
do not catch this defect by rereading their own packets.

## Three engine defects found by use

1. A cell declaration silently collides with a stdlib name; the error surfaces
   at the first use and names a generated module, so it reads as an engine
   fault rather than a name clash.
2. The display budget is spent by the evidence a cell sends, not the result it
   returns. A cell that routed one artifact rendered fine; the same cell over
   three committed, paid for its model calls, and showed six characters of its
   answer. Nothing says which binding consumed the allowance.
3. The Shoal workspace lock is per workspace path, not per session, so parallel
   sessions need separate clones. Three concurrent launches failed on this.
