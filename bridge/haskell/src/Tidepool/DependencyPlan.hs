{-# LANGUAGE OverloadedStrings #-}
{-# LANGUAGE RankNTypes #-}

-- | The request owns dependency observations separately from executable demand.
-- A retained terminal keeps its exact defining-use and ordered import occurrence.
module Tidepool.DependencyPlan
  ( DependencyOrigin(..), NativeOriginalUse
  , SourceDependencyQuery, sourceDependencyQuery, NativeDependencyQuery, nativeDependencyQuery
  , SourceDependencyReply, sourceDependencyRetained
  , NativeDependencyReply, nativeDependencyPackages, nativeDependencyRetained
  , exchangeSourceDependencies, exchangeNativeDependencies
  , RetainedOccurrence, DependencyTerminal, terminalBinding
  , CompilerDependencyBasis, FrozenDependencyBasis
  , newCompilerDependencyBasis, captureSourceDependencies, freezeDependencyBasis
  , frozenDependencyGenerations, selectDependencyTerminals
  , FinalizedSourceDependencies, finalizeSourceDependencies, finalizedSourceTerminals, finalizedSourceUses
  , requireUnambiguousDependencySymbols
  , nativeDependencyTerminals, encodeDependencyTerminal
  , NativePreparationFailure(..)
  ) where

import Control.Exception (Exception, throwIO)
import Control.Monad (foldM, forM_, replicateM, unless, when)
import Control.Concurrent.MVar (MVar, newMVar, withMVar)
import Data.IORef (IORef, newIORef, readIORef, writeIORef)
import qualified Data.Map.Strict as Map
import qualified Data.Set as Set
import Data.List (sortOn)
import Data.Word (Word32, Word64)
import qualified Data.ByteString as BS
import qualified Data.ByteString.Lazy as BL
import qualified Data.Text as T
import Codec.CBOR.Encoding
import qualified Codec.CBOR.Decoding as D
import Codec.CBOR.Read (deserialiseFromBytes)
import Codec.CBOR.Write (toStrictByteString)
import Tidepool.ExactScope (WorldOriginalReference, worldOriginalReferenceOwner, worldOriginalReferenceBindings)
import Tidepool.ExecutionSchema (SymbolIdentity(..))
import Tidepool.CertifiedProducts (encodeReceiptIdentity)
import Tidepool.WorkerServer (NativeDemandExchange, NativeDemandReply(..), exchangeNativeDemand
  , SourceDemandBorrow, borrowSourceDemand, exchangeSourceDemand)

data DependencyOrigin = SourceDefinition | NativeOriginal deriving (Eq,Ord,Show)
originTag :: DependencyOrigin -> Word64
originTag SourceDefinition = 0
originTag NativeOriginal = 1

data RetainedOccurrence = RetainedOccurrence
  !(BS.ByteString,BS.ByteString,Word32) !Word32 !(BS.ByteString,BS.ByteString)
  !SymbolIdentity !Word64 deriving (Eq,Ord,Show)
newtype SourceDependencyQuery = SourceDependencyQuery [NativeOriginalUse]
newtype NativeDependencyQuery = NativeDependencyQuery [NativeOriginalUse]

sourceDependencyQuery :: [NativeOriginalUse] -> SourceDependencyQuery
sourceDependencyQuery = SourceDependencyQuery . Set.toAscList . Set.fromList

nativeDependencyQuery :: [NativeOriginalUse] -> NativeDependencyQuery
nativeDependencyQuery = NativeDependencyQuery . Set.toAscList . Set.fromList

newtype SourceDependencyReply = SourceDependencyReply [RetainedOccurrence]
data NativeDependencyReply = NativeDependencyReply
  !(Map.Map SymbolIdentity BS.ByteString) ![RetainedOccurrence]

sourceDependencyRetained :: SourceDependencyReply -> [RetainedOccurrence]
sourceDependencyRetained (SourceDependencyReply retained) = retained

nativeDependencyPackages :: NativeDependencyReply -> Map.Map SymbolIdentity BS.ByteString
nativeDependencyPackages (NativeDependencyReply packages _) = packages

nativeDependencyRetained :: NativeDependencyReply -> [RetainedOccurrence]
nativeDependencyRetained (NativeDependencyReply _ retained) = retained

-- Only the private wire decoder carries both fields. Semantic consumers
-- receive the branch-specific reply, and SOURCE has no package surface.
data DecodedDependencyReply = DecodedDependencyReply
  !(Map.Map SymbolIdentity BS.ByteString) ![RetainedOccurrence]

exchangeSourceDependencies :: SourceDemandBorrow -> SourceDependencyQuery
  -> IO (Map.Map NativeOriginalUse SourceDependencyReply)
exchangeSourceDependencies exchange (SourceDependencyQuery uses) = do
  decoded <- exchangeSourceDemand exchange (encodeDependencyUses SourceDefinition uses)
    >>= readDependencyReply SourceDefinition uses
  pure (Map.map (\(DecodedDependencyReply _ retained) -> SourceDependencyReply retained) decoded)

exchangeNativeDependencies :: NativeDemandExchange -> Word64 -> NativeDependencyQuery
  -> IO (Map.Map NativeOriginalUse NativeDependencyReply)
exchangeNativeDependencies exchange stage (NativeDependencyQuery uses) = do
  decoded <- exchangeNativeDemand exchange stage (encodeDependencyUses NativeOriginal uses)
    >>= readDependencyReply NativeOriginal uses
  pure (Map.map (\(DecodedDependencyReply packages retained) -> NativeDependencyReply packages retained) decoded)

data DependencyTerminal = DependencyTerminal !DependencyOrigin !NativeOriginalUse !RetainedOccurrence
  deriving (Eq,Ord)

-- Compiler tasks share this operation's SOURCE-only borrow. The basis gate
-- covers discovery through publication; lock order is basis then transport.
data CompilerDependencyBasis = CompilerDependencyBasis !SourceDemandBorrow
  !(MVar ()) !(IORef (Map.Map NativeOriginalUse SourceDependencyReply))
data FrozenDependencyBasis = FrozenDependencyBasis [DependencyTerminal]
  (Map.Map SymbolIdentity (Set.Set ((BS.ByteString,BS.ByteString),Word64)))

newCompilerDependencyBasis :: NativeDemandExchange -> Word64 -> IO CompilerDependencyBasis
newCompilerDependencyBasis exchange stage = do
  borrowed <- borrowSourceDemand exchange stage
  CompilerDependencyBasis borrowed <$> newMVar () <*> newIORef Map.empty

captureSourceDependencies :: CompilerDependencyBasis -> [NativeOriginalUse] -> IO ()
captureSourceDependencies (CompilerDependencyBasis exchange gate state) uses = withMVar gate $ \_ -> do
  known <- readIORef state
  let pending = Set.toAscList (Set.fromList uses `Set.difference` Map.keysSet known)
  unless (null pending) $ do
    reply <- exchangeSourceDependencies exchange (sourceDependencyQuery pending)
    writeIORef state (Map.union known reply)

sourceTerminals :: Map.Map NativeOriginalUse SourceDependencyReply -> [DependencyTerminal]
sourceTerminals replies = [DependencyTerminal SourceDefinition use occurrence
  | (use,reply) <- Map.toAscList replies, occurrence <- sourceDependencyRetained reply]

nativeDependencyTerminals :: Map.Map NativeOriginalUse NativeDependencyReply -> Set.Set NativeOriginalUse -> IO [DependencyTerminal]
nativeDependencyTerminals replies uses = fmap concat $ mapM select (Set.toAscList uses)
  where
    select use = case Map.lookup use replies of
      Nothing -> throwIO (NativeReplyCorrupt "selected native use lacks its issued reply")
      Just reply -> pure [DependencyTerminal NativeOriginal use occurrence | occurrence <- nativeDependencyRetained reply]

freezeDependencyBasis :: CompilerDependencyBasis -> IO FrozenDependencyBasis
freezeDependencyBasis (CompilerDependencyBasis _ gate state) = withMVar gate $ \_ -> do
  terminals <- sourceTerminals <$> readIORef state
  let choices = Map.fromListWith Set.union
        [(identity,Set.singleton (binding,generation))
          | DependencyTerminal _ _ (RetainedOccurrence _ _ binding identity generation) <- terminals]
  pure (FrozenDependencyBasis terminals choices)

frozenDependencyGenerations :: FrozenDependencyBasis -> IO (Map.Map SymbolIdentity Word64)
frozenDependencyGenerations basis@(FrozenDependencyBasis _ choices) = pure (Map.fromList
  [(identity,generation) | identity <- Map.keys choices
    , SourceDependencyUnique generation <- [resolveSourceDependencyUse basis identity]])

-- Absence and conflicting historical owners are separate outcomes. Consumers
-- must check reached references against this owner before attempting recovery.
data ResolvedSourceDependencyUse = SourceDependencyAbsent
  | SourceDependencyUnique Word64 | SourceDependencyConflict

resolveSourceDependencyUse :: FrozenDependencyBasis -> SymbolIdentity -> ResolvedSourceDependencyUse
resolveSourceDependencyUse (FrozenDependencyBasis _ choices) requested =
  case maybe [] Set.toList (Map.lookup requested choices) of
    [] -> SourceDependencyAbsent
    [(_,generation)] -> SourceDependencyUnique generation
    _ -> SourceDependencyConflict

terminalBinding :: DependencyTerminal -> (SymbolIdentity,Word64)
terminalBinding (DependencyTerminal _ _ (RetainedOccurrence _ _ _ identity generation)) = (identity,generation)

terminalGenerations :: [DependencyTerminal] -> IO (Map.Map SymbolIdentity Word64)
terminalGenerations terminals = do
  selected <- foldM add Map.empty terminals
  pure (Map.map snd selected)
  where
    add known (DependencyTerminal _ _ (RetainedOccurrence _ _ binding identity generation)) = do
      forM_ (Map.lookup identity known) $ \old -> unless (old == (binding,generation))
        (throwIO (DependencyTerminalConflict identity))
      pure (Map.insert identity (binding,generation) known)

-- A generation projection cannot turn a known conflict into absent evidence.
-- Both recovery front doors call this with their actual reached references.
requireUnambiguousDependencySymbols :: FrozenDependencyBasis -> Set.Set SymbolIdentity -> IO ()
requireUnambiguousDependencySymbols basis reached = forM_ (Set.toAscList reached) $ \identity ->
  case resolveSourceDependencyUse basis identity of
    SourceDependencyConflict -> throwIO (DependencyTerminalConflict identity)
    SourceDependencyAbsent -> pure ()
    SourceDependencyUnique _ -> pure ()

selectDependencyTerminals :: Set.Set (SymbolIdentity,Word64) -> FrozenDependencyBasis -> IO [DependencyTerminal]
selectDependencyTerminals needed (FrozenDependencyBasis terminals _) = do
  let selected = filter ((`Set.member` needed) . terminalBinding) terminals
  _ <- terminalGenerations selected
  pure selected

-- A completed stage borrows only SOURCE decisions selected from its own
-- frozen operation. Native terminals cannot enter the inherited source route.
newtype FinalizedSourceDependencies = FinalizedSourceDependencies [DependencyTerminal]

finalizeSourceDependencies :: Maybe FrozenDependencyBasis -> Set.Set (SymbolIdentity,Word64)
  -> IO FinalizedSourceDependencies
finalizeSourceDependencies Nothing _ = pure (FinalizedSourceDependencies [])
finalizeSourceDependencies (Just basis) needed =
  FinalizedSourceDependencies <$> selectDependencyTerminals needed basis

finalizedSourceTerminals :: FinalizedSourceDependencies -> [DependencyTerminal]
finalizedSourceTerminals (FinalizedSourceDependencies terminals) = terminals

finalizedSourceUses :: FinalizedSourceDependencies -> [NativeOriginalUse]
finalizedSourceUses (FinalizedSourceDependencies terminals) = Set.toAscList (Set.fromList
  [use | DependencyTerminal SourceDefinition use _ <- terminals])

encodeDependencyTerminal :: DependencyTerminal -> Encoding
encodeDependencyTerminal (DependencyTerminal origin use (RetainedOccurrence (artifact,scope,ordinal) index retained identity generation)) =
  encodeListLen 7 <> encodeWord64 (originTag origin) <> encodeUse use
  <> encodeListLen 3 <> encodeBytes artifact <> encodeBytes scope <> encodeWord32 ordinal
  <> encodeWord32 index <> encodeBinding retained <> encodeReceiptIdentity identity <> encodeWord64 generation
  where
    encodeBinding (artifact,scope) = encodeListLen 2 <> encodeBytes artifact <> encodeBytes scope
    encodeUse (identity,reference) = let
      (unit,name) = worldOriginalReferenceOwner reference
      (interface,original) = worldOriginalReferenceBindings reference
      in encodeListLen 5 <> encodeString unit <> encodeString name
        <> encodeBinding interface <> encodeBinding original <> encodeReceiptIdentity identity

-- These replies are obligations from the held Rust input owner, not new
-- source or execution authority. Final issuance checks their captured bytes.
data NativeDemandRefusalReason
  = InvalidNativeWitness | OriginalUnavailable | NativeGroupUnavailable
  | PackageProductUnavailable | NativeInputConflict | NativeAdmissionLimit
  deriving (Show)

data NativePreparationFailure
  = NativeDemandRejected NativeDemandRefusalReason T.Text
  | NativeReplyCorrupt String
  | NativePackageSealConflict SymbolIdentity
  | DependencyTerminalConflict SymbolIdentity
  deriving (Show)
instance Exception NativePreparationFailure

encodeDependencyUses :: DependencyOrigin -> [NativeOriginalUse] -> BS.ByteString
encodeDependencyUses origin uses = toStrictByteString $ array
  [encodeString "TPDEPENDENCYQUERY1",encodeWord64 (originTag origin),list use (sortOn (\(symbol,reference) -> (reference,symbol)) uses)]
  where
    array fields = encodeListLen (fromIntegral (length fields)) <> mconcat fields
    list encode values = encodeListLen (fromIntegral (length values)) <> foldMap encode values
    use (symbol,reference) =
      let (unit,name) = worldOriginalReferenceOwner reference
          ((interface,interfaceScope),(original,originalScope)) = worldOriginalReferenceBindings reference
      in array [encodeString unit,encodeString name
        ,array [encodeBytes interface,encodeBytes interfaceScope]
        ,array [encodeBytes original,encodeBytes originalScope],encodeReceiptIdentity symbol]

type NativeOriginalUse = (SymbolIdentity,WorldOriginalReference)
type NativeUseCoordinate = (T.Text,T.Text,(BS.ByteString,BS.ByteString),(BS.ByteString,BS.ByteString),SymbolIdentity)

nativeUseCoordinate :: NativeOriginalUse -> NativeUseCoordinate
nativeUseCoordinate (identity,reference) =
  let (unit,name) = worldOriginalReferenceOwner reference
      (interface,original) = worldOriginalReferenceBindings reference
  in (unit,name,interface,original,identity)

readDependencyReply :: DependencyOrigin -> [NativeOriginalUse] -> NativeDemandReply
  -> IO (Map.Map NativeOriginalUse DecodedDependencyReply)
readDependencyReply origin expected reply = case reply of
  NativeRefused bytes -> decode bytes refusal >>= \(reason,diagnostic) ->
    throwIO (NativeDemandRejected reason diagnostic)
  NativeResolved bytes -> decode bytes (resolved (BS.length bytes))
  where
    decode :: BS.ByteString -> (forall s. D.Decoder s a) -> IO a
    decode bytes parser = case deserialiseFromBytes parser (BL.fromStrict bytes) of
      Left failure -> throwIO (NativeReplyCorrupt (show failure))
      Right (remaining,value) | BL.null remaining -> pure value
      Right _ -> throwIO (NativeReplyCorrupt "trailing native reply bytes")
    require :: Int -> D.Decoder s ()
    require fields = D.decodeListLen >>= \actual -> unless (actual == fields) (fail "native reply field count")
    magic :: T.Text -> D.Decoder s ()
    magic expected = D.decodeString >>= \actual -> unless (actual == expected) (fail "native reply tag")
    symbol :: D.Decoder s SymbolIdentity
    symbol = do
      require 5
      unit <- D.decodeString
      name <- D.decodeString
      namespace <- D.decodeString
      occurrence <- D.decodeString
      kind <- D.peekTokenType
      parent <- if kind == D.TypeNull then D.decodeNull >> pure Nothing else Just <$> D.decodeString
      unless (all (not . T.null) [unit,name,namespace,occurrence]) (fail "empty native symbol")
      pure (SymbolIdentity unit name namespace occurrence parent)
    digest :: D.Decoder s BS.ByteString
    digest = do
      bytes <- D.decodeBytes
      unless (BS.length bytes == 32 && BS.any (/= 0) bytes) (fail "native reply digest")
      pure bytes
    binding :: D.Decoder s (BS.ByteString,BS.ByteString)
    binding = require 2 >> ((,) <$> digest <*> digest)
    use :: D.Decoder s NativeUseCoordinate
    use = do
      require 5
      unit <- D.decodeString
      name <- D.decodeString
      interface <- binding
      original <- binding
      identity <- symbol
      pure (unit,name,interface,original,identity)
    resolved :: Int -> D.Decoder s (Map.Map NativeOriginalUse DecodedDependencyReply)
    resolved bound = do
      require 3
      magic "TPDEPENDENCYREPLY1"
      actualOrigin <- D.decodeWord64
      unless (actualOrigin == originTag origin) (fail "dependency reply origin")
      let expectedRows = Map.toAscList (Map.fromList [(nativeUseCoordinate entry,entry) | entry <- expected])
      count <- D.decodeListLen
      unless (count == length expectedRows && count == length expected && count <= bound)
        (fail "native reply use count")
      rows <- replicateM count $ do
        require 3
        coordinate <- use
        obligations <- D.decodeListLen
        unless (obligations <= bound) (fail "native reply obligation count")
        roots <- replicateM obligations $ do
          require 2
          identity <- symbol
          seal <- digest
          pure (identity,seal)
        unless (roots == Set.toAscList (Set.fromList roots)
            && Map.size (Map.fromList roots) == length roots)
          (fail "native reply obligation order or conflict")
        when (origin == SourceDefinition && not (null roots)) (fail "source query acquired packages")
        terminalCount <- D.decodeListLen
        unless (terminalCount <= bound) (fail "dependency terminal count")
        terminals <- replicateM terminalCount $ do
          require 5
          require 3
          artifact <- digest
          selection <- digest
          ordinal <- D.decodeWord32
          index <- D.decodeWord32
          retained <- binding
          identity <- symbol
          generation <- D.decodeWord64
          pure (RetainedOccurrence (artifact,selection,ordinal) index retained identity generation)
        unless (terminals == Set.toAscList (Set.fromList terminals))
          (fail "dependency terminal order or duplicate")
        pure (coordinate,DecodedDependencyReply (Map.fromList roots) terminals)
      unless (map fst rows == map fst expectedRows)
        (fail "native reply missing, foreign, duplicate or reordered use")
      pure (Map.fromList [(entry,roots) | ((_,entry),(_,roots)) <- zip expectedRows rows])
    refusal :: D.Decoder s (NativeDemandRefusalReason,T.Text)
    refusal = do
      require 3
      magic "TPNATIVEREFUSAL1"
      tag <- D.decodeWord
      reason <- case tag of
        0 -> pure InvalidNativeWitness
        1 -> pure OriginalUnavailable
        2 -> pure NativeGroupUnavailable
        3 -> pure PackageProductUnavailable
        4 -> pure NativeInputConflict
        5 -> pure NativeAdmissionLimit
        _ -> fail "native refusal reason"
      diagnostic <- D.decodeString
      pure (reason,diagnostic)

