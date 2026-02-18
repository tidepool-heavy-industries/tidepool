module Tidepool.GhcPipeline (runPipeline, PipelineResult(..), dumpCore) where

import GHC
import GHC.Driver.Main (hscDesugar)
import GHC.Core.Opt.Pipeline (core2core)
import GHC.Core.Ppr (pprCoreBindings)
import GHC.Driver.Session (updOptLevel, gopt_unset, GeneralFlag(..))
import GHC.Unit.Module.ModGuts (ModGuts(..))
import GHC.Core (CoreBind)
import GHC.Utils.Outputable (renderWithContext, defaultSDocContext)
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
    let dflags' = gopt_unset (updOptLevel 2 $ dflags
          { backend = noBackend
          , ghcLink = NoLink
          }) Opt_FullLaziness
    setSessionDynFlags dflags'
    target <- guessTarget path Nothing Nothing
    setTargets [target]
    _ <- depanal [] False
    modGraph <- getModuleGraph
    modSum <- case mgModSummaries modGraph of
      (ms:_) -> return ms
      []     -> liftIO $ ioError (userError "runPipeline: empty module graph")
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

dumpCore :: [CoreBind] -> String
dumpCore binds = renderWithContext defaultSDocContext (pprCoreBindings binds)

getLibdir :: IO FilePath
getLibdir = trim <$> readProcess "ghc" ["--print-libdir"] ""
  where trim = reverse . dropWhile (== '\n') . reverse
