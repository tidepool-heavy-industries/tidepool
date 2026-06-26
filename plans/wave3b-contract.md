# Wave 3b — VALUE BINDING: the convergence contract

The single interface spec the parallel subtrees (H = Haskell extract, R = codegen
bind primitive, T = runtime+repl integration) implement against. Frozen here so
the pieces compose on fold. Companion to `ghci-implementation-plan.md` §5.3 and
`ghci-domain-model.md`. **Read those two first.**

The mechanisms are all built+proven (value plane: `run_pure_and_bind` /
`add_function` / `run_fragment` / `RootSlot` / tenure / `deep_force`; type plane:
`Tidepool.Session.{mkThinSessionIface,writeSessionIface,injectSessionScope}` +
`GhcPipeline.runPipelineSession`; converge proof). Wave 3b WIRES them.

---

## 0. The two turn shapes

A `session_eval` turn is ONE of:

- **BIND** — input is `x <- action` or `let x = e`. Produces a value rooted on
  the live heap, named `x`, referenceable by later turns.
- **REFERENCE / EXPR** — input is a bare expression (may mention earlier
  bindings). Produces a JSON result; binds nothing.

Which one a turn is **is decided by GHC**, not a Rust scanner: the extract
parses the statement and reports its bound binders (empty ⇒ EXPR). The binder
NAME and TYPE come from GHC.

The repl already routes `session_eval` → `Session::run_eval`. Wave 3b splits
`run_eval` into the bind path and the reference path on the binder-presence
signal from the extract.

---

## 1. Naming + ids (already scaffolded — do not re-derive)

- `tidepool_repr::SessionModule::val(Generation(g))` → `"Tidepool.Session.Val.G<g>"`.
- The binder's stable id is minted ONCE in Haskell:
  `Translate.stableVarId(name) = 0xFE<<56 | fingerprintString("<module>:<occ>").hi64`.
  For a session binder `x` at gen g: `stableVarId` of the `Name` whose module is
  `Tidepool.Session.Val.G<g>` and occ is `x`.
- Rust **stores** that id (`SessionVarId::from_extract(raw_u64)`); it never
  recomputes the MD5. A later reference turn's Core carries `NVar(same id)` (same
  module:occ ⇒ same hash), so `ExternalEnv[id] = slot` matches by raw equality.
- `BindingTable` (rewritten, `tidepool-codegen/src/binding_table.rs`):
  `bind(BindingEntry) / resolve(name) / live_modules() / seed_external_env()`.
  `BoundValue::{Tier0Forced,Tier1Closure}(RootSlot)`.

---

## 2. Subtree R — codegen: the effectful bind primitive

**File:** `tidepool-codegen/src/jit_machine.rs`. **New method**, sibling of
`run_pure_and_bind`:

```rust
pub fn run_fragment_and_bind<U, H: DispatchEffect<U>>(
    &mut self,
    func_id: FuncId,         // an add_function-minted fragment
    table: &DataConTable,
    handlers: &mut H,
    user: &U,
    forced: bool,            // true = Tier0 (deep_force then tenure); false = Tier1 (tenure as-is)
) -> Result<crate::old_space::RootSlot, JitError>;
```

**Why it must exist (do NOT reuse `run_pure_and_bind` for binds):** a bind turn
compiles `result = do { x <- action; pure x } :: Eff stack T`. The Core is a
freer-simple `Eff` tree, NOT a bare `T`. `run_pure_and_bind` calls the entry
once and roots whatever pointer comes back — for an `Eff` result that is the
`Pure`-leaf wrapper, not the underlying value. The value is only exposed after
the **effect step loop** reduces the tree to `Yield::Done(ptr)` (`ptr` = the
value the final `pure` carries). So the bind primitive must run the effect loop.

**Construction:** clone the structure of `run_with_entry` (the effect step loop,
`Yield::Request` dispatch through `handlers.dispatch`, signal protection) but at
`Yield::Done(ptr)` do the BIND sequence from `run_pure_and_bind` INSTEAD of
`heap_to_value_forcing`:

1. `if forced { ptr = deep_force(vmctx, ptr) }` (signal-protected; check
   `take_runtime_error` after). Tier1 closures are **not** forced.
