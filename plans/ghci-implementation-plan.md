# tidepool-repl — Implementation Plan (consolidated, post-C-spike)

Single actionable build spec. Background/derivation lives in `ghci-session-persistence.md`
(research + decisions) and `ghci-swarm-orchestration.md` (wave history). This doc is the
current source of truth for *what to build*. Review target for adversarial review.

---

## 0. Status snapshot (2026-06-25)

| Piece | State |
|---|---|
| Value persistence (live JIT machine, heap roots, re-entry) | **Known-viable** (code-read: JITModule multi-round define/finalize confirmed in cranelift 0.129.1; GC external-root registry exists). Not built. |
| Type carrying across turns (**Option C**: serialize binder type → fat `.hi` → reload) | **PROVEN** (spike `c044ee6`, `haskell/spike-optionc/Spike.hs`) for simple + exotic types. |
| Wave 0 scaffold/contracts | **MERGED** (`scaffold`, `type-capture`, `case-trap-test`, `fix-test-debt`); workspace green (50/0 bin suite). |
| Lane A (declaration accumulation) | **Parked — needs re-spec** (the hand-rolled `=`-classifier is deleted; see §5.0). |
| Wave 1–3 | Not started. |

**The two problems, both now answered.** (1) *Values* across turns → keep one `JitEffectMachine`
alive; the value sits in the live heap, reached by a stable VarId → heap root (known-viable).
(2) *Types* across turns → Option C: GHC owns types, serialized as structured `IfaceType` to a
fat `.hi`, reloaded each turn (proven). B (ppr-string reconstruction) is **deleted**.

---

## 1. Architecture — two planes + one session

```
tidepool-repl  (a SEPARATE MCP server / binary; the `tidepool` eval server is untouched)
 └─ one Session (MVP: one active session), a RESIDENT worker thread owning:
     ├─ live JitEffectMachine        — VALUE plane: heap + GC roots, persists across turns
     ├─ BindingTable                 — the bridge: name → (heapRoot, ifaceDecl, genId)
     ├─ session declaration library  — user-written data/type/class/fn decls, as TEXT (Lane A)
     └─ session value ifaces          — synthesized fat .hi per value binding (Option C, TYPE plane)
```

- **Value plane** (JIT): a binding's runtime value lives in the persistent machine's heap, held by
  a GC root, addressed by a stable VarId. The JIT resolves a later turn's reference to it via the
  seeded `ExternalEnv` (the `0xFD`/external override scaffolded in Wave 0).
- **Type plane** (GHC, Option C): a binding's *type* is serialized as a structured `IfaceDecl`
  (type only, no unfolding) into a synthesized fat `.hi` for a session home module; each turn's
  fresh batch extract reloads it so references typecheck. GHC never holds the value.
- **Declaration plane** (Lane A): user-written declarations are accumulated as **source text** in a
  real session-library module, recompiled normally each turn (their faithful form is their source).
- **Session = a resident worker thread** in a new `tidepool-repl` server, NOT in the `tidepool`
  eval pool (no `eval_semaphore`/`continuations` coupling). MVP caps at one active session.

**Key convergence — the binding table is the bridge GHC already uses.** GHCi splits identity the
same way: `ic_tythings` (types) vs the linker's `closure_env` (values), keyed by `Name`. Our
`BindingTable` is exactly that bridge: the `ifaceDecl` half feeds GHC's type plane, the `heapRoot`
half feeds the JIT's value plane, keyed by one stable VarId.

---

## 2. The Option-C typing mechanism (productionize the spike)

The spike (`Spike.hs`) proved this path. Wave 3 productionizes it inside the haskell extract layer.

**Write (on bind, turn M):** after `typecheckModule`, take the binder's `TyThing` (its `Id` with the
inferred, tidied type), `tyThingToIfaceDecl` → assemble a **THIN** `ModIface` for the session module
`Tidepool.Session.Val.G<g>`, `writeBinIface` to a session `.hi`. The iface serves the **front-end
typechecker only** (HPT injection), NOT tidepool's `resolveExternals` back-end. **Critical (kimi B1):**
it must carry **no `mi_extra_decls` and no `ifIdUnfolding`** — the extractor runs
`-fwrite-if-simplified-core` (`GhcPipeline.hs:233`) and `resolveExternals` falls through to
`lookupFatIface` → `mi_extra_decls` (`Resolve.hs:148-172`, `FatIface.hs:56-128`); a fat session iface
would carry the binder's Core and inline it, defeating the override. Build the iface deliberately thin
(strip `mi_extra_decls`/unfoldings), and/or exclude session modules from the fat fallback.

