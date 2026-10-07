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
import qualified Tidepool.Agent.Contract as A

data Launcher mode = Launcher
  { launcherState :: mode :- State (Maybe Text)
  , launchAndAwait :: mode :- Call () NoReply
  , saveReply :: mode :- Call (Either ResponseFailure (ResponseResult Text)) NoReply
  , readReply :: mode :- Call () (R.Reply (Maybe Text))
  } deriving Generic

launchDefinition =
  R.definition "launch-review" (Actor.Selected (knownEffects @'[AgentLaunch, Replies, Actor])) Launcher
    { launcherState = Nothing
    , saveReply = \result -> R.put (Just (either (const "unavailable") responseValue result))
    , readReply = \() -> R.get
    , launchAndAwait = \() -> do
        Right workerAgent <- spawnSubagent (FreshCtx "Return the requested typed result.")
          (ForkWorktree currentCheckout)
          ((defaultSpawnOptions (A.defaultWorkbenchSpec @'[Replies, Commands, Lookup, BoundWorktree]))
            { spawnLabel = Just "review", spawnLifetime = ActorOwned })
        Right worker <- request @Text workerAgent ("reply once" :: Text)
          (defaultRequestOptions { requestLabel = Just "review" })
        own <- R.self @Launcher
        _ <- R.forwardResult worker (saveReply own)
        pure ()
    }
