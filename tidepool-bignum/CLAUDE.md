# tidepool-bignum — Integer/Natural ↔ Double/Float encoding

**Charter.** Belongs: `Integer`/`Natural` ↔ `Double`/`Float` encoding,
decoding, and `Show` formatting shared bit-for-bit by the eval oracle and the
JIT, plus the native ghc-bignum FFI shims (`__int_encodeDouble` et al.) that
survive with the native ghc-bignum backend. Does NOT belong: general
`PrimOpKind` numeric dispatch (`tidepool-eval`/`tidepool-codegen` call into
this crate for the shared policy, not the other way round).
