{-# LANGUAGE DataKinds #-}
{-# LANGUAGE FlexibleContexts #-}
{-# LANGUAGE OverloadedStrings #-}
{-# LANGUAGE ScopedTypeVariables #-}
{-# LANGUAGE TypeApplications #-}
{-# LANGUAGE TypeOperators #-}
module AgentSpec (agentSpec) where
import Control.Monad.Freer (Members)
import Tidepool.Agent.Contract
import Tidepool.Effects.Core (FsRead, FsWrite, Sleep)
import qualified Tidepool.Effects.Authored as Files
import Tidepool.Duration (seconds)
agentSpec
  :: forall effects.
     ( KnownToolEffects effects, AsyncEffects effects
     , Subset effects (SyncEffects effects), Members '[FsRead, FsWrite, Sleep] effects)
  => AgentSpec (HaskellTools effects) effects
agentSpec = (defaultWorkbenchSpec @effects)
  { initialContext = Just (\_ -> do
      before <- Files.readFile "initializer-count.txt" >>= either (error . show) pure
      Files.writeFile "initializer-count.txt" (before <> "x") >>= either (error . show) pure
      pause <- Files.readFile "pause-initializer.txt" >>= either (error . show) pure
      if pause == "pause" then Files.sleep (seconds 60) else pure ()
      pure (ContextSeed [ContextSection "restart witness" Nothing "initialization is retained"])) }
