//! Maths that gives the same bits on every target, and Python's integer and rounding rules
//! (DESIGN.md 7.3).
//!
//! - **Transcendental functions** go through the pinned, pure-Rust `libm`. The std methods call
//!   the platform's C library, whose last bit differs between targets, so clippy bans them.
//!   `sqrt`, `floor`, `ceil`, `trunc`, `abs`, `min`, `max` and `clamp` are exact in std and stay.
//! - **Integer division** follows Python: `//` floors and `%` takes the divisor's sign
//!   ([`floor_div`], [`floor_mod`]). Rust's `/` and `%` truncate toward zero.
//! - **Rounding** says which rule it means. Python's `round(x)` is half to even
//!   ([`round_half_even`]); `int(x + 0.5)` and `math.floor(x + 0.5)` are half away from zero for
//!   the non-negative values they were written for ([`round_half_away`]); `round(x, n)` rounds
//!   the exact binary value to `n` decimals ([`round_ndigits`]).
//! - **Float to int** conversions saturate, as Rust's `as` does, but under names that say so.
//!
//! Replaces the `math` module and the `round(` sites across `citar/engine` (77 plain, 17 with
//! digits).

/// `x` to the power `y` (Python's `x ** y` and `math.pow` on floats).
#[must_use]
#[inline]
pub fn pow(x: f64, y: f64) -> f64 {
    libm::pow(x, y)
}

/// e to the power `x`.
#[must_use]
#[inline]
pub fn exp(x: f64) -> f64 {
    libm::exp(x)
}

/// The natural logarithm (Python's `math.log` with one argument).
#[must_use]
#[inline]
pub fn ln(x: f64) -> f64 {
    libm::log(x)
}

/// The base-10 logarithm.
#[must_use]
#[inline]
pub fn log10(x: f64) -> f64 {
    libm::log10(x)
}

/// `sqrt(x² + y²)` without undue overflow or underflow (`math.hypot`, used by map generation).
#[must_use]
#[inline]
pub fn hypot(x: f64, y: f64) -> f64 {
    libm::hypot(x, y)
}

/// The sine of `x` radians.
#[must_use]
#[inline]
pub fn sin(x: f64) -> f64 {
    libm::sin(x)
}

/// The cosine of `x` radians.
#[must_use]
#[inline]
pub fn cos(x: f64) -> f64 {
    libm::cos(x)
}

/// The angle of the point `(x, y)` in radians, in `[-π, π]` (`math.atan2(y, x)`).
#[must_use]
#[inline]
pub fn atan2(y: f64, x: f64) -> f64 {
    libm::atan2(y, x)
}

// ---- Python integer division ------------------------------------------------------------------

/// Integers with Python's floor division and modulo.
pub trait FloorDiv: Copy {
    /// Python's `self // rhs`: the quotient rounded toward negative infinity.
    ///
    /// Python raises on division by zero; this returns 0. Where Python's unbounded ints would
    /// exceed the type (`MIN // -1`), it saturates.
    #[must_use]
    fn floor_div(self, rhs: Self) -> Self;

    /// Python's `self % rhs`: the remainder with the sign of `rhs`, so that
    /// `a == (a // b) * b + a % b`.
    ///
    /// Python raises on division by zero; this returns 0.
    #[must_use]
    fn floor_mod(self, rhs: Self) -> Self;
}

macro_rules! impl_floor_div {
    ($($t:ty),*) => {$(
        impl FloorDiv for $t {
            #[inline]
            fn floor_div(self, rhs: Self) -> Self {
                if rhs == 0 {
                    return 0;
                }
                let Some(q) = self.checked_div(rhs) else {
                    // MIN / -1: Python's answer is -MIN, one past MAX.
                    return <$t>::MAX;
                };
                // `%` cannot overflow once `/` did not.
                let r = self % rhs;
                if r != 0 && ((r < 0) != (rhs < 0)) { q - 1 } else { q }
            }

            #[inline]
            fn floor_mod(self, rhs: Self) -> Self {
                if rhs == 0 {
                    return 0;
                }
                let r = self.wrapping_rem(rhs);
                if r != 0 && ((r < 0) != (rhs < 0)) { r + rhs } else { r }
            }
        }
    )*};
}

