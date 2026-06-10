{-# LANGUAGE NoImplicitPrelude, OverloadedStrings #-}
-- | Exact-text file surgery with exactly-once semantics — the python
-- heredoc replacement. Big needles ride the eval `input` field (JSON),
-- so nothing needs escaping in code: `patchJ input` with
-- input = {file, old, new}.
module Patch where

import Tidepool.Prelude hiding (error)
import Tidepool.Effects
import qualified Data.Text as T

-- | 0, 1, or "more than one" (2) — two breakOn probes, no list build.
occurrences :: Text -> Text -> Int
occurrences needle hay =
  let (_, rest) = T.breakOn needle hay
  in if isNull rest
       then 0
       else let (_, rest2) = T.breakOn needle (sdrop (len needle) rest)
            in if isNull rest2 then 1 else 2

-- | Replace a needle EXACTLY ONCE: errors loudly if absent or ambiguous
-- (ambiguity is how string surgery corrupts files silently).
-- NOTE: occurrence checks are inlined in the do-block rather than via the
-- pure `occurrences` helper — #313 case-traps cross-module PURE Text fns
-- (Probe.occ2 is the minimal repro) while M-action-inline code is fine.
patchFile :: Text -> Text -> Text -> M Text
patchFile path old new = do
  src <- readFile path
  let (_, r1) = T.breakOn old src
  if isNull r1
    then error ("patchFile: needle not found in " <> path)
    else do
      let (_, r2) = T.breakOn old (sdrop (len old) r1)
      if not (isNull r2)
        then error ("patchFile: needle ambiguous in " <> path)
        else do
          writeFile path (replace old new src)
          pure ("patched " <> path)

-- | JSON-driven patch: expects {file, old, new} — pass via eval input.
patchJ :: Value -> M Text
patchJ v = case (txtAt "file", txtAt "old", txtAt "new") of
  (Just f, Just o, Just n) -> patchFile f o n
  _ -> error "patchJ: need {file, old, new} strings in input"
  where
    txtAt k = case v ?. k of
      Just x -> asText x
      Nothing -> Nothing

-- | Insert a block after the line containing the (unique) anchor.
insertAfter :: Text -> Text -> Text -> M Text
insertAfter path anchor block = do
  src <- readFile path
  let ls = lines src
  let hits = len (filter (\l -> anchor `isInfixOf` l) ls)
  if hits /= 1
    then error ("insertAfter: anchor matched " <> pack (show hits) <> " lines in " <> path)
    else do
      let go [] = []
          go (l : rest) = if anchor `isInfixOf` l then l : block : rest else l : go rest
      writeFile path (unlines (go ls))
      pure ("inserted into " <> path)
