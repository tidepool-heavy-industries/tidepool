-- | Structured boundary syntax for compiler-issued typed site identity.
module Tidepool.TypeSyntaxIdentity
  ( TypeSyntaxIdentityError(..), encodeTypeSyntaxIdentity ) where

import Control.Exception (Exception)
import Control.Monad (when)
import Control.Monad.State.Strict
import Codec.CBOR.Encoding qualified as CBOR
import Codec.CBOR.Write (toStrictByteString)
import Data.ByteString qualified as BS
import Data.Foldable (fold)
import Data.IntMap.Strict (IntMap)
import Data.IntMap.Strict qualified as IntMap
import Data.Map.Strict qualified as Map
import Data.Word (Word32)
import Tidepool.ConstructorIdentity (IdentityResolver, TypeIdentityFailure, resolveTypeIdentity)
import Tidepool.ExecutionEncode (encodeTypeIdentity)
import Tidepool.ExecutionSchema (TypeNodeId(..))
import Tidepool.ExecutionSchema qualified as W
import Tidepool.TypePolicy (TypeGraph, TypeGraphError(..), tgNodes, tgEdges)

-- Site identity seals scoped expression syntax and compiler-issued nominal
-- versions. Declaration templates remain independently checked graph evidence;
-- following them here would hide inconsistent reconstruction behind a new ID.
data TypeSyntaxIdentityError
  = TypeSyntaxGraphFailure TypeGraphError
  | TypeSyntaxNominalFailure TypeIdentityFailure
  deriving (Eq, Show)
instance Exception TypeSyntaxIdentityError

data SyntaxIdentityState = SyntaxIdentityState
  { syntaxIndices :: !(Map.Map TypeNodeId Word32)
  , syntaxNodes :: !(IntMap BS.ByteString)
  , syntaxBytes :: !Int
  , syntaxWork :: !Int
  }

-- Original arena indices and diagnostic renderings never enter these bytes.
-- The interned expression DAG is numbered by first traversal from the ordered
-- roots. Nominal declarations are leaves, so recursive definitions stay finite
-- and shared syntax is emitted once rather than expanded into a type tree.
encodeTypeSyntaxIdentity :: IdentityResolver -> TypeGraph -> [TypeNodeId]
  -> Either TypeSyntaxIdentityError BS.ByteString
encodeTypeSyntaxIdentity resolver graph roots = do
  (selected, final) <- runStateT (traverse visit roots)
    (SyntaxIdentityState Map.empty IntMap.empty 0 0)
  let nodes = IntMap.elems (syntaxNodes final)
      encodedBytes = headerBytes 2 + headerBytes (length selected)
        + sum (map (headerBytes . fromIntegral) selected) + headerBytes (length nodes)
        + sum [headerBytes (BS.length bytes) + BS.length bytes | bytes <- nodes]
  if encodedBytes > 16777216 then Left (TypeSyntaxGraphFailure TypeGraphByteLimit)
    else pure (toStrictByteString (array
      [list CBOR.encodeWord32 selected, list CBOR.encodeBytes nodes]))
 where
  -- Definite CBOR array/byte-string lengths and unsigned indices share the
  -- shortest primitive width. Include their framing before final allocation.
  headerBytes value
    | value < 24 = 1
    | value < 256 = 2
    | value < 65536 = 3
    | toInteger value < 4294967296 = 5
    | otherwise = 9
  array fields = CBOR.encodeListLen (fromIntegral (length fields)) <> fold fields
  list encode values = CBOR.encodeListLen (fromIntegral (length values)) <> foldMap encode values
  tag value = CBOR.encodeWord value
  tagged value fields = array (tag value : fields)
  failure = lift . Left . TypeSyntaxGraphFailure
  charge amount = do
    work <- gets ((+ amount) . syntaxWork)
    when (work > 16777216) (failure TypeGraphWorkLimit)
    modify' (\state -> state { syntaxWork = work })
  visit node@(TypeNodeId raw) = do
    charge 1
    known <- gets (Map.lookup node . syntaxIndices)
    case known of
      Just selected -> pure selected
      Nothing -> do
        count <- gets (Map.size . syntaxIndices)
        when (count >= 65535) (failure TypeGraphNodeLimit)
        let selected = fromIntegral count
        modify' (\state -> state { syntaxIndices = Map.insert node selected (syntaxIndices state) })
        original <- maybe (failure TypeGraphIncompleteReservation) pure
          (IntMap.lookup (fromIntegral raw) (tgNodes graph))
        metadata <- case original of
          W.TypeRoot domain flags _ -> pure (tagged 0
            [tag (case domain of W.ClosedRoot -> 0; W.ConstructorSchemeRoot -> 1)
            ,list (tag . (\flag -> case flag of W.SourceSpecified -> 1; W.SourceInferred -> 2)) flags])
          W.TypeDeclaration constructor _ _ _ -> do
            identity <- lift (either (Left . TypeSyntaxNominalFailure) Right
              (resolveTypeIdentity resolver constructor))
            pure (tagged 1 [encodeTypeIdentity identity])
          W.TypeConstructorTemplate{} -> failure TypeGraphIncompleteReservation
          W.TypeBound index -> pure (tagged 3 [CBOR.encodeWord32 index])
          W.TypeNominalApplication -> pure (tag 4)
          W.TypeApplication -> pure (tag 5)
          W.TypeFunction flag -> pure (tagged 6 [tag (case flag of
            W.TypeToType -> 0; W.TypeToConstraint -> 1; W.ConstraintToType -> 2; W.ConstraintToConstraint -> 3)])
          W.TypeForAll flag -> pure (tagged 7 [tag (case flag of
            W.ForAllRequired -> 0; W.ForAllSpecified -> 1; W.ForAllInferred -> 2)])
          W.TypeLiteral literal -> pure (tagged 8 (case literal of
            W.NaturalTypeLiteral value -> [tag 0, CBOR.encodeString value]
            W.SymbolTypeLiteral value -> [tag 1, CBOR.encodeString value]
            W.CharacterTypeLiteral value -> [tag 2, CBOR.encodeWord (fromIntegral (fromEnum value))]))
        let children = case original of
              W.TypeDeclaration{} -> []
              _ -> IntMap.findWithDefault [] (fromIntegral raw) (tgEdges graph)
        encodedChildren <- traverse (\(role, child) -> do
          encodedRole <- case role of
            W.TypeBinderKind index -> pure (tagged 0 [CBOR.encodeWord32 index])
            W.TypeBody -> pure (tag 1)
            W.TypeHead -> pure (tag 2)
            W.TypeArgument index -> pure (tagged 3 [CBOR.encodeWord32 index])
            W.TypeFunctionEdge -> pure (tag 4)
            W.TypeApplyArgument -> pure (tag 5)
            W.TypeMultiplicity -> pure (tag 6)
            W.TypeDomain -> pure (tag 7)
            W.TypeCodomain -> pure (tag 8)
            W.TypeKind -> pure (tag 9)
            _ -> failure TypeGraphIncompleteReservation
          selectedChild <- visit child
          pure (array [encodedRole, CBOR.encodeWord32 selectedChild])) children
        let bytes = toStrictByteString (array [metadata, list id encodedChildren])
        charge (BS.length bytes)
        total <- gets ((+ BS.length bytes) . syntaxBytes)
        when (total > 16777216) (failure TypeGraphByteLimit)
        modify' (\state -> state
          { syntaxNodes = IntMap.insert (fromIntegral selected) bytes (syntaxNodes state)
          , syntaxBytes = total })
        pure selected