impl_floor_div!(i32, i64);

/// Python's `a // b`. See [`FloorDiv::floor_div`].
#[must_use]
#[inline]
pub fn floor_div<T: FloorDiv>(a: T, b: T) -> T {
    a.floor_div(b)
}

/// Python's `a % b`. See [`FloorDiv::floor_mod`].
#[must_use]
#[inline]
pub fn floor_mod<T: FloorDiv>(a: T, b: T) -> T {
    a.floor_mod(b)
}

// ---- Rounding ---------------------------------------------------------------------------------

/// Python's `round(x)`: to the nearest integer, halves to even (`round(2.5) == 2`).
///
/// Python returns an int and raises on NaN and infinity; this returns the value as a float and
/// passes NaN and infinity through. [`round_half_even_i64`] and friends convert as well.
#[must_use]
#[inline]
pub fn round_half_even(x: f64) -> f64 {
    x.round_ties_even()
}

/// To the nearest integer, halves away from zero (`2.5` to `3`, `-2.5` to `-3`).
///
/// For the Python sites that wrote `int(x + 0.5)` or `math.floor(x + 0.5)`. Those agree with this
/// on the non-negative values they were written for, except where the float addition itself
/// rounded: `0.49999999999999994 + 0.5` is `1.0` in floats, so Python gave 1 where this gives 0.
#[must_use]
#[inline]
pub fn round_half_away(x: f64) -> f64 {
    #[allow(clippy::disallowed_methods, reason = "this is the one sanctioned half-away rounding")]
    libm::round(x)
}

/// Python's `round(x, ndigits)`: the exact binary value of `x` rounded to `ndigits` decimals,
/// halves to even, then read back as the nearest float.
///
/// So `round_ndigits(2.675, 2) == 2.67`, because the float nearest 2.675 is just below it, and
/// `round_ndigits(0.125, 2) == 0.12`, a true tie that goes to the even digit. A negative
/// `ndigits` rounds to tens, hundreds and so on. NaN, infinities, zeros and `ndigits` above 323
/// return `x`; `ndigits` below -308 returns a zero with the sign of `x`, all as Python does.
/// Where Python raises `OverflowError` (the result would be infinite), this returns `x`.
///
/// Python does this with `_Py_dg_dtoa` in mode 3. This uses Rust's exact float formatting, which
/// is correctly rounded, and decides exact ties itself from the bits of `x`, so the result does
/// not rest on how the formatter breaks ties.
#[must_use]
pub fn round_ndigits(x: f64, ndigits: i32) -> f64 {
    // floatobject.c: NDIGITS_MAX = (DBL_MANT_DIG - DBL_MIN_EXP) * 0.30103,
    // NDIGITS_MIN = -(DBL_MAX_EXP + 1) * 0.30103.
    const NDIGITS_MAX: i32 = 323;
    const NDIGITS_MIN: i32 = -308;
    if !x.is_finite() || x == 0.0 || ndigits > NDIGITS_MAX {
        return x;
    }
    if ndigits < NDIGITS_MIN {
        return 0.0 * x;
    }
    let a = x.abs();
    let digits = match usize::try_from(ndigits) {
        Ok(n) => round_to_decimals(a, n),
        Err(_) => round_to_tens(a, ndigits.unsigned_abs()),
    };
    // The digit strings built here always parse; the fallback only keeps this free of panics.
    let r: f64 = digits.parse().unwrap_or(a);
    if r.is_infinite() {
        return x;
    }
    if x.is_sign_negative() { -r } else { r }
}

/// `a` as `m * 2^e` with `m` odd: returns `(m, e)`. `a` must be finite and nonzero.
fn lowest_bit_exponent(a: f64) -> (u64, i32) {
    let bits = a.to_bits();
    let exp_field = ((bits >> 52) & 0x7ff) as i32;
    let frac = bits & ((1u64 << 52) - 1);
    let (mant, exp) =
        if exp_field == 0 { (frac, -1074) } else { (frac | (1u64 << 52), exp_field - 1075) };
    let tz = mant.trailing_zeros();
    (mant >> tz, exp + tz as i32)
}

