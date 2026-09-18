# Sync: breadth survey of Jev patterns, 2026-09-17

Sixteen experiments, run live against real artifacts in four parallel Shoal
sessions. Every number here came from a real call; the raw distributions are in
`NOTEBOOK.md` and `NOTES-lab{6,7,8}.md`, and every cell reruns.

Each claim below is labelled: **documented** means a published TypeSafe
pattern, **ours** means our recomposition, **observed** means what the run did.

## The headline

**Typed semantic action selection drove a multi-step investigation.** A
dispatcher whose alternatives carry real `git` commands as payloads fetched the
commit message, then the definition's diff, then every call site in the tree,
in three steps with no model turn between them. It never repeated an action.
That is the evidence chain a human had to hand our Sol agent yesterday.

## The five results worth carrying forward

**1. Alternatives phrased as conditions on state make a fetch loop terminate
its own branches.** Each alternative named what `observations` did not yet
contain, so satisfying one removed its own reason to be chosen: the commit
message alternative fell 0.59 → 0.29 → 0.05 as its text arrived. This was not
designed in; it fell out of following the wording rule.

**2. Self-fetched evidence recovers the right answer but not the confidence to
act on it.** The same repair-strategy choice, three states, nothing else
changed:

| state | key | mass | confidence | accepted under `merging` |
|---|---|---|---|---|
| nothing | `insufficient_evidence` | 1.00 | 0.99 | refuses, correctly |
| 1519 bytes the program fetched itself | `callers_catch_up` | 0.75 | 0.63 | held back by the policy |
| one sentence of stated intent | `callers_catch_up` | 0.97 | 0.94 | accepted |

Gathering evidence turns a flat refusal into a correct but unconfident answer.
Stating the intent turns it into an actionable one. **They are not
substitutes**, which sharpens rather than overturns the earlier lesson.

**3. A branch is judged on the text you put in front of it, not on what it
stands for.** A traversal choosing between five source files by filename picked
the wrong one at 0.51 against 0.47. The same traversal with each file's real
first forty lines as its description picked the right one, and the previous
winner fell to 0.18. The calibration worker hit the same wall from the other
side: raising a pool preview from 200 to 1200 characters moved answers by 0.25
to 0.32, because the deciding sentence sat just past the cut. **Truncation
silently deletes the deciding fact and nothing in the answer says so.**

**4. A confidence floor cannot detect ambiguity that is absent from the
evidence.** Three-way routing handled a lint and a missing-match-arm failure
correctly and confidently. On the third, the failure where two repairs genuinely
compete, it answered `mechanical` at 0.86 mass and 0.81 confidence, which every
one of our three named policies would have acted on.

Asking the structural question directly did not rescue it either: over
diagnostics alone, "would reverting the definition fix this" scored 0.42, 0.42
and 0.22, with the ambiguous case **lowest**. The reason is now clear. Whether
a failure admits two repairs is not a property of its diagnostics; it is a
property of the change that caused it. A program that wants to know when to ask
a person must fetch the diff first, and once it has the diff it can ask the
strategy question directly and needs no separate trigger. The dispatcher in
result 1 already fetches exactly that.

**5. A packet can lint its own questions before sending them.** Against our own
measured A/B of weak and strong wordings, one axis separated them: "states the
deciding fact" averaged 0.45 for measured-strong questions against 0.20 for
measured-weak. It independently flagged both live failures from my runs, the
dead comparison question at 0.19 and the belief-shaped completion question at
0.41. The backtick-naming axis showed no separation and should be dropped.

## Measurements against published claims

| claim | ours |
|---|---|
| 13 questions in one packet are 10.0x faster than sequential | **6.98x** (317 ms against 2213 ms). Answers agreed between the two runs, so the speed was not bought with worse answers. |
| batch size degrades pooled answers | **not in our range.** Two ground-truth anchors held at 0.82/0.79/0.81 and 0.04/0.04/0.04 across pools of 5, 15 and 30; max spread 0.03. Over 300 real commit subjects in 12 requests, anchors moved at most 0.09. |
| don't trust vendor thresholds, shadow-test locally | **done, and it found a live one.** 9 of 11 labelled situations from a real past run were classified correctly. The miss is the one that matters: a candidate carrying **no diff at all** was called `item_missing` at mass 0.91, margin 0.82 and confidence 0.88, clearing every floor of our shipped `merging` policy, so the production accept function would have accepted it. n = 11, so this is a direction to test, not a threshold to ship. |

## The gate's own stated weakness, reproduced under control

Our skill file claims that a checklist gate misses a condition no alternative
names, and that writing the likely-miss condition into the option text fixes
it. A paired test on an identical diff, changing only that sentence, moved the
gate from **0.60 confidence and wrong** to **0.99 confidence and right**. The
claim was written from a single field observation; it now has a controlled pair
behind it.

The same experiment found the warning signal we already ship and do not act on:
on the false accept above, the tripwire noul read 0.48 against 0.87 to 0.93
everywhere else. The gate ignored it. Requiring the tripwire alongside the
choice is a smaller change than moving a threshold, and the data supports it.

## Two failures that taught rules

**Mixed vocabulary inside one choice breaks it.** Four alternatives describing
artifacts plus one describing a belief: the odd one out scored 0.01 and never
won even when true. Rewriting it in artifact vocabulary moved it to 0.39 with
nothing else changed.

**Pruning satisfied alternatives made the judgment worse, not better.** Cutting
a five-option menu to the two still-live options dropped the correct option
from 0.39 to 0.15. Untested hypothesis: alternatives scoring near zero are
themselves evidence about the state. This contradicts the obvious code-side
optimisation and is worth one deliberate experiment.

## Still unresolved

- **Autonomous termination was never demonstrated.** No form of the stop
  question produced a run in which the loop stopped itself. The best in-choice
  completion reached 0.39; a separate noul scored worse, at 0.21. One of my own
  concrete wordings contained a conjunct that was false of the state, so the
  ceiling may be my question rather than the pattern.
- **Position bias is unmeasured.** The fixture chosen was unambiguous and every
  rotation landed the same key at mass 1.0, leaving no headroom. Honest null,
  not evidence of absence.
- Whether a beam that keeps two paths alive would rescue a name-only traversal.
  Adding evidence fixed that case outright, so the beam went untested.

## Three engine defects found by use

1. A cell declaration silently collides with a stdlib name; the error surfaces
   at the first *use* and names a generated module, so it reads as an engine
   fault rather than a name clash.
2. The display budget is spent by the evidence a cell sends, not by the result
   it returns. A cell that routed one artifact rendered fine; the same cell over
   three committed, made and paid for its model calls, and then showed six
   characters of its answer. Nothing says which binding consumed the allowance.
3. The Shoal workspace lock is per workspace path, not per session, so parallel
   sessions need separate clones. Three concurrent launches failed on this.

## What I would do next

The compositions that earned another look are the dispatcher and the
content-carrying traversal, and they compose: fetch the diff with the
dispatcher, then ask the strategy question, then route. That sequence is
supported by results 1, 2 and 4 together and is the one thing here I would put
in front of a real task.

I would not spend another wave on wording. The question-linting packet gets
most of that benefit for one extra request.
