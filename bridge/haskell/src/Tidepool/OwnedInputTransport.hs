{-# LANGUAGE OverloadedStrings #-}

-- One matched acquisition format for exact scopes and candidate offers. Logical
-- roles and seals remain receiving authority; arena locations transport bytes.
module Tidepool.OwnedInputTransport
  ( OriginalInputKind(..), OriginalInputImage(..), InputAcquisition(..), decodeInputAcquisition ) where

import Codec.CBOR.Decoding
import Codec.CBOR.Write (toStrictByteString)
import Codec.CBOR.Encoding qualified as E
import Control.Monad (replicateM, unless, when)
import Crypto.Hash.SHA256 qualified as SHA
import Data.ByteString qualified as BS
import Data.Set qualified as Set
import Data.Text qualified as T
import Numeric (showHex)
import System.FilePath (isAbsolute)
import Tidepool.RequestInputs (OriginalInputReference(..), ownedArenaRange)

data OriginalInputKind = InputInterface | InputPackages | InputCertificate
  | InputCore | InputNative | InputCensus | InputGraph deriving (Eq,Ord,Show)
data OriginalInputImage = OriginalInputImage String (String,String) String
  [(OriginalInputKind,OriginalInputReference)] deriving (Eq,Show)
data InputAcquisition = FreshFiles | ContinueOwnedOriginals [OriginalInputImage]
  deriving (Eq,Show)

inputKindTag :: OriginalInputKind -> String
inputKindTag kind = case kind of
  InputInterface -> "iface"; InputPackages -> "packages"; InputCertificate -> "certificate"
  InputCore -> "core"; InputNative -> "native"; InputCensus -> "census"; InputGraph -> "graph"

inputKindBound :: OriginalInputKind -> Int
inputKindBound kind = case kind of
  InputPackages -> 4 * 1024 * 1024
  InputCertificate -> 4 * 1024 * 1024
  InputGraph -> 64 * 1024 * 1024
  InputNative -> 64 * 1024 * 1024
  _ -> 32 * 1024 * 1024

inputImageDigest :: String -> (String,String)
  -> [(OriginalInputKind,OriginalInputReference)] -> String
inputImageDigest producer (unit,name) parts = digest $ toStrictByteString $
  E.encodeListLen 5 <> text "TPORIGINALINPUT1" <> text producer <> text unit <> text name
    <> E.encodeListLen (fromIntegral (length parts)) <> foldMap part parts
  where
    text = E.encodeString . T.pack
    part (kind,reference) = E.encodeListLen 3 <> text (inputKindTag kind)
      <> text (originalInputSha256 reference) <> E.encodeWord64 (fromIntegral (originalInputLength reference))

decodeInputAcquisition :: Decoder s InputAcquisition
decodeInputAcquisition = do
  fields <- decodeListLen
  tag <- string
  case (tag,fields) of
    ("fresh-files",1) -> pure FreshFiles
    ("continue-originals",3) -> do
      arenas <- bounded 4096 $ do
        array 2
        endpoint <- absolute
        extent <- decodeWord64
        unless (extent <= 9223372036854775807) (fail "owned arena extent exceeds host range")
        pure (endpoint,toInteger extent)
      unique "original input arena endpoints" (map fst arenas)
      images <- bounded 4096 $ do
        array 5
        producer <- digestField
        key <- (,) <$> nonempty <*> nonempty
        imageSha <- digestField
        parts <- bounded 4102 $ do
          array 6
          kindTag <- string
          kind <- case kindTag of
            "iface" -> pure InputInterface; "packages" -> pure InputPackages
            "certificate" -> pure InputCertificate; "core" -> pure InputCore
            "native" -> pure InputNative; "census" -> pure InputCensus
            "graph" -> pure InputGraph
            _ -> fail "unsupported original input kind"
          path <- absolute
          sha <- digestField
          bytes <- decodeWord64
          unless (bytes > 0 && bytes <= fromIntegral (inputKindBound kind))
            (fail "owned original input exceeds its artifact bound")
          origins <- bounded 4096 absolute
          unique "original input origins" origins
          array 2
          index <- decodeWord
          offset <- decodeWord64
          unless (index < fromIntegral (length arenas)) (fail "owned input arena index is absent")
          let (endpoint,extent) = arenas !! fromIntegral index
          unless (toInteger offset + toInteger bytes <= extent) (fail "owned input range leaves its arena")
          transport <- either fail pure (ownedArenaRange endpoint extent (toInteger offset))
          pure (kind,OriginalInputReference path sha (fromIntegral bytes) origins transport)
        let identities = [(kind,originalInputSha256 reference) | (kind,reference) <- parts]
        unless (not (null parts) && and (zipWith (<) identities (drop 1 identities))
          && inputImageDigest producer key parts == imageSha)
          (fail "original input image differs from its complete content identity")
        unique "original input image paths" (map (originalInputPath . snd) parts)
        pure (OriginalInputImage producer key imageSha parts)
      unique "original input image owners" [key | OriginalInputImage _ key _ _ <- images]
      unless (sum [length parts | OriginalInputImage _ _ _ parts <- images] <= 65536)
        (fail "owned original input part inventory exceeds bound")
      pure (ContinueOwnedOriginals images)
    _ -> fail "unsupported exact input acquisition"


string :: Decoder s String
string = T.unpack <$> decodeString

nonempty :: Decoder s String
nonempty = string >>= \value -> if null value then fail "empty original input owner" else pure value

absolute :: Decoder s FilePath
absolute = string >>= \value -> if isAbsolute value then pure value else fail "original input path is not absolute"

digestField :: Decoder s String
digestField = string >>= \value -> if length value == 64 && all (\c -> c >= '0' && c <= '9' || c >= 'a' && c <= 'f') value
  then pure value else fail "invalid original input SHA256"

array :: Int -> Decoder s ()
array expected = decodeListLen >>= \actual -> unless (actual == expected) (fail "invalid original input row arity")

bounded :: Int -> Decoder s a -> Decoder s [a]
bounded limit parse = do
  count <- decodeListLen
  when (count > limit) (fail "original input row count exceeds bound")
  replicateM count parse

unique :: Ord a => String -> [a] -> Decoder s ()
unique label values = unless (Set.size (Set.fromList values) == length values) (fail ("duplicate " ++ label))

digest :: BS.ByteString -> String
digest = concatMap (\byte -> let digits = showHex byte "" in replicate (2-length digits) '0' ++ digits) . BS.unpack . SHA.hash
