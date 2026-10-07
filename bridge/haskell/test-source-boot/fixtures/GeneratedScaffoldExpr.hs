{-# LANGUAGE PackageImports #-}
{-# LANGUAGE DataKinds #-}
module Expr where
import Control.Monad.Freer (Eff)
import qualified "tidepool-resume" Tidepool.Internal.Resume as TidepoolResume
import Prelude
__result :: Int
__result = case TidepoolResume.settle (pure answer :: Eff '[] Int) of
  TidepoolResume.Done value -> value
  TidepoolResume.Suspended _ _ -> error "unexpected effect"
__prepared = TidepoolResume.settle (pure __result :: Eff '[] Int)
__resume q value = TidepoolResume.settle (TidepoolResume.resumeLifted q value)
