//! GMP `mpn`-level multi-precision arithmetic, reimplemented over `num-bigint`.
//!
//! GHC's `ghc-bignum` (gmp backend) implements `Integer`/`Natural` operations by
//! calling into GMP's `mpn` (multi-precision natural, limb-level) C ABI, plus a
//! handful of GHC C-glue wrappers (`integer_gmp_*`). The Tidepool JIT can't link
//! GMP, so it intercepts those FFI symbols and routes them here. This crate is the
//! single source of truth for the arithmetic: the JIT (`tidepool-codegen`,
//! raw-pointer `extern "C"` shims) and the tree-walker (`tidepool-eval`,
//! `ByteArray#`-value shims) both call these functions, so the two engines agree
//! by construction (differential-oracle requirement).
//!
//! ## Limb model
//! A multi-precision natural is a little-endian array of 64-bit limbs
//! (`GmpLimb# = Word#`, 64-bit on the supported platforms). Inputs are `&[u64]`
//! slices whose length is the GMP "significant limb count" (`GmpSize#`). Outputs
//! are `&mut [u64]` slices that the **caller** has pre-sized to exactly the count
//! the corresponding C function writes (see each function's contract); every
//! function fills its entire output slice, zero-padding above the value's
//! significant limbs — because `newByteArray#` does NOT zero memory, partial
//! fills would leak garbage limbs into the result.
//!
//! ## Contracts
//! Each function documents the GMP contract it emulates: the exact number of limbs
//! written and the scalar return (carry/borrow/top-limb/remainder/size). These are
//! the high-risk surface; the unit tests pin them directly over raw limb arrays.

use num_bigint::BigUint;
use num_integer::Integer;
use num_traits::{ToPrimitive, Zero};

/// Decode a little-endian u64-limb slice into a `BigUint`.
#[inline]
fn from_limbs(s: &[u64]) -> BigUint {
    let mut bytes = Vec::with_capacity(s.len() * 8);
    for &l in s {
        bytes.extend_from_slice(&l.to_le_bytes());
    }
    BigUint::from_bytes_le(&bytes)
}

/// Write `v`'s little-endian u64 limbs into `dst`, zero-padding the high limbs.
/// Returns the number of significant limbs of `v` (0 if `v == 0`).
///
/// Debug-asserts that `v` fits in `dst` (a true contract violation otherwise).
#[inline]
fn write_limbs(dst: &mut [u64], v: &BigUint) -> usize {
    let digits: Vec<u64> = v.iter_u64_digits().collect();
    debug_assert!(
        digits.len() <= dst.len(),
        "tidepool-bignum: value ({} limbs) exceeds destination capacity ({} limbs)",
        digits.len(),
        dst.len()
    );
    for (i, slot) in dst.iter_mut().enumerate() {
        *slot = digits.get(i).copied().unwrap_or(0);
    }
    digits.len()
}

/// Fill `dst` with the low `dst.len()` limbs of `v` and return the next-higher
/// limb (the overflow/carry word), or 0 if `v` has no such limb.
#[inline]
fn spread_with_carry(dst: &mut [u64], v: &BigUint) -> u64 {
    let digits: Vec<u64> = v.iter_u64_digits().collect();
    for (i, slot) in dst.iter_mut().enumerate() {
        *slot = digits.get(i).copied().unwrap_or(0);
    }
    digits.get(dst.len()).copied().unwrap_or(0)
}

/// Low limb of a `BigUint` (0 if zero). Used for single-limb remainders.
#[inline]
fn low_limb(v: &BigUint) -> u64 {
    v.iter_u64_digits().next().unwrap_or(0)
}

// ────────────────────────── add / sub / mul by a single limb ──────────────────

