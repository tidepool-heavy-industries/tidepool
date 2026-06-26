{-# LANGUAGE ScopedTypeVariables #-}
{-# LANGUAGE LambdaCase #-}

-- | GO/NO-GO spike for the Option-C binder-resolution FRONT-HALF (haskell side).
--
-- Proves the BACK-END half of the tidepool-repl session-value mechanism on the
-- extract side: that a reference to a session value binding
-- (@Tidepool.Session.Val.G1.x@) reaches tidepool's Core as a KNOWN EXTERNAL
-- @NVar (stableVarId "Tidepool.Session.Val.G1:x")@ —
--   * NOT inlined           (kimi B1: a fat session iface would inline the body),
--   * NOT error-sentineled   (kimi B2: an unresolved external becomes @NVar 0x45…@),
--   * via a THIN injected iface, with the reference module compiled through the
--     NORMAL module pipeline to Core (kimi B4 — NOT GHCi's @tcRnStmt@ path).
--
-- The TYPE plane (typecheck-with-injection) was already proven by
-- @spike-optionc/Spike.hs@. THIS spike proves the VALUE plane reaches Core as a
-- resolvable external Var, exercising tidepool's REAL @translateModuleClosed@.
--
-- Pipeline per type (simple @Int -> Int@ and exotic
-- @forall a. (Ord a, Num a) => a -> Map a a@):
--   1. WRITE   : compile a session home module @Tidepool.Session.Val.G1@ with a
--                THIN iface (no -fwrite-if-simplified-core => no mi_extra_decls;
--                -fomit-interface-pragmas => no ifIdUnfolding).
--   2. HARDEN  : delete the session SOURCE so turn 2 cannot recompile it; only
--                the thin .hi survives.
--   3. INJECT  : fresh runGhc, NO source — readIface (raw path) -> typecheckIface
--                -> HomeModInfo -> HPT + finder cache (the proven path). Verify
--                the reconstructed binder carries NO unfolding (thin).
--   4. EMIT    : compile a REFERENCE module @Use@ that imports the session module
--                and uses the binder, to Core, via @summariseFile@ +
--                typecheckModule + hscDesugar + core2core — the normal module
--                pipeline, dodging downsweep (which rejects the source-less
--                session module, per spike-optionc).
--   5. RESOLVE : run that Core through tidepool's REAL @translateModuleClosed@
--                (varId / resolveExternals / translateModule) and assert the
--                emitted session-binder reference == @stableVarId(name)@, is
--                0xFE-tagged (external), is NOT 0x45-sentineled, and is NOT in
--                the unresolved set.

module Main (main) where

import GHC
import GHC.Driver.Session
  ( gopt_set, gopt_unset, updOptLevel, GeneralFlag(..) )
import GHC.Driver.Env (HscEnv, hsc_dflags, hsc_FC, hsc_home_unit, hscUpdateHPT, hsc_NC, hscUpdateFlags)
import GHC.Driver.Main (hscDesugar)
import GHC.Driver.Make (summariseFile)
import GHC.Core.Opt.Pipeline (core2core)
import GHC.Unit.Module.ModGuts (ModGuts(..))
import GHC.Core (CoreBind)
import GHC.Types.Name (nameOccName, nameStableString)
import GHC.Types.Name.Occurrence (occNameString)
import GHC.Types.Var (varName)
import GHC.Types.Id (idType, realIdUnfolding)
import GHC.Core (maybeUnfoldingTemplate)
import GHC.Types.TypeEnv (typeEnvIds)
import GHC.Tc.Types (tcg_type_env)
import GHC.Core.Type (Type)

import GHC.Unit.Home (homeUnitAsUnit)
import GHC.Unit.Finder (addHomeModuleToFinder)
import GHC.Unit.Module.Location (ModLocation(..))
import GHC.Unit.Module.ModDetails (ModDetails(..))
import GHC.Unit.Home.ModInfo (HomeModInfo(..), addHomeModInfoToHpt, emptyHomeModInfoLinkable)
import GHC.Unit.Types (ModuleNameWithIsBoot, GenWithIsBoot(..), mkModule)
import GHC.IfaceToCore (typecheckIface)
import GHC.Tc.Utils.Monad (initIfaceCheck)
import GHC.Iface.Load (readIface)
import GHC.Unit.Module.ModIface (mi_extra_decls)
import qualified GHC.Data.Maybe as MErr
import Language.Haskell.Syntax.ImpExp (IsBootInterface(..))

import GHC.Utils.Outputable (renderWithContext, defaultSDocContext, ppr)
import GHC.Types.Id (Id)

import Tidepool.Translate
  ( translateModuleClosed, FlatNode(..), UnresolvedVar(..), varId, stableVarId )

import Control.Monad.IO.Class (liftIO)
import Control.Exception (try, SomeException)
import System.Directory (createDirectoryIfMissing, removeFile, doesFileExist)
import System.Environment (lookupEnv)
import System.Process (readProcess)
import Data.Bits (shiftR)
import Data.Foldable (toList)
import Data.IORef
import Data.Maybe (listToMaybe)
import Numeric (showHex)
import Text.Printf (printf)

workDir :: FilePath
workDir = "spike-extract/work"

sessionModName :: String
sessionModName = "Tidepool.Session.Val.G1"

sessionHiPath :: FilePath
sessionHiPath = workDir ++ "/Tidepool/Session/Val/G1.hi"

sessionSrcPath :: FilePath
sessionSrcPath = workDir ++ "/Tidepool/Session/Val/G1.hs"

usePath :: FilePath
usePath = workDir ++ "/Use.hs"

getLibdir :: IO FilePath
getLibdir = lookupEnv "TIDEPOOL_GHC_LIBDIR" >>= \case
  Just dir -> pure dir
  Nothing  -> trim <$> readProcess "ghc" ["--print-libdir"] ""
  where trim = reverse . dropWhile (== '\n') . reverse

-- ---------------------------------------------------------------------------
-- Flag setups
-- ---------------------------------------------------------------------------

-- | THIN-iface flags: write the .hi, but with NO simplified-core (=> no
-- mi_extra_decls) and -fomit-interface-pragmas (=> no ifIdUnfolding). This is
-- the deliberately-thin session iface (kimi B1).
setupThinFlags :: GhcMonad m => m ()
setupThinFlags = do
  dflags0 <- getSessionDynFlags
  let dflags =
        (`gopt_set`   Opt_WriteInterface)
        $ (`gopt_set` Opt_OmitInterfacePragmas)      -- strip unfoldings from .hi
        $ (`gopt_unset` Opt_ExposeAllUnfoldings)
        $ (`gopt_unset` Opt_WriteIfSimplifiedCore)   -- => mi_extra_decls = Nothing
        $ updOptLevel 0
        $ dflags0
            { importPaths = workDir : importPaths dflags0
            , hiDir       = Just workDir
            , objectDir   = Just workDir
            }
  _ <- setSessionDynFlags dflags
  pure ()

-- | NORMAL tidepool extraction flags (the reference-module side): fat-iface ON,
-- all unfoldings exposed, -O2, no backend. Mirrors GhcPipeline.canonicalizeDFlags
-- essentials. The session .hi on disk is already thin, so nothing inlines.
setupExtractFlags :: GhcMonad m => m ()
setupExtractFlags = do
  dflags0 <- getSessionDynFlags
  let dflags =
        (`gopt_set`   Opt_WriteInterface)
        $ (`gopt_set` Opt_WriteIfSimplifiedCore)
        $ (`gopt_set` Opt_ExposeAllUnfoldings)
        $ (`gopt_unset` Opt_FullLaziness)
        $ updOptLevel 2
        $ dflags0
            { importPaths = workDir : importPaths dflags0
            , hiDir       = Just workDir
            , objectDir   = Just workDir
            }
  _ <- setSessionDynFlags dflags
  pure ()

-- ---------------------------------------------------------------------------
-- Phase 1: write the THIN session iface
-- ---------------------------------------------------------------------------

writeSessionTurn1 :: FilePath -> IO (Maybe Type, Maybe Type)
writeSessionTurn1 libdir = runGhc (Just libdir) $ do
  setupThinFlags
  t <- guessTarget sessionSrcPath Nothing Nothing
  setTargets [t]
  ok <- load LoadAllTargets
  case ok of
    Failed    -> liftIO (ioError (userError "PHASE1: session module load failed"))
    Succeeded -> pure ()
  modSum <- getModSummary (mkModuleName sessionModName)
  pm  <- parseModule modSum
  tcm <- typecheckModule pm
  let tcg = fst (tm_internals_ tcm)
      ids = typeEnvIds (tcg_type_env tcg)
      tyOf occ = fmap idType $ listToMaybe
        [ i | i <- ids, occNameString (nameOccName (varName i)) == occ ]
  pure (tyOf "x", tyOf "xe")

-- ---------------------------------------------------------------------------
-- Phase 3: inject the thin session iface (the spike-optionc proven path)
-- ---------------------------------------------------------------------------

-- Returns the reconstructed ModDetails (md_types has the reconstructed binders).
injectSession :: GhcMonad m => m ModDetails
injectSession = do
  hsc0 <- getSession
  let fc     = hsc_FC hsc0
      homeU  = hsc_home_unit hsc0
      modNm  = mkModuleName sessionModName
      theMod = mkModule (homeUnitAsUnit homeU) modNm
  readRes <- liftIO $ readIface (hsc_dflags hsc0) (hsc_NC hsc0) theMod sessionHiPath
  case readRes of
    MErr.Failed _ -> liftIO (ioError (userError "INJECT: readIface failed"))
    MErr.Succeeded iface -> do
      -- Thin check #1: the fat-iface vector must be empty.
      case mi_extra_decls iface of
        Nothing -> liftIO $ putStrLn "  [thin] mi_extra_decls = Nothing  (no Core carried — B1 safe)"
        Just ds -> liftIO $ putStrLn $
          "  [FAT!] mi_extra_decls = Just (" ++ show (length ds) ++ " decls) — B1 would inline!"
      details <- liftIO $ initIfaceCheck (ppr modNm) hsc0 (typecheckIface iface)
      let hmi  = HomeModInfo iface details emptyHomeModInfoLinkable
          hsc1 = hscUpdateHPT (addHomeModInfoToHpt hmi) hsc0
          modLoc = ModLocation
            { ml_hs_file      = Nothing
            , ml_hi_file      = sessionHiPath
            , ml_dyn_hi_file  = sessionHiPath
            , ml_obj_file     = workDir ++ "/Tidepool/Session/Val/G1.o"
            , ml_dyn_obj_file = workDir ++ "/Tidepool/Session/Val/G1.o"
            , ml_hie_file     = workDir ++ "/Tidepool/Session/Val/G1.hie"
            }
          mnwib = GWIB modNm NotBoot :: ModuleNameWithIsBoot
      _ <- liftIO $ addHomeModuleToFinder fc homeU mnwib modLoc
      setSession hsc1
      pure details

-- Pull a reconstructed binder Id out of the injected ModDetails.
sessionId :: ModDetails -> String -> Maybe Id
sessionId details occ = listToMaybe
  [ i | i <- typeEnvIds (md_types details)
      , occNameString (nameOccName (varName i)) == occ ]

-- ---------------------------------------------------------------------------
-- Phase 4: compile the REFERENCE module to Core (no downsweep)
-- ---------------------------------------------------------------------------

compileUseToCore :: GhcMonad m => m (HscEnv, [CoreBind])
compileUseToCore = do
  hsc0 <- getSession
  let hsc = hscUpdateFlags id hsc0
      homeU = hsc_home_unit hsc
  esum <- liftIO $ summariseFile hsc homeU mempty usePath Nothing Nothing
  modSum <- case esum of
    Left _    -> liftIO (ioError (userError "EMIT: summariseFile (Use.hs) failed"))
    Right ms  -> pure ms
  pm  <- parseModule modSum
  tcm <- typecheckModule pm
  let tcg = fst (tm_internals_ tcm)
  desugared  <- liftIO $ hscDesugar hsc modSum tcg
  simplified <- liftIO $ core2core hsc desugared
  hscFinal <- getSession
  pure (hscFinal, mg_binds simplified)

-- ---------------------------------------------------------------------------
-- The end-to-end run for one binder
-- ---------------------------------------------------------------------------

data Verdict = GO | PARTIAL | NOGO deriving (Eq, Show)

-- Returns (verdict, expectedId, found?, sentineled?, unresolved?)
runForBinder :: FilePath -> String -> String -> IO Verdict
runForBinder libdir occ targetName = do
  putStrLn $ "\n========== BINDER " ++ sessionModName ++ "." ++ occ
             ++ "  (target " ++ targetName ++ ") =========="
  r <- try $ runGhc (Just libdir) $ do
    setupExtractFlags
    details <- injectSession
    -- Thin check #2: the reconstructed binder must carry NO unfolding.
    case sessionId details occ of
      Nothing -> liftIO $ putStrLn $ "  [warn] reconstructed Id for " ++ occ ++ " not found"
      Just i  -> case maybeUnfoldingTemplate (realIdUnfolding i) of
        Nothing -> liftIO $ putStrLn $ "  [thin] realIdUnfolding " ++ occ
                     ++ " = NoTemplate  (no inlinable body — B1 safe)"
        Just _  -> liftIO $ putStrLn $ "  [FAT!] realIdUnfolding " ++ occ
                     ++ " carries a template — B1 would inline!"
    let mExpected = (\i -> (varId i, stableVarId (varName i), nameStableString (varName i)))
                      <$> sessionId details occ
    (hsc, binds) <- compileUseToCore
    (nodes, _usedDCs, unresolved, _reach) <-
      liftIO $ translateModuleClosed hsc binds targetName
    pure (mExpected, toList nodes, unresolved)
  case r of
    Left (e :: SomeException) -> do
      putStrLn $ "  EXCEPTION: " ++ oneLine (show e)
      pure NOGO
    Right (mExpected, nodes, unresolved) ->
      case mExpected of
        Nothing -> do
          putStrLn "  NO-GO: could not recover session binder Id from injected iface"
          pure NOGO
        Just (vid, sid, stable) -> do
          let nvarIds = [ v | NVar v <- nodes ]
              tag w = w `shiftR` 56
              found   = sid `elem` nvarIds
              sentinelPresent = any (\w -> tag w == 0x45) nvarIds
              inUnres = any (\uv -> uvKey uv == sid) unresolved
          putStrLn $ "  contract name      : " ++ stable
          putStrLn $ "  varId binder       : 0x" ++ showHex vid ""
          putStrLn $ "  stableVarId(name)  : 0x" ++ showHex sid ""
                     ++ "   (tag 0x" ++ showHex (tag sid) "" ++ ")"
          putStrLn $ "  consistency        : varId == stableVarId ? "
                     ++ show (vid == sid)
          putStrLn $ "  NVar ids emitted   : "
                     ++ show (map (\w -> "0x" ++ showHex w "") nvarIds)
          putStrLn $ "  -> external (0xFE) : " ++ show (tag sid == 0xFE)
          putStrLn $ "  -> emitted as NVar : " ++ show found
          putStrLn $ "  -> 0x45 sentinel ? : " ++ show sentinelPresent
          putStrLn $ "  -> in unresolved ? : " ++ show inUnres
          let go = found && tag sid == 0xFE && not inUnres && vid == sid
          if go
            then do putStrLn "  RESULT: GO"; pure GO
            else do putStrLn "  RESULT: NO-GO"; pure NOGO

-- ---------------------------------------------------------------------------
-- main
-- ---------------------------------------------------------------------------

main :: IO ()
main = do
  libdir <- getLibdir
  createDirectoryIfMissing True (workDir ++ "/Tidepool/Session/Val")
  writeSources

  putStrLn "=== PHASE 1: compile session module, write THIN iface ==="
  (mtx, mtxe) <- writeSessionTurn1 libdir
  putStrLn $ "  x  :: " ++ maybe "<none>" pprStr mtx
  putStrLn $ "  xe :: " ++ maybe "<none>" pprStr mtxe

  putStrLn "\n=== PHASE 2: HARDEN — delete session source (only .hi remains) ==="
  ex <- doesFileExist sessionSrcPath
  if ex then removeFile sessionSrcPath >> putStrLn "  removed session .hs"
        else putStrLn "  (already absent)"
  -- Also remove Use products so it recompiles cleanly against the .hi.
  mapM_ rmIfExists [ workDir ++ "/Use.hi", workDir ++ "/Use.o" ]

  results <- newIORef []
  vSimple <- runForBinder libdir "x"  "useX"
  modifyIORef results (("simple Int->Int", vSimple):)
  vExotic <- runForBinder libdir "xe" "useXe"
  modifyIORef results (("exotic forall a.(Ord a,Num a)=>a->Map a a", vExotic):)

  putStrLn "\n=== R4: instance-replay finding ==="
  putStrLn r4Finding

  rs <- reverse <$> readIORef results
  putStrLn "\n================ VERDICT ================"
  mapM_ (\(lbl, v) -> printf "  %-45s %s\n" lbl (show v)) rs
  let overall | all ((== GO) . snd) rs = "GO"
              | any ((== GO) . snd) rs = "PARTIAL"
              | otherwise              = "NO-GO"
  putStrLn $ "  OVERALL: " ++ overall

pprStr :: Type -> String
pprStr = renderWithContext defaultSDocContext . ppr

oneLine :: String -> String
oneLine = unwords . words

rmIfExists :: FilePath -> IO ()
rmIfExists p = doesFileExist p >>= \b -> if b then removeFile p else pure ()

writeSources :: IO ()
writeSources = do
  writeFile sessionSrcPath sessionSrc
  writeFile usePath useSrc

sessionSrc :: String
sessionSrc = unlines
  [ "module " ++ sessionModName ++ " (x, xe) where"
  , "import Data.Map (Map)"
  , "import qualified Data.Map as Map"
  , ""
  , "-- simple binder type"
  , "x :: Int -> Int"
  , "x n = n + 1"
  , ""
  , "-- exotic binder type (constraints + a library type)"
  , "xe :: (Ord a, Num a) => a -> Map a a"
  , "xe v = Map.singleton v (v + v)"
  ]

useSrc :: String
useSrc = unlines
  [ "module Use (useX, useXe) where"
  , "import " ++ sessionModName ++ " (x, xe)"
  , "import Data.Map (Map)"
  , "import qualified Data.Map as Map"
  , ""
  , "-- references the simple session binder"
  , "useX :: Int"
  , "useX = x 41 + x 0"
  , ""
  , "-- references the exotic session binder; the use needs the Ord/Num"
  , "-- instances at Int (resolved at the use site from base — see R4)"
  , "useXe :: Int -> Maybe Int"
  , "useXe n = Map.lookup n (xe (n + 1))"
  ]

r4Finding :: String
r4Finding = unlines
  [ "  The exotic reference `xe (n+1)` needs (Ord Int, Num Int) at the USE site."
  , "  Those dictionaries are resolved by GHC's typechecker from instances IN"
  , "  SCOPE in the reference module (base's `instance Num Int`, `instance Ord"
  , "  Int`) — NOT from the session iface. So for any binder whose type mentions"
  , "  only library classes/types, NO `mi_insts` replay into the injected session"
  , "  is required: the reference module already imports base."
  , "  `mi_insts` replay becomes necessary only when the needed instance is a"
  , "  SESSION/user-defined (orphan) instance — e.g. `instance Show MySessionType`"
  , "  defined in a prior turn. Then the injected session iface (or the in-scope"
  , "  Tidepool.Session.Lib.G<g> module) must carry that `mi_insts` entry so the"
  , "  reference's `show` resolves. Standard-library instances: free. User/orphan"
  , "  instances: replay required (a real Wave-3 requirement, plan §7.2)."
  ]
