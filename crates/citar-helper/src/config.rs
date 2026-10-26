//! The helper's configuration (design/phase4-helper.md P4.3.2): protocol 1's sources with their
//! meanings, and protocol 2's new settings.
//!
//! **Compatibility.** [`resolve`] ports `citar/worker/__main__.py` `build_config` (lines 107-153):
//! a file (`--config FILE`, else `helper.json` in the home), the `CITAR_WORKER_*` variables and
//! the flags, each setting taken from the first that has it, flag over file over environment; a
//! flag of `0` or `""` counts as not given, and so does a file value of `""` or `null`, as
//! `pick` had it. With nothing given anywhere and a person at the terminal, the first-run
//! questions ask for the server and token and offer to save them. The answers for a table of
//! configurations were recorded from the Python worker into `tests/data/helper_config_cases.json`
//! (4-03, before 4-05 deletes it), and `tests/config_cases.rs` holds this module to them.
//!
//! **New settings:** `--no-llm` (`no_llm`); `cpu: {enabled, threads, cpu_percent, memory_mb,
//! work}`, the machine's own switch and ceiling, **off unless its owner turns it on here**
//! (`setup` asks, `--cpu` for one run), so a 0.1.5 `helper.json`, which has no `cpu`, lends no
//! CPU; `updates` (`auto`, `notify`, `off`); `stream`; `ca_file` (`--ca-file`); `no_power`
//! (`--no-power`).

use std::io::{BufRead as _, IsTerminal as _, Write as _};
use std::path::{Path, PathBuf};

use serde_json::{Map, Value};

use crate::cli::Options;
use crate::home::{Env, Home};
use crate::protocol::{Kind, Provider, UpdatePolicy};
use crate::quiet::{self, Window};

/// The model endpoint protocol 1 defaulted to: LM Studio on this machine.
pub const DEFAULT_BASE_URL: &str = "http://localhost:1234/v1";
/// The most concurrent requests a machine may declare (`hello.llm.slots`, PROTOCOL.md).
pub const MAX_SLOTS: u32 = 16;

/// The machine's own CPU switch and ceiling, as configured; a figure left out is the default
/// share of the machine ([`Config::ceiling`]).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct LocalCpu {
    pub enabled: bool,
    pub threads: Option<u32>,
    pub cpu_percent: Option<u32>,
    pub memory_mb: Option<u64>,
    pub work: Option<Vec<Kind>>,
}

/// The local ceiling with every figure decided.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Ceiling {
    pub threads: u32,
    pub cpu_percent: u32,
    pub memory_mb: u64,
    pub work: Vec<Kind>,
}

/// What the ceiling's defaults are a share of.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Machine {
    pub logical_cpus: u32,
    pub memory_mb: u64,
}

/// The helper's effective settings.
#[derive(Clone, Debug, PartialEq)]
pub struct Config {
    pub server_url: String,
    pub token: String,
    pub base_url: String,
    pub provider: Provider,
    pub api_key_env: String,
    pub max_concurrent: u32,
    pub name: String,
    pub quiet: Vec<Window>,
    /// The owner's allowlist: only these models are offered and served.
    pub models: Option<Vec<String>>,
    pub insecure: bool,
    pub collect_hardware: bool,
    pub hardware_file: String,
    pub no_llm: bool,
    pub cpu: LocalCpu,
    pub updates: UpdatePolicy,
    pub stream: bool,
    pub ca_file: Option<PathBuf>,
    pub no_power: bool,
    /// Lines to log once the log exists (the config is read before it).
    pub warnings: Vec<String>,
}

impl Config {
    /// What protocol 1's `WorkerConfig` held, as the compatibility table records it.
    #[must_use]
    pub fn compat(&self) -> Value {
        serde_json::json!({
            "server_url": self.server_url,
            "token": self.token,
            "base_url": self.base_url,
            "provider": self.provider.as_str(),
            "api_key_env": self.api_key_env,
            "max_concurrent": self.max_concurrent,
            "name": self.name,
            "quiet_hours": self.quiet.iter().map(|w| [u16::from(w.weekday), w.start, w.end]).collect::<Vec<_>>(),
            "models": self.models,
            "insecure": self.insecure,
            "collect_hardware": self.collect_hardware,
            "hardware_file": self.hardware_file,
        })
    }

