# extract-wave operational block

**SUPERSEDED (2026-08-09): do NOT paste this file's Block into dev specs.**
The original instruction here was to copy it verbatim into every spec. That is
now the wrong practice, and this session is the proof.

Make it a mandatory **STEP 0** in every dev spec instead — and read it **BY
REF**, never from the worktree copy:

```
git show root.extract-wave:plans/post-restart/extract-wave/OPERATIONAL.md
```

**CORRECTED 2026-08-09 (boot): `cat`-ing the worktree copy has the SAME defect
it fixes.** A dev's copy is frozen at FORK time, not spawn time — which is
earlier. Boot measured it: `diff <(git show root.extract-wave:…) plans/…` DIFFERED
even in boot's own tree, one hop closer than any dev. So a dev following a
`cat`-based step 0 literally would read *a stale copy of the fix for staleness*,
invisible in exactly the same way.

`git show <ref>:<path>` reads the branch tip by ref — read-only, no checkout, no
worktree contact, canonical regardless of when the dev forked or last merged.
**This is the `ghc-slots.sh` lesson in a different artifact**: `$PWD/scripts/
ghc-slots.sh` silently gave 3 slots when the real broker had 6; a local
`OPERATIONAL.md` silently gives yesterday's rules. `git show <ref>:<path>` is
the absolute-parent-path equivalent.
Secondary benefit: it puts the text in the dev's own transcript, so "did you
read it" is answerable from the record rather than taken on trust.

Why the change (spawn-latency proposed it; the decisive argument is empirical):
a pasted Block **freezes at spawn time**, and this Block was amended roughly ten
times in one session — throttle v1 → v2, `detach`, the denominator rule, the
holder rule, the named-guard rule. A dev spawned in the morning with a pasted
copy spent the afternoon following **superseded rules**, and that is not
hypothetical: it is exactly what happened with "tier 1 is safe unattended" and
with "wrap every invocation including the quick tier". A reference resolves at
read time; a paste is a snapshot nobody re-takes.
It is also N copies that drift, which is the derive-don't-restate antipattern
this very file bans two sections down.

**A TL's own messages are frozen at send time too.** Tell every dev: *if the
canonical Block contradicts something I told you directly, the Block wins — and
tell me, so I reconcile.* Without that, consolidating onto one source just adds
a competing authority instead of replacing them (boot). The failure mode the paste guarded
against — a dev skipping a linked file — is covered by making the read STEP 0
with an explicit read-in-full instruction, not a passing citation.

## Namespace allocation (extract-wave, allocated by the wave TL up front)

Per the realm-spike conflict experiment, files are **deliberately NOT
pre-partitioned** between sub-TLs. Only shared *artifact* namespaces are
allocated, so plan/ledger writes never collide:

| Artifact | Owner |
|---|---|
| `plans/post-restart/extract-wave.md` | wave TL (extract-wave) — sub-TLs DO NOT edit |
| `plans/post-restart/extract-wave/OPERATIONAL.md` | wave TL |
| `plans/post-restart/extract-wave/boot/**` | sub-TL `boot` (plans + `LEDGER.md`) |
| `plans/post-restart/extract-wave/spawn-latency/**` | sub-TL `spawn-latency` (plans + `LEDGER.md`) |
| `plans/README.md` | wave TL only, at final fold |
| dev-level plan docs | under your own sub-TL directory, numbered `NN-<slug>.md` within it |

**Expected real code overlap** (do not negotiate it away; write minimal,
localized diffs and log it at fold):

- `haskell/app/Main.hs` — `boot` item 0 step 4 (render+loop from ONE extract
  invocation) touches turn-mode emission; `spawn-latency` D1 touches
  `writeWholeModuleClosed`'s metadata merge (~line 348). Same file, different
  regions, both live.
- `tidepool-runtime/src/session/compile.rs` / `turn.rs` — `boot`'s multi-target
  work vs `spawn-latency`'s timing brackets.
- The realm-build lane (a PARALLEL wave, not ours) also lives in
  `tidepool-runtime/src/session/resident.rs`. Its step 4 is HELD on our
  boot-site landing. Conflicts resolve at fold.

