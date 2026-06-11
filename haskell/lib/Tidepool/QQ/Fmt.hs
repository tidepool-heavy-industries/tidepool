{-# LANGUAGE TemplateHaskellQuotes #-}
-- | The @[fmt|...|]@ quasi-quoter: Text interpolation.
--
-- == Grammar
--
-- @
-- [fmt| literal text with {expr} holes |]
-- @
--
-- The result type is @Data.Text.Text@.  Literal segments are wrapped with
-- 'Data.Text.pack'; multiple segments are assembled with 'Data.Text.concat'.
-- An empty quote (@[fmt||]@) expands to @Data.Text.pack \"\"@.  A single
-- non-empty literal segment skips the @concat@ wrapper.
--
-- == Antiquote expressions
--
-- Inside @{...}@ is an arbitrary Haskell expression, parsed by the GHC parser
-- (vendored 'Tidepool.QQ.HsMeta.Parse.parseExp').  Operators, function
-- application, qualified names, sections, literals — anything the parser
-- accepts.  Names are resolved at the SPLICE SITE (every parsed 'TH.Name' is
-- built with 'Language.Haskell.TH.mkName'), so the imports in scope at the
-- call site apply.
--
-- Each hole expression is wrapped in @render@ (the 'Tidepool.Prelude.Render'
-- single-method coercion class), so holes need not already be @Text@: @Int@,
-- @Double@, @Bool@, @Char@, @String@ and @Text@ all interpolate directly.
-- @{show x}@ still works — its @Text@ result renders as itself.  @render@ is
-- resolved at the splice site too, so it must be in scope at the call site
-- (the MCP preamble and 'Tidepool.Prelude' export it).
--
-- A parse failure inside a hole is a compile-time 'fail' that quotes the hole
-- text and the GHC parser's line\/column and message.
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
import Data.Char                  (isSpace)
import qualified Data.Text as T
import Tidepool.QQ.HsMeta.Parse   (parseExp)

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
-- Antiquote parser (delegates to the vendored GHC parser)
------------------------------------------------------------------------

-- | Parse a hole's body as an arbitrary Haskell expression and wrap the
--   result in @render@ so the hole may be of any 'Render'-able type.  The
--   parsed expression is parenthesized first so operator holes (e.g.
--   @{n + 1}@) bind as a whole argument to @render@.
parseHole :: String -> Q Exp
parseHole raw
  | null t    = fail "fmt: empty antiquote '{}' — provide an expression"
  | otherwise = case parseExp t of
      Left (line, col, msg) ->
        fail $ "fmt: cannot parse antiquote " ++ show t
            ++ " (" ++ show line ++ ":" ++ show col ++ "): " ++ msg
      Right e -> return (AppE (VarE (mkName "render")) (ParensE e))
  where
    t = trim raw

------------------------------------------------------------------------
-- Utilities
------------------------------------------------------------------------

-- | Strip leading and trailing ASCII whitespace.
trim :: String -> String
trim = reverse . dropWhile isSpace . reverse . dropWhile isSpace
