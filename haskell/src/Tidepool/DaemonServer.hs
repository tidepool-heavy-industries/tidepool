-- | The resident compile daemon's transport (plans/compile-daemon-design.md,
-- Phase 0). Owns EVERYTHING socket-shaped: bind\/accept loop, the
-- length-prefixed frame codec, request decode \/ response encode, the
-- stdout\/stderr capture that turns an in-process dispatch call into the same
-- @{exit_code, stdout, stderr}@ shape a spawned process's 'System.Process'
-- 'System.Process.Output' would have produced, request-count rotation, the
-- RSS-ceiling backstop, the toolchain-stamp watch tick, and every clean-exit
-- path those three triggers share.
--
-- Knows NOTHING about GHC-API internals — 'runDaemon' is generic over a
-- caller-supplied 'RequestHandler' (@cwd -> argv -> IO ExitCode@), so it
-- never imports "GHC" or anything from "Tidepool.GhcPipeline". app\/Main.hs
-- is the one place that builds a 'RequestHandler' (from its own existing
-- argv dispatch, substituting 'Tidepool.GhcPipeline.withResidentPipeline'\'s
-- compiler for the one-shot 'Tidepool.GhcPipeline.runPipelineSession') and
-- hands it to 'runDaemon' — that seam is the boundary.
--
-- Wire (plans/compile-daemon-design.md Decisions item 6): a UNIX domain
-- socket, one request\/response per connection, length-prefixed frames — no
-- JSON, no serde-shaped dependency, both endpoints are in-repo so no
-- interchange format is needed. All multi-byte integers are little-endian.
--
-- > frame     ::= u32-LE length, then that many raw bytes (UTF-8 text here)
-- > request   ::= frame(cwd) u32-LE(argc) frame(argv[0]) .. frame(argv[argc-1])
-- > response  ::= i32-LE(exit_code) frame(stdout) frame(stderr)
--
-- EOF (a short read) mid-frame is the daemon-crashed-mid-request signal both
-- sides rely on (design §4.2\/§5.3) — 'recvExact' throws exactly there, and
-- 'serveOne' catches it, logs, and simply closes the connection WITHOUT
-- sending a response, rather than trying to report a synthetic error over a
-- protocol that may itself be the thing that broke.
module Tidepool.DaemonServer
  ( DaemonConfig(..)
  , RequestHandler
  , runDaemon
    -- * Frame codec (exposed for the Haskell unit round-trip test)
  , DaemonRequest(..), DaemonResponse(..)
  , encodeRequest, decodeRequest
  , encodeResponse, decodeResponse
  , sendRequest, recvResponse, sendRequestToDaemon
  ) where

import Network.Socket
import qualified Network.Socket.ByteString as NBS
import qualified Data.ByteString as BS
import qualified Data.Text as T
import qualified Data.Text.Encoding as TE
import qualified Data.Text.Encoding.Error as TEE
import Data.Bits (shiftL, shiftR, (.|.))
import Data.Word (Word32)
import Data.Int (Int32)
import Data.List (isPrefixOf)
import Control.Exception
  ( bracket, finally, catch, try, throwIO, SomeException, IOException )
