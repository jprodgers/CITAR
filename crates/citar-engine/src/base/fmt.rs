//! Numbers written the way Python writes them (DESIGN.md 7.3).
//!
//! Model-facing text (the briefing, views, tool errors, event text) printed floats with Python's
//! `repr`/`str`, often after `round(x, n)`: `briefing.py:608` writes `round(u.moves / sc, 1)`,
//! `views.py:82` and `189-197` round to one or two places. Rust's `Display` prints `2.0` as `2`
//! and never uses an exponent, and its `Debug` writes exponents differently, so the text would
//! differ from Python's in thousands of places. [`PyFloat`] writes Python's form, and
//! [`PyRound`] rounds first, as `round(x, n)` did. With a precision, [`PyFloat`] is Python's
//! fixed-point `.Nf` (`briefing.py:208` writes `f"{gpt:+.0f}"`).

use core::fmt::{self, Write as _};

use super::num::{lowest_bit_exponent, round_ndigits, round_to_decimals};

/// Python's formatting of a float.
///
/// Without a precision it is `repr(x)` (and `str(x)`):
/// - the shortest digits that read back as the same float, and of two such digit strings
///   equally close to `x`, the one ending in an even digit;
/// - a trailing `.0` on whole numbers: `2.0`;
/// - scientific notation when the decimal exponent is below -4 or at least 16, with a sign and
///   at least two exponent digits: `1e+16`, `1.5e-05`;
/// - `nan`, `inf`, `-inf`, and `-0.0` for negative zero.
///
/// With a precision it is Python's `format(x, ".Nf")`: `format!("{:.1}", PyFloat(x))` writes
/// what `f"{x:.1f}"` did, the exact binary value rounded half to even (`12.25` gives `12.2`),
/// with no exponent and no added `.0`.
///
/// Width, fill, alignment, `+` and `0` behave as in Python's format specs for numbers:
/// right-aligned by default, and zero padding goes after the sign (`{:08}` of -2.5 is
/// `-00002.5`).
#[derive(Clone, Copy, Debug)]
pub struct PyFloat(pub f64);

/// Python's `repr(round(x, n))`: see [`round_ndigits`]. A precision formats the rounded value as
/// [`PyFloat`] does.
#[derive(Clone, Copy, Debug)]
pub struct PyRound(pub f64, pub i32);

impl fmt::Display for PyFloat {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let x = self.0;
        // Python writes NaN without a sign, whatever its sign bit.
        let nonnegative = x.is_nan() || !x.is_sign_negative();
        if !x.is_finite() {
            return f.pad_integral(nonnegative, "", if x.is_nan() { "nan" } else { "inf" });
        }
        match f.precision() {
            None => {
                let mut out = StackStr::new();
                write_repr_abs(x.abs(), &mut out)?;
                f.pad_integral(nonnegative, "", out.as_str())
            }
            Some(n) => f.pad_integral(nonnegative, "", &round_to_decimals(x.abs(), n)),
        }
    }
}

impl fmt::Display for PyRound {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(&PyFloat(round_ndigits(self.0, self.1)), f)
    }
}

/// Python's `float_repr` (`PyOS_double_to_string(x, 'r', 0, Py_DTSF_ADD_DOT_0)`) of a finite
/// `a >= 0`, without a sign.
fn write_repr_abs(a: f64, out: &mut StackStr) -> fmt::Result {
    if a == 0.0 {
        return out.write_str("0.0");
    }
    let mut digits = StackStr::new();
    let exp = shortest_digits(a, &mut digits)?;
    let digits = digits.as_str();

    // Python's decpt: the value is 0.d1d2... * 10^decpt.
    let decpt = exp + 1;
    if decpt <= -4 || decpt > 16 {
        let (first, rest) = digits.split_at(1);
        out.write_str(first)?;
        if !rest.is_empty() {
            out.write_char('.')?;
            out.write_str(rest)?;
        }
        let sign = if exp < 0 { '-' } else { '+' };
        write!(out, "e{sign}{:02}", exp.unsigned_abs())
    } else if decpt <= 0 {
        out.write_str("0.")?;
        for _ in 0..decpt.unsigned_abs() {
            out.write_char('0')?;
        }
        out.write_str(digits)
    } else {
        let point = decpt.unsigned_abs() as usize;
        if point >= digits.len() {
            out.write_str(digits)?;
            for _ in digits.len()..point {
                out.write_char('0')?;
            }
            out.write_str(".0")
        } else {
            let (int, frac) = digits.split_at(point);
            out.write_str(int)?;
            out.write_char('.')?;
            out.write_str(frac)
        }
    }
}