    /// The local ceiling: the configured figures, else half the logical CPUs, 50%, a quarter of
    /// the memory and every kind of work (P4.3.2).
    #[must_use]
    pub fn ceiling(&self, m: Machine) -> Ceiling {
        Ceiling {
            threads: self.cpu.threads.unwrap_or((m.logical_cpus / 2).max(1)),
            cpu_percent: self.cpu.cpu_percent.unwrap_or(50),
            memory_mb: self.cpu.memory_mb.unwrap_or(m.memory_mb / 4),
            work: self
                .cpu
                .work
                .clone()
                .unwrap_or_else(|| vec![Kind::LabGame, Kind::RankingGame, Kind::ReferenceGame]),
        }
    }
}

/// Where the settings come from.
pub struct Sources<'a> {
    pub opts: &'a Options,
    pub env: Env<'a>,
    pub home: &'a Home,
}

/// The first-run conversation: a terminal, or a script in tests.
pub trait Prompt {
    /// Prints `question` and reads one line; `None` at the end of input.
    fn ask(&mut self, question: &str) -> Option<String>;
    /// Prints a line.
    fn say(&mut self, text: &str);
}

/// The process's own terminal.
pub struct Terminal;

impl Prompt for Terminal {
    fn ask(&mut self, question: &str) -> Option<String> {
        print!("{question}");
        let _flushed = std::io::stdout().flush();
        let mut line = String::new();
        match std::io::stdin().lock().read_line(&mut line) {
            Ok(0) | Err(_) => None,
            Ok(_) => Some(line.trim().to_owned()),
        }
    }

    fn say(&mut self, text: &str) {
        println!("{text}");
    }
}

/// Whether a person is at the terminal to answer the first-run questions.
#[must_use]
pub fn interactive() -> bool {
    std::io::stdin().is_terminal() && std::io::stdout().is_terminal()
}

/// Reads a settings file: a JSON object, a UTF-8 byte-order mark allowed.
///
/// # Errors
/// A file that cannot be read or is not a JSON object.
pub fn read_file(path: &Path) -> Result<Map<String, Value>, String> {
    let raw = std::fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let text = raw.strip_prefix('\u{feff}').unwrap_or(&raw);
    match serde_json::from_str::<Value>(text) {
        Ok(Value::Object(m)) => Ok(m),
        Ok(_) => Err(format!("{} is not a JSON object", path.display())),
        Err(e) => Err(format!("{} is not JSON: {e}", path.display())),
    }
}

/// A file value as `pick` read it: absent, `null` and `""` are not given.
fn file_value<'v>(data: &'v Map<String, Value>, key: &str) -> Option<&'v Value> {
    data.get(key).filter(|v| !v.is_null() && v.as_str() != Some(""))
}

fn file_text(data: &Map<String, Value>, key: &str, file: &str) -> Result<Option<String>, String> {
    match file_value(data, key) {
        None => Ok(None),
        Some(Value::String(s)) => Ok(Some(s.clone())),
        Some(other) => Err(format!("{file}: {key} must be text, not {other}")),
    }
}

/// Python's truthiness of a JSON value, for protocol 1's `bool(... or data.get(...))`.
fn truthy(v: Option<&Value>) -> bool {
    match v {
        None | Some(Value::Null) => false,
        Some(Value::Bool(b)) => *b,
        Some(Value::Number(n)) => n.as_f64().is_some_and(|x| x != 0.0),
        Some(Value::String(s)) => !s.is_empty(),
        Some(Value::Array(a)) => !a.is_empty(),
        Some(Value::Object(o)) => !o.is_empty(),
    }
}

fn env_text(env: Env<'_>, name: &str) -> Option<String> {
    env(name).map(|v| v.to_string_lossy().into_owned()).filter(|v| !v.is_empty())
}

/// `pick` for text: the flag, else the file, else the variable, else the default.
fn pick(
    flag: Option<&String>,
    data: &Map<String, Value>,
    key: &str,
    env: Env<'_>,
    var: &str,
    file: &str,
) -> Result<Option<String>, String> {
    if let Some(f) = flag.filter(|f| !f.is_empty()) {
        return Ok(Some(f.clone()));
    }
    if let Some(v) = file_text(data, key, file)? {
        return Ok(Some(v));
    }
    Ok(env_text(env, var))
}

