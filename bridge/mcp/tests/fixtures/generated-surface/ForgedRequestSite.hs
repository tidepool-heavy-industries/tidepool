{-# LANGUAGE DataKinds #-}
module ForgedRequestSite where
import Data.Coerce (coerce)
import Tidepool.Internal.RequestSite (RequestSite)
forgedReply :: RequestSite '[Int] Bool -> RequestSite '[Int] Int
forgedReply = coerce
