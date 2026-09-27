# Wave 18 Jev cost audit (read only)

Source: `wave18/.exomonad/logs/29e61b63-2bc9-45d5-b5d1-b67e20918bea.log`, parsed through line 115192 (`2026-09-27T01:25:49.095517Z`). The run was still writing; all figures are a point-in-time prefix. Reproduce with `python3 docs/reports/wave18-audit/jev-cost.py <run-log> --lines 115192 --output /tmp/reproduced.json`; retained aggregate is `jev-cost.json`. The parser retains only usage, timing, actor/tool identifiers, and disposition classes, never prompt or response bodies.

## Cost and outcomes

- **1,393 successful HTTP 200 Jev requests**: 15,033,124 input and 199,014 output tokens. Per request input p50 9,538, p90 22,132, p99 31,554, max 32,974; output p50/p90 104. Summed handler latency 216,204 ms (concurrent calls mean this is not run wall time).
- **After-tool watchdog**: 1,257 successes, 13,731,313 input (91.3% of measured input) and 130,728 output tokens; p50 input 9,190, p90 22,828. By tool: Haskell 737 calls/7,826,844 input; bash 386/4,303,871; lookup 120/1,475,033; read_output 9/53,939; write_stdin 5/71,626. **Explicit or other actor-authored** requests: 136 successes, 1,301,811 input, 68,286 output. These include higher-output semantic tasks and should be reviewed separately from the watchdog.
- **106 recorded Jev failures**: 101 HTTP 400 `max_tokens_exceeded` (33 bash, 55 Haskell, 13 lookup after-tool calls), five timeouts. Of all failures, 105 are after-tool and one explicit. Failed requests lack usage accounting, so token totals are lower bounds on work submitted and cannot establish billed tokens.
- **1,583 after-tool invocations**: 1,576 abstained, seven annotated, zero pruned. Abstention reasons: 1,250 `no heuristic crossed its floor` (all 1,250 had a successful Jev call; 13,608,989 input tokens); 144 cheap trivial-bash gate; 77 message-send gate; 105 transport failure. Seven annotations account for 122,324 input tokens. An abstention is a valid conservative result, not automatically waste; the very low intervention rate does justify measuring precision on representative calls before retaining an always-on battery.

## Structural source of repeated input

The active root `wave18/.exomonad/AgentSpec.hs` installs `Watchdog.watchBy (const Watchdog.coreHeuristics)` as `afterTool`; the workspace child spec installs the same core heuristics plus path-based additions. The implementation is `wave18/.exomonad/workspace/Project/Watchdog.hs`, especially `watchBy`, `recentToolActivity`, and `boundedEvidence`. `watchBy` calls Jev after every result outside its two deterministic gates. It sends the current tool arguments, up to 8,000 characters of current output, and **all matching call arguments plus up to 8,000 characters of each result from a two-turn `reflect` request (the owning Rust conversation reader includes the active turn)**. History is bounded in turns but not in call count or argument size. Those prior calls are resubmitted on successive requests. The same heuristic question is included in both `supported` and `trigger` prompts for each heuristic. Core asks about repetition and destructive commands even for observational tools; the latter question cannot trip on a lookup or read_output call. The request builder is workspace Haskell; `exomonad/actor/src/resident_workbench.rs` dispatches the slot and `bridge/handlers/src/handlers/jev.rs` performs/logs the HTTP call.

The growth pattern is visible without private payloads: root actor 1's first six after-tool input sizes were 2,705, 4,384, 6,786, 8,514, 10,949, and 11,412 tokens; actor 9's first ten climbed from 1,791 to 12,744. Later requests approach 33,000 tokens and 101 requests hit the provider's token ceiling. This is consistent with repeated recent-call context plus current evidence. The log does not contain request hashes or safe size breakdowns, so exact duplicate token share cannot be measured from it.

## Changes worth trying, in order

1. **Bound the watchdog packet by tokens or a conservative byte/character budget before Jev.** Cap the *whole* serialized state, especially historical call count and arguments, not only each result. Preserve the most recent failed result and a small set of likely related calls for `repeatingItself`; mark omitted history explicitly so Jev can abstain. This targets both the 101 hard failures and the 22,828-token p90 after-tool input. Measure whether the seven annotations still occur.
2. **Route by tool and observable outcome before Jev.** `lookup` and `read_output` consumed 1.53 million measured input tokens over 129 successful after-tool calls. Their lookup/read result itself cannot execute a destructive command, so a narrow gate can skip that question. `write_stdin` consumed 71,626 input tokens in five calls and must be assessed separately because sent input can execute work. For Haskell, identify provably read-only cells or simple message sends structurally; keep ambiguous cells on the semantic path. Current trivial-bash and message-send gates saved calls, showing the mechanism works.
3. **Ask only questions with available evidence.** The `Watchdog.hs` comments say completed turns only, but the owning Rust `read_conversation` includes the active turn while excluding aborted items and fabricated pending output; its current-turn calls can therefore grow the packet. The packet repeats whole prior-call records. Supply a compact typed digest of relevant prior failures instead of arguments and outputs for every prior call. Include the destructive question only for command-capable calls. Remove the duplicate heuristic-question text from the two subquestions if Jev's operator contract permits shared framing.
4. **Instrument the next run with privacy-safe packet metrics and outcomes:** serialized request bytes, current-result bytes, historical-call count/bytes, heuristic set, gate reason, and Jev error class. Keep sampled successful trigger probabilities or threshold bins to tell whether the 1,250 `no_floor` decisions were close calls. This makes the next cull evidence based without logging private payloads.