fn max_concurrent(
    opts: &Options,
    data: &Map<String, Value>,
    env: Env<'_>,
    file: &str,
    warnings: &mut Vec<String>,
) -> Result<u32, String> {
    let bad = |what: &str| format!("max_concurrent must be a whole number, not {what}");
    let n: i64 = if let Some(n) = opts.max_concurrent.filter(|n| *n != 0) {
        n
    } else if let Some(v) = file_value(data, "max_concurrent") {
        match v {
            Value::Number(n) => n.as_i64().ok_or_else(|| bad(&n.to_string()))?,
            Value::String(s) => s.trim().parse().map_err(|_| bad(s))?,
            Value::Bool(b) => i64::from(*b),
            other => return Err(format!("{file}: {}", bad(&other.to_string()))),
        }
    } else if let Some(s) = env_text(env, "CITAR_WORKER_MAX_CONCURRENT") {
        s.trim().parse().map_err(|_| bad(&s))?
    } else {
        1
    };
    // Protocol 1's `int(... or 1)`: a 0 found anywhere is the default.
    let n = if n == 0 { 1 } else { n };
    if n < 0 {
        return Err(format!("max_concurrent must be from 1 to {MAX_SLOTS}, not {n}"));
    }
    if n > i64::from(MAX_SLOTS) {
        warnings.push(format!(
            "max_concurrent {n} is more than a helper may declare; taking {MAX_SLOTS}"
        ));
        return Ok(MAX_SLOTS);
    }
    Ok(u32::try_from(n).unwrap_or(1))
}

fn models(
    opts: &Options,
    data: &Map<String, Value>,
    file: &str,
) -> Result<Option<Vec<String>>, String> {
    if !opts.models.is_empty() {
        return Ok(Some(opts.models.clone()));
    }
    match data.get("models") {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(s)) if s.is_empty() => Ok(None),
        // One model named alone: protocol 1 read a string as its characters, a bug.
        Some(Value::String(s)) => Ok(Some(vec![s.clone()])),
        Some(Value::Array(a)) if a.is_empty() => Ok(None),
        Some(Value::Array(a)) => a
            .iter()
            .map(|m| m.as_str().map(str::to_owned))
            .collect::<Option<Vec<_>>>()
            .map(Some)
            .ok_or_else(|| format!("{file}: models must be a list of model names")),
        Some(other) => Err(format!("{file}: models must be a list of model names, not {other}")),
    }
}

fn quiet_entries(
    opts: &Options,
    data: &Map<String, Value>,
    file: &str,
) -> Result<Vec<Value>, String> {
    if !opts.quiet.is_empty() {
        return Ok(opts.quiet.iter().cloned().map(Value::String).collect());
    }
    match data.get("quiet_hours") {
        None | Some(Value::Null) => Ok(Vec::new()),
        Some(Value::Array(a)) => Ok(a.clone()),
        Some(Value::String(s)) if s.is_empty() => Ok(Vec::new()),
        Some(Value::String(s)) => Ok(vec![Value::String(s.clone())]),
        Some(other) => Err(format!("{file}: quiet_hours must be a list of windows, not {other}")),
    }
}

fn kinds(v: &Value, file: &str) -> Result<Vec<Kind>, String> {
    let list = v.as_array().ok_or_else(|| format!("{file}: cpu.work must be a list of kinds"))?;
    let mut out = Vec::new();
    for k in list {
        let kind = match k.as_str() {
            Some("lab_game") => Kind::LabGame,
            Some("ranking_game") => Kind::RankingGame,
            Some("reference_game") => Kind::ReferenceGame,
            _ => {
                return Err(format!(
                    "{file}: cpu.work lists {k}, which is not lab_game, ranking_game or \
                     reference_game"
                ));
            }
        };
        if !out.contains(&kind) {
            out.push(kind);
        }
    }
    // In the order of `kind`, as the acknowledgement lists them.
    out.sort();
    Ok(out)
}

fn local_cpu(opts: &Options, data: &Map<String, Value>, file: &str) -> Result<LocalCpu, String> {
    let mut cpu = LocalCpu::default();
    if let Some(v) = data.get("cpu").filter(|v| !v.is_null()) {
        let m = v.as_object().ok_or_else(|| format!("{file}: cpu must be an object"))?;
        for key in m.keys() {
            if !["enabled", "threads", "cpu_percent", "memory_mb", "work"].contains(&key.as_str()) {
                return Err(format!(
                    "{file}: cpu has the unknown key {key:?} (it takes enabled, threads, \
                     cpu_percent, memory_mb and work)"
                ));
            }
        }
        let figure = |key: &str, max: u64| -> Result<Option<u64>, String> {
            match m.get(key) {
                None | Some(Value::Null) => Ok(None),
                Some(v) => v.as_u64().filter(|n| *n <= max).map(Some).ok_or_else(|| {
                    format!("{file}: cpu.{key} must be a whole number from 0 to {max}, not {v}")
                }),
            }
        };
        cpu.enabled = match m.get("enabled") {
            None | Some(Value::Null) => false,
            Some(Value::Bool(b)) => *b,
            Some(other) => {
                return Err(format!("{file}: cpu.enabled must be true or false, not {other}"));
            }
        };
        // 0 is "not set": the default share, as the site's 0 is the helper's ceiling.
        cpu.threads =
            figure("threads", 1024)?.filter(|n| *n > 0).map(|n| u32::try_from(n).unwrap_or(1024));
        cpu.cpu_percent =
            figure("cpu_percent", 100)?.filter(|n| *n > 0).map(|n| u32::try_from(n).unwrap_or(100));
        cpu.memory_mb = figure("memory_mb", 16_777_216)?.filter(|n| *n > 0);
        cpu.work = match m.get("work") {
            None | Some(Value::Null) => None,
            Some(v) => Some(kinds(v, file)?),
        };
    }
    if opts.cpu {
        cpu.enabled = true;
    }
    Ok(cpu)
}

