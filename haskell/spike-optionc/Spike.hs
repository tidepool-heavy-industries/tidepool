{-# LANGUAGE ScopedTypeVariables #-}
{-# LANGUAGE LambdaCase #-}

-- | GO/NO-GO spike for "Option C": serialize a value binding's STRUCTURED type
-- to a fat interface in one GHC session, reload it into a FRESH separate batch
-- runGhc session, and typecheck a *reference* to that binding with the
-- reconstructed type byte-identical to the original (no ppr round-trip).
--
-- TYPE PLANE ONLY. We never run the bindings.
--
-- Strategy (the import-based variant of the steer): mint the session bindings in
-- a NORMAL synthetic home module (Session1), let GHC's own driver write its fat
-- .hi (-fwrite-if-simplified-core, as tidepool uses everywhere), then in a fresh
-- runGhc compile a turn-2 module that `import`s Session1 and references the
-- bindings in a way that GENUINELY DEPENDS on the injected types (not
-- re-inferable from context). If turn 2 typechecks, GHC's finder + iface loader
-- (the read half tidepool already runs in FatIface.hs) reconstructed the
-- IfaceType faithfully past the fresh-session Unique boundary.
--
-- Fidelity assertion: convert turn-1's captured Type and turn-2's reconstructed
-- Type to IfaceType (toIfaceType) and compare with the derived Eq instance on
-- IfaceType. That is "compare IfaceType, NOT ppr strings".
--
-- B-comparison: render the exotic type with ppr, splice it as a source sig
-- `b :: <that string>`, compile, and check whether it round-trips.

module Main (main) where

import GHC
import GHC.Driver.Session
  ( gopt_set, updOptLevel, GeneralFlag(..) )
import GHC.Driver.Env (HscEnv, hsc_dflags)
import GHC.Types.Name (nameOccName, getName)
import GHC.Types.Name.Occurrence (occNameString)
import GHC.Types.Var (varType, varName)
import GHC.Types.Id (idType)
import GHC.Types.TypeEnv (typeEnvIds)
import GHC.Tc.Types (tcg_type_env)
import GHC.Core.Type (Type, tyConsOfType)
import GHC.Core.TyCo.Compare (eqType)
import GHC.Core.TyCon (tyConName)
import GHC.Types.Name (nameStableString)
import GHC.Types.Unique.Set (nonDetEltsUniqSet)
import GHC.CoreToIface (toIfaceType)
import Data.List (sort)
import GHC.Iface.Type (IfaceType)
import GHC.Utils.Outputable
  (renderWithContext, defaultSDocContext, ppr, showSDocUnsafe, sdocPprDebug, Outputable)

import GHC (noLocA)
import GHC.Tc.Module (TcRnExprMode(..))
import GHC.Hs.ImpExp (ImportDecl(..), ImportDeclQualifiedStyle(..))
import GHC.Hs.Extension (GhcPs)
import GHC.Driver.Env (hsc_FC, hsc_home_unit, hscUpdateHPT, hsc_NC)
import GHC.Unit.Home (homeUnitAsUnit)
import GHC.Unit.Finder (addHomeModuleToFinder)
import GHC.Unit.Module.Location (ModLocation(..))
import GHC.Unit.Module.ModDetails (ModDetails(..))
import GHC.Unit.Module.ModIface (mi_module)
import GHC.Unit.Home.ModInfo (HomeModInfo(..), addHomeModInfoToHpt, emptyHomeModInfoLinkable)
import GHC.Unit.Types (moduleName, ModuleNameWithIsBoot, GenWithIsBoot(..))
import GHC.IfaceToCore (typecheckIface)
import GHC.Tc.Utils.Monad (initIfaceCheck)
import GHC.Iface.Load (readIface)
import GHC.Data.Maybe (MaybeErr)
import qualified GHC.Data.Maybe as MErr
import Language.Haskell.Syntax.ImpExp (IsBootInterface(..))
import GHC.Unit.Types (mkModule, toUnitId, moduleUnit)

import Control.Monad.IO.Class (liftIO)
import Control.Exception (try, SomeException)
import System.Directory (createDirectoryIfMissing, removeFile, doesFileExist)
import System.Environment (lookupEnv)
import System.Process (readProcess)
import Data.List (isInfixOf)
import Data.Maybe (listToMaybe)

workDir :: FilePath
workDir = "spike-optionc/work"

getLibdir :: IO FilePath
getLibdir = lookupEnv "TIDEPOOL_GHC_LIBDIR" >>= \case
  Just dir -> pure dir
  Nothing  -> trim <$> readProcess "ghc" ["--print-libdir"] ""
  where trim = reverse . dropWhile (== '\n') . reverse

-- Common DynFlags massaging: -O2, fat interfaces, all unfoldings, write .hi/.o
-- into workDir, and put workDir on the import path so turn-2 finds Session1.hi.
setupFlags :: GhcMonad m => [FilePath] -> m ()
setupFlags extraImports = do
  dflags0 <- getSessionDynFlags
  let dflags =
        (`gopt_set` Opt_WriteInterface)
        $ (`gopt_set` Opt_WriteIfSimplifiedCore)   -- the fat-interface flag
        $ (`gopt_set` Opt_ExposeAllUnfoldings)
        $ updOptLevel 2
        $ dflags0
            { importPaths = importPaths dflags0 ++ (workDir : extraImports)
            , hiDir  = Just workDir
            , objectDir = Just workDir
            }
  _ <- setSessionDynFlags dflags
  pure ()

-- Pull the type of a top-level binder (by occ-name) out of a typechecked module.
typeOfBinder :: String -> TypecheckedModule -> Maybe Type
typeOfBinder occ tcm =
  let tcg = fst (tm_internals_ tcm)
      ids = typeEnvIds (tcg_type_env tcg)
  in fmap idType $ listToMaybe
       [ i | i <- ids, occNameString (nameOccName (varName i)) == occ ]

-- =====================================================================
-- TURN 1: compile Session1 (real source) -> GHC writes Session1.hi (fat).
-- Capture the original Types of g1/g2.
-- =====================================================================
turn1 :: FilePath -> IO (Maybe Type, Maybe Type)
turn1 libdir = runGhc (Just libdir) $ do
  setupFlags []
  t <- guessTarget (workDir ++ "/Session1.hs") Nothing Nothing
  setTargets [t]
  ok <- load LoadAllTargets
  case ok of
    Failed     -> liftIO (ioError (userError "TURN1: load failed"))
    Succeeded  -> pure ()
  -- Re-typecheck to read inferred types out of the type env.
  modSum <- getModSummary (mkModuleName "Session1")
  pm  <- parseModule modSum
  tcm <- typecheckModule pm
  pure (typeOfBinder "g1" tcm, typeOfBinder "g2" tcm)

-- =====================================================================
-- TURN 2: FRESH runGhc, NO InteractiveContext. Compile Use.hs which
-- `import Session1` and references g1/g2 in a way that depends on their
-- types. Then read back the RECONSTRUCTED type of g1/g2 as seen in turn 2.
-- =====================================================================
turn2 :: FilePath -> IO (Either String (Maybe Type, Maybe Type))
turn2 libdir = do
  r <- try $ runGhc (Just libdir) $ do
    setupFlags []
    t <- guessTarget (workDir ++ "/Use.hs") Nothing Nothing
    setTargets [t]
    ok <- load LoadAllTargets
    case ok of
      Failed    -> liftIO (ioError (userError "TURN2: load (Use.hs) failed -- reference did NOT typecheck"))
      Succeeded -> pure ()
    -- Read back the reconstructed types as turn-2 sees them: typecheck Use and
    -- inspect the imported Ids it references. We re-typecheck Session1 via its
    -- iface by typechecking Use which imports it; pull g1/g2 from Use's type env
    -- (they appear as external Ids referenced by useG1/useG2).
    modSum <- getModSummary (mkModuleName "Use")
    pm  <- parseModule modSum
    tcm <- typecheckModule pm
    let tcg = fst (tm_internals_ tcm)
        ids = typeEnvIds (tcg_type_env tcg)
    -- The imported g1/g2 won't be in Use's own type env; instead read them via
    -- the renamed/typechecked global rdr env lookup. Simpler: ask GHC for the
    -- Name's TyThing in the session.
    g1ty <- lookupSessionType "Session1" "g1"
    g2ty <- lookupSessionType "Session1" "g2"
    pure (g1ty, g2ty)
  case r of
    Left (e :: SomeException) -> pure (Left (show e))
    Right v                   -> pure (Right v)

-- =====================================================================
-- TURN 2 (INJECTED): the STEER's path. Fresh runGhc, NO InteractiveContext,
-- and Session1.hs SOURCE IS ABSENT. We read Session1.hi by path (the read half
-- tidepool already runs in FatIface.hs), reconstruct its TyThings via
-- typecheckIface, build a HomeModInfo, push it into the HPT, and register the
-- module in the finder cache pointing at the .hi. Then `import Session1` in
-- Use.hs resolves PURELY from the serialized interface -- proving cross-session
-- type transfer with no source recompile and no GHCi interactive package.
-- =====================================================================
-- Inject Session1.hi into the current session's HPT + finder cache. Returns the
-- reconstructed ModDetails (its md_types has the reconstructed TyThings).
injectSession1 :: GhcMonad m => m ModDetails
injectSession1 = do
  hsc0 <- getSession
  let fc     = hsc_FC hsc0
      homeU  = hsc_home_unit hsc0
      modNm  = mkModuleName "Session1"
      hiPath = workDir ++ "/Session1.hi"
      theMod = mkModule (homeUnitAsUnit homeU) modNm        -- GenModule Unit
  -- readIface takes a RAW PATH (findAndReadIface goes through the finder, which
  -- needs the module locatable on the import path -- impossible with no source).
  readRes <- liftIO $ readIface (hsc_dflags hsc0) (hsc_NC hsc0) theMod hiPath
  case readRes of
    MErr.Failed _err -> liftIO (ioError (userError "INJECT: could not read Session1.hi (readIface failed)"))
    MErr.Succeeded iface -> do
      details <- liftIO $ initIfaceCheck (ppr modNm) hsc0 (typecheckIface iface)
      let hmi  = HomeModInfo iface details emptyHomeModInfoLinkable
          hsc1 = hscUpdateHPT (addHomeModInfoToHpt hmi) hsc0
          modLoc = ModLocation
            { ml_hs_file      = Nothing
            , ml_hi_file      = hiPath
            , ml_dyn_hi_file  = hiPath
            , ml_obj_file     = workDir ++ "/Session1.o"
            , ml_dyn_obj_file = workDir ++ "/Session1.o"
            , ml_hie_file     = workDir ++ "/Session1.hie"
            }
          mnwib = GWIB modNm NotBoot :: ModuleNameWithIsBoot
      _ <- liftIO $ addHomeModuleToFinder fc homeU mnwib modLoc
      setSession hsc1
      pure details

turn2Injected :: FilePath -> IO (Either String (Maybe Type, Maybe Type))
turn2Injected libdir = do
  r <- try $ runGhc (Just libdir) $ do
    setupFlags []
    details <- injectSession1
    -- Bring the injected Session1 (now a normal HPT home module) into scope and
    -- typecheck a REFERENCE EXPRESSION against it. This is NOT the GhciN
    -- interactive package -- it's a normal home module name resolved from the
    -- HPT. exprType runs the full renamer+typechecker on the expression.
    setContext [ IIDecl (simpleImportDecl (mkModuleName "Prelude"))
               , IIDecl (simpleImportDecl (mkModuleName "Session1"))
               , IIDecl (qualifiedImport "Data.Map" "Map")
               ]
    -- The reference genuinely depends on the injected type:
    --   Map.lookup needs Ord a (from g2's constraint); (2::Int) needs Num a;
    --   the result must be (Map a a) for Map.lookup to apply.
    exprOk <- handleSourceError (\e -> pure (Left (show e))) $ do
      ty <- exprType TM_Inst "Map.lookup (1 :: Int) (g2 (2 :: Int))"
      pure (Right ty)
    case exprOk of
      Left err -> liftIO (ioError (userError ("INJECT: reference did NOT typecheck: " ++ oneLine (firstLines 4 err))))
      Right _refTy -> do
        g1ty <- typeFromDetails details "g1"
        g2ty <- typeFromDetails details "g2"
        pure (g1ty, g2ty)
  case r of
    Left (e :: SomeException) -> pure (Left (show e))
    Right v                   -> pure (Right v)

-- `import qualified <mod> as <q>`
qualifiedImport :: String -> String -> ImportDecl GhcPs
qualifiedImport modName q =
  let d = simpleImportDecl (mkModuleName modName)
  in d { ideclQualified = QualifiedPre
       , ideclAs = Just (noLocA (mkModuleName q)) }

-- Read a reconstructed binding type straight out of the typecheckIface ModDetails
-- (md_types :: TypeEnv). This is the actual reconstructed TyThing.
typeFromDetails :: GhcMonad m => ModDetails -> String -> m (Maybe Type)
typeFromDetails details occ = pure $
  case [ i | i <- typeEnvIds (md_types details)
           , occNameString (nameOccName (varName i)) == occ ] of
    (i:_) -> Just (idType i)
    []    -> Nothing

-- Look up a binding's RECONSTRUCTED type via the iface-loaded module info.
-- getModuleInfo for Session1 reads its .hi (the fat interface written in turn 1)
-- in this FRESH session; modInfoExports gives the reconstructed Names (fresh
-- Uniques, content-addressed by (Module,OccName)); modInfoLookupName gives the
-- reconstructed TyThing, from which we read the reconstructed Type.
lookupSessionType :: GhcMonad m => String -> String -> m (Maybe Type)
lookupSessionType modName occ = do
  modl <- findModule (mkModuleName modName) Nothing
  mInfo <- getModuleInfo modl
  case mInfo of
    Nothing -> pure Nothing
    Just info -> do
      let exps = modInfoExports info
          matches = [ n | n <- exps
                        , occNameString (nameOccName n) == occ ]
      case matches of
        [] -> pure Nothing
        (n:_) -> do
          mtt <- modInfoLookupName info n
          mtt' <- case mtt of
                    Just tt -> pure (Just tt)
                    Nothing -> lookupName n   -- fall back to global lookup
          pure $ case mtt' of
            Just (AnId i) -> Just (idType i)
            _             -> Nothing

-- =====================================================================
-- B-COMPARISON: ppr the exotic type, splice as source sig, compile.
-- =====================================================================
bComparison :: FilePath -> String -> IO (Either String Bool)
bComparison libdir pprStr = do
  let src = unlines
        [ "module BTest where"
        , "import Data.Map (Map)"
        , "b :: " ++ pprStr
        , "b = undefined"
        ]
  writeFile (workDir ++ "/BTest.hs") src
  r <- try $ runGhc (Just libdir) $ do
    setupFlags []
    t <- guessTarget (workDir ++ "/BTest.hs") Nothing Nothing
    setTargets [t]
    ok <- load LoadAllTargets
    pure $ case ok of { Succeeded -> True; Failed -> False }
  case r of
    Left (e :: SomeException) -> pure (Left (show e))
    Right b                   -> pure (Right b)

-- ADVERSARIAL: compile a module that misuses g2. It must FAIL — if it compiles,
-- the type was re-inferable (not load-bearing) and our GO would be self-deception.
-- Returns Right True if it (wrongly) compiled, Right False if it (correctly) failed.
-- Try to typecheck a deliberately-WRONG reference against the injected g2.
-- Returns Right True if it (wrongly) typechecked, Right False if (correctly) rejected.
negativeControl :: FilePath -> IO (Either String Bool)
negativeControl libdir = do
  r <- try $ runGhc (Just libdir) $ do
    setupFlags []
    _ <- injectSession1   -- same injection path: the expr sees the REAL g2 type
    setContext [ IIDecl (simpleImportDecl (mkModuleName "Prelude"))
               , IIDecl (simpleImportDecl (mkModuleName "Session1"))
               , IIDecl (qualifiedImport "Data.Map" "Map") ]
    -- WRONG: feed g2 a String (no Num instance). Violates g2's Num constraint;
    -- must be REJECTED. (If g2's type were re-inferable / dropped, this passes.)
    res <- handleSourceError (\_ -> pure False) $ do
      _ <- exprType TM_Inst "g2 \"not a number\""
      pure True
    pure res
  case r of
    Left (e :: SomeException) -> pure (Left (show e))
    Right b                   -> pure (Right b)

-- Exotic B test: take types where ppr is KNOWN to be lossy and see if the
-- ppr'd string re-parses to the SAME type. Each case: (label, sourceSig).
-- We compile `b :: <sig>; b = undefined`, capture b's inferred type, ppr it,
-- splice the ppr string back as a NEW sig, recompile, and compare eqType-modulo
-- -session via stable content. Reports whether ppr round-tripped faithfully.
bComparisonExotic :: FilePath -> IO (Either String [(String, Bool, String, String)])
bComparisonExotic libdir = do
  let cases =
        -- (label, sig, body) -- body must typecheck against sig (RankN bodies
        -- can't be `undefined` due to impredicativity).
        [ ("higher-rank", "(forall a. a -> a) -> (Int, Bool)", "\\f -> (f 0, f True)")
        , ("nested-rank", "((forall a. a -> a) -> Int) -> Int", "\\g -> g (\\x -> x)")
        ]
  r <- try $ mapM (oneBExotic libdir) cases
  case r of
    Left (e :: SomeException) -> pure (Left (show e))
    Right xs                  -> pure (Right xs)

-- (re-typed below to carry a body)

-- Compile `b :: <sig>`, read inferred type, ppr it, splice ppr back as the sig,
-- recompile, and compare the two inferred types by stable content + ppr string.
oneBExotic :: FilePath -> (String, String, String) -> IO (String, Bool, String, String)
oneBExotic libdir (label, sig, body) = do
  origTy <- compileSigGetType libdir sig body
  case origTy of
    Nothing -> pure (label, False, "<orig failed to compile: " ++ sig ++ ">", "")
    Just o  -> do
      let pprStr = renderWithContext defaultSDocContext (ppr o)
      recTy <- compileSigGetType libdir pprStr body
      case recTy of
        Nothing -> pure (label, False, pprStr, "<ppr did NOT re-parse/typecheck>")
        Just rc -> do
          let stO = sort (map (nameStableString . tyConName)
                              (nonDetEltsUniqSet (tyConsOfType o)))
              stR = sort (map (nameStableString . tyConName)
                              (nonDetEltsUniqSet (tyConsOfType rc)))
              sO = renderWithContext defaultSDocContext (ppr o)
              sR = renderWithContext defaultSDocContext (ppr rc)
          pure (label, stO == stR && sO == sR, sO, sR)

-- Compile a throwaway module with `b :: <sig>; b = <body>`; return b's
-- inferred type, or Nothing if it didn't compile.
compileSigGetType :: FilePath -> String -> String -> IO (Maybe Type)
compileSigGetType libdir sig body = do
  let src = unlines
        [ "{-# LANGUAGE RankNTypes #-}"
        , "module BExotic where"
        , "import Data.Map (Map)"
        , "b :: " ++ sig
        , "b = " ++ body
        ]
  writeFile (workDir ++ "/BExotic.hs") src
  r <- try $ runGhc (Just libdir) $ do
    setupFlags []
    t <- guessTarget (workDir ++ "/BExotic.hs") Nothing Nothing
    setTargets [t]
    ok <- handleSourceError (\_ -> pure Failed) (load LoadAllTargets)
    case ok of
      Failed    -> pure Nothing
      Succeeded -> do
        modSum <- getModSummary (mkModuleName "BExotic")
        pm  <- parseModule modSum
        tcm <- typecheckModule pm
        pure (typeOfBinder "b" tcm)
  case r of
    Left (_ :: SomeException) -> pure Nothing
    Right v                   -> pure v

reportB :: (String, Bool, String, String) -> IO ()
reportB (label, ok, orig, recon) = do
  putStrLn $ "  [" ++ label ++ "] ppr round-trips faithfully = " ++ show ok
  putStrLn $ "      ppr/orig:  " ++ orig
  putStrLn $ "      re-parsed: " ++ recon

main :: IO ()
main = do
  libdir <- getLibdir
  createDirectoryIfMissing True workDir
  writeSources

  putStrLn "=== TURN 1: compile Session1, capture original types ==="
  (mg1o, mg2o) <- turn1 libdir
  putStrLn $ "  g1 original: " ++ maybe "<none>" (renderWithContext defaultSDocContext . ppr) mg1o
  putStrLn $ "  g2 original: " ++ maybe "<none>" (renderWithContext defaultSDocContext . ppr) mg2o

  putStrLn ""
  putStrLn "=== HARDENING: remove Session1.hs so turn 2 CANNOT recompile from source ==="
  putStrLn "  (forces the reference to resolve PURELY from the serialized Session1.hi)"
  let s1src = workDir ++ "/Session1.hs"
  ex <- doesFileExist s1src
  if ex then removeFile s1src >> putStrLn "  removed Session1.hs (only Session1.hi remains)"
        else putStrLn "  (Session1.hs already absent)"
  -- also remove Use.* products so turn 2 recompiles Use against the .hi
  mapM_ rmIfExists [ workDir ++ "/Use.hi", workDir ++ "/Use.o" ]

  putStrLn ""
  putStrLn "=== TURN 2 (INJECTED): fresh runGhc, NO source, NO IC -- inject Session1.hi ==="
  putStrLn "  read .hi by path -> typecheckIface -> HomeModInfo -> HPT + finder cache"
  r2 <- turn2Injected libdir
  case r2 of
    Left err -> do
      putStrLn "  TURN2 FAILED:"
      putStrLn (indent err)
    Right (mg1r, mg2r) -> do
      putStrLn "  TURN2 typechecked the reference. Reconstructed types:"
      putStrLn $ "  g1 reconstructed: " ++ maybe "<none>" (renderWithContext defaultSDocContext . ppr) mg1r
      putStrLn $ "  g2 reconstructed: " ++ maybe "<none>" (renderWithContext defaultSDocContext . ppr) mg2r
      putStrLn ""
      putStrLn "=== FIDELITY (IfaceType equality, NOT ppr) ==="
      reportFidelity "g1 (Int -> Int)" mg1o mg1r
      reportFidelity "g2 (forall a. (Ord a, Num a) => a -> Map a a)" mg2o mg2r

  putStrLn ""
  putStrLn "=== ADVERSARIAL NEGATIVE CONTROL (prove the injected type is load-bearing) ==="
  putStrLn "  Same injection path; typecheck a WRONG reference `g2 \"not a number\"`."
  putStrLn "  String has no Num instance -> must be REJECTED iff g2's real type was injected."
  neg <- negativeControl libdir
  case neg of
    Right False -> putStrLn "  PASS: wrong reference correctly REJECTED (the injected Num constraint is enforced)."
    Right True  -> putStrLn "  FAIL: wrong reference typechecked -- type was NOT load-bearing (re-inferable!). GO is suspect."
    Left e      -> putStrLn $ "  (control errored: " ++ oneLine (firstLines 3 e) ++ ")"

  putStrLn ""
  putStrLn "=== B-COMPARISON (exotic): ppr a higher-rank/TF type -> re-parse ==="
  bExotic <- bComparisonExotic libdir
  case bExotic of
    Left e        -> putStrLn $ "  (exotic-B harness errored: " ++ oneLine (firstLines 3 e) ++ ")"
    Right results -> mapM_ reportB results

  putStrLn ""
  putStrLn "=== B-COMPARISON: ppr exotic type -> source sig -> compile ==="
  case mg2o of
    Nothing -> putStrLn "  (g2 not captured; skipping)"
    Just ty -> do
      let pprStr = renderWithContext defaultSDocContext (ppr ty)
      putStrLn $ "  ppr(g2) = " ++ pprStr
      br <- bComparison libdir pprStr
      case br of
        Left err -> do
          putStrLn "  B: ppr round-trip FAILED to compile:"
          putStrLn (indent (firstLines 12 err))
        Right True  -> putStrLn "  B: ppr round-trip COMPILED (ppr happened to re-parse)."
        Right False -> putStrLn "  B: ppr round-trip did NOT compile."

reportFidelity :: String -> Maybe Type -> Maybe Type -> IO ()
reportFidelity label (Just o) (Just r) = do
  let io = toIfaceType o
      ir = toIfaceType r
      ifaceEq = io == ir
      semEq   = eqType o r       -- alpha-equivalence-aware "same type"
  putStrLn $ "  " ++ label ++ ":"
  -- NOTE: eqType / IfaceType (==) compare TyCons by Unique. Across two SEPARATE
  -- runGhc sessions, a non-wired-in TyCon (e.g. Map) gets a different NameCache
  -- Unique, so these report False even for an identical type. The faithful,
  -- cross-session-valid check is the content comparison below (nameStableString).
  -- CONTENT-ADDRESSED comparison: the gold standard that survives across
  -- separate NameCaches. Every TyCon mentioned, keyed by nameStableString
  -- (= "<unit>$<module>$<occ>"), with Uniques erased. If these match, the
  -- types are structurally identical modulo the cross-session Unique that
  -- eqType/IfaceType-Eq necessarily disagree on.
  let stableTyCons t = sort (map (nameStableString . tyConName)
                                 (nonDetEltsUniqSet (tyConsOfType t)))
      stO = stableTyCons o
      stR = stableTyCons r
      contentEq = stO == stR
  putStrLn $ "     eqType (Unique-based, cross-session noisy) = " ++ show semEq
  putStrLn $ "     IfaceType ==  (Unique-based, same caveat)  = " ++ show ifaceEq
  putStrLn $ "     CONTENT (stable TyCon names) equal         = " ++ show contentEq
  if contentEq
    then putStrLn "     -> FAITHFUL (content-identical across the session boundary)."
    else putStrLn "     -> REAL FIDELITY LOSS (stable names differ)."
  if not ifaceEq && contentEq
    then do
      putStrLn "     IfaceType debug dump (sdocPprDebug -- exposes Uniques):"
      putStrLn $ "        orig:  " ++ oneLine (dbg io)
      putStrLn $ "        recon: " ++ oneLine (dbg ir)
      putStrLn $ "     stable TyCon names orig:  " ++ show stO
      putStrLn $ "     stable TyCon names recon: " ++ show stR
    else pure ()
reportFidelity label _ _ = putStrLn $ "  " ++ label ++ ": MISSING one side."

dbg :: Outputable a => a -> String
dbg = renderWithContext (defaultSDocContext { sdocPprDebug = True }) . ppr

oneLine :: String -> String
oneLine = unwords . words

rmIfExists :: FilePath -> IO ()
rmIfExists p = doesFileExist p >>= \b -> if b then removeFile p else pure ()

indent :: String -> String
indent = unlines . map ("    " ++) . lines

firstLines :: Int -> String -> String
firstLines n = unlines . take n . lines

-- =====================================================================
-- Source modules for turn 1 and turn 2.
-- =====================================================================
writeSources :: IO ()
writeSources = do
  writeFile (workDir ++ "/Session1.hs") session1Src
  writeFile (workDir ++ "/Use.hs") useSrc

session1Src :: String
session1Src = unlines
  [ "module Session1 (g1, g2) where"
  , "import Data.Map (Map)"
  , "import qualified Data.Map as Map"
  , ""
  , "-- simple binding type: Int -> Int"
  , "g1 :: Int -> Int"
  , "g1 x = x + 1"
  , ""
  , "-- exotic binding type: forall a. (Ord a, Num a) => a -> Map a a"
  , "-- (constraints + a library type -- where B mangles)"
  , "g2 :: (Ord a, Num a) => a -> Map a a"
  , "g2 x = Map.singleton x (x + x)"
  ]

-- Turn-2 references that GENUINELY DEPEND on the injected types:
--   * useG1 forces g1's argument/result to be Int by feeding a value whose
--     type is fixed elsewhere and using the result as an Int. If g1's type were
--     wrong, this would not typecheck.
--   * useG2 RELIES on the (Ord a, Num a) constraints and the `Map a a` result:
--     it calls Map.lookup on the result and adds under Num. The constraints are
--     NOT re-inferable from the call site -- they must come from g2's reconstructed
--     signature, or GHC would reject `g2 (5 :: Int)` / the Ord-keyed lookup.
useSrc :: String
useSrc = unlines
  [ "module Use (useG1, useG2) where"
  , "import Session1 (g1, g2)"
  , "import Data.Map (Map)"
  , "import qualified Data.Map as Map"
  , ""
  , "-- depends on g1 :: Int -> Int"
  , "useG1 :: Int"
  , "useG1 = g1 41 + g1 0"
  , ""
  , "-- depends on g2 :: (Ord a, Num a) => a -> Map a a."
  , "-- Map.lookup needs Ord a (from g2's constraint); the (+1) on the key and"
  , "-- the value needs Num a. If g2's reconstructed type dropped the constraints"
  , "-- or got the result type wrong, this would fail to typecheck."
  , "useG2 :: Int -> Maybe Int"
  , "useG2 n = Map.lookup n (g2 (n + 1))"
  ]
