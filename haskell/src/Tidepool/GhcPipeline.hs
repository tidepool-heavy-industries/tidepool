module Tidepool.GhcPipeline (runPipeline, PipelineResult(..)) where

import GHC
import GHC.Driver.Main (hscDesugar)
import GHC.Core.Opt.Pipeline (core2core)
import GHC.Driver.Session (updOptLevel)
import GHC.Driver.Backend (noBackend)
import GHC.Unit.Module.ModGuts (ModGuts(..))
import GHC.Core (CoreBind)
import GHC.Core.TyCon (TyCon)
import System.Process (readProcess)
import Control.Monad.IO.Class (liftIO)

data PipelineResult = PipelineResult
  { prBinds  :: [CoreBind]
  , prTyCons :: [TyCon]
  }

runPipeline :: FilePath -> IO PipelineResult
runPipeline path = do
  libdir <- getLibdir
  runGhc (Just libdir) $ do
    dflags <- getSessionDynFlags
    let dflags' = updOptLevel 2 $ dflags
          { backend = noBackend
          , ghcLink = NoLink
          }
    setSessionDynFlags dflags'
    target <- guessTarget path Nothing Nothing
    setTargets [target]
    _ <- depanal [] False
    modGraph <- getModuleGraph
    let modSum = head (mgModSummaries modGraph)
    parsed <- parseModule modSum
    typechecked <- typecheckModule parsed
    hscEnv <- getSession
    let tcGblEnv = fst (tm_internals_ typechecked)
    desugared <- liftIO $ hscDesugar hscEnv modSum tcGblEnv
    simplified <- liftIO $ core2core hscEnv desugared
    return PipelineResult
      { prBinds  = mg_binds simplified
      , prTyCons = mg_tcs simplified
      }

getLibdir :: IO FilePath
getLibdir = trim <$> readProcess "ghc" ["--print-libdir"] ""
  where trim = reverse . dropWhile (== '\n') . reverse
