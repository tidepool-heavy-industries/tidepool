module Main where

import System.Environment (getArgs)
import System.FilePath (takeBaseName, takeDirectory, (</>))
import System.Directory (createDirectoryIfMissing)
import qualified Data.ByteString as BS
import qualified Data.Map.Strict as Map
import qualified Data.Sequence as Seq
import Control.Exception (try, SomeException)
import Control.Monad (foldM)

import GHC.Core (CoreBind, Bind(..))
import GHC.Core.DataCon (DataCon, dataConSourceArity, dataConTag, dataConWorkId, dataConName, dataConSrcBangs, HsSrcBang(..), HsBang(..), SrcUnpackedness(..), SrcStrictness(..))
import GHC.Types.Name (nameOccName, isExternalName)
import GHC.Types.Id (idName)
import GHC.Types.Name.Occurrence (occNameString)
import GHC.Types.Unique (getKey)
import GHC.Types.Var (varUnique)
import Data.Word (Word64)
import Data.Text (Text)
import qualified Data.Text as T

import Tidepool.GhcPipeline (runPipeline, PipelineResult(..), dumpCore)
import Tidepool.Translate (translateBinds, translateModuleClosed, collectDataCons, collectUsedDataCons, collectTransitiveDCons, UnresolvedVar(..))
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
  , argIncludes :: [FilePath]
  , argFiles :: [String]
  }

