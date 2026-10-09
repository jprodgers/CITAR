//! `cargo xtask develop [--release] [--venv DIR]`: the laptop's dev loop for the extension
//! (DESIGN.md P2.6.7).
//!
//! `maturin develop` would write `citar/_engine.pyd` into the checkout, which on the laptop is
//! inside OneDrive (os error 32 and sync churn, DESIGN.md 2.7), and a running server holds the
//! file open. Instead this:
//! 1. refuses unless the venv (the active one, `VIRTUAL_ENV`, or `--venv`) belongs to this
//!    worktree: one venv per worktree, so two lanes never test each other's build. A venv
//!    belongs to the worktree its `citar-dev.pth` names, and a venv without one is claimed;
//! 2. installs the project's dependencies (and the `dev` extra's) into it when `pyproject.toml`
//!    changed since the last time (`citar-dev.pyproject` in the venv keeps the copy it was
//!    installed from);
//! 3. builds `citar-py` with cargo, with the `test-ops` feature, in the `ci` profile (or with
//!    `--release` the `release` one, for timing), with the build label from `git describe` of
//!    the nearest release tag (`v*`). The build goes to `$CARGO_TARGET_DIR/develop`: the label
//!    changes the engine's build, so sharing the test builds' directory would rebuild the engine
//!    on every switch between them;
//! 4. copies the library to `$CARGO_TARGET_DIR/citar-ext/_engine.<ext>`, renaming a copy a
//!    running process holds aside;
//! 5. writes `citar-dev.pth` into the venv: the checkout on `sys.path`, and `CITAR_EXT_DIR`,
//!    which `citar/__init__.py` puts first on the package's path, so `import citar._engine`
//!    finds the built library while every other module comes from the checkout.
//!
//! Nothing is written under the checkout. Exit codes: 0 built, 1 a step failed, 2 refused.

use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};

/// The file in the venv's site-packages that ties it to a worktree.
const PTH: &str = "citar-dev.pth";

/// The copy of `pyproject.toml` the venv's dependencies were installed from.
const STAMP: &str = "citar-dev.pyproject";

/// What `develop` was asked for.
#[derive(Debug, Default, PartialEq, Eq)]
struct Options {
    release: bool,
    venv: Option<PathBuf>,
}

fn options(args: &[String]) -> Result<Options, String> {
    let mut o = Options::default();
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--release" => o.release = true,
            "--venv" => o.venv = Some(it.next().ok_or("--venv needs a directory")?.into()),
            other => {
                return Err(format!(
                    "unknown option {other} (develop takes --release and --venv DIR)"
                ));
            }
        }
    }
    Ok(o)
}

/// A path as it is compared: absolute, with `\` as `/`, without a trailing `/`, and on Windows
/// in lower case, which the file system ignores.
fn same_path_key(p: &Path) -> String {
    let p = std::fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf());
    let mut s = p.to_string_lossy().replace('\\', "/");
    // canonicalize on Windows gives the verbatim form.
    if let Some(rest) = s.strip_prefix("//?/") {
        s = rest.to_owned();
    }
    while s.ends_with('/') && s.len() > 1 {
        s.pop();
    }
    if cfg!(windows) { s.to_lowercase() } else { s }
}

/// Whether `inner` is `outer` or inside it.
fn is_within(inner: &Path, outer: &Path) -> bool {
    let (i, o) = (same_path_key(inner), same_path_key(outer));
    i == o || i.starts_with(&format!("{o}/"))
}

/// The worktree a venv's `citar-dev.pth` names: its first line.
fn owner_of(pth_text: &str) -> Option<PathBuf> {
    pth_text
        .lines()
        .map(str::trim)
        .find(|l| !l.is_empty() && !l.starts_with('#'))
        .map(PathBuf::from)
}

/// The `.pth` that ties a venv to `root` and points the package at `ext`: the checkout on
/// `sys.path`, and `CITAR_EXT_DIR` unless the environment sets it already.
fn pth_text(root: &Path, ext: &Path) -> String {
    let r = root.to_string_lossy().replace('\\', "/");
    let e = ext.to_string_lossy().replace('\\', "/");
    format!(
        "{r}\nimport os; os.environ.setdefault('CITAR_EXT_DIR', {e:?})  # cargo xtask develop\n"
    )
}

