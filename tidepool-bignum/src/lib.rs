//! Integer/Natural → `Double` encoding helpers used by the JIT and tree-walker.
//!
//! With the native ghc-bignum backend, `Integer`/`Natural` arithmetic is pure
//! Core over `Word#`/`ByteArray#` primops — no `__gmpn_*`/`integer_gmp_*` FFI —
//! so the JIT compiles it directly. The only ghc-bignum FFI that survives is the
//! RTS `__int_encodeDouble`/`__word_encodeDouble` (`mantissa * 2^exp`), which
//! both consumers route here.

/// `__int_encodeDouble(mantissa, exp)`: the correctly-rounded value of
/// `mantissa * 2^exp` as a `double` (GHC's `intEncodeDouble#`, an `ldexp`).
/// Scaling by a power of two is exact, so we round `mantissa` to `f64` once and
/// shift the exponent in same-sign chunks (each factor stays finite), which
/// reproduces `ldexp` over/underflow (→ ±inf / 0) without premature overflow.
pub fn encode_double(mantissa: i64, exp: i64) -> f64 {
    scale_pow2(mantissa as f64, exp)
}

/// `__word_encodeDouble(mantissa, exp)`: as `encode_double` but with an UNSIGNED
/// mantissa (GHC's `wordEncodeDouble#`; the native bignum backend's
/// `bigNatToDouble#` uses this since the magnitude is a `Word#`).
pub fn encode_double_word(mantissa: u64, exp: i64) -> f64 {
    scale_pow2(mantissa as f64, exp)
}

/// `x * 2^e`, scaling the exponent in same-sign chunks so each factor stays
/// finite — reproduces `ldexp` over/underflow (→ ±inf / 0) without premature
/// overflow. Power-of-two scaling is exact, so the only rounding is `x`'s.
///
/// `e` is clamped to ±2200 first: the finite `f64` range spans 2^-1074 ..=
/// ~2^1024, so |e| ≥ 2200 already saturates to ±inf / ±0 for every finite `x`.
/// The clamp bounds the chunked loops to O(1) — the exponent arrives straight
/// from a user-supplied `Int#` (`encodeFloat`/`encodeDouble#`), and a value
/// near `i64::MAX` would otherwise iterate ~9e15 times.
fn scale_pow2(mut x: f64, e: i64) -> f64 {
    let mut e = e.clamp(-2200, 2200);
    while e > 1000 {
        x *= 2f64.powi(1000);
        e -= 1000;
    }
    while e < -1000 {
        x *= 2f64.powi(-1000);
        e += 1000;
    }
    x * 2f64.powi(e as i32)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encode_double_basic_and_range() {
        assert_eq!(encode_double(1, 0), 1.0);
        assert_eq!(encode_double(3, 2), 12.0);
        assert_eq!(encode_double(-5, 3), -40.0);
        assert_eq!(encode_double(1, 1023), 2f64.powi(1023));
        assert_eq!(encode_double(1, 2000), f64::INFINITY);
        assert_eq!(encode_double(1, -2000), 0.0);
        // Largest finite is < 2^1024; 5 * 2^1000 ~= 5.4e301 stays finite.
        assert!(encode_double(5, 1000).is_finite());
        // Mantissa wider than 53 bits rounds to nearest f64 (2^53 + 1 -> 2^53).
        assert_eq!(encode_double((1i64 << 53) + 1, 0), (1u64 << 53) as f64);
    }

    #[test]
    fn encode_double_extreme_exponents_saturate_promptly() {
        // A legal Int-typed exponent can be any i64; these must saturate in
        // O(1), not iterate |e|/1000 times.
        assert_eq!(encode_double(1, i64::MAX), f64::INFINITY);
        assert_eq!(encode_double(-1, i64::MAX), f64::NEG_INFINITY);
        assert_eq!(encode_double(1, i64::MIN), 0.0);
        assert_eq!(encode_double(0, i64::MAX), 0.0);
        assert_eq!(encode_double_word(u64::MAX, i64::MIN), 0.0);
        assert_eq!(encode_double_word(u64::MAX, i64::MAX), f64::INFINITY);
    }

    #[test]
    fn encode_double_word_unsigned() {
        // High-bit-set mantissa must be treated as unsigned (not negative).
        assert_eq!(encode_double_word(1u64 << 63, 0), 2f64.powi(63));
        assert_eq!(encode_double_word(3, 4), 48.0);
    }
}
