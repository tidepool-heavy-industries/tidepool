# tidepool-repl — Concrete Domain Model

The domain modeled in the type system, written during planning so the design is precise before
impl and the build is fill-in-the-bodies. Companion to `ghci-implementation-plan.md` (§8a states
the principle; this is the realization). Types are grounded in the proven spikes (front-half:
`spike-extract`; back-half: `spike-codegen`). "REVISES" = changes a Wave-0 scaffold type.

Guiding rule: **make illegal states unrepresentable; force exhaustive handling.** No bare
`u64`/`String`/`*mut` where a newtype or sum carries the invariant.

---

## 1. Identifiers (newtypes — never bare)

```rust
// tidepool-repr (or a shared id module)
/// Monotonic per-session generation (= GHCi's ic_mod_index). Only ever bumped.
#[derive(Copy, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
pub struct Generation(pub u64);
impl Generation { pub fn next(self) -> Generation { Generation(self.0 + 1) } }

#[derive(Copy, Clone, PartialEq, Eq, Hash, Debug)]
pub struct SessionId(pub u64);

/// The user-facing name of a binding ("x"). Distinct from its SessionVarId.
#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub struct BindingName(pub String);
```

```haskell
-- haskell extract side (mirror)
newtype Generation = Generation Word64 deriving (Eq, Ord, Show)
newtype BindingName = BindingName Text   deriving (Eq, Ord, Show)
```

## 2. Session module naming — the ONE place gen-versioned module strings are built

```rust
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub enum SessionModuleKind { Val, Lib } // Val = value-binding ifaces; Lib = user decls (Lane A)

#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub struct SessionModule { pub kind: SessionModuleKind, pub gen: Generation }

impl SessionModule {
    /// The ONLY constructor of the module-name string. "Tidepool.Session.Val.G3" etc.
    pub fn module_name(&self) -> String {
        let k = match self.kind { SessionModuleKind::Val => "Val", SessionModuleKind::Lib => "Lib" };
        format!("Tidepool.Session.{}.G{}", k, self.gen.0)
    }
    /// Path of the session .hi this module's iface is written to / read from.
    pub fn hi_path(&self, root: &Path) -> PathBuf { /* root/<module_name>.hi */ }
}
```

```haskell
data SessionModuleKind = ValMod | LibMod deriving (Eq, Show)
data SessionModule = SessionModule SessionModuleKind Generation deriving (Eq, Show)
renderSessionModule :: SessionModule -> ModuleName   -- mkModuleName "Tidepool.Session.Val.G3"
```

## 3. Var identity + the resolution sum (the kimi-B2 fix AS A TYPE)

`VarId` already exists (`tidepool_repr::VarId(u64)`). Add a decoded kind + a smart ctor for session ids.

```rust
/// Decoded high-byte tag of a VarId. Replaces bare byte comparisons in emit/expr.rs.
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub enum VarKind { External /*0xFE*/, ErrorSentinel /*0x45*/, Local /*other*/ }
impl VarId { pub fn kind(self) -> VarKind { /* match self.0 >> 56 */ } }

/// A VarId that names a session binder. Always 0xFE-tagged (a real external under Option C).
#[derive(Copy, Clone, PartialEq, Eq, Hash, Debug)]
pub struct SessionVarId(pub VarId);
impl SessionVarId {
    /// The hash rule lives HERE only: stableVarId("Tidepool.Session.Val.G<g>:<occ>").
    pub fn of(module: &SessionModule, occ: &str) -> SessionVarId { /* 0xFE | hash(module:occ) */ }
}
```

The **exhaustive resolution sum** — Haskell side, drives `Translate`/`Resolve`. The proven spike's
`Resolve.isResolvable` predicate is promoted to this sum so every site handles all three:

```haskell
-- haskell extract: how a free Var resolves during extraction
data VarResolution
  = Inlinable   CoreExpr        -- normal external with an unfolding -> inline (existing path)
  | SessionRef  StableVarId     -- a Tidepool.Session.Val.* binder -> emit direct (NVar id); JIT overrides
  | Unresolved  UnresolvedVar   -- nothing -> error-sentinel node (existing path)

-- the classifier (replaces the boolean isResolvable). SessionScope carries the session module prefix.
classifyVar :: SessionScope -> HscEnv -> Var -> IO VarResolution
-- translateModule then: SessionRef i -> NVar i (NOT added to tsUnresolvedIds, NOT inlined)
```

```rust
// Rust mirror at the emit/expr.rs Var-miss site — exhaustive match, no fallthrough:
enum ResolveOutcome<'a> { SeededExternal(&'a RootSlot), Sentinel(/*kind*/u8), Trap }
// SeededExternal -> load-through (§4); Sentinel -> poison; Trap -> unresolved_var_trap.
```

