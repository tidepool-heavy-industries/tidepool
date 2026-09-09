# Launch operator checklist

Launch preparation only; this file is not a claim of acceptance or deployment.

- Record final current-wave coordinator/lane checkpoints, separate native source,
  unmerged candidates and exact remaining failures. Select the new source including
  these plans; do not recycle an earlier commission hash or old actor identity.
- The previous wave ended in an OOM; preserved refs are in next-wave/resume.md.
  Preserve its worktrees/runtime evidence; use a new main-based root checkout and
  a distinct tmux session for the next wave.
- Materialize the finished canonical `examples/shoal-workspace/.shoal` package
  into the new root checkout’s authoritative `.shoal`. Before capture, append the
  launch-main `next-wave/verification-prompt.md` content to that materialized
  `.shoal/prompts/core.md` exactly once. Keep the canonical example project-neutral.
  `[prompts].core` selects this combined text for the shared developer instructions
  of every root/worker, including fresh-context children; an initial message or a
  plan reference alone is insufficient. Record the combined prompt hash with the
  frozen package, then capture once. Never inject it into a running wave.
- Check the selected package and record the actual runner/package identities and
  focused acceptance. Do not describe an earlier package's checks as this selection.
- Use immutable Shoal/native/extractor executables containing the steering repair.
  Record package identity, native pin and checks in the actual launch record.
  Put these separate identities at the top: live harness main revision and binary
  hash; selected native Codex revision; each task branch and checkout HEAD. Each
  worker's captured source remains in its existing fork receipt. Advancing main
  changes none of those running instances. Rebase/update product branches between
  runs; build the next harness from main, independently of those product candidates.
- Launch a unique session with Astra High and the selected planner root prompt;
  leave Sol as the worker default. Never recreate or resume an unrelated run.
  Give the planner the exact launch record and next-wave/commission.md, followed
  by the checked "Commission and review inside Shoal" recipe from planner.md.
  This supplies the selected imports, coordinator expression and review collector
  lifecycle directly; keep the detailed implementation plans in files. It
  commissions the Sol tree; implementation starts after it checks the Sol execution plans.
- Preserve run identity, tmux panes, existing run-map observations and opt-in
  bounded private traces for later visualization. Track actual models, forks,
  messages, compactions and usage where observable; missing coverage stays unknown.

Candidate code and prompt changes activate only at a later explicit swarm boundary.
Do not mutate running tools, add another logger or send raw event streams to Astra.
