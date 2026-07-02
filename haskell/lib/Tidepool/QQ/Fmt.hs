{-# LANGUAGE TemplateHaskellQuotes #-}
-- | The @[fmt|...|]@ quasi-quoter: Text interpolation with Python-f-string
-- format specs.
--
-- == Grammar
--
-- @
-- [fmt| literal text with {expr} and {expr:spec} holes |]
-- @
--
-- The result type is @Data.Text.Text@.  Literal segments are wrapped with
-- 'Data.Text.pack'; multiple segments are assembled with 'Data.Text.concat'.
--
-- == Antiquote expressions
--
-- Inside @{...}@ is an arbitrary Haskell expression, parsed by the GHC parser
-- (vendored 'Tidepool.QQ.HsMeta.Parse.parseExp').  Names resolve at the SPLICE
-- SITE (via 'Language.Haskell.TH.mkName'), so call-site imports apply.  The
-- hole lexer tracks bracket depth and skips string\/char literals, so braces,
-- colons and @|]@-free expressions inside a hole — record syntax, @let { … }@,
-- @T.pack \"a:b\"@ — are handled correctly (Phase 1 stopped at the first @}@).
--
-- == Format specs (after a @:@)
--
-- A hole may carry a Python-style format spec: @{expr:spec}@.  The spec grammar
-- and AST are vendored from PyF (see "Tidepool.QQ.PyF.Spec"); the spec is
-- interpreted at compile time and the hole expands to a call to a monomorphic,
-- JIT-safe helper from "Tidepool.QQ.Fmt.Runtime".  Supported:
--
-- * Fixed-point floats: @{d:.2f}@, @{d:f}@ (round-primop, no @floatToDigits@).
-- * Percent: @{d:%}@, @{d:.1%}@.
-- * Integer bases: @{n:d}@, @{n:x}@\/@{n:X}@, @{n:o}@, @{n:b}@ (+ @#@ prefix).
-- * Strings: @{t:s}@ (precision truncates).
-- * Width\/fill\/align: @{t:>10}@, @{t:<10}@, @{t:^10}@, @{t:*^10}@, @{n:04d}@.
-- * Sign: @{n:+}@, @{n:+d}@, @{n: d}@.  Grouping: @{n:,d}@.
--
-- @{e}@\/@{E}@ (exponential) and @{g}@\/@{G}@ (general) are rejected at compile
-- time (they need @floatToDigits@, which the JIT cannot run) with a message
-- pointing at @:f@.  A spec-less hole keeps the Phase 1 behaviour exactly:
-- @render expr@.
--
-- == Escapes in literal text
--
-- * @\\\{@ → literal @{@, @\\\\@ → literal @\\@, @\\c@ → @\\c@ unchanged.
-- * @{{@ → literal @{@, @}}@ → literal @}@ (PyF\/Python doubling).
-- * A bare @}@ outside a hole is a literal @}@.
-- * An unclosed @{@ → compile-time error.
--
-- == Limitation
--
-- @|]@ cannot appear inside the literal body — GHC terminates the quasi-quote
-- at the first occurrence of that token.
module Tidepool.QQ.Fmt (fmt) where

import Language.Haskell.TH        (Exp (..), Lit (..), Name, Q)
import Tidepool.Render            (render)
import Tidepool.QQ.Fmt.Runtime
  ( FSign (..), FAlign (..)
  , fmtInt, fmtFrac, fmtStr, fmtChar, fmtSigned, fmtPlain )
import Language.Haskell.TH.Quote  (QuasiQuoter (..))
import Data.Char                  (isSpace, isAlphaNum)
import Data.Maybe                 (fromMaybe)
import qualified Tidepool.Data.Text as T
import Tidepool.QQ.HsMeta.Parse   (parseExp)
import Tidepool.QQ.PyF.Spec
  ( FormatMode (..), Padding (..), Precision (..), TypeFormat (..)
  , AlternateForm (..), SignMode (..), Align (..), parseFormatSpec )

-- | @[fmt|...|]@: quasi-quoted 'Data.Text.Text' interpolation.  See the module
-- haddock for grammar, format specs, escapes and examples.
fmt :: QuasiQuoter
fmt = QuasiQuoter
  { quoteExp  = fmtExp
  , quotePat  = \_ -> fail "fmt: cannot be used in pattern position"
  , quoteType = \_ -> fail "fmt: cannot be used in type position"
  , quoteDec  = \_ -> fail "fmt: cannot be used in declaration position"
  }

------------------------------------------------------------------------
-- Item type
------------------------------------------------------------------------

-- | A lexed chunk of the format string.
data Item
  = ILit  String                 -- ^ Literal text, already escape-processed.
  | IHole String (Maybe String)  -- ^ @{expr}@ or @{expr:spec}@ (both trimmed).

------------------------------------------------------------------------
-- Top-level expansion
------------------------------------------------------------------------

fmtExp :: String -> Q Exp
fmtExp src = lexItems src >>= buildExp

buildExp :: [Item] -> Q Exp
buildExp items = do
    let merged   = mergeAdjacentLits items
        nonempty = filter (not . emptyLit) merged
    parts <- mapM itemToExp nonempty
    case parts of
      []  -> [| T.empty |]
      [e] -> return e
      es  -> [| T.concat $(return (ListE es)) |]
  where
    emptyLit (ILit "") = True
    emptyLit _         = False

itemToExp :: Item -> Q Exp
itemToExp (ILit s)            = [| T.pack $(return (LitE (StringL s))) |]
itemToExp (IHole e Nothing)   = renderHole e
itemToExp (IHole e (Just sp)) = specHole e sp

-- | Merge adjacent 'ILit' segments into a single one.
mergeAdjacentLits :: [Item] -> [Item]
mergeAdjacentLits (ILit a : ILit b : rest) = mergeAdjacentLits (ILit (a ++ b) : rest)
mergeAdjacentLits (x : xs)                  = x : mergeAdjacentLits xs
mergeAdjacentLits []                        = []

------------------------------------------------------------------------
-- Hole lexer (bracket-depth + literal aware)
------------------------------------------------------------------------

lexItems :: String -> Q [Item]
lexItems []                     = return []
lexItems ('{' : '{' : r)        = consLit "{"      (lexItems r)   -- {{ → {
lexItems ('}' : '}' : r)        = consLit "}"      (lexItems r)   -- }} → }
lexItems ('\\' : '{' : r)       = consLit "{"      (lexItems r)   -- \{ → {
lexItems ('\\' : '\\' : r)      = consLit "\\"     (lexItems r)   -- \\ → \
lexItems ('\\' : c : r)         = consLit ['\\', c] (lexItems r)  -- \c → \c
lexItems ('{' : r)              = do
    (body, rest) <- scanHole 0 False r
    let (e, msp) = splitExprSpec body
    fmap (IHole (trim e) (fmap trim msp) :) (lexItems rest)
lexItems ('}' : r)              = consLit "}" (lexItems r)        -- bare } literal
lexItems (c : r)                = consLit [c] (lexItems r)

consLit :: String -> Q [Item] -> Q [Item]
consLit s = fmap (ILit s :)

-- | Scan a hole body up to the matching @}@ (the one at bracket depth 0),
-- skipping string and char literals.  @pIdent@ tracks whether the previous
-- char was an identifier char, so a @'@ following one (a prime, e.g. @x'@) is
-- not mistaken for a char-literal opener.
scanHole :: Int -> Bool -> String -> Q (String, String)
scanHole _ _ [] = fail "fmt: unclosed '{' — no matching '}' before end of quote"
scanHole d pIdent (c : cs)
  | c == '}' && d == 0 = return ([], cs)
  | c == '"'           = do (lit, cs') <- scanLiteral '"' cs
                            prependBody ('"' : lit) (scanHole d False cs')
  | c == '\'' && not pIdent
                       = do (lit, cs') <- scanLiteral '\'' cs
                            prependBody ('\'' : lit) (scanHole d False cs')
  | otherwise          = prependBody [c] (scanHole (bump c d) (isIdentChar c) cs)
  where
    prependBody pre m = do { (b, r) <- m; return (pre ++ b, r) }

-- | Consume a string\/char literal body (after the opening delimiter) up to
-- and including the closing delimiter, honouring backslash escapes.
scanLiteral :: Char -> String -> Q (String, String)
scanLiteral delim = go
  where
    go []               = fail "fmt: unterminated literal inside a {hole}"
    go ('\\' : x : xs)  = do { (a, r) <- go xs; return ('\\' : x : a, r) }
    go (x : xs)
      | x == delim      = return ([delim], xs)
      | otherwise       = do { (a, r) <- go xs; return (x : a, r) }

bump :: Char -> Int -> Int
bump c d
  | c `elem` ("([{" :: String) = d + 1
  | c `elem` (")]}" :: String) = d - 1
  | otherwise                  = d

isIdentChar :: Char -> Bool
isIdentChar c = isAlphaNum c || c == '_' || c == '\''

-- | Split a hole body into the expression and an optional format spec.  The
-- separator is the first @:@ at bracket depth 0 that is not part of a @::@
-- (unless that @::@ is immediately followed by an alignment char, in which case
-- the first @:@ is the separator and the @:@ is a fill char — PyF's rule).
splitExprSpec :: String -> (String, Maybe String)
splitExprSpec = go 0 False
  where
    go :: Int -> Bool -> String -> (String, Maybe String)
    go _ _ [] = ([], Nothing)
    go d _ ('"' : cs) =
      let (lit, cs') = takeLiteral '"' cs in mapFst (('"' : lit) ++) (go d False cs')
    go d pIdent ('\'' : cs)
      | not pIdent =
          let (lit, cs') = takeLiteral '\'' cs in mapFst (('\'' : lit) ++) (go d False cs')
    go d _ (':' : ':' : cs)
      | d == 0 && not (startsAlign cs) = mapFst ("::" ++) (go d False cs)
    go d _ (':' : cs)
      | d == 0 = ([], Just cs)
    go d _ (c : cs) = mapFst (c :) (go (bump c d) (isIdentChar c) cs)

    startsAlign :: String -> Bool
    startsAlign (x : _) = x `elem` ("<>=^" :: String)
    startsAlign []      = False

    mapFst f (a, b) = (f a, b)

-- | Pure analogue of 'scanLiteral' (no failure: an unterminated literal just
-- consumes to end of input and lets the GHC parser report it).
takeLiteral :: Char -> String -> (String, String)
takeLiteral delim = go
  where
    go []              = ([], [])
    go ('\\' : x : xs) = let (a, r) = go xs in ('\\' : x : a, r)
    go (x : xs)
      | x == delim     = ([delim], xs)
      | otherwise      = let (a, r) = go xs in (x : a, r)

------------------------------------------------------------------------
-- Hole codegen
------------------------------------------------------------------------

-- | Spec-less hole: Phase 1 behaviour — @render (expr)@.
renderHole :: String -> Q Exp
renderHole raw
  | null t    = fail "fmt: empty antiquote '{}' — provide an expression"
  | otherwise = do e <- parseHoleExpr t
                   -- 'render (original-name quote): immune to a user
                   -- session binding named `render` shadowing the class
                   -- method (capture bug found live 2026-07-02).
                   return (AppE (VarE 'render) e)
  where t = trim raw

-- | Spec'd hole: parse the spec, interpret it, emit a helper call.
specHole :: String -> String -> Q Exp
specHole rawExpr rawSpec
  | null e    = fail "fmt: empty antiquote before ':' — provide an expression"
  | otherwise = case parseFormatSpec rawSpec of
      Left err  -> fail ("fmt: invalid format spec " ++ show rawSpec ++ ": " ++ err)
      Right fm  -> do x <- parseHoleExpr e
                      emitFormat rawSpec fm x
  where e = trim rawExpr

-- | Parse a hole expression and parenthesize it (so operator holes bind as a
-- whole argument).
parseHoleExpr :: String -> Q Exp
parseHoleExpr t = case parseExp t of
  Left (line, col, msg) ->
    fail $ "fmt: cannot parse antiquote " ++ show t
        ++ " (" ++ show line ++ ":" ++ show col ++ "): " ++ msg
  Right e -> return (ParensE e)

-- | Compile a parsed 'FormatMode' applied to the (parenthesized) hole
-- expression into a call to a "Tidepool.QQ.Fmt.Runtime" helper.
emitFormat :: String -> FormatMode -> Exp -> Q Exp
emitFormat specText (FormatMode padding tf grp) x =
  let (width, fill, mAlign) = extractPad padding
  in case tf of
    DefaultF _ sign        -> emitDefault sign width fill mAlign x
    DecimalF sign          -> emitInt sign 10 False False  grp width fill mAlign x
    BinaryF alt sign       -> emitInt sign 2  False (isAlt alt) grp width fill mAlign x
    OctalF alt sign        -> emitInt sign 8  False (isAlt alt) grp width fill mAlign x
    HexF alt sign          -> emitInt sign 16 False (isAlt alt) grp width fill mAlign x
    HexCapsF alt sign      -> emitInt sign 16 True  (isAlt alt) grp width fill mAlign x
    CharacterF             -> emitChar width fill mAlign x
    FixedF prec _ sign     -> emitFrac sign False (precOr 6 prec) width fill mAlign x
    FixedCapsF prec _ sign -> emitFrac sign False (precOr 6 prec) width fill mAlign x
    PercentF prec _ sign   -> emitFrac sign True  (precOr 6 prec) width fill mAlign x
    StringF prec           -> emitStr (precMaybe prec) width fill mAlign x
    ExponentialF{}         -> rejectExp specText "e" "exponential"
    ExponentialCapsF{}     -> rejectExp specText "E" "exponential"
    GeneralF{}             -> rejectExp specText "g" "general"
    GeneralCapsF{}         -> rejectExp specText "G" "general"

rejectExp :: String -> String -> String -> Q Exp
rejectExp specText flag desc =
  fail $ "fmt: format type ':" ++ flag ++ "' (" ++ desc
      ++ ") is not supported on the JIT (it needs floatToDigits); "
      ++ "use ':f' (fixed-point) instead. spec was " ++ show specText

------------------------------------------------------------------------
-- Per-category emitters
------------------------------------------------------------------------

-- Numeric default alignment is right; string/char/type-less is left.

emitInt :: SignMode -> Int -> Bool -> Bool -> Maybe Char -> Int -> Char -> Maybe Align -> Exp -> Q Exp
emitInt sign base upper alt grp width fill mAlign x =
  return $ applyN 'fmtInt
    [ signE sign, intE base, boolE upper, boolE alt, maybeCharE grp
    , intE width, charE fill, alignE (fromMaybe AlignRight mAlign), x ]

emitFrac :: SignMode -> Bool -> Int -> Int -> Char -> Maybe Align -> Exp -> Q Exp
emitFrac sign percent prec width fill mAlign x =
  return $ applyN 'fmtFrac
    [ signE sign, boolE percent, intE prec
    , intE width, charE fill, alignE (fromMaybe AlignRight mAlign), x ]

emitStr :: Maybe Int -> Int -> Char -> Maybe Align -> Exp -> Q Exp
emitStr mprec width fill mAlign x =
  return $ applyN 'fmtStr
    [ maybeIntE mprec, intE width, charE fill
    , alignE (fromMaybe AlignLeft mAlign), renderE x ]

emitChar :: Int -> Char -> Maybe Align -> Exp -> Q Exp
emitChar width fill mAlign x =
  return $ applyN 'fmtChar
    [ intE width, charE fill, alignE (fromMaybe AlignLeft mAlign), x ]

-- | Type-less hole.  An explicit @+@\/space sign routes through 'fmtSigned'
-- (numeric intent — sign recovered from the rendered text); otherwise the
-- rendered text is just padded.  Both paths render via 'Render'.
emitDefault :: SignMode -> Int -> Char -> Maybe Align -> Exp -> Q Exp
emitDefault sign width fill mAlign x = case sign of
  Minus -> return $ applyN 'fmtPlain
             [ intE width, charE fill, alignE (fromMaybe AlignLeft mAlign), renderE x ]
  _     -> return $ applyN 'fmtSigned
             [ signE sign, intE width, charE fill
             , alignE (fromMaybe AlignRight mAlign), renderE x ]

------------------------------------------------------------------------
-- Padding extraction + TH builders
------------------------------------------------------------------------

extractPad :: Padding -> (Int, Char, Maybe Align)
extractPad PaddingDefault       = (0, ' ', Nothing)
extractPad (Padding w mAlign)   = case mAlign of
  Nothing        -> (w, ' ', Nothing)
  Just (mc, al)  -> (w, fromMaybe ' ' mc, Just al)

isAlt :: AlternateForm -> Bool
isAlt AlternateForm = True
isAlt NormalForm    = False

precOr :: Int -> Precision -> Int
precOr def PrecisionDefault = def
precOr _   (Precision n)    = n

precMaybe :: Precision -> Maybe Int
precMaybe PrecisionDefault = Nothing
precMaybe (Precision n)    = Just n

applyN :: Name -> [Exp] -> Exp
applyN fn = foldl AppE (VarE fn)

renderE :: Exp -> Exp
renderE = AppE (VarE 'render)

intE :: Int -> Exp
intE n = LitE (IntegerL (fromIntegral n))

charE :: Char -> Exp
charE = LitE . CharL

boolE :: Bool -> Exp
boolE b = ConE (if b then 'True else 'False)

signE :: SignMode -> Exp
signE Plus  = ConE 'FPlus
signE Minus = ConE 'FMinus
signE Space = ConE 'FSpace

alignE :: Align -> Exp
alignE AlignLeft   = ConE 'FLeft
alignE AlignRight  = ConE 'FRight
alignE AlignCenter = ConE 'FCenter
alignE AlignInside = ConE 'FInside

maybeCharE :: Maybe Char -> Exp
maybeCharE Nothing  = ConE 'Nothing
maybeCharE (Just c) = AppE (ConE 'Just) (charE c)

maybeIntE :: Maybe Int -> Exp
maybeIntE Nothing  = ConE 'Nothing
maybeIntE (Just n) = AppE (ConE 'Just) (intE n)

------------------------------------------------------------------------
-- Utilities
------------------------------------------------------------------------

-- | Strip leading and trailing ASCII whitespace.
trim :: String -> String
trim = reverse . dropWhile isSpace . reverse . dropWhile isSpace
