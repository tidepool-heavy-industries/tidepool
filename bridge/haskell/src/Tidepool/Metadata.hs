module Tidepool.Metadata
  ( DCMeta(..)
  , collectDataCons
  , dcToMeta
  , mergeMetaPreserving
  , targetBindingHasIO
  , wiredInDataCons
  ) where

import Data.Text (Text)
import qualified Data.Text as T
import Data.Word (Word64)
import qualified Data.Map.Strict as Map
import GHC.Builtin.Names (ioTyConKey)
import GHC.Builtin.Types
  ( charDataCon, consDataCon, doubleDataCon, falseDataCon, floatDataCon
  , intDataCon, nilDataCon, ordEQDataCon, ordGTDataCon, ordLTDataCon
  , trueDataCon, tupleDataCon, unitDataCon, wordDataCon )
import GHC.Core (Bind(..), CoreBind)
import GHC.Core.DataCon
  ( DataCon, dataConFieldLabels, dataConName, dataConOrigArgTys
  , dataConRepArgTys, dataConSrcBangs, dataConTag, dataConTyCon
  , dataConWorkId, isVanillaDataCon )
import GHC.Core.Predicate (isCoVarType)
import GHC.Core.TyCo.Rep (Scaled(..))
import GHC.Core.TyCon (TyCon, isAlgTyCon, tyConDataCons, tyConUnique)
import GHC.Core.Type (splitFunTy_maybe, splitTyConApp_maybe)
import GHC.Data.FastString (unpackFS)
import GHC (HsBang(..), HsSrcBang(..), SrcStrictness(..), SrcUnpackedness(..))
import GHC.Types.FieldLabel (flLabel)
import GHC.Types.Id (idName, idType)
import GHC.Types.Name (nameOccName)
import GHC.Types.Name.Occurrence (occNameString)
import GHC.Types.Unique (getKey)
import GHC.Utils.Outputable (defaultSDocContext, ppr, renderWithContext)
import Language.Haskell.Syntax.Basic (Boxity(..), FieldLabelString(..))

import Tidepool.Identity (qualifiedName, varId)

data DCMeta = DCMeta
  { dcmId          :: !Word64
  , dcmName        :: !Text
  , dcmTag         :: !Int
  , dcmArity       :: !Int
  , dcmBangs       :: ![Text]
  , dcmQualName    :: !Text
  , dcmFieldLabels :: ![Text]
  , dcmTypeName    :: !Text
  , dcmFieldTypes  :: ![Text]
  }

dcToMeta :: DataCon -> DCMeta
dcToMeta dc = DCMeta
  { dcmId = varId (dataConWorkId dc)
  , dcmName = T.pack (occNameString (nameOccName (dataConName dc)))
  , dcmTag = dataConTag dc
  , dcmArity = valueRepArity dc
  , dcmBangs = map mapBang (dataConSrcBangs dc)
  , dcmQualName = qualifiedName (dataConName dc)
  , dcmFieldLabels = map (T.pack . unpackFS . field_label . flLabel) (dataConFieldLabels dc)
  , dcmTypeName = T.pack (renderWithContext defaultSDocContext (ppr (dataConTyCon dc)))
  , dcmFieldTypes = if isVanillaDataCon dc
      then [T.pack (renderWithContext defaultSDocContext (ppr ft)) | Scaled _ ft <- dataConOrigArgTys dc]
      else []
  }

mergeMetaPreserving :: [[DCMeta]] -> [DCMeta]
mergeMetaPreserving sources = Map.elems $ Map.fromList
  [ ((dcmId e, dcmQualName e), e) | e <- reverse (concat sources) ]

collectDataCons :: [TyCon] -> [DCMeta]
collectDataCons tycons =
  [ dcToMeta dc | tc <- tycons, isAlgTyCon tc, dc <- tyConDataCons tc ]

targetBindingHasIO :: [CoreBind] -> String -> Bool
targetBindingHasIO binds name = case filter isTarget (concatMap binders binds) of
  b : _ -> hasIOType (idType b)
  [] -> False
  where
    binders (NonRec b _) = [b]
    binders (Rec pairs) = map fst pairs
    isTarget b = occNameString (nameOccName (idName b)) == name
    hasIOType ty = case splitTyConApp_maybe ty of
      Just (tc, _) | getKey (tyConUnique tc) == getKey ioTyConKey -> True
      _ -> maybe False (\(_, _, _, result) -> hasIOType result) (splitFunTy_maybe ty)

wiredInDataCons :: [DCMeta]
wiredInDataCons = map dcToMeta $
  [ consDataCon, nilDataCon, trueDataCon, falseDataCon, charDataCon, unitDataCon
  , intDataCon, wordDataCon, doubleDataCon, floatDataCon
  ] ++ map (tupleDataCon Boxed) [2 .. 5]
    ++ [ordLTDataCon, ordEQDataCon, ordGTDataCon]

valueRepArity :: DataCon -> Int
valueRepArity dc = length
  [ () | Scaled _ ty <- dataConRepArgTys dc, not (isCoVarType ty) ]

mapBang :: HsSrcBang -> Text
mapBang (HsSrcBang _ (HsBang srcUnpack srcBang)) = case (srcUnpack, srcBang) of
  (_, SrcStrict) -> "SrcBang"
  (SrcUnpack, _) -> "SrcUnpack"
  _ -> "NoSrcBang"