import Control.Monad (replicateM, when)
import Data.IORef (newIORef, atomicModifyIORef', writeIORef, readIORef)
import System.Directory (removeFile, getTemporaryDirectory)
import System.IO
  ( Handle, stdout, stderr, hFlush, hClose, hPutStrLn, openTempFile )
import GHC.IO.Handle (hDuplicate, hDuplicateTo)
import System.IO.Error (isDoesNotExistError)
import System.Exit (ExitCode(..))
import System.Timeout (timeout)
import System.Posix.Signals (installHandler, sigTERM, Handler(Catch))

--------------------------------------------------------------------------------
-- Frame primitives
--------------------------------------------------------------------------------

putU32LE :: Word32 -> BS.ByteString
putU32LE w = BS.pack
  [ fromIntegral (w `shiftR` 0)
  , fromIntegral (w `shiftR` 8)
  , fromIntegral (w `shiftR` 16)
  , fromIntegral (w `shiftR` 24)
  ]

getU32LE :: BS.ByteString -> Word32
getU32LE bs = case BS.unpack bs of
  [b0, b1, b2, b3] ->
        fromIntegral b0
    .|. (fromIntegral b1 `shiftL` 8)
    .|. (fromIntegral b2 `shiftL` 16)
    .|. (fromIntegral b3 `shiftL` 24)
  _ -> error "getU32LE: need exactly 4 bytes"

encodeTextBytes :: String -> BS.ByteString
encodeTextBytes = TE.encodeUtf8 . T.pack

-- | Lenient: a daemon peer is always this same repo's own client, so
-- malformed UTF-8 should never occur — lenient decode keeps a corrupt
-- payload from crashing this connection's handling outright, replacing bad
-- bytes with the Unicode replacement character instead.
decodeTextBytes :: BS.ByteString -> String
decodeTextBytes = T.unpack . TE.decodeUtf8With TEE.lenientDecode

encodeFrame :: BS.ByteString -> BS.ByteString
encodeFrame b = putU32LE (fromIntegral (BS.length b)) <> b

--------------------------------------------------------------------------------
-- Pure request/response codec (Get-style manual parser — no new dependency)
--------------------------------------------------------------------------------

data DaemonRequest = DaemonRequest
  { reqCwd  :: FilePath
  , reqArgv :: [String]
  } deriving (Eq, Show)

data DaemonResponse = DaemonResponse
  { respExitCode :: Int
  , respStdout   :: String
  , respStderr   :: String
  } deriving (Eq, Show)

type Parser a = BS.ByteString -> Either String (a, BS.ByteString)

pWord32 :: Parser Word32
pWord32 bs
  | BS.length bs < 4 = Left "truncated u32 length prefix"
  | otherwise = Right (getU32LE (BS.take 4 bs), BS.drop 4 bs)

pFrame :: Parser BS.ByteString
pFrame bs = do
  (len, rest) <- pWord32 bs
  let n = fromIntegral len
  if BS.length rest < n
    then Left "truncated frame body"
    else Right (BS.take n rest, BS.drop n rest)

pN :: Int -> Parser a -> Parser [a]
pN 0 _ bs = Right ([], bs)
pN k p bs = do
  (x, bs') <- p bs
  (xs, bs'') <- pN (k - 1) p bs'
  Right (x : xs, bs'')

-- | Encode a 'DaemonRequest' to the wire shape the WIRE section documents.
-- Exposed (alongside 'decodeRequest') purely so the Haskell unit test can pin
-- the codec's round-trip and truncated-input behaviour without a real socket.
encodeRequest :: DaemonRequest -> BS.ByteString
encodeRequest (DaemonRequest cwd argv) =
  encodeFrame (encodeTextBytes cwd)
    <> putU32LE (fromIntegral (length argv))
    <> BS.concat (map (encodeFrame . encodeTextBytes) argv)

decodeRequest :: BS.ByteString -> Either String (DaemonRequest, BS.ByteString)
decodeRequest bs0 = do
  (cwdBytes, bs1) <- pFrame bs0
  (argc, bs2) <- pWord32 bs1
  (argvBytesList, bs3) <- pN (fromIntegral argc) pFrame bs2
  Right (DaemonRequest (decodeTextBytes cwdBytes) (map decodeTextBytes argvBytesList), bs3)

encodeResponse :: DaemonResponse -> BS.ByteString
encodeResponse (DaemonResponse code out err) =
  putU32LE (fromIntegral (fromIntegral code :: Int32))
    <> encodeFrame (encodeTextBytes out)
    <> encodeFrame (encodeTextBytes err)

decodeResponse :: BS.ByteString -> Either String (DaemonResponse, BS.ByteString)
decodeResponse bs0 = do
  (codeW, bs1) <- pWord32 bs0
  (outBytes, bs2) <- pFrame bs1
  (errBytes, bs3) <- pFrame bs2
  Right (DaemonResponse (fromIntegral (fromIntegral codeW :: Int32)) (decodeTextBytes outBytes) (decodeTextBytes errBytes), bs3)

--------------------------------------------------------------------------------
-- Socket-level framing (same shapes as above, streamed incrementally)
--------------------------------------------------------------------------------

-- | Read exactly @n@ bytes, or throw once the peer closes before delivering
-- them all — the EOF-mid-frame signal the module doc describes.
recvExact :: Socket -> Int -> IO BS.ByteString
recvExact _ 0 = pure BS.empty
recvExact sock n = go n []
  where
    go 0 acc = pure (BS.concat (reverse acc))
    go remaining acc = do
      chunk <- NBS.recv sock remaining
      if BS.null chunk
        then throwIO (userError "daemon: peer closed the connection mid-frame")
        else go (remaining - BS.length chunk) (chunk : acc)

recvU32 :: Socket -> IO Word32
recvU32 sock = getU32LE <$> recvExact sock 4

recvFrame :: Socket -> IO BS.ByteString
recvFrame sock = do
  n <- recvU32 sock
  recvExact sock (fromIntegral n)

-- | A corrupted or adversarial length prefix must not turn into an
-- unbounded read loop — this is a same-host, same-user, in-repo protocol
-- (design §5.3), so a generous but finite cap is a sanity backstop, not a
-- real limit any legitimate argv ever approaches.
maxArgc :: Word32
maxArgc = 100000

recvRequest :: Socket -> IO DaemonRequest
recvRequest sock = do
  cwdBytes <- recvFrame sock
  argc <- recvU32 sock
  when (argc > maxArgc) $
    throwIO (userError ("daemon: request argc " ++ show argc ++ " exceeds sanity cap"))
  argvBytesList <- replicateM (fromIntegral argc) (recvFrame sock)
  pure (DaemonRequest (decodeTextBytes cwdBytes) (map decodeTextBytes argvBytesList))

-- | Send one request using the same encoding as 'recvRequest' consumes.
sendRequest :: Socket -> DaemonRequest -> IO ()
sendRequest sock req = NBS.sendAll sock (encodeRequest req)

-- | Receive one response using the streamed counterpart of 'decodeResponse'.
recvResponse :: Socket -> IO DaemonResponse
recvResponse sock = do
  codeW <- recvU32 sock
  outBytes <- recvFrame sock
  errBytes <- recvFrame sock
  pure (DaemonResponse
    (fromIntegral (fromIntegral codeW :: Int32))
    (decodeTextBytes outBytes)
    (decodeTextBytes errBytes))

sendResponse :: Socket -> DaemonResponse -> IO ()
sendResponse sock resp = NBS.sendAll sock (encodeResponse resp)

-- | Connect to a running daemon for exactly one request/response exchange.
-- Socket ownership stays in this module so callers cannot accidentally grow
-- a second implementation of the wire protocol.
sendRequestToDaemon :: FilePath -> DaemonRequest -> IO DaemonResponse
sendRequestToDaemon path req =
  bracket (socket AF_UNIX Stream defaultProtocol) close $ \sock -> do
    connect sock (SockAddrUnix path)
    sendRequest sock req
    recvResponse sock

--------------------------------------------------------------------------------
-- stdout/stderr capture — turns an in-process IO action into the same
-- {exit_code, stdout, stderr} shape a spawned process's Output carries.
--------------------------------------------------------------------------------

-- | Redirect the REAL 'stdout'\/'stderr' handles to scratch files for the
-- duration of @act@, then read them back as the captured text. Temp FILES,
-- not a pipe: a pipe risks a full-buffer deadlock without a concurrent
-- drain thread, and the daemon serves one request at a time anyway, so a
-- file round-trip costs nothing observable and stays trivially correct.
captureOutput :: IO ExitCode -> IO (Int, String, String)
captureOutput act = do
  tmpDir <- getTemporaryDirectory
  (outPath, outH) <- openTempFile tmpDir "tidepool-daemon-stdout.txt"
  (errPath, errH) <- openTempFile tmpDir "tidepool-daemon-stderr.txt"
  exitCode <- runRedirected outH errH act
  outBytes <- BS.readFile outPath
  errBytes <- BS.readFile errPath
  removeFile outPath `catch` \(_ :: IOException) -> pure ()
  removeFile errPath `catch` \(_ :: IOException) -> pure ()
  pure (exitCodeToInt exitCode, decodeTextBytes outBytes, decodeTextBytes errBytes)

runRedirected :: Handle -> Handle -> IO ExitCode -> IO ExitCode
runRedirected outH errH act = do
  savedOut <- hDuplicate stdout
  savedErr <- hDuplicate stderr
  hDuplicateTo outH stdout
  hDuplicateTo errH stderr
  act `finally` do
    hFlush stdout
    hFlush stderr
    hDuplicateTo savedOut stdout
    hDuplicateTo savedErr stderr
    hClose savedOut
    hClose savedErr
    hClose outH
    hClose errH

exitCodeToInt :: ExitCode -> Int
exitCodeToInt ExitSuccess = 0
exitCodeToInt (ExitFailure n) = n

--------------------------------------------------------------------------------
-- The accept loop: rotation, RSS backstop, stamp watch, clean exits
--------------------------------------------------------------------------------

-- | @cwd -> argv -> IO ExitCode@ — one request's worth of work, exactly what
-- a spawned @tidepool-extract@ process's @main@ would have done for the same
-- argv, with 'System.Exit.exitWith' turned into a returned 'ExitCode' rather
-- than a real process exit. Never throws for an ORDINARY compile failure (a
-- real Haskell type error) — that is already reported as a normal
-- 'ExitFailure' with diagnostics on the captured stdout, exactly as a direct
-- spawn does; an exception escaping this action is treated as a genuine
-- daemon crash (see the module doc's EOF-mid-request contract).
type RequestHandler = FilePath -> [String] -> IO ExitCode

data DaemonConfig = DaemonConfig
  { dcSocketPath   :: FilePath
  , dcRotateAfter  :: Maybe Int
    -- ^ Clean-exit once this many requests have been served (design
    -- Decisions item 3: N=256 is the caller's provisional default, not
    -- this module's business).
  , dcRssCeilingMb :: Maybe Int
    -- ^ Clean-exit once resident memory (read from
    -- @\/proc\/self\/status@'s @VmRSS@) exceeds this many megabytes.
  , dcWatchStamp   :: Maybe FilePath
    -- ^ Byte-watch this toolchain stamp file (design §3's sanctioned
    -- reuse — NOT a fingerprint recompute): changed or deleted since boot
    -- means a redeploy happened under this daemon, so it exits and lets
    -- its supervisor relaunch a fresh process against the new toolchain.
  , dcRequestTimeoutSec :: Int
    -- ^ Bound on reading one request off an accepted connection, so a
    -- wedged or malformed peer cannot block the single worker forever.
  }

-- | How often an idle 'acceptLoop' wakes from a blocking @accept@ to re-check
-- 'shutdownRequested' — the bound on SIGTERM-to-exit latency while idle (see
-- 'runDaemon''s SIGTERM handling note). One second is "boring and portable":
-- short enough that a caller tearing the daemon down never mistakes it for a
-- hang, long enough that idle polling costs nothing observable.
acceptPollMicros :: Int
acceptPollMicros = 1000000

-- | Serve requests on @dcSocketPath cfg@ until rotation, the RSS ceiling, a
-- detected toolchain-stamp change, or SIGTERM trigger a clean exit — all four
-- share the SAME exit path (stop accepting, close and remove the socket,
-- return), never a second independent shutdown mechanism. Rotation/RSS/stamp
-- are checked once after each request completes: RSS/stamp checks are cheap
-- (a small @\/proc@ read and a small file read respectively — nothing like
-- the toolchain module's own blake3 fingerprint recompute), so checking on
-- every request keeps the bound tight without needing a separate timer
-- thread. Recorded as a deliberate Phase 0 simplification in
-- plans/compile-daemon-design.md §7 (the design doc frames this as a
-- background TICK, not per-request).
--
-- SIGTERM: an idle-blocked @accept@ does not itself observe a signal — a
-- caught SIGTERM only sets 'shutdownRequested', which is why 'acceptLoop'
-- bounds its own blocking wait to 'acceptPollMicros' instead of calling
-- 'accept' directly, and re-checks the flag on every wake (spawnrow-fix,
-- plans/compile-daemon-design.md §7: 15+s observed to exit on TERM while
-- idle-blocked in a plain @accept@, before this fix).
runDaemon :: DaemonConfig -> RequestHandler -> IO ()
runDaemon cfg handler = do
  bootStamp <- maybe (pure Nothing) readIfPresent (dcWatchStamp cfg)
  shutdownRequested <- newIORef False
  _ <- installHandler sigTERM (Catch (writeIORef shutdownRequested True)) Nothing
  bracket (openListener (dcSocketPath cfg)) (closeListener (dcSocketPath cfg)) $ \sock -> do
    countRef <- newIORef (0 :: Int)
    let acceptLoop = do
          termed <- readIORef shutdownRequested
          if termed then pure () else do
            accepted <- try (timeout acceptPollMicros (accept sock))
            case accepted of
              Left (_ :: SomeException) -> pure ()  -- listener closed out from under us
              Right Nothing -> acceptLoop            -- poll elapsed, re-check the flag
              Right (Just (conn, _)) -> do
                serveOne (dcRequestTimeoutSec cfg) handler conn `finally` close conn
                n <- atomicModifyIORef' countRef (\c -> (c + 1, c + 1))
                stop <- shouldStopNow cfg bootStamp n
                termedAfter <- readIORef shutdownRequested
                if stop || termedAfter then pure () else acceptLoop
    acceptLoop

shouldStopNow :: DaemonConfig -> Maybe BS.ByteString -> Int -> IO Bool
shouldStopNow cfg bootStamp n = do
  let rotated = maybe False (n >=) (dcRotateAfter cfg)
  rssOver <- case dcRssCeilingMb cfg of
    Nothing -> pure False
    Just ceilingMb -> (> ceilingMb) <$> currentRssMb
  stale <- case dcWatchStamp cfg of
    Nothing -> pure False
    Just path -> do
      now <- readIfPresent path
      pure (now /= bootStamp)
  pure (rotated || rssOver || stale)

readIfPresent :: FilePath -> IO (Maybe BS.ByteString)
readIfPresent path =
  (Just <$> BS.readFile path) `catch` \e ->
    if isDoesNotExistError e then pure Nothing else throwIO e

-- | Resident set size in megabytes, read from @\/proc\/self\/status@'s
-- @VmRSS@ line (kilobytes). @0@ if the line is missing or unparsable
-- (non-Linux, or a @\/proc@ hiccup) — a Phase 0 daemon with no working RSS
-- read simply never trips the backstop, which is safe-by-omission (rotation
-- is still the primary trigger).
currentRssMb :: IO Int
currentRssMb = go `catch` \(_ :: IOException) -> pure 0
  where
    go = do
      contents <- BS.readFile "/proc/self/status"
      let ls = lines (decodeTextBytes contents)
          vmrss = [l | l <- ls, "VmRSS:" `isPrefixOf` l]
      pure $ case vmrss of
        (l : _) -> case words l of
          (_ : kbStr : _) -> case reads kbStr of
            [(kb, "")] -> (kb :: Int) `div` 1024
            _ -> 0
          _ -> 0
        [] -> 0

serveOne :: Int -> RequestHandler -> Socket -> IO ()
serveOne timeoutSec handler conn = do
  result <- try $ do
    mReq <- timeout (timeoutSec * 1000000) (recvRequest conn)
    case mReq of
      Nothing -> throwIO (userError "daemon: timed out reading a request")
      Just req -> do
        (code, out, err) <- captureOutput (handler (reqCwd req) (reqArgv req))
        sendResponse conn (DaemonResponse code out err)
  case result of
    Left (e :: SomeException) ->
      putStrLn' ("daemon: request failed, closing without a response: " ++ show e)
    Right () -> pure ()
  where
    -- Writes to the REAL stderr — this runs OUTSIDE 'captureOutput''s
    -- redirect window (that window closed before 'result' is inspected), so
    -- it never lands in a captured response.
    putStrLn' = hPutStrLn stderr

removeStaleSocket :: FilePath -> IO ()
removeStaleSocket path =
  removeFile path `catch` \e ->
    if isDoesNotExistError e then pure () else throwIO e

openListener :: FilePath -> IO Socket
openListener path = do
  removeStaleSocket path
  sock <- socket AF_UNIX Stream defaultProtocol
  bind sock (SockAddrUnix path)
  listen sock 128
  pure sock

closeListener :: FilePath -> Socket -> IO ()
closeListener path sock = do
  close sock
  removeStaleSocket path
