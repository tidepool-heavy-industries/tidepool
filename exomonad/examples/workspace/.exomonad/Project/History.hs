{-# LANGUAGE DataKinds #-}
{-# LANGUAGE DeriveGeneric #-}
{-# LANGUAGE FlexibleContexts #-}
{-# LANGUAGE GADTs #-}
{-# LANGUAGE OverloadedLabels #-}
{-# LANGUAGE OverloadedRecordDot #-}
{-# LANGUAGE OverloadedStrings #-}
{-# LANGUAGE TypeFamilies #-}
{-# LANGUAGE TypeOperators #-}

-- | Read-only Git investigation, usable in cells, tools, and record actors.
module Project.History
  ( inspectHistory
  , Desk (..)
  , DeskEffects
  , desk
  ) where

import Control.Monad.Freer (Eff, Member)
import Control.Monad (forM)
import qualified Control.Monad.Freer.State as S
import Data.Text (Text)
import qualified Data.Text as T
import GHC.Generics (Generic)
import qualified Jev.Operators as J
import Jev.Operators (Packet ((:=), (:&)))
import qualified Tidepool.Actor as Actor
import qualified Tidepool.Actor.Record as R
import Tidepool.Actors.Exomonad
import qualified Tidepool.Command as Cmd
import Tidepool.Effects.Core (Commands, Jev)
import Tidepool.Effects.Row (knownEffects)

-- The selected commit is a next read, not a conclusion about the cause.
inspectHistory :: (Member Jev effects, Member Commands effects) => Text -> Eff effects Text
inspectHistory task = do
  history <- Cmd.quiet (Cmd.run (Cmd.argv
    ["git", "log", "-30", "--no-merges", "--format=%x1e%H%x09%s%n%b", "--stat"]))
  case Cmd.stdout history of
    Left issue -> pure ("Cannot read history: " <> T.pack (show issue))
    Right text -> do
      let commits = map (T.breakOn "\t")
            (filter (not . T.null) (map T.strip (T.splitOn "\x1e" text)))
      choice <- J.ask1
        (J.state (#task := task :& #evidence_scope := ("Recent non-merge commit messages and changed-file summaries; select what to inspect, not a proven cause." :: Text)))
        (J.choice "Which commit is the best next read for the task?"
          (J.alt #unresolved "None of these commit summaries identifies a useful next read" ()
            J..| J.many #commit fst (T.drop 1 . snd) commits))
      case choice of
        Left err -> pure ("Jev unavailable: " <> T.pack (show err))
        Right answer -> case J.settle J.lenient answer
          (#unresolved (\() -> pure "No useful candidate in these 30 commits.")
            J..| #commit (\_ (oid, _) -> do
              shown <- Cmd.quiet (Cmd.run (Cmd.argv ["git", "show", "--stat", "--format=short", oid]))
              pure (either (\issue -> "Cannot inspect commit: " <> T.pack (show issue)) id (Cmd.stdout shown)))) of
          Left doubt ->
            let candidates = take 3
                  [oid | (_, Just oid) <- J.contenders 0.10 answer
                    (#unresolved (\() -> Nothing)
                      J..| #commit (\_ (oid, _) -> Just oid))]
            in if null candidates
              then pure ("Needs a closer look: " <> doubt.why)
              else inspectPatches task candidates
          Right (J.Settled next) -> next

-- A near tie among summaries calls for stronger evidence, not a lower bar.
inspectPatches :: (Member Jev effects, Member Commands effects)
  => Text -> [Text] -> Eff effects Text
inspectPatches task candidates = do
  patches <- forM candidates $ \oid -> do
    result <- Cmd.quiet (Cmd.run (Cmd.argv ["git", "show", "--no-ext-diff", "--format=short", oid]))
    pure $ case Cmd.stdout result of
      Left issue -> Left (oid <> ": " <> T.pack (show issue))
      Right patch -> Right (oid, patchEvidence patch)
  case sequence patches of
    Left issue -> pure ("Cannot read candidate patch: " <> issue)
    Right evidence -> do
      choice <- J.ask1 (J.state (#task := task))
        (J.choice "Which patch most directly implements the change the task asks about? Distinguish the implementation from tests or documentation of it."
          (J.alt #unresolved "None of these patches establishes the requested change" ()
            J..| J.many #patch fst snd evidence))
      case choice of
        Left err -> pure ("Jev unavailable: " <> T.pack (show err))
        Right chosen -> case J.settle J.lenient chosen
          (#unresolved (\() -> pure "The candidate patches do not establish the requested change.")
            J..| #patch (\_ (oid, _) -> do
              result <- Cmd.quiet (Cmd.run (Cmd.argv ["git", "show", "--stat", "--format=short", oid]))
              pure (either (\issue -> "Cannot inspect commit: " <> T.pack (show issue)) id (Cmd.stdout result)))) of
          Left doubt -> pure ("Patch evidence remains ambiguous: " <> doubt.why)
          Right (J.Settled action) -> do
            result <- action
            pure ("Compared " <> T.pack (show (length candidates)) <> " candidate patches.\n\n" <> result)

-- The SHA retains an exact address for a fuller follow-up. A clipped patch is
-- explicitly partial evidence; it must not masquerade as the entire change.
patchEvidence :: Text -> Text
patchEvidence patch
  | T.length patch <= 4000 = patch
  | otherwise = "PARTIAL PATCH: first 4000 characters; omitted content is not evidence of absence.\n"
      <> T.take 4000 patch

data Desk mode = Desk
  { deskState :: mode :- State [(Text, Text)]
  , investigate :: mode :- Call Text (R.Reply Text)
  , findings :: mode :- Call () (R.Reply [(Text, Text)])
  } deriving Generic

type DeskEffects = LocalEffects Desk '[Replies, Actor, Notifications, Jev, Commands]

desk :: ActorSpec Desk DeskEffects
desk = R.definition "history-desk" (Actor.Selected knownEffects) Desk
  { deskState = []
  , investigate = \task -> do
      result <- inspectHistory task
      S.modify (++ [(task, result)])
      pure result
  , findings = \() -> S.get
  }
