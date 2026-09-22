{-# LANGUAGE TemplateHaskell #-}

module Tidepool.PreparedJson
  ( JsonAuthority, JsonSpec(..), JsonError(..)
  , classifyJson, resolveJsonAuthority, jsonAuthorityModule
  ) where

import Control.Exception (IOException, try)
import Data.ByteString (ByteString)
import Data.ByteString qualified as BS
import Data.List (find)
import Data.Text (Text)
import Data.Text qualified as Text
import GHC.Core.DataCon (DataCon, dataConName, dataConOrigArgTys)
import GHC.Core.TyCo.Rep (Scaled(..))
import GHC.Core.TyCon (tyConDataCons)
import GHC.Core.Type (splitFunTys, splitTyConApp_maybe)
import GHC.Core.TyCon qualified
import GHC.Driver.Env (HscEnv)
import GHC.Driver.Env.Types (hsc_unit_env)
import GHC.Types.Id (Id, idType, isDeadEndId)
import GHC.Types.Name (isExternalName, nameModule_maybe, nameOccName)
import GHC.Types.Name.Occurrence (occNameString)
import GHC.Types.PkgQual (PkgQual(NoPkgQual, OtherPkg))
import GHC.Types.Var (varName)
import GHC.Unit.Finder (FindResult(..), findImportedModule)
import GHC.Unit.Module (Module, mkModuleName, moduleName, moduleNameString, moduleUnit)
import GHC.Unit.Module.Location (ml_hs_file)
import GHC.Unit.Env (ue_units)
import GHC.Unit.Info (PackageName(..))
import GHC.Unit.State (lookupPackageName)
import GHC.Unit.Types (Unit)
import GHC.Data.FastString (fsLit)
import GHC.Utils.Outputable (ppr, showSDocUnsafe)
import Tidepool.ExecutionSchema (JsonLayout(..))
import Language.Haskell.TH.Syntax (addDependentFile, lift, loc_filename, location, runIO)
import System.FilePath (takeDirectory, (</>))

shippedValueSource :: ByteString
shippedValueSource = BS.pack $(do
  here <- loc_filename <$> location
  let source = takeDirectory here </> ".." </> ".." </> "lib" </> "Tidepool" </> "Aeson" </> "Value.hs"
  addDependentFile source
  lift . BS.unpack =<< runIO (BS.readFile source))

data JsonAuthority = JsonAuthority Module Unit deriving stock (Eq)
instance Show JsonAuthority where
  show (JsonAuthority owner _) = showSDocUnsafe (ppr owner)

-- | The compiler-resolved, byte-authenticated module that owns the shipped
-- JSON surface. Other compiler products can carry this identity as authority
-- without reconstructing it from a rendered module name.
jsonAuthorityModule :: JsonAuthority -> Module
jsonAuthorityModule (JsonAuthority owner _) = owner

resolveJsonAuthority :: HscEnv -> IO (Maybe JsonAuthority)
resolveJsonAuthority env = case lookupPackageName
    (ue_units (hsc_unit_env env)) (PackageName (fsLit "text")) of
  Nothing -> pure Nothing
  Just textUnit -> do
    textFound <- findImportedModule env (mkModuleName "Data.Text.Internal") (OtherPkg textUnit)
    found <- findImportedModule env (mkModuleName "Tidepool.Aeson.Value") NoPkgQual
    case (textFound, found) of
      (Found _ textOwner, Found moduleLocation owner) | Just source <- ml_hs_file moduleLocation -> do
        actual <- try (BS.readFile source) :: IO (Either IOException ByteString)
        pure $ case actual of
          Right bytes | bytes == shippedValueSource ->
            Just (JsonAuthority owner (moduleUnit textOwner))
          _ -> Nothing
      _ -> pure Nothing

data JsonSpec = DecodeJson DataCon (JsonLayout DataCon) DataCon DataCon
  | EncodeJson DataCon (JsonLayout DataCon)
  deriving stock (Eq)
data JsonError = InvalidJsonType Text | BottomingJsonDefinition Text
  deriving stock (Eq, Show)

classifyJson :: JsonAuthority -> Id -> Either JsonError (Maybe JsonSpec)
classifyJson (JsonAuthority owner textUnit) binder
  | not (isExternalName name) || nameModule_maybe name /= Just owner = Right Nothing
  | occurrence /= "eitherDecodeValue" && occurrence /= "encodeValue" = Right Nothing
  | isDeadEndId binder = Left (BottomingJsonDefinition label)
  | otherwise = case (occurrence, splitFunTys (idType binder)) of
      ("eitherDecodeValue", ([Scaled _ argument], result))
        | Just textConstructor <- textConstructorOf argument
        , Just (eitherTyCon, [failure, success]) <- splitTyConApp_maybe result
        , exact Nothing "GHC.Internal.Data.Either" "Either" eitherTyCon
        , isText failure, isValue success
        , Just layout <- jsonLayoutForValue success
        , Just left <- named "Left" (tyConDataCons eitherTyCon)
        , Just right <- named "Right" (tyConDataCons eitherTyCon) ->
            Right (Just (DecodeJson textConstructor layout left right))
      ("encodeValue", ([Scaled _ argument], result))
        | isValue argument
        , Just textConstructor <- textConstructorOf result
        , Just layout <- jsonLayoutForValue argument ->
            Right (Just (EncodeJson textConstructor layout))
      _ -> Left (InvalidJsonType (label <> ": " <> Text.pack (showSDocUnsafe (ppr (idType binder)))))
 where
  name = varName binder
  occurrence = occNameString (nameOccName name)
  label = Text.pack (showSDocUnsafe (ppr name))
  isText ty = case splitTyConApp_maybe ty of
    Just (tyCon, []) -> exact (Just textUnit) "Data.Text.Internal" "Text" tyCon
    _ -> False
  textConstructorOf ty = case splitTyConApp_maybe ty of
    Just (tyCon, []) | exact (Just textUnit) "Data.Text.Internal" "Text" tyCon ->
      case tyConDataCons tyCon of
        [constructor] -> Just constructor
        _ -> Nothing
    _ -> Nothing
  isValue ty = case splitTyConApp_maybe ty of
    Just (tyCon, []) -> exact (Just (moduleUnit owner)) "Tidepool.Aeson.Value" "Value" tyCon
    _ -> False
  valueTyConOf valueType = do
    (valueTyCon, []) <- splitTyConApp_maybe valueType
    if exact (Just (moduleUnit owner)) "Tidepool.Aeson.Value" "Value" valueTyCon
      then Just valueTyCon else Nothing
  jsonLayoutForValue valueType = do
    valueTyCon <- valueTyConOf valueType
    let valueCons = tyConDataCons valueTyCon
    objectCon <- named "Object" valueCons
    arrayCon <- named "Array" valueCons
    stringCon <- named "String" valueCons
    numberCon <- named "Number" valueCons
    boolCon <- named "Bool" valueCons
    mapTyCon <- soleFieldTyCon objectCon
    listTyCon <- soleFieldTyCon arrayCon
    textTyCon <- soleFieldTyCon stringCon
    scientificTyCon <- soleFieldTyCon numberCon
    boolTyCon <- soleFieldTyCon boolCon
    let scientificCons = tyConDataCons scientificTyCon
    scientificCon <- named "Scientific" scientificCons
    [coefficientType, exponentType] <- pure (map scaledThing (dataConOrigArgTys scientificCon))
    (integerTyCon, []) <- splitTyConApp_maybe coefficientType
    (intTyCon, []) <- splitTyConApp_maybe exponentType
    object <- named "Object" valueCons
    array <- named "Array" valueCons
    string <- named "String" valueCons
    number <- named "Number" valueCons
    bool <- named "Bool" valueCons
    null_ <- named "Null" valueCons
    bin <- named "Bin" (tyConDataCons mapTyCon)
    tip <- named "Tip" (tyConDataCons mapTyCon)
    true_ <- named "True" (tyConDataCons boolTyCon)
    false_ <- named "False" (tyConDataCons boolTyCon)
    cons <- named ":" (tyConDataCons listTyCon)
    nil <- named "[]" (tyConDataCons listTyCon)
    is <- named "IS" (tyConDataCons integerTyCon)
    ip <- named "IP" (tyConDataCons integerTyCon)
    in_ <- named "IN" (tyConDataCons integerTyCon)
    text <- case tyConDataCons textTyCon of [constructor] -> Just constructor; _ -> Nothing
    int <- named "I#" (tyConDataCons intTyCon)
    pure JsonLayout
      { jsonObject = object, jsonArray = array, jsonString = string, jsonNumber = number
      , jsonBool = bool, jsonNull = null_, jsonMapBin = bin, jsonMapTip = tip
      , jsonTrue = true_, jsonFalse = false_, jsonCons = cons, jsonNil = nil
      , jsonScientific = scientificCon, jsonIntegerSmall = is, jsonIntegerPositive = ip
      , jsonIntegerNegative = in_, jsonText = text, jsonInt = int }
  named wanted = find ((== wanted) . occNameString . nameOccName . dataConName)
  soleFieldTyCon constructor = case dataConOrigArgTys constructor of
    [Scaled _ field] -> fst <$> splitTyConApp_maybe field
    _ -> Nothing
  scaledThing (Scaled _ ty) = ty

exact :: Maybe Unit -> String -> String -> GHC.Core.TyCon.TyCon -> Bool
exact expectedUnit definingModule occurrence tyCon =
  occNameString (nameOccName (GHC.Core.TyCon.tyConName tyCon)) == occurrence
    && case nameModule_maybe (GHC.Core.TyCon.tyConName tyCon) of
      Just owner -> moduleNameString (moduleName owner) == definingModule
        && maybe True (== moduleUnit owner) expectedUnit
      Nothing -> False
