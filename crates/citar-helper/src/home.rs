//! The helper's home (design/phase4-helper.md P4.3.2): the per-user base the Python worker used,
//! or the directory `--home` (or `CITAR_DATA_DIR`) names. `helper.json` sits at its root, where
//! the 0.1.5 helper kept it; the helper's own state goes under `helper/`. The helper writes
//! nothing outside its home.
//!
//! **The development guard** (`dev-guard` or any test feature, P4.3.1): on this laptop the
//! platform's per-user base also holds the owner's `citar.db`, `secret_key` and `config/`, so a
//! development or test build refuses to start without an explicit `--home`, and refuses one inside
//! that base; a forgotten flag fails instead of writing there. Replaces `citar/paths.py`'s
//! `_user_base` for the helper.

use std::ffi::OsString;
use std::path::{Component, Path, PathBuf};

/// Where the helper keeps its files.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Home {
    root: PathBuf,
}

/// How the home was chosen.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HomeSource {
    /// `--home DIR`.
    Flag,
    /// `CITAR_DATA_DIR`.
    Env,
    /// The platform's per-user base.
    Platform,
}

impl Home {
    /// A home at `root`.
    #[must_use]
    pub fn at(root: impl Into<PathBuf>) -> Home {
        Home { root: root.into() }
    }

    /// The home's root.
    #[must_use]
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// `helper.json`: the server, the token and the rest of the configuration (protocol 1's file).
    #[must_use]
    pub fn config_file(&self) -> PathBuf {
        self.root.join("helper.json")
    }

    /// `helper/`: everything else the helper keeps.
    #[must_use]
    pub fn state_dir(&self) -> PathBuf {
        self.root.join("helper")
    }

    /// `helper/machine-key.json`: the machine key (P4.3.7).
    #[must_use]
    pub fn key_file(&self) -> PathBuf {
        self.state_dir().join("machine-key.json")
    }

    /// `helper/logs/`: `helper.log` and its rotations.
    #[must_use]
    pub fn log_dir(&self) -> PathBuf {
        self.state_dir().join("logs")
    }

    /// `helper/scan.json`: the last hardware scan and its hash (P4.3.5).
    #[must_use]
    pub fn scan_file(&self) -> PathBuf {
        self.state_dir().join("scan.json")
    }

    /// `helper/results/`: each run's result not yet acknowledged, as `<run>.json` (P4.2.5).
    #[must_use]
    pub fn results_dir(&self) -> PathBuf {
        self.state_dir().join("results")
    }

    /// `helper/update/`: an update's download and its markers (P4.3.8, `crate::update::files`).
    #[must_use]
    pub fn update_dir(&self) -> PathBuf {
        self.state_dir().join("update")
    }
}

/// Reads one environment variable; tests pass their own.
pub type Env<'a> = &'a dyn Fn(&str) -> Option<OsString>;

/// The process environment, for [`Env`].
#[must_use]
pub fn process_env(name: &str) -> Option<OsString> {
    std::env::var_os(name)
}

fn set(env: Env<'_>, name: &str) -> Option<PathBuf> {
    env(name).filter(|v| !v.is_empty()).map(PathBuf::from)
}

/// The platform's per-user base, as `paths._user_base` gives it without `CITAR_DATA_DIR`:
/// `%LOCALAPPDATA%\CITAR`, `~/Library/Application Support/CITAR`, or `$XDG_DATA_HOME/citar`
/// (`~/.local/share/citar`). `None` when the environment names no home directory at all.
#[must_use]
pub fn platform_base(env: Env<'_>) -> Option<PathBuf> {
    if cfg!(windows) {
        let local = set(env, "LOCALAPPDATA")
            .or_else(|| set(env, "USERPROFILE").map(|h| h.join("AppData").join("Local")))?;
        Some(local.join("CITAR"))
    } else if cfg!(target_os = "macos") {
        Some(set(env, "HOME")?.join("Library").join("Application Support").join("CITAR"))
    } else {
        let data = set(env, "XDG_DATA_HOME")
            .or_else(|| set(env, "HOME").map(|h| h.join(".local").join("share")))?;
        Some(data.join("citar"))
    }
}

/// The home: `--home`, else `CITAR_DATA_DIR`, else the platform's per-user base.
///
/// # Errors
/// No flag, no variable and no home directory to find the base from.
pub fn resolve(flag: Option<&Path>, env: Env<'_>) -> Result<(Home, HomeSource), String> {
    if let Some(dir) = flag {
        return Ok((Home::at(dir), HomeSource::Flag));
    }
    if let Some(dir) = set(env, "CITAR_DATA_DIR") {
        return Ok((Home::at(dir), HomeSource::Env));
    }
    platform_base(env)
        .map(|b| (Home::at(b), HomeSource::Platform))
        .ok_or_else(|| "no home: give --home DIR (or set CITAR_DATA_DIR)".to_owned())
}

