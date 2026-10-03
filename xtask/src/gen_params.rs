//! `cargo xtask gen-params`: writes `crates/citar-bot/src/params/gen.rs`, the parameters of bot
//! version `basic-1`, from its schema `crates/citar-bot/params/basic-1.json` (DESIGN.md P2.3.2).
//!
//! The schema is the source of truth (package 2-00b exported it from `PARAM_GROUPS`), and three
//! readers share it: the Bots page and `profiles.py` read the JSON, and the bot reads what this
//! writes from it, so struct and schema cannot drift. `cargo xtask check` regenerates the file in
//! memory and fails on any difference (`check::generated`).
//!
//! What it writes:
//! - `Params`, one field per key in the schema's order: `i32` for an `int`, `f64` for a `float`,
//!   `bool`, a generated enum per `choice` (`Option<_>` when `null` is a choice), and `NameList`
//!   for an `order` or a `list` (`null` and `"default"` both read as `NameList::Default`);
//!   `serde(deny_unknown_fields)` and no defaults, so the effective map (the schema's defaults
//!   overlaid with the overrides) must name every field and nothing else;
//! - the enums, each with `name()`, the schema's text;
//! - `SPECS`, every parameter as data (key, kind, label, default, choices, options, presets),
//!   which `clean()` checks values against and the resolution of names reads, and `BY_KEY`, the
//!   positions sorted by key.
//!
//! It refuses a schema it cannot turn into Rust: a key that is no identifier, two keys alike, a
//! default of the wrong type or outside `i32`, a choice whose names do not make distinct
//! variants.

use std::collections::BTreeMap;
use std::path::Path;

use serde_json::Value;

/// Appends a formatted line to a `String` (see `gen_uniques`'s `wl!`).
macro_rules! wl {
    ($o:expr, $($t:tt)*) => {{
        $o.push_str(&format!($($t)*));
        $o.push('\n');
    }};
}

/// The file it writes, relative to the workspace root.
pub const OUT: &str = "crates/citar-bot/src/params/gen.rs";

/// The schema it reads, relative to the workspace root.
pub const SCHEMA: &str = "crates/citar-bot/params/basic-1.json";

/// The text of `gen.rs` for the workspace at `root`.
///
/// # Errors
/// The schema cannot be read, or is not one this generator can turn into Rust.
pub fn generate(root: &Path) -> Result<String, String> {
    let path = root.join(SCHEMA);
    let text = std::fs::read_to_string(&path)
        .map_err(|e| format!("cannot read {}: {e}", path.display()))?;
    generate_from(&text).map_err(|e| format!("{SCHEMA}: {e}"))
}

/// A parameter's type, as the schema names it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Ty {
    Int,
    Float,
    Bool,
    Choice,
    Order,
    List,
}

impl Ty {
    fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "int" => Self::Int,
            "float" => Self::Float,
            "bool" => Self::Bool,
            "choice" => Self::Choice,
            "order" => Self::Order,
            "list" => Self::List,
            _ => return None,
        })
    }

    const fn variant(self) -> &'static str {
        match self {
            Self::Int => "Int",
            Self::Float => "Float",
            Self::Bool => "Bool",
            Self::Choice => "Choice",
            Self::Order => "Order",
            Self::List => "List",
        }
    }
}

/// One parameter, read and checked.
struct Param {
    key: String,
    ty: Ty,
    label: String,
    help: String,
    default: Value,
    min: Option<Value>,
    max: Option<Value>,
    unit: String,
    /// A choice's choices, `None` for `null`.
    choices: Vec<Option<String>>,
    options: Vec<String>,
    presets: Vec<(String, Vec<String>)>,
}

impl Param {
    /// The Rust type of its field.
    fn rust_type(&self) -> String {
        match self.ty {
            Ty::Int => "i32".to_owned(),
            Ty::Float => "f64".to_owned(),
            Ty::Bool => "bool".to_owned(),
            Ty::Choice if self.choices.contains(&None) => format!("Option<{}>", camel(&self.key)),
            Ty::Choice => camel(&self.key),
            Ty::Order | Ty::List => "NameList".to_owned(),
        }
    }

