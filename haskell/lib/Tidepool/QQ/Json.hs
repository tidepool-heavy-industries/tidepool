{-# LANGUAGE TemplateHaskellQuotes #-}

-- | The @[j|...|]@ quasi-quoter: JSON 'Value' literals (expression side)
-- and JSON shape matching (pattern side).
--
-- All parsing happens at COMPILE TIME inside the splice evaluator; the
-- emitted code is plain Core over 'Data.Text.Text' and the vendored
-- 'Tidepool.Aeson.Value.Value' — constructor applications, KeyMap
-- 'Tidepool.Aeson.KeyMap.fromList'/'Tidepool.Aeson.KeyMap.lookup',
-- @toJSON@, and ordinary @case@ dispatch. No runtime JSON parsing, no
-- @Generic@/@Typeable@, nothing the Cranelift JIT does not already run.
-- This does not reintroduce runtime JSON parsing (see PR #144 / the
-- @plans/qq-spike.md@ locked-decision clarification): an eval still cannot
-- parse a 'Data.Text.Text' it computed at runtime.
--
-- == Construction API
--
-- Objects expand to @'Object' (KM.fromList [('fromText' k, v), ...])@.
-- We use @KM.fromList@ + 'fromText' rather than 'object'/'(.=)' because the
-- sub-values are already 'Value's: routing them through @(.=)@ would add a
-- redundant @ToJSON Value@ (identity) dictionary indirection and pull in the
-- polymorphic @object@ machinery. @KM.fromList@ keeps the expansion at the
-- constructor level, which is exactly what the JIT wants.
--
-- == Expression grammar
--
-- Full JSON: objects @{"k": v, ...}@, arrays @[v, ...]@, strings (with the
-- standard JSON escapes @\\\" \\\\ \\\/ \\n \\t \\r \\b \\f \\uXXXX@,
-- including surrogate pairs), numbers (integer/fraction/exponent, all stored
-- in 'Number'\'s 'Double' field), @true@/@false@/@null@, and JSON whitespace.
--
-- Antiquotes appear in /value position only/:
--
--   * @$var@           expands to @toJSON var@
--   * @{expr}@         expands to @toJSON (expr)@, where @expr@ is a small
--                      application grammar: @expr := atom atom*@ (left-nested
--                      application), @atom := qualified-varid | varid |
--                      integer | ( expr )@.
--
-- A @{@ is disambiguated by the first non-whitespace character after it: a
-- @\"@ or @}@ starts a JSON object, anything else starts a @{expr}@
-- antiquote. Object keys must be string literals (antiquoted keys are a
-- compile error).
--
-- == Pattern grammar
--
-- Mirrors the expression grammar with pattern leaves, expanding to a
-- @ViewPatterns@ matcher @Value -> Maybe (Value, ...)@:
--
--   * @$var@           binds the 'Value' at that position (unwrap it yourself
--                      with @case@ or lenses).
--   * @_@              wildcard: matches any 'Value', binds nothing.
--   * literal          string\/number\/@true@\/@false@\/@null@ match exactly.
--   * @{"k": p, ...}@  OPEN-WORLD object match: every listed key must exist
--                      and match; extra keys are allowed.
--   * @[p1, p2]@       exact-length array match.
--   * @[p1, p2, ...]@  fixed-prefix array match (a trailing @...@ allows
--                      extra trailing elements; @[...]@ matches any array).
--
-- @{expr}@ antiquotes are rejected on the pattern side. Binders are bound
-- left-to-right in source order; duplicate binders are a compile error.
module Tidepool.QQ.Json (j) where

import Data.Char (chr, digitToInt, isAlpha, isAlphaNum, isDigit, isHexDigit)
import Data.Text (Text)
import qualified Tidepool.Data.Text as T

import Language.Haskell.TH
  ( Exp (..)
  , Pat
  , Q
  , appE
  , caseE
  , conP
  , integerL
  , lamE
  , letE
  , listE
  , listP
  , litE
  , match
  , mkName
  , newName
  , normalB
  , rationalL
  , stringL
  , tupE
  , tupP
  , valD
  , varE
  , varP
  , viewP
  , wildP
  )
import Language.Haskell.TH.Quote (QuasiQuoter (..))

import Tidepool.Aeson.Value (Value (..), fromText, toJSON)
import qualified Tidepool.Aeson.KeyMap as KM

-- | @[j| {"user": {"id": $uid}} |]@ — a JSON 'Value' literal with antiquotes
-- in expression position; an open-world JSON shape match in pattern position.
j :: QuasiQuoter
j = QuasiQuoter
  { quoteExp  = jExp
  , quotePat  = jPat
  , quoteType = \_ -> fail "[j|…|] cannot be used in a type context"
  , quoteDec  = \_ -> fail "[j|…|] cannot be used in a declaration context"
  }

-- ---------------------------------------------------------------------------
-- AST
-- ---------------------------------------------------------------------------

-- | A parsed @[j|…|]@ node. The expression parser produces 'NObject',
-- 'NArray' (with @False@ ellipsis), 'NString', 'NNumber', 'NBool', 'NNull',
-- 'NAntiVar' and 'NAntiExpr'; the pattern parser produces 'NObject', 'NArray',
-- 'NString', 'NNumber', 'NBool', 'NNull', 'NBind' and 'NWild'.
data Node
  = NObject [(Text, Node)]
  | NArray [Node] Bool        -- ^ elements and whether a trailing @...@ is present
  | NString Text
  | NNumber Double
  | NBool Bool
  | NNull
  | NAntiVar String           -- ^ @$var@ in expression position
  | NAntiExpr MiniExpr         -- ^ @{expr}@ in expression position
  | NBind String              -- ^ @$var@ in pattern position
  | NWild                     -- ^ @_@ in pattern position

-- | The small application grammar inside a @{expr}@ antiquote.
data MiniExpr
  = MApp MiniExpr MiniExpr
  | MVar String               -- ^ a varid or qualified varid (e.g. @T.toUpper@)
  | MInt Integer

-- ---------------------------------------------------------------------------
-- Compile-time parser
-- ---------------------------------------------------------------------------

-- | Parser state: bytes consumed so far and the remaining input.
data St = St !Int String

-- | A hand-rolled parser carrying position for error messages.
newtype P a = P { runP :: St -> Either String (a, St) }

instance Functor P where
  fmap f (P g) = P $ \s -> case g s of
    Left e        -> Left e
    Right (a, s') -> Right (f a, s')

instance Applicative P where
  pure x = P $ \s -> Right (x, s)
  P pf <*> P px = P $ \s -> case pf s of
    Left e         -> Left e
    Right (f, s')  -> case px s' of
      Left e         -> Left e
      Right (x, s'') -> Right (f x, s'')

instance Monad P where
  P g >>= k = P $ \s -> case g s of
    Left e        -> Left e
    Right (a, s') -> runP (k a) s'

instance MonadFail P where
  fail msg = P $ \(St pos rest) ->
    Left (msg ++ " (at offset " ++ show pos ++ ", near " ++ show (take 24 rest) ++ ")")

-- | Peek at the next character without consuming it.
peekC :: P (Maybe Char)
peekC = P $ \s@(St _ inp) -> Right (case inp of { [] -> Nothing; (c : _) -> Just c }, s)

-- | Consume the next character, if any.
nextC :: P (Maybe Char)
nextC = P $ \(St pos inp) -> case inp of
  []       -> Right (Nothing, St pos inp)
  (c : cs) -> Right (Just c, St (pos + 1) cs)

-- | True for JSON insignificant whitespace.
isJsonWs :: Char -> Bool
isJsonWs c = c == ' ' || c == '\t' || c == '\n' || c == '\r'

-- | Skip JSON whitespace.
skipWs :: P ()
skipWs = do
  mc <- peekC
  case mc of
    Just c | isJsonWs c -> nextC >> skipWs
    _                   -> pure ()

-- | Consume exactly the given character or fail.
expectC :: Char -> P ()
expectC ch = do
  mc <- nextC
  case mc of
    Just c | c == ch -> pure ()
    _                 -> fail ("expected '" ++ [ch] ++ "'")

-- | Require end of input.
eof :: P ()
eof = do
  mc <- peekC
  case mc of
    Nothing -> pure ()
    Just _  -> fail "unexpected trailing input"

-- | Consume the longest run of characters satisfying the predicate.
takeWhileP :: (Char -> Bool) -> P String
takeWhileP pr = do
  mc <- peekC
  case mc of
    Just c | pr c -> nextC >> ((c :) <$> takeWhileP pr)
    _             -> pure []

-- | Consume at least one character satisfying the predicate.
takeWhile1P :: (Char -> Bool) -> String -> P String
takeWhile1P pr msg = do
  s <- takeWhileP pr
  if null s then fail msg else pure s

-- | Run a parser over the whole quote body.
parseWith :: P a -> String -> Either String a
parseWith p str = case runP (skipWs *> p <* skipWs <* eof) (St 0 str) of
  Left e       -> Left e
  Right (a, _) -> Right a

isIdentStart :: Char -> Bool
isIdentStart c = isAlpha c || c == '_'

isIdentTail :: Char -> Bool
isIdentTail c = isAlphaNum c || c == '_' || c == '\''

-- | Lex a varid: a leading letter\/underscore then identifier characters.
pVarId :: P String
pVarId = do
  mc <- nextC
  case mc of
    Just c | isIdentStart c -> do
      rest <- takeWhileP isIdentTail
      pure (c : rest)
    _ -> fail "expected an identifier"

-- | Lex a (possibly qualified) varid for the @{expr}@ grammar.
pQualIdent :: P String
pQualIdent = do
  mc <- nextC
  case mc of
    Just c | isIdentStart c -> do
      rest <- takeWhileP (\ch -> isIdentTail ch || ch == '.')
      pure (c : rest)
    _ -> fail "expected an identifier"

-- | Lex an unsigned integer.
pInteger :: P Integer
pInteger = do
  ds <- takeWhile1P isDigit "expected an integer"
  pure (read ds :: Integer)

-- | Lex a JSON number into a 'Double'.
pNumber :: P Double
pNumber = do
  neg  <- optChar '-'
  intD <- takeWhile1P isDigit "expected a digit"
  frac <- optFrac
  ex   <- optExp
  let expStr = case ex of
        Nothing       -> ""
        Just (es, ed) -> "e" ++ (if es then "-" else "") ++ ed
      norm = (if neg then "-" else "")
           ++ intD ++ "." ++ maybe "0" id frac ++ expStr
  pure (read norm :: Double)
  where
    optChar ch = do
      mc <- peekC
      case mc of
        Just c | c == ch -> nextC >> pure True
        _                -> pure False
    optFrac = do
      mc <- peekC
      case mc of
        Just '.' -> do
          _  <- nextC
          ds <- takeWhile1P isDigit "expected a digit after '.'"
          pure (Just ds)
        _ -> pure Nothing
    optExp = do
      mc <- peekC
      case mc of
        Just c | c == 'e' || c == 'E' -> do
          _   <- nextC
          sgn <- peekC
          es  <- case sgn of
                   Just '+' -> nextC >> pure False
                   Just '-' -> nextC >> pure True
                   _        -> pure False
          ds  <- takeWhile1P isDigit "expected a digit in the exponent"
          pure (Just (es, ds))
        _ -> pure Nothing

-- | Lex a JSON string literal (the leading @\"@ must be next).
pStringLit :: P Text
pStringLit = do
  expectC '"'
  go []
  where
    go acc = do
      mc <- nextC
      case mc of
        Nothing   -> fail "unterminated string literal"
        Just '"'  -> pure (T.pack (reverse acc))
        Just '\\' -> do c <- pEscape; go (c : acc)
        Just c    -> go (c : acc)
    pEscape = do
      mc <- nextC
      case mc of
        Just '"'  -> pure '"'
        Just '\\' -> pure '\\'
        Just '/'  -> pure '/'
        Just 'n'  -> pure '\n'
        Just 't'  -> pure '\t'
        Just 'r'  -> pure '\r'
        Just 'b'  -> pure '\b'
        Just 'f'  -> pure '\f'
        Just 'u'  -> pUniEscape
        Just c    -> fail ("invalid escape '\\" ++ [c] ++ "'")
        Nothing   -> fail "unterminated escape"
    pUniEscape = do
      hi <- pHex4
      if hi >= 0xD800 && hi <= 0xDBFF
        then do
          expectC '\\'
          expectC 'u'
          lo <- pHex4
          if lo >= 0xDC00 && lo <= 0xDFFF
            then pure (chr (0x10000 + (hi - 0xD800) * 0x400 + (lo - 0xDC00)))
            else fail "invalid low surrogate in '\\u' escape"
        else if hi >= 0xDC00 && hi <= 0xDFFF
          then fail "unexpected low surrogate in '\\u' escape"
          else pure (chr hi)
    pHex4 = do
      a <- hexDigit
      b <- hexDigit
      c <- hexDigit
      d <- hexDigit
      pure ((((a * 16) + b) * 16 + c) * 16 + d)
    hexDigit = do
      mc <- nextC
      case mc of
        Just ch | isHexDigit ch -> pure (digitToInt ch)
        _                       -> fail "expected a hex digit in '\\u' escape"

-- | Parse a @key: value@ list, sharing the value parser between sides.
pObjectPairs :: P Node -> P [(Text, Node)]
pObjectPairs pVal = go []
  where
    go acc = do
      skipWs
      k <- pStringLit
      skipWs
      expectC ':'
      v  <- pVal
      skipWs
      mc <- nextC
      case mc of
        Just ',' -> go ((k, v) : acc)
        Just '}' -> pure (reverse ((k, v) : acc))
        _        -> fail "expected ',' or '}' in object"

-- ---- expression side ----

-- | Parse a JSON value (expression side).
pExpValue :: P Node
pExpValue = do
  skipWs
  mc <- peekC
  case mc of
    Nothing -> fail "unexpected end of input; expected a JSON value"
    Just c  -> case c of
      '{' -> pExpBrace
      '[' -> pExpArray
      '"' -> NString <$> pStringLit
      't' -> pKeyword "true" (NBool True)
      'f' -> pKeyword "false" (NBool False)
      'n' -> pKeyword "null" NNull
      '$' -> NAntiVar <$> (expectC '$' >> pVarId)
      '-' -> NNumber <$> pNumber
      _ | isDigit c -> NNumber <$> pNumber
        | otherwise -> fail ("unexpected character '" ++ [c] ++ "'; expected a JSON value")

-- | Consume a bare keyword and reject a trailing identifier character.
pKeyword :: String -> Node -> P Node
pKeyword kw node = do
  mapM_ expectC kw
  mc <- peekC
  case mc of
    Just c | isIdentTail c -> fail ("unexpected identifier near keyword '" ++ kw ++ "'")
    _                      -> pure node

-- | Parse a @{...}@: an object or, on the expression side, a @{expr}@ antiquote.
pExpBrace :: P Node
pExpBrace = do
  expectC '{'
  skipWs
  mc <- peekC
  case mc of
    Just '}' -> nextC >> pure (NObject [])
    Just '"' -> NObject <$> pObjectPairs pExpValue
    _        -> pExpAntiExpr

-- | Parse a @{expr}@ antiquote body (the leading @{@ is already consumed).
pExpAntiExpr :: P Node
pExpAntiExpr = do
  e <- pMiniExpr
  skipWs
  expectC '}'
  pure (NAntiExpr e)

-- | Parse the @expr := atom atom*@ application grammar.
pMiniExpr :: P MiniExpr
pMiniExpr = do
  a <- pMiniAtom
  pMiniApp a

pMiniApp :: MiniExpr -> P MiniExpr
pMiniApp acc = do
  skipWs
  mc <- peekC
  case mc of
    Just c | c == '}' || c == ')' -> pure acc
    Just _                        -> do a <- pMiniAtom; pMiniApp (MApp acc a)
    Nothing                       -> fail "unterminated antiquote; expected '}'"

pMiniAtom :: P MiniExpr
pMiniAtom = do
  skipWs
  mc <- peekC
  case mc of
    Just '(' -> do
      _ <- nextC
      e <- pMiniExpr
      skipWs
      expectC ')'
      pure e
    Just c
      | isIdentStart c -> MVar <$> pQualIdent
      | isDigit c      -> MInt <$> pInteger
    _ -> fail "expected an antiquote atom: identifier, integer, or ( … )"

-- | Parse a JSON array (expression side).
pExpArray :: P Node
pExpArray = do
  expectC '['
  skipWs
  mc <- peekC
  case mc of
    Just ']' -> nextC >> pure (NArray [] False)
    _        -> goElems []
  where
    goElems acc = do
      v  <- pExpValue
      skipWs
      mc <- nextC
      case mc of
        Just ',' -> goElems (v : acc)
        Just ']' -> pure (NArray (reverse (v : acc)) False)
        _        -> fail "expected ',' or ']' in array"

-- ---- pattern side ----

-- | Parse a JSON pattern (pattern side).
pPatValue :: P Node
pPatValue = do
  skipWs
  mc <- peekC
  case mc of
    Nothing -> fail "unexpected end of input; expected a JSON pattern"
    Just c  -> case c of
      '{' -> pPatObject
      '[' -> pPatArray
      '"' -> NString <$> pStringLit
      't' -> pKeyword "true" (NBool True)
      'f' -> pKeyword "false" (NBool False)
      'n' -> pKeyword "null" NNull
      '$' -> NBind <$> (expectC '$' >> pVarId)
      '_' -> pPatWild
      '-' -> NNumber <$> pNumber
      _ | isDigit c -> NNumber <$> pNumber
        | otherwise -> fail ("unexpected character '" ++ [c] ++ "'; expected a JSON pattern")

-- | Parse a @_@ wildcard, rejecting bare identifiers.
pPatWild :: P Node
pPatWild = do
  expectC '_'
  mc <- peekC
  case mc of
    Just c | isIdentTail c ->
      fail "bare identifiers are not patterns; use $name to bind or _ to ignore"
    _ -> pure NWild

-- | Parse an object pattern (pattern side); @{expr}@ antiquotes are rejected.
pPatObject :: P Node
pPatObject = do
  expectC '{'
  skipWs
  mc <- peekC
  case mc of
    Just '}' -> nextC >> pure (NObject [])
    Just '"' -> NObject <$> pObjectPairs pPatValue
    _        -> fail "'{expr}' antiquotes are not allowed in pattern position; object keys must be string literals"

-- | Parse an array pattern, honouring a trailing @...@ prefix marker.
pPatArray :: P Node
pPatArray = do
  expectC '['
  skipWs
  mc <- peekC
  case mc of
    Just ']' -> nextC >> pure (NArray [] False)
    Just '.' -> do pEllipsis; skipWs; expectC ']'; pure (NArray [] True)
    _        -> goElems []
  where
    goElems acc = do
      v  <- pPatValue
      skipWs
      mc <- nextC
      case mc of
        Just ',' -> do
          skipWs
          mc2 <- peekC
          case mc2 of
            Just '.' -> do
              pEllipsis
              skipWs
              expectC ']'
              pure (NArray (reverse (v : acc)) True)
            _ -> goElems (v : acc)
        Just ']' -> pure (NArray (reverse (v : acc)) False)
        _        -> fail "expected ',' or ']' in array pattern"

pEllipsis :: P ()
pEllipsis = expectC '.' >> expectC '.' >> expectC '.'

-- ---------------------------------------------------------------------------
-- Expression-side codegen
-- ---------------------------------------------------------------------------

jExp :: String -> Q Exp
jExp s = case parseWith pExpValue s of
  Left e     -> fail ("[j|…|] (expression): " ++ e)
  Right node -> expCodegen node

-- | Emit a 'Value' from a parsed expression-side node.
expCodegen :: Node -> Q Exp
expCodegen node = case node of
  NNull        -> [| Null |]
  NBool True   -> [| Bool True |]
  NBool False  -> [| Bool False |]
  NString t    -> [| String (T.pack $(litE (stringL (T.unpack t)))) |]
  NNumber d    -> [| Number $(litE (rationalL (toRational d))) |]
  NArray es _  -> [| Array $(listE (map expCodegen es)) |]
  NObject kvs  -> [| Object (KM.fromList $(listE (map pairE kvs))) |]
  NAntiVar v   -> [| toJSON $(varE (mkName v)) |]
  NAntiExpr me -> [| toJSON $(miniCodegen me) |]
  NBind _      -> fail "internal error: $-binder in expression position"
  NWild        -> fail "internal error: wildcard in expression position"
  where
    pairE (k, v) =
      [| (fromText (T.pack $(litE (stringL (T.unpack k)))), $(expCodegen v)) |]

-- | Emit the application built from a @{expr}@ antiquote.
miniCodegen :: MiniExpr -> Q Exp
miniCodegen me = case me of
  MVar s   -> varE (mkName s)
  MInt n   -> litE (integerL n)
  MApp f x -> appE (miniCodegen f) (miniCodegen x)

-- ---------------------------------------------------------------------------
-- Pattern-side codegen
-- ---------------------------------------------------------------------------

jPat :: String -> Q Pat
jPat s = case parseWith pPatValue s of
  Left e     -> fail ("[j|…|] (pattern): " ++ e)
  Right node ->
    let binders = collectBinders node
    in case findDup binders of
         Just d  -> fail ("[j|…|] (pattern): duplicate binder $" ++ d)
         Nothing -> do
           root    <- newName "root"
           matcher <- lamE [varP root] (buildMatch [(VarE root, node)] binders)
           viewP (pure matcher) (resultPat binders)

-- | Collect @$-binders@ in left-to-right source order.
collectBinders :: Node -> [String]
collectBinders node = case node of
  NBind v     -> [v]
  NObject kvs -> concatMap (collectBinders . snd) kvs
  NArray es _ -> concatMap collectBinders es
  _           -> []

-- | Return the first duplicated name, if any.
findDup :: [String] -> Maybe String
findDup = go []
  where
    go _ [] = Nothing
    go seen (x : xs)
      | x `elem` seen = Just x
      | otherwise     = go (x : seen) xs

-- | @Nothing@ as a 'Q' 'Exp', the failure result of every matcher branch.
nothingE :: Q Exp
nothingE = [| Nothing |]

-- | Build the @Maybe@-returning matcher body. The worklist threads
-- @(scrutinee, pattern)@ pairs in source order so binders are introduced in
-- the same order 'collectBinders' reports them; nested @case@/@let@ keep every
-- binder in scope for the final tuple.
buildMatch :: [(Exp, Node)] -> [String] -> Q Exp
buildMatch [] binders = finalJust binders
buildMatch ((scrut, node) : rest) binders =
  case node of
    NWild   -> cont
    NBind v -> letE [valD (varP (mkName v)) (normalB (pure scrut)) []] cont
    NNull   -> dispatch [ match (conP 'Null []) (normalB cont) [] ]
    NBool b -> dispatch [ match (conP 'Bool [conP (if b then 'True else 'False) []]) (normalB cont) [] ]
    NString t -> do
      x <- newName "s"
      dispatch
        [ match (conP 'String [varP x])
            (normalB
               [| if $(varE x) == T.pack $(litE (stringL (T.unpack t)))
                    then $(cont) else Nothing |]) [] ]
    NNumber d -> do
      x <- newName "n"
      dispatch
        [ match (conP 'Number [varP x])
            (normalB
               [| if $(varE x) == $(litE (rationalL (toRational d)))
                    then $(cont) else Nothing |]) [] ]
    NArray es ell -> do
      xs <- newName "xs"
      dispatch [ match (conP 'Array [varP xs]) (normalB (matchList (VarE xs) es ell rest binders)) [] ]
    NObject kvs -> do
      m <- newName "m"
      dispatch [ match (conP 'Object [varP m]) (normalB (matchObj (VarE m) kvs rest binders)) [] ]
    NAntiVar _  -> fail "internal error: $var antiquote in pattern position"
    NAntiExpr _ -> fail "internal error: {expr} antiquote in pattern position"
  where
    cont = buildMatch rest binders
    dispatch alts = caseE (pure scrut) (alts ++ [ match wildP (normalB nothingE) [] ])

-- | Match an array's element list, length-checking via a list pattern.
matchList :: Exp -> [Node] -> Bool -> [(Exp, Node)] -> [String] -> Q Exp
matchList xsExp es ell rest binders = do
  names <- mapM (const (newName "e")) es
  let listPat
        | ell       = foldr (\n acc -> conP '(:) [varP n, acc]) wildP names
        | otherwise = listP (map varP names)
      newWork = zip (map VarE names) es ++ rest
  caseE (pure xsExp)
    [ match listPat (normalB (buildMatch newWork binders)) []
    , match wildP (normalB nothingE) [] ]

-- | Match an object's listed keys (open-world), looking each up before
-- continuing with the accumulated @(value, subpattern)@ work.
matchObj :: Exp -> [(Text, Node)] -> [(Exp, Node)] -> [String] -> Q Exp
matchObj mExp kvs rest binders = go kvs []
  where
    go [] acc = buildMatch (reverse acc ++ rest) binders
    go ((k, sub) : more) acc = do
      vk <- newName "v"
      caseE [| KM.lookup (fromText (T.pack $(litE (stringL (T.unpack k))))) $(pure mExp) |]
        [ match (conP 'Just [varP vk]) (normalB (go more ((VarE vk, sub) : acc))) []
        , match (conP 'Nothing []) (normalB nothingE) [] ]

-- | The successful result: @Just ()@, @Just b@, or @Just (b1, ..., bn)@.
finalJust :: [String] -> Q Exp
finalJust []  = [| Just () |]
finalJust [b] = [| Just $(varE (mkName b)) |]
finalJust bs  = [| Just $(tupE (map (varE . mkName) bs)) |]

-- | The @ViewPatterns@ result pattern matching 'finalJust'.
resultPat :: [String] -> Q Pat
resultPat []  = conP 'Just [tupP []]
resultPat [b] = conP 'Just [varP (mkName b)]
resultPat bs  = conP 'Just [tupP (map (varP . mkName) bs)]