/// `__gmpn_add_1(rp, s1, n, s2limb)`: `rp = (s1 + s2limb) mod 2^(64n)`,
/// returns the carry-out (0 or 1). Contract: `rp.len() == s1.len() == n`.
pub fn mpn_add_1(rp: &mut [u64], s1: &[u64], s2limb: u64) -> u64 {
    debug_assert_eq!(rp.len(), s1.len());
    let v = from_limbs(s1) + BigUint::from(s2limb);
    spread_with_carry(rp, &v)
}

/// `__gmpn_sub_1(rp, s1, n, s2limb)`: `rp = (s1 - s2limb) mod 2^(64n)`,
/// returns the borrow (1 if `s1 < s2limb`, else 0). Contract: `rp.len() == s1.len() == n`.
pub fn mpn_sub_1(rp: &mut [u64], s1: &[u64], s2limb: u64) -> u64 {
    debug_assert_eq!(rp.len(), s1.len());
    let a = from_limbs(s1);
    let b = BigUint::from(s2limb);
    if a >= b {
        write_limbs(rp, &(a - b));
        0
    } else {
        let modulus = BigUint::from(1u8) << (64 * s1.len());
        write_limbs(rp, &(modulus + a - b));
        1
    }
}

/// `__gmpn_mul_1(rp, s1, n, s2limb)`: `rp = (s1 * s2limb) mod 2^(64n)`,
/// returns the high carry limb. Contract: `rp.len() == s1.len() == n`.
pub fn mpn_mul_1(rp: &mut [u64], s1: &[u64], s2limb: u64) -> u64 {
    debug_assert_eq!(rp.len(), s1.len());
    let v = from_limbs(s1) * BigUint::from(s2limb);
    spread_with_carry(rp, &v)
}

// ────────────────────────── add / sub / mul (n-limb) ──────────────────────────

/// `__gmpn_add(rp, s1, s1n, s2, s2n)` (requires `s1n >= s2n`):
/// `rp = (s1 + s2) mod 2^(64*s1n)`, returns the carry-out (0 or 1).
/// Contract: `rp.len() == s1.len() >= s2.len()`.
pub fn mpn_add(rp: &mut [u64], s1: &[u64], s2: &[u64]) -> u64 {
    debug_assert_eq!(rp.len(), s1.len());
    debug_assert!(s1.len() >= s2.len());
    let v = from_limbs(s1) + from_limbs(s2);
    spread_with_carry(rp, &v)
}

/// `__gmpn_sub(rp, s1, s1n, s2, s2n)` (requires `s1n >= s2n`):
/// `rp = (s1 - s2) mod 2^(64*s1n)`, returns the borrow (0 when `s1 >= s2`).
/// Contract: `rp.len() == s1.len() >= s2.len()`.
pub fn mpn_sub(rp: &mut [u64], s1: &[u64], s2: &[u64]) -> u64 {
    debug_assert_eq!(rp.len(), s1.len());
    debug_assert!(s1.len() >= s2.len());
    let a = from_limbs(s1);
    let b = from_limbs(s2);
    if a >= b {
        write_limbs(rp, &(a - b));
        0
    } else {
        let modulus = BigUint::from(1u8) << (64 * s1.len());
        write_limbs(rp, &(modulus + a - b));
        1
    }
}

/// `__gmpn_mul(rp, s1, s1n, s2, s2n)`: `rp = s1 * s2`, written as EXACTLY
/// `s1n + s2n` limbs; returns the most-significant limb (`rp[s1n+s2n-1]`, may be 0).
/// Contract: `rp.len() == s1.len() + s2.len()`.
pub fn mpn_mul(rp: &mut [u64], s1: &[u64], s2: &[u64]) -> u64 {
    debug_assert_eq!(rp.len(), s1.len() + s2.len());
    let v = from_limbs(s1) * from_limbs(s2);
    write_limbs(rp, &v);
    rp.last().copied().unwrap_or(0)
}

// ────────────────────────── compare ──────────────────────────

