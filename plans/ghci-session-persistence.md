# GHCi-style session persistence for tidepool eval

> **Recovered from a crashed session** (`61993207-558b-4701-806a-81e4734b8060.jsonl`,
> 2026-06-25, crashed ~20:27 UTC). Resume didn't work; this is the salvaged research.
> The web `/deep-research` workflow launched at 20:14 **never landed** (still "cooking"
> at the crash) — only the three `Explore` code-read agents returned and were synthesized.
> The one open question that report was meant to settle is flagged below.

## The goal

A custom harness driving a tidepool "ghci" session: `x <- longComputation` in one
eval call binds a value that **later, independently-compiled** eval calls can
slice / transform / examine. Full permissioning intact + useful utils. Leaning into
tidepool-as-a-model — the "hard half" (cross-call typed bindings), not the easy half.

The motivating worry that kicked off the research: *a spike showing `let x = [1,2,3]; sum x`
works across calls gives false confidence — Int might persist while Double, Text, ADTs,
closures, or lazy thunks each have distinct representation/identity hazards.* So:
understand the theory by type before experimenting.

## Headline: the per-type fear is mostly unfounded

Tidepool heap objects are **self-describing** — every object has an 8-byte header with a
structural tag at offset 0, and constructors carry their identity *in the object* as a
64-bit `DataConId` at offset 8. That `DataConId` is a **content-addressed hash of
`"module:conname"`** (`stableVarId`, `Translate.hs:1466`), not a per-compilation
sequential index. So a `(:)` built in call-1 has the *same* tag as a `(:)` interpreted
in call-2 — both hash `"GHC.List:(:)"`. This is a hand-rolled version of GHC's
info-table mechanism, and it's exactly why values can be portable across compilations.

The worry's specific example inverts: **Int and Double are byte-identical in
representation** — both are `Lit`-boxed (header + LitTag + 8 bytes), differing only in
the literal tag. If Int persists, Double/Char/Word persist by the identical mechanism.
There is **no "Int works, Double doesn't" fault line.** The real fault lines aren't
per-type at all.

## Reframe: the unit of persistence is the *machine*

The continuation/`ask` mechanism already persists a heap across a suspend/resume
boundary — by **keeping the same `JitEffectMachine` alive** (heap + code + GC roots all
live in the machine; the continuation is held MCP-side and the nursery is never torn
down). So "persist values across eval calls" is mechanically *"don't drop the machine
between calls."* That single move gives heap persistence, code persistence, and valid
closure/thunk code-pointers for free — all owned by the machine.

## Per-type matrix (assuming one long-lived machine)

| Value category | Persists? | Needs | Real risk |
|---|---|---|---|
| **Unboxed scalar** (Int, **Double**, Char, Word) | ✅ free | binding name | none — self-describing `Lit`, identical repr |
| **Nullary con** (Bool, `[]`, Nothing) | ✅ free | binding name | none — content-addressed tag |
| **Lists / tuples / Maybe / Either** (forced) | ✅ free | binding name | none structural |
| **Text** | ✅ free | binding name | none — `Con("Text")` over an in-heap ByteArray |
| **User ADT** (stable source) | ✅ free | binding name; constructor *names* stable | CASE-TRAP only if the type's constructor **names/module drift** between calls |
| **Function / closure / PAP** | ✅ *iff machine alive* | code retention (= machine alive) | stale code-ptr → SIGSEGV if machine dropped |
| **Pure lazy thunk** | ✅ *iff machine alive* | machine alive | same |
| **Lazy effect-result** (respond_list/stream) | ❌ today | machine alive **+ registry lifetime extended** | parked-stream registry cleared per-`run()`; cross-call force → registry-miss |

Three clean tiers:
- **Tier 0** — fully-forced first-order data: free once the machine persists, just needs naming.
- **Tier 1** — anything carrying a code-pointer (closures, thunks): free *if you keep the machine*.
- **Tier 2** — lazy effect-result values: the one genuinely separate piece of work, because
  `PARKED_STREAMS` is scoped to a single `run()` (`jit_machine.rs:117`, cleared in
  `RegistryGuard::drop`).

## The actual crux — smaller than first thought, and elsewhere

The DataConTable (first called "the hard part") is **already content-addressed and
stable**; it's only consulted for *rendering* names, not for structural case-dispatch
(dispatch reads the embedded tag). The real, load-bearing work, ranked:

1. **Binding-name stability + a session binding table — the genuine blocker.**
   Local bindings get `localVarId = hash(occName + GHC-unique)`, and the unique is
   *session-local*, so call-2 can't name call-1's `x`; it hits `unresolved_var_trap`.
   Need a stable naming scheme for session bindings + a `VarId → heap-root` table the
   JIT consults *before* trapping (and registered as GC roots). **This is the heart of it.**
