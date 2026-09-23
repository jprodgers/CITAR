//! The keyed random number generator (DESIGN.md 7.1 and 7.2).
//!
//! Every random draw in a game comes from [`Rng::keyed`]: a fresh xoshiro256++ stream derived
//! from the game seed, a [`Purpose`] and a few semantic integers (turn, tile, ids). A draw
//! therefore depends only on what it is for and where it happens, never on how many draws came
//! before it, so porting or tuning one system cannot shift another's dice, and loading a save
//! needs no generator state.
//!
//! Replaces `game.py:557-559` (`state_rng`, which seeded a Mersenne Twister from a blake2b of the
//! keys joined as text), the sequential stream `g.rng` with `save_rng` (`game.py:118-122`,
//! `995-1002`), and map generation's `_side_rng` (`mapgen.py:1476-1490`). Combat, which drew from
//! `g.rng`, keys its streams with a persisted `combat_seq` counter instead.
//!
//! The generator is our own: about a hundred lines that no crate release can change. Committed
//! vectors (`crates/citar-testkit/golden/rng.json`) pin the derivation and every distribution on
//! all five targets.

/// Mixes the seed in first, so seed 0 with no keys does not start from an all-zero hash.
const SEED_DOMAIN: u64 = 0x4349_5441_5220_524E; // "CITAR RN"

/// SplitMix64's increment, the golden ratio in 64 bits.
const GAMMA: u64 = 0x9E37_79B9_7F4A_7C15;

/// SplitMix64's output function: a bijection of `u64` with good avalanche.
#[inline]
const fn finalize(mut z: u64) -> u64 {
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    z ^ (z >> 31)
}

/// One SplitMix64 step from state `x`.
#[inline]
const fn mix(x: u64) -> u64 {
    finalize(x.wrapping_add(GAMMA))
}

