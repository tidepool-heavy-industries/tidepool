module Tidepool.CborEncode (encodeTree, encodeMetadata, encodeTurnOut) where

import Codec.CBOR.Encoding
import Codec.CBOR.Write (toStrictByteString)
import Data.ByteString (ByteString)
import qualified Data.ByteString as BS
import Data.Text (Text)
import qualified Data.Text as T
import Data.Word
import Data.Sequence (Seq)
import qualified Data.Sequence as Seq
import Tidepool.IR (FlatNode(..), LitEnc(..), FlatAlt(..), FlatAltCon(..))
import Tidepool.Metadata (DCMeta(..))
import Tidepool.Binders (TurnOut(..), BoundBinder(..), ExportItem(..))

-- | 8-byte version header: magic 'TPLR' + version 3.0.
--
-- Must be kept byte-identical to the Rust reader's own
-- @VERSION_MAJOR@/@VERSION_MINOR@ (tidepool-repr/src/serial/mod.rs) — there
-- is no shared formatter across the language boundary, so a version bump
-- needs a matching change on both sides.
--
-- 2.0 (from 1.1) was the breaking metadata-entry shape change: 7 -> 8
-- elements, parent-type-name channel. 2.1 was a MINOR bump: the OPTIONAL
-- @poisoned@ warnings key (sentinel slot -> qualified name). 3.0 is another
-- breaking metadata-entry shape change: 8 -> 9 elements, rendered field
-- types (in declaration order) as the 9th element.
tplrHeader :: ByteString
tplrHeader = BS.pack [0x54, 0x50, 0x4C, 0x52, 0x00, 0x03, 0x00, 0x00]

-- | Encodes the flattened node tree into a CBOR payload prepended with the TPLR version header.
encodeTree :: Seq FlatNode -> ByteString
encodeTree nodes = tplrHeader <> toStrictByteString (
  encodeListLen 2
  <> encodeNodesArray nodes
  <> encodeWord (fromIntegral (Seq.length nodes - 1)))  -- root index

encodeNodesArray :: Seq FlatNode -> Encoding
encodeNodesArray nodes =
  encodeListLen (fromIntegral (Seq.length nodes))
  <> foldMap encodeNode nodes

encodeNode :: FlatNode -> Encoding
encodeNode = \case
  NVar vid ->
    encodeListLen 2 <> encodeString "Var" <> encodeWord64 vid
  NLit lit ->
    encodeListLen 2 <> encodeString "Lit" <> encodeLitEnc lit
  NApp f a ->
    encodeListLen 3 <> encodeString "App" <> encodeWord (fromIntegral f) <> encodeWord (fromIntegral a)
  NLam bid body ->
    encodeListLen 3 <> encodeString "Lam" <> encodeWord64 bid <> encodeWord (fromIntegral body)
  NLetNonRec bid rhs body ->
    encodeListLen 4 <> encodeString "LetNonRec" <> encodeWord64 bid <> encodeWord (fromIntegral rhs) <> encodeWord (fromIntegral body)
  NLetRec bindings body ->
    encodeListLen 3 <> encodeString "LetRec"
    <> encodeListLen (fromIntegral (length bindings))
    <> foldMap (\(bid, rhs) -> encodeListLen 2 <> encodeWord64 bid <> encodeWord (fromIntegral rhs)) bindings
    <> encodeWord (fromIntegral body)
  NCase scrut bid alts ->
    encodeListLen 4 <> encodeString "Case"
    <> encodeWord (fromIntegral scrut) <> encodeWord64 bid
    <> encodeListLen (fromIntegral (length alts))
    <> foldMap encodeFlatAlt alts
  NCon dcid fields ->
    encodeListLen 3 <> encodeString "Con" <> encodeWord64 dcid
    <> encodeListLen (fromIntegral (length fields))
    <> foldMap (\f -> encodeWord (fromIntegral f)) fields
  NJoin lid params rhs body ->
    encodeListLen 5 <> encodeString "Join" <> encodeWord64 lid
    <> encodeListLen (fromIntegral (length params))
    <> foldMap encodeWord64 params
    <> encodeWord (fromIntegral rhs) <> encodeWord (fromIntegral body)
  NJump lid args ->
    encodeListLen 3 <> encodeString "Jump" <> encodeWord64 lid
    <> encodeListLen (fromIntegral (length args))
    <> foldMap (\a -> encodeWord (fromIntegral a)) args
  NPrimOp name args ->
    encodeListLen 3 <> encodeString "PrimOp" <> encodeString name
    <> encodeListLen (fromIntegral (length args))
    <> foldMap (\a -> encodeWord (fromIntegral a)) args

