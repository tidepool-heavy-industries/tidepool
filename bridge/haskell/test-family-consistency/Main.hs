module Main (main) where

import Control.Exception (bracket, evaluate)
import Control.Monad (forM, forM_, unless, void)
import Control.Monad.IO.Class (liftIO)
import Data.IORef (newIORef, readIORef, writeIORef)
import Data.List (isPrefixOf, sortOn)
import GHC
import GHC.Clock (getMonotonicTimeNSec)
import GHC.Core.FamInstEnv (famInstTyCon, emptyFamInstEnv, extendFamInstEnvList)
import GHC.Core.TyCon (tyConName)
import GHC.Driver.Env (hsc_HPT)
import GHC.Stats (allocated_bytes, getRTSStats, getRTSStatsEnabled)
import GHC.Types.Name (nameOccName)
import GHC.Types.Name.Occurrence (occNameString)
import GHC.Unit.Home.ModInfo (hm_details, lookupHpt)
import GHC.Unit.Module.ModDetails (md_fam_insts)
import System.Directory
import System.Environment (getArgs)
import System.FilePath ((</>))
import System.IO (hClose, openTempFile)
import System.Mem (performGC)
import System.Process (readProcess)
import Tidepool.DeclarationJoin
  ( JoinDecision(..), JoinRejection(..), validateRetainedFamilyInstances )
import Tidepool.ExactHydration (freshExactState)

-- These producers are individually legal GHC modules. They share original
-- Names in one NameCache, but never import each other's equations. Validation
-- combines their immutable axioms only after their original compilation.
main :: IO ()
main = do
  arguments <- getArgs
  let benchmarking = arguments == ["--benchmark"]
  unless (null arguments || benchmarking) (fail "expected --benchmark or no arguments")
  bracket scratch removeDirectoryRecursive $ \root -> do
    writeFixtures root (if benchmarking then 1024 else 16)
    libdir <- trim <$> readProcess "ghc" ["--print-libdir"] ""
    banks <- runGhc (Just libdir) $ do
      flags <- getSessionDynFlags
      void $ setSessionDynFlags flags
        { importPaths = [root], ghcLink = NoLink, backend = ncgBackend
        , hiDir = Just root, objectDir = Just root }
      initial <- getSession
      forM ["Original", "Compatible", "Conflict", "InjectiveConflict", "Shared"] $ \name -> do
        fresh <- liftIO (freshExactState initial)
        setSession fresh
        target <- guessTarget (root </> name ++ ".hs") Nothing Nothing
        setTargets [target]
        success <- load LoadAllTargets
        liftIO $ unless (succeeded success) (fail (name ++ " original fixture failed"))
        env <- getSession
        case lookupHpt (hsc_HPT env) (mkModuleName name) of
          Nothing -> liftIO (fail (name ++ " has no compiled details"))
          Just hmi -> pure (md_fam_insts (hm_details hmi))
    case banks of
      [original, compatible, conflict, injectiveConflict, shared] -> do
        semantics original compatible conflict injectiveConflict
        if benchmarking
          then benchmark original compatible conflict injectiveConflict shared
          else putStrLn "family consistency: PASS (12 original, duplicate, compatible, hidden/package, conflicting, associated and injective cases)"
      _ -> fail "incomplete original family fixture banks"
  where trim = reverse . dropWhile (`elem` "\r\n") . reverse

semantics :: [FamInst] -> [FamInst] -> [FamInst] -> [FamInst] -> IO ()
semantics original compatible conflict injectiveConflict = do
  accepted "original closure" [] original
  accepted "same exact axiom repeated" [] (original ++ original)
  accepted "compatible originals" [] (original ++ compatible)
  accepted "package and retained exact axiom" original (original ++ compatible)
  rejected "hidden family equation" [] (original ++ select "Plain" conflict)
  rejected "package family equation" original (select "Plain" conflict)
  rejected "associated type equation" [] (original ++ filter ((== "Associated") . familyOccurrence) conflict)
  rejected "associated data equation" [] (original ++ filter ((== "AssociatedData") . familyOccurrence) conflict)
  rejected "injective result collision" [] (original ++ select "Injective" injectiveConflict)
  rejected "package injective result collision" original (select "Injective" injectiveConflict)
  rejected "polymorphic injectivity" [] (original ++ select "Poly" injectiveConflict)
  rejected "package polymorphic injectivity" original (select "Poly" injectiveConflict)
  where
    accepted label packages retained = case validate packages retained of
      JoinAccepted -> pure ()
      other -> fail (label ++ ": " ++ show other)
    rejected label packages retained = case validate packages retained of
      JoinRejected FamilyInstanceConflict diagnostic | not (null diagnostic) -> pure ()
      other -> fail (label ++ " lost its family rejection: " ++ show other)
    validate packages = validateRetainedFamilyInstances
      (extendFamInstEnvList emptyFamInstEnv packages)

