# Resident compile-daemon design

Status: design only, no code. Written for operator review. Every claim below
cites the file/symbol it rests on; genuinely undecidable-from-the-code items
are called out as open questions at the end, not decided quietly.

## Problem statement

Operator context (2026-08-24): every `tidepool-extract` invocation is a full
GHC-as-library process — GHC boots, loads its own package interfaces, and
(re)compiles the whole stdlib closure it needs, every single time. Measured
cost: ~600-800MB resident and ~5s startup+load per spawn. This is paid
hundreds of times per test battery (`plans/test-time-cut.md` §1 sampled 133
real spawns averaging 5.35s each — see §6 below) and once per live model
round in dogfood (~7.7-17s measured per round). Twelve coincident spawns
recently filled the box's swap and degraded it — twelve independent ~700MB
GHC processes is close to 8.4GB before any of them has done useful work.

**One warm process holding the loaded stdlib, serving compile requests,**
amortizes all three costs: the per-spawn memory floor (N concurrent requests
share one loaded environment instead of paying N times), the battery wall
clock (hundreds of spawns stop each re-paying GHC boot + stdlib typecheck),
and live-round latency (a round's compile no longer waits on a cold GHC
boot). This document surveys what an extract invocation actually does today,
then proposes the daemon's shape: what persists, what must never leak
between requests, how it detects a stale toolchain, how it survives a
crashed or wedged compile, and how it plugs into the existing
`tidepool-extract-cmd` client seam without becoming a second, competing
invocation builder.

This design already has two committed prerequisites. The now-deleted
`plans/turn-latency-state-injection.md` (content preserved in git history at
`724c4945^:plans/turn-latency-state-injection.md`) named this direction
explicitly in a section titled "Direction: toward a resident compile
daemon," and shipped two steps toward it: injecting companion state as a
value rather than a source literal (commit `1997ddc9`, prerequisite 1), and
the `--build-products-dir` on-disk interface cache (commit `318e729c`,
prerequisite 2 — see §1). Both are load-bearing context for what follows,
not proposals of this doc.

---

## 1. Survey: what a `tidepool-extract` invocation actually does

Source: `haskell/app/Main.hs`, `haskell/src/Tidepool/GhcPipeline.hs`.

### 1.1 The per-process lifecycle

`main` (`Main.hs:64-124`) parses argv, dispatches on mode (turn-batch /
classify / turn / targets / session / one-shot — `Main.hs:86-124`), and every
non-batch mode ultimately calls `GhcPipeline.runPipeline`/
`runPipelineSession` (`runPipeline = runPipelineSession Nothing`,
`GhcPipeline.hs:106-107`), which is `runCompile variant path includes`
(`GhcPipeline.hs:238-242`). `runCompile` (`GhcPipeline.hs:196-227`) is the
literal per-process boot sequence:

1. `getLibdir` (`Tidepool/ExtractUtil.hs:16-22`) — `$TIDEPOOL_GHC_LIBDIR` if
   set, else shell out to `ghc --print-libdir` (a subprocess, paid on every
   spawn unless the env var is set).
2. `runGhc (Just libdir) $ do ...` — opens a fresh GHC API session
   (`GhcPipeline.hs:201`).
3. `setSessionDynFlags` on `extractionDynFlags dflags includes`
   (`GhcPipeline.hs:221-222,758-773`): pins the target platform to
   `genericPlatform` (JIT needs deterministic x86_64 Core regardless of
   host — comment at `GhcPipeline.hs:204-214`), disables host SIMD flags,
   exposes the hidden `ghc` package (needed by `Tidepool.QQ`'s splices,
   `GhcPipeline.hs:215-220`), and appends `includes` (the stdlib root plus
   any caller `--include` dirs) to `importPaths`.
4. `runCompileCycle` (`GhcPipeline.hs:325-638`) — the actual compile: build a
   `PipelineVariant`-supplied `CompilePlan` (`GhcPipeline.hs:158-182`),
   `depanal` the module graph, `load'` it (GHC's own make-mode downsweep +
   recompilation-checked load, `GhcPipeline.hs:379-385`), then per-module
   `parseModule`/`typecheckModule`/`hscDesugar`/`core2core`
   (`compileFront`/`compileBack`, `GhcPipeline.hs:441-492`), merge the
   resulting `CoreBind`s/`TyCon`s into one `PipelineResult`.
5. `Translate.translateModuleClosed` + `CborEncode.encodeTree`/
   `encodeMetadata` + file writes (`Main.hs:439-448` for the common
   `--target` path) — Core → the wire IR, then CBOR to disk.
6. Process exit, taking the whole loaded GHC session with it.

Every one of steps 2-4 is **make-mode already**: `load'` is GHC's own
dependency-aware, recompilation-checking loader (`GHC.Driver.Make.load'`,
imported at `GhcPipeline.hs:18`), not a bare one-shot `compileToCore`. The
"make-mode vs. oneshot" question the operator asked about is therefore
already answered by the existing pipeline — there is no mode switch to make.
What is missing is a process that lives long enough for `load'`'s own
recompilation avoidance (`checkOldIface`) to have anything warm to check
against.

### 1.2 What is per-request today, forced to be per-request only by process death

