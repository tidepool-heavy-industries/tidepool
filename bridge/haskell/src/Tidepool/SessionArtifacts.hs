module Tidepool.SessionArtifacts
  ( mkBoundBinders
  , parseValModule
  ) where

import Control.Monad (forM_)
import Control.Monad (forM)
import Data.List (nub)
import qualified Data.ByteString as BS
import qualified Data.Text as T
import Codec.CBOR.Encoding (encodeListLen, encodeString)
import Codec.CBOR.Write (toStrictByteString)
import GHC.Core.Type (tyConsOfType)
import GHC.Builtin.Names (gHC_PRIM)
import GHC.Core.TyCon (tyConName)
import GHC.Types.Unique.Set (nonDetEltsUniqSet)
import GHC.Types.Name (nameModule_maybe)
import GHC.Unit.Module (moduleName, moduleNameString, moduleUnit)
import GHC.Unit.Types (unitString)
import GHC.Unit.Home (homeUnitAsUnit)
import GHC.Driver.Env (hsc_home_unit)
import GHC.Types.Name.Occurrence (mkVarOcc)
import Data.Word (Word64)
import System.IO (hPutStrLn, stderr)

import Tidepool.Binders (BoundBinder(..), ValueTier(..))
import Tidepool.GhcPipeline
  ( PipelineResult(..), isClosureType, renderType, stripMonadHead
  , splitTupleType )
import Tidepool.Identity (stableVarId)
import Tidepool.HostBindingAuthority
  ( classifyHostBindingAuthority, resolveHostBindingAuthorities )
import Tidepool.Session
  ( Generation(..), SessionModule(..), SessionModuleKind(..)
  , mkThinSessionIface, parseSessionModule, sessionBinderName
  , sessionModuleString, writeSessionIface )
import Tidepool.Session (sessionHiPath)
import Tidepool.ExactHydration (ExactIfaceArtifact(..))
import Tidepool.PackageWitness (encodePackageImports, packageImportRoot)
import qualified Crypto.Hash.SHA256 as SHA256
import Numeric (showHex)
import Tidepool.TypePolicy (rootNominalHeadOfType, stabilizeEffectRows)

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
  let persistedTypes = map stabilizeEffectRows componentTypes
  authorities <- resolveHostBindingAuthorities persistedTypes hsc
  let build name ty persistedType =
        let
            occurrence = mkVarOcc name
            varId = stableVarId (sessionBinderName hsc sessionModule occurrence)
            moduleName = sessionModuleString sessionModule
            tier = if isClosureType persistedType then RetainOpaque else ForceData
            displayType = renderType ty
            rootHead = rootNominalHeadOfType persistedType
            hostAuthority = classifyHostBindingAuthority authorities persistedType
        in (BoundBinder name varId moduleName tier displayType rootHead hostAuthority, occurrence, persistedType)
      built = zipWith3 build bindNames componentTypes persistedTypes
      binders = [binder | (binder, _, _) <- built]
  iface <- mkThinSessionIface hsc sessionModule [(occ, ty) | (_, occ, ty) <- built]
  writeSessionIface hsc root sessionModule iface
  let path = sessionHiPath root sessionModule
      owners = nub [owner | ty <- persistedTypes
        , constructor <- nonDetEltsUniqSet (tyConsOfType ty)
        , Just owner <- [nameModule_maybe (tyConName constructor)]]
      home = homeUnitAsUnit (hsc_home_unit hsc)
      requirements = [(unitString (moduleUnit owner), moduleNameString (moduleName owner))
        | owner <- owners, moduleUnit owner == home]
  roots <- forM [owner | owner <- owners, moduleUnit owner /= home, owner /= gHC_PRIM] $ \owner ->
    packageImportRoot hsc owner >>= either fail pure
  bytes <- BS.readFile path
  let digest = concatMap (\byte -> let rendered = showHex byte "" in
        replicate (2 - length rendered) '0' ++ rendered) (BS.unpack (SHA256.hash bytes))
      artifact = ExactIfaceArtifact (unitString home) (sessionModuleString sessionModule) path digest requirements
      text = encodeString . T.pack
  BS.writeFile (path ++ ".packages") (encodePackageImports artifact roots)
  BS.writeFile (path ++ ".requirements") (toStrictByteString
    (encodeListLen (fromIntegral (length requirements))
      <> foldMap (\(unit,owner) -> encodeListLen 2 <> text unit <> text owner) requirements))
  forM_ binders $ \(BoundBinder name varId moduleName tier displayType rootHead hostAuthority) ->
    hPutStrLn stderr $ "  Wrote session iface: " ++ moduleName ++ " (" ++ name
      ++ " :: " ++ displayType ++ ", " ++ show tier ++ ", root " ++ show rootHead
      ++ ", authority " ++ show hostAuthority ++ ", varId " ++ show varId ++ ")"
  pure binders

parseValModule :: String -> Maybe SessionModule
parseValModule source = case parseSessionModule source of
  Just moduleName@(SessionModule ValMod _) -> Just moduleName
  _ -> Nothing