/// `__gmpn_cmp(s1, s2, n)`: compares two equal-length naturals.
/// Returns -1 / 0 / 1 for `s1 < / == / > s2`.
pub fn mpn_cmp(s1: &[u64], s2: &[u64]) -> i32 {
    use core::cmp::Ordering::{Equal, Greater, Less};
    match from_limbs(s1).cmp(&from_limbs(s2)) {
        Less => -1,
        Equal => 0,
        Greater => 1,
    }
}

// ────────────────────────── division ──────────────────────────

/// `__gmpn_tdiv_qr(qp, rp, 0, np, nn, dp, dn)`: truncating division.
/// Writes `nn - dn + 1` quotient limbs to `qp` and `dn` remainder limbs to `rp`.
/// Contract: `qp.len() == nn - dn + 1`, `rp.len() == dn`. `dp` must be non-zero.
pub fn mpn_tdiv_qr(qp: &mut [u64], rp: &mut [u64], np: &[u64], dp: &[u64]) {
    let (q, r) = from_limbs(np).div_rem(&from_limbs(dp));
    write_limbs(qp, &q);
    write_limbs(rp, &r);
}

/// `integer_gmp_mpn_tdiv_q(qp, np, nn, dp, dn)`: quotient only.
/// Contract: `qp.len() == nn - dn + 1`. `dp` must be non-zero.
pub fn mpn_tdiv_q(qp: &mut [u64], np: &[u64], dp: &[u64]) {
    write_limbs(qp, &(from_limbs(np) / from_limbs(dp)));
}

/// `integer_gmp_mpn_tdiv_r(rp, np, nn, dp, dn)`: remainder only.
/// Contract: `rp.len() == dn`. `dp` must be non-zero.
pub fn mpn_tdiv_r(rp: &mut [u64], np: &[u64], dp: &[u64]) {
    write_limbs(rp, &(from_limbs(np) % from_limbs(dp)));
}

/// `__gmpn_divrem_1(qp, qxn, np, nn, dlimb)`: divide `{np,nn}` (shifted up by
/// `qxn` fraction limbs) by the single limb `dlimb`; quotient to `qp`, returns
/// the remainder. Contract: `qp.len() == nn + qxn`. `dlimb` must be non-zero.
/// (ghc-bignum always passes `qxn == 0` — integer quotient/remainder by a word.)
pub fn mpn_divrem_1(qp: &mut [u64], qxn: usize, np: &[u64], dlimb: u64) -> u64 {
    debug_assert_eq!(qp.len(), np.len() + qxn);
    let n = from_limbs(np) << (64 * qxn);
    let (q, r) = n.div_rem(&BigUint::from(dlimb));
    write_limbs(qp, &q);
    low_limb(&r)
}

/// `__gmpn_mod_1(np, nn, dlimb)`: returns `{np,nn} mod dlimb`.
/// `dlimb` must be non-zero.
pub fn mpn_mod_1(np: &[u64], dlimb: u64) -> u64 {
    low_limb(&(from_limbs(np) % BigUint::from(dlimb)))
}

// ────────────────────────── Integer -> Double ──────────────────────────

/// `integer_gmp_mpn_get_d(sp, sn, exp)`: value of `{sp,|sn|} * 2^exp` as a
/// `double`, with sign taken from `sn` (negative `sn` => negative result).
/// The common caller (`fromIntegral :: Integer -> Double`) passes `exp == 0`,
/// which is exact (round-to-nearest of the big magnitude). For `exp != 0` the
/// magnitude is scaled; non-negative `exp` is folded exactly, negative `exp`
/// uses a single `f64` multiply.
pub fn mpn_get_d(sp: &[u64], sn: i64, exp: i64) -> f64 {
    let n = (sn.unsigned_abs() as usize).min(sp.len());
    let v = from_limbs(&sp[..n]);
    let mag = if exp >= 0 {
        // Fold the positive exponent into the integer for an exact rounding.
        (v << (exp as usize)).to_f64().unwrap_or(f64::INFINITY)
    } else {
        v.to_f64().unwrap_or(f64::INFINITY) * 2f64.powi(exp as i32)
    };
    if sn < 0 {
        -mag
    } else {
        mag
    }
}

