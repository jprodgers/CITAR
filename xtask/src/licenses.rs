//! `cargo xtask licenses [--check] [--metadata FILE] [--out FILE]` (design/phase5-release.md
//! P5.5, package 5-04): `packaging/THIRD-PARTY-LICENSES.txt`, the notices of every third-party
//! crate the release's binaries link.
//!
//! The wheels' extension (`citar-py`) and the helper (`citar-helper`) statically link about two
//! hundred crates whose licences (MIT, Apache-2.0, BSD, ISC, Unicode, ...) ask for their notices
//! to travel with the binaries. This reads `cargo metadata --locked` (or the copy `--metadata`
//! names) with the features the release builds turn on (pyproject.toml's maturin features for
//! citar-py, `packaged` for the helper's package-manager build), and takes the normal-dependency
//! closure of the two crates on each release target ([`TARGETS`], the helper's five, which are the
//! wheels' five too), each dependency's platform condition evaluated against that target's
//! `rustc --print cfg`: so a Windows-only or a Unix-only crate is in, and a crate only WebAssembly,
//! Android or a target CITAR does not build for would link is out. cargo metadata's resolve
//! unifies features across the whole workspace, its tests' dev-dependencies included, so the
//! closure can name a crate no release build compiles in (seven at 5-04: ed25519-dalek's `pkcs8`
//! chain, an older `rand_core`, `zerocopy-derive`), and never misses one: a test holds it to
//! `cargo tree`'s per-target builds, every crate of which must be in it. It writes each third-party
//! crate once, in name order: its version, its licence expression as its package declares it, the
//! targets it is built for when not all of them, and every licence and notice file its package
//! ships, read from the crate's sources in the registry (which `cargo metadata` and
//! `cargo fetch --locked` download): those at its top and those below it, where a crate keeps the
//! notices of code it bundles (ring's `third_party/fiat/LICENSE`, zstd-sys's `zstd/LICENSE`), but
//! none from its tests, benchmarks, examples or CI folders, which no build links. A text that is
//! byte for byte one printed earlier is named rather than printed again. A crate whose package
//! ships no licence file anywhere gets its licence's standard text, from [`STANDARD_TEXTS`], and is
//! refused when none of its licences has one there.
//!
//! A licence outside [`ALLOWED`] is refused by name, crate and version: the expression must be
//! satisfiable with the allowed licences alone (`OR` takes any branch, `AND` needs both, `WITH`
//! only the exceptions of [`ALLOWED_EXCEPTIONS`]; the old `/` reads as `OR`). `--check` writes
//! nothing and fails when the file differs from what this would write, naming the crates added and
//! removed: rust.yml's lint job runs it after `cargo fetch --locked`, and package.yml's `notices`
//! job before it uploads the file a release carries, so a lock changed without the file fails.
//!
//! The output depends on the lock, the crates' sources and the toolchain's target definitions
//! alone: no path, time or version of CITAR's own is written (a release's version bump leaves the
//! file as it is), and every text is read with LF line endings, so Windows and Linux write the
//! same bytes.
//!
//! Exit 0 written (with `--check`: current), 1 a licence refused or the file out of date, 2 could
//! not run.

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};

use serde::Deserialize;

use crate::manifest::TARGETS;

/// The file this writes, relative to the workspace root.
pub const OUT: &str = "packaging/THIRD-PARTY-LICENSES.txt";

/// The crates whose builds ship: the wheels' extension and the helper.
pub const ROOTS: &[&str] = &["citar-py", "citar-helper"];

/// Features the helper's release builds turn on beyond its defaults (P5.4.1: the packaged build).
/// citar-py's are pyproject.toml's maturin features, read from the file.
const HELPER_FEATURES: &[&str] = &["packaged"];

/// The licences a shipped crate may be used under (P5.5).
pub const ALLOWED: &[&str] = &[
    "MIT",
    "Apache-2.0",
    "BSD-2-Clause",
    "BSD-3-Clause",
    "ISC",
    "Zlib",
    "Unicode-3.0",
    "Unicode-DFS-2016",
    "MPL-2.0",
    "CC0-1.0",
    "BSL-1.0",
    "0BSD",
    "CDLA-Permissive-2.0",
];

/// The licence exceptions allowed, each with the one licence it may follow.
pub const ALLOWED_EXCEPTIONS: &[(&str, &str)] = &[("Apache-2.0", "LLVM-exception")];

/// Standard licence texts, for a crate whose package ships no licence file: the first of these
/// that satisfies its expression alone is printed, marked as the licence's standard text. Only
/// licences whose text names no copyright holder are here (Apache-2.0's appendix is a template),
/// so nothing is written in a crate's name that it did not write itself.
pub const STANDARD_TEXTS: &[(&str, &str)] =
    &[("Apache-2.0", include_str!("../licenses/Apache-2.0.txt"))];

/// File names (lower case) a licence or notice file starts with, wherever it is in its package.
const PREFIXES: &[&str] = &["license", "licence", "copying", "copyright", "notice", "unlicense"];
/// Folders (lower case) whose every file is a licence text, wherever they are (REUSE's
/// `LICENSES/`, or a `license/` of texts). Only these whole names: the windows crates' module
/// `LicenseProtection/` holds code.
const FOLDERS: &[&str] = &["license", "licenses", "licence", "licences"];
/// Folders (lower case) never searched, wherever they are: tests, benchmarks, examples and CI,
/// which no build links, so their files' notices are not the binaries'.
const NOT_SHIPPED: &[&str] = &["tests", "benches", "examples", ".github"];
/// Extensions of files that are not licence texts, whatever their names start with or whichever
/// folder holds them: source code, a bundled C library's included.
const NOT_TEXTS: &[&str] = &[
    "rs", "toml", "json", "yml", "yaml", "py", "sh", "lock", "c", "h", "cc", "cpp", "hpp", "s",
    "asm", "pl", "js", "go", "ps1", "bat",
];

/// The registry every published crate comes from, as Cargo.lock and `cargo metadata` name it.
const CRATES_IO: &[&str] =
    &["registry+https://github.com/rust-lang/crates.io-index", "sparse+https://index.crates.io/"];

/// A crate's section starts with this line.
const RULE: &str =
    "================================================================================";

/// Why the command stopped: the exit code (1 a problem found, 2 could not run) and the reason.
#[derive(Debug, PartialEq, Eq)]
pub struct Stop(pub u8, pub String);

fn cannot(why: impl Into<String>) -> Stop {
    Stop(2, why.into())
}

fn problem(why: impl Into<String>) -> Stop {
    Stop(1, why.into())
}

/// Runs the command; prints what it wrote or found, or why it stopped.
pub fn run(root: &Path, args: &[String]) -> ExitCode {
    match run_inner(root, args) {
        Ok(said) => {
            println!("licenses: {said}");
            ExitCode::SUCCESS
        }
        Err(Stop(code, why)) => {
            eprintln!("licenses: {why}");
            ExitCode::from(code)
        }
    }
}

#[derive(Debug, Default, PartialEq, Eq)]
struct Options {
    check: bool,
    metadata: Option<PathBuf>,
    out: Option<PathBuf>,
}

fn options(args: &[String]) -> Result<Options, Stop> {
    let mut o = Options::default();
    let mut it = args.iter();
    while let Some(a) = it.next() {
        let mut value = |name: &str| {
            it.next().map(PathBuf::from).ok_or_else(|| cannot(format!("{name} needs a value")))
        };
        match a.as_str() {
            "--check" => o.check = true,
            "--metadata" => o.metadata = Some(value(a)?),
            "--out" => o.out = Some(value(a)?),
            other => {
                return Err(cannot(format!(
                    "unknown argument {other} (it takes --check, --metadata FILE, --out FILE)"
                )));
            }
        }
    }
    Ok(o)
}

fn run_inner(root: &Path, args: &[String]) -> Result<String, Stop> {
    let o = options(args)?;
    let json = match &o.metadata {
        Some(file) => {
            std::fs::read(file).map_err(|e| cannot(format!("{}: {e}", file.display())))?
        }
        None => cargo_metadata(root)?,
    };
    let meta = Metadata::parse(&json)?;
    let targets = TARGETS
        .iter()
        .map(|(triple, _)| target_cfg(root, triple))
        .collect::<Result<Vec<_>, _>>()?;
    let notices = Notices::gather(&meta, ROOTS, &targets)?;
    let text = notices.render();
    let out = o.out.unwrap_or_else(|| root.join(OUT));
    let shown = shown_path(root, &out);
    let summary = notices.summary();
    if o.check {
        let found = std::fs::read(&out)
            .map_err(|e| problem(format!("{shown}: {e}: run `cargo xtask licenses`")))?;
        if found == text.as_bytes() {
            return Ok(format!("{shown} is current: {summary}"));
        }
        return Err(problem(format!(
            "{shown} is out of date, so a release would carry the wrong notices: run `cargo xtask \
             licenses` and commit it\n{}",
            difference(&String::from_utf8_lossy(&found), &text)
        )));
    }
    if let Some(dir) = out.parent() {
        std::fs::create_dir_all(dir).map_err(|e| cannot(format!("{}: {e}", dir.display())))?;
    }
    std::fs::write(&out, &text).map_err(|e| cannot(format!("{}: {e}", out.display())))?;
    Ok(format!("wrote {shown}: {summary}"))
}

