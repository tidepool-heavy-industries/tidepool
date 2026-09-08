# Launch operator checklist

Launch preparation only; this file is not a claim of acceptance or deployment.

- Record final current-wave coordinator/lane checkpoints, separate native source,
  unmerged candidates and exact remaining failures. Select the new source including
  these plans; do not recycle an earlier commission hash or old actor identity.
- Finish saving and settling the winding-down wave before replacing its canonical
  package. Preserve its committed handoffs and runtime evidence.
- Materialize the finished canonical `examples/shoal-workspace/.shoal` package
  into the original-root `.shoal`, preserving runtime artifacts. Capture once.
- Check the selected package with a separate persistent compiler. Candidate
  fe3b0550 passed 49 recipe assertions before the subsequent prose review; that
  evidence is historical. Record checks and identity for the actual new selection.
- Use immutable Shoal/native/extractor executables containing the steering repair.
  Record package identity, native pin and checks in the actual launch record.
- Launch a unique session with Astra High and the selected planner root prompt;
  leave Sol as the worker default. Never recreate or resume an unrelated run.
  Give the planner the exact launch record and next-wave/commission.md. It
  commissions the Sol tree; implementation starts after it checks the Sol execution plans.
- Preserve run identity, tmux panes, existing run-map observations and opt-in
  bounded private traces for later visualization. Track actual models, forks,
  messages, compactions and usage where observable; missing coverage stays unknown.

Candidate code and prompt changes activate only at a later explicit swarm boundary.
Do not mutate running tools, add another logger or send raw event streams to Astra.