## 4. Value plane (Rust) — GC-safe by construction

```rust
/// The stable, GC-updated slot a binding's live heap pointer lives in.
/// INVARIANT: the slot address is stable for the binding's life; the copying GC rewrites
/// `*slot` in place on every collection (registered via register_persistent_root). Value
/// resolution LOADS THROUGH this slot — never snapshots the pointer (spike-codegen GC fix).
#[derive(Copy, Clone, Debug)]
pub struct RootSlot(*mut *mut u8);
impl RootSlot {
    /// SAFETY: slot valid + registered as a persistent GC root until the session machine drops.
    pub unsafe fn current(self) -> *mut u8 { *self.0 }
    pub fn addr(self) -> *mut *mut u8 { self.0 } // the iconst'd immediate; load emitted at the Var-miss site
}

/// Models the strict-force-vs-store-as-is distinction at the type level.
#[derive(Copy, Clone, Debug)]
pub enum BoundValue {
    Tier0Forced(RootSlot),  // first-order data, deep_force'd to NF then tenured
    Tier1Closure(RootSlot), // closure/PAP — stored as-is, valid while the machine lives
}
impl BoundValue { pub fn root(&self) -> RootSlot { match self { Tier0Forced(r)|Tier1Closure(r) => *r } } }

/// REVISES tidepool-codegen/src/binding_table.rs BindingEntry (drops 0xFD value_id + type_string).
pub struct BindingEntry {
    pub name: BindingName,
    pub id: SessionVarId,        // 0xFE stableVarId of module:occ
    pub module: SessionModule,   // Tidepool.Session.Val.G<g> — derives the .hi path; what to inject
    pub value: BoundValue,       // the live heap root (GC-safe slot)
    pub type_display: Option<String>, // ppr for :t only; the STRUCTURED type lives in the .hi on disk
}

/// REVISES the scaffold BindingTable. name -> latest id; id -> entry (all live, incl. shadowed).
pub struct BindingTable {
    current: HashMap<BindingName, SessionVarId>, // shadowing: name -> newest gen's id
    live:    HashMap<SessionVarId, BindingEntry>, // every still-rooted binding (old gens kept)
    gen:     Generation,
}

/// What codegen consults at a Var-miss. id -> the slot to LOAD THROUGH (not a snapshot pointer).
/// REVISES the scaffold ExternalEnv (was VarId->*const u8; now VarId->RootSlot for GC-safety).
pub struct ExternalEnv(HashMap<VarId, RootSlot>);
```

## 5. Session surface + lifecycle (Rust, `tidepool-repl`)

```rust
pub enum SessionCommand {
    Def(DeclText),      // session_def: append a declaration to the Lib decl-log
    Eval(ExprText),     // session_eval: evaluate (may bind via `x <- e` / `let x = e`)
    Cmd(MetaCommand),   // session_cmd
    Close,              // session_close
}
pub enum MetaCommand { Type(ExprText) /*:t*/, Info(String) /*:i*/, Bindings /*:bindings*/, Reset }

pub struct DeclText(pub String);
pub struct ExprText(pub String);

/// The resident session. Owns the live machine + both planes. One per active session (MVP: 1).
pub struct Session {
    machine:  JitEffectMachine,  // value plane (heap persists across turns)
    bindings: BindingTable,      // the bridge
    decl_log: DeclLog,           // ordered user decls -> regenerates Lib.G<g> modules (Lane A)
    gen:      Generation,
    id:       SessionId,
}

/// Type-state so post-close ops don't typecheck (the worker owns Session<Open>; close consumes it).
pub struct Open; pub struct Closed;
pub struct SessionHandle<S> { inner: Session, _s: PhantomData<S> }
impl SessionHandle<Open> {
    pub fn run(&mut self, cmd: SessionCommand) -> TurnOutcome { /* … */ }
    pub fn close(self) -> SessionHandle<Closed> { /* drop machine, free_session_heap */ }
}

pub enum TurnOutcome { Value(serde_json::Value), Bound(BindingName), Suspended(AskPrompt), Error(String) }
```

## 6. The extract↔runtime contract (per turn)

The one interface between the Haskell extract and the Rust runtime. Bind turns return binders;
pure reference turns return none.

