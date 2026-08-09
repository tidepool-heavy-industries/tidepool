# agent-wave lanes — PRD 18 (typed headless subagents)

TL spec: [`../agent-wave.md`](../agent-wave.md). Design authority:
[`../../self-iterating-harness/18-typed-subagent-spawning-prd.md`](../../self-iterating-harness/18-typed-subagent-spawning-prd.md).
This directory is the wave's plan/receipt namespace. Worktree-wave owns
`../worktree-lanes/`; do not write there.

## Wave 1 lanes

| Lane | Owns | Deliverable |
|---|---|---|
| `dev-adapter-bringup` | `tidepool-agent/` (Rust) | Config isolation proven, app-server handshake, protocol fixtures pinned |
| `dev-mode-encoding` | `haskell/lib/Tidepool/Agent/Contract.hs`, `tidepool-runtime/tests/agent_mode_encoding.rs` | Gate 1(a) verdict + the eDSL contract algebra |
| `dev-structural-codec` | `haskell/lib/Tidepool/Agent/CodecSpike.hs`, `tidepool-runtime/tests/agent_structural_codec.rs` | Gate 1(b) verdict: list + recursive ADT round-trip on the real JIT |

Wave 2 (after wave 1 folds) carries the adapter's live vertical:
task → `item/tool/call` → typed reply → resume → structured completion.

## Scaffold already committed

`tidepool-agent/` is the containment crate. Its `src/seam.rs` is the
Tidepool-owned vocabulary; `src/backend/codex/` is the ONLY place
`codex-codes`, app-server JSON-RPC, or the word "Codex" may appear. That
boundary is PRD 18's non-goal "making `codex-codes` types part of Tidepool's
public Rust or Haskell API", made structural.

## Pinned backend versions

- Codex CLI: **0.146.0** (`codex --version` → `codex-cli 0.146.0`), from the
  operator's nix profile.
- `codex-codes`: **0.146.4** (latest on crates.io; the crate tracks CLI
  versions, and 0.146.x is the matching family — the patch-level skew against
  the CLI is recorded deliberately, not assumed harmless, and any protocol
  mismatch found during bring-up is attributed here first).

Pinning is CLI-and-crate together. Dynamic tools are an experimental
app-server surface; a version bump re-runs the fixtures rather than refreshing
a lockfile.

## Protocol reconnaissance already done (TL, offline, zero token spend)

`codex app-server generate-json-schema --out <dir>` emits the complete
version-matched protocol schema from the pinned CLI. Verified: it does not
touch `~/.codex` (top-level file size/mtime snapshot identical before and
after). This is the cheap way to answer protocol-shape questions — read the
schema before spending a ChatGPT turn on the same question.

Established from the 0.146.0 schema:

- `item/tool/call` **is** a `ServerRequest` — a server→host request that awaits
  a host response. That is the park/reply primitive the whole design rests on,
  present and not merely documented.
- `DynamicToolCallParams` carries `{threadId, turnId, callId, tool, arguments,
  namespace?}`. `callId` is the correlation token; `threadId`+`turnId` are what
  make cross-agent misrouting detectable.
- `DynamicToolCallResponse` is `{success: bool, contentItems: [...]}` where a
  content item is `inputText`/`inputImage`/`inputAudio`. **A tool *error* is
  `success: false` with content, not a JSON-RPC error** — that is the shape a
  failing Haskell handler must produce so it never strands a pending call.
- `TurnStartParams` requires `{threadId, input}` and accepts `cwd`, `model`,
  `effort`, `outputSchema`, `sandboxPolicy`. `outputSchema` at turn start is
  therefore available for structured completion.
- `ThreadStartParams` has no required fields and accepts `cwd`, `ephemeral`,
  `model`, `sandbox`, `developerInstructions`, `baseInstructions`, `config`.
  Supplying `cwd` only at turn start is expressible — which is the shape PRD 18
  wants for avoiding the project-trust write.
- **Open, and the first thing bring-up must resolve:** `dynamicTools` is not a
  declared property of `ThreadStartParams` or `TurnStartParams` in 0.146.0.
  `DynamicToolSpec` / `DynamicToolNamespaceTool` definitions exist (function
  and namespace forms, each `{name, description, inputSchema, deferLoading?}`)
  but nothing in the generated schema references them, and `ThreadStartParams.config`
  is an open `additionalProperties: true` object. Find the real attachment
  point from `codex-codes` and the CLI's own source before any live run.

Schema regenerate command (idempotent, offline, safe):

```bash
codex app-server generate-json-schema --out tidepool-agent/fixtures/app-server-0.146.0/
```

## HOLD lines (root announces each lift; all intact as of wave 1)

1. **generic-surface's fold** — no consumption of their Generic metadata
   utilities, no `16-generic-spike-receipts.md`, no `Harness.Prelude`
   integration, and no edit to `haskell/lib/Tidepool/Form.hs`, their Generic
   substrate, or `tidepool-mcp/src/preamble.rs`. All agent-wave Haskell is NEW
   files until then.
2. **worktree-wave's vertical core** — the coupled-spawn seam is designed
   JOINTLY, via root, when both sides are ready. Until then `seam::Workspace`
   is transitional data and every use site says so.
3. **root's realm step-4 go-signal** — nothing in `resident.rs`
   pending/`ChildSuspended`.

The realm parking machinery is consumed ONLY through
[`../realm-lanes/continuation-parking-contract.md`](../realm-lanes/continuation-parking-contract.md),
never by reading `jit_machine.rs`. Its consumer guidance is binding: derive the
declared handled prefix from the same value that constructed the handler stack,
never re-declare it at a dispatch site.

## Naming collision to resolve before the vertical core

`Tidepool.Agent` is already taken — `haskell/lib/Tidepool/Agent.hs` is the
harness answerer's capability row (`Eff '[AskUser, Fork, Finalize]`), unrelated
to PRD 18. PRD 18 asks for the public surface at `Tidepool.Agent`. Wave 1 sits
under `Tidepool.Agent.*` (legal alongside the existing module) and does not
rename anything. The rename-or-relocate decision is root's, taken with the
harness owner, not a lane's to make unilaterally.
