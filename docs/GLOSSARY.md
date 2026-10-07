# Glossary — the canonical vocabulary

This file is the naming authority for tidepool. The naming rule: **compositions of well-known industry terms beat coinage,
even at some length cost** — every invented term is a comprehension tax on
readers and a fluency tax on models. A coinage survives only when no
reasonable composition of standard terms carries the distinction. New
writing uses this vocabulary; old spellings are migrated on contact and by
staged sweeps (prose first, identifiers second, serialized fields last,
with compatibility).

## Execution units

Use **context window** only for the provider's token limit.

| Term | Means | Never means |
|---|---|---|
| **model round** | ONE provider request + reply | a whole interaction |
| **machine session** | the resident JIT machine, heap, and bindings | a provider exchange |

An **actor application** is the persistent supervised identity that may handle
many typed requests and model rounds. A root or child actor application becomes
idle when a model round ends; it is not completed by ending that round or by
settling one reply.

A **descendant** is any actor application whose spawn ancestry (`startAgent`
or a fork) reaches the caller through zero or more intermediate actors — its
immediate children and their own descendants alike; `rosterCreatorId`/
`rosterCreatorIncarnation` on an `AgentRosterEntry` name that ancestry, and
`creationTree` (`Tidepool.Actors.Observe`) walks it. **live descendants**
names the subset still running (`rosterState == RosterRunning`); a retired
descendant is not one. Use "descendant"/"live descendants" for this exact
relation, not "subtree" (the supervision tree, a different ancestry) or
"swarm" (every actor a `SwarmSnapshot` can reach).

## Model tiers

**Sol**, **Luna**, and **Astra** name model tiers, not people or actor roles.
Workspace configuration may offer aliases for those tiers. A person or actor
may use a configured model without taking on a prescribed task or role.

## Survivors table (what to write instead)

| Was | Write instead |
|---|---|
| decl plane / declaration plane / living structure / growing library | **persistent declaration environment** |
| value plane / binding plane / mounted roots | **persistent binding store** |
| realm | **runtime resource scope** (cancellation/ownership grouping) |
| scope / scope tree (lexical sense) | **lexical scope** / **scope tree** (fine — standard) |
| hole card / opening card | **typed request prompt** |
| custody / custody receipt / token | **owned handle** or **lease**, per actual behavior |
| ledger / receipt log | **journal** |
| effect row (model-facing) | **available effects** / the effect list itself (row is fine internally) |
| hylo boundary | say what crosses: the Haskell-expand / Rust-collapse split |
| one-session collapse / pillar A/B/D / lane coordinates | name the mechanism plainly; project coordinates never leave `plans/` |
| session (bare, for runtime state) | **machine session** (`ResidentSession` — the resident JIT machine + heap + bindings) |
| context fork / self-fork (Exomonad actor surface) | **captured context** (`ForkCtx`) |
| lane (model-facing work division) | **assignment**, **workstream**, or the actual named component |

## Reserved words (industry meaning only)

- **context window** — provider token limit. Nothing else.
- **turn / round** — one model exchange (prefer **model round**).
- **session** — qualify the concrete session being discussed; use **machine session** for resident JIT state.
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

`spawnSubagent` creates one idle child from an explicit captured or fresh context,
a shared, granted, or forked workspace, and the actual typed `AgentSpec`. A
successful spawn does not run inference; a typed request or human message does.
`SameDir` shares actual writable files, index, and HEAD. Actor labels are ordinary
optional text and do not determine identity or workspace selection.

**invocation-owned command work** is unfinished work cancelled when its creating
invocation exits. Command handles do not transfer ownership when returned or
captured. A child actor defaults to its parent actor's ownership, and a request
defaults to its caller actor's ownership. Borrowed handles permit observation
while available, never owner cancellation. Resources join a runtime scope only
through explicit `InScope scope` options.

An **`Await a`** is a typed description of readiness. `result` projects a request
into an `Await`; `await` observes it. `Await` values compose applicatively and
traverse collections. `eitherOf` selects the first terminal branch, including a
failure; all-branch composition requires each branch to succeed. A choice retained
by its watch owner remains valid even if a losing response is later released.
An **`EventSource a`** delivers retained source state and subsequent events to
serialized record-actor handlers. `R.start` creates a persistent record service
with actor lifetime. A **runtime scope** is an explicit cleanup delimiter;
resources join it through `InScope scope`, not ambient defaults.

## Model-facing prompt rules

1. Never address a model with "window", "residency", "plane", "realm",
   or "lane".
2. State mechanics directly: "you may use up to N model rounds", "top-level
   declarations persist beyond this session", "child sessions inherit
   ancestor declarations, never a sibling's".
3. Let types carry concepts: show the effect list and the concrete failure
   channel, such as `Either RequestError T` for request admission or
   `Either AwaitError T` for observation. A retained context reference is
   runtime-issued and unforgeable.
4. Prefer the vocabulary models already know: notebook cells, Haskell,
   `Control.Concurrent.Async`.