**Inject + reference (every later turn):** in the fresh batch `runGhc` extract —
1. `GHC.Iface.Load.readIface` by **raw path** (NOT `findAndReadIface` — the finder is source-anchored
   and rejects a source-less module).
2. `GHC.IfaceToCore.typecheckIface` inside `initIfaceCheck` → `ModDetails`/`md_types` (reconstructed
   `TyThing`s; this is the read-half `FatIface.hs` already runs).
3. Inject as a **normal HPT home module**: `HomeModInfo iface details emptyHomeModInfoLinkable` →
   `hscUpdateHPT (addHomeModInfoToHpt hmi)` → `addHomeModuleToFinder fc homeUnit (GWIB modNm NotBoot)
   modLoc` with `ml_hs_file = Nothing`. (Home module, NOT the `interactive:GhciN` package — that is
   exactly what sidesteps the finder-exclusion blocker.)
4. Bring the session module into scope and compile the **wrapped user turn as a normal module** via
   the EXISTING `typecheckModule` → `hscDesugar`/`core2core` pipeline (which already yields Core) — NOT
   GHCi's `tcRnStmt`/`deSugarExpr` interactive-stmt path. **(kimi B4):** the spike proved
   *typecheck* against an injected HPT iface (`exprType`), it did NOT prove Core emission, and
   `tcRnStmt` risks re-introducing the `interactive:GhciN` path the spike deliberately avoided. The
   production reference lives in a real wrapper module that `import`s the session module, so the normal
   module pipeline desugars it to Core with the injected iface in the HPT. **Follow-on proof required**
   (Wave-3 first task): extend the spike to *emit Core* that references an injected session binder
   (not just `exprType`). The bind-statement auto-detect (`foo <- bar`) is SEPARABLE — handle it by
   template-wrapping into a `do`-block compiled as a module, not by the interactive-stmt path.

**Type+value resolution — session binders are a DISTINCT resolution category (kimi B2).** A reference
to `Tidepool.Session.Val.G<g>.x` must NOT go through `resolveExternals`: an unresolved external is put
in `tsUnresolvedIds` and emitted as an **error-sentinel `NVar 0x45…`** (`Translate.hs:120,467-470`),
discarding the session id → the `ExternalEnv` override (keyed on the session id) never fires. Fix:
recognize session-module Names (`Tidepool.Session.Val.*`) in the extract and route them to a **direct
`NVar (stableVarId name)`** emission — excluded from BOTH unfolding-resolution (B1) AND
`tsUnresolvedIds` (B2). Then `stableVarId = hash("Tidepool.Session.Val.G<g>:x")` (`Translate.hs:1466`)
reaches codegen intact; the `emit/expr.rs:368` Var-miss site resolves it via `ExternalEnv` → heap
root. So: **iface = type (front-end only), direct-NVar + ExternalEnv = value, one `stableVarId` key.**
This is new extract/codegen wiring (a third category beside "resolved" and "unresolved"), not a
property we get for free — specify and test it explicitly in Wave 3.

**Fidelity test vehicle:** `nameStableString` over `tyConsOfType` (content-addressed). **NOT**
`eqType`/`IfaceType (==)`/`ppr` — those report false-negatives across sessions purely from
`NameCache` Unique reallocation (proven harmless; typechecking succeeds regardless).

**Honest scope of C's win:** `ppr` actually round-trips type *structure* fine; C's real edge is the
**name-resolution seam** (B re-renders `Map` unqualified and fails if the using-module imports it
qualified-only; C carries the original `Name`+module, exact regardless) + do-it-right uniformity.

---

## 3. Naming & shadowing — one unified scheme

