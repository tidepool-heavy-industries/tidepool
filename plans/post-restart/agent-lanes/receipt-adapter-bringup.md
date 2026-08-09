# Receipt — dev-adapter-bringup (PRD 18 Codex app-server adapter)

Lane: `plans/post-restart/agent-lanes/dev-adapter-bringup.md`. Branch:
`root.agent-wave.adapter-bringup`. All work confined to `tidepool-agent/`.

## Summary

All four phases landed, including the gated phase-4 live turn, which passed
completely on its one permitted attempt. Config isolation held across every
run — offline, token-free live, and token-spending live. `dynamicTools`
attaches as an experimental-gated, undocumented-by-schema field of
`ThreadStartParams`; `codex-codes` 0.146.4 doesn't expose it, so it's
hand-rolled under `backend::codex::dynamic_tools` and sent via the crate's raw
`request()` escape hatch, exactly as PRD 18 anticipated.

## Phase 1 — config isolation

Checker: `backend::codex::isolation::{ConfigSnapshot, IsolationReport}`.
Scoped to `config.toml`/`auth.json`/`installation_id` sha256s plus a
top-level file-listing diff — not a whole-`~/.codex` diff. Refined once,
empirically: `codex app-server` starting causes SQLite to create `-shm`/
`-wal` sidecars for `goals_1.sqlite`/`memories_1.sqlite`/`state_5.sqlite` on
first open even though their content is untouched; the checker now excludes
a new sidecar for an *already-existing* `.sqlite` base file while still
flagging one for a genuinely new database. Both cases covered by unit tests
(`wal_sidecar_appearing_for_a_preexisting_database_is_ignored` /
`wal_sidecar_for_a_brand_new_database_is_still_flagged`).

**Per-file sha256, held identical across phase 3 (handshake+model/list) AND
the phase 4 live turn (thread/start+turn/start+item/tool/call+turn/completed),
independently re-verified outside the test process both times** (a checker
that only ever validates itself is not evidence — the outside check is what
makes the "identical" claim credible, not just the assertion inside the test
that wrote the fixture):

| File | sha256 |
|---|---|
| `config.toml` | `a8e4e036c780554b55cab718649628f631148cd87ab379af84ee85be45356b65` |
| `auth.json` | `264a9dd25b0677d4869f864411ddfffedbca861fb154f8a1dfc715bdd56bba9d` |
| `installation_id` | `ea5a60ad-5d2d-41c3-8ce0-d0e3b732030f` |

New top-level files: none, in either run (after excluding the characterized
WAL sidecars). No orphaned `codex` process after any run (`/proc/<pid>`
polled to absence on shutdown; also independently checked via `ps aux`).

**Live-turn-specific check** (per the go's isolation condition): re-read
`config.toml` fresh after the turn and asserted byte-for-byte text equality
with the pre-run read, AND asserted the text does not contain the turn's
tempdir path. `config.toml` already held 3 pre-existing `[projects."..."]`
entries from the operator's own prior interactive CLI use
(`/home/inanna/dev/tidepool`, `/home/inanna/dev/exomonad`,
`/home/inanna/.claude/projects/.../memory`) — none for the ephemeral tempdir,
confirming the project-trust write did not fire for the cwd-at-turn-start
shape.

## Phase 2 — protocol truth (offline)

`codex-codes = "=0.146.4"` pinned, builds clean. Full sourcing in
`tidepool-agent/fixtures/app-server-0.146.0/PROTOCOL-NOTES.md`, extended past
the schema into `openai/codex`'s own source at git tag `rust-v0.146.0` (the
schema alone is insufficient — it silently drops `#[experimental(...)]`-gated
fields, which is why `dynamicTools` looked absent at first; PROTOCOL-NOTES.md
now states this caveat plainly, per the go).

| Question | Answer |
|---|---|
| Where does `dynamicTools` attach? | Top-level field of `ThreadStartParams`, sibling to `cwd`/`config`/`model`. NOT nested in `config`. Thread-scoped (frozen at creation). Gated by `#[experimental("thread/start.dynamicTools")]`; absent from `codex-codes` 0.146.4 entirely (schema-generation drops experimental fields at every version — hand-rolled under `backend::codex::dynamic_tools`, sent via `request()` raw escape hatch). |
| Experimental-APIs opt-in | `InitializeParams.capabilities.experimentalApi: bool`. Typed in `codex-codes`. `ExperimentalFeatureListParams`/`ExperimentalFeatureEnablementSetParams` are a *different*, unrelated mechanism (named feature-flag admin API — confirmed no `dynamic_tools`-named entry in `codex features list`'s ~100 flags). |
| Tool-error shape | `success: false` + `contentItems`, never a JSON-RPC error. Confirmed against the CLI's own `fallback_response` (`app-server/src/dynamic_tools.rs`), used on every decode/transport failure. |
| `outputSchema` on `TurnStartParams` | Arbitrary JSON Schema, not gated, fully typed in `codex-codes`. Constrains the *text* of the final `agentMessage` thread item — there is no separate structured-output field on `Turn`/`ThreadItem`; the model's final message text IS the schema-conforming JSON, decoded by the driver. |

**Finding recorded, not decided** (per the go): PROTOCOL-NOTES.md's new "what
`codex-codes` is actually doing for us" section lays out precisely what's
load-bearing (process lifecycle, raw JSONL framing + stderr drain, JSON-RPC
envelope types, and generated types for the entire STABLE surface used) vs.
what it doesn't provide (anything on the experimental `dynamicTools` surface,
and no correlation/dispatch logic this crate actually calls — built our own
either way, for reasons independent of the gap). Vendor-or-keep is root's
call on that evidence.

## Phase 3 — process lifecycle, live, token-free

