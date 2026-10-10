module Tidepool.CborEncode
  ( encodeMetadata
  , encodeTurnOut
  , encodeCellOut
  , encodeBoundBinder
  ) where

import Codec.CBOR.Encoding
import Codec.CBOR.Write (toStrictByteString)
import Data.ByteString (ByteString)
import qualified Data.ByteString as BS
import Data.Text (Text)
import qualified Data.Text as T
import Tidepool.Metadata (DCMeta(..))
import Tidepool.ExecutionEncode (encodeSymbol)
import Tidepool.Binders
  ( TurnOut(..), BoundBinder(..), ExportItem(..), ValueTier(..), HostBindingAuthority(..)
  , CellSourcePlan(..), CellAnalysisItem(..), CellAnalysisSourceItem(..)
  , CellSourceSpan(..), CheckedBinderPin(..), SourcePrologue(..)
  , CellExpressionPlan
  , LocatedPragma(..), LocatedImport(..), PragmaKind(..), DeclarationSource(..)
  , StmtBinders(..), turnKindWireName )
import Tidepool.CheckedCell (encodeCheckedTypeWitness, encodeRequestTypeSignatures, encodeCellExpressionPlan)
import Tidepool.EffectSchema (NominalHead(..), SiteType(..), YieldSite(..))

-- | 8-byte version header: magic 'TPLR' + version 5.0.
--
-- Must be kept byte-identical to the Rust reader's own
-- @VERSION_MAJOR@/@VERSION_MINOR@ (tidepool/repr/src/serial/mod.rs) — there
-- is no shared formatter across the language boundary, so a version bump
-- needs a matching change on both sides.
--
-- 5.0 requires the complete compiler-issued constructor symbol identity.
tplrHeader :: ByteString
tplrHeader = BS.pack [0x54, 0x50, 0x4C, 0x52, 0x00, 0x05, 0x00, 0x00]

-- | Encode the DataCon table + a warnings map: @[entries_array, warnings_map]@.
-- The warnings map always carries @has_io@; when a captured type is present
-- (the eval's @__user@ binding type — see GhcPipeline.capturedUserType) it also
-- carries @captured_type@. The Rust reader (serial/read.rs parse_warnings)
-- tolerates either map shape, so omitting the key on Nothing is backward-safe.
-- The @[Text]@ is the GHC diagnostic warnings for the target module
-- (see GhcPipeline.prWarnings) — an empty list omits the @warnings@ key
-- entirely, keeping a clean compile's meta.cbor byte-identical to before.
encodeMetadata :: [DCMeta] -> Bool -> Maybe Text -> [Text] -> ByteString
encodeMetadata entries hasIO mCapturedType warnings = tplrHeader <> toStrictByteString (
  encodeListLen 2
  <> (encodeListLen (fromIntegral (length entries)) <> foldMap encodeMetaEntry entries)
  <> warningsMap)
  where
    -- Optional keys are simply omitted when absent; the Rust reader accepts
    -- omitted keys but rejects unknown ones (metadata_strictness.rs), so new
    -- keys require a version bump on both sides.
    warningsMap =
      encodeMapLen (1 + maybe 0 (const 1) mCapturedType
                      + (if null warnings then 0 else 1))
      <> encodeString "has_io" <> encodeBool hasIO
      <> maybe mempty (\ty -> encodeString "captured_type" <> encodeString ty) mCapturedType
      <> (if null warnings then mempty else
            encodeString "warnings"
            <> encodeListLen (fromIntegral (length warnings))
            <> foldMap encodeString warnings)

encodeMetaEntry :: DCMeta -> Encoding
encodeMetaEntry DCMeta{dcmIdentity, dcmId, dcmName, dcmTag, dcmArity, dcmBangs, dcmQualName, dcmFieldLabels, dcmTypeName, dcmFieldTypes} =
  let
    tagWord :: Word
    tagWord =
      if dcmTag < 0
        then error "encodeMetaEntry: negative constructor tag"
        else fromIntegral dcmTag
  in
  -- The Rust reader requires exactly ten elements. Positional constructors
  -- carry an empty labels array. The 8th
  -- element is the rendered name of the constructor's parent TyCon (e.g.
  -- "Verdict"), always present — every DataCon has a parent type. The 9th
  -- element is the constructor's field types, rendered in declaration order
  -- (same @ppr@ convention as the 8th element and asks.json), always present
  -- (empty array for a nullary constructor).
  encodeListLen 10
  <> encodeWord64 dcmId
  <> encodeString dcmName
  <> encodeWord tagWord
  <> encodeInt dcmArity
  <> encodeListLen (fromIntegral (length dcmBangs))
  <> foldMap encodeString dcmBangs
  <> encodeString dcmQualName
  <> encodeListLen (fromIntegral (length dcmFieldLabels))
  <> foldMap encodeString dcmFieldLabels
  <> encodeString dcmTypeName
  <> encodeListLen (fromIntegral (length dcmFieldTypes))
  <> foldMap encodeString dcmFieldTypes
  <> encodeSymbol dcmIdentity

