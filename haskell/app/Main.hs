module Main where

import System.Environment (getArgs)
import System.FilePath (takeBaseName, takeDirectory, (</>))
import System.Directory (createDirectoryIfMissing)
import qualified Data.ByteString as BS
import qualified Data.Sequence as Seq
import Control.Exception (try, SomeException)

import Tidepool.GhcPipeline (runPipeline, PipelineResult(..))
import Tidepool.Translate (translateBinds, collectDataCons)
import Tidepool.CborEncode (encodeTree, encodeMetadata)

main :: IO ()
main = do
  args <- getArgs
  case args of
    [] -> putStrLn "Usage: tidepool-harness <file.hs> ..."
    files -> mapM_ processFile files

processFile :: FilePath -> IO ()
processFile path = do
  putStrLn $ "Processing: " ++ path
  res <- try $ do
    result <- runPipeline path
    let binds = prBinds result
        tycons = prTyCons result
    putStrLn $ "  Top-level bindings: " ++ show (length binds)
    
    let outDir = takeDirectory path </> takeBaseName path ++ "_cbor"
    createDirectoryIfMissing True outDir
    
    -- Translate and encode each binding
    let translated = translateBinds binds
    mapM_ (\(name, nodes) -> do
      let cbor = encodeTree nodes
      let outFile = outDir </> name ++ ".cbor"
      BS.writeFile outFile cbor
      putStrLn $ "  Wrote: " ++ outFile ++ " (" ++ show (Seq.length nodes) ++ " nodes, " ++ show (BS.length cbor) ++ " bytes)"
      ) translated
    
    -- Write DataCon metadata
    let meta = collectDataCons tycons
    let metaCbor = encodeMetadata meta
    let metaFile = outDir </> "meta.cbor"
    BS.writeFile metaFile metaCbor
    putStrLn $ "  Wrote: " ++ metaFile ++ " (" ++ show (length meta) ++ " entries, " ++ show (BS.length metaCbor) ++ " bytes)"
  
  case res of
    Left (e :: SomeException) -> putStrLn $ "  Error processing " ++ path ++ ": " ++ show e
    Right () -> return ()