/// The development guard's rule for the home: an explicit `--home`, not inside the platform's
/// per-user base (compared as written and as the file system resolves it).
///
/// # Errors
/// What it refuses, in a sentence.
pub fn guard(flag: Option<&Path>, env: Env<'_>) -> Result<(), String> {
    let Some(home) = flag else {
        return Err("this is a development or test build: it starts only with an explicit \
                    --home DIR outside the per-user CITAR folder"
            .to_owned());
    };
    let Some(base) = platform_base(env) else { return Ok(()) };
    if is_within(home, &base) {
        return Err(format!(
            "this is a development or test build: --home {} is inside the per-user CITAR \
             folder {}, which holds a real installation's data; use a scratch directory",
            home.display(),
            base.display()
        ));
    }
    Ok(())
}

/// `p` made absolute and with `.` and `..` resolved by their names, without touching the disk.
fn lexical(p: &Path) -> PathBuf {
    let abs = std::path::absolute(p).unwrap_or_else(|_| p.to_path_buf());
    let mut out = PathBuf::new();
    for c in abs.components() {
        match c {
            Component::CurDir => {}
            Component::ParentDir => {
                out.pop();
            }
            other => out.push(other.as_os_str()),
        }
    }
    out
}

/// The path's parts as compared: on Windows case does not matter.
fn parts(p: &Path) -> Vec<String> {
    p.components()
        .map(|c| {
            let s = c.as_os_str().to_string_lossy().replace('\\', "/");
            if cfg!(windows) { s.to_lowercase() } else { s }
        })
        .collect()
}

fn under(inner: &Path, outer: &Path) -> bool {
    let (i, o) = (parts(inner), parts(outer));
    i.len() >= o.len() && i[..o.len()] == o[..]
}

/// Whether `inner` is `outer` or inside it, by name or as the file system resolves either.
#[must_use]
pub fn is_within(inner: &Path, outer: &Path) -> bool {
    let (li, lo) = (lexical(inner), lexical(outer));
    if under(&li, &lo) {
        return true;
    }
    let resolve = |p: &Path| -> Option<PathBuf> {
        // The deepest existing ancestor, resolved, and the rest of the path after it.
        let mut existing = p.to_path_buf();
        let mut rest = Vec::new();
        while !existing.exists() {
            rest.push(existing.file_name()?.to_os_string());
            if !existing.pop() {
                return None;
            }
        }
        let mut out = std::fs::canonicalize(&existing).ok()?;
        for part in rest.iter().rev() {
            out.push(part);
        }
        Some(out)
    };
    match (resolve(&li), resolve(&lo)) {
        (Some(ri), Some(ro)) => under(&ri, &ro),
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    fn env_of(pairs: &[(&str, &str)]) -> impl Fn(&str) -> Option<OsString> + use<> {
        let map: BTreeMap<String, OsString> =
            pairs.iter().map(|(k, v)| ((*k).to_owned(), OsString::from(v))).collect();
        move |k| map.get(k).cloned()
    }

    #[test]
    fn the_home_is_the_flag_then_the_variable_then_the_platform_base() {
        let env = env_of(&[
            ("CITAR_DATA_DIR", "/data/citar"),
            ("LOCALAPPDATA", "/la"),
            ("HOME", "/h"),
            ("XDG_DATA_HOME", "/xdg"),
        ]);
        let (h, s) = resolve(Some(Path::new("/given")), &env).expect("a home");
        assert_eq!((h.root(), s), (Path::new("/given"), HomeSource::Flag));
        let (h, s) = resolve(None, &env).expect("a home");
        assert_eq!((h.root(), s), (Path::new("/data/citar"), HomeSource::Env));
        let env = env_of(&[("LOCALAPPDATA", "/la"), ("HOME", "/h"), ("XDG_DATA_HOME", "/xdg")]);
        let (h, s) = resolve(None, &env).expect("a home");
        assert_eq!(s, HomeSource::Platform);
        let want = if cfg!(windows) {
            PathBuf::from("/la/CITAR")
        } else if cfg!(target_os = "macos") {
            PathBuf::from("/h/Library/Application Support/CITAR")
        } else {
            PathBuf::from("/xdg/citar")
        };
        assert_eq!(h.root(), want);
        assert!(resolve(None, &env_of(&[])).is_err());
        assert_eq!(h.config_file(), want.join("helper.json"));
        assert_eq!(h.key_file(), want.join("helper").join("machine-key.json"));
    }

    #[test]
    fn the_guard_wants_an_explicit_home_outside_the_base() {
        let tmp = std::env::temp_dir().join(format!("citar-home-guard-{}", std::process::id()));
        let env = env_of(&[
            ("LOCALAPPDATA", tmp.join("la").to_str().expect("utf-8")),
            ("HOME", tmp.join("h").to_str().expect("utf-8")),
            ("XDG_DATA_HOME", tmp.join("xdg").to_str().expect("utf-8")),
        ]);
        let base = platform_base(&env).expect("a base");
        assert!(guard(None, &env).expect_err("no --home").contains("explicit --home"));
        assert!(guard(Some(&base), &env).expect_err("the base").contains("inside"));
        assert!(guard(Some(&base.join("x").join("..").join("y")), &env).is_err());
        if cfg!(windows) {
            let upper = PathBuf::from(base.to_string_lossy().to_uppercase()).join("z");
            assert!(guard(Some(&upper), &env).is_err(), "case does not matter on Windows");
        }
        assert!(guard(Some(&tmp.join("scratch")), &env).is_ok());
        assert!(guard(Some(&base.with_file_name("CITAR-other")), &env).is_ok());
    }
}
