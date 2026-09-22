{-# LANGUAGE GADTs #-}
{-# LANGUAGE OverloadedStrings #-}
{-# LANGUAGE TypeApplications #-}
{-# OPTIONS_GHC -O1 #-}
module TypeEvidence where

import Data.Text (Text)
import Data.Kind (Type)
import Numeric.Natural (Natural)
import Tidepool.Effects.Core (runLLMTurn)

newtype Identity a = Identity a
newtype Wrap (f :: Type -> Type) a = Wrap (f a)
newtype Loop = Loop Loop
data Phantom (a :: Type) = Phantom
data Choice a where
  OnlyInt :: Choice Int
data Chain = End | Link Chain
data Nest a = Nest (Nest [a])
data Packed = Packed {-# UNPACK #-} !Int
data Progress progress
  = ProgressPending
  | ProgressUpdate progress
  | ProgressClosed

boolAnswer :: Maybe Bool
boolAnswer = runLLMTurn @Bool "bool"

nestedIdentity :: Maybe (Identity (Identity Int))
nestedIdentity = runLLMTurn @(Identity (Identity Int)) "nested"

higherKinded :: Maybe (Wrap Maybe Int)
higherKinded = runLLMTurn @(Wrap Maybe Int) "higher-kinded"

recursiveNewtype :: Maybe Loop
recursiveNewtype = runLLMTurn @Loop "recursive newtype"

impossibleGadt :: Maybe (Choice Bool)
impossibleGadt = runLLMTurn @(Choice Bool) "impossible"

recursiveData :: Maybe Chain
recursiveData = runLLMTurn @Chain "recursive"

expandingData :: Maybe (Nest Int)
expandingData = runLLMTurn @(Nest Int) "expanding"

phantomInt :: Maybe (Phantom Int)
phantomInt = runLLMTurn @(Phantom Int) "phantom int"

phantomBool :: Maybe (Phantom Bool)
phantomBool = runLLMTurn @(Phantom Bool) "phantom bool"

eitherIntBool :: Maybe (Either Int Bool)
eitherIntBool = runLLMTurn @(Either Int Bool) "either int bool"

eitherBoolInt :: Maybe (Either Bool Int)
eitherBoolInt = runLLMTurn @(Either Bool Int) "either bool int"

textAnswer :: Maybe Text
textAnswer = runLLMTurn @Text "text"

integerAnswer :: Maybe Integer
integerAnswer = runLLMTurn @Integer "integer"

naturalAnswer :: Maybe Natural
naturalAnswer = runLLMTurn @Natural "natural"

packedAnswer :: Maybe Packed
packedAnswer = runLLMTurn @Packed "packed"

-- An ordinary effect GADT: requests carry no dynamic site, so the projector
-- emits a synthetic reply row for each constructor with a closed reply index.
data Console a where
  Print :: Text -> Console ()
  Fetch :: Text -> Console (Either Bool Text)
  Echo :: a -> Console a
  ObserveProgress :: Console (Progress progress)
  FunctionReply :: Console (Int -> Int)

printRequest :: Console ()
printRequest = Print "hi"

fetchRequest :: Console (Either Bool Text)
fetchRequest = Fetch "path"

echoRequest :: Console Int
echoRequest = Echo 1

progressRequest :: Console (Progress Int)
progressRequest = ObserveProgress

functionRequest :: Console (Int -> Int)
functionRequest = FunctionReply

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
