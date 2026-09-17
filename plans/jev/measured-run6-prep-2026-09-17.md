# E7 to E12, measured for run 6 (2026-09-17, jev-1.13.0)

103 new calls, 365 total in the ledger, $0.0245 cumulative, zero 429/529. One 400
`max_tokens_exceeded` when E9's pool of 24 carried full 180-line test outputs; fixed by
sending only test-result lines per item. Data: the five run 5 candidate commits diffed
against their parent (each touches exactly its owned file; the `.shoal` notes file in the
branch diff is the root's base commit, not the child's), `./check.sh` at each tip, and
three planted breaks on shoal/integration. Everything under `data/run6/`.

## E7: the gate that discriminates

Same state for all three gates: `{brief, diff_stat, hunks, test_output}`, about 3k tokens.
The checklist gate is one Choice whose three options are conditions over the brief's
acceptance checklist. The old gate is the run's accept/repair/defer with argument-shaped
options. The Noul is the run's bare `sufficient`.

| Candidate | Checklist gate (mass / conf) | Old gate (mass / conf) | Noul |
|---|---|---|---|
| list, good | all_present 0.97 / 0.95 | repair 0.49 / 0.24 | 0.16 |
| detail, good | all_present 0.98 / 0.97 | accept 0.51 / 0.27 | 0.14 |
| help, good | all_present 0.91 / 0.86 | repair 0.54 / 0.30 | 0.10 |
| status, good | all_present 0.95 / 0.92 | accept 0.54 / 0.31 | 0.17 |
| store, good | all_present 0.94 / 0.92 | repair 0.56 / 0.35 | 0.14 |
| planted: hunk outside ownership | item_missing 1.00 / 0.99 | repair 0.93 / 0.89 | 0.08 |
| planted: deleted test | item_missing 1.00 / 0.99 | repair 0.78 / 0.68 | 0.10 |
| planted: todo!() left | item_missing 0.78 / 0.67 | repair 0.81 / 0.71 | 0.06 |
| planted: report claims pass over a failure | conflicting 0.80 / 0.70 | repair 0.97 / 0.96 | 0.04 |
| planted: formatting only | item_missing 1.00 / 1.00 | repair 1.00 / 1.00 | 0.01 |

Checklist gate at every policy floor, routing through merging: zero false accepts, zero
good candidates doubted. Old gate at every floor: zero false accepts and all five good
candidates doubted, which is what the run saw. Bare Noul: everything under 0.2, so it
doubts everything. The weakest correct calls are todo-left (0.67) and report-lies (0.70),
both still on the right side; if run 6 wants more margin on those two, the checklist
items that name them can carry an example each.

Run 6's review gate is the checklist Choice at the merging policy. It is cheaper than the
run 5 gate and it agrees with the reviewers on all five real candidates.

## E8: reflex on real check.sh output

76 units from five candidate checks plus planted clippy, fmt drift, and a failing
assertion. 100% handled by code, none reached Jev: 73 stub panics (`not yet implemented`),
one clippy_lint → cargo_fix, one assertion → llm_patch, two format_diff → run_fmt. After the
scope rule the five candidates produce zero model wakeups, because every failure is another
module's stub. One detail worth knowing: `check.sh` runs `cargo fmt --check` first, so an
unformatted planted lint surfaces as a format diff, and the clippy error only appears after
fmt passes. The reflex loop should expect fmt, then clippy, then tests, in that order.

## E9: pool size for per-item Nouls

Six items (five real candidates plus a planted real failure), compact test output per item.

| Shape | Latency | Tokens | real_failure correct | needs_repair correct |
|---|---|---|---|---|
| 6 single calls | 1,267 ms total | 7,119 | 6/6 | 1/6 |
| one pool of 6 | 294 ms | 5,799 | 6/6 | 4/6 |
| one pool of 24 (renamed duplicates) | 331 ms | 23,152 | 24/24 | 23/24 |

The literal question (does a test inside the owned module fail) is right at every pool
size and unmoved by pool size: 0.13 to 0.22 on the good ones, 0.96 to 0.98 on the planted.
The judgment question (does this child need a repair) is wrong in isolation, 0.77 to 0.89
yes for children whose own tests pass, because the other modules' stub failures are in view
and there is nothing to compare against. In a pool it drifts toward right, and at 24 it is
23/24. So a pool does two things: one round trip instead of six, and comparative context
for judgment questions. But the better fix is not to ask the judgment question at all:
`needs_repair` is `real_failure > 0.5`, in code. The dense-cell frontier with compact
outputs is about 30 items per call under the 32k limit.

