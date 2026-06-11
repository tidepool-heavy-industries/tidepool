module Main where

import System.Environment (getArgs)
import System.FilePath (takeBaseName, takeDirectory, (</>))
import System.Directory (createDirectoryIfMissing)
import qualified Data.ByteString as BS
import qualified Data.Map.Strict as Map
import qualified Data.Sequence as Seq
import Control.Exception (evaluate, try, SomeException, fromException)
import Data.Char (toUpper)
import Data.List (isPrefixOf)
import Control.Monad (foldM)
import System.Exit (exitFailure)
import System.IO (hPutStrLn, stderr)

import GHC.Types.SourceError (SourceError)
import GHC (moduleName, moduleNameString)
import GHC.Core (CoreBind, Bind(..))
import GHC.Core.DataCon (DataCon)
import GHC.Types.Name (nameOccName, isExternalName, nameModule_maybe)
import GHC.Types.Id (idName)
import GHC.Types.Name.Occurrence (occNameString)
import GHC.Types.Unique (getKey)
import GHC.Types.Var (varUnique)
import Data.Word (Word64)
import Data.Text (Text)
import qualified Data.Text as T

import Tidepool.GhcPipeline (runPipeline, PipelineResult(..), dumpCore)
import Tidepool.Translate (translateBinds, translateModuleClosed, collectDataCons, collectUsedDataCons, collectTransitiveDCons, wiredInDataCons, UnresolvedVar(..), dcToMeta, valueRepArity, mapBang, targetBindingHasIO)
import Tidepool.CborEncode (encodeTree, encodeMetadata)

main :: IO ()
main = do
  rawArgs <- getArgs
  let args = parseArgs rawArgs
  case argFiles args of
    [] -> putStrLn "Usage: tidepool-harness [--output-dir <dir>] [--target <name>] [--include <dir>] [--dump-core] <file.hs> ..."
    files -> mapM_ (processFile args) files

data Args = Args
  { argOutDir :: Maybe FilePath
  , argTarget :: Maybe String
  , argDumpCore :: Bool
  , argAllClosed :: Bool
  , argTargetModuleOnly :: Bool
  , argIncludes :: [FilePath]
  , argFiles :: [String]
  }

parseArgs :: [String] -> Args
parseArgs = go (Args Nothing Nothing False False False [] [])
  where
    go a ("--output-dir" : dir : rest) = go a { argOutDir = Just dir } rest
    go a ("--target" : name : rest) = go a { argTarget = Just name } rest
    go a ("--dump-core" : rest) = go a { argDumpCore = True } rest
    go a ("--all-closed" : rest) = go a { argAllClosed = True } rest
    go a ("--target-module-only" : rest) = go a { argTargetModuleOnly = True } rest
    go a ("--include" : dir : rest) = go a { argIncludes = argIncludes a ++ [dir] } rest
    go a (x : rest) = go a { argFiles = argFiles a ++ [x] } rest
    go a [] = a

