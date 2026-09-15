//! Integer/Natural ↔ `Double`/`Float` encoding, decoding, and `show` helpers
//! used by native primitive adapters and host presentation.
//!
//! With the native ghc-bignum backend, `Integer`/`Natural` arithmetic is pure
//! Core over `Word#`/`ByteArray#` primops — no `__gmpn_*`/`integer_gmp_*` FFI —
//! so the JIT compiles it directly. The only ghc-bignum FFI that survives is the
//! RTS `__int_encodeDouble`/`__word_encodeDouble` (`mantissa * 2^exp`), which
//! both consumers route here. `decodeDouble_Int64#`/`decodeFloat_Int#` (the
//! inverse direction) and Haskell's `Show Double` formatting are the same kind
//! of fact — a numeric policy both backends must agree on bit-for-bit — so they
//! live here too, one home instead of two hand-kept-in-sync copies.

#![warn(clippy::unwrap_used, clippy::expect_used)]
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

/// `decodeDouble_Int64#`: decompose a `Double` into `(mantissa, exponent)`
/// such that `mantissa * 2^exponent == d` for finite values, in GHC's raw IEEE
/// form. A nonzero exponent field, including infinity and NaN, uses the raw
/// 52-bit fraction plus the implicit leading bit. Subnormals are normalized to
/// a 53-bit significand with a correspondingly reduced exponent. GHC does not
/// remove trailing zeros or invent exceptional-value sentinels. This is the
/// shared numeric owner consumed by native primitive adapters.
pub fn decode_double_int64(d: f64) -> (i64, i64) {
    if d == 0.0 {
        return (0, 0);
    }
    let bits = d.to_bits();
    let sign: i64 = if bits >> 63 == 0 { 1 } else { -1 };
    let raw_exp = ((bits >> 52) & 0x7ff) as i32;
    let raw_man = (bits & 0x000f_ffff_ffff_ffff) as i64;
    let (man, exp) = if raw_exp == 0 {
        // Normalize subnormals to the same 53-bit significand as normal values.
        let shift = raw_man.leading_zeros() - 11;
        (raw_man << shift, 1 - 1023 - 52 - shift as i32)
    } else {
        // Every nonzero exponent field, including infinity and NaN, receives
        // the implicit leading bit in GHC's raw IEEE mapping.
        (raw_man | (1i64 << 52), raw_exp - 1023 - 52)
    };
    (sign * man, exp as i64)
}

/// `decodeFloat_Int#`: same shape as [`decode_double_int64`], but over
/// Float's own IEEE754 single layout (8-bit exponent field / 23-bit explicit
/// mantissa, bias 127). NOT reusable via widening to Double first: that would
/// decode into Double's wider mantissa/exponent and give a wrong answer for
/// Float.
pub fn decode_float_int(f: f32) -> (i64, i64) {
    if f == 0.0 || f.is_nan() {
        return (0, 0);
    }
    if f.is_infinite() {
        return (if f > 0.0 { 1 } else { -1 }, 0);
    }
    let bits = f.to_bits();
    let sign: i64 = if bits >> 31 == 0 { 1 } else { -1 };
    let raw_exp = ((bits >> 23) & 0xff) as i32;
    let raw_man = (bits & 0x007f_ffff) as i64;
    let (man, exp) = if raw_exp == 0 {
        // Normalize subnormals to the same 24-bit significand as normal values.
        let shift = raw_man.leading_zeros() - 40;
        (raw_man << shift, 1 - 127 - 23 - shift as i32)
    } else {
        // normal: implicit leading 1
        (raw_man | (1i64 << 23), raw_exp - 127 - 23)
    };
    (sign * man, exp as i64)
}

