{-# LANGUAGE TemplateHaskell #-}

-- | Closed compiler-issued authority for values a resident host can build.
-- A host mount is allowed only when GHC resolved the exact root TyCon from one
-- of these authenticated owners.
module Tidepool.HostBindingAuthority
  ( HostBindingAuthority(..)
  , HostBindingAuthorities
  , resolveHostBindingAuthorities
  , HostBindingRepresentation
  , hostBindingRepresentationForType
  , hostBindingRepresentationAuthority
  , hostBindingRepresentationConstructors
  , hostBindingRepresentationJsonAuthority
  ) where

import Data.ByteString (ByteString)
import Data.ByteString qualified as BS
import Data.Foldable (toList)
import Data.Map.Strict qualified as Map
import GHC.Core.DataCon
  ( DataCon, StrictnessMark(MarkedStrict), dataConName, dataConOrigArgTys, dataConRepStrictness )
import GHC.Core.TyCo.Rep (Scaled(..), Type(CastTy))
import GHC.Core.Type (coreView, splitTyConApp_maybe)
import GHC.Core.TyCon (TyCon, tyConDataCons, tyConName)
import GHC.Driver.Env (HscEnv)
import GHC.Driver.Env.Types (hsc_unit_env)
import GHC.Tc.Utils.TcType (tcSplitSigmaTy)
import GHC.Types.Name (nameModule_maybe, nameOccName)
import GHC.Types.Name.Occurrence (occNameString)
import GHC.Types.PkgQual (PkgQual(OtherPkg))
import GHC.Unit.Finder (FindResult(..), findImportedModule)
import GHC.Unit.Module (Module, mkModuleName, moduleName, moduleNameString, moduleUnit)
import GHC.Unit.Env (ue_units)
import GHC.Unit.Info (PackageName(..))
import GHC.Unit.State (lookupPackageName)
import GHC.Data.FastString (fsLit)
import Language.Haskell.TH.Syntax (addDependentFile, lift, loc_filename, location, runIO)
import System.FilePath (takeDirectory, (</>))
import Tidepool.ExactScope (CanonicalInterfaceAdmission, resolveShippedHomeModule)
import Tidepool.PreparedJson
  ( JsonAuthority, jsonAuthorityLayout, jsonValueLayoutForType, resolveJsonAuthorityWithCanonicalInterfaces )

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
resolveHostBindingAuthorities :: [Type] -> HscEnv
  -> Map.Map (String,String) CanonicalInterfaceAdmission -> IO HostBindingAuthorities
resolveHostBindingAuthorities roots env admitted = do
  let needsJson = any (hasRoot "Tidepool.Aeson.Value" "Value") roots
      needsText = any (hasRoot "Data.Text.Internal" "Text") roots
      needsJob = any (hasRoot "Tidepool.Command.Types" "Job") roots
  jsonValueAuthority <- if needsJson
    then resolveJsonAuthorityWithCanonicalInterfaces env admitted
    else pure Nothing
  textModule <- if needsText || needsJob then resolveTextModule env else pure Nothing
  commandJobModule <- if needsJob then resolveCommandJobModule env admitted else pure Nothing
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

resolveCommandJobModule :: HscEnv
  -> Map.Map (String,String) CanonicalInterfaceAdmission -> IO (Maybe Module)
resolveCommandJobModule env admitted =
  resolveShippedHomeModule env admitted "Tidepool.Command.Types" shippedCommandTypesSource

-- The representation and authority are issued together. An authority tag
-- cannot survive native projection without its exact compiler constructors.
data HostBindingRepresentation
  = JsonValueRepresentation JsonAuthority
  | TextRepresentation DataCon
  | CommandJobRepresentation DataCon DataCon

hostBindingRepresentationAuthority :: HostBindingRepresentation -> HostBindingAuthority
hostBindingRepresentationAuthority representation = case representation of
  JsonValueRepresentation _ -> JsonValueAuthority
  TextRepresentation _ -> TextAuthority
  CommandJobRepresentation _ _ -> CommandJobAuthority

hostBindingRepresentationConstructors :: HostBindingRepresentation -> [DataCon]
hostBindingRepresentationConstructors representation = case representation of
  JsonValueRepresentation authority -> toList (jsonAuthorityLayout authority)
  TextRepresentation constructor -> [constructor]
  CommandJobRepresentation job text -> [job, text]

hostBindingRepresentationJsonAuthority :: HostBindingRepresentation -> Maybe JsonAuthority
hostBindingRepresentationJsonAuthority representation = case representation of
  JsonValueRepresentation authority -> Just authority
  _ -> Nothing

-- | Admit only the exact outer TyCon and its complete representation. This
-- neither reads a rendered type nor lends authority to its arguments.
hostBindingRepresentationForType :: HostBindingAuthorities -> Type -> Maybe HostBindingRepresentation
hostBindingRepresentationForType authorities ty =
  case jsonValueAuthority authorities of
    Just authority | Just _ <- jsonValueLayoutForType authority ty ->
      Just (JsonValueRepresentation authority)
    _ -> case textModule authorities of
      Just text | Just constructor <- exactTextConstructor text ty ->
        Just (TextRepresentation constructor)
      _ -> case (commandJobModule authorities, textModule authorities) of
        (Just job, Just text) -> do
          (jobConstructor, textConstructor) <- exactJobConstructors job text ty
          pure (CommandJobRepresentation jobConstructor textConstructor)
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

exactTextConstructor :: Module -> Type -> Maybe DataCon
exactTextConstructor owner ty = do
  textTyCon <- rootTyCon ty
  case tyConDataCons textTyCon of
    [text]
      | isExactRoot owner "Data.Text.Internal" "Text" ty
      , occNameString (nameOccName (dataConName text)) == "Text" -> Just text
    _ -> Nothing

exactJobConstructors :: Module -> Module -> Type -> Maybe (DataCon, DataCon)
exactJobConstructors jobModule textModule ty = do
  jobTyCon <- rootTyCon ty
  case tyConDataCons jobTyCon of
    [constructor]
      | isExactRoot jobModule "Tidepool.Command.Types" "Job" ty
      , occNameString (nameOccName (dataConName constructor)) == "Job"
      , dataConRepStrictness constructor == [MarkedStrict]
      , [Scaled _ field] <- dataConOrigArgTys constructor -> do
          text <- exactTextConstructor textModule field
          pure (constructor, text)
    _ -> Nothing
