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
--   'VisitedCheck' is therefore a correctness mechanism, not a
--   diagnostics-quality nicety — without it a self-referential type builds
--   an infinite shape when forced.
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
  , VisitedCheck
  , Visited
  ) where

import Data.Kind (Constraint, Type)
import GHC.Generics (Meta (..))
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
-- offering a correction. The fall-through case is empty, so a supported
-- field costs nothing.
--
-- Order matters: @[Char]@ must precede @[a]@ so a @String@ field gets the
-- Text advice rather than the generic list advice.
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
  FieldCheck n (Maybe (Maybe a)) =
    TypeError
      ( 'Text "`" ':<>: 'Text n ':<>: 'Text "` has nested optionality."
          ':$$: 'Text "Use one Maybe layer, or define an explicit sum with domain-named constructors."
      )
  FieldCheck n a = ()

-- | The set of datatypes already entered on the current derivation path.
type Visited = [Type]

-- | Reject a type that contains itself, naming the field that closes the
-- cycle.
--
-- This is load-bearing for CORRECTNESS, not just for message quality. A
-- recursive type such as
--
-- > data Tree = Leaf | Node { left :: Tree, right :: Tree }
--
-- satisfies every instance GHC looks for — the recursion lives in the values,
-- not the dictionaries — so it compiles cleanly and then builds an infinite
-- shape the moment anything forces it. The interpreter threads a 'Visited'
-- list down each field and each sum branch, and this family fires when a type
-- reappears on its own path.
type family VisitedCheck (n :: Symbol) (a :: Type) (seen :: Visited) :: Constraint where
  VisitedCheck n a '[] = ()
  VisitedCheck n a (a ': rest) =
    TypeError
      ( 'Text "Cannot derive a finite form for recursive field `" ':<>: 'Text n ':<>: 'Text "`."
          ':$$: 'Text "Recursive and repeated forms are not supported in v1."
      )
  VisitedCheck n a (b ': rest) = VisitedCheck n a rest
