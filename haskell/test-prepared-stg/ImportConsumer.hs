{-# LANGUAGE MagicHash #-}
module ImportConsumer where

import GHC.Exts (Int#)
import ImportProducer (producerFn, producerValue)

-- | Applies the imported function. The Haskell projection test
-- ('ExecutionProjectionTest.verifyRetainedImportProjection') projects this
-- as its entry to prove both imports become 'GlobalDecl's. It is NOT in the
-- pinned 'fixtures/import-consumer.cbor' closure: calling an imported
-- closure from generated code has no mechanism this wave, and admission is
-- whole-program, so a closure containing this body cannot install.
consumerResult :: Int
consumerResult = producerFn (length producerValue)

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
-- recognise a foreign program's constructor (S3 finding), even for a
-- default-only alternative.
consumerEntries :: Int# -> ([Int], Int -> Int)
consumerEntries n = (consumerValueAt n, producerFn)
{-# NOINLINE consumerEntries #-}