- The target file (`path`, `guessTarget path Nothing Nothing`,
  `runCompileCycle`'s first line, `GhcPipeline.hs:330-331`).
- The `SessionScope` (`--session-root`/`--inject-val`,
  `Tidepool.Session.hs:199-208`) for a repl-session turn.
- `includes` (`--include` dirs) — currently threaded as a function
  parameter into `extractionDynFlags` at session-setup time
  (`GhcPipeline.hs:221`, once per process), so it is technically
  session-wide DynFlags state, not per-compile — but because the process
  dies afterward, it reads as "per request."

### 1.3 What already persists across *compiles within one process* — the existing analog

`runBatchPipeline` (`GhcPipeline.hs:697-730`, the `--turn-batch` mode,
`Main.hs:91`) is the closest thing in the codebase to a resident compile
loop today: **one** `runGhc` boot, **one** `setSessionDynFlags`, then N
`runCompileCycle` calls in a loop (`go`, `GhcPipeline.hs:711-730|703-710`),
threading two caller-provided caches across every cycle:

- `ModIfaceCache` (`GHC.Driver.Make.newIfaceCache`, `GhcPipeline.hs:708`) —
  GHC's own iface cache, so cycles 2..N skip re-parsing/re-typechecking
  stdlib package interfaces the first cycle already loaded.
- `GutsMemo` (`Map ModuleName GutsMemoEntry`, `GhcPipeline.hs:266-289,709`)
  — a hand-rolled per-module memo of post-`core2core` guts, reused verbatim
  on a later cycle that recompiles the same `ModuleName`
  (`GhcPipeline.hs:497-518`).

The `GutsMemoEntry` haddock (`GhcPipeline.hs:266-271`) states the memo's
correctness invariant explicitly: *"A module's SOURCE cannot change within
one batch spawn (a stdlib/library module is stable; a batch item's own turn
module is always freshly, uniquely named by the caller)."* This invariant is
supplied by the ONE caller sequencing a known list of items
(`app/Main.hs`'s `--turn-batch` mode reading one `plan.json`) — it is not a
property GHC or this module enforces on its own. §2 below explains why a
daemon serving independent client requests cannot inherit this invariant for
free.

`--build-products-dir` (`Main.hs:71-73,278-289`,
`GhcPipeline.hs:775-805`) is the disk-based cross-**process** analog of the
same idea: point `hiDir`/`objectDir` at a persistent directory and turn on
`-fwrite-interface`, so a LATER process's `load'` can `checkOldIface` its way
past an unchanged home module instead of recompiling it — spike-verified in
`haskell/spike-build-products/Spike.hs` (three fresh `runGhc` sessions
sharing one on-disk dir: cold → warm-identical → warm-changed, each getting
the expected `NeedsRecompile`/`UpToDate` verdict). This mechanism exists
*because* the current model has no persistent process to hold the cache in
memory — a resident daemon does, and should prefer the in-memory
`ModIfaceCache`/`GutsMemo` shape over the disk round-trip for its own warm
state (§2, §7). `--build-products-dir` is unaffected by this design and
remains exactly what it is today: the fallback path's own cross-process
mechanism, not something the daemon needs to also do internally.

### 1.4 The fat-interface cost being amortized

`extractionDynFlags` pins `-O2`-equivalent optimization with exposed
unfoldings (`canonicalizeDFlags`, referenced throughout `GhcPipeline.hs`,
e.g. `compileFront`'s re-canonicalization at line 442) — this is what the
codebase calls "fat interfaces" (`haskell/CLAUDE.md`'s
`TIDEPOOL_IFACE_DEBUG=1` diagnostic: *"Missing unfoldings / 'unresolved
external' mysteries"*). Loading a package interface WITH unfoldings is
substantially more expensive than loading a bare type signature, and every
one of the ~40-51 stdlib modules (`Tidepool.Prelude`, `.Effects`, `.Form`,
`.Agent.*`, …) plus every external package they touch (`base`, `aeson`,
`freer-simple`, `lens`, …) pays this cost fresh on every process, before a
single byte of the target's own source is read. Measured evidence this
dominates: `plans/test-time-cut.md` §1 found session-scoped and non-session
compiles cost **the same** per spawn (5.39s vs. 5.32s average) — *"session
compiles are not slow because of what they do, they are slow because every
one of them is a full GHC invocation, same as any other."* The repl census
log line quoted in that same doc — `tidepool-compile-summary modules=41
wall_ms=3111` — is the stdlib typecheck alone, paid on every process that
touches session state, independent of whatever the target module itself
needs.

---

## 2. Session-scope isolation

**The requirement, restated concretely.** Today, isolation between two
compiles is a side effect of the OS: process A's `HscEnv`, HPT, and EPS die
with process A, so process B can never observe them. A resident daemon
removes that side effect on purpose — so it needs its own mechanism, named,
not assumed.

### 2.1 The mechanism that already exists, and its actual scope

Two structural facts, both already true of the pipeline before this design
touches anything:

1. **`load'` wipes the whole HPT on every call.** This is stated as existing
   fact in the codebase's own commentary on the `depanal`/`load'` sequence
   (`GhcPipeline.hs:337-356`, the EPS-unpoisoning comment, which depends on
   this behavior to justify its own fix) and is exactly what
   `runCompileCycle` re-runs, unconditionally, on every cycle — including
   every cycle inside `runBatchPipeline`'s loop. A daemon that calls
   `runCompileCycle` once per incoming request, exactly as `runBatchPipeline`
   already calls it once per batch item, inherits this wipe for free: no
   home-module binding from request N-1 is visible when request N's `load'`
   runs, unless something *outside* `load'` re-injects it.
2. **Session state is disk-resident, not server-memory-resident, already.**
   `injectSessionIface` (`Tidepool/Session.hs:286-305`) reads a turn's
   `Val.G<g>`/`Lib.G<g>` iface by RAW PATH off disk (`readIface`,
   `sessionHiPath root sm`) on *every* invocation that needs it — nothing in
   `GhcPipeline`/`Main` caches a session's decl content in the process
   itself. A daemon that keeps reusing this same function keeps this
   property: the daemon process holds zero session-specific state between
   requests, by construction, because the mechanism that supplies session
   state never wrote any into the process to begin with.

Combined, these two facts mean the daemon does not need to *invent*
isolation — it needs to avoid *breaking* isolation that a naive "share
everything for speed" instinct would reach for.

### 2.2 Where the naive instinct breaks, named concretely

The one thing worth widening the batch-mode `GutsMemo`/`ModIfaceCache` for
speed is exactly the thing whose correctness invariant (§1.3) a daemon
cannot uphold: **module names are not globally unique across independent
top-level requests.**

- Every session turn's wrapper compiles a binder named `__result`
  (`scaffoldTargetName`, `Session.hs:339-340`) or `result`
  (`scaffoldOutputBase`, `Session.hs:346-347`) — this name is the SAME
  string across every session, every generation, every caller.
- A `Val.G<g>` module's generation number `g` is scoped to one session's
  `ssRoot` directory (`SessionScope.ssRoot`, `Session.hs:199-202`) — two
  independent repl sessions both mint `Tidepool.Session.Val.G1`,
  `...G2`, … starting from 1. The module NAME string
  (`sessionModuleString`, `Session.hs:150-152`) is identical across
  sessions; only the on-disk root differentiates the content.

Inside `runBatchPipeline`, this is safe because the ONE caller
(`app/Main.hs`'s turn-batch mode) sequences a list it built itself and
guarantees the uniqueness the `GutsMemoEntry` haddock asserts. A daemon
serving independent connections from independent clients — different repl
sessions, different harness turns, a battery test and a live round
overlapping — has no such caller-level guarantee. If a request-spanning
`GutsMemo` keyed only on `ModuleName` were shared across requests, session
B's `__result` compile could silently receive session A's cached
`__result` guts, or session B's `Tidepool.Session.Val.G1` compile (if
`Val.G<g>` modules were ever routed through the compiled-module memo rather
than the disk-inject path) could receive session A's G1 content. **This is
the concrete failure mode the design must name and close, not merely a
theoretical caveat.**

### 2.3 The proposed mechanism

**A fresh `runCompileCycle` per request, over a shared but request-blind
base, with the module-level memo scoped only to content proven
session-invariant:**

- **Shared across every request (the win):** the `runGhc` session itself
  (opened once, at daemon boot), its `DynFlags` (`extractionDynFlags` run
  once), and a `ModIfaceCache` + `GutsMemo` populated **only** by compiling
  the fixed stdlib/preamble tree (`Tidepool.Prelude`, `.Effects`, and every
  other module under the resolved stdlib root — the same tree
  `stdlib_fingerprint` walks, §3) once at boot or on first use. This tree's
  module set and content are invariant for the daemon's whole lifetime
  (until a redeploy, §3) — its names never collide with a request's own
  target/session modules, which live under distinct, request-supplied
  paths and (for session turns) the reserved `__result`/`Tidepool.Session.*`
  names that are explicitly excluded from this shared memo.
- **Never shared across requests:** anything keyed by `ModuleName` for a
  request's own target or session modules. The daemon calls
  `runCompileCycle` with `mMemoRef = Nothing` for the per-request portion of
  the compile (mirroring what a lone, non-batch `runCompile` already passes
  today, `GhcPipeline.hs:227`) — a request's target/session modules are
  always compiled fresh, exactly as they are in a spawned process today,
  just without re-paying the stdlib's own parse/typecheck/`core2core` cost.
  `injectSessionScope` (`Session.hs:325-327`) keeps reading `Val`/`Lib`
  ifaces off disk per request, unchanged.
- **The HPT wipe stays the isolation backstop, not a nice-to-have.** Even if
  a future change accidentally widened the shared memo's scope, `load'`'s
  unconditional per-cycle HPT clear (§2.1) means a leaked cache entry could
  at worst serve a STALE-but-still-content-addressed guts blob for a name
  that recurs — never a live, mutable reference into another request's
  in-flight compile. The memo-scoping rule above is what prevents the stale
  serve from happening at all; the wipe is why a bug in that rule fails
  loud (wrong output, caught by whatever consumes the CBOR) rather than
  fails by leaking one session's *live* state into another's request.

Concretely, this is "a fresh `HscEnv` layer per request over a shared base"
— but the layering mechanism is not a new one to invent: it is
`runCompileCycle`'s own `load'` call plus the existing discipline of never
letting a request-scoped `ModuleName` enter a request-spanning memo. Naming
it any more elaborately (a hypothetical GHC-level "session fork" primitive)
would invent machinery GHC's API does not offer and this pipeline has never
needed.

### 2.4 Relationship to the compile-memo's session-lane exclusion (item #3)

`tidepool-runtime/CLAUDE.md`'s Compile cache section states the existing
memo's session-scope exclusion plainly: `--session-bind`/`--inject-val`/
`--session-root` flags are not on the `invocation_key` allowlist
(`tidepool-runtime/src/cache.rs:558-608`), so a session-scoped invocation
keys to `None` and always compiles cold. `plans/test-time-cut.md` item #3
proposes closing this gap by widening the allowlist to admit session state
under a content-addressed key (hash the injected `Val.G<g>` iface bytes, not
the session-root path) — sized there as "roughly comparable scope to the
original compile-memo lane itself... NOT a quick win."

**The daemon does not supersede item #3's mechanism — it removes most of
item #3's motivation.** The memo and the daemon attack different axes of the
same measured cost:

- The memo's job is to serve a repeat compile of **byte-identical input**
  without recompiling at all. Item #3 would let a session turn benefit from
  that when its injected content happens to repeat.
- The daemon's job is to make **every** compile — repeat or novel content —
  cheaper by removing the ~5.3s GHC-boot-plus-stdlib-typecheck tax that
  `plans/test-time-cut.md` §1 measured as *identical* between session and
  non-session compiles. This tax is what item #3 was actually chasing for
  the common case: most of a session compile's cost is not the session's
  own content, it is the same fixed tax every non-session compile also
  pays.

Once the daemon exists, item #3's remaining addressable value shrinks to the
genuinely narrow slice `plans/test-time-cut.md` §2 already measured:
literal duplicate session content within one binary (`acceptance_cross_turn`:
24 distinct among 31 session-scoped spawns — 7 duplicate spawns, 23%). For
that slice, a warm daemon still pays the target/session module's own
typecheck+desugar+`core2core` on every repeat (§2.3 deliberately never
memoizes those), whereas a working item #3 would skip it entirely. That is
a real, smaller win, not a fabricated one — but it is now bounded by
"however long a single non-stdlib module takes to typecheck" rather than by
"however long a full GHC boot takes," which is a much smaller number.
**Recommendation: item #3 is complementary but low-priority — defer it and
re-measure the session-lane's residual cost after the daemon lands,** rather
than building the content-addressed keying work now against a cost profile
that is about to change substantially.

---

## 3. Invalidation

**Never a second freshness mechanism — ride the existing deploy handshake.**
`tidepool-runtime/src/toolchain.rs` already owns exactly this problem for
the two existing servers: `ToolchainStamp{extract, stdlib}`
(`toolchain.rs:452-465`) records content fingerprints —
`extract_fingerprint` (blake3 of the binary's own content plus any wrapper
targets, `toolchain.rs:375-381`) and `stdlib_fingerprint` (blake3 over every
`.hs` file under the resolved `Tidepool/` root, sorted by relative path,
content-only and path-independent, `toolchain.rs:396-416`) — written once by
`scripts/redeploy.sh`'s closing `tidepool --write-toolchain-stamp`
(`haskell/CLAUDE.md`'s Deploy handshake section) and checked by
`check_handshake`/`enforce_handshake` (`toolchain.rs:643-668,712+`) at
server startup, with severity controlled by `$TIDEPOOL_TOOLCHAIN_HANDSHAKE`
(`toolchain.rs:671-690`).

**At daemon boot:** call `locate_extract`/`locate_stdlib`
(`toolchain.rs:235,288`) then `enforce_handshake` exactly like `tidepool` and
`tidepool-repl` already do — a skewed pair refuses to boot (default
severity), naming `scripts/redeploy.sh` as the fix, same message shape as
every other server. No new stamp format, no new fingerprint function.

**While the daemon is alive:** a long-lived process is exactly the case the
existing once-at-startup handshake was never asked to cover — a
`scripts/redeploy.sh` run while the daemon is up updates the extract binary,
the stdlib tree, and the stamp, but the daemon's already-loaded `HscEnv`/
`ModIfaceCache` (§2.3) has no way to notice. Two ingredients close this
without inventing a second mechanism:

1. **Detection reuses `check_handshake` verbatim.** The daemon re-runs
   `check_handshake(extract, stdlib)` against its OWN boot-time-resolved
   paths on a cheap interval (a background tick, not per-request — the
   toolchain module's own doc notes the fingerprint cost is "memoized...
   at most once per extract version per machine," i.e., cheap to repeat but
   not free enough to redo per compile). A `HandshakeOutcome::Skew` means
   the live binary/stdlib content no longer matches what this daemon loaded
   at boot.
2. **The daemon's own response to detected skew is to exit, never to keep
   serving from stale state.** It is not this process's job to hot-swap a
   new GHC environment in place — GHC's API has no supported "reload the
   package database under a live session" operation, and inventing one
   would be new, unproven machinery exactly where correctness matters most.
   Exiting cleanly hands the problem to whatever supervises the daemon's
   lifecycle (§4) to relaunch a fresh process against the new toolchain —
   the same "stale process dies, a fresh one boots against current state"
   shape every other invalidation path in this codebase already uses
   (`ExtractNotFound`/skew failures are refuse-and-fix, never silently
   tolerated).

This is additive to the existing handshake, not a parallel check: same
stamp file, same fingerprint functions, same severity env var. A daemon
running with `$TIDEPOOL_TOOLCHAIN_HANDSHAKE=off` (an existing escape hatch,
`toolchain.rs:677-682`) simply never notices skew either at boot or at
runtime — consistent with what that setting already means everywhere else.

---

## 4. Crash/poison containment

### 4.1 What is already contained, and what genuinely is not

Every *typed* compile failure — a real Haskell type error, an unresolved
external, a `SourceError` — is already caught inside the process without
ending it: `Main.reportDiags` (`Main.hs:196-208`) wraps the whole pipeline
call in `try`, and `GhcPipeline.gTryAny` (`GhcPipeline.hs:644-645`) is the
`Ghc`-monad-safe version of the same thing, already used by
`runBatchPipeline`'s own loop (`GhcPipeline.hs:713-730`) to keep the shared
session alive across a failing batch item and attribute the failure to the
right item. A daemon reusing this same `try`/`gTryAny` wrapping per request
inherits this for free — a bad user program is a normal diagnostics
response, not a daemon-killing event, exactly as it is not a
process-killing event today.

What `try :: IO a -> IO (Either SomeException a)` structurally cannot catch:
a genuine segfault, or a GHC-internal panic that calls `exitImmediately`
below Haskell exception handling, or an unbounded hang. This is a real
surface here specifically because the pipeline executes untrusted-ish code
paths at compile time: `Tidepool.QQ`'s quasiquoters run GHC's own
splice/bytecode machinery during compilation (`GhcPipeline.hs:337-356`'s
comment on `enableCodeGenForTH`'s bytecode provisioning for TH/QQ-carrying
modules), and a bug in vendored splice code or in GHC itself can wedge or
crash the whole process mid-compile. A spawn-per-invocation model turns this
into "that one process died, the caller sees a spawn/exit failure"; a
resident daemon turns it into "every QUEUED and IN-FLIGHT request behind
the crashed one needs an answer too."

### 4.2 Respawn semantics

**At-most-once per request, never silent retry against the same process.**
A request whose connection the daemon-side compile was serving when the
process died gets a typed "the daemon crashed mid-request" error — never a
hang, and never a silent internal retry against a session that may carry
whatever corrupted state caused the crash. This mirrors the "cancel, then a
bounded grace period, then declare it stuck" idiom
`tidepool_runtime::session::supervisor::TurnSupervisor`
(`tidepool-runtime/src/session/supervisor.rs:52-84`) already establishes for
JIT-side turn aborts — the concrete lever differs (there is no
`CancelHandle`-equivalent cooperative abort inside a wedged GHC compile;
the only real lever is killing the OS process), but the shape ("bound the
wait, then treat as stuck, never wait forever") is the same discipline
already standing in this codebase, not a new one invented for this design.

Concretely:
- The daemon process runs under a supervisor (an OS-level restart-on-exit
  wrapper — see open question 4) that relaunches it on any exit, clean or
  not.
- Each client request carries a bounded timeout on its own read of the
  daemon's response. A closed/reset socket or an elapsed timeout is a typed
  daemon-unavailable error at the CLIENT (`tidepool-extract-cmd`, §5) —
  never a hang.
- On a typed daemon-unavailable error, the client falls back to a direct
  spawn of the same `ExtractCmd` (§5) for that one request — the existing,
  always-correct path — rather than retrying the daemon immediately (which
  may still be mid-restart).

### 4.3 Memory growth over the daemon's lifetime

GHC-as-library sessions are not designed to run forever — interned-name
tables, the EPS, and TH-adjacent state are known to grow with a long-lived
session's history, and this codebase's own `TIDEPOOL_VARID_AUDIT`
diagnostic (`haskell/CLAUDE.md`) exists specifically because distinct
binders across compiles can collide or accumulate in ways a single short
process never surfaces. A daemon that never restarts risks trading "12
short-lived 700MB processes" for "one long-lived process that slowly grows
past what 12 short-lived ones would ever have used," which is the opposite
of this design's goal.

**Proposed policy: restart after N served requests, as the primary,
predictable trigger; an RSS ceiling as a secondary backstop.** A
request-count rotation is simpler to reason about and test than a live
memory measurement (no platform-specific `/proc` reading needed for the
primary path, and its behavior is deterministic given a request stream),
and it bounds the *number* of TH/QQ executions and module compiles any one
process lifetime accumulates — the two things named above as growth
sources. An RSS ceiling (checked on the same cheap interval as the
handshake re-check, §3) catches the case where a single unusually heavy
session (a very large batch of session turns, a module with a large
splice-driven expansion) grows past a safe bound before N requests have
been served. Both trigger the same clean-exit-and-let-the-supervisor-restart
path as a detected toolchain skew (§3) — one exit mechanism, two triggers
that feed it, never a third independent shutdown path. Concrete values for
N and the RSS ceiling are sizing questions this doc's boundary (no process
touches, no live measurement) cannot answer — flagged as open question 3.

---

## 5. Protocol + client seam

### 5.1 The client seam does not move

`tidepool-extract-cmd` stays the ONE `tidepool-extract` invocation builder
(crate charter, `tidepool-extract-cmd/CLAUDE.md`; Mechanism Index, root
`CLAUDE.md`). `ExtractCmd` already separates argument construction
(`.target()`, `.session_root()`, `.turn()`, … — `lib.rs:456-592`) from HOW
the built argv is launched (`Launcher::Direct`/`Wrapped`, `lib.rs:265-307`)
from the RUN itself (`ExtractCmd::run`/`run_with`, `lib.rs:602-626`,
returning `ExtractRun{output: std::process::Output, elapsed}`,
`lib.rs:349-359`). Every real call site —
`tidepool-runtime/src/artifacts.rs:584`,
`tidepool-runtime/src/session/mod.rs:732`,
`tidepool-runtime/src/session/turn.rs:576,944,1116` — calls `.run()` and
reads `ExtractRun`. A daemon transport that preserves this exact return
shape needs **zero changes at any of these five call sites.**

### 5.2 Proposed shape: a third launch path behind `ExtractCmd::run`

`Launcher` already has two variants for "how is the built argv executed"
(`Direct`, `Wrapped` — `lib.rs:265-274`). A daemon is a third: not a
`Command` at all, but a socket round-trip carrying the same argv. Concretely:

- Add `Launcher::Daemon(PathBuf)` (a UNIX domain socket path) alongside
  `Direct`/`Wrapped`.
- `ExtractCmd::run()` tries the daemon launcher first when
  `$TIDEPOOL_EXTRACT_DAEMON_SOCKET` names a socket that accepts a
  connection within a short bounded timeout; otherwise (unset, or connect
  fails, or the bounded read times out — §4.2) it falls back to today's
  `self.launcher` (`Direct`, resolved via the existing strict
  `resolve_bin`). **No daemon running is not an error — it is the default,
  unconditionally supported path**, exactly as it is today for every
  existing caller and every existing test that spawns `tidepool-extract`
  directly.
- The daemon-launcher response is synthesized into the SAME
  `std::process::Output` shape `ExtractRun` already carries — on Unix,
  `std::os::unix::process::ExitStatusExt::from_raw(code)` constructs a real
  `ExitStatus` from an integer exit code with no OS process behind it, so
  `ExtractRun::success()`/`stderr_lossy()` (`lib.rs:361-372`) work
  unmodified against a daemon-served response.
- The spawn counter (`EXTRACT_SPAWNS`, `lib.rs:63-79`) increments on a
  daemon-served request exactly as it does on a real spawn today — it
  counts "a `tidepool-extract` invocation was served," which stays true
  whether transport was a process or a socket, and every existing
  acceptance test that reads this counter (`plans/test-time-cut.md`'s
  cited "boot compile count" test) keeps its meaning unchanged.

### 5.3 Wire: a UNIX domain socket, one request/response per connection, JSON

**Plain and boring, not novel.** A UNIX domain socket (not TCP — this is a
same-host, same-user local dev tool; no reason to expose a network-bindable
service for it). One connection per invocation, request written, response
read, connection closed — this makes crash detection unambiguous (a broken
pipe or unexpected EOF mid-response IS the daemon-crashed-mid-request
signal §4.2 needs, with no separate heartbeat protocol to design) and keeps
the daemon's per-connection state trivial (nothing to garbage-collect
between requests beyond closing the socket).

Request: the built `argv()` (`lib.rs:596-600`, already exactly what the
direct-spawn path sends as process arguments) as a JSON array of strings,
one line, newline-terminated. Positional inputs are still file PATHS — the
daemon reads the same files a spawned process would, from the same
filesystem, so this requires no new file-transfer mechanism as long as the
daemon runs as the same user on the same host as its clients (true for
every deployment this codebase targets today; a remote daemon is out of
scope and not implied by anything in this design).

Response: one JSON object, one line — `{"exit_code": i32, "stdout": string,
"stderr": string}` (`stdout`/`stderr` as the process would have produced
them — the diagnostics-JSON-on-stdout contract `Main.hs`'s module doc
states, `Main.hs:59-63`, is unaffected; the daemon is a transport for that
same contract, not a redesign of it).

This intentionally does not reuse `tidepool-repr`'s durable-JSONL primitive
(`jsonl.rs`) — that mechanism is for DURABLE, append-only, torn-tail-
tolerant logs read back across process restarts; this is transient
request/response IPC over a socket that is closed after every message, with
no durability requirement and no tail to tear. Reaching for the durable
primitive here would be exactly the "an API one notch too narrow" mistake
in reverse — borrowing a mechanism built for a different problem shape
because it happens to also involve JSON lines.

### 5.4 Concurrency: default to one worker, argued, not assumed

**GHC's own session is a single mutable handle with no built-in
synchronization for concurrent use.** The `Ghc` monad's session-escape
bridge (`reifyGhc`/`reflectGhc`, imported at `GhcPipeline.hs:15` and used by
`gTryAny`, `GhcPipeline.hs:644-645`) exists precisely because a `Ghc` action
closes over one live session value — nothing in this codebase's use of the
GHC API introduces locking or otherwise makes concurrent mutation from
multiple OS threads against one `HscEnv`/HPT/EPS safe, and GHC's API was not
designed for that use. Running N compiles concurrently inside one daemon
process would require either serializing them anyway (defeating the point
of N workers) or maintaining N independent GHC sessions in one process
(which is just N processes' worth of the ~600-800MB memory floor this
design exists to eliminate, now colocated instead of separate — no memory
win at all).

**Recommendation: one worker, a FIFO request queue.** This directly targets
the memory incident (one resident environment, period, regardless of how
many callers queue behind it) and is the simplest correct starting point —
serialized compiles are exactly what today's spawn-per-invocation model
already gives you when multiple callers happen to overlap (the OS just
queues them onto CPU instead of onto an explicit daemon queue). If measured
tail latency under real concurrent load (a live model round competing with
a background battery, say) shows queueing delay dominating, the escape
hatch is a small, explicit, config-gated worker count (2, not "auto-scaled
to core count") — each additional worker is a second full GHC session and
should be sized as a deliberate memory-for-latency trade the operator
signs off on, not a default. This doc does not have the live-load
measurement needed to pick between 1 and 2 today (open question 1).

### 5.5 Fallback is not a special case

Because §5.2 makes "no daemon" the untouched default path through the exact
same `ExtractCmd::run()` every caller already uses, there is nothing
daemon-specific for a caller, a test, or an existing spawn-count acceptance
test to special-case. The daemon is additive infrastructure a caller can
benefit from without knowing it exists.

---

## 6. What it unblocks — quantified projection

**Battery wall time.** `plans/test-time-cut.md` §1 sampled 133 real spawns
totaling 712.0s (avg 5.35s/spawn), of which 55 (41%, 296.7s) are
session-scoped and — per that doc's own §6 item #3 sizing — structurally
uncacheable by the existing memo. §1.4/§2.4 above establish that this ~5.3s
average is dominated by the fixed GHC-boot-plus-stdlib-typecheck tax, not by
per-request content. A warm daemon removes that fixed tax for **every**
spawn, session-scoped or not — including the 41% slice the memo can never
help and the eval-lane slice (`jit_surface`: 0% session-scoped, 0%
duplicated — memo cannot help there either, per test-time-cut.md §2) that
today pays full GHC-boot cost on every one of its 45 genuinely-distinct
compiles. Combined with the memo (unaffected, still serving instant hits on
repeat fixed-template content — the measured 119s → 99s cold → 47s warm on
`golden_path + acceptance_askuser + selfharness_spine`,
`tidepool-harness/CLAUDE.md:117-128`), the daemon's marginal value sits
exactly where the memo structurally cannot reach: the session-scoped 41%
and the always-distinct eval-lane majority. A conservative projection —
collapsing the ~5.3s fixed tax to whatever the target module's own
typecheck/desugar/codegen costs (`plans/test-time-cut.md` §3's own finding
that an *additional* target in one spawn costs +110ms, i.e., single-digit
percent, suggests the non-fixed remainder per compile is small) — points at
the bulk of that 712.0s sampled wall time collapsing toward the low tens of
seconds for the same 133 spawns, once warm. This is a projection from
measured per-spawn averages, not a new measurement; §7 phase 1 is exactly
where it gets checked against reality before being relied on.

**Live round latency.** Per the operator, dogfood measures ~7.7-17s per
round attributable to extract. The same fixed tax applies here (a live
round's compile is not exempt from the GHC-boot cost every other invocation
pays). Expect this to collapse toward low-single-digit seconds once warm —
stated as a directional expectation from the same measured fixed-cost
share, not a number this design doc can certify without a real warm-daemon
spike (flagged in §7's migration order as the first thing to measure, not
assume).

**Memory ceiling.** Today, N coincident spawns cost N × ~600-800MB — the
operator's 12-coincident incident is ≈7.2-9.6GB transient, consistent with
"filled swap." A one-worker resident daemon (§5.4) collapses concurrent
extract memory to a single ~600-800MB resident footprint regardless of how
many callers queue behind it — this is a direct, mechanism-level fix for
the memory incident (not merely a speed side effect): the incident was
caused by concurrency fan-out, and a serialized daemon caps fan-out at one
by construction.

---

## 7. Migration: incremental adoption order

**Phase 0 — build, opt-in, no default-path change.** The daemon mode is a
new flag on the *same* `tidepool-extract-bin` entry point (`--daemon
--socket <path>`), not a second binary: the request-serving loop it needs
is `runBatchPipeline`'s existing `go` loop (`GhcPipeline.hs:711-730`)
generalized to read one request at a time off a socket instead of off a
pre-built `[BatchItem]`, over the same shared `ModIfaceCache`/`GutsMemo`
bootstrap (§2.3) instead of a batch's request-scoped one. `ExtractCmd`
grows the `Launcher::Daemon` path (§5.2) gated on
`$TIDEPOOL_EXTRACT_DAEMON_SOCKET` being set to a live, connectable socket.
With the env var unset — true for every existing caller, test, and CI job
— behavior is byte-identical to today. **First measurement here, before
anything else proceeds:** a real warm-daemon spike measuring actual
per-request latency and RSS growth over a realistic request sequence,
checking §6's projections against reality.

**Phase 1 — batteries.** `scripts/battery.sh`/`battery-shard.sh` start (or
connect to an already-running) daemon for the run's duration and set the
env var for the whole nextest invocation, tearing the daemon down after.
The compile memo's cache dir and the daemon are orthogonal — the memo
caches OUTPUT bytes keyed by invocation content, the daemon caches a LOADED
ENVIRONMENT — so both stay on together with no interaction to design; a
memo hit still short-circuits before any daemon round-trip happens (the
memo check lives in `tidepool-runtime`, above `ExtractCmd::run()` entirely).
This is also where the ~380s test-tier kill window (root `CLAUDE.md`'s Test
tiers section) interacts: a daemon crash/restart mid-battery must degrade
to per-invocation spawn fallback (§5.2/§5.5) rather than stalling the whole
run waiting on a dead daemon.

**Phase 2 — live harness/selfharness driver.** The highest-value target for
latency (§6), opted in once Phase 1 has soaked — matching the soak-gate
idiom this codebase already uses for comparable resident-state changes
(e.g. `plans/resident-session-kernel-design.md`'s Phase 6 gating on
production soak before a legacy path is retired). The selfharness driver
starts the daemon as part of its own boot (or connects to an
operator-started one — an ops decision, open question 4) and points its own
`ExtractCmd`-based calls at it via the same env var.

**Kill switch.** Unset `$TIDEPOOL_EXTRACT_DAEMON_SOCKET`, or simply stop the
daemon process. Every caller already falls back to direct spawn with zero
code-path change (§5.2/§5.5) — there is no separate "disable the daemon
feature" flag to design, because the daemon was never a hard dependency to
begin with.

---

## Decisions (operator, 2026-08-24)

1. **Worker count: 1.** GHC's single-mutable-session constraint plus the
   memory-incident motivation (§5.4). No config knob until Phase 0's
   warm-daemon spike shows queueing delay actually hurting.
2. **Build placement: `--daemon` flag on the existing
   `tidepool-extract-bin`.** One binary, one deploy path — the toolchain
   stamp already fingerprints exactly this binary, so the handshake covers
   the daemon with zero new mechanism. The serving loop itself lives in its
   own module, not in `Main.hs` (§7 phase 0 implementation note below).
3. **Rotation thresholds: provisional defaults, tuned by the spike.**
   Request-count rotation N=256 as primary, RSS ceiling 2048MB as backstop,
   both settable via daemon flags. Phase 0's measurement is the sizing
   authority; these are conservative starting values, not conclusions.
4a. **Deferred ("eventually", operator 2026-08-24): orchestrator-owned
   wave-shared daemons.** One daemon per agent wave, socket injected into
   children's env, owner = the orchestrator — would consolidate N
   per-context daemons and retire the slot semaphore entirely. Deferred
   because general daemon management in the orchestrator is too
   heavyweight for now; per-context ownership (below) stands.
4. **Lifecycle ownership: per-context, no shared singleton.** The
   selfharness driver spawns and owns a daemon for its own lifetime
   (Phase 2); each battery run starts its own daemon on a per-run socket
   and tears it down after (Phase 1). No systemd unit, no flock-guarded
   auto-spawn singleton — ownership, restart-on-crash, and redeploy-skew
   restart responsibility are always unambiguous because the owner is
   always the single process that created the daemon. Worst case under
   concurrent lanes (N runs × ~800MB, N bounded by the ghc-slots
   semaphore) is accepted; a box-global singleton is a possible later
   consolidation, only with soak evidence.
5. **Sequencing: Phase 0 fires now** (opt-in only, env var unset
   everywhere by default, near-zero overlap with in-flight lanes).
   Alongside it, `scripts/ghc-slots.sh` shrinks 6→2 slot files (box-wide
   ceiling 2×4=8 concurrent extracts) — the durable fix for the
   swap-fill incident, independent of the daemon landing.
6. **Wire framing: length-prefixed frames, not JSON** — a root-TL
   correction to §5.3. `tidepool-extract-cmd`'s charter is a std-only,
   zero-dependency leaf; a JSON wire would force either a hand-rolled JSON
   codec (an escaping bug farm) or a serde dependency (charter break).
   Both endpoints are in-repo, so the wire needs no interchange format:
   length-prefixed byte strings (u32 length + bytes, over the same
   one-connection-per-request UNIX socket) carry the argv array and the
   `{exit_code, stdout, stderr}` response with trivially-correct codecs on
   both sides. Everything else in §5.3 — UDS, one request/response per
   connection, EOF-as-crash-signal, paths-not-content — stands unchanged.

---

## §7. Implementation deviations (Phase 0, recorded per the doc-history rule)

Every point below is a place the shipped code diverges from this document's
literal wording, discovered while building against the real pipeline rather
than re-derivable from the design alone. Each is forced by a correctness
fact the design doc did not have in hand at write time.

1. **The shared `GutsMemo` isolation mechanism is a post-cycle SANITIZE, not
   a literal `mMemoRef = Nothing` per request** (§2.3's "The daemon calls
   `runCompileCycle` with `mMemoRef = Nothing` for the per-request portion
   of the compile"). Passing `Nothing` for a resident cycle disables BOTH
   reads and writes for EVERY module in that cycle's `runCompileCycle`
   call — including the already-warmed STDLIB entries the whole mechanism
   exists to serve, which would defeat the daemon's purpose entirely
   (confirmed empirically: request 2's `typecheck_ms` did not drop under
   the literal reading). The shipped mechanism instead passes `Just` a
   SHARED memo ref for every cycle, and strips exactly the cycle's own
   target-module name plus any `Tidepool.Session.*` name from it
   immediately after the cycle returns
   (`Tidepool.GhcPipeline.sanitizeMemo`). This upholds the actual invariant
   §2.2 states (one request's `__result`/`Val.G<g>` guts must never reach
   another request's compile of the same name) while keeping the read-side
   win the design's own performance case depends on. `parseSessionModule`
   (the existing session-module-name recognizer) is reused rather than a
   second hand-rolled prefix check.

2. **The `OptimizeCoreReachable` tier (`normalVariant` — every plain,
   non-session `--target` compile) is now ALSO memo-aware**, not left
   memo-blind as an earlier implementation draft assumed. The initial
   approach routed every resident request through `sessionVariant`
   (`OptimizeEveryModule`) unconditionally, on the reasoning that a
   non-reachable module's Core is "thrown away either way" so the tier
   choice couldn't affect wire output. That reasoning covers the FINAL
   emitted bindings but not `meta.cbor`: `writeClosedTargets`'s metadata
   merge walks `cmReachBinds`/DataCon usage over whichever Core a module
   actually got (raw desugared under `OptimizeCoreReachable`'s
   non-reachable branch vs. fully `core2core`'d under
   `OptimizeEveryModule`), and this lane's own integration test
   (`daemon_integration`'s check (a)) caught a real `meta.cbor` byte
   divergence between the two tiers on the SAME fixture before this was
   corrected. The fix: `residentCompileOne` selects the SAME variant a
   direct spawn of the same argv would (`runPipelineSession`'s own
   `isSessionScopeActive` gate, unchanged), and `runCompileCycle`'s
   `OptimizeCoreReachable` arm was extended to consult `mMemoRef` the same
   way `OptimizeEveryModule` always has — a memo hit reuses a module's
   cached front (for the reachability walk) and, if reachable this cycle,
   its cached `core2core`'d result; a miss compiles fresh and, if
   reachable, inserts into the memo. This is provably behavior-preserving
   for every PRE-EXISTING caller: every one of them passes `mMemoRef =
   Nothing`, under which the new code takes the exact same
   `compileFront`-then-`compileBack` path the old code always did (verified
   against `extract-fidelity-test`, `session-c-test`, and
   `varid-mechanism-test`, all still green). The new behavior — memo
   consultation — activates only when `mMemoRef = Just`, a combination only
   the resident daemon ever produces.

3. **`tidepool-extract-internal` (the Haskell library) gained the `network`
   Hackage dependency** for `DaemonServer`'s UNIX-domain-socket transport.
   `unix` (already in the with-packages GHC closure) does not expose socket
   syscalls; `network` is the standard, well-tested way to do this in
   Haskell and resolves the same way `QuickCheck`/`cborg`/`half` already do
   (pinned `index-state`, not the ambient nix GHC package DB) — this is an
   existing, established pattern in this codebase, not a new one.
   `tidepool-extract-cmd`'s std-only, zero-dependency charter (the crate
   this design's own §5.1/anti-patterns actually protect) is unaffected —
   the daemon CLIENT adds nothing.

4. **Rotation, the RSS ceiling, and the toolchain-stamp watch are checked
   once after EVERY served request, not on a separate background tick**
   (§4.3/§3 both describe a periodic timer, distinct from per-compile
   checks). Both checks are cheap file reads (`/proc/self/status`'s
   `VmRSS` line; a byte-compare against the boot-time stamp bytes) rather
   than the toolchain module's own blake3 fingerprint recompute the design
   was specifically avoiding making per-compile — so checking them
   per-request costs nothing observable and avoids a second thread/timer
   with its own shutdown-race surface. The tradeoff is a slightly less
   prompt reaction to an external change (bounded by "how long until the
   NEXT request arrives" rather than a fixed tick interval) — acceptable
   for Phase 0's opt-in, explicitly-started daemons; worth revisiting only
   if a production deployment shows requests arriving too infrequently for
   this bound to matter.

5. **Spike measurement (design §6/§7's own migration-order mandate — "first
   measurement here, before anything else proceeds").** ~30 requests
   (20 distinct one-shot compiles + 10 session bind/reference pairs)
   against one resident daemon: request 1 (first-ever, paying the stdlib's
   first typecheck) took 718ms; warm one-shot requests thereafter averaged
   ~196ms (min 166ms, max 286ms); warm session-reference requests averaged
   ~255ms. Daemon RSS grew from ~290MB (boot) to ~439MB after the first few
   distinct one-shot compiles, to ~540MB by request 10, then grew slowly
   and non-linearly to ~681MB by request 40 (roughly +375MB total over 40
   requests, clearly decelerating rather than unbounded — most growth
   happened touching NEW stdlib modules for the first time, not from
   request count alone). This is well inside a single spawn's already-
   measured ~600-800MB floor (§1), validating §6's core projection: the
   fixed ~5.3s GHC-boot-plus-stdlib-typecheck tax collapses to
   low-hundreds-of-milliseconds once warm, for both session-scoped and
   plain one-shot requests, at a bounded (not runaway) memory cost. Sizes
   the provisional rotation defaults (§ Decisions item 3, N=256 requests /
   2048MB ceiling) as conservative, not tight — 40 requests reached under
   35% of the RSS ceiling.
