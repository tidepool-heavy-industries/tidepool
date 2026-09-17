# E13 and E14: confidence is not evidence coverage (2026-09-17)

Astra's claim, measured: a gate can be confident on a state that omits the thing it should
have seen. Both experiments say yes, and they say where the fix belongs. 37 calls; ledger at 402.

## E13: parent-derived versus child-reported state

Checklist gate at the merging policy, same packet as E7, plus coverage Nouls. A = `diff_stat`
and `hunks` from the parent's own `git diff <parent>..<oid>`. B = stat rebuilt from the files
the child reported and hunks restricted to those files.

| Case | A (parent-derived) | B (child-reported) |
|---|---|---|
| five real candidates | all_present 0.87 to 0.95 | all_present 0.88 to 0.95 |
| planted: commit touches a second file, report omits it | item_missing 1.00 | **all_present 0.95** |
| planted: report lists a file the commit did not touch | all_present 0.94 | all_present 0.71, hunks_cover_stat 0.17 |

The child-reported variant passed a case the parent-derived one failed, at 0.95. Once is
enough: the brief says parent-derived only. The coverage Nouls cannot catch this one
(0.98 both directions), because the reported stat and the restricted hunks are consistent
with each other; the omission happened before the state was built. The second planted case
is caught, by the stat naming a file with no hunk (0.17) and by the gate's confidence
dropping under the floor, but that is the easy direction.

## E14: omitted hunks with the stat intact

| Variant (five candidates) | Gate verdict | Gate confidence | hunks_cover_stat | counts_match |
|---|---|---|---|---|
| intact | all_present | 0.85 to 0.95 | 0.98 | 0.72 to 0.85 |
| all hunks removed | all_present | 0.62 to 0.82 | 0.12 to 0.13 | 0.11 to 0.15 |
| one hunk of several removed | all_present | 0.82 to 0.94 | 0.97 to 0.98 | 0.72 to 0.86 |
| extra stat line with no hunk | item_missing | 0.97 to 0.98 | 0.16 to 0.20 | 0.32 to 0.42 |

Three things.

- With every hunk gone the gate still says all_present, at 0.62 to 0.82. Two of five sit
  under the merging floor by only a little and list sits at 0.82. Confidence measured the
  distribution over three options given what was in view; it did not measure what was
  missing. This is the dangerous case, and it is real.
- The file-level coverage Noul catches every file-level omission, 10 of 10, at 0.12 to 0.20
  against 0.98 intact. It costs nothing in the same call.
- Partial omission within a file is invisible to Jev: one hunk removed, coverage 0.97, counts
  0.72 to 0.86 unchanged from intact. That is the documented counting limit. Only code can
  check that the stat's insertion and deletion counts match the hunks, and it is arithmetic.

## What goes in the brief

1. The gate's state is assembled by the parent from git, never from the child's report.
   `git diff <parent>..<oid> --stat` and the hunks from the same command, in one step, so
   coverage is total by construction. The child's `files_changed` is a claim to check
   against the stat, not an input to the gate.
2. Coverage is a code check before the packet is sent: every stat file has a hunk, and the
   stat's counts equal the hunk line counts. If either fails, the gate is not asked.
3. Keep `hunks_cover_stat` in the packet anyway, as a tripwire for a state assembled by
   hand or by a helper. It is free and it caught every file-level gap.
4. Never read a confident `all_present` as "the evidence was complete". Confidence is about
   the options in view. Completeness is the parent's job and it is a git command.
