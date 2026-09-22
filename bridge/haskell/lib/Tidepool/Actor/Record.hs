{-# LANGUAGE AllowAmbiguousTypes #-}
{-# LANGUAGE ConstraintKinds #-}
{-# LANGUAGE DataKinds #-}
{-# LANGUAGE EmptyDataDecls #-}
{-# LANGUAGE FlexibleContexts #-}
{-# LANGUAGE FlexibleInstances #-}
{-# LANGUAGE GADTs #-}
{-# LANGUAGE KindSignatures #-}
{-# LANGUAGE InstanceSigs #-}
{-# LANGUAGE MultiParamTypeClasses #-}
{-# LANGUAGE OverloadedStrings #-}
{-# LANGUAGE RankNTypes #-}
{-# LANGUAGE ScopedTypeVariables #-}
{-# LANGUAGE TypeApplications #-}
{-# LANGUAGE TypeFamilies #-}
{-# LANGUAGE TypeOperators #-}
{-# LANGUAGE UndecidableInstances #-}

-- | One record describes an actor's private state, calls and source handlers.
-- Generic derivation builds typed endpoints and dispatch; the ordinary actor
-- runtime still owns mailbox admission, serialization and state checkpoints.
module Tidepool.Actor.Record
  ( (:-), State, Call, NoReply, Reply, Event
  , Shape, Definition, Client, Self, Private
    -- The field, not the constructor: a handle can be read for the exact
    -- incarnation it names — which is how one actor compares a handle it holds
    -- against a reference it was sent — but it cannot be forged from parts.
  , ActorState, Handler, ActorSpec, ActorHandle (actorRef)
  , Send, Request, EventHandler, EventSource
  , on, progress, settlement, lifecycle, command
  , get, gets, put, modify'
  , start, client, send, call, finish, replace
  , definition, withWorktree
  , self, sender, ActorInputOrigin (..)
  , LocalEffects, Forwarding, forwardResult, forwardingExit
  -- Named by exported signatures ('definition', 'self', 'sender',
  -- 'LocalEffects'), so a cell binding's pinned type can name it. Abstract:
  -- the constructor stays private.
  , Message
  ) where

import Control.Monad.Freer (Eff, Member)
import qualified Control.Monad.Freer as Eff
import qualified Control.Monad.Freer.State as S
import Data.Kind (Constraint, Type)
import Data.Text (Text)
import GHC.Generics
import GHC.TypeLits (ErrorMessage (..), TypeError)
import qualified Tidepool.Actor as Actor
import Tidepool.Actor (ActorExit, EffectProfile)
import qualified Tidepool.Actor.Internal as ActorInternal
import Tidepool.Actor.Source (Source)
import qualified Tidepool.Actor.Source as Source
import Tidepool.Command.Types (Job)
import Tidepool.Effects.Core (CommandResult)
import Tidepool.Agent.Reply.Internal
  ( Progress, ProgressState, Response, ResponseFailure, ResponseResult )
import Tidepool.Effects.Core (Actor, ActorLocal, ActorInputOrigin (..))
import qualified Tidepool.Effects.Core as Core
import qualified Tidepool.Internal.ActorRef as Internal

data State (s :: Type)
data Call (input :: Type) (reply :: Type)
data NoReply
data Reply (output :: Type)
data Event (input :: Type)

data Shape
data Definition (m :: Type -> Type)
data Client
data Self
data Private = Private

type family mode :- field :: Type where
  Shape :- field = field
  Definition m :- State s = s
  Definition m :- Call input NoReply = input -> m ()
  Definition m :- Call input (Reply output) = input -> m output
  Definition m :- Event input = EventHandler m input
  Client :- State s = Private
  Client :- Call input NoReply = Send input
  Client :- Call input (Reply output) = Request input output
  Client :- Event input = Private
  Self :- State s = Private
  Self :- Call input NoReply = Send input
  Self :- Call input (Reply output) = Private
  Self :- Event input = Private
infix 0 :-

-- Endpoints contain only the capability for one route. Their constructors are
-- private; a client cannot manufacture source events or a dispatch function.
newtype Send input = Send
  { runSend :: forall effects. Member Actor effects => input -> Eff effects () }
newtype Request input output = Request
  { runCall :: forall effects. Member Actor effects => input -> Eff effects output }

send :: Member Actor effects => Send input -> input -> Eff effects ()
send endpoint input = runSend endpoint input

call :: Member Actor effects => Request input output -> input -> Eff effects output
call endpoint input = runCall endpoint input

newtype EventSource event = EventSource
  { connect :: forall protocol. (event -> protocol ()) -> [Source protocol] }

instance Functor EventSource where
  fmap f source = EventSource (\receive -> connect source (receive . f))

instance Semigroup (EventSource event) where
  left <> right = EventSource (\receive -> connect left receive ++ connect right receive)

instance Monoid (EventSource event) where
  mempty = EventSource (const [])

data EventHandler m event = EventHandler
  { eventSource :: EventSource event
  , eventHandler :: event -> m ()
  }

on :: EventSource event -> (event -> m ()) -> EventHandler m event
on = EventHandler

command :: Job -> EventSource CommandResult
command job = EventSource (\receive -> [Source.commandSource job receive])

progress :: Progress p -> EventSource (ProgressState p)
progress handle = EventSource (\receive -> [Actor.progressSource handle receive])

settlement :: Response r -> EventSource (Either ResponseFailure (ResponseResult r))
settlement handle = EventSource (\receive -> [Actor.settlementSource handle receive])

lifecycle :: ActorHandle api -> EventSource Actor.ActorLifecycle
lifecycle ActorHandle { actorRef = ref } =
  EventSource (\receive -> [Actor.lifecycleSource ref receive])

type Handler state effects = Eff (S.State state ': effects)

type family HandlerState (effects :: [Type -> Type]) :: Type where
  HandlerState (S.State state ': effects) = state
  HandlerState (effect ': effects) = HandlerState effects
  HandlerState '[] = TypeError ('Text "Actor state operations require a handler state effect")

type HasState state effects = (state ~ HandlerState effects, Member (S.State state) effects)

get :: HasState state effects => Eff effects state
get = S.get

gets :: HasState state effects => (state -> value) -> Eff effects value
gets = S.gets

put :: HasState state effects => state -> Eff effects ()
put = S.put

modify' :: HasState state effects => (state -> state) -> Eff effects ()
modify' f = do
  old <- S.get
  let new = f old
  new `seq` S.put new

type family Fields (shape :: Type -> Type) mode :: Type -> Type where
  Fields (M1 i meta fields) mode = M1 i meta (Fields fields mode)
  Fields (left :*: right) mode = Fields left mode :*: Fields right mode
  Fields (K1 i field) mode = K1 i (mode :- field)

type family States (shape :: Type -> Type) :: [Type] where
  States (M1 i meta fields) = States fields
  States (left :*: right) = Append (States left) (States right)
  States (K1 i (State s)) = '[s]
  States (K1 i field) = '[]

type family Append xs ys where
  Append '[] ys = ys
  Append (x ': xs) ys = x ': Append xs ys

type family OneState states where
  OneState '[s] = s
  OneState other = TypeError StateFieldError

type StateFieldError = 'Text "An actor record must declare exactly one State field"

-- ActorState can remain unevaluated when no route reads state. The launch
-- constraint must reject malformed records even in that case.
type family ValidState states :: Constraint where
  ValidState '[s] = ()
  ValidState other = TypeError StateFieldError

type ActorState api = OneState (States (Rep (api Shape)))
type Schema api = Rep (api Shape)
type LocalEffects api effects = ActorLocal (Message api) ': effects

-- Dispatch selects a handler polymorphically in its monad. It does not capture
-- a caller's effect environment or constrain the actor's effect-row order.
--
-- Indexed by the actor record type, never by its generic representation:
-- exported types name `Message api`, so a notebook binder's pinned type is
-- nameable (`R.Message Api`) instead of rendering `Rep (Api Shape)`.
newtype Message api result = Message
  { dispatch :: forall m. View (Schema api) (Definition m) -> m result }

newtype View shape mode = View { unView :: Fields shape mode () }

type Select api part = forall mode. View (Schema api) mode -> View part mode

newtype Address api = Address (Int, Int)

sendTo :: Member Actor effects => Address api -> Message api () -> Eff effects ()
sendTo (Address address) = Eff.send . Core.ActorCastWith address

callTo :: Member Actor effects => Address api -> Message api result -> Eff effects result
callTo (Address address) = Eff.send . Core.ActorCallWith address

class GActor api part where
  endpoints
    :: Address api
    -> Select api part
    -> View part Client
  selfEndpoints :: Address api -> Select api part -> View part Self
  sources
    :: View (Schema api) (Definition m)
    -> Select api part
    -> [Source (Message api)]
  states
    :: View part (Definition m)
    -> [OneState (States (Schema api))]

instance GActor api fields => GActor api (M1 i meta fields) where
  endpoints ref select = View (M1 (unView (endpoints @api @fields ref (\api -> View (unM1 (unView (select api)))))))
  selfEndpoints ref select = View (M1 (unView (selfEndpoints @api @fields ref (\api -> View (unM1 (unView (select api)))))))
  sources spec select = sources @api @fields spec (\api -> View (unM1 (unView (select api))))
  states :: forall m. View (M1 i meta fields) (Definition m) -> [OneState (States (Schema api))]
  states (View (M1 fields)) = states @api @fields @m (View fields)

instance (GActor api left, GActor api right) => GActor api (left :*: right) where
  endpoints ref select = View $
    unView (endpoints @api @left ref (\api -> case unView (select api) of left :*: _ -> View left))
      :*: unView (endpoints @api @right ref (\api -> case unView (select api) of _ :*: right -> View right))
  selfEndpoints ref select = View $
    unView (selfEndpoints @api @left ref (\api -> case unView (select api) of left :*: _ -> View left))
      :*: unView (selfEndpoints @api @right ref (\api -> case unView (select api) of _ :*: right -> View right))
  sources spec select =
    sources @api @left spec (\api -> case unView (select api) of left :*: _ -> View left)
      ++ sources @api @right spec (\api -> case unView (select api) of _ :*: right -> View right)
  states :: forall m. View (left :*: right) (Definition m) -> [OneState (States (Schema api))]
  states (View (left :*: right)) = states @api @left @m (View left) ++ states @api @right @m (View right)

instance (s ~ OneState (States (Schema api))) => GActor api (K1 i (State s)) where
  endpoints _ _ = View (K1 Private)
  selfEndpoints _ _ = View (K1 Private)
  sources _ _ = []
  states (View (K1 value)) = [value]

instance GActor api (K1 i (Call input NoReply)) where
  endpoints ref select = View (K1 (Send (\input -> sendTo ref (Message (\spec -> unK1 (unView (select spec)) input)))))
  selfEndpoints ref select = View (K1 (Send (\input -> sendTo ref (Message (\spec -> unK1 (unView (select spec)) input)))))
  sources _ _ = []
  states _ = []

instance GActor api (K1 i (Call input (Reply output))) where
  endpoints ref select = View (K1 (Request (\input -> callTo ref (Message (\spec -> unK1 (unView (select spec)) input)))))
  selfEndpoints _ _ = View (K1 Private)
  sources _ _ = []
  states _ = []

instance GActor api (K1 i (Event event)) where
  endpoints _ _ = View (K1 Private)
  selfEndpoints _ _ = View (K1 Private)
  sources spec select =
    connect (eventSource (unK1 (unView (select spec))))
      (\event -> Message (\current -> eventHandler (unK1 (unView (select current))) event))
  states _ = []

data ActorSpec api effects = ActorSpec
  { specLabel :: Text
  , specProfile :: EffectProfile (Message api) effects
  , specRecord :: api (Definition (Handler (ActorState api) effects))
  , specWorktree :: Maybe Core.WorktreeId
  }

definition
  :: Text
  -> EffectProfile (Message api) effects
  -> api (Definition (Handler (ActorState api) effects))
  -> ActorSpec api effects
definition label profile record = ActorSpec label profile record Nothing

-- | Start the actor holding one registered worktree. Custody is exclusive and
-- integrate authority follows custody, so this is how a record actor comes
-- to own the tree it merges into: the parent creates the worktree and hands
-- the (still unbound) id here. An actor with a worktree resolves to the
-- coding role; one without resolves to research. The host admits at most one
-- worktree per actor, so a second call replaces the first.
withWorktree :: Core.WorktreeId -> ActorSpec api effects -> ActorSpec api effects
withWorktree tree spec = spec { specWorktree = Just tree }

type Derive api effects =
  ( Generic (api Shape)
  , ValidState (States (Schema api))
  , Generic (api Client)
  , Generic (api (Definition (Handler (ActorState api) effects)))
  , Rep (api Client) ~ Fields (Schema api) Client
  , Rep (api (Definition (Handler (ActorState api) effects)))
      ~ Fields (Schema api) (Definition (Handler (ActorState api) effects))
  , GActor api (Schema api)
  , Member (ActorLocal (Message api)) effects
  )

newtype ActorHandle api = ActorHandle
  { actorRef :: Actor.ActorRef (Message api) (ActorState api) }

-- Inspection reveals exact identity, never the private mailbox or exit cell.
instance Show (ActorHandle api) where
  showsPrec precedence ActorHandle { actorRef = ref } = showParen (precedence > 10) $
    showString "ActorHandle " . shows (Internal.actorAddress ref)

lower
  :: forall api effects. Derive api effects
  => ActorSpec api effects
  -> (Actor.ActorDefinition (ActorState api) (Message api) (ActorState api), ActorState api)
lower ActorSpec { specLabel = label, specProfile = profile, specRecord = record, specWorktree = worktree } =
  let spec = View (from record)
      step :: forall result. ActorState api -> Message api result
           -> Eff effects (result, ActorState api)
      step state message = S.runState state (dispatch message spec)
      hold = maybe id (\(Core.WorktreeId raw) -> ActorInternal.withLaunchWorktree raw) worktree
      actor = hold (Actor.withSources (sources @api @(Schema api) spec id)
        (Actor.stateful label profile step))
  in case states @api @(Schema api) spec of
    [initial] -> (actor, initial)
    _ -> error "record derivation violated the single State field invariant"

start
  :: (Derive api effects, Member Actor parent)
  => ActorSpec api effects -> Eff parent (ActorHandle api)
start spec = let (actor, initial) = lower spec in ActorHandle <$> Actor.startActor actor initial

client
  :: forall api. (Generic (api Client), Rep (api Client) ~ Fields (Schema api) Client,
                 GActor api (Schema api))
  => ActorHandle api -> api Client
client ActorHandle { actorRef = Internal.ActorRef actor incarnation _ } =
  to (unView (endpoints @api @(Schema api) (Address (actor, incarnation)) id))

self
  :: forall api effects.
     ( Generic (api Self), Rep (api Self) ~ Fields (Schema api) Self
     , GActor api (Schema api)
     , Member (ActorLocal (Message api)) effects )
  => Eff effects (api Self)
self = do
  (address, _) <- Eff.send @(ActorLocal (Message api)) Core.ActorLocalContextWith
  pure (to (unView (selfEndpoints @api @(Schema api) (Address address) id)))

sender
  :: forall api effects. Member (ActorLocal (Message api)) effects
  => Eff effects ActorInputOrigin
sender = snd <$> Eff.send @(ActorLocal (Message api)) Core.ActorLocalContextWith

finish :: Member Actor effects => ActorHandle api -> Eff effects (ActorExit (ActorState api))
finish ActorHandle { actorRef = ref } = Actor.drainActor ref >> Actor.awaitExit ref

replace
  :: (Derive api effects, Member Actor parent)
  => ActorHandle api -> ActorSpec api effects -> Eff parent (ActorHandle api)
replace ActorHandle { actorRef = ref } spec =
  let (actor, _) = lower spec in ActorHandle <$> Actor.replaceActor ref actor

-- A settlement source publishes exactly once. This actor has no public input
-- endpoint, so after forwarding that value there is no accepted tail to lose.
data Forward value result = Forward (Either ResponseFailure (ResponseResult value)) result
newtype Forwarding value = Forwarding (Actor.ActorRef (Forward value) ())

instance Show (Forwarding value) where
  showsPrec precedence (Forwarding ref) = showParen (precedence > 10) $
    showString "Forwarding " . shows (Internal.actorAddress ref)

forwardingExit :: Member Actor effects => Forwarding value -> Eff effects (Maybe (ActorExit ()))
forwardingExit (Forwarding ref) = Actor.pollExit ref

forwardResult
  :: Member Actor effects
  => Response value
  -> Send (Either ResponseFailure (ResponseResult value))
  -> Eff effects (Forwarding value)
forwardResult response endpoint = Forwarding <$> Actor.startActor
  (Actor.withSources [Actor.settlementSource response (\value -> Forward value ())]
    Actor.ActorDefinition
      { Actor.label = "forward-result"
      , Actor.effectProfile = Actor.ReadOnly
      , Actor.initialization = pure
      , Actor.behavior = \() () -> Actor.receive (\(Forward value reply) -> do
          send endpoint value
          pure (reply, ()))
      , Actor.onShutdown = const (pure ())
      }) ()