    /// The names a choice's enum has, `None` aside.
    fn names(&self) -> Vec<&str> {
        self.choices.iter().filter_map(|c| c.as_deref()).collect()
    }
}

struct Group {
    name: String,
    help: String,
    params: Vec<Param>,
}

/// `gen.rs` from the schema's text.
pub fn generate_from(text: &str) -> Result<String, String> {
    let doc: Value = serde_json::from_str(text).map_err(|e| format!("not JSON: {e}"))?;
    if doc.get("engine").and_then(Value::as_str) != Some("basic-1") {
        return Err("its engine is not \"basic-1\"".to_owned());
    }
    let groups = read_groups(&doc)?;
    let params: Vec<&Param> = groups.iter().flat_map(|g| &g.params).collect();
    let mut o = String::new();
    header(&mut o, params.len());
    params_struct(&mut o, &groups);
    schema_defaults(&mut o, &params);
    for p in params.iter().filter(|p| p.ty == Ty::Choice) {
        choice_enum(&mut o, p);
    }
    specs(&mut o, &params);
    Ok(o)
}

fn read_groups(doc: &Value) -> Result<Vec<Group>, String> {
    let raw = doc.get("groups").and_then(Value::as_array).ok_or("no list of groups")?;
    let mut seen: BTreeMap<String, ()> = BTreeMap::new();
    let mut types: BTreeMap<String, String> = BTreeMap::new();
    let mut out = Vec::with_capacity(raw.len());
    for g in raw {
        let name = text(g, "name").ok_or("a group without a name")?;
        let help = text(g, "help").unwrap_or_default();
        let specs = g
            .get("params")
            .and_then(Value::as_array)
            .ok_or_else(|| format!("group {name}: no list of params"))?;
        let mut params = Vec::with_capacity(specs.len());
        for s in specs {
            let p = read_param(s).map_err(|e| format!("group {name}: {e}"))?;
            if seen.insert(p.key.clone(), ()).is_some() {
                return Err(format!("{}: the key appears twice", p.key));
            }
            if p.ty == Ty::Choice {
                let ty = camel(&p.key);
                if matches!(ty.as_str(), "Params" | "NameList" | "Spec" | "Kind" | "Fixed") {
                    return Err(format!(
                        "{}: its enum {ty} would shadow a type gen.rs uses",
                        p.key
                    ));
                }
                if let Some(other) = types.insert(ty.clone(), p.key.clone()) {
                    return Err(format!("{} and {other} both make the enum {ty}", p.key));
                }
            }
            params.push(p);
        }
        out.push(Group { name, help, params });
    }
    Ok(out)
}

fn text(v: &Value, key: &str) -> Option<String> {
    v.get(key).and_then(Value::as_str).map(str::to_owned)
}

fn strings(v: Option<&Value>, what: &str) -> Result<Vec<String>, String> {
    match v {
        None => Ok(Vec::new()),
        Some(Value::Array(a)) => a
            .iter()
            .map(|x| x.as_str().map(str::to_owned).ok_or_else(|| format!("{what} are names")))
            .collect(),
        Some(_) => Err(format!("{what} are a list of names")),
    }
}

fn read_param(s: &Value) -> Result<Param, String> {
    let key = text(s, "key").ok_or("a parameter without a key")?;
    if !is_field(&key) {
        return Err(format!("{key}: a key must be a lowercase Rust identifier"));
    }
    let ty = text(s, "type")
        .and_then(|t| Ty::parse(&t))
        .ok_or_else(|| format!("{key}: type is int, float, bool, choice, order or list"))?;
    let label = text(s, "label").ok_or_else(|| format!("{key}: no label"))?;
    let default = s.get("default").cloned().ok_or_else(|| format!("{key}: no default"))?;
    let choices: Vec<Option<String>> = match s.get("choices") {
        None => Vec::new(),
        Some(Value::Array(a)) => a
            .iter()
            .map(|c| match c {
                Value::Null => Ok(None),
                Value::String(n) => Ok(Some(n.clone())),
                _ => Err(format!("{key}: a choice is a name or null")),
            })
            .collect::<Result<_, _>>()?,
        Some(_) => return Err(format!("{key}: choices are a list")),
    };
    let options = strings(s.get("options"), &format!("{key}: options"))?;
    let presets = match s.get("presets") {
        None => Vec::new(),
        Some(Value::Object(m)) => m
            .iter()
            .map(|(name, names)| {
                Ok((name.clone(), strings(Some(names), &format!("{key}: preset {name}"))?))
            })
            .collect::<Result<_, String>>()?,
        Some(_) => return Err(format!("{key}: presets are an object")),
    };
    let p = Param {
        help: text(s, "help").unwrap_or_default(),
        min: s.get("min").cloned().filter(|v| !v.is_null()),
        max: s.get("max").cloned().filter(|v| !v.is_null()),
        unit: text(s, "unit").unwrap_or_default(),
        key,
        ty,
        label,
        default,
        choices,
        options,
        presets,
    };
    check_param(&p)?;
    Ok(p)
}

