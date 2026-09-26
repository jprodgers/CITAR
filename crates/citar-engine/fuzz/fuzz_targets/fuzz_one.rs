//! Any bytes as a start and a sequence of tool calls, ends of turn and `RandomAgent` turns, every
//! step checked for the properties P2 to P7 (`citar_testkit::fuzz::fuzz_one`).

#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    citar_testkit::fuzz::fuzz_one(data);
});