parseArgs :: [String] -> Args
parseArgs = go (Args Nothing Nothing False False [] [])
  where
    go a ("--output-dir" : dir : rest) = go a { argOutDir = Just dir } rest
    go a ("--target" : name : rest) = go a { argTarget = Just name } rest
    go a ("--dump-core" : rest) = go a { argDumpCore = True } rest
    go a ("--all-closed" : rest) = go a { argAllClosed = True } rest
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
        -- Only export bindings with External names (user-defined top-level).
        -- GHC-floated locals (isEven, go, etc.) have Internal/System names.
        let externalBinders = [ b | bind <- binds
                              , b <- case bind of
                                       NonRec b _ -> [b]
                                       Rec pairs  -> map fst pairs
                              , isExternalName (idName b) ]
            uniqueNames = Map.keys $ Map.fromList
              [(n, ()) | b <- externalBinders
              , let n = occNameString (nameOccName (idName b))
              , not (null n), head n /= '$']
        allMetaMap <- foldM (\acc name -> do
          let (nodes, usedDCs, unresolved) = translateModuleClosed binds name
          if not (null unresolved) then
            putStrLn $ "  WARNING (" ++ name ++ "): " ++ show (length unresolved) ++ " unresolved external(s)"
          else return ()
          let cbor = encodeTree nodes
          let outFile = outDir </> name ++ ".cbor"
          BS.writeFile outFile cbor
          putStrLn $ "  Wrote: " ++ outFile ++ " (" ++ show (Seq.length nodes) ++ " nodes, " ++ show (BS.length cbor) ++ " bytes)"
          let usedMeta = map dcToMeta (Map.elems usedDCs)
          return $ acc `Map.union` Map.fromList [(dcid, entry) | entry@(dcid, _, _, _, _) <- usedMeta]
          ) Map.empty uniqueNames

        -- Write merged metadata
        let tyconMeta = collectDataCons tycons
            scanMeta = collectUsedDataCons binds
            transitiveMeta = collectTransitiveDCons binds
            mergedMap = Map.fromList [(dcid, entry) | entry@(dcid, _, _, _, _) <- tyconMeta]
                        `Map.union` allMetaMap
                        `Map.union` Map.fromList [(dcid, entry) | entry@(dcid, _, _, _, _) <- scanMeta]
                        `Map.union` Map.fromList [(dcid, entry) | entry@(dcid, _, _, _, _) <- transitiveMeta]
            allMeta = Map.elems mergedMap
        let metaCbor = encodeMetadata allMeta
        let metaFile = outDir </> "meta.cbor"
        BS.writeFile metaFile metaCbor
        putStrLn $ "  Wrote: " ++ metaFile ++ " (" ++ show (length allMeta) ++ " entries, " ++ show (BS.length metaCbor) ++ " bytes)"

      (Just targetName, False) -> do
        -- Whole-module mode: serialize all bindings as nested lets around the target
        let (nodes, usedDCs, unresolved) = translateModuleClosed binds targetName
        if not (null unresolved) then do
          putStrLn $ "  WARNING: " ++ show (length unresolved) ++ " unresolved external(s):"
          mapM_ (\uv -> putStrLn $ "    " ++ uvModule uv ++ "." ++ uvName uv ++ " (v_" ++ show (uvKey uv) ++ ")") unresolved
        else return ()
        let cbor = encodeTree nodes
        let outFile = outDir </> targetName ++ ".cbor"
        BS.writeFile outFile cbor
        putStrLn $ "  Wrote: " ++ outFile ++ " (" ++ show (Seq.length nodes) ++ " nodes, " ++ show (BS.length cbor) ++ " bytes)"

        -- Write metadata: merge TyCon-derived + translation-derived + raw-binding-scan + transitive
        let tyconMeta = collectDataCons tycons
            usedMeta = map dcToMeta (Map.elems usedDCs)
            scanMeta = collectUsedDataCons binds
            transitiveMeta = collectTransitiveDCons binds
            mergedMap = Map.fromList [(dcid, entry) | entry@(dcid, _, _, _, _) <- tyconMeta]
                        `Map.union`
                        Map.fromList [(dcid, entry) | entry@(dcid, _, _, _, _) <- usedMeta]
                        `Map.union`
                        Map.fromList [(dcid, entry) | entry@(dcid, _, _, _, _) <- scanMeta]
                        `Map.union`
                        Map.fromList [(dcid, entry) | entry@(dcid, _, _, _, _) <- transitiveMeta]
            allMeta = Map.elems mergedMap
        let metaCbor = encodeMetadata allMeta
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

        -- Write DataCon metadata: merge TyCon-derived + usage-derived + transitive
        let tyconMeta = collectDataCons tycons
            usedMeta = collectUsedDataCons binds
            transitiveMeta = collectTransitiveDCons binds
            mergedMap = Map.fromList [(dcid, entry) | entry@(dcid, _, _, _, _) <- tyconMeta]
                        `Map.union`
                        Map.fromList [(dcid, entry) | entry@(dcid, _, _, _, _) <- usedMeta]
                        `Map.union`
                        Map.fromList [(dcid, entry) | entry@(dcid, _, _, _, _) <- transitiveMeta]
            allMeta = Map.elems mergedMap
        let metaCbor = encodeMetadata allMeta
        let metaFile = outDir </> "meta.cbor"
        BS.writeFile metaFile metaCbor
        putStrLn $ "  Wrote: " ++ metaFile ++ " (" ++ show (length allMeta) ++ " entries, " ++ show (BS.length metaCbor) ++ " bytes)"

  case res of
    Left (e :: SomeException) -> putStrLn $ "  Error processing " ++ path ++ ": " ++ show e
    Right () -> return ()

dcToMeta :: DataCon -> (Word64, Text, Int, Int, [Text])
dcToMeta dc =
  ( fromIntegral (getKey (varUnique (dataConWorkId dc)))
  , T.pack (occNameString (nameOccName (dataConName dc)))
  , dataConTag dc
  , dataConSourceArity dc
  , map mapBang (dataConSrcBangs dc)
  )

mapBang :: HsSrcBang -> Text
mapBang (HsSrcBang _ (HsBang srcUnpack srcBang)) =
  case (srcUnpack, srcBang) of
    (_, SrcStrict) -> "SrcBang"
    (SrcUnpack, _) -> "SrcUnpack"
    _              -> "NoSrcBang"

-- | Deduplicate binding names by appending _1, _2, etc. for collisions.
dedup :: Map.Map String Int -> [(String, a)] -> [(String, a)]
dedup _ [] = []
dedup seen ((name, val) : rest) =
  case Map.lookup name seen of
    Nothing -> (name, val) : dedup (Map.insert name 1 seen) rest
    Just n  -> (name ++ "_" ++ show n, val) : dedup (Map.insert name (n + 1) seen) rest