/// The checks that keep the generated code compiling and its defaults deserializing.
fn check_param(p: &Param) -> Result<(), String> {
    let key = &p.key;
    let bad = |what: &str| Err(format!("{key}: {what}"));
    match p.ty {
        Ty::Int => {
            let fits = p.default.as_i64().is_some_and(|n| i32::try_from(n).is_ok());
            if !fits {
                return bad("an int's default is a whole number inside i32");
            }
        }
        // A float default is written with a point (`3.0`), as the exporter's RETYPED wrote it.
        Ty::Float => {
            if !p.default.as_f64().is_some_and(f64::is_finite) || !p.default.is_f64() {
                return bad("a float's default is a number written with a point");
            }
        }
        Ty::Bool => {
            if !p.default.is_boolean() {
                return bad("a bool's default is true or false");
            }
        }
        Ty::Choice => {
            let names = p.names();
            if names.is_empty() {
                return bad("a choice has names to choose from");
            }
            let mut variants: Vec<String> = names.iter().map(|n| variant(n)).collect();
            if variants.iter().any(String::is_empty) {
                return bad("each choice must make a variant name (letters and digits)");
            }
            variants.sort();
            variants.dedup();
            if variants.len() != names.len() {
                return bad("two choices make the same variant name");
            }
            let ok = match &p.default {
                Value::Null => p.choices.contains(&None),
                Value::String(s) => names.contains(&s.as_str()),
                _ => false,
            };
            if !ok {
                return bad("a choice's default is one of its choices");
            }
        }
        Ty::Order | Ty::List => {
            if p.options.is_empty() {
                return bad("an order or a list has options");
            }
            for (name, names) in &p.presets {
                if let Some(n) = names.iter().find(|n| !p.options.contains(n)) {
                    return Err(format!("{key}: preset {name} names {n}, which is no option"));
                }
            }
            let ok = match &p.default {
                Value::Null => true,
                Value::String(s) => p.presets.iter().any(|(n, _)| n == s),
                Value::Array(a) => {
                    a.iter().all(|x| x.as_str().is_some_and(|n| p.options.iter().any(|o| o == n)))
                }
                _ => false,
            };
            if !ok {
                return bad(
                    "an order's or a list's default is null, a preset's name or names among its \
                     options",
                );
            }
        }
    }
    if p.ty != Ty::Choice && !p.choices.is_empty() {
        return bad("only a choice has choices");
    }
    if !matches!(p.ty, Ty::Order | Ty::List) && !(p.options.is_empty() && p.presets.is_empty()) {
        return bad("only an order or a list has options and presets");
    }
    Ok(())
}

/// A field name: lowercase ASCII letters, digits and underscores, not starting with a digit, and
/// no keyword.
fn is_field(key: &str) -> bool {
    const KEYWORDS: [&str; 51] = [
        "abstract", "as", "async", "await", "become", "box", "break", "const", "continue", "crate",
        "do", "dyn", "else", "enum", "extern", "false", "final", "fn", "for", "gen", "if", "impl",
        "in", "let", "loop", "macro", "match", "mod", "move", "mut", "override", "priv", "pub",
        "ref", "return", "self", "static", "struct", "super", "trait", "true", "try", "type",
        "typeof", "union", "unsafe", "unsized", "use", "virtual", "where", "while",
    ];
    let mut chars = key.chars();
    chars.next().is_some_and(|c| c.is_ascii_lowercase() || c == '_')
        && key.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
        && !KEYWORDS.contains(&key)
}

