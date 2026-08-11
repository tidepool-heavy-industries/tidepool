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
-- @AsServerT m@ interprets those endpoints as concrete 'Tool' values.
-- 'compileTools' walks that interpretation once and derives each declaration
-- and dispatch entry from the same selector and 'Tool' value. Other
-- interpretations can be added without changing the authored record.
--
-- A tool input's @input_schema@ is 'Tidepool.Aeson.Schema.JsonSchema' — the
-- schema of the SAME generic encoding 'Tidepool.Aeson.FromJSON.FromJSON'
-- decodes the dispatched argument with, re-exported here so an authored tools
-- record needs one import.
module Tidepool.Agent.Contract
  ( -- * Endpoint algebra and server interpretation
    Call
  , Notify
  , AsServerT
  , (:-)

    -- * Tool values
  , Tool (..)
  , tool
  , notify

    -- * Input schema (re-exported; the schema of the generic JSON encoding)
  , JsonSchema (..)

    -- * Generic compilation
  , HasAgentApi
  , compileTools
  , CompiledTools (..)
  , DynamicToolDeclaration (..)
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
import Tidepool.Aeson.Value (Value, ToJSON (..))
import Tidepool.Aeson.FromJSON (FromJSON (..), Result (..), fromJSON)
import Tidepool.Aeson.Schema (JsonSchema (..))

-- ---------------------------------------------------------------------------
-- Endpoint algebra and server interpretation
-- ---------------------------------------------------------------------------

-- | A request\/response endpoint.
data Call input output

-- | A fire-and-forget endpoint, interpreted as @Tool m input ()@ by the
-- server mode.
data Notify input

-- | The server-side interpretation of a tools record.
data AsServerT (m :: Type -> Type)

-- | Interpret one endpoint under a record mode. The closed fallthrough gives
-- an author-facing error at an unsupported field.
type family mode :- endpoint where
  AsServerT m :- Call input output = Tool m input output
  AsServerT m :- Notify input = Tool m input ()
  mode :- endpoint =
    TypeError
      ( 'Text "unsupported agent tool endpoint: `"
          ':<>: 'ShowType endpoint
          ':<>: 'Text "`."
          ':$$: 'Text "A tools-record field must have type `mode :- Call input output` or `mode :- Notify input`."
      )

infixr 0 :-

-- ---------------------------------------------------------------------------
-- Tool values
-- ---------------------------------------------------------------------------

-- | Documentation and handler are values, not type-level 'GHC.TypeLits.Symbol's
-- — so a description can be assembled with resident state (@fmt@) at agent
-- creation. Compiled once per agent thread (Codex dynamic tools are
-- thread-scoped, not turn-scoped).
data Tool m input output = Tool
  { description :: Text
  , handler :: input -> m output
  }

-- | Build a request\/response 'Tool'. An alias for 'Tool' — kept distinct
-- from 'notify' so authored code reads its intent at the call site.
tool :: Text -> (input -> m output) -> Tool m input output
tool = Tool

-- | Build a fire-and-forget 'Tool' (@output ~ ()@).
notify :: Text -> (input -> m ()) -> Tool m input ()
notify = Tool

-- ---------------------------------------------------------------------------
-- compileTools — one field-ordered traversal, declaration + dispatch from
-- the same leaf visit
-- ---------------------------------------------------------------------------

-- | Wire-visible tool identity. Plain 'Text', matching
-- @tidepool_agent::seam::DynamicToolDeclaration@'s @name@ on the Rust side.
type ToolName = Text

-- | The JSON value shuttled across dispatch.
type StructuralValue = Value

-- | One dynamic tool as declared to a backend at agent creation. Field order
-- and names line up with @tidepool_agent::seam::DynamicToolDeclaration@
-- (@{name, description, input_schema}@); this type does not depend on that
-- crate, it just doesn't invent a gratuitously different shape.
data DynamicToolDeclaration = DynamicToolDeclaration
  { dtdName :: Text
  , dtdDescription :: Text
  , dtdInputSchema :: Value
  }
  deriving (Eq, Show)

data CompiledTools m = CompiledTools
  { declarations :: [DynamicToolDeclaration]
  , dispatch :: ToolName -> StructuralValue -> m StructuralValue
  , synopsis :: Text
  , -- | Every wire name 'dispatch' will resolve without hitting its
    -- "unknown tool" fallthrough. NOT part of the PRD's minimum
    -- 'CompiledTools' shape ("at least" declarations/dispatch/synopsis) —
    -- added so the single-traversal invariant (declaration key set ==
    -- dispatch key set) is something a test can assert directly instead of
    -- only exercising indirectly.
    dispatchNames :: [ToolName]
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
data ToolEntry m = ToolEntry
  { entryRecordName :: Text
  , entrySelector :: Text
  , entryWireName :: Text
  , entryDescription :: Text
  , entryInputSchema :: Value
  , entryRun :: StructuralValue -> m StructuralValue
  }

-- | The single Generic traversal: read the selector name, obtain the input
-- schema and the description/handler from the 'Tool' value found at that
-- leaf, and produce ONE entry carrying everything both the declaration and
-- the dispatcher need.
class GCompileTools (f :: Type -> Type) m where
  gCompileEntries :: f a -> [ToolEntry m]

instance (Datatype d, GCompileTools f m) => GCompileTools (M1 D d f) m where
  gCompileEntries (M1 x) = map setRecordName (gCompileEntries x)
    where
      setRecordName e = e {entryRecordName = recName}
      recName = T.pack (datatypeName (M1 Proxy :: M1 D d Proxy ()))

instance GCompileTools f m => GCompileTools (M1 C c f) m where
  gCompileEntries (M1 x) = gCompileEntries x

instance (GCompileTools a m, GCompileTools b m) => GCompileTools (a :*: b) m where
  gCompileEntries (a :*: b) = gCompileEntries a ++ gCompileEntries b

instance GCompileTools U1 m where
  gCompileEntries U1 = []

-- | An agent tools record itself must be a single-constructor product of
-- endpoints — the same restriction the endpoint schema places on tool
-- inputs/outputs, at the outer level.
instance
  TypeError
    ( 'Text "an agent tools record must be a single-constructor record of endpoints; "
        ':<>: 'Text "this type has multiple constructors."
    ) =>
  GCompileTools (a :+: b) m
  where
  gCompileEntries _ = error "unreachable: multi-constructor tools record is a compile-time TypeError"

-- | Every record leaf is exactly @Tool m input output@; unit-output tools use
-- the same instance as request/response tools.
instance
  (Selector s, FromJSON input, JsonSchema input, ToJSON output, Functor m) =>
  GCompileTools (M1 S s (K1 R (Tool m input output))) m
  where
  gCompileEntries (M1 (K1 (Tool desc h))) =
    [ ToolEntry
        { entryRecordName = T.empty
        , entrySelector = fieldName
        , entryWireName = fieldName
        , entryDescription = desc
        , entryInputSchema = jsonSchema (Proxy :: Proxy input)
        , entryRun = \sv -> case fromJSON sv of
            Success input' -> toJSON <$> h input'
            Error msg -> error (T.unpack fieldName ++ ": compileTools dispatch could not decode tool input: " ++ msg)
        }
    ]
    where
      fieldName = T.pack (selName (M1 Proxy :: M1 S s Proxy ()))

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
  , GCompileTools (Rep (tools (AsServerT m))) m
  )

-- | Compile a server-interpreted tools record into declarations and a
-- dispatcher. Both are projections of one 'GCompileTools' traversal.
compileTools ::
  forall tools m.
  HasAgentApi tools m =>
  tools (AsServerT m) ->
  Either ToolCompileError (CompiledTools m)
compileTools v =
  let raw = gCompileEntries (from v) :: [ToolEntry m]
      named = [e {entryWireName = toSnakeCase (entrySelector e)} | e <- raw]
   in case checkNames named of
        Left err -> Left err
        Right () ->
          let table = Map.fromList [(entryWireName e, entryRun e) | e <- named]
              dispatchFn n sv = case Map.lookup n table of
                Just run -> run sv
                Nothing -> error (T.unpack (T.pack "compileTools: dispatch called with unknown tool \"" <> n <> T.pack "\""))
           in Right
                CompiledTools
                  { declarations = [DynamicToolDeclaration (entryWireName e) (entryDescription e) (entryInputSchema e) | e <- named]
                  , dispatch = dispatchFn
                  , synopsis = T.intercalate (T.pack "\n") [entryWireName e <> T.pack ": " <> entryDescription e | e <- named]
                  , dispatchNames = map entryWireName named
                  }

checkNames :: [ToolEntry m] -> Either ToolCompileError ()
checkNames named = do
  mapM_ checkIdentifier named
  checkDuplicates named

checkIdentifier :: ToolEntry m -> Either ToolCompileError ()
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
checkDuplicates :: [ToolEntry m] -> Either ToolCompileError ()
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
