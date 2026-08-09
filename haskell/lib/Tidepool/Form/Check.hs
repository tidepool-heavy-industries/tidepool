{-# LANGUAGE NoImplicitPrelude #-}
{-# LANGUAGE DataKinds #-}
{-# LANGUAGE PolyKinds #-}
{-# LANGUAGE TypeFamilies #-}
{-# LANGUAGE TypeOperators #-}
{-# LANGUAGE UndecidableInstances #-}

-- | Compile-time diagnostics for derived forms. Compile errors are part of
-- this API: an agent that writes an underivable type must be told which
-- FIELD is wrong and what to write instead, in the vocabulary it used —
-- never in the vocabulary of a generic representation.
--
-- The mechanism is a closed type family dispatching on the @Meta@ that
-- @GHC.Generics@ already carries on @M1 S@, so the selector name is in scope
-- at exactly the point the check fires. Nothing here mentions @M1@, @K1@, or
-- an interpreter class in its output.
--
-- Two things this module must get right, both learned from the feasibility
-- spike (@plans\/self-iterating-harness\/16-generic-spike-receipts.md@):
--
-- * A diagnostic must fire BEFORE GHC's own "no instance" message, or the
--   author reads about a class they never wrote.
-- * Recursive types COMPILE and then diverge at run time. GHC does not stop
--   them: the instance is found and the recursion is at the value level.
--   'Occurs' is therefore a correctness mechanism, not a diagnostics-quality
--   nicety — without it a self-referential type builds an infinite shape when
--   forced.
--
-- These families must be used in a position where GHC has to SOLVE the
-- constraint — an instance context, discharged when the instance is
-- selected. A check written as a GIVEN (a binding's own signature context)
-- is deferred to that binding's call sites and reports there, or not at all
-- if the binding is never used. Both behaviors were confirmed against the
-- real extractor; only the instance-context position fires at the author's
-- @askUser \@T@ site, which is the one that matters.
module Tidepool.Form.Check
  ( SelKey
  , FieldCheck
  , NeedsDerivingGeneric
  , Occurs
  , RecursiveFieldError
  , Visited
  ) where

import Prelude (Bool (..), Char, Maybe (..))
import Data.Kind (Constraint, Type)
import Data.Map.Strict (Map)
import GHC.Generics (D, M1, Meta (..))
import GHC.TypeLits (ErrorMessage (..), Symbol, TypeError)

-- | The source-level key for a field, recovered from @M1 S@ metadata: a
-- record selector's own name, or a marker for a positional field.
--
-- Used only to build error messages — the runtime field key comes from
-- @selName@ and, for positional fields, the one-based index.
type family SelKey (s :: Meta) :: Symbol where
  SelKey ('MetaSel ('Just n) su ss ds) = n
  SelKey ('MetaSel 'Nothing su ss ds) = "<positional field>"

-- | Reject a field whose type cannot become a form, naming the field and
-- offering a correction.
--
-- Order matters: @[Char]@ must precede @[a]@ so a @String@ field gets the
-- @Text@ advice rather than the generic list advice.
--
-- The fall-through case is empty. That is deliberate: this family answers
-- "is this shape known-unpresentable", not "is this shape supported". A type
-- it says nothing about is handed to the interpreter, which either finds a
-- leaf, a blessed container, or a @Generic@ instance for it — and if none of
-- those exist, GHC reports the missing @Generic@ instance for that exact
-- type.
type family FieldCheck (n :: Symbol) (a :: Type) :: Constraint where
  FieldCheck n [Char] =
    TypeError
      ( 'Text "`" ':<>: 'Text n ':<>: 'Text " :: String` is not supported; use Text."
      )
  FieldCheck n [a] =
    TypeError
      ( 'Text "`" ':<>: 'Text n ':<>: 'Text "` is a list."
          ':$$: 'Text "Lists need a repeated-field editor and are not supported in v1."
      )
  FieldCheck n (Map k v) =
    TypeError
      ( 'Text "`" ':<>: 'Text n ':<>: 'Text "` is a Map."
          ':$$: 'Text "Maps need a repeated-field editor and are not supported in v1."
          ':$$: 'Text "Use a record with one field per key you actually need."
      )
  FieldCheck n (a -> b) =
    TypeError
      ( 'Text "`" ':<>: 'Text n ':<>: 'Text "` is a function."
          ':$$: 'Text "A human cannot fill in a function; ask for the data it would be applied to."
      )
  FieldCheck n (Maybe (Maybe a)) =
    TypeError
      ( 'Text "`" ':<>: 'Text n ':<>: 'Text "` has nested optionality."
          ':$$: 'Text "Use one Maybe layer, or define an explicit sum with domain-named constructors."
      )
  FieldCheck n a = ()

-- | Name the FIELD whose type has no @Generic@ instance.
--
-- GHC's own @No instance for (Generic Environment)@ is correct and appears
-- alongside this; what it cannot say is which field led there, and for a
-- nested type that is the part the author needs.
--
-- The single equation is the whole mechanism. When @a@ derives @Generic@,
-- @Rep a@ reduces to a @M1 D@ and this discharges to nothing. When it does
-- not, @Rep a@ is STUCK — not apart from @M1 D@, since it might still reduce —
-- so no fall-through equation can fire and no 'TypeError' can be raised.
-- Whether a type has an instance is not observable from a type family. What is
-- left is to carry the field name inside the constraint that fails, which is
-- what the @n@ parameter is for: it appears verbatim in GHC's report.
type family NeedsDerivingGeneric (n :: Symbol) (a :: Type) (r :: Type -> Type) :: Constraint where
  NeedsDerivingGeneric n a (M1 D d f) = ()

-- | The set of datatypes already entered on the current derivation path.
type Visited = [Type]

-- | Is @a@ already on the derivation path?
--
-- This is load-bearing for CORRECTNESS, not just for message quality. A
-- recursive type such as
--
-- > data Tree = Leaf | Node { left :: Tree, right :: Tree }
--
-- satisfies every instance GHC looks for — the recursion lives in the values,
-- not the dictionaries — so it compiles cleanly and then builds an infinite
-- shape the moment anything forces it.
--
-- 'Occurs' answers with a plain 'Bool' and never errors, so an interpreter can
-- DISPATCH on it. That is the whole point. Rejecting the cycle by making the
-- extended path itself a 'TypeError' does not work: instance heads match on
-- the generic representation, not on the path, so the erroring path is carried
-- along as an opaque type and the next level down is demanded anyway — GHC
-- unrolls the type forever instead of reporting anything. A 'Bool' the
-- interpreter must reduce to pick an instance is what actually stops the
-- descent, because the instance it picks asks for nothing further.
type family Occurs (a :: Type) (seen :: Visited) :: Bool where
  Occurs a '[] = 'False
  Occurs a (a ': rest) = 'True
  Occurs a (b ': rest) = Occurs a rest

-- | The message for a field that closes a cycle. A 'Constraint' rather than a
-- family so it can sit in the context of the instance chosen when 'Occurs'
-- says 'True' — the position where GHC has to solve it, and so the position
-- where it fires at the author's use site.
type RecursiveFieldError (n :: Symbol) =
  TypeError
    ( 'Text "Cannot derive a finite form for recursive field `" ':<>: 'Text n ':<>: 'Text "`."
        ':$$: 'Text "Recursive and repeated forms are not supported in v1."
    )
