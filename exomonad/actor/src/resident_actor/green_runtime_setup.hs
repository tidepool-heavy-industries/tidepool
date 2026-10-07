{-# LANGUAGE FlexibleContexts #-}
import Tidepool.Prelude hiding (error, note, print)
import Control.Monad.Freer (Eff, Member, send)
import qualified Tidepool.Effects.Core as Core

let awaitGreenForm :: Member Core.AskUser effects => Text -> a -> Eff effects a
    awaitGreenForm name original = do
      opened <- send (Core.FormOpenWith (toJSON name))
      lease <- case opened of
        Right value -> pure value
        Left _ -> Core.error "controlled form open failed"
      answered <- send (Core.FormAwaitWith lease)
      attempt <- case answered of
        Right (Core.FormSubmitted value _) -> pure value
        _ -> Core.error "controlled form answer failed"
      committed <- send (Core.FormCommitWith lease attempt (toJSON name))
      case committed of
        Right Core.FormApplied -> pure original
        _ -> Core.error "controlled form commit failed"
