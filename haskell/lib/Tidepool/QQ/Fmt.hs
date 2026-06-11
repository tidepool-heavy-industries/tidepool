{-# LANGUAGE TemplateHaskellQuotes #-}
-- | The @[fmt|...|]@ quasi-quoter: Text interpolation.
--
-- == Grammar
--
-- @
-- [fmt| literal text with {antiquote} holes |]
-- @
--
-- The result type is @Data.Text.Text@.  Literal segments are wrapped with
-- 'Data.Text.pack'; multiple segments are assembled with 'Data.Text.concat'.
-- An empty quote (@[fmt||]@) expands to @Data.Text.pack \"\"@.  A single
-- non-empty literal segment skips the @concat@ wrapper.
--
-- == Antiquote expressions
--
-- Inside @{...}@ the mini-grammar is:
--
-- @
-- expr := atom atom*
-- atom := qualified-varid | varid | integer | \'(\' expr \')\'
-- @
--
-- Juxtaposition is left-nested function application ('AppE').
-- A @varid@ matches @[a-z_][A-Za-z0-9_\']*@; a @qualified-varid@ is one or
-- more @Conid.@ prefixes followed by a varid (e.g. @T.strip@,
-- @Data.Text.toUpper@).  Names are resolved at the SPLICE SITE via
-- 'Language.Haskell.TH.mkName', so the imports in scope at the call site
-- apply.
--
-- Antiquoted sub-expressions must be 'Data.Text.Text'-typed at the splice
-- site.  The quoter performs no implicit conversion.  In the tidepool eval
-- dialect @show@ returns @Text@, so @{show x}@ is valid.
--
-- Anything outside the grammar (operators, lambdas, string literals, record
-- syntax, empty holes) causes a compile-time 'fail'; factor those into a
-- @where@-bound helper and reference the helper name from the hole.
--
-- == Escapes in literal text
--
-- * @\\\{@ → literal @{@
-- * @\\\\@ → literal @\\@
-- * @\\c@ for any other @c@: passes through as @\\c@ unchanged.
-- * A bare @}@ outside a hole is a literal @}@.
-- * An unclosed @{@ (no matching @}@ before end of input) → compile-time
--   error.
--
-- == Multi-line quotes
--
-- Newlines in the body pass through verbatim; multi-line @[fmt|...|]@ quotes
-- are legal.
--
-- == Limitation
--
-- @|]@ cannot appear inside the literal body — GHC terminates the
-- quasi-quote at the first occurrence of that token.
module Tidepool.QQ.Fmt (fmt) where

import Language.Haskell.TH        (Exp (..), Lit (..), Q, mkName)
import Language.Haskell.TH.Quote  (QuasiQuoter (..))
import Data.Char                   (isAlpha, isAlphaNum, isDigit, isSpace)
import qualified Data.Text as T

-- | @[fmt|...|]@: quasi-quoted 'Data.Text.Text' interpolation.
--
-- See the module haddock for grammar, escape rules, and examples.
fmt :: QuasiQuoter
fmt = QuasiQuoter
  { quoteExp  = fmtExp
  , quotePat  = \_ -> fail "fmt: cannot be used in pattern position"
  , quoteType = \_ -> fail "fmt: cannot be used in type position"
  , quoteDec  = \_ -> fail "fmt: cannot be used in declaration position"
  }

------------------------------------------------------------------------
-- Segment type
------------------------------------------------------------------------

-- | A single parsed segment of the format string.
data Segment
  = SLit  String  -- ^ Literal text, already escape-processed.
  | SHole String  -- ^ Antiquote content, whitespace-trimmed.

------------------------------------------------------------------------
-- Top-level expansion
------------------------------------------------------------------------

fmtExp :: String -> Q Exp
fmtExp src = parseSegments src >>= buildExp

buildExp :: [Segment] -> Q Exp
buildExp segs = do
    let merged   = mergeAdjacentLits segs
        nonempty = filter (not . emptyLit) merged
    parts <- mapM segToExp nonempty
    case parts of
      []  -> [| T.pack "" |]
      [e] -> return e
      es  -> [| T.concat $(return (ListE es)) |]
  where
    emptyLit (SLit "") = True
    emptyLit _         = False

segToExp :: Segment -> Q Exp
segToExp (SLit s)  = [| T.pack $(return (LitE (StringL s))) |]
segToExp (SHole h) = parseHole h

-- | Merge adjacent 'SLit' segments into a single one.
mergeAdjacentLits :: [Segment] -> [Segment]
mergeAdjacentLits (SLit a : SLit b : rest) =
    mergeAdjacentLits (SLit (a ++ b) : rest)
mergeAdjacentLits (x : xs) = x : mergeAdjacentLits xs
mergeAdjacentLits []        = []

------------------------------------------------------------------------
-- Segment lexer
------------------------------------------------------------------------

parseSegments :: String -> Q [Segment]
parseSegments []              = return []
parseSegments ('\\' : '{' : r)  = fmap (SLit "{" :)    (parseSegments r)
parseSegments ('\\' : '\\' : r) = fmap (SLit "\\" :)   (parseSegments r)
parseSegments ('\\' : c : r)    = fmap (SLit ['\\', c] :) (parseSegments r)
parseSegments ('{' : r)       = do
    (h, after) <- lexHole r
    rest <- parseSegments after
    return (SHole (trim h) : rest)
parseSegments (c : r)         = do
    rest <- parseSegments r
    return $ case rest of
      SLit s : more -> SLit (c : s) : more
      _             -> SLit [c] : rest

-- | Consume the content of a @{...}@ hole.
--   Returns @(hole-content, text-after-\'}\')@.
lexHole :: String -> Q (String, String)
lexHole s = case break (== '}') s of
    (_, [])  -> fail "fmt: unclosed '{' — no matching '}' before end of quote"
    (h, _:r) -> return (h, r)

------------------------------------------------------------------------
-- Antiquote parser
------------------------------------------------------------------------

parseHole :: String -> Q Exp
parseHole raw
  | null t    = fail "fmt: empty antiquote '{}' — provide an expression or a where-bound name"
  | otherwise = do
      (e, r) <- parseExpr t
      if null (trim r)
        then return e
        else fail ( "fmt: unexpected text in antiquote after expression: "
                 ++ show (trim r)
                 ++ " — factor operators or complex expressions into a where-bound helper" )
  where
    t = trim raw

-- | Parse an expression: one or more atoms assembled as left-nested 'AppE'.
parseExpr :: String -> Q (Exp, String)
parseExpr s = do
    (a, r) <- parseAtom s
    parseApps a (trim r)

-- | Accumulate further atoms as application arguments.
parseApps :: Exp -> String -> Q (Exp, String)
parseApps acc []     = return (acc, [])
parseApps acc s@(c : _)
  | couldStartAtom c = do
      (a, r) <- parseAtom s
      parseApps (AppE acc a) (trim r)
  | otherwise        = return (acc, s)

-- | True when @c@ could be the first character of an atom.
couldStartAtom :: Char -> Bool
couldStartAtom '(' = True
couldStartAtom c   = isAlpha c || c == '_' || isDigit c

-- | Parse exactly one atom, failing in Q when none matches.
parseAtom :: String -> Q (Exp, String)
parseAtom [] = fail "fmt: expected an expression atom but found end of antiquote"
parseAtom ('(' : r) = do
    (e, r2) <- parseExpr (trim r)
    case trim r2 of
      ')' : r3 -> return (e, r3)
      _        -> fail "fmt: unclosed '(' in antiquote expression"
parseAtom s = case parseAtomPure s of
    Left  msg    -> fail ("fmt: " ++ msg
                          ++ " — use a where-bound helper for complex expressions")
    Right result -> return result

-- | Parse one atom without Q effects.
parseAtomPure :: String -> Either String (Exp, String)
parseAtomPure []     = Left "unexpected end of expression"
parseAtomPure s@(c : _)
  | isDigit c =
      let (ds, r) = span isDigit s
      in Right (LitE (IntegerL (read ds)), r)
  | isAlpha c || c == '_' =
      let (name, r) = spanQualified s
      in Right (VarE (mkName name), r)
  | otherwise =
      Left ("unexpected character " ++ show c ++ " in antiquote")

------------------------------------------------------------------------
-- Name lexers
------------------------------------------------------------------------

-- | Span a possibly-qualified name: @Conid.Conid.varid@.
spanQualified :: String -> (String, String)
spanQualified s =
    let (seg, r) = spanIdent s
    in case r of
         '.' : r2@(c : _) | isAlpha c || c == '_' ->
             let (rest, r3) = spanQualified r2
             in (seg ++ '.' : rest, r3)
         _ -> (seg, r)

-- | Span one identifier segment: @[A-Za-z_][A-Za-z0-9_\']*@.
spanIdent :: String -> (String, String)
spanIdent = span (\c -> isAlphaNum c || c == '_' || c == '\'')

------------------------------------------------------------------------
-- Utilities
------------------------------------------------------------------------

-- | Strip leading and trailing ASCII whitespace.
trim :: String -> String
trim = reverse . dropWhile isSpace . reverse . dropWhile isSpace
