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
-- (An @[sg|...|]@ ast-grep-pattern quoter used to live here; it was removed
-- with the SG effect. A @[glob|...|]@ quoter was deliberately OMITTED from v1:
-- the name collides with the eval-visible @glob :: Text -> M [Text]@ Fs verb.)
module Tidepool.QQ.Validate
  ( uri
  , mkValidatorQQ
  ) where

import Data.Text (Text)
import qualified Tidepool.Data.Text as T

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
