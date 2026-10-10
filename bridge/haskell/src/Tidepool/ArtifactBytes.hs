-- | Owned immutable encoded bytes. Content identity carries no semantic,
-- filesystem, admission or publication authority.
module Tidepool.ArtifactBytes
  ( ArtifactBytes, captureArtifactBytes, artifactBytes, artifactSha256
  , artifactDigestBytes, artifactLength, artifactContentKey, checkArtifactSeal
  ) where

import qualified Crypto.Hash.SHA256 as SHA
import qualified Data.ByteString as BS
import Numeric (showHex)

data ArtifactBytes = ArtifactBytes !BS.ByteString !BS.ByteString !String

instance Eq ArtifactBytes where
  a == b = artifactContentKey a == artifactContentKey b

instance Show ArtifactBytes where
  show body = "ArtifactBytes " ++ show (artifactContentKey body)

-- Force the digest and its rendering at acquisition; consumers never hash an
-- existing body again. The strict ByteString is shared with its issuing owner.
captureArtifactBytes :: BS.ByteString -> ArtifactBytes
captureArtifactBytes bytes =
  let digest = SHA.hash bytes
      seal = concatMap (\byte -> let rendered = showHex byte ""
        in replicate (2 - length rendered) '0' ++ rendered) (BS.unpack digest)
  in length seal `seq` ArtifactBytes bytes digest seal

artifactBytes :: ArtifactBytes -> BS.ByteString
artifactBytes (ArtifactBytes bytes _ _) = bytes

artifactSha256 :: ArtifactBytes -> String
artifactSha256 (ArtifactBytes _ _ seal) = seal

artifactDigestBytes :: ArtifactBytes -> BS.ByteString
artifactDigestBytes (ArtifactBytes _ digest _) = digest

artifactLength :: ArtifactBytes -> Int
artifactLength = BS.length . artifactBytes

artifactContentKey :: ArtifactBytes -> (String,Int)
artifactContentKey body = (artifactSha256 body,artifactLength body)

checkArtifactSeal :: String -> ArtifactBytes -> Either String ()
checkArtifactSeal expected body
  | expected == artifactSha256 body = Right ()
  | otherwise = Left "artifact bytes differ from the expected seal"
