//! A small Rust tokenizer, just enough for the source checks.
//!
//! It drops comments and keeps string contents, so a path mentioned in a doc comment or a message
//! is never mistaken for code, and it sees inside macro bodies, which a syntax tree would leave
//! as opaque tokens. It does not need to reject invalid Rust: the compiler already has.

/// One token of Rust source.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Tok {
    /// An identifier or keyword; a raw identifier `r#name` is stored as `name`.
    Ident(String),
    /// The contents of a string literal of any kind, escapes left as written.
    Str(String),
    /// `::`
    PathSep,
    /// Any other punctuation, one character at a time.
    Punct(char),
    /// A number or character literal, or a lifetime: never interesting here.
    Other,
}

/// A token and the 1-based line it starts on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Token {
    pub tok: Tok,
    pub line: u32,
}

impl Token {
    /// Whether this is the identifier `name`.
    pub fn is_ident(&self, name: &str) -> bool {
        matches!(&self.tok, Tok::Ident(s) if s == name)
    }

    /// Whether this is the punctuation character `c`.
    pub fn is_punct(&self, c: char) -> bool {
        self.tok == Tok::Punct(c)
    }

    /// Whether this is `::`.
    pub fn is_path_sep(&self) -> bool {
        self.tok == Tok::PathSep
    }
}

struct Lexer<'a> {
    chars: &'a [char],
    pos: usize,
    line: u32,
    out: Vec<Token>,
}

impl Lexer<'_> {
    fn peek(&self, ahead: usize) -> Option<char> {
        self.chars.get(self.pos + ahead).copied()
    }

    fn bump(&mut self) -> Option<char> {
        let c = self.chars.get(self.pos).copied()?;
        self.pos += 1;
        if c == '\n' {
            self.line += 1;
        }
        Some(c)
    }

    fn push(&mut self, tok: Tok, line: u32) {
        self.out.push(Token { tok, line });
    }

    fn run(mut self) -> Vec<Token> {
        while let Some(c) = self.peek(0) {
            let line = self.line;
            if c.is_whitespace() {
                self.bump();
            } else if c == '/' && self.peek(1) == Some('/') {
                while self.peek(0).is_some_and(|c| c != '\n') {
                    self.bump();
                }
            } else if c == '/' && self.peek(1) == Some('*') {
                self.block_comment();
            } else if c == '"' {
                self.bump();
                let s = self.quoted();
                self.push(Tok::Str(s), line);
            } else if c == '\'' {
                self.quote_or_lifetime();
                self.push(Tok::Other, line);
            } else if c.is_ascii_digit() {
                self.number();
                self.push(Tok::Other, line);
            } else if is_ident_start(c) {
                self.word(line);
            } else if c == ':' && self.peek(1) == Some(':') {
                self.bump();
                self.bump();
                self.push(Tok::PathSep, line);
            } else {
                self.bump();
                self.push(Tok::Punct(c), line);
            }
        }
        self.out
    }

    fn block_comment(&mut self) {
        // Rust block comments nest.
        let mut depth = 0usize;
        while let Some(c) = self.peek(0) {
            if c == '/' && self.peek(1) == Some('*') {
                depth += 1;
                self.bump();
                self.bump();
            } else if c == '*' && self.peek(1) == Some('/') {
                depth -= 1;
                self.bump();
                self.bump();
                if depth == 0 {
                    return;
                }
            } else {
                self.bump();
            }
        }
    }

    /// The rest of a string whose opening quote was consumed, with escapes.
    fn quoted(&mut self) -> String {
        let mut s = String::new();
        while let Some(c) = self.bump() {
            match c {
                '\\' => {
                    s.push(c);
                    if let Some(next) = self.bump() {
                        s.push(next);
                    }
                }
                '"' => break,
                _ => s.push(c),
            }
        }
        s
    }

    /// A raw string at `r`: `hashes` `#` marks, then the opening quote.
    fn raw_string(&mut self, hashes: usize) -> String {
        self.bump(); // r
        for _ in 0..hashes {
            self.bump();
        }
        self.bump(); // "
        let mut s = String::new();
        while let Some(c) = self.bump() {
            if c == '"' && (0..hashes).all(|k| self.peek(k) == Some('#')) {
                for _ in 0..hashes {
                    self.bump();
                }
                break;
            }
            s.push(c);
        }
        s
    }

    fn quote_or_lifetime(&mut self) {
        self.bump(); // '
        match (self.peek(0), self.peek(1)) {
            (Some('\\'), _) => {
                self.bump();
                self.bump();
                while self.peek(0).is_some_and(|c| c != '\'') {
                    self.bump();
                }
                self.bump();
            }
            (Some(_), Some('\'')) => {
                self.bump();
                self.bump();
            }
            _ => {
                // A lifetime or a loop label.
                while self.peek(0).is_some_and(is_ident_continue) {
                    self.bump();
                }
            }
        }
    }

    fn number(&mut self) {
        while self.peek(0).is_some_and(is_ident_continue) {
            self.bump();
        }
        // A fraction, but not a range (`1..2`) or a method call (`1.max(2)`).
        if self.peek(0) == Some('.') && self.peek(1).is_some_and(|c| c.is_ascii_digit()) {
            self.bump();
            while self.peek(0).is_some_and(is_ident_continue) {
                self.bump();
            }
        }
    }

    /// An identifier, or a literal that starts like one: `b'x'`, `b"..."`, `c"..."`, `r"..."`,
    /// `r#"..."#`, `br"..."`, `cr"..."`, or a raw identifier `r#name`.
    fn word(&mut self, line: u32) {
        let c = self.peek(0);
        let prefixed = matches!(c, Some('b' | 'c'));
        if c == Some('b') && self.peek(1) == Some('\'') {
            self.bump();
            self.quote_or_lifetime();
            self.push(Tok::Other, line);
            return;
        }
        if prefixed && self.peek(1) == Some('"') {
            self.bump();
            self.bump();
            let s = self.quoted();
            self.push(Tok::Str(s), line);
            return;
        }
        let r_at = if prefixed && self.peek(1) == Some('r') {
            Some(1)
        } else if c == Some('r') {
            Some(0)
        } else {
            None
        };
        if let Some(r_at) = r_at {
            let mut hashes = 0;
            while self.peek(r_at + 1 + hashes) == Some('#') {
                hashes += 1;
            }
            if self.peek(r_at + 1 + hashes) == Some('"') {
                for _ in 0..r_at {
                    self.bump();
                }
                let s = self.raw_string(hashes);
                self.push(Tok::Str(s), line);
                return;
            }
            if r_at == 0 && hashes == 1 && self.peek(2).is_some_and(is_ident_start) {
                self.bump();
                self.bump();
            }
        }
        let mut s = String::new();
        while let Some(c) = self.peek(0).filter(|&c| is_ident_continue(c)) {
            s.push(c);
            self.bump();
        }
        self.push(Tok::Ident(s), line);
    }
}

