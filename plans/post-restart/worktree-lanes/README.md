# worktree-wave lanes (PRD 19 — managed worktrees + typed repository events)

TL spec: `../worktree-wave.md`. Design authority:
`../../self-iterating-harness/19-managed-worktrees-events-prd.md` (its revision
blocks are binding over older prose). This directory holds the lane receipts.

## Scaffold (landed before the fork; frozen contract)

- `tidepool-worktree/` — new GHC-free crate. Core types, error ADT, the single
  `GitCli` call site, and stubbed modules per lane. See its `CLAUDE.md`.
- `haskell/lib/Tidepool/Worktree.hs`, `haskell/lib/Tidepool/Event.hs` — the
  ideal authored DSL, written FIRST. Per the PRD's design stance, if the
  runtime cannot meet these semantics the runtime work grows; the DSL does not
  shrink. Both re-export from `Tidepool.Effects`, which L4 must generate.

`src/id.rs`, `src/error.rs`, and `GitCli` are frozen: a lane that needs them
changed says so rather than changing them, because every other lane keys on
them.

## Lanes

| Lane | Owns | Files |
|---|---|---|
| L1 worktree core | registry durability, clean creation, restart lookup, binding | `src/registry.rs`, `src/create.rs`, `src/binding.rs`, `src/git.rs::inspect::dirty_summary` |
| L2 dirty snapshot | temp-index synthetic commit + untouched-source proof | `src/snapshot.rs` |
| L3 event monitor | poll/reconcile, coalesced deltas, journal | `src/monitor.rs`, `src/journal.rs` |
| L4 surface + `withHandler` | effect defs, handler module, realm interpreter | `tidepool-mcp/src/effect_defs.rs` (additive), a new handler module, new driver code |
| L5 Exomonad record | mapped-modules decision record — STOPS at human review | this directory only |

L1 owns `create.rs` including the wiring of the dirty branch; L2 fills
`snapshot::snapshot_source` behind that wiring. `dirty_summary` lives in
`git.rs::inspect` rather than in `snapshot.rs` precisely because both lanes need
it and two drifting notions of "dirty" would let a source be refused by one path
and captured differently by the other.

## HOLD lines (do not cross without root)

1. **Coupled-spawn seam.** `workspaceOf`, the spawn signature, and wiring
   binding enforcement to real agent spawns are designed JOINTLY with the agent
   lane via root. The binding STATE MACHINE is in scope now and is proven
   against a scripted writer with an opaque `AgentRef`; nothing may reach for a
   real agent handle type.
2. **Exomonad adoption.** L5 produces a decision record and throwaway
   prototypes, then STOPS for human review. Mapping and prototyping yes;
   adopting, extracting, or vendoring anything, no.
3. **dev-tree dogfood compile** (PRD acceptance 9) waits on the agent lane
   converging AND on `harness-dogfooding/dev-tree/Harness.hs` getting its
   coupling sync pass, which is Inanna's. Read that file for design pressure;
   do not edit it.

Known divergences for that sync pass: `Harness.hs` writes `payload observed`
where the PRD's `Observed` names its field `value`, and defines `pokeAgent =
sendMessage` when `sendMessage`/`followupTask` no longer exist as separate
operations — they are `pokeAgent`. The PRD wins; the file is not ours to
correct.

An earlier revision of this note claimed the second divergence was that
`Harness.hs` assumes guaranteed delivery where the PRD had made pokes
fire-and-forget. That is superseded and was propagated into a dev spec before
being caught: Inanna's PRD 18 revision makes `pokeAgent` a DURABLE PER-AGENT
QUEUE — a poke is accepted, stays queued until deliverable, is never silently
discarded, and delivery to an idle agent starts or queues a follow-up turn.
PRD 19's paragraph is synced at root's 930f326e. So a `headChanged` handler
that just pokes and returns is correct, with no error-handling choreography
for unsteerable agents; residents own REACTION policy, the runtime owns
delivery.

Also changed on root's tip (4eb9283b, 930f326e) and material to L4: agents may
CONTINUE RUNNING between resident cycles, so the unfold/fold can span cycles
and each cycle re-registers its `withHandler` reactions from explicit `State`
and stable worktree IDs. What still never crosses a cycle boundary is
unchanged and reinforced: an attached Haskell handle, a parked continuation, or
an event subscription.

## Territory

Rust work is overwhelmingly NEW modules. Do NOT touch `resident.rs` pending /
`ChildSuspended` machinery (the realm step-4 hold binds every lane), and do not
touch `tidepool-harness` observability/error files while harness-lifecycle is
live. Ask root before touching any shared file beyond the additive
`effect_defs.rs` / `handlers/mod.rs` lines L4 needs.

Shared-artifact namespaces follow the conflict experiment: do not pre-partition
source files with agent-wave (which owns `plans/post-restart/agent-lanes/`) or
any other live lane. Log real conflicts at fold.

## Verify

- `cargo nextest run -p tidepool-worktree` (fast tier, no GHC).
- Workspace `cargo check`, `cargo clippy`, `cargo fmt --all -- --check` clean
  before submit.
- Git-behaviour acceptance against REAL temporary repositories driven by a
  scripted writer. Never a mock of git.
- Receipts carry per-binary counts, never exit codes.

## Operational rules (verbatim in every dev spec)

- Every GHC-heavy run goes through
  `/home/inanna/dev/tidepool/scripts/ghc-slots.sh run -- <cmd>`
  (absolute path). NEVER `exclusive` mode.
- THROTTLE (root, 2026-08-08, active until root lifts it — the box hit load
  average 92 and took the operator's SSH down): the wrapper now covers MORE
  than GHC. Wrap `cargo check`/`build --workspace`, `cargo nextest run` at ANY
  tier including the quick pure-Rust one, and `cargo clippy --workspace`.
  Exempt: single-crate `cargo check -p <crate>`, edits, greps, and temp-repo
  git operations. A slot wait over 15 minutes is starvation — report it, never
  bypass the wrapper. A bypassed run on an overloaded box is the exact failure
  this exists to prevent.
- Use `ghc-slots.sh detach -- <cmd>` (NOT `run`) for any slot-taking work.
  This is a correctness fix, not a convenience: a QUEUED plain `run` dies at
  this environment's ~380s kill without ever acquiring its slot, so under a
  busy queue it can never complete and takes its slot down with it. `detach`
  runs under `setsid`, prints a pid + log path, returns immediately, queues
  durably, and releases its slot even if the pane dies. Poll the log across
  turns.
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
