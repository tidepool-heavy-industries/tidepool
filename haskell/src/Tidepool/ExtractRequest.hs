-- | Versioned request protocol between the Rust launcher and the Haskell
-- compiler worker. The worker receives domain fields, not command-line
-- options; CLI compatibility is owned by the launcher layer.
module Tidepool.ExtractRequest
  ( RequestField(..)
  , InspectionRequest(..)
  , WorkerRequest(..)
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
  | BindGen Word64
  | SessionRoot FilePath
  | InjectVal String
  | Turn
  | TurnTemplate String FilePath
  | TurnOut FilePath
  | TurnVerdict String
  | Classify
  | ClassifyOut FilePath
  | Cell
  | CellTemplate FilePath
  | CellOut FilePath
  | TurnPin String
  | HarnessProfile
  | BuildProductsDir FilePath
  | InspectType String
  | InspectInfo String
  | InspectBrowse String
  | InspectBrowseExpanded String
  | InspectOut FilePath
  deriving (Eq, Show)

-- | A decoded compiler-worker invocation. This is the Haskell boundary's
-- domain request; the executable never reconstructs or parses CLI syntax.
data WorkerRequest = WorkerRequest
  { requestOutDir :: Maybe FilePath
  , requestTarget :: Maybe String
  , requestTargets :: [String]
  , requestDumpCore :: Bool
  , requestAllClosed :: Bool
  , requestTargetModuleOnly :: Bool
  , requestIncludes :: [FilePath]
  , requestFiles :: [FilePath]
  , requestBindGen :: Maybe Word64
  , requestSessionRoot :: Maybe FilePath
  , requestInjectVals :: [String]
  , requestTurn :: Bool
  , requestTurnTemplates :: [(String, FilePath)]
  , requestTurnOut :: Maybe FilePath
  , requestTurnVerdict :: Maybe String
  , requestClassify :: Bool
  , requestClassifyOut :: Maybe FilePath
  , requestCell :: Bool
  , requestCellTemplate :: Maybe FilePath
  , requestCellOut :: Maybe FilePath
  , requestTurnPin :: Maybe String
  , requestHarnessProfile :: Bool
  , requestBuildProductsDir :: Maybe FilePath
  , requestInspections :: [InspectionRequest]
  , requestInspectOut :: Maybe FilePath
  }
  deriving (Eq, Show)

emptyWorkerRequest :: WorkerRequest
emptyWorkerRequest = WorkerRequest
  { requestOutDir = Nothing
  , requestTarget = Nothing
  , requestTargets = []
  , requestDumpCore = False
  , requestAllClosed = False
  , requestTargetModuleOnly = False
  , requestIncludes = []
  , requestFiles = []
  , requestBindGen = Nothing
  , requestSessionRoot = Nothing
  , requestInjectVals = []
  , requestTurn = False
  , requestTurnTemplates = []
  , requestTurnOut = Nothing
  , requestTurnVerdict = Nothing
  , requestClassify = False
  , requestClassifyOut = Nothing
  , requestCell = False
  , requestCellTemplate = Nothing
  , requestCellOut = Nothing
  , requestTurnPin = Nothing
  , requestHarnessProfile = False
  , requestBuildProductsDir = Nothing
  , requestInspections = []
  , requestInspectOut = Nothing
  }

data InspectionRequest
  = InspectTypeOf String
  | InspectNameInfo String
  | InspectModule String Bool
  deriving (Eq, Show)

requestFromFields :: [RequestField] -> WorkerRequest
requestFromFields = foldl apply emptyWorkerRequest
  where
    apply request field = case field of
      Input path -> request { requestFiles = requestFiles request ++ [path] }
      OutputDir path -> request { requestOutDir = Just path }
      Target name -> request { requestTarget = Just name }
      Targets names -> request { requestTargets = requestTargets request ++ names }
      DumpCore -> request { requestDumpCore = True }
      AllClosed -> request { requestAllClosed = True }
      TargetModuleOnly -> request { requestTargetModuleOnly = True }
      Include path -> request { requestIncludes = requestIncludes request ++ [path] }
      BindGen generation -> request { requestBindGen = Just generation }
      SessionRoot path -> request { requestSessionRoot = Just path }
      InjectVal name -> request { requestInjectVals = requestInjectVals request ++ [name] }
      Turn -> request { requestTurn = True }
      TurnTemplate kind path -> request
        { requestTurnTemplates = requestTurnTemplates request ++ [(kind, path)] }
      TurnOut path -> request { requestTurnOut = Just path }
      TurnVerdict verdict -> request { requestTurnVerdict = Just verdict }
      Classify -> request { requestClassify = True }
      ClassifyOut path -> request { requestClassifyOut = Just path }
      Cell -> request { requestCell = True }
      CellTemplate path -> request { requestCellTemplate = Just path }
      CellOut path -> request { requestCellOut = Just path }
      TurnPin pin -> request { requestTurnPin = Just pin }
      HarnessProfile -> request { requestHarnessProfile = True }
      BuildProductsDir path -> request { requestBuildProductsDir = Just path }
      InspectType expression -> request
        { requestInspections = requestInspections request ++ [InspectTypeOf expression] }
      InspectInfo name -> request
        { requestInspections = requestInspections request ++ [InspectNameInfo name] }
      InspectBrowse name -> request
        { requestInspections = requestInspections request ++ [InspectModule name False] }
      InspectBrowseExpanded name -> request
        { requestInspections = requestInspections request ++ [InspectModule name True] }
      InspectOut path -> request { requestInspectOut = Just path }

workerRequestFlag :: String
workerRequestFlag = "--worker-request-v6"

workerArgv :: [RequestField] -> [String]
workerArgv fields = [workerRequestFlag, encodeHex (encodeRequest fields)]

-- | Decode the worker's complete argv. The compiler boundary deliberately has
-- no command-line grammar: exactly one versioned payload is accepted.
workerRequestFromArgv :: [String] -> Either String (Maybe WorkerRequest)
workerRequestFromArgv [] = Right Nothing
workerRequestFromArgv [flag] | flag == workerRequestFlag = Left (workerRequestFlag ++ ": payload is required")
workerRequestFromArgv [flag, payload]
  | flag == workerRequestFlag = Just . requestFromFields <$> (decodeHex payload >>= decodeRequest)
workerRequestFromArgv _ = Left "worker requires exactly one versioned request"

type Parser a = BS.ByteString -> Either String (a, BS.ByteString)

decodeRequest :: BS.ByteString -> Either String [RequestField]
decodeRequest bytes = do
  let (magic, body) = BS.splitAt 8 bytes
  if magic /= "TPREQ006"
    then Left "worker request: unsupported magic or version"
    else do
      (count, rest) <- pWord32 body
      (fields, trailing) <- pN (fromIntegral count) pField rest
      if BS.null trailing
        then Right fields
        else Left "worker request: trailing bytes"

encodeRequest :: [RequestField] -> BS.ByteString
encodeRequest fields = "TPREQ006" <> putU32 (length fields) <> BS.concat (map encodeField fields)

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
  BindGen value -> BS.singleton 11 <> putU64 value
  SessionRoot value -> taggedText 12 value
  InjectVal value -> taggedText 13 value
  Turn -> BS.singleton 16
  TurnTemplate kind path -> BS.singleton 17 <> textFrame kind <> textFrame path
  TurnOut value -> taggedText 18 value
  TurnVerdict value -> taggedText 19 value
  Classify -> BS.singleton 20
  ClassifyOut value -> taggedText 21 value
  HarnessProfile -> BS.singleton 24
  BuildProductsDir value -> taggedText 25 value
  InspectType value -> taggedText 26 value
  InspectInfo value -> taggedText 27 value
  InspectOut value -> taggedText 28 value
  InspectBrowse value -> taggedText 29 value
  InspectBrowseExpanded value -> taggedText 30 value
  Cell -> BS.singleton 31
  CellTemplate value -> taggedText 32 value
  CellOut value -> taggedText 33 value
  TurnPin value -> taggedText 34 value

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
    9  -> retired tag
    10 -> retired tag
    11 -> mapParser BindGen pWord64 rest
    12 -> mapParser SessionRoot pText rest
    13 -> mapParser InjectVal pText rest
    14 -> retired tag
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
    26 -> mapParser InspectType pText rest
    27 -> mapParser InspectInfo pText rest
    28 -> mapParser InspectOut pText rest
    29 -> mapParser InspectBrowse pText rest
    30 -> mapParser InspectBrowseExpanded pText rest
    31 -> Right (Cell, rest)
    32 -> mapParser CellTemplate pText rest
    33 -> mapParser CellOut pText rest
    34 -> mapParser TurnPin pText rest
    _  -> Left ("worker request: unknown field tag " ++ show tag)
  where
    retired tag = Left ("worker request: retired field tag " ++ show tag)

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