// ────────────────────────── gcd ──────────────────────────

/// `integer_gmp_gcd_word(a, b)`: gcd of two words.
pub fn gcd_word(mut a: u64, mut b: u64) -> u64 {
    while b != 0 {
        let t = a % b;
        a = b;
        b = t;
    }
    a
}

/// `integer_gmp_mpn_gcd_1(sp, sn, b)`: gcd of `{sp,sn}` and the word `b`
/// (result fits in a word). `b` must be non-zero.
pub fn mpn_gcd_1(sp: &[u64], b: u64) -> u64 {
    let r = low_limb(&(from_limbs(sp) % BigUint::from(b)));
    gcd_word(r, b)
}

/// `integer_gmp_mpn_gcd(rp, s1, s1n, s2, s2n)`: gcd of two naturals into `rp`,
/// returns the significant limb count of the result. Contract: `rp.len()` is at
/// least `min(s1n, s2n)` (the gcd cannot exceed the smaller operand).
pub fn mpn_gcd(rp: &mut [u64], s1: &[u64], s2: &[u64]) -> usize {
    let a = from_limbs(s1);
    let b = from_limbs(s2);
    let g = if a.is_zero() {
        b
    } else if b.is_zero() {
        a
    } else {
        a.gcd(&b)
    };
    write_limbs(rp, &g)
}

// ────────────────────────── shifts ──────────────────────────

/// `integer_gmp_mpn_lshift(rp, sp, count)`: `rp = sp << count`. Returns the
/// most-significant limb of the result. Contract (matching the GHC C wrapper):
/// `rp.len() == sn + count/64 + (count%64 != 0 ? 1 : 0)`.
pub fn mpn_lshift(rp: &mut [u64], sp: &[u64], count: u64) -> u64 {
    let v = from_limbs(sp) << (count as usize);
    write_limbs(rp, &v);
    rp.last().copied().unwrap_or(0)
}

/// `integer_gmp_mpn_rshift(rp, sp, count)`: `rp = sp >> count` (logical).
/// Returns the most-significant limb of the result. Contract:
/// `rp.len() == sn - count/64` (ghc-bignum guarantees `count < sn*64`).
pub fn mpn_rshift(rp: &mut [u64], sp: &[u64], count: u64) -> u64 {
    let v = from_limbs(sp) >> (count as usize);
    write_limbs(rp, &v);
    rp.last().copied().unwrap_or(0)
}

/// `integer_gmp_mpn_rshift_2c(rp, sp, count)`: two's-complement (arithmetic)
/// right shift used for negative Integers — the magnitude rounds toward minus
/// infinity, i.e. `ceil(sp / 2^count)` (add 1 when any shifted-out bit was set).
/// Returns the top limb. Contract: `rp.len() == sn - (count-1)/64`.
pub fn mpn_rshift_2c(rp: &mut [u64], sp: &[u64], count: u64) -> u64 {
    let one = BigUint::from(1u8);
    let v = from_limbs(sp);
    let shifted = &v >> (count as usize);
    let low_mask = (&one << (count as usize)) - &one;
    let result = if (&v & &low_mask).is_zero() {
        shifted
    } else {
        shifted + &one
    };
    write_limbs(rp, &result);
    rp.last().copied().unwrap_or(0)
}

