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
-- | PRD 18 gate 1(a): the Servant-style agent tool contract algebra.
--
-- An agent's callable tools are a 'Generic' record parameterized by @mode@,
-- one field per endpoint:
--
-- > data WorkerTools mode = WorkerTools
-- >   { askParent      :: mode :- Call Question Decision
-- >   , reportProgress :: mode :- Notify Progress
-- >   } deriving (Generic)
--
-- Interpreting @mode@ as @AsServerT m@ turns every field into a real
-- 'Tool': @askParent :: Tool m Question Decision@. 'compileTools' walks that
-- interpreted record ONCE (field order), and from each leaf ('Selector' name
-- + the 'Tool' value found there) emits both a wire declaration and a
-- dispatch entry — the declaration and the dispatcher cannot drift apart
-- because there is only one traversal, and the description/handler cannot
-- drift apart because they live in the same 'Tool' value.
--
-- @mode :- endpoint@ is a CLOSED type family (not open instances, per the
-- PRD's explicit "class instead of open instances if it elaborates smaller
-- or errors better" escape hatch): the endpoint vocabulary is fixed
-- ('Call'\/'Notify'), so there is nothing an open family's extensibility
-- would buy here, and a closed family gets a source-level 'TypeError'
-- fallthrough equation for free instead of GHC's generic "no instance"
-- dump. See @plans\/post-restart\/agent-lanes\/receipt-mode-encoding.md@ for
-- the elaboration evidence that decided GO on this encoding.
--
-- The structural interpreter in this module ('AgentSchema') is
-- DELIBERATELY shallow: single-constructor records and primitives only, no
-- lists, no recursion. That is dev-structural-codec's gate (1(b)); this
-- gate only needs enough structure to prove the mode encoding dispatches on
-- the real JIT. A multi-constructor endpoint type is rejected the same way
-- 'Tidepool.Aeson.Value.ToJSON' rejects multi-constructor sums: a
-- source-level 'TypeError', not a runtime crash.
module Tidepool.Agent.Contract
  ( -- * Endpoint markers
    Call
  , Notify
  , AsServerT
  , (:-)

    -- * Tool values
  , Tool (..)
  , tool
  , notify

    -- * Structural schema (shallow: records + primitives only)
  , AgentSchema (..)
  , GAgentSchema (..)

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
import Tidepool.Aeson.Value (Value (..), object, ToJSON (..))
import Tidepool.Aeson.FromJSON (FromJSON (..), Result (..), fromJSON)

-- ---------------------------------------------------------------------------
-- Endpoint markers and the mode encoding
-- ---------------------------------------------------------------------------

-- | A request\/response endpoint: the child calls in with @input@, the
-- handler runs in the parent's effect monad, the child gets @output@ back.
data Call input output

-- | A fire-and-forget endpoint: equivalent to @Call input ()@, but names
-- the intent distinctly (and may render differently in synopses/traces).
data Notify input

-- | The server-side interpretation of a tools record: @mode :- endpoint@
-- becomes the concrete 'Tool' value a resident author supplies a handler
-- for.
data AsServerT (m :: Type -> Type)

-- | Interpret one tools-record field's endpoint under @mode@.
--
-- CLOSED, not open: see the module header for why. The third equation is
-- the authoring diagnostic for "record field is not a supported endpoint" —
-- it fires at the exact source span of the offending field, because that is
-- where GHC must reduce this family application to build the record's
-- 'Generic' representation.
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

-- | Documentation and handler are values, not type-level 'GHC.TypeLits.Symbol's
-- — so a description can be assembled with resident state (@fmt@) at agent
-- creation. Compiled once per agent thread (Codex dynamic tools are
-- thread-scoped, not turn-scoped).
data Tool m input output = Tool
  { description :: Text
  , handler :: input -> m output
  }

-- | Build a request\/response 'Tool'. An alias for 'Tool' — kept distinct
-- from 'notify' so authored code reads its intent at the call site, mirroring
-- 'Call'\/'Notify'.
tool :: Text -> (input -> m output) -> Tool m input output
tool = Tool

-- | Build a fire-and-forget 'Tool' (@output ~ ()@).
notify :: Text -> (input -> m ()) -> Tool m input ()
notify = Tool

-- ---------------------------------------------------------------------------
-- Structural schema — shallow: single-constructor records + primitives
-- ---------------------------------------------------------------------------

-- | A JSON-Schema-shaped structural description of a 'Call' input type, used
-- for 'DynamicToolDeclaration''s @input_schema@. Reuses 'GHC.Generics' the
-- same way 'Tidepool.Aeson.Value.ToJSON' does: a default method resolves via
-- @deriving (Generic, AgentSchema)@ (needs @DeriveAnyClass@ at the use site).
class AgentSchema a where
  agentSchema :: Proxy a -> Value
  default agentSchema :: (Generic a, GAgentSchema (Rep a)) => Proxy a -> Value
  agentSchema _ = gAgentSchema (Proxy :: Proxy (Rep a))

instance AgentSchema Int where
  agentSchema _ = object [(T.pack "type", String (T.pack "integer"))]

instance AgentSchema Text where
  agentSchema _ = object [(T.pack "type", String (T.pack "string"))]

instance AgentSchema Bool where
  agentSchema _ = object [(T.pack "type", String (T.pack "boolean"))]

instance AgentSchema Double where
  agentSchema _ = object [(T.pack "type", String (T.pack "number"))]

instance AgentSchema () where
  agentSchema _ = object [(T.pack "type", String (T.pack "null"))]

-- | Shallow: optionality (@required@) is not modeled from 'Maybe' in v1 — a
-- 'Maybe' field's schema is just its payload's schema.
instance AgentSchema a => AgentSchema (Maybe a) where
  agentSchema _ = agentSchema (Proxy :: Proxy a)

-- | Structural walk over a 'GHC.Generics' representation, mirroring
-- 'Tidepool.Aeson.Value.GToJSON''s shape (transparent @M1 D@, object-from-record
-- @M1 C@, 'TypeError' on any sum).
class GAgentSchema (f :: Type -> Type) where
  gAgentSchema :: Proxy f -> Value

instance GAgentSchema f => GAgentSchema (M1 D d f) where
  gAgentSchema _ = gAgentSchema (Proxy :: Proxy f)

instance GAgentSchemaObj f => GAgentSchema (M1 C c f) where
  gAgentSchema _ =
    object
      [ (T.pack "type", String (T.pack "object"))
      , (T.pack "properties", Object (Map.fromList fields))
      , (T.pack "required", Array (map (String . fst) fields))
      ]
    where
      fields = gAgentSchemaFields (Proxy :: Proxy f)

-- | This gate's schema is single-constructor records only — the same
-- restriction 'Tidepool.Aeson.Value.ToJSON' already carries. Lists and
-- recursive shapes are dev-structural-codec's gate 1(b), not this one's.
instance
  TypeError
    ( 'Text "Tidepool.Agent.Contract's structural schema supports single-constructor records only; "
        ':<>: 'Text "this endpoint type has multiple constructors."
        ':$$: 'Text "Write the endpoint as a record, or reach for the list/recursive structural codec (gate 1(b))."
    ) =>
  GAgentSchema (a :+: b)
  where
  gAgentSchema _ = error "unreachable: multi-constructor schema is a compile-time TypeError"

class GAgentSchemaObj (f :: Type -> Type) where
  gAgentSchemaFields :: Proxy f -> [(Text, Value)]

instance (GAgentSchemaObj a, GAgentSchemaObj b) => GAgentSchemaObj (a :*: b) where
  gAgentSchemaFields _ = gAgentSchemaFields (Proxy :: Proxy a) ++ gAgentSchemaFields (Proxy :: Proxy b)

instance GAgentSchemaObj U1 where
  gAgentSchemaFields _ = []

instance (Selector s, AgentSchema c) => GAgentSchemaObj (M1 S s (K1 R c)) where
  gAgentSchemaFields _ = [(fieldName, agentSchema (Proxy :: Proxy c))]
    where
      fieldName = T.pack (selName (M1 Proxy :: M1 S s Proxy ()))

-- ---------------------------------------------------------------------------
-- compileTools — one field-ordered traversal, declaration + dispatch from
-- the same leaf visit
-- ---------------------------------------------------------------------------

-- | Wire-visible tool identity. Plain 'Text', matching
-- @tidepool_agent::seam::DynamicToolDeclaration@'s @name@ on the Rust side.
type ToolName = Text

-- | The wire value shuttled across dispatch. Reuses the already-proven
-- 'Value' codec rather than inventing a parallel one — see the module
-- header on why this gate's schema stays shallow.
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
-- endpoints — the same restriction the endpoint schema places on 'Call'
-- inputs/outputs, at the outer level.
instance
  TypeError
    ( 'Text "an agent tools record must be a single-constructor record of endpoints; "
        ':<>: 'Text "this type has multiple constructors."
    ) =>
  GCompileTools (a :+: b) m
  where
  gCompileEntries _ = error "unreachable: multi-constructor tools record is a compile-time TypeError"

-- | The leaf: a field interpreted through @AsServerT m@ is always exactly
-- @Tool m input output@ (both 'Call' and 'Notify' reduce to this shape —
-- 'Notify' just fixes @output ~ ()@), so one instance covers both endpoint
-- kinds.
instance
  (Selector s, FromJSON input, AgentSchema input, ToJSON output, Functor m) =>
  GCompileTools (M1 S s (K1 R (Tool m input output))) m
  where
  gCompileEntries (M1 (K1 (Tool desc h))) =
    [ ToolEntry
        { entryRecordName = T.empty
        , entrySelector = fieldName
        , entryWireName = fieldName
        , entryDescription = desc
        , entryInputSchema = agentSchema (Proxy :: Proxy input)
        , entryRun = \sv -> case fromJSON sv of
            Success input' -> toJSON <$> h input'
            Error msg -> error (T.unpack fieldName ++ ": compileTools dispatch could not decode tool input: " ++ msg)
        }
    ]
    where
      fieldName = T.pack (selName (M1 Proxy :: M1 S s Proxy ()))

-- | Everything 'compileTools' needs: a 'Generic' interpreted tools record
-- whose leaves 'GCompileTools' can walk. Two type parameters (not the PRD
-- sketch's one) because the constraint is over BOTH the record shape and
-- the effect monad @m@ it is interpreted at — "the exact class/row spelling
-- follows the existing Tidepool effect machinery" (PRD).
--
-- A CONSTRAINT-KIND SYNONYM, not a class: an earlier version was a
-- zero-method class with only a superclass context (@class (Generic ...,
-- GCompileTools ...) => HasAgentApi tools m@) plus its matching instance.
-- That elaborated to a dictionary with nothing in it — and tripped a real
-- extract-pipeline bug (a "Dangling NVar reference" for the culled
-- @C:HasAgentApi@ dictionary constructor; see the receipt). A synonym has no
-- dictionary of its own to construct or cull — it macro-expands to the raw
-- tuple at every use site — so it sidesteps the whole class of bug and
-- still gives authored signatures the same @HasAgentApi tools m =>@ shape.
--
-- A @tools@ that forgets @deriving (Generic)@ fails at a 'compileTools' call
-- site with GHC's own "no instance for (Generic ...)" — not an authored
-- 'TypeError'. See the receipt for why: intercepting that case needs two
-- instances with the same head distinguished only by constraint
-- satisfiability, which plain instance resolution can't do.
type HasAgentApi tools m =
  ( Generic (tools (AsServerT m))
  , GCompileTools (Rep (tools (AsServerT m))) m
  )

-- | Compile an interpreted tools record into declarations + a dispatcher.
-- One 'GCompileTools' traversal produces 'ToolEntry' list @named@;
-- 'declarations' and the dispatch table are both plain projections of that
-- SAME list — see 'ToolEntry'.
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
