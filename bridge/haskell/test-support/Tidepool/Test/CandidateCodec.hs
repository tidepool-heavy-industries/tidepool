{-# LANGUAGE OverloadedStrings #-}

-- Typed structural candidate inputs from the production codec. These packets
-- carry no compiler capture, protected grant, or executable authority.
module Tidepool.Test.CandidateCodec
  ( CandidateCodecCase(..), writeCandidateCodecFixture
  , writeEmptyScopeCodecFixture, writeCellPurposeCodecFixture ) where

import Codec.CBOR.Term (Term(..), encodeTerm)
import Codec.CBOR.Write (toStrictByteString)
import qualified Data.Text as T
import Tidepool.ExactHydration (ExactIfaceArtifact(..))
import System.FilePath ((</>))
import Tidepool.Test.FixturePacket (issueCodecFixturePacket)

data CandidateCodecCase
  = EmptyCandidateInventory
  | CompactCandidateInventory
  | BoundedExpandedCandidateInventory
  | SingleGroupCandidateInventory

writeCandidateCodecFixture :: FilePath -> CandidateCodecCase -> IO FilePath
writeCandidateCodecFixture work scenario = do
  let operation = case scenario of
        EmptyCandidateInventory -> "candidate_empty"
        CompactCandidateInventory -> "candidate_compact"
        BoundedExpandedCandidateInventory -> "candidate_expanded_within_bound"
        SingleGroupCandidateInventory -> "candidate_structural_group"
  packet <- issueCodecFixturePacket work (toStrictByteString (encodeTerm
    (TList [TString "TPCODECFIXTURE1", TString operation, TList []])))
  pure (packet </> "module-candidates.cbor")

-- No compiler is invoked: the owning scope serializer issues empty metadata.
writeEmptyScopeCodecFixture :: FilePath -> IO FilePath
writeEmptyScopeCodecFixture work = do
  packet <- issueCodecFixturePacket work (toStrictByteString (encodeTerm
    (TList [TString "TPCODECFIXTURE1", TString "scope_empty", TList []])))
  pure (packet </> "scope" </> "exact-declaration-scope.cbor")

-- The fixture protocol carries typed inputs, rather than a purpose wire array.
-- Rust's checked-cell and search serializers own the emitted authorization.
writeCellPurposeCodecFixture :: FilePath -> [FilePath] -> [ExactIfaceArtifact] -> IO FilePath
writeCellPurposeCodecFixture work includes values = do
  let text = TString . T.pack
      value artifact = TList [text (exactUnit artifact),text (exactModule artifact)
        ,text (exactPath artifact),text (exactSha256 artifact)]
  packet <- issueCodecFixturePacket work (toStrictByteString (encodeTerm
    (TList [TString "TPCODECFIXTURE1", TString "cell_purpose"
      ,TList [TList (map text includes),TList (map value values)]])))
  pure (packet </> "purpose.cbor")
