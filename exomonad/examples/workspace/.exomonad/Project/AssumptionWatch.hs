{-# LANGUAGE DataKinds #-}
{-# LANGUAGE DeriveGeneric #-}
{-# LANGUAGE FlexibleContexts #-}
{-# LANGUAGE OverloadedStrings #-}
{-# LANGUAGE TypeFamilies #-}
{-# LANGUAGE TypeOperators #-}

-- | Observe typed changes and apply a caller's policy without a model turn.
-- Event sources own ordering and lifetime. This actor retains the latest value
-- and decision; it neither polls nor retries an unresolved judgment.
module Project.AssumptionWatch
  ( AssumptionWatch (assumptionView)
  , AssumptionEffects
  , Change (..)
  , ChangeDecision (..)
  , AssumptionState (..)
  , assumptionLastChange
  , AssumptionChange (..)
  , BaselineStatus (..)
  , assumptionDefinition
  , watchAssumption
  , watchIncorporatedBaseline
  ) where

import Control.Monad.Freer (Eff, Member, raise)
import Data.Maybe (listToMaybe)
import Data.Text (Text)
import qualified Data.Text as Text
import GHC.Generics (Generic)
import qualified Tidepool.Actor as Actor
import qualified Tidepool.Actor.Record as R
import Tidepool.Actors.Exomonad
import Tidepool.Effects.Core (Actor, Jev)
import Tidepool.Effects.Row (knownEffects)
import Tidepool.Worktree (renderGitOid)
import Project.Types (Incorporation (..))

-- | The policy receives domain values, not their rendered descriptions.
data Change value = Change
  { changeBefore :: value
  , changeAfter :: value
  } deriving (Show, Eq)

-- | Reasons remain inspectable even when no notification is needed.
-- Unresolved judgments notify the owner; they never silently mean irrelevant.
data ChangeDecision
  = IgnoreChange Text
  | ReportChange Text
  | UnresolvedChange Text
  deriving (Show, Eq)

data AssumptionChange value = AssumptionChange
  { assumptionChange :: Change value
  , changeDecision :: ChangeDecision
  , changeNotice :: Maybe (Either NotificationError NotificationReceipt)
  }

instance Show value => Show (AssumptionChange value) where
  show observed = "AssumptionChange " ++ show (assumptionChange observed)
    ++ " " ++ show (changeDecision observed)
    ++ " notice=" ++ case changeNotice observed of
      Nothing -> "none"
      Just (Left refusal) -> show refusal
      Just (Right _) -> "admitted"

data AssumptionState value = AssumptionState
  { assumptionCurrent :: value
  , assumptionRecentChanges :: [AssumptionChange value]
  , assumptionChangeCount :: Int
  }

-- Newest first, bounded to 32 decisions. The count includes evicted decisions;
-- an ignored later event does not immediately erase the preceding alert.
assumptionLastChange :: AssumptionState value -> Maybe (AssumptionChange value)
assumptionLastChange = listToMaybe . assumptionRecentChanges

-- Full before/after values and exact receipts remain in assumptionRecentChanges.
instance Show value => Show (AssumptionState value) where
  show state = "AssumptionState current=" ++ show (assumptionCurrent state)
    ++ " changes=" ++ show (assumptionChangeCount state)
    ++ " lastDecision=" ++ show (changeDecision <$> assumptionLastChange state)

data AssumptionWatch value observation mode = AssumptionWatch
  { assumptionState :: mode :- State (AssumptionState value)
  , assumptionView :: mode :- Call () (R.Reply (AssumptionState value))
  , assumptionEvents :: mode :- Event observation
  } deriving Generic

type AssumptionEffects value observation =
  LocalEffects (AssumptionWatch value observation) '[Replies, Actor, Notifications, Jev]

data BaselineStatus
  = BaselineAt GitOid
  | BaselineUnavailable Text
  deriving (Show, Eq)

-- | Project an event onto the domain value that matters. Nothing ignores an
-- event; equal values skip the policy entirely. A changed value runs the policy
-- once, which can use Jev. Lift a deterministic policy with @pure . decide@.
--
-- The callback's ordinary effect result contains the decision; it does not need
-- access to the watcher's private state. Supply enough context in a report for
-- its recipient to act. Full typed before/after values remain in the snapshot.
assumptionDefinition
  :: Eq value
  => AgentRef
  -> value
  -> R.EventSource observation
  -> (observation -> Maybe value)
  -> (Change value -> Eff (AssumptionEffects value observation) ChangeDecision)
  -> ActorSpec (AssumptionWatch value observation) (AssumptionEffects value observation)
assumptionDefinition owner initial source project decide =
  R.definition "assumption-watch" (Actor.Selected knownEffects) AssumptionWatch
    { assumptionState = AssumptionState initial [] 0
    , assumptionView = \() -> R.get
    , assumptionEvents = R.on source $ \observation -> case project observation of
        Nothing -> pure ()
        Just current -> do
          prior <- R.gets assumptionCurrent
          if prior == current then pure () else do
            let change = Change prior current
            decision <- raise (decide change)
            receipt <- case decision of
              IgnoreChange _ -> pure Nothing
              ReportChange message -> Just <$> sendMessage owner message
              UnresolvedChange reason -> Just <$> sendMessage owner
                ("Assumption judgment unresolved: " <> reason)
            R.modify' $ \state -> state
              { assumptionCurrent = current
              , assumptionRecentChanges = take 32
                  (AssumptionChange change decision receipt : assumptionRecentChanges state)
              , assumptionChangeCount = assumptionChangeCount state + 1
              }
    }

watchAssumption
  :: (Eq value, Member Actor effects)
  => AgentRef
  -> value
  -> R.EventSource observation
  -> (observation -> Maybe value)
  -> (Change value -> Eff (AssumptionEffects value observation) ChangeDecision)
  -> Eff effects (ActorHandle (AssumptionWatch value observation))
watchAssumption owner initial source project decide =
  R.start (assumptionDefinition owner initial source project decide)

-- | A pending child's source advance is a deterministic specialization of the
-- same watcher. The exact incorporation response remains the source of truth.
watchIncorporatedBaseline
  :: Member Actor effects
  => AgentRef -> GitOid -> Text -> Response Incorporation
  -> Eff effects (ActorHandle (AssumptionWatch BaselineStatus (Either ResponseFailure (ResponseResult Incorporation))))
watchIncorporatedBaseline owner baseline pendingChild incorporation =
  watchAssumption owner (BaselineAt baseline)
    (R.settlement incorporation) project (pure . decide)
  where
    project (Right result) = Just $ case responseValue result of
      Incorporated _ headOid _ -> BaselineAt headOid
      IncorporationBlocked _ reason evidence -> BaselineUnavailable
        (reason <> "; evidence: " <> Text.intercalate ", " evidence)
    project (Left failure) = Just (BaselineUnavailable (Text.pack (show failure)))
    describe (BaselineAt oid) = renderGitOid oid
    describe (BaselineUnavailable reason) = "unavailable: " <> reason
    decide change = ReportChange $ Text.unlines
      [ "Revisit " <> pendingChild <> ": its incorporated source changed."
      , "Before: " <> describe (changeBefore change)
      , "After: " <> describe (changeAfter change)
      ]
