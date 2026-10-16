{-# LANGUAGE DataKinds #-}
module BuiltinInstallers where

import Control.Monad.Freer (Eff)
import Tidepool.Actors.Workbench

workbench :: Eff BuiltinDispatcherEffects ()
workbench = installDefaultWorkbench

asyncWorkbench :: Eff BuiltinDispatcherEffects ()
asyncWorkbench = installDefaultAsyncWorkbench