2. tenure: `from = gc_active_range().expect(...)`; `slot =
   self.session.old_space.tenure(ptr, from_range)` (registers the persistent
   root). Same as `run_pure_and_bind` lines ~891-909.
3. Return `slot`.

**CRITICAL reclaim ordering (UAF risk).** `run_with_entry` arms reclaim
*before* the step loop (line ~406). `run_pure_and_bind` arms reclaim *after*
tenure (line ~916), because the tenure reads `self.session` and the guard holds
a raw `*mut self.session`. For the bind variant you MUST follow the
`run_pure_and_bind` ordering: do NOT arm reclaim before the loop; tenure first,
then `_guard.arm_reclaim(&mut self.session, &vmctx)` LAST, after all
`self.session` access. The effect loop needs a live `vmctx` it owns — keep the
`CompiledEffectMachine`/`vmctx` on this frame through the tenure, and arm reclaim
against that same vmctx pointer at the end. Mirror `run_pure_and_bind`'s SAFETY
comments verbatim where they apply.

`assert!(self.session.is_some(), "...requires a session machine")` like
`run_pure_and_bind`.

**Verify (R):** add a test to `tidepool-codegen/tests/converge_proof.rs` (or a
sibling) that binds via an EFFECTFUL fragment: build a fragment whose Core is an
`Eff`-wrapped value that performs one `Console::Print` then yields the value;
`run_fragment_and_bind(..., forced=true)`; assert the returned slot holds the
right tenured value (reference it from a second fragment, as the existing proof
does). A Tier1 variant: bind a closure (`forced=false`), then a later fragment
applies it. If wiring a real `Eff` Core fixture is heavy, a no-effect `Eff`
(`pure v` shaped tree that still goes through the step loop to `Done`) is an
acceptable smoke — the point is the step loop + bind sequence, not effects.

Keep all existing tests green (`cargo test -p tidepool-codegen`).

---

## 3. Subtree H — Haskell extract: bind mode + reference mode + stmt binders

**Files:** `haskell/src/Tidepool/Binders.hs`, `haskell/src/Tidepool/GhcPipeline.hs`,
`haskell/app/Main.hs`, reuse `haskell/src/Tidepool/Session.hs` (already has
`mkThinSessionIface`/`writeSessionIface`/`injectSessionScope`).

### 3a. Statement binder extraction (extends Binders.hs)

Today `emitBinders` handles top-level *declarations* (Lane A). Add **statement**
binder extraction for a session-eval turn: given the raw turn text, decide
bind-vs-expr and report bound names, parse-only (GHC parser, no typecheck).

- Parse the turn text as a `do`-statement (`parseStmt`-style): a single
  `LStmt`. Classify:
  - `BindStmt pat action` → the binders of `pat` (`collectLStmtBinders` /
    `collectPatBinders`) — kind `"bind"`.
  - `LetStmt binds` → the let-bound names — kind `"bind"`.
  - `BodyStmt expr` (a bare expression) → **no binders** — it's an EXPR turn.
- New CLI mode `--emit-stmt-binders <out.json>` writing:
  ```json
  {"kind":"bind","binders":["x"]}      // or {"kind":"expr","binders":[]}
  ```
  MVP scope: single simple var binder (`x <- e` / `let x = e`). Tuple/pattern
  binds may report multiple names but the bind path (T) handles only the
  single-binder case for the acceptance; report them faithfully regardless.

### 3b. Bind compile mode (extends GhcPipeline + Main)

Rust hands the extract a wrapped module file `result = do { x <- action; pure x }`
(see §4 for the exact wrap; for `let x = e` it is `result = let x = e in pure x`,
also `Eff`-typed). New flags:

```
--session-bind            # this turn binds; capture binder type + write the iface
--bind-name <occ>         # the bound name, e.g. "x"  (from 3a; GHC-sourced)
--bind-gen <g>            # the generation; module = Tidepool.Session.Val.G<g>
--session-root <dir>      # where Val.G<g>.hi is written / earlier Val ifaces read
--inject-val <module>     # repeatable: a live Val.G<g'> module to inject (refs in `action`)
```

Behaviour (`--session-bind`):
1. Build `SessionScope { ssRoot = <session-root>, ssValIfaces = [<--inject-val>...] }`
   and compile `result` via `runPipelineSession (Just scope)` (existing path —
   injects the earlier Val ifaces so `action` may reference earlier bindings).
