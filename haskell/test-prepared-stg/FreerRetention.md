# Freer retention boundary

`freerRequest` is a real `freer-simple` `send` program. Its returned `E`
constructor contains a function continuation. The retained-result tests inspect
that outer constructor and keep the continuation as an opaque managed handle.
They do not recursively force it or add a second effect interpreter.

## Regeneration

From `haskell/` in the repository Nix environment, use a fresh output directory:

```sh
cabal test execution-corpus-projection --test-options='test-prepared-stg/FreerRetention.hs FreerRetention test-prepared-stg/FreerRetentionTargets /tmp/freer-retention lib'
```

Verify that `manifest.json` has exactly one projected row with identity
`main:FreerRetention:value:freerRequest` and artifact `0.prepared.cbor`, then
copy that artifact to `test-prepared-stg/fixtures/freer-retention.cbor`.
The manifest check distinguishes successful projection from the probe merely
finishing with a rejection record.
