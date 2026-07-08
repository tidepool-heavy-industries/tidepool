{-# LANGUAGE TypeApplications #-}
-- | Fixed-shape JSON diagnostics report emitted on stdout by
-- @tidepool-extract-bin@ for EVERY invocation (success or failure) — see
-- @app/Main.hs@. Hand-rolled serializer (no @aeson@ dependency): the shape is
-- small and fixed, so a dependency buys nothing here.
module Tidepool.DiagJson
  ( Diag(..)
  , diagsFromSourceError
  , diagFromException
  , renderDiagsJson
  ) where

import Control.Exception (SomeException)
import Data.Char (ord)
import Data.Foldable (toList)
import Data.List (intercalate)
import Numeric (showHex)

import GHC (SrcSpan(..), srcSpanFile, srcSpanStartLine, srcSpanStartCol, srcSpanEndLine, srcSpanEndCol)
import GHC.Types.Error
  ( MsgEnvelope(..), Severity(..), diagnosticMessage, defaultDiagnosticOpts
  , getMessages )
import GHC.Types.SourceError (SourceError, srcErrorMessages)
import GHC.Driver.Errors.Types (GhcMessage)
import GHC.Utils.Error (formatBulleted)
import GHC.Utils.Outputable (renderWithContext, defaultSDocContext, SDocContext(..), mkErrStyle)
import GHC.Data.FastString (unpackFS)

-- | One diagnostic: an optional source span, a severity ("error"/"warning"),
-- and the rendered message text.
data Diag = Diag
  { dFile     :: Maybe (String, Int, Int, Int, Int)
  -- ^ @(file, startLine, startCol, endLine, endCol)@; 'Nothing' when GHC has
  -- no real span for the diagnostic ('UnhelpfulSpan').
  , dSeverity :: String
  -- ^ @"error"@ or @"warning"@.
  , dMessage  :: String
  }

-- | Every diagnostic (errors + warnings) carried by a caught 'SourceError',
-- in the order GHC collected them.
diagsFromSourceError :: SourceError -> [Diag]
diagsFromSourceError se = map envelopeToDiag (toList (getMessages (srcErrorMessages se)))

envelopeToDiag :: MsgEnvelope GhcMessage -> Diag
envelopeToDiag env = Diag
  { dFile     = spanOf (errMsgSpan env)
  , dSeverity = severityOf (errMsgSeverity env)
  , dMessage  = renderWithContext ctx
                  (formatBulleted (diagnosticMessage (defaultDiagnosticOpts @GhcMessage) (errMsgDiagnostic env)))
  }
  where
    ctx = defaultSDocContext { sdocStyle = mkErrStyle (errMsgContext env) }

spanOf :: SrcSpan -> Maybe (String, Int, Int, Int, Int)
spanOf (RealSrcSpan rss _) =
  Just (unpackFS (srcSpanFile rss), srcSpanStartLine rss, srcSpanStartCol rss, srcSpanEndLine rss, srcSpanEndCol rss)
spanOf (UnhelpfulSpan _) = Nothing

severityOf :: Severity -> String
severityOf SevError   = "error"
severityOf SevWarning = "warning"
severityOf SevIgnore  = "warning"

-- | A non-'SourceError' exception (parse failure before a GHC diagnostic
-- session exists, an @error@ call, IO failure, ...): no real span, always
-- severity @"error"@, message is @show@ of the exception.
diagFromException :: SomeException -> Diag
diagFromException e = Diag { dFile = Nothing, dSeverity = "error", dMessage = show e }

-- | Render the fixed-shape report:
-- @{"version":1,"diagnostics":[{"span":{...}|null,"severity":"...","message":"..."}]}@
renderDiagsJson :: [Diag] -> String
renderDiagsJson diags =
  "{\"version\":1,\"diagnostics\":[" ++ intercalate "," (map renderDiag diags) ++ "]}"

renderDiag :: Diag -> String
renderDiag (Diag mspan sev msg) =
  "{\"span\":" ++ renderSpan mspan
    ++ ",\"severity\":" ++ jstr sev
    ++ ",\"message\":" ++ jstr msg ++ "}"

renderSpan :: Maybe (String, Int, Int, Int, Int) -> String
renderSpan Nothing = "null"
renderSpan (Just (file, sl, sc, el, ec)) =
  "{\"file\":" ++ jstr file
    ++ ",\"startLine\":" ++ show sl
    ++ ",\"startCol\":" ++ show sc
    ++ ",\"endLine\":" ++ show el
    ++ ",\"endCol\":" ++ show ec ++ "}"

jstr :: String -> String
jstr s = '"' : concatMap esc s ++ "\""
  where
    esc '"'  = "\\\""
    esc '\\' = "\\\\"
    esc '\n' = "\\n"
    esc '\r' = "\\r"
    esc '\t' = "\\t"
    esc c
      | c < '\x20' = "\\u" ++ pad4 (showHex (ord c) "")
      | otherwise  = [c]
    pad4 s' = replicate (4 - length s') '0' ++ s'