/// Format binary64 using GHC's strict rounding intervals. Decimal notation
/// is used for decimal exponents 0 through 7; both forms include a fraction.
pub fn haskell_show_double(d: f64) -> String {
    if d.is_nan() {
        return "NaN".to_owned();
    }
    if d.is_infinite() {
        return if d > 0.0 { "Infinity" } else { "-Infinity" }.to_owned();
    }
    if d == 0.0 {
        return if d.is_sign_negative() { "-0.0" } else { "0.0" }.to_owned();
    }
    let (digits, exponent) = decimal_digits(d.abs());
    let mut result = if d.is_sign_negative() { "-" } else { "" }.to_owned();
    if !(0..=7).contains(&exponent) {
        result.push(char::from(digits[0]));
        result.push('.');
        if digits.len() == 1 {
            result.push('0');
        } else {
            result.extend(digits[1..].iter().copied().map(char::from));
        }
        result.push('e');
        result.push_str(&(exponent - 1).to_string());
    } else {
        let split = exponent as usize;
        if split == 0 {
            result.push('0');
        }
        for index in 0..split {
            result.push(char::from(digits.get(index).copied().unwrap_or(b'0')));
        }
        result.push('.');
        if split >= digits.len() {
            result.push('0');
        } else {
            result.extend(digits[split..].iter().copied().map(char::from));
        }
    }
    result
}