No source edits, builds, tests, or live-run changes were made. The report does not estimate dollars: price and failed-request billing are not in this log.

## Annotation replay and the 90% token target

I matched all seven `Annotated` spans to their input units in the structured host trace, then to the exact Codex rollout tool calls and immediate outcomes. The private local replay fixture is `/tmp/wave18-jev-annotation-episodes.private.json` (mode 0600); it contains source and output, so keep it out of the repository. The table gives only bounded summaries. All seven batteries returned the same advice: “You have tried this before without success. Read the earlier failure before trying again.”

| UTC time | Actor / slot | Input tokens | Current unit and result | Assessment |
|---|---:|---:|---|---|
| 23:52:02 | 11 / 9 | 18,367 | Import in corrected Haskell batch; committed | False trigger on a non-action unit |
| 23:52:04 | 11 / 11 | 18,610 | Task value declaration; committed | Stale advice after earlier parse/type failures |
| 23:52:05 | 11 / 12 | 18,701 | Second task value declaration; committed | Stale advice after earlier parse/type failures |
| 23:52:30 | 11 / 13 | 18,504 | Corrected `unfold` call; committed and started children | Redundant: the earlier failed call had been revised successfully |
| 23:52:44 | 11 / 14 | 19,809 | Message text binding; committed | False trigger on a non-action unit |
| 00:45:38 | 10 / 107 | 15,645 | Corrected parent-message cell after a type error; committed | Redundant after the type fix |
| 01:15:46 | 9 / 329 | 12,688 | Text binding after failed answer construction; committed | False trigger on a non-action unit; later answer submitted |

The seven do **not** provide evidence of a useful intervention in this trace. They cluster around earlier Haskell compile failures and are applied to the successful repair, often to individual declarations. This does not prove the watchdog has no value in other runs; it gives a concrete replay set for checking a narrower gate or revised heuristic.

The explicit Jev cost is almost entirely `Project.Lookup.select`: **133 of 136 calls and 1,297,041 of 1,301,811 explicit input tokens**. `Tidepool.Lookup.Tools.execute` calls the selector whenever the first lookup has nonempty one-degree candidates and no issue; the workspace selector then sends up to 16,384 characters of recent conversation, up to 8,192 characters of primary lookup results, and candidate metadata/rubrics. The p50 explicit request is 9,878 input tokens. Of these 133 lookup executions, **120 also ran the after-tool watchdog** on the same tool result: two Jev calls for one lookup, with 1,475,033 additional measured watchdog input tokens. `Tidepool.Lookup.hs` describes the underlying Lookup effect as read-only; `read_output` pages retained output without rerunning a command. `write_stdin` can submit executing input and is not covered by that read-only reasoning.

A 90% cut from this prefix means **at most 1,503,312 measured input tokens**, down from 15,033,124. Keeping explicit Jev unchanged leaves only 201,501 tokens for after-tool, a 98.5% reduction of its 13.73 million. A less brittle path is to reduce both: for example, a 95% cut to after-tool and a 50% cut to explicit would total about 1.34 million, or 91.1% below baseline. These are arithmetic scenarios, not measured savings or a reason to remove quality judgments blindly. The best first replay targets are (a) the seven false/redundant annotation episodes, (b) the 120 double-Jev lookup executions, and (c) calls with `max_tokens_exceeded`. For lookup, test a cheap decision using query outcome, candidate count, and whether the primary answer already resolves the question; retain semantic ranking for ambiguous or unresolved cases. Compare candidate quality before suppressing its call.

## Authorized changes

The default after-tool watchdog is being removed from the shared workspace and
harness AgentSpecs. Opt-in monitors remain available with bounded, explicit
evidence; task-specific semantic routers remain available. Lookup and Sift will
share a small recent user/assistant message context, excluding tool calls/results,
with independent message and total-text limits. Actual lookup evidence remains
separate.

The user target is at least 90% lower total Jev input use. Removing the default
watchdog eliminates a call class responsible for 91.3% of this measured prefix,
not a measured future-run saving. A new wave's task mix and explicit use can
change the result. Compare post-installation usage and useful interventions;
source edits and message delivery do not prove a live actor installed them.

Per-turn or task-event supervision is a subsequent bounded experiment. It should
ask a concrete useful question over selected evidence, rather than run the same
generic checklist less frequently. The running Astra owns adoption timing in
wave18 and retains its planning conversation with Inanna.