/// `out` relative to the workspace root when it is inside it, with `/` separators.
fn shown_path(root: &Path, out: &Path) -> String {
    let rel = out.strip_prefix(root).unwrap_or(out);
    rel.to_string_lossy().replace('\\', "/")
}

/// `cargo metadata --locked` for the workspace, with the features the release builds turn on.
fn cargo_metadata(root: &Path) -> Result<Vec<u8>, Stop> {
    let mut features: Vec<String> =
        HELPER_FEATURES.iter().map(|f| format!("citar-helper/{f}")).collect();
    features.extend(maturin_features(root)?.into_iter().map(|f| format!("citar-py/{f}")));
    let cargo = std::env::var("CARGO").unwrap_or_else(|_| "cargo".to_owned());
    let mut cmd = Command::new(cargo);
    cmd.args(["metadata", "--locked", "--format-version", "1", "--manifest-path"])
        .arg(root.join("Cargo.toml"));
    if !features.is_empty() {
        cmd.arg("--features").arg(features.join(","));
    }
    let out = cmd.output().map_err(|e| cannot(format!("running cargo metadata: {e}")))?;
    if !out.status.success() {
        return Err(cannot(format!(
            "cargo metadata --locked failed (a Cargo.lock that needs updating fails here too): {}",
            String::from_utf8_lossy(&out.stderr).trim()
        )));
    }
    Ok(out.stdout)
}

/// pyproject.toml's `[tool.maturin] features`: what every wheel's citar-py is built with.
fn maturin_features(root: &Path) -> Result<Vec<String>, Stop> {
    let path = root.join("pyproject.toml");
    let text =
        std::fs::read_to_string(&path).map_err(|e| cannot(format!("{}: {e}", path.display())))?;
    let doc: toml::Table = text.parse().map_err(|e| cannot(format!("{}: {e}", path.display())))?;
    let features = doc
        .get("tool")
        .and_then(|t| t.get("maturin"))
        .and_then(|m| m.get("features"))
        .and_then(toml::Value::as_array)
        .map(|a| a.iter().filter_map(toml::Value::as_str).map(str::to_owned).collect())
        .unwrap_or_default();
    Ok(features)
}

// ------------------------------------------------------------------------------- cargo metadata

#[derive(Debug, Deserialize)]
struct Metadata {
    packages: Vec<Package>,
    workspace_members: Vec<String>,
    resolve: Option<Resolve>,
}

#[derive(Debug, Deserialize)]
struct Package {
    name: String,
    version: String,
    id: String,
    #[serde(default)]
    license: Option<String>,
    #[serde(default)]
    license_file: Option<String>,
    manifest_path: String,
    #[serde(default)]
    source: Option<String>,
    #[serde(default)]
    repository: Option<String>,
}

#[derive(Debug, Deserialize)]
struct Resolve {
    nodes: Vec<Node>,
}

#[derive(Debug, Deserialize)]
struct Node {
    id: String,
    #[serde(default)]
    deps: Vec<NodeDep>,
}

#[derive(Debug, Deserialize)]
struct NodeDep {
    pkg: String,
    #[serde(default)]
    dep_kinds: Vec<DepKind>,
}

#[derive(Debug, Deserialize)]
struct DepKind {
    /// `None` for a normal dependency, else `"dev"` or `"build"`.
    kind: Option<String>,
    /// The platform condition (`cfg(...)` or a target triple), `None` on every platform.
    target: Option<String>,
}

impl Metadata {
    fn parse(json: &[u8]) -> Result<Self, Stop> {
        serde_json::from_slice(json).map_err(|e| cannot(format!("reading cargo metadata: {e}")))
    }
}

// ------------------------------------------------------------------------------ targets and cfg

/// A release target and the configuration `rustc --print cfg` gives it.
#[derive(Debug, Clone)]
pub struct Target {
    pub triple: String,
    cfg: BTreeSet<(String, Option<String>)>,
}

impl Target {
    /// A target from `rustc --print cfg`'s lines: `name` or `name="value"`.
    fn from_print(triple: &str, text: &str) -> Target {
        let cfg = text
            .lines()
            .map(str::trim)
            .filter(|l| !l.is_empty())
            .map(|l| match l.split_once('=') {
                Some((k, v)) => (k.trim().to_owned(), Some(v.trim().trim_matches('"').to_owned())),
                None => (l.to_owned(), None),
            })
            .collect();
        Target { triple: triple.to_owned(), cfg }
    }

    fn matches(&self, platform: &Platform) -> bool {
        match platform {
            Platform::Triple(t) => *t == self.triple,
            Platform::Cfg(c) => self.holds(c),
        }
    }

    fn holds(&self, c: &Cfg) -> bool {
        match c {
            Cfg::All(cs) => cs.iter().all(|c| self.holds(c)),
            Cfg::Any(cs) => cs.iter().any(|c| self.holds(c)),
            Cfg::Not(c) => !self.holds(c),
            Cfg::Is(name) => self.cfg.contains(&(name.clone(), None)),
            Cfg::Eq(name, value) => self.cfg.contains(&(name.clone(), Some(value.clone()))),
        }
    }
}

