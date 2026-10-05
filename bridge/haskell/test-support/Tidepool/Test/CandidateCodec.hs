-- Typed structural candidate inputs from the production codec. These packets
-- carry no compiler capture, protected grant, or executable authority.
module Tidepool.Test.CandidateCodec
  ( CandidateCodecCase(..), writeCandidateCodecFixture ) where

import Codec.CBOR.Term (Term(..), encodeTerm)
import Codec.CBOR.Write (toStrictByteString)
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
