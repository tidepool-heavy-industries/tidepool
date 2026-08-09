# Spec: worktree-wave TL (PRD 19 — managed worktrees + typed repository events)

Executes `plans/self-iterating-harness/19-managed-worktrees-events-prd.md`
up to its holds. The PRD is the design authority (including the
fire-and-forget poke decision, the ideal-DSL stance, and the
agent/worktree coupling revision); this spec adds sequencing, territory
boundaries, and the explicit HOLD lines. Started 2026-08-08 (Inanna:
spawn now, go halfway then wait is useful).

## ANTI-PATTERNS

- DO NOT add git workflow verbs (`rebaseOnto`, `merge`, `cherryPick`,
  conflict resolution, branch promotion) to the runtime surface — the
  PRD's boundary is creation, lookup, inspection, events. Choreography
  is authored code.
- DO NOT let runtime machinery shape the authored vocabulary — the DSL
  is designed as the ideal authored surface; if the runtime can't meet
  the authored semantics, the runtime work grows, the DSL does not
  shrink (PRD design stance).
- DO NOT touch `resident.rs` pending/ChildSuspended machinery (realm
  step-4 hold binds every lane), and do not touch tidepool-harness
  observability/error files while harness-lifecycle is live — your
  Rust work is overwhelmingly NEW modules (worktree, event, registry).
  If you genuinely need a shared file, ask root first.
- DO NOT decide the Exomonad adoption questions unilaterally — that
  lane's deliverable is a DECISION RECORD presented to root/Inanna for
  human review, per the PRD. Mapping and prototyping yes; adopting no.
- DO NOT auto-delete anything: retain-first is a locked decision. No GC,
  no cleanup policy, manual deletion becomes `WorktreeLost`.
- DO NOT dirty the source repository: registry and worktree paths
  outside the source working tree; Tidepool-owned ref namespace;
  `allowDirtySnapshot` must leave branch/HEAD/index/working bytes
  untouched (and refuses mid-merge/rebase sources).
- DO NOT read parking machinery from `jit_machine.rs` — consume
  `plans/post-restart/realm-lanes/continuation-parking-contract.md`;
  derive prefixes from the constructing row, never re-declare.
- Operational, copy VERBATIM into every dev spec:
  - Every GHC-heavy run goes through
    `/home/inanna/dev/tidepool/scripts/ghc-slots.sh run -- <cmd>`
    (absolute path). NEVER `exclusive` mode.
  - `export XDG_CACHE_HOME="$PWD/.cache"` before any tidepool-harness
    test shard (persistent per-worktree, not mktemp).
  - Spawns pass an explicit `model: sonnet` (or `opus` for sub-TLs);
    never fable.
  - Never path-unscoped `pkill -f`; scope kills to PID or full worktree
    path.
  - Commit with `--no-verify`. Never `git add -A`. Repo-root `tmp/` is
    protected. Grep/Read over LSP.
  - Inherited-red claims require a cache-consistent A/B in the dev's own
    worktree (same cache state both legs, diff absent vs present);
    diff-file-overlap arguments are invalid for global surfaces.

## READ FIRST

- `plans/self-iterating-harness/19-managed-worktrees-events-prd.md` —
  whole thing; the revision blocks are binding over older prose.
- `plans/self-iterating-harness/18-typed-subagent-spawning-prd.md` —
  the Workspace boundary + coupling revision; `sendMessage` semantics
  your events compose with (fire-and-forget, typed error to sender).
- `plans/post-restart/realm-lanes/continuation-parking-contract.md` —
  the frozen API `withHandler`'s implementation consumes.
- `harness-dogfooding/dev-tree/Harness.hs` — the executable design
  target. NOTE: written pre-coupling; it needs a sync pass that is
  Inanna/ChatGPT's, not yours — read it for pressure, don't edit it.
- Exomonad precedent (read-only reference, external repo):
  `rust/exo/src/tools/spawn.rs`, `rust/exo-node/src/hooksock/`,
  `rust/exo-node/src/inbound.rs` under `~/dev/exomonad` (verify paths).

## STEPS (go-now vs HOLD)

GO NOW, decomposed into dev lanes as you see fit:

1. **Worktree core** (no Agent dependency): `Worktree` effect + errors +
   `WorktreeId`/registry/receipt types; clean `fromCurrentRepository` /
   `fromRef` / `fromWorktree` creation; stable runtime-owned paths,
   branch namespace, durable registry; restart lookup (`lookupWorktree`,
   `listWorktrees`, `WorktreeLost`). Acceptance in a temporary
   repository.
2. **Dirty snapshot**: temp-index synthetic commit; prove source branch,
   HEAD, index, staged/unstaged bytes, ignored files untouched; refuse
   dirty submodules and in-progress merge/rebase states loudly.
3. **Event monitor**: `headChanged` polling/reconciliation with
   coalesced-delta semantics and honest `UnknownChange`; `commit`
   classification only when honestly inferable; shared `EventId` for one
   underlying event; durable journal (no replay to subscribers).
   Acceptance drives transitions with a SCRIPTED writer (plain git
   commands standing in for a coding agent) — no LLM needed.
4. **`withHandler` interpreter** on the landed realm machinery per the
   parking contract: lexical register/drain/unregister, broadcast, one
   handler at a time per subscription, queued observations, bounded
   queue with loud overflow, handler failure fails the scope. The realm
   machine (steps 1–3+5 + falsifier suite) is LANDED on this tip —
   this is buildable now.

HOLD until agent-wave's adapter seam exists (root announces): the
coupled-spawn integration — spawn signature designed JOINTLY with
agent-wave via root; `workspaceOf` conversion for real agents; the
one-worktree-per-agent binding enforcement (design it against the
scripted-writer acceptance now, wire it to real spawns then).

HOLD for human review: the Exomonad integration lane produces the
mapped-modules decision record and any throwaway prototypes, then STOPS
for Inanna/root review before adopting/extracting anything.

HOLD until both agent-wave converges and the dev-tree harness gets its
coupling sync pass: the dev-tree dogfood compile (acceptance 9).

## Shared-artifact namespaces (conflict experiment applies)

Plan/receipt files under `plans/post-restart/worktree-lanes/`. Do not
pre-partition source files with agent-wave or any live lane; log real
conflicts at fold. Agent-wave owns `plans/post-restart/agent-lanes/`.

## VERIFY

- `cargo nextest run -p <touched crates>`; workspace `cargo check`
  before submit; clippy + fmt clean.
- Git-behavior acceptance tests run against REAL temporary repositories
  (init in test temp dirs), never mocks of git.
- Receipts per-binary counts, never exit codes.

## DONE CRITERIA (for the halfway point)

- Steps 1–4 landed and green: create/lookup/snapshot/events/handlers
  proven against temporary repositories with a scripted writer,
  registry surviving restart, all retention and isolation invariants
  test-enforced.
- HOLD lines intact and stated in the receipt; Exomonad decision record
  drafted and waiting on human review.
- Then WAIT: report [idle] to root and hold for the agent-wave seam
  announcement.
