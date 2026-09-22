{-# LANGUAGE OverloadedStrings #-}

-- | Capability-handle mailboxes over a green thread.
--
-- __Handles are capabilities: possession is permission.__ There is no
-- registry, no node ids, no addressing scheme, and no lookup-by-name or
-- enumeration anywhere in this module. A child gets its 'Uplink' and its
-- 'inbox' as ARGUMENTS to its own body ('NodeCtx') because it was handed
-- them — it never asks for either by identity.
--
-- __@up@\/@down@ are ABSOLUTE tree directions, not relative to whoever holds
-- the value.__ A parent 'sendDown's and observes 'received' (the child's up
-- messages); a child observes 'inbox' (the parent's down messages) and
-- 'sendUp's. Both ends of one fork name the SAME two type parameters the
-- SAME way — 'NodeHandle' is @NodeHandle up down r@ throughout, never
-- flipped per which end happens to be reading it.
--
-- == A widened handle, and why
--
-- The scaffold this module implements sketched @NodeHandle down r@ with no
-- typed way for the PARENT to observe an up message — an omission inherited
-- from that original sketch, which names 'sendUp' and
-- 'folded' but never says where an up message is read. Escalation
-- (@Up = Escalate Failure | Progress Text@ in that sketch's own prose) is the
-- whole point of the uplink, so the omission is a gap in the sketch, not a
-- deliberate one-directional design. 'NodeHandle' is therefore
-- @NodeHandle up down r@ here, with 'received' as 'inbox's parent-side
-- sibling — approved 2026-08-17 (see this lane's PR notes). 'forkNode'
-- mints BOTH directions' mailboxes itself, so a caller never sees a bare
-- mailbox id and "forking yields a handle" (singular) stays true.
--
-- == A widened fold, and why
--
-- The scaffold likewise sketched @folded :: NodeHandle down r -> Event r@
-- but described it as \"'waitEvent' on the underlying thread\" —
-- 'Tidepool.Event.waitEvent' carries the HANDLE, never the value, by
-- design (the typed result stays on the heap; a closure result would not
-- survive being forced into a bare 'Event' projection, which is pure and
-- cannot itself perform the effectful 'Tidepool.Async.wait'). The sketch's
-- own type and its own description disagree; this module keeps the
-- description and widens the type to match: 'folded' returns
-- @Event (Async r)@, exactly 'waitEvent' on the node's thread. A caller
-- reads the typed fold with one immediate 'Tidepool.Async.wait' after the
-- 'Event' fires, same as any other 'waitEvent' user.
--
-- == Why a separate module from "Tidepool.Async"
--
-- 'Tidepool.Async' is pure thread lifecycle (spawn\/join\/cancel) with no
-- opinion on what a thread talks about; this module is a messaging
-- PROTOCOL layered on top of one green thread ('forkNode' is 'async' plus
-- two minted mailboxes). Keeping them apart mirrors "Tidepool.Answerer.Fork" living
-- apart from the answerer surface it has nothing to do with — a reader who
-- only wants thread lifecycle should not have to read a mailbox protocol to
-- find it.
--
-- == Sends never block, and bursts coalesce
--
-- 'sendDown'\/'sendUp' append to the mailbox and return; nothing here ever
-- waits for the far end to read. A burst of sends sharing the same
-- constructor TAG (the JSON @tag@ field a generic 'ToJSON' sum produces
-- under its @TaggedObject@ encoding) coalesces to the LAST payload — the
-- coalesce key is derived HERE, in Haskell, from the message's own shape,
-- and passed to the runtime explicitly, so the Rust handler never
-- interprets a message; it only ever compares two caller-supplied keys for
-- equality. A message whose 'toJSON' is not an object with a @tag@ field
-- (a bare record, a primitive) coalesces under one shared key — every send
-- of that type replaces the last, which is the same coalescing contract
-- applied to a payload with only one \"variant\".
--
-- __Receive is an 'Event' source, not a second blocking primitive.__
-- 'inbox' and 'received' both plug into "Tidepool.Event"'s algebra
-- alongside 'Tidepool.Event.waitEvent' and @after@, so
-- @'nextEvent' (fmap Left inbox \<|\> fmap Right (after ms))@ is an
-- ordinary select. There is no blocking mailbox receive anywhere in this
-- module, and there must never be one — a second blocking primitive is
-- exactly what riding the existing 'Tidepool.Event' algebra avoids.
--
-- __Messages are reconciliation hints, not durable state.__ A coalesced or
-- (bound-overflow) dropped message must never be a correctness hole — every
-- instruction here is meant to be re-derivable from git plus the run
-- journal. No durable mailbox machinery exists or is wanted.
--
-- This module is reachable only in rows containing BOTH @Green@ (to fork
-- the thread) and @RepoEvent@ (the mailbox substrate and the whole event
-- algebra both live there) — the same coupling 'Tidepool.Event.waitEvent'
-- already has, which 'folded' below reuses directly.
module Tidepool.Node
  ( NodeHandle
  , Uplink
  , NodeCtx (..)  -- ^ includes the 'inbox' field accessor
  , forkNode
  , sendDown
  , sendUp
  , received
  , folded
  ) where

import Prelude

import Data.Text (Text)
import Tidepool.Aeson.FromJSON (FromJSON, Result (..), fromJSON)
import Tidepool.Aeson.Value (ToJSON (..), Value (..))
import qualified Tidepool.Aeson.KeyMap as KM
import Tidepool.Async (Async, async)
import Tidepool.Effects (M, liftEither, mailboxNew, mailboxSend)
-- `Event`/`mailbox` are DEFINITIONS in `Tidepool.Event`, not
-- the generated `Tidepool.Effects` module.
import Tidepool.Event (Event, mailbox, waitEvent)

-- | The parent's end of a forked node: send messages DOWN to it, observe
-- what it sends UP ('received'), and await its fold ('folded') once it
-- finishes. Opaque — the two mailbox ids and the underlying thread are
-- implementation detail; there is no way to construct one except by
-- 'forkNode'.
data NodeHandle up down r = NodeHandle
  { nhDown :: !Int
  , nhUp :: !Int
  , nhThread :: !(Async r)
  }

-- | The child's end: send messages UP to the parent. Opaque — wraps the
-- mailbox 'forkNode' minted for this direction; there is no accessor to its
-- raw id and no way to construct one except by being handed a value of this
-- type.
newtype Uplink up = Uplink Int

-- | What a forked node's body runs with: its send capability ('uplink') and
-- its receive source ('inbox'), both scoped to the ONE fork that created
-- them.
data NodeCtx up down = NodeCtx
  { uplink :: Uplink up
  , inbox :: Event down
  }

-- | Fork a node: mint both directions' mailboxes, hand the child its
-- 'NodeCtx', and run it as an ordinary green thread ('Tidepool.Async.async').
-- Returns as soon as the thread is registered, same as 'Tidepool.Async.async'
-- itself — the body runs independently from here on.
--
-- 'FromJSON' is required on BOTH type parameters here, not only at
-- 'inbox'\/'received' call sites, because 'NodeCtx' and 'NodeHandle' carry
-- their receive sources as concrete 'Event' FIELDS built at construction
-- time, not as functions computed later — so the decode capability has to
-- already be in hand when this constructs them. Fixing the types once, here,
-- is also why a body cannot request a differently-typed inbox than the fork
-- that created it declared.
forkNode
  :: (FromJSON up, FromJSON down)
  => (NodeCtx up down -> M r)
  -> M (NodeHandle up down r)
forkNode body = do
  downMid <- mailboxNew >>= liftEither
  upMid <- mailboxNew >>= liftEither
  thread <- async (body (NodeCtx (Uplink upMid) (decodeMailbox downMid)))
  pure (NodeHandle downMid upMid thread)

-- | Send a message DOWN to a forked node's child. Never blocks: appends and
-- returns. A burst sharing 'coalesceKey' coalesces to the LAST payload.
sendDown :: ToJSON down => NodeHandle up down r -> down -> M ()
sendDown h msg = mailboxSend (nhDown h) (coalesceKey msg) (toJSON msg) >>= liftEither

-- | Send a message UP to a forked node's parent. Never blocks: appends and
-- returns. A burst sharing 'coalesceKey' coalesces to the LAST payload.
sendUp :: ToJSON up => Uplink up -> up -> M ()
sendUp (Uplink mid) msg = mailboxSend mid (coalesceKey msg) (toJSON msg) >>= liftEither

-- | The parent's view of the child's up-messages — 'inbox's sibling for the
-- other direction, named for how it reads inside a select:
-- @nextEvent (fmap Left (received h) \<|\> fmap Right deadline)@.
received :: FromJSON up => NodeHandle up down r -> Event up
received h = decodeMailbox (nhUp h)

-- | Fires once when the node's thread reaches a terminal state (settled or
-- cancelled), carrying the HANDLE — 'Tidepool.Event.waitEvent' on the
-- underlying thread, so the typed fold result is one immediate
-- 'Tidepool.Async.wait' away. See this module's header for why the type
-- widens past the scaffold's @Event r@ sketch.
folded :: NodeHandle up down r -> Event (Async r)
folded h = waitEvent (nhThread h)

-- | A raw mailbox's JSON payloads, decoded to a typed 'Event' source. A
-- payload that fails to decode is a Tidepool bug (the sender and receiver
-- disagreeing on a type), not an authored-code condition to recover from —
-- it fails loudly rather than silently dropping the message.
decodeMailbox :: FromJSON a => Int -> Event a
decodeMailbox mid = fmap decodeOrFail (mailbox mid)
  where
    decodeOrFail v = case fromJSON v of
      Success a -> a
      Error e -> error ("Tidepool.Node: mailbox payload failed to decode: " ++ e)

-- | The coalesce key a send derives from its own payload: the JSON @tag@
-- field a generic sum's 'ToJSON' produces under the TaggedObject encoding,
-- or one shared empty key when there is none (a payload type with only one
-- shape coalesces every send against itself, which is the same contract
-- applied to a "sum" of one variant).
coalesceKey :: ToJSON a => a -> Text
coalesceKey msg = case toJSON msg of
  Object o -> case KM.lookup "tag" o of
    Just (String t) -> t
    _ -> ""
  _ -> ""
