//! The filter grammar (DESIGN.md 5.7): Python's `multi_filter` and `_and_parts`
//! (`uniques.py:294-327`).
//!
//! `{a} {b}` is a conjunction, split at bracket depth 0, and `non-[a]` a negation; anything else is
//! one term, which each domain's predicate answers for. Python applied the grammar again at every
//! evaluation; here a filter is parsed once, at load, into an [`Expr`] over its terms.

use super::expr::Expr;

/// How deep a filter may nest. Evaluation recurses over the tree, and a stack overflow aborts the
/// process rather than panicking (README, rule 5), so a deeper filter does not load. The ruleset
/// nests two deep at most.
pub const MAX_DEPTH: usize = 16;

/// A filter nested deeper than [`MAX_DEPTH`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TooDeep;

/// Parses a filter into its terms, as `multi_filter` read it.
///
/// # Errors
/// [`TooDeep`] for a filter nested deeper than [`MAX_DEPTH`].
pub fn parse(text: &str) -> Result<Expr<&str>, TooDeep> {
    walk(text, 0)
}

fn walk(text: &str, depth: usize) -> Result<Expr<&str>, TooDeep> {
    if depth > MAX_DEPTH {
        return Err(TooDeep);
    }
    if is_conjunction(text) {
        let parts: Result<Vec<Expr<&str>>, TooDeep> =
            and_parts(text).into_iter().map(|p| walk(p, depth + 1)).collect();
        return Ok(Expr::All(parts?.into()));
    }
    if let Some(inner) = negated(text) {
        return Ok(Expr::Not(Box::new(walk(inner, depth + 1)?)));
    }
    Ok(Expr::Leaf(text))
}

/// The terms of a filter, in order, as `multi_filter` would reach them. A filter nested deeper
/// than [`MAX_DEPTH`] gives what lies below as one term; [`parse`] refuses it.
pub fn terms(text: &str) -> Vec<&str> {
    let mut out = Vec::new();
    // An explicit stack: the nesting is the ruleset's to choose.
    let mut stack: Vec<(&str, usize)> = vec![(text, 0)];
    while let Some((t, depth)) = stack.pop() {
        if depth > MAX_DEPTH {
            out.push(t);
        } else if is_conjunction(t) {
            stack.extend(and_parts(t).into_iter().rev().map(|p| (p, depth + 1)));
        } else if let Some(inner) = negated(t) {
            stack.push((inner, depth + 1));
        } else {
            out.push(t);
        }
    }
    out
}

/// `{a} {b}`: braced at both ends with a `} {` somewhere (`uniques.py:302`).
fn is_conjunction(text: &str) -> bool {
    text.starts_with('{') && text.ends_with('}') && text.contains("} {")
}

/// The `x` of `non-[x]` (`uniques.py:304`).
fn negated(text: &str) -> Option<&str> {
    text.strip_prefix("non-[").and_then(|t| t.strip_suffix(']'))
}

/// `{a} {b}` split at depth 0 (`uniques.py:308-327`). Brackets and braces count alike, and an
/// unbalanced text is split as Python split it.
fn and_parts(text: &str) -> Vec<&str> {
    let inner = &text[1..text.len() - 1];
    let bytes = inner.as_bytes();
    let mut parts = Vec::new();
    let mut depth: i64 = 0;
    let mut cur = 0;
    let mut i = 0;
    while i < bytes.len() {
        if depth == 0 && bytes[i..].starts_with(b"} {") {
            parts.push(&inner[cur..i]);
            i += 3;
            cur = i;
            continue;
        }
        match bytes[i] {
            b'[' | b'{' => depth += 1,
            b']' | b'}' => depth -= 1,
            _ => {}
        }
        i += 1;
    }
    parts.push(&inner[cur..]);
    parts
}

#[cfg(test)]
mod tests {
    use super::*;

    fn leaf(s: &str) -> Expr<&str> {
        Expr::Leaf(s)
    }

    #[test]
    fn conjunctions_and_negations_parse_as_multi_filter_read_them() {
        assert_eq!(parse("Aircraft"), Ok(leaf("Aircraft")));
        assert_eq!(
            parse("{Military} {Water}"),
            Ok(Expr::All([leaf("Military"), leaf("Water")].into()))
        );
        assert_eq!(parse("non-[Air]"), Ok(Expr::Not(Box::new(leaf("Air")))));
        assert_eq!(
            parse("{non-[Natural Wonder]} {Mountain}"),
            Ok(Expr::All([Expr::Not(Box::new(leaf("Natural Wonder"))), leaf("Mountain")].into()))
        );
        assert_eq!(parse("{Land}"), Ok(leaf("{Land}")), "one braced term is a term");
        assert_eq!(parse("{a {b} c} {d}"), Ok(Expr::All([leaf("a {b} c"), leaf("d")].into())));
        assert_eq!(parse("pre-[Industrial era]"), Ok(leaf("pre-[Industrial era]")));
        assert_eq!(parse("non-fresh water"), Ok(leaf("non-fresh water")), "not the non-[x] form");
        assert_eq!(parse(""), Ok(leaf("")));
    }

    #[test]
    fn a_deep_filter_is_refused_and_its_terms_stop() {
        let at = |n: usize| format!("{}x{}", "non-[".repeat(n), "]".repeat(n));
        assert!(parse(&at(MAX_DEPTH)).is_ok());
        assert_eq!(parse(&at(MAX_DEPTH + 1)), Err(TooDeep));
        let deep = at(10_000);
        assert_eq!(parse(&deep), Err(TooDeep), "refused without overflowing the stack");
        assert_eq!(terms(&deep).len(), 1);
    }

    #[test]
    fn terms_come_in_order() {
        assert_eq!(terms("{non-[Air]} {Wounded}"), ["Air", "Wounded"]);
        assert_eq!(terms("{a} {b} {c}"), ["a", "b", "c"]);
        assert_eq!(terms("{a} {{b} {c}}"), ["a", "b", "c"]);
        assert_eq!(terms("x"), ["x"]);
    }
}