One monotonic per-session generation counter `g` (= GHCi's `ic_mod_index`). **Everything is
gen-versioned by module name** — because `DataConId`/`stableVarId = hash(module:occ)`
(`Translate.hs:1466-1474`) is **module-name-addressed, NOT shape-addressed** (kimi R3): redefining
`Foo`'s shape in the *same* module yields the same `DataConId` → `insert_checked` collision
(`datacon_table.rs:73-85`). Gen-versioned module names are what make redefinition coexist safely.
- **User decls** (Lane A) → module **`Tidepool.Session.Lib.G<g>`** (NOT a single regenerated module —
  kimi B3). A turn adding/redefining a decl mints `Lib.G<g+1>` (re-exporting `Lib.G<g>` + the change);
  old gen modules are stable + cached. Redefining `Foo`'s shape → new module → new `DataConId`, so
  old `Foo` values stay dispatchable/renderable against `Lib.G<g_old>`'s still-live iface. Functions
  shadow latest-wins (newest gen).
- **Value binding** `x` at gen `g` → module **`Tidepool.Session.Val.G<g>`**; VarId =
  **`stableVarId("Tidepool.Session.Val.G<g>:x")`** — a normal **`0xFE`-tagged external id**, since the
  binder has a real module name under C. **Drop the scaffold's `0xFD` counter-minted id and the
  `type_string` field** — both were the pre-C (synthetic-decl) design (`binding_table.rs:12-13,43-45`);
  under C the gen-versioned module name already gives a fresh, collision-free id per (re)bind, and the
  type carrier is the structured iface, not a string. **BindingTable revised to**
  `name → (stableVarId[0xFE], root_slot, ifaceDecl)`.
- **Rebinding** `x` → bump `g`, new `Val.G<g'>`, new stableVarId, new iface; old gen module + heap
  root stay alive (already-compiled refs to old `x` keep resolving via their old stableVarId).
  `BindingTable` maps current name `"x"` → latest stableVarId, retains all live stableVarId→root.
- A redefined-shape `Foo` value sliced by new-`Foo` code is a **clean type error** (distinct modules =
  distinct types), or at runtime a graceful `CaseTrap` (proven), never SIGILL.
- Realizes H1/H2/C1 via the module namespace. Cache key += `(sessionId, g)` — keyed so two sessions'
  identical-text `Lib.G<g>` don't collide and a gen bump invalidates correctly (R6: spell out in Lane A).

---

## 4. The value plane (Wave 1) — known-viable, the real engineering

Sequential `1.A → 1.B` (shared `jit_machine.rs` lifecycle seam; frozen as a 0.1 contract). Each a
spawn→review→merge unit.

**1.A — buffer retention + persistent roots + tenuring (`tidepool-codegen`):**
- **E′ (the load-bearing fix):** after the first GC the live heap migrates off the machine's
  `Nursery` into `GcState::active_buffer` (a thread-local that `RegistryGuard::drop` frees every run,
  `host_fns.rs:623,186`). Make `active_buffer` **machine-owned**; split `clear_gc_state` into
  `clear_run_scratch()` (per-run) vs `free_session_heap()` (machine-drop). **Also (kimi R1):**
  `install_registries` (`jit_machine.rs:200-206`) calls `set_gc_state` with `self.nursery.start()`
  **every run** — for a session machine with a retained `active_buffer`, re-point GC state at the
  retained buffer + its high-water mark, NOT back to `nursery.start()`, or run-2 silently runs against
  the stale empty nursery and the persisted heap is orphaned. This is the same fix as F (persistent
  cursor) and must cover both the `RegistryGuard::drop` and `install_registries` ends of the lifecycle.
- **D — `PERSISTENT_ROOTS`:** a thread-local parallel to `RUST_ROOTS`, appended to GC roots, cleared
  only at machine drop. `register_persistent_root` (stub landed in scaffold, `unsafe`).
- **E — tenuring:** split `GcState` into nursery (gen-0) + append-only growable `old_space` (gen-1);
  `tenure(ptr)` evacuates a strict-forced binding once into old_space; minor GC's from-range = nursery
  only (old_space auto-skipped by `is_in_range`, `raw.rs:136`); no write barrier (strict-forced
  immutable values). Compact old_space only when a binding generation dies.
- **F — persistent nursery cursor:** thread the session high-water mark through `make_vmctx` so a
  re-entered fragment bumps from the last run's boundary.

**1.B — re-entry APIs + env-seeding + strict-force (`tidepool-codegen`):**
- `add_function(name, &CoreExpr, &ExternalEnv) -> FuncId` (declare+define into the live JITModule,
  re-`finalize_definitions` — multi-round-safe), `run_fragment(func_id, …)` (reuse live heap via the
  0.1 install/teardown contract).
- `external_env` already threaded through `compile_expr` (0.5 scaffold). Var-miss resolution
  (`emit/expr.rs:368`): look up the VarId in `ExternalEnv` → `SsaVal::from_external_pointer` → fresh
  `iconst` **per fragment** (never a shared SSA value — the scaffold invariant).
- **K — strict-force-at-bind:** `deep_force` first-order results to NF then tenure; closures/PAPs
  (Tier-1) stored as-is (rooted, machine-alive), NOT deep-forced.

**Converge:** mechanical smoke test (build value frag-1, tenure + register root, seed env, JIT
frag-2 `case x of …`, run; variant forcing a real GC that swaps `active_buffer`). NOT the acceptance
proof — that's the multi-turn real-entry-point test in Wave 2/3.

---

## 5. Waves

### 5.0 Lane A — re-spec (explicit tools + GHC binders), ships standalone

- **Tools:** `session_open` · `session_def` (append a declaration to the session library) ·
  `session_eval` (evaluate an expression) · `session_cmd` (`:t`/`:i`/`:bindings`/`:reset`) ·
  `session_close`. **No decl-vs-expr classifier** — the tool name classifies. (`tidepool-repl`.)
- **Declarations** → ordered decl log → regenerate the whole session-library module each turn
  (pure function of the log; atomic write; on include path at highest precedence). Functions
  shadow latest-wins; types append-only-coexisting per §3.
- **Binder names** (for shadowing / the binding table) come from **GHC**, never a Rust scanner:
  parse with `parseStmt`/the module parser, `collectLStmtBinders`/`collectPatBinders` for the bound
  names, in the haskell extract layer, returned as data.
- Cache key += `(sessionId, g)`. Independent of the value/type planes — ships as a usable decl-REPL.

### 5.1 Wave 1 — value plane (§4). Sequential 1.A → 1.B → converge smoke.

### 5.2 Wave 2 — `tidepool-repl` server + resident session worker
- New crate/binary `tidepool-repl` reusing `tidepool-runtime`/`-codegen`. Resident worker thread
  owns the live machine + binding table + decl log + session ifaces. Commands on a single-consumer
  channel (serialized). NOT permit-gated; own `sessions` registry; `session_close` drops the machine.
- Reuse the parked-thread mechanism for in-command `ask`. Per-command timeout via `PauseGate`.
- **Acceptance (real path, multi-turn, natural GC):** many `session_eval` turns; machine/heap persist
  across an organic collection.

### 5.3 Wave 3 — value binding end-to-end + the C path
- **C GATE first (proven, now productionize):** port `Spike.hs`'s write/inject path into the haskell
  extract (`GhcPipeline.hs`/`FatIface.hs`): capture binder `TyThing` → type-only iface → session `.hi`;
  inject via `readIface`+HPT each turn. Verify a session binder reaches codegen as an *external Var*
  (no inlined unfolding) so the JIT override applies.
- **Bind** (`x <- action` / `let x = e`): GHC parse→typecheck→`deSugarExpr`→Core (auto-detect via
  `tcRnStmt`); JIT-run; strict-force (K); tenure + `register_persistent_root`; mint VarId
  `stableVarId("Tidepool.Session.G<g>:x")`; write the session iface; record in `BindingTable`.
- **Reference** (later turn): inject session ifaces → typechecks; Core `Var` for the session binder →
  JIT resolves via seeded `ExternalEnv` → heap root.
- **Acceptance (real path, multi-turn, natural GC; correctness sweep, not a demo):** bind an **Int**
  (Tier-0), a **JSON `Value`** (Tier-0 structured + DataConTable merge), and a **function** (Tier-1
  closure — proves prior-fragment code stays callable after `add_function`); read/call each several
  turns later, after an organic GC; custom-ADT value rendered (real con names, not `<unknown>`) +
  case-matched in a later turn. Plus a reorder/reshape-type regression (graceful, not SIGILL).
- **N — DataConTable merge:** session holds a merged table; passed to render + run.

### 5.4 Wave 4 — polish
`:t`/`:i` (from captured type), `x#h` hash-qualified refs, GC-reachability-tied root cleanup,
structured-iface escalation already default (C), multi-session.

---

## 6. Test strategy (standing rule)

Acceptance/integration tests drive the **real `tidepool-repl` entry point** over **multiple real
turns** with allocation/GC happening **organically** — never bespoke internal wiring or forced GC
(those are unit smoke checks only). The Wave-1 converge smoke is necessary-not-sufficient; the
Wave-2/3 multi-turn sweeps are the proofs. Fidelity comparisons use `nameStableString`, never
`eqType`/`ppr`.

---

## 7. Open risks / unknowns (post-C-GO, post-kimi-r1, post-spike-extract)

1. ~~Thin-iface synthesis (B1)~~ — **PROVEN** (`spike-extract`, merged `da1c238`). The thin iface
   prevents inlining; the production mechanism is the `isSessionValVar` exclusion in `resolveExternals`
   (`Resolve.hs:206,222` — gated on the `Tidepool.Session.Val.` module prefix, inert for normal evals)
   routing session binders to direct `NVar(stableVarId)`. Negative control: disabling the exclusion
   collapses binders to the `0x45` sentinel (confirms B2). Promote `isSessionValVar` → the
   `VarResolution` sum (domain model §3) in the Wave-3 productionization.
2. **Instance/orphan replay (kimi R4 — elevated, still open)** — NOT exotic-only: even `show sessionMap`
   needs the relevant `Show`/`Ord` instances in scope. The injected ifaces (or in-scope session-library
   modules) must carry/replay `mi_insts`. Real Wave-3 requirement; `spike-extract` noted what's needed.
3. ~~Core-emission-with-injection (B4)~~ — **PROVEN** (`spike-extract`): Core emitted through the normal
   module pipeline (`summariseFile` → `typecheckModule` → `hscDesugar` → `core2core`) against an
   injected source-less session iface, for both simple and exotic types. Not `tcRnStmt`.
4. **Binder-name extraction coverage (kimi R5)** — verify `collectLStmtBinders`/`collectPatBinders`
   handle the bind forms we accept (`x <-`, `(a,b) <-`, `let x =`, pattern binds) in the extract.
5. **Session-iface ↔ session-library Name resolution** — `x :: Foo` (Foo a Lane-A type): confirm the
   `Val.G<g>` iface and the `Lib.G<g'>` module are co-resolvable each turn.
6. **Per-session memory** — one live machine; bounded by one-session MVP + `TIDEPOOL_MAX_HEAP`.
7. **Cranelift FuncId non-redefinition** — `add_function` adds NEW ids (fine); never redefine an id.

## 9. Review round 1 (kimi `kimi-review`) — BLOCKING fixes applied

| Finding | Resolution (where) |
|---|---|
| **B1** fat-iface (`mi_extra_decls`, `-fwrite-if-simplified-core`) would inline the session binder, defeating the override | §2 Write — thin iface (no `mi_extra_decls`/unfolding) + exclude session modules from fat fallback |
| **B2** unresolved external → error-sentinel `NVar 0x45…` (`Translate.hs:120,467-470`), session id discarded; "JIT overrides unresolved external" is false | §2 — session binders are a **distinct category**: recognized by `Tidepool.Session.Val.*` module, emitted as **direct `NVar(stableVarId)`**, excluded from unfolding-resolution AND `tsUnresolvedIds`; resolved at codegen via `ExternalEnv`. New extract/codegen wiring, specified + tested in Wave 3. |
| **B3** single regenerated Lane-A module contradicts type coexistence; `DataConId` is module-addressed so same-module reshape collides | §3 — gen-version **both** `Lib.G<g>` (decls) and `Val.G<g>` (values); reshape → new module → new `DataConId`, old gen stays |
| **B4** spike only proved `exprType`, not Core emission; `tcRnStmt` risks the GhciN path | §2 step 4 — production uses the **normal module pipeline** (not `tcRnStmt`) with HPT injection; **follow-on Core micro-spike** required (§7.3) |
| **R1** `install_registries` resets `set_gc_state` to `nursery.start()` every run | §4 1.A E′ — re-point GC state at the retained `active_buffer`, both lifecycle ends |
| **R2** scaffold `BindingTable` is `0xFD`/`type_string` (pre-C) | §3 — drop `0xFD` + `type_string`; revise to `name → (stableVarId[0xFE], root_slot, ifaceDecl)` |
| **R3** "content-addressed DataConId" misleading | §3 — stated as **module-name-addressed**, the reason gen-versioning is needed |
| **R4** instance replay under-scoped | §7.2 — elevated to a real Wave-3 requirement |
| **R5/R6, NITs** | §7.4, §3 cache-key note; terminology reconciled (thin iface, not "fat") |

Bottom line shift: the C *gate* stays GREEN (typecheck-with-injection proven), but **value
resolution of a session reference is new extract/codegen wiring** (B1+B2), not free — Wave 3's first
tasks are (a) the thin-iface + direct-NVar + ExternalEnv mechanism and (b) the Core-emission spike,
before any binding-table wiring.

---

## 8a. Type-modeling principle (user, 2026-06-25)

Model the domain with the type system — newtypes, sum types, type-state — over bare
pointers/strings/ints. Make illegal states unrepresentable and force exhaustive handling. The
load-bearing ones (apply in Wave-1/2/3 impl, not the throwaway spikes):

- **`VarResolution` sum (the kimi-B2 fix, do it as a TYPE):** classify every external Var exhaustively
  — `Inlinable(Unfolding) | SessionBinding(SessionVarId) | Unresolved` — instead of ad-hoc
  `tsUnresolvedIds`/`isResolvable` boolean checks. The compiler then forces all three branches at
  every site (`Resolve.hs`/`Translate.hs`/`emit/expr.rs`). The spike's `Resolve.isResolvable`
  predicate is the seed; promote it to this sum.
- **`SessionModule { kind: Val | Lib, gen: Generation }`** — the ONE place the gen-versioned module
  name string `"Tidepool.Session.{Val|Lib}.G<g>"` is constructed. No bare module-name strings sprinkled
  around; render through this type. Kills a whole class of "wrong module string" bugs.
- **Newtypes for the identifiers (never bare):** `Generation(u64)` (monotonic), `SessionId`,
  `BindingName` (user-facing "x"), `SessionVarId` (the binder's `stableVarId`, with a smart ctor
  `SessionVarId::of(SessionModule, OccName)` so the hash rule lives in one place).
- **`VarKind` sum decoding the high-byte tag** — `External(0xFE) | SessionBinding | ErrorSentinel(0x45)
  | Local` — replace bare byte comparisons in `emit/expr.rs` with a decode-to-enum + match.
- **`RootSlot(*mut *mut u8)`** newtype encoding the GC-liveness contract (the stable, GC-updated slot
  the value resolution LOADS through — §2/the spike-codegen GC-safety fix). The unsafe accessor lives
  on the newtype; callers can't pass a bare pointer by accident.
- **`BoundValue` sum:** `Tier0Forced(RootSlot) | Tier1Closure(RootSlot)` — models the strict-force vs
  store-as-is distinction at the type level (Wave-3 bind path).
- **`BindingEntry { name: BindingName, id: SessionVarId, value: BoundValue, iface: IfaceDecl }`** — the
  revised binding table (drops the scaffold's `0xFD` `value_id` and `type_string`; the iface is the
  structured type carrier, not a string — kimi R2/NIT).
- **`SessionCommand` sum:** `Def(DeclText) | Eval(ExprText) | Cmd(MetaCommand) | Close` — the
  `tidepool-repl` surface, not stringly-typed dispatch.
- **Type-state where it pays (Wave 2):** consider a `Session<Open>`/`Session<Closed>` phantom so
  post-close ops don't typecheck; the eval `Response::{Complete,Stream}` channel is already this shape.

Bias toward these when wiring each wave; flag in spec reviews where a bare type should be a newtype/sum.

## 8. Critical path

`scaffold(done)` → **Lane A re-spec (ships standalone)** ∥ **Wave 1 (1.A→1.B)** → Wave 2
(`tidepool-repl` server) → Wave 3 (C port + value binding) → Wave 4. The C gate is already GREEN;
nothing downstream is research-risk, only engineering.
