{-# LANGUAGE NoImplicitPrelude #-}
module DeploymentNativeConsumer where

import Data.Text (Text)
import qualified Tidepool.FilePath as Path

result :: Text -> Text
result = Path.normalise
