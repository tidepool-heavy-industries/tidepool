# Bounded model turns, run timeline and remote Buck handoff

Frozen source candidates, 2026-09-30. The local Tidepool main was fetched and
fast-forwarded to `d9d82f46ff5f702a16257c2dd9e42fe745e27265`; implementation is
isolated from that checkout and the active engine swarm. No application release
was activated, no provider calls were made, and no shared daemon was restarted.

## Candidates and application order

| Repository | Branch | Candidate | Base | Worktree |
| --- | --- | --- | --- | --- |
| Harness | `rsi/bounded-model-turns` | `3f7ec2d7432151e569174d00ceb7dd3dcb69f905` | `814b1697226344e8fd16196666e41c184a73531d` | `/home/inanna/dev/rsi-model-turns/harness` |
| Tidepool | `rsi/model-effect` | `74c16129ca4b1f060d053e8b7091d4b7bf0f6bf8` | `d9d82f46ff5f702a16257c2dd9e42fe745e27265` | `/home/inanna/dev/rsi-model-turns/model-effect` |
| Infrastructure | `rsi/remote-buck` | `cac522727aa56d144a0815c9965448fba67e616b` | `518a1fb` | `/home/inanna/dev/rsi-model-turns/infra` |

The harness exclusive range contains `61537d8b65055f61e2926142302d0fa10a175684`
and its reviewed refusal repair `3f7ec2d7432151e569174d00ceb7dd3dcb69f905`.

Apply the harness companion and select its source before compiling the Tidepool
ModelCall adapter. The Tidepool candidate deliberately leaves Cargo dependency
pins unchanged: the remote engine owner owns the final matched pin and native
Buck graph. Focused checks used the explicit path override shown below.

The Tidepool branch already contains the observability, remote-client and release
handoff commits; do not apply their original parcel branches as well. Its exclusive
commits in order are:

- `38eb4d5e1c8d0b36751f9c92e50af1c18f264bc3`: bounded timeline export.
- `f0afa2fdfb6127615d40b70e921425712c502e6d`: valid timing, fixed labels and tracks.
- `c6515e3754dcbec3c8c30b440399b6c9b93df4bd`: portable remote Buck client config.
- `1db92f139541413b61ce7430315a40ea0235d149`: actual remote test executor.
- `4a8633bbb17b3e9680e1f0723a55da178d9b92ae`: release ownership handoff.
- `e66eebaac155c1ab7c96a49ba2afd5e1e0910706`: repaired trace test provenance.
- `2d8eaf52dcd4b87da1cf2936050c976d4858d755`: nullable Haskell schemas.
- `74c16129ca4b1f060d053e8b7091d4b7bf0f6bf8`: ModelCall and compiled examples.

## Delivered behavior

`Tidepool.Model` builds reusable `textTurn` and `typedTurn` values around the
existing `AgentSpec`, then `invokeModel` runs one bounded turn. Only supplied
JSON object-input Call/Notify tools are admitted; unsupported kinds and schemas
are typed preflight failures. Callbacks run in the caller's effect row. The host
supplies model authority, cell identity, transport, scheduler and Store.

One lazy cell budget is shared by nested and concurrent invocations. Narrower
invocation limits and the cumulative cell allowance are both enforced. Exhaustion
cancels provider work and prevents new callbacks; a callback already handed to
Haskell finishes cooperatively and its actual result is retained. The execution
owner must call the concrete service's `cancel()` on cell cancellation/disposal.

The existing Engine remains the sole provider loop. Its bounded invocation
profile receives the complete provider round before running sequential callbacks,
retains each output, then runs that output's hook before the next callback. Full
transcripts stay in Store. Receipts contain request references and usage; missing
usage stays unknown. Pruning changes only the exact operation's provider view,
and its durable handle still resolves the original output after invocation removal.

Four compiled authoring seeds cover actual-diff change routing, owner dependency
clarification, evidenced handoff preparation and retained-output investigation.
See `bridge/haskell/examples/model-turns/`.

Run-map exports bounded Perfetto JSON and supports actor, execution, call and time
selection. Fixed event labels avoid exporting arbitrary log prose. Known close
intervals start before their completion timestamps; other durations remain event
arguments. Unknowns, omissions and read coverage are explicit; no causal edges
are inferred. Store usage reconciliation remains unknown until an owning read-only
export exists.

## Verification