2. Capture the bound value's TYPE `T`: the type of `result` is `Eff stack T`;
   strip the `Eff`/`M` head to `T` (the result type of the monadic value). Reuse
   the `capturedUserType` machinery (it already reads `__user`'s `idType` and
   renders) — here read `result`'s type and strip the monad. Render `T` to a
   `type_display` string too (for `:t`).
3. Mint the thin iface: `iface <- mkThinSessionIface hsc (SessionModule ValMod
   (Generation g)) [(mkVarOcc bindName, T)]`; `writeSessionIface hsc
   sessionRoot (Val g) iface`. (Both already exist in Session.hs.)
4. Compute the binder id: `stableVarId` of the `Name` minted in that module
   (module `Tidepool.Session.Val.G<g>`, occ `bindName`) — the SAME hash the
   reference turn will compute. Emit it.
5. Emit the BoundBinder record (see §5) AND the Core CBOR for `result` (the
   normal `translateModuleClosed`/whole-module emission targeting `result`), so
   the runtime gets both.

Tier: `Tier1Closure` iff `T` is a function type (`isFunTy T` after stripping
foralls/contexts), else `Tier0Data`.

### 3c. Reference compile mode

Already mostly present: `runPipelineSession (Just scope)` with
`ssValIfaces = [<--inject-val>...]`. Wire the same `--session-root` /
`--inject-val` flags (no `--session-bind`) so a bare-expr turn injects the live
Val ifaces and emits Core for `result` that references the session binders as
externals (`Resolve.isSessionValVar` already routes them to `NVar(stableVarId)`).

### 3d. CLI/runtime invocation summary

