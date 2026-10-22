# G2 closeout: finish the current private/native join

The controlling code checkpoint is `5c759554c6f9478bc2344ef17ee026c6ec23fed0`,
handed over at `3ed7a15c83998e93a31a768d2ae2bd42bcc5c59a` on
`g2-closeout/deepseek-20261021`. This pack's new revision is a review and plan,
not a runtime repair or G2 acceptance.

Start with [STATE](STATE.md), [REVIEW-5c759554](REVIEW-5c759554.md), and
[NEXT-WAVE](NEXT-WAVE.md). [CONTRACTS](CONTRACTS.md) and the
[G2 contract](../g2-completion.md) remain controlling. Concrete private-state
semantics are refined by [PRIVATE-STATE](PRIVATE-STATE.md); exact native/package
ownership by [PACKAGE-CLOSEOUT](PACKAGE-CLOSEOUT.md).

The direction remains B: bounded correctness/quality repair, with C research
separate. Keep the existing live compiler and its useful integrated work.
Resolve the current joins instead of adding more stale-check exceptions.

## Worker tree

Use one working supervisor, two working implementation owners and bounded leaves:

| Owner | Assignment |
|---|---|
| S | [SUPERVISOR](SUPERVISOR.md): design decisions, integration and actual evidence |
| N | Private-state progression; owns runtime admission/resident/publication hunks |
| N actor leaf | Activation and caller migration after N publishes the small interface |
| P | Exact native/package row through certification, completion and installation |
| P producer leaf | Target-local transitive HOME demand, after P's coordinate contract |
| H | The diagnosed support-projection custody loss, in functions coordinated with P |
| T | Production-admission histories; remove test-only authority shortcuts |
| R-N / R-P | Independent review of the two joined areas and refusal paths |
| V | [V-qualification](V-qualification.md): joined gates, map and matched Persona bundle |

These are proposed assignments for the external DeepSeek harness, not observed
running workers. Reuse context-loaded owners where possible, with new exact bases
and acknowledged source-hunk ownership. A parent should implement/review its own
area, not exist only to route messages. Resource admission depends on measurements,
not worker headcount. No native Codex goals.

## Milestones

1. Current private snapshot, accepted-result reconciliation and exact program
   progress; package/native support reaches actual runtime installation.
2. Activation, retirement/freeze, source-selection composition and required joined
   notebook/discovery/reload histories pass on the same tip.
3. One matched frozen bundle; actual Persona selection/tools/initial memory;
   bounded DeepSeek multi-turn smoke, then fresh user conversation.
4. G3 force/fork/callback/recovery matrices, full bundle gates, map/timing evidence
   and reviewed release/main convergence.

See NEXT-WAVE for discriminators and scope. G2 is explicitly unqualified until
its actual acceptance passes. Component counts, review reports and progress to a
later error are not replacements.

Earlier REVIEW-c910d1 and REVIEW-WIP-5c759554 preserve provenance. Earlier P/N/H/W/Q
assignment files are historical reference, not parallel work queues. Do not
restart completed occurrence-format, witness, logging or owner-census migrations.
Preserve their guarantees and check the joined consumers.
