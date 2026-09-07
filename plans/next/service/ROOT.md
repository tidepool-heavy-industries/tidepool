# Previous-wave service migration reference

This directory retains source contracts and review evidence from the earlier
controller-service/observer effort. It is **not an active implementation assignment**.
The current decision is to keep existing interactive Codex TUI execution and
steering. [NEXT.md](../../../NEXT.md) owns the implementation plan.

Do not redispatch the former service team, treat its acceptance matrix as a gate
for Haskell workers, move hosted forwarding into a new controller merely to
finish this plan, or replace worker TUIs with `codex observe`.

The delivered native controller and read-only observer components are real source,
but they do not establish an integrated migration with the same interactive TUI
UX. Preserve useful source and evidence without making their adoption a goal.

Historical entry points:

- [Native client handoff](native-client-handoff.md): the former proposed adapter
  and its native dependencies.
- [Preparation acceptance](root-preparation-acceptance.md): exact reviewed source
  and limited acceptance evidence.
- [Canonical pairing review](canonical-host-pairing-review.md): retained owning
  boundary findings and checks.
- [Wave closeout](../evidence/wave-closeout.md): what was integrated and what was
  unverified at closeout.

Production behavior and invariants belong to current owning source and
contributor guidance. Prior instructions about a service TL, root dispatch,
migration gates, or an observer-only TUI are historical, not current authority.
