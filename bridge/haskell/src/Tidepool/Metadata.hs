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
import GHC.Core.TyCo.Tidy (tidyOpenType)
import GHC.Core.TyCon (TyCon, isAlgTyCon, tyConDataCons, tyConUnique)
import GHC.Core.Type (Type, splitFunTy_maybe, splitTyConApp_maybe)
import GHC.Data.FastString (unpackFS)
import GHC (HsBang(..), HsSrcBang(..), SrcStrictness(..), SrcUnpackedness(..))
import GHC.Types.FieldLabel (flLabel)
import GHC.Types.Id (idName, idType)
import GHC.Types.Name (nameOccName)
import GHC.Types.Name.Occurrence (occNameString)
import GHC.Types.Unique (getKey)
import GHC.Types.Var.Env (emptyTidyEnv)
import GHC.Utils.Outputable
  (SDoc, defaultSDocContext, ppr, renderWithContext, sdocSuppressUniques)
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
  , dcmTypeName = renderMetaType (ppr (dataConTyCon dc))
  , dcmFieldTypes = if isVanillaDataCon dc
      then [renderFieldType ft | Scaled _ ft <- dataConOrigArgTys dc]
      else []
  }

-- | Render an 'SDoc' the way every @meta.cbor@ text field is rendered — the
-- sole point that text reaches 'DCMeta', so this context covers every field.
--
-- Field types (and, defensively, the type name) can carry compiler-generated
-- (kind-inference, or interface-reloaded) type/kind variables. Those
-- variables have GHC's \"System\" name sort, and GHC's default printer
-- (@'GHC.Types.Name.pprSystem'@) shows a System name as its 'OccName' plus
-- @_@ plus its raw 'Unique' UNCONDITIONALLY — unlike an ordinary name, this
-- is not gated on debug/dump style, so 'tidyOpenType' alone cannot suppress
-- it (tidying only reassigns 'OccName's to avoid a same-name clash; it does
-- not change a name's sort, and the unique suffix is added at print time
-- regardless of the tidied 'OccName'). Confirmed empirically: a synthetic
-- System-named tyvar renders with its unique both before and after
-- 'tidyOpenType', and stops only once 'sdocSuppressUniques' is set.
--
-- A long-running worker's 'Unique' supply has advanced by however many prior
-- compiles it has served, so an unsuppressed unique leaks worker history
-- into 'meta.cbor', breaking byte-determinism (and the content-addressed
-- compile cache) across otherwise-identical compiles. 'sdocSuppressUniques'
-- removes that leak; 'tidyOpenType' (applied at the one call site that needs
-- it, 'renderFieldType') still does its own job of giving two distinct free
-- variables distinct display names so suppressing uniques never makes
-- genuinely different variables print identically.
renderMetaType :: SDoc -> Text
renderMetaType = T.pack . renderWithContext (defaultSDocContext { sdocSuppressUniques = True })

-- | Render a field type for 'dcmFieldTypes'. 'tidyOpenType' assigns a fresh,
-- structurally-determined display name (a, b, c, ...) to every free
-- type/kind variable, starting from 'emptyTidyEnv', so distinct variables
-- never collide once 'renderMetaType' hides their uniques; see
-- 'renderMetaType' for why that hiding is the part that actually closes the
-- worker-history leak.
renderFieldType :: Type -> Text
renderFieldType ft = renderMetaType (ppr (tidyOpenType emptyTidyEnv ft))

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
