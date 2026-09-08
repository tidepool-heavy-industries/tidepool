# Launch operator checklist

Launch preparation only; this file is not a claim of acceptance or deployment.

- Record the exact committed seed, including both design plans and authored engine
  regressions, while preserving unrelated edits. Workers must see plans at source.
- Materialize the finished canonical `examples/shoal-workspace/.shoal` package
  into the original-root `.shoal`, preserving runtime artifacts. Capture once.
- Check that selection with the chosen fixed runner. The peer's 43 coordination
  checks used the previous runner; record new-run validation separately.
- Use immutable Shoal/native/extractor executables containing the steering repair.
  Record package identity, native pin and checks in the actual launch record.
- Launch a unique session with Sol; never recreate or resume an unrelated run.
  Seed the coordinator with the exact source and this wave's README. Initially
  commission readbacks only.
- Preserve run identity, tmux panes, existing run-map observations and opt-in
  bounded private traces for later visualization. Track actual models, forks,
  messages, compactions and usage where observable; missing coverage stays unknown.

Candidate code and prompt changes activate only at a later explicit swarm boundary.
Do not mutate running tools, add another logger or send raw event streams to Astra.
