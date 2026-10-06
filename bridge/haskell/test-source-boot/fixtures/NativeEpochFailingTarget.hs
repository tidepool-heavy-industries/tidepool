{-# LANGUAGE DataKinds, TemplateHaskell #-}
module NativeEpochTarget where

import Control.Concurrent (threadDelay)
import Control.Monad (unless)
import Data.Proxy (Proxy(..))
import Language.Haskell.TH (runIO)
import NativeEpochProvider (nativeType)
import NativeEpochMissing (missingType)
import System.Directory (doesFileExist)
import System.Timeout (timeout)

$(do
    _ <- nativeType
    runIO $ do
      writeFile "EPOCH_REACHED" "actual native call completed\n"
      let wait = doesFileExist "EPOCH_RELEASE" >>= \released ->
            unless released (threadDelay 10000 >> wait)
      released <- timeout 20000000 wait
      case released of
        Just () -> pure ()
        Nothing -> fail "physical native epoch observer did not release the remote splice within 20 seconds"
    pure [])

__result :: Proxy $(missingType)
__result = Proxy