## Block

```
### OPERATIONAL RULES (verbatim, non-negotiable)

**THE ONE RULE, from which most of the rest follow (boot, 2026-08-09):**

> **Verify the thing your claim is about, not a thing adjacent to it.**
> In the moment, ask: *what exactly is my evidence about, and is that the same
> object as my claim?*

Every verification failure in this wave — two TLs' and four devs' — was evidence
about a NEIGHBOUR of the claim:

| the claim was about | the evidence was about |
|---|---|
| a `TypeError` constraint | a token inside an error string |
| whether a commit was relevant | whether it was recent |
| an environment | a session (`setsid`) |
| queue pressure | waiters, not the holders |
| whether a leg ran | its exit code, not its log |
| whether a suite ran | a count, with no denominator |
| what the code does | a comment above it |
| the canonical rules | a worktree copy frozen at fork |
| what a tool is FOR | what it was observed doing |

**A distinct sub-case worth naming** (spawn-latency), because it is not an
instrument returning a plausible wrong answer — it is consulting the wrong KIND
of source: **when a claim is about PURPOSE, the source of truth is the
documented contract, not observed behaviour.** Behaviour tells you what
something does; only the contract tells you what it is for, and *"it does not
currently do X"* is not evidence that X is out of scope. Worked example: the
claim "the broker exists to cap extract spawns, so a `cabal build` is out of
scope" was refuted by `ghc-slots.sh:2`, which names "extract builds" as its
FIRST in-scope category — one `sed` from being checked, never consulted.
Corollary for arguing scope changes: argue the reclassification on its merits,
never on a claim about original intent you have not read.

"Verify more" is unactionable. This is checkable in the moment, and it subsumes
the named-guard rule, the denominator rule, the holder rule, and the instrument
rules below — each is this rule applied to one artifact.


- Commit with `git commit --no-verify`. NEVER `git add -A` — stage explicit
  paths only.
- The repo-root `tmp/` directory is PROTECTED: never delete or overwrite
  anything under it.
- NEVER run a path-unscoped `pkill -f`. Scope every kill to your own worktree
  path.
- Do NOT run `scripts/redeploy.sh`. Root owns the redeploy at dogfood resume.
  A wire break is already in effect (`--emit-stmt-binders`/`--emit-binders` are
  gone; the deployed pair on this box is the old consistent pair and dogfood is
  PAUSED). Your test runs build the repo extract fresh, so your lanes are
  unaffected.
- **THROTTLE DIRECTIVE (root, 2026-08-08 — IN FORCE until root lifts it).**
  The box hit load average **92**: the operator's SSH sessions died and a dev
  pane died in the same window. Seven concurrent `tidepool-extract` compiles
  were observed — nextest's `ghc-heavy` cap is **per-run, not box-wide**, so
  parallel worktrees multiply it.

  **THROTTLE V2 SUPERSEDES V1 (root, 2026-08-08). READ THIS, NOT THE HISTORY
  ABOVE.** V1 routed 2-minute quick tiers and clippy through the same 3-slot
  FIFO as 30-minute shards, which manufactured a 44-deep queue and a priority
  inversion. V2 splits heavy work in two by KIND:

  **(a) Pure-Rust heavy work EXITS the slot queue.** Do NOT broker-wrap
  `cargo check/build --workspace`, quick-tier `cargo nextest run`, or
  `cargo clippy`. Instead bound and deprioritise it:

      export CARGO_BUILD_JOBS=4
      nice -n 15 cargo <cmd>            # nextest additionally: -j 4

  Rationale worth keeping: a queue cannot govern what it cannot see, but the
  scheduler can. Unbrokered `rustc` was measured at 564% across 4 procs — the
  box's largest consumer — so it is handled by deprioritisation, not queueing.

  **(a2) TOOLCHAIN BUILDS ARE ALSO OUT OF THE QUEUE** (root, `efd1630a`,
  2026-08-09). `cabal build tidepool-extract-bin` and kin run **unbrokered**
  under the same envelope pure-Rust got:

      nice -n 15 cabal build -j4        # at most ONE per lane

  Why: a build is one GHC chain with cappable internal parallelism and a
  footprint knowable in advance — bounded and deprioritizable, unlike a test
  fan-out. Leaving it in the queue reproduced the exact short-behind-long
  inversion V2 was created to remove: a 2–5 minute build stuck behind an
  88-minute 877-test suite, both holding one undifferentiated slot.
  Note this is a **reclassification**, not a correction: `ghc-slots.sh:2` names
  "extract builds" as an in-scope category, and it was.

  **SHARD YOUR ACQUISITIONS.** A long suite run must scope with `-E` (per
  binary or group) so no single hold runs toward an hour where the receipts
  allow it. The measured argument: of six holders sampled, the 18-minute one was
  `battery-shard … -E 'binary(…)'` and the 89-minute one was a bare
  `--ignore-default-filter -p <crate>`. Six slots held for an hour behave like
  zero, and raising the slot count does not fix hold time.

  **(b) EXTRACT-FANNING work STAYS slot-brokered**, absolute path, NEVER
  exclusive:

      /home/inanna/dev/tidepool/scripts/ghc-slots.sh run -- <cmd>

  **THE ABSOLUTE PATH IS LOAD-BEARING, and here is the mechanism (root,
  2026-08-08).** Slots are now **6** (`4958ada6`), but **only PARENT-path
  invocations see slot4 and slot5** — a stale worktree copy of the script has
  the old `SLOTS` array and never even TRIES them. That stacks with the old
  sleep-poll code, so a worktree-copy invocation is **doubly dead**: it competes
  for a third of the available slots using a wait loop that burns CPU and dies
  at ~380s. Never invoke `scripts/ghc-slots.sh` by a relative or worktree-local
  path. Never copy it.

  That means anything spawning `tidepool-extract`, any `cabal test`, and any
  `--ignore-default-filter` run. With the small jobs gone, the queue belongs to
  these.

  `scripts/battery.sh` / `scripts/battery-shard.sh` self-acquire — do NOT wrap
  them. Unchanged across both versions.
  EXEMPT entirely: single-crate `cargo check -p <X>`, file edits, greps.

  **The spin is fixed at the mechanism level** (root's `2d72434e`, live at the
  absolute path above — verified): waiters kernel-block with jittered `flock -w`
  rotation instead of sleep-polling, so a blocked waiter costs ~zero CPU. No
  action needed; new invocations get it automatically. (The "43 sleep-pollers
  burning 1.2 cores" figure originally cited for this fix was MINE and was
  WRONG — see the queue-depth instrument below. Blocking still beats polling,
  so the fix stands on its own merits; only its stated magnitude was fiction.)

  **QUEUE DEPTH: use `lslocks`, never a process grep.**

      lslocks | grep tidepool-ghc      # WRITE = holder, WRITE* = blocked waiter

  Kernel truth, immune to the args-grep trap. `pgrep -f ghc-slots.sh` matches
  every `.claude-unwrapp` AGENT SESSION whose command line mentions the script —
  measured here as 22 agent sessions + 8 bash + 5 zsh, against **4 real holders
  and 4 real waiters**. The wave's "50-deep queue with 60-minute waiters" was
  that misclassification, and it was mine.
  Note what this is: **the identical trap this file already documents for
  `pgrep -fc tidepool-extract`, committed one level up by the person who
  documented it**, and then propagated upward and cited in a commit message
  before anyone checked the referent. A grep over process ARGS counts agents,
  not work. Reach for the kernel's own accounting.

  **THE GENERALISATION (spawn-latency, and it is the transferable part):
  verifying one instrument silently discharges the obligation for its
  neighbours.** Three instances in one day, and the shape is identical every
  time — the corrected instrument gets checked, the one on the adjacent line
  does not. The sharpest case: a single message that carefully used the
  anchored `^tidepool-extrac` pattern for extract count used a naive args grep
  for waiter count *on the line above it*, written by the person who had
  diagnosed that exact trap one message earlier. Not an inherited figure — an
  independently generated instance of a failure being demonstrated as
  understood in the same breath.
  So: when you fix one measurement, **audit every other number in the same
  report.** Fixing a number is not evidence about its neighbours, and the
  feeling of having just been careful is actively misleading.

  **REPORT THE HOLDERS, NOT JUST THE WAITERS** (boot, from being caught by it).
  A queue-depth number names a SYMPTOM; the holder list names a CAUSE. When a
  17-deep queue was escalated here, checking the four holders showed two were
  the escalating lane's own child. Before reporting starvation, run
  `lslocks | grep tidepool-ghc` and read BOTH columns — and check the holders'
  ages and commands (`ps -o etime=,args= -p <pid>`), since a slot held for an
  hour by a serialised run is a different problem from four slots doing work.

  **DRAIN, DON'T KILL — a running holder and a queued waiter are different
  objects** (boot-targets' corollary). Killing a RUNNING holder wastes the slot
  time already spent and frees the slot no sooner than letting it finish; kill
  it only when the work is known-void, never to reclaim capacity. Cancelling a
  QUEUED waiter is free and does relieve pressure — so cancel those freely.
  This is the operational half of "report the holders, not just the waiters":
  once you have both columns, the two columns afford different actions.

  **AT MOST ONE BROKERED LEG PER DEV AT A TIME.** Sequence gate legs; do not
  launch them concurrently. `detach` makes concurrency *easy* and does not make
  it *permitted* — detach is about surviving the WAIT, not about running things
  in PARALLEL. Those are independent, and a dev handed the mechanism without
  the envelope will reasonably infer the wrong one.
  The general form, boot's, and it is the companion to "a new mechanism
  announced is not a mechanism adopted": **a mechanism adopted is not a
  mechanism bounded.** When you hand down a capability, state its envelope in
  the same breath.

  **KNOW THE NOISE FLOOR BEFORE CLAIMING A TREND** (spawn-latency, measured).
  On this box, `ghc_setup` ranged **68 → 287 ms across five turns at a FLAT
  module graph** — a 4.2x spread with zero growth in the thing being varied. So
  a small-N sweep of an ms-scale quantity cannot distinguish a real trend from
  contention: clearing that floor for `depanal`-vs-depth needs ~8–10
  generations, not 2–3. Before reporting "X grows with Y", state the spread of
  X at constant Y. RATIOS survive contention far better than absolutes — C1's
  arm ratio moved <2 pp across a loadavg swing of ~11→~34 while absolutes swung
  ~40% — so prefer a ratio when one is available, and say which you are quoting.

  **NAME THE INSTRUMENT ALONGSIDE ANY NUMBER IN A RECEIPT** (boot). A reader
  must be able to check what a figure counts rather than trust its label. Prefer
  counting AT THE SOURCE, inside the code path, over pattern-matching process
  lists from outside: `acceptance_boot_compile_count` is trustworthy precisely
  because it counts extract spawns in-code, in a week when three separate
  external-observation instruments were each wrong in a different direction. An
  in-code counter establishes its referent structurally; an external pattern
  match only asserts one. This is the named-guard rule one level down.

  The MemAvailable floor is a soft guard and is NOT binding at 18 GB. If you see
  a floor rejection, that is real memory pressure, not this.

  **USE `detach` FOR ANYTHING THAT MIGHT QUEUE — this is now the default for
  GHC-heavy work, not an escape hatch:**

      /home/inanna/dev/tidepool/scripts/ghc-slots.sh detach -- <cmd>

  New session via `setsid`; prints pid + log path and returns instantly. Poll
  the log across turns. Slot discipline is FULLY honoured — the detached child
  is `run` itself, so it holds and releases the `flock`. This is not a bypass.

  Why it exists (root's `bf3026af`, from this wave's finding): `ghc-slots.sh
  run` has **no timeout and no give-up path** — `while :;` forever. Combined
  with the environment's ~380s process kill and 3 slots shared across three
  waves, "wrap every GHC-heavy invocation" was an **unsatisfiable triple**: a
  fully compliant dev's attempt is not refused a slot, it is KILLED WHILE
  QUEUED, making zero progress indefinitely. Demonstrated, not inferred:
  `boot-targets` died twice at ~340s without ever acquiring. `detach` decouples
  the queue wait from the agent's process lifetime. It is also strictly better
  than the old behaviour absent any kill: a dead pane no longer drops a queued
  waiter.

  If you launch a detached job and then abandon it, **reap it** — an orphaned
  waiter holds a queue position nobody is waiting on, and relaunches stack.

  **MEASURE FROM INSIDE THE DETACHED COMMAND, NEVER FROM THE QUEUEING SHELL**
  (spawn-latency, 2026-08-08). `detach` decouples QUEUE time from EXECUTION
  time, so the box conditions visible where you enqueue a job are NOT the
  conditions it runs under — and with queue ages reaching 60 minutes the gap is
  enormous. Any sample labelled with the queueing shell's loadavg, extract
  count, or cap state is MISLABELLED. Emit those readings from inside the
  detached command, to its own log, immediately before the measured work
  starts. This is the same failure as the phase rows that opened this wave — a
  label asserting more than it establishes — arriving through a new door that
  the fix for the previous one opened.

  If a slot wait exceeds ~15 minutes, REPORT it upward as a starvation signal
  rather than bypassing the broker.
  **MEASURING actual load — the obvious commands are both wrong.**
  `pgrep -fc tidepool-extract` OVER-counts wildly (22–24 on a box with 4 real
  compiles): it matches every agent shell, `.claude-unwrapp`, `bash`, `zsh` and
  `timeout` process carrying `TIDEPOOL_EXTRACT=…` in its command line or
  environment, so it tracks how many AGENTS exist, not how much GHC runs.
  But matching on `comm` with the full binary name UNDER-counts to a constant
  zero: a USERSPACE process's `comm` is capped at 15 characters
  (`TASK_COMM_LEN` is 16 including the NUL — verified directly:
  `/proc/<pid>/comm` for a live extract reads `tidepool-extrac`, length 15), so
  `grep tidepool-extract-bin` can NEVER match.
  Precision, because a naive check refutes the general form: `ps -eo comm=` DOES
  show longer values — 39, 36, 33 characters — but every one of them is a
  KERNEL thread (`nvidia-modeset/…`, `kworker/…`, `rcu_…`), which is a
  different naming path. The 15-char cap holds for every binary we care about.
  Anchor and stop at `extrac`; do not generalise past userspace.
  The correct instruments:

      ps -eo comm= | grep -c '^tidepool-extrac'    # real compiles (note: 15-char truncation)
      cat /proc/loadavg                            # actual load

  Use both. A zero from a mistyped pattern reads exactly like a quiet box.
  **Bias toward fewer, better-batched runs.** This wave is the heaviest GHC
  consumer on the box, so the throttle bites hardest here.
- `export XDG_CACHE_HOME="$PWD/.cache"` before harness shards.
- NEVER run bare `scripts/battery.sh` — this environment hard-kills background
  processes at ~380s and the full battery is hours. Use:
  - tier 1 `cargo nextest run` (pure-Rust) — under THROTTLE V2 this is
    **case (a): do NOT broker-wrap it.** Run it as
    `export CARGO_BUILD_JOBS=4; nice -n 15 cargo nextest run -j 4`.
    (Two superseded phrasings, recorded so a stale copy is recognisable:
    "safe unattended" was WITHDRAWN at 24f6d7a7; "MUST be broker-wrapped" was
    v1 and is superseded by v2.)
  - tier 2 `scripts/battery.sh -p <crate> -E 'test(<name>)'`;
  - tier 3 `scripts/battery-shard.sh <crate>`;
  - tier 4 `TIDEPOOL_EXPENSIVE_TESTS=1 scripts/battery-shard.sh <crate> ...`,
    deliberately, one suite at a time, outside the ~380s assumption.
  Size every shard for the ~380s kill.
- INHERITED-RED RULE: a "pre-existing / inherited red" claim requires a
  cache-consistent A/B baseline run in YOUR OWN worktree — same compile-cache
  state on both legs, your diff absent vs present. An argument from "my diff
  doesn't touch the failing test's files" is INVALID for global surfaces
  (prelude exports, pragma/extension sets, shared flags): every Haskell compile
  is downstream of those whether or not its file is in the diff. Note the cache
  confound explicitly — a fingerprint-invalidating change makes a naive
  comparison measure cold-vs-warm, not the diff.
- **The "extractor id-stability is a PINNED invariant" shorthand OVER-CLAIMS —
  corrected 2026-08-09.** Three permanent tests exist and each pins a real
  property well, but the phrase reads as blanket coverage of the id space and
  is not. What they actually observe:

  The trio, **enumerated by exact path because it was never written down
  anywhere** (see the meta-note below) and each one read to confirm:

  | test | what it pins |
  |---|---|
  | `tidepool-repr::extend_checked_equivalence::distinct_ids_sharing_a_qualified_name_collide_regardless_of_input_order` | two distinct **DataConId**s sharing a qualified name is a hard error in `insert_checked`/`extend_checked`, in either arrival order |
  | `tidepool-repr::extend_checked_equivalence::merge_table_skip_filter_cannot_dodge_the_qualified_name_collision_guard` | `merge_table`'s skip-identical pre-filter cannot elide a colliding **DataConId** before the guard sees it |
  | `tidepool-runtime::session_table_qualified_identity` | no qualified constructor name maps to >1 **DataConId** in the accumulated table; freer-five ConTags still resolve to what bootstrap froze |

  **ALL THREE ARE DataConId GUARDS. None observes VarIds at all** — not
  `localVarId`, not `stableVarId`. So the label "extractor id-stability" never
  covered the VarId space by any of these tests. `localVarId` bakes the raw GHC
  `Unique` for internal/floated bindings and is allocation-order-sensitive *by
  its own doc comment*; determinism there was never claimed and is not watched.
  A green triple means **constructor-identity guarding is intact**. **It does
  not mean ids did not move, and their silence is not consent.**
  Still true: if your change FIRES any of them, STOP and escalate to your TL —
  that is a design conversation with root, not a test to silence. But do not
  read a PASS as blanket id coverage; if your change touches `localVarId`'s
  path, no pinned test is watching and you owe a direct experiment.
- RECEIPTS ARE PER-BINARY PASS/FAIL COUNTS, never exit codes. Paste the counts
  (`N passed, M failed` per test binary) in your submit note. "It passed" with
  no counts is not a receipt.
- **EVERY RECEIPT CARRIES A DENOMINATOR: `N passed / M total` per leg.** A
  count without a denominator cannot distinguish a completed run from a
  truncated one. Three ways a leg looks done without being done, all found in
  one day, all on the MANDATED tier:

  | variant | appearance | reality |
  |---|---|---|
  | never started | instant exit, clean log | ran nothing (PATH lacked GHC) |
  | queued, killed | log shows only "all slots busy" | ran nothing |
  | **fail-fast truncated** | **real PASS lines, names, timings** | **198/877 — 23%** |

  The first two are visible in the log. **The third looks exactly like a run**
  and answers YES to every sweep question; the only tell is the denominator.
  `--no-fail-fast` is NOT in the battery scripts (verified), so any inherited
  red truncates the mandated tier silently. Pass it explicitly on gate legs, and
  check the completed count against the crate total before banking a receipt.
  Check log CONTENT, never exit status: an environment failure exits in
  milliseconds and reads as a fast pass.

- **PREFER AN ARGUMENT AN INCOMPLETE SEARCH CANNOT MISLEAD** (spawn-latency).
  When a claim rests on a grep being exhaustive, look for a route to the same
  conclusion that does not. Worked example: asked to bound the blast radius of
  downgrading CHECK B, the grep-based answer was "only tests asserting
  extraction FAILS are at risk" — sound, but only as good as the search. The
  structural answer is stronger and cheaper: **CHECK B is NEW, introduced by
  this very item, so nothing predating it can depend on its fatality.** Blast
  radius is exactly the tests the item wrote. Same conclusion, immune to a
  missed file.
  This is the constructive twin of the instrument rules below: those say a
  search can silently return a plausible wrong answer; this says when you can,
  do not stake the claim on a search at all.

- **HOLD A PRIOR EXPECTATION OF THE ANSWER'S SIZE BEFORE YOU RUN THE COMMAND**
  (spawn-latency — the deepest of the instrument lessons, because it says care
  is not the defense). Counting `#[test]` declarations across 11 acceptance
  binaries took three successive grep patterns; **the first two returned a clean
  `0`**, defeated by `#[tokio::test(flavor = "multi_thread", …)]` carrying
  arguments. Each attempt looked careful and each returned a plausibly-shaped
  answer. What caught it was not pattern discipline — it was **implausibility**:
  zero tests across eleven acceptance binaries cannot be true.
  So the earlier rule ("a correction to an instrument needs the same
  verification as the instrument") is necessary but insufficient: more care with
  the regex would not have helped. **A zero is only catchable if you knew
  roughly what non-zero should look like.** State the expected magnitude first,
  then run the command, then compare. This is the same instinct as the
  denominator rule — know what the total ought to be before you read what it
  was.

- **A COMMENT SAYING "THIS CAN DRIFT" IS NOT A MITIGATION** (boot). It is a
  recorded decision to keep a mechanism that fails silently, and it reads as
  diligence while providing none. Live proof: `eval_harness.rs:387` documents
  that `EFFECT_NAMES` can drift from production and cites the prior instance
  (`f1a480e6`) — then it drifted again (`Fork`). The comment is evidence that
  hand-maintenance was already tried and already failed. Derive from the source;
  do not restate it and annotate the restatement. Same family as a sync comment
  standing in for a shared predicate: documentation substituting for a
  mechanism.

- NAMED-GUARD RULE (wave-wide, from spawn-latency, 2026-08-08): **if a gate
  exists to catch ONE specific failure mode, the receipt must show that
  specific test passing by name, with its own pass line — not the aggregate
  that contains it.** An aggregate count proves a suite ran; it does not prove
  the guard executed. The residual hole it closes: the test is present in the
  tree but the shard's filter does not select it — a renamed binary, an
  `#[ignore]`, a cfg, an env-gated early return. Then the base commit is
  correct, the count is green, and the guard never ran.
  Where a guard is CROSS-LANE (it lives in one lane's branch and protects
  another's change), the receipt must ALSO name the base commit it ran
  against. Base proves which tree ran; the test name proves execution. Both,
  or neither is established.
  This is the same instrument that produced the C1 finding: do not trust that
  a label ("harness acceptance, N passed") covers what its name implies.
- Never touch another agent's worktree. Never checkout another branch. You are
  your worktree.
- `TIDEPOOL_EXTRACT` must point at a freshly built `tidepool-extract-bin` for
  any test that compiles Haskell; the battery scripts do this for you when it
  is unset. Symptom of a missing one: `Metadata entry must be an array of
  exactly 7`.
```

## Correctness gates (this wave's standard; sub-TLs enforce per item)

- **hardened differential** with its floors — `haskell_suite_differential`
  (`#[ignore]`d + expensive: `TIDEPOOL_EXPENSIVE_TESTS=1 scripts/battery-shard.sh
  tidepool-codegen --run-ignored all -E 'test(haskell_suite_differential)'`).
  `COMPARED_FLOOR` must not drop.
- **corpus_report** — same shape,
  `-E 'test(corpus_report)'`.
- **extract-fidelity-test — ALL tests, report the actual N/N.** Do NOT match a
  hardcoded number: this line said `26/26` and D1-A added four `D1Defense`
  checks, making it `30/30`. A dev reporting `26/26` against a stale spec would
  be reporting a **truncated run that matches the doc** — the denominator rule
  defeated by the gate list itself. The count is whatever the suite currently
  holds; report it and require zero failures.
  (Historical: 26 pre-D1-A, 30 after.) Invocation —
  `/home/inanna/dev/tidepool/scripts/ghc-slots.sh run -- bash -c 'cd haskell && cabal test extract-fidelity-test'`.
  26 of 26, no fewer.
- **harness acceptance** — `scripts/battery-shard.sh tidepool-harness
  -E 'binary(/^acceptance_/)'` (shard further if it exceeds the budget).
- **E6 additionally**: the FULL set above, zero tolerance — exposed unfoldings
  change what extraction sees, so a single regression blocks the item.
