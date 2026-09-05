# Glossary — the canonical vocabulary

This file is the naming authority for tidepool. The rule (root `CLAUDE.md`,
"Terminology"): **compositions of well-known industry terms beat coinage,
even at some length cost** — every invented term is a comprehension tax on
readers and a fluency tax on models. A coinage survives only when no
reasonable composition of standard terms carries the distinction. New
writing uses this vocabulary; old spellings are migrated on contact and by
staged sweeps (prose first, identifiers second, serialized fields last,
with compatibility).

## The three execution units

The old bare "window"/"turn"/"cycle" muddle collapses into exactly three
units. Bare **"window" is banned everywhere** — the ONLY surviving use is
**context window**, meaning the provider's token limit, nothing else.

| Term | Means | Never means |
|---|---|---|
| **model round** | ONE provider request + reply | a whole interaction |
| **agent session** | the possibly-multi-round interaction answering ONE typed request (was: "window", "cognition window", "answerer window", "residency") | the runtime machine state |
| **loop iteration** | one authored `render`/`loop`/checkpoint pass (was: "cycle", "loop", sometimes "window") | a model round |

Qualified companions: a **child agent session** (was "fork/branch child
window"); a session is **reusable** (the per-loop answerer) or **one-shot**.

An **actor application** is the persistent supervised identity that may handle
many agent sessions and model rounds. A root or child actor application becomes
idle when a model round ends; it is not completed by ending that round or by
settling one reply.

## Survivors table (what to write instead)

| Was | Write instead |
|---|---|
| decl plane / declaration plane / living structure / growing library | **persistent declaration environment** |
| value plane / binding plane / mounted roots | **persistent binding store** |
| realm | **runtime resource scope** (cancellation/ownership grouping) |
| scope / scope tree (lexical sense) | **lexical scope** / **scope tree** (fine — standard) |
| hole card / opening card | **typed request prompt** |
| custody / custody receipt / token | **owned handle** or **lease**, per actual behavior |
| wave | A local scaffold/unfold/fold cycle; not a global barrier or runtime identity. |
| ledger / receipt log | **journal** |
| branch position (model-facing) | show the type: `Either InvocationExit T` |
| effect row (model-facing) | **available effects** / the effect list itself (row is fine internally) |
| hylo boundary | say what crosses: the Haskell-expand / Rust-collapse split |
| one-session collapse / pillar A/B/D / lane coordinates | name the mechanism plainly; project coordinates never leave `plans/` |
| session (bare, for runtime state) | **machine session** (`ResidentSession` — the resident JIT machine + heap + bindings) |
| context fork / self-fork (Shoal actor surface) | **context unfold** for the applicative expansion; **blocking answerer fork** for `Tidepool.Answerer.Fork` |

## Reserved words (industry meaning only)

- **context window** — provider token limit. Nothing else.
- **turn / round** — one model exchange (prefer **model round**).
- **session** — always qualified: **agent session** or **machine session**.
- **journal** — append-only durable records.
- **generation** — stale-writer fencing value.
- **timeout interval** — elapsed-time limits (never "window").

## Earned terms (keep, they name real distinctions)

**hylomorphism / algebra / coalgebra** (in actual recursion-scheme code);
**effect row** (internal type-level discussion — standard extensible-effects
vocabulary); **parked continuation**; **green thread**; **resident** (as in
machine session: state genuinely stays in memory across calls);
**`ContextRef` / frozen context snapshot** (an opaque capability reference
to an exact retained transcript prefix).

**context unfold** is the applicative construction of persistent child actor
applications from one active provider call and immutable Haskell binding tip.
It shares context, narrows authority explicitly, and returns typed handles;
results return later through replies and watches. **fold** is ordinary Haskell
composition of those typed results and worktree evidence, not an automatic
merge of model contexts.

## Model-facing prompt rules

1. Never address a model with "window", "residency", "plane", "realm",
   "branch position", or "lane". Use "wave" only for a local work cycle.
2. State mechanics directly: "you may use up to N model rounds", "top-level
   declarations persist beyond this session", "child sessions inherit
   ancestor declarations, never a sibling's".
3. Let types carry concepts: show the effect list, show
   `Either InvocationExit T`, say `ContextRef` is runtime-issued and
   unforgeable.
4. Prefer the vocabulary models already know: GHCi, Haskell,
   `Control.Concurrent.Async`.
