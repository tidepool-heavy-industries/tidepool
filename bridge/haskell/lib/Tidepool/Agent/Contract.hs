{-# LANGUAGE OverloadedStrings #-}
{-# LANGUAGE FlexibleInstances #-}
{-# LANGUAGE FlexibleContexts #-}
{-# LANGUAGE MultiParamTypeClasses #-}
{-# LANGUAGE DefaultSignatures #-}
{-# LANGUAGE TypeOperators #-}
{-# LANGUAGE ScopedTypeVariables #-}
{-# LANGUAGE DataKinds #-}
{-# LANGUAGE UndecidableInstances #-}
{-# LANGUAGE TypeFamilies #-}
{-# LANGUAGE KindSignatures #-}
{-# LANGUAGE EmptyDataDecls #-}
{-# LANGUAGE ConstraintKinds #-}
{-# LANGUAGE DeriveGeneric #-}
{-# LANGUAGE TypeApplications #-}
{-# LANGUAGE PolyKinds #-}
{-# LANGUAGE DuplicateRecordFields #-}
-- | Mode-interpreted agent tool records compiled into declarations and
-- dynamic dispatch.
--
-- > data WorkerTools mode = WorkerTools
-- >   { askParent      :: mode :- Call Question Decision
-- >   , reportProgress :: mode :- Notify Progress
-- >   } deriving (Generic)
--
-- @AsServerT m@ interprets coupled headless tools; @AsActorT m state exit@
-- adds resident state transitions and typed actor completion. Both traverse
-- the same record entries, so declarations and dispatch cannot drift.
--
-- Tool input and output schemas use 'Tidepool.Aeson.Schema.JsonSchema' over
-- the same generic encodings dispatch decodes and replies encode. Endpoint
-- kind and both schemas come from the same field traversal as dispatch, so
-- declaration and execution cannot drift.
module Tidepool.Agent.Contract
  ( -- * Endpoint algebra and server interpretation
    Call
  , RawCall
  , Notify
  , Sync
  , HaskellCell
  , Update
  , Finish
  , AsServerT
  , AsActorT
  , (:-)

    -- * Tool values
  , Tool (..)
  , tool
  , RawTool
  , rawTool
  , Presented
  , PresentableTool
  , ToolOutput
  , presentWith
  , presentJson
  , presentDisplay
  , notify
  , SyncTool
  , SyncRawTool
  , syncTool
  , syncRawTool
  , syncNotify
  , HaskellTool
  , haskellTool
  , HaskellTools (..)
  , AsyncHaskellTools
  , haskellTools
  , asyncHaskellTools
  , defaultWorkbenchSpec
  , defaultAsyncWorkbenchSpec
  , SyncEffects
  , KnownToolEffects
  , AsyncEffects
  , Subset
  , ToolSchedule (..)
  , ToolImplementation (..)
  , updateTool
  , finishTool

    -- * Input schema (re-exported; the schema of the generic JSON encoding)
  , JsonSchema (..)

    -- * Generic compilation
  , HasAgentApi
  , HasActorApi
  , HasInstalledAgentApi
  , compileInstalledTools
  , compileTools
  , serveTools
  , serveToolsWith
  , serveToolsWithInitialUser
  , declarationsToJson
  , CompiledTools (..)
  , ToolDeclaration (..)
  , ToolKind (..)
  , ToolCompileError (..)
  , ToolDispatchError (..)
  , ToolDispatchSuccess (..)
  , renderToolDispatchError
  , toolDispatchReply
  , renderToolCompileError
  , ToolName
  , StructuralValue

    -- * The agent spec: one tools record, plus System 1 slots
  , AgentSpec (..)
  , NoTools (..)
  , defaultSpec
  , installSpec
  , toolCallEntry
  , afterToolEntry
  , ToolCall (..)
  , ToolResult (..)
  , ResultHandle
  , Annotation (..)
  , annotationToJson

    -- * Naming (exposed for the diagnostics fixtures)
  , toSnakeCase
  ) where

import Prelude
import Data.Text (Text)
import qualified Tidepool.Data.Text as T
import qualified Data.Map.Strict as Map
import Data.Kind (Type)
import Data.Proxy (Proxy (..))
import GHC.Generics
import GHC.TypeLits (TypeError, ErrorMessage (..))
import Tidepool.Inspection (Display (..))
import Tidepool.Aeson.Value (Value (..), ToJSON (..), encodeValue, object, (.=))
import Tidepool.Aeson.FromJSON (FromJSON (..), Result (..), fromJSON, withObject, (.:), (.:?), (.!=))
import Tidepool.Aeson.Schema (JsonSchema (..))
import Control.Monad.Freer (Eff, Member, raise, send)
import Tidepool.Effects.Core (AgentTools (..))
import Tidepool.Agent.ToolEffects

-- ---------------------------------------------------------------------------
-- Endpoint algebra and server interpretation
-- ---------------------------------------------------------------------------

-- | A request\/response endpoint.
data Call input output

-- | A native custom tool receiving literal text, with bounded text presentation.
data RawCall output

-- | A fire-and-forget endpoint. Hosted fields require presented unit-output
-- handlers; programmatic actors use semantic unit-output handlers.
data Notify input

-- | Hold the calling agent's next inference until this invocation settles.
data Sync endpoint

-- | A native notebook with an exact selected effect profile.
data HaskellCell (effects :: [Type -> Type])

-- | A request\/response endpoint that installs new resident policy state.
data Update input output

-- | A request\/response endpoint that completes its actor after replying.
data Finish input output

-- | The server-side interpretation of a tools record.
data AsServerT (m :: Type -> Type)

-- | Resident-actor interpretation of a tools record. State and exit belong to
-- the surrounding server, not to the external wire protocol.
data AsActorT (m :: Type -> Type) state exit

-- | Interpret one endpoint under a record mode. The closed fallthrough gives
-- an author-facing error at an unsupported field.
type family mode :- endpoint where
  AsServerT (Eff base) :- Sync (Call input output) = Presented (SyncTool base input output)
  AsServerT (Eff base) :- Sync (Notify input) = Presented (SyncTool base input ())
  AsServerT (Eff base) :- Sync (RawCall output) = Presented (SyncRawTool base output)
  AsServerT (Eff base) :- HaskellCell effects = HaskellTool 'Asynchronous effects base
  AsServerT (Eff base) :- Sync (HaskellCell effects) = HaskellTool 'BeforeNextInference effects base
  AsServerT m :- RawCall output = Presented (RawTool m output)
  AsActorT m state exit :- RawCall output = RawTool m output
  AsServerT m :- Call input output = Presented (Tool m input output)
  AsServerT m :- Notify input = Presented (Tool m input ())
  AsActorT m state exit :- Call input output = Tool m input output
  AsActorT m state exit :- Notify input = Tool m input ()
  AsActorT m state exit :- Update input output = UpdateTool m state input output
  AsActorT m state exit :- Finish input output = FinishTool m exit input output
  mode :- endpoint =
    TypeError
      ( 'Text "unsupported agent tool endpoint: `"
          ':<>: 'ShowType endpoint
          ':<>: 'Text "`."
          ':$$: 'Text "A tools-record field must use Call, RawCall, Notify, HaskellCell, Sync, Update, or Finish under a compatible server interpretation."
      )

infixr 0 :-

-- ---------------------------------------------------------------------------
-- Tool values
-- ---------------------------------------------------------------------------

-- | Documentation and handler are values, not type-level 'GHC.TypeLits.Symbol's
-- — so a description can be assembled with resident state (@fmt@) at agent
-- creation. A resident state transition rebuilds handler closures while Rust
-- requires the declared tool surface itself to remain stable.
data ToolKind
  = CallKind
  | RawKind
  | NotifyKind
  | UpdateKind
  | FinishKind
  deriving (Eq, Show)

data Tool m input output = Tool
  { toolKind :: ToolKind
  , description :: Text
  , handler :: input -> m output
  }

-- | Build a request\/response 'Tool'. An alias for 'Tool' — kept distinct
-- from 'notify' so authored code reads its intent at the call site.
tool :: Text -> (input -> m output) -> Tool m input output
tool description run = Tool CallKind description run

-- | Literal input is passed as data to an already compiled handler.
data RawTool m output = RawTool
  { rawDescription :: Text
  , rawHandler :: Text -> m output
  }

rawTool :: Text -> (Text -> m output) -> RawTool m output
rawTool description run = RawTool description run

-- | How a successful value of one named tool becomes the text a hosted model
-- sees. The semantic JSON value remains a separate result.
data Presented handler = Presented handler (ToolOutput handler -> Text)

class PresentableTool handler where
  type ToolOutput handler :: Type
  -- | Finish a semantic handler by selecting its model-facing text. Hosted
  -- fields require this value; programmatic actor fields retain the handler.
  presentWith :: (ToolOutput handler -> Text) -> handler -> Presented handler

instance PresentableTool (Tool m input output) where
  type ToolOutput (Tool m input output) = output
  presentWith presenter value = Presented value presenter

instance PresentableTool (RawTool m output) where
  type ToolOutput (RawTool m output) = output
  presentWith presenter value = Presented value presenter

instance PresentableTool (SyncTool base input output) where
  type ToolOutput (SyncTool base input output) = output
  presentWith presenter value = Presented value presenter

instance PresentableTool (SyncRawTool base output) where
  type ToolOutput (SyncRawTool base output) = output
  presentWith presenter value = Presented value presenter

-- | Use the JSON encoding as a tool's model-facing text.
presentJson :: ToJSON output => output -> Text
presentJson = encodeValue . toJSON

-- | Use the selected 'Display' instance as a tool's model-facing text.
presentDisplay :: Display output => output -> Text
presentDisplay = renderToolOutput

-- | A precompiled handler in the canonical synchronous row.
data SyncTool base input output = SyncTool ToolKind Text (input -> Eff (SyncEffects base) output)

data SyncRawTool base output = SyncRawTool Text (Text -> Eff (SyncEffects base) output)

syncTool :: Text -> (input -> Eff (SyncEffects base) output) -> SyncTool base input output
syncTool description run = SyncTool CallKind description run

syncNotify :: Text -> (input -> Eff (SyncEffects base) ()) -> SyncTool base input ()
syncNotify description run = SyncTool NotifyKind description run

syncRawTool :: Text -> (Text -> Eff (SyncEffects base) output) -> SyncRawTool base output
syncRawTool description run = SyncRawTool description run

-- The constructor retains a checked profile, not an executable handler.
data HaskellTool (schedule :: ToolSchedule) (effects :: [Type -> Type]) (base :: [Type -> Type]) =
  HaskellTool Text ToolSchedule [Text]

haskellTool
  :: forall schedule effects base.
     ( KnownToolSchedule schedule, KnownToolEffects effects
     , Subset effects (SupportedEffects schedule base), ValidToolProfile schedule effects
     )
  => Text -> HaskellTool schedule effects base
haskellTool description = HaskellTool description (toolSchedule (Proxy @schedule)) (toolEffectNames (Proxy @effects))

haskellCellDescription :: Text
haskellCellDescription =
  "Run raw Haskell in this actor's persistent notebook. Compose Kleisli arrows, optics and closures; define local types for agent RPC and control languages, and record actors to interpret them with state, events and Jev-selected continuations. Cells accept declarations, let bindings, effectful <- bindings and expressions. Values persist without automatic rendering; use display value for bounded output. The whole cell typechecks before execution; success publishes declarations and bindings together. Failure before publication installs no names; completed effects remain, so inspect receipts before retrying. Column-1 boundaries split units; keep respond value on one line with nothing after it. Use hosted lookup for signatures or doc workbench for composition and cell rules; doc queries are not Haskell."

asyncHaskellDescription :: Text
asyncHaskellDescription =
  "Schedule a cell asynchronously; the next inference need not wait for completion. " <> haskellCellDescription

syncHaskellDescription :: Text
syncHaskellDescription =
  "Wait for the cell to settle before this actor's next inference. Shares haskell's effects and scope; this endpoint alone grants no context-editing authority. " <> haskellCellDescription

-- | Notebook tools for a host without context-transaction support.
data AsyncHaskellTools effects mode = AsyncHaskellTools
  { haskell :: mode :- HaskellCell effects
  } deriving (Generic)

asyncHaskellTools
  :: forall effects. (KnownToolEffects effects, AsyncEffects effects)
  => AsyncHaskellTools effects (AsServerT (Eff effects))
asyncHaskellTools = AsyncHaskellTools
  { haskell = HaskellTool asyncHaskellDescription Asynchronous (toolEffectNames (Proxy @effects))
  }

-- | Both notebooks use the same effects and resident scope. The synchronous
-- endpoint holds the calling actor's next inference until the cell settles.
data HaskellTools effects mode = HaskellTools
  { haskell :: mode :- HaskellCell effects
  , haskellSync :: mode :- Sync (HaskellCell effects)
  } deriving (Generic)

haskellTools
  :: forall effects. (KnownToolEffects effects, AsyncEffects effects)
  => HaskellTools effects (AsServerT (Eff effects))
haskellTools = HaskellTools
  { haskell = HaskellTool asyncHaskellDescription Asynchronous (toolEffectNames (Proxy @effects))
  , haskellSync = HaskellTool syncHaskellDescription BeforeNextInference
      (toolEffectNames (Proxy @effects))
  }

-- | Build a fire-and-forget 'Tool' (@output ~ ()@).
notify :: Text -> (input -> m ()) -> Tool m input ()
notify description run = Tool NotifyKind description run

data UpdateTool m state input output = UpdateTool
  { updateDescription :: Text
  , updateHandler :: input -> m (output, state)
  }

-- | Build an endpoint that replies and replaces recursive server state.
updateTool :: Text -> (input -> m (output, state)) -> UpdateTool m state input output
updateTool = UpdateTool

data FinishTool m exit input output = FinishTool
  { finishDescription :: Text
  , finishHandler :: input -> m (output, exit)
  }

-- | Build an endpoint that replies and then returns a typed actor exit.
finishTool :: Text -> (input -> m (output, exit)) -> FinishTool m exit input output
finishTool = FinishTool

-- ---------------------------------------------------------------------------
-- compileTools — one field-ordered traversal, declaration + dispatch from
-- the same leaf visit
-- ---------------------------------------------------------------------------

-- | Wire-visible tool identity. Plain 'Text', matching
-- @exomonad_node::ToolDeclaration@'s @name@ on the Rust side.
type ToolName = Text

-- | The JSON value shuttled across dispatch.
type StructuralValue = Value

-- | One dynamic tool as declared to a backend at agent creation. Field order
-- and names line up with @exomonad_tool::ToolDeclaration@
-- including scheduling and its certified effect profile.
data ToolImplementation = ResidentHandler | NativeHaskellCell
  deriving (Eq, Show)

data ToolDeclaration = ToolDeclaration
  { dtdName :: Text
  , dtdDescription :: Text
  , dtdInputSchema :: Value
  , dtdOutputSchema :: Value
  , dtdKind :: ToolKind
  , dtdSchedule :: ToolSchedule
  , dtdImplementation :: ToolImplementation
  , dtdEffectKeys :: Maybe [Text]
  }
  deriving (Eq, Show)

data CompiledTools m = CompiledTools
  { declarations :: [ToolDeclaration]
  , dispatch :: ToolName -> StructuralValue -> m (Either ToolDispatchError ToolDispatchSuccess)
  , synopsis :: Text
  }

-- | Semantic tool output and its deliberately selected model-facing text.
-- Keep the two projections together through dispatch so neither consumer has
-- to recover meaning from rendered output.
data ToolDispatchSuccess = ToolDispatchSuccess
  { dispatchOutput :: StructuralValue
  , dispatchPresentation :: Text
  }
  deriving (Eq, Show)

-- | Rejected before the handler runs. A refusal is separate from the tool's
-- declared output schema.
data ToolDispatchError
  = UnknownTool ToolName
  | InvalidToolInput ToolName Text
  | NativeToolInvocation ToolName
  deriving (Eq, Show)

renderToolDispatchError :: ToolDispatchError -> Text
renderToolDispatchError problem = case problem of
  UnknownTool name -> "no such tool: " <> name
  InvalidToolInput name message -> "invalid input for tool " <> name <> ": " <> message
  NativeToolInvocation name -> "native Haskell tool requires the resident notebook executor: " <> name

-- | Authoring failures only detectable once selector NAMES (not just
-- selector TYPES) are in hand: two selectors normalizing to the same wire
-- name, or a normalized name that isn't a valid backend tool identifier.
-- Both require inspecting string values, not just types, so both are
-- runtime ('compileTools'-time) values rather than 'TypeError's — see the
-- receipt for the full split against the type-level diagnostics.
data ToolCompileError
  = DuplicateWireName
      { dupRecordName :: Text
      , dupSelectors :: [Text]
      , dupWireName :: Text
      }
  | InvalidToolIdentifier
      { invalidRecordName :: Text
      , invalidSelector :: Text
      , invalidWireName :: Text
      , invalidReason :: Text
      }
  deriving (Eq, Show)

-- | Human-readable rendering asserted on by the diagnostics fixtures: names
-- the record, the selector(s), the offending wire name, and the smallest
-- fix. Never mentions 'Rep', JSON-RPC, @codex-codes@, or backend schema
-- types.
renderToolCompileError :: ToolCompileError -> Text
renderToolCompileError (DuplicateWireName recName sels wire) =
  recName
    <> T.pack ": selectors "
    <> T.intercalate (T.pack ", ") sels
    <> T.pack " all normalize to the wire name \""
    <> wire
    <> T.pack "\". Rename one of the selectors so their snake_case forms differ."
renderToolCompileError (InvalidToolIdentifier recName sel wire reason) =
  recName
    <> T.pack "."
    <> sel
    <> T.pack ": normalized wire name \""
    <> wire
    <> T.pack "\" is not a valid tool identifier ("
    <> reason
    <> T.pack "). Rename the selector so its snake_case form is valid."

-- | camelCase -> snake_case, deterministic, ASCII-scoped (selectors are
-- Haskell identifiers). No @Named@ override in v1 (PRD: "one deterministic
-- camel-to-snake conversion").
toSnakeCase :: Text -> Text
toSnakeCase = T.pack . go . T.unpack
  where
    go [] = []
    go (c : cs) = lowerChar c : rest cs
    rest [] = []
    rest (c : cs)
      | isUpperChar c = '_' : lowerChar c : rest cs
      | otherwise = c : rest cs
    isUpperChar c = c >= 'A' && c <= 'Z'
    lowerChar c
      | isUpperChar c = toEnum (fromEnum c + 32)
      | otherwise = c

-- | An intermediate leaf produced by 'GCompileTools' — one per tools-record
-- selector. Both the declaration and the dispatch table entry in
-- 'compileTools' are plain projections of this SAME list; there is no
-- second traversal that could disagree with the first.
data ToolBody m result
  = HandlerBody (StructuralValue -> Either ToolDispatchError (m result))
  | HaskellBody [Text]

data ToolEntry m result = ToolEntry
  { entryRecordName :: Text
  , entrySelector :: Text
  , entryWireName :: Text
  , entryDescription :: Text
  , entryInputSchema :: Value
  , entryOutputSchema :: Value
  , entryKind :: ToolKind
  , entrySchedule :: ToolSchedule
  , entryBody :: ToolBody m result
  }

-- | The single Generic traversal: read the selector name, obtain the input
-- schema and the description/handler from the 'Tool' value found at that
-- leaf, and produce ONE entry carrying everything both the declaration and
-- the dispatcher need.
-- Installation certifies the source row independently of the dispatcher row.
-- A concrete sync-row Tool cannot masquerade as an ordinary async endpoint.
data InRow
data Installed (base :: [Type -> Type])

type family ToolSource policy (target :: Type -> Type) :: Type -> Type where
  ToolSource InRow target = target
  ToolSource (Installed base) target = Eff base

class LiftTool policy source target where
  liftTool :: Proxy policy -> source value -> target value

instance LiftTool InRow m m where liftTool _ = id
instance LiftTool (Installed base) (Eff base) (Eff (SyncEffects base)) where liftTool _ = raise

class GCompileTools policy (f :: Type -> Type) m result where
  gCompileEntries :: Proxy policy -> f a -> [ToolEntry m result]

instance (Datatype d, GCompileTools policy f m result) => GCompileTools policy (M1 D d f) m result where
  gCompileEntries policy (M1 x) = map setRecordName (gCompileEntries policy x)
    where
      setRecordName e = e {entryRecordName = recName}
      recName = T.pack (datatypeName (M1 Proxy :: M1 D d Proxy ()))

instance GCompileTools policy f m result => GCompileTools policy (M1 C c f) m result where
  gCompileEntries policy (M1 x) = gCompileEntries policy x

instance (GCompileTools policy a m result, GCompileTools policy b m result) => GCompileTools policy (a :*: b) m result where
  gCompileEntries policy (a :*: b) = gCompileEntries policy a ++ gCompileEntries policy b

instance GCompileTools policy U1 m result where
  gCompileEntries _ U1 = []

-- | An agent tools record itself must be a single-constructor product of
-- endpoints — the same restriction the endpoint schema places on tool
-- inputs/outputs, at the outer level.
instance
  TypeError
    ( 'Text "an agent tools record must be a single-constructor record of endpoints; "
        ':<>: 'Text "this type has multiple constructors."
    ) =>
  GCompileTools policy (a :+: b) m result
  where
  gCompileEntries _ _ = error "unreachable: multi-constructor tools record is a compile-time TypeError"

-- | Hosted handler leaves require selected presentation; unit-output tools use
-- the same instance as request/response tools.
instance
  (Selector s, FromJSON input, JsonSchema input, ToJSON output, JsonSchema output, Functor source, LiftTool policy source m) =>
  GCompileTools policy (M1 S s (K1 R (Presented (Tool source input output)))) m ToolDispatchSuccess
  where
  gCompileEntries policy (M1 (K1 (Presented (Tool kind desc h) present))) =
    [ ToolEntry
        { entryRecordName = T.empty
        , entrySelector = fieldName
        , entryWireName = fieldName
        , entryDescription = desc
        , entryInputSchema = jsonSchema (Proxy :: Proxy input)
        , entryOutputSchema = jsonSchema (Proxy :: Proxy output)
        , entryKind = kind
        , entrySchedule = Asynchronous
        , entryBody = HandlerBody $ \sv -> case fromJSON sv of
            Success input' -> Right (liftTool policy (presentResult <$> h input'))
            Error msg -> Left (InvalidToolInput (toSnakeCase fieldName) (T.pack msg))
        }
    ]
    where
      fieldName = T.pack (selName (M1 Proxy :: M1 S s Proxy ()))
      presentResult output = ToolDispatchSuccess (toJSON output) (present output)

instance
  (Selector s, ToJSON output, JsonSchema output, Functor source, LiftTool policy source m) =>
  GCompileTools policy (M1 S s (K1 R (Presented (RawTool source output)))) m ToolDispatchSuccess
  where
  gCompileEntries policy (M1 (K1 (Presented (RawTool desc h) present))) =
    [ ToolEntry
        { entryRecordName = T.empty
        , entrySelector = fieldName
        , entryWireName = fieldName
        , entryDescription = desc
        , entryInputSchema = jsonSchema (Proxy :: Proxy Text)
        , entryOutputSchema = jsonSchema (Proxy :: Proxy output)
        , entryKind = RawKind
        , entrySchedule = Asynchronous
        , entryBody = HandlerBody $ \value -> case fromJSON value of
            Success input -> Right (liftTool policy (presentResult <$> h input))
            Error message -> Left (InvalidToolInput (toSnakeCase fieldName) (T.pack message))
        }
    ]
    where
      fieldName = T.pack (selName (M1 Proxy :: M1 S s Proxy ()))
      presentResult output = ToolDispatchSuccess (toJSON output) (present output)

instance
  (Selector s, FromJSON input, JsonSchema input, ToJSON output, JsonSchema output) =>
  GCompileTools (Installed base) (M1 S s (K1 R (Presented (SyncTool base input output)))) (Eff (SyncEffects base)) ToolDispatchSuccess
  where
  gCompileEntries _ (M1 (K1 (Presented (SyncTool kind desc run) present))) =
    [ (actorEntry fieldName kind desc (jsonSchema (Proxy @output)) (fmap presentResult . run))
        { entrySchedule = BeforeNextInference }
    ]
    where
      fieldName = T.pack (selName (M1 Proxy :: M1 S s Proxy ()))
      presentResult output = ToolDispatchSuccess (toJSON output) (present output)

instance
  (Selector s, ToJSON output, JsonSchema output) =>
  GCompileTools (Installed base) (M1 S s (K1 R (Presented (SyncRawTool base output)))) (Eff (SyncEffects base)) ToolDispatchSuccess
  where
  gCompileEntries _ (M1 (K1 (Presented (SyncRawTool desc run) present))) =
    [ (actorEntry fieldName RawKind desc (jsonSchema (Proxy @output)) (fmap presentResult . run))
        { entrySchedule = BeforeNextInference }
    ]
    where
      fieldName = T.pack (selName (M1 Proxy :: M1 S s Proxy ()))
      presentResult output = ToolDispatchSuccess (toJSON output) (present output)

instance (Selector s, ToolSource policy m ~ Eff base) =>
  GCompileTools policy (M1 S s (K1 R (HaskellTool schedule effects base))) m result
  where
  gCompileEntries _ (M1 (K1 (HaskellTool desc schedule keys))) =
    [ ToolEntry
        { entryRecordName = T.empty
        , entrySelector = fieldName
        , entryWireName = fieldName
        , entryDescription = desc
        , entryInputSchema = jsonSchema (Proxy @Text)
        , entryOutputSchema = jsonSchema (Proxy @Text)
        , entryKind = RawKind
        , entrySchedule = schedule
        , entryBody = HaskellBody keys
        }
    ]
    where fieldName = T.pack (selName (M1 Proxy :: M1 S s Proxy ()))

instance
  (Selector s, Display output, Functor m) =>
  GCompileTools InRow (M1 S s (K1 R (RawTool m output))) m (ActorToolStep state exit)
  where
  gCompileEntries _ (M1 (K1 (RawTool desc run))) =
    [ actorEntry fieldName RawKind desc (jsonSchema (Proxy @Text)) $ \input ->
        ActorToolStay . toJSON . renderToolOutput <$> run input
    ]
    where fieldName = T.pack (selName (M1 Proxy :: M1 S s Proxy ()))

data ActorToolStep state exit
  = ActorToolStay StructuralValue
  | ActorToolUpdate StructuralValue state
  | ActorToolFinish StructuralValue exit

instance
  (Selector s, FromJSON input, JsonSchema input, ToJSON output, JsonSchema output, Functor m) =>
  GCompileTools InRow
    (M1 S s (K1 R (Tool m input output)))
    m
    (ActorToolStep state exit)
  where
  gCompileEntries _ (M1 (K1 (Tool kind desc h))) =
    [ actorEntry fieldName kind desc (jsonSchema (Proxy :: Proxy output)) $ \input ->
        ActorToolStay . toJSON <$> h input
    ]
    where
      fieldName = T.pack (selName (M1 Proxy :: M1 S s Proxy ()))

instance
  (Selector s, FromJSON input, JsonSchema input, ToJSON output, JsonSchema output, Functor m) =>
  GCompileTools InRow
    (M1 S s (K1 R (UpdateTool m state input output)))
    m
    (ActorToolStep state exit)
  where
  gCompileEntries _ (M1 (K1 (UpdateTool desc h))) =
    [ actorEntry fieldName UpdateKind desc (jsonSchema (Proxy :: Proxy output)) $ \input ->
        (\(output, state) -> ActorToolUpdate (toJSON output) state) <$> h input
    ]
    where
      fieldName = T.pack (selName (M1 Proxy :: M1 S s Proxy ()))

instance
  (Selector s, FromJSON input, JsonSchema input, ToJSON output, JsonSchema output, Functor m) =>
  GCompileTools InRow
    (M1 S s (K1 R (FinishTool m exit input output)))
    m
    (ActorToolStep state exit)
  where
  gCompileEntries _ (M1 (K1 (FinishTool desc h))) =
    [ actorEntry fieldName FinishKind desc (jsonSchema (Proxy :: Proxy output)) $ \input ->
        (\(output, exit) -> ActorToolFinish (toJSON output) exit) <$> h input
    ]
    where
      fieldName = T.pack (selName (M1 Proxy :: M1 S s Proxy ()))

-- | A field whose type is itself a server-interpreted tools record SPLICES
-- that record's tools in at its position. The outer selector contributes
-- nothing to any tool name — it is a place in the field order, not a
-- namespace — so
--
-- > data MyTools mode = MyTools
-- >   { shell         :: Shell.ShellTools mode
-- >   , triage_search :: mode :- Call Triage Text
-- >   } deriving (Generic)
--
-- declares @bash, write_stdin, read_output, cancel_command,
-- triage_search@, in that order. This is how an agent keeps the shell tools
-- while adding its own: a record that does not nest them does not have them.
--
-- 'OVERLAPPABLE' because the leaf instances above are the intended answer for
-- a @Presented@\/@Tool@\/@RawTool@\/@UpdateTool@\/@FinishTool@ field. In practice the two
-- kinds of field never both match — matching this head would require a leaf's
-- final type argument to BE @AsServerT m@ — but the pragma keeps a partly
-- resolved field type from being reported as an ambiguous overlap.
--
-- The entries are spliced into the ONE flat list 'compileEntrySet' checks, so
-- a nested tool that collides with an outer one is rejected by exactly the
-- 'DuplicateWireName' path that rejects two colliding outer selectors.
instance
  {-# OVERLAPPABLE #-}
  ( Generic (inner (AsServerT source))
  , GCompileTools policy (Rep (inner (AsServerT source))) m result
  ) =>
  GCompileTools policy (M1 S s (K1 R (inner (AsServerT source)))) m result
  where
  gCompileEntries policy (M1 (K1 nested)) = gCompileEntries policy (from nested)

actorEntry
  :: forall input m result
   . (FromJSON input, JsonSchema input)
  => Text
  -> ToolKind
  -> Text
  -> Value
  -> (input -> m result)
  -> ToolEntry m result
actorEntry fieldName kind desc outputSchema run =
  ToolEntry
    { entryRecordName = T.empty
    , entrySelector = fieldName
    , entryWireName = fieldName
    , entryDescription = desc
    , entryInputSchema = jsonSchema (Proxy :: Proxy input)
    , entryOutputSchema = outputSchema
    , entryKind = kind
    , entrySchedule = Asynchronous
    , entryBody = HandlerBody $ \sv -> case fromJSON sv of
        Success input' -> Right (run input')
        Error msg -> Left (InvalidToolInput (toSnakeCase fieldName) (T.pack msg))
    }

-- | The constraints needed to walk the server interpretation of a tools
-- record.
--
-- A CONSTRAINT-KIND SYNONYM, deliberately not a zero-method class: the class
-- encoding elaborates an empty dictionary whose culled @C:HasAgentApi@
-- constructor trips a real extract-pipeline bug ("Dangling NVar reference").
-- A synonym macro-expands at every use site and has no dictionary to cull.
-- Do not "tidy" this into a class.
type HasAgentApi tools m =
  ( Applicative m
  , Generic (tools (AsServerT m))
  , GCompileTools InRow (Rep (tools (AsServerT m))) m ToolDispatchSuccess
  )

-- The authored record stays in the base row; only its installed dispatcher
-- runs in the superset so async handlers can be raised and sync handlers reused.
type HasInstalledAgentApi tools effects =
  ( Generic (tools (AsServerT (Eff effects)))
  , GCompileTools (Installed effects) (Rep (tools (AsServerT (Eff effects)))) (Eff (SyncEffects effects)) ToolDispatchSuccess
  )

compileInstalledTools
  :: forall tools effects. HasInstalledAgentApi tools effects
  => tools (AsServerT (Eff effects))
  -> Either ToolCompileError (CompiledTools (Eff (SyncEffects effects)))
compileInstalledTools value = toCompiledTools <$> compileEntrySet
  (gCompileEntries (Proxy @(Installed effects)) (from value) :: [ToolEntry (Eff (SyncEffects effects)) ToolDispatchSuccess])

type HasActorApi tools m state exit =
  ( Applicative m
  , Generic (tools (AsActorT m state exit))
  , GCompileTools InRow
      (Rep (tools (AsActorT m state exit)))
      m
      (ActorToolStep state exit)
  )

-- | Compile a server-interpreted tools record into declarations and a
-- dispatcher. Both are projections of one 'GCompileTools' traversal.
compileTools ::
  forall tools m.
  HasAgentApi tools m =>
  tools (AsServerT m) ->
  Either ToolCompileError (CompiledTools m)
compileTools v =
  toCompiledTools
    <$> compileEntrySet
      (gCompileEntries (Proxy @InRow) (from v) :: [ToolEntry m ToolDispatchSuccess])

data CompiledEntrySet m result = CompiledEntrySet
  { entryDeclarations :: [ToolDeclaration]
  , entryDispatch :: ToolName -> StructuralValue -> m (Either ToolDispatchError result)
  , entrySynopsis :: Text
  }

compileEntrySet
  :: Applicative m
  => [ToolEntry m result]
  -> Either ToolCompileError (CompiledEntrySet m result)
compileEntrySet raw =
  let named = [e {entryWireName = toSnakeCase (entrySelector e)} | e <- raw]
   in case checkNames named of
        Left err -> Left err
        Right () ->
          let table = Map.fromList [(entryWireName e, entryBody e) | e <- named]
              dispatchFn n sv = case Map.lookup n table of
                Just (HandlerBody run) -> either (pure . Left) (fmap Right) (run sv)
                Just (HaskellBody _) -> pure (Left (NativeToolInvocation n))
                Nothing -> pure (Left (UnknownTool n))
           in Right
                CompiledEntrySet
                  { entryDeclarations = [ToolDeclaration
                      (entryWireName e) (entryDescription e) (entryInputSchema e) (entryOutputSchema e) (entryKind e)
                      (entrySchedule e) (case entryBody e of HandlerBody _ -> ResidentHandler; HaskellBody _ -> NativeHaskellCell)
                      (case entryBody e of HandlerBody _ -> Nothing; HaskellBody keys -> Just keys)
                    | e <- named]
                  , entryDispatch = dispatchFn
                  , entrySynopsis = T.intercalate (T.pack "\n") [entryWireName e <> T.pack ": " <> entryDescription e | e <- named]
                  }

toCompiledTools :: CompiledEntrySet m ToolDispatchSuccess -> CompiledTools m
toCompiledTools compiled =
  CompiledTools
    { declarations = entryDeclarations compiled
    , dispatch = entryDispatch compiled
    , synopsis = entrySynopsis compiled
    }

-- | The one external encoding of a compiled declaration set.
declarationsToJson :: [ToolDeclaration] -> Value
declarationsToJson decls =
  toJSON
    [ object $
        [ "name" .= dtdName d
        , "description" .= dtdDescription d
        , "inputSchema" .= dtdInputSchema d
        , "outputSchema" .= dtdOutputSchema d
        , "kind" .= toolKindText (dtdKind d)
        , "schedule" .= scheduleText (dtdSchedule d)
        , "implementation" .= implementationText (dtdImplementation d)
        ] ++ case dtdEffectKeys d of
          Nothing -> []
          Just keys -> ["effectKeys" .= keys]
    | d <- decls
    ]

scheduleText :: ToolSchedule -> Text
scheduleText Asynchronous = "async"
scheduleText BeforeNextInference = "before_next_inference"

implementationText :: ToolImplementation -> Text
implementationText ResidentHandler = "resident_handler"
implementationText NativeHaskellCell = "haskell_cell"

toolKindText :: ToolKind -> Text
toolKindText kind = case kind of
  RawKind -> "raw"
  CallKind -> "call"
  NotifyKind -> "notify"
  UpdateKind -> "update"
  FinishKind -> "finish"

-- Bound evaluation before crossing the bridge. The host additionally applies
-- its UTF-8 byte budget to the complete response, including effect output.
renderToolOutput :: Display a => a -> Text
renderToolOutput value =
  let (text, omitted) = displayWith 32500 value
  in if omitted then text <> "\n[Tool result shortened; request a smaller result.]" else text

-- ---------------------------------------------------------------------------
-- The agent spec — one tools record, plus the slots the runtime applies at
-- supported events
-- ---------------------------------------------------------------------------

-- | The handle under which a complete tool result stays addressable after a
-- slot has offered a selection of it. Plain 'Text': the runtime issues it, and
-- a slot only carries it back.
type ResultHandle = Text

-- | What the runtime asked a tool to do, as the slot sees it.
data ToolCall = ToolCall
  { toolCallName :: Text
  , toolCallArguments :: Value
  }

-- | What the tool answered, the runtime-issued one-based result ordinal, and
-- the handle the whole answer stays addressable under. The ordinal and the
-- decimal suffix of the handle identify the same result.
data ToolResult = ToolResult
  { toolResultName :: Text
  , toolResultHandle :: ResultHandle
  , toolResultOrdinal :: Int
  , toolResultValue :: Value
  , toolResultOutput :: Text
  }

instance FromJSON ToolCall where
  parseJSON = withObject "ToolCall" $ \o ->
    ToolCall <$> (o .: T.pack "name") <*> ((o .:? T.pack "arguments") .!= Null)

instance FromJSON ToolResult where
  parseJSON = withObject "ToolResult" $ \o ->
    ToolResult
      <$> (o .: T.pack "name")
      <*> ((o .:? T.pack "handle") .!= T.empty)
      <*> (o .: T.pack "ordinal")
      <*> ((o .:? T.pack "value") .!= Null)
      <*> ((o .:? T.pack "output") .!= T.empty)

-- | What the runtime shows the after-tool slot: one finished call, and what it
-- answered.
data AfterToolInput = AfterToolInput ToolCall ToolResult

instance FromJSON AfterToolInput where
  parseJSON = withObject "AfterToolInput" $ \o ->
    AfterToolInput <$> (o .: T.pack "call") <*> (o .: T.pack "result")

-- | What a slot has to say about a result it was shown. Annotate or prune,
-- never rewrite: a pruned view states that it is a selection and names the
-- handle the whole result is still addressable under.
data Annotation
  = -- | Nothing worth adding. The result is delivered exactly as the tool
    -- produced it and nothing is recorded beyond the invocation itself.
    NoAnnotation
  | -- | A deliberate non-decision, with its reason. Silent to the model:
    -- the reason belongs in the receipt, because the model never asked for a
    -- judgement on this result.
    Abstained Text
  | -- | Derived context attached beside the tool's own output, and attributed
    -- as derived so an observation is never read as a judgement.
    Annotated Text
  | -- | A selection of the result, and the handle the complete result stays
    -- addressable under.
    Pruned Text ResultHandle

-- | The one external encoding of an annotation, mirroring
-- 'declarationsToJson': the runtime reads this shape, nothing else.
annotationToJson :: Annotation -> Value
annotationToJson annotation = case annotation of
  NoAnnotation -> object ["kind" .= T.pack "none"]
  Abstained reason -> object ["kind" .= T.pack "abstained", "reason" .= reason]
  Annotated text -> object ["kind" .= T.pack "annotated", "text" .= text]
  Pruned text handle ->
    object ["kind" .= T.pack "pruned", "text" .= text, "handle" .= handle]

-- | A tools record with no fields. The type of 'defaultSpec'\'s tools, so the
-- default is a total value rather than a bottom waiting for a record update.
data NoTools mode = NoTools deriving (Generic)

-- | One agent's tools and its System 1 slots, in one value.
--
-- Written as a record update over 'defaultSpec', never as a bare constructor
-- application:
--
-- > agentSpec = defaultSpec
-- >   { specTools = Tools.definitions
-- >   , afterTool = Just AfterTool.run
-- >   }
--
-- That is the point of the default. A slot added later is a new field with a
-- default, so every spec already written keeps compiling untouched; a spec
-- spelled as a constructor application would break on every addition.
data AgentSpec tools effects = AgentSpec
  { -- | The tools record this spec installs. Each field names a tool, and its
    -- types generate the schemas.
    --
    -- Not spelled @tools@: every authored tools module imports this module
    -- unqualified and names its own record @tools@, and a field selector by
    -- that name would make each of those modules ambiguous.
    specTools :: tools (AsServerT (Eff effects))
  , -- | Applied when a tool call finishes, to that call and its result, in the
    -- actor's own resident machine.
    afterTool :: Maybe (ToolCall -> ToolResult -> Eff effects Annotation)
  }

-- | The spec every field of which is its default: no tools, no slots.
defaultSpec :: AgentSpec NoTools effects
defaultSpec = AgentSpec {specTools = NoTools, afterTool = Nothing}

-- | Built-in hosted policy when no workspace spec is supplied. Empty bounded
-- model specs still use 'defaultSpec'.
defaultWorkbenchSpec
  :: (KnownToolEffects effects, AsyncEffects effects)
  => AgentSpec (HaskellTools effects) effects
defaultWorkbenchSpec = defaultSpec { specTools = haskellTools }

-- | Hosted default when the runtime supplies only ordinary notebook effects.
defaultAsyncWorkbenchSpec
  :: (KnownToolEffects effects, AsyncEffects effects)
  => AgentSpec (AsyncHaskellTools effects) effects
defaultAsyncWorkbenchSpec = defaultSpec { specTools = asyncHaskellTools }

-- | The entry index the retained dispatcher serves an ordinary tool call at.
toolCallEntry :: Int
toolCallEntry = 0

-- | The entry index the retained dispatcher serves the after-tool slot at.
afterToolEntry :: Int
afterToolEntry = 1

-- | Install one spec: its declared tool surface and every slot it fills, from
-- a single compile of a single module.
--
-- Declarations and the retained dispatcher are two products of that one
-- compile, so a schema can never advertise a handler built from another
-- revision, and a slot can never be a revision ahead of the tools beside it.
-- The runtime selects what to run by entry index — 'toolCallEntry' for a tool
-- call, 'afterToolEntry' for the after-tool slot.
installSpec
  :: forall effects tools. (HasInstalledAgentApi tools effects, KnownToolEffects effects, AsyncEffects effects)
  => AgentSpec tools effects -> Eff (AgentTools ': SyncEffects effects) ()
installSpec spec = case compileInstalledTools (specTools spec) of
  Left problem -> error (T.unpack (renderToolCompileError problem))
  Right compiled -> send (AgentToolsInstallWith installation entry)
    where
      installation =
        object
          [ "tools" .= declarationsToJson (map resolvedProfile (declarations compiled))
          , "slots" .= toJSON slots
          , "slotEffectKeys" .= object [(slot, toJSON (toolEffectNames (Proxy @effects))) | slot <- slots]
          ]
      resolvedProfile declaration = case dtdEffectKeys declaration of
        Just _ -> declaration
        Nothing -> declaration { dtdEffectKeys = Just $ case dtdSchedule declaration of
          Asynchronous -> toolEffectNames (Proxy @effects)
          BeforeNextInference -> toolEffectNames (Proxy @(SyncEffects effects)) }
      slots = case afterTool spec of
        Nothing -> []
        Just _ -> [T.pack "afterTool"]

      entry :: Int -> Eff (AgentTools ': SyncEffects effects) Text
      entry index
        | index == afterToolEntry = runAfterTool (afterTool spec)
        | otherwise = runToolCall compiled

      runToolCall :: CompiledTools (Eff (SyncEffects effects)) -> Eff (AgentTools ': SyncEffects effects) Text
      runToolCall tooling = do
        (name, arguments) <- send AgentToolsInputWith
        result <- raise (dispatch tooling name arguments)
        pure (encodeValue (toolDispatchReply result))

      -- An unfilled slot is never selected by the runtime, which reads the
      -- installed slot list; asking for one anyway is the honest silence the
      -- slot itself would have produced.
      runAfterTool
        :: Maybe (ToolCall -> ToolResult -> Eff effects Annotation)
        -> Eff (AgentTools ': SyncEffects effects) Text
      runAfterTool Nothing = pure (renderAnnotation NoAnnotation)
      runAfterTool (Just slot) = do
        (_, payload) <- send AgentToolsInputWith
        case fromJSON payload of
          Error message ->
            pure (renderAnnotation (Abstained (T.pack ("after-tool slot input: " ++ message))))
          Success (AfterToolInput call result) ->
            renderAnnotation <$> raise (raise (slot call result))

-- The runtime boundary keeps typed refusal distinct from authored output.
toolDispatchReply :: Either ToolDispatchError ToolDispatchSuccess -> Value
toolDispatchReply result = case result of
  Right (ToolDispatchSuccess output presentation) -> object
    ["status" .= ("success" :: Text), "output" .= output, "presentation" .= presentation]
  Left problem -> refusalReply problem

toolValueDispatchReply :: Either ToolDispatchError Value -> Value
toolValueDispatchReply result = case result of
  Right output -> object ["status" .= ("success" :: Text), "output" .= output]
  Left problem -> refusalReply problem

refusalReply :: ToolDispatchError -> Value
refusalReply problem = object
    ([ "status" .= ("refused" :: Text)
     , "error" .= renderToolDispatchError problem
     ] ++ case problem of
       UnknownTool name ->
         ["kind" .= ("unknown_tool" :: Text), "tool" .= name]
       InvalidToolInput name message ->
         ["kind" .= ("invalid_input" :: Text), "tool" .= name, "detail" .= message]
       NativeToolInvocation name ->
         ["kind" .= ("native_tool" :: Text), "tool" .= name])

renderAnnotation :: Annotation -> Text
renderAnnotation = encodeValue . annotationToJson

-- | Install an immutable tools record as this actor's resident tool policy.
-- Rust resumes this loop only with names from the declarations published by
-- the same 'CompiledTools' value; decoding and handler execution stay in
-- Haskell under the actor's normal effect row.
serveTools ::
  forall tools effs exit.
  (HasActorApi tools (Eff effs) () exit, Member AgentTools effs) =>
  tools (AsActorT (Eff effs) () exit) ->
  Eff effs exit
serveTools tools = serveToolsWith () (const tools)

-- | Install a tools record and use the supplied text as the attached agent
-- session's first User message. Haskell owns the task value; Rust transports
-- it without reconstructing it. The message is offered only on the first
-- policy await, never repeated after a tool invocation.
serveToolsWithInitialUser ::
  forall tools effs exit.
  (HasActorApi tools (Eff effs) () exit, Member AgentTools effs) =>
  Text ->
  tools (AsActorT (Eff effs) () exit) ->
  Eff effs exit
serveToolsWithInitialUser initialUser tools =
  serveToolsLoop (Just initialUser) () (const tools)

-- | Serve one stable declaration surface with ordinary recursive Haskell
-- state. The builder may close plain handlers over the current state; only an
-- 'Update' endpoint can replace it, and only a 'Finish' endpoint can return.
serveToolsWith ::
  forall tools effs state exit.
  (HasActorApi tools (Eff effs) state exit, Member AgentTools effs) =>
  state ->
  (state -> tools (AsActorT (Eff effs) state exit)) ->
  Eff effs exit
serveToolsWith = serveToolsLoop Nothing

serveToolsLoop ::
  forall tools effs state exit.
  (HasActorApi tools (Eff effs) state exit, Member AgentTools effs) =>
  Maybe Text ->
  state ->
  (state -> tools (AsActorT (Eff effs) state exit)) ->
  Eff effs exit
serveToolsLoop initialUser initial build = loop initialUser initial
  where
    loop startupMessage state =
      case compileActorTools (build state) of
        Left err -> error (T.unpack (renderToolCompileError err))
        Right compiled -> do
          (name, arguments) <-
            send
              ( AgentToolsAwaitWith
                  (declarationsToJson (entryDeclarations compiled))
                  (entrySynopsis compiled)
                  startupMessage
              )
          step <- entryDispatch compiled name arguments
          case step of
            Left problem -> do
              send (AgentToolsReplyWith (toolValueDispatchReply (Left problem)))
              loop Nothing state
            Right (ActorToolStay result) -> do
              send (AgentToolsReplyWith (toolValueDispatchReply (Right result)))
              loop Nothing state
            Right (ActorToolUpdate result next) -> do
              send (AgentToolsReplyWith (toolValueDispatchReply (Right result)))
              loop Nothing next
            Right (ActorToolFinish result exit) -> do
              send (AgentToolsReplyWith (toolValueDispatchReply (Right result)))
              pure exit

compileActorTools ::
  forall tools m state exit.
  HasActorApi tools m state exit =>
  tools (AsActorT m state exit) ->
  Either ToolCompileError (CompiledEntrySet m (ActorToolStep state exit))
compileActorTools v =
  compileEntrySet
    (gCompileEntries (Proxy @InRow) (from v) :: [ToolEntry m (ActorToolStep state exit)])

checkNames :: [ToolEntry m result] -> Either ToolCompileError ()
checkNames named = do
  mapM_ checkIdentifier named
  checkDuplicates named

checkIdentifier :: ToolEntry m result -> Either ToolCompileError ()
checkIdentifier e = case validIdentifier (entryWireName e) of
  Right () -> Right ()
  Left reason -> Left (InvalidToolIdentifier (entryRecordName e) (entrySelector e) (entryWireName e) reason)

-- | Backend tool identifier rules: lowercase-letter start, only
-- @[a-z0-9_]@ after that, 64 chars max. A selector prefixed with @_@ (a
-- common Haskell record-field convention) is the realistic way to trip
-- this: its snake_case form still starts with @_@.
validIdentifier :: Text -> Either Text ()
validIdentifier n
  | T.null n = Left (T.pack "the normalized name is empty")
  | not (isLowerAZ (T.head n)) = Left (T.pack "must start with a lowercase letter a-z")
  | not (T.all isIdentChar n) = Left (T.pack "may contain only lowercase letters, digits, and underscores")
  | T.length n > 64 = Left (T.pack "must be 64 characters or fewer")
  | otherwise = Right ()
  where
    isLowerAZ c = c >= 'a' && c <= 'z'
    isIdentChar c = (c >= 'a' && c <= 'z') || (c >= '0' && c <= '9') || c == '_'

-- | A selector already spelled in snake_case (e.g. @ask_parent@) and a
-- camelCase sibling (@askParent@) normalize to the identical wire name —
-- the realistic, common way this collision happens (not a contrived
-- adversarial spelling).
checkDuplicates :: [ToolEntry m result] -> Either ToolCompileError ()
checkDuplicates named = case firstDup (map entryWireName named) of
  Nothing -> Right ()
  Just w ->
    let dupEntries = filter ((== w) . entryWireName) named
        recName = case dupEntries of
          (e : _) -> entryRecordName e
          [] -> T.pack ""
     in Left (DuplicateWireName recName (map entrySelector dupEntries) w)

firstDup :: [Text] -> Maybe Text
firstDup = go []
  where
    go _ [] = Nothing
    go seen (x : xs)
      | x `elem` seen = Just x
      | otherwise = go (x : seen) xs
