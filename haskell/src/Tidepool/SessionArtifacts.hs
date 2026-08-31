module Tidepool.SessionArtifacts
  ( mkBoundBinders
  , parseValModule
  ) where

import Control.Monad (forM_)
import GHC.Types.Name.Occurrence (mkVarOcc)
import Data.Word (Word64)
import System.IO (hPutStrLn, stderr)

import Tidepool.Binders (BoundBinder(..), ValueTier(..))
import Tidepool.GhcPipeline
  ( PipelineResult(..), isClosureType, renderType, stripMonadHead
  , splitTupleType )
import Tidepool.Identity (stableVarId)
import Tidepool.Session
  ( Generation(..), SessionModule(..), SessionModuleKind(..)
  , mkThinSessionIface, parseSessionModule, sessionBinderName
  , sessionModuleString, writeSessionIface )
import Tidepool.TypePolicy (stabilizeEffectRows)

-- | Describe and publish the values materialized by one session bind. The
-- captured result type is split for multi-binds, checked for cross-compilation
-- safety, and written as the thin interface later turns import.
mkBoundBinders :: [String] -> Word64 -> FilePath -> PipelineResult -> IO [BoundBinder]
mkBoundBinders bindNames generation root result = do
  resultType <- case prResultType result of
    Just ty -> pure ty
    Nothing -> error "session bind has no captured result type"
  let hsc = prHscEnv result
      sessionModule = SessionModule ValMod (Generation generation)
      valueType = stripMonadHead resultType
  componentTypes <- case bindNames of
    [_] -> pure [valueType]
    _ -> case splitTupleType valueType of
      Nothing -> error $ "multi-bind result is not a tuple: " ++ renderType valueType
      Just types
        | length types == length bindNames -> pure types
        | otherwise -> error $ "multi-bind has " ++ show (length bindNames)
            ++ " names but its result has " ++ show (length types) ++ " fields"
  let build name ty =
        let persistedType = stabilizeEffectRows ty
            occurrence = mkVarOcc name
            varId = stableVarId (sessionBinderName hsc sessionModule occurrence)
            moduleName = sessionModuleString sessionModule
            tier = if isClosureType persistedType then Tier1Closure else Tier0Data
            displayType = renderType ty
        in (BoundBinder name varId moduleName tier displayType, occurrence, persistedType)
      built = zipWith build bindNames componentTypes
      binders = [binder | (binder, _, _) <- built]
  iface <- mkThinSessionIface hsc sessionModule [(occ, ty) | (_, occ, ty) <- built]
  writeSessionIface hsc root sessionModule iface
  forM_ binders $ \(BoundBinder name varId moduleName tier displayType) ->
    hPutStrLn stderr $ "  Wrote session iface: " ++ moduleName ++ " (" ++ name
      ++ " :: " ++ displayType ++ ", " ++ show tier ++ ", varId " ++ show varId ++ ")"
  pure binders

parseValModule :: String -> Maybe SessionModule
parseValModule source = case parseSessionModule source of
  Just moduleName@(SessionModule ValMod _) -> Just moduleName
  _ -> Nothing
