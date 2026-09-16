{-# LANGUAGE GADTs #-}
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

unrelated :: Int
unrelated = 42