/// `free_gp_early` as a type name: `FreeGpEarly`.
fn camel(key: &str) -> String {
    key.split('_').filter(|w| !w.is_empty()).map(capitalised).collect()
}

/// A choice's name as a variant: its words capitalised and joined, everything but ASCII letters
/// and digits dropped (`Great Scientist` is `GreatScientist`, `classic` is `Classic`). Empty
/// when nothing is left; prefixed with `V` when it would start with a digit.
fn variant(name: &str) -> String {
    let v: String = name
        .split(|c: char| !c.is_ascii_alphanumeric())
        .filter(|w| !w.is_empty())
        .map(capitalised)
        .collect();
    if v.starts_with(|c: char| c.is_ascii_digit()) { format!("V{v}") } else { v }
}

fn capitalised(word: &str) -> String {
    let mut c = word.chars();
    c.next().map(|f| f.to_ascii_uppercase().to_string() + c.as_str()).unwrap_or_default()
}

/// Text for a doc comment: markdown's link and tag brackets escaped, so rustdoc reads the
/// schema's prose as prose.
fn doc_text(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        if matches!(c, '[' | ']' | '<' | '>' | '`' | '*' | '_') {
            out.push('\\');
        }
        out.push(c);
    }
    out
}

/// `text` as doc-comment lines of at most `width` characters, each starting with `indent///`.
fn doc_lines(o: &mut String, indent: &str, text: &str) {
    const WIDTH: usize = 100;
    let room = WIDTH.saturating_sub(indent.len() + 4).max(20);
    let mut line = String::new();
    for word in text.split_whitespace() {
        if !line.is_empty() && line.chars().count() + 1 + word.chars().count() > room {
            wl!(o, "{indent}/// {line}");
            line.clear();
        }
        if !line.is_empty() {
            line.push(' ');
        }
        line.push_str(word);
    }
    if !line.is_empty() {
        wl!(o, "{indent}/// {line}");
    }
}

/// A JSON number as the schema writes it, for a doc comment.
fn num(v: &Value) -> String {
    match v {
        Value::Number(n) => n.to_string(),
        other => other.to_string(),
    }
}

fn header(o: &mut String, count: usize) {
    o.push_str(
        "//! GENERATED by `cargo xtask gen-params` from `crates/citar-bot/params/basic-1.json`. \
         Do not\n//! edit: change the schema and run the command; `cargo xtask check` fails while \
         this file is\n//! stale (DESIGN.md P2.3.2).\n\n",
    );
    o.push_str("use serde::Deserialize;\n\n");
    o.push_str("use super::NameList;\n");
    o.push_str("use super::schema::{Fixed, Kind, Spec};\n\n");
    wl!(o, "/// How many parameters `basic-1` has.");
    wl!(o, "pub const COUNT: usize = {count};");
    o.push('\n');
}

