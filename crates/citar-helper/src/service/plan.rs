//! What a service command does, as a list of steps (design/phase4-helper.md P4.3.9): the same list
//! is printed by `--dry-run`, which changes nothing, and carried out otherwise, so what a person
//! reads is exactly what would happen. The steps' text is built by the per-OS modules from their
//! inputs alone, which is why the unit tests on every OS can hold each OS's plan to its committed
//! file (`tests/data/service/`).

use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::process::Command;

/// How a command line is shown: as a POSIX shell reads it, or as Windows' C runtime splits it
/// (what `cmd.exe` and the standard library's process spawning use).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Quoting {
    Posix,
    Windows,
}

/// One step of a plan.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Step {
    /// Make a folder and its parents.
    Folder(PathBuf),
    /// Write a file with this text (on Unix with this mode).
    Write { path: PathBuf, text: String, mode: u32 },
    /// Copy a file.
    Copy { from: PathBuf, to: PathBuf },
    /// Give a folder and everything under it a protected DACL granting `principals` alone (SID
    /// strings), owned by `owner` (Windows: the service's home).
    Secure { path: PathBuf, owner: String, principals: Vec<String> },
    /// Run a program; a step that `may_fail` notes its failure and goes on (a stop of something not
    /// running, the removal of something not there).
    Run { program: String, args: Vec<String>, may_fail: bool },
    /// Remove a file if it is there.
    Remove(PathBuf),
    /// Start a program and wait for it to return (it detaches what it starts: `run --detach`).
    Start { program: PathBuf, args: Vec<String> },
    /// Stop the helper a home's Run supervisor keeps (Windows' per-user service), if one runs.
    StopSupervisor { home: PathBuf },
    /// Wait until the Windows service `name` has stopped, at most `seconds`.
    WaitStopped { name: String, seconds: u32 },
    /// A line for the person.
    Note(String),
}

/// A command's steps, and how its command lines are shown.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Plan {
    pub title: String,
    pub quoting: Quoting,
    pub steps: Vec<Step>,
}

/// `arg` as a POSIX shell reads it: bare when it holds only characters no shell treats specially,
/// else in single quotes.
#[must_use]
pub fn posix_quote(arg: &str) -> String {
    let plain = !arg.is_empty()
        && arg.chars().all(|c| c.is_ascii_alphanumeric() || "@%+=:,./_-".contains(c));
    if plain { arg.to_owned() } else { format!("'{}'", arg.replace('\'', r"'\''")) }
}

/// `arg` as Windows' C runtime splits a command line (the rules `CommandLineToArgvW` and the
/// standard library follow): bare when it holds no space, tab or quote and `force` is false, else
/// in quotes, each quote and the backslashes before it (and before the closing quote) escaped.
#[must_use]
pub fn windows_quote(arg: &str, force: bool) -> String {
    if !force && !arg.is_empty() && !arg.contains([' ', '\t', '"']) {
        return arg.to_owned();
    }
    let mut out = String::from("\"");
    let mut backslashes = 0usize;
    for c in arg.chars() {
        match c {
            '\\' => backslashes += 1,
            '"' => {
                out.push_str(&"\\".repeat(backslashes * 2 + 1));
                out.push('"');
                backslashes = 0;
            }
            other => {
                out.push_str(&"\\".repeat(backslashes));
                backslashes = 0;
                out.push(other);
            }
        }
    }
    out.push_str(&"\\".repeat(backslashes * 2));
    out.push('"');
    out
}

impl Quoting {
    /// `base` with `parts` added, joined by this style's separator whatever OS builds the plan, so
    /// the unit tests on every OS render each OS's paths as that OS writes them.
    #[must_use]
    pub fn join(self, base: &Path, parts: &[&str]) -> PathBuf {
        let sep = match self {
            Quoting::Posix => '/',
            Quoting::Windows => '\\',
        };
        let mut s = base.display().to_string();
        for p in parts {
            if !s.ends_with(sep) {
                s.push(sep);
            }
            s.push_str(p);
        }
        PathBuf::from(s)
    }

    /// A program and its arguments as one command line.
    #[must_use]
    pub fn line(self, program: &str, args: &[String]) -> String {
        let mut parts = Vec::with_capacity(args.len() + 1);
        for a in std::iter::once(program).chain(args.iter().map(String::as_str)) {
            parts.push(match self {
                Quoting::Posix => posix_quote(a),
                Quoting::Windows => windows_quote(a, false),
            });
        }
        parts.join(" ")
    }
}

/// A path as the plan shows it.
fn shown(p: &Path) -> String {
    p.display().to_string()
}

