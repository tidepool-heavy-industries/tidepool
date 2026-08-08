# Dev spec: reasoning-continuity

We pay to receive encrypted reasoning items from the OAuth backend and then
throw them away. The model enters every turn of a multi-turn exchange seeing
its prior answer but not its prior thinking — most damagingly in the
corrective-retry loop, which is exactly where live dogfooding hurts.

**Queued, not yet spawned:** the fix touches `harness.rs`, which
`registry-unify` is rewriting. Spawn after that folds.

## The defect (verified at HEAD)

`provider/oauth.rs`'s `codex_responses` sends `"store": false` (~579) and
`"include": ["reasoning.encrypted_content"]` (~581) — a correct stateless
Responses setup. The encrypted reasoning items then arrive in the SSE stream
and are dropped on the floor: `to_input_item` (~516) reconstructs ONLY
role/content text messages for the next call's input.

Stateless Responses usage requires echoing prior reasoning items back in
subsequent input. We ask for them, we are billed for them, we discard them.

What IS captured today is the reasoning **summary text**
(`response.reasoning_summary_text.delta`, ~680) → `TurnResponse.reasoning`.
That is the human-facing "thinking" string, NOT the encrypted item the backend
wants echoed. They are different things; do not conflate them.

## The design — decided, do not re-derive

The obvious shape ("a per-convo side-store next to `transcript`") is the wrong
one. `TurnRequest { messages, max_tokens }` carries **no node identity**, and
the provider is a single shared `Arc<dyn DynModelProvider>` across every node
(one signed-in client), so a provider-internal map cannot key by node. Pushing
node identity into the provider to fix that would be a much larger change than
this defect deserves.

**Instead: extend `Message` with an opaque, non-serialized reasoning payload.**

- `NodeConvo.transcript: Vec<Message>` is ALREADY the per-node store, and is
  already threaded into `TurnRequest.messages`. Reuse it. No new `NodeConvo`
  field, no new plumbing through take/put.
- Interleaving in the message list preserves original position order for free —
  which is precisely what the stateless-Responses echo requires.
- Add the field as `#[serde(skip)]` (or equivalent). **This keeps the durable
  wire format untouched by construction:** `log::Event::TurnDelta` (~123)
  serializes `role`/`content`/`usage`/`reasoning` as its own fields — it never
  serializes a `Message` — so the opaque blob cannot leak into the log. Verify
  that claim yourself before relying on it.
- Continuity across process restart is NOT required (a restart already loses
  the parked session), which is why in-memory-only is correct here.

## Steps

1. Capture the encrypted reasoning items from the SSE stream in `oauth.rs`,
   alongside the existing `reasoning_summary_text` accumulation. Keep them
   opaque — do not parse or reshape them.
2. Return them on `TurnResponse` (a new opaque field, same `#[serde(skip)]`
   discipline).
3. In `harness.rs`, attach them to the assistant `Message` appended after the
   turn. There are three assistant-append sites (~989/999, ~1079/1089,
   ~2384/2396 — **re-locate; those line numbers pre-date the registry-unify
   fold**). All three, or the continuity is silently partial.
4. In `to_input_item`, emit the carried reasoning items in original position
   order alongside the text messages.
5. Providers that never produce them (`ApiKeyProvider`, `ReplayProvider`) carry
   `None`/empty and are unaffected. Confirm `ReplayProvider` still round-trips.

## Also in scope — the empty first user turn_delta (small, same file)

Every answerer node's transcript and durable log opens with an EMPTY user
turn: `{"ev":"turn_delta", "turn":0, "role":"user", "content":""}`.

Cause, already traced — do not re-investigate: `selfharness/driver.rs`
(~887) creates the answerer with `create_root_framed("loop answerer", "", …)`.
The empty prompt is deliberate: an answerer's context comes from the `render`
framing (its system message), not from a user turn. `create_root_framed`
stashes that prompt in `seeds`, and `Harness::force_inner` then logs it
**unconditionally** as `turn_delta(node, 0, Role::User, seed)`.

Fix: skip the seed `turn_delta` when the seed is empty. A node with no opening
prompt has no opening user turn — logging one is a false record, and it is the
first line a person reads when tailing a run.

Do NOT change `create_root_framed`'s signature or make the empty prompt an
error: an intentionally-framing-only node is a legitimate shape, and the
self-iterating harness depends on it.

Pin it with a test asserting the answerer's log has no empty user turn_delta at
turn 0, and that a node created WITH a prompt still logs it.

## Verify

The live backend is not unit-testable. **The fixture-server tests in
`oauth.rs` (~824) are the mechanical assertion:** a reasoning item present in a
fixture response must appear in the NEXT request's `input`, in position.

- Mutation-close it: drop the echo in `to_input_item` → the test goes red.
  Report the mutant's assertion message.
- Assert the durable log is unchanged: a turn carrying reasoning items must
  produce a byte-identical `TurnDelta` to one without. That is the guard on
  the wire-format caveat.

Standard tiers: `cargo check --workspace --all-targets`, `cargo fmt --all --
--check`, `cargo clippy --workspace` (three pre-existing warnings —
tidepool-codegen `large_enum_variant`, `engine.rs` `TurnOutcome`
`large_enum_variant`, `selfharness_compaction_fixes` `type_complexity` — are
not yours). Quick tier `cargo nextest run` with the tests-RUN count. GHC-heavy:
`acceptance_selfharness`, `golden_path`, `acceptance_cross_turn` are enough —
this does not touch the compile path.

Extract, shared and READ-ONLY (never rebuild it, never touch `haskell/`):

```
export PATH=/nix/store/i7xkw0wd599j23fbsz8ydmsfj4dp9831-ghc-native-bignum-9.12.2-with-packages/bin:$PATH
export TIDEPOOL_EXTRACT=/home/inanna/dev/tidepool/haskell/dist-newstyle/build/x86_64-linux/ghc-9.12.2/tidepool-extract-0.1.0.0/x/tidepool-extract-bin/build/tidepool-extract-bin/tidepool-extract-bin
```

## Contention rules — verbatim, non-negotiable

- Every GHC-heavy run through
  `/home/inanna/dev/tidepool/scripts/ghc-slots.sh run -- <cmd>` (absolute
  path). NEVER `exclusive` mode. Do not override `.config/nextest.toml`'s
  default-deny `ghc-heavy` group.
- No LSP / rust-analyzer — `grep` and `Read` only.
- Scope every kill to your OWN PID or worktree path. NEVER a bare `pkill -f`.
- `--no-fail-fast` on any suite with a known red; gate on tests-RUN counts,
  never exit codes; capture full output to files and extract after.
- Never `git add -A`; never force-push; repo-root `tmp/` is protected; commit
  with `--no-verify` (standing directive).
- A flaky test never lands.

## Done criteria

- Encrypted reasoning items captured, carried per-node on the transcript, and
  echoed in position on the next request.
- Fixture-server echo test green and mutation-closed.
- Durable log byte-identical — asserted, not assumed.
- All three assistant-append sites covered.
- check / fmt / clippy clean; tiers reported with tests-RUN counts.
