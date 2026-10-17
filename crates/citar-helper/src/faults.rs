//! A helper that cheats on purpose, for the verification gates (design/phase4-helper.md P4.4.6):
//! `citar-helper --test-fault MODE`, repeatable, in a build with the `test-faults` feature only.
//! `cargo xtask check` refuses the feature in every shipped build, and such a build dials loopback
//! servers only ([`crate::app::DEV_GUARD`]), so no real server ever meets one.
//!
//! Jobs are counted from this process's first accepted job, 1 for the first: the `N` of the modes.
//!
//! | Mode | What the helper does | What catches it (P4.5.8) |
//! |---|---|---|
//! | `result:N` | from the `N`th job on, one score of the result changed and its hash recomputed: the claim is validly signed and the content wrong (the cheater) | a replica: same chain, different result |
//! | `result-at:N,M,…` | the same, only for the jobs listed (the selective cheater) | a sampled replica, then withdrawal of every unverified result |
//! | `digest:R` | round `R`'s digest changed and the chain folded from it (broken hardware) | a replica: the chains part at round `R` |
//! | `sig` | every claim signed with another key than the machine's | the signature check, at once |
//! | `cpu:F` | `F` times the CPU seconds reported | the observed bound on credits (4-12) |
//! | `abandon:N` | the `N`th job accepted and never finished: nothing more is said of it | the silent run's expiry: an abandon |
//! | `hold:S` | each result held `S` seconds before it is sent, the held time reported as CPU | the reference cost's bound on credits (4-12) |
//!
//! A build without the feature has [`Faults`] with nothing in it, and none of this module's other
//! code.

#[cfg(feature = "test-faults")]
pub use self::on::{Faults, alter_digest, alter_result};

#[cfg(not(feature = "test-faults"))]
pub use self::off::Faults;

#[cfg(not(feature = "test-faults"))]
mod off {
    use std::time::Duration;

    /// No fault: a build without the `test-faults` feature.
    #[derive(Clone, Debug, Default)]
    pub struct Faults;

    impl Faults {
        /// Nothing to do.
        #[must_use]
        pub fn active(&self) -> bool {
            false
        }

        /// The `n`th job's result is altered: never.
        #[must_use]
        pub fn alters(&self, _n: u64) -> bool {
            false
        }

        /// The `n`th job is abandoned: never.
        #[must_use]
        pub fn abandons(&self, _n: u64) -> bool {
            false
        }

        /// The round whose digest is altered: none.
        #[must_use]
        pub fn digest_round(&self) -> Option<u32> {
            None
        }

        /// How many times the CPU seconds are reported: once.
        #[must_use]
        pub fn cpu_factor(&self) -> f64 {
            1.0
        }

        /// How long a result is held before it is sent: not at all.
        #[must_use]
        pub fn hold(&self) -> Option<Duration> {
            None
        }

        /// Another key to sign claims with: none.
        #[must_use]
        pub fn signer(&self) -> Option<&citar_jobs::claim::SigningKey> {
            None
        }
    }
}

#[cfg(feature = "test-faults")]
mod on {
    use std::collections::BTreeSet;
    use std::time::Duration;

    use citar_jobs::claim::SigningKey;
    use serde_json::{Map, Value};

    /// The faults a helper plays (the modes of the module's table).
    #[derive(Clone, Default)]
    pub struct Faults {
        result_from: Option<u64>,
        result_at: BTreeSet<u64>,
        digest: Option<u32>,
        sig: Option<SigningKey>,
        cpu: Option<f64>,
        abandon: Option<u64>,
        hold: Option<f64>,
        modes: Vec<String>,
    }