encodeLitEnc :: LitEnc -> Encoding
encodeLitEnc = \case
  LEInt n    -> encodeListLen 2 <> encodeString "LitInt" <> encodeInt64 n
  LEWord n   -> encodeListLen 2 <> encodeString "LitWord" <> encodeWord64 n
  LEChar n   -> encodeListLen 2 <> encodeString "LitChar" <> encodeWord32 n
  LEString b -> encodeListLen 2 <> encodeString "LitString" <> encodeBytes b
  LEByteArray b -> encodeListLen 2 <> encodeString "LitByteArray" <> encodeBytes b
  LEFloat n  -> encodeListLen 2 <> encodeString "LitFloat" <> encodeWord64 n
  LEDouble n -> encodeListLen 2 <> encodeString "LitDouble" <> encodeWord64 n

encodeFlatAlt :: FlatAlt -> Encoding
encodeFlatAlt (FlatAlt con binders body) =
  encodeListLen 3
  <> encodeFlatAltCon con
  <> encodeListLen (fromIntegral (length binders))
  <> foldMap encodeWord64 binders
  <> encodeWord (fromIntegral body)

encodeFlatAltCon :: FlatAltCon -> Encoding
encodeFlatAltCon = \case
  FDataAlt dcid -> encodeListLen 2 <> encodeString "DataAlt" <> encodeWord64 dcid
  FLitAlt lit   -> encodeListLen 2 <> encodeString "LitAlt" <> encodeLitEnc lit
  FDefault      -> encodeListLen 1 <> encodeString "Default"