fn flag_or_file_bool(
    flag: bool,
    data: &Map<String, Value>,
    key: &str,
    file: &str,
) -> Result<bool, String> {
    if flag {
        return Ok(true);
    }
    match data.get(key) {
        None | Some(Value::Null) => Ok(false),
        Some(Value::Bool(b)) => Ok(*b),
        Some(other) => Err(format!("{file}: {key} must be true or false, not {other}")),
    }
}

/// The endpoint's kind, by the name the settings give it.
///
/// # Errors
/// A name that is none of the four.
pub fn provider(name: &str) -> Result<Provider, String> {
    Ok(match name {
        "lmstudio" => Provider::LmStudio,
        "ollama" => Provider::Ollama,
        "openai_compatible" => Provider::OpenAiCompatible,
        "openai" => Provider::OpenAi,
        other => {
            return Err(format!(
                "Unknown provider {other:?}: use lmstudio, ollama, openai_compatible or openai"
            ));
        }
    })
}

/// The model endpoint's settings, as [`resolve`] reads them.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EndpointSettings {
    pub base_url: String,
    pub provider: Provider,
    pub api_key_env: String,
    /// The owner's allowlist (`models`, `--model`).
    pub models: Option<Vec<String>>,
    pub no_llm: bool,
}

/// The settings file and its name for messages: `--config FILE`, else `helper.json` in `home`
/// (none when it does not exist, or there is no home).
fn settings_file(
    opts: &Options,
    home: Option<&Home>,
) -> Result<(Map<String, Value>, String), String> {
    if let Some(path) = &opts.config {
        if !path.exists() {
            return Err(format!("No such config file: {}", path.display()));
        }
        return Ok((read_file(path)?, path.display().to_string()));
    }
    let Some(home) = home else { return Ok((Map::new(), "the settings".to_owned())) };
    let saved = home.config_file();
    if saved.exists() {
        Ok((read_file(&saved)?, saved.display().to_string()))
    } else {
        Ok((Map::new(), saved.display().to_string()))
    }
}

/// The endpoint's settings from the flags, the file and the variables.
fn endpoint_part(
    opts: &Options,
    data: &Map<String, Value>,
    env: Env<'_>,
    file: &str,
) -> Result<EndpointSettings, String> {
    let base_url =
        pick(opts.base_url.as_ref(), data, "base_url", env, "CITAR_WORKER_BASE_URL", file)?
            .unwrap_or_else(|| DEFAULT_BASE_URL.to_owned());
    let provider_name =
        pick(opts.provider.as_ref(), data, "provider", env, "CITAR_WORKER_PROVIDER", file)?
            .unwrap_or_else(|| "lmstudio".to_owned());
    let api_key_env = pick(
        opts.api_key_env.as_ref(),
        data,
        "api_key_env",
        env,
        "CITAR_WORKER_API_KEY_ENV",
        file,
    )?
    .unwrap_or_default();
    Ok(EndpointSettings {
        base_url,
        provider: provider(&provider_name)?,
        api_key_env,
        models: models(opts, data, file)?,
        no_llm: flag_or_file_bool(opts.no_llm, data, "no_llm", file)?,
    })
}

/// The model endpoint's settings alone, read as [`resolve`] reads them (the flags, then `--config
/// FILE` or `home`'s `helper.json`, then the variables), for `scan`: it needs no server or token,
/// asks nothing and writes nothing. Without a home, only the flags and variables count.
///
/// # Errors
/// A settings file that cannot be read, or an endpoint setting it cannot take.
pub fn endpoint(
    opts: &Options,
    env: Env<'_>,
    home: Option<&Home>,
) -> Result<EndpointSettings, String> {
    let (data, file) = settings_file(opts, home)?;
    endpoint_part(opts, &data, env, &file)
}

