//! The small expressions a script writes as strings starting with `=`: arithmetic over numbers
//! and bound variables, and the coordinates of tile references (`tests/rules/README.md`).
//!
//! ```text
//! expr    := product (("+" | "-") product)*
//! product := unary (("*" | "/" | "//" | "%") unary)*
//! unary   := "-" unary | atom
//! atom    := number | 'text' | null | true | false | name path? | name "(" expr ")" | "(" expr ")"
//! ```
//!
//! A name is a bound variable, followed by any path segments (`r.techs_added.0`,
//! `ids[0]`). The functions are `x(ref)` and `y(ref)`, a tile reference's coordinates, and
//! `len(v)`, the length of a list, string or object. Whole numbers stay whole under `+`, `-`, `*`,
//! `//` and `%` (floor division and a modulo with the divisor's sign, as Python's); `/` always
//! gives a float. `+` also joins two strings.

use serde_json::{Map, Number, Value};

use super::path;

/// What an expression may read besides its variables: a tile reference's coordinates.
pub trait Env {
    /// The variables bound so far.
    fn vars(&self) -> &Map<String, Value>;
    /// The coordinates of a tile reference.
    fn tile(&mut self, reference: &Value) -> Result<(i32, i32), String>;
}

/// Evaluates an expression (the text after the `=`).
pub fn eval(text: &str, env: &mut dyn Env) -> Result<Value, String> {
    let mut p = Parser { s: text, i: 0, env };
    let v = p.expr()?;
    p.space();
    if p.i < text.len() {
        return Err(format!("expression `{text}`: unexpected `{}`", &text[p.i..]));
    }
    Ok(v)
}

struct Parser<'a, 'e> {
    s: &'a str,
    i: usize,
    env: &'e mut dyn Env,
}

/// A number as arithmetic sees it.
#[derive(Clone, Copy)]
enum Num {
    Int(i64),
    Float(f64),
}

impl Num {
    fn of(v: &Value) -> Option<Self> {
        let n = v.as_number()?;
        n.as_i64().map(Self::Int).or_else(|| n.as_f64().map(Self::Float))
    }

    fn float(self) -> f64 {
        match self {
            #[allow(clippy::cast_precision_loss, reason = "script numbers are small")]
            Self::Int(i) => i as f64,
            Self::Float(f) => f,
        }
    }

    fn value(self) -> Result<Value, String> {
        match self {
            Self::Int(i) => Ok(Value::from(i)),
            Self::Float(f) => Number::from_f64(f)
                .map(Value::Number)
                .ok_or_else(|| "the result is not finite".to_owned()),
        }
    }
}

impl Parser<'_, '_> {
    fn space(&mut self) {
        while self.s[self.i..].starts_with(' ') {
            self.i += 1;
        }
    }

    fn eat(&mut self, token: &str) -> bool {
        self.space();
        if self.s[self.i..].starts_with(token) {
            self.i += token.len();
            true
        } else {
            false
        }
    }

    fn err(&self, what: &str) -> String {
        format!("expression `{}`: {what} at `{}`", self.s, &self.s[self.i..])
    }

    fn expr(&mut self) -> Result<Value, String> {
        let mut v = self.product()?;
        loop {
            if self.eat("+") {
                let w = self.product()?;
                v = match (&v, &w) {
                    (Value::String(a), Value::String(b)) => Value::String(format!("{a}{b}")),
                    _ => arith(&v, &w, '+')?,
                };
            } else if self.eat("-") {
                let w = self.product()?;
                v = arith(&v, &w, '-')?;
            } else {
                return Ok(v);
            }
        }
    }

    fn product(&mut self) -> Result<Value, String> {
        let mut v = self.unary()?;
        loop {
            let op = if self.eat("//") {
                'f'
            } else if self.eat("*") {
                '*'
            } else if self.eat("/") {
                '/'
            } else if self.eat("%") {
                '%'
            } else {
                return Ok(v);
            };
            let w = self.unary()?;
            v = arith(&v, &w, op)?;
        }
    }

    fn unary(&mut self) -> Result<Value, String> {
        if self.eat("-") {
            let v = self.unary()?;
            return arith(&Value::from(0), &v, '-');
        }
        self.atom()
    }

