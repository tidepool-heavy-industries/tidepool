{-# LANGUAGE OverloadedStrings #-}
{-# LANGUAGE BangPatterns #-}
{-# LANGUAGE FlexibleInstances #-}
{-# LANGUAGE FlexibleContexts #-}
{-# LANGUAGE DefaultSignatures #-}
{-# LANGUAGE TypeOperators #-}
{-# LANGUAGE DataKinds #-}
{-# LANGUAGE UndecidableInstances #-}
{-# LANGUAGE TypeFamilies #-}
{-# LANGUAGE KindSignatures #-}
{-# LANGUAGE ScopedTypeVariables #-}
-- | Vendored aeson Value type with construction.
--
-- This module provides the core JSON Value type and construction helpers.
--
-- Differences from upstream aeson:
--   - Array uses [Value] instead of V.Vector Value (avoids Array# primop)
--   - KeyMap uses Data.Map.Strict instead of HashMap (avoids hash primops)
module Tidepool.Aeson.Value
  ( -- * Core types
    Value(..)
  , Scientific
  , scientific
  , coefficient
  , base10Exponent
  , fromFloatDigits
  , toRealFloat
  , Key
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
    -- * Decoding (primop anchor; the public decoder is
    --   'Tidepool.Aeson.FromJSON.eitherDecode')
  , eitherDecodeValue
    -- * ToJSON class
  , ToJSON(..)
  , GToJSON(..)
  , GAllFieldsNamed
  , IsNullarySum
  , IsRecordCon
  , genericToJSON
  ) where

import Prelude
import Data.Text (Text)
import qualified Tidepool.Data.Text as T
import qualified Data.Map.Strict as Map
import qualified Data.Set as Set
import Data.Char (chr, ord)
import Tidepool.Aeson.Scientific
  ( Scientific, scientific, coefficient, base10Exponent
  , fromFloatDigits, toRealFloat, isFiniteDouble )
import Data.Kind (Type)
import Data.Proxy (Proxy(..))
import GHC.Generics
import GHC.TypeLits (TypeError, ErrorMessage(Text, (:<>:)))

-- | A JSON object key. Transparently Text (like 'FilePath') — object keys are
-- just Text, so @KM.lookup "k" m@ and @KM.keys@ work directly with no wrapper.
type Key = Text

-- | Convert Text to a Key. Identity (kept for call-site compatibility).
fromText :: Text -> Key
fromText = id

-- | Convert a Key back to Text. Identity (kept for call-site compatibility).
toText :: Key -> Text
toText = id

-- | KeyMap backed by Data.Map.Strict (avoids HashMap primop issues).
type KeyMap v = Map.Map Key v

-- | A JSON object.
type Object = KeyMap Value

-- | A JSON array (uses list instead of Vector to avoid Array# primops).
type Array = [Value]

-- | A key-value pair for building objects.
type Pair = (Key, Value)

-- | A JSON value. @Number@ carries an exact 'Scientific'
-- (@coefficient * 10 ^ base10Exponent@) — integer and fractional JSON numbers
-- share one aeson-faithful representation, with no @Double@ round-trip and no
-- Int64 cap (arbitrary-precision integers run on the JIT via @tidepool-bignum@).
data Value
  = Object !Object
  | Array Array
  | String !Text
  | Number !Scientific
  | Bool !Bool
  | Null
  deriving (Eq, Ord, Show)

-- | Construct a JSON object from key-value pairs.
object :: [Pair] -> Value
object = Object . Map.fromList

-- | Pair a text key with a JSON-encodable value.
(.=) :: ToJSON v => Text -> v -> Pair
k .= v = (k, toJSON v)
infixr 8 .=

-- | Empty JSON object.
emptyObject :: Value
emptyObject = Object Map.empty

-- | Empty JSON array.
emptyArray :: Value
emptyArray = Array []

-- | Decode a JSON document into an @'Either' 'Text' 'Value'@ — @Right v@ on
-- success, @Left msg@ (approximating the serde_json parse error) on failure.
-- This is the internal primop anchor that the public
-- @'Tidepool.Aeson.FromJSON.eitherDecode'@ is derived from — prefer that on
-- the surface.
--
-- PURE — no effect. On the Core route, calls to @eitherDecodeValue@ are
-- intercepted in the extractor (Translate.hs) and lowered to the
-- @JsonDecode@ primop, which dispatches to Rust @serde_json@ and builds the
-- ADT directly on the heap — the body below never runs there. No such
-- interception exists on the prepared-STG route (the default engine): a
-- prepared program compiles this module from source and runs the body
-- verbatim, so it must be a real, total RFC 8259 parser, not a stub. The two
-- routes are kept behaviorally equivalent (same 'Value' shapes, same
-- 'Scientific' normal form via the 'scientific' smart constructor, same
-- accept/reject grammar) but are NOT bit-for-bit identical on error message
-- text: the prepared route's messages are hand-written, approximating
-- serde_json's wording rather than reproducing it, which is fine because
-- every caller only distinguishes @Left@ from @Right@ (see
-- @STG_KNOWN_ISSUES.md@, \"Host answers and effects\").
--
-- OPAQUE (not merely NOINLINE): the Core-route interceptor matches the name
-- @eitherDecodeValue@, but returning @Either@ makes GHC's CPR worker/wrapper
-- split it into @$weitherDecodeValue@ at -O2 — which the interceptor would
-- miss. OPAQUE forbids w/w (and specialization/inlining), so the wrapper
-- name survives verbatim into every caller's Core, on both routes.
{-# OPAQUE eitherDecodeValue #-}
eitherDecodeValue :: Text -> Either Text Value
eitherDecodeValue input = case jsonParseTopLevel input of
  Left err -> Left (T.pack err)
  Right v  -> Right v

-- ---------------------------------------------------------------------------
-- RFC 8259 JSON parser (recursive descent over 'Text').
--
-- Used only on the prepared-STG route (the Core route intercepts
-- 'eitherDecodeValue' before this body ever runs). Errors are threaded as
-- plain 'String' internally and packed to 'Text' once, at the top
-- ('eitherDecodeValue') — 'T.pack' on a literal is ambiguous under
-- @Tidepool.Data.Text@'s polymorphic 'Pack' class, so error TEXT is built as
-- 'String' (via @(++)@ / 'show') and only the final result crosses to 'Text'.
--
-- Every scan uses either 'T.uncons' (no predicate closure, always safe to
-- call on an externally-defined 'Text') or the home-compiled predicate
-- family ('T.span', 'T.dropWhile', ...) that "Tidepool.Data.Text" vendors
-- specifically so a HOME module's predicate closures don't cross into
-- external-package unfoldings under the JIT (see that module's Haddock).
-- Nothing here uses 'Data.Text.foldl'' or similar EXTERNAL higher-order
-- functions with a locally-defined closure argument.
-- ---------------------------------------------------------------------------

-- | Maximum object/array nesting depth, matching serde_json's default
-- recursion limit (128) — see @tidepool-bridge/src/json_builder.rs@'s
-- @json_to_value@ Haddock (\"Recursion depth is bounded by serde_json's own
-- nesting limit (128 by default)\").
jsonMaxDepth :: Int
jsonMaxDepth = 128

-- | Parse a complete JSON document: one value, optional surrounding
-- whitespace, and nothing else. Trailing non-whitespace content is a parse
-- error, matching @serde_json::from_str@'s whole-input contract.
jsonParseTopLevel :: Text -> Either String Value
jsonParseTopLevel input = do
  (v, rest) <- jsonValue 0 (jsonSkipWs input)
  let rest' = jsonSkipWs rest
  if T.null rest'
    then Right v
    else Left "trailing characters after JSON value"

jsonSkipWs :: Text -> Text
jsonSkipWs = T.dropWhile jsonIsWs

jsonIsWs :: Char -> Bool
jsonIsWs c = c == ' ' || c == '\t' || c == '\n' || c == '\r'

jsonIsDigit :: Char -> Bool
jsonIsDigit c = c >= '0' && c <= '9'

-- | One JSON value, plus the unconsumed remainder. Literal keywords
-- (@true@/@false@/@null@) are matched before falling into the
-- structural/number dispatch so a truncated keyword (e.g. @"tru"@) is
-- rejected rather than misparsed as something else.
jsonValue :: Int -> Text -> Either String (Value, Text)
jsonValue depth t
  | Just rest <- T.stripPrefix "true" t  = Right (Bool True, rest)
  | Just rest <- T.stripPrefix "false" t = Right (Bool False, rest)
  | Just rest <- T.stripPrefix "null" t  = Right (Null, rest)
  | otherwise = case T.uncons t of
      Nothing -> Left "unexpected end of input"
      Just ('"', rest) -> do
        (s, rest') <- jsonStringBody rest
        Right (String s, rest')
      Just ('{', rest) -> jsonObject (depth + 1) rest
      Just ('[', rest) -> jsonArray (depth + 1) rest
      Just (c, _) | c == '-' || jsonIsDigit c -> jsonNumber t
      Just (c, _) -> Left ("unexpected character " ++ show c ++ " in JSON value")

-- | An object body, just past the opening @{@.
jsonObject :: Int -> Text -> Either String (Value, Text)
jsonObject depth t
  | depth > jsonMaxDepth = Left "exceeded maximum nesting depth"
  | otherwise = case T.uncons (jsonSkipWs t) of
      Just ('}', rest) -> Right (Object Map.empty, rest)
      _ -> jsonMembers depth (jsonSkipWs t) Map.empty

-- | One or more @"key": value@ members, comma-separated, ending in @}@. A
-- trailing comma (e.g. @{"a":1,}@) is rejected: after consuming a comma this
-- always requires another string key, and @}@ is not one.
jsonMembers :: Int -> Text -> Object -> Either String (Value, Text)
jsonMembers depth t acc = do
  (key, t1) <- jsonStringAt (jsonSkipWs t)
  t2 <- jsonExpect ':' (jsonSkipWs t1)
  (val, t3) <- jsonValue depth (jsonSkipWs t2)
  -- Duplicate keys: last write wins (matches serde_json's `Map::insert`
  -- accumulation and `Data.Map.Strict.insert`'s own overwrite-on-collision
  -- semantics), so no special-casing is needed beyond a plain 'Map.insert'.
  let acc' = Map.insert key val acc
  case T.uncons (jsonSkipWs t3) of
    Just (',', rest) -> jsonMembers depth rest acc'
    Just ('}', rest) -> Right (Object acc', rest)
    _ -> Left "expected ',' or '}' in object"

-- | An array body, just past the opening @[@.
jsonArray :: Int -> Text -> Either String (Value, Text)
jsonArray depth t
  | depth > jsonMaxDepth = Left "exceeded maximum nesting depth"
  | otherwise = case T.uncons (jsonSkipWs t) of
      Just (']', rest) -> Right (Array [], rest)
      _ -> do
        (v, t1) <- jsonValue depth (jsonSkipWs t)
        jsonElements depth t1 [v]

-- | One or more comma-separated elements, ending in @]@. A trailing comma
-- (e.g. @[1,]@) is rejected the same way 'jsonMembers' rejects one: after a
-- comma another value is always required.
jsonElements :: Int -> Text -> [Value] -> Either String (Value, Text)
jsonElements depth t acc = case T.uncons (jsonSkipWs t) of
  Just (',', rest) -> do
    (v, t1) <- jsonValue depth (jsonSkipWs rest)
    jsonElements depth t1 (v : acc)
  Just (']', rest) -> Right (Array (reverse acc), rest)
  _ -> Left "expected ',' or ']' in array"

jsonExpect :: Char -> Text -> Either String Text
jsonExpect c t = case T.uncons t of
  Just (c', rest) | c' == c -> Right rest
  _ -> Left ("expected " ++ show c)

-- | A string, including its opening quote.
jsonStringAt :: Text -> Either String (Text, Text)
jsonStringAt t = case T.uncons t of
  Just ('"', rest) -> jsonStringBody rest
  _ -> Left "expected string"

-- | A string body, just past the opening quote: alternating runs of plain
-- (unescaped, non-control) characters and single escape sequences, ending at
-- the closing quote. Built as a list of already-decoded 'Text' chunks and
-- 'T.concat'ed once at the end, so this stays linear in the string length —
-- no per-character 'T.append', no 'T.length'/'T.index' re-scans.
jsonStringBody :: Text -> Either String (Text, Text)
jsonStringBody = go []
  where
    go acc t = case T.span jsonIsPlainStringChar t of
      (chunk, rest) -> case T.uncons rest of
        Nothing -> Left "unterminated string literal"
        Just ('"', rest') -> Right (T.concat (reverse (chunk : acc)), rest')
        Just ('\\', rest') -> do
          (esc, rest'') <- jsonEscape rest'
          go (esc : chunk : acc) rest''
        Just (c, _) -> Left ("control character " ++ show c ++ " in string literal")

jsonIsPlainStringChar :: Char -> Bool
jsonIsPlainStringChar c = c /= '"' && c /= '\\' && not (jsonIsControl c)

jsonIsControl :: Char -> Bool
jsonIsControl c = c < '\x20'

-- | One escape sequence, just past the backslash.
jsonEscape :: Text -> Either String (Text, Text)
jsonEscape t = case T.uncons t of
  Nothing -> Left "unterminated escape sequence"
  Just ('"', rest)  -> Right ("\"", rest)
  Just ('\\', rest) -> Right ("\\", rest)
  Just ('/', rest)  -> Right ("/", rest)
  Just ('b', rest)  -> Right ("\b", rest)
  Just ('f', rest)  -> Right ("\f", rest)
  Just ('n', rest)  -> Right ("\n", rest)
  Just ('r', rest)  -> Right ("\r", rest)
  Just ('t', rest)  -> Right ("\t", rest)
  Just ('u', rest)  -> jsonUnicodeEscape rest
  Just (c, _)       -> Left ("invalid escape character " ++ show c)

-- | @\\uXXXX@, just past the @u@. A high surrogate (@0xD800@-@0xDBFF@) must
-- be immediately followed by a @\\uXXXX@ low surrogate (@0xDC00@-@0xDFFF@),
-- combined into the astral code point; either surrogate appearing alone is
-- rejected, matching serde_json (which errors on a lone surrogate rather
-- than substituting U+FFFD).
jsonUnicodeEscape :: Text -> Either String (Text, Text)
jsonUnicodeEscape t = do
  (n, rest) <- jsonHex4 t
  if n >= 0xD800 && n <= 0xDBFF
    then case T.stripPrefix "\\u" rest of
      Just rest2 -> do
        (n2, rest3) <- jsonHex4 rest2
        if n2 >= 0xDC00 && n2 <= 0xDFFF
          then
            let cp = 0x10000 + (n - 0xD800) * 0x400 + (n2 - 0xDC00)
            in Right (T.singleton (chr cp), rest3)
          else Left "low surrogate must follow a high surrogate"
      Nothing -> Left "unpaired high surrogate in \\u escape"
    else
      if n >= 0xDC00 && n <= 0xDFFF
        then Left "unpaired low surrogate in \\u escape"
        else Right (T.singleton (chr n), rest)

jsonHex4 :: Text -> Either String (Int, Text)
jsonHex4 t0 = do
  (d0, t1) <- jsonHexDigit t0
  (d1, t2) <- jsonHexDigit t1
  (d2, t3) <- jsonHexDigit t2
  (d3, t4) <- jsonHexDigit t3
  Right (((d0 * 16 + d1) * 16 + d2) * 16 + d3, t4)

jsonHexDigit :: Text -> Either String (Int, Text)
jsonHexDigit t = case T.uncons t of
  Just (c, rest)
    | c >= '0' && c <= '9' -> Right (ord c - ord '0', rest)
    | c >= 'a' && c <= 'f' -> Right (ord c - ord 'a' + 10, rest)
    | c >= 'A' && c <= 'F' -> Right (ord c - ord 'A' + 10, rest)
  _ -> Left "invalid \\u escape (expected 4 hex digits)"

-- | A JSON number: @[-] int [frac] [exp]@ per RFC 8259 — no leading zeros
-- (@01@), no leading @+@, no bare @.5@/@5.@ (a digit is required on both
-- sides of the decimal point). Mirrors
-- @tidepool-bridge/src/shapes.rs@'s @parse_decimal_token@: the coefficient
-- is the integer-part digits followed by the fractional-part digits (leading
-- zeros immaterial to the 'Integer' value), and the base-10 exponent is the
-- parsed @exp@ shifted down by the fractional digit count — so @1.50e2@ and
-- @150@ both parse to the numeric value 150 (coefficient\/exponent pair need
-- not match bit-for-bit: 'Scientific'\'s 'Eq' compares by value via
-- 'toRational', and its 'Show' re-normalizes via 'stripZeros' at render
-- time, so any faithful pair renders and compares identically).
jsonNumber :: Text -> Either String (Value, Text)
jsonNumber t = do
  (isNeg, t1) <- jsonOptMinus t
  (intDigits, t2) <- jsonIntPart t1
  (fracDigits, t3) <- jsonOptFrac t2
  (expValue, t4) <- jsonOptExp t3
  let maxIntBound = toInteger (maxBound :: Int)
      minIntBound = toInteger (minBound :: Int)
  if expValue > maxIntBound || expValue < minIntBound
    then Left "unparseable exponent in JSON number"
    else
      let magnitude = jsonDigitsToInteger (intDigits `T.append` fracDigits)
          coeff = if isNeg then negate magnitude else magnitude
          expo = fromInteger expValue - T.length fracDigits
      in Right (Number (scientific coeff expo), t4)

jsonOptMinus :: Text -> Either String (Bool, Text)
jsonOptMinus t = case T.uncons t of
  Just ('-', rest) -> Right (True, rest)
  _ -> Right (False, t)

-- | The integer part: @0@ alone, or a leading @1@-@9@ digit followed by any
-- number of further digits. A second digit right after a leading @0@ (e.g.
-- @01@) is deliberately NOT consumed here — it is left in the remainder,
-- where the caller (expecting @.@/@e@/@E@/end-of-value) rejects it.
jsonIntPart :: Text -> Either String (Text, Text)
jsonIntPart t = case T.uncons t of
  Just ('0', rest) -> Right ("0", rest)
  Just (c, _) | jsonIsDigit c -> Right (T.span jsonIsDigit t)
  _ -> Left "expected a digit"

-- | An optional @.digits@ fractional part; if the @.@ is present at least
-- one digit must follow.
jsonOptFrac :: Text -> Either String (Text, Text)
jsonOptFrac t = case T.uncons t of
  Just ('.', rest) -> case T.span jsonIsDigit rest of
    (digits, rest') | not (T.null digits) -> Right (digits, rest')
    _ -> Left "expected a digit after the decimal point"
  _ -> Right (T.empty, t)

-- | An optional @e@/@E@ exponent, with an optional sign; if present, at
-- least one digit must follow the (optional) sign.
jsonOptExp :: Text -> Either String (Integer, Text)
jsonOptExp t = case T.uncons t of
  Just (e, rest) | e == 'e' || e == 'E' -> case T.uncons rest of
    Just (s, rest2) | s == '+' || s == '-' -> jsonExpDigits (s == '-') rest2
    _ -> jsonExpDigits False rest
  _ -> Right (0, t)
  where
    jsonExpDigits isNeg t' = case T.span jsonIsDigit t' of
      (digits, rest') | not (T.null digits) ->
        let val = jsonDigitsToInteger digits
        in Right (if isNeg then negate val else val, rest')
      _ -> Left "expected a digit in the exponent"

-- | Decimal digits (most-significant first) to an 'Integer', via
-- 'T.uncons' only — no external higher-order function (e.g. 'T.foldl'')
-- receives a locally-defined closure here.
jsonDigitsToInteger :: Text -> Integer
jsonDigitsToInteger = go 0
  where
    go !acc t = case T.uncons t of
      Nothing -> acc
      Just (c, rest) -> go (acc * 10 + toInteger (ord c - ord '0')) rest

-- | A class for types that can be converted to JSON Value.
--
-- The default method encodes generically via 'GHC.Generics':
-- @data Rec = Rec {..} deriving (Generic, ToJSON)@ builds a field-name-keyed
-- JSON object; @data Mode = Observing | Deciding | Acting deriving (Generic,
-- ToJSON)@ (a nullary sum — every constructor has no fields, i.e. an enum)
-- encodes each constructor as its bare name string. A sum with payload
-- constructors encodes as aeson's default @TaggedObject@ shape — a JSON
-- object with a @"tag"@ field naming the constructor and the constructor's
-- RECORD fields alongside it (a POSITIONAL payload constructor nests its
-- fields under @"contents"@ instead — bare for one field, an array for
-- several, aeson's default) — SYMMETRIC with what
-- 'Tidepool.Aeson.FromJSON.FromJSON'\'s default decodes.
class ToJSON a where
  toJSON :: a -> Value
  default toJSON :: (Generic a, GToJSON (Rep a)) => a -> Value
  toJSON = genericToJSON

-- | Encode a single-constructor record as a field-name-keyed JSON object, a
-- nullary-sum (enum) as its constructor-name string, or a payload sum as a
-- tagged object. This is the implementation behind the 'ToJSON' default
-- method.
genericToJSON :: (Generic a, GToJSON (Rep a)) => a -> Value
genericToJSON = gToJSON . from

-- | Encode a 'GHC.Generics' representation. @M1 D@/@M1 C@ are the outer layers;
-- the record fields underneath emit @[Pair]@ via 'GToRecord'.
class GToJSON f where
  gToJSON :: f a -> Value

-- | Emit the record fields of one constructor as JSON object pairs.
class GToRecord f where
  gToRecord :: f a -> [Pair]

instance GToJSON f => GToJSON (M1 D d f) where
  gToJSON (M1 x) = gToJSON x

instance GToRecord f => GToJSON (M1 C c f) where
  gToJSON (M1 x) = object (gToRecord x)

instance (GToRecord a, GToRecord b) => GToRecord (a :*: b) where
  gToRecord (a :*: b) = gToRecord a ++ gToRecord b

instance (Selector s, ToJSON c) => GToRecord (M1 S s (K1 R c)) where
  gToRecord m@(M1 (K1 c)) = [(T.pack (selName m), toJSON c)]

instance GToRecord U1 where
  gToRecord _ = []

-- Sum types: a NULLARY sum (every constructor has no fields — an enum)
-- encodes as its constructor-name string via 'GSumNullaryToJSON'; a sum with
-- payload constructors encodes as the tagged-object shape via
-- 'GToJSONTaggedSum'. 'IsNullarySum' decides which branch of 'GToJSONSum'
-- applies — mirroring the decode split in "Tidepool.Aeson.FromJSON".
instance GToJSONSum (IsNullarySum (a :+: b)) (a :+: b) => GToJSON (a :+: b) where
  gToJSON = gToJSONSum (Proxy :: Proxy (IsNullarySum (a :+: b)))

-- | Does every constructor reachable through this sum skeleton carry zero
-- fields (@M1 C c U1@)? Computed structurally over the '(:+:)' tree so it
-- works for any number of constructors, not just two.
type family IsNullarySum (f :: Type -> Type) :: Bool where
  IsNullarySum (a :+: b) = IsNullarySumAnd (IsNullarySum a) (IsNullarySum b)
  IsNullarySum (M1 C c U1) = 'True
  IsNullarySum (M1 C c f) = 'False

type family IsNullarySumAnd (a :: Bool) (b :: Bool) :: Bool where
  IsNullarySumAnd 'True 'True = 'True
  IsNullarySumAnd a b = 'False

-- | Does this constructor's payload use RECORD syntax (named selectors)?
-- Haskell guarantees all-or-nothing per constructor, so inspecting the first
-- field suffices. A nullary constructor counts as a (trivially empty) record
-- — its encoding is the tag-only object either way. Shared by the encode
-- ("Tidepool.Aeson.Value"), decode ("Tidepool.Aeson.FromJSON"), and schema
-- ("Tidepool.Aeson.Schema") sides so all three take the same branch.
type family IsRecordCon (f :: Type -> Type) :: Bool where
  IsRecordCon U1 = 'True
  IsRecordCon (a :*: b) = IsRecordCon a
  IsRecordCon (M1 S ('MetaSel ('Just n) su ss ds) f) = 'True
  IsRecordCon (M1 S ('MetaSel 'Nothing su ss ds) f) = 'False

-- | Dispatch on whether a sum is all-nullary: 'True' routes to the
-- constructor-name encoder, 'False' to the tagged-object encoder.
class GToJSONSum (allNullary :: Bool) f where
  gToJSONSum :: Proxy allNullary -> f a -> Value

instance GSumNullaryToJSON f => GToJSONSum 'True f where
  gToJSONSum _ = gSumNullaryToJSON

instance GToJSONTaggedSum f => GToJSONSum 'False f where
  gToJSONSum _ = gToJSONTaggedSum

-- | aeson's default @TaggedObject@ shape, encode side: one JSON object per
-- value carrying @"tag": <constructor name>@ plus the constructor's payload —
-- the exact shape "Tidepool.Aeson.FromJSON"\'s @GFromJSONTaggedSum@ decodes,
-- so a payload sum round-trips through the two defaults. A nullary
-- constructor in a mixed sum is an object carrying only @tag@. A RECORD
-- constructor's named fields sit alongside @tag@ (upstream's "records are
-- unpacked in the tagged object"); a POSITIONAL constructor's fields nest
-- under @"contents"@ — the single value bare for one field, an array for
-- several — matching upstream's non-record TaggedObject encoding.
-- 'IsRecordCon' picks the branch per constructor.
class GToJSONTaggedSum f where
  gToJSONTaggedSum :: f a -> Value

instance (GToJSONTaggedSum a, GToJSONTaggedSum b) => GToJSONTaggedSum (a :+: b) where
  gToJSONTaggedSum (L1 x) = gToJSONTaggedSum x
  gToJSONTaggedSum (R1 x) = gToJSONTaggedSum x

instance (Constructor c, GToJSONTaggedCon (IsRecordCon f) f) => GToJSONTaggedSum (M1 C c f) where
  gToJSONTaggedSum m@(M1 x) =
    object
      ((T.pack "tag", String (T.pack (conName m)))
         : gTaggedConPairs (Proxy :: Proxy (IsRecordCon f)) x)

-- | One constructor's payload as tagged-object pairs, dispatched on
-- 'IsRecordCon': record fields inline beside @tag@, positional fields under
-- one @"contents"@ key.
class GToJSONTaggedCon (isRecord :: Bool) f where
  gTaggedConPairs :: Proxy isRecord -> f a -> [Pair]

instance (GToRecord f, GAllFieldsNamed f) => GToJSONTaggedCon 'True f where
  gTaggedConPairs _ = gToRecord

instance GToPositional f => GToJSONTaggedCon 'False f where
  gTaggedConPairs _ x = [(T.pack "contents", wrap (gToPositional x))]
    where
      wrap [v] = v
      wrap vs = Array vs

-- | A positional payload's field values, in declaration order.
class GToPositional f where
  gToPositional :: f a -> [Value]

instance (GToPositional a, GToPositional b) => GToPositional (a :*: b) where
  gToPositional (a :*: b) = gToPositional a ++ gToPositional b

instance ToJSON c => GToPositional (M1 S s (K1 R c)) where
  gToPositional (M1 (K1 c)) = [toJSON c]

-- | For RECORD payload constructors only (the 'IsRecordCon' @'True@ branch):
-- proof that no field is named @tag@ (reserved for the discriminator). The
-- positional-field instance below is unreachable for a well-formed type —
-- Haskell constructors are all-record or all-positional, and positional
-- constructors take the @"contents"@ branch — and is kept as a defensive
-- backstop.
class GAllFieldsNamed (f :: Type -> Type)
instance GAllFieldsNamed U1
instance (GAllFieldsNamed a, GAllFieldsNamed b) => GAllFieldsNamed (a :*: b)
instance
  {-# OVERLAPPING #-}
  TypeError
    ( 'Text "generic JSON: a record field named `tag` cannot appear in a payload constructor of a sum; "
        ':<>: 'Text "`tag` is reserved for the constructor discriminator. Rename the field."
    ) =>
  GAllFieldsNamed (M1 S ('MetaSel ('Just "tag") su ss ds) (K1 R c))
instance {-# OVERLAPPABLE #-} GAllFieldsNamed (M1 S ('MetaSel ('Just name) su ss ds) (K1 R c))
instance
  TypeError ('Text "generic JSON: a payload constructor in a sum must use record syntax "
             ':<>: 'Text "(named fields) — positional fields have no JSON key.")
    => GAllFieldsNamed (M1 S ('MetaSel 'Nothing su ss ds) (K1 R c))

-- | Encode a nullary-constructors-only sum leaf/branch as its constructor
-- name. Only reachable once 'IsNullarySum' has established every constructor
-- in the sum is nullary.
class GSumNullaryToJSON f where
  gSumNullaryToJSON :: f a -> Value

instance (GSumNullaryToJSON a, GSumNullaryToJSON b) => GSumNullaryToJSON (a :+: b) where
  gSumNullaryToJSON (L1 x) = gSumNullaryToJSON x
  gSumNullaryToJSON (R1 x) = gSumNullaryToJSON x

instance Constructor c => GSumNullaryToJSON (M1 C c U1) where
  gSumNullaryToJSON m = String (T.pack (conName m))

instance ToJSON Value where
  toJSON = id

instance ToJSON Text where
  toJSON = String

instance ToJSON Int where
  toJSON n = Number (scientific (fromIntegral n) 0)

-- Non-finite Doubles have no numeric JSON representation; upstream aeson
-- encodes them as Null, so we match that instead of letting 'fromFloatDigits'
-- parse the letters of "NaN"/"Infinity" as decimal digits (see
-- 'Tidepool.Aeson.Scientific.isFiniteDouble').
instance ToJSON Double where
  toJSON d
    | isFiniteDouble d = Number (fromFloatDigits d)
    | otherwise         = Null

instance ToJSON Float where
  toJSON f
    | isFiniteDouble (realToFrac f) = Number (fromFloatDigits f)
    | otherwise                      = Null

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
  -- Exact at any magnitude: 'Scientific' carries the full 'Integer' coefficient.
  toJSON n = Number (scientific n 0)

instance ToJSON Word where
  toJSON n = Number (scientific (fromIntegral n) 0)

instance ToJSON Char where
  toJSON c = String (T.singleton c)

instance ToJSON Ordering where
  toJSON LT = String "LT"
  toJSON EQ = String "EQ"
  toJSON GT = String "GT"

instance (ToJSON a, ToJSON b) => ToJSON (Either a b) where
  toJSON (Left a)  = Object (Map.singleton "Left" (toJSON a))
  toJSON (Right b) = Object (Map.singleton "Right" (toJSON b))

instance (ToJSON a, ToJSON b) => ToJSON (a, b) where
  toJSON (a, b) = Array [toJSON a, toJSON b]

instance (ToJSON a, ToJSON b, ToJSON c) => ToJSON (a, b, c) where
  toJSON (a, b, c) = Array [toJSON a, toJSON b, toJSON c]

instance (ToJSON a, ToJSON b, ToJSON c, ToJSON d) => ToJSON (a, b, c, d) where
  toJSON (a, b, c, d) = Array [toJSON a, toJSON b, toJSON c, toJSON d]

instance (ToJSON a, ToJSON b, ToJSON c, ToJSON d, ToJSON e) => ToJSON (a, b, c, d, e) where
  toJSON (a, b, c, d, e) = Array [toJSON a, toJSON b, toJSON c, toJSON d, toJSON e]

instance ToJSON a => ToJSON (Map.Map Text a) where
  toJSON m = Object (Map.map toJSON m)  -- keys are already Text (= Key)

instance ToJSON a => ToJSON (Set.Set a) where
  toJSON = Array . map toJSON . Set.toList
