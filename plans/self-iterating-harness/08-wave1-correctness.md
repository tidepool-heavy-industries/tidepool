# Wave 1 — architectural correctness (wire the thesis)

An independent full-review pass (post-v1, HEAD cec66863) found the core thesis
is **not actually wired**. This wave fixes that plus the correctness gaps it
surfaced — the critical path to the distillation loop (Wave 2). Verify policy:
targeted only (07's policy — `scripts/battery.sh -p <crate> -E 'binary(<x>)'`,
never bare battery; env kills bg procs at ~380s).

## What the review found (code vs thesis)

- **C1 — `render` never reaches the model.** `assemble_request` uses the
  hardcoded `SYSTEM_FRAMING` only; `render_framing`'s output goes into
  `CycleOutcome` (which the tests assert on) but never into the agent's system
  prompt. `render` is observational — a distilled conditional has no effect.
- **C2 — no context window.** `service_runllm_hole` `create_root`s a fresh,
  context-free node per hole; N holes = N isolated RPCs. The hylo's fused
  intermediate doesn't exist in the implementation.
- **C3 / D2 — compaction watches the wrong number** (2048 per-turn *output* cap,
  not context) and fires only *after* the loop (can't end anything early;
  summarizes a context-free node → confabulates).
- **C4 — a non-`finalize` suspension hard-crashes the cycle**, though
  `SYSTEM_FRAMING` tells the model to use `dialogForm`/nested `runLLMTurn`.
- **D5 — State-to-disk + restart-reload is unbuilt** (§04 durability + the
  distillation substrate — no writer, no reader; State survives in-process only).
- Smaller: **H1** (`Harness = M` / `HarnessEff` diverge once base effects grow),
  **H3** (`ask_tag` misnamed → `suspend_tag`), **C5** (`unwrap_or_default` →
  `ok_or`), **D1** (model-name inconsistency + `gpt-5.4-mini` isn't a real id),
  **D3** (`Closing` unreachable), **D4** (redundant `initial_state_json`
  compile), **J1** (`state_in` `error` → typed `DriverError`), **J2**
  (`haskell_string_literal` reimplements the input-lane escaper).
- **Solid (keep):** the DataConId import-the-module fix, the shared `suspend_tag`
  threshold, `finalize`'s `forall v a` shape, the async bridge.

## Locked decisions (this session)

- **Context window = one render-seeded Agent session per loop.** `render`'s
  output is that session's **system prompt** (fixes C1). The Agent's effects =
  **gui (`dialogForm`/`Ask`) + `finalize`**, NOT `runLLMTurn` (no recursive
  model-spawning). Each `runLLMTurn` hole is a **bounded multi-turn interaction**
  in the session: up to **16** tool-call rounds accumulating context (fixes C2);
  at 16 the runtime nudges ("approaching max tool calls, finalize now with
  `@A`"); at **32** it hard-fails the `runLLMTurn` effect. Budget is per-hole;
  the session persists across the loop's holes.
- **Runaway guards — three cumulative caps.** (1) Per-hole: the 16-round nudge /
  32-round hard-fail above. (2) **Per-loop total inference-call cap = 1024** —
  hard-stop the loop if total model calls across all its holes + rounds reaches
  1024. (3) **Between-loops human gate** — `run_loop` prompts "press Enter to
  continue" before each new cycle, defeatable via a `--yes`/`--auto` flag for
  CI/replay (the acceptance path drives `run_one_cycle` directly and bypasses
  it). These keep a misbehaving harness from running away regardless of
  compaction.
- **Compaction = runtime-owned Rust, mid-loop, in-place relief.** At ~80% of a
  real **context-window budget** (a NEW config — not `max_tokens`, which is a
  2048 per-turn *output* cap), summarize the inner Agent session's transcript to
  text, **replace its context with the summary so the loop CONTINUES**, and carry
  the summary to the next `render`'s `Maybe Text`. **No loop-abort plumbing.**
  Entirely outside the harness Haskell's control (`render` is a passive
  recipient — that's why it takes the optional summary).
- **Persistence = local files.** State → json (survives restart), transcript/log
  → jsonl. `run_loop` persists State each cycle + restores on start; restart
  reloads the harness module (the §02/§04 versioning mechanism).
- **Closures through `finalize` = reference-passing.** Don't deep-force the
  finalized value (the heap bridge rejects closures by a tested invariant); keep
  the closure **live in the shared heap** and hand back a **reference** the
  harness applies in place — full "it's just Haskell" comms between harness and
  agent code.

## Workstreams

- **W1 (opus) — context-window + answerer reshape** (C1+C2+C4). Foundational.
  Reshape `service_runllm_hole` → one render-seeded Agent session per loop,
  bounded multi-turn holes (16 nudge / 32 hard-fail); thread `render`'s output
  into `assemble_request` as the system message; scope the Agent effect set to
  gui + `finalize` (drop `runLLMTurn` from the answerer); the per-loop
  1024-inference-call hard cap; and the between-loops press-Enter human gate in
  `run_loop` (with a `--yes`/`--auto` skip). **Merge first.**
- **W2 (opus) — compaction, in-place relief** (C3/D2). New context-window budget
  config; watch the inner session's real size; at threshold, mid-loop, summarize
  + replace the session context (loop continues) + carry to the next render's
  `Maybe Text`. After W1.
- **W3 (sonnet) — persistence + reload** (D5). State→json (persist each
  cycle, restore on start), transcript→jsonl; restart reloads the harness. After
  W1 (touches `run_loop`).
- **W4 (opus) — closures through `finalize`** (reference-passing). Keep the
  finalized closure live in the shared heap, hand back a reference. Mostly
  independent; runs alongside W1.
- **W5 (sonnet) — hardening batch.** Aeson nullary-sum `Generic` derive; the
  pre-existing failures (`more_text_recursive` ×7, `works_ui`,
  `forkmap_rejects_partial_application`); model-name fix (D1); review + discard
  `acceptance_conv_dialogue`; smaller debt (H1/H3/C5/D3/D4/J1/J2). Independent;
  alongside W1.

**Sequencing:** W1 first (foundational, touches the shared driver/engine/harness
loop). **W4 + W5 parallel with W1** (low file overlap). **W2 + W3 after W1
merges** (they build on the reshaped context window / `run_loop`). Merge order
serial with a `cargo check` gate between folds.
