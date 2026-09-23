//! Numbers written the way Python writes them (DESIGN.md 7.3).
//!
//! Model-facing text (the briefing, views, tool errors, event text) printed floats with Python's
//! `repr`/`str`, often after `round(x, n)`: `briefing.py:608` writes `round(u.moves / sc, 1)`,
//! `views.py:82` and `189-197` round to one or two places. Rust's `Display` prints `2.0` as `2`
//! and never uses an exponent, and its `Debug` writes exponents differently, so the text would
//! differ from Python's in thousands of places. [`PyFloat`] writes Python's form, and
//! [`PyRound`] rounds first, as `round(x, n)` did.

use core::fmt::{self, Write as _};

use super::num::round_ndigits;

/// Python's `repr(x)` (and `str(x)`) of a float.
///
/// - the shortest digits that read back as the same float;
/// - a trailing `.0` on whole numbers: `2.0`;
/// - scientific notation when the decimal exponent is below -4 or at least 16, with a sign and
///   at least two exponent digits: `1e+16`, `1.5e-05`;
/// - `nan`, `inf`, `-inf`, and `-0.0` for negative zero.
///
/// It honours width and alignment (`{:>8}`), as `str` padding does.
#[derive(Clone, Copy, Debug)]
pub struct PyFloat(pub f64);

/// Python's `repr(round(x, n))`: see [`round_ndigits`].
#[derive(Clone, Copy, Debug)]
pub struct PyRound(pub f64, pub i32);

impl fmt::Display for PyFloat {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut out = StackStr::new();
        write_repr(self.0, &mut out)?;
        f.pad(out.as_str())
    }
}

impl fmt::Display for PyRound {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(&PyFloat(round_ndigits(self.0, self.1)), f)
    }
}

/// Python's `float_repr`: `PyOS_double_to_string(x, 'r', 0, Py_DTSF_ADD_DOT_0)`.
fn write_repr(x: f64, out: &mut StackStr) -> fmt::Result {
    if x.is_nan() {
        return out.write_str("nan");
    }
    if x.is_infinite() {
        return out.write_str(if x > 0.0 { "inf" } else { "-inf" });
    }
    if x.is_sign_negative() {
        out.write_char('-')?;
    }
    let a = x.abs();
    if a == 0.0 {
        return out.write_str("0.0");
    }

    // Rust's LowerExp without a precision writes the shortest round-trip digits, the same digits
    // Python's repr chooses: "d.ddde-5".
    let mut sci = StackStr::new();
    write!(sci, "{a:e}")?;
    let (mantissa, exp) = sci.as_str().split_once('e').ok_or(fmt::Error)?;
    let exp: i32 = exp.parse().map_err(|_| fmt::Error)?;
    let mut digits = StackStr::new();
    for c in mantissa.chars().filter(|c| *c != '.') {
        digits.write_char(c)?;
    }
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

    fn as_str(&self) -> &str {
        // Only whole `str`s are ever copied in, so the bytes are valid UTF-8.
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
    }

    #[test]
    fn padding_and_rounding() {
        assert_eq!(format!("{:>6}", PyFloat(2.0)), "   2.0");
        assert_eq!(PyRound(2.675, 2).to_string(), "2.67");
        assert_eq!(PyRound(7.0 / 3.0, 1).to_string(), "2.3");
        assert_eq!(PyRound(0.5, 0).to_string(), "0.0");
    }
}
