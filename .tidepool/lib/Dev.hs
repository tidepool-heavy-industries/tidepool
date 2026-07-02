{-# LANGUAGE NoImplicitPrelude, OverloadedStrings, OverloadedRecordDot #-}
-- | The dev inner loop as verbs: run-and-fail-loudly, file slicing,
-- in-file grep, build diagnostics, session memoization.
module Dev where

import Tidepool.Prelude hiding (error)
import Tidepool.Effects

-- | Run a command; return stdout. Non-zero exit = loud error with stderr.
sh :: Text -> M Text
sh cmd = do
  p <- run cmd
  if ok p
    then pure p.stdout
    else error ("sh: exit " <> pack (show p.exitCode) <> ": " <> cmd <> "\n" <> p.stderr)

-- | sh split into lines; inherits sh's loud-error on non-zero exit.
shLines :: Text -> M [Text]
shLines cmd = lines <$> sh cmd

-- | Run a command; return the full Proc record (exitCode, stdout, stderr).
-- Escape hatch when the caller needs the exit code or raw stderr without erroring.
shProc :: Text -> M Proc
shProc = run

-- | grep -rn equivalent: search all files under a directory for a regex.
-- @grepIn pat dir@ — content regex FIRST, directory path SECOND (same order as grepGlob).
-- Returns "path:line| text" lines. Searches recursively via dir\/\*\*.
-- Example: grepIn "unresolved variable" "tidepool-codegen\/src"
grepIn :: Text -> Text -> M [Text]
grepIn pat dir = do
  hits <- grepGlob pat (dir <> "/**")
  pure (map (\h -> h.path <> ":" <> pack (show h.line) <> "| " <> strip h.text) hits)

-- | sed -n 'lo,hi p' equivalent with line numbers.
slice :: Text -> Int -> Int -> M [Text]
slice f lo hi = do
  content <- readFile f
  let numbered = map (\(i, l) -> pack (show (i + 1)) <> "| " <> l) (zipWithIndex (lines content))
  pure (take (hi - lo + 1) (drop (lo - 1) numbered))

-- | cargo check, returning only diagnostic header lines (errors/warnings).
-- Filtering happens SHELL-SIDE: Haskell-side filter/map over lines of a
-- partially-consumed effect tuple miscompiles in lib modules ("undefined
-- forced" — open JIT bug; minimal repro preserved in Probe.hs t1-t8,
-- inline equivalents work). shLines consumes all fields, like gitS.
cargoCheck :: M [Text]
cargoCheck = shLines "cargo check --workspace 2>&1 | grep -E '^(error|warning)' || true"

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
