# Wave 6 observations (run 02a1c2fd, 2026-09-25)

## 05:15Z operator intervention

- The harness's `.exomonad/workspace` submodule was still at a5bbb3b at
  launch, so the run compiled the old workspace: none of the template fixes
  (unique constructor names, watchdog tool-first, discard-hold rollback
  intent, review and command skill text) were live. The deployed binary's
  pin (46de15c) only governs `exomonad new`. Bumped the submodule to
  46de15c and committed the gitlink on harness master (0831b07).
- Two exact-commit reviewers (14@1, 16@1) returned Blocked: the harness's
  review prompt only described `ReviewTask` input, and `reviewCommit`
  forks with `CommitReview`, whose acceptance needs the reviewer to build
  its own Task with the `task` defaults constructor (documented only in a
  Types.hs comment). Added an "Exact-commit reviews" section to
  prompts/review.md (harness 20a5a4f, template ade0e14a0). One reviewer had
  already verified the expected-red test (trial 2 evidence: the red test was
  named as such and confirmed offline).
- The compaction implementer (12@1) returned Blocked before any work: the
  contract needs `schemars` and its owned paths exclude Cargo.toml and
  Cargo.lock. Correct "Blocked early" behaviour from the new prompts; the
  fix belongs to its lead.
- Prompts are compiled into the workspace at run start, so the edits reach
  new forks only after the root calls `reload_agent_spec` (run scope; the
  reload typechecks and refuses on failure). Pasted an operator note into
  the root asking for the reload, the two re-reviews, and the manifest
  ownership fix.
- State at 05:15Z: 16 actors, no fenced or waiting inboxes, root busy;
  run-map reports 19 rejected units and 3 unclassified dispatch failures,
  to be read at the next wake.
