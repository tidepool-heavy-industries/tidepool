{-# LANGUAGE DataKinds #-}
{-# LANGUAGE OverloadedStrings #-}

module ForkReplyContracts where

import Control.Monad.Freer (Eff, send)
import qualified Tidepool.Effects.Core as Core

type Effects = '[Core.Actor, Core.Forks]

actorBegin :: Eff Effects ()
actorBegin = do
  (group, path, branches) <- send (Core.ActorBeginForkGroupWith False "group" ["branch"])
  if group == 7 && path == "group" && branches == ["branch"]
    then pure ()
    else error "wrong raw group reply"

actorCommit :: Eff Effects ()
actorCommit = send (Core.ActorCommitForkGroupWith 7)

actorAbort :: Eff Effects ()
actorAbort = send (Core.ActorAbortForkGroupWith 7)

forksBegin :: Eff Effects ()
forksBegin = do
  answer <- send (Core.ForksBeginWith False "group" ["branch"])
  case answer of
    Right (group, path, branches)
      | group == 7 && path == "group" && branches == ["branch"] -> pure ()
    _ -> error "wrong fallible group reply"

forksCommit :: Eff Effects ()
forksCommit = do
  answer <- send (Core.ForksCommitWith 7)
  case answer of
    Right () -> pure ()
    _ -> error "wrong fallible commit reply"

forksAbort :: Eff Effects ()
forksAbort = do
  answer <- send (Core.ForksAbortWith 7)
  case answer of
    Right () -> pure ()
    _ -> error "wrong fallible abort reply"

actorTwoBegins :: Eff Effects ()
actorTwoBegins = do
  _ <- send (Core.ActorBeginForkGroupWith False "held" ["branch"])
  _ <- send (Core.ActorBeginForkGroupWith False "next" ["branch"])
  pure ()

forksRefusal :: Eff Effects ()
forksRefusal = do
  answer <- send (Core.ForksBeginWith False "group" ["branch"])
  case answer of
    Left "admission refused" -> pure ()
    _ -> error "fallible refusal must remain data"
