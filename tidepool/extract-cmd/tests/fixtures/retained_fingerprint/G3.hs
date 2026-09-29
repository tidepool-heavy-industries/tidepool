module Tidepool.Session.Val.G3 (job2) where
import qualified Tidepool.Command.Types as TidepoolCarrierType
import qualified GHC.Magic as TidepoolCarrierMagic
{-# NOINLINE job2 #-}
job2 :: TidepoolCarrierType.Job
job2 = TidepoolCarrierMagic.lazy job2
