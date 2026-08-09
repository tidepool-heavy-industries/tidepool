# Spec: agent-wave TL (PRD 18 — typed headless subagents)

Executes `plans/self-iterating-harness/18-typed-subagent-spawning-prd.md`
up to its holds. The PRD is the design authority; this spec adds
sequencing, territory boundaries, and the explicit HOLD lines. Started
2026-08-08 (Inanna: spawn now, go halfway then wait is useful).

## ANTI-PATTERNS

- DO NOT touch generic-surface's territory before their fold:
  `haskell/lib/Tidepool/Form.hs`, their Generic substrate/metadata
  utilities, `Harness.Prelude`, `tidepool-mcp/src/preamble.rs`. Your
  Haskell work lives in NEW files (`Tidepool/Agent*.hs`, spike files)
  until the HOLD lifts.
- DO NOT run the five app-server spikes as an exhaustive pre-gate — the
  PRD's approach revision is binding: pinned `codex-codes`, plug in,
  debug as we go, confirm the spike checklist opportunistically, record
  fixtures when a question is actually answered.
- DO NOT let `codex-codes` or app-server types cross the Tidepool
  adapter boundary, and do not let backend vocabulary (Codex, MCP,
  JSON-RPC) reach the authored Haskell surface.
- DO NOT touch `resident.rs` pending/ChildSuspended machinery — the
  realm step-4 hold binds every lane until root's go-signal.
- DO NOT reintroduce the writer-lease/loose-workspace model: agent and
  worktree creation are COUPLED (one worktree per agent, all agents
  isolated — PRD 19 revision). Until worktree-wave's core exists, use
  the transitional `Workspace` data shape and mark every use site
  transitional.
- DO NOT read the runtime's parking machinery from `jit_machine.rs` —
  consume `plans/post-restart/realm-lanes/continuation-parking-contract.md`
  only; derive prefixes from the row that built the handler stack, never
  re-declare them.
- The five app-server spike questions burn the operator's real ChatGPT
  subscription — keep runs purposeful, no soak tests. This environment
  kills background processes at ~380s; structure long park-interval
  tests as foreground with bounded timeouts.
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

- `plans/self-iterating-harness/18-typed-subagent-spawning-prd.md` — the
  whole thing; the blockquote notes and revision blocks are binding.
- `plans/post-restart/realm-lanes/continuation-parking-contract.md` —
  the frozen park/resume API you consume. §1 is your contract; the
  internal/churnable list is off-limits.
- `plans/self-iterating-harness/19-managed-worktrees-events-prd.md` —
  the coupling decision and the fire-and-forget poke semantics you must
  not contradict.
- `plans/post-restart/codex-review-2026-08-08.md` — items 1 (landed),
  2, 6 (generic-surface's), 8: know what's fixed and what's pending.
- `plans/self-iterating-harness/15-generic-surface-wave.md` — separate
  interpreters by design; your message/tool codec is NOT their form
  codec.

## STEPS (go-now vs HOLD)

GO NOW, parallel lanes:

1. **Backend adapter bring-up** (Rust-only): pin Codex CLI +
   `codex-codes` together; start app-server over stdio; one ephemeral
   thread; one dynamic tool declared; drive `item/tool/call` →
   host-reply → same-turn resume with a Rust-only driver in a temp
   workspace. Record observed protocol behavior as fixtures as each
   spike-checklist question (park duration, steer/interrupt while
   parked, correlation, completion protocol, config isolation) gets a
   real answer. Config isolation (spike 5) is checked FIRST — no run may
   mutate the operator's Codex user config.
2. **Gate-1 spikes** (new Haskell spike files, real extract/JIT): (a)
   Servant-style mode encoding — `mode :- Call Question Decision` in an
   HKD record through elaboration; the flattened
   `Tool m input output` record is the named fallback, take it without
   ceremony if elaboration or diagnostics are materially worse. (b) a
   list-carrying and a recursive ADT round-tripping through a structural
   codec — the polarity the form interpreter rejects and yours requires.
3. **eDSL contract algebra**: `Call`/`Notify`/`Tool`/`AsServerT`,
   `compileTools` single-traversal shape, selector→snake_case naming,
   collision/identifier diagnostics as `TypeError`s. New files only.

HOLD until generic-surface's fold (root announces): consuming their
Generic metadata utilities, `16-generic-spike-receipts.md`, Harness.Prelude
integration, any edit to their files.

HOLD until worktree-wave's vertical core exists (root announces): the
coupled-spawn workspace seam. Design the spawn signature JOINTLY with
worktree-wave via root when both sides are ready; until then Workspace
stays transitional data.

HOLD until root's realm step-4 go-signal: anything in
`resident.rs` pending/ChildSuspended.

## Shared-artifact namespaces (conflict experiment applies)

Plan/receipt files under `plans/post-restart/agent-lanes/`. Do not
pre-partition source files with worktree-wave or any live lane; log real
conflicts at fold. Worktree-wave owns `plans/post-restart/worktree-lanes/`.

## VERIFY

- Rust: `cargo nextest run -p <touched crates>`; workspace `cargo check`
  before submit; clippy + fmt clean.
- Spikes through the real extract (`--ignore-default-filter` battery
  tiers 2/3; never bare battery.sh).
- Receipts per-binary counts, never exit codes. Fixtures committed for
  every answered backend question, with pinned versions stated.

## DONE CRITERIA (for the halfway point)

- Adapter: one worker thread completes task→tool-call→typed-reply→resume→
  structured completion end to end with a Rust driver, versions pinned,
  fixtures recorded, zero operator-config mutation proven.
- Gate-1: both spikes verdict-ed (mode encoding GO or fallback taken,
  with the elaboration evidence; list/recursive codec round-trips on the
  JIT), written into `agent-lanes/` receipts.
- eDSL algebra typechecks with its diagnostics fixtures.
- All HOLD lines still intact, stated in the receipt.
- Then WAIT: report [idle] to root and hold for the generic-surface fold
  announcement before the vertical core converges.
