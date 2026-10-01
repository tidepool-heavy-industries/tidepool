{-# LANGUAGE FlexibleContexts #-}
{-# LANGUAGE MonoLocalBinds #-}
{-# LANGUAGE OverloadedStrings #-}

-- | Tools for ordinary Haskell checks of resident recipes. Only @exomonad check
-- --recipes@ installs this effect; checked actors keep their normal capabilities.
module Tidepool.Check
  ( RecipeCheck, CheckActor, Activation (..)
  , root, turn, activation, git, writeFile, readFile
  , present, notPresented, unconfirmed, check, restart
  , assertThat, assertEventually, assertCell, awaitCell
  , output, lastOutput, literal, gitOidLiteral, checkpoint, awaitOutput
  ) where

import Prelude hiding (readFile, writeFile)
import Control.Monad.Freer (Eff, Member, send)
import Data.Text (Text)
import qualified Data.Text as Text
import Tidepool.Aeson (Value (..), eitherDecode)
import qualified Tidepool.Aeson.KeyMap as KeyMap
import Tidepool.Effects.Core (RecipeCheck (..))

-- The driver checks the swarm namespace as well as exact actor incarnation.
newtype CheckActor = CheckActor (Text, Int, Int) deriving (Show, Eq)

data Activation = Activation
  { checkActor :: CheckActor
  , checkLabel :: Text
  , checkContext :: Text
  , checkModel :: Maybe Text
  } deriving (Show)

root :: Member RecipeCheck effects => Eff effects CheckActor
root = CheckActor <$> send RecipeRoot

turn :: Member RecipeCheck effects => CheckActor -> Text -> Eff effects Value
turn (CheckActor actor) source = do
  encoded <- send (RecipeTurn actor source)
  case eitherDecode encoded of
    Right value@(Object fields) | KeyMap.lookup "status" fields `elem` [Just (String "committed"), Just (String "replied")] -> pure value
    _ -> do
      -- The recipe machine may be latched after a failed cell. Report through
      -- the host assertion effect so its retained receipt is not lost while
      -- trying to recover a Haskell exception from that machine.
      check ("Resident check turn failed: " <> encoded) False
      error "RecipeAssert returned after a failed assertion"

activation :: Member RecipeCheck effects => Eff effects Activation
activation = do
  (actor, (label, context, model)) <- send RecipeActivation
  pure (Activation (CheckActor actor) label context model)

git :: Member RecipeCheck effects => CheckActor -> [Text] -> Eff effects Text
git (CheckActor actor) = send . RecipeGit actor

writeFile :: Member RecipeCheck effects => CheckActor -> Text -> Text -> Eff effects ()
writeFile (CheckActor actor) path contents = send (RecipeWrite actor path contents)

readFile :: Member RecipeCheck effects => CheckActor -> Text -> Eff effects Text
readFile (CheckActor actor) = send . RecipeRead actor

-- These exercise the native presentation seam; presentation never proves incorporation.
present :: Member RecipeCheck effects => Eff effects Text
present = send RecipePresent

notPresented :: Member RecipeCheck effects => Text -> Eff effects Text
notPresented = send . RecipeNotPresented

unconfirmed :: Member RecipeCheck effects => Text -> Eff effects Text
unconfirmed = send . RecipeUnconfirmed

-- | Assert that an observed condition is true. The host reports the expected
-- value (`True`) and this observed value when the assertion fails.
check :: Member RecipeCheck effects => Text -> Bool -> Eff effects ()
check name observed = send (RecipeAssert name observed)

-- | Fail the checked cell before returning an effect action when the predicate
-- is false. Discarding the unit result cannot discard the assertion.
assertThat :: Text -> Bool -> Eff effects ()
assertThat name observed =
  if observed then pure ()
  else error (Text.unpack ("Check assertion failed: " <> name))

-- | Observe a typed predicate at most 120 times in the checked actor.
assertEventually :: Text -> Eff effects Bool -> Eff effects ()
assertEventually name observe = go (120 :: Int)
  where
    go remaining = do
      observed <- observe
      if observed then pure ()
      else if remaining == 1 then assertThat name False
      else go (remaining - 1)

-- | Evaluate a pure Bool expression in the checked actor's lexical scope.
-- The host records success only after the assertion cell completes normally.
assertCell :: Member RecipeCheck effects => CheckActor -> Text -> Text -> Eff effects ()
assertCell actor name source = do
  _ <- turn actor (assertionCell "assertThat" name source)
  check name True

-- | Await a typed effectful predicate in the checked actor's lexical scope.
awaitCell :: Member RecipeCheck effects => CheckActor -> Text -> Text -> Eff effects ()
awaitCell actor name source = do
  _ <- turn actor (assertionCell "assertEventually" name source)
  check name True

assertionCell :: Text -> Text -> Text -> Text
assertionCell assertion name source = Text.unlines
  [ "import qualified Tidepool.Check"
  , "Tidepool.Check." <> assertion <> " " <> literal name <> " ("
  , source
  , ")"
  ]

-- Intentionally closes this model-free swarm, then captures the edited package.
-- Earlier CheckActor values cannot address the new swarm even if IDs repeat.
restart :: Member RecipeCheck effects => Eff effects Text
restart = send RecipeRestart

output :: Value -> Text
output = Text.intercalate "\n" . outputs

-- Setup bindings in a multi-unit example also have output. Assertions about
-- its final expression should not depend on those workbench display receipts.
lastOutput :: Value -> Text
lastOutput value = case reverse (outputs value) of
  final : _ -> final
  [] -> ""

outputs :: Value -> [Text]
outputs (Object fields) = case KeyMap.lookup "items" fields of
  Just (Array items) -> [text | Object item <- items, Just (String text) <- [KeyMap.lookup "output" item]]
  _ -> []
outputs _ = []

literal :: Text -> Text
literal = Text.pack . show

-- | Render trusted `git rev-parse` output as a typed expression for a fixture cell.
gitOidLiteral :: Text -> Text
gitOidLiteral value = "(GitOid " <> literal value <> ")"

checkpoint :: Member RecipeCheck effects => CheckActor -> Text -> Text -> Text -> Eff effects Text
checkpoint actor path contents message = do
  writeFile actor path contents
  _ <- git actor ["add", "--", path]
  _ <- git actor ["-c", "commit.gpgsign=false", "commit", "--quiet", "-m", message]
  git actor ["rev-parse", "HEAD"]

-- Poll a retained observation while an automatic callback finishes. This does
-- not launch work or infer readiness from elapsed time.
awaitOutput :: Member RecipeCheck effects => CheckActor -> Text -> (Text -> Bool) -> Eff effects Text
awaitOutput actor source ready = go (120 :: Int)
  where
    go remaining = do
      observed <- output <$> turn actor source
      if ready observed then pure observed
      else if remaining == 0 then error (Text.unpack ("Check observation never became ready: " <> observed))
      else go (remaining - 1)
