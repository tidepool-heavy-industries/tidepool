# E15, E16, replay baseline, anchors, linter, GHC rows (2026-09-17)

Ledger at 530 calls, $0.03 cumulative, still zero 429/529.

## E15a: field ablation on the five skill packets

Drop each state field, keep the questions, measure what moves. Choice movement is reported
as confidence change or a flip.

| Packet | Field dropped | What moved |
|---|---|---|
| dense triage | task | keep_help, keep_status, keep_store by 0.11 to 0.32 |
| | previews | every keep_* by up to 0.26 |
| skill gate (diff_stat, test_output, review_scope) | diff_stat | conf 0.56 → 0.22, same choice |
| | test_output | flips to one_absent at 0.88 (correct) |
| | review_scope | flips to one_absent at 0.83 (correct) |
| continuation | test_output | flips to inspect_by_hand (correct) |
| pool cell | failure | rel_retry by 0.38; best unchanged |
| | descriptions | best flips fetch → retry at 0.72 |
| | premise | rel_retry by 0.14 |
| rule-2 checklist gate (brief, diff_stat, hunks, test_output) | brief | nothing: 0.86 → 0.86 |
| | diff_stat | nothing: 0.86 → 0.91 |
| | hunks | 0.86 → 0.57, same choice |
| | test_output | 0.86 → 0.68, same choice |

What to change in the skill:

- **The rule-2 gate does not read `brief` or `diff_stat`.** The checklist is already in the
  option text, so the brief is redundant, and the hunks carry the file headers, so the stat
  is redundant to the gate. Drop `brief` from the gate's state (about 400 tokens per call).
  Keep `diff_stat` in the packet only because the `#covered` tripwire and the code coverage
  check read it, not the gate.
- **The gate reads hunks and test output, and nothing flips when either is missing.** With
  hunks gone it still says all_present at 0.57 and with test output gone at 0.68. That is
  E14 again from the other side: the gate degrades gracefully but does not refuse. The
  coverage code check is what refuses, and it must run first.
- **The skill's own gate example scores only 0.56 intact** because its options say "the
  named paths" and the state names no paths. Give it `owned_paths` in the state and options
  that reference `diff_stat` and `owned_paths` by name; E7's gate, which does that, sits at
  0.86 to 0.97.
- **In the pool cell, `best` is fragile and the per-file nouls are not.** Drop the
  descriptions and `best` flips; rephrase the question and it flips (see the linter). The
  per-file "is this file on the path the timeout takes" answers barely move. Derive `best`
  in code as the argmax of the per-file nouls, and keep the Choice only when one file must
  win for a continuation.

## E15b: question linter (two phrasings, one call; delta ≥ 0.10 is a judgment question)

| Packet | Question | Tag | Delta |
|---|---|---|---|
| dense triage | enough (preview sufficient?) | literal | 0.06 |
| | keep_* (worth reading in full?) | judgment | 0.11 to 0.21 |
| skill gate | gate | literal | 0.00 |
| continuation | cont | literal | 0.00 |
| pool cell | best (which file explains the timeout) | judgment | flips fetch ↔ retry, 0.90 vs 0.37 |
| | rel_fetch, rel_store (on the path?) | literal | 0.01 to 0.08 |
| | rel_retry | judgment | 0.14 |
| | fixed (under the premise) | literal | 0.02 |
| rule-2 gate | gate | judgment | flips all_present 0.84 → item_missing 0.54 |

The last row is the one to learn from. The alternate phrasing shortened item_missing to
"Some checklist item does not hold", and the gate flipped to it. The enumeration inside the
item_missing option, naming each way an item can fail, is load-bearing. Rule for the skill:
every option in a checklist gate enumerates its conditions; a vague option attracts mass.

Before/after pairs for the skill, all measured:

- "Which file explains the timeout?" (flips with phrasing) → "Is `files.<k>` on the path
  the timeout takes?" per file, argmax in code (stable).
- "Is the report sufficient?" (0.47 to 0.54, uninformative) → the three-condition checklist
  Choice (0.95, zero variance).
