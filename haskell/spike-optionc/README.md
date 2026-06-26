# Option C GO/NO-GO spike

Proves: a value binding's **structured type** can be (1) captured after typecheck
in one GHC session, (2) serialized to a fat `.hi`, and (3) reloaded into a FRESH,
separate batch `runGhc` session so a later module typechecks a *reference* to that
binding — with the reconstructed type **content-identical** to the original (no
`ppr` round-trip). TYPE PLANE ONLY.

## Verdict: **GO** (both simple `Int->Int` and exotic `forall a.(Ord a,Num a)=>a->Map a a`)

## Run

```bash
GHC=/nix/store/<hash>-ghc-native-bignum-9.12.2-with-packages   # or: nix develop
$GHC/bin/ghc -package ghc -O0 -outputdir spike-optionc/build \
    -o spike-optionc/spike spike-optionc/Spike.hs
rm -f spike-optionc/work/*
TIDEPOOL_GHC_LIBDIR=$($GHC/bin/ghc --print-libdir) PATH=$GHC/bin:$PATH \
    ./spike-optionc/spike
```

## What it does

- **Turn 1** (`turn1`): compile a real `Session1.hs` (`g1`, `g2`), GHC writes the
  fat `Session1.hi` (`-fwrite-if-simplified-core`). Capture the original `Type`s.
- **Hardening**: DELETE `Session1.hs` — only `Session1.hi` survives, so turn 2
  cannot recompile from source.
- **Turn 2** (`turn2Injected` → `injectSession1`): fresh `runGhc`, NO
  `InteractiveContext` build of GhciN modules. Read `Session1.hi` by raw path with
  `GHC.Iface.Load.readIface`; reconstruct `TyThing`s with
  `GHC.IfaceToCore.typecheckIface` (inside `initIfaceCheck`); build a
  `HomeModInfo` and push it into the HPT via `hscUpdateHPT`/`addHomeModInfoToHpt`;
  register the module in the finder cache (`addHomeModuleToFinder`, `ml_hs_file =
  Nothing`). Then `setContext [import Session1]` and `exprType` a reference
  expression that genuinely depends on g2's constraints + Map result.
- **Fidelity**: compare original vs reconstructed `Type` by the content-addressed
  `nameStableString` over every `tyConsOfType` (NOT `ppr`, NOT `eqType` — the
  latter compare TyCon Uniques which legitimately differ across NameCaches).
- **Negative control**: `g2 "not a number"` must be REJECTED (proves the injected
  type is load-bearing, not re-inferable).
- **B-comparison**: `ppr` the exotic type, re-parse it.

## Key finding on the injection

`load`/downsweep does NOT honor an HPT-only injection (it re-finds modules by name
and "Could not find module Session1" if there's no source target). The working
path is **HPT inject + `setContext` import + typecheck a reference EXPRESSION**
(`exprType`), bypassing downsweep. `findAndReadIface` also fails (it goes through
the finder, which needs the module on the import path) — use `readIface` with the
raw `.hi` path instead.
