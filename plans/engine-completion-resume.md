# Restart handoff: engine completion, 2026-09-29

The user offered a restart to enable eight worker agents. All three workers
checkpointed and stopped. Work is NOT complete. Resume implementation, using
Sol for the state-machine/compiler owners and Luna for bounded leaves/reviews.
The old session tool enforced root plus three workers; inspect the new limit.
Do not create native goals. Pushes remain deferred. Live G5 launch still needs
its concrete readiness packet and separate user approval.

Read `engine-completion.md`, `engine-completion-findings.md`, and each owner
handoff below before editing. Root interactive checkout main is not the work
checkout; do not switch its branch or disturb unrelated worktrees.

## Checkpoints and immediate integration queue

All paths below start `/srv/swarm/checkouts/`.

| Checkout / branch | Checkpoint | State |
| --- | --- | --- |
| tidepool-completion / integration/engine-completion | dc87a0c32ff1505d2e411d6023c5a331202f7683 before this documentation commit | Joined compiler v4 provenance, exact continuation cleanup, execution record, initial M1 wiring, native image-key repair. Known permanent-root readiness bug fixed only on harness branch below. |
| tidepool-completion-compiler / completion/compiler | 876e28885312bbd18860ab7c87e9e6c7edb6fb52 | Clean; repr API and request v13 transport ready, no actual durable reuse yet. |
| tidepool-completion-runtime / completion/runtime | 15643f3a839432c628f4f225d6cde608437b36ac | Clean; verified capture repairs followed by explicitly unverified publication WIP. |
| tidepool-completion-native / completion/native-demand | 73d30dc263b85bbc2b66bb827c34bbb0788cb0f7 | Clean; entry-free compilation/demand/SCC batch WIP, production runtime wiring unfinished. |
| tidepool-completion-harness / completion/harness | 65ea5604634cee8cfca6bedabb610f5e0f385deb | Clean; verified browser rotation/settled compaction tests, then compiled readiness fix requiring actual host-loop test. |
| tidepool-completion-git / completion/git-acceptance | 6b5c8f002 | Clean; all changes already integrated. |

Owner handoffs (read from their respective worktrees, not stale root copies):
- Compiler: `plans/engine-completion-compiler-resume.md`.
- Runtime: `plans/engine-completion-runtime-resume.md`.
- Native: `plans/engine-completion-native-resume.md`.

Root has already integrated compiler qualifier commits 691eba05d + 9fc7b2d72
as a93d576a5 + e472a3b7e, and runtime fe32541ee as c1cdb71b3.
Queued verified runtime repairs: a6ab1dbb3 (real Haskell capture tests),
9a330b222 (retain release cleanup), 9350b1939 (idempotent exact acknowledgment).
Review these together. Runtime 15643f3a8 includes uncompiled publication WIP;
do not treat its branch tip as accepted M2.

Compiler repr API 8a65dcddd is also cherry-picked on native as 7b700ef45;
apply only one equivalent copy. Compiler d307cdb18 adds request v13 and bounded
ModuleCandidates transport, but has no production hydration consumer yet.
Read the compiler note before integrating the protocol change.

Native immutable cache-key repair 9f8e5615f is already joined as f5afe6e1b.
It keys existing images by PreparedProgram, removing mutable linked import
snapshot generation/evaluatedness. Linking and installation still validate
actual imports. Ten registry tests and two runtime reuse/install tests passed;
logs native `target/completion-evidence/native-key-r2.log` and
`native-key-consumer-r2.log`. The first runtime selection ran zero tests because
of nextest's default filter; only the r2 `--ignore-default-filter` run counts.
The native owner now uses its own target directory; root used the integration
target for that bounded key repair only and has finished doing so.

## Critical M1 readiness correction

Joined dc87a0c32 waits for first SessionReady before starting the embedded Engine.
That is WRONG for the permanent root. ExomonadDriver.rootDriver is
`attachAgent Nothing` followed by `serve`; it never emits a typed SessionReady
without a typed request. The actor owner confirmed PolicyInstalled is emitted
after actor readiness makes its reference usable and installs the immutable
policy/source. SessionReady is subsequent typed activation input, not generic
application readiness.

Harness 65ea56046 repairs this: start direct fleet-owned Engine and browser
readiness on PolicyInstalled, admit its optional initial input, retain
SessionReady for typed inputs, remove the extra pending-launch registry. This
removes 68 lines while adding 28. Facade library compilation passed via the
pinned shell (log `/tmp/tidepool-m1-readiness-checkpoint.log`). Actual host-loop
acceptance is STILL REQUIRED; do not merely rerun the lower service test.
No live embedded run was launched, and Codex remains default.