/// `a >= 0` rounded to `n >= 0` decimals, as a decimal string.
fn round_to_decimals(a: f64, n: usize) -> String {
    // a = m * 2^e with m odd, so a * 10^n = (m * 5^n) * 2^(e + n) with m * 5^n odd: that has a
    // fractional part of exactly one half when e + n == -1, and only then.
    let (_, e) = lowest_bit_exponent(a);
    let tie = i64::from(e) + n as i64 == -1;
    if !tie {
        return format!("{a:.n$}");
    }
    // A tie has exactly n + 1 decimals, the last a 5, so this formatting is exact.
    let mut s = format!("{a:.prec$}", prec = n + 1);
    s.pop();
    if s.ends_with('.') {
        s.pop();
    }
    if s.as_bytes().last().is_some_and(|d| (d - b'0') % 2 == 1) {
        increment_decimal(&mut s);
    }
    s
}

/// `a >= 0` rounded to a multiple of `10^k`, `k >= 1`, as a decimal string.
fn round_to_tens(a: f64, k: u32) -> String {
    let k = k as usize;
    let whole = a.trunc();
    let frac = a - whole;
    // An integral float formats exactly.
    let int_digits = format!("{whole:.0}");
    let padded = if int_digits.len() < k + 1 {
        format!("{}{int_digits}", "0".repeat(k + 1 - int_digits.len()))
    } else {
        int_digits
    };
    let (head, rest) = padded.split_at(padded.len() - k);
    // Compare the remainder rest.frac with one half of 10^k, digit by digit.
    let half = format!("5{}", "0".repeat(k - 1));
    let up = match rest.cmp(half.as_str()) {
        core::cmp::Ordering::Greater => true,
        core::cmp::Ordering::Less => false,
        core::cmp::Ordering::Equal => {
            frac > 0.0 || head.as_bytes().last().is_some_and(|d| (d - b'0') % 2 == 1)
        }
    };
    let mut out = head.to_owned();
    if up {
        increment_decimal(&mut out);
    }
    out.push_str(&"0".repeat(k));
    out
}

/// Adds one unit in the last place to a decimal string of digits and at most one point.
fn increment_decimal(s: &mut String) {
    let mut bytes = core::mem::take(s).into_bytes();
    let mut i = bytes.len();
    loop {
        if i == 0 {
            bytes.insert(0, b'1');
            break;
        }
        i -= 1;
        match bytes[i] {
            b'.' => {}
            b'9' => bytes[i] = b'0',
            d => {
                bytes[i] = d + 1;
                break;
            }
        }
    }
    // Only ASCII digits and a point were touched.
    *s = String::from_utf8(bytes).unwrap_or_default();
}

// ---- Saturating conversions -------------------------------------------------------------------

