{-# LANGUAGE FlexibleContexts #-}
{-# LANGUAGE OverloadedStrings #-}
module AgentSpec (agentSpec) where

import Control.Monad.Freer (Member)
import Tidepool.Agent.Contract
import qualified Tidepool.Command as Cmd
import qualified Project.Tools as Tools

agentSpec :: (KnownToolEffects effects, AsyncEffects effects, Member Cmd.Commands effects)
          => AgentSpec (Tools.SpecTools effects) effects
agentSpec = defaultSpec
  { specTools = Tools.tools
  , afterTool = Just afterDelivery
  }
  where
    afterDelivery call _
      | toolCallName call == "probe" || toolCallName call == "haskell" = do
          _ <- Cmd.quiet (Cmd.run (Cmd.argv
            [ "sh", "-c"
            , "printf ready > after-tool-ready; read release < after-tool-release"
            ]))
          pure (Annotated "presentation-finished")
      | otherwise = pure NoAnnotation
