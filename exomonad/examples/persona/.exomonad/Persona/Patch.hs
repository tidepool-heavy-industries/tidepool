{-# LANGUAGE DeriveGeneric #-}
{-# LANGUAGE FlexibleContexts #-}
{-# LANGUAGE OverloadedStrings #-}
-- | Small literal, sequential, single-file patches. No fuzzy matching.
module Persona.Patch (Edit(..), PatchError(..), Prepared, applyEdits, prepare, preview, commit, commitReceipt, preparedReference, checks) where
import Control.Monad (foldM)
import Control.Monad.Freer (Eff, Member)
import Data.Text (Text)
import qualified Data.Text as T
import Tidepool.Effects.Authored (FsRead, FsWrite)
import Tidepool.Records (FsError, WriteOutcome)
import GHC.Generics (Generic)
import Tidepool.Aeson (ToJSON, FromJSON)
import Tidepool.Aeson.Schema (JsonSchema)
import qualified Tidepool.File as File

data Edit = Replace { oldText :: Text, newText :: Text } deriving (Eq, Show, Generic)
instance ToJSON Edit
instance FromJSON Edit
instance JsonSchema Edit
data PatchError = EmptyPatch | EmptyAnchor Int | MissingAnchor Int | AmbiguousAnchor Int | ReadFailed FsError | VersionMismatch File.FileReference File.FileReference deriving (Eq, Show, Generic)
instance ToJSON PatchError
instance JsonSchema PatchError
-- Constructor hidden: committing always uses the original captured snapshot.
data Prepared = Prepared File.FileSnapshot Text [Edit]

-- Edits match the result of preceding edits. Indexes in failures are 1-based.
-- Check overlapping occurrences too: 'aa' in 'aaa' is ambiguous.
applyEdits :: [Edit] -> Text -> Either PatchError Text
applyEdits [] _ = Left EmptyPatch
applyEdits edits original = foldM step original (zip [1..] edits)
  where
    step body (n, Replace old new)
      | T.null old = Left (EmptyAnchor n)
      | T.null suffix = Left (MissingAnchor n)
      | old `T.isInfixOf` T.drop 1 suffix = Left (AmbiguousAnchor n)
      | otherwise = Right (prefix <> new <> T.drop (T.length old) suffix)
      where (prefix, suffix) = T.breakOn old body

prepare :: Member FsRead effects => File.FileReference -> [Edit] -> Eff effects (Either PatchError Prepared)
prepare expected edits = do
  result <- File.readSnapshot (File.referencePath expected)
  pure $ case result of
    Left err -> Left (ReadFailed err)
    Right snapshot | File.snapshotReference snapshot /= expected -> Left (VersionMismatch expected (File.snapshotReference snapshot))
    Right snapshot -> do
      body <- applyEdits edits (File.snapshotText snapshot)
      pure (Prepared snapshot body edits)

-- Exact before/after hunks, not a unified diff; full proposed contents retained.
preview :: Prepared -> Text
preview (Prepared _ _ edits) = T.intercalate "\n" (zipWith render [1 :: Int ..] edits)
  where
    render n (Replace old new) = "Edit " <> T.pack (show n) <> "\n"
      <> T.unlines (map ("- " <>) (T.splitOn "\n" old))
      <> T.unlines (map ("+ " <>) (T.splitOn "\n" new))

-- A conflict is returned, never retried against fresh contents.
-- All hunks are prepared before this single conditional write.
commit :: Member FsWrite effects => Prepared -> Eff effects (Either FsError WriteOutcome)
commit (Prepared snapshot body _) = File.writeSnapshot snapshot body

commitReceipt :: Member FsWrite effects => Prepared -> Eff effects (Either FsError File.CommitOutcome)
commitReceipt (Prepared snapshot body _) = File.writeSnapshotReceipt snapshot body

preparedReference :: Prepared -> File.FileReference
preparedReference (Prepared snapshot _ _) = File.snapshotReference snapshot

checks :: [(Text, Bool)]
checks =
  [ ("replacement", ok [Replace "cat" "dog"] "a cat!" "a dog!")
  , ("insertion via anchor", ok [Replace "cat" "small cat"] "cat" "small cat")
  , ("deletion", ok [Replace " cat" ""] "a cat" "a")
  , ("sequential", ok [Replace "cat" "dog", Replace "dog" "owl"] "cat" "owl")
  , ("missing", case applyEdits [Replace "x" "y"] "cat" of Left (MissingAnchor 1) -> True; _ -> False)
  , ("ambiguous", case applyEdits [Replace "cat" "x"] "cat cat" of Left (AmbiguousAnchor 1) -> True; _ -> False)
  , ("overlapping", case applyEdits [Replace "aa" "x"] "aaa" of Left (AmbiguousAnchor 1) -> True; _ -> False)
  , ("empty anchor", case applyEdits [Replace "" "x"] "cat" of Left (EmptyAnchor 1) -> True; _ -> False)
  , ("empty patch", case applyEdits [] "cat" of Left EmptyPatch -> True; _ -> False)
  , ("later failure", case applyEdits [Replace "cat" "dog", Replace "owl" "x"] "cat" of Left (MissingAnchor 2) -> True; _ -> False)
  , ("unicode and newline", ok [Replace "α\nβ" "γ\n"] "α\nβ" "γ\n")
  ]
  where ok edits input expected = case applyEdits edits input of Right actual -> actual == expected; _ -> False