/// Exact binary64 intervals avoid the midpoint tie choices made by Rust's
/// shortest formatter. GHC requires both interval endpoints to be excluded;
/// when both decimal candidates fit it rounds a decimal tie upward.
/// Contract: GHC 9.12.2, GHC.Internal.Float.floatToDigits / formatRealFloat.
/// <https://github.com/ghc/ghc/blob/ghc-9.12.2-release/libraries/ghc-internal/src/GHC/Internal/Float.hs>
fn decimal_digits(value: f64) -> (Vec<u8>, i32) {
    use num_bigint::BigUint;

    let bits = value.to_bits();
    let raw_exponent = ((bits >> 52) & 0x7ff) as i32;
    let fraction = bits & ((1_u64 << 52) - 1);
    let mantissa = if raw_exponent == 0 {
        fraction
    } else {
        fraction | (1_u64 << 52)
    };
    let binary_exponent = raw_exponent.max(1) - 1023 - 52;
    // At normal powers of two the previous number is half as far away.
    let asymmetric = raw_exponent > 1 && fraction == 0;
    let mut numerator = BigUint::from(mantissa) << 2_usize;
    let mut denominator = BigUint::from(4_u8);
    let mut upper = BigUint::from(2_u8);
    let mut lower = BigUint::from(if asymmetric { 1_u8 } else { 2_u8 });
    if binary_exponent >= 0 {
        numerator <<= binary_exponent as usize;
        upper <<= binary_exponent as usize;
        lower <<= binary_exponent as usize;
    } else {
        denominator <<= (-binary_exponent) as usize;
    }
    // Use a floating estimate only to choose a starting scale. Exact integer
    // comparisons correct it, including an upper endpoint exactly at 10^k.
    let upper_bound = &numerator + &upper;
    let contains_upper = |exponent: i32| {
        let power = BigUint::from(10_u8).pow(exponent.unsigned_abs());
        if exponent >= 0 {
            upper_bound <= &denominator * power
        } else {
            &upper_bound * power <= denominator
        }
    };
    let mut exponent = value.log10().floor() as i32 + 1;
    while !contains_upper(exponent) {
        exponent += 1;
    }
    while contains_upper(exponent - 1) {
        exponent -= 1;
    }
    let power = BigUint::from(10_u8).pow(exponent.unsigned_abs());
    if exponent >= 0 {
        denominator *= power;
    } else {
        numerator *= &power;
        upper *= &power;
        lower *= power;
    }
    let mut digits = Vec::with_capacity(17);
    loop {
        numerator *= 10_u8;
        upper *= 10_u8;
        lower *= 10_u8;
        // The scale bounds the quotient to one digit; repeated subtraction
        // avoids a general bignum division for this small quotient.
        let mut digit = 0;
        while numerator >= denominator {
            numerator -= &denominator;
            digit += 1;
        }
        let down = numerator < lower;
        let up = &numerator + &upper > denominator;
        if up && (!down || (&numerator << 1_usize) >= denominator) {
            digit += 1;
        }
        digits.push(b'0' + digit);
        if down || up {
            return (digits, exponent);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn haskell_show_double_excludes_decimal_rounding_midpoints() {
        assert_eq!(haskell_show_double(1e23), "9.999999999999999e22");
        assert_eq!(haskell_show_double(-1e23), "-9.999999999999999e22");
    }

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

    #[test]
    fn decode_float_int_basic() {
        assert_eq!(decode_float_int(1.0), (8388608, -23));
        assert_eq!(decode_float_int(3.0), (12582912, -22));
        assert_eq!(decode_float_int(16777216.0), (8388608, 1));
        assert_eq!(decode_float_int(-3.0), (-12582912, -22));
        assert_eq!(decode_float_int(0.0), (0, 0));
    }

    #[test]
    fn decode_float_int_subnormal() {
        // Native GHC 9.12.2 decodeFloat retains a full-width significand.
        for (bits, expected) in [
            (1, (8388608, -172)),
            (2, (8388608, -171)),
            (3, (12582912, -171)),
            (257, (8421376, -164)),
            (0x00400000, (8388608, -150)),
            (0x007fffff, (16777214, -150)),
            (0x00800000, (8388608, -149)),
        ] {
            assert_eq!(decode_float_int(f32::from_bits(bits)), expected);
            assert_eq!(
                decode_float_int(-f32::from_bits(bits)),
                (-expected.0, expected.1)
            );
        }
        assert_eq!(decode_float_int(-0.0), (0, 0));
    }

    #[test]
    fn decode_double_int64_basic() {
        assert_eq!(decode_double_int64(1.0), (4503599627370496, -52));
        assert_eq!(decode_double_int64(3.0), (6755399441055744, -51));
        assert_eq!(
            decode_double_int64(9007199254740992.0),
            (4503599627370496, 1)
        );
        assert_eq!(decode_double_int64(-3.0), (-6755399441055744, -51));
        assert_eq!(decode_double_int64(0.0), (0, 0));
    }

    #[test]
    fn decode_double_int64_subnormal() {
        for (bits, expected) in [
            (1, (4503599627370496, -1126)),
            (2, (4503599627370496, -1125)),
            (3, (6755399441055744, -1125)),
            (257, (4521191813414912, -1118)),
            (0x0008000000000000, (4503599627370496, -1075)),
            (0x000fffffffffffff, (9007199254740990, -1075)),
            (0x0010000000000000, (4503599627370496, -1074)),
        ] {
            assert_eq!(decode_double_int64(f64::from_bits(bits)), expected);
            assert_eq!(
                decode_double_int64(-f64::from_bits(bits)),
                (-expected.0, expected.1)
            );
        }
        assert_eq!(decode_double_int64(-0.0), (0, 0));
    }

    #[test]
    fn decode_double_int64_matches_pinned_ghc_ieee_patterns() {
        for (bits, expected) in [
            (0x0000_0000_0000_0000, (0, 0)),
            (0x8000_0000_0000_0000, (0, 0)),
            (0x3ff0_0000_0000_0000, (1_i64 << 52, -52)),
            (0xbff0_0000_0000_0000, (-(1_i64 << 52), -52)),
            (0x0000_0000_0000_0001, (1_i64 << 52, -1126)),
            (0x7fef_ffff_ffff_ffff, ((1_i64 << 53) - 1, 971)),
            (0x7ff0_0000_0000_0000, (1_i64 << 52, 972)),
            (0xfff0_0000_0000_0000, (-(1_i64 << 52), 972)),
            (0x7ff8_0000_0000_0001, (0x0018_0000_0000_0001, 972)),
            (0xfff8_0000_0000_0abc, (-0x0018_0000_0000_0abc, 972)),
        ] {
            assert_eq!(decode_double_int64(f64::from_bits(bits)), expected);
        }
    }

    #[test]
    fn haskell_show_double_decimal_range() {
        assert_eq!(haskell_show_double(1.0), "1.0");
        assert_eq!(haskell_show_double(-1.0), "-1.0");
        assert_eq!(haskell_show_double(0.0), "0.0");
        assert_eq!(haskell_show_double(-0.0), "-0.0");
        assert_eq!(haskell_show_double(f64::NAN), "NaN");
        assert_eq!(haskell_show_double(f64::INFINITY), "Infinity");
        assert_eq!(haskell_show_double(f64::NEG_INFINITY), "-Infinity");
    }

    #[test]
    fn haskell_show_double_scientific_always_has_decimal_point() {
        assert_eq!(haskell_show_double(1e10), "1.0e10");
        assert_eq!(haskell_show_double(-1e10), "-1.0e10");
        assert_eq!(haskell_show_double(f64::from_bits(1)), "5.0e-324");
        assert_eq!(haskell_show_double(2e8), "2.0e8");
        assert_eq!(haskell_show_double(1e100), "1.0e100");
    }
}
