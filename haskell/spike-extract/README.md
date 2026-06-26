# Option-C binder-resolution FRONT-HALF spike (extract / back-end half)

Proves the **value-plane** counterpart to `spike-optionc` (which proved the
type plane). A reference to a tidepool-repl session value binding
(`Tidepool.Session.Val.G1.x`) reaches tidepool's **Core** as a known external

    NVar (stableVarId "Tidepool.Session.Val.G1:x")

— a `0xFE`-tagged external id — **not inlined** (kimi B1), **not error-sentinel
rewritten** (kimi B2), via a **thin** injected iface, with the reference module
compiled through the **normal module pipeline** to Core (kimi B4, *not* GHCi's
`tcRnStmt`).

## Verdict: **GO** (simple `Int -> Int` AND exotic `forall a.(Ord a,Num a)=>a->Map a a`)

| Check | x (simple) | xe (exotic) |
|---|---|---|
| thin iface (`mi_extra_decls = Nothing`) | ✓ | ✓ |
| thin binder (`realIdUnfolding = NoTemplate`) | ✓ | ✓ |
| Core emitted via normal pipeline (B4) | ✓ | ✓ |
| emitted `NVar == stableVarId(name)` | ✓ `0xfeb6f67ceb1fe189` | ✓ `0xfe9f3f1fc94371c9` |
| external `0xFE`-tagged | ✓ | ✓ |
| **NOT** `0x45` sentineled (B2) | ✓ | ✓ |
| **NOT** in `tsUnresolvedIds` | ✓ | ✓ |

Negative control (the fix is load-bearing): with the one-line `isSessionValVar`
exclusion removed from `Resolve.hs`, both references collapse to the
`0x4500000000000004` sentinel — exactly the kimi B2 failure — so the GO is not a
false positive.

## Run

```bash
GHC=/nix/store/<hash>-ghc-native-bignum-9.12.2-with-packages   # or: nix develop
cd haskell
cabal build spike-extract --with-compiler=$GHC/bin/ghc
rm -rf spike-extract/work
TIDEPOOL_GHC_LIBDIR=$($GHC/bin/ghc --print-libdir) PATH=$GHC/bin:$PATH \
    $(cabal list-bin spike-extract --with-compiler=$GHC/bin/ghc)
```

## What it does (per binder type)

1. **WRITE** — compile a session home module `Tidepool.Session.Val.G1` with a
   **thin** iface: no `-fwrite-if-simplified-core` (⇒ no `mi_extra_decls`) and
   `-fomit-interface-pragmas` (⇒ no `ifIdUnfolding`).
2. **HARDEN** — delete the session **source** so turn 2 can only resolve from
   the thin `.hi`.
3. **INJECT** — fresh `runGhc`: `readIface` (raw path) → `typecheckIface`
   (`initIfaceCheck`) → `HomeModInfo` → HPT (`addHomeModInfoToHpt`) + finder
   cache (`addHomeModuleToFinder`, `ml_hs_file = Nothing`) — the `spike-optionc`
   path. Verifies the reconstructed binder carries no unfolding.
4. **EMIT** — compile reference module `Use` (imports the session module, uses
   the binder) to Core via **`summariseFile` + `typecheckModule` + `hscDesugar`
   + `core2core`**. `summariseFile` summarises a single file *without* downsweep,
   which is what dodges the `spike-optionc` blocker ("downsweep re-finds modules
   and rejects the source-less session module"). Core is actually produced.
5. **RESOLVE** — run that Core through tidepool's **real**
   `Tidepool.Translate.translateModuleClosed` (varId / `resolveExternals` /
   `translateModule`) and assert the emitted session-binder reference.

## The minimal extract change this spike validates

`Resolve.hs` — `isResolvable` now excludes `Tidepool.Session.Val.*` vars
(`isSessionValVar`). This single exclusion handles **both** kimi findings:
resolving would (B1) try to inline a body that isn't there, and failing to
resolve would (B2) drop the binder into `tsUnresolvedIds` → Translate emits the
`0x45` sentinel, discarding the session id. Excluding the var leaves the bare
external `Var` in place, which `varId`/`translateHead` already emit as
`NVar (stableVarId name)` (Translate.hs:1434-1435, 1167) — the contract id.

Plus the **thin iface** (no `mi_extra_decls` / no unfolding) so GHC itself never
inlines at the source level. The two together = the realized contract.

## R4 — instance replay

The exotic `xe (n+1)` needs `(Ord Int, Num Int)` at the **use site**. Those
dictionaries are resolved by GHC's typechecker from instances **in scope in the
reference module** (base's `Num`/`Ord Int`), *not* from the session iface — so
for binders whose type mentions only library classes/types, **no `mi_insts`
replay is required**. Replay becomes necessary only when the needed instance is
a **session/user-defined (orphan)** instance (e.g. `instance Show MySessionType`
from a prior turn): then the injected session iface (or the in-scope
`Tidepool.Session.Lib.G<g>` module) must carry that `mi_insts` entry. Standard
instances: free. User/orphan instances: replay required (plan §7.2).
