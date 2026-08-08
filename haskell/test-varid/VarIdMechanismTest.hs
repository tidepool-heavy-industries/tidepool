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

import GHC.Types.Name (mkExternalName)
import GHC.Types.Name.Occurrence (mkRecFieldOccFS, mkVarOccFS)
import GHC.Types.Unique (mkUniqueGrimily)
import GHC.Unit.Types (mkModule, stringToUnit)
import GHC.Unit.Module (mkModuleName)
import GHC.Data.FastString (fsLit)
import GHC.Types.SrcLoc (noSrcSpan)

import Tidepool.Translate (stableVarId, fieldParentDisamb, normalizeMod, checkedKeyToIdx)

import Control.Exception (SomeException, evaluate, try)
import Control.Monad (forM_, unless)
import qualified Data.Map.Strict as Map
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

  unless (all snd checks && all snd collisionChecks) exitFailure
  putStrLn "all varId-mechanism checks passed"
  exitSuccess
