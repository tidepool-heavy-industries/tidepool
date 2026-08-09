# dev-adapter-bringup — Codex app-server adapter (Rust only)

PRD: `plans/self-iterating-harness/18-typed-subagent-spawning-prd.md`, sections
"Codex app-server backend", "Rust client dependency", "Falsification spikes"
(read the **approach revision** at the top of that last section — it is
binding). Lane index: `plans/post-restart/agent-lanes/README.md` — read its
"Protocol reconnaissance already done" section FIRST; it saves you an hour and
several dollars.

You own `tidepool-agent/`. Nothing else. No Haskell, no other crate.

## The one-sentence goal

Drive one Codex app-server worker end to end from Rust — task → generated
tool call → typed host reply → same-turn resume → structured completion —
against the operator's real ChatGPT login, without ever mutating the
operator's Codex configuration.

## Binding constraints

- **The operator's real ChatGPT subscription pays for every live turn.** Runs
  are purposeful. No soak tests, no retry loops, no "let me try five prompts".
  Every live turn should be one you can say in advance what it will prove.
- **Debug as we go.** Do NOT run the PRD's five spikes as an exhaustive
  pre-gate. Build the adapter, plug it in, and record a fixture when a
  question actually gets answered.
- `codex-codes` types and app-server JSON-RPC types may appear ONLY under
  `tidepool-agent/src/backend/codex/`. Everything crossing out is
  `tidepool-agent::seam` vocabulary. This is structural containment, not
  style: it is what makes vendoring or replacing the client a local change.
- This environment kills background processes at **~380s**. Structure every
  probe as a bounded FOREGROUND run with an explicit timeout. A long
  park-interval probe is a foreground run with a timeout, never a background
  daemon you poll.
- `seam::Workspace` is TRANSITIONAL (PRD 19 couples agent creation to worktree
  allocation, one worktree per agent). Use it, mark every use site
  transitional, and do NOT build a writer-lease or shared-workspace model.

## Phases — phase 4 is GATED, stop and report before it

### Phase 1 — config isolation, before anything else

PRD 18 acceptance criterion 11: no normal worker run mutates the operator's
global Codex configuration. Prove it before you spend a token, not after.

Build a small reusable checker (a test helper in this crate, not a shell
script you throw away). The naive approach — diffing all of `~/.codex` — is
useless here: that directory holds live sqlite databases (`logs_2.sqlite`,
`goals_1.sqlite`, `memories_1.sqlite`, plus `-wal`/`-shm`) that the operator's
own Codex sessions write to continuously. A whole-directory diff is pure noise
and will make you report a false positive.

Scope the assertion to **configuration and credentials**:

- `~/.codex/config.toml` — sha256 byte-identical before and after. This is the
  sharp one: Codex records project trust as entries under this file, and the
  documented project-trust write on a workspace-write thread start is exactly
  the mutation PRD 18 is trying to avoid. A new `projects.<path>` entry here is
  a FAILED isolation check, not a curiosity.
- `~/.codex/auth.json` — sha256 byte-identical. Credentials are read, never
  rewritten. If a run rotates this, stop and report; do not work around it.
- `~/.codex/installation_id` — unchanged.
- No new top-level files.

Record the pre-state, run, record the post-state, assert. Report the checker's
verdict as **per-file sha256 comparisons**, never as an exit code.

Do NOT copy or rewrite ChatGPT credentials into an isolated `CODEX_HOME` to
sidestep this. PRD 18 forbids it explicitly: that needs a separately designed
credential boundary, not an ad-hoc copy.

### Phase 2 — protocol truth, offline

The pinned CLI can tell you almost everything for free. `tidepool-agent/fixtures/app-server-0.146.0/`
already holds the complete generated protocol schema (committed; regenerate
with `codex app-server generate-json-schema --out <dir>`). Read it.

Add `codex-codes = "=0.146.4"` (exact pin) to `tidepool-agent/Cargo.toml` and
confirm it builds. Then answer, from the schema and the crate's generated
types — **no live run needed**:

