-- | Frame-codec round-trip pin for the resident compile daemon's wire
-- (plans/compile-daemon-design.md, Phase 0). Pure — no GHC session, no
-- extract compile — pinning 'Tidepool.DaemonServer.encodeRequest'/
-- 'decodeRequest'/'encodeResponse'/'decodeResponse' against each other and
-- against truncated input, mirroring the Rust-side unit tests in
-- @tidepool-extract-cmd/src/daemon.rs@ for the SAME wire shape (both sides
-- pin their own half of the codec; this module never talks to a socket).
module Fidelity.DaemonCodec (checks) where

import Fidelity.Harness (Check, check)
import Tidepool.DaemonServer
  ( DaemonRequest(..), DaemonResponse(..)
  , encodeRequest, decodeRequest, encodeResponse, decodeResponse )

import qualified Data.ByteString as BS

checks :: IO [Check]
checks = pure
  [ check "request round-trips through encode/decode" requestRoundTrips
  , check "request with empty argv round-trips" emptyArgvRoundTrips
  , check "request with an empty-string arg round-trips" emptyStringArgRoundTrips
  , check "response round-trips through encode/decode" responseRoundTrips
  , check "response with a negative exit code round-trips" negativeExitCodeRoundTrips
  , check "response with empty stdout/stderr round-trips" emptyStdoutStderrRoundTrips
  , check "decodeRequest reports Left on a truncated cwd length prefix" truncatedCwdLengthIsLeft
  , check "decodeRequest reports Left on a truncated cwd body" truncatedCwdBodyIsLeft
  , check "decodeRequest reports Left on a truncated argv frame" truncatedArgvFrameIsLeft
  , check "decodeResponse reports Left on a truncated exit-code prefix" truncatedExitCodeIsLeft
  , check "decodeResponse reports Left on a truncated stdout frame" truncatedStdoutFrameIsLeft
  , check "decodeResponse reports Left on a truncated stderr frame" truncatedStderrFrameIsLeft
  , check "decodeRequest reports unconsumed trailing bytes rather than dropping them" noTrailingBytesAfterRequestDecode
  , check "decodeResponse reports unconsumed trailing bytes rather than dropping them" noTrailingBytesAfterResponseDecode
  ]

sampleRequest :: DaemonRequest
sampleRequest = DaemonRequest
  { reqCwd  = "/home/turn/scratch"
  , reqArgv = ["Expr.hs", "--output-dir", "out", "--target", "result"]
  }

requestRoundTrips :: Bool
requestRoundTrips = case decodeRequest (encodeRequest sampleRequest) of
  Right (req, rest) -> req == sampleRequest && BS.null rest
  Left _             -> False

emptyArgvRoundTrips :: Bool
emptyArgvRoundTrips =
  let req = DaemonRequest { reqCwd = "/tmp", reqArgv = [] }
  in case decodeRequest (encodeRequest req) of
       Right (req', rest) -> req' == req && BS.null rest
       Left _              -> False

emptyStringArgRoundTrips :: Bool
emptyStringArgRoundTrips =
  let req = DaemonRequest { reqCwd = "", reqArgv = ["", "x", ""] }
  in case decodeRequest (encodeRequest req) of
       Right (req', rest) -> req' == req && BS.null rest
       Left _              -> False

sampleResponse :: DaemonResponse
sampleResponse = DaemonResponse
  { respExitCode = 1
  , respStdout   = "{\"version\":1,\"diagnostics\":[]}"
  , respStderr   = "Processing: Expr.hs\n"
  }

responseRoundTrips :: Bool
responseRoundTrips = case decodeResponse (encodeResponse sampleResponse) of
  Right (resp, rest) -> resp == sampleResponse && BS.null rest
  Left _              -> False

negativeExitCodeRoundTrips :: Bool
negativeExitCodeRoundTrips =
  let resp = DaemonResponse { respExitCode = -1, respStdout = "", respStderr = "" }
  in case decodeResponse (encodeResponse resp) of
       Right (resp', rest) -> resp' == resp && BS.null rest
       Left _               -> False

emptyStdoutStderrRoundTrips :: Bool
emptyStdoutStderrRoundTrips =
  let resp = DaemonResponse { respExitCode = 0, respStdout = "", respStderr = "" }
  in case decodeResponse (encodeResponse resp) of
       Right (resp', rest) -> resp' == resp && BS.null rest
       Left _               -> False

isLeft :: Either a b -> Bool
isLeft (Left _) = True
isLeft (Right _) = False

truncatedCwdLengthIsLeft :: Bool
truncatedCwdLengthIsLeft = isLeft (decodeRequest (BS.pack [1, 2]))

truncatedCwdBodyIsLeft :: Bool
truncatedCwdBodyIsLeft =
  -- Claims a 10-byte cwd, supplies only 3.
  isLeft (decodeRequest (BS.pack [10, 0, 0, 0, 97, 98, 99]))

truncatedArgvFrameIsLeft :: Bool
truncatedArgvFrameIsLeft =
  let full = encodeRequest DaemonRequest { reqCwd = "/x", reqArgv = ["hello"] }
      -- Chop off the last 2 bytes of the one argv frame's body.
      truncated = BS.take (BS.length full - 2) full
  in isLeft (decodeRequest truncated)

truncatedExitCodeIsLeft :: Bool
truncatedExitCodeIsLeft = isLeft (decodeResponse (BS.pack [0, 1]))

truncatedStdoutFrameIsLeft :: Bool
truncatedStdoutFrameIsLeft =
  -- exit_code (4 bytes) + a stdout length prefix claiming 20 bytes, 0 supplied.
  isLeft (decodeResponse (BS.pack [0, 0, 0, 0, 20, 0, 0, 0]))

truncatedStderrFrameIsLeft :: Bool
truncatedStderrFrameIsLeft =
  let full = encodeResponse DaemonResponse { respExitCode = 0, respStdout = "ok", respStderr = "err text" }
  in isLeft (decodeResponse (BS.take (BS.length full - 3) full))

noTrailingBytesAfterRequestDecode :: Bool
noTrailingBytesAfterRequestDecode =
  case decodeRequest (encodeRequest sampleRequest <> BS.pack [9, 9, 9]) of
    Right (req, rest) -> req == sampleRequest && rest == BS.pack [9, 9, 9]
    Left _             -> False

noTrailingBytesAfterResponseDecode :: Bool
noTrailingBytesAfterResponseDecode =
  case decodeResponse (encodeResponse sampleResponse <> BS.pack [7, 7]) of
    Right (resp, rest) -> resp == sampleResponse && rest == BS.pack [7, 7]
    Left _              -> False
