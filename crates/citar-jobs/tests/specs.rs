//! The specs' robustness (design/phase4-helper.md P4.6, 4-07's gate 5): 2,000 mutated `LabSpec`
//! and 2,000 mutated `ReferenceSpec` values through `check_spec` never panic it; whatever is
//! refused is refused with a reason (`bad_spec`), and whatever is accepted hashes again to its own
//! hash from its canonical form. The bases hold list and order overrides (and a level that is
//! one), which a seat may carry. The frames that carry them were fuzzed in 4-01, where
//! `job_offer.spec` is an opaque object; the job host's standard input is 4-08's.

mod common;

use citar_engine::rules::Ruleset;
use citar_jobs::{Kind, check_spec};
use common::{lab, mutate, reference};
use proptest::prelude::*;
use serde_json::Value;

fn check(kind: Kind, spec: &Value) -> Result<(), TestCaseError> {
    let rules = Ruleset::shared();
    match check_spec(rules, kind, spec, None) {
        Ok(c) => {
            // Accepted: its canonical form is accepted again, with the same hash.
            let again = check_spec(rules, kind, &c.canonical_value(), Some(&c.hash.to_hex()));
            prop_assert!(again.is_ok(), "{spec}: {again:?}");
        }
        Err(e) => prop_assert!(!e.0.is_empty(), "a refusal says why"),
    }
    Ok(())
}

proptest! {
    #![proptest_config(ProptestConfig {
        cases: 2000,
        failure_persistence: None,
        ..ProptestConfig::default()
    })]

    #[test]
    fn mutated_lab_specs_are_refused_with_a_reason_or_accepted_and_never_panic(
        which in 0usize..400, op in any::<u8>(), pick in any::<u8>(), n in any::<u64>(),
        ranked in any::<bool>(),
    ) {
        let kind = if ranked { Kind::RankingGame } else { Kind::LabGame };
        check(kind, &mutate(&lab(), which, op, pick, n))?;
    }

    #[test]
    fn mutated_reference_specs_are_refused_with_a_reason_or_accepted_and_never_panic(
        which in 0usize..100, op in any::<u8>(), pick in any::<u8>(), n in any::<u64>(),
    ) {
        check(Kind::ReferenceGame, &mutate(&reference(), which, op, pick, n))?;
    }
}

/// The base specs are good, and the mutations reach refusals of each sort.
#[test]
fn the_fuzzing_reaches_the_refusals() {
    let rules = Ruleset::shared();
    assert!(check_spec(rules, Kind::LabGame, &lab(), None).is_ok());
    assert!(check_spec(rules, Kind::ReferenceGame, &reference(), None).is_ok());
    let mut reasons = std::collections::BTreeSet::new();
    for which in 0..60 {
        for op in 0..4u8 {
            for pick in 0..14u8 {
                let v = mutate(&lab(), which, op, pick, 7 + u64::from(pick));
                if let Err(e) = check_spec(rules, Kind::LabGame, &v, None) {
                    let r = &e.0;
                    for sort in
                        ["unknown field", "invalid type", "missing field", "more than", "not a"]
                    {
                        if r.contains(sort) {
                            reasons.insert(sort);
                        }
                    }
                }
            }
        }
    }
    assert_eq!(reasons.len(), 5, "{reasons:?}");
}