--------------------------------------------------------------------------------
-- Turn-mode rich result (--turn) — independent of the constructor-metadata
-- wire above: no TPLR header, no version coupling. A 2-element list, a string
-- tag ("Decl"/"Bind"/"Expr") plus the variant's payload, using the same
-- tagged-list convention as the other execution wires. This is the ONLY
-- serialization of a 'TurnOut': the
-- parallel JSON rendering (@--json-output@ / @renderTurnOutJson@) was a second
-- hand-maintained serializer with no reader and was deleted.
--------------------------------------------------------------------------------

encodeTurnOut :: TurnOut -> ByteString
encodeTurnOut turnOut = toStrictByteString $ case turnOut of
  TDecl bs items source ->
    encodeListLen 2 <> encodeString "Decl"
    <> (encodeListLen 3 <> encodeTextList bs <> encodeExportItems items
        <> encodeDeclarationSource source)
  TBind bs var bbs aks wrapped ->
    encodeListLen 2 <> encodeString "Bind"
    <> (encodeListLen 5
        <> encodeTextList bs
        <> encodeInt var
        <> encodeBoundBinders bbs
        <> encodeAsks aks
        <> encodeString wrapped)
  TExpr var aks wrapped ->
    encodeListLen 2 <> encodeString "Expr"
    <> (encodeListLen 3
        <> encodeInt var
        <> encodeAsks aks
        <> encodeString wrapped)

-- | Matched whole-cell observations, version 2. The payload contains items,
-- diagnostic pins, checked source, the authored prologue and expression plans.
-- Exact native signatures live in the separately authenticated checked receipt.
encodeCellOut
  :: CellSourcePlan
  -> [CheckedBinderPin]
  -> [CellExpressionPlan]
  -> String
  -> ByteString
encodeCellOut plan pins expressions checkedSource = toStrictByteString $
  let items = cellPlanItems plan in
  encodeListLen 3 <> encodeString "TPCELLOBSERVATIONS" <> encodeWord 3
  <> encodeListLen 5
  <> encodeListLen (fromIntegral (length items))
  <> foldMap encodeCellItem items
  <> encodeListLen (fromIntegral (length pins))
  <> foldMap encodeCheckedBinderPin pins
  <> encodeString (T.pack checkedSource)
  <> encodeSourcePrologue (cellPlanPrologue plan)
  <> encodeListLen (fromIntegral (length expressions))
  <> foldMap encodeCellExpressionPlan expressions

encodeDeclarationSource :: DeclarationSource -> Encoding
encodeDeclarationSource (DeclarationSource prologue body) =
  encodeListLen 2 <> encodeSourcePrologue prologue <> encodeString (T.pack body)

encodeSourcePrologue :: SourcePrologue -> Encoding
encodeSourcePrologue SourcePrologue { prologuePragmas = pragmas, prologueImports = imports } =
  encodeListLen 2
  <> encodeListLen (fromIntegral (length pragmas))
  <> foldMap encodeLocatedPragma pragmas
  <> encodeListLen (fromIntegral (length imports))
  <> foldMap encodeLocatedImport imports

encodeLocatedPragma :: LocatedPragma -> Encoding
encodeLocatedPragma (LocatedPragma kind sourceSpan source) =
  encodeListLen 3
  <> encodeString (case kind of
       LanguagePragma -> "language"
       OptionsGhcPragma -> "options_ghc")
  <> encodeCellSpan sourceSpan
  <> encodeString (T.pack source)

encodeLocatedImport :: LocatedImport -> Encoding
-- Observations project presentation only. Import demand is retained by the
-- original parser plan during compilation, never decoded from this projection.
encodeLocatedImport imported =
  encodeListLen 2 <> encodeCellSpan (locatedImportSpan imported)
    <> encodeString (T.pack (locatedImportSource imported))

encodeCellSpan :: CellSourceSpan -> Encoding
encodeCellSpan (CellSourceSpan startLine startColumn endLine endColumn) =
  encodeListLen 4
  <> encodeInt startLine
  <> encodeInt startColumn
  <> encodeInt endLine
  <> encodeInt endColumn

encodeCellItem :: CellAnalysisItem -> Encoding
encodeCellItem CellAnalysisItem
  { cellAnalysisSpan = CellSourceSpan startLine startColumn endLine endColumn
  , cellAnalysisSource = source
  , cellAnalysisVerdict = StmtBinders kind binders items
  , cellAnalysisSourceItems = sourceItems
  , cellAnalysisPrologueOnly = prologueOnly
  } =
  encodeListLen 6
  <> encodeListLen 4
  <> encodeInt startLine
  <> encodeInt startColumn
  <> encodeInt endLine
  <> encodeInt endColumn
  <> encodeString (T.pack (turnKindWireName kind))
  <> encodeString (T.pack source)
  <> (encodeListLen 2
      <> encodeStringList binders
      <> encodeExportItems items)
  <> encodeListLen (fromIntegral (length sourceItems))
  <> foldMap encodeCellSourceItem sourceItems
  <> encodeBool prologueOnly