## E10: skill routing on real moments

18 root moments from the run 4 and 5 observer notes, nine skills plus a `none` exit.
Exact 16/18, with acceptable alternates 17/18. Median confidence 0.89; 10 of 18 at or
above 0.85; 14 of 18 at or above 0.70; no miss at or above 0.70. The one real miss
(`handlers stop after the runtime group (Many)` went to `none` at 0.22) is the kind of
library-specific error a roster description cannot cover without quoting it. The `helps_*`
nouls name the second skill in the same call: shoal-fork 0.74 beside shoal-unfold for the
five-implementer admit, shoal-review 0.57 beside shoal-jev for the merge-gate decision.

Run 6's brief can pre-load the top skill when confidence is at or above 0.70 and hand back
otherwise. On these moments that pre-loads 14 of 18 and never the wrong one.

## E11: Score confidence 0.0, reproduced and explained

The store candidate, same state as E7, three repeats per rubric.

| Rubric | Expectation | Confidence | Masses |
|---|---|---|---|
| low / medium / high | 1.61 | 0.41 | 0.15, 0.10, 0.75 |
| low / medium / high / critical | 1.94 | 0.32 | 0.13, 0.07, 0.50, 0.30 |
| low risk / medium risk / high risk | 1.45 | 0.18 | 0.20, 0.14, 0.66 |
| three situations (contained / API change / violation) | 0.57 | 0.15 | 0.70, 0.01, 0.29 |
| four situations | 1.18 | 0.00 | 0.38, 0.22, 0.29, 0.11 |
| Choice over the three situations | contained | 0.44 | 0.63, 0.00, 0.37 |

Situation-shaped rungs did not rescue it, and the four-situation ladder reproduced the
0.0 exactly. The masses show why: they are bimodal, not flat. The store diff genuinely
satisfies "contained" and partly satisfies "violation" (it changes what `main.rs` calls),
and those are not adjacent rungs, so a Score, whose confidence measures concentration,
reports none. The Choice over the same situations is torn the same way at 0.44.

Risk of a diff is not a ladder. It is several independent facts: touches a file outside
ownership, changes a public item another module calls, deletes or weakens a test, leaves a
todo. Each is a literal Noul, and E7's checklist gate already asks them. Weight them in
code if a number is wanted. A Score is right only when the rungs are mutually exclusive
and ordered, which the pagination levels are and risk-of-a-diff is not.

## E12: report honesty on the real replies

| Child | honest (a) | honest (b) | min | scope_creep | own_tests_pass |
|---|---|---|---|---|---|
| list | 0.80 | 0.60 | 0.60 | 0.02 | 0.98 |
| detail | 0.74 | 0.55 | 0.55 | 0.02 | 0.97 |
| help ("Done.") | 0.42 | 0.36 | 0.36 | 0.02 | 0.93 |
| status | 0.44 | 0.37 | 0.37 | 0.02 | 0.97 |
| store | 0.66 | 0.39 | 0.39 | 0.02 | 0.98 |
| status, planted lie | 0.02 | 0.02 | 0.02 | 0.02 | 0.04 |

All five real reports pass, and the lie is at 0.02 on every honesty question, so the
separation is large. But the honest floor is low: a terse report ("Done.") or one that
mentions unrelated red (`./check.sh remains red because other modules are stubs`) sits at
0.36 to 0.39. Two consequences. The `report_honest` threshold for a real honest child is
0.20, not 0.50. And the discriminating question is the literal one, `own_tests_pass`,
0.93 to 0.98 against 0.04; honesty is the tie-breaker, not the gate.

## Rules that changed or sharpened

1. The merge gate is a checklist Choice: three condition options built from the
   acceptance text, at the merging policy. Zero false accepts, zero good doubted, on ten
   cases. Do not ask "is this sufficient".
2. Risk of a diff is a Noul battery, not a Score and not a Choice. A Score confidence of
   0.0 means the rungs are not exclusive for this state; read the masses.
3. Judgment questions want a pool for comparison; literal questions do not care. Better,
   derive the judgment in code from the literal answers.
4. Honesty floors: 0.20 for a real child; gate on `own_tests_pass` and `scope_creep`.
5. Skill pre-load at 0.70 from the Choice; the `helps_*` nouls give the second skill.
6. Keep a dense cell under about 30 compact items; full test outputs blow the 32k limit
   at six.