/// Defines [`Purpose`] and [`Purpose::ALL`] from one list, so a purpose cannot be added without
/// being listed, and so pinned in `rng.json`.
macro_rules! purposes {
    ($($(#[$group:meta])* $name:ident = $code:literal,)*) => {
        /// What a random draw is for. Each purpose is its own family of streams.
        ///
        /// **The discriminants are frozen.** Changing one rerolls every draw of that purpose in
        /// every game, so a new purpose takes a new number and an old one is never reused. The
        /// groups leave room to grow; `crates/citar-testkit/golden/rng.json` lists them all, so a
        /// change fails the golden check. `BotBase` and above are reserved for the Phase 2 bot,
        /// one per decision type.
        #[repr(u32)]
        #[non_exhaustive]
        #[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
        pub enum Purpose {
            $($(#[$group])* $name = $code,)*
        }

        impl Purpose {
            /// Every purpose, in discriminant order. The macro that defines the enum writes this
            /// list too, so it cannot miss one.
            pub const ALL: &'static [Purpose] = &[$(Self::$name,)*];
        }
    };
}

purposes! {
    // Combat
    Combat = 0x0100,
    Intercept = 0x0101,
    InterceptOrder = 0x0102,
    Nuke = 0x0103,
    // Barbarians
    BarbPlace = 0x0200,
    BarbUnit = 0x0201,
    BarbSpawn = 0x0202,
    BarbCountdown = 0x0203,
    BarbSack = 0x0204,
    Wander = 0x0205,
    // Cities
    Demand = 0x0300,
    DemandNew = 0x0301,
    CaptureGold = 0x0302,
    CaptureBuildings = 0x0303,
    // City-states
    CsInit = 0x0400,
    CsUnit = 0x0401,
    CsGiftUnit = 0x0402,
    CsGp = 0x0403,
    CsGpGiver = 0x0404,
    CsAttacked = 0x0405,
    Quest = 0x0406,
    Quests = 0x0407,
    // Espionage
    Spy = 0x0500,
    Election = 0x0501,
    ElectionDelay = 0x0502,
    // Religion and ruins
    Prophet = 0x0600,
    Ruins = 0x0601,
    // Uniques
    Trigger = 0x0700,
    Chance = 0x0701,
    // Turns
    Revolt = 0x0800,
    RevoltDelay = 0x0801,
    UnVote = 0x0802,
    // Workers
    Pillage = 0x0900,
    // Setup
    NationShuffle = 0x0A00,
    MapPrepare = 0x0A01,
    // The production advisor
    Advisor = 0x0B00,
    // Map generation: one stream per phase, so tuning one phase leaves the others alone
    MapIce = 0x0C00,
    MapLand = 0x0C01,
    MapClimate = 0x0C02,
    MapRelief = 0x0C03,
    MapLakes = 0x0C04,
    MapVegetation = 0x0C05,
    MapRivers = 0x0C06,
    MapStarts = 0x0C07,
    MapWonders = 0x0C08,
    MapResources = 0x0C09,
    MapRuins = 0x0C0A,
    // Tests
    TestAgent = 0x0F00,
    // Reserved for the Phase 2 bot: BotBase and up, one per decision type
    BotBase = 0x1000_0000,
}

impl Purpose {
    /// The frozen discriminant.
    #[must_use]
    #[inline]
    pub const fn code(self) -> u32 {
        self as u32
    }
}

/// A part of an RNG key: a semantic integer.
///
/// Ids give their raw value (below 2^32), `bool` gives 0 or 1, and `i32` (turns) its two's
/// complement bits as a `u32`. `None` gives `u64::MAX`, which no id or turn can equal, so
/// `None` never collides with tile 0 or player 0. Python's text keys kept them apart as `"None"`
/// and `"0"`.
///
/// Keys are never floats, strings or volatile counts (DESIGN.md 7.2).
pub trait KeyPart {
    /// The key word.
    fn key(self) -> u64;
}

impl KeyPart for u8 {
    #[inline]
    fn key(self) -> u64 {
        u64::from(self)
    }
}

impl KeyPart for u16 {
    #[inline]
    fn key(self) -> u64 {
        u64::from(self)
    }
}

impl KeyPart for u32 {
    #[inline]
    fn key(self) -> u64 {
        u64::from(self)
    }
}

/// Taken as it is: a `u64` key must stay below `u64::MAX` to keep clear of `None`.
impl KeyPart for u64 {
    #[inline]
    fn key(self) -> u64 {
        self
    }
}

impl KeyPart for i32 {
    #[inline]
    fn key(self) -> u64 {
        u64::from(self.cast_unsigned())
    }
}

impl KeyPart for bool {
    #[inline]
    fn key(self) -> u64 {
        u64::from(self)
    }
}

impl<T: KeyPart> KeyPart for Option<T> {
    #[inline]
    fn key(self) -> u64 {
        self.map_or(u64::MAX, KeyPart::key)
    }
}

/// A xoshiro256++ generator. Only [`Rng::keyed`] makes one.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Rng {
    s: [u64; 4],
}

impl Rng {
    /// The stream for `purpose` and `keys` in the game with this seed.
    ///
    /// The seed, the purpose, the number of keys and each key are folded in turn through
    /// SplitMix64, and four SplitMix64 outputs from the result make the state. The outputs come
    /// from four distinct inputs of a bijection, so at most one is zero and the state never is.
    /// The count of keys goes in first, so `[a]` and `[a, 0]` are different streams.
    #[must_use]
    pub fn keyed(seed: u64, purpose: Purpose, keys: &[u64]) -> Self {
        let mut h = mix(seed ^ SEED_DOMAIN);
        h = mix(h ^ u64::from(purpose.code()));
        h = mix(h ^ keys.len() as u64);
        for &k in keys {
            h = mix(h ^ k);
        }
        let mut s = [0u64; 4];
        for word in &mut s {
            h = h.wrapping_add(GAMMA);
            *word = finalize(h);
        }
        Self { s }
    }

    /// The next 64 random bits.
    #[inline]
    pub fn next_u64(&mut self) -> u64 {
        let [s0, s1, s2, s3] = self.s;
        let result = s0.wrapping_add(s3).rotate_left(23).wrapping_add(s0);
        let t = s1 << 17;
        let s2 = s2 ^ s0;
        let s3 = s3 ^ s1;
        let s1 = s1 ^ s2;
        let s0 = s0 ^ s3;
        let s2 = s2 ^ t;
        let s3 = s3.rotate_left(45);
        self.s = [s0, s1, s2, s3];
        result
    }

    /// A uniform integer in `0..n`, without bias (Lemire's method, with rejection).
    ///
    /// `n == 0` returns 0 without drawing. Otherwise it draws once, and again on the rare
    /// rejection (at most `n / 2^64` of the time).
    #[inline]
    pub fn below(&mut self, n: u64) -> u64 {
        if n == 0 {
            return 0;
        }
        let mut m = u128::from(self.next_u64()) * u128::from(n);
        if (m as u64) < n {
            let threshold = n.wrapping_neg() % n;
            while (m as u64) < threshold {
                m = u128::from(self.next_u64()) * u128::from(n);
            }
        }
        (m >> 64) as u64
    }

    /// A uniform integer in `lo..=hi`. Returns `lo` without drawing when `hi < lo`.
    pub fn range(&mut self, lo: i64, hi: i64) -> i64 {
        if hi < lo {
            return lo;
        }
        let span = i128::from(hi) - i128::from(lo) + 1;
        match u64::try_from(span) {
            Ok(span) => {
                // lo + below(span) lies in lo..=hi, so it fits an i64.
                let v = i128::from(lo) + i128::from(self.below(span));
                i64::try_from(v).unwrap_or(hi)
            }
            // The whole of i64: every bit pattern is a valid answer.
            Err(_) => self.next_u64().cast_signed(),
        }
    }

    /// A uniform float in `[0, 1)`, a multiple of 2^-53.
    #[inline]
    pub fn unit(&mut self) -> f64 {
        const TWO_POW_M53: f64 = 1.0 / 9_007_199_254_740_992.0;
        (self.next_u64() >> 11) as f64 * TWO_POW_M53
    }

    /// True with probability `p`: one draw of [`unit`](Self::unit) against it. Always draws, so a
    /// `p` of 0 or 1 consumes the same as any other.
    #[inline]
    pub fn chance(&mut self, p: f64) -> bool {
        self.unit() < p
    }

    /// Shuffles in place: Fisher-Yates from the last element down.
    pub fn shuffle<T>(&mut self, v: &mut [T]) {
        for i in (1..v.len()).rev() {
            // below(i + 1) <= i, so it indexes the slice.
            let j = self.below(i as u64 + 1) as usize;
            v.swap(i, j);
        }
    }

    /// A uniform element, or `None` from an empty slice (without drawing).
    pub fn pick<'a, T>(&mut self, v: &'a [T]) -> Option<&'a T> {
        if v.is_empty() {
            return None;
        }
        v.get(self.below(v.len() as u64) as usize)
    }

    /// An index drawn with probability proportional to its weight.
    ///
    /// The weights are summed in index order, and one [`unit`](Self::unit) times the total is
    /// located in the running sum. Weights that are not positive and finite count as zero.
    /// Returns `None` without drawing when no weight counts.
    pub fn weighted(&mut self, w: &[f64]) -> Option<usize> {
        let counts = |x: f64| x > 0.0 && x.is_finite();
        let mut total = 0.0;
        for &x in w {
            if counts(x) {
                total += x;
            }
        }
        if !(total > 0.0 && total.is_finite()) {
            return None;
        }
        let r = self.unit() * total;
        let mut acc = 0.0;
        let mut last = None;
        for (i, &x) in w.iter().enumerate() {
            if !counts(x) {
                continue;
            }
            acc += x;
            last = Some(i);
            if r < acc {
                return Some(i);
            }
        }
        // Only reachable if rounding left r at the very top of the sum.
        last
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn discriminants_are_unique_and_ascending() {
        for pair in Purpose::ALL.windows(2) {
            assert!(pair[0].code() < pair[1].code(), "{:?} and {:?}", pair[0], pair[1]);
        }
        assert_eq!(Purpose::ALL.first(), Some(&Purpose::Combat));
        assert_eq!(Purpose::ALL.last(), Some(&Purpose::BotBase));
        assert_eq!(Purpose::BotBase.code(), 0x1000_0000);
    }

    #[test]
    fn keys_are_distinct_streams() {
        let draw = |keys: &[u64]| Rng::keyed(7, Purpose::Combat, keys).next_u64();
        assert_ne!(draw(&[]), draw(&[0]));
        assert_ne!(draw(&[0]), draw(&[0, 0]));
        assert_ne!(draw(&[None::<u32>.key()]), draw(&[Some(0u32).key()]));
        assert_ne!(
            Rng::keyed(7, Purpose::Combat, &[]).next_u64(),
            Rng::keyed(7, Purpose::Nuke, &[]).next_u64()
        );
        assert_eq!(draw(&[1, 2]), draw(&[1, 2]));
    }

    #[test]
    fn distributions_stay_in_range() {
        let mut rng = Rng::keyed(1, Purpose::TestAgent, &[]);
        for n in [1u64, 2, 3, 7, 1 << 40, u64::MAX] {
            for _ in 0..100 {
                assert!(rng.below(n) < n);
            }
        }
        assert_eq!(rng.below(0), 0);
        for _ in 0..100 {
            let v = rng.range(-3, 3);
            assert!((-3..=3).contains(&v));
            let u = rng.unit();
            assert!((0.0..1.0).contains(&u));
        }
        assert_eq!(rng.range(5, 4), 5);
        let _any: i64 = rng.range(i64::MIN, i64::MAX);
        assert_eq!(rng.weighted(&[0.0, -1.0, f64::NAN]), None);
        assert_eq!(rng.weighted(&[0.0, 2.0, 0.0]), Some(1));
        assert_eq!(rng.pick::<u8>(&[]), None);
        let mut v: Vec<u32> = (0..50).collect();
        rng.shuffle(&mut v);
        v.sort();
        assert_eq!(v, (0..50).collect::<Vec<_>>());
    }
}