| Check | Execution evidence |
| --- | --- |
| Harness `cargo test -p harness --lib invocation::tests` | 15 passed, 236 filtered; real Engine, scripted transport |
| Harness `cargo test -p harness --lib finalize::tests` | 12 passed, 238 filtered; exact integer bounds, nullable/tagged schemas |
| Tidepool `cargo test -p tidepool --no-default-features --lib model_turn::tests` | 7 passed, 236 filtered; authority, nested cap, durable pruning, host cancel, abandoned hooks |
| Tidepool `cargo test -p tidepool --no-default-features --lib run_map::` | 19 passed, 224 filtered; exported Perfetto JSON and retained-wave fixture |
| Tidepool `cargo test -p tidepool --no-default-features --bin exomonad` | 5 passed; actual CLI target compiled |
| `bash scripts/check-model-turns.sh target/model-haskell-production` | Native Haskell executable ran callback/caller-state/hook/result checks; four example procedures compiled against production-generated Core |
| Protocol integration target | 52 passed, 1 pre-existing fork-count failure; see below |
| Protocol standalone library tests | 12 passed using exact Rust 1.93.0 |
| Generation, formatting and whitespace | Generator `--check`, Rust formatting, harness `cargo fmt --check`, shell syntax and `git diff --check` passed |

Tidepool Cargo checks entered `scripts/dev-shell.sh`, set `CARGO_BUILD_JOBS=2`
and added this temporary argument without committing the lockfile alteration:

```text
--config 'patch."https://github.com/tidepool-heavy-industries/exomonad-harness.git".harness.path="/home/inanna/dev/rsi-model-turns/harness/crates/harness"'
```

Haskell used pinned GHC 9.12.2. The adapter export test produced Core with the
actual `tidepool_mcp::effects_core_module_source()`. Its native interpreted test
is not resident/JIT or live-provider acceptance. New native Buck target registration
and the default Codex feature build remain integration checks for the final graph.

The protocol failure is
`exomonad_control_contract::fork_options_are_optional_at_the_existing_launch_boundary`:
existing ForksStartWith has 17 fields while the assertion expects 15. A separately
compiled copy of exact base `d9d82f46` reproduces the identical assertion. No fork
contract or test was changed to hide it.

Logs remain in the Tidepool worktree's `target/model-*-check.log`,
`target/model-adapter-final.log`, `target/model-haskell-production/`,
`target/model-protocol-baseline/result.log`, and `/tmp/bounded-{invocation,finalize}-final.log`.
The final invocation check covers the completion-refusal repair; the finalize
schema check ran before that repair, which does not change the schema owner.

## Remote execution readiness

Actual Buck Rust compilation and all eight Rust test cases executed on the
isolated server worker. Haskell compilation, independent-client cache reuse and
changed-source invalidation passed. Full evidence, exact build IDs and commands
are in the infrastructure candidate's `docs/acceptance.md`; client instructions
are in Tidepool `docs/swarm-builds.md`.

Worker: `nativelink-tidepool-d9d82f46-r2`, PID 3080543 at qualification; one action,
12 GiB maximum, four CPU equivalents. Existing NativeLink PID 1154 stayed running.
The tunnel forwards local 50071. The retained worker bundle is
`/home/inanna/remote-buck-infra/worker-result` on swarm-01. This worker is transient;
boot persistence is a separate administrative activation. Qualification covers
these focused targets, not all extractor/browser/runtime actions or cancellation
and host-secret isolation gates.

## Remaining engine-owner and release work

`plans/model-call-integration-handoff.md` specifies the single per-admitted-cell
installation hook, cancellation signal, concrete row/attenuation additions and
joined resident acceptance. ModelCall is intentionally not advertised in the
always-loaded prompts before that service is available. Universal type vocabulary
does not install a handler or grant an effect.

`plans/release-preparation-handoff.md` identifies the current packaging, deploy
and run owners and their missing matched-release, durable run pin, GC retention
and state-compatibility interfaces. This wave delivered that handoff; it did not
implement or activate a second release selector. Existing release selection and
rollback still need those owner changes.

Independent review repaired unsupported tool reinterpretation, integer rounding,
missing abstention evidence, abandoned hooks, completion timestamp projection and
stream polling/identity hazards. Final exact-candidate review is recorded below.

Final independent read-only review approved Tidepool
`74c16129ca4b1f060d053e8b7091d4b7bf0f6bf8` with harness
`3f7ec2d7432151e569174d00ceb7dd3dcb69f905`. The reviewer identified an
undeclared text `finalize` acceptance that could panic a scheduler task and leave
the Engine waiting forever. The repair rejects it before scheduling, replaces
the missing-tool panic with a typed refusal and adds a two-second timeout
regression. All 15 invocation tests passed after repair. No blocking findings
remain; the reviewer did not rerun the tests.

## Transfer without publishing

Verified Git bundles for all three candidates are retained under
`/home/inanna/dev/rsi-model-turns/handoff/`. They include the respective base
commit as well as the branch, so restoration does not require the base to exist
in the receiving repository. The Tidepool bundle also contains this handoff.
No remote branch or running engine checkout was changed.
