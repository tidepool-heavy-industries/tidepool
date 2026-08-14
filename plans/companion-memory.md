# Companion memory: agent-curated store + directive finalize

**Status: ACTIVE (planned 2026-08-14 with the operator; decisions below are
settled unless marked open).**

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

## The answer type: `finalize @[Directive]`

The act window's answer becomes a SERIES OF DIRECTIVES, applied in order.
`[]` is the blessed no-change answer (more auditable than `id`). Memory verbs
are typed INTENT with prose PAYLOAD (the agent is the parser; the digest
shows slugs so prose can name them); bag edits are legible data; the function
thing survives as deliberately-scoped escape hatches, keeping closure
delivery dogfooded as the marked exception:

```haskell
data Directive
  -- Memory tier: executed by the curator agent, batched per loop
  = Remember Text
  | Modify Text
  | Forget Text
  -- Typed bag: interpreted by the loop, pure, fully legible in logs
  | OpenThread Text
  | UpdateThread Int ThreadEdit
  | Propose Text
  | SetIdentity Text
  -- Escape hatches (closure-carrying; the deep-sentinel delivery lane)
  | OnScratch (Value -> Value)
  | Custom (State -> State)

data ThreadEdit = SetStatus ThreadStatus | SetStance Text | Rephrase Text
```

Wins over the endomorphism: Finalize observer events become a legible account
(today they log `"<closure>"`); authority is the sum plus a VISIBLE escape
hatch; every loop's durable footprint is auditable at a glance.

**State v3 (the typed bag):** `memories` and `aboutOperator` LEAVE State
(killing checkpoint bloat and the render-bloat thread at once). Remaining:
identity, loopN, threads, proposals, scratch, lastExpectation, plus
`memoryDigest :: Text`, `pendingMemOps :: [Directive]` (memory verbs awaiting
a successful curator run), and the persistent curator `WorktreeId`.
Migration is the curator's first live job: hand it the v2 checkpoint's memory
JSON and let it file everything — the bootstrap IS the smoke test.

## Settled decisions

- Directive list with escape-hatch constructors (operator, 2026-08-14).
- Memory verbs: `Remember | Modify | Forget`, prose payloads, typed intent.
- Explicit companion authorship (no loop auto-harvest of memories).
- Batched per-loop curator runs; receipt-carried digest; non-fatal failure.
- Nondeterministic filing ACCEPTED (mitigation: versioned AGENTS.md + git
  history + operator prune access).
- No separate de-risk phase: the live-spawn smoke is the first hour of
  Phase 1, not a gate.

## Phases

**Phase 1 — outer-row Subagent (the dev-tree unblock, shared):**
- Live smoke first: one `spawnAgent` → Codex adapter → trivial brief in a
  scratch repo → typed receipt. Confirms backend wiring + that the agent
  policy allowlist admits git/file tools.
- Add `worktree_decl` type deps + `subagent_decl` to `outer_decls()`; decide
  the driver's route for the resulting effect (suspension-serviced like outer
  `askUser`, vs a handler stack owned by the selfharness binary — OPEN, the
  main Phase 1 engineering question; the selfharness driver currently runs
  all-suspending rows with no handler dispatch).
- Verify closure delivery through a LIST of sums (`[Directive]` with
  `Custom`/`OnScratch`) — one spike test extending
  `selfharness_fn_finalize_spike.rs`.

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