impl Step {
    /// The step as `--dry-run` prints it: one line, and for a file its text indented.
    #[must_use]
    pub fn render(&self, quoting: Quoting) -> String {
        match self {
            Step::Folder(p) => format!("make the folder {}", shown(p)),
            Step::Write { path, text, mode } => {
                let mut s = if quoting == Quoting::Posix {
                    format!("write {} (mode {mode:04o}):", shown(path))
                } else {
                    format!("write {}:", shown(path))
                };
                for line in text.lines() {
                    s.push('\n');
                    if !line.is_empty() {
                        s.push_str("    ");
                        s.push_str(line);
                    }
                }
                s
            }
            Step::Copy { from, to } => format!("copy {} to {}", shown(from), shown(to)),
            Step::Secure { path, owner, principals } => {
                let names: Vec<String> = principals.iter().map(|p| principal_name(p)).collect();
                format!(
                    "secure {} and everything in it: owner {}, access for {} alone, nothing \
                     inherited (SDDL of the folder: {})",
                    shown(path),
                    principal_name(owner),
                    names.join(", "),
                    folder_sddl(owner, principals)
                )
            }
            Step::Run { program, args, may_fail } => {
                let line = quoting.line(program, args);
                if *may_fail {
                    format!("run: {line} (a failure is noted)")
                } else {
                    format!("run: {line}")
                }
            }
            Step::Remove(p) => format!("remove {} (if it is there)", shown(p)),
            Step::Start { program, args } => {
                format!("start: {}", quoting.line(&program.display().to_string(), args))
            }
            Step::StopSupervisor { home } => format!(
                "stop the helper this home's Run supervisor keeps (its process ids in {}), if one runs",
                shown(&quoting.join(home, &["helper", super::supervisor::PID_FILE]))
            ),
            Step::WaitStopped { name, seconds } => {
                format!("wait until the service {name} has stopped (at most {seconds} s)")
            }
            Step::Note(text) => format!("note: {text}"),
        }
    }
}

/// A SID's usual name, for the plan's text.
#[must_use]
pub fn principal_name(sid: &str) -> String {
    match sid {
        "S-1-5-18" => "SYSTEM (S-1-5-18)".to_owned(),
        "S-1-5-32-544" => "Administrators (S-1-5-32-544)".to_owned(),
        other => match super::windows::named_service_sid(other) {
            Some(account) => format!("{account} ({other})"),
            None => other.to_owned(),
        },
    }
}

/// The SDDL a folder of [`Step::Secure`] gets: owner, protected DACL, full control inherited by
/// files and folders.
#[must_use]
pub fn folder_sddl(owner: &str, principals: &[String]) -> String {
    let mut s = format!("O:{owner}D:P");
    for p in principals {
        let _w = write!(s, "(A;OICI;FA;;;{p})");
    }
    s
}

impl Plan {
    /// The plan as `--dry-run` prints it.
    #[must_use]
    pub fn render(&self) -> String {
        let mut out =
            format!("{} (a dry run: nothing is written, started or changed)\n", self.title);
        for (i, step) in self.steps.iter().enumerate() {
            let _w = writeln!(out, "{}. {}", i + 1, step.render(self.quoting));
        }
        out
    }
}

/// Why a plan stopped: its step, and what went wrong.
#[derive(Debug)]
pub struct Failed {
    pub step: usize,
    pub what: String,
}

/// Runs `program` with `args`, its output captured; `Ok` with its standard output when it exits 0,
/// else `Err` with its code and the end of what it said.
///
/// # Errors
/// It could not be started, or it failed.
pub fn run_program(program: &str, args: &[String]) -> Result<String, (Option<i32>, String)> {
    let mut cmd = Command::new(program);
    cmd.args(args).stdin(std::process::Stdio::null());
    super::prepare(&mut cmd, program);
    let out = cmd.output().map_err(|e| (None, format!("could not start {program}: {e}")))?;
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    if out.status.success() {
        return Ok(stdout);
    }
    let said = format!("{}{}", String::from_utf8_lossy(&out.stderr), stdout);
    let said = said.trim();
    let tail: String = said.chars().rev().take(400).collect::<Vec<_>>().into_iter().rev().collect();
    Err((out.status.code(), tail))
}

