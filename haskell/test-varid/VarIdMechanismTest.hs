{-# LANGUAGE ScopedTypeVariables #-}
{-# LANGUAGE PatternSynonyms #-}

-- | Contract pin for the record-selector varId disambiguator
-- ('Tidepool.Translate.fieldParentDisamb' + 'stableVarId').
--
-- Unlike the @varid_audit_dup_record_fields_zero_collisions@ canary in
-- @tidepool-runtime/tests/gotcha_registry.rs@ (which passes both pre- and
-- post-fix, because home-module selectors carry 'RecSelId' either way), this
-- test pins the CURRENT mechanism's contract directly, with no dependence on the
-- historical (stale-binary) collision:
--
--   1. Two field selectors sharing a label ('path') but under DIFFERENT parent
--      types get DISTINCT disambiguators — @"@Hit"@ vs @"@FileRead"@ — hence distinct
--      'stableVarId's even though module + occ name coincide.
--   2. A non-field 'Name' gets the EMPTY disambiguator, so 'stableVarId' stays
--      byte-identical to the original scheme (no DataConTable / fixture drift).
--
-- If someone reverts 'fieldParentDisamb' to a constant @""@ (or otherwise breaks
-- the FldName-namespace read), check (1) fails. Exit code is non-zero unless
-- every check passes.
--
-- Run: @cabal run varid-mechanism-test@ (needs the nix with-packages GHC on
-- PATH; same toolchain as @session-c-test@).
module Main (main) where

import GHC.Types.Name (mkExternalName, mkInternalName)
import GHC.Types.Name.Occurrence (mkRecFieldOccFS, mkVarOccFS)
import GHC.Types.Unique (mkUniqueGrimily)
import GHC.Unit.Types (mkModule, stringToUnit)
import GHC.Unit.Module (mkModuleName)
import GHC.Data.FastString (fsLit)
import GHC.Types.SrcLoc (noSrcSpan)
import GHC.Types.Var (Var)
import GHC.Types.Id (mkLocalId, mkSysLocal)
import GHC.Core.Multiplicity (pattern ManyTy)
import GHC.Builtin.Types (intTy)
import GHC.Core (Bind(..), Expr(..), CoreBind)

import Tidepool.Translate (stableVarId, fieldParentDisamb, normalizeMod, checkedKeyToIdx, varId, stabilizeLocalUniques, translateModule)

import Control.Exception (SomeException, evaluate, try)
import Control.Monad (forM_, unless)
import Data.Word (Word64)
import qualified Data.Map.Strict as Map
import qualified Data.Set as Set
import qualified Data.Text as T
import System.Exit (exitFailure, exitSuccess)

main :: IO ()
main = do
  -- All three Names share module "Tidepool.Records" and occ string "path";
  -- only the OccName namespace (FldName parent vs VarName) differs.
  let recMod   = mkModule (stringToUnit "main") (mkModuleName "Tidepool.Records")
      mkNm u o = mkExternalName (mkUniqueGrimily u) recMod o noSrcSpan
      pathHit  = mkNm 1 (mkRecFieldOccFS (fsLit "Hit") (fsLit "path"))
      pathFileRead  = mkNm 2 (mkRecFieldOccFS (fsLit "FileRead") (fsLit "path"))
      plain    = mkNm 3 (mkVarOccFS (fsLit "path"))

      -- Module-scoping checks for 'normalizeMod' / 'stableVarId' / 'checkedKeyToIdx'.
      nmIn m u o = mkExternalName (mkUniqueGrimily u)
                     (mkModule (stringToUnit "main") (mkModuleName m)) (mkVarOccFS (fsLit o)) noSrcSpan

      -- (a) a user module merely containing "Internal" as a path segment
      -- must NOT alias against its non-"Internal" counterpart.
      fooInternalBarThing = nmIn "Foo.Internal.Bar" 10 "thing"
      fooBarThing         = nmIn "Foo.Bar" 11 "thing"

      -- (b) bare "Internal" / "Internal.Foo" are not entries in the
      -- allowlist and must pass through normalizeMod unchanged.
      -- (c) the allowlist's real entries still alias, both as strings and
      -- (via 'stableVarId') as ids for the same occurrence name.
      dataTextInternalEmpty = nmIn "Data.Text.Internal" 20 "empty"
      dataTextEmpty         = nmIn "Data.Text" 21 "empty"
      ghcInternalMaybeJust  = nmIn "GHC.Internal.Maybe" 22 "Just"
      ghcMaybeJust          = nmIn "GHC.Maybe" 23 "Just"

      checks :: [(String, Bool)]
      checks =
        [ ("field parent Hit -> \"@Hit\"", fieldParentDisamb pathHit == "@Hit")
        , ("field parent FileRead -> \"@FileRead\"", fieldParentDisamb pathFileRead == "@FileRead")
        , ("shared label, different parent -> distinct disamb",
            fieldParentDisamb pathHit /= fieldParentDisamb pathFileRead)
        , ("non-field -> empty disamb", fieldParentDisamb plain == "")
        , ("shared label, different parent -> distinct stableVarId",
            stableVarId pathHit /= stableVarId pathFileRead)
        , ("Foo.Internal.Bar vs Foo.Bar -> distinct stableVarId",
            stableVarId fooInternalBarThing /= stableVarId fooBarThing)
        , ("bare \"Internal\" not rewritten", normalizeMod "Internal" == "Internal")
        , ("\"Internal.Foo\" not rewritten", normalizeMod "Internal.Foo" == "Internal.Foo")
        , ("Data.Text.Internal -> Data.Text", normalizeMod "Data.Text.Internal" == "Data.Text")
        , ("GHC.Internal.Maybe -> GHC.Maybe", normalizeMod "GHC.Internal.Maybe" == "GHC.Maybe")
        , ("Data.Text.Internal/Data.Text empty -> same stableVarId",
            stableVarId dataTextInternalEmpty == stableVarId dataTextEmpty)
        , ("GHC.Internal.Maybe/GHC.Maybe Just -> same stableVarId",
            stableVarId ghcInternalMaybeJust == stableVarId ghcMaybeJust)
        ]

  forM_ checks $ \(label, ok) ->
    putStrLn ((if ok then "ok   - " else "FAIL - ") ++ label)

  -- (d) checkedKeyToIdx: distinct qualified names sharing an id error loudly;
  -- the SAME qualified name recorded twice (same entity, revisited) is silent.
  collisionResult <- try (evaluate (Map.size (checkedKeyToIdx
    [(0x1, T.pack "Mod.A"), (0x1, T.pack "Mod.B")]))) :: IO (Either SomeException Int)
  sameNameResult <- try (evaluate (Map.size (checkedKeyToIdx
    [(0x1, T.pack "Mod.A"), (0x1, T.pack "Mod.A")]))) :: IO (Either SomeException Int)

  let collisionChecks :: [(String, Bool)]
      collisionChecks =
        [ ("checkedKeyToIdx errors on distinct qualified names sharing an id",
            either (const True) (const False) collisionResult)
        , ("checkedKeyToIdx is silent on the same qualified name sharing an id",
            either (const False) (== 1) sameNameResult)
        ]

  forM_ collisionChecks $ \(label, ok) ->
    putStrLn ((if ok then "ok   - " else "FAIL - ") ++ label)

  -- (e) 'Tidepool.Translate.stabilizeLocalUniques' — the mechanism behind
  -- the cold/warm build-products-dir fix: a nested Id's VarId must depend
  -- only on its TRAVERSAL POSITION, never on the raw magnitude of the GHC
  -- Unique a particular compile session happened to assign it.
  let mkLocalVar :: Word64 -> String -> Var
      mkLocalVar u nm = mkSysLocal (fsLit nm) (mkUniqueGrimily u) ManyTy intTy
      mkTopVar u nm = mkLocalId
        (mkInternalName (mkUniqueGrimily u) (mkVarOccFS (fsLit nm)) noSrcSpan)
        ManyTy
        intTy

      -- \x -> x, built twice with DIFFERENT starting uniques for `x` —
      -- standing in for a cold vs. a warm compile's differing session-wide
      -- Unique consumption before reaching this same logical binder.
      identityWith :: Word64 -> CoreBind
      identityWith xUniq =
        let x = mkLocalVar xUniq "x"
            topB = mkLocalVar 999 "top"
        in NonRec topB (Lam x (Var x))

      binderOccVarIds :: CoreBind -> Maybe (Word64, Word64)
      binderOccVarIds (NonRec _ (Lam b (Var v))) = Just (varId b, varId v)
      binderOccVarIds _ = Nothing

      agrees :: Maybe (Word64, Word64) -> Bool
      agrees = maybe False (uncurry (==))

      [stabCold] = stabilizeLocalUniques [identityWith 500]
      [stabWarm] = stabilizeLocalUniques [identityWith 90210]
      coldIds = binderOccVarIds stabCold
      warmIds = binderOccVarIds stabWarm

      -- Two sibling local binders sharing an OccName ("x") at DISTINCT
      -- traversal positions: \x1 -> (\x2 -> x2) x1. Positional
      -- disambiguation must tell them apart even though
      -- 'stabilizeLocalUniques' discards their original (here: also
      -- distinct, but irrelevantly so) GHC uniques entirely.
      siblingBind :: CoreBind
      siblingBind =
        let x1 = mkLocalVar 10 "x"
            x2 = mkLocalVar 20 "x"
            topB = mkLocalVar 999 "top"
        in NonRec topB (Lam x1 (App (Lam x2 (Var x2)) (Var x1)))

      siblingVarIds :: CoreBind -> Maybe (Word64, Word64)
      siblingVarIds (NonRec _ (Lam x1' (App (Lam x2' (Var _)) (Var _)))) =
        Just (varId x1', varId x2')
      siblingVarIds _ = Nothing

      [stabSibling] = stabilizeLocalUniques [siblingBind]

      targetBind =
        let x = mkLocalVar 30 "x"
            topB = mkTopVar 998 "target"
        in NonRec topB (Lam x (Var x))
      unusedBind =
        let x = mkLocalVar 40 "unusedX"
            topB = mkTopVar 997 "unused"
        in NonRec topB (Lam x (Var x))
      (withUnused, _, _, _, _) =
        translateModule [unusedBind, targetBind] "target" Set.empty
      (withoutUnused, _, _, _, _) =
        translateModule [targetBind] "target" Set.empty

      stabChecks :: [(String, Bool)]
      stabChecks =
        [ ("stabilizeLocalUniques: binder and occurrence agree post-stabilization (cold)",
            agrees coldIds)
        , ("stabilizeLocalUniques: binder and occurrence agree post-stabilization (warm)",
            agrees warmIds)
        , ("stabilizeLocalUniques: same source, different starting session uniques -> same stabilized varId",
            coldIds == warmIds && coldIds /= Nothing)
        , ("stabilizeLocalUniques: sibling binders sharing an OccName at distinct positions -> distinct varId",
            case siblingVarIds stabSibling of
              Just (v1, v2) -> v1 /= v2
              Nothing -> False)
        , ("translateModule: unreachable bindings do not perturb emitted VarIds",
            withUnused == withoutUnused)
        ]

  forM_ stabChecks $ \(label, ok) ->
    putStrLn ((if ok then "ok   - " else "FAIL - ") ++ label)

  unless (all snd checks && all snd collisionChecks && all snd stabChecks) exitFailure
  putStrLn "all varId-mechanism checks passed"
  exitSuccess
