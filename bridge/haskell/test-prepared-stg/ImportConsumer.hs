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

-- | Direct use of a retained function import. The dynamic production
-- producer/retain/consumer test keeps this target separate so its admission
-- refusal cannot hide the successful retained data control.
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
