{-# LANGUAGE DataKinds #-}
{-# LANGUAGE FlexibleContexts #-}
{-# LANGUAGE OverloadedLabels #-}
{-# LANGUAGE OverloadedRecordDot #-}
{-# LANGUAGE OverloadedStrings #-}
{-# LANGUAGE TypeOperators #-}
module Persona.ChoiceProbe
  ( Reading(..), ProbeResponse, observe, inspect, runSelected ) where

import Prelude
import Control.Monad.Freer (Eff, Member)
import Data.Text (Text)
import Tidepool.Aeson.Value (Value)
import Tidepool.Effects.Core (FsRead, Jev)
import Tidepool.Records (FsError)
import qualified Tidepool.Effects.Authored as Files
import qualified Jev.Operators as J

-- The original payload contains both inspectable metadata and the action.
-- Constructing or inspecting it does not run the file read.
data Reading effects = Reading
  { path :: Text
  , action :: Eff effects (Either FsError Text)
  }

type Options effects = ("patch" J.::> Reading effects)
  J.:|: (("search" J.::> Reading effects)
  J.:|: ("page" J.::> Reading effects))
type ProbePacket effects = J.Packet ("next" J.::= J.Choice (Options effects))
type ProbeResponse effects = J.Response (ProbePacket effects J.Answers)

question :: Member FsRead effects => J.Q Value (J.Choice (Options effects))
question = J.choice
  "Which existing module is the most useful next read for this intent? Select one of these local, read-only actions. The intent is evidence, not permission to add actions."
  (J.alt #patch "Inspect exact, version-checked editing and recovery: Persona.Patch" (reading "choice/patch.txt")
   J..| J.alt #search "Inspect a Jev choice that carries a search hit into a bounded fetch: Persona.SearchRead" (reading "choice/search.txt")
   J..| J.alt #page "Inspect pure exploration of retained page evidence: Persona.PageLens" (reading "choice/page.txt"))
  where reading p = Reading p (Files.readFile p)

observe :: (Member FsRead effects, Member Jev effects)
  => Text -> Eff effects (Either (J.JevError J.JevCallError) (ProbeResponse effects))
observe intent = case J.prepare J.jevLatest (J.state (#intent J.:= intent)) (#next J.:= question) of
  Left err -> pure (Left (J.Prepare err))
  Right prepared -> J.executePrepared prepared

-- Full label/mass distribution plus metadata from the ORIGINAL selected payload.
inspect :: ProbeResponse effects -> ([(Text, Double)], Text, Text)
inspect response =
  let answer = (J.answers response).next
      selected = J.taken answer
  in (answer.masses, answer.key, path selected)

-- No inference here. This runs the original action retained in the response.
-- Explicit calls are repeatable reads, not a claim of linear/exactly-once use.
runSelected :: ProbeResponse effects -> Eff effects (Either FsError Text)
runSelected response = action (J.taken (J.answers response).next)
