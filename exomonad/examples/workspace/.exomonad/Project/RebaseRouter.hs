{-# LANGUAGE DataKinds #-}
{-# LANGUAGE DeriveGeneric #-}
{-# LANGUAGE FlexibleContexts #-}
{-# LANGUAGE GADTs #-}
{-# LANGUAGE OverloadedLabels #-}
{-# LANGUAGE OverloadedRecordDot #-}
{-# LANGUAGE OverloadedStrings #-}
{-# LANGUAGE TypeApplications #-}
{-# LANGUAGE TypeFamilies #-}
{-# LANGUAGE TypeOperators #-}

-- | Advise children when an integration advance overlaps work they have
-- already committed. Git establishes ancestry and path overlap; Jev is only
-- consulted for the ambiguous middle, and this actor never rebases for them.
module Project.RebaseRouter
  ( RebaseRouter (..)
  , RebaseRouterEffects
  , RouterState (..)
  , Child (..)
  , RebaseFacts (..)
  , Advice (..)
  , rebaseRouter
  , exactFacts
  , adviceFor
  ) where

import Control.Monad (forM, forM_)
import Data.Map.Strict (Map)
import qualified Data.Map.Strict as Map
import Data.Set (Set)
import qualified Data.Set as Set
import Data.Text (Text)
import qualified Data.Text as Text
import GHC.Generics (Generic)
import qualified Jev.Operators as J
import Jev.Operators (Packet ((:=), (:&)))
import qualified Tidepool.Actor as Actor
import qualified Tidepool.Actor.Record as R
import qualified Tidepool.Command as Cmd
import qualified Tidepool.Event as Event
import Tidepool.Actors.Exomonad hiding (WorktreeHandle, start)
import Tidepool.Aeson.Value (Value (String), object, (.=), toJSON)
import Tidepool.Effects.Core
  ( Actor
  , BoundWorktree
  , Commands
  , GitOid (..)
  , Journal
  , Jev
  , Notifications
  , Sleep
  , WorktreeHandle (..)
  , WorktreeId
  , WorktreeRegistry
  , WorktreeReceipt (..)
  )
import Tidepool.Effects.Row (knownEffects)
import Tidepool.Effects (RepoEvent, sleep)
import Tidepool.Journal (record)

data Child = Child
  { childAgent :: AgentRef
  , childWorktree :: WorktreeId
  , childBase :: GitOid
  , childPaths :: Set Text
  }
  deriving (Show, Generic)

data RouterState = RouterState
  { routerChildren :: Map Text Child
  , routerIntegration :: Maybe WorktreeHandle
  , routerSubscription :: Maybe Event.SubscriptionId
  , routerStopping :: Bool
  }
  deriving (Generic)

data RebaseRouter mode = RebaseRouter
  { routerState :: mode :- State RouterState
  , register :: mode :- Call (Text, AdmissionReceipt) NoReply
  , start :: mode :- Call () NoReply
  , tick :: mode :- Call () NoReply
  , stop :: mode :- Call () NoReply
  }
  deriving Generic

type RebaseRouterEffects = R.LocalEffects RebaseRouter
  '[ Replies, BoundWorktree, WorktreeRegistry, RepoEvent, Commands, Actor
   , Notifications, Jev, Journal, Sleep
   ]

data RebaseFacts = RebaseFacts
  { factsBehind :: Bool
  , factsOverlap :: Set Text
  }
  deriving (Show, Eq)

data Advice
  = SendNudge
  | DeferJudgment
  | RecordOnly
  deriving (Show, Eq)

-- | Pure exact facts: no rebase is possible when the child is not behind, and
-- only paths changed by the advance can overlap the child's committed paths.
exactFacts :: Bool -> [Text] -> Set Text -> RebaseFacts
exactFacts behind advancedPaths childPaths = RebaseFacts
  { factsBehind = behind
  , factsOverlap = if behind then Set.fromList advancedPaths `Set.intersection` childPaths else Set.empty
  }

-- | Exact overlap is sufficient to advise. Otherwise Jev's likelihood uses
-- a high floor to send, an explicit middle band to defer, and a low answer to
-- record without interrupting the child.
adviceFor :: RebaseFacts -> Double -> Advice
adviceFor facts likelihood
  | not (factsBehind facts) = RecordOnly
  | not (Set.null (factsOverlap facts)) = SendNudge
  | likelihood >= 0.75 = SendNudge
  | likelihood >= 0.35 = DeferJudgment
  | otherwise = RecordOnly

rebaseRouter :: WorktreeId -> ActorSpec RebaseRouter RebaseRouterEffects
rebaseRouter integration = R.withWorktree integration $
  R.definition "rebase-router" (Actor.Selected knownEffects) RebaseRouter
    { routerState = RouterState Map.empty Nothing Nothing False
    , register = registerChild
    , start = startRouter
    , tick = tickRouter
    , stop = stopRouter
    }

registerChild :: (Text, AdmissionReceipt) -> Handler RouterState RebaseRouterEffects ()
registerChild (label, receipt) = do
  let launched = launchedWorktree receipt
  found <- lookupWorktree (treeId launched)
  case found of
    Left failure -> record "rebase-router" label
      (object ["outcome" .= ("register_failed" :: Text), "error" .= Text.pack (show failure)])
    Right handle -> do
      initialPaths <- gitText (cwd (handleReceipt handle))
        ["diff", "--name-only", unGitOid (sourceHead launched), "HEAD"]
      case initialPaths of
        Left problem -> record "rebase-router" label
          (outcomeJson "register_failed" ["error" .= problem])
        Right paths -> R.modify' $ \state -> state
          { routerChildren = Map.insert label Child
              { childAgent = admittedAgent receipt
              , childWorktree = treeId launched
              , childBase = sourceHead launched
              , childPaths = Set.fromList (Text.lines paths)
              }
              (routerChildren state)
          }

startRouter :: () -> Handler RouterState RebaseRouterEffects ()
startRouter () = do
  bound <- boundWorktree
  case bound of
    Left failure -> record "rebase-router" "router"
      (object ["outcome" .= ("start_failed" :: Text), "error" .= Text.pack (show failure)])
    Right integrationTree -> do
      children <- R.gets routerChildren
      handles <- resolveChildren children
      subscription <- Event.subscribe (watchEvents integrationTree handles)
      R.modify' $ \state -> state
        { routerIntegration = Just integrationTree
        , routerSubscription = Just subscription
        , routerStopping = False
        }
      endpoints <- R.self @RebaseRouter
      R.send (tick endpoints) ()

-- Each tick is a short mailbox call. It drains what has arrived, then sleeps
-- once before enqueueing the next tick, leaving the actor available to accept
-- stop and registration calls between ticks.
tickRouter :: () -> Handler RouterState RebaseRouterEffects ()
tickRouter () = do
  state <- R.get
  case (routerSubscription state, routerIntegration state) of
    (Just subscription, Just integrationTree)
      | routerStopping state -> retire subscription
      | otherwise -> do
          children <- R.gets routerChildren
          handles <- resolveChildren children
          Event.drainSubscription (watchEvents integrationTree handles)
            (handleWatch integrationTree) subscription
          sleep (seconds 20)
          stopping <- R.gets routerStopping
          if stopping
            then retire subscription
            else do
              endpoints <- R.self @RebaseRouter
              R.send (tick endpoints) ()
    _ -> pure ()
  where
    retire subscription = do
      Event.unsubscribe subscription
      R.put (RouterState Map.empty Nothing Nothing False)

stopRouter :: () -> Handler RouterState RebaseRouterEffects ()
stopRouter () = R.modify' $ \state -> state { routerStopping = True }

resolveChildren
  :: Map Text Child
  -> Handler RouterState RebaseRouterEffects [(Text, Child, WorktreeHandle)]
resolveChildren children = fmap concat $ forM (Map.toList children) $ \(label, child) -> do
  found <- lookupWorktree (childWorktree child)
  pure $ case found of
    Left _ -> []
    Right handle -> [(label, child, handle)]

data RouterWatch
  = ChildCommitted Text (Event.Observed Event.CommitReceipt)
  | IntegrationAdvanced (Event.Observed Event.HeadChangeReceipt)

watchEvents
  :: WorktreeHandle
  -> [(Text, Child, WorktreeHandle)]
  -> Event.Event RouterWatch
watchEvents integration children =
  foldr (Event.<|>) (fmap IntegrationAdvanced (Event.headChanged integration))
    [fmap (ChildCommitted label) (Event.commit handle) | (label, _, handle) <- children]

handleWatch
  :: WorktreeHandle
  -> RouterWatch
  -> Handler RouterState RebaseRouterEffects ()
handleWatch integration watch = case watch of
  ChildCommitted label observed -> onChildCommit label observed
  IntegrationAdvanced observed -> onAdvance integration observed

onChildCommit
  :: Text
  -> Event.Observed Event.CommitReceipt
  -> Handler RouterState RebaseRouterEffects ()
onChildCommit label observed = do
  let receipt = Event.value observed
  current <- R.gets routerChildren
  case Map.lookup label current of
    Nothing -> pure ()
    Just child -> do
      integration <- R.gets routerIntegration
      case integration of
        Nothing -> pure ()
        Just handle -> do
          headOid <- worktreeHead handle
          case headOid of
            Left failure -> record "rebase-router" label
              (outcomeJson "facts_failed" ["error" .= Text.pack (show failure)])
            Right (GitOid integrationHead) -> do
              let parents = Event.parents receipt
              nextBase <- case parents of
                parent : _ -> do
                  parentIsIntegrated <- isAncestor (cwd (handleReceipt handle)) (unGitOid parent) integrationHead
                  pure $ if parentIsIntegrated then parent else childBase child
                [] -> pure (childBase child)
              let changed = Set.fromList (Event.files receipt)
              R.modify' $ \state -> state
                { routerChildren = Map.adjust
                    (\entry -> entry { childBase = nextBase, childPaths = childPaths entry `Set.union` changed })
                    label (routerChildren state)
                }
              record "rebase-router" label
                (object ["outcome" .= ("child_commit" :: Text), "commit" .= Event.oid receipt, "paths" .= Set.toList changed])

onAdvance
  :: WorktreeHandle
  -> Event.Observed Event.HeadChangeReceipt
  -> Handler RouterState RebaseRouterEffects ()
onAdvance integration observed = case Event.kind change of
  Event.Advanced _ -> do
    children <- R.gets routerChildren
    oldHead <- pure (maybe (unGitOid (Event.newHead change)) unGitOid (Event.oldHead change))
    newHead <- pure (unGitOid (Event.newHead change))
    pathResult <- gitText (cwd (handleReceipt integration)) ["diff", "--name-only", oldHead, newHead]
    case pathResult of
      Left problem -> forM_ (Map.toList children) $ \(label, _) ->
        record "rebase-router" label (outcomeJson "facts_failed" ["error" .= problem])
      Right changedPaths -> do
        results <- forM (Map.toList children) $ \(label, child) -> do
          behindResult <- isAncestorResult (cwd (handleReceipt integration)) (unGitOid (childBase child)) newHead
          pure $ case behindResult of
            Left problem -> Left (label, problem)
            Right isAncestorOfHead ->
              let behind = isAncestorOfHead && unGitOid (childBase child) /= newHead
               in Right (label, child,
                    exactFacts behind (Text.lines changedPaths) (childPaths child))
        forM_ [failure | Left failure <- results] $ \(label, problem) ->
          record "rebase-router" label (outcomeJson "facts_failed" ["error" .= problem])
        judgeAndRoute newHead [row | Right row <- results]
  _ -> pure ()
  where
    change = Event.value observed

judgeAndRoute
  :: Text -> [(Text, Child, RebaseFacts)]
  -> Handler RouterState RebaseRouterEffects ()
judgeAndRoute newHead rows = do
  let ambiguous = [row | row@(_, _, facts) <- rows,
        factsBehind facts && Set.null (factsOverlap facts)]
  judgments <- if null ambiguous
    then pure (Right Map.empty)
    else do
      result <- J.ask (J.rawState (object
          [ "new_head" .= newHead
          , "children" .=
              [ object [ "label" .= label, "behind" .= factsBehind facts
                       , "overlap" .= Set.toList (factsOverlap facts)
                       , "committed_paths" .= Set.toList (childPaths child)
                       ]
              | (label, child, facts) <- ambiguous
              ]
          ]))
        (#children := J.each (\(label, _, _) -> label) judgeOne ambiguous)
      pure $ case result of
        Left failure -> Left failure
        Right response -> Right (Map.fromList (map toJudgment response.children))
  forM_ rows $ \(label, child, facts) -> do
    let (likelihood, costAnswer) = case judgments of
          Left _ | factsBehind facts && Set.null (factsOverlap facts) -> (0.5, Nothing)
          _ -> Map.findWithDefault (0, Nothing) label (either (const Map.empty) id judgments)
        advice = adviceFor facts likelihood
    (outcome, notificationError) <- case advice of
      SendNudge -> do
        sent <- sendMessage (childAgent child)
          ("Integration advanced to " <> Text.take 12 newHead <> ". Rebase your worktree onto that head. "
            <> if Set.null (factsOverlap facts)
              then "Jev judged a conflict likely despite no exact path overlap."
              else "Overlapping paths: " <> Text.intercalate ", " (Set.toList (factsOverlap facts)))
        pure $ case sent of
          Left failure -> ("nudge_failed" :: Text, Just (Text.pack (show failure)))
          Right _ -> (adviceName advice, Nothing)
      DeferJudgment -> pure (adviceName advice, Nothing)
      RecordOnly -> pure (adviceName advice, Nothing)
    record "rebase-router" label
      (object
        [ "outcome" .= outcome
        , "new_head" .= newHead
        , "behind" .= factsBehind facts
        , "overlap" .= Set.toList (factsOverlap facts)
        , "conflict_likelihood" .= likelihood
        , "rebase_cost" .= costAnswer
        , "notification_error" .= notificationError
        ])
  where
    judgeOne (_, child, facts) =
      #likely_to_conflict := J.noul "Is rebasing this child onto the new integration head likely to conflict, given the exact ancestry and path facts?"
        :& #rebase_cost := J.score "How cheap would rebasing be at this point?"
          ( J.level #low "Expensive or disruptive to rebase now" ("expensive" :: Text)
              J..| J.level #moderate "Some work or coordination is needed" ("moderate" :: Text)
              J..| J.level #cheap "Straightforward to rebase now" ("cheap" :: Text)
          )
    toJudgment (row, judged) =
      ( rowLabel row
      , (judged.likely_to_conflict.yes, Just (toJSON judged.rebase_cost))
      )
    rowLabel (label, _, _) = label

isAncestor :: Text -> Text -> Text -> Handler RouterState RebaseRouterEffects Bool
isAncestor directory ancestor descendant = do
  result <- isAncestorResult directory ancestor descendant
  pure (either (const False) id result)

isAncestorResult :: Text -> Text -> Text -> Handler RouterState RebaseRouterEffects (Either Text Bool)
isAncestorResult directory ancestor descendant = do
  result <- Cmd.run (Cmd.inDirectory directory
    (Cmd.argv ["git", "merge-base", "--is-ancestor", ancestor, descendant]))
  pure $ case Cmd.commandOutcome (Cmd.commandResult result) of
    Cmd.CommandExited 0 -> Right True
    Cmd.CommandExited 1 -> Right False
    other -> Left (Text.pack (show other))

gitText :: Text -> [Text] -> Handler RouterState RebaseRouterEffects (Either Text Text)
gitText directory arguments = do
  result <- Cmd.run (Cmd.inDirectory directory (Cmd.argv ("git" : arguments)))
  pure $ case (Cmd.commandOutcome (Cmd.commandResult result), Cmd.stdout result) of
    (Cmd.CommandExited 0, Right output) -> Right (Text.strip output)
    (outcome, Left issue) -> Left (Text.pack (show (outcome, issue)))
    (outcome, _) -> Left (Text.pack (show outcome))

outcomeJson :: Text -> [ (Text, Value) ] -> Value
outcomeJson name fields = object ([("outcome", String name)] <> fields)

adviceName :: Advice -> Text
adviceName SendNudge = "nudged"
adviceName DeferJudgment = "deferred"
adviceName RecordOnly = "recorded_only"

unGitOid :: GitOid -> Text
unGitOid (GitOid value) = value
