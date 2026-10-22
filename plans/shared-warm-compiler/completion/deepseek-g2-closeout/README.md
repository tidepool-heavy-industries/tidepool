# G2 closeout: complete the joined owners

Reviewed code: `29231ed3236bb4dd6d13b6e459315533cd171a3e`, incoming
documentation `5b3e15d7dc093fc7ea7f28f1592abfe65f150f36`, branch
`root/g2-final-20261021`. **Changes requested; G2 remains unqualified.**
This peer checkpoint updates plans, not the runtime.

The pasteable supervisor instruction is [KICKOFF](KICKOFF.md).
Start with [STATE](STATE.md), [REVIEW-29231ed3](REVIEW-29231ed3.md) and
[NEXT-WAVE](NEXT-WAVE.md). The current implementation contracts are
[PRIVATE-STATE](PRIVATE-STATE.md), [PACKAGE-CLOSEOUT](PACKAGE-CLOSEOUT.md),
[COMPILER-EVIDENCE](COMPILER-EVIDENCE.md) and [TEST-GATES](TEST-GATES.md).
[CONTRACTS](CONTRACTS.md) and the [G2 contract](../g2-completion.md) preserve
the cross-cutting requirements; these newer plans refine their implementation.

## Worker tree

One working supervisor reviews/integrates three primary owners:
N private lifecycle, P native selection/installation, C compiler evidence.
N delegates actor activation/cleanup after freezing its small boundary.
H repairs custody narrowing; T corrects production test premises; V runs
joined gates and eventually freezes the bundle. Independent reviewers inspect
actual production consumers of each consolidated cut. See
[SUPERVISOR](SUPERVISOR.md) for handoff and integration rules.

These are proposed external DeepSeek assignments. The four Sol peer reviewers
produced reports; this document does not assert DeepSeek workers are active.

Retain the live compiler, exact stage composition, current snapshots, fresh
transitive demand and useful tests. Finish their joins. No new architecture,
compatibility path, cache or generic lifecycle framework is needed.

Earlier REVIEW-* files are historical evidence. Old P/N/H/W/Q assignment files
are superseded work queues; do not revive them from their filenames.
