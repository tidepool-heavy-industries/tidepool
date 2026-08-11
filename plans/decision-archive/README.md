# Decision archive

Backstory pulled out of `CLAUDE.md` files so the working guides stay
build/test/hazard-focused.

**Scope is narrow, and narrower than it was.** The default for inline
doc-history is DELETION, not archival — git is the store (root `CLAUDE.md`,
"Doc history"). A note earns a page here only when losing the backstory
invites re-tripping a hazard: the class where a fix was silently re-derived
from scratch because nobody wrote down that it had already been made. Such a
note gets a ONE-LINE pointer at its site in the working guide, never inline
prose.

Nothing here is an architecture contract. Current decisions live in the root
`CLAUDE.md` **Key Decisions Reference** (authoritative, verbatim, never
archived) and in each crate's own `CLAUDE.md`. If you want "what's true now,"
look there.

## Index

- [haskell.md](haskell.md) — extracted from `haskell/CLAUDE.md`.
  §1 (call-graph workspace scoping) is the live pointer: a fix made once and
  re-derived from scratch a session later. §2 (`formqq-parser-test`
  module-listing wording) is no longer pointed at from anywhere — it is
  superseded prose kept only because archive pages are append-only.

`plans/decision-archive/RESTRUCTURE-RECEIPT.md` holds the per-file inventory
from the 2026-08-08 restructure that created this directory.
