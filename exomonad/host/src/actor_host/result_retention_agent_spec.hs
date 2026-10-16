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
import Tidepool.Aeson (FromJSON)
import qualified Tidepool.Command as Cmd
import Tidepool.Effects.Core (Console, say)

data ReadInput = ReadInput { input :: Text } deriving (Generic)
instance FromJSON ReadInput
instance JsonSchema ReadInput

data RetentionTools effects mode = RetentionTools
  { notebook :: HaskellTools effects mode
  , throwing :: mode :- Call ReadInput Text
  , oversized :: mode :- Call ReadInput Text
  , wide :: mode :- Call ReadInput [Text]
  , pruned :: mode :- Call ReadInput Text
  , budgeted :: mode :- Call ReadInput Text
  , late :: mode :- Call ReadInput Text
  , failed :: mode :- Call ReadInput Text
  } deriving (Generic)

answer :: (Member Cmd.Commands effects, Member Console effects) => ReadInput -> Eff effects Text
answer _ = do
  say "explicit handler output"
  job <- Cmd.run (Cmd.argv ["cat", "sources.txt"])
  _ <- Cmd.observe (Cmd.Observation Cmd.UntilFinished 8192) (Cmd.completedJob job)
  pure (either (const "") id (Cmd.stdout job))

agentSpec :: (KnownToolEffects effects, AsyncEffects effects, Member Cmd.Commands effects, Member Console effects)
  => AgentSpec (RetentionTools effects) effects
agentSpec = defaultSpec { specTools = RetentionTools
  { notebook = haskellTools
  , throwing = presentWith (\_ -> error "presenter exploded") (tool "Read once, then fail presentation." answer)
  , wide = presentWith (const "wide result retained") (tool "Retain more than a renderer list allowance." (fmap (replicate 10001) . answer))
  , late = presentWith (const "late result retained") (tool "Emit explicit text after automatic command output."
      (\input -> do { result <- answer input; say (T.replicate 13000 "λ"); pure result }))
  , failed = presentWith id (tool "Fail before semantic result publication."
      (\input -> answer input >> error "handler stopped before semantic result"))
  , pruned = presentWith (const "returned value selected") (tool "Prune explicitly emitted handler output." answer)
  , budgeted = presentWith (const (T.replicate 13000 "ω")) (tool "Bound explicit output and return presentation together."
      (\input -> say (T.replicate 40000 "λ") >> answer input))
  , oversized = presentWith (\_ -> T.replicate 400000 "λ") (tool "Read once, then exceed presentation allowance." answer)
  }
  , afterTool = Just (\call result -> pure (if toolCallName call == "pruned"
      then Pruned "pruned selection" (toolResultHandle result) else NoAnnotation))
  }