/// The requirements to install from `pyproject.toml`: the project's, then its `dev` extra's,
/// the project's own extras (`citar[all]`) expanded, each once, in the file's order.
fn requirements(pyproject: &str) -> Result<Vec<String>, String> {
    let doc: toml::Table = pyproject.parse().map_err(|e| format!("pyproject.toml: {e}"))?;
    let project = doc
        .get("project")
        .and_then(toml::Value::as_table)
        .ok_or("pyproject.toml has no [project]")?;
    let req = Reqs {
        name: project.get("name").and_then(toml::Value::as_str).unwrap_or("citar"),
        extras: project.get("optional-dependencies").and_then(toml::Value::as_table),
    };
    let mut seen = vec!["dev".to_owned()];
    let mut out = Vec::new();
    req.expand(&strings(project.get("dependencies")), &mut seen, &mut out);
    req.expand(&req.extra("dev"), &mut seen, &mut out);
    Ok(out)
}

/// A TOML array of strings.
fn strings(v: Option<&toml::Value>) -> Vec<String> {
    v.and_then(toml::Value::as_array)
        .map(|a| a.iter().filter_map(toml::Value::as_str).map(str::to_owned).collect())
        .unwrap_or_default()
}

/// The project's name and extras, to expand its requirements with.
struct Reqs<'a> {
    name: &'a str,
    extras: Option<&'a toml::Table>,
}

impl Reqs<'_> {
    fn extra(&self, name: &str) -> Vec<String> {
        strings(self.extras.and_then(|x| x.get(name)))
    }

    /// The extras a requirement on the project itself names (`citar[all]`), else `None`.
    fn own_extras<'r>(&self, req: &'r str) -> Option<Vec<&'r str>> {
        let rest = req.strip_prefix(self.name)?.trim_start().strip_prefix('[')?;
        let names = rest.split(']').next().unwrap_or_default();
        Some(names.split(',').map(str::trim).filter(|e| !e.is_empty()).collect())
    }

    /// Adds `reqs` to `out`, each once, expanding the project's own extras not `seen` yet.
    fn expand(&self, reqs: &[String], seen: &mut Vec<String>, out: &mut Vec<String>) {
        for r in reqs {
            match self.own_extras(r) {
                Some(names) => {
                    for e in names {
                        if !seen.iter().any(|s| s == e) {
                            seen.push(e.to_owned());
                            self.expand(&self.extra(e), seen, out);
                        }
                    }
                }
                None if !out.contains(r) => out.push(r.clone()),
                None => {}
            }
        }
    }
}

/// The library cargo writes for `citar-py` and the name Python imports it by.
const fn library_names() -> (&'static str, &'static str) {
    if cfg!(windows) {
        ("_engine.dll", "_engine.pyd")
    } else if cfg!(target_os = "macos") {
        ("lib_engine.dylib", "_engine.abi3.so")
    } else {
        ("lib_engine.so", "_engine.abi3.so")
    }
}

/// The venv's interpreter.
fn venv_python(venv: &Path) -> PathBuf {
    if cfg!(windows) {
        venv.join("Scripts").join("python.exe")
    } else {
        venv.join("bin").join("python")
    }
}

/// Runs `cmd`, failing with its name when it does not succeed.
fn run(cmd: &mut Command, what: &str) -> Result<(), String> {
    println!("develop: {what}");
    let status = cmd.status().map_err(|e| format!("{what}: {e}"))?;
    if status.success() { Ok(()) } else { Err(format!("{what} failed ({status})")) }
}

/// The text a command prints, trimmed.
fn output(cmd: &mut Command, what: &str) -> Result<String, String> {
    let out = cmd.output().map_err(|e| format!("{what}: {e}"))?;
    if !out.status.success() {
        return Err(format!("{what} failed: {}", String::from_utf8_lossy(&out.stderr).trim()));
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_owned())
}

/// Copies the built library to `dest`, renaming aside a copy a running process holds (Windows
/// refuses to replace a loaded library but lets it be renamed), and clearing the copies set
/// aside before that nothing holds any more.
fn install_library(built: &Path, dest: &Path) -> Result<(), String> {
    let dir = dest.parent().ok_or("the extension's folder has no parent")?;
    std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    let stem = dest.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    if let Ok(entries) = std::fs::read_dir(dir) {
        for e in entries.flatten() {
            let n = e.file_name().to_string_lossy().into_owned();
            if n.starts_with(&format!("{stem}.old-")) {
                // Still held by a running process: it goes next time.
                let _held = std::fs::remove_file(e.path()).is_err();
            }
        }
    }
    if dest.exists() && std::fs::remove_file(dest).is_err() {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_nanos());
        let aside = dir.join(format!("{stem}.old-{now}"));
        std::fs::rename(dest, &aside).map_err(|e| {
            format!("{} is held and could not be renamed aside: {e}", dest.display())
        })?;
        println!("develop: {} is in use; set it aside as {}", dest.display(), aside.display());
    }
    std::fs::copy(built, dest).map_err(|e| format!("copy to {}: {e}", dest.display()))?;
    Ok(())
}

