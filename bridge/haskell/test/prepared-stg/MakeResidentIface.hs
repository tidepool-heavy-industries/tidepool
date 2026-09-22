module Main (main) where

import Control.Monad.IO.Class (liftIO)
import GHC (getSession, getSessionDynFlags, runGhc, setSessionDynFlags)
import GHC.Builtin.Types (intTy, mkListTy)
import GHC.Core.Type (mkVisFunTyMany)
import GHC.Types.Name.Occurrence (mkVarOcc)
import System.Environment (getArgs)
import Tidepool.Session
  ( Generation(..), SessionModule(..), SessionModuleKind(..)
  , mkThinSessionIface, writeSessionIface )

main :: IO ()
main = do
  args <- getArgs
  (libdir, sessionRoot) <- case args of
    [libdir, sessionRoot] -> pure (libdir, sessionRoot)
    _ -> ioError (userError "usage: MakeResidentIface GHC_LIBDIR SESSION_ROOT")
  runGhc (Just libdir) $ do
    flags <- getSessionDynFlags
    _ <- setSessionDynFlags flags
    session <- getSession
    let moduleId = SessionModule ValMod (Generation 1)
    iface <- liftIO $ mkThinSessionIface session moduleId
      [ (mkVarOcc "retainedClosure", mkVisFunTyMany intTy intTy)
      , (mkVarOcc "retainedEnvironment", mkListTy intTy)
      ]
    liftIO $ writeSessionIface session sessionRoot moduleId iface