Suggested exact host test: use TestCampaign::start_with_config to retain its
ActorHostConfig, then drive actual run_interactive_applications with its real
PolicyInstalled installation and remaining deployment receiver. Permanent root
must become EmbeddedReady without a SessionReady or synthetic typed request.
TestCampaign::take_deployments can transfer its receiver; prefix the consumed
root PolicyInstalled through a test-owned forwarding channel if needed. Use
real InteractiveFleet, local service/settings, and invalid empty credential
fixture `{}` so a deliberately submitted browser turn fails locally before any
provider network call. Verify task failure/retirement and joined host shutdown,
not only snapshot projection. No test was written before this restart.

Harness queued commits, in order after already integrated 2ec1c6f50:
- 40933facc adds cookie flags and restart/secret rotation assertions. Its first
  run failed BEFORE those assertions: 5-second Haskell-settlement guard expired.
- 39d17cc2b waits up to 30 seconds for real compilation/settlement and surfaces
  early Engine failure. Combined test then passed 1/1, 553 skipped, 56.168s;
  `/tmp/tidepool-m1-rotation-r2.log`. Do not count the first run as passing.
- 8a6ea2faa forces actual configured plain-text compaction after settled raw
  Haskell output. Offline transport verifies compactor receives actual `42`,
  returns a summary, and subsequent requests retain it with one browser input.
  Same real service/Engine/resident/browser test passed 1/1, 553 skipped,
  54.842s; `/tmp/tidepool-m1-compaction.log`.
- 65ea56046 readiness repair described above, compiled only.

These tests cover settled-output compaction and reconnects, NOT compaction of
an unfinished embedded actor call, nor actual host-loop readiness. Secret
rotation is verified on server restart; not hot reload. Cookie Max-Age=28800,
Secure, HttpOnly and SameSite=Strict checked; actual expiry remains covered only
by pinned harness component tests and should be included in final gates.

## Active matched build: inspect before launching another

Root source at dc87a0c32 is held unchanged while a matched build runs. Do not
change production sources in the integration checkout until this command has
finished or has been deliberately stopped; other worktrees remain available.
Unit: `tidepool-matched-source-0929.service` in the user manager.
Log: integration `target/completion-evidence/matched-source.log`.
It was still materializing Codex Nix dependencies at handoff, NOT a passed build.
The tool session ID was 3367, if preserved by resume. Collected systemd units
can disappear, so retain final exit evidence/log rather than assume success.

Ordinary `just exomonad-build` remains blocked by Nix Git source capture of
Codex 2d58f00c6f139d745e0c123d31dfe6d2f04ff997, not advertised at remote main.
A Git direct-object fetch succeeded, but Nix still refused the main ref; do not
claim remote publication is fixed. Explicit submodules=0 was overridden by the
flake self attribute. No credentials/push change was made.

Working offline path: git-archive exact root dc87a0c32, exact Codex 2d58f00c6,
and exact workspace submodule 5248b927e7b432d1747891df5285d6827eace7d6 into a
source-only temporary directory; add it to Nix store. No caches, build outputs,
or credentials were copied. Immutable source:
`/nix/store/szmnsagrn7iyb448p8xygsrpjc3y5pk1-tidepool-completion-source.lsTYYL`.
Original source-only directory is recorded in
`target/completion-evidence/matched-source-directory.txt`.
Command, inside admitted service and integration cwd:

```
CARGO_BUILD_JOBS=8 nix develop --cores 8 --max-jobs 1 --no-write-lock-file   /nix/store/szmnsagrn7iyb448p8xygsrpjc3y5pk1-tidepool-completion-source.lsTYYL#exomonad   --command bash exomonad/scripts/exomonad-build.sh
```

This realizes the declared shell from exact committed source; the matched build
script compiles local integration sources. It is not a clean remote package
proof. Nix daemon work is separate from the build slice; expensive realizations
were serialized. Other workers were told not to start another realization.

## Resumption rules and decomposition

Heavy commands remain admitted through `systemd-run --user --quiet --wait
--pipe --collect --unit=UNIQUE --slice=tidepool-completion-build.slice
--working-directory=CHECKOUT /run/current-system/sw/bin/bash -lc 'COMMAND'`.
Slice limits: high88GiB/max104GiB/swap2GiB; last peak about14GiB. Check current
cgroups and units after restart. Do not restart Nix/compiler/shared services.
The session's own shell was outside the build slice. /srv/build and
/srv/swarm/state top levels were not writable; do not infer permissions from
the old host guide. Assigned checkout targets are writable.

Resume the three meaningful Sol owners above. With additional slots, separate
compiler declaration-join validation from module hydration through an agreed
new owning module/API (avoid simultaneous protocol/GhcPipeline edits). Use Luna
for M1 host acceptance, exact-commit reviews, companion gates and evidence
inventory. Root integrates/reviews. Freeze shared API before splitting leaves.
Do not pursue a headcount target or run broad batteries per worker.

Final gates remain full sequential M1, actual module reuse/hydration, exact
reachable native demand with atomic SCC install, concurrent private execution
and compiler-validated paired publication, independent capture lifecycle,
worker-tree acceptance, full joined verification, matched package, and deferred
delivery/live approval. Structural fixes must have production consumers.
