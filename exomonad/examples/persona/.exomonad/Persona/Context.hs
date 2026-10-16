{-# LANGUAGE FlexibleContexts #-}
module Persona.Context (ActivationError(..), RefreshError(..), activate, activateInitial, refresh) where

import Prelude
import Control.Lens (view)
import Control.Monad.Freer (Eff, Member)
import Data.Text (Text)
import qualified Data.Text as T
import Tidepool.Effects.Core (FsRead, ContextReadWrite)
import Tidepool.Records (FsError)
import Tidepool.Inspection.Display (Display(..))
import qualified Tidepool.Agent.Context as C
import qualified Persona.Orientation as Orientation

data ActivationError = MissingBlock | AmbiguousBlock | NotAuthoredText
  | InitialContextUnavailable C.InitialContextObservation
  | StageRefused [C.ContextProblem]
  deriving (Eq, Show)
instance Display ActivationError where displayTree = displayTree . T.pack . show

-- | The caller obtains this identifier from the context owner. It conveys no
-- extra authority. The runtime validates the synchronous edit at commit.
activate :: Member ContextReadWrite effects
  => C.ContextReference -> Text -> Eff effects (Either ActivationError C.ContextTransaction)
activate reference body = do
  current <- C.getContext
  let blocks = view C.contextBlocks current
      matches block = case C.blockProvenance block of
        C.AuthoredProvenance candidate _ -> candidate == Just reference
        C.NativeProvenance candidate -> candidate == reference
  case filter matches blocks of
    [] -> pure (Left MissingBlock)
    [C.Text {}] -> do
      case filter (\target -> C.targetReference target == reference && C.targetSelector target == C.MessageText 0) (C.textTargets current) of
        [target] -> do
          staged <- C.stageContext (C.contextPatch [C.replaceText target body])
          pure $ case staged of
            C.ContextStaged transaction -> Right transaction
            C.ContextStageRefused problems -> Left (StageRefused problems)
        [] -> pure (Left NotAuthoredText)
        _ -> pure (Left AmbiguousBlock)
    [_] -> pure (Left NotAuthoredText)
    _ -> pure (Left AmbiguousBlock)

-- | Read the owner's receipt without replaying startup. A historical reference
-- can now be absent: activate checks the current context instead of recreating it.
activateInitial :: Member ContextReadWrite effects
  => Text -> Eff effects (Either ActivationError C.ContextTransaction)
activateInitial body = do
  observed <- C.getInitialContext
  case observed of
    C.InitialContextPublished _ _ _ reference -> activate reference body
    other -> pure (Left (InitialContextUnavailable other))

-- Refresh is explicit and uses the same full-file rendering as startup. The
-- historical startup reference does not authorize recreating a removed block.
data RefreshError = MemoryReadFailed FsError | ActivationFailed ActivationError
  deriving (Eq, Show)
instance Display RefreshError where displayTree = displayTree . T.pack . show

refresh :: (Member FsRead effects, Member ContextReadWrite effects)
  => Eff effects (Either RefreshError C.ContextTransaction)
refresh = do
  observed <- Orientation.renderMemory
  case observed of
    Left problem -> pure (Left (MemoryReadFailed problem))
    Right body -> either (Left . ActivationFailed) Right <$> activateInitial body
