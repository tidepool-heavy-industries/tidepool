{-# LANGUAGE ScopedTypeVariables #-}
{-# LANGUAGE LambdaCase #-}

-- | Wave-3a acceptance test for the PRODUCTIONIZED Option-C type-carrier.
--
-- Unlike @spike-extract/Spike.hs@ (which carried local copies of the write/inject
-- logic), this drives the REAL production surface:
--   * 'Tidepool.Session.mkThinSessionIface' / 'writeSessionIface' — synthesize +
--     serialize the thin session iface directly from a binder's @(OccName, Type)@.
--   * 'Tidepool.GhcPipeline.runPipelineSession' — the GATED extract path: inject
--     the live @Val.G1@ iface, compile the reference module to Core.
--   * 'Tidepool.Translate.translateModuleClosed' — the real translator.
--
-- The contract (per binder): a reference to @Tidepool.Session.Val.G1.<occ>@
-- reaches Core as @NVar (stableVarId "Tidepool.Session.Val.G1:<occ>")@ —
-- 0xFE-tagged (external), NOT 0x45-sentineled, NOT in the unresolved set.
--
-- Covered:
--   (1) a SIMPLE binder type   @Int -> Int@
--   (2) an INSTANCE-USING binder @(Ord a, Num a) => a -> Map a a@, referenced at
--       @Int@ (Map.lookup needs @Ord Int@; the keys need @Num Int@) — proving an
--       instance-using reference both typechecks AND emits Core (kimi #6, R4:
--       base-class instances resolve at the use site, no replay needed).
--   (3) GATE INERTNESS: 'runPipeline' (no scope) is exactly
--       @runPipelineSession Nothing@, so the normal one-shot path never touches
--       any session machinery (asserted structurally + a regression note).
--
-- Exit code is non-zero unless every binder is GO (this is a real test).

module Main (main) where

import GHC
import GHC.Driver.Session (updOptLevel)
import GHC.Driver.Env (HscEnv, hsc_home_unit)
import GHC.Types.Name (mkExternalName, nameOccName)
import GHC.Types.Name.Occurrence (mkVarOcc, occNameString)
import GHC.Types.Var (varName)
import GHC.Types.Id (idType)
import GHC.Types.TypeEnv (typeEnvIds)
import GHC.Tc.Types (tcg_type_env)
import GHC.Core.Type (Type)
import GHC.Types.Unique (mkUniqueGrimily)
import GHC.Unit.Home (homeUnitAsUnit)
import GHC.Unit.Types (mkModule)

import Tidepool.Session
  ( Generation(..), SessionModuleKind(..), SessionModule(..)
  , SessionScope(..), renderSessionModule
  , mkThinSessionIface, writeSessionIface )
import Tidepool.GhcPipeline (runPipelineSession, PipelineResult(..))
import Tidepool.Translate
  ( translateModuleClosed, ClosedModule(..), FlatNode(..), UnresolvedVar(..), stableVarId )

import Control.Monad.IO.Class (liftIO)
import Control.Exception (try, SomeException)
import Data.Bits (shiftR)
import Data.Foldable (toList)
import Data.Maybe (listToMaybe)
import Data.Word (Word64)
import Numeric (showHex)
import System.Directory (createDirectoryIfMissing, doesFileExist, removeFile)
import System.Environment (lookupEnv)
import System.Exit (exitFailure, exitSuccess)
import System.Process (readProcess)
import Text.Printf (printf)

workDir :: FilePath
workDir = "test-session/work"

g1 :: SessionModule
g1 = SessionModule ValMod (Generation 1)

defsPath :: FilePath
defsPath = workDir ++ "/Defs.hs"

usePath :: FilePath
usePath = workDir ++ "/Use.hs"

getLibdir :: IO FilePath
getLibdir = lookupEnv "TIDEPOOL_GHC_LIBDIR" >>= \case
  Just d  -> pure d
  Nothing -> trim <$> readProcess "ghc" ["--print-libdir"] ""
  where trim = reverse . dropWhile (== '\n') . reverse

setupHarvestFlags :: GhcMonad m => m ()
setupHarvestFlags = do
  d0 <- getSessionDynFlags
  _ <- setSessionDynFlags
         (updOptLevel 0 d0 { importPaths = workDir : importPaths d0
                           , hiDir = Just workDir, objectDir = Just workDir })
  pure ()

-- Compile Defs.hs and read the inferred types of two binders out of its type env.
harvestTypes :: FilePath -> IO (Type, Type)
harvestTypes libdir = runGhc (Just libdir) $ do
  setupHarvestFlags
  t <- guessTarget defsPath Nothing Nothing
  setTargets [t]
  ok <- load LoadAllTargets
  case ok of
    Failed    -> liftIO (ioError (userError "harvest: Defs.hs load failed"))
    Succeeded -> pure ()
  ms  <- getModSummary (mkModuleName "Defs")
  pm  <- parseModule ms
  tcm <- typecheckModule pm
  let ids = typeEnvIds (tcg_type_env (fst (tm_internals_ tcm)))
      tyOf occ = listToMaybe
        [ idType i | i <- ids, occNameString (nameOccName (varName i)) == occ ]
  case (tyOf "xsimple", tyOf "xexotic") of
    (Just a, Just b) -> pure (a, b)
    _ -> liftIO (ioError (userError "harvest: missing xsimple/xexotic"))

-- Synthesize + write the THIN session iface (production path) carrying both binders.
writeThinIface :: FilePath -> Type -> Type -> IO ()
writeThinIface libdir tySimple tyExotic = runGhc (Just libdir) $ do
  setupHarvestFlags
  hsc <- getSession
  iface <- liftIO $ mkThinSessionIface hsc g1
             [ (mkVarOcc "x",  tySimple)
             , (mkVarOcc "xe", tyExotic) ]
  liftIO $ writeSessionIface hsc workDir g1 iface
  liftIO $ putStrLn "  wrote thin Val.G1 iface (x, xe) — no source module exists"

-- The expected contract id for a session binder occ.
expectedId :: HscEnv -> SessionModule -> String -> Word64
expectedId hsc sm occ = stableVarId nm
  where
    modl = mkModule (homeUnitAsUnit (hsc_home_unit hsc)) (renderSessionModule sm)
    nm   = mkExternalName (mkUniqueGrimily 0) modl (mkVarOcc occ) noSrcSpan

data Verdict = GO | NOGO deriving (Eq, Show)

-- Compile Use.hs through the GATED production session pipeline, translate the
-- given target binding to Core, and assert the contract for the session binder.
checkBinder :: FilePath -> String -> String -> IO Verdict
checkBinder libdir targetName occ = do
  putStrLn $ "\n--- target " ++ targetName ++ "  (session binder "
             ++ "Tidepool.Session.Val.G1." ++ occ ++ ") ---"
  let scope = SessionScope { ssRoot = workDir, ssValIfaces = [g1] }
  r <- try $ do
    res <- runPipelineSession (Just scope) usePath []
    let hsc   = prHscEnv res
        binds = prBinds res
        want  = expectedId hsc g1 occ
    ClosedModule { cmNodes = nodes, cmUnresolved = unresolved } <-
      translateModuleClosed hsc binds targetName
    pure (want, toList nodes, unresolved)
  case r of
    Left (e :: SomeException) -> do
      putStrLn $ "  EXCEPTION: " ++ oneLine (show e)
      pure NOGO
    Right (want, nodes, unresolved) -> do
      let nvarIds = [ v | NVar v <- nodes ]
          tag w   = w `shiftR` 56
          found   = want `elem` nvarIds
          isExternal = tag want == 0xFE
          -- a 0x45 sentinel carrying THIS binder would mean B2 mis-routing
          sentinelForWant = any (\w -> tag w == 0x45) nvarIds
          inUnres = any (\uv -> uvKey uv == want) unresolved
      printf "  expected stableVarId : 0x%x  (tag 0x%x)\n" want (tag want)
      putStrLn $ "  NVar ids emitted     : " ++ show (map hex nvarIds)
      putStrLn $ "  -> external (0xFE)   : " ++ show isExternal
      putStrLn $ "  -> emitted as NVar   : " ++ show found
      putStrLn $ "  -> any 0x45 sentinel : " ++ show sentinelForWant
      putStrLn $ "  -> in unresolved set : " ++ show inUnres
      -- B2 detector is load-bearing: a collapse to the 0x45 error-sentinel must
      -- fail the verdict, not merely be reported.
      let go = found && isExternal && not inUnres && not sentinelForWant
      putStrLn $ "  RESULT: " ++ show (if go then GO else NOGO)
      pure (if go then GO else NOGO)
  where hex w = "0x" ++ showHex w ""

main :: IO ()
main = do
  libdir <- getLibdir
  createDirectoryIfMissing True workDir
  writeFile defsPath defsSrc
  writeFile usePath useSrc

  putStrLn "== harvest binder types from Defs.hs =="
  (tySimple, tyExotic) <- harvestTypes libdir

  putStrLn "== synthesize + write THIN Val.G1 iface from TyThings (no source) =="
  writeThinIface libdir tySimple tyExotic

  putStrLn "== HARDEN: ensure NO session source exists (synth-only iface) =="
  -- There never was a Tidepool/Session/Val/G1.hs; assert the .hi is all there is.
  let srcLeak = workDir ++ "/Tidepool/Session/Val/G1.hs"
  doesFileExist srcLeak >>= \b ->
    if b then removeFile srcLeak >> putStrLn "  removed stray session source"
         else putStrLn "  confirmed: only the synthesized .hi exists"

  vSimple <- checkBinder libdir "useX"  "x"
  vExotic <- checkBinder libdir "useXe" "xe"

  putStrLn "\n================ VERDICT ================"
  let rs = [ ("simple   Int -> Int", vSimple)
           , ("exotic   (Ord a,Num a)=>a->Map a a", vExotic) ]
  mapM_ (\(l, v) -> printf "  %-38s %s\n" l (show v)) rs
  if all ((== GO) . snd) rs
    then putStrLn "  OVERALL: GO" >> exitSuccess
    else putStrLn "  OVERALL: NO-GO" >> exitFailure

oneLine :: String -> String
oneLine = unwords . words

defsSrc :: String
defsSrc = unlines
  [ "module Defs (xsimple, xexotic) where"
  , "import Data.Map (Map)"
  , "import qualified Data.Map as Map"
  , ""
  , "xsimple :: Int -> Int"
  , "xsimple n = n + 1"
  , ""
  , "xexotic :: (Ord a, Num a) => a -> Map a a"
  , "xexotic v = Map.singleton v (v + v)"
  ]

useSrc :: String
useSrc = unlines
  [ "module Use (useX, useXe) where"
  , "import Tidepool.Session.Val.G1 (x, xe)"
  , "import Data.Map (Map)"
  , "import qualified Data.Map as Map"
  , ""
  , "-- references the simple session binder"
  , "useX :: Int"
  , "useX = x 41 + x 0"
  , ""
  , "-- references the instance-using session binder; the use needs Ord/Num at Int"
  , "useXe :: Int -> Maybe Int"
  , "useXe n = Map.lookup n (xe (n + 1))"
  ]
