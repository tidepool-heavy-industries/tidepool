{-# LANGUAGE MagicHash #-}
module ImportConsumer where

import GHC.Exts (Int#)
import ImportProducer (producerFn, producerValue)

-- | Applies the imported function. Exact application of an imported closure
-- now resolves through the owning program (X2): a foreign top called with
-- exactly its own arity links and runs through the real cross-program call
-- path, the same mechanism 'consumerResultAt' below exercises from a
-- projectable entry. (Foreign PAP, partial, or excess application is not
-- covered here -- that stays the typed reusable 'UnresolvedCallee' until
-- phase 2.) The Haskell projection test
-- ('ExecutionProjectionTest.verifyRetainedImportProjection') also projects
-- this as its entry to prove both imports become 'GlobalDecl's.
consumerResult :: Int
consumerResult = producerFn (length producerValue)

-- | An 'Int#'-argument entry with 'consumerResult''s value, shaped like
-- 'consumerValueAt' so it can be projected and run the same way.
--
-- Projected as its OWN separate target/fixture
-- ('fixtures/import-consumer-result.cbor'), never combined with
-- 'consumerEntries' into one artifact: 'consumerResult''s body calls the
-- imported 'producerFn' directly by name (@'ValueRef::Global'@), and
-- admission's whole-program check
-- (@tidepool-codegen/src/prepared_program/admission.rs@'s @ExprFrame::Call@
-- arm) has no case at all for a 'Global' callee -- only
-- @'ValueRef::Local'@ is matched; every other callee shape, 'Global'
-- included, falls through to the wildcard rejection. So a program whose
-- reachable closure contains ANY direct call to an imported function
-- fails to install AT ALL (\"admission is whole-program\": one
-- unsupported node anywhere rejects the whole artifact), regardless of
-- whether X2's runtime dispatch could serve the call. Confirmed against
-- this real fixture, not just a synthetic repro: installing THIS binder
-- alone reproduces the rejection
-- ('tidepool-runtime/tests/prepared_execution.rs's
-- @s6_direct_global_call_is_not_yet_admitted@). Keeping it out of the
-- pinned 'consumerEntries' artifact means the gap does not also break
-- 'consumerValueAt'/'consumerEntries', which install and run correctly
-- today.
consumerResultAt :: Int# -> Int
consumerResultAt _ = consumerResult
{-# NOINLINE consumerResultAt #-}

-- | Data-only use of a retained import: returns 'producerValue' without
-- applying 'producerFn' or scrutinising the list, so the consumer's own
-- generated code only loads the import's slot and returns it. A function of
-- an unboxed argument, as 'FreerResume.resumeInt' is: a plain alias is a
-- trivial binding GHC substitutes away even under NOINLINE, and a top-level
-- constructor holding the import is static data with a field known only at
-- install, which the compiled-program image does not admit yet (follow-up
-- S3b in the Wave 6B plan).
consumerValueAt :: Int# -> [Int]
consumerValueAt _ = producerValue
{-# NOINLINE consumerValueAt #-}

-- | Pinned projection target: pulls 'consumerValueAt' into the closure and
-- holds the imported function as a constructor field, never applying it,
-- so a function-typed import flows through binding, linking and install.
-- The list field is left lazy on purpose: forcing it here (`seq`) compiles
-- to an algebraic Case on the imported list, and Case dispatch cannot yet
-- recognise a foreign program's constructor (S3 finding). That finding was
-- about 'compile_for_install''s single-heap admission path; this fixture is
-- projected standalone by the extractor (@--target@, not
-- @compile_for_install@), so nothing here demonstrates the gap is closed --
-- the field stays lazy so the fixture keeps testing the data-only path it
-- was built for.
consumerEntries :: Int# -> ([Int], Int -> Int)
consumerEntries n = (consumerValueAt n, producerFn)
{-# NOINLINE consumerEntries #-}