/// The shortest round-trip digits of a finite `a > 0` as Python's `repr` chooses them (dtoa
/// mode 0), into `digits`; returns the decimal exponent `e` of `d.ddd * 10^e`.
///
/// Rust's `LowerExp` without a precision gives the shortest digits and, of the candidates that
/// short, the one nearest `a`. When `a` lies exactly halfway between two of them it takes the
/// upper one, where Python takes the one ending in an even digit, if that one still reads back
/// as `a` (`1 + 2^-17` is `1.0000076293945312`, not `...313`). Such a tie needs `a`'s exact
/// decimal expansion to be one digit longer than the shortest form, so it is decided from the
/// bits, and the formatter's tie rule never matters.
fn shortest_digits(a: f64, digits: &mut StackStr) -> Result<i32, fmt::Error> {
    let mut sci = StackStr::new();
    write!(sci, "{a:e}")?;
    let (mantissa, exp) = sci.as_str().split_once('e').ok_or(fmt::Error)?;
    let exp: i32 = exp.parse().map_err(|_| fmt::Error)?;
    for c in mantissa.chars().filter(|c| *c != '.') {
        digits.write_char(c)?;
    }
    let len = digits.len();
    if !last_digit_is_odd(digits.as_str()) || !is_halfway(a, len, exp) {
        return Ok(exp);
    }
    // `a` has exactly len + 1 significant digits, the last a 5, so this formatting is exact.
    let mut exact = StackStr::new();
    write!(exact, "{a:.len$e}")?;
    let (exact_mantissa, exact_exp) = exact.as_str().split_once('e').ok_or(fmt::Error)?;
    if exact_exp.parse::<i32>() != Ok(exp) {
        return Ok(exp);
    }
    // The candidate below `a` (the exact digits cut short) and the one above it: the one Rust
    // did not choose ends in an even digit.
    let mut other = StackStr::new();
    for c in exact_mantissa.chars().filter(|c| *c != '.').take(len) {
        other.write_char(c)?;
    }
    if other.as_str() == digits.as_str() && !increment_last_digit(&mut other) {
        return Ok(exp);
    }
    // A trailing zero would mean a shorter form reads back as `a`, and Rust would have found
    // it; the check keeps a wrong reading from producing a malformed repr.
    if other.as_str().ends_with('0') {
        return Ok(exp);
    }
    let mut probe = StackStr::new();
    let len_i32 = i32::try_from(len).map_err(|_| fmt::Error)?;
    write!(probe, "{}e{}", other.as_str(), exp - (len_i32 - 1))?;
    if probe.as_str().parse::<f64>().is_ok_and(|b| b.to_bits() == a.to_bits()) {
        digits.clear();
        digits.write_str(other.as_str())?;
    }
    Ok(exp)
}

fn last_digit_is_odd(digits: &str) -> bool {
    digits.as_bytes().last().is_some_and(|d| (d - b'0') % 2 == 1)
}

/// Whether `a` is exactly halfway between two numbers of `len` significant digits in the decade
/// of `10^exp`: whether `2 * a * 10^k` is an odd integer, where `k = len - 1 - exp` is the
/// number of decimals of the last digit.
fn is_halfway(a: f64, len: usize, exp: i32) -> bool {
    // a = m * 2^e with m odd, so 2 * a * 10^k = m * 5^k * 2^(e + 1 + k). With k >= 0 that is an
    // odd integer exactly when e + 1 + k == 0; with k < 0 it also needs 5^-k to divide m.
    let (m, e) = lowest_bit_exponent(a);
    let Ok(len) = i64::try_from(len) else { return false };
    let k = len - 1 - i64::from(exp);
    if i64::from(e) + 1 + k != 0 {
        return false;
    }
    if k >= 0 {
        return true;
    }
    u32::try_from(-k).ok().and_then(|j| 5u64.checked_pow(j)).is_some_and(|p| m % p == 0)
}

/// Adds one to the last digit of a digit string whose last digit is not 9. False (and nothing
/// changed) if it is a 9.
fn increment_last_digit(s: &mut StackStr) -> bool {
    match s.len.checked_sub(1).map(|i| (i, s.buf[i])) {
        Some((i, d)) if d.is_ascii_digit() && d != b'9' => {
            s.buf[i] = d + 1;
            true
        }
        _ => false,
    }
}

/// A short string on the stack. A float's repr is at most 24 bytes (`-1.2345678901234567e-308`),
/// so formatting one allocates nothing.
struct StackStr {
    buf: [u8; 40],
    len: usize,
}

impl StackStr {
    const fn new() -> Self {
        Self { buf: [0; 40], len: 0 }
    }

    const fn len(&self) -> usize {
        self.len
    }

    const fn clear(&mut self) {
        self.len = 0;
    }

    fn as_str(&self) -> &str {
        // Only whole `str`s and ASCII digits are ever written in, so the bytes are valid UTF-8.
        core::str::from_utf8(&self.buf[..self.len]).unwrap_or("")
    }
}

