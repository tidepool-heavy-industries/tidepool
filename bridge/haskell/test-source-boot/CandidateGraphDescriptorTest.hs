{-# LANGUAGE OverloadedStrings #-}

-- Mutate a genuine current producer offer, retaining its canonical module rows.
module CandidateGraphDescriptorTest (candidateGraphDescriptorChecks) where

import Codec.CBOR.Read (deserialiseFromBytes)
import Codec.CBOR.Term (Term(..), decodeTerm, encodeTerm)
import Codec.CBOR.Write (toStrictByteString)
import Control.Exception (finally)
import Control.Monad (forM_, unless)
import Data.Bits (xor)
import Data.ByteString qualified as BS
import Data.ByteString.Lazy qualified as BL
import Data.List (isInfixOf)
import Data.Set qualified as Set
import Data.Text qualified as T
import Numeric (showHex)
import System.Directory (copyFile, removeFile, renameFile)
import System.FilePath ((</>), takeDirectory)
import System.IO (IOMode(WriteMode), hSetFileSize, withBinaryFile)
import Tidepool.ExactScope
import Tidepool.ExecutionSource
import Tidepool.ModuleCandidates

candidateGraphDescriptorChecks :: FilePath -> FilePath -> IO ()
candidateGraphDescriptorChecks scopePath candidatePath = do
  scope <- readExactScope scopePath >>= either fail pure
  bytes <- BS.readFile candidatePath
  fields <- decode bytes >>= row 7
  unless (take 2 fields == [TString "TPMCAN", TString "11"])
    (fail "descriptor fixture requires the genuine current candidate format")
  nativeRows <- values (fields !! 4)
  mapM_ (row 16) nativeRows
  parcel <- row 2 (fields !! 5)
  descriptors <- values (head parcel)
  (graphSha, graphPath) <- case descriptors of
    TList [TString sha, TString path] : _ -> pure (sha,T.unpack path)
    _ -> fail "genuine candidate offer omitted its execution graph descriptor"
  graphBytes <- BS.readFile graphPath
  unless (BS.length bytes <= 4*1024*1024 && BS.length graphBytes > 4*1024*1024
      && BS.length graphBytes <= executionSourceGraphBytesLimit)
    (fail "genuine fixture did not separate bounded metadata from its larger graph")
  fresh <- readModuleCandidates candidatePath >>= either fail pure
  known <- readModuleCandidatesWithGraphs (scopeExecutionGraphs scope) candidatePath >>= either fail pure
  unless (not (null fresh) && fresh == known
      && all (maybe False (not . null . fst) . candidateExecutionSources) fresh)
    (fail "known exact graph inventory changed the complete candidate capabilities")
  let owners = Set.fromList (map candidateOriginalIdentity fresh)
      accepted = readModuleCandidates candidatePath >>= either fail (\candidates ->
        unless (Set.fromList (map candidateOriginalIdentity candidates) == owners)
          (fail "restored graph lost original candidates"))
      refused label action = action >>= \result -> case result of
        Left _ -> pure ()
        Right _ -> fail ("descriptor transport admitted " ++ label)
      changed fields' = toStrictByteString (encodeTerm (TList fields'))
      withOffer fields' action = (BS.writeFile candidatePath (changed fields') >> action)
        `finally` BS.writeFile candidatePath bytes
      withParcel value action = withOffer (replace 5 value fields) action
      refuseParcel label value = withParcel value (refused label (readModuleCandidates candidatePath))
      descriptor sha path = TList [TString sha,TString (T.pack path)]
      references = parcel !! 1
  forM_ ["8","9"] $ \version -> withOffer (replace 1 (TString version) fields)
    (refused "legacy candidate wire" (readModuleCandidates candidatePath))
  refuseParcel "inline graph bytes" (TList [TList [TList [TString graphSha,TBytes graphBytes]],references])
  refuseParcel "duplicate graph digest" (TList [TList (descriptors ++ descriptors),references])
  refuseParcel "missing promised graph" (TList [TList [],references])
  refs <- values references
  firstRef <- row 6 (head refs)
  refuseParcel "substituted native owner" (TList [head parcel,TList
    (TList (replace 1 (TString "Absent.Native.Owner") firstRef) : tail refs)])
  refuseParcel "substituted graph reference" (TList [head parcel,TList
    (TList (replace 5 (TString (T.replicate 64 "e")) firstRef) : tail refs)])
  withOffer (replace 6 (TString (T.replicate 64 "f")) fields)
    (refused "substituted canonical compiler producer" (readModuleCandidates candidatePath))
  let missing = graphPath ++ ".missing"
  (renameFile graphPath missing >> do
      refused "missing graph" (readModuleCandidates candidatePath)
      refused "missing graph hidden by exact inventory"
        (readModuleCandidatesWithGraphs (scopeExecutionGraphs scope) candidatePath))
    `finally` renameFile missing graphPath
  accepted
  -- Length-preserving corruption must still fail when a decoded exact graph is known.
  let damaged = BS.cons (BS.head graphBytes `xor` 1) (BS.tail graphBytes)
  (BS.writeFile graphPath damaged >> do
      refused "graph digest mismatch" (readModuleCandidates candidatePath)
      refused "known graph digest mismatch"
        (readModuleCandidatesWithGraphs (scopeExecutionGraphs scope) candidatePath))
    `finally` BS.writeFile graphPath graphBytes
  accepted
  (BS.appendFile graphPath "x" >> do
      refused "growing graph" (readModuleCandidates candidatePath)
      refused "known graph growth"
        (readModuleCandidatesWithGraphs (scopeExecutionGraphs scope) candidatePath))
    `finally` BS.writeFile graphPath graphBytes
  accepted
  let root = takeDirectory candidatePath
      outside = takeDirectory scopePath </> "outside-candidate-graph.cbor"
  (copyFile graphPath outside >>
      refuseParcel "graph outside request root" (TList [TList [descriptor graphSha outside],references]))
    `finally` removeFile outside
  -- Sparse files prove aggregate rejection before capture; no 64 MiB buffers.
  let largeA = root </> "aggregate-a.cbor"
      largeB = root </> "aggregate-b.cbor"
      aggregate = TList [TList [descriptor (T.replicate 64 "e") largeA,
        descriptor (T.replicate 64 "f") largeB],references]
      aggregateRead inventory = withParcel aggregate $ do
        result <- readModuleCandidatesWithGraphs inventory candidatePath
        case result of
          Left reason | "retained byte bound" `isInfixOf` reason -> pure ()
          _ -> fail "aggregate graph budget did not reject before hashing or decoding"
  (do
      forM_ [largeA,largeB] $ \path -> withBinaryFile path WriteMode $ \handle ->
        hSetFileSize handle (32*1024*1024+1)
      aggregateRead []
      aggregateRead (scopeExecutionGraphs scope))
    `finally` mapM_ removeFile [largeA,largeB]
  let many = TList [descriptor (T.pack (replicate (64-length sha) '0' ++ sha)) graphPath
        | ordinal <- [0 :: Int .. 4096], let sha = showHex ordinal ""]
  refuseParcel "4097 graph descriptors" (TList [many,references])
  (BS.writeFile candidatePath (BS.replicate (4*1024*1024+1) 0) >>
      refused "metadata above four MiB" (readModuleCandidates candidatePath))
    `finally` BS.writeFile candidatePath bytes
  accepted
  unlessM ((== bytes) <$> BS.readFile candidatePath) "descriptor checks changed canonical offer bytes"
  putStrLn "candidate graph transport: current genuine offer, known graph authentication, bounds, refusals and recovery passed"

replace :: Int -> a -> [a] -> [a]
replace index replacement values' = take index values' ++ replacement : drop (index+1) values'

row :: Int -> Term -> IO [Term]
row size (TList fields) | length fields == size = pure fields
row _ _ = fail "genuine descriptor fixture has another typed row shape"

values :: Term -> IO [Term]
values (TList fields) = pure fields
values _ = fail "genuine descriptor fixture has another inventory shape"

decode :: BS.ByteString -> IO Term
decode bytes = case deserialiseFromBytes decodeTerm (BL.fromStrict bytes) of
  Right (rest,term) | BL.null rest -> pure term
  _ -> fail "genuine descriptor fixture has invalid CBOR"

unlessM :: IO Bool -> String -> IO ()
unlessM check reason = check >>= \valid -> unless valid (fail reason)
