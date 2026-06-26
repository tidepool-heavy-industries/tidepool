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
inferred, tidied type), `tyThingToIfaceDecl` → assemble a `ModIface` (`mkIfaceTc`/`mkFullIface`) for
the session home module `Tidepool.Session.G<g>`, `writeBinIface` to a session `.hi`. **Critical:
the binder `IfaceDecl` must carry the TYPE only, NO unfolding** — so tidepool's `resolveExternals`
can't inline a (nonexistent) definition and instead leaves it an unresolved external the JIT
overrides. (Verify: a session binder reaches codegen as an external Var, not an inlined body.)

**Inject + reference (every later turn):** in the fresh batch `runGhc` extract —
1. `GHC.Iface.Load.readIface` by **raw path** (NOT `findAndReadIface` — the finder is source-anchored
   and rejects a source-less module).
2. `GHC.IfaceToCore.typecheckIface` inside `initIfaceCheck` → `ModDetails`/`md_types` (reconstructed
   `TyThing`s; this is the read-half `FatIface.hs` already runs).
3. Inject as a **normal HPT home module**: `HomeModInfo iface details emptyHomeModInfoLinkable` →
   `hscUpdateHPT (addHomeModInfoToHpt hmi)` → `addHomeModuleToFinder fc homeUnit (GWIB modNm NotBoot)
   modLoc` with `ml_hs_file = Nothing`. (Home module, NOT the `interactive:GhciN` package — that is
   exactly what sidesteps the finder-exclusion blocker.)
4. Bring into scope + typecheck: `setContext [IIDecl (simpleImportDecl modNm), …]`, then the user
   expression goes through `tcRnStmt` → `deSugarExpr` → **`CoreExpr`** (we feed our JIT, not GHC's
   `hscCompileCoreExpr`).

**Type+value resolution, unified by stable VarId.** The iface declares `x :: T` in module
`Tidepool.Session.G<g>`. A later reference compiles to a Core `Var` for `Tidepool.Session.G<g>.x`.
`stableVarId = hash("Tidepool.Session.G<g>:x")` (`Translate.hs:1466`) — the SAME id the JIT seeds in
`ExternalEnv` → the heap root. So **iface = type, ExternalEnv = value, one key.**

**Fidelity test vehicle:** `nameStableString` over `tyConsOfType` (content-addressed). **NOT**
`eqType`/`IfaceType (==)`/`ppr` — those report false-negatives across sessions purely from
`NameCache` Unique reallocation (proven harmless; typechecking succeeds regardless).

**Honest scope of C's win:** `ppr` actually round-trips type *structure* fine; C's real edge is the
**name-resolution seam** (B re-renders `Map` unqualified and fails if the using-module imports it
qualified-only; C carries the original `Name`+module, exact regardless) + do-it-right uniformity.

---

## 3. Naming & shadowing — one unified scheme

One monotonic per-session generation counter `g` (= GHCi's `ic_mod_index`):
- **Value binding** `x` at gen `g` → home module `Tidepool.Session.G<g>`, declared in its synthesized
  iface; VarId `= stableVarId("Tidepool.Session.G<g>:x")`. The JIT binding table maps that VarId → heap root.
- **Rebinding** `x` (new value/type) → bump `g`, new module `G<g'>`, new VarId, new iface entry; the
  **old `G<g>` iface + heap root stay alive** (already-compiled references to old `x` keep resolving).
  `BindingTable` maps current name `"x"` → latest VarId, and retains all live VarId→root.
- **User type/class decls** (Lane A) → a real session-library module; redefining a *type's shape*
  bumps `g` too (coexisting `Foo` generations), so old values of old `Foo` stay dispatchable
  (content-addressed `DataConId`; case-trap is graceful — proven). Functions shadow latest-wins (text).
- This realizes H1 (Ids-shadow / TyCons-coexist), H2 (single `g`), C1 (counter-minted ids) concretely
  via the module-name namespace. Cache key includes `(sessionId, g)`.

---

## 4. The value plane (Wave 1) — known-viable, the real engineering

Sequential `1.A → 1.B` (shared `jit_machine.rs` lifecycle seam; frozen as a 0.1 contract). Each a
spawn→review→merge unit.

**1.A — buffer retention + persistent roots + tenuring (`tidepool-codegen`):**
- **E′ (the load-bearing fix):** after the first GC the live heap migrates off the machine's
  `Nursery` into `GcState::active_buffer` (a thread-local that `RegistryGuard::drop` frees every run,
  `host_fns.rs:623,186`). Make `active_buffer` **machine-owned**; split `clear_gc_state` into
  `clear_run_scratch()` (per-run) vs `free_session_heap()` (machine-drop).
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

## 7. Open risks / unknowns (post-C-GO)

1. **Type-only iface synthesis** — must confirm `tyThingToIfaceDecl` of a session binder yields a
   TYPE-only decl (no unfolding) so `resolveExternals` leaves it external for the JIT override; if it
   carries an unfolding, strip it. (Wave-3 first task, right after porting the spike.)
2. **Instance/orphan replay** — if a session binding's type or a later reference needs an instance
   accumulated in a prior turn, the injected ifaces must carry/replay `mi_insts`. Simple data-wrangling
   types don't; flag for the first exotic case.
3. **Session-iface ↔ session-library Name resolution** — a value binding `x :: Foo` where `Foo` is a
   Lane-A user type: the iface references `Foo` by Name in the (real, in-scope) session-library
   module. Confirm both are in scope together each turn.
4. **Per-session memory** — one live machine + a warm-ish extract; bounded by one-session MVP and
   `TIDEPOOL_MAX_HEAP`. Multi-session is Wave 4.
5. **Cranelift FuncId non-redefinition** — `add_function` adds NEW ids (fine); never redefine an id.

---

## 8. Critical path

`scaffold(done)` → **Lane A re-spec (ships standalone)** ∥ **Wave 1 (1.A→1.B)** → Wave 2
(`tidepool-repl` server) → Wave 3 (C port + value binding) → Wave 4. The C gate is already GREEN;
nothing downstream is research-risk, only engineering.
