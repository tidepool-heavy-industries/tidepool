{-# LANGUAGE PackageImports #-}
{-# LANGUAGE DataKinds #-}
module Expr where
import Control.Monad.Freer (Eff)
import qualified "tidepool-resume" Tidepool.Internal.Resume as TidepoolResume
import ExecutionClass (C(..))

__result :: Int
__result = fromInteger (c (0 :: Int))
__prepared = TidepoolResume.settle (pure __result :: Eff '[] Int)
__resume q value = TidepoolResume.settle (TidepoolResume.resumeLifted q value)
