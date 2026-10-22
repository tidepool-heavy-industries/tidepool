# Frozen package transport fixture

This directory is the narrow exception to the generated-artifact rule for one
immutable recovery-reader regression. The four `TPPACKAGEGROUP1` envelopes and
two defining interfaces are historical compiler output. They are not current
compiler issuance, and they do not qualify a new package-selection path.

`manifest.json` is a validated, versioned ownership contract. It binds:

- the exact durable capture receipt and its SHA-256;
- explicit unknowns for source revision and generation command;
- the accepted wire, prepared-schema, ABI, projection-profile and toolchain
  versions;
- each interface and envelope content identity, byte length and group digest;
- the test purpose and removal rule.

The reader test materializes these exact bytes, restores the product through the
production recovery reader, and checks decoded owner, interface, envelope and
product identity. Current package selection must use a declared fixture producer
and the current completed-output receipt reader; this fixture cannot substitute
for that evidence.

Remove the fixture only after a current producer supplies a discriminating
replacement and that replacement is accepted. Never rewrite or restamp these
bytes to match a later compiler.
