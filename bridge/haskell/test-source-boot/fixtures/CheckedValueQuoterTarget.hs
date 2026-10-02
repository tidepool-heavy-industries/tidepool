{-# LANGUAGE QuasiQuotes #-}
module CheckedValueQuoterTarget where

import Tidepool.Session.Val.G8 (answer)

__result :: Int
__result = [answer|quoted|]
