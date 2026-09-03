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
  , Notify
  , Update
  , Finish
  , AsServerT
  , AsActorT
  , (:-)

    -- * Tool values
  , Tool (..)
  , tool
  , notify
  , updateTool
  , finishTool

    -- * Input schema (re-exported; the schema of the generic JSON encoding)
  , JsonSchema (..)

    -- * Generic compilation
  , HasAgentApi
  , HasActorApi
  , compileTools
  , serveTools
  , serveToolsWith
  , serveToolsWithInitialUser
  , declarationsToJson
  , CompiledTools (..)
  , ToolDeclaration (..)
  , ToolKind (..)
  , ToolCompileError (..)
  , renderToolCompileError
  , ToolName
  , StructuralValue

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
import Tidepool.Aeson.Value (Value, ToJSON (..), object, (.=))
import Tidepool.Aeson.FromJSON (FromJSON (..), Result (..), fromJSON)
import Tidepool.Aeson.Schema (JsonSchema (..))
import Control.Monad.Freer (Eff, Member, send)
import Tidepool.Effects.Core (AgentTools (..))

-- ---------------------------------------------------------------------------
-- Endpoint algebra and server interpretation
-- ---------------------------------------------------------------------------

-- | A request\/response endpoint.
data Call input output

-- | A fire-and-forget endpoint, interpreted as @Tool m input ()@ by the
-- server mode.
data Notify input

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
  AsServerT m :- Call input output = Tool m input output
  AsServerT m :- Notify input = Tool m input ()
  AsActorT m state exit :- Call input output = Tool m input output
  AsActorT m state exit :- Notify input = Tool m input ()
  AsActorT m state exit :- Update input output = UpdateTool m state input output
  AsActorT m state exit :- Finish input output = FinishTool m exit input output
  mode :- endpoint =
    TypeError
      ( 'Text "unsupported agent tool endpoint: `"
          ':<>: 'ShowType endpoint
          ':<>: 'Text "`."
          ':$$: 'Text "A tools-record field must use Call, Notify, Update, or Finish under a compatible server interpretation."
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
tool = Tool CallKind

-- | Build a fire-and-forget 'Tool' (@output ~ ()@).
notify :: Text -> (input -> m ()) -> Tool m input ()
notify = Tool NotifyKind

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
-- @tidepool_node::ToolDeclaration@'s @name@ on the Rust side.
type ToolName = Text

-- | The JSON value shuttled across dispatch.
type StructuralValue = Value

-- | One dynamic tool as declared to a backend at agent creation. Field order
-- and names line up with @tidepool_tool::ToolDeclaration@
-- (@{name, description, input_schema, output_schema, kind}@); this type does
-- not depend on that
-- crate, it just doesn't invent a gratuitously different shape.
data ToolDeclaration = ToolDeclaration
  { dtdName :: Text
  , dtdDescription :: Text
  , dtdInputSchema :: Value
  , dtdOutputSchema :: Value
  , dtdKind :: ToolKind
  }
  deriving (Eq, Show)

data CompiledTools m = CompiledTools
  { declarations :: [ToolDeclaration]
  , dispatch :: ToolName -> StructuralValue -> m StructuralValue
  , synopsis :: Text
  }

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
data ToolEntry m result = ToolEntry
  { entryRecordName :: Text
  , entrySelector :: Text
  , entryWireName :: Text
  , entryDescription :: Text
  , entryInputSchema :: Value
  , entryOutputSchema :: Value
  , entryKind :: ToolKind
  , entryRun :: StructuralValue -> m result
  }

-- | The single Generic traversal: read the selector name, obtain the input
-- schema and the description/handler from the 'Tool' value found at that
-- leaf, and produce ONE entry carrying everything both the declaration and
-- the dispatcher need.
class GCompileTools (f :: Type -> Type) m result where
  gCompileEntries :: f a -> [ToolEntry m result]

instance (Datatype d, GCompileTools f m result) => GCompileTools (M1 D d f) m result where
  gCompileEntries (M1 x) = map setRecordName (gCompileEntries x)
    where
      setRecordName e = e {entryRecordName = recName}
      recName = T.pack (datatypeName (M1 Proxy :: M1 D d Proxy ()))

instance GCompileTools f m result => GCompileTools (M1 C c f) m result where
  gCompileEntries (M1 x) = gCompileEntries x

instance (GCompileTools a m result, GCompileTools b m result) => GCompileTools (a :*: b) m result where
  gCompileEntries (a :*: b) = gCompileEntries a ++ gCompileEntries b

instance GCompileTools U1 m result where
  gCompileEntries U1 = []

-- | An agent tools record itself must be a single-constructor product of
-- endpoints — the same restriction the endpoint schema places on tool
-- inputs/outputs, at the outer level.
instance
  TypeError
    ( 'Text "an agent tools record must be a single-constructor record of endpoints; "
        ':<>: 'Text "this type has multiple constructors."
    ) =>
  GCompileTools (a :+: b) m result
  where
  gCompileEntries _ = error "unreachable: multi-constructor tools record is a compile-time TypeError"

-- | Every record leaf is exactly @Tool m input output@; unit-output tools use
-- the same instance as request/response tools.
instance
  (Selector s, FromJSON input, JsonSchema input, ToJSON output, JsonSchema output, Functor m) =>
  GCompileTools (M1 S s (K1 R (Tool m input output))) m StructuralValue
  where
  gCompileEntries (M1 (K1 (Tool kind desc h))) =
    [ ToolEntry
        { entryRecordName = T.empty
        , entrySelector = fieldName
        , entryWireName = fieldName
        , entryDescription = desc
        , entryInputSchema = jsonSchema (Proxy :: Proxy input)
        , entryOutputSchema = jsonSchema (Proxy :: Proxy output)
        , entryKind = kind
        , entryRun = \sv -> case fromJSON sv of
            Success input' -> toJSON <$> h input'
            Error msg -> error (T.unpack fieldName ++ ": compileTools dispatch could not decode tool input: " ++ msg)
        }
    ]
    where
      fieldName = T.pack (selName (M1 Proxy :: M1 S s Proxy ()))

data ActorToolStep state exit
  = ActorToolStay StructuralValue
  | ActorToolUpdate StructuralValue state
  | ActorToolFinish StructuralValue exit

instance
  (Selector s, FromJSON input, JsonSchema input, ToJSON output, JsonSchema output, Functor m) =>
  GCompileTools
    (M1 S s (K1 R (Tool m input output)))
    m
    (ActorToolStep state exit)
  where
  gCompileEntries (M1 (K1 (Tool kind desc h))) =
    [ actorEntry fieldName kind desc (jsonSchema (Proxy :: Proxy output)) $ \input ->
        ActorToolStay . toJSON <$> h input
    ]
    where
      fieldName = T.pack (selName (M1 Proxy :: M1 S s Proxy ()))

instance
  (Selector s, FromJSON input, JsonSchema input, ToJSON output, JsonSchema output, Functor m) =>
  GCompileTools
    (M1 S s (K1 R (UpdateTool m state input output)))
    m
    (ActorToolStep state exit)
  where
  gCompileEntries (M1 (K1 (UpdateTool desc h))) =
    [ actorEntry fieldName UpdateKind desc (jsonSchema (Proxy :: Proxy output)) $ \input ->
        (\(output, state) -> ActorToolUpdate (toJSON output) state) <$> h input
    ]
    where
      fieldName = T.pack (selName (M1 Proxy :: M1 S s Proxy ()))

instance
  (Selector s, FromJSON input, JsonSchema input, ToJSON output, JsonSchema output, Functor m) =>
  GCompileTools
    (M1 S s (K1 R (FinishTool m exit input output)))
    m
    (ActorToolStep state exit)
  where
  gCompileEntries (M1 (K1 (FinishTool desc h))) =
    [ actorEntry fieldName FinishKind desc (jsonSchema (Proxy :: Proxy output)) $ \input ->
        (\(output, exit) -> ActorToolFinish (toJSON output) exit) <$> h input
    ]
    where
      fieldName = T.pack (selName (M1 Proxy :: M1 S s Proxy ()))

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
    , entryRun = \sv -> case fromJSON sv of
        Success input' -> run input'
        Error msg -> error (T.unpack fieldName ++ ": tool dispatch could not decode input: " ++ msg)
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
  ( Generic (tools (AsServerT m))
  , GCompileTools (Rep (tools (AsServerT m))) m StructuralValue
  )

type HasActorApi tools m state exit =
  ( Generic (tools (AsActorT m state exit))
  , GCompileTools
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
      (gCompileEntries (from v) :: [ToolEntry m StructuralValue])

data CompiledEntrySet m result = CompiledEntrySet
  { entryDeclarations :: [ToolDeclaration]
  , entryDispatch :: ToolName -> StructuralValue -> m result
  , entrySynopsis :: Text
  }

compileEntrySet
  :: [ToolEntry m result]
  -> Either ToolCompileError (CompiledEntrySet m result)
compileEntrySet raw =
  let named = [e {entryWireName = toSnakeCase (entrySelector e)} | e <- raw]
   in case checkNames named of
        Left err -> Left err
        Right () ->
          let table = Map.fromList [(entryWireName e, entryRun e) | e <- named]
              dispatchFn n sv = case Map.lookup n table of
                Just run -> run sv
                Nothing -> error (T.unpack (T.pack "compileTools: dispatch called with unknown tool \"" <> n <> T.pack "\""))
           in Right
                CompiledEntrySet
                  { entryDeclarations = [ToolDeclaration (entryWireName e) (entryDescription e) (entryInputSchema e) (entryOutputSchema e) (entryKind e) | e <- named]
                  , entryDispatch = dispatchFn
                  , entrySynopsis = T.intercalate (T.pack "\n") [entryWireName e <> T.pack ": " <> entryDescription e | e <- named]
                  }

toCompiledTools :: CompiledEntrySet m StructuralValue -> CompiledTools m
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
    [ object
        [ "name" .= dtdName d
        , "description" .= dtdDescription d
        , "inputSchema" .= dtdInputSchema d
        , "outputSchema" .= dtdOutputSchema d
        , "kind" .= toolKindText (dtdKind d)
        ]
    | d <- decls
    ]

toolKindText :: ToolKind -> Text
toolKindText kind = case kind of
  CallKind -> "call"
  NotifyKind -> "notify"
  UpdateKind -> "update"
  FinishKind -> "finish"

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
            ActorToolStay result -> do
              send (AgentToolsReplyWith result)
              loop Nothing state
            ActorToolUpdate result next -> do
              send (AgentToolsReplyWith result)
              loop Nothing next
            ActorToolFinish result exit -> do
              send (AgentToolsReplyWith result)
              pure exit

compileActorTools ::
  forall tools m state exit.
  HasActorApi tools m state exit =>
  tools (AsActorT m state exit) ->
  Either ToolCompileError (CompiledEntrySet m (ActorToolStep state exit))
compileActorTools v =
  compileEntrySet
    (gCompileEntries (from v) :: [ToolEntry m (ActorToolStep state exit)])

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
