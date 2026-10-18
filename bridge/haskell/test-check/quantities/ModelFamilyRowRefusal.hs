{-# LANGUAGE DataKinds, OverloadedStrings #-}
module Main where
import Control.Monad.Freer (Eff)
import Data.Text (Text)
import Tidepool.Agent.Contract (AgentSpec, NoTools, defaultSpec)
import Tidepool.Model
model :: Model OpenAI
model = openAI "catalog" "openai" (OpenAIOptions OpenAILow)
wrong :: Eff '[ModelCall Argon] (ModelResult Text)
wrong = invokeModel (textTurn model (defaultSpec :: AgentSpec NoTools '[ModelCall Argon]) "bounded") "input"
main :: IO ()
main = pure ()
