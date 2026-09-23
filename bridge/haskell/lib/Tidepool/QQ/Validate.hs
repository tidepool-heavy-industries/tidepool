{-# LANGUAGE TemplateHaskellQuotes #-}
{-# LANGUAGE OverloadedStrings #-}

-- | Validator quasi-quoter: @[uri|...|]@.
--
-- A thin wrapper around 'mkValidatorQQ': a compile-time CHECK over the quote
-- body, and on success an emitted 'Data.Text.Text' literal — exactly the bytes
-- you wrote.  The point is to move a class of silent runtime traps (a
-- scheme-less URI) to a precise COMPILE error, while still producing an
-- ordinary 'Text' the JIT runs as a plain string literal.
--
-- The checker runs inside the splice evaluator (full GHC available), so it may
-- use library predicates freely — nothing here is translated to Core.  Only
-- the emitted @'Data.Text.pack' "…"@ ever reaches the JIT.
--
-- == What the checker enforces
--
--   * @uri@ — must start @http:\/\/@ or @https:\/\/@, have a non-empty host,
--     and contain no whitespace.  Structural only.
--
-- (A @[glob|...|]@ quoter is deliberately OMITTED: the name collides with the
-- eval-visible @glob :: Text -> M [Text]@ Fs verb.)
module Tidepool.QQ.Validate
  ( uri
  , mkValidatorQQ
  , mkValidatorQQWith
  ) where

import Data.Text (Text)
import qualified Tidepool.Data.Text as T

import Language.Haskell.TH (Exp, Q, litE, stringL)
import Language.Haskell.TH.Quote (QuasiQuoter (..))

-- | Build a validator quasi-quoter that emits the checked 'Text' literal.
mkValidatorQQ :: String -> (Text -> Either Text ()) -> QuasiQuoter
mkValidatorQQ name check = mkValidatorQQWith name check $ \s ->
  [| T.pack $(litE (stringL s)) |]

-- | Validate at splice time, then emit the checked expression.
mkValidatorQQWith :: String -> (Text -> Either Text ()) -> (String -> Q Exp) -> QuasiQuoter
mkValidatorQQWith name check emit = QuasiQuoter
  { quoteExp  = checkExp
  , quotePat  = \_ -> fail (name ++ ": cannot be used in pattern position (use it in expression position)")
  , quoteType = \_ -> fail (name ++ ": cannot be used in a type context")
  , quoteDec  = \_ -> fail (name ++ ": cannot be used in a declaration context")
  }
  where
    checkExp :: String -> Q Exp
    checkExp s = case check (T.pack s) of
      Right ()  -> emit s
      Left msg  -> fail (name ++ ": " ++ T.unpack msg)

-- | @[uri| https://example.com/x |]@ — an http(s) URI, structure checked.
uri :: QuasiQuoter
uri = mkValidatorQQ "[uri|…|]" uriCheck

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
