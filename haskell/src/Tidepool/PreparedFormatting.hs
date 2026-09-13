{-# LANGUAGE TemplateHaskell #-}

module Tidepool.PreparedFormatting
  ( FormattingAuthority(..), FormattingIntrinsic(..), FormattingSpec(..)
  , FormattingError(..), classifyFormatting, resolveFormattingAuthority
  ) where

import Control.Exception (IOException, try)
import Data.ByteString (ByteString)
import Data.ByteString qualified as BS
import Data.Text (Text)
import Data.Text qualified as Text
import GHC.Builtin.Types (doubleTy, intTy)
import GHC.Core.DataCon (DataCon)
import GHC.Core.TyCo.Compare (eqType)
import GHC.Core.TyCo.Rep (Scaled(..))
import GHC.Core.Type (splitFunTys, splitTyConApp_maybe)
import GHC.Core.TyCon (tyConDataCons, tyConName)
import GHC.Driver.Env (HscEnv)
import GHC.Types.Id (Id, idType, isDeadEndId)
import GHC.Types.Name (isExternalName, nameModule_maybe, nameOccName)
import GHC.Types.Name.Occurrence (occNameString)
import GHC.Types.Var (varName)
import GHC.Unit.Module (Module, mkModuleName, moduleName, moduleNameString)
import GHC.Unit.Finder (FindResult(..), findImportedModule)
import GHC.Unit.Module.Location (ml_hs_file)
import GHC.Types.PkgQual (PkgQual(NoPkgQual))
import GHC.Utils.Outputable (ppr, showSDocUnsafe)
import Language.Haskell.TH.Syntax (addDependentFile, lift, loc_filename, location, runIO)
import System.FilePath (takeDirectory, (</>))

-- | Pin the exact shipped implementation at extractor build time. The source
-- is an explicit Cabal dependency, so rebuilding the extractor refreshes this
-- authority when the shipped module changes.
shippedDoubleSource :: ByteString
shippedDoubleSource = BS.pack $(do
  here <- loc_filename <$> location
  let source = takeDirectory here </> ".." </> ".." </> "lib" </> "Tidepool" </> "Double.hs"
  addDependentFile source
  lift . BS.unpack =<< runIO (BS.readFile source))

-- | The compiler resolves the shipped module once. Carry its
-- complete identity, including unit, into projection; never infer authority
-- from an occurrence string seen while recovering arbitrary package bodies.
newtype FormattingAuthority = FormattingAuthority Module deriving stock (Eq)
instance Show FormattingAuthority where
  show (FormattingAuthority owner) = showSDocUnsafe (ppr owner)

-- | Grant authority only to the actual loaded source when it matches the
-- shipped implementation byte for byte. Module identity alone cannot exclude
-- a same-named source in the home unit. Source-less package interfaces remain
-- ordinary recovery, and an unreadable source never gains authority.
resolveFormattingAuthority :: HscEnv -> IO (Maybe FormattingAuthority)
resolveFormattingAuthority env = do
  found <- findImportedModule env (mkModuleName "Tidepool.Double") NoPkgQual
  case found of
    Found modLocation owner | Just source <- ml_hs_file modLocation -> do
      actual <- try (BS.readFile source) :: IO (Either IOException ByteString)
      pure $ case actual of
        Right bytes | bytes == shippedDoubleSource -> Just (FormattingAuthority owner)
        _ -> Nothing
    _ -> pure Nothing

data FormattingIntrinsic = RenderDouble | RenderDoublePrec deriving stock (Eq, Show)
data FormattingSpec = FormattingSpec
  { formattingKind :: FormattingIntrinsic
  , formattingTextConstructor :: DataCon
  }
data FormattingError
  = InvalidFormattingType Text
  | BottomingFormattingDefinition Text
  deriving stock (Eq, Show)

-- | The returning source implementation must already have informed GHC's
-- optimization. A late projection override cannot repair bottoming callers.
-- Constructor layout is subsequently checked by projection's owning interner.
classifyFormatting :: FormattingAuthority -> Id -> Either FormattingError (Maybe FormattingSpec)
classifyFormatting (FormattingAuthority owner) binder
  | not (isExternalName name) || nameModule_maybe name /= Just owner = Right Nothing
  | otherwise = case occNameString (nameOccName name) of
      "renderDouble" -> inspect RenderDouble [doubleTy]
      "renderDoublePrec" -> inspect RenderDoublePrec [intTy, doubleTy]
      _ -> Right Nothing
  where
    name = varName binder
    label = Text.pack (showSDocUnsafe (ppr name))
    inspect kind expected
      | isDeadEndId binder = Left (BottomingFormattingDefinition label)
      | otherwise =
          let (arguments, result) = splitFunTys (idType binder)
              actual = [ty | Scaled _ ty <- arguments]
          in if length actual /= length expected || not (and (zipWith eqType actual expected))
             then Left (InvalidFormattingType label)
             else case splitTyConApp_maybe result of
               Just (constructor, [])
                 | occNameString (nameOccName (tyConName constructor)) == "Text"
                 , Just textOwner <- nameModule_maybe (tyConName constructor)
                 , moduleNameString (moduleName textOwner) == "Data.Text.Internal"
                 , [dataConstructor] <- tyConDataCons constructor ->
                     Right (Just (FormattingSpec kind dataConstructor))
               _ -> Left (InvalidFormattingType label)
