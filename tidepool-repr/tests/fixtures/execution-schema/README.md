# M3 execution-schema fixtures

The exact encoding is
`plans/parallel-dogfood/next-wave/m3-wire-contract-r7.md`.
`haskell/test-prepared-stg/M3Vertical.hs` is the producer fixture; its checked
schema-v2 output is `haskell/test-prepared-stg/fixtures/m3-vertical.cbor`, generated
with `cd haskell && cabal run execution-schema-projection -- test-prepared-stg/fixtures/m3-vertical.cbor`.
It retains an
imported value, a recursive local control path and a strict constructor field.
The projection test must inspect the prepared form before claiming those shapes.

The decoder/linker tests derive malformed artifacts from the encoded valid
fixture, one mutation per case: stale schema, target mismatch, truncated bytes,
trailing bytes, invalid dense-ID kind, out-of-scope reference, duplicate
definition, signature/layout mismatch and missing/import-contract mismatch.
Failed parse/link must publish no executable program or changed import snapshot;
a following valid resident request must still succeed.