fn params_struct(o: &mut String, groups: &[Group]) {
    o.push_str(
        "/// The effective parameters of a `basic-1` spec: the schema's defaults overlaid with its\n\
         /// overrides, one field per key in the schema's order. There are no defaults and no\n\
         /// unknown fields: the map deserialized must name every parameter and nothing else.\n",
    );
    o.push_str("#[derive(Clone, Debug, PartialEq, Deserialize)]\n#[serde(deny_unknown_fields)]\n");
    o.push_str("pub struct Params {\n");
    for (i, g) in groups.iter().enumerate() {
        if i > 0 {
            o.push('\n');
        }
        let help = if g.help.is_empty() { String::new() } else { format!(": {}", g.help) };
        let mut first = true;
        let mut line = String::new();
        // The group's name and help as a plain comment, wrapped.
        for word in format!("{}{help}", g.name).split_whitespace() {
            if !line.is_empty() && line.len() + 1 + word.len() > 88 {
                wl!(o, "    // {}{line}", if first { "---- " } else { "" });
                first = false;
                line.clear();
            }
            if !line.is_empty() {
                line.push(' ');
            }
            line.push_str(word);
        }
        wl!(o, "    // {}{line}", if first { "---- " } else { "" });
        for p in &g.params {
            let mut range = String::new();
            if let (Some(lo), Some(hi)) = (&p.min, &p.max) {
                let unit = if p.unit.is_empty() { String::new() } else { format!(" {}", p.unit) };
                range = format!(" ({} to {}{unit})", num(lo), num(hi));
            }
            let help =
                if p.help.is_empty() { ".".to_owned() } else { format!(": {}", doc_text(&p.help)) };
            doc_lines(o, "    ", &format!("**{}**{range}{help}", doc_text(&p.label)));
            if p.rust_type().starts_with("Option<") {
                // serde reads a missing `Option` field as `None`; this makes it required, as
                // every other field is, so a map without the key is refused.
                wl!(o, "    #[serde(deserialize_with = \"Option::deserialize\")]");
            }
            wl!(o, "    pub {}: {},", p.key, p.rust_type());
        }
    }
    o.push_str("}\n");
}

/// `Params::schema_defaults()`: the schema's defaults as values, which cannot fail to build;
/// the bot's tests hold it equal to the defaults map deserialized.
fn schema_defaults(o: &mut String, params: &[&Param]) {
    o.push_str("\nimpl Params {\n");
    o.push_str(
        "    /// Every parameter at the schema's default (`DEFAULT_PARAMS`), as the defaults map\n\
         \x20   /// deserializes.\n",
    );
    o.push_str("    #[must_use]\n    pub fn schema_defaults() -> Self {\n        Self {\n");
    for p in params {
        let value = match (p.ty, &p.default) {
            (Ty::Int | Ty::Bool, v) => v.to_string(),
            (Ty::Float, v) => float_lit(v.as_f64().unwrap_or_default()),
            (Ty::Choice, Value::String(s)) => {
                let v = format!("{}::{}", camel(&p.key), variant(s));
                if p.choices.contains(&None) { format!("Some({v})") } else { v }
            }
            (Ty::Choice, _) => "None".to_owned(),
            (_, Value::String(s)) if s != "default" => format!("NameList::Preset({s:?}.into())"),
            (_, Value::Array(a)) => {
                let names: Vec<String> =
                    a.iter().filter_map(Value::as_str).map(|n| format!("{n:?}.into()")).collect();
                format!("NameList::Names(vec![{}])", names.join(", "))
            }
            _ => "NameList::Default".to_owned(),
        };
        wl!(o, "            {}: {value},", p.key);
    }
    o.push_str("        }\n    }\n}\n");
}

fn choice_enum(o: &mut String, p: &Param) {
    let ty = camel(&p.key);
    o.push('\n');
    doc_lines(o, "", &format!("`{}`: {}.", p.key, doc_text(&p.label)));
    o.push_str("#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Deserialize)]\n");
    wl!(o, "pub enum {ty} {{");
    for n in p.names() {
        wl!(o, "    /// `{n:?}`");
        wl!(o, "    #[serde(rename = {n:?})]");
        wl!(o, "    {},", variant(n));
    }
    o.push_str("}\n\n");
    wl!(o, "impl {ty} {{");
    o.push_str("    /// Its name in the schema.\n    #[must_use]\n");
    o.push_str("    pub const fn name(self) -> &'static str {\n        match self {\n");
    for n in p.names() {
        wl!(o, "            Self::{} => {n:?},", variant(n));
    }
    o.push_str("        }\n    }\n}\n");
}

/// A Rust float literal: always with a point or an exponent.
fn float_lit(x: f64) -> String {
    format!("{x:?}")
}

fn str_list(names: &[String]) -> String {
    let items: Vec<String> = names.iter().map(|n| format!("{n:?}")).collect();
    format!("&[{}]", items.join(", "))
}

