module Tidepool.PreparedFormatting
  ( FormattingAuthority(..), FormattingIntrinsic(..), FormattingSpec(..)
  , FormattingError(..), classifyFormatting
  ) where

import Data.Text (Text)
import Data.Text qualified as Text
import GHC.Builtin.Types (doubleTy, intTy)
import GHC.Core.DataCon (DataCon)
import GHC.Core.TyCo.Compare (eqType)
import GHC.Core.TyCo.Rep (Scaled(..))
import GHC.Core.Type (splitFunTys, splitTyConApp_maybe)
import GHC.Core.TyCon (tyConDataCons, tyConName)
import GHC.Types.Id (Id, idType, isDeadEndId)
import GHC.Types.Name (isExternalName, nameModule_maybe, nameOccName)
import GHC.Types.Name.Occurrence (occNameString)
import GHC.Types.Var (varName)
import GHC.Unit.Module (Module, moduleName, moduleNameString)
import GHC.Utils.Outputable (ppr, showSDocUnsafe)

-- | W5_FORMATTING: the compiler resolves the shipped module once. Carry its
-- complete identity, including unit, into projection; never infer authority
-- from an occurrence string seen while recovering arbitrary package bodies.
newtype FormattingAuthority = FormattingAuthority Module deriving stock (Eq)
instance Show FormattingAuthority where
  show (FormattingAuthority owner) = showSDocUnsafe (ppr owner)

data FormattingIntrinsic = RenderDouble | RenderDoublePrec deriving stock (Eq, Show)
data FormattingSpec = FormattingSpec
  { formattingKind :: FormattingIntrinsic
  , formattingBinder :: Id
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
                     Right (Just (FormattingSpec kind binder dataConstructor))
               _ -> Left (InvalidFormattingType label)