```haskell
-- haskell extract output (CBOR -> Rust)
data TurnResult = TurnResult
  { trCore     :: CoreExpr            -- the JIT-able Core for this turn
  , trBinders  :: [BoundBinder]       -- names this turn binds ([] for a pure expression turn)
  , trTable    :: DataConTable        -- this turn's cons; Rust merges into the session table (component N)
  , trWarnings :: [Text]
  }

data BoundBinder = BoundBinder
  { bbName        :: BindingName       -- "x"
  , bbVarId       :: StableVarId       -- SessionVarId.of(module, occ)
  , bbModule      :: SessionModule     -- Tidepool.Session.Val.G<g>  (its thin .hi is written as a side effect)
  , bbTier        :: ValueTier         -- Tier0Data | Tier1Closure (from the type/forcing analysis)
  , bbTypeDisplay :: Text              -- ppr of the type, for :t
  }

data ValueTier = Tier0Data | Tier1Closure deriving (Eq, Show)

-- extract INPUT: what to compile this turn + which session state to bring into scope
data TurnInput = TurnInput
  { tiKind        :: TurnKind          -- DeclTurn | EvalTurn (explicit tool, no classifier)
  , tiSource      :: Text
  , tiGen         :: Generation
  , tiInjectVal   :: [SessionModule]   -- live value-binding ifaces to readIface+HPT-inject
  , tiLibModules  :: [SessionModule]   -- Lib.G<g> decl modules on the include path
  }
data TurnKind = DeclTurn | EvalTurn deriving (Eq, Show)
```

```rust
// Rust mirror of the boundary
pub struct TurnResult { pub core: CoreExpr, pub binders: Vec<BoundBinder>,
                        pub table: DataConTable, pub warnings: Vec<String> }
pub struct BoundBinder { pub name: BindingName, pub id: SessionVarId, pub module: SessionModule,
                         pub tier: ValueTier, pub type_display: String }
pub enum ValueTier { Tier0Data, Tier1Closure }
```

## 7. Haskell extract internals (the proven-spike mechanisms, typed)

```haskell
-- SessionScope: what the extract needs to recognize + inject session state this turn.
data SessionScope = SessionScope
  { ssSelf       :: Maybe SessionModule   -- the module this turn's binders go into (bind turns)
  , ssValIfaces  :: [SessionModule]       -- inject these (readIface raw path -> typecheckIface -> HPT)
  }

-- The thin iface for a value binding (spike B1: no mi_extra_decls, no ifIdUnfolding).
mkThinSessionIface :: HscEnv -> SessionModule -> [(OccName, Type)] -> IO ModIface
-- write/inject (proven in spike-extract / spike-optionc):
writeSessionIface  :: SessionModule -> ModIface -> IO ()
injectSessionIface :: HscEnv -> SessionModule -> IO HscEnv   -- readIface + addHomeModInfoToHpt + addHomeModuleToFinder ml_hs_file=Nothing

-- isResolvable promoted to the VarResolution sum; SessionRef short-circuits inlining + sentinel:
classifyVar :: SessionScope -> HscEnv -> Var -> IO VarResolution
isSessionBinder :: SessionScope -> Name -> Bool   -- module ∈ Tidepool.Session.Val.*
```

## 8. Where each type lives / revises

| Type | Crate / module | New / Revises |
|---|---|---|
| `Generation`,`SessionId`,`BindingName`,`SessionVarId`,`VarKind` | `tidepool-repr` (ids) | new |
| `SessionModule(Kind)` | `tidepool-repr` + Haskell mirror | new |
| `RootSlot`,`BoundValue`,`BindingEntry`,`BindingTable`,`ExternalEnv` | `tidepool-codegen` | **REVISES** `binding_table.rs`, `emit/mod.rs` ExternalEnv |
| `ResolveOutcome` (Var-miss) | `tidepool-codegen/emit/expr.rs` | new (exhaustive match) |
| `SessionCommand`,`MetaCommand`,`Session`,`SessionHandle<S>`,`TurnOutcome` | `tidepool-repl` | new |
| `TurnResult`,`BoundBinder`,`ValueTier`,`TurnInput`,`TurnKind` | boundary: Haskell + `tidepool-runtime` | new |
| `VarResolution`,`classifyVar`,`SessionScope`,`mkThinSessionIface`,`inject…` | `haskell/src/Tidepool/{Resolve,Translate,Session}.hs` | promotes spike's `isResolvable`; new Session module |

## 9. Invariants the types enforce (the payoff)

- A session binder reference can only be `SessionRef`/`SessionBinding` → exhaustively distinct from
  `Inlinable`/`Unresolved`; the B2 mis-route (sentinel) is now a non-exhaustive-match compile error.
- A value can only be reached through a `RootSlot` (load-through) → the GC-stale-pointer bug is
  unrepresentable (no `from_external_pointer(raw)` in the API).
- A gen-versioned module string can only come from `SessionModule::module_name` → no drift.
- A binding's id can only be minted by `SessionVarId::of` → the `module:occ` hash rule is single-source.
- Post-`close` operations don't typecheck (`SessionHandle<Closed>` has no `run`).
- `BoundValue` makes the Tier-0/Tier-1 forcing decision explicit at every use.
