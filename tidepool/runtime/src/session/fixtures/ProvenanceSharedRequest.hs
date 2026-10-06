{-# LANGUAGE DataKinds, OverloadedStrings, TypeApplications #-}
module ProvenanceSharedRequest where

import Control.Monad.Freer (Eff)
import Data.Text (Text)
import Tidepool.Agent.Reply (Replies)
import qualified Tidepool.Agent.Ref as Ref
import qualified Tidepool.Actors.Internal.Agent as Agents

{-# OPAQUE emit #-}
emit :: Text -> Eff '[Replies] ()
emit input = do
  let target = Ref.internalAgentRef 17 1
      requestLabel = either (error . show) id (Agents.labelFromText "shared-site")
  _ <- Agents.request @() target (Agents.assignment requestLabel input)
  pure ()
