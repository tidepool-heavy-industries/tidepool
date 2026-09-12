# Development friction notes

Concrete check and iteration costs observed during the corpus and notebook
integration pass. These are follow-up candidates, not evidence that the tests
are nondeterministic.

- The full `shoal check --recipes` run reached a late
  `Project.RoutingChecks.routing` assertion after roughly 26 minutes. The CLI
  has no recipe selector, so isolating that case required a temporary workspace
  copy and edits to both its `checks` list and recipe body. Add a targeted
  recipe selector.
- `Tidepool.Check.check` reports the assertion label but not the observed
  value. A failed `ReplyOpen` equality therefore needed an instrumented rerun
  to distinguish changed semantics from a stale display expectation. Let
  checks attach expected and observed values directly.
- A cell with an import followed by an expression renders both the binding
  notice and the expression (`defined at generation 1\nReplyOpen`). Recipe
  assertions comparing the entire output to a constructor are brittle; checks
  should expose expression results separately from binding notices.
- The routing recipe combines several actor lifecycles in one check. A failure
  near its end recompiles and replays earlier cases when rerun. Split
  independent scenarios into separately selectable recipes.
- Even the isolated scenario takes minutes: it starts a fresh extractor
  process for each hosted cell. Reuse a warm extractor across cells in one
  recipe run if its compilation and custody contracts permit it.
- The first temporary extraction of one scenario failed with an ambiguous
  `Member RecipeCheck effects` constraint because the displaced recipe body
  needed its own signature. A supported scenario filter would avoid source
  surgery and this diagnostic detour.
- A published-example assertion expected `Low` while the shipped example
  explicitly selected `Medium`. Keep expected launch policy adjacent to the
  fixture that sets it, or assert the policy from the recipe value.
