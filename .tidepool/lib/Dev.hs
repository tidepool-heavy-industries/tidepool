{-# LANGUAGE NoImplicitPrelude, OverloadedStrings, OverloadedRecordDot #-}
-- | The dev inner loop as verbs: run-and-fail-loudly, file slicing,
-- in-file grep, build diagnostics, session memoization.
module Dev where

import Tidepool.Prelude hiding (error)
import Tidepool.Effects
import Tidepool.Shell (sh)
import qualified Tidepool.Data.Text as T

-- | sh split into lines; inherits sh's loud-error on non-zero exit.
shLines :: Text -> M [Text]
shLines cmd = lines <$> sh cmd

-- | Run a command and return the full Proc record (exitCode, stdout, stderr) without erroring on failure.
shProc :: Text -> M Proc
shProc cmd = run cmd >>= liftEither

-- | grep -rn equivalent: search for a regex, formatting hits as "path:line| text".
-- @grepIn pat glob@ — content regex FIRST, glob pattern SECOND (same order as grepGlob).
-- The second argument is a real glob pattern passed straight to grepGlob, NOT a
-- bare directory — grepGlob itself already recurses a directory (@dir\/**\/*@) and
-- matches a literal file path as-is, so no "\/\*\*" is appended here.
-- Examples: grepIn "unresolved variable" "tidepool-codegen\/src"  (directory, recurses)
--           grepIn "unresolved variable" "tidepool-codegen\/src\/effect_machine.rs"  (single file)
--           grepIn "unresolved variable" "**\/*.rs"  (explicit recursive glob)
grepIn :: Text -> Text -> M [Text]
grepIn pat g = do
  hits <- grepGlob pat g >>= liftEither
  pure (map (\h -> h.path <> ":" <> pack (show h.line) <> "| " <> strip h.text) hits)

-- | sed -n 'lo,hi p' equivalent with line numbers.
slice :: Text -> Int -> Int -> M [Text]
slice f lo hi = do
  content <- readFile f >>= liftEither
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
  mods <- glob ".tidepool/lib/*.hs" >>= liftEither
  sigLists <- mapM sigsOf mods
  pure (concat sigLists)
  where
    sigsOf m = do
      src <- readFile m >>= liftEither
      let name = replace ".hs" "" (fromMaybe m (lastMay (splitOn "/" m)))
      let topSig l = " :: " `isInfixOf` l && not (" " `isPrefixOf` l) && not ("--" `isPrefixOf` l)
      pure (map (\s -> name <> "." <> s) (filter topSig (lines src)))

-- | Look up a GHC error code (e.g. "GHC-39999") on errors.haskell.org and
-- return its one-line title. Born from the error-plane work: every GHC
-- diagnostic carries a [GHC-nnnnn] code; this fetches what it means.
explainGhc :: Text -> M Text
explainGhc code = do
  r <- httpGet ("https://errors.haskell.org/messages/" <> code <> "/")
  pure (case r of
    Left e -> "fetch failed: " <> pack (show e)
    Right (String html) -> T.strip (between "<title>" "</title>" html)
    Right _ -> "unexpected non-text body")
  where
    between a b t = fst (T.breakOn b (T.drop (T.length a) (snd (T.breakOn a t))))
