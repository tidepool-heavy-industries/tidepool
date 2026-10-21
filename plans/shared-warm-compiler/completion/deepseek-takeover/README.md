# DeepSeek takeover: close G2 without another compiler redesign

Reviewed 2026-10-21. This directory is the next-wave execution plan. Read
[STATE.md](STATE.md) before assigning work: G2 is **not accepted**. The user wants
DeepSeek workers to continue from this review, preserving OpenAI budget for
later architectural judgment. Do not start another Sol implementation wave.

The existing [G2 contract](../g2-completion.md) still owns acceptance. This pack
supersedes older status paragraphs and work sequencing, not the underlying
authority, publication, cancellation or historical-dependency contracts.

[Cause analysis and diagnostic decisions](06-cause-analysis.md) records what the
staff review established, what remains a hypothesis, and the observation that
distinguishes each plausible cause. Read it before treating a guard as the bug.

## Outcome and architectural judgment

Deliver one matched frozen bundle that supports the real Persona profile,
ordinary notebook cells, discovery, retained values and compatible reload, with
the required cold/eviction/cutoff controls and a bounded real DeepSeek smoke.
Preserve authored files and memory; start fresh conversation state.

The compiler/runtime direction is viable: the complete finite history executes
expressions, retained callbacks and producer removal. The remaining failures
expose incompletely closed ownership boundaries, excessive simultaneous stack
residency, and test oracles aimed at the wrong projection. Finish those boundaries;
do not restore the old compiler path, invent another planner, or weaken refusals.

Keep these distinctions explicit:

- Retained custody is not selection, and selection is not executable demand.
- Full binding plus the issued native witness carries authority. An outer
  artifact wrapper may change during custody merging; consumers still must use
  the final acquired wrapper consistently.
- Public facade visibility is not the compiler World's primary implementation
  selection. Historical execution follows issued edges, not a latest-module map.
- A small boxed-future handle does not prove a small native poll stack.
- Compilation, executed component tests, joined histories, frozen qualification
  and deployment are different evidence.

## Worker tree and ownership

Use one DeepSeek supervisor, four implementation leaves and one independent
reviewer. The supervisor owns integration, documentation, resource admission and
final qualification. Leaves return exact base-to-tip candidates; they do not
merge into the shared integration branch or change the live Persona themselves.

| Leaf | Primary assignment | Adjacent review questions |
|---|---|---|
| A | [Close compiler acquisitions](01-acquisition-closure.md) | Duplicate refresh work; restore bypasses; precise identity-refusal evidence |
| B | [Separate HOME from package custody](02-home-identity.md) | Index replacement/removal; overlapping owners; unchanged generic observations |
| C | [Bound initialization stack by phase ownership](03-initialization-stack.md) | Affine payload lifetime; cancellation/cleanup; unnecessary copies |
| D | [Repair and complete production histories](04-history-oracles.md) | Wrong-projection oracles; Haskell fixture construction; real eviction evidence |
| Reviewer | Review exact A/B/C/D candidates and joined consumers | Surviving bypass APIs; unsupported claims; tests that never reach their stated boundary |

A primarily owns compiler_projection.rs and request closure. B owns the inventory
index and the narrowly named HOME predicate in certified_products.rs. C owns actor
initialization/publication phase control. D owns tests and their fixture files.
Coordinate any shared-file edit explicitly; do not resolve conflicts by dropping
another parcel's refusal or test. No additional worker is needed just to poll.

## Waves and integration

1. Verify the clean source base and terminal operation state in STATE. Create a
   closeout branch from the documentation checkpoint containing source 655f049a;
   do not branch from the older canonical/GitHub tip. Preserve the existing warm
   checkout and outputs, but change its source only while its jobs are settled.
2. Start A, B and C independently. D can correct the known query fixture and
   design the cold/eviction assertions while their production dependencies run.
   Compile each owner's library **and affected test consumers** early.
3. Review A/B together as an ownership tranche, then replay one actual notebook
   case and the cold history before accumulating more dependent changes. The
   Prelude refusal has not yet been causally tied to A; preserve that uncertainty.
4. Review C's exact phase transfer and run unchanged default-stack Discovery.
   Merge reviewed work into the closeout branch; run the complete ordinary gate
   set on the joined source rather than repeating a full bundle for each leaf.
5. Complete D's required cold/eviction evidence, reconcile the map and ledger,
   then follow [qualification and delivery](05-qualification.md).

Every handoff includes base, tip, owned paths, actual commands/counts, artifact
hashes, first failure, cleanup status and remaining uncertainty. Parents review
the cumulative diff, send concrete corrections, then integrate. Children notify
their parent on completion or blockage; parents continue the unblocked frontier.
Use completion events or available agent waits, not busy polling or a native goal.

## Scope and stop conditions

Internal API breakage that removes invalid intermediate states is allowed.
No on-disk format change is planned; a newly necessary one needs an explicit
cutover proposal. Remove replaced entry points rather than retaining backdoors.

Finish unblocked work and leave a concrete handoff if progress requires changing
authority semantics, a second execution route, a new scheduler, or abandoning
exact historical dependencies. Do not guess through that boundary. A new local
fixture or implementation defect within the existing contract can be repaired
with an independent regression and review.

G3's full force/fork/callback/process-recovery matrix and full M1-M3 remain
separate. Required G2 historical cold/eviction, ordinary reload, caller-loss and
uncertain-END controls are **not** deferred by that distinction.

## Deferred work is retained, not forgotten

After G2, examine repeated Haskell SOURCE validation/capture and phase hydration
using production measurements. Morning review found repeated work, not a measured
speedup. Also retain the SOURCE/boot adjacency asymmetry, definer/export ambiguity
negative controls, observation memo-versus-envelope failure ordering, optional
World-delta no-op optimization, and unproven canonical/Original closure audit lead.
These do not justify another speculative implementation tranche during closeout.

Old ledgers and docs candidate b6682b3c1f are useful inputs, not current truth.
Reconcile them with terminal receipts; do not blindly cherry-pick stale status.