impl fmt::Write for StackStr {
    fn write_str(&mut self, s: &str) -> fmt::Result {
        let end = self.len + s.len();
        let dst = self.buf.get_mut(self.len..end).ok_or(fmt::Error)?;
        dst.copy_from_slice(s.as_bytes());
        self.len = end;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn python_repr_known_answers() {
        let cases: [(f64, &str); 20] = [
            (0.0, "0.0"),
            (-0.0, "-0.0"),
            (1.0, "1.0"),
            (2.5, "2.5"),
            (0.1, "0.1"),
            (100.0, "100.0"),
            (1e15, "1000000000000000.0"),
            (1e16, "1e+16"),
            (1.5e16, "1.5e+16"),
            (9_999_999_999_999_998.0, "9999999999999998.0"),
            (0.0001, "0.0001"),
            (0.00001, "1e-05"),
            (1.5e-5, "1.5e-05"),
            (123.456, "123.456"),
            (1e100, "1e+100"),
            (5e-324, "5e-324"),
            (f64::MAX, "1.7976931348623157e+308"),
            (f64::NAN, "nan"),
            (f64::NEG_INFINITY, "-inf"),
            (-2.675, "-2.675"),
        ];
        for (x, want) in cases {
            assert_eq!(PyFloat(x).to_string(), want, "repr of {x:e}");
        }
        assert_eq!(PyFloat(-f64::NAN).to_string(), "nan");
    }

    /// Exactly halfway between two shortest candidates: Python takes the even one, unless only
    /// the odd one reads back (2^-24, where the gap below a power of two is half the gap above).
    #[test]
    fn python_repr_breaks_ties_to_even() {
        let cases: [(f64, &str); 7] = [
            (1.0 + 1.0 / 131_072.0, "1.0000076293945312"),
            (-86_993.0 / 131_072.0, "-0.6637039184570312"),
            (3_380_906_019_076_029.0 / 4.0, "845226504769007.2"),
            (f64::from_bits(0x3E60_0000_0000_0000), "2.9802322387695312e-08"), // 2^-25
            (f64::from_bits(0x3E70_0000_0000_0000), "5.960464477539063e-08"),  // 2^-24
            (1.0 + 3.0 / 131_072.0, "1.0000228881835938"),
            (1.0 + 5.0 / 131_072.0, "1.0000381469726562"),
        ];
        for (x, want) in cases {
            assert_eq!(PyFloat(x).to_string(), want, "repr of {x:e}");
            let back: f64 = want.parse().expect("a float");
            assert_eq!(back.to_bits(), x.to_bits(), "{want} reads back");
        }
    }

    #[test]
    fn padding_and_rounding() {
        assert_eq!(format!("{:>6}", PyFloat(2.0)), "   2.0");
        assert_eq!(format!("{:6}", PyFloat(2.0)), "   2.0");
        assert_eq!(format!("{:<6}|", PyFloat(-2.5)), "-2.5  |");
        assert_eq!(format!("{:x^9}", PyFloat(2.5)), "xxx2.5xxx");
        assert_eq!(format!("{:08}", PyFloat(-2.5)), "-00002.5");
        assert_eq!(format!("{:06}", PyFloat(f64::NAN)), "000nan");
        assert_eq!(format!("{:+}", PyFloat(2.0)), "+2.0");
        assert_eq!(format!("{:+}", PyFloat(-0.0)), "-0.0");
        assert_eq!(PyRound(2.675, 2).to_string(), "2.67");
        assert_eq!(PyRound(7.0 / 3.0, 1).to_string(), "2.3");
        assert_eq!(PyRound(0.5, 0).to_string(), "0.0");
    }

    /// A precision is Python's `.Nf`, never a cut of the repr.
    #[test]
    fn precision_is_pythons_fixed_point() {
        let cases: [(f64, usize, &str); 12] = [
            (12.25, 1, "12.2"),
            (2.675, 2, "2.67"),
            (0.125, 2, "0.12"),
            (2.0, 2, "2.00"),
            (2.5, 0, "2"),
            (3.5, 0, "4"),
            (0.5, 0, "0"),
            (-0.3, 0, "-0"),
            (-0.0, 1, "-0.0"),
            (1e16, 1, "10000000000000000.0"),
            (-5e-324, 3, "-0.000"),
            (f64::INFINITY, 2, "inf"),
        ];
        for (x, n, want) in cases {
            assert_eq!(format!("{:.n$}", PyFloat(x)), want, "{x:e} to {n} places");
        }
        assert_eq!(format!("{:+.0}", PyFloat(2.5)), "+2");
        assert_eq!(format!("{:+.0}", PyFloat(f64::NAN)), "+nan");
        assert_eq!(format!("{:>7.1}", PyFloat(-12.25)), "  -12.2");
        assert_eq!(format!("{:.2}", PyRound(2.675, 2)), "2.67");
        assert_eq!(format!("{:.3}", PyRound(1.0 / 3.0, 1)), "0.300");
    }
}