/// The update policy from `--updates`, then the settings file's `updates`, then the build's
/// default: `notify` in a package manager's build (P4.3.8: it reports updates and leaves installing
/// them to the package manager; `auto` said there is refused when an update comes,
/// `crate::update`), else `auto`.
fn update_policy(
    opts: &Options,
    data: &Map<String, Value>,
    file: &str,
) -> Result<UpdatePolicy, String> {
    // A value of another type is refused, as for every new key: `"updates": false` must not
    // quietly mean the most permissive policy.
    let from_file = match data.get("updates") {
        None | Some(Value::Null) => None,
        Some(Value::String(s)) => Some(s.as_str()),
        Some(other) => {
            return Err(format!("{file}: updates must be auto, notify or off, not {other}"));
        }
    };
    match opts.updates.as_deref().or(from_file) {
        None if cfg!(feature = "packaged") => Ok(UpdatePolicy::Notify),
        None | Some("auto") => Ok(UpdatePolicy::Auto),
        Some("notify") => Ok(UpdatePolicy::Notify),
        Some("off") => Ok(UpdatePolicy::Off),
        Some(other) => Err(format!("{file}: updates must be auto, notify or off, not {other:?}")),
    }
}

/// The update policy alone, read as [`resolve`] reads it (the flag, then `--config FILE` or
/// `home`'s `helper.json`, then the build's default), for `update check`: it needs no server or
/// token, so a helper a package manager has just installed, with no `helper.json` yet, still says
/// its policy. It asks nothing and writes nothing.
///
/// # Errors
/// A settings file that cannot be read, or an `updates` it cannot take.
pub fn updates(opts: &Options, home: &Home) -> Result<UpdatePolicy, String> {
    let (data, file) = settings_file(opts, Some(home))?;
    update_policy(opts, &data, &file)
}

/// The effective settings, as protocol 1's `build_config` made them, with protocol 2's added.
/// `prompt` is the terminal when a person is there to answer the first-run questions.
///
/// # Errors
/// Protocol 1's messages for a missing file, server or token, and a sentence for any value it
/// cannot take.
pub fn resolve(src: &Sources<'_>, prompt: Option<&mut dyn Prompt>) -> Result<Config, String> {
    let opts = src.opts;
    let env = src.env;
    let (mut data, file) = settings_file(opts, Some(src.home))?;
    let given = opts.server_url.as_deref().is_some_and(|s| !s.is_empty())
        || opts.token.as_deref().is_some_and(|s| !s.is_empty())
        || env_text(env, "CITAR_WORKER_SERVER").is_some()
        || env_text(env, "CITAR_WORKER_TOKEN").is_some();
    let saved_both = truthy(data.get("server_url")) && truthy(data.get("token"));
    if !given
        && !saved_both
        && let Some(p) = prompt
    {
        data = first_run(p, data, &src.home.config_file(), true)?;
    }

    let mut warnings = Vec::new();
    let server_url =
        pick(opts.server_url.as_ref(), &data, "server_url", env, "CITAR_WORKER_SERVER", &file)?
            .ok_or("Give --server (or CITAR_WORKER_SERVER): the CITAR server to connect to.")?;
    let token = pick(opts.token.as_ref(), &data, "token", env, "CITAR_WORKER_TOKEN", &file)?
        .ok_or("Give --token (or CITAR_WORKER_TOKEN): issue one on the Servers page.")?;
    let endpoint = endpoint_part(opts, &data, env, &file)?;
    let max_concurrent = max_concurrent(opts, &data, env, &file, &mut warnings)?;
    let name = pick(opts.name.as_ref(), &data, "name", env, "CITAR_WORKER_NAME", &file)?
        .unwrap_or_default();
    let quiet = quiet::parse(&quiet_entries(opts, &data, &file)?)?;
    let hardware_file = pick(
        opts.hardware_file.as_ref(),
        &data,
        "hardware_file",
        env,
        "CITAR_WORKER_HARDWARE_FILE",
        &file,
    )?
    .unwrap_or_default();
    let updates = update_policy(opts, &data, &file)?;
    let stream = match data.get("stream") {
        None | Some(Value::Null) => true,
        Some(Value::Bool(b)) => *b,
        Some(other) => return Err(format!("{file}: stream must be true or false, not {other}")),
    };
    let ca_file = match &opts.ca_file {
        Some(p) => Some(p.clone()),
        None => file_text(&data, "ca_file", &file)?.map(PathBuf::from),
    };
    Ok(Config {
        server_url,
        token,
        base_url: endpoint.base_url,
        provider: endpoint.provider,
        api_key_env: endpoint.api_key_env,
        max_concurrent,
        name,
        quiet,
        models: endpoint.models,
        insecure: opts.insecure || truthy(data.get("insecure")),
        collect_hardware: !opts.no_hardware,
        hardware_file,
        no_llm: endpoint.no_llm,
        cpu: local_cpu(opts, &data, &file)?,
        updates,
        stream,
        ca_file,
        no_power: flag_or_file_bool(opts.no_power, &data, "no_power", &file)?,
        warnings,
    })
}

