# Wave15 command baseline (native history)

Source run: `2518edd8-8a6c-439c-b823-5055b96096d9`, 26 September 2026. This is a count of **observed calls**, not a forecast of saved calls or tokens. The nine native Codex rollout histories whose metadata has `/tmp/exomonad-actor-workspace` as cwd were matched to this run's actor cgroups in `/home/inanna/.codex/shell_snapshots/`. Source files are `/home/inanna/.codex/sessions/2026/09/26/rollout-*-<thread-id>.jsonl`. I counted `response_item` `function_call` and `custom_tool_call` rows by unique `call_id`, matched their corresponding output rows, and checked for duplicate call IDs across histories. There were none. This avoids counting fork-inherited conversation text as another invocation. The run host log is `/home/inanna/dev/exomonad-harness-runs/wave15/.exomonad/logs/2518edd8-8a6c-439c-b823-5055b96096d9.jsonl`. Actor 5 has host activity but no matching native rollout; the figures below cover every native rollout with the actor workspace cwd found that day, not internal host commands.

| Actor / native thread suffix | Bash starts | Empty-input `write_stdin` | Haskell cells |
| --- | ---: | ---: | ---: |
| 1 / `01a0dd1d` | 69 | 28 | 73 |
| 2 / `01a0dd20` | 21 | 0 | 5 |
| 3 / `01a0dd21-00e9` | 78 | 2 | 22 |
| 4 / `01a0dd21-0876` | 58 | 7 | 12 |
| 6 / `01a0dd2a` | 9 | 1 | 3 |
| 7 / `01a0dd30` | 7 | 1 | 3 |
| 8 / `01a0dd34` | 5 | 0 | 3 |
| 9 / `01a0dd38` | 12 | 5 | 5 |
| 10 / `01a0dd3e` | 11 | 9 | 4 |
| **Total** | **270** | **53** | **130** |

Of the 270 Bash starts, **247 returned terminal**, **15 returned running in foreground**, and **8 were explicit `background: true` starts** that returned a completion-notice promise. These are mutually exclusive presentation categories. The 53 empty-input waits concerned 25 retained session IDs: **29 returned running**, **23 returned terminal**, and **one returned a `CommandUnavailable("unknown command job")` presentation error**. Their requested yields were 38 at 1 s, 14 at 30 s, and one at 10 s. All 53 omitted `chars` or supplied the empty string; no nonempty stdin write appeared. Seven of the eight explicit-background jobs also received one immediate `write_stdin` observation; the eighth had no native wait. Preserve explicit background as a distinct user choice.

For a strict repeated-output measure, I compared each running wait's presented body after the `terminal:` field with the preceding running wait on the same session ID. **12 of 29 running waits repeated that body byte-for-byte**. The repeats occurred in six sessions: `d1c47ce9` (2), `2a61439d` (2), `8c9bc429` (2), `0b6a368b` (2), `57d2e11d` (3), and `0c29a713` (1). This measure excludes unchanged output between a Bash start and its first wait, truncated bodies that differ because of `max_output_bytes`, terminal transitions, and the presentation error, so it is conservative.

Fourteen of the 15 foreground-running starts later had a terminal native observation **3.3–35.6 s after their start-call timestamps**; those 14 jobs received 37 empty waits. Their actual process completion times can precede the terminal observation, so the interval is an upper bound. A 60 s completion-oriented foreground budget is relevant to these exact cases, but the history does not prove how many model turns a new policy would save. The remaining foreground-running job (`8c9bc429`) had three running waits, then a presentation error on the next wait; it must not be treated as a confirmed terminal outcome.

Concrete trace points (UTC; native call IDs permit rechecking the source):

| Event | Timestamp | Call ID | Retained session / outcome |
| --- | --- | --- | --- |
| Root foreground start | 10:05:13.057 | `call_RdDG33iecxQKF175mveLj4S1` | `2a61439d-0090-4198-903a-5d796b24e8e2`, running |
| First unchanged wait | 10:05:17.767 | `call_iMSgvhBWXetPQjmqr4sa3CyT` | Running, no output |
| Second unchanged wait | 10:05:21.642 | `call_rCb2cFNAFB2QZFC6kci0kyiD` | Running, no output |
| Third unchanged wait | 10:05:26.083 | `call_jKok8aGi0NWT516gCdHqxRXH` | Running, no output |
| Partial-output wait | 10:05:30.145 | `call_By4GlvWpBkMKEzJTdUsav1e8` | Running |
| Terminal wait | 10:05:35.700 | `call_vbGKdh0Z9K9RgcG7Eejrlxej` | `CommandExited 0` |
| Explicit background start | 09:56:03.906 | `call_Vgq5nRp06bC01J0AbtVG4XWn` | `30d70c0d-a50e-4318-97f8-3bbc36613a28`, completion promised |
| Immediate background observation | 09:56:09.364 | `call_rJLCu5lRimz6t4sYuuwE32rZ` | Same session, terminal |
| Haskell focused-check start | 09:51:56.547 | `call_maM0YIkFWSHBBhlekH2omTQQ` | `TE.startFocused`, `FocusedRun` returned |
| Haskell focused-check watch | 09:52:11.410 | `call_RrI3nmwwNwyVcRw7k2wbB7vO` | `CR.watchChecks` requested |

Haskell composition was used for one focused-check workflow: `TE.startFocused`, `CR.watchChecks`, `CR.readChecks`, `TE.collectFocused`, and retained-output recovery. A first `Project.TestEvidence.startFocused` cell at 09:51:45.157 (`call_LQRdIo6UnI3Js9xw5yFUvYbt`) failed to compile because the module was not imported; the corrected call above started the job. I found **no direct `Tidepool.Command`/`Cmd.`/`Project.Shell` command-helper call** in the 130 native Haskell cell inputs. This is an adoption observation, not a reason to favor direct Bash over Haskell composition.

**Model rounds and tokens saved: unknown.** The native histories contain token-count events, but this audit did not establish a one-to-one attribution from those events to provider requests for this exact run and its inherited fork context. The command-call counts above are reproducible from call IDs; no token or turn saving is inferred from them.
