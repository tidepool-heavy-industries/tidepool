{-# LANGUAGE DeriveGeneric #-}
{-# LANGUAGE FlexibleContexts #-}
{-# LANGUAGE OverloadedStrings #-}
{-# LANGUAGE TypeFamilies #-}
{-# LANGUAGE TypeOperators #-}
module AgentSpec (agentSpec) where

import Control.Monad.Freer (Eff, Member)
import Data.Text (Text)
import qualified Data.Text as T
import GHC.Generics (Generic)
import Tidepool.Agent.Contract
import qualified Tidepool.Command as Cmd

data RetentionTools effects mode = RetentionTools
  { notebook :: HaskellTools effects mode
  , throwing :: mode :- Call Text Text
  , oversized :: mode :- Call Text Text
  , wide :: mode :- Call Text [Text]
  } deriving (Generic)

answer :: Member Cmd.Commands effects => Text -> Eff effects Text
answer _ = do
  job <- Cmd.run (Cmd.argv ["cat", "sources.txt"])
  pure (either (const "") id (Cmd.stdout job))

agentSpec :: (KnownToolEffects effects, AsyncEffects effects, Member Cmd.Commands effects)
  => AgentSpec (RetentionTools effects) effects
agentSpec = defaultSpec { specTools = RetentionTools
  { notebook = haskellTools
  , throwing = presentWith (\_ -> error "presenter exploded") (tool "Read once, then fail presentation." answer)
  , wide = presentWith (const "wide result retained") (tool "Retain more than a renderer list allowance." (fmap (replicate 10001) . answer))
  , oversized = presentWith (\_ -> T.replicate 400000 "λ") (tool "Read once, then exceed presentation allowance." answer)
  } }