2. **Machine retention across calls** — cheap lifecycle plumbing; the `ask` path already
   proves the pattern (don't drop the machine).
3. **Registry lifetime for lazy effect values (Tier 2)** — the one hard, isolated
   extension. Everything else works without it. (See dodge below.)
4. **A usage constraint, not a gap:** redefining a user ADT's constructor *names/module*
   between calls invalidates persisted values of that type (tag mismatch → CASE TRAP).
   Adding/removing/**reordering** constructors *should* be fine since tags are name-hashed,
   not positional — **but one code-read agent's example assumed positional tags. This is
   the single thing to nail down before relying on it** (and what the dead web-theory
   report — GHC info-tables, GHCi `InteractiveContext`, cross-unit identity — was going
   to cross-check).

Net: research made the scope *smaller and more concrete*. The bet is **"keep one machine
alive + add a stable binding table"**; first-order forced data (incl. Double) rides along
for free; closures need only the machine; lazy effect-results are the lone separate hard
piece.

## Tier 2 in detail (registry / lazy effect-results)

The registry bridges a Rust-side producer and a lazily-materialized Haskell list. When an
effect returns a lazy list, the spine isn't fully built — each unforced tail is a
`stream_chunk`/`stream_element` thunk capturing `(id, offset)`; forcing it does
`PARKED_STREAMS.get(id).source.get(offset)` to materialize the next element. The *data*
(`Vec<Value>` or live iterator) lives in `PARKED_STREAMS[id]`; the thunk holds only the id.

The cross-call break, precisely: `x <- lazyEffect` in call-1 leaves `x`'s tail as a thunk
holding id `N`; `take 3 x` in call-2 forces it → `PARKED_STREAMS.get(N)` → **miss**,
because the registry was cleared at call-1's `RegistryGuard::drop` (`jit_machine.rs:117`,
`clear_parked_streams`).

**Key realization: it's already thread-local, and indexed entries already "live to
teardown" — just the wrong teardown.** Today "teardown" = the `run()` call. The registry
isn't torn down because it's hard to keep alive; it's torn down because `RegistryGuard` is
scoped to one run. If you keep one machine alive across calls on one thread (which Tier 1
needs anyway), the minimal fix is: **stop clearing indexed entries in
`RegistryGuard::drop`; clear them at machine drop instead.** The thread-local then
persists naturally. `STREAM_NEXT_ID` is monotonic, so ids stay unique across runs. A
lifetime re-binding, not new machinery.

What stays hard, honestly:
1. **`respond_stream` (live iterator) vs `respond_list` (pure Vec).** A `ReadySource` is a
   pre-converted `Vec<Value>` with Arc-shared Text — pure, GC-inert, trivially safe to
   park across calls. An `IterSource` is a *live producer* that may hold call-scoped /
   handler-scoped state and likely isn't `Send`; persisting it is semantically dubious.
   Clean rule: **indexed/list sources persist; un-exhausted sequential streams either
   don't, or get drained to a `Vec` at the call boundary.**
2. **Memory.** A session-scoped registry leaks: every `x <- lazyEffect` holds its producer
   alive until session end, even after `x` is dead. Proper fix ties registry-entry
   lifetime to GC reachability of the referencing thunks — the genuinely fiddly bit. Cheap
   version just leaks for the session.
3. **Threading.** Thread-local works iff the machine is pinned to one thread per session.
   Migration (thread pool) needs a `Send + Sync` shared registry, which fights non-`Send`
   live producers. Pin one machine to one thread per session to sidestep.

**The clean dodge that deletes the whole problem — strict session bindings.** If `x <- action`
forces `x` to (deep) normal form *before* binding, then `x` is Tier-0 forced first-order
data: no thunk, no code-pointer, no registry id. Persists trivially; skips Tier 1 *and*
Tier 2. Only cost: can't bind an un-forced/infinite structure and `take 3` it later. For a
data-wrangling REPL (`x <- bigQuery`, then slice/examine across calls) strict bindings are
exactly what you want — you bound `x` to look at it, you want it computed. GHCi keeps
bindings lazy; a custom REPL can choose strict and dodge the hardest tier.

## Web deep-research findings (2026-06-25, re-run after the crash, adversarially verified)

The web report that died in the crash was re-run (5 angles, 20 sources, 77 claims → 25
verified, 21 confirmed / 4 killed). It **settles the load-bearing question** and, on the
key point, *inverts which scheme is at risk*:

- **GHC's native constructor tags are POSITIONAL** — assigned sequentially by declaration
  order from `fIRST_TAG` and stored in the info table, *not* content/name-derived
  (`mkDataCon`: `zip (tyConDataCons tc) [fIRST_TAG..]`, no hashing — verified GHC 8.2–9.4).
  So under *GHC's* scheme, reordering/adding/removing constructors between two compilations
  silently changes tags and a previously-built value case-matches wrong. *(confidence: high.
  Source: Marlow ptr-tagging paper; GHC.Core.DataCon)*
  → **The agent who "assumed positional tags" was describing GHC, not tidepool.** Tidepool's
  `hash("module:conname")` (`stableVarId`, `Translate.hs:1466`) is exactly the fix for this
  GHC gap: reorder-safe because tags are name-derived, not positional. The research framed
  tidepool's scheme as *strictly safer than GHC's* on this axis. **So #4 in the matrix is
  resolved in tidepool's favor** — pending the one empirical check below.
- **GHC heap objects are self-describing** via a header info-pointer to both entry code and
  the (backwards-laid) info table (object type + GC layout + tag). Consuming *any* boxed
  value needs the producer's info table + code pointers valid; unload/recompile the producer
  → stale pointers → corruption/segfault. *(high)* Confirms the "machine must stay alive"
  reframe — the machine is what keeps those pointers valid.
- **GHCi never carries values across a recompilation boundary.** Cross-statement survival is
  pure *within-session* linker bookkeeping: `closure_env` (Name → HValue) + `itbl_env`
  (DataCon → info-table addr), both rebuilt on link and **explicitly discarded on
  `:load`/`:reload`/`:add`/`:unadd`**. Prompt bindings (`let`, `x <- action`) survive a
  `:module` scope change but are *lost* on reload. *(high)* → tidepool's "one long-lived
  machine + session binding table" is the correct analog to GHCi's within-session mechanism;
  there is no GHCi magic to copy for *across-recompilation* survival — nobody does it.
- **GHCi redefinition shadows, not overwrites** (old `TyThing`/DataCons persist under a
  distinct `GhciN.T` name; prior values stay valid). *(high)* Validates the planned
  shadowing/overlapping-names-across-turns goal as sound and precedented.
- **Closures/thunks/PAPs embed code/info pointers** (`FunClosure`/`ThunkClosure`/
  `PAPClosure`/`APClosure`, each with an `StgInfoTable`) → fundamentally hazardous as raw
  bytes if the producer is unloaded. *(high)* Confirms Tier 1 = "free *iff machine alive*."
- **deepseq / normal-form forcing** is corroborated as the standard technique to drop
  code-pointer/thunk hazards — i.e. the **strict-bindings dodge is the principled move**, not
  a hack. *(but see caveat)*

**Caveats on the research itself:**
- Angle 5 (Smalltalk/Lisp image persistence, fasl, strict-vs-lazy for cross-session
  survival) produced **zero surviving verified claims** — the prior-art lessons are
  *synthesized inference from the GHC findings, not independently verified* against
  Smalltalk/Lisp primary sources. Re-research if you want to lean on image-system precedent.
- Much GHCi linker evidence is cited from GHC 7.4–7.10 sources; the mechanism survives into
  9.12+ but field names changed (`GHC.Linker.Types`/`LinkerEnv`, `HValue → ForeignHValue`).
  Verify against current source before relying on API specifics.
- 4 claims were *killed* in verification, two of them name-derived-identity claims about GHC
  — reinforcing that **GHC is positional at runtime**; the name/content-addressing is
  tidepool's own addition, not something GHC gives you.

## Eval lifecycle today + what the build actually requires (code-read 2026-06-25)

Current eval is **fully one-shot** — confirmed in source:
- Fresh GHC session **per eval call**, no `InteractiveContext`/`runStmt`; the whole module
  (preamble + `__user = <code>` + `result` wrapper) is regenerated and recompiled each call
  (`eval_prep.rs:139` `template_haskell`; `GhcPipeline.hs:40` `runGhc`).
- `JitEffectMachine` created + run + dropped per call (`tidepool-runtime/src/lib.rs:217-218`);
  it lives on the eval thread's stack and dies when the thread exits (`server.rs:535-567`).
- A cross-call reference to a prior `x` fails **at GHC compile time** ("not in scope") —
  *before* `unresolved_var_trap` is ever reached. The trap (`emit/expr.rs:368-410`) only fires
  for in-scope-but-unfoldable symbols. So GHC visibility is the *first* gate, not the JIT.

**Correction to the recovered synthesis:** it claimed "the `ask` path already proves the
machine-retention pattern." It does **not**, for our case. Ask/resume keeps a machine alive
only by **parking the eval thread mid-computation on an mpsc channel** (`ask.rs:223-262`;
`EvalSession{ thread: JoinHandle, response_tx, .. }` stored in
`server.continuations: HashMap<String, EvalSession>`, `server.rs:74,334`). On resume it
re-enters the **same frozen code** — it never JITs *new* code against the *existing* heap.
Re-entry-with-newly-compiled-code is the actually-novel move and is **unbuilt**.

**Three coupled pieces the build needs (none of which exist today):**
- **(A) GHC visibility + type of each binding** so a later call's source typechecks the
  reference to `x` — a persistent `InteractiveContext` (GHCi-faithful) *or* injecting a
  synthetic `x :: T` decl into each generated module. **This is the first gate.**
- **(B) stable VarId** for a session binding, shared across the binding call and using call
  (today `localVarId` mixes in a session-local GHC unique, so it can't be re-referenced).
- **(C) live heap + GC root + retained machine** that new code is JITed into — the part the
  ask/resume path does *not* cover.

Session scoping today = server-process lifetime; KV (`.tidepool/kv.json`) is the one thing
that already persists across calls. No explicit `Session` struct.

## Two build paths (the real fork)

**Path 1 — Serialize-to-KV (cheap, modest).** Strict-force `x` at bind time, serialize it
(JSON/CBOR) into the already-persistent KV store; a later call restores it via typed
`fromJSON`/`parseJson` (the eval boundary already works this way for `input`). **Sidesteps
A, B, and C entirely** — no machine retention, no GC roots, no `InteractiveContext`. Works
today-ish for any serializable first-order value. *Limits:* needs `ToJSON`/`FromJSON`,
re-parses each call, loses some type fidelity (Double vs Int, custom ADTs need instances),
and can't persist closures / lazy-infinite structures (which strict bindings already
exclude). Honest downside for the writeup: it's "typed value-caching between calls," largely
expressible with KV that exists — impressive-modest, not novel-systems.

**Path 2 — Live-heap persistence (big, impressive). ← RECOMMENDED.** Keep one
`JitEffectMachine` alive per session, JIT each new fragment into it, resolve session bindings
via a `VarId → heap-root` table registered as GC roots, and make GHC typecheck references via
persistent `InteractiveContext` (or synthetic decls). **Preserves arbitrary typed values
exactly** (custom ADTs, no instances written) across separately-compiled fragments — the
genuinely novel result: *"a custom Cranelift JIT with GHCi-style typed value persistence
across independently-compiled fragments, safe because constructor tags are content-addressed,
not positional."* Costs A + B + C.

## Feasibility of Path 2 (code-read 2026-06-25): CONFIRMED — (B) moderate, ~1–2 weeks, no redesign

The re-entry move ("keep machine alive → JIT a 2nd fragment into it → run it referencing
1st-run heap pointers") is **already 70% supported**; the rest is API exposure, not
architecture. What already works:
- **Machine survives runs** (`run(&mut self, …)` does not consume). **CORRECTION (round-3 review,
  2026-06-25):** the heap does NOT stay put by default — after the first GC the live heap migrates
  off the `Nursery` field into `GcState::active_buffer`, a thread-local (`host_fns.rs:623`), and
  `RegistryGuard::drop`→`clear_gc_state` *frees it every run* (`host_fns.rs:186-191`). So "pointers
  stay valid until machine drop" holds **only before the first collection**. Making `active_buffer`
  machine-owned + splitting `clear_gc_state` (per-run vs machine-drop) is component **E′** in the
  orchestration plan — load-bearing, not free. The pre-GC happy path is what made this look "70%
  there"; the honest figure is lower until E′ lands.
- **External GC-root registry exists** — `register_rust_root(slot: *mut *mut u8)`
  (`host_fns.rs:104`) registers a Rust-side pointer slot; the copying GC **updates it in place
  on compaction** (`cheney_copy`, `host_fns.rs:695`). This is the piece most likely to have
  been missing, and it is built.
- **VarId → constant-heap-pointer resolution exists** — the lazy-poison path bakes a pointer
  as an `iconst` in IR (`emit/expr.rs:368-410`). A general version of this is exactly the
  binding-resolution mechanism.
- **Cranelift `JITModule` supports multi-round `declare`/`define`/`finalize`** — capability
  present, not yet exposed on the machine.

The four blockers (all plumbing):
1. **No re-entrant compile API** — module is finalized after the first `compile` and never
   re-opened (`jit_machine.rs:154-166`). Add `JitEffectMachine::add_function(name, expr,
   external_env) -> FuncId` + `run_fragment(func_id, …)`.
2. **Env-seeding not exposed** — `compile_expr` builds a fresh empty `EmitContext` internally
   (`emit/expr.rs:1638`). Thread an `external_env: &ScopedEnv` through so a fragment can
   resolve a session VarId to a seeded heap pointer.
3. **Generalize the constant-pointer path** — add `SsaVal::from_external_pointer(*const u8)`
   reusing the poison-ptr template.
4. **`RegistryGuard::drop` wipes roots per run** (`jit_machine.rs:117`) — the *same*
   teardown-scope issue the synthesis flagged for `PARKED_STREAMS`. Move root/registry clear
   to **machine drop**, not run drop; add `register_persistent_root`. One fix serves both
   bindings and lazy-effect-results.

**Design decision the agent under-weighted (settle in Phase 1):** the alloc cursor resets to
`nursery.start()` each run (fresh `VMContext`), so run-2 would allocate *over* run-1's value
unless it's been moved. Two options: **(a) persistent bump-cursor** — don't reset; keep
allocating from run-1's high-water mark, GC-compact when full (updating registered roots); or
**(b) force a compaction at bind time** — registering the binding as a root then GC'ing
relocates it to to-space, after which run-2 can safely reset+bump. (a) is cleaner for a
long-lived session; (b) reuses existing GC triggers. Prototype (a) first.

## Implementation plan (Path 2)

**Phase 0 — codegen API exposure (~1 wk). Pure Rust, no MCP/GHC; gated by a unit test.**
Deliver the four blockers above as machine methods, and a `tidepool-codegen` test that
encodes the novel move end-to-end:
1. `add_function` / `run_fragment` on `JitEffectMachine` (re-open + re-finalize the module).
2. `external_env` threaded through `compile_expr` → `EmitContext`.
3. `SsaVal::from_external_pointer` + general VarId→seeded-pointer resolution in `emit/expr.rs`.
4. `register_persistent_root` + move `RegistryGuard` clears to machine drop; choose heap-cursor
   option (a)/(b).
- **Test (the proof):** hand-build Core for fragment-1 `Con Foo [Lit 42]`; run; take the
  returned heap ptr; `register_persistent_root`; seed env `{VarId(x) → ptr}`; `add_function`
  fragment-2 `case x of Foo n -> n`; `run_fragment`; assert `== 42`. Then add a variant that
  forces a GC between the two runs and asserts the root was relocated correctly. **Green here =
  the impressive version is real.**

**Phase 1 — session-scoped machine in the MCP server (~3 days).**
- New `Session { machine: JitEffectMachine, bindings: BindingTable, ghc_ic: …, created_at }`.
- Hold sessions in `server.rs` alongside `continuations` (`Arc<Mutex<HashMap<SessionId,
  Session>>>`). For v1, one implicit session = server process (matches today's KV scoping); add
  an explicit id later.
- The eval thread no longer drops its machine; on a session eval it borrows the session's
  machine, `add_function` + `run_fragment` instead of `compile`+`run`. (Keep the one-shot path
  for non-session evals.)
- **Pin the session machine to one thread** (the GC roots + nursery are thread-local) — a
  dedicated session worker thread, or a thread-affinity guard.

**Phase 2 — GHC typing of cross-call references (gate A) (~3–5 days, the fiddly one).**
The first gate is GHC scope/typecheck: a later call's `x` must be in scope *with its type*.
Two sub-options, prototype the cheaper first:
- **2a (cheap) — synthetic decls:** after binding `x :: T`, record `(name, rendered-type)`;
  `template_haskell` injects `x :: T; x = <opaque>` (a foreign-import-style/`undefined`-typed
  placeholder that typechecks but whose *value* is supplied by the JIT via the seeded env, not
  by GHC). Risk: getting GHC to accept a decl whose runtime value comes from elsewhere; may
  need the value's type rendered faithfully (reuse the `Value`→type machinery / `:t`-style
  render). Where types get hairy (higher-rank, constraints) this leaks.
- **2b (faithful) — persistent `InteractiveContext`:** make the extract GHC session persistent
  (today fresh per call, `GhcPipeline.hs:40`) and use `ic_tythings` + `runStmt`-style binding,
  the way GHCi does (research-confirmed mechanism). Bigger change to the extractor, but it is
  the *correct* model and dovetails with `:t`/`:i` later. Recommended end state; 2a is the MVP.

**Phase 3 — binding surface + naming + strict force (~3 days).**
- **Stable session VarId (gate B):** assign session bindings `stableVarId("session:<name>")`
  (the `0xFE`-tagged content-addressed scheme already used for externals) so the binding call
  and using call agree on the id — *not* `localVarId` (session-local GHC unique, unshareable).
- **Surface syntax:** support `x <- action` and `let x = expr` at the eval top level as
  *binding* forms (vs today's single-expression `__user`). On bind: **strict-force to NF**
  (the research-confirmed hazard-drop — drops Tier 1/2), store ptr in `BindingTable`, register
  as persistent root, record `(name, VarId, type)`.
- Reference resolution: using call's free `x` → session VarId → seeded env ptr (Phase 0 #2/#3).

**Phase 4 — polish / optional.**
- GC-reachability-tied root cleanup (drop a root when no live binding references it) — replaces
  the v1 "leak for session." Genuinely fiddly; defer.
- `:t` (type) and `:i` (info) verbs — easy once 2b's `InteractiveContext` exists.
- Lazy/closure persistence (Tiers 1–2): only if a use case demands binding un-forced/infinite
  values; strict-force makes it unnecessary for the data-wrangling REPL.
- Regression guard: the reorder-a-constructor-between-calls test (confirms content-addressed
  dispatch end-to-end through the real surface).

**Critical path:** Phase 0 (proof) → Phase 1 (machine survives in server) → Phase 2a (GHC
accepts the reference) → Phase 3 (real `x <-` syntax). Phases 0–3 = the working MVP:
`x <- bigComputation` then slice/transform/examine `x` across turns, for any strict-forced
typed value including custom ADTs. Estimate ~2.5–3.5 weeks for the MVP.

## Deep research round 2 (2026-06-25): what "doing it right" requires

Five read-only code-reads (GC, GHC-typing, identity/shadowing, concurrency, stack-safety/durability).
Verdicts per thread, then the reframes they force.

| Thread | Verdict |
|---|---|
| **GC for long-lived heaps** | Single-generation Cheney semispace; **re-copies ALL roots every GC** (O(total bound data)/cycle); heap auto-grows ×2 to 1 GiB (`TIDEPOOL_MAX_HEAP`). Fine for realistic REPL sizes (thousands of bindings); perf cliff at huge accumulation. Old-gen/pinning is a v2 optimization, not MVP-blocking. (`host_fns.rs:535-657`, `tidepool-heap/src/gc/raw.rs`) |
| **GHC type capture** | Precise hook: after `typecheckModule` (`GhcPipeline.hs:119`), read `idType` of the `__user` binding, render via `ppr`+`renderWithContext` → a re-injectable type string. Repr strips types, so capture **must** happen at this GHC stage. Pipeline is pure batch — no `InteractiveContext`/`runStmt` today. |
| **Identity / shadowing** | `stableVarId("session:x")` **collides on rebind** → clobbers root → old refs CASE-TRAP. Fix: generation counter `session:<gen>:x`. Collision guard `insert_checked` is load-bearing (`datacon_table.rs:73`). |
| **Concurrency / cache / ask-resume** | 4-way semaphore, spawn-per-eval, **all GC/root/stream state is thread-local** → a session machine **must pin to one thread**. Cache key = `blake3(source+target+includes+binary)` — **no session state** → session evals need session-keyed cache or bypass. **Sessions ≈ the existing ask/resume parked-thread continuation.** |
| **Stack-safety / durability** | `deep_force`, `Value::drop`, GC copy, GC rewrite are **all already iterative/stack-safe** — strict-forcing big first-order values is safe. No Value-serialization codec exists → **durable (restart-surviving) bindings are a separate ~1–1.5k-LOC feature**; live-heap-only for MVP. |

### Reframes (these change the plan)

**R1 — Sessions ARE multi-window continuations; build on ask/resume, don't parallel it.**
The ask/resume path already pins a thread, persists thread-local GC/heap state across a
suspend, and manages a continuation table (`EvalSession` in `server.rs:74,334`). The *only*
missing capability is the narrow one from Phase 0: **let a parked session thread accept a NEW
fragment, JIT it into its live machine, and run it against the existing heap.** So Phase 1
becomes "extend `EvalSession`," not "build a new session subsystem." Simplification.

**R2 — The unit of persistence is the DECLARATION ENVIRONMENT, not just values.**
To `case x` on a `Foo` in turn 3, `Foo`'s *definition* must be in scope in turn 3 without
redeclaring it. So a session must accumulate **types + functions + type-sigs**, not only value
bindings. This splits session state into two kinds with different mechanisms:
- **Declarations** (`data`/`type`/`class`/function defs / sigs) = **source text** → accumulate
  in a persistent **session library module** that's on the include path and recompiled each
  turn. This **reuses the existing include-dir + cache-fingerprint machinery** (a fingerprinted
  include dir is already cache-safe) — cheap, no new runtime mechanism.
- **Values** (`x <- bigComputation`) = runtime heap objects, **not** re-expressible as source →
  need the live-heap binding table + VarId-override + captured-type machinery.

This is a *cleaner decomposition than the original 4-phase plan*: the easy half (declarations)
is text accumulation; only the hard half (values) needs live-heap surgery. And it makes the
synthetic-decl typing approach (2a) actually sufficient for values, because the *types those
values inhabit* are declared in the session library — so `x :: Foo` typechecks.

**R3 — Naming has two layers with OPPOSITE policies (corrects sub-agent #3).**
The identity agent recommended turn-qualified module names (`Expr_N`) for safety — but that
**breaks the core use case**: it would give turn-1 `Foo` and turn-3 `Foo` different
`DataConId`s (`hash("Expr_1:A")` ≠ `hash("Expr_3:A")`), so a value bound in turn 1 could never
be `case`-matched by turn-3 code. Cross-check against the dispatch code-read: dispatch keys on
the content-addressed `DataConId` (offset 8), **not** the positional `tag: u32` (the agent
conflated the two), so a *stable* module name is exactly what makes old values interoperate
with new code. Correct policy:
- **Type/decl namespace = STABLE** (e.g. one `Session` module) → `DataConId`s stable → old
  values usable by new code. Redefining a type's *shape* (arity/fields) is then a hazard
  (collision guard fires / GHCi-style "old values unusable after type redef" — accept it).
- **Value-binding namespace = generation-counted** (`session:<gen>:x`) → rebinding `x` shadows
  without clobbering captured references.

**R4 — Confirmed: the alloc cursor resets per run, must change.** `make_vmctx` sets `alloc` to
`buffer.as_mut_ptr()` (`nursery.rs`) every run; the two GC/re-entry reads only *appear* to
disagree because one read the literal code and one reasoned about intent. Literal code resets →
a persistent session needs a persistent bump-cursor or compact-and-continue (design decision
from earlier, now confirmed real).

**R5 — Cache must be session-aware.** Extend the key with session id (or bypass for session
evals) — else a session eval returns another session's cached result.

### Refined architecture ("doing it right")

```
Session (= a pinned worker thread, extending EvalSession/continuations)
 ├─ live JitEffectMachine  (heap persists; alloc cursor made persistent; one thread)
 ├─ Declaration env  → session library .hs (types/fns/sigs as TEXT; include-path; cache-fingerprinted)
 │     stable module name → DataConIds stable → old values interoperate
 └─ Value binding table  → name → (gen-namespaced VarId, heap-root[GC-registered], captured GHC type)
       x <- action: strict-force to NF (stack-safe) → store root → record type
       later ref to x: synthetic `x :: <captured type>` decl (OPAQUE/NOINLINE) typechecks;
                       JIT overrides the VarId with the real heap root (seeded env)
 Cache: key += session id.   Durability: out of scope (live-heap only; CBOR ground-values later).
```

### Open tensions / decisions this surfaced

- **2a vs 2b, reconsidered:** with R2, the cheap path is *2a-for-values + a text session-library
  for declarations* — 2b's full persistent `InteractiveContext` becomes optional polish (gives
  `:t`/`:i` and avoids synthetic decls). Recommend the hybrid; defer 2b.
- **Type-redefinition semantics:** adopt GHCi's rule (redefining a type shadows; old values of
  the old type become unusable by new code). Don't fight it.
- **GC scaling:** accept O(N)-per-GC for the MVP; revisit old-gen/pinning only if real sessions
  get huge. Note it as a known ceiling, don't silently ship it.
- **Strict-only bindings:** confirmed the right call — deep paths are stack-safe for forced
  first-order data; closures deliberately excluded (can't deep-force a closure = the Tier-1
  boundary, consistent).

## Deep research round 3 (2026-06-25): code-grounded + web prior art → principled refinements

11-agent workflow (5 code+web research → 5 adversarial verifiers → synthesis); 22 refinements
survived. Two load-bearing external claims verified verbatim against GHC docs (`OPAQUE`
semantics; `ic_mod_index`). The three headlines:

**H1 — The namespace is THREE-way, not two-way; tidepool must keep old type decls harder than
GHCi.** Round-2's R2/R3 split (decls=text, values=heap) is right but flattens an asymmetry GHCi
encodes explicitly: *"Ids are easily removed when shadowed, but Classes and TyCons are not"*
(`GHC.Runtime.Context`, verified). For tidepool the reason is *sharper* — a turn-1 heap value
bakes `DataConId = hash("Session:Foo")` at offset 8 (`Translate.hs:1466`; dispatch reads it
`case.rs:193`), so dropping the old `Foo` decl strands every persisted value into a
render-miss/CASE-TRAP. **This OVERTURNS the round-2 open-tension "redefining a type makes old
values unusable, accept it":** with gen-suffixed type names that are never deleted, old values
stay fully dispatchable. Three policies, by entity kind:
- **functions/value-Ids** → append-and-shadow (latest text wins; already how `__user` works).
- **type/class/instance decls** → append-only, never textually removed; a reshape mints
  `Foo__g3` while `Foo__g1` stays live.
- **value heap-roots** → generation-counted (as planned).

**H2 — One session generation index `g`, exactly GHCi's `ic_mod_index`.** Round-2 kept "stable
type module" and "`session:<gen>:x` value VarIds" as two unrelated counters. GHCi unifies both
with a single monotonic index *"incremented whenever we add something to ic_tythings"*
(verified). Collapsing to one `g` (a) gives a single integer for the cache key (cleanly resolves
R5 — `cache.rs:16` hashes no session field today), and (b) makes type-rebind and value-rebind
structurally identical instead of two ad-hoc schemes.

**H3 — `OPAQUE`, not `NOINLINE`, for the synthetic value placeholder — a correctness fix.** GHC
docs (verbatim): an `OPAQUE` function *"is left untouched by the Worker/Wrapper transformation …
every call … remains a call of that named function, not some name-mangled variant."* `NOINLINE`
gives none of that. Gate B needs the binding's identity to survive to the JIT as a *stable*
VarId; a `NOINLINE` placeholder could be w/w-split into a `$wx` worker the seeded-env override
would miss. Upgrades the round-2 "OPAQUE/NOINLINE placeholder" (under-specified) to justified.

### The strongest signal: triangulation

**H1 (GHCi's Ids-vs-TyCons asymmetry), Julia world-age (per-call generation snapshot), and Unison
(coexisting content-hashed types) are three INDEPENDENT prior-art derivations of the same
mechanism:** gen/content-suffix the type name, keep all generations, let distinct `DataConId`s
give a clean *no-match* instead of invalidation. When GHCi, Julia, and Unison agree from three
different traditions, that's the part to build with confidence.

### Refinements by area (condensed; full report in workflow `wpovcaaia` output)

**Declaration env (Cling PTU model) — OVERTURNS the drift toward 2b.** Keep the GHC pipeline
fully batch; regenerate the *entire* session module from the ordered decl list each turn ("the
session module is a pure function of the decl list"). A turn that fails to typecheck just isn't
appended (the decl list is the recovery unit). clang-repl split Cling's one growing TU into
per-decl Partial Translation Units precisely because in-place mutation "still has rough edges";
tidepool's batch pipeline + content-fingerprinted include dirs (`cache.rs:24`,
`GhcPipeline.hs:38`) is already that replay model. **Text-accumulation is not a stopgap — it's
the more robust model; defer 2b's persistent `InteractiveContext` indefinitely.** Strongest "do
less" finding. (Existing `.tidepool/lib/Library.hs` re-export facade is the exact shape.)

**GC — old-gen tenuring is NEARLY FREE; pull it earlier than the round-2 "v2".** Split `GcState`
into nursery (gen-0) + append-only `old_space` (gen-1). At bind, evacuate the strict-forced value
once into `old_space`; per-fragment GC collects only the nursery. Why it's almost free:
`cheney_copy` only evacuates pointers inside `[from_start,from_end)` via `is_in_range`
(`raw.rs:136,154`) — make from-space the nursery alone and the whole accumulated binding set is
skipped automatically → O(total-live)/cycle becomes O(nursery-live)/cycle, killing the perf
cliff. **No write barrier needed:** bindings are strict-forced to NF and tidepool heap objects are
immutable post-construction, so no old→new pointer can arise (OCaml's "immutable ⇒ no barrier"
result; GHC gen-0/gen-1 is the direct model). `old_space` = growable bump region, compacted only
when a binding generation dies. The in-place root update this needs is already built
(`register_rust_root` + GC updates the slot, `raw.rs:139`).

**GC precondition — `PERSISTENT_ROOTS` (confirms blocker #4).** `RegistryGuard::drop` →
`clear_gc_state` → `clear_rust_roots` (`host_fns.rs:186`) wipes the root registry *every run*, so
a tenured root vanishes when its fragment returns. Add a session-scoped `PERSISTENT_ROOTS`
thread-local (parallel to `RUST_ROOTS`) cleared only at machine drop. Same teardown-scope fix the
round-2 reframe found shared with lazy-effect streams — one fix serves both. The existing
`rust_roots_mark`/`truncate_rust_roots` proves scoped root lifetimes are already supported.

**Identity (Unison) — two-layer `name → valueId → heap-root`.** Mint a fresh `valueId` per bind
(never reused), tag `0xFD` in the high byte (parallel to externals' `0xFE`, sentinel `0x45`);
rebind only repoints the mutable `name → valueId` map, so old roots stay reachable from captures
and the `insert_checked` collision guard is structurally never tripped. Var resolution already
dispatches purely on the high byte (`emit/expr.rs:368`), so `0xFD` slots into the existing switch.
*Honest non-transfer:* tidepool valueIds are counter-minted **provenance** ids, not content
hashes of the value (content-hashing a 100 MB forced value per bind is absurd) — so Unison's
"perfect cross-session cache" applies to the **decl text only**, not values.

**Continuations — `SessionKind::Session` (sharpens R1).** Add a third `SessionKind` variant
(alongside `AwaitingAnswer`/`Paused`, `ask.rs:42`) whose parked loop blocks at an *outer* prompt
receiving new fragments to JIT, with `AskDispatcher` nesting unchanged inside (Racket's
native-vs-serializable two-state split; Multicore OCaml multi-shot-outer / one-shot-inner). The
session prompt is multi-shot, the Ask prompt one-shot. `EvalSession` already holds the
`JoinHandle` + channels + gate — it *is* the continuation record. Removes round-2's residual "two
tables" framing. **Session affinity is a checked invariant** (all GC/root/stream state is
thread-local → a fragment on a non-owner thread is a correctness bug, not a deployment note);
owns a *resident* worker thread, not spawn-per-eval.

**Bindings are capability-passed state (resolves 2a-vs-2b on principle).** GHC sees an opaque
synthetic decl so the *reference* typechecks; the JIT overrides VarId resolution with the real
heap root via the seeded `external_env`. Type plane (GHC owns names+types) and value plane
(machine owns bytes) are deliberately decoupled — `EffectContext` already passes `user: &U`
capability state into every handler (`dispatch.rs:149`). Since repr strips types and `Translate`
hashes names, **GHC never needs the value**, so 2b is only ever for `:t`/`:i` ergonomics.

### The "empirical gate" — RESOLVED by code-read (2026-06-25); it's a non-blocker

The world-age story worried that a `case` over a value whose `DataConId` matches no alt would
`trap user2` → SIGILL. **Code-read refutes this — the path is already graceful.** A `Con` whose
`DataConId` matches no data-alt (no default) does NOT hit a bare Cranelift trap; it calls the
host fn `runtime_case_trap` (`case.rs:380→396`, jumps to merge with its return), which prints
`[CASE TRAP]` diagnostics, sets `RuntimeError::CaseTrap` (`host_fns.rs:3067`), and returns
`error_poison_ptr()`. The flag is detected when `with_signal_protection` returns → a clean
`CallToolResult::error`. Process + MCP connection survive. The only bare `trap`s in codegen are
in `primop.rs:42,495` (primop guards), unrelated to constructor dispatch — so the codegen
CLAUDE.md "SIGILL = case trap" note is about those sites, not this one.

What remains (both minor):
- **Compile-time vs runtime error (UX polish, not safety):** ideally gen-suffixing makes GHC
  treat `Foo__g1` ≠ `Foo__g3` as distinct types, so slicing an old value with redefined-type
  code is a *compile* error, not a runtime `CaseTrap`. Either outcome is graceful; confirm with a
  ~10-line check, not a gate.
- **Silent wrong-match via hash collision** is a *different* concern (tension #2), governed by
  `insert_checked` + the 56-bit hash space, and avoided by gen-suffixed distinct names.

Note "graceful" = a clean **error** (non-exhaustive match is correct Haskell semantics), not a
silent `Nothing`/wrong-answer. Keep the reorder/reshape test — but as a *confirmation*, not a
blocker.

### Open tensions (round 3)

1. **Clean no-match vs CASE-TRAP** — RESOLVED by code-read (graceful `CaseTrap` error, not
   SIGILL; see the gate section). Residual: compile-time-vs-runtime-error is UX polish only.
2. **`0xFD` valueId tag space** — the low-56-bit counter must not alias a real `fingerprint`
   external id; start the counter high / reserve a sub-range; one-line audit vs `TIDEPOOL_VARID_AUDIT`.
3. **Tenuring vs strict-force memory** — a `x <- bigQuery` tenures the *whole* forced result
   immortally; a rebound-but-still-captured value can't be collected. Acceptable for MVP
   ("leak for the session"); bites at the Phase-4 GC-reachability cleanup.
4. **2b: defer or delete?** Correctness never needs it. `:t` fidelity from a captured type
   *string* is fine for *display* but risky for *typechecking a reference* (`ppr` is not
   parser-faithful — ghc-exactprint exists for this reason); if synthetic-decl typechecking
   proves fragile, capture a structured `IfaceType` rather than a string.

### Updated punch-list (ranked by leverage; supersedes the round-2 phases where they conflict)

1. **Gen-suffix type names + keep all generations; one `g` counter** (H1+H2). Highest leverage;
   triangulated by GHCi/Julia/Unison. Touches only the naming input of `stableVarId`
   (`Translate.hs:1466`) + the cache key (`cache.rs:16`).
2. ~~Empirically gate the cross-generation no-match~~ — **DONE (code-read): graceful, non-blocker.**
   Keep a reorder/reshape test as confirmation; optionally make the error compile-time not runtime.
3. **`PERSISTENT_ROOTS` (run-scoped vs session-scoped roots).** Precondition for all heap
   persistence; small given the existing mark/truncate pattern.
4. **Tenure into `old_space`, nursery-only minor GC.** Nearly free via `is_in_range`; kills the
   O(total)/GC cliff — pull earlier than round-2's "v2".
5. **`SessionKind::Session` resident worker + affinity assert.** Reuses `EvalSession`'s
   `JoinHandle`/channels/gate.
6. **`OPAQUE` (not `NOINLINE`) placeholder + capability-passed binding.** Defers 2b to ergonomics.
7. **Two-layer `name → valueId(0xFD) → root` + decl-log cache key.** Verify `0xFD` first.
8. **Ship function/type-sig accumulation FIRST** (pure text, existing pipeline+cache, zero
   live-heap surgery — the easy 80%); `x#h` hash-qualified refs last (polish).

### Honest non-transfer

Unison assumes immutable/pure content-hashed *definitions*; tidepool's valueIds are
counter-minted provenance over a mutable-effect custom heap → Unison's value-level "perfect cache"
applies only to decl text. Temporal/Racket give *framing* for the decl(replayable)/value(live)
boundary but no mechanism tidepool lacks. The Smalltalk/Lisp image-persistence angle remains
unverified against primary sources (as round-2 noted) — none of these refinements lean on it.

## Deep research round 4 (2026-06-25): cross-turn TYPES — InteractiveContext, the serialize/reload idea, A/B/C

11-agent code+web workflow on *how a later turn typechecks a reference to a prior value binding*.
Decision-oriented; **decides Wave 3 typing only — Lane A (decls-as-text) is unaffected.**

**The two-problem framing (locked):** (1) passing *values* across turns is solved by keeping the JIT
machine alive (values never cross a gap — they sit in the live heap; binding table + GC root). (2)
passing *types* is the hard one because **types have no persistent home** in our architecture — repr
strips them, GHC is one-shot. Types are needed *only* to make GHC accept the source (runtime is
content-addressed, type-free); a *wrong-but-accepted* type is the only soundness risk, so fidelity
matters. The binding table is the bridge: `name → (heap-root [value], type-rep [the question])`.

**Confirmed: our architecture IS GHCi's, pre-discovered.** `ic_tythings` = types only; the linker's
`closure_env :: NameEnv (Name, ForeignHValue)` = values, separate, keyed by `Name`. That's our
binding-table-as-bridge exactly (GHC owns types, JIT owns values). The Ids-shadow / TyCons-coexist
asymmetry + `ic_mod_index` = our H1/H2 verbatim.

**The stmt→Core mechanism is named (kills the hand-rolled parser):** `tcRnStmt` typechecks
`foo <- bar` / `(a,b,c) <- foo` *in context* and returns the bound `[Id]` (names+types);
`deSugarExpr :: HscEnv -> LHsExpr GhcTc -> IO (.., Maybe CoreExpr)` turns that typechecked expr into
a **`CoreExpr`** — which we feed our Cranelift JIT instead of GHC's `hscCompileCoreExpr` (bytecode).
Core is a first-class intermediate; bytecode is strictly downstream (`hscParsedStmt` in
`GHC.Driver.Main`). `collectLStmtBinders` gives the binder names. So Wave-3 auto-detect = GHC's own
parse+typecheck, never a Rust scanner.

### The A/B/C verdict

- **A — long-lived `InteractiveContext`** (hint/IHaskell): **rejected.** Its only edge is fidelity,
  and C matches it without a persistent process. Real costs: GHC-session leaks (`hint` #96 ~4KB/run,
  GHC #698 never returns RAM), session corruption on module move/delete (forced restarts), and it
  demolishes the one-shot batch pipeline. No honest "do it right" points at A. **Defer indefinitely.**
- **B — synthetic `x :: <ppr-string>` decl each turn**: works, one-shot-clean, but **lossy** exactly
  where `ppr ≠ parser` (higher-rank, constraints, type families). It's the lone *ad-hoc seam*
  (re-render an inferred type to text and hope it re-parses).
- **C — serialize/reload `ic_tythings` via the iface path**: the **headline Unique fear is DEAD** —
  `BinIface` writes `(UnitId, ModuleName, OccName)` (a symtab index), reconstructs the Name on read
  allocating a fresh Unique. Names are content-addressed by `(Module, OccName)` across the boundary —
  *the same trick as our `stableVarId = hash("module:occ")` (`Translate.hs:1466`)*. Fidelity is
  perfect (structured `IfaceType`). Write-half: `mkIfaceTc`/`tyThingToIfaceDecl`. Read-half **already
  ships** (`FatIface.hs`, every eval). **Real blocker:** GHC keeps interactive modules out of the
  HPT/finder by design, so a synthetic iface can't be `import`ed — it must be **injected
  programmatically** into the type env + instances replayed. Primitives exist; **no prior art** for
  this workflow.

### DECISION (user, 2026-06-25): standardize on C — "do it right" — gated by a GO/NO-GO spike

Use the **faithful representation per plane, drop the lossy B**:
- **Declarations** → source text (Lane A). Text *is* their faithful form; the user's syntax
  re-parses perfectly. No fidelity problem.
- **Inferred value-binding types** → structured **`IfaceType` via C** (no source form exists for an
  inferred type, so the iface is the faithful carrier). Content-addressed both sides — matches
  tidepool's core.
- **B is deleted** — it was the only ad-hoc seam (exotic-type detection + ppr round-trip). Removing it
  makes the model uniform: every binding is `name → (heap-root, serialized IfaceDecl)`, no per-type
  branching; the type half is crash-durable on disk.

This is *less* mechanism than the B+C hybrid and is the natural expression of the content-addressed
core. **But C's injection step is unproven (no prior art), so it's gated:**

**Wave-3 GATE spike (first Wave-3 step, before any value-decl wiring):** stand up the C
write→inject→typecheck round-trip in the *existing batch pipeline* for **two** bindings — one simple
(`Int -> Int`) and one deliberately exotic (`forall a. (Ord a, Num a) => a -> Map a a`):
1. turn 1: after `typecheckModule`, `mkIfaceTc`/`tyThingToIfaceDecl` → `writeBinIface` to a session `.hi`.
2. turn 2 (fresh `runGhc`, no `InteractiveContext`): **inject** the iface programmatically past the
   HPT/finder exclusion (+ replay instances), compile a module that *references* the binding.
3. assert: typechecks, reconstructed type **byte-identical** to the original (no ppr), reference
   resolves to the seeded heap root. Run B on the same exotic type and diff to confirm C's edge is real.
- **GO** (injection clean + robust across both shapes) → standardize on C; **B is never built.**
- **NO-GO** (injection gnarly/fragile) → B is the floor (still ships), C the per-binding upgrade.

### SPIKE OUTCOME (2026-06-25): **GO — C is proven, for both simple and exotic types. Standardize on C; B is deleted.**

A reference in a **fresh batch `runGhc`** session typechecked against a faithfully-reconstructed
type loaded from a serialized fat `.hi` — no `InteractiveContext`, no GhciN modules, no source
recompile, no `ppr` round-trip — for both `Int -> Int` and `forall a. (Ord a, Num a) => a -> Map a a`.
Reference impl: `haskell/spike-optionc/Spike.hs` (committed `c044ee6`).

**The working injection path (use this in Wave 3, NOT import/downsweep):**
1. `GHC.Iface.Load.readIface` by **raw path** — NOT `findAndReadIface` (which routes through the
   source-anchored finder and fails on a source-less module).
2. `GHC.IfaceToCore.typecheckIface` inside `initIfaceCheck` → `ModDetails`/`md_types` (the read-half
   `FatIface.hs` already runs).
3. Inject: `HomeModInfo iface details emptyHomeModInfoLinkable` → `hscUpdateHPT
   (addHomeModInfoToHpt hmi)` → `addHomeModuleToFinder fc homeUnit (GWIB modNm NotBoot) modLoc`
   with `ml_hs_file = Nothing`.
4. Typecheck the reference via `setContext [IIDecl (simpleImportDecl "Session1"), …]` + `exprType`
   (generalizes to `tcRnStmt`/`deSugarExpr` for Core). Bypasses downsweep entirely.
This uses a **normal HPT home module** (e.g. `Tidepool.Session.G1`), which is exactly why it
sidesteps the documented GhciN/interactive-package finder exclusion. The naive import-of-a-
source-less-home-module path IS blocked ("Could not find module") — downsweep needs a source target.

**Methodological finding (matters for the regression test):** `eqType` and `IfaceType (==)` report
**False** across sessions — but it's NOT fidelity loss: the only diff is the `Map` TyCon's Unique
(separate `NameCache`s per `runGhc` reallocate it). The faithful, cross-session comparison vehicle is
**`nameStableString` over `tyConsOfType`** (content-addressed), which is **True**. The dead-Unique-
fear is confirmed dead. Wave-3 fidelity tests must use the content comparison, never `eqType`/`ppr`.

**Adversarial GO check passed:** a negative control (`g2 "not a number"`) is correctly **rejected**
(String has no `Num`) through the same injection path — proving the injected constraints are
load-bearing, consumed from the interface, not re-inferred from context.

**Honest B-nuance:** `ppr` actually round-trips type *structure* faithfully even for the exotic
cases — so B is NOT broken on structure. B's real mechanical break is **name-scope/qualifier
resolution**: `ppr` renders `Map` unqualified, and a using-module importing `Data.Map` qualified-only
fails "Not in scope: Map". C has no such seam — it carries the original `Name`+module, exact
regardless of import style. So C wins on the *name-resolution seam* + do-it-right uniformity (one
mechanism, structured `IfaceType`, crash-durable on disk), not on structure-mangling as first assumed.

**Knock-on consequences (now locked):**
- **B is deleted** from the plan. Wave-3 typing = C only.
- The round-3 "captured-type-as-string fragility" risk (tension #4) is **closed** — we carry
  structured iface, not a string.
- `type-capture` (merged, captures a `ppr` *string*) is now only useful for `:t`/`:i` *display*;
  the typechecking carrier is the serialized `IfaceDecl`. Its role narrows, doesn't disappear.
- Binding table is uniform: `name → (heap-root [value, Wave 1/2], serialized IfaceDecl [type])`.
- Value persistence (Wave 1/2) is unchanged and unblocked — C is the type plane only.

**C is the TYPE plane only.** It does NOT reduce value-persistence work (live machine, stable VarIds,
`PERSISTENT_ROOTS`, tenuring — Wave 1/2, shared by all options). Cracking C makes the type *carrier*
faithful; the value lives in the JIT heap regardless. Wave 1 proceeds independent of this.

## Open / next

- [x] ~~Re-run the web `/deep-research`~~ — **done** (2026-06-25). Findings above.
- [x] ~~Tag-dispatch check~~ — **RESOLVED by code-read (2026-06-25), reorder-safe.**
      Case dispatch loads the 64-bit `DataConId` from offset 8 (`CON_TAG_OFFSET`) and does an
      equality-chain compare against the compile-time `DataConId` hash
      (`tidepool-codegen/src/emit/case.rs:193-258`; explicit comment: *"DataConIds are large
      GHC Uniques... not small sequential integers"*). The offset-0 byte is only the
      object-kind tag (closure/thunk/con/lit, `layout.rs:22-28`), read only to decide forcing
      (`emit_data_dispatch`, tag < 2). The positional `tag: u32` in `datacon.rs` is
      metadata-only — **dispatch never reads it.** Con construction stores `DataConId` at
      offset 8 (`emit/expr.rs:412-500`). `DataConId = stableVarId = fingerprint("module:occ")`
      (`Translate.hs:1466`), order-independent. GADT/EqSpec is type-level only, doesn't touch
      dispatch. ⇒ a value built in call-A case-matches correctly in a separately-compiled
      call-B even if constructors were reordered/added/removed. The one thing that breaks it:
      changing a constructor's *name or module*. Keep an empirical reorder test as a
      regression guard, but it's confirmation, not a gate.
- [ ] **Gray-zone per-type check:** boxed constructors with embedded function/thunk fields,
      and Text/String (ByteArray# / cons-cell paths) — the research left these unresolved
      between "survives as raw bytes" and "needs shared code." Probe each on tidepool.
- [ ] **Minimal shared metadata:** determine what must travel with a persisted boxed value
      for safe cross-fragment consumption — just the constructor-id→layout table, or also GC
      layout descriptors + entry code — and whether normal-form forcing at bind time reduces
      this to layout-table-only.
- [ ] Design the **session binding table**: stable session-binding naming +
      `VarId → heap-root` lookup before `unresolved_var_trap`, registered as GC roots.
- [ ] Machine-retention plumbing: keep `JitEffectMachine` alive across eval calls
      (model on the `ask`/continuation path).
- [ ] Decide bind semantics: **strict bindings** (recommended — skips Tiers 1 & 2, and the
      research confirms forcing-to-normal-form as the principled way to drop code/thunk
      hazards) vs lazy.
- [ ] *(optional)* Re-research angle 5 (Smalltalk/Lisp image persistence) against primary
      sources if you want verified prior-art backing for the strict-binding design choice.

## Sources (web research)

- Marlow, "Faster laziness using dynamic pointer tagging" (ICFP'07) — positional tags,
  self-describing objects, cross-module tag-zero fallback: <https://simonmar.github.io/bib/papers/ptr-tagging.pdf>
- GHC `GHC.Core.DataCon` (tag = positional zip) — <https://hackage.haskell.org/package/ghc-9.2.1/docs/GHC-Core-DataCon.html>
- GHC `GHC.Runtime.Context` (`ic_tythings`, `ic_mod_index`, shadowing) — <https://hackage.haskell.org/package/ghc-lib-parser-9.2.3.20220527/docs/GHC-Runtime-Context.html>
- GHC Linker (`closure_env`, `itbl_env`, `extendLinkEnv`) — <https://downloads.haskell.org/~ghc/7.10.3/docs/html/libraries/ghc-7.10.3/src/Linker.html>
- GHCi User's Guide (temporary bindings lost on `:load`/`:reload`) — <https://downloads.haskell.org/ghc/latest/docs/users_guide/ghci.html>
- `ghc-heap` closure types (Fun/Thunk/PAP/AP) — <https://hackage.haskell.org/package/ghc-heap-9.10.1/docs/GHC-Exts-Heap.html>
- `Control.DeepSeq` (normal-form forcing) — <https://hackage.haskell.org/package/deepseq-1.4.2.0/docs/Control-DeepSeq.html>

## Source pointers (from the code-read agents)

- `stableVarId` / content-addressed `DataConId` — `Translate.hs:1466`
- `localVarId = hash(occName + GHC-unique)` (session-local) — Translate.hs (varId hashing)
- `PARKED_STREAMS`, `clear_parked_streams`, `RegistryGuard::drop` — `jit_machine.rs:117`
- continuation/`ask` machine-retention pattern — JitEffectMachine lifecycle