fn fixed(p: &Param) -> String {
    match (&p.ty, &p.default) {
        (Ty::Int, v) => format!("Fixed::Int({})", v.as_i64().unwrap_or_default()),
        (Ty::Float, v) => format!("Fixed::Float({})", float_lit(v.as_f64().unwrap_or_default())),
        (Ty::Bool, v) => format!("Fixed::Bool({})", v.as_bool().unwrap_or_default()),
        (_, Value::Null) => "Fixed::Name(None)".to_owned(),
        (_, Value::String(s)) => format!("Fixed::Name(Some({s:?}))"),
        (_, Value::Array(a)) => {
            let names: Vec<String> =
                a.iter().filter_map(Value::as_str).map(str::to_owned).collect();
            format!("Fixed::Names({})", str_list(&names))
        }
        (_, other) => format!("Fixed::Name(Some({:?}))", other.to_string()),
    }
}

fn specs(o: &mut String, params: &[&Param]) {
    o.push('\n');
    o.push_str(
        "/// Every parameter of `basic-1`, in the schema's order: what `clean()` checks a value \
         against\n/// and the names of an order or a list resolve from.\n",
    );
    o.push_str("pub static SPECS: [Spec; COUNT] = [\n");
    // One line each: the table is read by key, not by eye.
    for p in params {
        let choices: Vec<String> = p
            .choices
            .iter()
            .map(|c| c.as_ref().map_or_else(|| "None".to_owned(), |n| format!("Some({n:?})")))
            .collect();
        let presets: Vec<String> =
            p.presets.iter().map(|(n, names)| format!("({n:?}, {})", str_list(names))).collect();
        wl!(
            o,
            "    Spec {{ key: {:?}, kind: Kind::{}, label: {:?}, default: {}, choices: &[{}], \
             options: {}, presets: &[{}] }},",
            p.key,
            p.ty.variant(),
            p.label,
            fixed(p),
            choices.join(", "),
            str_list(&p.options),
            presets.join(", ")
        );
    }
    o.push_str("];\n\n");
    o.push_str("/// The position in [`SPECS`] of each key, sorted by key.\n");
    o.push_str("pub static BY_KEY: [(&str, u16); COUNT] = [\n");
    let mut keys: Vec<(&str, usize)> =
        params.iter().enumerate().map(|(i, p)| (p.key.as_str(), i)).collect();
    keys.sort_unstable();
    for (k, i) in keys {
        wl!(o, "    ({k:?}, {i}),");
    }
    o.push_str("];\n");
}

#[cfg(test)]
mod tests {
    use super::*;

