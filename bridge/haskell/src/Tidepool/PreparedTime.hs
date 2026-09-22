{-# LANGUAGE TemplateHaskell #-}

module Tidepool.PreparedTime
  ( TimeAuthority(..), TimeSpec(..), TimeError(..)
  , classifyTime, resolveTimeAuthority
  ) where

import Control.Exception (IOException, try)
import Data.ByteString (ByteString)
import Data.ByteString qualified as BS
import Data.List (find)
import Data.Text (Text)
import Data.Text qualified as Text
import GHC.Core.DataCon (DataCon, dataConName)
import GHC.Core.TyCo.Compare (eqType)
import GHC.Core.TyCo.Rep (Scaled(..))
import GHC.Core.Type (splitFunTys, splitTyConApp_maybe)
import GHC.Core.TyCon (TyCon, tyConDataCons, tyConName)
import GHC.Driver.Env (HscEnv)
import GHC.Driver.Env.Types (hsc_unit_env)
import GHC.Types.Id (Id, idType, isDeadEndId)
import GHC.Types.Name (isExternalName, nameModule_maybe, nameOccName)
import GHC.Types.Name.Occurrence (occNameString)
import GHC.Data.FastString (fsLit)
import GHC.Types.PkgQual (PkgQual(NoPkgQual, OtherPkg))
import GHC.Types.Var (varName)
import GHC.Unit.Finder (FindResult(..), findImportedModule)
import GHC.Unit.Module (Module, mkModuleName, moduleName, moduleNameString, moduleUnit)
import GHC.Unit.Module.Location (ml_hs_file)
import GHC.Unit.Env (ue_units)
import GHC.Unit.Info (PackageName(..))
import GHC.Unit.State (lookupPackageName)
import GHC.Unit.Types (Unit)
import GHC.Utils.Outputable (ppr, showSDocUnsafe)
import Language.Haskell.TH.Syntax (addDependentFile, lift, loc_filename, location, runIO)
import System.FilePath (takeDirectory, (</>))

shippedTimeSource :: ByteString
shippedTimeSource = BS.pack $(do
  here <- loc_filename <$> location
  let source = takeDirectory here </> ".." </> ".." </> "lib" </> "Tidepool" </> "Data" </> "Time.hs"
  addDependentFile source
  lift . BS.unpack =<< runIO (BS.readFile source))

data TimeAuthority = TimeAuthority Module Unit deriving stock (Eq)
instance Show TimeAuthority where
  show (TimeAuthority owner _) = showSDocUnsafe (ppr owner)

resolveTimeAuthority :: HscEnv -> IO (Maybe TimeAuthority)
resolveTimeAuthority env = case lookupPackageName
    (ue_units (hsc_unit_env env)) (PackageName (fsLit "text")) of
  Nothing -> pure Nothing
  Just selectedTextUnit -> do
    textFound <- findImportedModule env (mkModuleName "Data.Text.Internal")
      (OtherPkg selectedTextUnit)
    found <- findImportedModule env (mkModuleName "Tidepool.Data.Time") NoPkgQual
    -- Tidepool.Data.Time is a home library module. Its source is authenticated
    -- by bytes, while its explicit `"text"` import is pinned by the resolved
    -- package unit carried below.
    case (textFound, found) of
      (Found _ textOwner, Found modLocation owner) | Just source <- ml_hs_file modLocation -> do
        actual <- try (BS.readFile source) :: IO (Either IOException ByteString)
        pure $ case actual of
          Right bytes | bytes == shippedTimeSource -> Just (TimeAuthority owner (moduleUnit textOwner))
          _ -> Nothing
      _ -> pure Nothing

data TimeSpec = TimeSpec
  { timeTextConstructor :: DataCon
  , timeLeftConstructor :: DataCon
  , timeRightConstructor :: DataCon
  }

data TimeError
  = InvalidTimeType Text
  | BottomingTimeDefinition Text
  deriving stock (Eq, Show)

classifyTime :: TimeAuthority -> Id -> Either TimeError (Maybe TimeSpec)
classifyTime (TimeAuthority owner textUnit) binder
  | not (isExternalName name) || nameModule_maybe name /= Just owner = Right Nothing
  | occNameString (nameOccName name) /= "parseISO8601" = Right Nothing
  | isDeadEndId binder = Left (BottomingTimeDefinition label)
  | otherwise = inspect
  where
    name = varName binder
    label = Text.pack (showSDocUnsafe (ppr name))
    invalid = Left (InvalidTimeType (label <> ": " <> Text.pack (showSDocUnsafe (ppr (idType binder)))))
    inspect = case splitFunTys (idType binder) of
      ([Scaled _ argument], result)
        | Just (textTyCon, []) <- splitTyConApp_maybe argument
        , exactTyCon (Just textUnit) "Data.Text.Internal" "Text" textTyCon
        , [textConstructor] <- tyConDataCons textTyCon
        , Just (eitherTyCon, [failure, success]) <- splitTyConApp_maybe result
        , exactTyCon Nothing "GHC.Internal.Data.Either" "Either" eitherTyCon
        , eqType failure argument
        , Just (timeTyCon, []) <- splitTyConApp_maybe success
        , nameModule_maybe (tyConName timeTyCon) == Just owner
        , occNameString (nameOccName (tyConName timeTyCon)) == "UTCTime"
        , Just left <- constructorNamed "Left" (tyConDataCons eitherTyCon)
        , Just right <- constructorNamed "Right" (tyConDataCons eitherTyCon) ->
            Right (Just (TimeSpec textConstructor left right))
      _ -> invalid

exactTyCon :: Maybe Unit -> String -> String -> TyCon -> Bool
exactTyCon expectedUnit definingModule occurrence tyCon =
  occNameString (nameOccName (tyConName tyCon)) == occurrence
    && case nameModule_maybe (tyConName tyCon) of
      Just owner -> moduleNameString (moduleName owner) == definingModule
        && maybe True (== moduleUnit owner) expectedUnit
      Nothing -> False

constructorNamed :: String -> [DataCon] -> Maybe DataCon
constructorNamed occurrence = find
  ((== occurrence) . occNameString . nameOccName . dataConName)
