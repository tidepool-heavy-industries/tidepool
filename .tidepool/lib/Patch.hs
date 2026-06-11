{-# LANGUAGE NoImplicitPrelude, OverloadedStrings #-}
-- | Exact-text file surgery with exactly-once semantics — the python
-- heredoc replacement. Big needles ride the eval `input` field (JSON),
-- so nothing needs escaping in code: `patchJ input` with
-- input = {file, old, new}.
module Patch where

import Tidepool.Prelude hiding (error)
import Tidepool.Effects
import qualified Data.Text as T

-- | #313 LANDMINE (removed from use): a pure module-level Text helper of
-- this shape miscompiles via the join-wiring bug — calls case-trap at
-- runtime. Kept commented as a reminder until #313's emit fix lands;
-- the checks live inlined in patchFile instead.
-- occurrences :: Text -> Text -> Int  — see git history

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

-- | Compute-check-commit: write content only if every named check holds.
-- The caller computes the checks against the CANDIDATE content (pure),
-- so the failure report names exactly what blocked the write — no
-- half-written files, no silent clobbering.
--
--   let new = render thing
--   writeChecked "out.md" [ ("nonempty", not (isNull new))
--                         , ("has header", "# " `isPrefixOf` new) ] new
writeChecked :: Text -> [(Text, Bool)] -> Text -> M Value
writeChecked path checks content = do
  let failed = [name | (name, ok) <- checks, not ok]
  if null failed
    then do
      writeFile path content
      pure (object [ "file" .= path, "written" .= True
                   , "checks" .= length checks ])
    else pure (object [ "file" .= path, "written" .= False
                      , "failed" .= failed ])
