{-# LANGUAGE AllowAmbiguousTypes, FlexibleContexts #-}
{-# LANGUAGE GADTs, UnboxedTuples #-}
{-# LANGUAGE DataKinds #-}
{-# LANGUAGE FlexibleInstances #-}
{-# LANGUAGE OverloadedStrings #-}
{-# LANGUAGE TypeApplications #-}
{-# OPTIONS_GHC -O1 #-}
module TypeEvidence where

import Control.Monad.Freer (Eff, Member, send)
import Tidepool.Internal.RequestSite (RequestSite)
import Data.Text (Text)
import Data.Kind (Type)
import Numeric.Natural (Natural)
import Tidepool.Effects.Core
  ( AgentSession (AgentAttachWith), AgentTools (AgentToolsInstallWith) )
import Tidepool.Actor (receive)

newtype Identity a = Identity a
newtype Wrap (f :: Type -> Type) a = Wrap (f a)
newtype Loop = Loop Loop
data Phantom (a :: Type) = Phantom
data Choice a where
  OnlyInt :: Choice Int
data Chain = End | Link Chain
data Nest a = Nest (Nest [a])
data Nest2 a = Nest2 (Nest2 [[a]])
data SameLayout = SameLayout Int
data Packed = Packed {-# UNPACK #-} !Int
data Progress progress
  = ProgressPending
  | ProgressUpdate progress
  | ProgressClosed

-- Source telescope positions, unused binders and kinds remain semantic even
-- when variables have identical diagnostic spelling.
data ScopeReply answer where
  FirstScope :: forall a b. a -> b -> ScopeReply a
  SecondScope :: forall b a. a -> b -> ScopeReply a
  RepeatedScope :: forall a b. a -> b -> ScopeReply (Either a a)
  DistinctScope :: forall a b. a -> b -> ScopeReply (Either a b)
  AlphaScope :: forall a b. a -> b -> ScopeReply a
  AlphaRenamed :: forall reply ignored. reply -> ignored -> ScopeReply reply
  UnusedScope :: forall (ignored :: Type) a. a -> ScopeReply a
  NoUnusedScope :: forall a. a -> ScopeReply a
  HigherKindScope :: forall (ignored :: Type -> Type) a. a -> ScopeReply a

data Alts f xs where
  (:|) :: Alts f a -> Alts f rest -> Alts f (Either a rest)
infixr 5 :|

data EffectProfile (protocol :: Type -> Type) (effects :: [Type -> Type]) where
  ReadOnly :: EffectProfile protocol '[protocol, Maybe]

profileWitness :: EffectProfile Maybe '[Maybe, Maybe]
profileWitness = ReadOnly

boolAnswer :: Maybe Bool
boolAnswer = receive @Bool "bool"

nestedIdentity :: Maybe (Identity (Identity Int))
nestedIdentity = receive @(Identity (Identity Int)) "nested"

higherKinded :: Maybe (Wrap Maybe Int)
higherKinded = receive @(Wrap Maybe Int) "higher-kinded"

recursiveNewtype :: Maybe Loop
recursiveNewtype = receive @Loop "recursive newtype"

impossibleGadt :: Maybe (Choice Bool)
impossibleGadt = receive @(Choice Bool) "impossible"

recursiveData :: Maybe Chain
recursiveData = receive @Chain "recursive"

expandingData :: Maybe (Nest Int)
expandingData = receive @(Nest Int) "expanding"

expandingPair :: Maybe (Nest Int, Nest Bool)
expandingPair = receive @(Nest Int, Nest Bool) "two nonregular applications"

fasterExpanding :: Maybe (Nest2 Int)
fasterExpanding = receive @(Nest2 Int) "two nested lists per declaration"

phantomInt :: Maybe (Phantom Int)
phantomInt = receive @(Phantom Int) "phantom int"

phantomBool :: Maybe (Phantom Bool)
phantomBool = receive @(Phantom Bool) "phantom bool"

eitherIntBool :: Maybe (Either Int Bool)
eitherIntBool = receive @(Either Int Bool) "either int bool"

eitherBoolInt :: Maybe (Either Bool Int)
eitherBoolInt = receive @(Either Bool Int) "either bool int"

textAnswer :: Maybe Text
textAnswer = receive @Text "text"

integerAnswer :: Maybe Integer
integerAnswer = receive @Integer "integer"

naturalAnswer :: Maybe Natural
naturalAnswer = receive @Natural "natural"

packedAnswer :: Maybe Packed
packedAnswer = receive @Packed "packed"

-- An ordinary effect GADT: requests carry no dynamic site, so the projector
-- emits its intrinsic graph even when the final reply index is unresolved.
data Console a where
  Print :: Text -> Console ()
  Fetch :: Text -> Console (Either Bool Text)
  Echo :: a -> Console a
  ObserveProgress :: Console (Progress progress)
  FunctionReply :: Console (Int -> Int)
  PartialReply :: Console (Maybe a)
  TemplateReply :: Console SameLayout
  GenuineCarrier :: RequestSite '[Int] a -> Console a
  MismatchedCarrier :: RequestSite '[Int] Bool -> Console Int
  StrictCarrier :: {-# UNPACK #-} !(RequestSite '[Int] Int) -> Console Int
  IntegerPayload :: Int -> Console Int
  ProvenLeading :: RequestSite '[Int] Int -> Console Int
  ProvenSubmit :: Int -> RequestSite '[()] Int -> Int -> Console ()
  ProvenPublication :: Int -> RequestSite '[(), Int] Int -> Int -> Console ()
  ProvenProgressPublication :: Int -> RequestSite '[(), Bool, Int] Int -> Int -> Console ()
  ProvenExit :: Int -> RequestSite '[Int] () -> Console ()
  SplitCarrier :: Int -> RequestSite '[Int] () -> (# Int, Int #) -> Console ()

class CarriesInt a where
  carrierInt :: Int

data DictConsole parameter reply where
  DictionaryCarrier :: CarriesInt parameter => RequestSite '[] Int -> DictConsole parameter Int
  DictionaryNonLeading :: CarriesInt parameter => Int -> RequestSite '[Int] () -> Int -> DictConsole parameter ()

{-# OPAQUE customSend #-}
customSend :: Member Console effects => Eff effects ()
customSend = send (Print "custom")

templateReply :: Console SameLayout
templateReply = TemplateReply

partialReply :: Console (Maybe Int)
partialReply = PartialReply

genuineCarrier :: RequestSite '[Int] Bool -> Console Bool
genuineCarrier = GenuineCarrier

mismatchedCarrier :: RequestSite '[Int] Bool -> Console Int
mismatchedCarrier = MismatchedCarrier

strictCarrier :: RequestSite '[Int] Int -> Console Int
strictCarrier = StrictCarrier

dictionaryCarrier :: CarriesInt parameter => RequestSite '[] Int -> DictConsole parameter Int
dictionaryCarrier = DictionaryCarrier

integerPayload :: Console Int
integerPayload = IntegerPayload 1

printRequest :: Console ()
printRequest = Print "hi"

fetchRequest :: Console (Either Bool Text)
fetchRequest = Fetch "path"

echoRequest :: Console Int
echoRequest = Echo 1

progressRequest :: Console (Progress Int)
progressRequest = ObserveProgress

agentAttachRequest :: AgentSession ()
agentAttachRequest = AgentAttachWith Nothing

agentToolsInstallRequest :: AgentTools ()
agentToolsInstallRequest = AgentToolsInstallWith

functionRequest :: Console (Int -> Int)
functionRequest = FunctionReply

polyChoice :: Alts f (Either a b)
polyChoice = undefined :| undefined

polyChoiceNested :: Alts f (Either (Maybe a) b)
polyChoiceNested = undefined :| undefined

unrelated :: Int
unrelated = 42

-- Models an eta-unexpanded auxiliary root whose STG result type is the whole
-- function arrow rather than its codomain: an admitted auxiliary root that
-- is not itself a declared site, whose own
-- 'Left'/'Right' evidence a program must still carry even when 'unrelated'
-- -- the only entry that reaches it -- never otherwise constructs or
-- observes an 'Either'.
auxiliaryRootDecodeHelper :: Text -> Either Text Int
auxiliaryRootDecodeHelper _ = Left "unused"

auxiliaryRootDecode :: Text -> Either Text Int
auxiliaryRootDecode = auxiliaryRootDecodeHelper

provenLeading :: RequestSite '[Int] Int -> Console Int
provenLeading = ProvenLeading
provenSubmit :: Int -> RequestSite '[()] Int -> Int -> Console ()
provenSubmit = ProvenSubmit
provenPublication :: Int -> RequestSite '[(), Int] Int -> Int -> Console ()
provenPublication = ProvenPublication
provenProgressPublication :: Int -> RequestSite '[(), Bool, Int] Int -> Int -> Console ()
provenProgressPublication = ProvenProgressPublication
provenExit :: Int -> RequestSite '[Int] () -> Console ()
provenExit = ProvenExit
splitCarrier :: Int -> RequestSite '[Int] () -> (# Int, Int #) -> Console ()
splitCarrier = SplitCarrier
dictionaryNonLeading :: CarriesInt parameter => Int -> RequestSite '[Int] () -> Int -> DictConsole parameter ()
dictionaryNonLeading = DictionaryNonLeading
