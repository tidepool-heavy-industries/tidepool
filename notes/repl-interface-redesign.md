# Repl Interface Redesign — evolved → designed

2026-07-01. Origin: live dogfooding hit the annotation tax (`\(c :: Commit) ->`
required where GHCi needs nothing); Inanna's framing: "having to provide type
signatures like that is a signal it's unergonomic… it's evolved, not designed."
This doc is the design position + the fix threads.

## The invariant

**You write Haskell; the session decides how to compile it.**

The current surface violates this by leaking compilation topology upward:
item boundaries are inference boundaries, decl-vs-bind is a *type-system*
distinction the user must know, and errors speak elaboration truth
(synthetic `pure x` frames, `G2.hs:29` coordinates, tmp paths).

## Evidence (all probed live, session `playground`, 2026-07-01)

| # | Probe | Result | Leak |
|---|-------|--------|------|
| 1 | `let five = 5` vs decl `fiveD = 5` | `Int` vs `Integer` | Same text, different type by plane — and Integer is the JIT-risky one |
| 2 | `let heat = \c -> … c.date …` | `HasField r0` error at synthetic `pure heat` | Bind plane materializes eagerly → can't generalize; GHCi defers to use site |
| 3 | same lambda as bare decl `heatOf now c = …` | compiles polymorphic, resolves at use site | The decl plane already has the right semantics — the split is the problem |
| 4 | sig + body as two items | GHC-44432 "lacks an accompanying binding", tmp path, wrong line, no hint | Tool knows the same-item rule (it's in its own docs) but the error doesn't teach it |
| 5 | typed hole `_` in a decl | hole type + relevant bindings survive; diagnostics printed TWICE; valid-hole-fits MISSING (stock GHC default-on); tmp paths leak | GHC's best discovery tool arrives degraded and framed as failure |
| 6 | define+use split across items of ONE call | fails at the definition item | A block looks like one program but is typechecked as N disjoint universes |

## Redesign moves

### A. Whole-block elaboration (the deep fix — NOT YET SPAWNED, needs Inanna)
Treat the submitted block as ONE program. Two-phase:
1. **Inference pass**: synthesize a single module from the block (all decls,
   SCC-grouped, + the stmt sequence as one `do`), typecheck it once. Extract
   the pinned type of every bind.
2. **Execution pass**: per-item compilation exactly as today, but with types
   injected from pass 1 (and decl SCCs sharing modules).
Kills: item-as-inference-universe (#6), one-decl-per-item / sig-same-item
(#4), and most of the bind-plane annotation tax (#2) in the define+use case.
Preserves: per-item results, stop-on-first-error, heap materialization
granularity. Cross-block mutual recursion stays impossible (GHCi parity).

### B. Auto-demote unmaterializable binds (spawned: `bind-plane-generalize`)
When `let x = e` fails ONLY with unsolved-constraint/ambiguity at the
synthetic `pure x`: recompile the same text as a declaration `x = e`
(which generalizes), report `kind:"decl"` + a note that no value was
materialized. Strictly better — today's alternative is an error. Covers the
cross-block case A can't (polymorphic `let` used only in a LATER block).

### C. Unify defaulting across planes (folded into B's thread)
`let x = 5` :: Int vs decl `x = 5` :: Integer is indefensible. One
defaulting story, both planes; prefer Int (JIT-safe).

### D. Errors speak user syntax (spawned: `error-plane-truth`, TL)
The whole class, not just coordinates: item-relative positions (re-land of
decl-error-mapping under its revert spec — rewriter anchored to the generated
module's basename ONLY, decl_plane fixture), synthetic-frame suppression
(`pure x` → "the value bound to `x`"), dedup the double-printed
typecheck/compile diagnostics (#5), and a taught hint on GHC-44432 until A
obsoletes it.

### E. Holes as first-class queries (spawned: `holes-as-queries`)
`_` should not be a failed item; it's the user asking the tool a question.
Return `kind:"hole"` with structured `{holeType, relevantBindings, fits}`;
turn valid-hole-fits on (with the stdlib in scope, fits are a vocabulary
discovery engine). A hole item succeeding (as a query) should not abort the
rest of the block's typecheck-only items.

## What "done" feels like

The teach-burden collapses to: `x <- e` runs an effect; everything else is
Haskell. No plane knowledge, no annotation tax, no same-item rules, errors
point at your text, and `_` answers questions instead of raising them.
