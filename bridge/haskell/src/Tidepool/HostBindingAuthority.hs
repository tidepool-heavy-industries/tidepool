{-# LANGUAGE TemplateHaskell #-}

-- | Closed compiler-issued authority for values a resident host can build.
-- A host mount is allowed only when GHC resolved the exact root TyCon from one
-- of these authenticated owners.
module Tidepool.HostBindingAuthority
  ( HostBindingAuthority(..)
  , HostBindingAuthorities
  , resolveHostBindingAuthorities
  , classifyHostBindingAuthority
  ) where

import Control.Exception (IOException, try)
import Data.ByteString (ByteString)
import Data.ByteString qualified as BS
import GHC.Core.DataCon
  ( StrictnessMark(MarkedStrict), dataConName, dataConOrigArgTys, dataConRepStrictness )
import GHC.Core.TyCo.Rep (Scaled(..), Type(CastTy))
import GHC.Core.Type (coreView, splitTyConApp_maybe)
import GHC.Core.TyCon (TyCon, tyConDataCons, tyConName)
import GHC.Driver.Env (HscEnv, hsc_home_unit)
import GHC.Driver.Env.Types (hsc_unit_env)
import GHC.Tc.Utils.TcType (tcSplitSigmaTy)
import GHC.Types.Name (nameModule_maybe, nameOccName)
import GHC.Types.Name.Occurrence (occNameString)
import GHC.Types.PkgQual (PkgQual(NoPkgQual, OtherPkg))
import GHC.Unit.Finder (FindResult(..), findImportedModule)
import GHC.Unit.Module (Module, mkModuleName, moduleName, moduleNameString, moduleUnit)
import GHC.Unit.Module.Location (ml_hs_file)
import GHC.Unit.Env (ue_units)
import GHC.Unit.Info (PackageName(..))
import GHC.Unit.Home (homeUnitAsUnit)
import GHC.Unit.State (lookupPackageName)
import GHC.Data.FastString (fsLit)
import Language.Haskell.TH.Syntax (addDependentFile, lift, loc_filename, location, runIO)
import System.FilePath (takeDirectory, (</>))
import Tidepool.PreparedJson
  ( JsonAuthority, jsonValueLayoutForType, resolveJsonAuthority )

data HostBindingAuthority
  = JsonValueAuthority
  | TextAuthority
  | CommandJobAuthority
  deriving stock (Eq, Show)

data HostBindingAuthorities = HostBindingAuthorities
  { jsonValueAuthority :: Maybe JsonAuthority
  , textModule :: Maybe Module
  , commandJobModule :: Maybe Module
  }

shippedCommandTypesSource :: ByteString
shippedCommandTypesSource = BS.pack $(do
  here <- loc_filename <$> location
  let source = takeDirectory here </> ".." </> ".." </> "lib" </> "Tidepool" </> "Command" </> "Types.hs"
  addDependentFile source
  lift . BS.unpack =<< runIO (BS.readFile source))

-- | Resolve only authorities whose spelling occurs at a binding root. The
-- cheap candidate scan is deliberately before every module-finder and source
-- read: an ordinary @Int@ notebook bind must do no host-authority I/O.
resolveHostBindingAuthorities :: [Type] -> HscEnv -> IO HostBindingAuthorities
resolveHostBindingAuthorities roots env = do
  let needsJson = any (hasRoot "Tidepool.Aeson.Value" "Value") roots
      needsText = any (hasRoot "Data.Text.Internal" "Text") roots
      needsJob = any (hasRoot "Tidepool.Command.Types" "Job") roots
  jsonValueAuthority <- if needsJson
    then resolveJsonAuthority env
    else pure Nothing
  textModule <- if needsText || needsJob then resolveTextModule env else pure Nothing
  commandJobModule <- if needsJob then resolveCommandJobModule env else pure Nothing
  pure HostBindingAuthorities { jsonValueAuthority, textModule, commandJobModule }

resolveTextModule :: HscEnv -> IO (Maybe Module)
resolveTextModule env = case lookupPackageName
    (ue_units (hsc_unit_env env)) (PackageName (fsLit "text")) of
  Nothing -> pure Nothing
  Just selected -> do
    found <- findImportedModule env (mkModuleName "Data.Text.Internal") (OtherPkg selected)
    pure $ case found of
      Found _ owner -> Just owner
      _ -> Nothing

resolveCommandJobModule :: HscEnv -> IO (Maybe Module)
resolveCommandJobModule env = do
  found <- findImportedModule env (mkModuleName "Tidepool.Command.Types") NoPkgQual
  case found of
    Found moduleLocation owner
      | moduleUnit owner == homeUnitAsUnit (hsc_home_unit env)
      , Just source <- ml_hs_file moduleLocation -> do
      actual <- try (BS.readFile source) :: IO (Either IOException ByteString)
      pure $ case actual of
        Right bytes | bytes == shippedCommandTypesSource -> Just owner
        _ -> Nothing
    _ -> pure Nothing

-- | Classify only the exact outer TyCon. This neither reads a rendered type
-- nor descends into arguments, so @Job Text@ cannot borrow Text authority.
classifyHostBindingAuthority :: HostBindingAuthorities -> Type -> Maybe HostBindingAuthority
classifyHostBindingAuthority authorities ty = do
  case jsonValueAuthority authorities of
    Just authority | Just _ <- jsonValueLayoutForType authority ty -> Just JsonValueAuthority
    _ -> case textModule authorities of
      Just text | isExactRoot text "Data.Text.Internal" "Text" ty -> Just TextAuthority
      _ -> case (commandJobModule authorities, textModule authorities) of
        (Just job, Just text) | isExactJob job text ty -> Just CommandJobAuthority
        _ -> Nothing
rootTyCon :: Type -> Maybe TyCon
rootTyCon ty = go body
  where
    (_, _, body) = tcSplitSigmaTy ty
    go candidate = case coreView candidate of
      Just expanded -> go expanded
      Nothing -> case candidate of
        CastTy inner _ -> go inner
        _ -> fst <$> splitTyConApp_maybe candidate

hasRoot :: String -> String -> Type -> Bool
hasRoot expectedModule expectedName ty = case rootTyCon ty of
  Just tyCon -> case nameModule_maybe (tyConName tyCon) of
    Just owner ->
      moduleNameString (moduleName owner) == expectedModule
        && occNameString (nameOccName (tyConName tyCon)) == expectedName
    Nothing -> False
  Nothing -> False

isExactRoot :: Module -> String -> String -> Type -> Bool
isExactRoot expected expectedModule expectedName ty = case rootTyCon ty of
  Just tyCon -> case nameModule_maybe (tyConName tyCon) of
    Just owner -> owner == expected
      && moduleUnit owner == moduleUnit expected
      && moduleNameString (moduleName owner) == expectedModule
      && occNameString (nameOccName (tyConName tyCon)) == expectedName
    Nothing -> False
  Nothing -> False

isExactJob :: Module -> Module -> Type -> Bool
isExactJob jobModule textModule ty = case rootTyCon ty of
  Just jobTyCon
    | isExactRoot jobModule "Tidepool.Command.Types" "Job" ty ->
        case tyConDataCons jobTyCon of
          [constructor]
            | occNameString (nameOccName (dataConName constructor)) == "Job"
            , dataConRepStrictness constructor == [MarkedStrict]
            , [Scaled _ field] <- dataConOrigArgTys constructor ->
                isExactRoot textModule "Data.Text.Internal" "Text" field
          _ -> False
  _ -> False