1. **Where does `dynamicTools` attach?** This is the open question and it
   blocks everything. `DynamicToolSpec` and `DynamicToolNamespaceTool` exist in
   0.146.0's schema (function form: `{name, description, inputSchema,
   deferLoading?}`; namespace form wrapping a tool list) but nothing in the
   generated schema references them, and `ThreadStartParams.config` is an open
   `additionalProperties: true` object. Find the real attachment point in
   `codex-codes`'s types or the CLI's own source. If it turns out to be
   thread-scoped via `config`, say so precisely — the PRD's claim that tools
   are frozen per thread rather than per turn depends on this answer.
2. Does the initialize handshake need an explicit experimental-APIs opt-in, and
   what is it called at 0.146.0? (`codex app-server --enable <FEATURE>` and
   `-c features.<name>=true` exist as CLI-level switches; check whether the
   protocol has its own.) `ExperimentalFeatureListParams` /
   `ExperimentalFeatureEnablementSetParams` exist in v2 — check whether they
   are the mechanism.
3. Confirm the reply shape for `item/tool/call`: `DynamicToolCallResponse` is
   `{success: bool, contentItems: [...]}`. Note precisely how a **tool error**
   is expressed (`success: false` plus content, versus a JSON-RPC error), since
   a failing Haskell handler must produce it and must never strand a call.
4. Confirm `outputSchema` on `TurnStartParams` and what it accepts.

### Phase 3 — process lifecycle and handshake, live but token-free

Start `codex app-server` over stdio from Rust, complete initialization, call a
metadata request that spends no model tokens (`model/list` or the v2
equivalent), and shut the process down cleanly. Assert phase 1's isolation
check across this.

Record the actual JSONL frames (initialize request/response, the metadata
round-trip) as a fixture under `tidepool-agent/fixtures/`. Redact nothing that
matters and include nothing from `auth.json`.

Prove clean shutdown: no orphaned `codex` process survives the test. Scope any
kill to a PID or the full worktree path — **never** a path-unscoped `pkill -f`.

### Phase 4 — GATED. Report to the TL and wait.

Before the first turn that spends model tokens, `notify_parent` with:

- the phase 1 isolation verdict (per-file sha256s);
- the answer to "where does `dynamicTools` attach";
- the exact thread-start + turn-start request bodies you intend to send;
- what the single live turn will prove.

Then STOP and wait for the go. Do not start a live turn on your own judgment.
This gate exists because the operator's subscription pays, not because the
work is risky.

After the go, the live vertical, in one bounded run where possible:

1. Create an **ephemeral** thread with one generated tool, `ask_parent`, whose
   input schema is a single string field. Omit `cwd` from thread start and
   supply it at turn start — the PRD's preferred shape for avoiding the
   project-trust write. If the schema forces `cwd` at thread start, report that
   instead of working around it silently.
2. Start a turn in a **temporary** workspace (a `tempfile` dir, never this
   worktree, never the parent repo), with `sandbox` scoped to that directory
   and an `outputSchema` for a small terminal result.
3. Prompt the worker so that calling `ask_parent` is the obvious path — e.g.
   it must learn one fact only the parent knows before it can finish.
4. Receive `item/tool/call`, correlate on `callId`, reply from Rust with a
   `DynamicToolCallResponse`, and prove **the same turn resumes** and reaches a
   structured completion your driver decodes.
5. Assert phase 1 isolation across the whole run.

Opportunistically, while you are already there and only if it costs no extra
turn: note the observed park duration you exercised, and whether the pending
call's correlation carries `threadId`/`turnId` (the cross-agent misroute
guard). Do NOT add runs to chase the other spike questions — steer/interrupt
while parked, multi-agent correlation, and the `finish_task` comparison are
wave-2 work.

## Deliverables

- `tidepool-agent/` compiles, `cargo nextest run -p tidepool-agent` green,
  clippy and fmt clean.
- The isolation checker is a committed test helper, not a scratch script.
- Fixtures committed with the pinned CLI **and** crate versions stated in the
  fixture directory or a sibling README.
- A receipt at `plans/post-restart/agent-lanes/receipt-adapter-bringup.md`:
  what each phase proved, the live-turn transcript summary, per-file isolation
  sha256s, and every spike-checklist question that got a real answer (with the
  ones still open named as open).
- Report counts, never exit codes: "N/N tests pass", "config.toml sha256
  identical", not "the test passed".

## Verification

- `cargo nextest run -p tidepool-agent`
- `cargo check --workspace` before submitting
- `cargo clippy --workspace` and `cargo fmt --all -- --check` clean

This crate is pure Rust with no GHC dependency, so the fast default tier
covers it. You should not need a GHC slot at all; if you think you do, you
have wandered out of your lane.

## Operational rules (VERBATIM — do not paraphrase, do not skip)

- Every GHC-heavy run goes through
  `/home/inanna/dev/tidepool/scripts/ghc-slots.sh run -- <cmd>` (absolute
  path). NEVER `exclusive` mode.
- `export XDG_CACHE_HOME="$PWD/.cache"` before any tidepool-harness test shard
  (persistent per-worktree, not mktemp).
- Spawns pass an explicit `model: sonnet` (or `opus` for sub-TLs); never fable.
- Never path-unscoped `pkill -f`; scope kills to PID or full worktree path.
- Commit with `--no-verify`. Never `git add -A`. Repo-root `tmp/` is protected.
  Grep/Read over LSP.
- Inherited-red claims require a cache-consistent A/B in the dev's own worktree
  (same cache state both legs, diff absent vs present); diff-file-overlap
  arguments are invalid for global surfaces.
- Never run bare `scripts/battery.sh`; use targeted tier 2/3 runs.