encodeCellSourceItem :: CellAnalysisSourceItem -> Encoding
encodeCellSourceItem CellAnalysisSourceItem
  { cellAnalysisSourceOrdinal = ordinal
  , cellAnalysisSourceSpan = CellSourceSpan startLine startColumn endLine endColumn
  , cellAnalysisSourceKind = kind
  } =
  encodeListLen 3
  <> encodeInt ordinal
  <> (encodeListLen 4
      <> encodeInt startLine
      <> encodeInt startColumn
      <> encodeInt endLine
      <> encodeInt endColumn)
  <> encodeString (T.pack (turnKindWireName kind))

encodeCheckedBinderPin :: CheckedBinderPin -> Encoding
encodeCheckedBinderPin CheckedBinderPin
  { checkedPinKey = key
  , checkedPinType = ty
  , checkedPinHeads = heads
  } =
  encodeListLen 3
  <> encodeString (T.pack key)
  <> encodeString (T.pack ty)
  <> encodeHeads heads

encodeTextList :: [Text] -> Encoding
encodeTextList xs = encodeListLen (fromIntegral (length xs)) <> foldMap encodeString xs

encodeStringList :: [String] -> Encoding
encodeStringList xs = encodeListLen (fromIntegral (length xs)) <> foldMap (encodeString . T.pack) xs

encodeExportItems :: [ExportItem] -> Encoding
encodeExportItems items = encodeListLen (fromIntegral (length items)) <> foldMap encodeExportItem items

encodeExportItem :: ExportItem -> Encoding
encodeExportItem item = case item of
  EValue n     -> encodeListLen 2 <> encodeString "EValue" <> encodeString (T.pack n)
  EType n cons -> encodeListLen 3 <> encodeString "EType" <> encodeString (T.pack n) <> encodeStringList cons
  EClass n ms  -> encodeListLen 3 <> encodeString "EClass" <> encodeString (T.pack n) <> encodeStringList ms

encodeBoundBinders :: [BoundBinder] -> Encoding
encodeBoundBinders bs = encodeListLen (fromIntegral (length bs)) <> foldMap encodeBoundBinder bs

encodeBoundBinder :: BoundBinder -> Encoding
encodeBoundBinder (BoundBinder name varid modul tier tdisp rootHead hostAuthority) =
  encodeListLen 7
  <> encodeString (T.pack name)
  <> encodeWord64 varid
  <> encodeString (T.pack modul)
  <> encodeString (case tier of
      ForceData -> "ForceData"
      RetainOpaque -> "RetainOpaque")
  <> encodeString (T.pack tdisp)
  <> maybe encodeNull encodeHead rootHead
  <> maybe encodeNull encodeAuthority hostAuthority
  where
    encodeHead (NominalHead unit headModule headName) =
      encodeListLen 3 <> encodeString unit <> encodeString headModule <> encodeString headName
    encodeAuthority JsonValueAuthority = encodeString "JsonValue"
    encodeAuthority TextAuthority = encodeString "Text"
    encodeAuthority CommandJobAuthority = encodeString "CommandJob"

-- | Presentation and native signatures for compiler-issued typed sites.
-- The metadata names each exact input witness independently of wire graphs.
encodeAsks :: [YieldSite] -> Encoding
encodeAsks xs = encodeListLen (fromIntegral (length xs)) <> foldMap encodeAsk xs

encodeAsk :: YieldSite -> Encoding
encodeAsk (YieldSite site origin ordinal (SiteType ty modules heads) inputs witnesses declaration signatures) =
  encodeListLen 10
  <> encodeWord64 site
  <> encodeString origin
  <> encodeWord64 ordinal
  <> encodeString ty
  <> encodeTextList modules
  <> encodeHeads heads
  <> encodeListLen (fromIntegral (length inputs))
  <> foldMap encodeSiteType inputs
  <> maybe encodeNull encodeString declaration
  <> encodeListLen (fromIntegral (length witnesses))
  <> foldMap (maybe encodeNull (maybe encodeNull (encodeBytes . toStrictByteString) . encodeCheckedTypeWitness)) witnesses
  <> maybe encodeNull (encodeBytes . toStrictByteString . encodeRequestTypeSignatures) signatures

encodeSiteType :: SiteType -> Encoding
encodeSiteType (SiteType ty modules heads) =
  encodeListLen 3 <> encodeString ty <> encodeTextList modules <> encodeHeads heads

encodeHeads :: [NominalHead] -> Encoding
encodeHeads heads = encodeListLen (fromIntegral (length heads)) <> foldMap encodeHead heads
  where
    encodeHead (NominalHead unit modul name) =
      encodeListLen 3 <> encodeString unit <> encodeString modul <> encodeString name