    fn atom(&mut self) -> Result<Value, String> {
        self.space();
        let rest = &self.s[self.i..];
        if self.eat("(") {
            let v = self.expr()?;
            if !self.eat(")") {
                return Err(self.err("expected `)`"));
            }
            return Ok(v);
        }
        if let Some(body) = rest.strip_prefix('\'') {
            let end = body.find('\'').ok_or_else(|| self.err("unclosed string"))?;
            let text = body[..end].to_owned();
            self.i += end + 2;
            return Ok(Value::String(text));
        }
        if rest.starts_with(|c: char| c.is_ascii_digit() || c == '.') {
            let len = rest
                .find(|c: char| !(c.is_ascii_digit() || c == '.' || c == 'e' || c == 'E'))
                .unwrap_or(rest.len());
            let text = &rest[..len];
            self.i += len;
            if let Ok(n) = text.parse::<i64>() {
                return Ok(Value::from(n));
            }
            let f = text.parse::<f64>().map_err(|_| self.err("bad number"))?;
            return Num::Float(f).value();
        }
        if rest.starts_with(|c: char| c.is_ascii_alphabetic() || c == '_') {
            let len =
                rest.find(|c: char| !(c.is_ascii_alphanumeric() || c == '_')).unwrap_or(rest.len());
            let name = rest[..len].to_owned();
            self.i += len;
            match name.as_str() {
                "null" => return Ok(Value::Null),
                "true" => return Ok(Value::Bool(true)),
                "false" => return Ok(Value::Bool(false)),
                _ => {}
            }
            if self.s[self.i..].starts_with('(') {
                self.i += 1;
                let arg = self.expr()?;
                if !self.eat(")") {
                    return Err(self.err("expected `)`"));
                }
                return match name.as_str() {
                    "x" => Ok(Value::from(self.env.tile(&arg)?.0)),
                    "y" => Ok(Value::from(self.env.tile(&arg)?.1)),
                    "len" => match &arg {
                        Value::Array(a) => Ok(Value::from(a.len())),
                        Value::String(s) => Ok(Value::from(s.chars().count())),
                        Value::Object(o) => Ok(Value::from(o.len())),
                        other => Err(format!("len of {other}: not a list, string or object")),
                    },
                    _ => Err(format!("expression `{}`: no function {name}", self.s)),
                };
            }
            let (segs, end) = path::parse_tail(self.s, self.i)?;
            self.i = end;
            let base = self
                .env
                .vars()
                .get(&name)
                .ok_or_else(|| format!("expression `{}`: no variable {name}", self.s))?;
            return path::get(base, &segs).cloned().ok_or_else(|| {
                format!("expression `{}`: {name} has nothing at that path", self.s)
            });
        }
        Err(self.err("expected a value"))
    }
}

/// `a op b` over numbers: `+ - * / // %`, the last three with `f`, `/`, `%`.
fn arith(a: &Value, b: &Value, op: char) -> Result<Value, String> {
    let (Some(x), Some(y)) = (Num::of(a), Num::of(b)) else {
        return Err(format!("cannot compute {a} {op} {b}"));
    };
    let overflow = || "the result does not fit a whole number".to_owned();
    match (x, y, op) {
        (Num::Int(i), Num::Int(j), '+') => Ok(Value::from(i.checked_add(j).ok_or_else(overflow)?)),
        (Num::Int(i), Num::Int(j), '-') => Ok(Value::from(i.checked_sub(j).ok_or_else(overflow)?)),
        (Num::Int(i), Num::Int(j), '*') => Ok(Value::from(i.checked_mul(j).ok_or_else(overflow)?)),
        (Num::Int(_), Num::Int(0), 'f' | '%') => Err("division by zero".to_owned()),
        (Num::Int(i), Num::Int(j), 'f') => {
            Ok(Value::from(i.div_euclid(j) - i64::from(j < 0 && i.rem_euclid(j) != 0)))
        }
        (Num::Int(i), Num::Int(j), '%') => {
            let r = i.rem_euclid(j);
            Ok(Value::from(if j < 0 && r != 0 { r + j } else { r }))
        }
        _ => {
            let (p, q) = (x.float(), y.float());
            if matches!(op, '/' | 'f' | '%') && q == 0.0 {
                return Err("division by zero".to_owned());
            }
            let r = match op {
                '+' => p + q,
                '-' => p - q,
                '*' => p * q,
                '/' => p / q,
                'f' => (p / q).floor(),
                _ => p - q * (p / q).floor(),
            };
            Num::Float(r).value()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    struct Vars(Map<String, Value>);

    impl Env for Vars {
        fn vars(&self) -> &Map<String, Value> {
            &self.0
        }

        fn tile(&mut self, r: &Value) -> Result<(i32, i32), String> {
            match r.as_str() {
                Some("A") => Ok((5, 5)),
                _ => Err("no such tile".to_owned()),
            }
        }
    }

    fn run(text: &str) -> Result<Value, String> {
        let vars = json!({"r": {"ids": [7, 8]}, "g": 100});
        let mut env = Vars(vars.as_object().cloned().unwrap_or_default());
        eval(text, &mut env)
    }

    #[test]
    fn arithmetic_as_python_does_it() {
        assert_eq!(run("1 + 2 * 3"), Ok(json!(7)));
        assert_eq!(run("(1 + 2) * 3"), Ok(json!(9)));
        assert_eq!(run("7 // 2"), Ok(json!(3)));
        assert_eq!(run("-7 // 2"), Ok(json!(-4)));
        assert_eq!(run("-7 % 3"), Ok(json!(2)));
        assert_eq!(run("7 % -3"), Ok(json!(-2)));
        assert_eq!(run("7 / 2"), Ok(json!(3.5)));
        assert_eq!(run("g - 1"), Ok(json!(99)));
        assert_eq!(run("r.ids[1]"), Ok(json!(8)));
        assert_eq!(run("len(r.ids)"), Ok(json!(2)));
        assert_eq!(run("x('A') + y('A')"), Ok(json!(10)));
        assert_eq!(run("'a' + 'b'"), Ok(json!("ab")));
        assert_eq!(run("null"), Ok(Value::Null));
        assert!(run("1 // 0").is_err());
        assert!(run("nope").is_err());
        assert!(run("1 +").is_err());
    }
}
