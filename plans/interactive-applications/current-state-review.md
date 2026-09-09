# Applications plan review after resource isolation

Review date: 2026-09-09. This is a source-based continuation map, not acceptance
of retained candidates. No product branch was merged or rebased during this review.
The A0–A8 contracts remain the desired product outcome.

## Source and evidence boundaries

| Source | What it supplies | Evidence limit |
|---|---|---|
| Main runtime `2b27c3f2`, native `fe15831c` | Exact pane supervisor, bounded command trees, admission and retained cleanup, packaged normal TUI | Resource-slice packaged OOM/steering/follow-up test passed; full applications matrix not established |
| Applications `978029124` | Earlier admission/delivery foundations plus later producer-seal custody work (`0caad300d`, `7dec4d98a`, `978029124`) | Older lane handoff predates these later changes; inspect source, not just its status prose |
| Recovered applications `3bed6d67` | Installs deferred input-seal custody on binding; consolidates pre-launch failure cleanup | Preserved dirty bytes, not independently accepted implementation |
| Native bridge `72f7b600` | In-process native input-control path and admission foundation | Historical focused checks; complete socket/PTY failure-path acceptance missing |
| Native `3fc507260a` plus recovered `d73bdc83` | Durable input terminal retention, watermark compaction, withdrawal tombstones, recovered `Compacted` propagation | Newer than the older handoff's native pin; recovered changes need matched validation |
| A7 candidate `78a77c3c` | Source-only recovery incarnation/order validation, declaration/retraction reporting and explicit lost live state | Not an ancestor of `978029124`; separate integration and engine join remain |

Full hashes, recovery refs, original dirty-file archives and continuation rules are
in [resume guidance](../parallel-dogfood/next-wave/resume.md). Main remains the
running-tool baseline. Preserve recovered originals and make new continuation
branches reconciled onto that baseline. The engine/combined checkpoint is a source
asset, not a substitute for the applications/native pair.

## Concrete integration findings

1. **Recovered wire mismatch:** native `d73bdc83` adds `Outcome::Compacted` and
   propagates compacted admission. Tidepool `3bed6d67` still has no `Compacted`
   variant in `backend/codex/input_control.rs::OutcomeWire`; its serde decoder
   cannot accept that response. Define the matched outcome and its host meaning,
   update both consumers, and execute a compaction/query/admission round trip.
   Do not map it to successful presentation or permit uncertain redispatch.
2. **A5 has more implementation than the older handoff reports.** The retained
   `HostedRetirement` owns a deferred producer-seal operation and separates input
   seal, hosted seal, resident cleanup and HTTP drain. Recovered binding code
   installs that operation before delivery starts; it does not eagerly execute
   the seal. Reconcile this with main's supervisor/resource owner rather than
   replacing that owner or restarting A5 from scratch. Review whether every
   pre-/post-producer failure path proves the claimed absence or retains custody.
3. **Input completion is not hosted-call completion.** Native `3fc507260a`
   changes queue outcome retention/compaction. It does not prove A6's native
   hosted-Haskell completion ownership, persisted result/fork boundary or
   coordination-failure behavior. Keep those obligations distinct.
4. **A7 is a real separate candidate.** Inspect and integrate its source-only
   recovery changes with the engine's actual machine-disposition contract. Its
   existence does not establish reconnect, live-helper adoption, full retirement,
   or restoration of runtime values/authority.
5. **The old handoff includes obsolete operating observations.** Its live actor
   custody statements and compatibility-window question belong to that historical
   run. Current resume guidance records the OOM and recovery; there are no external
   TPLR consumers and no native aarch64 runner. Do not replay those questions or
   treat old active actors as current executing owners.

## Recommended plan iteration

Keep mechanism documents 01–05 as contracts. Use three explicit statuses in the
execution map: accepted on main, retained candidate, and remaining acceptance.
Unchecked boxes currently conflate absent code with implemented but unaccepted
work. Do not check them merely because a matching symbol or commit exists.

Reorganize the next implementation allocation around these concrete joins:

1. **Reconcile sources and protocol:** main plus recovered applications/native
   candidates, preserving new command admission and supervisor paths. Resolve
   the `Compacted` mismatch and inventory other changed wire/storage contracts.
2. **Finish admission/delivery acceptance (A1–A3):** complete the existing
   submit/query/withdraw/seal/ack socket path, Remote unavailable behavior,
   lost-ack/stale-generation real-TUI checks, late reconciliation and no overtaking.
   Reuse existing canonical envelope/inbox/native-store machinery.
3. **Finish retirement (A5):** integrate retained producer custody with exact
   process exit, hosted-work settlement and resource release. Exercise interrupted
   launch, lost waiters and failed cleanup at the joined production boundary.
4. **Finish hosted completion (A6):** audit current native completion owner and
   implement only missing native-session retention/context-fork behavior. Join
   its evidence into A5 rather than inferring completion from terminal rendering.
5. **Integrate recovery (A7):** incorporate the retained source-recovery candidate
   against the checked engine contract; complete reconnect/operator paths and
   truthful unrecoverable-state reporting.
6. **Matched product release (A8):** run the complete applications matrix on the
   reconciled pair, remove superseded consumers and select a reviewed product pin.
   The resource release's package is useful baseline evidence, not this acceptance.

A4's accepted mechanisms are foundations to preserve. Its remaining terminal and
owner-loss matrix can run alongside the joins above. A0 becomes baseline and
fixture reconciliation, not rebuilding the initial scaffold. Keep explicit source,
check and acceptance evidence with each join; no new orchestration terminology is
needed.

The [Haskell command workbench](../next/haskell-command-workbench.md) is a separate
future capability. Its command values and tool migration should consume these
process/resource owners, but must not expand A0–A8 or delay closing its existing
native-input/completion/recovery obligations.

## Review limits

This pass inspected retained handoffs, Git ancestry and the consequential source
changes named above. It did not execute recovered product candidates or perform a
line-by-line acceptance review of every A0–A8 implementation. The full native suite
running on `fe15831c` validates the main resource pin, not `d73bdc83` or the complete
applications candidate. Recovered work must receive its own matched checks.