    const MINI: &str = r#"{"engine": "basic-1", "groups": [
        {"name": "One", "help": "The first [group].", "params": [
            {"key": "a_count", "default": 3, "type": "int", "label": "Count", "help": "How many.",
             "min": 0, "max": 9, "unit": "turns"},
            {"key": "weight", "default": 1.5, "type": "float", "label": "Weight", "help": "",
             "min": 0, "max": 5, "unit": "x"},
            {"key": "mode", "default": "classic", "type": "choice", "label": "Mode", "help": "",
             "choices": ["classic", "Great Scientist"]},
            {"key": "focus", "default": null, "type": "choice", "label": "Focus", "help": "",
             "choices": [null, "food"]},
            {"key": "on", "default": true, "type": "bool", "label": "On", "help": ""},
            {"key": "order", "default": "early", "type": "order", "label": "Order", "help": "",
             "options": ["A", "B"], "presets": {"default": ["A", "B"], "early": ["B", "A"]}},
            {"key": "lines", "default": ["A"], "type": "list", "label": "Lines", "help": "",
             "options": ["A", "B"], "presets": {}}
        ]}
    ]}"#;

    #[test]
    fn a_schema_becomes_a_struct_enums_and_specs() {
        let out = generate_from(MINI).expect("generates");
        assert!(out.starts_with("//! GENERATED by `cargo xtask gen-params`"), "{out}");
        assert!(out.contains("pub const COUNT: usize = 7;"));
        assert!(out.contains("#[serde(deny_unknown_fields)]\npub struct Params {"));
        assert!(out.contains("    pub a_count: i32,"));
        assert!(out.contains("    pub weight: f64,"));
        assert!(out.contains("    pub mode: Mode,"));
        assert!(out.contains(
            "    #[serde(deserialize_with = \"Option::deserialize\")]\n    pub focus: Option<Focus>,"
        ));
        assert!(out.contains("    pub on: bool,"));
        assert!(out.contains("    pub order: NameList,"));
        assert!(out.contains("    pub lines: NameList,"));
        assert!(out.contains("    #[serde(rename = \"Great Scientist\")]\n    GreatScientist,"));
        assert!(out.contains("            Self::GreatScientist => \"Great Scientist\","));
        assert!(out.contains("pub enum Focus {\n    /// `\"food\"`"), "null is no variant");
        assert!(out.contains("/// **Count** (0 to 9 turns): How many."));
        assert!(out.contains("// ---- One: The first [group]."));
        assert!(out.contains(
            "    Spec { key: \"weight\", kind: Kind::Float, label: \"Weight\", default: \
             Fixed::Float(1.5), choices: &[], options: &[], presets: &[] },"
        ));
        assert!(out.contains("default: Fixed::Name(None), choices: &[None, Some(\"food\")],"));
        assert!(out.contains(
            "presets: &[(\"default\", &[\"A\", \"B\"]), (\"early\", &[\"B\", \"A\"])] },"
        ));
        assert!(out.contains("default: Fixed::Names(&[\"A\"]),"));
        // The defaults as values.
        assert!(out.contains("    pub fn schema_defaults() -> Self {"));
        for line in [
            "a_count: 3,",
            "weight: 1.5,",
            "mode: Mode::Classic,",
            "focus: None,",
            "on: true,",
            "order: NameList::Preset(\"early\".into()),",
            "lines: NameList::Names(vec![\"A\".into()]),",
        ] {
            assert!(out.contains(&format!("            {line}\n")), "{line}");
        }
        // Sorted by key, with the schema's positions.
        assert!(out.contains("    (\"a_count\", 0),\n    (\"focus\", 3),\n    (\"lines\", 6),"));
        assert_eq!(generate_from(MINI), Ok(out), "the same schema, the same text");
    }

    #[test]
    fn a_schema_rust_cannot_hold_is_refused() {
        let swap = |from: &str, to: &str| generate_from(&MINI.replacen(from, to, 1));
        let refused = |r: Result<String, String>, part: &str| {
            let e = r.expect_err("refused");
            assert!(e.contains(part), "{e}");
        };
        refused(swap("\"a_count\"", "\"type\""), "identifier");
        refused(swap("\"a_count\"", "\"weight\""), "twice");
        refused(swap("\"default\": 3,", "\"default\": 3.5,"), "whole number");
        refused(swap("\"default\": 3,", "\"default\": 3000000000,"), "i32");
        refused(swap("\"default\": 1.5,", "\"default\": 2,"), "with a point");
        refused(swap("\"default\": true", "\"default\": 1"), "true or false");
        refused(swap("\"default\": \"classic\"", "\"default\": \"modern\""), "one of its choices");
        refused(swap("\"Great Scientist\"", "\"classic!\""), "same variant");
        refused(swap("\"default\": \"early\"", "\"default\": \"late\""), "preset");
        refused(swap("\"early\": [\"B\", \"A\"]", "\"early\": [\"C\"]"), "no option");
        refused(swap("\"basic-1\"", "\"basic-2\""), "engine");
        refused(swap("\"key\": \"mode\"", "\"key\": \"params\""), "shadow");
    }

    #[test]
    fn the_committed_schema_generates() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("..");
        let out = generate(&root).expect("the schema generates");
        assert!(out.contains("pub const COUNT: usize = 373;"), "basic-1 has 373 parameters");
    }

    #[test]
    fn names_become_identifiers() {
        assert_eq!(camel("free_gp_early"), "FreeGpEarly");
        assert_eq!(variant("Great Scientist"), "GreatScientist");
        assert_eq!(variant("classic"), "Classic");
        assert_eq!(variant("3rd way"), "V3rdWay");
        assert_eq!(variant("!!"), "");
        assert!(is_field("u_food") && !is_field("type") && !is_field("Food") && !is_field("1a"));
        assert_eq!(doc_text("power > theirs [x]"), "power \\> theirs \\[x\\]");
    }
}
