-- | Runtime half of the Suite.hs prepared-corpus oracle generator.
--
-- Each renderer corresponds to exactly one arm of @compare_values@ in
-- @tidepool-testing/src/prepared_corpus.rs@ and emits that arm's
-- @Expectation@ JSON (@{"kind": ..., "value": ...}@). Rendering follows the
-- constructor shape the prepared observer materialises, never 'Show' text:
--
-- * @Int@ is @int@ (compared through @I#@ or a literal word).
-- * @Char@ is @char@ (compared through @C#@ or a canonical word).
-- * @[Char]@ is a @list@ of @char@, because the observation is a @:@/@[]@
--   chain; only @Data.Text.Text@ renders as @text@.
-- * @Double@ is @float64_approx@ with zero tolerance; GHC's 'show' is the
--   shortest round-tripping decimal and serde parses it correctly rounded.
--   Non-finite values have no JSON number and are refused.
-- * Tuples, 'Maybe' and 'Either' are compared by constructor name.
--
-- Anything else has no expectation kind and is refused by the compile-time
-- classifier ("SuiteOracleTH") before a renderer is chosen.
module SuiteOracleRender
  ( OracleEntry (..)
  , Unrepresentable (..)
  , renderInt
  , renderBool
  , renderChar
  , renderDouble
  , renderText
  , renderList
  , renderTuple
  , renderMaybe
  , renderEither
  , errorExpectation
  , jsonString
  ) where

import Control.Exception (Exception, throw)
import Data.Char (ord)
import Data.List (intercalate)
import qualified Data.Text as T
import Numeric (showHex)

-- | One manifest expectation key, resolved against Suite's own scope.
data OracleEntry
  = -- | Not a name Suite declares: a simplifier float, worker, dictionary,
    -- wrapper or other compiler-introduced top. No source oracle exists.
    CompilerIntroduced
  | -- | A source top whose type is a function or is polymorphic.
    SourceNotClosed String
  | -- | A closed source top whose type has no expectation kind.
    SourceUnrepresentable String
  | -- | A closed source top: an action forcing it to weak head normal form,
    -- and its lazily rendered expectation.
    SourceValue (IO ()) String

-- | Raised while rendering a value that has no JSON expectation.
newtype Unrepresentable = Unrepresentable String
  deriving Show

instance Exception Unrepresentable

expectation :: String -> String -> String
expectation kind value =
  "{\"kind\":" ++ jsonString kind ++ ",\"value\":" ++ value ++ "}"

renderInt :: Int -> String
renderInt n = expectation "int" (show n)

renderBool :: Bool -> String
renderBool b = expectation "bool" (if b then "true" else "false")

renderChar :: Char -> String
renderChar c = expectation "char" (jsonString [scalar c])

renderDouble :: Double -> String
renderDouble x
  | isNaN x || isInfinite x =
      throw (Unrepresentable ("non-finite Double " ++ show x))
  | otherwise =
      expectation "float64_approx"
        ("{\"expected\":" ++ show x ++ ",\"absolute_tolerance\":0.0}")

renderText :: T.Text -> String
renderText t = expectation "text" (jsonString (map scalar (T.unpack t)))

renderList :: (a -> String) -> [a] -> String
renderList render xs =
  expectation "list" ("[" ++ intercalate "," (map render xs) ++ "]")

renderTuple :: [String] -> String
renderTuple fields = expectation "tuple" ("[" ++ intercalate "," fields ++ "]")

renderMaybe :: (a -> String) -> Maybe a -> String
renderMaybe _ Nothing = expectation "maybe" "null"
renderMaybe render (Just x) = expectation "maybe" (render x)

renderEither :: (a -> String) -> (b -> String) -> Either a b -> String
renderEither left _ (Left x) = expectation "either_left" (left x)
renderEither _ right (Right y) = expectation "either_right" (right y)

-- | @ExpectedFailure@ is snake_case: @"raised_exception"@ or @"blackhole"@.
errorExpectation :: String -> String
errorExpectation failure = expectation "error" (jsonString failure)

-- | A Haskell 'Char' may be a surrogate code point, which neither JSON text
-- nor a Rust @char@ can carry.
scalar :: Char -> Char
scalar c
  | ord c >= 0xD800 && ord c <= 0xDFFF =
      throw (Unrepresentable ("surrogate code point U+" ++ showHex (ord c) ""))
  | otherwise = c

-- | ASCII-only JSON string: every non-ASCII scalar is a @\\u@ escape (a
-- surrogate pair above the BMP), so output never depends on locale encoding.
jsonString :: String -> String
jsonString s = "\"" ++ concatMap escape s ++ "\""
  where
    escape '"' = "\\\""
    escape '\\' = "\\\\"
    escape '\n' = "\\n"
    escape '\r' = "\\r"
    escape '\t' = "\\t"
    escape c
      | ord c < 0x20 || (ord c >= 0x7F && ord c <= 0xFFFF) = unit (ord c)
      | ord c > 0xFFFF =
          let v = ord c - 0x10000
           in unit (0xD800 + v `div` 0x400) ++ unit (0xDC00 + v `mod` 0x400)
      | otherwise = [c]
    unit n = let h = showHex n "" in "\\u" ++ replicate (4 - length h) '0' ++ h
