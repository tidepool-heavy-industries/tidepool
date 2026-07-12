{-# LANGUAGE NoImplicitPrelude, OverloadedStrings, OverloadedRecordDot #-}
module RustSections where

import Tidepool.Prelude hiding (error)
import Tidepool.Effects

import qualified Data.Text as T

-- | Split a Rust source file into its top-level `pub fn` sections.
-- Key = fn-name prefix before the first underscore; value = body lines
-- until the next `pub fn`. Complements RustAudit (panic triage) with
-- structural slicing for pretty-printed macro/builder files.
sections :: Text -> [(Text, [Text])]
sections body = go (T.lines body)
  where
    go [] = []
    go (l : ls) = case T.stripPrefix "pub fn " l of
      Just rest ->
        let ident = T.takeWhile (\c -> c /= '(' && c /= ' ' && c /= '<') rest
            nm = T.takeWhile (/= '_') ident
            (sec, more) = span (not . T.isPrefixOf "pub fn ") ls
        in (nm, sec) : go more
      Nothing -> go ls

-- | Count entries of a named `field: &[ ... ]` array inside section lines:
-- nonempty, non-comment lines between the opener and the closing `],`/`]`.
arrayLen :: Text -> [Text] -> Int
arrayLen field ls = length (filter entry block)
  where
    opener = field <> ": &["
    afterOpen = drop 1 (dropWhile (not . T.isInfixOf opener) ls)
    block = takeWhile (\l -> let s = T.strip l in s /= "]," && s /= "]") afterOpen
    entry l = let s = T.strip l in not (T.null s) && not ("//" `T.isPrefixOf` s)
