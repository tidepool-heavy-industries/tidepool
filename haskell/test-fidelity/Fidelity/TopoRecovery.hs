-- | The topological-recovery-order regression (haskell/CLAUDE.md's Known
-- Limits "failing generated Tidepool.Effects module cascades" entry).
--
-- 'Tidepool.GhcPipeline.normalVariant''s per-module recovery loop redoes each
-- summary's own 'parseModule'\/'typecheckModule' INDEPENDENTLY of the earlier
-- 'load'' call, so when a genuine failure throws a spanned 'SourceError' it
-- stops that loop immediately. Pre-fix, the loop walked summaries in whatever
-- order 'mgModSummaries' happened to hold them — NOT dependency order — so
-- whichever of a failing module and its dependent the loop visited FIRST
-- decided what a caller saw: visit the dependent first and its own typecheck
-- chokes on the failing import with GHC's generic "attempting to use module
-- `X` ... which is not loaded", and the failing module's real diagnostic is
-- never reached at all.
--
-- Both checks below pick module names that sort alphabetically DEPENDENT-
-- before-DEPENDENCY (@A...@ imports @Z...@) — empirically confirmed (see the
-- fix's commit) to reproduce the pre-fix ordering that triggers the cascade
-- reliably, not by luck.
module Fidelity.TopoRecovery (checks) where

import Fidelity.Harness (Check, check, extractBinding)

import Data.List (isInfixOf)
import System.Directory (createDirectoryIfMissing)

checks :: IO [Check]
checks = sequence [ maskedDependencyCheck, voidShimCheck ]

-- | The minimal two-module reproduction: 'ZTopoBroken' has a genuine type
-- error of its own; 'ATopoTarget' imports it and does nothing else unusual.
-- Post-fix, the recovery loop always reaches 'ZTopoBroken' — which has no
-- unresolved dependency of its own — before it can ever reach 'ATopoTarget',
-- so 'ZTopoBroken''s real diagnostic fires and the loop never gets far enough
-- to produce the "not loaded" noise at all.
maskedDependencyCheck :: IO Check
maskedDependencyCheck = do
  let dir = "test-fidelity/work/" ++ tag
  createDirectoryIfMissing True dir
  writeFile (dir ++ "/ZTopoBroken.hs") brokenSrc
  result <- extractBinding tag "ATopoTarget" targetSrc "target"
  let (ok, err) = case result of
        Left e  -> ( "ZTopoBroken.hs" `isInfixOf` e
                     && "Couldn't match type" `isInfixOf` e
                     && not (maskMsg `isInfixOf` e)
                   , e )
        Right _ -> (False, "<extraction SUCCEEDED — expected a loud failure>")
  pure $ check
    ("topological recovery: the failing dependency's own diagnostic surfaces, \
     \not the dependent's \"not loaded\" cascade: " ++ err)
    ok
  where
    tag = "topo-recovery-mask"
    maskMsg = "which is not loaded"

brokenSrc :: String
brokenSrc = unlines
  [ "module ZTopoBroken where"
  , ""
  , "brokenValue :: Int"
  , "brokenValue = \"not an Int\""
  ]

targetSrc :: String
targetSrc = unlines
  [ "module ATopoTarget where"
  , ""
  , "import ZTopoBroken (brokenValue)"
  , ""
  , "target :: Int"
  , "target = brokenValue"
  ]

-- | The specific real-world shape behind the incident this fix was written
-- for: a GENERATED effects-shim module whose defect is an UNRESOLVED NAME in
-- its own @type M@ row (haskell/CLAUDE.md's Known Limits: "its @type M@ row
-- names an unresolved type") — not a type mismatch, a distinct failure shape
-- from 'maskedDependencyCheck' above — imported by an ordinary user module.
-- Same masking mechanism (module-name sort order), same fix, but pinning that
-- the fix covers an unresolved-NAME failure inside the shim, not just a
-- type-mismatch one. 'ZShimGenerated' stands in for the real generated
-- @Tidepool.Effects@ shim without touching haskell/lib/ or any real
-- generated module.
voidShimCheck :: IO Check
voidShimCheck = do
  let dir = "test-fidelity/work/" ++ tag
  createDirectoryIfMissing True dir
  writeFile (dir ++ "/ZShimGenerated.hs") shimSrc
  result <- extractBinding tag "AUserModule" userSrc "target"
  let (ok, err) = case result of
        Left e  -> ( "ZShimGenerated.hs" `isInfixOf` e
                     && "Not in scope" `isInfixOf` e
                     && not (maskMsg `isInfixOf` e)
                   , e )
        Right _ -> (False, "<extraction SUCCEEDED — expected a loud failure>")
  pure $ check
    ("topological recovery: an unresolved-name shim failure surfaces, not the \
     \importing user module's cascade: " ++ err)
    ok
  where
    tag = "topo-recovery-shim"
    maskMsg = "which is not loaded"

shimSrc :: String
shimSrc = unlines
  [ "module ZShimGenerated where"
  , ""
  , "type M = NotARealEffectRow"
  , ""
  , "shimValue :: Int"
  , "shimValue = 42"
  ]

userSrc :: String
userSrc = unlines
  [ "module AUserModule where"
  , ""
  , "import ZShimGenerated (shimValue)"
  , ""
  , "target :: Int"
  , "target = shimValue"
  ]