fn is_ident_start(c: char) -> bool {
    c == '_' || c.is_alphabetic()
}

fn is_ident_continue(c: char) -> bool {
    c == '_' || c.is_alphanumeric()
}

/// Splits Rust source into tokens, dropping whitespace and comments.
pub fn lex(src: &str) -> Vec<Token> {
    let chars: Vec<char> = src.chars().collect();
    Lexer { chars: &chars, pos: 0, line: 1, out: Vec::new() }.run()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn idents(src: &str) -> Vec<String> {
        lex(src)
            .into_iter()
            .filter_map(|t| match t.tok {
                Tok::Ident(s) => Some(s),
                _ => None,
            })
            .collect()
    }

    fn strings(src: &str) -> Vec<String> {
        lex(src)
            .into_iter()
            .filter_map(|t| match t.tok {
                Tok::Str(s) => Some(s),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn comments_are_dropped() {
        let src =
            "a // crate::api\n/* outer /* crate::api */ still */ b /// doc crate::api\n//! x\nc";
        assert_eq!(idents(src), ["a", "b", "c"]);
    }

    #[test]
    fn strings_keep_their_contents_and_hide_code() {
        assert_eq!(idents(r#"let s = "crate::api \" x";"#), ["let", "s"]);
        assert_eq!(strings(r#"f("1b-05")"#), ["1b-05"]);
        assert_eq!(
            strings(r####"r#"a "quoted" b"# r"x" br##"y"## b"z" c"w""####),
            [r#"a "quoted" b"#, "x", "y", "z", "w"]
        );
    }

    #[test]
    fn chars_and_lifetimes() {
        let src = "fn f<'a>(x: &'a str) -> char { let _ = b'\\''; 'outer: loop {} '\"' }";
        let ids = idents(src);
        assert!(ids.contains(&"loop".to_string()));
        assert!(!ids.contains(&"a".to_string()), "a lifetime is not an identifier: {ids:?}");
        assert!(strings(src).is_empty(), "the quote in a char literal opened a string");
    }

    #[test]
    fn raw_identifiers_and_numbers() {
        assert_eq!(idents("r#type 1.5e3 0x1F 1..2 x.0"), ["type", "x"]);
    }

    #[test]
    fn path_separators_and_lines() {
        let toks = lex("use crate::api;\n\nx");
        assert!(toks[1].is_ident("crate"));
        assert!(toks[2].is_path_sep());
        assert!(toks[3].is_ident("api"));
        assert_eq!(toks.last().map(|t| t.line), Some(3));
    }
}