benchmark :: [FamInst] -> [FamInst] -> [FamInst] -> [FamInst] -> [FamInst] -> IO ()
benchmark original compatible conflict injectiveConflict shared = do
  enabled <- getRTSStatsEnabled
  unless enabled (fail "allocation benchmark requires +RTS -T")
  putStrLn "scenario,family_count,retained_axioms,repetitions,elapsed_ns,allocated_bytes"
  forM_ [16, 64, 256, 1024] $ \count -> do
    let plain = take count (select "Plain" original)
        plainPair = take count (select "Plain" compatible)
        injective = take count (select "Injective" original)
        injectivePair = take count (select "Injective" compatible)
        cases =
          [ ("original", plain, False)
          , ("original-duplicate", plain ++ plain, False)
          , ("compatible-pair", plain ++ plainPair, False)
          , ("conflicting-pair", plain ++ plainPair ++ take 1 (select "Plain" conflict), True)
          , ("injective-original", injective, False)
          , ("injective-compatible-pair", injective ++ injectivePair, False)
          , ("injective-conflicting-pair", injective ++ take 1 (select "Injective" injectiveConflict), True)
          , ("shared-injective", take count shared, False) ]
    forM_ cases $ \(label, retained, rejects) -> do
      -- Force lazy interface/type payloads before measurement. The IORef makes
      -- every repetition invoke the pure validator anew rather than time a
      -- shared result thunk. The input graph itself remains immutable/shared.
      forceDecision rejects retained
      input <- newIORef retained
      forM_ [1 .. 5 :: Int] $ \sample -> do
        performGC
        before <- getRTSStats
        start <- getMonotonicTimeNSec
        forM_ [1 .. 5 :: Int] $ \_ -> do
          current <- readIORef input
          forceDecision rejects current
          writeIORef input current
        end <- getMonotonicTimeNSec
        performGC
        after <- getRTSStats
        putStrLn (label ++ "-sample" ++ show sample ++ "," ++ show count ++ ","
          ++ show (length retained) ++ ",5," ++ show (end - start) ++ ","
          ++ show (allocated_bytes after - allocated_bytes before))
  where
    forceDecision rejects retained = do
      decision <- evaluate (validateRetainedFamilyInstances emptyFamInstEnv retained)
      case decision of
        JoinAccepted | not rejects -> pure ()
        JoinRejected FamilyInstanceConflict message | rejects -> void (evaluate (length message))
        other -> fail ("benchmark changed acceptance: " ++ show other)

-- Axiom occurrences include GHC's generated coercion prefix, so classify by
-- the original family Name rather than by a rendered axiom occurrence.
select :: String -> [FamInst] -> [FamInst]
select prefix = sortOn familyOccurrence . filter (isPrefixOf prefix . familyOccurrence)

familyOccurrence :: FamInst -> String
familyOccurrence = occNameString . nameOccName . tyConName . famInstTyCon

writeFixtures :: FilePath -> Int -> IO ()
writeFixtures root count = do
  writeFile (root </> "Definitions.hs") $ unlines $
    pragmas ++ ["module Definitions where", "type family Poly a = r | r -> a"
      , "class Owner a where", "  type Associated a", "  data AssociatedData a"]
    ++ concat [["type family Plain" ++ show n ++ " a"
      , "type family Injective" ++ show n ++ " a = r | r -> a"] | n <- [0 .. count - 1]]
  writeFile (root </> "Original.hs") $ unlines $ pragmas
    ++ ["module Original where", "import Definitions", "type instance Poly [a] = Maybe a"
      , "instance Owner Int where", "  type Associated Int = Bool"
      , "  data AssociatedData Int = OriginalData Bool"]
    ++ concat [["type instance Plain" ++ show n ++ " Int = Bool"
      , "type instance Injective" ++ show n ++ " Int = Bool"] | n <- [0 .. count - 1]]
  writeFile (root </> "Compatible.hs") $ unlines $ pragmas
    ++ ["module Compatible where", "import Definitions"]
    ++ concat [["type instance Plain" ++ show n ++ " Double = Char"
      , "type instance Injective" ++ show n ++ " Double = Char"] | n <- [0 .. count - 1]]
  writeFile (root </> "Conflict.hs") $ unlines $ pragmas
    ++ ["module Conflict where", "import Definitions", "type instance Plain0 Int = Char"
      , "instance Owner Int where", "  type Associated Int = Char"
      , "  data AssociatedData Int = ConflictingData Char"]
  writeFile (root </> "InjectiveConflict.hs") $ unlines $ pragmas
    ++ ["module InjectiveConflict where", "import Definitions"
      , "type instance Injective0 Double = Bool", "type instance Poly (Maybe a) = Maybe a"]
  writeFile (root </> "Shared.hs") $ unlines $ pragmas
    ++ ["module Shared where", "type family Shared a = r | r -> a"]
    ++ concat [["data Key" ++ show n ++ " = Key" ++ show n
      , "type instance Shared Key" ++ show n ++ " = Key" ++ show n] | n <- [0 .. count - 1]]
  where pragmas = ["{-# LANGUAGE TypeFamilies, TypeFamilyDependencies #-}"]

scratch :: IO FilePath
scratch = do
  parent <- getTemporaryDirectory
  (path, handle) <- openTempFile parent "tidepool-family-consistency"
  hClose handle
  removeFile path
  createDirectory path
  pure path
