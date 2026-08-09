# Decision archive

Historical/narrative context extracted out of `CLAUDE.md` files (root and
per-crate) during the 2026-08-08 restructure, so the working guides stay
build/test/hazard-focused instead of accumulating doc-history asides.

This directory holds two kinds of thing, kept separate from each other and
from the working guides:

1. **Doc-history notes** — "why does this sentence exist / why does it say
   what it says" backstory that was previously inline. Purely explanatory;
   removing it changes no rule.
2. Nothing here is an architecture contract. Current, load-bearing
   architecture decisions live in the root `CLAUDE.md` **Key Decisions
   Reference** (authoritative, verbatim, not archived) and in each crate's
   own `CLAUDE.md`. If you're looking for "what's true now," look there
   first — this directory is "what used to be written down about how we got
   here."

## Index

- [haskell.md](haskell.md) — doc-history notes extracted from
  `haskell/CLAUDE.md` (call-graph workspace-scoping backstory, the
  `formqq-parser-test` module-listing note's prior wording).

No other `CLAUDE.md` file (root, `tidepool-codegen`, `tidepool-repr`,
`tidepool-mcp`, `tidepool-handlers`, `tidepool-repl`, `tidepool-lsp`,
`tidepool-eval`) had extractable historical/narrative content as of this
restructure — see `plans/decision-archive/RESTRUCTURE-RECEIPT.md` for the
full per-file inventory and reasoning, and the suspected-stale list for items
that were left in place (not moved) but may be worth a human's second look.
