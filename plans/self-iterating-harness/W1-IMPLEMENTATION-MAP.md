# W1 implementation map (traced) — context-window + answerer reshape

Traced against HEAD ce4bb16a. Every edit below is located; execute as a
mechanical job (ideally one focused dev per section, they're mostly disjoint
after C1's threading lands). Verify TARGETED (`scripts/battery.sh -p
tidepool-harness -E 'binary(<x>)'`, nix develop).

## C1 — `render` becomes the answerer's SYSTEM message (per-node framing)

The answerer's system message is hardcoded: `assemble_request` (engine.rs:312)
pushes `SYSTEM_FRAMING` (engine.rs:257). The path from a node's turn down to it:
`drive_turn` (harness.rs:655) → `stream_turn` (harness.rs:333) →
`drive_model_turn` (engine.rs:748) → `assemble_request`. Thread a per-node
`framing` through it:

1. **engine.rs:312** `assemble_request(transcript, max_tokens)` → add
   `framing: Option<&str>`; `system = framing.unwrap_or(SYSTEM_FRAMING)`.
2. **engine.rs:748** `drive_model_turn(provider, transcript, max_tokens, sink)`
   → add `framing: Option<&str>`, pass to `assemble_request`.
3. **harness.rs:333** `stream_turn(node, transcript)` → add
   `framing: Option<&str>`, pass to `drive_model_turn`.
4. **harness.rs:100 `NodeConvo`** → add `framing: Option<String>`.
5. **harness.rs:655 `drive_turn`** → snapshot `convo.framing` alongside the
   transcript (under the same lock, lines 657-661), pass `framing.as_deref()`
   to `stream_turn`.
6. **harness.rs:520 `create_root`** → add `create_root_framed(title, prompt,
   framing: Option<String>)`; keep `create_root` = `create_root_framed(.., None)`.
   Store framing next to the prompt: change `seeds` from
   `HashMap<NodeId, String>` to `HashMap<NodeId, (String, Option<String>)>`
   (insert at create_root.rs:533; the ONLY other use is force's remove at
   harness.rs:581-585).
7. **harness.rs:539 `force`** → when it removes the seed (581-585), destructure
   `(prompt, framing)`; set `NodeConvo.framing = framing` in the insert (596-607).
   (The forked-transcript branch, 573-576, keeps `framing: None` — fork
   answerers aren't render-seeded; that path is unused by the self-harness.)
8. **driver.rs** — build the answerer framing and pass it. The system prompt =
   `render`'s output + a NARROW answerer instruction (NOT the full
   `SYSTEM_FRAMING`, which advertises `runLLMTurn`/`run`/etc — see effect-scoping
   below). Add a driver const `ANSWERER_FRAMING_SUFFIX` explaining: you answer by
   using `dialogForm`/`Ask` across turns, then `finalize @A value`. Framing =
   `format!("{render_out}\n\n{ANSWERER_FRAMING_SUFFIX}")`. Store the current
   render output on the driver (set in `run_one_cycle` before
   `run_loop_fragment`) so `service_runllm_hole` can build the framing.

**Test (proves C1):** assert the assembled request's System-role content
contains render's text. Unit-testable on `assemble_request` directly + an
integration assertion that the answerer node's framing is render-derived.

## C2 — one render-seeded Agent session per loop (holes accumulate)

Today `service_runllm_hole` (driver.rs:450) `create_root`s a FRESH node per hole
(465) → no accumulation. Reshape:

- Driver holds `answerer: Option<NodeId>` (the per-loop session). Create it ONCE
  at loop start (in `run_loop_fragment`, before running `loop`), via
  `create_root_framed("loop answerer", "", Some(render_framing))` + `force`.
- `service_runllm_hole`: instead of create_root, `push_user_turn`
  (harness.rs:2166) the hole's `hole_card(prompt, ty)` onto the EXISTING
  `answerer` node, then drive it (the bounded multi-turn loop below) to finalize.
  The node's transcript persists → hole #2 sees hole #1 (fixes C2).
- Clear `answerer` at loop end (drop the node) so the next loop gets a fresh
  render-seeded session.

## Effect-scoping — answerer stack = gui + finalize only

The answerer must NOT have `runLLMTurn` (no recursion) or the base effects.
`self.agent` is built by the caller (binary) with `EngineConfig::standard` (full
stack). Build the driver's answerer `Harness` with a SCOPED decl list =
`[ask_decl(), finalize_decl()]` (Ask provides `dialogForm`/`Ask`; Finalize
provides `finalize`) — mirror `outer_decls()` (driver.rs:97, the RunLLMTurn-only
list) with an `answerer_decls()`. Drop `runLLMTurn`/`run`/base-effect mentions
from the narrow answerer framing (C1.8). (This is the first real slice of the
per-context-effect-set goal, §03.)

## Runaway guards (three cumulative caps)

- **Per-hole 16/32** — in `service_runllm_hole`'s drive loop, count model rounds
  (each `drive_turn` that does NOT finalize). At 16, `push_user_turn` a nudge
  ("approaching max tool calls; finalize now with `finalize @{ty} …`") and keep
  driving. At 32, return `DriverError` (hard-fail the runLLMTurn effect).
- **Per-loop 1024** — driver field `loop_inference_calls: u32`, reset in
  `run_loop_fragment`, incremented per `drive_turn`; if it hits 1024, hard-stop
  the loop with a `DriverError`.
- **Between-loops human gate** — in `run_loop` (driver.rs:374), before each new
  cycle after the first, print "press Enter to continue" and read a line from
  stdin, UNLESS a `--yes`/`--auto` flag is set (thread a `bool auto` into
  `run_loop`; the binary passes it from args). `run_one_cycle` (the acceptance
  path) has NO gate.

## Debt to fold in (same files)

- **H3** — rename `ask_tag` → `suspend_tag` (engine.rs:429,494-518 + all uses;
  it's the suspend threshold for Ask|RunLLMTurn|Finalize, not "Ask").
- **C5** — `take_finalized_value_with_table` (harness.rs:1155): the
  `unwrap_or_default()` on `suspend_table` → `ok_or(HarnessError…)` (loud, not
  silent misrender).
- **J1** — `state_in` (state_cross.rs:76): the `eitherDecode … error e` splice →
  return a typed `DriverError` on decode failure, not a JIT `error`.
- **J2** — `haskell_string_literal` (state_cross.rs:87): reuse
  `tidepool_mcp::eval_prep::input_binding_source`'s escaper instead of the
  hand-rolled one.
- **D3** — `SelfHarnessState::Closing` (lifecycle.rs): wire it (the human gate /
  Ctrl-C in `run_loop` transitions to it) or drop the variant.
- **D4** — `initial_state_json` (driver.rs:294): use `state_in(None)` directly
  instead of a redundant compile.
- **driver model-name** — align driver.rs:240's `llm_model` default with a real
  id (the binary side is a separate hardening dev).
- **H1** — `Harness = M` vs `HarnessEff = Eff '[RunLLMTurn]` (Tidepool.Harness.hs):
  add a load-bearing comment that they diverge once base effects are appended,
  or make one the source of truth.
