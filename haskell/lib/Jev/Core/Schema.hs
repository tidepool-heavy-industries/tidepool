{-# LANGUAGE AllowAmbiguousTypes #-}
{-# LANGUAGE ConstraintKinds #-}
{-# LANGUAGE DataKinds #-}
{-# LANGUAGE DuplicateRecordFields #-}
{-# LANGUAGE FlexibleContexts #-}
{-# LANGUAGE FlexibleInstances #-}
{-# LANGUAGE FunctionalDependencies #-}
{-# LANGUAGE GADTs #-}
{-# LANGUAGE KindSignatures #-}
{-# LANGUAGE LambdaCase #-}
{-# LANGUAGE MultiParamTypeClasses #-}
{-# LANGUAGE OverloadedStrings #-}
{-# LANGUAGE PolyKinds #-}
{-# LANGUAGE RankNTypes #-}
{-# LANGUAGE ScopedTypeVariables #-}
{-# LANGUAGE StandaloneKindSignatures #-}
{-# LANGUAGE TypeApplications #-}
{-# LANGUAGE TypeFamilies #-}
{-# LANGUAGE TypeOperators #-}
{-# LANGUAGE UndecidableInstances #-}
-- 'key', 'mass' and 'margin' are answer fields, and this module binds all
-- three as ordinary locals; the shadowing is deliberate and local.
{-# OPTIONS_GHC -Wno-name-shadowing #-}

-- | The agent-facing form: an anonymous, type-indexed packet of questions,
-- alternatives and rubric levels as type-level chains of labels, pools as
-- packet cells. Polymorphic over the JSON value through "Jev.Core.Json";
-- "Jev.Operators" fixes it.
--
-- The packet's type is inferred from the questions written. Labels,
-- declarations, and handler completeness are checked at compile time with
-- messages in the author's vocabulary; wording, runtime candidates, level
-- counts, and pool correspondence are checked at preparation.
module Jev.Core.Schema
  ( -- * Modes
    Questions, Answers, type (:-)
    -- * Packets
  , type (::=), Label (..), Cell (..), CellKind, CellJson, ToQ, CellOk, Packet (..), type (++), (++.)
  , Unique, Get, Lookup
    -- * Endpoints
  , Noul, Choice, Score, Each, Group, PoolDecl
  , Q (..), A (..)
    -- * Alternatives and rubric levels
  , type (::>), type (:|:), Many, Offer, Handler, Level, Interp
  , Alts (..), Single, (.|), alt, many, manyFrom, onMany, level
  , Alternatives, AltsOk, Match, Rubric, RubricOk, Index, Selected (..)
    -- * Builders
  , noul, choice, score, each, pool, eachIn, askAbout, given, about
  , Ref (..), PoolUse, Worded (..)
    -- * Results
  , selectedKey, contenders, handle, accept, explain, Doubt (..), Policy (..)
  , massAtOrAbove
    -- * The operation
  , Schema (..), PacketSchema, Model (..), jevLatest
  , request, decode, Response (..), JevError (..), roundTrip, jev1
    -- * Internals for extension (capture replay lives outside the library)
  , Endpoint (..), Path (..), encodePath, extend, Compiled (..), leaf, lookupAnswer
  , previewAnswer, checkLegend, checkExpectation, prepareWire
  ) where

import Data.Kind (Constraint, Type)
import Data.List (nub, sortOn)
import Data.Proxy (Proxy (..))
import Data.String (IsString (..))
import Data.Text (Text)
import qualified Data.Text as T
import Data.Type.Equality (type (==), type (~~))
import GHC.OverloadedLabels (IsLabel (..))
import GHC.Records (HasField (..))
import GHC.TypeLits
import Jev.Core.Contract
import Jev.Core.Json
import Numeric (showFFloat)

-- ---------------------------------------------------------------------------
-- Modes and the interpretation of a cell
-- ---------------------------------------------------------------------------

data Questions (v :: Type)
data Answers (v :: Type)

-- | How a cell of endpoint @e@ reads under a mode. Questions are always the
-- leaf; answers are transparent for nesting, and a pool has no answer.
type family mode :- (e :: Type) :: Type where
  Questions v :- e = Q v e
  Answers v :- Group s = s (Answers v)
  Answers v :- Each s = [(Text, s (Answers v))]
  Answers v :- PoolDecl n a = ()
  Answers v :- e = A v e
infixr 0 :-

-- ---------------------------------------------------------------------------
-- Endpoints
-- ---------------------------------------------------------------------------

data Noul
data Choice (alts :: Type)
data Score (levels :: k)
data Each (s :: Type -> Type)
data Group (s :: Type -> Type)
data PoolDecl (name :: Symbol) (a :: Type)

data family Q (v :: Type) (e :: Type)
data family A (v :: Type) (e :: Type)

-- ---------------------------------------------------------------------------
-- Alternatives and levels: one chain, three shapes
-- ---------------------------------------------------------------------------

-- | A labeled alternative with a local payload; wording is supplied with
-- the payload by 'alt'.
data (k :: Symbol) ::> (p :: Type)
-- | A chain. Alternatives are @label ::> payload@ or @Many payload@; rubric
-- levels are bare labels.
data (a :: ka) :|: (b :: kb)
-- | A runtime group of alternatives sharing a payload type; keys and
-- wording per element at the value level.
data Many (p :: Type)
infix 6 ::>
infixr 4 :|:

data Label (k :: Symbol) = Label
instance k ~ k' => IsLabel k (Label k') where fromLabel = Label

-- | Interpretations of an alternative: what an offer supplies, what a
-- handler receives, what a level carries.
data Offer (v :: Type)
data Handler (v :: Type) (r :: Type)
data Level (v :: Type)

type family Interp (f :: Type) (x :: Type) :: Type where
  Interp (Offer v) (k ::> p) = (v, p)
  Interp (Offer v) (Many p) = ManyOffer v p
  Interp (Handler v r) (k ::> p) = p -> r
  Interp (Handler v r) (Many p) = Text -> p -> r

data ManyOffer v p = ManyOffer [(Text, v, p)] (Maybe (PoolUse v))

-- | Offers, handlers, or levels for a whole chain.
type Alts :: Type -> forall k. k -> Type
data Alts f alts where
  One :: KnownSymbol k => Interp f (k ::> p) -> Alts f (k ::> p)
  Grp :: Interp f (Many p) -> Alts f (Many p)
  Lvl :: KnownSymbol l => v -> Alts (Level v) (l :: Symbol)
  (:|) :: Alts f x -> Alts f rest -> Alts f (x :|: rest)
infixr 4 :|

-- | The left of a chain is one element; the chain associates to the
-- right, so no parentheses are needed and none are accepted.
type Single :: forall k. k -> Constraint
type family Single x where
  Single @Type (a :|: b) = TypeError ('Text "a parenthesised group stands where one alternative or level is expected; .| associates to the right, so write a .| b .| c without parentheses")
  Single x = ()

(.|) :: Single x => Alts f x -> Alts f rest -> Alts f (x :|: rest)
(.|) = (:|)
infixr 4 .|

-- | One alternative: its label, its wording for the provider, its payload
-- for the program. The alternative's type is inferred from this.
alt :: KnownSymbol k => Label k -> v -> p -> Alts (Offer v) (k ::> p)
alt _ d p = One (d, p)

many :: [(Text, v, p)] -> Alts (Offer v) (Many p)
many es = Grp (ManyOffer es Nothing)

onMany :: (Text -> p -> r) -> Alts (Handler v r) (Many p)
onMany = Grp

-- | One rubric level: its label and its wording.
level :: KnownSymbol l => Label l -> v -> Alts (Level v) l
level _ = Lvl

-- Handlers are written with labels; the label and the function fix the
-- alternative, so a handler list is an ordinary value with an inferred
-- type. 'handle' checks it against the alternatives in declaration order,
-- with messages that name both.
type family HandlerShape (k :: Symbol) (x :: Type) :: Constraint where
  HandlerShape k (p -> r) = ()
  HandlerShape k (a, b) = TypeError ('Text "offers are written alt #" ':<>: 'Text k ':<>: 'Text " wording payload; #" ':<>: 'Text k ':<>: 'Text " alone builds a handler")
  HandlerShape k x = TypeError ('Text "#" ':<>: 'Text k ':<>: 'Text " takes a handler: a function of the payload")
type family ArgOf (x :: Type) :: Type where ArgOf (p -> r) = p
type family ResOf (x :: Type) :: Type where ResOf (p -> r) = r

-- When the alternative is already known from context, its label is checked
-- here, with the same messages 'handle' gives when it is inferred first.
type LabelFits :: Symbol -> forall ka. ka -> Constraint
type family LabelFits k alt where
  LabelFits k @Type (k ::> p) = ()
  LabelFits k @Type (Many p) = TypeError ('Text "#" ':<>: 'Text k ':<>: 'Text " is written where the runtime group (Many) of this disjunction stands; use onMany")
  LabelFits k @Type (k' ::> p) = TypeError ('Text "#" ':<>: 'Text k ':<>: 'Text " is written where the alternative #" ':<>: 'Text k' ':<>: 'Text " stands (handlers follow declaration order)")
  LabelFits k @Type (a :|: b) = TypeError ('Text "#" ':<>: 'Text k ':<>: 'Text " stands alone where the disjunction continues; chain handlers with .| (right-associated, without parentheses)")
  LabelFits k alt = ()

instance (KnownSymbol k, HandlerShape k x, LabelFits k alt, x ~ (ArgOf x -> ResOf x), f ~ Handler v (ResOf x), alt ~~ (k ::> ArgOf x)) => IsLabel k (x -> Alts f alt) where
  fromLabel = One

-- | Handlers against alternatives, position by position.
type family Match (hs :: Type) (alts :: Type) :: Constraint where
  Match (k ::> p) (k ::> p') = p ~ p'
  Match (Many p) (Many p') = p ~ p'
  Match (h :|: hs) (a :|: as) = (Match h a, Match hs as)
  Match (k ::> p) (Many p') = TypeError ('Text "#" ':<>: 'Text k ':<>: 'Text " is written where the runtime group (Many) of this disjunction stands; use onMany")
  Match (Many p) (k ::> p') = TypeError ('Text "onMany is written where the alternative #" ':<>: 'Text k ':<>: 'Text " stands (handlers follow declaration order)")
  Match (k ::> p) (k' ::> p') = TypeError ('Text "#" ':<>: 'Text k ':<>: 'Text " is written where the alternative #" ':<>: 'Text k' ':<>: 'Text " stands (handlers follow declaration order)")
  Match (h :|: hs) a = TypeError ('Text "handlers continue past the end of the disjunction: " ':<>: Describe hs ':<>: 'Text " has no alternative")
  Match h (a :|: as) = TypeError ('Text "handlers stop after " ':<>: Describe h ':<>: 'Text "; " ':<>: Describe a ':<>: 'Text " still needs a handler; chain handlers with .|")
type family Describe (x :: Type) :: ErrorMessage where
  Describe (k ::> p) = 'Text "#" ':<>: 'Text k
  Describe (Many p) = 'Text "the runtime group (Many)"
  Describe (h :|: hs) = Describe h

-- | The selected alternative, carrying the payload it was offered with.
data Selected v alts where
  SelOne :: p -> Selected v (k ::> p)
  SelMany :: Text -> v -> p -> Selected v (Many p)
  SelLeft :: Selected v x -> Selected v (x :|: rest)
  SelRight :: Selected v rest -> Selected v (x :|: rest)

-- | Static labels unique, checked at compile time; counts at preparation.
type family AltsOk (alts :: Type) :: Constraint where
  AltsOk alts = UniqueLabels (AltLabels alts)
type family AltLabels (alts :: Type) :: [Symbol] where
  AltLabels (k ::> p) = '[k]
  AltLabels (Many p) = '[]
  AltLabels (x :|: rest) = AltLabels x ++ AltLabels rest
type family UniqueLabels (ls :: [Symbol]) :: Constraint where
  UniqueLabels '[] = ()
  UniqueLabels (l ': ls) = (SymbolAbsent l ls, UniqueLabels ls)
type family SymbolAbsent (l :: Symbol) (ls :: [Symbol]) :: Constraint where
  SymbolAbsent l '[] = ()
  SymbolAbsent l (l ': ls) = TypeError ('Text "Jev: duplicate label #" ':<>: 'Text l)
  SymbolAbsent l (j ': ls) = SymbolAbsent l ls

-- | Compile, decode, and eliminate a disjunction shape by shape.
class Alternatives (alts :: Type) where
  altWire :: JsonValue v => Text -> Alts (Offer v) alts -> Either PrepError [(Text, v)]
  altUses :: Alts (Offer v) alts -> [PoolUse v]
  altSelect :: Alts (Offer v) alts -> Text -> Maybe (Selected v alts)
  altHandle :: Alts (Handler v r) alts -> Selected v alts -> r
  altKeyOf :: Selected v alts -> Text

instance KnownSymbol k => Alternatives (k ::> p) where
  altWire key (One (d, _)) = checkDescription key (label @k) d >> Right [(label @k, d)]
  altUses _ = []
  altSelect (One (_, p)) sel = if sel == label @k then Just (SelOne p) else Nothing
  altHandle (One h) (SelOne p) = h p
  altKeyOf _ = label @k

instance Alternatives (Many p) where
  altWire key (Grp (ManyOffer es pooled)) = do
    let keys = [k | (k, _, _) <- es]
    if length keys /= length (nub keys) then Left (DuplicateKeys key [k | k <- nub keys, length (filter (== k) keys) > 1]) else Right ()
    case pooled of
      Nothing -> mapM_ (\(k, d, _) -> checkDescription key k d) es >> Right [(k, d) | (k, d, _) <- es]
      Just _ -> Right [(k, jNull) | (k, _, _) <- es]
  altUses (Grp (ManyOffer _ u)) = maybe [] pure u
  altSelect (Grp (ManyOffer es _)) sel =
    case [SelMany k d p | (k, d, p) <- es, k == sel] of
      e : _ -> Just e
      [] -> Nothing
  altHandle (Grp h) (SelMany k _ p) = h k p
  altKeyOf (SelMany k _ _) = k

instance (Alternatives x, Alternatives rest) => Alternatives (x :|: rest) where
  altWire key (c :| rest) = (++) <$> altWire key c <*> altWire key rest
  altUses (c :| rest) = altUses c ++ altUses rest
  altSelect (c :| rest) sel = case altSelect c sel of
    Just s -> Just (SelLeft s)
    Nothing -> SelRight <$> altSelect rest sel
  altHandle (c :| rest) = \case
    SelLeft s -> altHandle c s
    SelRight s -> altHandle rest s
  altKeyOf = \case
    SelLeft s -> altKeyOf s
    SelRight s -> altKeyOf s

label :: forall k. KnownSymbol k => Text
label = T.pack (symbolVal (Proxy @k))

-- ---------------------------------------------------------------------------
-- Rubrics: a chain of bare labels
-- ---------------------------------------------------------------------------

class Rubric (levels :: k) where
  rubricEntries :: Alts (Level v) levels -> [(Text, v)]

instance KnownSymbol l => Rubric (l :: Symbol) where
  rubricEntries (Lvl d) = [(label @l, d)]

instance (Rubric x, Rubric rest) => Rubric ((x :: kx) :|: (rest :: kr)) where
  rubricEntries (x :| rest) = rubricEntries x ++ rubricEntries rest

type RubricLabels :: forall k. k -> [Symbol]
type family RubricLabels levels where
  RubricLabels @Symbol l = '[l]
  RubricLabels @Type (x :|: rest) = RubricLabels x ++ RubricLabels rest
  RubricLabels x = TypeError ('Text "Jev: a rubric is a chain of bare labels, such as \"low\" :|: \"high\"; found " ':<>: 'ShowType x)

type family RubricOk (levels :: k) :: Constraint where
  RubricOk levels = UniqueLabels (RubricLabels levels)

type family Index (l :: Symbol) (levels :: k) :: Nat where
  Index l levels = IndexIn l (RubricLabels levels)
type family IndexIn (l :: Symbol) (ls :: [Symbol]) :: Nat where
  IndexIn l '[] = TypeError ('Text "Jev: no level #" ':<>: 'Text l ':<>: 'Text " in this rubric")
  IndexIn l (l ': ls) = 0
  IndexIn l (j ': ls) = 1 + IndexIn l ls

-- ---------------------------------------------------------------------------
-- Leaves
-- ---------------------------------------------------------------------------

type PoolUse v = (Text, v)   -- pool name, serialized {key: description}

data instance Q v Noul = NoulQ (Instructions v) (Presence (Maybe (Criteria v))) [PoolUse v]

-- | What the provider said about a proposition, in one field.
newtype instance A v Noul = NoulA { yes :: Double }

data instance Q v (Choice alts) = ChoiceQ (Instructions v) (Alts (Offer v) alts)

-- | What the provider chose, with everything a caller judges it by. Read
-- the fields with record dot: @a.next.key@, @a.next.margin@.
data instance A v (Choice alts) = Chosen
  { chosen :: Selected v alts            -- ^ the winner, carrying its payload
  , key :: Text                          -- ^ the winner's wire key
  , mass :: Double                       -- ^ the winner's probability
  , margin :: Double                     -- ^ winner minus runner-up; the mass when it stands alone
  , confidence :: Double                 -- ^ the provider's own confidence
  , masses :: [(Text, Double)]           -- ^ the full distribution, best first
  , ranked :: [(Double, Selected v alts)] -- ^ every alternative as a selection, best first
  }

data instance Q v (Score levels) = ScoreQ (Instructions v) (Alts (Level v) levels)

-- | Where on the rubric the provider landed. Read with record dot:
-- @a.urgency.nearest@, @a.urgency.expectation@.
data instance A v (Score levels) = Scored
  { expectation :: Double        -- ^ the expected level index
  , nearest :: Text              -- ^ the label of the level nearest the expectation
  , confidence :: Double         -- ^ the provider's own confidence
  , masses :: [(Text, Double)]   -- ^ the distribution, by level label, in level order
  }

newtype instance Q v (Each s) = EachQ [(Text, s (Questions v))]
newtype instance A v (Each s) = EachA [(Text, s (Answers v))]

newtype instance Q v (Group s) = GroupQ (s (Questions v))
newtype instance A v (Group s) = GroupA (s (Answers v))

newtype instance Q v (PoolDecl n a) = PoolQ [(Text, v, a)]
data instance A v (PoolDecl n a) = PoolA

-- | A reference into a declared pool; constructor hidden.
data Ref v (n :: Symbol) a = Ref
  { refKey :: Text
  , refDescription :: v
  , refPayload :: a
  , refUse :: PoolUse v
  }

-- Internal readers: 'confidence' and 'masses' are fields of two records, so
-- the module names them by pattern rather than by an ambiguous selector.
chosenMasses :: A v (Choice alts) -> [(Text, Double)]
chosenMasses Chosen { masses = ms } = ms

chosenConfidence :: A v (Choice alts) -> Double
chosenConfidence Chosen { confidence = c } = c

scoreMasses :: A v (Score levels) -> [(Text, Double)]
scoreMasses Scored { masses = ms } = ms

scoreConfidence :: A v (Score levels) -> Double
scoreConfidence Scored { confidence = c } = c

-- | Answers print as their own fields. Probabilities are shown to two
-- decimals: they are a provider's judgment, not an exact quantity.
instance Show (A v Noul) where
  show a = "Noul {yes = " <> T.unpack (fmt2 (yes a)) <> "}"

instance Show (A v (Choice alts)) where
  show a@Chosen { key = k, mass = m, margin = g } =
    "Choice {key = " <> show k <> ", mass = " <> T.unpack (fmt2 m) <> ", margin = " <> T.unpack (fmt2 g)
      <> ", confidence = " <> T.unpack (fmt2 (chosenConfidence a)) <> ", masses = " <> T.unpack (showMasses (chosenMasses a)) <> "}"

instance Show (A v (Score levels)) where
  show a@Scored { nearest = l, expectation = e } =
    "Score {nearest = " <> show l <> ", expectation = " <> T.unpack (fmt2 e)
      <> ", confidence = " <> T.unpack (fmt2 (scoreConfidence a)) <> ", masses = " <> T.unpack (showMasses (scoreMasses a)) <> "}"

fmt2 :: Double -> Text
fmt2 x = T.pack (showFFloat (Just 2) x "")

showMasses :: [(Text, Double)] -> Text
showMasses ms = "[" <> T.intercalate ", " [T.pack (show k) <> " " <> fmt2 m | (k, m) <- ms] <> "]"

-- ---------------------------------------------------------------------------
-- Builders (all total; shapes are checked at preparation)
-- ---------------------------------------------------------------------------

noul :: JsonValue v => Text -> Q v Noul
noul t = NoulQ (question t) Omitted []

choice :: forall alts v. (JsonValue v, AltsOk alts) => Text -> Alts (Offer v) alts -> Q v (Choice alts)
choice t = ChoiceQ (question t)

score :: forall levels v. (JsonValue v, RubricOk levels) => Text -> Alts (Level v) levels -> Q v (Score levels)
score t = ScoreQ (question t)

each :: [(Text, s (Questions v))] -> Q v (Each s)
each = EachQ

-- | A pool declaration, named at its binding; the cell it is placed in
-- must carry the same label.
pool :: Label n -> [(Text, v, a)] -> Q v (PoolDecl n a)
pool _ = PoolQ

poolUse :: forall n v a. (JsonValue v, KnownSymbol n) => Q v (PoolDecl n a) -> PoolUse v
poolUse (PoolQ es) = (label @n, jObject [(k, d) | (k, d, _) <- es])

refs :: forall n v a. (JsonValue v, KnownSymbol n) => Q v (PoolDecl n a) -> [Ref v n a]
refs p@(PoolQ es) = [Ref k d a (poolUse p) | (k, d, a) <- es]

-- | Runtime alternatives drawn from a pool: null wording on the wire, the
-- pool named in the question, the pool's wording retained locally.
manyFrom :: forall n v a. (JsonValue v, KnownSymbol n) => Q v (PoolDecl n a) -> Alts (Offer v) (Many a)
manyFrom p@(PoolQ es) = Grp (ManyOffer es (Just (poolUse p)))

eachIn :: forall n v a s. (JsonValue v, KnownSymbol n) => Q v (PoolDecl n a) -> (Ref v n a -> s (Questions v)) -> Q v (Each s)
eachIn p f = EachQ [(refKey r, f r) | r <- refs p]

-- | A question about one pool entry, addressed by structured fields.
askAbout :: forall n v a. (JsonValue v, KnownSymbol n) => Ref v n a -> Text -> Q v Noul
askAbout r t = NoulQ (Structured [("question", jString t), ("pool", jString (label @n)), ("key", jString (refKey r))]) Omitted [refUse r]

-- | Endpoints whose wording can be reshaped after building.
class Worded e where
  reword :: (Instructions v -> Instructions v) -> Q v e -> Q v e

instance Worded Noul where reword f (NoulQ i c u) = NoulQ (f i) c u
instance Worded (Choice alts) where reword f (ChoiceQ i o) = ChoiceQ (f i) o
instance Worded (Score levels) where reword f (ScoreQ i d) = ScoreQ (f i) d

-- | Prefix a runtime premise. The original wording is preserved under the
-- premise; nested premises wrap again.
given :: Worded e => Text -> Q v e -> Q v e
given p = reword (Premised p)

-- | Add structured members beside the question: @{"question": …, …}@. A
-- duplicate member is a preparation error.
about :: (JsonValue v, Worded e) => [(Text, v)] -> Q v e -> Q v e
about kv = reword (extras kv)

-- ---------------------------------------------------------------------------
-- Results
-- ---------------------------------------------------------------------------

-- | The wire key of a selection: a label or a runtime element key.
selectedKey :: Alternatives alts => Selected v alts -> Text
selectedKey = altKeyOf

-- | A selection reads its own wire key: @s.key@, the same field an answer
-- carries.
instance Alternatives alts => HasField "key" (Selected v alts) Text where
  getField = altKeyOf

-- | The fundamental eliminator: a selection (the chosen one, an accepted
-- one, or a contender) against a handler per alternative in declaration
-- order. A missing, extra, or misordered handler is a type error naming
-- the labels.
handle :: forall alts hs v r. (Alternatives alts, Match hs alts, hs ~ alts) => Selected v alts -> Alts (Handler v r) hs -> r
handle s hs = altHandle hs s

-- | Every alternative at or above a mass floor, best first, as typed
-- selections the same handlers eliminate.
contenders :: Double -> A v (Choice alts) -> [(Double, Selected v alts)]
contenders floor' a = [(m, s) | (m, s) <- ranked a, m >= floor']

data Doubt
  = NearTie (Text, Double) (Text, Double)  -- winner and runner-up too close
  | Underweight Double                     -- the winner's mass is below the floor
  | Unconfident Double                     -- the provider's confidence is below the floor
  deriving (Show, Eq)

data Policy = Policy
  { minMass :: Double
  , minMargin :: Double
  , minConfidence :: Double
  }

-- | Pure policy-aware selection: the chosen alternative, or structured
-- doubt. The answer stays in hand for inspection or resumption.
accept :: Alternatives alts => Policy -> A v (Choice alts) -> Either Doubt (Selected v alts)
accept policy a =
  let winner = altKeyOf (chosen a)
      mass = maybe 0 id (lookup winner (chosenMasses a))
      runnerUp = [r | r@(k, _) <- chosenMasses a, k /= winner]
  in if chosenConfidence a < minConfidence policy then Left (Unconfident (chosenConfidence a))
     else if mass < minMass policy then Left (Underweight mass)
     else case runnerUp of
       (k2, p2) : _ | mass - p2 < minMargin policy -> Left (NearTie (winner, mass) (k2, p2))
       _ -> Right (chosen a)

-- | One line explaining why 'accept' returned what it did: which check
-- passed or failed, and the numbers behind it. Two-decimal formatting.
explain :: forall alts v. Alternatives alts => Policy -> A v (Choice alts) -> Text
explain policy a@Chosen { mass = mass, margin = margin } =
  let conf = chosenConfidence a
      items = [("confidence" :: Text, conf, minConfidence policy), ("mass", mass, minMass policy), ("margin", margin, minMargin policy)]
  in case accept policy a of
    Right _ -> "accepted: " <> T.intercalate ", " [n <> " " <> fmt2 v <> " \8805 " <> fmt2 t | (n, v, t) <- items]
    Left doubt ->
      let (ctor, failedName, failedValue, floorValue) = case doubt of
            Unconfident c -> ("Unconfident", "confidence" :: Text, c, minConfidence policy)
            Underweight m -> ("Underweight", "mass", m, minMass policy)
            NearTie (_, m) (_, m2) -> ("NearTie", "margin", m - m2, minMargin policy)
          floorLine = failedName <> " " <> fmt2 failedValue <> " < " <> fmt2 floorValue <> " by " <> fmt2 (floorValue - failedValue)
          rest = [n <> " " <> fmt2 v | (n, v, _) <- items, n /= failedName]
      in "doubted (" <> ctor <> "): " <> floorLine <> "; " <> T.intercalate ", " rest

-- | Mass at or beyond a level, by label.
massAtOrAbove :: forall l levels v. KnownNat (Index l levels) => Label l -> A v (Score levels) -> Double
massAtOrAbove _ a = sum [m | (i, m) <- zip [0 :: Integer ..] (map snd (scoreMasses a)), i >= natVal (Proxy @(Index l levels))]

-- | The label of the level nearest an expectation, over the levels in order.
nearestLevel :: Double -> [Text] -> Text
nearestLevel e ls = case drop (round e) ls of
  l : _ -> l
  [] -> if null ls then "" else last ls

-- ---------------------------------------------------------------------------
-- Paths and compilation output
-- ---------------------------------------------------------------------------

data Path = Segments [Text] | Exactly Text

encodePath :: Path -> Text
encodePath = \case
  Exactly k -> k
  Segments ss -> T.intercalate "." (map escape ss)
  where
    escape = T.concatMap (\c -> case c of
      '\\' -> "\\\\"
      '.' -> "\\."
      _ -> T.singleton c)

extend :: Path -> Text -> Path
extend (Segments ss) s = Segments (ss ++ [s])
extend (Exactly k) s = Segments [k, s]

isTopLevel :: Path -> Bool
isTopLevel = \case
  Segments [_] -> True
  Exactly _ -> True
  _ -> False

data Compiled v = Compiled
  { wire :: [(Text, WireQuestion v)]
  , declared :: [PoolUse v]
  , used :: [PoolUse v]
  }
instance Semigroup (Compiled v) where
  Compiled w d u <> Compiled w' d' u' = Compiled (w ++ w') (d ++ d') (u ++ u')
instance Monoid (Compiled v) where
  mempty = Compiled [] [] []

-- ---------------------------------------------------------------------------
-- Endpoints: compile, decode, unwrap, preview
-- ---------------------------------------------------------------------------

class JsonValue v => Endpoint v e where
  compileQ :: Path -> Q v e -> Either PrepError (Compiled v)
  decodeA :: Path -> Q v e -> [(Text, v)] -> Either DecodeError (A v e)
  -- | The answer as a cell reads it (transparent for nesting and pools).
  unwrapA :: A v e -> Answers v :- e
  -- | A payload-independent summary for inspection.
  previewA :: Answers v :- e -> v

-- | Preview an answer whose endpoint is fixed by the answer type.
previewAnswer :: forall v e. Endpoint v e => A v e -> v
previewAnswer = previewA @v @e . unwrapA

lookupAnswer :: Path -> [(Text, v)] -> Either DecodeError v
lookupAnswer p ws = maybe (Left (MissingAnswer key)) Right (lookup key ws)
  where key = encodePath p

leaf :: JsonValue v => Text -> WireQuestion v -> Compiled v
leaf key q = Compiled [(key, q)] [] []

instance JsonValue v => Endpoint v Noul where
  compileQ p (NoulQ i c uses) = do
    let key = encodePath p
    checkInstructions key i
    case c of
      Present (Just (Criteria y n)) -> do
        mapM_ (checkDescription key "true") [d | Present d <- [y]]
        mapM_ (checkDescription key "false") [d | Present d <- [n]]
      _ -> Right ()
    Right (leaf key (WNoul i c)) { used = uses }
  decodeA p _ ws = lookupAnswer p ws >>= \v -> do
    NoulAnswer x <- parseNoul (encodePath p) v
    Right (NoulA x)
  unwrapA = id
  previewA a = jObject [("yes", jNumber (yes a))]

instance (JsonValue v, Alternatives alts) => Endpoint v (Choice alts) where
  compileQ p (ChoiceQ i0 offer) = do
    let key = encodePath p
    i <- case altUses offer of
      [] -> Right i0
      [(n, _)] -> Right (extras [("pool", jString n)] i0)
      _ -> Left (MultiplePoolsInChoice key)
    checkInstructions key i
    alts <- altWire key offer
    if null alts then Left (EmptyOffer key) else Right ()
    if length alts > 255 then Left (TooManyAlternatives key (length alts)) else Right ()
    case [k | k <- map fst alts, length (filter (== k) (map fst alts)) > 1] of
      k : _ -> Left (KeyCollidesWithLabel key k)
      [] -> Right ()
    Right (leaf key (WChoice i alts)) { used = altUses offer }
  decodeA p (ChoiceQ _ offer) ws = lookupAnswer p ws >>= \v -> do
    let key = encodePath p
    ChoiceAnswer sel ms conf <- parseChoice key v
    alts <- altWire key offer `orDecode` key
    let keys = map fst alts
    winner <- maybe (Left (UnknownSelection key sel)) Right (altSelect offer sel)
    distribution key keys ms conf
    let best = sortOn (negate . snd) ms
        rankedAll = [(m, s) | (k, m) <- best, Just s <- [altSelect offer k]]
        winnerKey = altKeyOf winner
        winnerMass = maybe 0 id (lookup winnerKey best)
        runnerUp = [m | (k, m) <- best, k /= winnerKey]
        winnerMargin = case runnerUp of { m : _ -> winnerMass - m; [] -> winnerMass }
    Right Chosen
      { chosen = winner
      , key = winnerKey
      , mass = winnerMass
      , margin = winnerMargin
      , confidence = conf
      , masses = best
      , ranked = rankedAll
      }
  unwrapA = id
  previewA a@Chosen { key = k, mass = m, margin = g } = jObject
    [ ("key", jString k)
    , ("mass", jNumber m)
    , ("margin", jNumber g)
    , ("confidence", jNumber (chosenConfidence a))
    , ("masses", jObject [(mk, jNumber mm) | (mk, mm) <- chosenMasses a])
    ]

orDecode :: Either PrepError x -> Text -> Either DecodeError x
orDecode e key = either (const (Left (Malformed key "retained offer failed to render"))) Right e

instance (JsonValue v, Rubric levels) => Endpoint v (Score levels) where
  compileQ p (ScoreQ i rubric) = do
    let key = encodePath p
        entries = rubricEntries rubric
    checkInstructions key i
    if null entries || length entries > 10 then Left (BadLevelCount key (length entries)) else Right ()
    mapM_ (\(ix, (_, l)) -> checkLevel key ix l) (zip [0 ..] entries)
    Right (leaf key (WScore i (map snd entries)))
  decodeA p (ScoreQ _ rubric) ws = lookupAnswer p ws >>= \v -> do
    let key = encodePath p
        entries = rubricEntries rubric
        labels = map fst entries
        indices = [T.pack (show i) | i <- [0 .. length labels - 1]]
    ScoreAnswer e lg ms conf <- parseScore key v
    distribution key indices ms conf
    checkLegend key (map snd entries) lg
    checkExpectation key (length labels) e
    let byIndex = [(l, maybe 0 id (lookup i ms)) | (i, l) <- zip indices labels]
    Right Scored { expectation = e, nearest = nearestLevel e labels, confidence = conf, masses = byIndex }
  unwrapA = id
  previewA a@Scored { expectation = e, nearest = l } = jObject
    [ ("nearest", jString l)
    , ("expectation", jNumber e)
    , ("confidence", jNumber (scoreConfidence a))
    , ("masses", jObject [(ml, jNumber m) | (ml, m) <- scoreMasses a])
    ]

checkLegend :: JsonValue v => Text -> [v] -> [(Text, v)] -> Either DecodeError ()
checkLegend key levels lg =
  let indices = [T.pack (show i) | i <- [0 .. length levels - 1]]
      matches = and [maybe False (jEqual l) (lookup i lg) | (i, l) <- zip indices levels]
  in if not matches || length lg /= length indices || any (`notElem` indices) (map fst lg)
       then Left (LegendMismatch key) else Right ()

checkExpectation :: Text -> Int -> Double -> Either DecodeError ()
checkExpectation key n e =
  if isNaN e || isInfinite e || e < 0 || e > fromIntegral (n - 1) then Left (ValueOutOfRange key "score") else Right ()

instance Schema v s => Endpoint v (Each s) where
  compileQ p (EachQ items) = mconcat <$> mapM (\(k, q) -> compileSchema (extend p k) q) items
  decodeA p (EachQ items) ws = EachA <$> mapM (\(k, q) -> (,) k <$> decodeSchema (extend p k) q ws) items
  unwrapA (EachA xs) = xs
  previewA xs = jObject [(k, previewSchema x) | (k, x) <- xs]

instance Schema v s => Endpoint v (Group s) where
  compileQ p (GroupQ q) = compileSchema p q
  decodeA p (GroupQ q) ws = GroupA <$> decodeSchema p q ws
  unwrapA (GroupA x) = x
  previewA = previewSchema

instance (JsonValue v, KnownSymbol n) => Endpoint v (PoolDecl n a) where
  compileQ p q@(PoolQ es) = do
    if isTopLevel p then Right () else Left (PoolDeclaredInNested (label @n))
    let keys = [k | (k, _, _) <- es]
    case [k | k <- nub keys, length (filter (== k) keys) > 1] of
      k : _ -> Left (DuplicatePoolKey (label @n) k)
      [] -> Right ()
    Right (Compiled [] [poolUse q] [])
  decodeA _ _ _ = Right PoolA
  unwrapA _ = ()
  previewA _ = jNull

-- ---------------------------------------------------------------------------
-- Packets
-- ---------------------------------------------------------------------------

data (k :: Symbol) ::= (e :: Type)

-- | A cell: a labeled question under 'Questions', a decoded answer under
-- 'Answers'. A pool cell's label is its pool's name, by construction.
data Cell (k :: Symbol) (e :: Type) mode where
  (:=) :: (ToQ x, CellOk k (CellKind x)) => Label k -> x -> Cell k (CellKind x) (Questions (CellJson x))
  Answered :: (Answers v :- e) -> Cell k e (Answers v)
infix 6 :=

-- | What a cell may hold: a question, or a nested packet.
type family CellKind (x :: Type) :: Type where
  CellKind (Q v e) = e
  CellKind (Packet fs (Questions v)) = Group (Packet fs)
  CellKind x = TypeError ('Text "Jev: a cell holds a question or a nested packet; this is " ':<>: 'ShowType x)

type family CellJson (x :: Type) :: Type where
  CellJson (Q v e) = v
  CellJson (Packet fs (Questions v)) = v

class ToQ (x :: Type) where
  toQ :: x -> Q (CellJson x) (CellKind x)
instance ToQ (Q v e) where toQ = id
instance ToQ (Packet fs (Questions v)) where toQ = GroupQ

type family CellOk (k :: Symbol) (e :: Type) :: Constraint where
  CellOk k (PoolDecl n a) = PoolNamed k n
  CellOk k e = ()

type family PoolNamed (k :: Symbol) (n :: Symbol) :: Constraint where
  PoolNamed k k = ()
  PoolNamed k n = TypeError ('Text "Jev: pool #" ':<>: 'Text n ':<>: 'Text " placed under label #" ':<>: 'Text k ':<>: 'Text "; a pool's cell label must be its name")

data Packet (fs :: [Type]) mode where
  Nil :: Packet '[] mode
  (:&) :: Cell k e mode -> Packet fs mode -> Packet (k ::= e ': fs) mode
infixr 5 :&

type family (++) (a :: [k]) (b :: [k]) :: [k] where
  '[] ++ b = b
  (x ': a) ++ b = x ': (a ++ b)

(++.) :: Packet fs m -> Packet gs m -> Packet (fs ++ gs) m
Nil ++. g = g
(c :& p) ++. g = c :& (p ++. g)
infixr 5 ++.

type family Labels (fs :: [Type]) :: ErrorMessage where
  Labels '[] = 'Text "nothing"
  Labels '[k ::= e] = 'Text "#" ':<>: 'Text k
  Labels (k ::= e ': fs) = 'Text "#" ':<>: 'Text k ':<>: 'Text ", " ':<>: Labels fs

type family Unique (fs :: [Type]) :: Constraint where
  Unique '[] = ()
  Unique (k ::= e ': fs) = (Absent k fs, Unique fs)
type family Absent (k :: Symbol) (fs :: [Type]) :: Constraint where
  Absent k '[] = ()
  Absent k (k ::= e ': fs) = TypeError ('Text "Jev: duplicate packet label #" ':<>: 'Text k)
  Absent k (j ::= e ': fs) = Absent k fs

type family Lookup (k :: Symbol) (fs :: [Type]) :: Type where
  Lookup k (k ::= e ': fs) = e
  Lookup k (j ::= e ': fs) = Lookup k fs

-- | Field access on an answers packet, carrying the full label list for
-- the error message.
class Get (k :: Symbol) (fs :: [Type]) (all :: [Type]) (e :: Type) | k fs all -> e where
  get :: Packet fs (Answers v) -> Answers v :- e
instance (TypeError ('Text "Jev: this packet has no #" ':<>: 'Text k ':<>: 'Text "; it has " ':<>: Labels all), e ~ ())
  => Get k '[] all e where
  get = undefined
instance (flag ~ (k == j), Get' flag k (j ::= e' ': fs) all e) => Get k (j ::= e' ': fs) all e where
  get = get' @flag @k @(j ::= e' ': fs) @all
class Get' (flag :: Bool) (k :: Symbol) (fs :: [Type]) (all :: [Type]) (e :: Type) | flag k fs all -> e where
  get' :: Packet fs (Answers v) -> Answers v :- e
instance Get' 'True k (k ::= e ': fs) all e where
  get' (Answered a :& _) = a
instance Get k fs all e => Get' 'False k (j ::= e' ': fs) all e where
  get' (_ :& p) = get @k @fs @all p

instance (Get k fs fs e, r ~ (Answers v :- e)) => HasField k (Packet fs (Answers v)) r where
  getField = get @k @fs @fs

-- | The packet traversal, by induction over the labels.
class JsonValue v => PacketSchema v (fs :: [Type]) where
  packetCompile :: Path -> Packet fs (Questions v) -> Either PrepError (Compiled v)
  packetDecode :: Path -> Packet fs (Questions v) -> [(Text, v)] -> Either DecodeError (Packet fs (Answers v))
  packetPreview :: Packet fs (Answers v) -> [(Text, v)]

instance JsonValue v => PacketSchema v '[] where
  packetCompile _ Nil = Right mempty
  packetDecode _ Nil _ = Right Nil
  packetPreview Nil = []

instance (KnownSymbol k, Endpoint v e, PacketSchema v fs) => PacketSchema v (k ::= e ': fs) where
  packetCompile p (_ := x :& rest) = (<>) <$> compileQ (extend p (label @k)) (toQ x) <*> packetCompile p rest
  packetDecode p (_ := x :& rest) ws = (:&) <$> (Answered . unwrapA <$> decodeA (extend p (label @k)) (toQ x) ws) <*> packetDecode p rest ws
  packetPreview (Answered a :& rest) = (label @k, previewA @v @e a) : packetPreview rest

instance (Show v, PacketSchema v fs) => Show (Packet fs (Answers v)) where
  show p = show (jObject (packetPreview p))

-- ---------------------------------------------------------------------------
-- Schemas
-- ---------------------------------------------------------------------------

class JsonValue v => Schema v (s :: Type -> Type) where
  compileSchema :: Path -> s (Questions v) -> Either PrepError (Compiled v)
  decodeSchema :: Path -> s (Questions v) -> [(Text, v)] -> Either DecodeError (s (Answers v))
  previewSchema :: s (Answers v) -> v

instance (JsonValue v, Unique fs, PacketSchema v fs) => Schema v (Packet fs) where
  compileSchema = packetCompile
  decodeSchema = packetDecode
  previewSchema = jObject . packetPreview

-- ---------------------------------------------------------------------------
-- The operation: request, decode
-- ---------------------------------------------------------------------------

newtype Model = Model Text deriving (Eq, Show)
instance IsString Model where fromString = Model . T.pack

jevLatest :: Model
jevLatest = Model "jev-latest"

-- | The flattened questions and declared pools, with every preparation
-- check applied.
prepareWire :: Schema v s => s (Questions v) -> Either PrepError (Compiled v)
prepareWire q = do
  c@(Compiled qs decl uses) <- compileSchema (Segments []) q
  case [n | (n, _) <- decl, length (filter ((== n) . fst) decl) > 1] of
    n : _ -> Left (DuplicatePool n)
    [] -> Right ()
  mapM_ (\(n, u) -> case lookup n decl of
    Nothing -> Left (UndeclaredPool n)
    Just d -> if jEqual d u then Right () else Left (ConflictingPool n)) uses
  let keys = map fst qs
  if null qs then Left EmptyQuestionMap else Right ()
  case [k | k <- keys, T.null k] of
    _ : _ -> Left (EmptyQuestionKey "")
    [] -> Right ()
  case [k | k <- keys, length (filter (== k) keys) > 1] of
    k : _ -> Left (DuplicateQuestionPath k)
    [] -> Right ()
  Right c

-- | The request body a transport sends.
request :: Schema v s => Model -> State v -> s (Questions v) -> Either JevError v
request (Model m) st q = either (Left . Prepare) Right $ do
  checkState st
  Compiled qs decl _ <- prepareWire q
  Right (jObject
    [ ("model", jString m)
    , ("state", if null decl then stateValue st else jObject [("context", stateValue st), ("pools", jObject decl)])
    , ("questions", jObject [(k, questionValue w) | (k, w) <- qs])
    ])

data Response v s = Response
  { answers :: s (Answers v)
  , responseModel :: Text
  , usage :: v
  , diagnostics :: [Text]
  }

-- | A response prints as its answers: the packet's labels over each
-- answer's own fields, nested packets nested.
instance (Show v, Schema v s) => Show (Response v s) where
  show r = show (previewSchema (answers r))

-- | Decode a response body against the packet that produced the request.
decode :: Schema v s => s (Questions v) -> v -> Either JevError (Response v s)
decode q body = do
  Compiled qs _ _ <- either (Left . Prepare) Right (prepareWire q)
  either (Left . Decode) Right $ parseEnvelope body >>= \case
    Rejected r -> Left (ProviderRejected r)
    Evaluated model use ws -> do
      let expected = map fst qs
          got = map fst ws
      case filter (`notElem` expected) got of
        k : _ -> Left (UnexpectedAnswer k)
        [] -> Right ()
      case [k | k <- got, length (filter (== k) got) > 1] of
        k : _ -> Left (DuplicateAnswer k)
        [] -> Right ()
      ans <- decodeSchema (Segments []) q ws
      let drift = [ k <> ": distribution sums to " <> T.pack (show total)
                  | (k, a) <- ws, Just total <- [driftOf a], abs (total - 1) > 0.01 ]
      Right (Response ans model use drift)

data JevError = Prepare PrepError | Transport Text | Decode DecodeError
  deriving (Show, Eq)

roundTrip
  :: (Monad m, Schema v s)
  => (v -> m (Either Text v)) -> Model -> State v -> s (Questions v)
  -> m (Either JevError (Response v s))
roundTrip transport model st q = case request model st q of
  Left e -> pure (Left e)
  Right body -> transport body >>= \case
    Left t -> pure (Left (Transport t))
    Right resp -> pure (decode q resp)

-- | The tiny use: one question, one answer.
jev1
  :: (Monad m, Endpoint v e, CellOk "value" e)
  => (v -> m (Either Text v)) -> Model -> State v -> Q v e
  -> m (Either JevError (Answers v :- e))
jev1 transport model st q = fmap (fmap (\r -> case answers r of Answered a :& Nil -> a)) (roundTrip transport model st ((Label :: Label "value") := q :& Nil))
