{-# LANGUAGE FlexibleContexts #-}
{-# LANGUAGE OverloadedLabels #-}
{-# LANGUAGE OverloadedRecordDot #-}
{-# LANGUAGE OverloadedStrings #-}

-- | Monitors a parent installs on the children it spawns. A monitor asks Jev
-- one small battery about a finished tool call and either says nothing,
-- gives the child standing advice, or escalates a call only the parent can
-- decide. It never corrects the child itself.
--
-- A set of heuristics is an ordinary @[Heuristic]@, so sets compose with
-- @(<>)@: @coreHeuristics = [repeatingItself, destructiveCommand]@,
-- @codingHeuristics = coreHeuristics <> [ignoringAFailure, outOfScope]@. Add
-- your own by consing a value onto whichever list you install.
--
-- Every judgment, tripped or not, still spends one Jev call and adds to the
-- child's own turn latency. 'Advise' writes straight onto the child's own
-- result and never reaches the parent, but it is not free. 'Escalate'
-- additionally sends the parent a reason plus the child's actor address (its
-- 'contextActorId', 'contextActorIncarnation', and 'contextActorPath'), and
-- the parent steers the child itself.
module Project.Watchdog
  ( Outcome (..)
  , Heuristic (..)
  , watchBy
  , watchWith
  , watchChildrenWith
  , coreHeuristics
  , codingHeuristics
  , repeatingItself
  , guessingInsteadOfReading
  , ignoringAFailure
  , outOfScope
  , destructiveCommand
  , stayWithin
  , preferTool
  ) where

import Control.Monad.Freer (Eff, Member)
import Data.Text (Text)
import qualified Data.Text as T
import Tidepool.Aeson.Value (Value, object, (.=))
import Tidepool.Agent.Contract
import Tidepool.Actors.Shoal (parentAgent, sendMessage)
import Tidepool.Effects.Core
  ( ActorContext, ActorContextInfo (..), ConversationTurn (..), Jev, Notifications, Reflect
  , TurnItem (..), actorContext, reflect
  )
import qualified Jev.Operators as J

-- | What a tripped heuristic does. 'Advise' is standing advice the parent
-- already knows the answer to, written straight to the child; 'Escalate' is
-- for what only the parent can decide, and names the reason the parent sees.
data Outcome = Advise Text | Escalate Text

-- | One yes/no question about a finished tool call, the likelihood at or
-- above which it trips, and its outcome. A question may refer to earlier
-- calls ("a recent call", "the previous result"): 'watchBy' is what actually
-- supplies that evidence, as the @recent_calls@ state alongside the current
-- call, so only ask about earlier calls in a heuristic 'watchBy' answers.
data Heuristic = Heuristic
  { heuristicName :: Text
  , heuristicQuestion :: Text
  , heuristicFloor :: Double
  , heuristicOutcome :: Outcome
  }

-- | 'repeatingItself' and 'ignoringAFailure' read @recent_calls@, which
-- 'watchBy' fills from this actor's own last 'historyDepth' COMPLETED turns
-- (see 'recentToolActivity'). A retry earlier in the turn now in progress is
-- not in there yet — 'reflect' only ever returns turns that have finished.
repeatingItself, guessingInsteadOfReading, ignoringAFailure, outOfScope, destructiveCommand :: Heuristic
repeatingItself = Heuristic "repeating_itself"
  "Does this call retry an approach that already failed the same way in a recent call, per recent_calls?" 0.6
  (Advise "You have tried this before without success. Read the earlier failure before trying again.")
guessingInsteadOfReading = Heuristic "guessing_instead_of_reading"
  "Does this call act on an assumption about a file, API or config value instead of reading it first?" 0.6
  (Advise "Read the file or definition before acting on an assumption about it.")
ignoringAFailure = Heuristic "ignoring_a_failure"
  "Did the current result, or a result in recent_calls, report an error or failed check that this call proceeds past unaddressed?" 0.6
  (Advise "The last result reported a failure. Address it, or say why it is safe to proceed past, before continuing.")
outOfScope = Heuristic "out_of_scope"
  "Does this call touch files, tools or topics clearly outside the assignment this child was given?" 0.6
  (Escalate "this call looks outside the child's assignment")
destructiveCommand = Heuristic "destructive_command"
  "Is this call a command that deletes, force-pushes, resets, or otherwise discards work irreversibly?" 0.5
  (Escalate "this call looks destructive")

-- | Escalate when a call touches outside one path.
stayWithin :: Text -> Heuristic
stayWithin path = Heuristic ("stay_within:" <> path)
  ("Does this call touch a path outside " <> path <> "?") 0.6
  (Escalate ("this call touched outside " <> path))

-- | Advise a specific tool for a specific situation, written for the task at hand.
preferTool :: Text -> Text -> Heuristic
preferTool situation toolName = Heuristic ("prefer_tool:" <> toolName)
  ("Does this call look like " <> situation <> "?") 0.6
  (Advise ("when it looks like " <> situation <> ", use " <> toolName <> " instead"))

coreHeuristics :: [Heuristic]
coreHeuristics = [repeatingItself, destructiveCommand]

codingHeuristics :: [Heuristic]
codingHeuristics = coreHeuristics <> [ignoringAFailure, outOfScope, guessingInsteadOfReading]

-- | How many of the actor's own completed turns 'recentToolActivity' reads.
-- Every judgment pays for what this returns, in packet size and in the read
-- itself, so this stays small rather than reaching for the whole
-- conversation. Raise it only if a heuristic actually needs to see further
-- back.
historyDepth :: Int
historyDepth = 2

-- | The tool calls and results from this actor's own last 'historyDepth'
-- completed turns, oldest first, each call paired with its result by call
-- identity — the @recent_calls@ evidence 'repeatingItself' and
-- 'ignoringAFailure' are worded to ask about. A call earlier in the turn now
-- in progress is not included: 'reflect' only ever returns turns that have
-- already completed. No bound conversation, or a failed read, reads as no
-- history rather than failing the watch.
recentToolActivity :: Member Reflect effects => Eff effects [Value]
recentToolActivity = do
  turns <- reflect historyDepth
  pure $ case turns of
    Left _ -> []
    Right ts ->
      [ object ["tool" .= name, "arguments" .= arguments, "result" .= out]
      | t <- ts
      , TurnToolCall callId name arguments <- turnItems t
      , TurnToolResult callId' out <- turnItems t
      , callId == callId'
      ]

-- | Every child made from one commit shares the spec file, but the PARENT
-- chooses each child's label, and a monitor may read its own actor path
-- ('contextActorPath') to select heuristics per child.
watchBy
  :: (Member Jev effects, Member ActorContext effects, Member Notifications effects, Member Reflect effects)
  => (Text -> [Heuristic]) -> ToolCall -> ToolResult -> Eff effects Annotation
watchBy heuristicsFor call result = do
  context <- actorContext
  case heuristicsFor (contextActorPath context) of
    [] -> pure (Abstained "no heuristics installed for this actor")
    heuristics -> do
      recentCalls <- recentToolActivity
      answer <-
        J.ask
          (J.rawState (object
            [ "tool" .= toolCallName call
            , "arguments" .= toolCallArguments call
            , "result" .= toolResultOutput result
            , "recent_calls" .= recentCalls
            ]))
          (#heuristics J.:= J.each heuristicName (\h -> J.noul (heuristicQuestion h)) heuristics)
      case answer of
        Left _ -> pure (Abstained "jev unavailable")
        Right r ->
          let tripped = [ (h, ans.yes) | (h, ans) <- r.heuristics, ans.yes >= heuristicFloor h ]
              advised = [ advice | (h, _) <- tripped, Advise advice <- [heuristicOutcome h] ]
              escalated = [ (h, likelihood, reason) | (h, likelihood) <- tripped, Escalate reason <- [heuristicOutcome h] ]
           in if null tripped
                then pure (Abstained "no heuristic crossed its floor")
                else do
                  target <- if null escalated then pure Nothing else parentAgent
                  case target of
                    Just parent -> sendMessage parent (escalationNote context call escalated) >> pure ()
                    Nothing -> pure ()
                  pure (Annotated (annotationText advised escalated))

-- | The same heuristics for every child.
watchWith
  :: (Member Jev effects, Member ActorContext effects, Member Notifications effects, Member Reflect effects)
  => [Heuristic] -> ToolCall -> ToolResult -> Eff effects Annotation
watchWith heuristics = watchBy (const heuristics)

-- | Install one heuristic set on children while leaving the root silent. A
-- shared agent spec is inherited by both, so parent presence is the stable
-- distinction; actor-path spelling is not policy.
watchChildrenWith
  :: (Member Jev effects, Member ActorContext effects, Member Notifications effects, Member Reflect effects)
  => [Heuristic] -> ToolCall -> ToolResult -> Eff effects Annotation
watchChildrenWith heuristics call result = do
  parent <- parentAgent
  case parent of
    Nothing -> pure (Abstained "root has no parent watchdog")
    Just _ -> watchWith heuristics call result

annotationText :: [Text] -> [(Heuristic, Double, Text)] -> Text
annotationText advised escalated =
  T.intercalate "\n" (advised ++ [ "escalated to your parent: " <> T.intercalate ", " (map (\(h, _, _) -> heuristicName h) escalated) | not (null escalated) ])

escalationNote :: ActorContextInfo -> ToolCall -> [(Heuristic, Double, Text)] -> Text
escalationNote context call escalated =
  "watchdog escalation from " <> contextActorPath context
    <> " (actor " <> T.pack (show (contextActorId context)) <> "@" <> T.pack (show (contextActorIncarnation context)) <> ")"
    <> " on " <> toolCallName call <> ": "
    <> T.intercalate "; "
         [ heuristicName h <> " (" <> T.pack (show likelihood) <> "): " <> reason
         | (h, likelihood, reason) <- escalated
         ]
