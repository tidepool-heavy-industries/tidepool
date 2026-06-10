{-# LANGUAGE NoImplicitPrelude, OverloadedStrings #-}
-- | The dev inner loop as verbs: run-and-fail-loudly, file slicing,
-- in-file grep, build diagnostics, session memoization.
module Dev where

import Tidepool.Prelude hiding (error)
import Tidepool.Effects

-- | Run a command; return stdout. Non-zero exit = loud error with stderr.
sh :: Text -> M Text
sh cmd = do
  (code, out, err) <- run cmd
  if code == 0
    then pure out
    else error ("sh: exit " <> pack (show code) <> ": " <> cmd <> "\n" <> err)

-- | sh, split into lines.
shLines :: Text -> M [Text]
shLines cmd = lines <$> sh cmd

-- | grep -n equivalent: regex hits in files matching a glob, "line| text".
grepIn :: Text -> Text -> M [Text]
grepIn pat g = do
  hits <- grepGlob pat g
  pure (map (\(f, l, t) -> f <> ":" <> pack (show l) <> "| " <> strip t) hits)

-- | sed -n 'lo,hi p' equivalent with line numbers.
slice :: Text -> Int -> Int -> M [Text]
slice f lo hi = do
  content <- readFile f
  let numbered = map (\(i, l) -> pack (show (i + 1)) <> "| " <> l) (zipWithIndex (lines content))
  pure (take (hi - lo + 1) (drop (lo - 1) numbered))

-- | cargo check, returning only diagnostic header lines (errors/warnings).
-- Exit code intentionally ignored: diagnostics ARE the result.
cargoCheck :: M [Text]
cargoCheck = do
  (_, _, err) <- run "cargo check --workspace"
  pure (filter (\l -> "error" `isPrefixOf` l || "warning" `isPrefixOf` l) (lines err))

-- | git status --short, as lines.
gitS :: M [Text]
gitS = shLines "git status --short"

-- | The library's own vocabulary: top-level signatures from every
-- .tidepool/lib module. Discoverability for future sessions.
vocab :: M [Text]
vocab = do
  mods <- glob ".tidepool/lib/*.hs"
  sigLists <- mapM sigsOf mods
  pure (concat sigLists)
  where
    sigsOf m = do
      src <- readFile m
      let name = replace ".hs" "" (last (splitOn "/" m))
      let topSig l = " :: " `isInfixOf` l && not (" " `isPrefixOf` l) && not ("--" `isPrefixOf` l)
      pure (map (\s -> name <> "." <> s) (filter topSig (lines src)))
