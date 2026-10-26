{-# LANGUAGE OverloadedStrings #-}

-- Typed fixture requests consumed by production codecs and certifiers.
module Tidepool.Test.CandidateCodec
  ( CandidateCodecCase(..), writeCandidateCodecFixture
  , writeEmptyScopeCodecFixture, writeInspectionPurposeCodecFixture
  , writeOriginalResolutionScopeCodecFixture ) where

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
  issueCodecFixturePacket work "module-candidates.cbor" (toStrictByteString (encodeTerm
    (TList [TString "TPCODECFIXTURE1", TString operation, TList []])))

-- No compiler is invoked: the owning scope serializer issues empty metadata.
writeEmptyScopeCodecFixture :: FilePath -> IO FilePath
writeEmptyScopeCodecFixture work = do
  issueCodecFixturePacket work ("scope" </> "exact-declaration-scope.cbor") (toStrictByteString (encodeTerm
    (TList [TString "TPCODECFIXTURE1", TString "scope_empty", TList []])))

-- Actual Rust certifier issuance supplies scope-admission facts. This fixture
-- does not establish GHC interface hydration or executable retained captures.
writeOriginalResolutionScopeCodecFixture :: FilePath -> IO FilePath
writeOriginalResolutionScopeCodecFixture work =
  issueCodecFixturePacket work ("scope" </> "exact-declaration-scope.cbor") (toStrictByteString (encodeTerm
    (TList [TString "TPCODECFIXTURE1", TString "scope_original_resolutions", TList []])))

-- The fixture protocol carries typed inputs, rather than a purpose wire array.
-- Rust's inspection and search serializers own the emitted authorization.
writeInspectionPurposeCodecFixture :: FilePath -> [FilePath] -> [ExactIfaceArtifact] -> IO FilePath
writeInspectionPurposeCodecFixture work includes values = do
  let text = TString . T.pack
      value artifact = TList [text (exactUnit artifact),text (exactModule artifact)
        ,text (exactPath artifact),text (exactSha256 artifact)]
  issueCodecFixturePacket work "purpose.cbor" (toStrictByteString (encodeTerm
    (TList [TString "TPCODECFIXTURE1", TString "inspection_purpose"
      ,TList [TList (map text includes),TList (map value values)]])))
