# Companion memory: agent-curated store + directive finalize

**Status: LANDED THROUGH PHASE 2 (2026-08-14). Phase 1 (outer-row Subagent,
suspension-serviced; work items 1-9) and Phase 2 (store bootstrap script,
binary wiring behind TIDEPOOL_MEMORY_REPO, companion HarnessTypes v3 +
loop v4 with the Turn contract and per-loop curator batching, migration
seeded as initialState.pendingMemOps) are committed and green. AWAITING:
the operator's bounce — the v3 checkpoint is a WIRE BREAK with the live v2
checkpoint (StateDecode fails on restore; reset the dogfood checkpoint and
restart via run.sh, which auto-seeds the store). Phase 3 remains
friction-driven.**

One epic, two products: (1) the companion's memory leaves its serialized
`State` and becomes an agent-curated git repo of markdown files; (2) the
curator is the FIRST LIVE exercise of the PRD 18 typed-subagent seam
(`spawnAgent @r`) — a known-viable task (semantic file curation) exercising
the unproven wiring, which is also exactly the outer-row work the dev-tree
harness is blocked on.

## Architecture

**The store** — a standalone git repo (NOT inside the tidepool source tree;
durable data dir, default `~/.local/share/tidepool/companion-memory`,
env-overridable), registered with the PRD 19 worktree machinery so the
curator spawns against it:

```
companion-memory/
├── AGENTS.md      ← the curation ruleset, versioned WITH the store
├── MEMORY.md      ← the digest: one line per memory + hook; agent-maintained, capped
├── operator.md    ← the operator model, rendered in full each loop
└── memories/<slug>.md  ← one fact per file; frontmatter: description,
                          provenance, date; [[wiki-links]] between files
```

