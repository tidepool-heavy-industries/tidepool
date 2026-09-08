# Parallel dogfood wave

Status: preparing the Astra planner session; implementation has not started.

Deliver both existing designs: full interactive Codex applications (A0–A8) and
prepared-STG production execution (M0–M7). Reviewed partial slices are useful;
keep the remaining feature gates open.

A Shoal-managed Astra planner sets up the work with the human.
Its Sol coordinator owns integration; two Sol leads own the delivery branches.
Each lead implements, scaffolds shared decisions, forks useful Sol subtrees,
then integrates and repeats. Use Sol Low for bounded work, Medium for substantial
leads. Each lane names focused Astra consultations at consequential decisions.

## Read only what your assignment needs

| Reader | Next file |
|---|---|
| Astra planner | [Planner assignment](planner.md) |
| Coordinator | [Coordination](coordination.md), then the two short lane maps |
| Applications lead | [Applications](applications.md) |
| Engine lead | [Engine](engine.md) |
| Descendant | Assigned mechanism section and accepted shared contract |
| Launch operator | [Launch](launch.md) |

Fork from the useful common reasoning before loading unrelated mechanisms.
Link detailed evidence; do not copy whole plans into child packets. Follow the
selected workbench guidance rather than repeating its recipes here.

**Before implementation:** the planner commissions a coordinator, which admits only the
two leads. Delivery stays pending through review. The planner receives
consolidated Attention through Shoal, reviews the artifacts, and tells the coordinator when the plans are ready to implement. Routine local waves then proceed within that agreement.

The human selected parallel dogfooding, superseding the application plan's
sequential/non-dogfood execution restriction. Its semantic and acceptance gates
still apply. Running tools and `.shoal` stay frozen; candidate activation requires
a later explicit swarm boundary.
