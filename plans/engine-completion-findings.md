# Engine completion structural findings

This log distinguishes source findings from validated repairs. The user authorizes
repairs supporting completion; broader changes remain recorded for later work.
Current execution contracts and acceptance gates are in `engine-completion.md`.

| Finding | Impact and owning source | Disposition / proof required |
| --- | --- | --- |
| Cancellation can outlive the awaiting checkout future | `exomonad/actor/src/resident_workbench.rs`: blocking machine work may produce a parked continuation after its async caller is gone. Actor-wide parked-set subtraction cannot identify ownership safely under concurrency. | In progress: exact continuation owner travels through producing checkout, settlement and re-suspension. Require late-suspension, unrelated-hole, successor and normal-settlement regressions. |
| Cleanup failure can skip request/replay settlement | `exomonad/actor/src/resident_actor.rs`: fork-scope retirement error can return before exact unsubmitted-request rollback and replay recording. | In progress with runtime cleanup parcel. Preserve original operation outcome plus observable cleanup failure; do not lose accepted-operation evidence. |
| Readiness assumes a Codex thread | `bridge/facade/src/actor_host.rs` and run status consumers: embedded service has no genuine Codex thread binding. | In progress: typed embedded readiness with actual browser/service/conversation/actor conditions, exhaustive consumers and host-loss checks. |
| Postcompile evidence cannot authorize independent module reuse | Worker dependency evidence and `tidepool/toolchain/src/artifacts.rs` currently support full-invocation reuse. Raw entry-free sidecars do not prove precompile module identity. | Compiler owner implementing precompile inventory, exact provenance and interface hydration before enabling independent hits. Corpus acceptance covers sidecar encoding only. |
| Declaration wrapper typechecking misses duplicate imported class instances | Existing six-case `declaration-join-proof` accepts a wrapper whose later instance use fails. | Required M2 worker-owned combined instance validation, including instance-only imports. No name-only join or wrapper-only acceptance. |
| Product sidecar may silently omit a graph module | `bridge/haskell/app/Main.hs` `writeModuleProducts` logs/skips missing interfaces or rejected group projections. Product-to-node validation does not prove node-to-product coverage. | Required before independent reuse/demand: explicit typed availability or required-product coverage refusal. Distinguish valid interface-only/empty code from unavailable output. Current raw sidecar is not an admission proof. |
| Incremental worker artifact inconsistency | Compiler owner observed Cabal success with unchanged worker bytes, then an absent executable still reported up to date. Existing freshness check rejected it. | Root cause unconfirmed. Retain commands/hashes; fresh isolated worker build and new inventory behavior test establish the replacement. Do not rely on manual copying as the final build recipe. |
| Three output-reader threads per retained-view command | `exomonad/node/src/view_command.rs`: stdout, stderr and setup each have a dedicated reader. The helper removes the host fork but retains this per-command overhead. | Deferred: existing paired test shows helper benefit at 1 GiB host RSS. Measure thread/allocation cost under realistic concurrent command load before considering a shared I/O mechanism. Preserve byte streams and failure cleanup. |

The output-reader entry is an optimization opportunity, not a confirmed defect.
Record measured improvements separately from structural simplification.