fn yes(answer: Option<&str>, default: bool) -> bool {
    match answer.map(|a| a.trim().to_lowercase()) {
        Some(a) if a.is_empty() => default,
        Some(a) => a == "y" || a == "yes",
        None => default,
    }
}

/// The first-run questions (protocol 1's `_first_run`, and the CPU question): the server and a
/// token, both needed; whether to lend CPU, default no; with `ask_to_save`, whether to remember
/// them in `path` (default yes), else they are saved. Returns the settings with the answers.
///
/// # Errors
/// An answer missing, or the file not written.
pub fn first_run(
    p: &mut dyn Prompt,
    mut data: Map<String, Value>,
    path: &Path,
    ask_to_save: bool,
) -> Result<Map<String, Value>, String> {
    p.say("CITAR helper - first run");
    p.say(
        "On your CITAR server, open Servers, pick this machine (or add it), and issue a worker \
         token.\n",
    );
    let current = data.get("server_url").and_then(Value::as_str).unwrap_or_default().to_owned();
    let question = if current.is_empty() {
        "CITAR server address (e.g. https://citar.example.com): ".to_owned()
    } else {
        format!("CITAR server address [{current}]: ")
    };
    let mut server = p.ask(&question).unwrap_or_default();
    if server.is_empty() {
        server = current;
    }
    let token = p.ask("Worker token: ").unwrap_or_default();
    if server.is_empty() || token.is_empty() {
        return Err("A server address and a token are both needed.".to_owned());
    }
    if !server.starts_with("http://") && !server.starts_with("https://") {
        server = format!("https://{server}");
    }
    let was_on =
        data.get("cpu").and_then(|c| c.get("enabled")).and_then(Value::as_bool).unwrap_or(false);
    let lend = yes(
        p.ask(if was_on {
            "Lend this machine's spare CPU to the server? [Y/n] "
        } else {
            "Lend this machine's spare CPU to the server? [y/N] "
        })
        .as_deref(),
        was_on,
    );
    data.insert("server_url".to_owned(), Value::String(server));
    data.insert("token".to_owned(), Value::String(token));
    let mut cpu = data.get("cpu").and_then(Value::as_object).cloned().unwrap_or_default();
    cpu.insert("enabled".to_owned(), Value::Bool(lend));
    data.insert("cpu".to_owned(), Value::Object(cpu));
    let save = !ask_to_save
        || yes(p.ask(&format!("Remember these in {}? [Y/n] ", path.display())).as_deref(), true);
    if save {
        write_owner_only(path, &data)?;
        p.say(&format!("Saved. Delete {} to forget them.\n", path.display()));
    }
    Ok(data)
}