The extract is invoked per turn by the runtime (§4). It must keep the existing
non-session modes byte-identical (no `--session-*` flag ⇒ today's behaviour).

---

## 4. Subtree T — runtime + repl wiring (integration; owned by TL)

### 4a. The wrap templates (Rust-side, in repl/runtime)

- EXPR turn: today's `template_haskell(..., expr_text, ...)` targeting `result`.
- BIND `x <- action`: `result = do { x <- action; pure x }` in the session
  module surface (effect stack + imports + injected Val modules in scope).
- BIND `let x = e`: `result = let x = e in pure x`.

The bound name `x` is taken from the §3a stmt-binder report (GHC-sourced), NOT a
Rust regex. Rust only needs the cheap bind-vs-expr signal to pick the template —
which also comes from §3a (`kind`).

### 4b. runtime: a session-aware compile

Extend `tidepool-runtime` with a session-turn entry (new fn or `*_salted`
variant) that:
- runs `--emit-stmt-binders` first (fast, parse-only) → `(kind, binders)`;
- for a BIND, invokes the extract with `--session-bind --bind-name <x>
  --bind-gen <g> --session-root <dir> --inject-val <m>...` and parses the
  `BoundBinder` (§5) + Core + table;
- for an EXPR, invokes with `--session-root <dir> --inject-val <m>...` (no bind),
  returns Core + table.
- Returns `(CoreExpr, DataConTable, MetaWarnings, Vec<BoundBinder>)`.

`DataConTable` merge: the repl keeps a session-accumulated table = union of each
turn's table via `insert_checked` (the loud collision guard); pass the MERGED
table to `run_fragment` and `value_to_json` so a custom-ADT value bound earlier
renders with real con names later.

### 4c. repl `Session` (session.rs): the orchestration

State to add: `bindings: BindingTable`, `gen: Generation`, `session_table:
DataConTable` (merged), `session_root: PathBuf` (where Val ifaces live — under
the existing session include tree).

`run_eval(turn_text)`:
1. Compile (4b) → `(expr, table, warnings, binders)`. Merge `table` into
   `session_table`.
2. **BIND** (`binders` non-empty): bootstrap/`add_function` the fragment with
   `seed_external_env()` (so `action` resolves earlier bindings); call
   `machine.run_fragment_and_bind(fid, &session_table, handlers, captured,
   forced = binder.tier == Tier0)` → `RootSlot`. Construct `BoundValue` by tier,
   `bindings.bind(BindingEntry{ name, id: SessionVarId::from_extract(binder.var_id),
   module: Val(g), value, type_display })`. Bump `gen`. Return
   `TurnOutcome::Bound(name)`. (The thin iface was already written by the
   extract under `session_root`.)
3. **EXPR** (`binders` empty): `env = bindings.seed_external_env()`;
   `add_function(frag, &expr, &session_table, &env)` then `run_fragment(fid,
   &session_table, handlers, captured)` → `Value` → `value_to_json(&v,
   &session_table, 0)`. (First turn still bootstraps the machine via
   `compile_session` if `machine` is None — but note a first turn that is a BIND
   must also bootstrap; see 4d.)

`:bindings` lists `bindings.iter_current()`.

### 4d. First-turn bootstrap

`run_pure_and_bind`/`run_fragment_and_bind` require a session machine
(`compile_session`). If `machine` is `None` on a BIND turn, bootstrap it first
(e.g. `compile_session` on the bind fragment's Core, then bind by running an
`add_function`'d fragment — or `compile_session` then immediately
`add_function`+`run_fragment_and_bind`). Keep the existing first-EXPR-turn
bootstrap working. Simplest: always `compile_session` with a trivial dummy entry
on machine creation (as the converge proof does), then every turn — bind or
expr — is an `add_function` + run. Refactor `run_eval` to that uniform shape if
it reduces special-casing.

### 4e. Acceptance sweep (the headline; real extract env)

New `tidepool-repl` integration test, multi-turn, through the REAL entry point,
natural GC:
1. open session;
2. bind `x <- pure (42 :: Int)`; later turn `x + 1` ⇒ `43` (Tier-0 scalar);
3. bind a JSON `Value` (e.g. `v <- pure (object [...])` or a parsed value);
   a later turn slices/inspects it (Tier-0 structured + DataConTable render);
4. bind `f <- pure (\n -> n + 1)`; later turn `f 10` ⇒ `11` (Tier-1 closure —
   proves prior-fragment code stays callable after `add_function`);
5. interleave enough allocation that a GC fires organically between bind and
   read; assert every binding still resolves/renders AFTER a natural collection.

Real-extract env (see CLAUDE memory + task CONTEXT):
`PATH=/nix/store/anfsdk3cv6yx9b393w92h86c4lhkmh4v-ghc-native-bignum-9.12.2-with-packages/bin:$PATH`,
`TIDEPOOL_EXTRACT=$(cd haskell && cabal list-bin tidepool-extract-bin)`,
`TIDEPOOL_GHC_LIBDIR=$(ghc --print-libdir)`, `rm -rf ~/.cache/tidepool` if the
extract binary changed. Gate the test on `TIDEPOOL_EXTRACT` being set (skip
cleanly otherwise) so `cargo tq` stays green without the nix env.

---

## 5. The BoundBinder boundary (H emits → T consumes)

Per bind turn the extract emits a JSON sidecar (path passed by a flag, e.g.
`--emit-bound-binders <out.json>`), mirroring the existing `--emit-binders`
style:

```json
{"binders":[
  {"name":"x",
   "varId":"18446181123756131000",     // u64 as decimal string (avoid JSON f64 precision loss)
   "module":"Tidepool.Session.Val.G3",
   "tier":"Tier0Data",                   // or "Tier1Closure"
   "typeDisplay":"Int"}
]}
```

`varId` is a DECIMAL STRING of the u64 (JSON numbers are f64 — a 64-bit id loses
precision; serialize as string, parse with `u64::from_str`). T builds
`SessionVarId::from_extract(varId.parse()?)`.

Rust mirror (in `tidepool-runtime`):
```rust
pub struct BoundBinder { pub name: String, pub var_id: u64,
                         pub module: String, pub tier: ValueTier,
                         pub type_display: String }
pub enum ValueTier { Tier0Data, Tier1Closure }
```

---

## 6. Fold order

Wave 1 (parallel): **R** (codegen, own branch) ∥ **H** (haskell, own branch).
Wave 2: TL folds R+H, then builds **T** (runtime+repl+acceptance) on the merged
base and runs the headline sweep with the real extract env. Keep one-shot eval +
Lane-A decl accumulation + the existing repl decl/expr turns green throughout.