processFile :: Args -> FilePath -> IO ()
processFile args path = do
  let mOutDir = argOutDir args
      mTarget = argTarget args
  putStrLn $ "Processing: " ++ path
  res <- try $ do
    result <- runPipeline path (argIncludes args)
    let binds = prBinds result
        tycons = prTyCons result
        hscEnv = prHscEnv result
    putStrLn $ "  Top-level bindings: " ++ show (length binds)

    if argDumpCore args
      then putStrLn (dumpCore binds)
      else return ()

    let outDir = case mOutDir of
          Just dir -> dir
          Nothing  -> takeDirectory path </> takeBaseName path ++ "_cbor"
    createDirectoryIfMissing True outDir

    case (mTarget, argAllClosed args) of
      (_, True) -> do
        -- All-closed mode: translate each binding independently via translateModuleClosed
        -- Use original names (not deduped) since translateModuleClosed looks up by name.
        -- Skip duplicates (GHC may produce multiple bindings with the same name).
        -- Include all top-level binders, not just External ones.
        -- GHC may mark user-defined bindings as Internal after optimization.
        -- Filter out GHC-generated names (starting with '$').
        -- Errors from translateModuleClosed are caught and those bindings are skipped.
        -- With --target-module-only, restrict fixture emission to binders
        -- DEFINED in the target module (by basename convention, mirroring
        -- GhcPipeline). Dep-module bindings (e.g. quasi-quoter internals
        -- from Tidepool.QQ) still participate in closed translation as
        -- dependencies — they just don't get their own fixtures, keeping
        -- the fixture sweep (and the JIT differential that walks it) to
        -- user-authored bindings.
        let targetModName = capitalizeMod (takeBaseName path)
            keepBinder b
              | not (argTargetModuleOnly args) = True
              | otherwise = case nameModule_maybe (idName b) of
                  Just m  -> moduleNameString (moduleName m) == targetModName
                  Nothing -> True
            allBinders = [ b | bind <- binds
                         , b <- case bind of
                                  NonRec b _ -> [b]
                                  Rec pairs  -> map fst pairs ]
            uniqueNames = Map.keys $ Map.fromList
              [(n, ()) | b <- allBinders
              , keepBinder b
              , let n = occNameString (nameOccName (idName b))
              , not ("$" `isPrefixOf` n)]
        (allMetaMap, allClosedBinds) <- foldM (\(acc, closedAcc) name -> do
          result <- try $ do
            (nodes, usedDCs, unresolved, closedBinds) <- translateModuleClosed hscEnv binds name
            if not (null unresolved) then do
              let names = map (\uv -> uvModule uv ++ "." ++ uvName uv) unresolved
              putStrLn $ "  SKIPPED (" ++ name ++ "): unresolved external(s): " ++ unwords names
              return Nothing
            else do
              let cbor = encodeTree nodes
              -- Force CBOR encoding to surface errors from lazy thunks (e.g. unsupported FFI calls)
              _ <- evaluate (BS.length cbor)
              let outFile = outDir </> name ++ ".cbor"
              BS.writeFile outFile cbor
              putStrLn $ "  Wrote: " ++ outFile ++ " (" ++ show (Seq.length nodes) ++ " nodes, " ++ show (BS.length cbor) ++ " bytes)"
              let usedMeta = map dcToMeta (Map.elems usedDCs)
              return (Just (Map.fromList [(dcid, entry) | entry@(dcid, _, _, _, _, _) <- usedMeta], closedBinds))
          case result of
            Left (e :: SomeException) -> do
              hPutStrLn stderr $ "  SKIPPED (" ++ name ++ "): " ++ show e
              return (acc, closedAcc)
            Right Nothing -> return (acc, closedAcc)
            Right (Just (metaMap, closedBinds)) ->
              return (acc `Map.union` metaMap, closedAcc ++ closedBinds)
          ) (Map.empty, []) uniqueNames

        -- Write merged metadata
        let tyconMeta = collectDataCons tycons
            scanMeta = collectUsedDataCons allClosedBinds
            transitiveMeta = collectTransitiveDCons allClosedBinds
            wiredInMeta = wiredInDataCons
            mergedMap = Map.fromList [(dcid, entry) | entry@(dcid, _, _, _, _, _) <- wiredInMeta]
                        `Map.union` Map.fromList [(dcid, entry) | entry@(dcid, _, _, _, _, _) <- tyconMeta]
                        `Map.union` allMetaMap
                        `Map.union` Map.fromList [(dcid, entry) | entry@(dcid, _, _, _, _, _) <- scanMeta]
                        `Map.union` Map.fromList [(dcid, entry) | entry@(dcid, _, _, _, _, _) <- transitiveMeta]
            allMeta = Map.elems mergedMap
            hasIO = any (targetBindingHasIO binds) uniqueNames
        let metaCbor = encodeMetadata allMeta hasIO
        let metaFile = outDir </> "meta.cbor"
        BS.writeFile metaFile metaCbor
        putStrLn $ "  Wrote: " ++ metaFile ++ " (" ++ show (length allMeta) ++ " entries, " ++ show (BS.length metaCbor) ++ " bytes)"

      (Just targetName, False) -> do
        -- Whole-module mode: serialize all bindings as nested lets around the target
        (nodes, usedDCs, unresolved, closedBinds) <- translateModuleClosed hscEnv binds targetName
        if not (null unresolved) then do
          let names = map (\uv -> uvModule uv ++ "." ++ uvName uv) unresolved
          error $ "Unresolved external(s): " ++ unwords names
            ++ "\nThese functions don't expose their implementation to the GHC API."
            ++ "\nDefine them in your source or use equivalent inline definitions."
        else return ()
        let cbor = encodeTree nodes
        let outFile = outDir </> targetName ++ ".cbor"
        BS.writeFile outFile cbor
        putStrLn $ "  Wrote: " ++ outFile ++ " (" ++ show (Seq.length nodes) ++ " nodes, " ++ show (BS.length cbor) ++ " bytes)"

        -- Write metadata: merge TyCon-derived + translation-derived + raw-binding-scan + transitive + wired-in
        let tyconMeta = collectDataCons tycons
            usedMeta = map dcToMeta (Map.elems usedDCs)
            scanMeta = collectUsedDataCons closedBinds
            transitiveMeta = collectTransitiveDCons closedBinds
            wiredInMeta = wiredInDataCons
            mergedMap = Map.fromList [(dcid, entry) | entry@(dcid, _, _, _, _, _) <- wiredInMeta]
                        `Map.union`
                        Map.fromList [(dcid, entry) | entry@(dcid, _, _, _, _, _) <- tyconMeta]
                        `Map.union`
                        Map.fromList [(dcid, entry) | entry@(dcid, _, _, _, _, _) <- usedMeta]
                        `Map.union`
                        Map.fromList [(dcid, entry) | entry@(dcid, _, _, _, _, _) <- scanMeta]
                        `Map.union`
                        Map.fromList [(dcid, entry) | entry@(dcid, _, _, _, _, _) <- transitiveMeta]
            allMeta = Map.elems mergedMap
            hasIO = targetBindingHasIO binds targetName
        let metaCbor = encodeMetadata allMeta hasIO
        let metaFile = outDir </> "meta.cbor"
        BS.writeFile metaFile metaCbor
        putStrLn $ "  Wrote: " ++ metaFile ++ " (" ++ show (length allMeta) ++ " entries, " ++ show (BS.length metaCbor) ++ " bytes)"

      (Nothing, False) -> do
        -- Per-binding mode (original behavior)
        let translated = translateBinds binds
            dedupd = dedup Map.empty translated
        mapM_ (\(name, nodes) -> do
          let cbor = encodeTree nodes
          let outFile = outDir </> name ++ ".cbor"
          BS.writeFile outFile cbor
          putStrLn $ "  Wrote: " ++ outFile ++ " (" ++ show (Seq.length nodes) ++ " nodes, " ++ show (BS.length cbor) ++ " bytes)"
          ) dedupd

        -- Write DataCon metadata: merge TyCon-derived + usage-derived + transitive + wired-in
        let tyconMeta = collectDataCons tycons
            usedMeta = collectUsedDataCons binds
            transitiveMeta = collectTransitiveDCons binds
            wiredInMeta = wiredInDataCons
            mergedMap = Map.fromList [(dcid, entry) | entry@(dcid, _, _, _, _, _) <- wiredInMeta]
                        `Map.union`
                        Map.fromList [(dcid, entry) | entry@(dcid, _, _, _, _, _) <- tyconMeta]
                        `Map.union`
                        Map.fromList [(dcid, entry) | entry@(dcid, _, _, _, _, _) <- usedMeta]
                        `Map.union`
                        Map.fromList [(dcid, entry) | entry@(dcid, _, _, _, _, _) <- transitiveMeta]
            allMeta = Map.elems mergedMap
        let metaCbor = encodeMetadata allMeta False
        let metaFile = outDir </> "meta.cbor"
        BS.writeFile metaFile metaCbor
        putStrLn $ "  Wrote: " ++ metaFile ++ " (" ++ show (length allMeta) ++ " entries, " ++ show (BS.length metaCbor) ++ " bytes)"

  case res of
    Left (e :: SomeException) -> do
      case fromException e of
        -- Show the actual GHC diagnostics: under the GHC API the error
        -- messages are carried in the SourceError, NOT printed to stderr by
        -- 'load' — swallowing them leaves callers (tidepool-runtime
        -- ExtractFailed) with a bare "Compilation failed." and nothing to act
        -- on. SourceError's Show instance renders the message envelopes.
        Just (se :: SourceError) -> hPutStrLn stderr ("Compilation failed.\n" ++ show se)
        Nothing -> hPutStrLn stderr $ "Error: " ++ show e
      exitFailure
    Right () -> return ()

-- | Module name from file basename, mirroring GhcPipeline's convention.
capitalizeMod :: String -> String
capitalizeMod [] = []
capitalizeMod (c:cs) = toUpper c : cs

-- | Deduplicate binding names by appending _1, _2, etc. for collisions.
dedup :: Map.Map String Int -> [(String, a)] -> [(String, a)]
dedup _ [] = []
dedup seen ((name, val) : rest) =
  case Map.lookup name seen of
    Nothing -> (name, val) : dedup (Map.insert name 1 seen) rest
    Just n  -> (name ++ "_" ++ show n, val) : dedup (Map.insert name (n + 1) seen) rest
