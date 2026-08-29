-- | Versioned request protocol between the Rust launcher and the Haskell
-- compiler worker. The worker receives domain fields, not command-line
-- options; CLI compatibility is owned by the launcher layer.
module Tidepool.ExtractRequest
  ( RequestField(..)
  , workerRequestFromArgv
  , workerArgv
  ) where

import qualified Data.ByteString as BS
import qualified Data.Text as T
import qualified Data.Text.Encoding as TE
import Data.Bits ((.|.), shiftL, shiftR)
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
  | HarnessProfile
  | BuildProductsDir FilePath
  deriving (Eq, Show)

workerRequestFlag :: String
workerRequestFlag = "--worker-request-v1"

workerArgv :: [RequestField] -> [String]
workerArgv fields = [workerRequestFlag, encodeHex (encodeRequest fields)]

-- | Decode the worker's complete argv. The compiler boundary deliberately has
-- no command-line grammar: exactly one versioned payload is accepted.
workerRequestFromArgv :: [String] -> Either String (Maybe [RequestField])
workerRequestFromArgv [] = Right Nothing
workerRequestFromArgv [flag] | flag == workerRequestFlag = Left (workerRequestFlag ++ ": payload is required")
workerRequestFromArgv [flag, payload]
  | flag == workerRequestFlag = Just <$> (decodeHex payload >>= decodeRequest)
workerRequestFromArgv _ = Left "worker requires exactly one versioned request"

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

encodeRequest :: [RequestField] -> BS.ByteString
encodeRequest fields = "TPREQ001" <> putU32 (length fields) <> BS.concat (map encodeField fields)

encodeField :: RequestField -> BS.ByteString
encodeField field = case field of
  Input value -> taggedText 1 value
  OutputDir value -> taggedText 2 value
  Target value -> taggedText 3 value
  Targets values -> BS.singleton 4 <> putU32 (length values) <> BS.concat (map textFrame values)
  DumpCore -> BS.singleton 5
  AllClosed -> BS.singleton 6
  TargetModuleOnly -> BS.singleton 7
  Include value -> taggedText 8 value
  SessionBind -> BS.singleton 9
  BindName value -> taggedText 10 value
  BindGen value -> BS.singleton 11 <> putU64 value
  SessionRoot value -> taggedText 12 value
  InjectVal value -> taggedText 13 value
  EmitBoundBinders value -> taggedText 14 value
  ProbeOnly -> BS.singleton 15
  Turn -> BS.singleton 16
  TurnTemplate kind path -> BS.singleton 17 <> textFrame kind <> textFrame path
  TurnOut value -> taggedText 18 value
  TurnVerdict value -> taggedText 19 value
  Classify -> BS.singleton 20
  ClassifyOut value -> taggedText 21 value
  HarnessProfile -> BS.singleton 24
  BuildProductsDir value -> taggedText 25 value

taggedText :: Word8 -> String -> BS.ByteString
taggedText tag value = BS.singleton tag <> textFrame value

textFrame :: String -> BS.ByteString
textFrame value = let bytes = TE.encodeUtf8 (T.pack value) in putU32 (BS.length bytes) <> bytes

putU32 :: Int -> BS.ByteString
putU32 value = BS.pack
  [ fromIntegral (value `shiftR` 0)
  , fromIntegral (value `shiftR` 8)
  , fromIntegral (value `shiftR` 16)
  , fromIntegral (value `shiftR` 24)
  ]

putU64 :: Word64 -> BS.ByteString
putU64 value = BS.pack [fromIntegral (value `shiftR` shift) | shift <- [0, 8 .. 56]]

encodeHex :: BS.ByteString -> String
encodeHex = concatMap byteHex . BS.unpack
  where
    digits = "0123456789abcdef"
    byteHex byte = [digits !! fromIntegral (byte `div` 16), digits !! fromIntegral (byte `mod` 16)]

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
