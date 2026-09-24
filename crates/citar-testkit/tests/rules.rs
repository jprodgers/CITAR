//! Every rule script in `tests/rules/` as its own test (DESIGN.md 9.3), through libtest-mimic so
//! that nextest lists and runs each one: `cargo nextest run -p citar-testkit --test rules`.
//! `tests/rules/README.md` is the script language.

use citar_testkit::script;
use libtest_mimic::{Arguments, Failed, Trial};

fn main() {
    let args = Arguments::from_args();
    let trials: Vec<Trial> = match script::discover() {
        Ok(paths) => paths
            .into_iter()
            .map(|path| {
                let name = path.file_stem().and_then(|s| s.to_str()).unwrap_or("script").to_owned();
                Trial::test(name, move || {
                    let s = script::load(&path).map_err(Failed::from)?;
                    script::run(&s).map_err(Failed::from)
                })
            })
            .collect(),
        Err(e) => vec![Trial::test("discover", move || Err(Failed::from(e)))],
    };
    libtest_mimic::run(&args, trials).exit();
}
