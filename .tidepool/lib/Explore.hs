{-# LANGUAGE NoImplicitPrelude, OverloadedStrings, OverloadedRecordDot #-}
-- | Task-shaped exploration verbs: pure result-shapers for the
-- effect ops (glob, grepGlob, sg finds, readFile). Effectful
-- plumbing stays at the call site; these shape the answers.
module Explore where

import Tidepool.Prelude hiding (error)
import Tidepool.Effects

-- | Histogram of file extensions from a path listing.
extHisto :: [Text] -> [(Text, Int)]
extHisto paths = sortBy (\a b -> compare (snd b) (snd a)) (foldl' bump [] paths)
  where
    ext p = case splitOn "." p of { [_] -> "(none)"; ps -> last ps }
    bump acc p = ins (ext p) acc
    ins k [] = [(k, 1)]
    ins k ((k', n) : rest) = if k == k' then (k', n + 1) : rest else (k', n) : ins k rest

-- | Top-N heaviest entries from (path, size) pairs.
sizeRank :: Int -> [(Text, Int)] -> [(Text, Int)]
sizeRank n = take n . sortBy (\a b -> compare (snd b) (snd a))

-- | Group grep hits into per-file counts, densest first.
hitsByFile :: [Hit] -> [(Text, Int)]
hitsByFile hs = sortBy (\a b -> compare (snd b) (snd a)) (foldl' bump [] hs)
  where
    bump acc h = ins h.path acc
    ins k [] = [(k, 1)]
    ins k ((k', n) : rest) = if k == k' then (k', n + 1) : rest else (k', n) : ins k rest

-- | Slice a window of numbered lines around a target line (1-based).
aroundLine :: Int -> Int -> Text -> [Text]
aroundLine target radius content =
  let ls = zipWithIndex (lines content)
      lo = target - radius
      hi = target + radius
      num (i, l) = pack (show (i + 1)) <> "| " <> l
  in map num (filter (\(i, _) -> i + 1 >= lo && i + 1 <= hi) ls)

-- | Scaffold a correctly-headered .tidepool/lib module: returns
-- (path, contents) for writeFile. Avoids the implicit-Prelude trap.
defMod :: Text -> Text -> (Text, Text)
defMod name body =
  ( ".tidepool/lib/" <> name <> ".hs"
  , unlines
      [ "{-# LANGUAGE NoImplicitPrelude, OverloadedStrings #-}"
      , "module " <> name <> " where"
      , ""
      , "import Tidepool.Prelude"
      , ""
      , body
      ]
  )

-- ===== Effectful verbs (Tidepool.Effects is importable now) =====

-- | Find a Rust function and show it with surrounding context. One call.
defWithContext :: Text -> [Text] -> M [Text]
defWithContext name roots = do
  ms <- rsFn name roots
  case ms of
    (Match _ f l _ _ : _) -> do
      content <- readFile f
      pure ((f <> ":" <> pack (show l)) : aroundLine l 4 content)
    [] -> pure []

-- | Per-file reference counts for a regex, densest first.
refs :: Text -> Text -> M [(Text, Int)]
refs pat g = hitsByFile <$> grepGlob pat g

-- | One-call codebase overview for a glob: count, extensions, heaviest.
census :: Text -> M Value
census pat = do
  ps <- glob pat
  sized <- mapM (\p -> do { s <- getFileSize p; pure (p, fromMaybe 0 s) }) ps
  pure (object ["files" .= len ps, "exts" .= take 5 (extHisto ps), "heaviest" .= sizeRank 5 sized])
