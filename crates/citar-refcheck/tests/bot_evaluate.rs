//! `citar_bot::evaluate_offer`, which `EngineGame.bot_evaluate` binds (citar-py's
//! `Game.bot_evaluate`; design/phase3-features.md P3.6.6, package 3-14's gate 7), on the deals of
//! refcheck's `deal_checks` for the committed states: the deal items as Python wrote them, read as
//! `open_negotiation` reads them, are worth exactly what `citar_bot::evaluate` makes of the
//! proposal from each side, which is the `bot_value` the recording holds (enforced, with
//! `--with-bot`). Items the engine cannot read are refused alike.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use citar_bot::{BotSpec, Overrides, Tuning, VersionId};
use citar_engine::base::ids::PlayerId;
use citar_engine::game::Game;
use citar_engine::game::diplomacy::deals::make_proposal;
use citar_engine::rules::Ruleset;
use serde_json::Value;

fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("..").join("..")
}

fn gunzip(path: &Path) -> Vec<u8> {
    use std::io::Read;
    let f = std::fs::File::open(path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
    let mut out = Vec::new();
    flate2::read::GzDecoder::new(f).read_to_end(&mut out).expect("gzip");
    out
}

/// Every committed state: the mini and the late fixtures.
fn states() -> Vec<PathBuf> {
    let mut out = Vec::new();
    for set in ["refcheck/fixtures-mini", "refcheck/fixtures-late"] {
        let dir = root().join(set);
        let mut cases: Vec<PathBuf> = std::fs::read_dir(&dir)
            .expect("a fixture set")
            .map(|e| e.expect("an entry").path())
            .collect();
        cases.sort();
        for case in cases {
            let mut turns: Vec<PathBuf> = std::fs::read_dir(&case)
                .expect("a case")
                .map(|e| e.expect("an entry").path())
                .filter(|p| p.to_string_lossy().ends_with(".json.gz"))
                .collect();
            turns.sort();
            out.extend(turns);
        }
    }
    out
}

/// The bot Python's recording asked: `basic-1` at its defaults and the default aggression.
fn fresh_bot() -> BotSpec {
    let tuning = Arc::new(Tuning::new(VersionId::Basic1, Overrides::default()));
    BotSpec::new(VersionId::Basic1, tuning, None, None)
}

fn pid(v: &Value) -> PlayerId {
    PlayerId(v.as_u64().and_then(|n| u8::try_from(n).ok()).expect("a player id"))
}

#[test]
fn bot_evaluate_is_the_bots_evaluate_on_every_recorded_deal() {
    let spec = fresh_bot();
    let (mut deals, mut valued) = (0usize, 0usize);
    for path in states() {
        let doc: Value = serde_json::from_slice(&gunzip(&path)).expect("a fixture");
        let state = serde_json::to_vec(&doc["state"]).expect("a state");
        let g: Game = Game::from_python(Ruleset::shared(), &state).expect("the state loads").0;
        let recorded =
            doc["queries"]["deal_checks"]["deals"].as_array().cloned().unwrap_or_default();
        for d in &recorded {
            deals += 1;
            let (a, b) = (pid(&d["a"]), pid(&d["b"]));
            let (give, receive) = (&d["give"], &d["receive"]);
            let offer = citar_bot::evaluate_offer(&g, &spec, a, b, Some(give), Some(receive));
            let theirs = citar_bot::evaluate_offer(&g, &spec, b, a, Some(receive), Some(give));
            match make_proposal(&g, a, b, Some(give), Some(receive)) {
                Err(e) => {
                    let refused = offer.expect_err("refused as the engine refuses it");
                    assert_eq!(refused.message, e.message, "{}: {d}", path.display());
                }
                Ok(None) => assert_eq!(offer.expect("no items"), 0.0),
                Ok(Some(t)) => {
                    valued += 1;
                    let mine = citar_bot::evaluate(&g, &spec, a, b, t.gives(a), t.gives(b));
                    let other = citar_bot::evaluate(&g, &spec, b, a, t.gives(b), t.gives(a));
                    let got = offer.expect("valued");
                    assert_eq!(got.to_bits(), mine.to_bits(), "{}: {d}", path.display());
                    assert_eq!(
                        theirs.expect("valued").to_bits(),
                        other.to_bits(),
                        "{}",
                        path.display()
                    );
                    // and so the recording's: refcheck enforces `bot_value` with `--with-bot`
                    let want = d["bot_value"][a.0.to_string()].as_f64().expect("a recorded value");
                    assert!(
                        (got - want).abs() < 1e-6,
                        "{}: {got} against {want}: {d}",
                        path.display()
                    );
                }
            }
        }
    }
    assert!(deals >= 80 && valued >= 60, "{deals} recorded deals, {valued} valued");
}
