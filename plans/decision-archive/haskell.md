# Archived history — `haskell/CLAUDE.md`

Doc-history context pulled out of `haskell/CLAUDE.md` during the 2026-08-08
restructure. The operative rule each item backed is unchanged and still
lives in `haskell/CLAUDE.md` — only the backstory/self-referential framing
moved here. Nothing on this page is authoritative; if it conflicts with
`haskell/CLAUDE.md` or the code, trust those.

## Call-graph workspace scoping — why the note exists

`haskell/CLAUDE.md`'s "Call-graph walks need workspace scoping" hazard
(default to `transitiveLocal*` for call-graph questions) used to end with
this parenthetical, verbatim:

> (This scoping was already solved once, dated "2026-07-01, from the
> chart-noise finding" in `Lsp.hs`, and got silently re-derived from scratch
> in a later session purely because it wasn't written down anywhere a fresh
> session would see it before diving in — that's the reason this note
> exists.)

In other words: this isn't a new discovery, it's a rediscovery of a fix that
was already made in `.tidepool/lib/Lsp.hs` (`isLocal`/`localCallees`/
`localCallers`) and `.tidepool/lib/LspGraph.hs`
(`transitiveLocalCallers`/`transitiveLocalCallees`) on 2026-07-01. The note
was added specifically so a future session wouldn't re-derive it a third
time. That reasoning is preserved here; `haskell/CLAUDE.md` keeps only the
operative rule and a pointer to this file.

## `formqq-parser-test` module-listing note — prior wording

The Eval stdlib module-map entry describing the `Ui`/`FormQQ` modules used to
end with this sentence (verbatim tail clause):

> ...that dependency is real and expected to stay in sync, unlike the old
> blanket list this passage used to describe.

Context: an earlier revision of this same doc passage apparently listed
every `lib/Tidepool/**` module individually as a host-side build dependency
of `tidepool-extract-bin`. That was inaccurate — the production binary's
import closure contains zero `lib/Tidepool/**` modules; only the
`formqq-parser-test` test-suite stanza host-compiles a `lib/` module
(`Tidepool.FormQQ.Parse`) directly. The passage was rewritten to state the
current fact plainly, and this trailing clause was left pointing back at the
now-deleted old wording. `haskell/CLAUDE.md` now states the current fact
without the self-referential aside; this file is where that aside lives now,
for anyone tracing why the doc says what it says.
