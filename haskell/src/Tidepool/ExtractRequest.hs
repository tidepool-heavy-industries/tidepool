-- | Versioned request protocol between the Rust launcher and the Haskell
-- compiler worker. The worker receives domain fields, not command-line
-- options; CLI compatibility is owned by the launcher layer.
module Tidepool.ExtractRequest
  ( RequestField(..)
  , workerRequestFromArgv
  ) where

import qualified Data.ByteString as BS
import qualified Data.Text as T
import qualified Data.Text.Encoding as TE
import Data.Bits ((.|.), shiftL)
import Data.Char (digitToInt, isHexDigit)
import Data.Word (Word32, Word64, Word8)

data RequestField
  = Input FilePath
  | OutputDir FilePath
  | Target String
  | Targets [String]
  | DumpCore
  | AllClosed
  | TargetModuleOnly
  | Include FilePath
  | SessionBind
  | BindName String
  | BindGen Word64
  | SessionRoot FilePath
  | InjectVal String
  | EmitBoundBinders FilePath
  | ProbeOnly
  | Turn
  | TurnTemplate String FilePath
  | TurnOut FilePath
  | TurnVerdict String
  | Classify
  | ClassifyOut FilePath
  | TurnBatch FilePath
  | BatchOut FilePath
  | HarnessProfile
  | BuildProductsDir FilePath
  deriving (Eq, Show)

workerRequestFlag :: String
workerRequestFlag = "--worker-request-v1"

-- | Find and decode the one worker payload in a launcher's argv. Arguments
-- before the marker are a temporary compatibility rendering and are ignored.
workerRequestFromArgv :: [String] -> Either String (Maybe [RequestField])
workerRequestFromArgv = go
  where
    go [] = Right Nothing
    go [flag] | flag == workerRequestFlag = Left (workerRequestFlag ++ ": payload is required")
    go [_] = Right Nothing
    go (flag : payload : rest)
      | flag == workerRequestFlag = do
          if workerRequestFlag `elem` rest
            then Left (workerRequestFlag ++ ": request marker appears more than once")
            else Just <$> (decodeHex payload >>= decodeRequest)
      | otherwise = go (payload : rest)

type Parser a = BS.ByteString -> Either String (a, BS.ByteString)

decodeRequest :: BS.ByteString -> Either String [RequestField]
decodeRequest bytes = do
  let (magic, body) = BS.splitAt 8 bytes
  if magic /= "TPREQ001"
    then Left "worker request: unsupported magic or version"
    else do
      (count, rest) <- pWord32 body
      (fields, trailing) <- pN (fromIntegral count) pField rest
      if BS.null trailing
        then Right fields
        else Left "worker request: trailing bytes"

pField :: Parser RequestField
pField bytes = do
  (tag, rest) <- pWord8 bytes
  case tag of
    1  -> mapParser Input pText rest
    2  -> mapParser OutputDir pText rest
    3  -> mapParser Target pText rest
    4  -> do
      (count, rest') <- pWord32 rest
      mapParser Targets (pN (fromIntegral count) pText) rest'
    5  -> Right (DumpCore, rest)
    6  -> Right (AllClosed, rest)
    7  -> Right (TargetModuleOnly, rest)
    8  -> mapParser Include pText rest
    9  -> Right (SessionBind, rest)
    10 -> mapParser BindName pText rest
    11 -> mapParser BindGen pWord64 rest
    12 -> mapParser SessionRoot pText rest
    13 -> mapParser InjectVal pText rest
    14 -> mapParser EmitBoundBinders pText rest
    15 -> Right (ProbeOnly, rest)
    16 -> Right (Turn, rest)
    17 -> do
      (kind, rest') <- pText rest
      (path, rest'') <- pText rest'
      Right (TurnTemplate kind path, rest'')
    18 -> mapParser TurnOut pText rest
    19 -> mapParser TurnVerdict pText rest
    20 -> Right (Classify, rest)
    21 -> mapParser ClassifyOut pText rest
    22 -> mapParser TurnBatch pText rest
    23 -> mapParser BatchOut pText rest
    24 -> Right (HarnessProfile, rest)
    25 -> mapParser BuildProductsDir pText rest
    _  -> Left ("worker request: unknown field tag " ++ show tag)

mapParser :: (a -> b) -> Parser a -> Parser b
mapParser f parser bytes = do
  (value, rest) <- parser bytes
  Right (f value, rest)

pN :: Int -> Parser a -> Parser [a]
pN 0 _ bytes = Right ([], bytes)
pN count parser bytes = do
  (value, rest) <- parser bytes
  (values, trailing) <- pN (count - 1) parser rest
  Right (value : values, trailing)

pWord8 :: Parser Word8
pWord8 bytes = case BS.uncons bytes of
  Nothing -> Left "worker request: truncated field tag"
  Just pair -> Right pair

pWord32 :: Parser Word32
pWord32 bytes
  | BS.length bytes < 4 = Left "worker request: truncated u32"
  | otherwise =
      let b0 = BS.index bytes 0
          b1 = BS.index bytes 1
          b2 = BS.index bytes 2
          b3 = BS.index bytes 3
          value = fromIntegral b0
            .|. (fromIntegral b1 `shiftL` 8)
            .|. (fromIntegral b2 `shiftL` 16)
            .|. (fromIntegral b3 `shiftL` 24)
      in Right (value, BS.drop 4 bytes)

pWord64 :: Parser Word64
pWord64 bytes
  | BS.length bytes < 8 = Left "worker request: truncated u64"
  | otherwise =
      let octets = zip [0, 8 .. 56] (BS.unpack (BS.take 8 bytes))
          value = foldr (\(shift, byte) acc -> acc .|. (fromIntegral byte `shiftL` shift)) 0 octets
      in Right (value, BS.drop 8 bytes)

pFrame :: Parser BS.ByteString
pFrame bytes = do
  (len, rest) <- pWord32 bytes
  let count = fromIntegral len
  if BS.length rest < count
    then Left "worker request: truncated frame"
    else Right (BS.take count rest, BS.drop count rest)

pText :: Parser String
pText bytes = do
  (frame, rest) <- pFrame bytes
  case TE.decodeUtf8' frame of
    Left err -> Left ("worker request: invalid UTF-8: " ++ show err)
    Right value -> Right (T.unpack value, rest)

decodeHex :: String -> Either String BS.ByteString
decodeHex input
  | odd (length input) = Left "worker request: hex payload has odd length"
  | otherwise = BS.pack <$> pairs input
  where
    pairs [] = Right []
    pairs (hi : lo : rest)
      | isHexDigit hi && isHexDigit lo =
          let byte = fromIntegral (digitToInt hi * 16 + digitToInt lo)
          in (byte :) <$> pairs rest
      | otherwise = Left "worker request: hex payload contains a non-hex digit"
    pairs _ = Left "worker request: hex payload has odd length"