macro_rules! float_to_int {
    ($($(#[$doc:meta])* $name:ident -> $t:ty = $how:expr;)*) => {$(
        $(#[$doc])*
        ///
        /// Saturates at the type's bounds, and NaN gives 0.
        #[must_use]
        #[inline]
        pub fn $name(x: f64) -> $t {
            let f: fn(f64) -> f64 = $how;
            f(x) as $t
        }
    )*};
}

float_to_int! {
    /// Python's `int(x)`: toward zero.
    trunc_i32 -> i32 = |x| x;
    /// Python's `int(x)`: toward zero.
    trunc_i64 -> i64 = |x| x;
    /// Python's `math.floor(x)`.
    floor_i32 -> i32 = f64::floor;
    /// Python's `math.floor(x)`.
    floor_i64 -> i64 = f64::floor;
    /// Python's `math.ceil(x)`.
    ceil_i32 -> i32 = f64::ceil;
    /// Python's `math.ceil(x)`.
    ceil_i64 -> i64 = f64::ceil;
    /// Python's `round(x)`, halves to even.
    round_half_even_i32 -> i32 = round_half_even;
    /// Python's `round(x)`, halves to even.
    round_half_even_i64 -> i64 = round_half_even;
    /// Halves away from zero; see [`round_half_away`].
    round_half_away_i32 -> i32 = round_half_away;
    /// Halves away from zero; see [`round_half_away`].
    round_half_away_i64 -> i64 = round_half_away;
    /// Toward zero, into `0..=255`.
    trunc_u8 -> u8 = |x| x;
    /// Toward zero, into `0..=65535`.
    trunc_u16 -> u16 = |x| x;
    /// Toward zero, into `0..=u32::MAX`.
    trunc_u32 -> u32 = |x| x;
}

/// `n` as an `i32`, saturating at its bounds.
#[must_use]
#[inline]
pub fn saturate_i32(n: i64) -> i32 {
    i32::try_from(n).unwrap_or(if n < 0 { i32::MIN } else { i32::MAX })
}

/// `n` as a `u32`, saturating at `u32::MAX`.
#[must_use]
#[inline]
pub fn saturate_u32(n: usize) -> u32 {
    u32::try_from(n).unwrap_or(u32::MAX)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn floor_division_follows_python() {
        let cases: [(i64, i64, i64, i64); 8] = [
            (7, 2, 3, 1),
            (-7, 2, -4, 1),
            (7, -2, -4, -1),
            (-7, -2, 3, -1),
            (6, 3, 2, 0),
            (-6, 3, -2, 0),
            (i64::MIN, -1, i64::MAX, 0),
            (5, 0, 0, 0),
        ];
        for (a, b, q, r) in cases {
            assert_eq!((floor_div(a, b), floor_mod(a, b)), (q, r), "{a} // {b}");
        }
        assert_eq!(floor_div(-1i32, 1000), -1);
        assert_eq!(floor_mod(-1i32, 1000), 999);
    }

    #[test]
    fn round_half_even_and_away() {
        assert_eq!(round_half_even(2.5).to_bits(), 2.0f64.to_bits());
        assert_eq!(round_half_even(3.5).to_bits(), 4.0f64.to_bits());
        assert_eq!(round_half_even(-2.5).to_bits(), (-2.0f64).to_bits());
        assert_eq!(round_half_away(2.5).to_bits(), 3.0f64.to_bits());
        assert_eq!(round_half_away(-2.5).to_bits(), (-3.0f64).to_bits());
        assert_eq!(round_half_even_i32(1e20), i32::MAX);
        assert_eq!(trunc_i32(f64::NAN), 0);
        assert_eq!(trunc_i64(-2.9), -2);
        assert_eq!(floor_i64(-2.1), -3);
    }

    #[test]
    fn round_ndigits_known_answers() {
        let cases: [(f64, i32, f64); 14] = [
            (0.125, 2, 0.12),
            (0.375, 2, 0.38),
            (2.675, 2, 2.67),
            (2.5, 0, 2.0),
            (3.5, 0, 4.0),
            (-0.5, 0, -0.0),
            (1.0625, 3, 1.062),
            (1e16, 2, 1e16),
            (1e-5, 3, 0.0),
            (15.0, -1, 20.0),
            (25.0, -1, 20.0),
            (5.0, -1, 0.0),
            (1234.5678, -2, 1200.0),
            (9.99, 1, 10.0),
        ];
        for (x, n, want) in cases {
            let got = round_ndigits(x, n);
            assert_eq!(got.to_bits(), want.to_bits(), "round({x}, {n}) = {got}, want {want}");
        }
        assert!(round_ndigits(-0.04, 1).is_sign_negative());
        assert!(round_ndigits(f64::NAN, 2).is_nan());
        assert_eq!(round_ndigits(1.5, 400).to_bits(), 1.5f64.to_bits());
        assert_eq!(round_ndigits(-1.5, -400).to_bits(), (-0.0f64).to_bits());
        assert_eq!(round_ndigits(f64::MAX, -308).to_bits(), f64::MAX.to_bits());
    }
}