/// `cargo xtask develop`.
pub fn run_develop(root: &Path, args: &[String]) -> ExitCode {
    match develop(root, args) {
        Ok(()) => ExitCode::SUCCESS,
        Err(Step::Refused(why)) => {
            eprintln!("develop: refused: {why}");
            ExitCode::from(2)
        }
        Err(Step::Failed(why)) => {
            eprintln!("develop: {why}");
            ExitCode::from(1)
        }
    }
}

/// Why `develop` stopped.
enum Step {
    Refused(String),
    Failed(String),
}

fn develop(root: &Path, args: &[String]) -> Result<(), Step> {
    let opts = options(args).map_err(Step::Refused)?;
    let venv = opts
        .venv
        .clone()
        .or_else(|| std::env::var_os("VIRTUAL_ENV").map(PathBuf::from))
        .ok_or_else(|| {
            Step::Refused(
                "no venv: activate this worktree's venv (outside the checkout), or name it with \
                 --venv DIR"
                    .to_owned(),
            )
        })?;
    let python = venv_python(&venv);
    if !python.exists() {
        return Err(Step::Refused(format!(
            "{} is no venv: {} is missing",
            venv.display(),
            python.display()
        )));
    }
    if is_within(&venv, root) {
        return Err(Step::Refused(format!(
            "the venv {} is inside the checkout; keep it outside synced folders",
            venv.display()
        )));
    }
    let target = std::env::var_os("CARGO_TARGET_DIR").map(PathBuf::from).ok_or_else(|| {
        Step::Refused(
            "CARGO_TARGET_DIR is not set: point it outside the checkout and outside synced \
             folders, one directory per worktree"
                .to_owned(),
        )
    })?;
    let target = if target.is_absolute() { target } else { root.join(target) };
    if is_within(&target, root) {
        return Err(Step::Refused(format!(
            "CARGO_TARGET_DIR {} is inside the checkout; develop writes nothing there",
            target.display()
        )));
    }

    // 1. The venv belongs to this worktree.
    let site = output(
        Command::new(&python)
            .args(["-c", "import sysconfig; print(sysconfig.get_paths()['purelib'])"]),
        "finding the venv's site-packages",
    )
    .map_err(Step::Failed)?;
    let site = PathBuf::from(site);
    let pth = site.join(PTH);
    if let Ok(text) = std::fs::read_to_string(&pth)
        && let Some(owner) = owner_of(&text)
        && same_path_key(&owner) != same_path_key(root)
    {
        return Err(Step::Refused(format!(
            "the venv {} belongs to the worktree {}; activate this worktree's own venv \
             (python -m venv DIR, outside the checkout)",
            venv.display(),
            owner.display()
        )));
    }

    // 2. The dependencies, when pyproject.toml changed.
    let pyproject = std::fs::read_to_string(root.join("pyproject.toml"))
        .map_err(|e| Step::Failed(format!("pyproject.toml: {e}")))?;
    let stamp = venv.join(STAMP);
    if std::fs::read_to_string(&stamp).ok().as_deref() != Some(pyproject.as_str()) {
        let reqs = requirements(&pyproject).map_err(Step::Failed)?;
        run(
            Command::new(&python)
                .args(["-m", "pip", "install", "--disable-pip-version-check"])
                .args(&reqs),
            &format!("installing {} requirements from pyproject.toml", reqs.len()),
        )
        .map_err(Step::Failed)?;
        std::fs::write(&stamp, &pyproject)
            .map_err(|e| Step::Failed(format!("{}: {e}", stamp.display())))?;
    }

    // 3. The build. The label names the nearest release tag (`v*`): the archive tag
    // `python-engine-0.1.6` (package 2-12) is nearer on this branch, and a Rust build labelled
    // after the Python engine would say the opposite of what it is.
    let label = output(
        Command::new("git")
            .args(["describe", "--tags", "--match", "v*", "--always", "--dirty"])
            .current_dir(root),
        "git describe",
    )
    .unwrap_or_else(|_| "unknown".to_owned());
    let profile = if opts.release { "release" } else { "ci" };
    let build_dir = target.join("develop");
    run(
        Command::new(std::env::var("CARGO").unwrap_or_else(|_| "cargo".into()))
            .args([
                "build",
                "--locked",
                "-p",
                "citar-py",
                "--features",
                "test-ops",
                "--profile",
                profile,
            ])
            .arg("--target-dir")
            .arg(&build_dir)
            .current_dir(root)
            .env("CITAR_BUILD_ID", &label)
            .env("PYO3_PYTHON", &python),
        &format!(
            "building citar-py ({profile} profile, test-ops, label {label}) into {}",
            build_dir.display()
        ),
    )
    .map_err(Step::Failed)?;

    // 4. The library, where the venv finds it.
    let (built_name, import_name) = library_names();
    let built = build_dir.join(profile).join(built_name);
    let ext = target.join("citar-ext");
    install_library(&built, &ext.join(import_name)).map_err(Step::Failed)?;

    // 5. The venv's link to this worktree and the library.
    std::fs::write(&pth, pth_text(root, &ext))
        .map_err(|e| Step::Failed(format!("{}: {e}", pth.display())))?;
    println!(
        "develop: {} -> {}; {} points {} at this worktree and CITAR_EXT_DIR at {}",
        built.display(),
        ext.join(import_name).display(),
        PTH,
        venv.display(),
        ext.display()
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn options_read_release_and_a_venv() {
        assert_eq!(options(&[]), Ok(Options::default()));
        let o = options(&["--release".into(), "--venv".into(), "C:/v".into()]).expect("valid");
        assert!(o.release);
        assert_eq!(o.venv, Some(PathBuf::from("C:/v")));
        assert!(options(&["--venv".into()]).is_err());
        assert!(options(&["--debug".into()]).is_err());
    }

    #[test]
    fn a_pth_names_its_worktree_first_and_the_extension_folder() {
        let root = Path::new("C:/work/CITAR/.claude/worktrees/p-2-06a");
        let text = pth_text(root, Path::new("C:/dev/target/citar-p-2-06a/citar-ext"));
        assert_eq!(owner_of(&text), Some(root.to_path_buf()));
        assert!(text.contains(
            "os.environ.setdefault('CITAR_EXT_DIR', \"C:/dev/target/citar-p-2-06a/citar-ext\")"
        ));
        assert_eq!(
            owner_of("\n# a comment\n  /home/x/CITAR \n"),
            Some(PathBuf::from("/home/x/CITAR"))
        );
        assert_eq!(owner_of(""), None);
    }

    #[test]
    fn paths_compare_as_the_file_system_does() {
        let a = Path::new("C:/dev/target/x");
        assert!(is_within(Path::new("C:/dev/target/x/develop"), a));
        assert!(is_within(a, a));
        assert!(!is_within(Path::new("C:/dev/target/xy"), a), "a sibling with a longer name");
        if cfg!(windows) {
            assert!(is_within(Path::new(r"c:\DEV\target\X\develop"), a));
        }
    }

    #[test]
    fn requirements_expand_the_projects_own_extras_once() {
        let py = r#"
[project]
name = "citar"
dependencies = ["fastapi>=0.115", "httpx>=0.27"]
[project.optional-dependencies]
anthropic = ["anthropic>=1.0"]
keyring = ["keyring>=25.0", "cryptography>=42.0"]
all = ["citar[anthropic,keyring]"]
server = ["citar[anthropic]"]
dev = ["citar[all]", "citar[server]", "ruff>=0.6", "httpx>=0.27"]
"#;
        assert_eq!(
            requirements(py).expect("reads"),
            [
                "fastapi>=0.115",
                "httpx>=0.27",
                "anthropic>=1.0",
                "keyring>=25.0",
                "cryptography>=42.0",
                "ruff>=0.6",
            ]
        );
        assert!(requirements("[tool.x]\n").is_err());
    }

    #[test]
    fn the_real_pyproject_names_the_server_and_the_dev_tools() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).parent().expect("the workspace");
        let text = std::fs::read_to_string(root.join("pyproject.toml")).expect("pyproject.toml");
        let reqs = requirements(&text).expect("reads");
        for want in ["fastapi", "ruff", "anthropic", "mkdocs-material"] {
            assert!(reqs.iter().any(|r| r.starts_with(want)), "{want} in {reqs:?}");
        }
        assert!(!reqs.iter().any(|r| r.starts_with("citar")), "{reqs:?}");
    }
}