`backend::codex::process::Session` drives the raw JSONL transport directly
(not `codex_codes::AsyncClient`) so every frame is capturable. One run:
`initialize` → `initialized` → `model/list` → clean shutdown. Fixture:
`fixtures/app-server-0.146.0/phase3-handshake.jsonl` (6 frames). Isolation
held (table above). Model catalog observed: default `gpt-5.6-sol`, plus
`gpt-5.6-terra` (pinned for phase 4 per the go), `gpt-5.6-luna`, `gpt-5.5`,
`gpt-5.4`, `gpt-5.4-mini`.

## Phase 4 — GATED live turn

Go received from agent-wave (2026-08-08) with four conditions: pin
`gpt-5.6-terra`; reply to `item/tool/call` promptly, no park-duration
probing; retry freely before `turn/start`, ONE attempt after; record the
correlation triple. All four honored.

**Free dry-run first** (no tokens — no `turn/start`): confirmed
`initialize{capabilities.experimentalApi:true}` + hand-rolled
`dynamicTools` on `thread/start` is accepted by the real 0.146.0 server.
Re-run as many times as needed at zero cost; only once this was solid did the
one live turn fire.

**The one live turn** (`phase4_live_vertical_ask_parent_round_trip`, run
exactly once): ephemeral thread with one `ask_parent` tool (single `question`
string field), `cwd` omitted at `thread/start` and supplied only at
`turn/start` (a fresh `tempfile::tempdir()`, never this worktree or the
parent repo), `sandboxPolicy: workspaceWrite` scoped to that dir,
`model: "gpt-5.6-terra"`, `outputSchema` requiring `{result: string}`. Prompt
told the worker it needed a passphrase only the parent knew.

Result: **passed completely, first attempt, ~7s wall-clock.**

- `item/tool/call` fired for `ask_parent` with arguments
  `{"question":"What is the secret passphrase?"}`.
- **Correlation triple observed:** `threadId=019fe4de-3248-7182-9ed9-8e8232dfe824`,
  `turnId=019fe4de-3309-75b1-8a4b-41f926ae46f7`,
  `callId=exec-f115b10a-9448-43d5-8865-bae933ff44c7`. Both `threadId` and
  `turnId` ride the correlation payload, as the README's reconnaissance
  predicted — the cross-agent misroute guard wave 2 needs is present.
- **Park duration observed: 0ms** (rounds to zero — the driver replies
  synchronously with no artificial delay, per condition 2; this is a floor
  for wave 2's ladder, not a park-duration probe in its own right).
- Rust replied `DynamicToolCallResponse{success:true, contentItems:[InputText]}`.
- **The same turn resumed** (no new `turn/start`) and reached
  `turn/completed` with `status: Completed`.
- The final `agentMessage` item's text decoded as JSON and
  `result` matched the passphrase (`tidepool-cobalt-7-do-not-reuse`)
  byte-for-byte.
- Isolation held (table above), including the fresh `config.toml`
  text-equality + tempdir-absence check.

Full 35-frame transcript committed:
`fixtures/app-server-0.146.0/phase4-live-turn.jsonl`. Frame shape:
`initialize` → `initialized` → `thread/start` → `turn/start` →
`thread/started` → several `mcpServer/startupStatus/updated` +
`thread/settings/updated` → `turn/started` → two prior `item/started`/
`item/completed` pairs (reasoning/plan items) → `item/tool/call` (frame 23) →
our reply (frame 24) → `item/completed` → usage/rate-limit notifications →
final `item/started`/`item/agentMessage/delta`/`item/completed` →
`turn/completed`.

## Spike checklist — real answers vs. still open

| Spike | Status |
|---|---|
| 1. Parked typed tool | **Answered, live.** `item/tool/call` fires, park/reply/resume all confirmed against the real 0.146.0 server, not just source. |
| 2. Steer/interrupt while parked | **Open — deliberately out of scope this run** (condition 2 forbids probing park duration; PRD/README name this wave-2 work). |
| 3. Concurrency / one-server ownership | **Open — wave 2** (README: multi-agent correlation is wave-2 work; this run was single-agent). |
| 4. Structured completion | **Partially answered.** Successful `outputSchema`-constrained completion confirmed end-to-end. Malformed/semantically-rejected results and the `finish_task` tool comparison are **open — wave 2** per the README's explicit deferral. |
| 5. Configuration isolation | **Answered, definitively**, across offline, token-free-live, and token-spending-live runs, with an explicit fresh check for the specific project-trust write this shape is meant to avoid. |

## Deliverables checklist

- `cargo nextest run -p tidepool-agent`: **15/15 tests pass** (fast default
  tier; 3 live-process tests correctly skipped by `#[ignore]`, run
  separately and reported above).
- `cargo check --workspace`: clean.
- `cargo clippy --workspace --all-targets`: clean (pre-existing warnings in
  `tidepool-harness`/`tidepool-codegen` only, none in `tidepool-agent`).
- `cargo fmt --all -- --check` (scoped to `tidepool-agent`, the only crate
  touched): clean.
- Isolation checker: committed test helper
  (`backend::codex::isolation`), not a scratch script.
- Fixtures committed with pinned CLI (`0.146.0`) and crate (`0.146.4`)
  versions stated in `fixtures/README.md` and `PROTOCOL-NOTES.md`.
- HOLD lines intact: no edits to `haskell/`, `tidepool-runtime/tests/`,
  `resident.rs`, or realm parking outside the continuation-parking contract
  (none of that territory was touched — this lane never needed to).

## What's next (wave 2, not this lane)

Steer/interrupt while parked, multi-agent concurrency and cross-agent
misroute correlation (the triple is already flowing — this is proving it
under contention), `finish_task` vs. bounded-retry comparison, and the
vendor-or-keep decision on `codex-codes` given the dynamicTools gap.