- "Some checklist item does not hold" (attracts 0.70) → "At least one item does not hold: a
  changed file outside the owned file, a failing or missing owned test, a deleted or weakened
  test, a remaining todo!(), or an implementation that does not match the goal" (0.08 on a
  clean candidate, 1.00 on a planted one).
- Risk as a Score with "low / medium / high" (confidence 0.0 to 0.4) → four literal Nouls
  (touches outside ownership, changes a public item, deletes a test, leaves a todo) weighted
  in code.
- "Does this child need a repair?" alone (0.77 to 0.89 yes on clean children) → derived from
  the literal `real_failure` and `scope_creep` in code.

## E16: when no offered option fits (Astra's question)

Four states whose options all describe something else. A: no exit. B: an exit written as a
condition. C: a bare "None of the above". Four repeats each.

| State | A no exit | B described exit | C bare other |
|---|---|---|---|
| one failing assertion, options are other test shapes | rerun_suite at conf **0.94** | none 1.00 | other 0.96 |
| rustfmt diff, options are test classes | test_assertion 0.50 | none 1.00 | other 0.98 |
| clean candidate, options are violations | own_test_fails 0.42 | none 0.44, conf 0.28 | split 2/2, conf 0.19 |
| gitignore error, options are unrelated skills | shoal-cleanup 0.68 | none 0.74 | other 0.75 |

Without an exit, Jev does not reliably expose misfit: twice it spread (0.42, 0.50), and
twice it picked the least-wrong option confidently, once at 0.94. So a model authoring its
own alternatives without an exit will sometimes get a confident wrong answer, and E1
already showed the same thing from the other side (a 1.0 means nothing else competes). With
a described exit it took the exit on all four, at 1.00, 1.00, 0.74, and 0.44. The 0.44 case
is a clean candidate asked which violation it shows; the described exit still won but under
every floor, which is the right outcome: a question that presupposes a violation cannot be
answered confidently about a clean state. The bare "other" was weaker than the described
exit in every case and lost outright in that one. Rule: every Choice a model authors gets
an exit, and the exit is a condition, not "none of the above".

## Replay baseline (standing script)

`replay.py --model <id> [--filter prefix] [--limit N]` replays ledger requests and diffs
the answers, writing `replay-<model>.jsonl`. Baseline, same model against itself, 14
requests and 130 answers from the E7 prefix:

| Type | n | mean Δ | p95 Δ | flips |
|---|---|---|---|---|
| noul | 10 | 0.006 | 0.02 | 0 |
| choice (confidence) | 20 | 0.040 | 0.18 | 0 |
| score | 100 | 0.022 | 0.08 | 2 rounding flips |

That is the noise floor. When `jev-preview` moves, run `replay.py --model jev-preview` and
treat any choice flip, or a mean noul Δ above 0.02, as a version change to review before
`jev-latest` is allowed to follow. The full ledger replays for about three cents.

## Anchors for the per-child triage pool

`anchors.txt` holds both items verbatim (compact test output, the shape the scope rule leaves).
ANCHOR_CLEAN: `src/panels/list.rs`, list's real run 5 output, 5 own tests pass and 13
other-module stubs fail; expected `real_failure` about 0.15, `scope_creep` about 0.02.
ANCHOR_FAILING: `src/panels/status.rs`, the planted assertion flip, 17 pass and one own test
fails; expected `real_failure` about 0.97. Put both in every triage pool. If either drifts
past 0.5 the pool is wrong before any real item is read.

## GHC rows added to the reflex table

Seven message-keyed rows ahead of the plain GHC code rows (`reflex_table_flat.json`, 85
rows): Text-vs-Label and String-vs-Text mismatches (both GHC-83865, distinguished by
message), ambiguous type variable (GHC-39999 with Contains Replies, Render, or Show),
Render overlap (GHC-43085), ZonkAny in the wrapper (GHC-76037), the hidden
`Jev.Core.Schema` type (GHC-76037), and the `handlers stop after the runtime group (Many)`
prepare error. Each carries the rewrite to show in the cell error. The first two were
verified against GHC 9.12 today; the rest against the literal run 4 and 5 messages.