The agent commits per run with the intention as the message: `git log` IS the
memory edit history, forget-is-delete is safe (git keeps everything — this
dissolves State v2's retention-horizon defect rather than fixing it), and the
operator inspects/contests/prunes memory with ordinary git tools. No standing
field; the archive is git. The repo holds operator-personal content: local
only, never a remote.

**Write path.** The companion emits memory directives from its act window
(explicit authorship — it chooses WHAT to remember; the curator decides HOW
to file it). The authored loop batches them — ONE spawn per loop, in
`finish` — into `spawnAgent @MemReceipt` with a brief: read AGENTS.md, apply
these intentions, regenerate MEMORY.md, commit. Latency: the coupled lane-1
spawn blocks the loop turn (~30–120s); acceptable batched, `note`-narrated;
an async lane is a later friction-driven change.

**Read path** — receipt-carried digest, so NO file-read effect enters any
row: `MemReceipt { digest :: Text, touched :: [Text], summary :: Text }` —
the curator returns the fresh MEMORY.md contents; the loop stores it in
`State`; render shows it. The digest survives restarts via the ordinary
checkpoint. `Subagent` (+ its worktree type dependencies) is the ONLY
capability the outer row gains. Failure is non-fatal: no receipt → the
directives stay pending, render shows "N unfiled intentions", retried next
loop (capped).

**Recall** (semantic queries beyond the digest): deferred until lived
friction names its shape.

## The answer type: directives BESIDE the edit, not instead of it

(Revised with the operator, 2026-08-14: "don't pack everything in
directive".) The act window's answer pairs OUTWARD INSTRUCTIONS with the
INWARD EDIT — fst is what the loop executes against the world, snd is the
same endomorphism as today:

```haskell
-- Spelled as a record because the record-of-functions finalize delivery is
-- an already-proven standing acceptance (selfharness_fn_finalize_spike);
-- semantically it is the ([Directive], State -> State) tuple.
data Turn = Turn { directives :: [Directive], edit :: State -> State }

-- v1: the memory verbs only — typed INTENT, prose PAYLOAD (the curator
-- agent is the parser; the digest shows slugs so prose can name them).
-- Future outward-instruction kinds join this sum as they earn their keep.
data Directive = Remember Text | Modify Text | Forget Text
```

`Turn [] id` is the blessed no-change. Every existing edit combinator
(`openThread`, `updateThread`, `onScratch`, …) survives unchanged — no
reification of bag edits — and closure delivery keeps its full dogfood role
as half the answer rather than an escape hatch. The observability win lands
exactly where the new risk is: the memory channel logs as legible data;
the edit half stays `"<closure>"` as today, by choice.

**State v3 (the typed bag):** `memories` and `aboutOperator` LEAVE State
(killing checkpoint bloat and the render-bloat thread at once). Remaining:
identity, loopN, threads, proposals, scratch, lastExpectation, plus
`memoryDigest :: Text`, `pendingMemOps :: [Directive]` (directives whose
curator run failed, carried for retry — the happy path hands directives to
the loop OUT OF BAND as `Turn.directives`, never through State), and the
persistent curator `WorktreeId`.
Migration is the curator's first live job: hand it the v2 checkpoint's memory
JSON and let it file everything — the bootstrap IS the smoke test.

## Settled decisions

- Answer type `Turn { directives, edit }` — directives beside the
  endomorphism, not instead of it (operator, 2026-08-14).
- Memory verbs: `Remember | Modify | Forget`, prose payloads, typed intent.
- Explicit companion authorship (no loop auto-harvest of memories).
- Batched per-loop curator runs; receipt-carried digest; non-fatal failure.
- Nondeterministic filing ACCEPTED (mitigation: versioned AGENTS.md + git
  history + operator prune access).
- No separate de-risk phase: the live-spawn smoke is the first hour of
  Phase 1, not a gate.

## Phases

**Phase 1 — outer-row Subagent (the dev-tree unblock, shared). Wiring shape
SETTLED by recon (2026-08-14): suspension-serviced, driver-owned handler —
NOT a handler stack on the outer session.**

Why suspension-serviced is the only viable shape: the one-session collapse
shares ONE machine between the outer session and every answerer realm, and
the machine's established prefix + session-wide `ask_tag` mean a non-empty
outer handled prefix would silently dispatch the ANSWERER's `AskUser`/`Fork`
(tags 0/1) into handler slots — breaking the documented all-suspending
capability boundary. So: `outer_decls() = [runllmturn, askuser, worktree,
subagent]` with interposed effects FIRST keeps `suspend_tag = 0` (everything
suspends; pin with a unit test). `worktree_decl` is a HARD companion of
`subagent_decl` (type deps AND the `renderSpawnError` →
`renderWorktreeError` helper dependency). The servicing conversion is
near-free: the generated `SubagentReq: FromCore` decodes the suspended
request against the compile's table, `EffectHandler::handle(req,
EffectContext::with_user(..))` runs the real handler, and the
`Response::Complete` value resumes the parked hole — the identical generated
path a dispatched effect takes. `CodexAgentBackend` owns its own tokio
runtime, so the dispatch wraps in `tokio::task::block_in_place` (the
OperatorGate precedent). The handler rides the DRIVER (an
`Option<SubagentHandler>` + `set_subagent_handler`, mirroring `set_gate`) —
never the boxed stack, which is destroyed on machine rotation while the
handler holds a flocked BindingTable and a live app-server. NON-ITEMS,
permanently: `Subagent` in `base_effects!`, or any non-empty outer handled
prefix while outer + answerer share a machine.

Runtime facts recon pinned: `WorktreeSpec` names no repository — the source
repo is HANDLER CONFIG (`SubagentHandler::new(registry_root, worktree_root,
binding_root, source_repo, backend)`), so the standalone memory repo slots in
directly; registry roots must live OUTSIDE any git work tree; Codex reads the
operator's own `~/.codex/auth.json` (`CODEX_HOME` overrides), models resolve
via `ModelPolicy` allowlist (live grant: `CheapestGpt56` + low effort), turn
timeout 300s; live tiers are opt-in via `TIDEPOOL_AGENT_LIVE=1` (examples
`live_one_cycle`, `live_tool_loop`; nextest never spends tokens).

Work items: (1) live smoke via the existing example, one-attempt — DONE, see
below; (2) widen `outer_decls()` + suspend_tag pin + update
`companion_typechecks`; (3) `HoleRouting::Subagent` arm in `classify_hole`
keyed on constructor names, carrying the raw request `Value` (args are ADTs —
never decoded at classify time); (4) `service_outer_subagent_hole` in the
driver + the dispatch arm at the current "unserviceable hole" error + an
observer `Event`; (5) the `set_subagent_handler` seam + binary wiring behind
an env flag; (6) store bootstrap (git init + seed, roots outside the repo);
(7) mock-tier driver test (outer `spawnAgent @r` round trip on a
`MockBackend` handler); (8) prefix-compat regression test (answerer still
suspends on `AskUser` under the widened row); (9) the `Turn`
delivery spike — a record pairing `[Directive]` data with a `State -> State`
closure — extending `selfharness_fn_finalize_spike.rs` (expected cheap: the
record-of-functions lane is the proven one).

**Phase 2 — the memory epic:**
- Repo bootstrap: seeded AGENTS.md (drafted from Claude Code's own memory
  rules: one fact per file, kebab slugs, description frontmatter,
  dedupe-before-write, update-over-append, delete-what's-wrong, liberal
  wiki-links, digest cap, commit per run, never touch outside the repo),
  git init, worktree registration.
- HarnessTypes v3: State reshape, `Directive`/`ThreadEdit`, the loop's
  directive interpreter (partition: bag-fold + curator batch), digest in
  render, unfiled-intentions line.
- Prompt updates (act window teaches directives) + migration run.
- Tests: mock-backend tier for loop logic (scripted receipts), recording tier
  for the brief/receipt contract, live tier gated expensive.

**Phase 3 — friction-driven:** recall verb, async spawn, digest
regenerate-on-boot (a crash between agent-commit and checkpoint leaves the
digest one run stale — accepted for now).
