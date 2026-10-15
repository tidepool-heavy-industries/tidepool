{-# LANGUAGE DeriveAnyClass #-}
{-# LANGUAGE DeriveGeneric #-}
{-# LANGUAGE FlexibleContexts #-}
{-# LANGUAGE OverloadedStrings #-}
{-# LANGUAGE TypeOperators #-}
module AgentSpec (agentSpec) where

import Control.Monad.Freer (Eff, Member)
import Data.Text (Text)
import qualified Data.Text as Text
import GHC.Generics (Generic)
import Tidepool.Aeson (FromJSON, Value(..))
import Tidepool.Agent.Contract
import Tidepool.Effects.Core (FsRead, FsWrite)
import qualified Tidepool.Effects.Authored as Files

data Input = Input { text :: Text, copies :: Int }
  deriving (Generic, FromJSON, JsonSchema)

data Tools mode = Tools
  { ordinary :: mode :- Call Input Text
  , synchronous :: mode :- Sync (Call Input Text)
  } deriving (Generic)

record :: (Member FsRead effects, Member FsWrite effects) => Text -> Eff effects ()
record name = Files.appendFile (".exomonad/state/" <> name) "x"
  >>= either (error . show) pure

large :: (Member FsRead effects, Member FsWrite effects) => Text -> Input -> Eff effects Text
large name (Input value count) = do
  record name
  pure (Text.replicate count value)

agentSpec :: (Member FsRead effects, Member FsWrite effects) => AgentSpec Tools effects
agentSpec = defaultSpec
  { specTools = Tools
      { ordinary = presentWith (const "ordinary complete") $ tool "Produce a large typed value." (large "ordinary")
      , synchronous = presentWith (const "synchronous complete") $ syncTool "Produce a large typed value." (large "synchronous")
      }
  , afterTool = Just $ \call result -> do
      case toolResultValue result of
        String value | value == Text.replicate 70000 "λ" -> record ("after-" <> toolCallName call)
        _ -> error "the after-tool input lost the complete semantic value"
      pure NoAnnotation
  }