/// The target's configuration from the workspace's toolchain (rust-toolchain.toml, through
/// rustup in the workspace's folder), as a release build (`-O`) sees it. rustc prints any built-in
/// target's configuration without its standard library installed.
fn target_cfg(root: &Path, triple: &str) -> Result<Target, Stop> {
    let rustc = std::env::var("RUSTC").unwrap_or_else(|_| "rustc".to_owned());
    let out = Command::new(rustc)
        .current_dir(root)
        .args(["-O", "--print", "cfg", "--target", triple])
        .output()
        .map_err(|e| cannot(format!("running rustc --print cfg: {e}")))?;
    if !out.status.success() {
        return Err(cannot(format!(
            "rustc --print cfg --target {triple} failed: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        )));
    }
    Ok(Target::from_print(triple, &String::from_utf8_lossy(&out.stdout)))
}

/// A dependency's platform condition.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Platform {
    Triple(String),
    Cfg(Cfg),
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Cfg {
    All(Vec<Cfg>),
    Any(Vec<Cfg>),
    Not(Box<Cfg>),
    Is(String),
    Eq(String, String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum CfgToken {
    Ident(String),
    Str(String),
    Open,
    Close,
    Comma,
    Equals,
}

fn parse_platform(s: &str) -> Result<Platform, String> {
    let s = s.trim();
    if let Some(inner) = s.strip_prefix("cfg(").and_then(|r| r.strip_suffix(')')) {
        let tokens = cfg_tokens(inner)?;
        let mut at = 0;
        let cfg = cfg_predicate(&tokens, &mut at)?;
        if at != tokens.len() {
            return Err(format!("`{s}`: something after its predicate"));
        }
        return Ok(Platform::Cfg(cfg));
    }
    if !s.is_empty() && s.chars().all(|c| c.is_ascii_alphanumeric() || "-_.".contains(c)) {
        return Ok(Platform::Triple(s.to_owned()));
    }
    Err(format!("`{s}` is neither a cfg(...) nor a target triple"))
}

fn cfg_tokens(s: &str) -> Result<Vec<CfgToken>, String> {
    let mut out = Vec::new();
    let mut chars = s.chars().peekable();
    while let Some(&c) = chars.peek() {
        match c {
            c if c.is_whitespace() => {
                chars.next();
            }
            '(' | ')' | ',' | '=' => {
                chars.next();
                out.push(match c {
                    '(' => CfgToken::Open,
                    ')' => CfgToken::Close,
                    ',' => CfgToken::Comma,
                    _ => CfgToken::Equals,
                });
            }
            '"' => {
                chars.next();
                let mut text = String::new();
                loop {
                    match chars.next() {
                        Some('"') => break,
                        Some('\\') => text.extend(chars.next()),
                        Some(c) => text.push(c),
                        None => return Err(format!("`{s}`: a string without its end")),
                    }
                }
                out.push(CfgToken::Str(text));
            }
            c if c.is_ascii_alphabetic() || c == '_' => {
                let mut name = String::new();
                while let Some(&c) = chars.peek() {
                    if !(c.is_ascii_alphanumeric() || c == '_') {
                        break;
                    }
                    name.push(c);
                    chars.next();
                }
                out.push(CfgToken::Ident(name));
            }
            other => return Err(format!("`{s}`: unexpected `{other}`")),
        }
    }
    Ok(out)
}

fn cfg_predicate(tokens: &[CfgToken], at: &mut usize) -> Result<Cfg, String> {
    let Some(CfgToken::Ident(name)) = tokens.get(*at) else {
        return Err(format!("expected a name at token {}", *at + 1));
    };
    *at += 1;
    match tokens.get(*at) {
        Some(CfgToken::Open) => {
            *at += 1;
            let mut list = Vec::new();
            loop {
                if tokens.get(*at) == Some(&CfgToken::Close) {
                    *at += 1;
                    break;
                }
                list.push(cfg_predicate(tokens, at)?);
                match tokens.get(*at) {
                    Some(CfgToken::Comma) => *at += 1,
                    Some(CfgToken::Close) => {
                        *at += 1;
                        break;
                    }
                    _ => return Err(format!("expected `,` or `)` in {name}(...)")),
                }
            }
            match name.as_str() {
                "all" => Ok(Cfg::All(list)),
                "any" => Ok(Cfg::Any(list)),
                "not" if list.len() == 1 => Ok(Cfg::Not(Box::new(list.remove(0)))),
                "not" => Err(format!("not(...) takes one predicate, not {}", list.len())),
                other => Err(format!("unknown cfg operator {other}(...)")),
            }
        }
        Some(CfgToken::Equals) => {
            *at += 1;
            match tokens.get(*at) {
                Some(CfgToken::Str(value)) => {
                    *at += 1;
                    Ok(Cfg::Eq(name.clone(), value.clone()))
                }
                _ => Err(format!("{name} = needs a string")),
            }
        }
        _ => Ok(Cfg::Is(name.clone())),
    }
}

// ----------------------------------------------------------------------------- the closure

/// Each third-party package the roots link, by id, with the indices of the targets it is built
/// for. A dependency counts on a target when one of its kinds is normal and its condition, if
/// any, holds there; workspace members are CITAR's own and are walked through, not listed.
fn closure(
    meta: &Metadata,
    roots: &[&str],
    targets: &[Target],
) -> Result<BTreeMap<String, BTreeSet<usize>>, Stop> {
    let resolve = meta
        .resolve
        .as_ref()
        .ok_or_else(|| cannot("cargo metadata has no resolve (was it run with --no-deps?)"))?;
    let nodes: BTreeMap<&str, &Node> = resolve.nodes.iter().map(|n| (n.id.as_str(), n)).collect();
    let members: BTreeSet<&str> = meta.workspace_members.iter().map(String::as_str).collect();
    let mut start = Vec::new();
    for name in roots {
        let p = meta
            .packages
            .iter()
            .find(|p| p.name == *name && members.contains(p.id.as_str()))
            .ok_or_else(|| cannot(format!("no workspace member named {name}")))?;
        start.push(p.id.as_str());
    }
    let mut platforms: BTreeMap<&str, Platform> = BTreeMap::new();
    for node in &resolve.nodes {
        for dep in &node.deps {
            if dep.dep_kinds.is_empty() {
                return Err(cannot(format!(
                    "cargo metadata names no dependency kinds for {}'s dependency on {} (it needs \
                     cargo 1.41 or later)",
                    node.id, dep.pkg
                )));
            }
            for kind in &dep.dep_kinds {
                if let Some(t) = &kind.target
                    && !platforms.contains_key(t.as_str())
                {
                    let p = parse_platform(t).map_err(|e| {
                        cannot(format!("{}'s dependency on {}: {e}", node.id, dep.pkg))
                    })?;
                    platforms.insert(t, p);
                }
            }
        }
    }
    let mut out: BTreeMap<String, BTreeSet<usize>> = BTreeMap::new();
    for (index, target) in targets.iter().enumerate() {
        let mut seen: BTreeSet<&str> = BTreeSet::new();
        let mut queue: VecDeque<&str> = start.iter().copied().collect();
        while let Some(id) = queue.pop_front() {
            if !seen.insert(id) {
                continue;
            }
            let node = nodes
                .get(id)
                .ok_or_else(|| cannot(format!("cargo metadata's resolve has no node {id}")))?;
            for dep in &node.deps {
                let linked = dep.dep_kinds.iter().any(|k| {
                    k.kind.is_none()
                        && k.target.as_deref().is_none_or(|t| target.matches(&platforms[t]))
                });
                if linked {
                    queue.push_back(dep.pkg.as_str());
                }
            }
        }
        for id in seen {
            if !members.contains(id) {
                out.entry(id.to_owned()).or_default().insert(index);
            }
        }
    }
    Ok(out)
}

// ------------------------------------------------------------------------------- the licences

/// An SPDX licence expression.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Expr {
    Id(String),
    With(String, String),
    And(Box<Expr>, Box<Expr>),
    Or(Box<Expr>, Box<Expr>),
}

fn parse_expression(s: &str) -> Result<Expr, String> {
    let mut tokens = Vec::new();
    for word in s.replace('(', " ( ").replace(')', " ) ").replace('/', " OR ").split_whitespace() {
        tokens.push(word.to_owned());
    }
    if tokens.is_empty() {
        return Err("it is empty".to_owned());
    }
    let mut at = 0;
    let e = expr_or(&tokens, &mut at)?;
    if at != tokens.len() {
        return Err(format!("`{}` after a whole expression", tokens[at]));
    }
    Ok(e)
}

fn is_op(word: &str, op: &str) -> bool {
    word.eq_ignore_ascii_case(op)
}

fn expr_or(tokens: &[String], at: &mut usize) -> Result<Expr, String> {
    let mut left = expr_and(tokens, at)?;
    while tokens.get(*at).is_some_and(|w| is_op(w, "OR")) {
        *at += 1;
        left = Expr::Or(Box::new(left), Box::new(expr_and(tokens, at)?));
    }
    Ok(left)
}

fn expr_and(tokens: &[String], at: &mut usize) -> Result<Expr, String> {
    let mut left = expr_with(tokens, at)?;
    while tokens.get(*at).is_some_and(|w| is_op(w, "AND")) {
        *at += 1;
        left = Expr::And(Box::new(left), Box::new(expr_with(tokens, at)?));
    }
    Ok(left)
}

fn expr_with(tokens: &[String], at: &mut usize) -> Result<Expr, String> {
    let word = tokens.get(*at).ok_or("it ends where a licence was expected")?;
    let e = if word == "(" {
        *at += 1;
        let inner = expr_or(tokens, at)?;
        if tokens.get(*at).map(String::as_str) != Some(")") {
            return Err("a `(` without its `)`".to_owned());
        }
        *at += 1;
        inner
    } else if word == ")" || ["AND", "OR", "WITH"].iter().any(|op| is_op(word, op)) {
        return Err(format!("`{word}` where a licence was expected"));
    } else {
        *at += 1;
        Expr::Id(word.clone())
    };
    if tokens.get(*at).is_some_and(|w| is_op(w, "WITH")) {
        // SPDX: an exception follows one licence, never a parenthesised expression.
        let after_group = tokens.get(*at - 1).map(String::as_str) == Some(")");
        *at += 1;
        let (Expr::Id(id), Some(exception), false) = (&e, tokens.get(*at), after_group) else {
            return Err("WITH must follow a licence and name an exception".to_owned());
        };
        if exception == "("
            || exception == ")"
            || ["AND", "OR", "WITH"].iter().any(|op| is_op(exception, op))
        {
            return Err(format!("`{exception}` where an exception was expected"));
        }
        *at += 1;
        return Ok(Expr::With(id.clone(), exception.clone()));
    }
    Ok(e)
}

/// A licence the allow-list takes; `+` ("or any later version") takes the version named.
fn allowed_id(id: &str) -> bool {
    ALLOWED.contains(&id.trim_end_matches('+'))
}

/// Whether the expression can be met with allowed licences alone.
fn allowed(e: &Expr) -> bool {
    match e {
        Expr::Id(id) => allowed_id(id),
        Expr::With(id, exception) => {
            ALLOWED_EXCEPTIONS.iter().any(|(l, x)| *l == id.trim_end_matches('+') && x == exception)
        }
        Expr::And(a, b) => allowed(a) && allowed(b),
        Expr::Or(a, b) => allowed(a) || allowed(b),
    }
}

/// Every licence (or licence with its exception) in the expression that the allow-list refuses.
fn refused_parts(e: &Expr, out: &mut BTreeSet<String>) {
    match e {
        Expr::Id(id) if !allowed_id(id) => {
            out.insert(id.clone());
        }
        Expr::With(..) if !allowed(e) => {
            if let Expr::With(id, exception) = e {
                out.insert(format!("{id} WITH {exception}"));
            }
        }
        Expr::And(a, b) | Expr::Or(a, b) => {
            refused_parts(a, out);
            refused_parts(b, out);
        }
        _ => {}
    }
}

/// Whether taking `id` alone meets the expression.
fn met_by(e: &Expr, id: &str) -> bool {
    match e {
        Expr::Id(x) => x.trim_end_matches('+') == id,
        Expr::With(..) => false,
        Expr::And(a, b) => met_by(a, id) && met_by(b, id),
        Expr::Or(a, b) => met_by(a, id) || met_by(b, id),
    }
}

/// Why a package's licence is refused, or `None` when the allow-list takes it.
fn licence_refusal(p: &Package) -> Option<String> {
    let Some(expression) = p.license.as_deref() else {
        let file = p.license_file.as_deref().unwrap_or("none");
        return Some(format!(
            "{} {} declares no licence expression (its licence file: {file}), so the allow-list \
             cannot take it",
            p.name, p.version
        ));
    };
    match parse_expression(expression) {
        Err(e) => {
            Some(format!("{} {}'s licence `{expression}` does not read: {e}", p.name, p.version))
        }
        Ok(e) if allowed(&e) => None,
        Ok(e) => {
            let mut parts = BTreeSet::new();
            refused_parts(&e, &mut parts);
            let parts: Vec<String> = parts.into_iter().collect();
            Some(format!(
                "{} {} is licensed `{expression}`, which needs {} (outside the allow-list, \
                 xtask/src/licenses.rs ALLOWED)",
                p.name,
                p.version,
                parts.join(", ")
            ))
        }
    }
}

// --------------------------------------------------------------------------------- the texts

/// One licence or notice text, as printed.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Text {
    /// Its file's path in the package, `/` separated, or the standard text's heading.
    name: String,
    body: String,
}

/// A text with LF line endings, no byte-order mark, no blank lines before its first and one final
/// newline: what a checkout of the file holds on every OS (`.gitattributes` keeps it LF), whatever
/// the crate packaged, and the same bytes for the same licence whether or not its copy began with
/// an empty line (apache.org's does). Leading spaces of the first line stay: they centre a title.
fn normalise(bytes: &[u8]) -> String {
    let text = String::from_utf8_lossy(bytes);
    let text = text.strip_prefix('\u{feff}').unwrap_or(&text);
    let text = text.replace("\r\n", "\n").replace('\r', "\n");
    let mut start = 0;
    for line in text.split_inclusive('\n') {
        if !line.trim().is_empty() {
            break;
        }
        start += line.len();
    }
    format!("{}\n", text[start..].trim_end())
}

/// Whether a file named `name` (lower case) is a licence or notice text: in a licence folder, any
/// file but code; elsewhere, one whose name starts as a licence's does.
fn is_text_name(lower: &str, in_licence_folder: bool) -> bool {
    let code = Path::new(lower)
        .extension()
        .is_some_and(|x| NOT_TEXTS.contains(&x.to_string_lossy().as_ref()));
    !code && (in_licence_folder || PREFIXES.iter().any(|p| lower.starts_with(p)))
}

/// Adds to `out` the paths, from the package's top with `/` separators, of the licence and notice
/// files in `dir` (at `rel` in the package) and every folder below it but [`NOT_SHIPPED`] ones.
fn find_texts(
    dir: &Path,
    rel: &str,
    in_licence_folder: bool,
    out: &mut Vec<String>,
) -> Result<(), String> {
    let mut entries = Vec::new();
    for entry in std::fs::read_dir(dir).map_err(|e| format!("{}: {e}", dir.display()))? {
        let entry = entry.map_err(|e| format!("{}: {e}", dir.display()))?;
        let kind = entry.file_type().map_err(|e| format!("{}: {e}", dir.display()))?;
        entries.push((entry.file_name().to_string_lossy().into_owned(), kind.is_dir()));
    }
    entries.sort();
    for (name, is_dir) in entries {
        let lower = name.to_ascii_lowercase();
        let path = if rel.is_empty() { name.clone() } else { format!("{rel}/{name}") };
        if is_dir {
            if !NOT_SHIPPED.contains(&lower.as_str()) {
                let inside = in_licence_folder || FOLDERS.contains(&lower.as_str());
                find_texts(&dir.join(&name), &path, inside, out)?;
            }
        } else if is_text_name(&lower, in_licence_folder) {
            out.push(path);
        }
    }
    Ok(())
}

/// The licence and notice files of the package in `dir`, in path order: those whose names say
/// so and every file of a licence folder, at its top or below it (a bundled library's notices
/// sit with its sources), outside its tests, benchmarks, examples and CI; and its `license-file`
/// if it names another. An empty file is no text.
fn licence_files(dir: &Path, license_file: Option<&str>) -> Result<Vec<Text>, String> {
    let read = |rel: &str| -> Result<Text, String> {
        let bytes = std::fs::read(dir.join(rel))
            .map_err(|e| format!("{}: {e}", dir.join(rel).display()))?;
        Ok(Text { name: rel.to_owned(), body: normalise(&bytes) })
    };
    let mut paths = Vec::new();
    find_texts(dir, "", false, &mut paths)?;
    let mut out = paths.iter().map(|rel| read(rel)).collect::<Result<Vec<_>, _>>()?;
    if let Some(file) = license_file {
        let rel = file.replace('\\', "/");
        let rel = rel.trim_start_matches("./");
        if !out.iter().any(|t| t.name == rel) {
            out.push(read(rel)?);
        }
    }
    out.retain(|t| t.body != "\n");
    out.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(out)
}

/// One crate, as the file shows it.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Entry {
    name: String,
    version: String,
    expression: String,
    /// Where it comes from: its crates.io page, or the source cargo names.
    source: String,
    repository: Option<String>,
    /// The targets it is built for, when not all of them.
    only_on: Option<Vec<String>>,
    texts: Vec<Text>,
    /// The licence whose standard text stands in for files its package does not ship.
    standard: Option<&'static str>,
}

impl Entry {
    fn read(p: &Package, built_for: &BTreeSet<usize>, targets: &[Target]) -> Result<Entry, Stop> {
        let dir = Path::new(&p.manifest_path).parent().ok_or_else(|| {
            cannot(format!("{} {}: its manifest path has no folder", p.name, p.version))
        })?;
        let mut texts = licence_files(dir, p.license_file.as_deref())
            .map_err(|e| cannot(format!("{} {}'s licence files: {e}", p.name, p.version)))?;
        let expression = p.license.clone().unwrap_or_default();
        let mut standard = None;
        if texts.is_empty() {
            let e = parse_expression(&expression).map_err(|e| problem(e.to_string()))?;
            let Some((id, text)) = STANDARD_TEXTS.iter().find(|(id, _)| met_by(&e, id)) else {
                return Err(problem(format!(
                    "{} {} ships no licence file, and `{expression}` is not met by a licence \
                     whose standard text xtask holds ({}): add one under xtask/licenses/",
                    p.name,
                    p.version,
                    STANDARD_TEXTS.iter().map(|(id, _)| *id).collect::<Vec<_>>().join(", ")
                )));
            };
            standard = Some(*id);
            texts.push(Text {
                name: format!("{id} (the licence's standard text)"),
                body: normalise(text.as_bytes()),
            });
        }
        let only_on = (built_for.len() < targets.len())
            .then(|| built_for.iter().map(|&i| targets[i].triple.clone()).collect());
        let source = match p.source.as_deref() {
            Some(s) if CRATES_IO.contains(&s) => {
                format!("https://crates.io/crates/{}/{}", p.name, p.version)
            }
            Some(s) => s.to_owned(),
            None => "a path in this repository".to_owned(),
        };
        Ok(Entry {
            name: p.name.clone(),
            version: p.version.clone(),
            expression,
            source,
            repository: p.repository.clone(),
            only_on,
            texts,
            standard,
        })
    }

    fn label(&self) -> String {
        format!("{} {}", self.name, self.version)
    }
}

/// Every third-party crate the release links, read and checked.
#[derive(Debug)]
struct Notices {
    targets: Vec<String>,
    entries: Vec<Entry>,
}

impl Notices {
    fn gather(meta: &Metadata, roots: &[&str], targets: &[Target]) -> Result<Notices, Stop> {
        let linked = closure(meta, roots, targets)?;
        let packages: BTreeMap<&str, &Package> =
            meta.packages.iter().map(|p| (p.id.as_str(), p)).collect();
        let mut chosen = Vec::new();
        for (id, built_for) in &linked {
            let p = packages
                .get(id.as_str())
                .ok_or_else(|| cannot(format!("cargo metadata lists no package {id}")))?;
            chosen.push((*p, built_for));
        }
        chosen.sort_by(|(a, _), (b, _)| {
            let order = |p: &Package| (p.name.clone(), semver::Version::parse(&p.version).ok());
            order(a).cmp(&order(b)).then_with(|| a.version.cmp(&b.version)).then(a.id.cmp(&b.id))
        });
        let refusals: Vec<String> = chosen.iter().filter_map(|(p, _)| licence_refusal(p)).collect();
        if !refusals.is_empty() {
            return Err(problem(format!(
                "{} crate(s) the release links are under a licence it may not ship:\n  {}",
                refusals.len(),
                refusals.join("\n  ")
            )));
        }
        let entries = chosen
            .iter()
            .map(|(p, built_for)| Entry::read(p, built_for, targets))
            .collect::<Result<Vec<_>, _>>()?;
        Ok(Notices { targets: targets.iter().map(|t| t.triple.clone()).collect(), entries })
    }

    fn summary(&self) -> String {
        let texts: usize = self.entries.iter().map(|e| e.texts.len()).sum();
        let distinct: BTreeSet<&str> =
            self.entries.iter().flat_map(|e| e.texts.iter().map(|t| t.body.as_str())).collect();
        let standard = self.entries.iter().filter(|e| e.standard.is_some()).count();
        format!(
            "{} crates on {} targets, {texts} licence and notice texts ({} distinct, each printed \
             once), {standard} standard text(s) for packages that ship none",
            self.entries.len(),
            self.targets.len(),
            distinct.len()
        )
    }

    fn render(&self) -> String {
        let targets = match self.targets.as_slice() {
            [] => String::new(),
            [one] => one.clone(),
            [rest @ .., last] => format!("{} and {last}", rest.join(", ")),
        };
        let paragraphs = [
            format!(
                "CITAR's own code is MPL-2.0 (LICENSE and NOTICE.md). Its Python extension, \
                 citar._engine (the citar-py crate), and its helper, citar-helper, statically \
                 link the third-party Rust crates below: the normal dependencies of the two on \
                 the release targets, {targets}, as CITAR's Cargo.lock pins them. The list errs \
                 on the side of more: it takes the crate features the whole workspace turns on, \
                 its tests' included, so it may name a crate no release build compiles in."
            ),
            "Each crate is listed once, in name order, with its version, the licence expression \
             its package declares, the targets it is built for when not all of them, and the \
             licence and notice files its package ships, those of code it bundles included, as \
             they are in its published sources. \
             A text byte for byte the same as one printed earlier is not printed again: its \
             heading names that one. Where a package ships no licence file, the standard text of \
             a licence it is offered under stands in, marked so."
                .to_owned(),
            "The Python packages CITAR depends on are installed separately, each with its own \
             licence; the Windows bundle lists those it ships in PYTHON-LICENSES.txt."
                .to_owned(),
            "This file is generated by `cargo xtask licenses` from CITAR's Cargo.lock and the \
             crates' published sources, and is not edited by hand."
                .to_owned(),
        ];
        let mut s = String::from("CITAR: third-party licences\n===========================\n");
        for p in &paragraphs {
            s.push('\n');
            s.push_str(&wrap(p, 78));
        }
        s.push_str(&format!("\nThe crates ({}):\n\n", self.entries.len()));
        for e in &self.entries {
            s.push_str(&format!("  {}: {}\n", e.label(), e.expression));
        }
        let mut first: BTreeMap<&str, String> = BTreeMap::new();
        for e in &self.entries {
            s.push_str(&format!(
                "\n{RULE}\n{}\nLicence: {}\nSource: {}\n",
                e.label(),
                e.expression,
                e.source
            ));
            if let Some(repository) = &e.repository {
                s.push_str(&format!("Repository: {repository}\n"));
            }
            if let Some(only) = &e.only_on {
                s.push_str(&format!("Targets: {}\n", only.join(", ")));
            }
            match e.standard {
                Some(id) => s.push_str(&format!(
                    "Files: none in its package; the {id} licence's standard text stands in\n"
                )),
                None => {
                    let names: Vec<&str> = e.texts.iter().map(|t| t.name.as_str()).collect();
                    s.push_str(&format!("Files: {}\n", names.join(", ")));
                }
            }
            for t in &e.texts {
                let here = format!("{}: {}", e.label(), t.name);
                match first.get(t.body.as_str()) {
                    Some(earlier) => {
                        s.push_str(&format!("\n---- {here}: the same text as {earlier} ----\n"));
                    }
                    None => {
                        s.push_str(&format!("\n---- {here} ----\n\n{}", t.body));
                        first.insert(t.body.as_str(), here);
                    }
                }
            }
        }
        s
    }
}

/// `text` in lines of at most `width` characters (a longer word on a line of its own), each ending
/// in a newline.
fn wrap(text: &str, width: usize) -> String {
    let mut out = String::new();
    let mut line = String::new();
    for word in text.split_whitespace() {
        if !line.is_empty() && line.chars().count() + 1 + word.chars().count() > width {
            out.push_str(&line);
            out.push('\n');
            line.clear();
        }
        if !line.is_empty() {
            line.push(' ');
        }
        line.push_str(word);
    }
    if !line.is_empty() {
        out.push_str(&line);
        out.push('\n');
    }
    out
}

/// What differs between the file found and the one expected: the crates added and removed (the
/// index lines), else the first line that differs.
fn difference(found: &str, expected: &str) -> String {
    fn index(text: &str) -> BTreeSet<&str> {
        text.lines().take_while(|l| *l != RULE).filter(|l| l.starts_with("  ")).collect()
    }
    let (old, new) = (index(found), index(expected));
    let mut out = Vec::new();
    out.extend(new.difference(&old).map(|l| format!("  added:   {}", l.trim())));
    out.extend(old.difference(&new).map(|l| format!("  removed: {}", l.trim())));
    if out.is_empty() {
        let mut found_lines = found.lines();
        for (n, want) in expected.lines().enumerate() {
            let got = found_lines.next();
            if got != Some(want) {
                out.push(format!(
                    "  line {}: expected `{want}`, found `{}`",
                    n + 1,
                    got.unwrap_or("(the end of the file)")
                ));
                break;
            }
        }
        if out.is_empty() {
            out.push("  the file is longer than expected, or its line endings differ".to_owned());
        }
    }
    out.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn target(triple: &str, cfg: &str) -> Target {
        Target::from_print(triple, cfg)
    }

    fn windows() -> Target {
        target(
            "x86_64-pc-windows-msvc",
            "panic=\"unwind\"\ntarget_arch=\"x86_64\"\ntarget_env=\"msvc\"\n\
             target_family=\"windows\"\ntarget_os=\"windows\"\ntarget_vendor=\"pc\"\nwindows\n",
        )
    }

    fn linux() -> Target {
        target(
            "x86_64-unknown-linux-gnu",
            "target_arch=\"x86_64\"\ntarget_env=\"gnu\"\ntarget_family=\"unix\"\n\
             target_os=\"linux\"\ntarget_vendor=\"unknown\"\nunix\n",
        )
    }

    fn holds(t: &Target, platform: &str) -> bool {
        t.matches(&parse_platform(platform).unwrap())
    }

    #[test]
    fn cfg_conditions_are_read_and_held_to_a_target() {
        let (w, l) = (windows(), linux());
        assert!(holds(&w, "cfg(windows)") && !holds(&l, "cfg(windows)"));
        assert!(holds(&l, "cfg(unix)") && !holds(&w, "cfg(unix)"));
        assert!(holds(&w, "x86_64-pc-windows-msvc") && !holds(&w, "x86_64-pc-windows-gnu"));
        assert!(holds(
            &l,
            "cfg(all(unix, not(target_os = \"android\"), not(target_vendor = \"apple\")))"
        ));
        assert!(!holds(&l, "cfg(all(target_arch = \"wasm32\", target_os = \"unknown\"))"));
        assert!(holds(
            &w,
            "cfg(not(all(target_arch = \"wasm32\", any(target_os = \"unknown\", target_os = \"none\"))))"
        ));
        // A cfg no release build sets (a --cfg flag) never holds; an empty any() never, all() always.
        assert!(!holds(
            &w,
            "cfg(all(target_arch = \"x86_64\", target_env = \"msvc\", windows_raw_dylib))"
        ));
        assert!(holds(
            &w,
            "cfg(all(any(target_arch = \"x86_64\", target_arch = \"arm64ec\"), target_env = \"msvc\", not(windows_raw_dylib)))"
        ));
        assert!(!holds(&w, "cfg(any())") && holds(&w, "cfg(all())"));
        assert!(holds(&l, "cfg(all(target_os = \"linux\", target_env = \"gnu\",))"));
    }

    #[test]
    fn a_condition_that_does_not_read_is_refused() {
        for bad in [
            "cfg(windows",
            "cfg(not(unix, windows))",
            "cfg(target_os = linux)",
            "cfg(maybe(unix))",
            "cfg(unix) extra",
            "x86 64",
            "cfg(target_os = \"linux)",
        ] {
            assert!(parse_platform(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn rustc_print_cfg_is_read_as_names_and_pairs() {
        let t = target(
            "t",
            "debug_assertions\ntarget_has_atomic=\"64\"\n\ntarget_os=\"macos\"\nunix\n",
        );
        assert!(t.cfg.contains(&("unix".to_owned(), None)));
        assert!(t.cfg.contains(&("target_os".to_owned(), Some("macos".to_owned()))));
        assert!(t.cfg.contains(&("target_has_atomic".to_owned(), Some("64".to_owned()))));
        assert_eq!(t.cfg.len(), 4);
    }

    #[test]
    fn licence_expressions_are_held_to_the_allow_list() {
        let takes = |s: &str| allowed(&parse_expression(s).unwrap());
        for ok in [
            "MIT",
            "MIT OR Apache-2.0",
            "MIT/Apache-2.0",
            "(MIT OR Apache-2.0) AND Unicode-3.0",
            "Apache-2.0 AND ISC",
            "Apache-2.0 WITH LLVM-exception OR Apache-2.0 OR MIT",
            "MIT OR Apache-2.0 OR LGPL-2.1-or-later",
            "Unlicense OR MIT",
            "CC0-1.0 OR MIT-0 OR Apache-2.0",
            "CDLA-Permissive-2.0",
            "Zlib OR Apache-2.0 OR MIT",
            "BSD-3-Clause",
            "MIT or Apache-2.0",
            "Apache-2.0+",
        ] {
            assert!(takes(ok), "{ok}");
        }
        for refused in [
            "GPL-3.0-only",
            "MIT AND GPL-3.0-only",
            "(MIT OR Apache-2.0) AND LGPL-2.1-or-later",
            "Apache-2.0 WITH Classpath-exception-2.0",
            "MIT WITH LLVM-exception",
            "Unlicense",
            "MIT-0",
        ] {
            assert!(!takes(refused), "{refused}");
        }
        let mut parts = BTreeSet::new();
        refused_parts(
            &parse_expression("(MIT OR GPL-2.0+) AND AGPL-3.0-only").unwrap(),
            &mut parts,
        );
        assert_eq!(parts.into_iter().collect::<Vec<_>>(), ["AGPL-3.0-only", "GPL-2.0+"]);
    }

    #[test]
    fn an_expression_that_does_not_read_is_refused() {
        for bad in [
            "",
            "MIT OR",
            "(MIT",
            "MIT)",
            "AND MIT",
            "MIT Apache-2.0",
            "(MIT) WITH X",
            "Apache-2.0 WITH",
            "Apache-2.0 WITH OR MIT",
            "Apache-2.0 WITH (LLVM-exception)",
        ] {
            assert!(parse_expression(bad).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn a_standard_text_meets_an_expression_only_alone() {
        let met = |s: &str| met_by(&parse_expression(s).unwrap(), "Apache-2.0");
        assert!(met("Zlib OR Apache-2.0 OR MIT") && met("Apache-2.0"));
        assert!(
            !met("Apache-2.0 AND ISC") && !met("MIT") && !met("Apache-2.0 WITH LLVM-exception")
        );
    }

    #[test]
    fn texts_are_read_with_lf_endings_whatever_was_packaged() {
        assert_eq!(normalise(b"\xef\xbb\xbfa\r\nb\rc\n\n\n"), "a\nb\nc\n");
        assert_eq!(normalise(b"one line"), "one line\n");
        assert_eq!(normalise(b"bad \xff byte\r\n"), "bad \u{fffd} byte\n");
        // Blank lines before the first go; the first line's indent, which centres a title, stays.
        assert_eq!(normalise(b"\n  \r\n     Title\n\nbody\n"), "     Title\n\nbody\n");
        assert_eq!(normalise(b" \n\n"), "\n");
    }

    #[test]
    fn the_standard_apache_text_is_apache_orgs() {
        use sha2::{Digest as _, Sha256};
        // https://www.apache.org/licenses/LICENSE-2.0.txt, whose SHA-256 this is: its copy here is
        // byte for byte that file (a checkout with CRLF endings read back as LF).
        let (id, text) = STANDARD_TEXTS[0];
        assert_eq!(id, "Apache-2.0");
        let bytes = text.replace("\r\n", "\n");
        let digest: String =
            Sha256::digest(bytes.as_bytes()).iter().map(|b| format!("{b:02x}")).collect();
        assert_eq!(digest, "cfc7749b96f63bd31c3c42b5c471bf756814053e847c10f3eb003417bc523d30");
        assert!(
            normalise(text.as_bytes())
                .starts_with("                                 Apache License\n")
        );
    }

    fn scratch(name: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("xtask-licenses-{name}-{}", std::process::id()));
        let _gone = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("scratch dir");
        dir
    }

    #[test]
    fn a_packages_licence_files_are_found_by_name_and_folder() {
        let dir = scratch("files");
        for (name, text) in [
            ("LICENSE-MIT", "mit\r\n"),
            ("license-apache-2.0", "apache"),
            ("COPYRIGHT", "c"),
            ("NOTICE", "n"),
            ("UNLICENSE", "u"),
            ("license.rs", "fn main() {}"),
            ("README.md", "read me"),
            ("Cargo.toml", "[package]"),
            ("custom.txt", "custom"),
        ] {
            std::fs::write(dir.join(name), text).unwrap();
        }
        std::fs::create_dir_all(dir.join("LICENSES")).unwrap();
        std::fs::write(dir.join("LICENSES/MIT.txt"), "reuse mit").unwrap();
        std::fs::create_dir_all(dir.join("license-tools")).unwrap();
        let found = licence_files(&dir, Some("custom.txt")).unwrap();
        let names: Vec<&str> = found.iter().map(|t| t.name.as_str()).collect();
        assert_eq!(
            names,
            [
                "COPYRIGHT",
                "LICENSE-MIT",
                "LICENSES/MIT.txt",
                "NOTICE",
                "UNLICENSE",
                "custom.txt",
                "license-apache-2.0"
            ]
        );
        assert_eq!(found[1].body, "mit\n");
        // A license-file the package does not hold cannot be read.
        assert!(licence_files(&dir, Some("gone.txt")).is_err());
        let _gone = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn licence_files_below_the_top_are_found_but_not_in_tests_or_code() {
        let dir = scratch("nested");
        for (path, text) in [
            ("LICENSE-MIT", "top mit"),
            // A bundled library's notices, with its sources (ring, zstd-sys, tracing-core).
            ("third_party/fiat/LICENSE", "fiat"),
            ("zstd/COPYING", "gpl option"),
            ("zstd/LICENSE", "bsd"),
            ("src/spin/LICENSE", "spin"),
            // A folder of licence texts, wherever it is; its code is still code.
            ("license/MIT.txt", "folder mit"),
            ("vendor/x/LICENSES/Apache-2.0.txt", "reuse apache"),
            ("src/license/mod.rs", "pub mod x;"),
            ("src/license/table.c", "int x;"),
            // A folder only named like one holds code (the windows crates' module).
            ("src/Win32/LicenseProtection/mod.rs", "pub fn f() {}"),
            ("src/Win32/LicenseProtection/notes.txt", "not a licence"),
            // Not linked: tests, benchmarks, examples and CI, wherever they are.
            ("tests/data/LICENSE", "test fixture"),
            ("benches/COPYING", "bench"),
            ("examples/demo/NOTICE", "example"),
            (".github/LICENSE", "ci"),
            ("vendor/x/tests/LICENSE", "vendored test fixture"),
            ("vendor/x/notice.c", "int notice;"),
        ] {
            let file = dir.join(path);
            std::fs::create_dir_all(file.parent().unwrap()).unwrap();
            std::fs::write(file, text).unwrap();
        }
        let found = licence_files(&dir, None).unwrap();
        let names: Vec<&str> = found.iter().map(|t| t.name.as_str()).collect();
        assert_eq!(
            names,
            [
                "LICENSE-MIT",
                "license/MIT.txt",
                "src/spin/LICENSE",
                "third_party/fiat/LICENSE",
                "vendor/x/LICENSES/Apache-2.0.txt",
                "zstd/COPYING",
                "zstd/LICENSE"
            ]
        );
        assert_eq!(found[3].body, "fiat\n");
        let _gone = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_package_whose_only_licence_file_is_below_its_top_needs_no_standard_text() {
        let dir = scratch("below");
        let meta = metadata(
            &dir,
            &["citar-py", "citar-helper"],
            &[
                ("citar-py", "0.1.5", Some("MPL-2.0"), vec![("bare", None, None)]),
                ("citar-helper", "0.1.5", Some("MPL-2.0"), vec![]),
                ("bare", "1.0.0", Some("MIT"), vec![]),
            ],
        );
        // Its only text, in a licence folder: printed under its path, with no standard text (and
        // not refused, though MIT has none in xtask).
        let folder = dir.join("bare-1.0.0/license");
        std::fs::create_dir_all(&folder).unwrap();
        std::fs::write(folder.join("MIT.txt"), "MIT, by bare\n").unwrap();
        let text = Notices::gather(&meta, ROOTS, &[linux()]).unwrap().render();
        assert!(text.contains("\nFiles: license/MIT.txt\n"), "{text}");
        assert!(text.contains("---- bare 1.0.0: license/MIT.txt ----\n\nMIT, by bare\n"));
        assert!(!text.contains("standard text stands in"));
        let _gone = std::fs::remove_dir_all(&dir);
    }

    /// A metadata document: (name, version, licence, platform edges as (to, kind, target)).
    type Spec<'a> =
        (&'a str, &'a str, Option<&'a str>, Vec<(&'a str, Option<&'a str>, Option<&'a str>)>);

    fn metadata(dir: &Path, members: &[&str], specs: &[Spec<'_>]) -> Metadata {
        let id = |name: &str| format!("{name}-id");
        let mut packages = Vec::new();
        let mut nodes = Vec::new();
        for (name, version, licence, deps) in specs {
            let crate_dir = dir.join(format!("{name}-{version}"));
            std::fs::create_dir_all(&crate_dir).unwrap();
            if *name != "bare" {
                std::fs::write(crate_dir.join("LICENSE-MIT"), format!("MIT, by {name}\n")).unwrap();
                std::fs::write(crate_dir.join("LICENSE-APACHE"), "the Apache text\n").unwrap();
            }
            packages.push(serde_json::json!({
                "name": name, "version": version, "id": id(name), "license": licence,
                "manifest_path": crate_dir.join("Cargo.toml").to_string_lossy(),
                "source": if members.contains(name) { serde_json::Value::Null } else {
                    serde_json::json!(CRATES_IO[0]) },
                "repository": format!("https://example.com/{name}"),
            }));
            let deps: Vec<_> = deps
                .iter()
                .map(|(to, kind, target)| {
                    serde_json::json!({"pkg": id(to), "dep_kinds": [{"kind": kind, "target": target}]})
                })
                .collect();
            nodes.push(serde_json::json!({"id": id(name), "deps": deps}));
        }
        let doc = serde_json::json!({
            "packages": packages,
            "workspace_members": members.iter().map(|m| id(m)).collect::<Vec<_>>(),
            "resolve": {"nodes": nodes},
        });
        Metadata::parse(doc.to_string().as_bytes()).unwrap()
    }

    fn sample(dir: &Path, licence_of_zeta: &str) -> Metadata {
        metadata(
            dir,
            &["citar-py", "citar-helper", "citar-engine"],
            &[
                (
                    "citar-py",
                    "0.1.5",
                    Some("MPL-2.0"),
                    vec![("citar-engine", None, None), ("pyo3-build", Some("build"), None)],
                ),
                (
                    "citar-helper",
                    "0.1.5",
                    Some("MPL-2.0"),
                    vec![
                        ("citar-engine", None, None),
                        ("winonly", None, Some("cfg(windows)")),
                        ("unixonly", None, Some("cfg(unix)")),
                        ("wasmonly", None, Some("cfg(target_arch = \"wasm32\")")),
                        ("devonly", Some("dev"), None),
                    ],
                ),
                (
                    "citar-engine",
                    "0.1.5",
                    Some("MPL-2.0"),
                    vec![("zeta", None, None), ("alpha", None, None)],
                ),
                ("alpha", "1.10.0", Some("MIT OR Apache-2.0"), vec![]),
                ("zeta", "0.2.0", Some(licence_of_zeta), vec![]),
                // Windows-only, and its own Unix-only dependency never links anywhere.
                (
                    "winonly",
                    "0.52.0",
                    Some("MIT OR Apache-2.0"),
                    vec![("never", None, Some("cfg(unix)"))],
                ),
                ("unixonly", "0.2.1", Some("MIT"), vec![("bare", None, None)]),
                ("bare", "0.3.2", Some("Zlib OR Apache-2.0 OR MIT"), vec![]),
                ("never", "1.0.0", Some("MIT"), vec![]),
                ("wasmonly", "1.0.0", Some("MIT"), vec![]),
                ("devonly", "1.0.0", Some("GPL-3.0-only"), vec![]),
                ("pyo3-build", "1.0.0", Some("GPL-3.0-only"), vec![]),
            ],
        )
    }

    #[test]
    fn the_closure_is_the_normal_dependencies_on_each_target() {
        let dir = scratch("closure");
        let meta = sample(&dir, "MIT");
        let linked = closure(&meta, ROOTS, &[windows(), linux()]).unwrap();
        let names: BTreeMap<String, Vec<usize>> = linked
            .iter()
            .map(|(id, t)| (id.trim_end_matches("-id").to_owned(), t.iter().copied().collect()))
            .collect();
        let want: BTreeMap<String, Vec<usize>> = [
            ("alpha", vec![0, 1]),
            ("bare", vec![1]),
            ("unixonly", vec![1]),
            ("winonly", vec![0]),
            ("zeta", vec![0, 1]),
        ]
        .into_iter()
        .map(|(n, t)| (n.to_owned(), t))
        .collect();
        assert_eq!(names, want);
        let _gone = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_file_lists_each_crate_once_with_its_texts_in_name_order() {
        let dir = scratch("render");
        let notices = Notices::gather(&sample(&dir, "MIT"), ROOTS, &[windows(), linux()]).unwrap();
        let text = notices.render();
        let index: Vec<&str> =
            text.lines().take_while(|l| *l != RULE).filter(|l| l.starts_with("  ")).collect();
        assert_eq!(
            index,
            [
                "  alpha 1.10.0: MIT OR Apache-2.0",
                "  bare 0.3.2: Zlib OR Apache-2.0 OR MIT",
                "  unixonly 0.2.1: MIT",
                "  winonly 0.52.0: MIT OR Apache-2.0",
                "  zeta 0.2.0: MIT",
            ]
        );
        assert_eq!(text.matches(&format!("{RULE}\n")).count(), 5);
        let words = text.split_whitespace().collect::<Vec<_>>().join(" ");
        assert!(words.contains(
            "the release targets, x86_64-pc-windows-msvc and x86_64-unknown-linux-gnu, as"
        ));
        assert!(text.lines().take_while(|l| *l != RULE).all(|l| l.chars().count() <= 78));
        assert!(text.contains("\nwinonly 0.52.0\nLicence: MIT OR Apache-2.0\nSource: https://crates.io/crates/winonly/0.52.0\nRepository: https://example.com/winonly\nTargets: x86_64-pc-windows-msvc\nFiles: LICENSE-APACHE, LICENSE-MIT\n"));
        // Every crate is built for both but these two, so only they name targets.
        assert_eq!(text.matches("\nTargets: ").count(), 3);
        // A text printed once; the same bytes later name it.
        assert_eq!(text.matches("the Apache text").count(), 1);
        assert!(text.contains(
            "---- zeta 0.2.0: LICENSE-APACHE: the same text as alpha 1.10.0: LICENSE-APACHE ----"
        ));
        assert!(text.contains("---- zeta 0.2.0: LICENSE-MIT ----\n\nMIT, by zeta\n"));
        // A package without files gets the standard text of a licence that meets its expression.
        assert!(
            text.contains("\nbare 0.3.2\n")
                && text.contains(
                    "Files: none in its package; the Apache-2.0 licence's standard text stands in"
                )
        );
        assert!(
            text.contains("---- bare 0.3.2: Apache-2.0 (the licence's standard text) ----\n\n")
        );
        assert!(text.contains("Apache License") && text.contains("Version 2.0, January 2004"));
        // No CITAR crate, no path, no version of CITAR's own.
        assert!(
            !text.contains("citar-engine")
                && !text.contains("0.1.5")
                && !text.contains(&*dir.to_string_lossy())
        );
        assert!(!text.contains('\r'));
        // Written twice, the same bytes.
        let again =
            Notices::gather(&sample(&dir, "MIT"), ROOTS, &[windows(), linux()]).unwrap().render();
        assert_eq!(text, again);
        assert!(notices.summary().starts_with("5 crates on 2 targets"), "{}", notices.summary());
        let _gone = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_licence_outside_the_allow_list_is_refused_naming_the_crate() {
        let dir = scratch("refused");
        let Err(Stop(code, why)) =
            Notices::gather(&sample(&dir, "GPL-3.0-only"), ROOTS, &[windows(), linux()])
        else {
            panic!("a GPL crate was taken");
        };
        assert_eq!(code, 1);
        assert!(
            why.contains("zeta 0.2.0 is licensed `GPL-3.0-only`, which needs GPL-3.0-only"),
            "{why}"
        );
        // The dev-only and build-only GPL crates are not linked, so they are not named.
        assert!(!why.contains("devonly") && !why.contains("pyo3-build"), "{why}");
        let _gone = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_package_without_files_or_a_standard_text_is_refused() {
        let dir = scratch("bare");
        let meta = metadata(
            &dir,
            &["citar-py", "citar-helper"],
            &[
                ("citar-py", "0.1.5", Some("MPL-2.0"), vec![("bare", None, None)]),
                ("citar-helper", "0.1.5", Some("MPL-2.0"), vec![]),
                ("bare", "1.0.0", Some("MIT"), vec![]),
            ],
        );
        let Err(Stop(code, why)) = Notices::gather(&meta, ROOTS, &[linux()]) else {
            panic!("a crate without a licence text was taken");
        };
        assert_eq!(code, 1);
        assert!(why.contains("bare 1.0.0 ships no licence file"), "{why}");
        let _gone = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_difference_names_the_crates_added_and_removed() {
        let old = "head\n\n  alpha 1.0.0: MIT\n  beta 2.0.0: MIT\n\nRULE\n";
        let new = "head\n\n  alpha 1.0.0: MIT\n  beta 2.0.1: MIT\n\nRULE\n";
        let said = difference(old, new);
        assert_eq!(said, "  added:   beta 2.0.1: MIT\n  removed: beta 2.0.0: MIT");
        assert_eq!(difference("a\nb\n", "a\nc\n"), "  line 2: expected `c`, found `b`");
        assert_eq!(
            difference("a\n", "a\nb\n"),
            "  line 2: expected `b`, found `(the end of the file)`"
        );
    }

    #[test]
    fn paragraphs_wrap_at_the_width() {
        assert_eq!(wrap("a bb ccc dddd", 6), "a bb\nccc\ndddd\n");
        assert_eq!(wrap("averyveryverylongword x", 6), "averyveryverylongword\nx\n");
        assert_eq!(wrap("", 6), "");
    }

    #[test]
    fn its_arguments_are_read_and_others_refused() {
        let args = |a: &[&str]| options(&a.iter().map(|s| (*s).to_owned()).collect::<Vec<_>>());
        assert_eq!(
            args(&["--check", "--metadata", "m.json", "--out", "o.txt"]).unwrap(),
            Options {
                check: true,
                metadata: Some(PathBuf::from("m.json")),
                out: Some(PathBuf::from("o.txt"))
            }
        );
        assert_eq!(args(&["--metadata"]).unwrap_err().0, 2);
        assert_eq!(args(&["--chek"]).unwrap_err().0, 2);
    }

    #[test]
    fn the_targets_are_the_ones_package_yml_builds() {
        // The wheels' and the helpers' matrices in package.yml name the same five targets as
        // manifest::TARGETS, which this command reads them from.
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("..");
        let text = std::fs::read_to_string(root.join(".github/workflows/package.yml")).unwrap();
        let mut named: BTreeSet<&str> = BTreeSet::new();
        let mut legs = 0;
        for line in text.lines() {
            // A matrix leg's `target: <triple>`, on its own line or in a `{ ... }` one; the steps'
            // `target: ${{ matrix.target }}` is not a value.
            if line.trim_start().starts_with('#') {
                continue;
            }
            let Some(at) = line.find("target: ") else { continue };
            let value = line[at + "target: ".len()..].split([',', '}']).next().unwrap_or("").trim();
            if value.starts_with('$') || line[..at].ends_with('_') {
                continue;
            }
            legs += 1;
            named.insert(value.trim_matches('"'));
        }
        let ours: BTreeSet<&str> = TARGETS.iter().map(|(t, _)| *t).collect();
        assert_eq!(named, ours);
        // The wheels' five legs and the helpers' five.
        assert_eq!(legs, 10, "{named:?}");
    }

    #[test]
    fn the_real_closure_covers_every_release_target() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("..");
        let meta = Metadata::parse(&cargo_metadata(&root).unwrap()).unwrap();
        let targets: Vec<Target> =
            TARGETS.iter().map(|(t, _)| target_cfg(&root, t).unwrap()).collect();
        let linked = closure(&meta, ROOTS, &targets).unwrap();
        let by_name: BTreeMap<&str, Vec<&str>> = linked
            .iter()
            .map(|(id, on)| {
                let p = meta.packages.iter().find(|p| &p.id == id).unwrap();
                (p.name.as_str(), on.iter().map(|&i| targets[i].triple.as_str()).collect())
            })
            .collect();
        let on = |name: &str| by_name.get(name).cloned().unwrap_or_default();
        // Windows-only, Unix-only, Linux-only and macOS-only crates are in, each on its targets.
        let (windows, macs, linuxes): (Vec<&str>, Vec<&str>, Vec<&str>) = (
            vec!["x86_64-pc-windows-msvc"],
            vec!["aarch64-apple-darwin", "x86_64-apple-darwin"],
            vec!["x86_64-unknown-linux-gnu", "aarch64-unknown-linux-gnu"],
        );
        assert_eq!(on("windows-sys"), windows);
        assert_eq!(on("security-framework"), macs);
        assert_eq!(on("openssl-probe"), linuxes);
        assert_eq!(on("signal-hook-registry"), [macs.clone(), linuxes.clone()].concat());
        assert_eq!(on("libc").len(), 5);
        // Crates for targets CITAR does not build for are out.
        for absent in ["wasm-bindgen", "js-sys", "jni", "r-efi", "winapi-x86_64-pc-windows-gnu"] {
            assert!(!by_name.contains_key(absent), "{absent} is linked on {:?}", on(absent));
        }
        // Dev-only crates are out (proptest is a dev-dependency of the helper and the engine);
        // CITAR's own crates are walked through, never listed.
        assert!(!by_name.contains_key("proptest"));
        assert!(by_name.keys().all(|n| !n.starts_with("citar-")));
        assert!(by_name.contains_key("pyo3") && by_name.contains_key("tokio"));

        // An independent reading: every crate `cargo tree` builds for a target (cargo's own
        // per-target feature resolution, normal edges only) is in the closure on that target. The
        // closure may hold a few more, which only the workspace's unified features turn on.
        let cargo = std::env::var("CARGO").unwrap_or_else(|_| "cargo".to_owned());
        let versions: BTreeMap<(&str, &str), &BTreeSet<usize>> = linked
            .iter()
            .map(|(id, on)| {
                let p = meta.packages.iter().find(|p| &p.id == id).unwrap();
                ((p.name.as_str(), p.version.as_str()), on)
            })
            .collect();
        for (index, target) in targets.iter().enumerate() {
            let out = Command::new(&cargo)
                .current_dir(&root)
                .args(["tree", "--locked", "-e", "normal", "--target", &target.triple])
                .args([
                    "-p",
                    "citar-py",
                    "-p",
                    "citar-helper",
                    "--features",
                    "citar-helper/packaged",
                ])
                .args(["--prefix", "none", "--format", "{p}"])
                .output()
                .unwrap();
            assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
            let mut built = 0;
            for line in String::from_utf8_lossy(&out.stdout).lines() {
                let mut words = line.split_whitespace();
                let (Some(name), Some(version)) = (words.next(), words.next()) else { continue };
                if name.starts_with("citar-") {
                    continue;
                }
                let version = version.trim_start_matches('v');
                built += 1;
                assert!(
                    versions.get(&(name, version)).is_some_and(|on| on.contains(&index)),
                    "cargo tree builds {name} {version} for {}, which the closure misses",
                    target.triple
                );
            }
            assert!(built > 50, "cargo tree named {built} crates for {}", target.triple);
        }
    }
}
