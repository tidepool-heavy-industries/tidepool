# Jev playbook for run 6 (record-dot style; state shapes and option wording literal)

1. State is always an object with named fields; questions name the fields in backticks. Never paste a child's narration into an evidence field.
2. Review gate (merging policy):
   `st = {brief: {owned_file, acceptance_checklist: [..]}, diff_stat, hunks, test_output}`
   `gate := J.choice "Which statement describes the candidate?" (alt #all_present "Every item of the checklist holds: <items joined by ;>." .| alt #item_missing "At least one item does not hold: a changed file outside the owned file, a failing or missing owned test, a deleted or weakened test, a remaining todo!(), or an implementation that does not match the goal." .| alt #conflicting "The items are all present but contradict each other, for example the report claims a test passes that the test output shows failing.")`
   `J.accept J.merging a.gate`; on Left, the doubt names the checklist item to send back.
3. Never ask "is this sufficient" or "does this need repair". Ask the literal facts and derive the judgment in code.
4. Per-child triage, one call for all children, compact `test_output` (only `test ...`, `panicked at`, and `test result` lines):
   `#real := J.askAbout r "Does any test whose path is inside `owned_file`'s module fail in `test_output`?"`, `#scope := J.askAbout r "Does `files_changed` include any file other than `owned_file`?"`; `needs_repair = a.real.yes > 0.5 || a.scope.yes > 0.5`.
5. Report honesty is a tie-breaker, threshold 0.20: `#honest := J.noul "Is every claim in `worker_report` supported by what `test_output` and `files_changed` actually show?"` A terse "Done." sits near 0.4; a lie sits at 0.02.
6. Risk of a diff is a Noul battery, not a Score: `touches_outside_ownership`, `changes_public_item_used_elsewhere`, `deletes_or_weakens_test`, `leaves_todo`. Rank merges by the count, in code. A Score whose confidence is 0.0 has non-exclusive rungs; read `a.masses`.
7. Use a Score only for an ordered ladder of exclusive situations. Rungs describe what a thing on that rung looks like, never "low / medium / high".
8. Use a Choice only when exactly one option wins; options describe the condition that makes them apply, in terms of state fields. Not the action, not the argument.
9. A 1.0 means nothing else in the pool competes. If that surprises you, the missing option is the one to add.
10. Policies by stakes: `J.routing` (read-only routing, which skill), `J.spawning` (spawn a worker, choose an approach), `J.merging` (merge, stop, anything with a receipt). Gate on `a.confidence` first.
11. check.sh output never needs Jev: exit 0 is green; fmt diff → `cargo fmt`; a clippy name → `cargo clippy --fix` up to three times; `not yet implemented` → a stub; `assertion ... failed` → a real failure. Only unmatched output goes to Jev.
12. Scope rule before any classification: a failing test outside the child's owned module is expected, not a wakeup.
13. Skill pre-load: `#first := J.choice "Which skill should the root load first for `moment`?" roster` plus one `helps_<skill>` noul each; load at `a.first.confidence >= 0.70`, else read the roster yourself.
14. Pagination: one Score per 5-line chunk (noise / context / essential, as situations); essential inline, context under `[n lines, expand: cNN-cMM]`, noise dropped. Pin first and last `^error` lines in code.
15. Keep a dense cell under ~30 compact items; full test outputs exceed 32k tokens at six items.
16. Literal questions are stable across phrasings (Δ < 0.03); judgment questions move up to 0.45. Pair only `honest` and `stuck`, take the minimum.
17. Read `a.key`, `a.mass`, `a.margin`, `a.confidence`, `a.masses`; `a.yes`; `a.expectation`, `a.level`. A bare `answer` displays. No projection lambdas.
18. Every call logs a ledger row: the questions, the answers as above, the model id from the response, and the action taken.
19. When Jev returns Left, fall back to reading in order and keep going; a recurring handback is a missing branch to add by hand.
20. Jev is evidence, not authority: the checklist gate agreed with five fresh reviewers on five real candidates; it replaces the reviewer only where the checklist is the whole contract.
