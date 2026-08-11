{-# LANGUAGE AllowAmbiguousTypes #-}
{-# LANGUAGE ConstraintKinds #-}
{-# LANGUAGE DeriveGeneric #-}
{-# LANGUAGE FlexibleContexts #-}
{-# LANGUAGE MultiParamTypeClasses #-}
{-# LANGUAGE OverloadedStrings #-}
{-# LANGUAGE ScopedTypeVariables #-}
{-# LANGUAGE TypeApplications #-}

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
-- > data WorkerResult = WorkerResult
-- >   { summary  :: Text
-- >   , blocked  :: Maybe Text
-- >   , caveats  :: [Text]
-- >   } deriving (Show, Eq, Generic, FromJSON, JsonSchema)
-- >
-- > result <- spawnAgent (spawnSpec wspec "porter" "port the handler")
-- > case result of
-- >   Right (outcome, r) -> ...
-- >   Left err           -> say (renderSpawnError err)
--
-- __The result type must be a single-constructor RECORD, not a sum.__
-- Established live against the pinned backend on 2026-08-11, not derived from
-- the docs: a sum renders @{\"oneOf\": [...]}@ at the schema root, and the
-- turn is refused whole at request validation —
-- @invalid_json_schema: In context=(), \'oneOf\' is not permitted@. So model
-- an alternative as a field (@blocked :: Maybe Text@ above), not as a
-- constructor.
--
-- This is a constraint on the RESULT type only. Tool INPUT types are
-- unaffected — they are declared through a different field
-- (@dynamicTools[].inputSchema@) which the same run confirmed accepts the
-- schemas "Tidepool.Aeson.Schema" emits, sums included. The failure is loud,
-- immediate, and costs no tokens (the request never reaches the model), so a
-- sum-typed result is a mistake you find in seconds rather than one that
-- corrupts a run.
--
-- @spawnAgentWithTools@ is the same call for a child that may CALL BACK: the
-- tools it declares are an authored "Tidepool.Agent.Contract" record, and each
-- call the child makes is answered by that record's own Haskell handler, in
-- the parent's @M@, while the child's turn is parked.
--
-- __Row gating.__ This module builds on the generated @agentBeginRaw@\/
-- @agentResumeRaw@, so it only compiles in a row containing @Subagent@ — and
-- @Subagent@'s own types reference @WorktreeSpec@\/@WorktreeHandle@\/
-- @WorktreeError@, so the row needs @Worktree@ too. Exactly "Tidepool.Form"'s
-- situation with @AskUser@: importing it from a row that lacks those effects
-- fails at the import, not at the call site.
--
-- __Scope.__ One call in, one typed outcome or one typed error out —
-- synchronous run-to-completion, one cycle, with the parent serving tool calls
-- inside it. PRD 18's async handle\/@waitAgent@\/poke surface is later work and
-- is deliberately not here.
module Tidepool.Agent.Spawn
  ( spawnAgent
  , spawnAgentWithTools
  , ToolRounds (..)
  , ToolAnswer (..)
  , NoTools (..)
  ) where

import Prelude
import Data.Proxy (Proxy (..))
import GHC.Generics (Generic)

import Tidepool.Effects
  ( AgentId
  , AgentStep (..)
  , CyclePayload (..)
  , M
  , SpawnError (..)
  , SpawnOutcome (..)
  , SpawnSpec
  , SpawnStage (..)
  , agentBeginRaw
  , agentResumeRaw
  )
import Tidepool.Agent.Contract
  ( AsServerT
  , CompiledTools (..)
  , DynamicToolDeclaration (..)
  , HasAgentApi
  , ToolName
  , compileTools
  , renderToolCompileError
  )
import Tidepool.Aeson.FromJSON (FromJSON, fromJSON, resultToEither)
import Tidepool.Aeson.Schema (JsonSchema (..))
import Tidepool.Aeson.Value (Value, object, toJSON, (.=))
import Data.Text (Text)
import qualified Tidepool.Data.Text as T

-- | How many tool-call rounds the parent will serve before it stops
-- dispatching.
--
-- POLICY, and the resident's: past the cap the loop answers a REFUSAL instead
-- of dispatching, so the child reads "that budget is gone" and finishes its
-- turn normally rather than being interrupted mid-thought. Counted in
-- DISPATCHES, not in stops: a refused call (over the cap, or naming a tool
-- that was never declared) costs nothing and does not consume budget.
--
-- Distinct from the runtime's @MAX_TOOL_ROUNDS@ backstop
-- (@tidepool-agent/src/spawn.rs@), which is a catastrophe bound — it exists
-- only so a missing or broken policy cap cannot spin against a live backend,
-- and it fails the whole spawn rather than answering politely.
newtype ToolRounds = ToolRounds Int
  deriving (Eq, Show)

-- | What a parent handler answered a parked tool call with.
--
-- AUTHORED-SIDE ONLY — never a wire type. It crosses to the runtime as
-- @agentResumeRaw@'s @Bool@ + @Value@ pair, which is what the
-- @serde_json::Value@ lane admits (inbound JSON cannot ride inside a bridged
-- record). An authored type does not have to be a wire type.
data ToolAnswer
  = -- | The handler ran and produced this JSON body.
    ToolAnswered Value
  | -- | The call was NOT dispatched, and this text says why. The child reads
    -- it and reacts to it — a refusal is an ordinary conversational fact, not
    -- a transport error, and never a way to leave a call unanswered.
    ToolRefused Text
  deriving (Show)

-- | The empty tools record: what 'spawnAgent' declares.
--
-- 'spawnAgent' IS 'spawnAgentWithTools' at zero tools, so the no-tools path is
-- the same driver loop rather than a second implementation that could drift
-- from it. Every call a child makes against this record names a tool that was
-- never declared, and is refused.
data NoTools mode = NoTools
  deriving (Generic)

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
spawnAgent = spawnAgentWithTools @NoTools @r (ToolRounds 0) NoTools

-- | One-cycle coupled spawn whose child holds DYNAMIC TOOLS, each one answered
-- by the parent's own Haskell handler.
--
-- > data WorkerTools mode = WorkerTools
-- >   { askParent      :: mode :- Call Question Decision
-- >   , reportProgress :: mode :- Notify Progress
-- >   } deriving (Generic)
-- >
-- > result <- spawnAgentWithTools @WorkerTools @WorkerResult
-- >             (ToolRounds 4) workerTools (spawnSpec wspec "porter" task)
--
-- The loop is: 'compileTools' ONCE (declarations and dispatch are two
-- projections of one traversal, so what the child is told about and what the
-- parent can answer cannot drift), declare them at thread start, then answer
-- each parked call and drive on until the turn completes.
--
-- __The parent is not suspended while a handler runs.__ Each
-- @agentBeginRaw@\/@agentResumeRaw@ is an ordinary synchronous effect call that
-- returns normally, and the handler runs between two of them as plain Haskell
-- in @M@ — so a handler may perform any parent effect. What is parked is the
-- CHILD's request, on the far side of the seam, where parking costs nothing
-- but an unwritten response. The consequence is worth stating: the child's
-- turn stays parked for as long as the handler runs, and if the parent eval
-- dies mid-dispatch the call is never answered. That is bounded by ownership,
-- not by a protocol trick — dropping the handler kills the backend process.
--
-- Three ways a call is answered without being dispatched, all of them
-- REFUSALS (the child reads the text and finishes its turn) rather than
-- silence or an abort:
--
-- * a tool name that is not in @dispatchNames@ — @dispatch@'s own fallthrough
--   @error@s, and an @error@ inside a dispatch would abort the whole eval with
--   the child's turn still parked, so the membership check is load-bearing;
-- * a call past the 'ToolRounds' cap, refused with text naming the cap;
-- * (before any of that) a tools record that does not compile — a
--   'ToolCompileError' is returned as
--   @'SpawnDriveFailed' 'StageAllocating'@ BEFORE @agentBeginRaw@ is called,
--   so nothing is allocated, nothing is bound, and no process is spawned.
spawnAgentWithTools ::
  forall tools r.
  (HasAgentApi tools M, FromJSON r, JsonSchema r) =>
  ToolRounds ->
  tools (AsServerT M) ->
  SpawnSpec ->
  M (Either SpawnError (SpawnOutcome, r))
spawnAgentWithTools rounds tools spec =
  case compileTools tools of
    -- BEFORE agentBeginRaw: an unusable tools record must not cost a worktree,
    -- a binding, or a backend process.
    Left err ->
      pure (Left (SpawnDriveFailed StageAllocating (renderToolCompileError err)))
    Right compiled -> do
      begun <-
        agentBeginRaw
          spec
          (declarationsToJson (declarations compiled))
          (jsonSchema (Proxy :: Proxy r))
      case begun of
        Left err -> pure (Left err)
        Right step -> driveToolLoop @r rounds compiled 0 step

-- | The tools array the runtime reads: @[{name, description, inputSchema}]@.
--
-- ALWAYS an array, @[]@ for zero tools — a non-array @tools@ argument is
-- REFUSED by the runtime (@SpawnDriveFailed StageAllocating@), never read as
-- "no tools".
declarationsToJson :: [DynamicToolDeclaration] -> Value
declarationsToJson decls =
  toJSON
    [ object
        [ "name" .= dtdName d
        , "description" .= dtdDescription d
        , "inputSchema" .= dtdInputSchema d
        ]
    | d <- decls
    ]

-- | Answer parked calls until the turn completes, then decode its payload.
--
-- @served@ is how many calls have actually been DISPATCHED so far; refusals do
-- not advance it.
driveToolLoop ::
  forall r.
  FromJSON r =>
  ToolRounds ->
  CompiledTools M ->
  Int ->
  AgentStep ->
  M (Either SpawnError (SpawnOutcome, r))
driveToolLoop rounds compiled served step = case step of
  StepDone outcome -> pure (decodeOutcome @r outcome)
  StepToolCall agent callId toolName args -> do
    (answer, served') <- answerCall rounds compiled served toolName args
    next <- resumeWith agent callId answer
    case next of
      Left err -> pure (Left err)
      Right step' -> driveToolLoop @r rounds compiled served' step'

-- | Decide what one parked call is answered with, and what that costs the
-- round budget.
--
-- Name membership is checked FIRST, and against 'dispatchNames' rather than by
-- calling 'dispatch' and hoping: an undeclared name reaching 'dispatch' hits
-- its "unknown tool" fallthrough, which @error@s — aborting the eval with the
-- child's turn still parked, which is the one outcome this loop exists to
-- prevent. Checking the name first also gives the child the more useful of the
-- two refusals when both would apply.
answerCall ::
  ToolRounds ->
  CompiledTools M ->
  Int ->
  ToolName ->
  Value ->
  M (ToolAnswer, Int)
answerCall (ToolRounds cap) compiled served toolName args
  | toolName `notElem` dispatchNames compiled =
      pure (ToolRefused ("no such tool: " <> toolName), served)
  | served >= cap =
      pure
        ( ToolRefused ("tool-call round cap reached (" <> T.pack (show cap) <> ")")
        , served
        )
  | otherwise = do
      body <- dispatch compiled toolName args
      pure (ToolAnswered body, served + 1)

-- | Project the authored 'ToolAnswer' onto the wire's @ok@ + @body@ pair and
-- drive the turn on. A refusal crosses as the plain text the child reads, not
-- as a transport failure.
resumeWith :: AgentId -> Text -> ToolAnswer -> M (Either SpawnError AgentStep)
resumeWith agent callId answer = case answer of
  ToolAnswered body -> agentResumeRaw agent callId True body
  ToolRefused reason -> agentResumeRaw agent callId False (toJSON reason)

-- | The terminal payload, decoded against the caller's result type.
--
-- 'SpawnResultMalformed' is produced HERE and only here — Rust has no way to
-- know what @r@ is and must never guess it.
decodeOutcome ::
  forall r.
  FromJSON r =>
  SpawnOutcome ->
  Either SpawnError (SpawnOutcome, r)
decodeOutcome outcome = case outcomePayload outcome of
  PayloadStructured v -> case resultToEither (fromJSON v) of
    Right value -> Right (outcome, value)
    Left detail -> Left (SpawnResultMalformed detail)
  PayloadUnstructured t ->
    Left (SpawnResultMalformed ("terminal message was not JSON: " <> t))
  PayloadAbsent ->
    Left (SpawnResultMalformed "no terminal message")