-- | Encode the DataCon table + a warnings map: @[entries_array, warnings_map]@.
-- The warnings map always carries @has_io@; when a captured type is present
-- (the eval's @__user@ binding type — see GhcPipeline.capturedUserType) it also
-- carries @captured_type@. The Rust reader (serial/read.rs parse_warnings)
-- tolerates either map shape, so omitting the key on Nothing is backward-safe.
-- The @[Text]@ is the GHC diagnostic warnings for the target module
-- (see GhcPipeline.prWarnings) — an empty list omits the @warnings@ key
-- entirely, keeping a clean compile's meta.cbor byte-identical to before.
-- The trailing @[(Word64, Text)]@ is the @poisoned@ table (wire 2.1): the
-- identity slot each emitted @0x45@ kind-4 sentinel carries, paired with the
-- qualified name of the unresolved external it replaced (Translate.cmPoisoned).
-- Same omit-when-empty rule, so an extraction that poisoned nothing is
-- byte-identical to a 2.0 payload apart from the header's minor.
encodeMetadata :: [DCMeta] -> Bool -> Maybe Text -> [(Word64, Text)] -> [Text] -> [(Word64, Text)] -> ByteString
encodeMetadata entries hasIO mCapturedType varNames warnings poisoned = tplrHeader <> toStrictByteString (
  encodeListLen 2
  <> (encodeListLen (fromIntegral (length entries)) <> foldMap encodeMetaEntry entries)
  <> warningsMap)
  where
    -- Optional keys are simply omitted when absent; the Rust reader accepts
    -- omitted keys but rejects unknown ones (metadata_strictness.rs), so new
    -- keys require a version bump on both sides.
    warningsMap =
      encodeMapLen (1 + maybe 0 (const 1) mCapturedType
                      + (if null varNames then 0 else 1)
                      + (if null warnings then 0 else 1)
                      + (if null poisoned then 0 else 1))
      <> encodeString "has_io" <> encodeBool hasIO
      <> maybe mempty (\ty -> encodeString "captured_type" <> encodeString ty) mCapturedType
      <> (if null varNames then mempty else
            encodeString "var_names" <> encodeIdNamePairs varNames)
      <> (if null warnings then mempty else
            encodeString "warnings"
            <> encodeListLen (fromIntegral (length warnings))
            <> foldMap encodeString warnings)
      <> (if null poisoned then mempty else
            encodeString "poisoned" <> encodeIdNamePairs poisoned)

-- | The @[[id, name], …]@ value shape shared by the @var_names@ and
-- @poisoned@ warnings keys (Rust: @serial::read::parse_id_name_pairs@).
encodeIdNamePairs :: [(Word64, Text)] -> Encoding
encodeIdNamePairs pairs =
  encodeListLen (fromIntegral (length pairs))
  <> foldMap (\(k, v) -> encodeListLen 2 <> encodeWord64 k <> encodeString v) pairs

encodeMetaEntry :: DCMeta -> Encoding
encodeMetaEntry DCMeta{dcmId, dcmName, dcmTag, dcmArity, dcmBangs, dcmQualName, dcmFieldLabels, dcmTypeName, dcmFieldTypes} =
  let
    tagWord :: Word
    tagWord =
      if dcmTag < 0
        then error "encodeMetaEntry: negative constructor tag"
        else fromIntegral dcmTag
  in
  -- The Rust reader requires exactly nine elements. Positional constructors
  -- carry an empty labels array. The 8th
  -- element is the rendered name of the constructor's parent TyCon (e.g.
  -- "Verdict"), always present — every DataCon has a parent type. The 9th
  -- element is the constructor's field types, rendered in declaration order
  -- (same @ppr@ convention as the 8th element and asks.json), always present
  -- (empty array for a nullary constructor).
  encodeListLen 9
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

--------------------------------------------------------------------------------
-- Turn-mode rich result (--turn) — independent of the frozen tree format
-- above: no TPLR header, no version coupling. A 2-element list, a string tag
-- ("Decl"/"Bind"/"Expr") plus the variant's payload, mirroring 'encodeNode's
-- tagged-list convention. This is the ONLY serialization of a 'TurnOut': the
-- parallel JSON rendering (@--json-output@ / @renderTurnOutJson@) was a second
-- hand-maintained serializer with no reader and was deleted.
--------------------------------------------------------------------------------

encodeTurnOut :: TurnOut -> ByteString
encodeTurnOut turnOut = toStrictByteString $ case turnOut of
  TDecl bs items ->
    encodeListLen 2 <> encodeString "Decl"
    <> (encodeListLen 2 <> encodeTextList bs <> encodeExportItems items)
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
encodeBoundBinder (BoundBinder name varid modul tier tdisp) =
  encodeListLen 5
  <> encodeString (T.pack name)
  <> encodeWord64 varid
  <> encodeString (T.pack modul)
  <> encodeString (T.pack tier)
  <> encodeString (T.pack tdisp)

-- | @modules@ (the third element, added alongside @site@/@type@ — see
-- 'Tidepool.Translate.modulesOfType') is the defining-module set a shim
-- must import to resolve @type@ by name. This is the SAME wire a resident
-- session turn's suspension classifies against (@tidepool-harness@'s
-- fork/finalize servicing reads it back to pin a fork child's @Finalize@
-- row) — the asks.json sidecar is a SEPARATE encoding of the identical data
-- for the multi-target/whole-module compile path.
encodeAsks :: [(Word64, Text, [Text])] -> Encoding
encodeAsks xs = encodeListLen (fromIntegral (length xs)) <> foldMap encodeAsk xs

encodeAsk :: (Word64, Text, [Text]) -> Encoding
encodeAsk (site, ty, modules) =
  encodeListLen 3 <> encodeWord64 site <> encodeString ty <> encodeTextList modules
