# Repl Interface Redesign — open thread

## Holes as first-class queries (not yet built)

`_` should not be a failed item; it's the user asking the tool a question.
Return `kind:"hole"` with structured `{holeType, relevantBindings, fits}`, and
turn `valid-hole-fits` on (with the stdlib in scope, fits are a vocabulary
discovery engine). A hole item succeeding (as a query) should not abort the
rest of the block's typecheck-only items.
