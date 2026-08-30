module Tidepool.SessionArtifacts
  ( emitBindArtifacts
  , mkBoundBinders
  , parseValModule
  ) where

import Control.Monad (forM_)
import GHC.Types.Name.Occurrence (mkVarOcc)
import Data.Word (Word64)
import System.IO (hPutStrLn, stderr)

import Tidepool.Binders (BoundBinder(..), renderBoundBinderJson)
import Tidepool.ExtractRequest (WorkerRequest(..))
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
            tier = if isClosureType persistedType then "Tier1Closure" else "Tier0Data"
            displayType = renderType ty
        in (BoundBinder name varId moduleName tier displayType, occurrence, persistedType)
      built = zipWith build bindNames componentTypes
      binders = [binder | (binder, _, _) <- built]
  iface <- mkThinSessionIface hsc sessionModule [(occ, ty) | (_, occ, ty) <- built]
  writeSessionIface hsc root sessionModule iface
  forM_ binders $ \(BoundBinder name varId moduleName tier displayType) ->
    hPutStrLn stderr $ "  Wrote session iface: " ++ moduleName ++ " (" ++ name
      ++ " :: " ++ displayType ++ ", " ++ tier ++ ", varId " ++ show varId ++ ")"
  pure binders

-- | Emit the interface and optional binder-description sidecar for a bind
-- request.
emitBindArtifacts :: WorkerRequest -> PipelineResult -> IO ()
emitBindArtifacts request result = do
  bindNames <- case requestBindNames request of
    [] -> error "session bind requires at least one binder"
    names -> pure names
  generation <- requireField "bind generation" (requestBindGen request)
  root <- requireField "session root" (requestSessionRoot request)
  binders <- mkBoundBinders bindNames generation root result
  forM_ (requestEmitBoundBinders request) $ \output -> do
    writeFile output (renderBoundBindersJson binders)
    hPutStrLn stderr $ "  Wrote bound-binder sidecar: " ++ output

parseValModule :: String -> Maybe SessionModule
parseValModule source = case parseSessionModule source of
  Just moduleName@(SessionModule ValMod _) -> Just moduleName
  _ -> Nothing

requireField :: String -> Maybe a -> IO a
requireField name = maybe (error ("session bind requires " ++ name)) pure

renderBoundBindersJson :: [BoundBinder] -> String
renderBoundBindersJson binders =
  "{\"binders\":[" ++ commaSep (map renderBoundBinderJson binders) ++ "]}"
  where
    commaSep [] = ""
    commaSep [item] = item
    commaSep (item : items) = item ++ "," ++ commaSep items