impl Plan {
    /// Carries the plan out, one step at a time, saying each on standard output.
    ///
    /// # Errors
    /// The step that failed (the steps before it are done).
    pub fn run(&self, log: &crate::log::Log) -> Result<(), Failed> {
        for (i, step) in self.steps.iter().enumerate() {
            let fail = |what: String| Failed { step: i + 1, what };
            match step {
                Step::Note(text) => println!("{text}"),
                other => {
                    println!("{}", other.render(self.quoting).lines().next().unwrap_or_default())
                }
            }
            match step {
                Step::Folder(p) => {
                    std::fs::create_dir_all(p).map_err(|e| fail(format!("{}: {e}", shown(p))))?
                }
                Step::Write { path, text, mode } => write_file(path, text, *mode).map_err(fail)?,
                Step::Copy { from, to } => {
                    // A new file, never a write through whatever is there (a hard link's target).
                    match std::fs::remove_file(to) {
                        Ok(()) => {}
                        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                        Err(e) => return Err(fail(format!("{}: {e}", shown(to)))),
                    }
                    std::fs::copy(from, to)
                        .map_err(|e| fail(format!("{} to {}: {e}", shown(from), shown(to))))?;
                }
                Step::Secure { path, owner, principals } => {
                    secure(path, owner, principals).map_err(fail)?
                }
                Step::Run { program, args, may_fail } => match run_program(program, args) {
                    Ok(_) => {}
                    Err((code, said)) if *may_fail => log.info(format!(
                        "{} said {} (going on): {said}",
                        self.quoting.line(program, args),
                        code.map_or_else(|| "nothing".to_owned(), |c| format!("exit {c}"))
                    )),
                    Err((code, said)) => {
                        return Err(fail(format!(
                            "{} failed{}: {said}",
                            self.quoting.line(program, args),
                            code.map_or_else(String::new, |c| format!(" (exit {c})"))
                        )));
                    }
                },
                Step::Remove(p) => match std::fs::remove_file(p) {
                    Ok(()) => {}
                    Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                    Err(e) => return Err(fail(format!("{}: {e}", shown(p)))),
                },
                Step::Start { program, args } => {
                    let status = Command::new(program)
                        .args(args)
                        .stdin(std::process::Stdio::null())
                        .status()
                        .map_err(|e| fail(format!("{}: {e}", program.display())))?;
                    if !status.success() {
                        return Err(fail(format!(
                            "{} exited with {}",
                            self.quoting.line(&program.display().to_string(), args),
                            status.code().map_or_else(|| "a signal".to_owned(), |c| c.to_string())
                        )));
                    }
                }
                Step::StopSupervisor { home } => {
                    super::supervisor::stop_running(home, log).map_err(fail)?;
                }
                Step::WaitStopped { name, seconds } => {
                    super::windows::wait_stopped(name, *seconds).map_err(fail)?
                }
                Step::Note(_) => {}
            }
        }
        Ok(())
    }
}

/// Writes `text` to `path` (written beside it, then renamed over it), with `mode` on Unix.
fn write_file(path: &Path, text: &str, mode: u32) -> Result<(), String> {
    let tmp = path.with_extension("citar-tmp");
    std::fs::write(&tmp, text).map_err(|e| format!("{}: {e}", shown(&tmp)))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(mode))
            .map_err(|e| format!("{}: {e}", shown(&tmp)))?;
    }
    #[cfg(not(unix))]
    let _ = mode;
    std::fs::rename(&tmp, path).map_err(|e| {
        let _gone = std::fs::remove_file(&tmp);
        format!("{}: {e}", shown(path))
    })
}

/// [`Step::Secure`] on this OS.
fn secure(path: &Path, owner: &str, principals: &[String]) -> Result<(), String> {
    #[cfg(windows)]
    {
        let p: Vec<&str> = principals.iter().map(String::as_str).collect();
        // What may own what is there already: these principals, and the administrator installing.
        let me = citar_jobs::sys::current_user_sid().map_err(|e| e.to_string())?;
        let mut owners = p.clone();
        owners.push(&me);
        citar_jobs::sys::restrict_tree_to(path, Some(owner), &p, &owners)
            .map(|_| ())
            .map_err(|e| format!("{}: {e}", shown(path)))
    }
    #[cfg(not(windows))]
    {
        let _ = (owner, principals);
        Err(format!("{}: an access list is Windows' own", shown(path)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn windows_quoting_round_trips_as_the_c_runtime_splits_it() {
        assert_eq!(windows_quote("plain", false), "plain");
        assert_eq!(windows_quote("plain", true), "\"plain\"");
        assert_eq!(windows_quote("", false), "\"\"");
        assert_eq!(windows_quote(r"C:\Program Files\x.exe", false), r#""C:\Program Files\x.exe""#);
        // a quote inside, and backslashes before it and before the closing quote
        assert_eq!(windows_quote(r#"a "b" c"#, false), r#""a \"b\" c""#);
        assert_eq!(windows_quote(r#"x\"y"#, false), r#""x\\\"y""#);
        assert_eq!(windows_quote(r"C:\dir\", true), r#""C:\dir\\""#);
        assert_eq!(
            windows_quote(r#""C:\a b\c.exe" run --home "C:\h""#, false),
            r#""\"C:\a b\c.exe\" run --home \"C:\h\"""#
        );
    }

    #[test]
    fn posix_quoting_leaves_plain_words_and_quotes_the_rest() {
        assert_eq!(posix_quote("/opt/citar/citar-helper"), "/opt/citar/citar-helper");
        assert_eq!(posix_quote("a b"), "'a b'");
        assert_eq!(posix_quote("it's"), r"'it'\''s'");
        assert_eq!(posix_quote(""), "''");
    }
}
