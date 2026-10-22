{-# LANGUAGE TemplateHaskell #-}

-- | Closed compiler-issued authority for values a resident host can build.
-- A host mount is allowed only when GHC resolved the exact root TyCon from one
-- of these authenticated owners.
module Tidepool.HostBindingAuthority
  ( HostBindingAuthority(..)
  , HostBindingAuthorities
  , resolveHostBindingAuthorities, hostBindingOriginsRequired
  , HostBindingRepresentation
  , hostBindingRepresentationForType
  , HostBindingDecision(..), HostBindingRefusal(..), HostRepresentationRefusal(..)
  , classifyHostBinding, renderHostBindingRefusal, observeHostBindingDecision
  , hostBindingRepresentationAuthority
  , hostBindingRepresentationConstructors
  , hostBindingRepresentationJsonAuthority
  ) where

import Control.Monad (unless)
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
import GHC.Unit.Types (unitString)
import GHC.Unit.Module (Module, mkModuleName, moduleName, moduleNameString, moduleUnit)
import GHC.Unit.Env (ue_units)
import GHC.Unit.Info (PackageName(..))
import GHC.Unit.State (lookupPackageName)
import GHC.Data.FastString (fsLit)
import Language.Haskell.TH.Syntax (addDependentFile, lift, loc_filename, location, runIO)
import System.FilePath (takeDirectory, (</>))
import Tidepool.ExactScope
  ( CanonicalInterfaceAdmission, resolveShippedHomeModuleWithEvidence
  , ShippedHomeModuleRefusal, ShippedHomeModuleOrigin )
import Tidepool.Logging (LogLevel(..), LogValue(..), readLogger, logEvent)
import Tidepool.PreparedJson
  ( JsonAuthority, jsonAuthorityLayout, jsonValueLayoutForType, resolveJsonAuthorityWithCanonicalInterfaces )

data HostBindingAuthority
  = JsonValueAuthority
  | TextAuthority
  | CommandJobAuthority
  deriving stock (Eq, Show)

data HostBindingAuthorities = HostBindingAuthorities
  { jsonValueAuthority :: Maybe JsonAuthority
  , textModule :: Either HostRepresentationRefusal Module
  , commandJobModule :: Maybe (Either ShippedHomeModuleRefusal (Module,ShippedHomeModuleOrigin))
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
-- Text is package-owned; only JSON and Job require HOME source origins.
-- Test this before obtaining the operation's selected TYPE origin context.
hostBindingOriginsRequired :: [Type] -> Bool
hostBindingOriginsRequired = any (\ty -> hasRoot "Tidepool.Aeson.Value" "Value" ty
  || hasRoot "Tidepool.Command.Types" "Job" ty)

resolveHostBindingAuthorities :: [Type] -> HscEnv
  -> Map.Map (String,String) CanonicalInterfaceAdmission -> IO HostBindingAuthorities
resolveHostBindingAuthorities roots env admitted = do
  let needsJson = any (hasRoot "Tidepool.Aeson.Value" "Value") roots
      needsText = any (hasRoot "Data.Text.Internal" "Text") roots
      needsJob = any (hasRoot "Tidepool.Command.Types" "Job") roots
  jsonValueAuthority <- if needsJson
    then resolveJsonAuthorityWithCanonicalInterfaces env admitted
    else pure Nothing
  textModule <- if needsText || needsJob then resolveTextModule env else pure (Left HostTextNotRequested)
  commandJobModule <- if needsJob then Just <$> resolveCommandJobModule env admitted else pure Nothing
  pure HostBindingAuthorities { jsonValueAuthority, textModule, commandJobModule }

resolveTextModule :: HscEnv -> IO (Either HostRepresentationRefusal Module)
resolveTextModule env = case lookupPackageName
    (ue_units (hsc_unit_env env)) (PackageName (fsLit "text")) of
  Nothing -> pure (Left HostTextPackageUnavailable)
  Just selected -> do
    found <- findImportedModule env (mkModuleName "Data.Text.Internal") (OtherPkg selected)
    pure $ case found of
      Found _ owner -> Right owner
      _ -> Left HostTextModuleUnavailable

resolveCommandJobModule :: HscEnv
  -> Map.Map (String,String) CanonicalInterfaceAdmission
  -> IO (Either ShippedHomeModuleRefusal (Module,ShippedHomeModuleOrigin))
resolveCommandJobModule env admitted =
  resolveShippedHomeModuleWithEvidence env admitted "Tidepool.Command.Types" shippedCommandTypesSource

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

data HostRepresentationRefusal
  = HostTextNotRequested
  | HostTextPackageUnavailable
  | HostTextModuleUnavailable
  | HostJsonAuthorityUnavailable
  | HostJsonRepresentationMismatch
  | HostJobOriginUnavailable
  | HostRootOwnerMismatch (String,String) (Maybe (String,String))
  | HostConstructorCount String Int
  | HostConstructorName String String
  | HostJobStrictness [Bool]
  | HostJobFieldCount Int
  | HostTypeHasNoRoot
  deriving (Eq, Show)

data HostBindingRefusal
  = HostOriginRefused ShippedHomeModuleRefusal
  | HostRepresentationRefused HostRepresentationRefusal
  deriving (Show)

data HostBindingDecision
  = NotHostBinding
  | HostBindingRejected HostBindingRefusal
  | HostBindingAccepted HostBindingRepresentation

renderHostBindingRefusal :: HostBindingRefusal -> String
-- A refusal message is diagnostic evidence, not a limit on a valid type.
renderHostBindingRefusal = take 4096 . show

-- Candidate spelling only decides whether authentication is relevant. The
-- selected module and complete constructor representation still issue authority.
classifyHostBinding :: HostBindingAuthorities -> Type -> HostBindingDecision
classifyHostBinding authorities ty
  | hasRoot "Tidepool.Aeson.Value" "Value" ty = accepted $ do
      authority <- maybe (Left (HostRepresentationRefused HostJsonAuthorityUnavailable)) Right
        (jsonValueAuthority authorities)
      _ <- maybe (Left (HostRepresentationRefused HostJsonRepresentationMismatch)) Right
        (jsonValueLayoutForType authority ty)
      Right (JsonValueRepresentation authority)
  | hasRoot "Data.Text.Internal" "Text" ty = accepted $ do
      text <- representation (textModule authorities)
      TextRepresentation <$> representation (checkedTextConstructor text ty)
  | hasRoot "Tidepool.Command.Types" "Job" ty = accepted $ do
      job <- maybe (Left (HostRepresentationRefused HostJobOriginUnavailable))
        (either (Left . HostOriginRefused) (Right . fst)) (commandJobModule authorities)
      text <- representation (textModule authorities)
      (jobConstructor,textConstructor) <- representation (checkedJobConstructors job text ty)
      Right (CommandJobRepresentation jobConstructor textConstructor)
  | otherwise = NotHostBinding
  where
    representation :: Either HostRepresentationRefusal a -> Either HostBindingRefusal a
    representation = either (Left . HostRepresentationRefused) Right
    accepted = either HostBindingRejected HostBindingAccepted

-- | Admit only the exact outer TyCon and its complete representation.
hostBindingRepresentationForType :: HostBindingAuthorities -> Type -> Maybe HostBindingRepresentation
hostBindingRepresentationForType authorities ty = case classifyHostBinding authorities ty of
  HostBindingAccepted representation -> Just representation
  NotHostBinding -> Nothing
  HostBindingRejected _ -> Nothing

-- Ordinary values never enter diagnostic configuration or authority I/O.
-- Binder names include their stage generation for correlation with item output.
observeHostBindingDecision :: String -> HostBindingAuthorities -> Type -> IO ()
observeHostBindingDecision binder authorities ty = case classifyHostBinding authorities ty of
  NotHostBinding -> pure ()
  HostBindingAccepted value -> observe (show (hostBindingRepresentationAuthority value))
  HostBindingRejected refusal -> observe (renderHostBindingRefusal refusal)
  where
   observe result = do
    logger <- readLogger "host_binding_authority"
    let root = case rootTyCon ty of
          Nothing -> Nothing
          Just value -> fmap moduleOwner (nameModule_maybe (tyConName value))
    logEvent logger LogDebug "classified" [ ("binder",LogText binder)
      , ("root_owner",LogText (show root))
      , ("text_owner",LogText (show (fmap moduleOwner (textModule authorities))))
      , ("job_origin",LogText (show (fmap (fmap (\(owner,origin) -> (moduleOwner owner,origin)))
          (commandJobModule authorities))))
      , ("decision",LogText result) ]

moduleOwner :: Module -> (String,String)
moduleOwner owner = (unitString (moduleUnit owner),moduleNameString (moduleName owner))

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

checkedRoot :: Module -> String -> String -> Type -> Either HostRepresentationRefusal TyCon
checkedRoot owner expectedModule expectedName ty = do
  root <- maybe (Left HostTypeHasNoRoot) Right (rootTyCon ty)
  unless (isExactRoot owner expectedModule expectedName ty)
    (Left (HostRootOwnerMismatch (moduleOwner owner) (moduleOwner <$> nameModule_maybe (tyConName root))))
  Right root

checkedTextConstructor :: Module -> Type -> Either HostRepresentationRefusal DataCon
checkedTextConstructor owner ty = do
  root <- checkedRoot owner "Data.Text.Internal" "Text" ty
  case tyConDataCons root of
    [constructor] -> do
      let name = occNameString (nameOccName (dataConName constructor))
      unless (name == "Text") (Left (HostConstructorName "Text" name))
      Right constructor
    constructors -> Left (HostConstructorCount "Text" (length constructors))

checkedJobConstructors :: Module -> Module -> Type
  -> Either HostRepresentationRefusal (DataCon, DataCon)
checkedJobConstructors jobModule expectedText ty = do
  root <- checkedRoot jobModule "Tidepool.Command.Types" "Job" ty
  case tyConDataCons root of
    [constructor] -> do
      let name = occNameString (nameOccName (dataConName constructor))
          strictness = map (== MarkedStrict) (dataConRepStrictness constructor)
      unless (name == "Job") (Left (HostConstructorName "Job" name))
      unless (strictness == [True]) (Left (HostJobStrictness strictness))
      case dataConOrigArgTys constructor of
        [Scaled _ field] -> do
          text <- checkedTextConstructor expectedText field
          Right (constructor,text)
        fields -> Left (HostJobFieldCount (length fields))
    constructors -> Left (HostConstructorCount "Job" (length constructors))
