module Main (main) where

import Data.ByteString qualified as BS
import DeferredFunctionProjectionTest (verifyDeferredFunctionProjection)
import ExecutionProjectionTest
  (projectProjectionContract, verifyRetainedImportProjection)
import System.Environment (getArgs)
import Tidepool.ExecutionEncode (encodeWireProgram)
import Tidepool.GhcPipeline
  ( PipelineSelection(PreparedStg), PreparedPipelineResult(..)
  , runPipelineSelected )
import System.Directory (getCurrentDirectory)
import System.FilePath ((</>))

main :: IO ()
main = do
  root <- getCurrentDirectory
  verifyDeferredFunctionProjection
  verifyRetainedImportProjection
  let fixtureDir = root </> "test-prepared-stg"
  result <- runPipelineSelected PreparedStg
    (fixtureDir </> "M3Vertical.hs") [fixtureDir]
  program <- projectProjectionContract (pprModules result)
  arguments <- getArgs
  case arguments of
    [] -> pure ()
    [output] -> BS.writeFile output (encodeWireProgram program)
    _ -> ioError (userError "usage: execution-schema-projection [output.cbor]")