    impl std::fmt::Debug for Faults {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            // Never the other key's secret.
            write!(f, "Faults({})", self.modes.join(" "))
        }
    }

    fn count(text: &str, mode: &str) -> Result<u64, String> {
        match text.trim().parse::<u64>() {
            Ok(n) if n >= 1 => Ok(n),
            _ => {
                Err(format!("--test-fault {mode}: {text:?} is not a job number (1 for the first)"))
            }
        }
    }

    fn figure(text: &str, mode: &str) -> Result<f64, String> {
        match text.trim().parse::<f64>() {
            Ok(x) if (0.0..=86_400.0).contains(&x) => Ok(x),
            _ => Err(format!("--test-fault {mode}: {text:?} is not a number from 0 to 86400")),
        }
    }

    impl Faults {
        /// The modes the command line gave.
        ///
        /// # Errors
        /// A mode this helper does not know, or a figure that is not one.
        pub fn parse(modes: &[String]) -> Result<Faults, String> {
            let mut f = Faults { modes: modes.to_vec(), ..Faults::default() };
            for mode in modes {
                let (name, arg) = mode.split_once(':').unwrap_or((mode.as_str(), ""));
                match name {
                    "result" => f.result_from = Some(count(arg, mode)?),
                    "result-at" => {
                        for n in arg.split(',') {
                            f.result_at.insert(count(n, mode)?);
                        }
                    }
                    "digest" => {
                        let r = count(arg, mode)?;
                        f.digest = Some(
                            u32::try_from(r)
                                .map_err(|_| format!("--test-fault {mode}: no such round"))?,
                        );
                    }
                    "sig" if arg.is_empty() => {
                        let mut seed = [0u8; 32];
                        getrandom::fill(&mut seed)
                            .map_err(|e| format!("--test-fault sig: no random bytes: {e}"))?;
                        f.sig = Some(SigningKey::from_bytes(&seed));
                    }
                    "cpu" => f.cpu = Some(figure(arg, mode)?),
                    "abandon" => f.abandon = Some(count(arg, mode)?),
                    "hold" => f.hold = Some(figure(arg, mode)?),
                    _ => {
                        return Err(format!(
                            "--test-fault {mode:?}: the modes are result:N, result-at:N,M,..., digest:R, sig, \
                             cpu:F, abandon:N and hold:S"
                        ));
                    }
                }
            }
            Ok(f)
        }

        /// Whether any fault is played.
        #[must_use]
        pub fn active(&self) -> bool {
            !self.modes.is_empty()
        }

        /// The modes, as the log names them.
        #[must_use]
        pub fn describe(&self) -> String {
            self.modes.join(" ")
        }

        /// Whether the `n`th job's result is altered (`result:N`, `result-at:`).
        #[must_use]
        pub fn alters(&self, n: u64) -> bool {
            self.result_from.is_some_and(|from| n >= from) || self.result_at.contains(&n)
        }

        /// Whether the `n`th job is abandoned (`abandon:N`).
        #[must_use]
        pub fn abandons(&self, n: u64) -> bool {
            self.abandon == Some(n)
        }

        /// The round whose digest is altered (`digest:R`).
        #[must_use]
        pub fn digest_round(&self) -> Option<u32> {
            self.digest
        }

        /// How many times the CPU seconds are reported (`cpu:F`).
        #[must_use]
        pub fn cpu_factor(&self) -> f64 {
            self.cpu.unwrap_or(1.0)
        }

        /// How long each result is held before it is sent (`hold:S`).
        #[must_use]
        pub fn hold(&self) -> Option<Duration> {
            self.hold.map(Duration::from_secs_f64)
        }

        /// The other key claims are signed with (`sig`).
        #[must_use]
        pub fn signer(&self) -> Option<&SigningKey> {
            self.sig.as_ref()
        }
    }

    /// One score of a result changed, as the cheater changes it: a lab or ranking game's first
    /// player's score raised by one, a reference game's bots' errors counted one more, a crash's
    /// message added to. Nothing the spec decides changes, so the result still answers its spec
    /// and only a replica tells it apart. Returns whether something changed.
    pub fn alter_result(result: &mut Map<String, Value>) -> bool {
        if let Some(players) = result.get_mut("players").and_then(Value::as_object_mut)
            && let Some(score) = players.values_mut().next().and_then(|p| p.get_mut("score"))
            && let Some(n) = score.as_i64()
        {
            *score = Value::from(n + 1);
            return true;
        }
        if let Some(errors) = result.get_mut("bot_errors")
            && let Some(n) = errors.as_u64()
        {
            *errors = Value::from(n + 1);
            return true;
        }
        match result.get_mut("crash") {
            Some(Value::String(message)) => {
                message.push_str(" (altered)");
                true
            }
            Some(Value::Object(crash)) => match crash.get_mut("message") {
                Some(Value::String(message)) => {
                    message.push_str(" (altered)");
                    true
                }
                _ => false,
            },
            _ => false,
        }
    }

    /// Round `round`'s digest (1 for the first) changed, as broken hardware would compute it:
    /// returns whether the chain has that round. The caller folds the chain from it again.
    pub fn alter_digest(digests: &mut [String], round: u32) -> bool {
        let Some(d) = usize::try_from(round)
            .ok()
            .and_then(|r| r.checked_sub(1))
            .and_then(|i| digests.get_mut(i))
        else {
            return false;
        };
        let changed = blake3::hash(format!("broken hardware: {d}").as_bytes());
        *d = changed.to_hex().to_string();
        true
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        fn modes(m: &[&str]) -> Result<Faults, String> {
            Faults::parse(&m.iter().map(|s| (*s).to_owned()).collect::<Vec<_>>())
        }

        #[test]
        fn the_modes_read_and_say_what_they_do() {
            let f = modes(&["result:3"]).expect("reads");
            assert!(!f.alters(2) && f.alters(3) && f.alters(9));
            let f = modes(&["result-at:6,9"]).expect("reads");
            assert!(f.alters(6) && f.alters(9) && !f.alters(7) && !f.alters(10));
            let f = modes(&["digest:12", "cpu:3", "abandon:1", "hold:2.5", "sig"]).expect("reads");
            assert_eq!(f.digest_round(), Some(12));
            assert!((f.cpu_factor() - 3.0).abs() < 1e-12);
            assert!(f.abandons(1) && !f.abandons(2));
            assert_eq!(f.hold(), Some(Duration::from_millis(2500)));
            assert!(f.signer().is_some() && f.active());
            assert!(!format!("{f:?}").contains("Signing"), "the debug form names the modes only");
            for bad in [
                &["result:0"][..],
                &["result-at:6,x"],
                &["digest:"],
                &["cpu:-1"],
                &["bogus"],
                &["sig:2"],
            ] {
                assert!(modes(bad).is_err(), "{bad:?}");
            }
            assert!(!Faults::default().active());
        }

        #[test]
        fn a_result_and_a_digest_change_in_one_place() {
            let mut lab = serde_json::json!({"players": {"0": {"score": 10}, "1": {"score": 4}}, "turns": 30});
            assert!(alter_result(lab.as_object_mut().expect("an object")));
            assert_eq!(lab["players"]["0"]["score"], 11);
            assert_eq!(lab["players"]["1"]["score"], 4);
            let mut reference = serde_json::json!({"bot_errors": 0, "turns": 30});
            assert!(alter_result(reference.as_object_mut().expect("an object")));
            assert_eq!(reference["bot_errors"], 1);
            let mut crash = serde_json::json!({"crash": {"turn": 3, "message": "boom"}});
            assert!(alter_result(crash.as_object_mut().expect("an object")));
            assert_eq!(crash["crash"]["message"], "boom (altered)");
            let mut digests: Vec<String> = (0..20).map(|i| format!("{i:064x}")).collect();
            let before = digests.clone();
            assert!(alter_digest(&mut digests, 12));
            assert_eq!(citar_jobs::parted_at(&before, &digests), Some(12));
            assert!(!alter_digest(&mut digests, 21), "a chain shorter than the round");
            assert!(!alter_digest(&mut digests, 0));
        }
    }
}
