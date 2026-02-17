module Tidepool.CborEncode (encodeTree, encodeMetadata) where

import Codec.CBOR.Encoding
import Codec.CBOR.Write (toStrictByteString)
import Data.ByteString (ByteString)
import Data.Text (Text)
import Data.Word
import Data.Int
import Data.Sequence (Seq)
import qualified Data.Sequence as Seq
import Tidepool.Translate

encodeTree :: Seq FlatNode -> ByteString
encodeTree nodes = toStrictByteString $
  encodeListLen 2
  <> encodeNodesArray nodes
  <> encodeWord (fromIntegral (Seq.length nodes - 1))  -- root index

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

encodeMetadata :: [(Word64, Text, Int, Int, [Text])] -> ByteString
encodeMetadata entries = toStrictByteString $
  encodeListLen (fromIntegral (length entries))
  <> foldMap encodeMetaEntry entries

encodeMetaEntry :: (Word64, Text, Int, Int, [Text]) -> Encoding
encodeMetaEntry (dcid, name, tag, arity, bangs) =
  encodeListLen 5
  <> encodeWord64 dcid
  <> encodeString name
  <> encodeInt tag
  <> encodeInt arity
  <> encodeListLen (fromIntegral (length bangs))
  <> foldMap encodeString bangs
