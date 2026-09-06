# Numeric lane scaffold

The six FfiIs{Float,Double}{NaN,Infinite,NegativeZero} PrimOpKind variants
are named-wire additions, each accepting one matching width operand and returning
Int# 0 or 1. NaN includes quiet/signalling NaN without payload preservation.
NegativeZero is exactly the sign-bit-only encoding. Infinite excludes NaN.

Native Nix GHC 9.12.2 GHC.Internal.Float interface confirms all six static
symbols and the Core ABI: Float#/Double# -> State# RealWorld ->
(# State# RealWorld, Int# #). Translate's existing stateful-single-result
lowering strips the state input/output; do not treat its arity as two after
that lowering. GHC's isNaN uses DEFAULT -> True, 0# -> False.

Structured recognition is available through idDetails / FCallId (CCall
(CCallSpec (StaticTarget _ label _ _) convention safety)); label is
CLabelString. Dynamic targets are unsupported, not substring matches.

This commit intentionally adds only the IR vocabulary; backends still reject
these operations through their existing unsupported-operation path until the
runtime worker implements them. No successful behavior is stubbed.

Ownership: numeric TL owns types.rs and integration. Lowering worker owns
Translate.hs and haskell tests, plus any necessary extractor cache/version
analysis (report migration decisions). Runtime worker owns numeric match arms
in eval.rs and emit/primop.rs, with focused tests inside eval.rs and a new
codegen test file registered inside an existing suite only if necessary;
coordinate test registration rather than rewriting another lane's work.
Strictness sibling owns primop unboxing helpers; leave them untouched.
Independent campaign validation owns broader generated/native/display tests.