/// Writes a settings file only its owner can read (it holds the token): written aside, made
/// owner-only, then moved into place.
///
/// # Errors
/// The file or its folder could not be written.
pub fn write_owner_only(path: &Path, data: &Map<String, Value>) -> Result<(), String> {
    let dir = path.parent().unwrap_or_else(|| Path::new("."));
    std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    let tmp = path.with_extension(format!("json.tmp-{}", std::process::id()));
    let text = serde_json::to_string_pretty(&Value::Object(data.clone())).unwrap_or_default();
    let written = (|| -> std::io::Result<()> {
        std::fs::write(&tmp, b"")?;
        citar_jobs::sys::restrict_to_owner(&tmp)?;
        std::fs::write(&tmp, text.as_bytes())?;
        std::fs::rename(&tmp, path)
    })();
    if let Err(e) = written {
        let _gone = std::fs::remove_file(&tmp);
        return Err(format!("{}: {e}", path.display()));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cli::Cli;
    use clap::Parser as _;
    use std::collections::BTreeMap;
    use std::ffi::OsString;

    struct Script(Vec<&'static str>, Vec<String>);

    impl Prompt for Script {
        fn ask(&mut self, question: &str) -> Option<String> {
            self.1.push(question.to_owned());
            if self.0.is_empty() { None } else { Some(self.0.remove(0).to_owned()) }
        }
        fn say(&mut self, text: &str) {
            self.1.push(text.to_owned());
        }
    }

    fn scratch(name: &str) -> PathBuf {
        let d =
            std::env::temp_dir().join(format!("citar-helper-config-{}-{name}", std::process::id()));
        let _fresh = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).expect("scratch");
        d
    }

    fn opts(args: &[&str]) -> Options {
        Cli::try_parse_from(std::iter::once("citar-helper").chain(args.iter().copied()))
            .expect("parses")
            .opts
    }

    #[test]
    fn the_first_run_asks_saves_owner_only_and_lends_no_cpu_by_default() {
        let dir = scratch("first");
        let home = Home::at(&dir);
        let env = |_: &str| -> Option<OsString> { None };
        let o = opts(&[]);
        let mut p = Script(vec!["citar.example.com", "cwt_tok", "", ""], Vec::new());
        let c =
            resolve(&Sources { opts: &o, env: &env, home: &home }, Some(&mut p)).expect("resolves");
        assert_eq!(c.server_url, "https://citar.example.com");
        assert_eq!(c.token, "cwt_tok");
        assert!(!c.cpu.enabled, "CPU lending is off unless the owner says so");
        let saved = read_file(&home.config_file()).expect("saved");
        assert_eq!(saved["cpu"]["enabled"], Value::Bool(false));
        assert_eq!(citar_jobs::sys::exposure(&home.config_file()).expect("read"), None);
        // Saved: the next run asks nothing.
        let mut p = Script(vec![], Vec::new());
        let again =
            resolve(&Sources { opts: &o, env: &env, home: &home }, Some(&mut p)).expect("resolves");
        assert_eq!(again.token, "cwt_tok");
        assert!(p.1.is_empty(), "{:?}", p.1);
        // A missing answer is protocol 1's error.
        let other = Home::at(dir.join("other"));
        let mut p = Script(vec!["s"], Vec::new());
        let e = resolve(&Sources { opts: &o, env: &env, home: &other }, Some(&mut p))
            .expect_err("no token");
        assert_eq!(e, "A server address and a token are both needed.");
        std::fs::remove_dir_all(&dir).expect("cleaned");
    }

    #[test]
    fn the_cpu_section_and_its_flag() {
        let dir = scratch("cpu");
        let home = Home::at(&dir);
        let map: BTreeMap<&str, &str> = BTreeMap::new();
        let env = |k: &str| map.get(k).map(OsString::from);
        std::fs::write(
            home.config_file(),
            r#"{"server_url": "http://127.0.0.1:1", "token": "t", "cpu": {"threads": 3, "work": ["reference_game", "lab_game"]}}"#,
        )
        .expect("written");
        let c =
            resolve(&Sources { opts: &opts(&[]), env: &env, home: &home }, None).expect("resolves");
        assert!(!c.cpu.enabled);
        let ceiling = c.ceiling(Machine { logical_cpus: 8, memory_mb: 16000 });
        assert_eq!(
            ceiling,
            Ceiling {
                threads: 3,
                cpu_percent: 50,
                memory_mb: 4000,
                work: vec![Kind::LabGame, Kind::ReferenceGame]
            }
        );
        let c = resolve(&Sources { opts: &opts(&["--cpu"]), env: &env, home: &home }, None)
            .expect("resolves");
        assert!(c.cpu.enabled);
        std::fs::write(
            home.config_file(),
            r#"{"server_url": "x", "token": "t", "cpu": {"thread": 3}}"#,
        )
        .expect("written");
        assert!(resolve(&Sources { opts: &opts(&[]), env: &env, home: &home }, None).is_err());
        let one = test_config().ceiling(Machine { logical_cpus: 1, memory_mb: 1000 });
        assert_eq!((one.threads, one.cpu_percent, one.memory_mb), (1, 50, 250), "never 0 threads");
        assert_eq!(one.work, [Kind::LabGame, Kind::RankingGame, Kind::ReferenceGame]);
        let e = resolve(
            &Sources { opts: &opts(&["--config", "nope.json"]), env: &env, home: &home },
            None,
        )
        .expect_err("no such file");
        assert_eq!(e, "No such config file: nope.json");
        std::fs::remove_dir_all(&dir).expect("cleaned");
    }

    fn test_config() -> Config {
        Config {
            server_url: String::new(),
            token: String::new(),
            base_url: DEFAULT_BASE_URL.to_owned(),
            provider: Provider::LmStudio,
            api_key_env: String::new(),
            max_concurrent: 1,
            name: String::new(),
            quiet: Vec::new(),
            models: None,
            insecure: false,
            collect_hardware: true,
            hardware_file: String::new(),
            no_llm: false,
            cpu: LocalCpu::default(),
            updates: UpdatePolicy::Auto,
            stream: true,
            ca_file: None,
            no_power: false,
            warnings: Vec::new(),
        }
    }

    #[test]
    fn slots_beyond_sixteen_are_capped_and_negative_ones_refused() {
        let dir = scratch("slots");
        let home = Home::at(&dir);
        let env = |_: &str| -> Option<OsString> { None };
        let base = ["--server", "s", "--token", "t", "--max-concurrent"];
        let mut args = base.to_vec();
        args.push("40");
        let c = resolve(&Sources { opts: &opts(&args), env: &env, home: &home }, None)
            .expect("resolves");
        assert_eq!(c.max_concurrent, 16);
        assert_eq!(c.warnings.len(), 1);
        std::fs::write(home.config_file(), r#"{"max_concurrent": -2}"#).expect("written");
        assert!(
            resolve(&Sources { opts: &opts(&base[..4]), env: &env, home: &home }, None).is_err()
        );
        std::fs::remove_dir_all(&dir).expect("cleaned");
    }

    #[test]
    fn updates_takes_its_three_words_and_refuses_anything_else() {
        let dir = scratch("updates");
        let home = Home::at(&dir);
        let env = |_: &str| -> Option<OsString> { None };
        let run = |file: &str, args: &[&str]| {
            std::fs::write(home.config_file(), file).expect("written");
            resolve(&Sources { opts: &opts(args), env: &env, home: &home }, None)
        };
        let base = r#""server_url": "http://127.0.0.1:1", "token": "t""#;
        let policy = |file: String, args: &[&str]| run(&file, args).map(|c| c.updates);
        // A package manager's build reports updates and leaves installing them to it (P4.3.8).
        let default =
            if cfg!(feature = "packaged") { UpdatePolicy::Notify } else { UpdatePolicy::Auto };
        assert_eq!(policy(format!("{{{base}}}"), &[]), Ok(default));
        assert_eq!(policy(format!(r#"{{{base}, "updates": null}}"#), &[]), Ok(default));
        assert_eq!(
            policy(format!(r#"{{{base}, "updates": "auto"}}"#), &[]),
            Ok(UpdatePolicy::Auto),
            "auto said is auto, packaged or not (the updater refuses it there)"
        );
        assert_eq!(policy(format!(r#"{{{base}, "updates": "off"}}"#), &[]), Ok(UpdatePolicy::Off));
        assert_eq!(
            policy(format!(r#"{{{base}, "updates": "off"}}"#), &["--updates", "notify"]),
            Ok(UpdatePolicy::Notify),
            "the flag first"
        );
        for bad in ["false", "0", "true", r#"["off"]"#, r#""never""#] {
            let e =
                policy(format!(r#"{{{base}, "updates": {bad}}}"#), &[]).expect_err("not a policy");
            assert!(e.contains("updates must be auto, notify or off"), "{bad}: {e}");
        }
        // Even beside a flag that names one: the file is wrong either way.
        assert!(policy(format!(r#"{{{base}, "updates": false}}"#), &["--updates", "off"]).is_err());
        std::fs::remove_dir_all(&dir).expect("cleaned");
    }

    /// `update check` reads the policy with no server or token: a home a package manager has just
    /// installed into has no `helper.json`, and its build's default is still said.
    #[test]
    fn the_policy_alone_needs_no_server_or_token() {
        let dir = scratch("updates-alone");
        let home = Home::at(&dir);
        let default =
            if cfg!(feature = "packaged") { UpdatePolicy::Notify } else { UpdatePolicy::Auto };
        let env = |_: &str| -> Option<OsString> { None };
        assert_eq!(updates(&opts(&[]), &home), Ok(default), "no helper.json at all");
        assert!(resolve(&Sources { opts: &opts(&[]), env: &env, home: &home }, None).is_err());
        std::fs::write(home.config_file(), r#"{"updates": "auto"}"#).expect("written");
        assert_eq!(updates(&opts(&[]), &home), Ok(UpdatePolicy::Auto));
        assert_eq!(updates(&opts(&["--updates", "off"]), &home), Ok(UpdatePolicy::Off));
        std::fs::write(home.config_file(), r#"{"updates": "never"}"#).expect("written");
        assert!(updates(&opts(&[]), &home).expect_err("not a policy").contains("updates must be"));
        std::fs::write(home.config_file(), "{not json").expect("written");
        assert!(updates(&opts(&[]), &home).is_err(), "a file that does not read");
        std::fs::remove_dir_all(&dir).expect("cleaned");
    }
}
