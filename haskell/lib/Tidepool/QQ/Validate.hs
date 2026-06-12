{-# LANGUAGE TemplateHaskellQuotes #-}
{-# LANGUAGE OverloadedStrings #-}

-- | Validator quasi-quoters: @[sg|...|]@ and @[uri|...|]@.
--
-- Each is a thin wrapper around 'mkValidatorQQ': a compile-time CHECK over the
-- quote body, and on success an emitted 'Data.Text.Text' literal — exactly the
-- bytes you wrote.  The point is to move a class of silent runtime traps
-- (ast-grep's @$$NAME@ no-match, a scheme-less URI) to a precise COMPILE
-- error, while still producing an ordinary 'Text' the JIT runs as a plain
-- string literal.
--
-- The checkers run inside the splice evaluator (full GHC available), so they
-- use 'Data.Char' classy predicates freely — nothing here is translated to
-- Core.  Only the emitted @'Data.Text.pack' "…"@ ever reaches the JIT.
--
-- == What each checker enforces
--
--   * @sg@  — the ast-grep /metavariable layer/ only (patterns are
--     language-specific; the rest is accepted): @$NAME@ is a single capture,
--     @$$$NAME@ a multi capture, both requiring an UPPERCASE name.  @$$NAME@
--     (the documented silent no-match trap) and @$lowercase@ are rejected, as
--     are unbalanced @()[]{}@ brackets (string literals are skipped so a
--     bracket inside @\"…\"@ never trips the balance).
--   * @uri@ — must start @http:\/\/@ or @https:\/\/@, have a non-empty host,
--     and contain no whitespace.  Structural only.
--
-- A @[glob|...|]@ quoter was deliberately OMITTED from v1: the name collides
-- with the eval-visible @glob :: Text -> M [Text]@ Fs verb (an ambiguous
-- occurrence in every quoter-using eval), and glob validation is marginal
-- (almost any string is a valid glob). If demand appears, revisit it under a
-- non-colliding name such as @globp@.
module Tidepool.QQ.Validate
  ( sg
  , uri
  , mkValidatorQQ
  ) where

import Data.Char (isAlphaNum, isLower, isUpper)
import Data.Text (Text)
import qualified Data.Text as T

import Language.Haskell.TH (Exp, Q, litE, stringL)
import Language.Haskell.TH.Quote (QuasiQuoter (..))

-- | Build a validator quasi-quoter from a name (used in error messages) and a
-- compile-time check.  On 'Right' the quote body is emitted as a 'Text'
-- literal; on 'Left' the splice fails with the quoter name and the message.
-- Pattern\/type\/declaration positions are rejected with a pointer to
-- expression position.
mkValidatorQQ :: String -> (Text -> Either Text ()) -> QuasiQuoter
mkValidatorQQ name check = QuasiQuoter
  { quoteExp  = checkExp
  , quotePat  = \_ -> fail (name ++ ": cannot be used in pattern position (it builds a Text literal; use it in expression position)")
  , quoteType = \_ -> fail (name ++ ": cannot be used in a type context")
  , quoteDec  = \_ -> fail (name ++ ": cannot be used in a declaration context")
  }
  where
    checkExp :: String -> Q Exp
    checkExp s = case check (T.pack s) of
      Right ()  -> [| T.pack $(litE (stringL s)) |]
      Left msg  -> fail (name ++ ": " ++ T.unpack msg)

-- | @[sg| fn $NAME |]@ — an ast-grep pattern, metavariable layer checked.
sg :: QuasiQuoter
sg = mkValidatorQQ "[sg|…|]" sgCheck

-- | @[uri| https://example.com/x |]@ — an http(s) URI, structure checked.
uri :: QuasiQuoter
uri = mkValidatorQQ "[uri|…|]" uriCheck

-- ---------------------------------------------------------------------------
-- ast-grep metavariable + bracket check
-- ---------------------------------------------------------------------------

-- | Single-pass scan: track an open-bracket stack, skip string literals
-- (double and single quoted, so brackets inside them never affect balance),
-- and check every @$@-run against the metavariable grammar.
sgCheck :: Text -> Either Text ()
sgCheck t = scan (T.unpack t) []
  where
    scan :: String -> [Char] -> Either Text ()
    scan [] stack
      | null stack = Right ()
      | otherwise  = Left "unbalanced brackets: missing closing bracket"
    scan ('"' : cs) stack  = skipStr '"' cs stack
    scan ('\'' : cs) stack = skipStr '\'' cs stack
    scan all_@('$' : _) stack =
      let (dollars, rest) = span (== '$') all_
      in checkMeta (length dollars) rest stack
    scan (c : cs) stack
      | c == '(' || c == '[' || c == '{' = scan cs (c : stack)
      | c == ')' = popBracket '(' c cs stack
      | c == ']' = popBracket '[' c cs stack
      | c == '}' = popBracket '{' c cs stack
      | otherwise = scan cs stack

    -- After a run of k '$' characters, classify the metavariable.
    checkMeta :: Int -> String -> [Char] -> Either Text ()
    checkMeta k rest stack
      | k == 2 = Left "ast-grep metavariable '$$' is invalid; did you mean $$$NAME (multi) or $NAME (single)?"
      | k >= 4 = Left "too many '$' in an ast-grep metavariable; use $NAME (single) or $$$NAME (multi)"
      | otherwise = case rest of           -- k == 1 (single) or k == 3 (multi)
          (d : _)
            | isLower d ->
                Left ("ast-grep metavariable must be UPPERCASE, got lowercase '"
                      <> T.pack [d] <> "'; use $NAME or $$$NAME with an UPPERCASE name")
            | isMetaStart d -> scan (dropWhile isMetaTail rest) stack
            | otherwise     -> scan rest stack   -- a bare '$' (or '$$$') not starting a name: a literal '$'
          [] -> Right ()                         -- trailing '$' run, no name: treat as literal

    isMetaStart d = isUpper d || d == '_'
    isMetaTail  d = isAlphaNum d || d == '_'

    popBracket :: Char -> Char -> String -> [Char] -> Either Text ()
    popBracket opener closer cs stack = case stack of
      (o : os) | o == opener -> scan cs os
      _ -> Left ("unbalanced bracket '" <> T.pack [closer] <> "'")

    -- Skip a string literal.  If it never closes, treat the opening quote as a
    -- literal char and resume (never reject on a stray quote — patterns are
    -- language-specific and may use ' as a Rust lifetime, etc.).
    skipStr :: Char -> String -> [Char] -> Either Text ()
    skipStr delim afterOpen stack = case closeAt afterOpen of
      Just rest -> scan rest stack
      Nothing   -> scan afterOpen stack
      where
        closeAt [] = Nothing
        closeAt ('\\' : _ : xs) = closeAt xs
        closeAt (x : xs)
          | x == delim = Just xs
          | otherwise  = closeAt xs

-- ---------------------------------------------------------------------------
-- URI check
-- ---------------------------------------------------------------------------

uriCheck :: Text -> Either Text ()
uriCheck t
  | T.any isWs t = Left "URI must not contain whitespace"
  | Just rest <- T.stripPrefix "http://" t  = checkHost rest
  | Just rest <- T.stripPrefix "https://" t = checkHost rest
  | otherwise = Left "URI must start with 'http://' or 'https://'"
  where
    isWs c = c == ' ' || c == '\t' || c == '\n' || c == '\r'
    checkHost rest =
      let host = T.takeWhile (\c -> c /= '/' && c /= '?' && c /= '#') rest
      in if T.null host then Left "URI has an empty host" else Right ()
