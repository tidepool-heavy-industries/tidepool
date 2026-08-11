{-# LANGUAGE AllowAmbiguousTypes #-}
{-# LANGUAGE OverloadedStrings #-}
{-# LANGUAGE ScopedTypeVariables #-}

-- | Spawn a headless subagent and get a TYPED result back.
--
-- @spawnAgent \@WorkerResult spec@ derives the @outputSchema@ the worker's
-- final message must conform to from @WorkerResult@'s own structure, runs one
-- cycle, and decodes the terminal payload into a @WorkerResult@. The type IS
-- the schema: there is no second description of the result shape to drift from
-- the type the caller pattern-matches on — the same discipline
-- "Tidepool.Form"'s @askUser@ applies to operator input, applied to model
-- output.
--
-- > data WorkerResult = Completed { summary :: Text, caveats :: [Text] }
-- >                   | Blocked   { blocker :: Text, evidence :: [Text] }
-- >   deriving (Show, Eq, Generic, FromJSON, JsonSchema)
-- >
-- > result <- spawnAgent (spawnSpec wspec "porter" "port the handler")
-- > case result of
-- >   Right (outcome, Completed s _) -> ...
-- >   Right (_, Blocked b _)         -> ...
-- >   Left err                       -> say (renderSpawnError err)
--
-- __Row gating.__ This module builds on the generated @spawnAgentRaw@, so it
-- only compiles in a row containing @Subagent@ — and @Subagent@'s own types
-- reference @WorktreeSpec@\/@WorktreeHandle@\/@WorktreeError@, so the row needs
-- @Worktree@ too. Exactly "Tidepool.Form"'s situation with @AskUser@:
-- importing it from a row that lacks those effects fails at the import, not at
-- the call site.
--
-- __Lane 1 scope.__ One call in, one typed outcome or one typed error out —
-- synchronous run-to-completion, one cycle. PRD 18's async
-- handle\/@waitAgent@\/poke surface is lanes 2–5 and is deliberately not here.
module Tidepool.Agent.Spawn
  ( spawnAgent
  ) where

import Prelude
import Data.Proxy (Proxy (..))

import Tidepool.Effects
  ( CyclePayload (..)
  , M
  , SpawnError (..)
  , SpawnOutcome (..)
  , SpawnSpec
  , spawnAgentRaw
  )
import Tidepool.Aeson.FromJSON (FromJSON, fromJSON, resultToEither)
import Tidepool.Aeson.Schema (JsonSchema (..))

-- | One-cycle coupled spawn, typed by its result.
--
-- The schema handed to the backend describes exactly what
-- 'Tidepool.Aeson.FromJSON.FromJSON' reads back — 'Tidepool.Aeson.Schema' is
-- the schema OF that generic encoding, over the same @Generic@ metadata — so
-- a worker that satisfied the schema decodes by construction. There is no
-- codec on this boundary: the model writes ordinary JSON.
--
-- A payload that does not decode is a typed FAILURE — 'SpawnResultMalformed',
-- carrying the decoder's message (@\"key \\\"caveats\\\" not present\"@). It
-- is never a success with a defaulted field, and never an exception. The
-- three ways a result can fail to be an @r@ are
-- distinguished in the message, because they call for different fixes:
--
-- * @'PayloadStructured' v@ that misses the schema — the model produced JSON of
--   the wrong shape.
-- * @'PayloadUnstructured' t@ — the model's terminal message was not JSON at
--   all (note that @outputSchema@ constrains the final @agentMessage@ TEXT;
--   there is no separate structured-output field to fall back on).
-- * @'PayloadAbsent'@ — the cycle ended with no terminal message.
--
-- The 'SpawnOutcome' is returned ALONGSIDE the decoded value rather than
-- discarded: it carries the receipt (worktree id, binding ref, backend thread,
-- exact resolved model) that a caller needs in order to write a checkable
-- record of the run.
spawnAgent ::
  forall r.
  (FromJSON r, JsonSchema r) =>
  SpawnSpec ->
  M (Either SpawnError (SpawnOutcome, r))
spawnAgent spec = do
  raw <- spawnAgentRaw spec (jsonSchema (Proxy :: Proxy r))
  pure $ case raw of
    Left err -> Left err
    Right outcome -> case outcomePayload outcome of
      PayloadStructured v -> case resultToEither (fromJSON v) of
        Right value -> Right (outcome, value)
        Left detail -> Left (SpawnResultMalformed detail)
      PayloadUnstructured t ->
        Left (SpawnResultMalformed ("terminal message was not JSON: " <> t))
      PayloadAbsent ->
        Left (SpawnResultMalformed "no terminal message")
