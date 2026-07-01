{-# LANGUAGE ScopedTypeVariables #-}

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
--      types get DISTINCT disambiguators — @"@Hit"@ vs @"@Doc"@ — hence distinct
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

import GHC.Types.Name (mkExternalName)
import GHC.Types.Name.Occurrence (mkRecFieldOccFS, mkVarOccFS)
import GHC.Types.Unique (mkUniqueGrimily)
import GHC.Unit.Types (mkModule, stringToUnit)
import GHC.Unit.Module (mkModuleName)
import GHC.Data.FastString (fsLit)
import GHC.Types.SrcLoc (noSrcSpan)

import Tidepool.Translate (stableVarId, fieldParentDisamb)

import Control.Monad (forM_, unless)
import System.Exit (exitFailure, exitSuccess)

main :: IO ()
main = do
  -- All three Names share module "Tidepool.Records" and occ string "path";
  -- only the OccName namespace (FldName parent vs VarName) differs.
  let recMod   = mkModule (stringToUnit "main") (mkModuleName "Tidepool.Records")
      mkNm u o = mkExternalName (mkUniqueGrimily u) recMod o noSrcSpan
      pathHit  = mkNm 1 (mkRecFieldOccFS (fsLit "Hit") (fsLit "path"))
      pathDoc  = mkNm 2 (mkRecFieldOccFS (fsLit "Doc") (fsLit "path"))
      plain    = mkNm 3 (mkVarOccFS (fsLit "path"))

      checks :: [(String, Bool)]
      checks =
        [ ("field parent Hit -> \"@Hit\"", fieldParentDisamb pathHit == "@Hit")
        , ("field parent Doc -> \"@Doc\"", fieldParentDisamb pathDoc == "@Doc")
        , ("shared label, different parent -> distinct disamb",
            fieldParentDisamb pathHit /= fieldParentDisamb pathDoc)
        , ("non-field -> empty disamb", fieldParentDisamb plain == "")
        , ("shared label, different parent -> distinct stableVarId",
            stableVarId pathHit /= stableVarId pathDoc)
        ]

  forM_ checks $ \(label, ok) ->
    putStrLn ((if ok then "ok   - " else "FAIL - ") ++ label)

  unless (all snd checks) exitFailure
  putStrLn "all varId-mechanism checks passed"
  exitSuccess