// ────────────────────────── encodeDouble ──────────────────────────

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
fn scale_pow2(mut x: f64, mut e: i64) -> f64 {
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

    const LO: u64 = u64::MAX; // all-ones limb, for carry/borrow edge tests

    // ── single-limb add/sub/mul: carry / borrow contracts ──

    #[test]
    fn add_1_no_carry() {
        let mut rp = [0u64; 2];
        let c = mpn_add_1(&mut rp, &[10, 20], 5);
        assert_eq!(rp, [15, 20]);
        assert_eq!(c, 0);
    }

    #[test]
    fn add_1_carry_propagates_and_returns() {
        // [MAX, MAX] + 1 = 0,0 with carry-out 1
        let mut rp = [0u64; 2];
        let c = mpn_add_1(&mut rp, &[LO, LO], 1);
        assert_eq!(rp, [0, 0]);
        assert_eq!(c, 1);
    }

    #[test]
    fn add_1_carry_into_high_limb_no_overflow() {
        // [MAX, 0] + 1 = [0, 1], carry 0
        let mut rp = [0u64; 2];
        let c = mpn_add_1(&mut rp, &[LO, 0], 1);
        assert_eq!(rp, [0, 1]);
        assert_eq!(c, 0);
    }

    #[test]
    fn sub_1_borrow() {
        // [0,1] - 1 = [MAX, 0], borrow 0
        let mut rp = [0u64; 2];
        let b = mpn_sub_1(&mut rp, &[0, 1], 1);
        assert_eq!(rp, [LO, 0]);
        assert_eq!(b, 0);
    }

    #[test]
    fn sub_1_underflow_sets_borrow() {
        // [0] - 1 wraps to [MAX], borrow 1
        let mut rp = [0u64; 1];
        let b = mpn_sub_1(&mut rp, &[0], 1);
        assert_eq!(rp, [LO]);
        assert_eq!(b, 1);
    }

    #[test]
    fn mul_1_full_word_carry() {
        // [MAX] * 2 = low MAX-1 (0xFFFF...E), carry 1
        let mut rp = [0u64; 1];
        let c = mpn_mul_1(&mut rp, &[LO], 2);
        assert_eq!(rp, [LO - 1]);
        assert_eq!(c, 1);
    }

    // ── n-limb add/sub: carry, borrow, zero-pad of the shorter operand ──

    #[test]
    fn add_n_unequal_lengths_zero_pads() {
        // [MAX, MAX] + [1] = [0, 0], carry 1
        let mut rp = [0u64; 2];
        let c = mpn_add(&mut rp, &[LO, LO], &[1]);
        assert_eq!(rp, [0, 0]);
        assert_eq!(c, 1);
    }

    #[test]
    fn sub_n_basic() {
        let mut rp = [0u64; 2];
        let b = mpn_sub(&mut rp, &[5, 7], &[9]); // [5,7]-9
        assert_eq!(rp, [u64::MAX - 3, 6]); // borrow within, no top borrow
        assert_eq!(b, 0);
    }

    // ── mul: writes exactly s1n+s2n limbs, returns top (possibly 0) ──

    #[test]
    fn mul_writes_full_width_top_limb_returned() {
        // 2^64 * 2^64 = 2^128 -> [0, 0, 1, 0] in 4 limbs (2+2); top limb is 0.
        let mut rp = [9u64; 4]; // pre-fill with junk to prove zero-padding
        let top = mpn_mul(&mut rp, &[0, 1], &[0, 1]);
        assert_eq!(rp, [0, 0, 1, 0]);
        assert_eq!(top, 0);
    }

    #[test]
    fn mul_top_limb_nonzero() {
        // [MAX] * [MAX] = 0xFFFF...FE_0000...01 over 2 limbs; top = MAX-1.
        let mut rp = [0u64; 2];
        let top = mpn_mul(&mut rp, &[LO], &[LO]);
        assert_eq!(rp, [1, LO - 1]);
        assert_eq!(top, LO - 1);
    }

    #[test]
    fn mul_value_correct_three_limb() {
        // (2^64 + 7) * 3 via n-limb mul against a known product.
        let a = [7u64, 1]; // 2^64 + 7
        let bnum = [3u64];
        let mut rp = [0u64; 3];
        mpn_mul(&mut rp, &a, &bnum);
        let got = from_limbs(&rp);
        let want = (BigUint::from(1u8) << 64) * 3u32 + 21u32;
        assert_eq!(got, want);
    }

    // ── cmp ──

    #[test]
    fn cmp_orders() {
        assert_eq!(mpn_cmp(&[1, 2], &[1, 2]), 0);
        assert_eq!(mpn_cmp(&[0, 2], &[9, 2]), -1);
        assert_eq!(mpn_cmp(&[9, 2], &[0, 2]), 1);
        assert_eq!(mpn_cmp(&[0, 3], &[9, 2]), 1); // high limb dominates
    }

    // ── division: quotient size nn-dn+1, remainder size dn, exact values ──

    #[test]
    fn tdiv_qr_sizes_and_values() {
        // np = 2^128 + 5 (3 limbs), dp = 2^64 (2 limbs). nn=3, dn=2.
        let np = [5u64, 0, 1];
        let dp = [0u64, 1];
        let mut qp = [0u64; 2]; // nn-dn+1 = 2
        let mut rp = [0u64; 2]; // dn = 2
        mpn_tdiv_qr(&mut qp, &mut rp, &np, &dp);
        // (2^128 + 5) / 2^64 = 2^64, remainder 5
        assert_eq!(from_limbs(&qp), BigUint::from(1u8) << 64);
        assert_eq!(from_limbs(&rp), BigUint::from(5u8));
    }

    #[test]
    fn tdiv_q_and_r_match_qr() {
        let np = [123456789u64, 42, 7];
        let dp = [99u64, 1];
        let mut q1 = [0u64; 2];
        let mut r1 = [0u64; 2];
        mpn_tdiv_qr(&mut q1, &mut r1, &np, &dp);
        let mut q2 = [0u64; 2];
        let mut r2 = [0u64; 2];
        mpn_tdiv_q(&mut q2, &np, &dp);
        mpn_tdiv_r(&mut r2, &np, &dp);
        assert_eq!(q1, q2);
        assert_eq!(r1, r2);
        // q*d + r == n
        let recomposed = from_limbs(&q1) * from_limbs(&dp) + from_limbs(&r1);
        assert_eq!(recomposed, from_limbs(&np));
    }

    #[test]
    fn divrem_1_qxn0() {
        // (2^64 + 7) / 5
        let np = [7u64, 1];
        let mut qp = [0u64; 2]; // nn + qxn = 2
        let r = mpn_divrem_1(&mut qp, 0, &np, 5);
        let n = (BigUint::from(1u8) << 64) + 7u32;
        assert_eq!(from_limbs(&qp), &n / 5u32);
        assert_eq!(r, low_limb(&(&n % 5u32)));
    }

    #[test]
    fn mod_1_basic() {
        let np = [7u64, 1]; // 2^64 + 7
        let n = (BigUint::from(1u8) << 64) + 7u32;
        assert_eq!(mpn_mod_1(&np, 5), low_limb(&(n % 5u32)));
    }

    // ── get_d ──

    #[test]
    fn get_d_exp0_exact_small() {
        assert_eq!(mpn_get_d(&[42], 1, 0), 42.0);
        assert_eq!(mpn_get_d(&[42], -1, 0), -42.0);
        assert_eq!(mpn_get_d(&[0], 0, 0), 0.0);
    }

    #[test]
    fn get_d_exp0_large() {
        // 2^64 as a double is exactly 1.8446744073709552e19
        let v = mpn_get_d(&[0, 1], 2, 0);
        assert_eq!(v, 18446744073709551616.0);
    }

    #[test]
    fn get_d_positive_exp_folds() {
        // value 3, exp 4 -> 48
        assert_eq!(mpn_get_d(&[3], 1, 4), 48.0);
    }

    // ── gcd ──

    #[test]
    fn gcd_word_basic() {
        assert_eq!(gcd_word(48, 36), 12);
        assert_eq!(gcd_word(17, 5), 1);
        assert_eq!(gcd_word(0, 9), 9);
    }

    #[test]
    fn mpn_gcd_1_basic() {
        // gcd(2^64, 6) = 2
        assert_eq!(mpn_gcd_1(&[0, 1], 6), 2);
    }

    #[test]
    fn mpn_gcd_value_and_size() {
        // gcd(2^65, 2^64 * 3) -> 2^64 (1 limb of value 0 then 1 -> 2 limbs significant)
        let s1 = [0u64, 2]; // 2^65
        let s2 = [0u64, 3]; // 3 * 2^64
        let mut rp = [0u64; 2];
        let len = mpn_gcd(&mut rp, &s1, &s2);
        assert_eq!(from_limbs(&rp), BigUint::from(1u8) << 64); // gcd = 2^64
        assert_eq!(len, 2); // 2^64 occupies 2 significant limbs ([0, 1])
    }

    #[test]
    fn lshift_sub_limb_and_cross_limb() {
        // [1] << 4 = [16], one extra limb (bit_shift != 0 contract)
        let mut rp = [0u64; 2];
        let top = mpn_lshift(&mut rp, &[1], 4);
        assert_eq!(rp, [16, 0]);
        assert_eq!(top, 0);
        // [1] << 64 = [0, 1], limb_shift=1, bit_shift=0 -> 2 limbs
        let mut rp = [0u64; 2];
        let top = mpn_lshift(&mut rp, &[1], 64);
        assert_eq!(rp, [0, 1]);
        assert_eq!(top, 1);
        // [1] << 68 = [0, 16], limb_shift=1 bit_shift=4 -> 3 limbs
        let mut rp = [0u64; 3];
        let top = mpn_lshift(&mut rp, &[1], 68);
        assert_eq!(rp, [0, 16, 0]);
        assert_eq!(top, 0);
    }

    #[test]
    fn rshift_basic() {
        // [0, 1] (2^64) >> 4 = 2^60
        let mut rp = [0u64; 2];
        let top = mpn_rshift(&mut rp, &[0, 1], 4);
        assert_eq!(from_limbs(&rp), BigUint::from(1u8) << 60);
        assert_eq!(top, rp[1]);
        // [0, 1] >> 64 = [1], limb_shift=1 -> 1 limb
        let mut rp = [0u64; 1];
        let top = mpn_rshift(&mut rp, &[0, 1], 64);
        assert_eq!(rp, [1]);
        assert_eq!(top, 1);
    }

    #[test]
    fn rshift_2c_rounds_toward_neg_inf() {
        // magnitude 5 (-5) >> 1 arithmetic: ceil(5/2) = 3
        let mut rp = [0u64; 1];
        let top = mpn_rshift_2c(&mut rp, &[5], 1);
        assert_eq!(rp, [3]);
        assert_eq!(top, 3);
        // magnitude 8 >> 2: 8 is divisible -> floor == ceil == 2, no round-up
        let mut rp = [0u64; 1];
        mpn_rshift_2c(&mut rp, &[8], 2);
        assert_eq!(rp, [2]);
        // carry into a new limb: (2^64 - 1) ... ceil need headroom
        // [MAX] >> 1 = 2^63 - 1, +1 (odd) -> 2^63. fits 1 limb.
        let mut rp = [0u64; 1];
        mpn_rshift_2c(&mut rp, &[u64::MAX], 1);
        assert_eq!(rp, [1u64 << 63]);
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
    fn write_limbs_zero_pads_and_counts() {
        let mut dst = [9u64; 4];
        let n = write_limbs(&mut dst, &(BigUint::from(1u8) << 64)); // value [0,1]
        assert_eq!(dst, [0, 1, 0, 0]);
        assert_eq!(n, 2);
    }
}
