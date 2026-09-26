{-# LANGUAGE DataKinds #-}
{-# LANGUAGE DeriveGeneric #-}
{-# LANGUAGE FlexibleContexts #-}
{-# LANGUAGE GADTs #-}
{-# LANGUAGE MonoLocalBinds #-}
{-# LANGUAGE OverloadedStrings #-}
{-# LANGUAGE QuasiQuotes #-}
{-# LANGUAGE ScopedTypeVariables #-}
{-# LANGUAGE TypeApplications #-}
{-# LANGUAGE TypeFamilies #-}
{-# LANGUAGE TypeOperators #-}

module LaunchFixture where
import Data.Text (Text)
import GHC.Generics (Generic)
import qualified Tidepool.Actor as Actor
import qualified Tidepool.Actor.Record as R
import Tidepool.Actors.Exomonad

data Launcher mode = Launcher
  { launcherState :: mode :- State (Maybe Text)
  , launchAndAwait :: mode :- Call () NoReply
  , saveReply :: mode :- Call (Either ResponseFailure (ResponseResult Text)) NoReply
  , readReply :: mode :- Call () (R.Reply (Maybe Text))
  } deriving Generic

launchDefinition =
  R.definition "launch-review" (Actor.Selected (knownEffects @ResearchEffects)) Launcher
    { launcherState = Nothing
    , saveReply = \result -> R.put (Just (either (const "unavailable") responseValue result))
    , readReply = \() -> R.get
    , launchAndAwait = \() -> do
        worker <- unfold (batch "resident" "review")
          (child (withContext (selected (\input -> input))
            (researching @Text currentCheckout
              (assignment [label|review|] ("reply once" :: Text)))))
        own <- R.self @Launcher
        _ <- R.forwardResult worker (saveReply own)
        pure ()
    }
