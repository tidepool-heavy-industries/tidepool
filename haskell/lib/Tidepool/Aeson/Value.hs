{-# LANGUAGE OverloadedStrings #-}
-- | Vendored aeson Value type with construction, encoding, and parsing.
--
-- This module provides the core JSON Value type, construction helpers,
-- and a pure Haskell JSON parser — no FFI or unsupported primops.
--
-- Differences from upstream aeson:
--   - Array uses [Value] instead of V.Vector Value (avoids Array# primop)
--   - KeyMap uses Data.Map.Strict instead of HashMap (avoids hash primops)
--   - Parser is a simple recursive descent (no attoparsec dependency)
module Tidepool.Aeson.Value
  ( -- * Core types
    Value(..)
  , Key(..)
  , KeyMap
  , Object
  , Array
  , Pair
    -- * Key construction
  , fromText
  , toText
    -- * Value construction
  , object
  , (.=)
  , emptyObject
  , emptyArray
    -- * ToJSON class
  , ToJSON(..)
    -- * Encoding
  , encode
    -- * Decoding
  , decode
  , eitherDecode
    -- * Result type
  , Result(..)
  ) where

import Prelude
import Data.Text (Text)
import qualified Data.Text as T
import qualified Data.Map.Strict as Map
import qualified Data.Set as Set

-- | A JSON key — thin wrapper around Text.
newtype Key = Key Text
  deriving (Eq, Ord, Show)

-- | Convert Text to a Key.
fromText :: Text -> Key
fromText = Key

-- | Convert a Key back to Text.
toText :: Key -> Text
toText (Key t) = t

-- | KeyMap backed by Data.Map.Strict (avoids HashMap primop issues).
type KeyMap v = Map.Map Key v

-- | A JSON object.
type Object = KeyMap Value

-- | A JSON array (uses list instead of Vector to avoid Array# primops).
type Array = [Value]

-- | A key-value pair for building objects.
type Pair = (Key, Value)

-- | A JSON value.
data Value
  = Object !Object
  | Array Array
  | String !Text
  | Number !Double
  | Bool !Bool
  | Null
  deriving (Eq, Ord, Show)

-- | Construct a JSON object from key-value pairs.
object :: [Pair] -> Value
object = Object . Map.fromList

-- | Pair a text key with a JSON-encodable value.
(.=) :: ToJSON v => Text -> v -> Pair
k .= v = (Key k, toJSON v)
infixr 8 .=

-- | Empty JSON object.
emptyObject :: Value
emptyObject = Object Map.empty

-- | Empty JSON array.
emptyArray :: Value
emptyArray = Array []

-- | A class for types that can be converted to JSON Value.
class ToJSON a where
  toJSON :: a -> Value

instance ToJSON Value where
  toJSON = id

instance ToJSON Text where
  toJSON = String

instance ToJSON Int where
  toJSON n = Number (fromIntegral n)

instance ToJSON Double where
  toJSON = Number

instance ToJSON Float where
  toJSON = Number . realToFrac

instance ToJSON Bool where
  toJSON = Bool

instance {-# OVERLAPPABLE #-} ToJSON a => ToJSON [a] where
  toJSON = Array . map toJSON

instance {-# OVERLAPPING #-} ToJSON [Char] where
  toJSON cs = String (T.pack cs)

instance ToJSON a => ToJSON (Maybe a) where
  toJSON Nothing  = Null
  toJSON (Just a) = toJSON a

instance ToJSON () where
  toJSON () = Null

instance ToJSON Integer where
  toJSON n = Number (fromIntegral n)

instance ToJSON Word where
  toJSON n = Number (fromIntegral n)

instance ToJSON Char where
  toJSON c = String (T.singleton c)

instance ToJSON Ordering where
  toJSON LT = String "LT"
  toJSON EQ = String "EQ"
  toJSON GT = String "GT"

instance (ToJSON a, ToJSON b) => ToJSON (Either a b) where
  toJSON (Left a)  = Object (Map.singleton (Key "Left") (toJSON a))
  toJSON (Right b) = Object (Map.singleton (Key "Right") (toJSON b))

instance (ToJSON a, ToJSON b) => ToJSON (a, b) where
  toJSON (a, b) = Array [toJSON a, toJSON b]

instance (ToJSON a, ToJSON b, ToJSON c) => ToJSON (a, b, c) where
  toJSON (a, b, c) = Array [toJSON a, toJSON b, toJSON c]

instance (ToJSON a, ToJSON b, ToJSON c, ToJSON d) => ToJSON (a, b, c, d) where
  toJSON (a, b, c, d) = Array [toJSON a, toJSON b, toJSON c, toJSON d]

instance (ToJSON a, ToJSON b, ToJSON c, ToJSON d, ToJSON e) => ToJSON (a, b, c, d, e) where
  toJSON (a, b, c, d, e) = Array [toJSON a, toJSON b, toJSON c, toJSON d, toJSON e]

instance ToJSON a => ToJSON (Map.Map Text a) where
  toJSON m = Object (Map.mapKeys Key (Map.map toJSON m))

instance ToJSON a => ToJSON (Set.Set a) where
  toJSON = Array . map toJSON . Set.toList

-- | Encode a Value as JSON Text.
encode :: Value -> Text
encode (String t) = T.concat ["\"", escapeJSON t, "\""]
encode (Number d) = let n = Prelude.truncate d :: Int
                     in if Prelude.fromIntegral n == d
                        then T.pack (Prelude.show n)
                        else T.pack (Prelude.show d)
encode (Bool True) = "true"
encode (Bool False) = "false"
encode Null = "null"
encode (Array vs) = T.concat ["[", intercalateT "," (Prelude.map encode vs), "]"]
encode (Object m) = T.concat ["{", intercalateT "," (Prelude.map encPair (Map.toList m)), "}"]
  where encPair (Key k, v) = T.concat ["\"", escapeJSON k, "\":", encode v]

-- | Escape special JSON characters in a Text.
escapeJSON :: Text -> Text
escapeJSON = T.concatMap esc
  where
    esc '\\' = "\\\\"
    esc '"'  = "\\\""
    esc '\n' = "\\n"
    esc '\t' = "\\t"
    esc '\r' = "\\r"
    esc c    = T.singleton c

-- | Intercalate Text values.
intercalateT :: Text -> [Text] -> Text
intercalateT _ []     = T.empty
intercalateT _ [x]    = x
intercalateT sep (x:xs) = T.concat [x, sep, intercalateT sep xs]

-- | The result of a JSON conversion.
data Result a = Error Prelude.String | Success a
  deriving (Eq, Show)

-- ---------------------------------------------------------------------------
-- JSON Decoder (recursive descent, pure Haskell)
-- ---------------------------------------------------------------------------

-- | Decode a JSON Text into a Value, or Nothing on parse failure.
decode :: Text -> Maybe Value
decode t = case eitherDecode t of
  Left _  -> Nothing
  Right v -> Just v

-- | Decode a JSON Text into a Value, with error message on failure.
eitherDecode :: Text -> Either Prelude.String Value
eitherDecode t = case parseValue (skipWS t) of
  Nothing       -> Left "JSON parse error: unexpected end of input"
  Just (v, rest)
    | T.null (skipWS rest) -> Right v
    | otherwise -> Left ("JSON parse error: trailing content: " ++ take 20 (T.unpack rest))

-- | Parse a JSON value, returning the value and remaining text.
parseValue :: Text -> Maybe (Value, Text)
parseValue t = case T.uncons t of
  Nothing     -> Nothing
  Just (c, _)
    | c == '"'  -> parseString t
    | c == '{'  -> parseObject t
    | c == '['  -> parseArray t
    | c == 't'  -> parseLit "true" (Bool True) t
    | c == 'f'  -> parseLit "false" (Bool False) t
    | c == 'n'  -> parseLit "null" Null t
    | c == '-' || isDigitC c -> parseNumber t
    | otherwise -> Nothing

-- | Parse a JSON string literal.
parseString :: Text -> Maybe (Value, Text)
parseString t = case T.uncons t of
  Just ('"', rest) -> case parseStringContents rest of
    Just (s, rest') -> Just (String s, rest')
    Nothing         -> Nothing
  _ -> Nothing

-- | Parse string contents up to closing quote.
parseStringContents :: Text -> Maybe (Text, Text)
parseStringContents = go []
  where
    go acc t = case T.uncons t of
      Nothing        -> Nothing  -- unterminated string
      Just ('"', rest) -> Just (T.pack (Prelude.reverse acc), rest)
      Just ('\\', rest) -> case T.uncons rest of
        Just ('"', rest')  -> go ('"' : acc) rest'
        Just ('\\', rest') -> go ('\\' : acc) rest'
        Just ('/', rest')  -> go ('/' : acc) rest'
        Just ('n', rest')  -> go ('\n' : acc) rest'
        Just ('t', rest')  -> go ('\t' : acc) rest'
        Just ('r', rest')  -> go ('\r' : acc) rest'
        Just ('b', rest')  -> go ('\b' : acc) rest'
        Just ('f', rest')  -> go ('\f' : acc) rest'
        Just ('u', rest')  -> case parseHex4 rest' of
          Just (ch, rest'') -> go (ch : acc) rest''
          Nothing           -> Nothing
        _                  -> Nothing  -- invalid escape
      Just (c, rest) -> go (c : acc) rest

-- | Parse 4 hex digits into a Char.
parseHex4 :: Text -> Maybe (Char, Text)
parseHex4 t
  | T.length t < 4 = Nothing
  | otherwise =
      let hex = T.take 4 t
          rest = T.drop 4 t
      in case hexToInt hex of
           Just n  -> Just (Prelude.toEnum n, rest)
           Nothing -> Nothing

hexToInt :: Text -> Maybe Int
hexToInt = T.foldl' step (Just 0)
  where
    step Nothing  _ = Nothing
    step (Just n) c
      | c >= '0' && c <= '9' = Just (n * 16 + (ord c - ord '0'))
      | c >= 'a' && c <= 'f' = Just (n * 16 + (ord c - ord 'a' + 10))
      | c >= 'A' && c <= 'F' = Just (n * 16 + (ord c - ord 'A' + 10))
      | otherwise             = Nothing
    ord = Prelude.fromEnum

-- | Parse a JSON number.
parseNumber :: Text -> Maybe (Value, Text)
parseNumber t =
  let (numStr, rest) = T.span isNumChar t
  in if T.null numStr
     then Nothing
     else case parseDouble numStr of
       Just d  -> Just (Number d, rest)
       Nothing -> Nothing
  where
    isNumChar c = isDigitC c || c == '.' || c == '-' || c == '+' || c == 'e' || c == 'E'

-- | Simple double parser for JSON numbers.
parseDouble :: Text -> Maybe Double
parseDouble t = case T.uncons t of
  Nothing -> Nothing
  Just ('-', rest) -> fmap negate (parsePos rest)
  Just _           -> parsePos t
  where
    parsePos s = case T.break (\c -> c == '.' || c == 'e' || c == 'E') s of
      (intPart, rest)
        | T.null intPart -> Nothing
        | not (T.all isDigitC intPart) -> Nothing
        | T.null rest -> Just (Prelude.fromIntegral (parseDigits intPart))
        | otherwise -> case T.uncons rest of
            Just ('.', afterDot) ->
              let (fracPart, afterFrac) = T.span isDigitC afterDot
              in if T.null fracPart
                 then Just (Prelude.fromIntegral (parseDigits intPart))
                 else let whole = Prelude.fromIntegral (parseDigits intPart) :: Double
                          frac  = Prelude.fromIntegral (parseDigits fracPart) :: Double
                          denom = Prelude.fromIntegral (pow10 (T.length fracPart)) :: Double
                          base  = whole + frac / denom
                      in case T.uncons afterFrac of
                           Just (e, expPart) | e == 'e' || e == 'E' ->
                             applyExp base expPart
                           _ -> Just base
            Just (e, expPart) | e == 'e' || e == 'E' ->
              applyExp (Prelude.fromIntegral (parseDigits intPart)) expPart
            _ -> Nothing
    applyExp base expText = case T.uncons expText of
      Just ('+', ds) -> if T.all isDigitC ds && not (T.null ds)
                        then Just (base * pow10d (parseDigits ds))
                        else Nothing
      Just ('-', ds) -> if T.all isDigitC ds && not (T.null ds)
                        then Just (base / pow10d (parseDigits ds))
                        else Nothing
      Just (d, _) | isDigitC d -> if T.all isDigitC expText
                        then Just (base * pow10d (parseDigits expText))
                        else Nothing
      _ -> Nothing
    parseDigits :: Text -> Int
    parseDigits = T.foldl' (\acc c -> acc * 10 + (Prelude.fromEnum c - 48)) 0
    pow10 :: Int -> Int
    pow10 0 = 1
    pow10 n = 10 * pow10 (n - 1)
    pow10d :: Int -> Double
    pow10d 0 = 1.0
    pow10d n = 10.0 * pow10d (n - 1)

-- | Parse a JSON object.
parseObject :: Text -> Maybe (Value, Text)
parseObject t = case T.uncons t of
  Just ('{', rest) ->
    let rest' = skipWS rest
    in case T.uncons rest' of
      Just ('}', rest'') -> Just (Object Map.empty, rest'')
      _ -> case parseMembers rest' of
        Just (pairs, rest'') -> Just (Object (Map.fromList pairs), rest'')
        Nothing -> Nothing
  _ -> Nothing

parseMembers :: Text -> Maybe ([(Key, Value)], Text)
parseMembers t = case parseMember t of
  Nothing -> Nothing
  Just (pair, rest) ->
    let rest' = skipWS rest
    in case T.uncons rest' of
      Just ('}', rest'') -> Just ([pair], rest'')
      Just (',', rest'') -> case parseMembers (skipWS rest'') of
        Just (pairs, rest''') -> Just (pair : pairs, rest''')
        Nothing -> Nothing
      _ -> Nothing

parseMember :: Text -> Maybe ((Key, Value), Text)
parseMember t = case parseString t of
  Just (String k, rest) ->
    let rest' = skipWS rest
    in case T.uncons rest' of
      Just (':', rest'') -> case parseValue (skipWS rest'') of
        Just (v, rest''') -> Just ((Key k, v), rest''')
        Nothing -> Nothing
      _ -> Nothing
  _ -> Nothing

-- | Parse a JSON array.
parseArray :: Text -> Maybe (Value, Text)
parseArray t = case T.uncons t of
  Just ('[', rest) ->
    let rest' = skipWS rest
    in case T.uncons rest' of
      Just (']', rest'') -> Just (Array [], rest'')
      _ -> case parseElements rest' of
        Just (elems, rest'') -> Just (Array elems, rest'')
        Nothing -> Nothing
  _ -> Nothing

parseElements :: Text -> Maybe ([Value], Text)
parseElements t = case parseValue t of
  Nothing -> Nothing
  Just (v, rest) ->
    let rest' = skipWS rest
    in case T.uncons rest' of
      Just (']', rest'') -> Just ([v], rest'')
      Just (',', rest'') -> case parseElements (skipWS rest'') of
        Just (vs, rest''') -> Just (v : vs, rest''')
        Nothing -> Nothing
      _ -> Nothing

-- | Parse a literal keyword.
parseLit :: Text -> Value -> Text -> Maybe (Value, Text)
parseLit lit val t
  | T.isPrefixOf lit t = Just (val, T.drop (T.length lit) t)
  | otherwise          = Nothing

-- | Skip whitespace.
skipWS :: Text -> Text
skipWS = T.dropWhile (\c -> c == ' ' || c == '\n' || c == '\r' || c == '\t')

isDigitC :: Char -> Bool
isDigitC c = c >= '0' && c <= '9'
