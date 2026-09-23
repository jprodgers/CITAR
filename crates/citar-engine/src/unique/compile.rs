//! The unique compiler (DESIGN.md 5.5-5.6): every unique text of a ruleset to a [`Unique`] and
//! its [`UniqueMeta`], once, at load.
//!
//! It replaces the parsing half of Python's `Unique` (`uniques.py:105-161`), the unique maps
//! `Rules._umap` built for every object (`rules.py:92-167`, `uniques.py:163-209`) and the
//! runtime copy `economy.temp_unique` made of a timed unique (`economy.py:64-75`). For each
//! source, in `civ_umaps` order, and each text in the order the file lists it:
//!
//! 1. take the text apart (`unique::text`) and look the placeholder up; an unknown text with no
//!    parameters and no modifiers may be a tag (step 8), anything else unknown is an error;
//! 2. refuse a type the engine does not support (`unique_supported.toml`);
//! 3. compile the parameters by kind (`unique::params`), with the relevant-promotion fixup of
//!    `units.py:153`;
//! 4. fold the modifiers by role: conditionals in order, one trigger at most, the action
//!    modifiers into [`ActionMods`], game speed into a flag, `for [n] turns` into a timer; a
//!    `for every` multiplier, which Python read as 1 (`uniques.py:1081-1083`), is an error, and
//!    so is a modifier the unique's role has no use for (a trigger on a standing effect, action
//!    modifiers off an action, a timer on a requirement, a region condition off map generation);
//! 5. mark it `LOCAL` when a parameter or a conditional says `in this city` (`uniques.py:130`);
//! 6. key it (FNV-1a-64 of source kind, source name, occurrence and text, DESIGN.md 7.2);
//! 7. give each timed unique a variant without the timer, in the `Temporary` block;
//! 8. make each text a filter names a tag: an unknown text becomes a [`UniqueData::Tag`], a
//!    known one without parameters gives its source the tag;
//! 9. split each source's uniques by what the engine does with them ([`SourceUniques`]). Only a
//!    building's and a resource's `LOCAL` uniques hold in one city alone.
//!
//! Every problem of every text is reported, the unknown tags included, before the load stops.

use super::generated::{
    CondData, ModifierData, Support, TYPE_INFO, TriggerCond, UniqueData, UniqueType,
};
use super::params::{Lexicon, Param, ParamCx};
use super::table::{
    ActionMods, Cond, CondDeps, CondSpan, Role, Source, SourceUniques, StaticDomain, UFlags,
    Unique, UniqueMeta, UniqueTable, unique_key,
};
use super::text::{placeholder, split_modifiers};
use crate::base::collections::{DetMap, DetSet};
use crate::base::ids::{AbilityKey, Id, IdVec, PromotionId, TagId, TextId, TileFilterId, UniqueId};
use crate::base::sets::{BitSet, TagSet};
use crate::rules::Ruleset;
use crate::rules::errors::{Problems, RulesetErrorKind};

/// One source object's unique texts, as the ruleset lists them.
pub(crate) struct SourceTexts<'a> {
    pub(crate) source: Source,
    /// The file, as errors name it.
    pub(crate) file: &'static str,
    /// The object's name.
    pub(crate) name: &'a str,
    pub(crate) texts: &'a [String],
}

/// What the compiler produces for the ruleset to keep.
pub(crate) struct Compiled {
    pub(crate) table: UniqueTable,
    pub(crate) fracs: IdVec<crate::base::ids::FracId, f64>,
    /// Each source's uniques, in the order the sources were given.
    pub(crate) sources: Vec<SourceUniques>,
    /// The handles of the tile filters given outside uniques, in the order given.
    pub(crate) tile_filters: Vec<TileFilterId>,
}

/// A problem with one text.
struct Refusal {
    kind: RulesetErrorKind,
    message: String,
}

impl Refusal {
    fn new(kind: RulesetErrorKind, message: impl Into<String>) -> Self {
        Self { kind, message: message.into() }
    }
}

/// A text compiled, before tags, temporary variants and ids.
struct Staged {
    /// Which source, as an index into the sources given.
    source: usize,
    /// `None` for a text that may be a tag (step 8).
    data: Option<UniqueData>,
    ty: Option<UniqueType>,
    role: Role,
    conds: CondSpan,
    flags: UFlags,
    text: TextId,
    occurrence: u16,
    timed: Option<u16>,
    trigger: Option<TriggerCond>,
    actions: ActionMods,
    ability: Option<AbilityKey>,
    /// The placeholder, when the unique has no parameters: the term a filter names it by. For an
    /// unknown text that is its trimmed main text, as Python's `has_tag` read `u.ph`
    /// (`uniques.py:182-188`).
    bare: Option<String>,
    tag: Option<TagId>,
    /// Stands for the timed unique at this position in `staged` (step 7).
    temporary_of: Option<usize>,
}

/// Compiles every source's texts. `filters` are the filter texts the ruleset holds outside
/// uniques that name terrains (improvements' `terrainsCanBeBuiltOn`), and `tile_filters` those
/// that are tile filters (start biases); both may name tags too, and the second are interned as
/// tile filters. Every problem is pushed to `p`; `None` means there was at least one.
pub(crate) fn compile(
    rules: &Ruleset,
    sources: &[SourceTexts<'_>],
    filters: &[&str],
    tile_filters: &[(&'static str, &str, &str)],
    p: &mut Problems,
) -> Option<Compiled> {
    let mut lx = Lexicon::new(rules);
    let mut extra = Vec::with_capacity(tile_filters.len());
    for &(file, object, text) in tile_filters {
        match lx.tile_filter(text) {
            Ok(id) => extra.push(id),
            Err(e) => {
                p.push(RulesetErrorKind::UniqueParameter, file, object, format!("[{text}]: {e}"))
            }
        }
    }
    let mut conds: Vec<Cond> = Vec::new();
    let mut staged: Vec<Staged> = Vec::new();
    let mut failed = false;
    // Every bracketed term of the texts that failed: one of them may be the filter that names a
    // tag, which must not then be reported as a typo alongside the real problem.
    let mut failed_terms: Vec<String> = Vec::new();
    for (si, src) in sources.iter().enumerate() {
        let mut seen: DetMap<&str, u16> = DetMap::default();
        for text in src.texts {
            let occurrence = {
                let n = seen.entry(text.as_str()).or_insert(0);
                let this = *n;
                *n = n.saturating_add(1);
                this
            };
            match compile_text(&mut lx, &mut conds, text) {
                Ok(mut s) => {
                    s.source = si;
                    s.occurrence = occurrence;
                    staged.push(s);
                }
                Err(r) => {
                    failed = true;
                    p.push(r.kind, src.file, src.name, format!("{text:?}: {}", r.message));
                    bracketed_terms(text, &mut failed_terms);
                }
            }
        }
    }
    // The tags are judged even when a text failed, so that one report holds every problem.
    let tag_problems = give_tags(&mut lx, &mut staged, filters, &failed_terms);
    for (i, kind, message) in &tag_problems {
        let src = &sources[staged[*i].source];
        let text = lx.text_of(staged[*i].text);
        p.push(*kind, src.file, src.name, format!("{text:?}: {message}"));
    }
    if failed || !tag_problems.is_empty() || extra.len() < tile_filters.len() {
        return None;
    }
    let variant_of = add_variants(&mut staged);
    let order = lay_out(&staged, sources);
    check_count(order.len(), p)?;
    let mut id_of = vec![UniqueId(0); staged.len()];
    for (pos, &i) in order.iter().enumerate() {
        id_of[i] = UniqueId(u16::try_from(pos).unwrap_or(u16::MAX));
    }
    let mut uniques = Vec::with_capacity(order.len());
    let mut metas = Vec::with_capacity(order.len());
    for &i in &order {
        let s = &staged[i];
        let source = match s.temporary_of {
            Some(orig) => Source::Temporary(id_of[orig]),
            None => sources[s.source].source,
        };
        let text = lx.text_of(s.text);
        let key = match s.temporary_of {
            Some(orig) => {
                let o = &sources[staged[orig].source];
                let of = format!("{}/{}", o.source.kind_name(), o.name);
                unique_key(source.kind_name(), &of, staged[orig].occurrence, text)
            }
            None => unique_key(source.kind_name(), sources[s.source].name, s.occurrence, text),
        };
        let temp_variant = variant_of[i].map(|j| id_of[j]);
        let deps = if s.conds.is_empty() { CondDeps::empty() } else { deps_of(&conds, s.conds) };
        let data = s.data.unwrap_or(UniqueData::Tag(s.tag.unwrap_or(TagId(0))));
        uniques.push(Unique::new(data, s.conds, deps, s.flags));
        metas.push(UniqueMeta {
            ty: s.ty,
            role: s.role,
            source,
            text: s.text,
            key,
            occurrence: s.occurrence,
            timed: s.timed,
            temp_variant,
            trigger: s.trigger,
            actions: s.actions,
            ability: s.ability,
            tag: s.tag,
        });
    }
    let mut table = UniqueTable {
        uniques: IdVec::from_vec(uniques),
        meta: IdVec::from_vec(metas),
        conds: conds.into_iter().collect(),
        ..UniqueTable::default()
    };
    let parts = partitions(&table, sources, &order, &staged, &id_of);
    let fracs = lx.finish(&mut table);
    Some(Compiled { table, fracs, sources: parts, tile_filters: extra })
}

/// Refuses more uniques than a [`UniqueId`] numbers. The last id must leave room for one past
/// it, which ends a [`SourceUniques::all`] range: so at most `u16::MAX` uniques, not 65,536.
fn check_count(n: usize, p: &mut Problems) -> Option<()> {
    if n > usize::from(u16::MAX) {
        p.push(
            RulesetErrorKind::Capacity,
            "",
            "",
            format!("{n} uniques, more than a UniqueId (u16) numbers; widen UniqueId"),
        );
        return None;
    }
    Some(())
}

/// The span of the conditionals `start..end`, if its end fits a `u16`: then every id in it
/// does too, and [`CondSpan::ids`] cannot overflow.
fn cond_span(start: usize, end: usize) -> Option<CondSpan> {
    let start = u16::try_from(start).ok()?;
    let end = u16::try_from(end).ok()?;
    Some(CondSpan { start, len: end.checked_sub(start)? })
}

/// What the conditionals of a span read, together.
fn deps_of(conds: &[Cond], span: CondSpan) -> CondDeps {
    span.ids().fold(CondDeps::empty(), |d, c| d | conds[c.index()].deps)
}

/// Compiles one text (steps 1 to 5).
fn compile_text(
    lx: &mut Lexicon<'_>,
    conds: &mut Vec<Cond>,
    text: &str,
) -> Result<Staged, Refusal> {
    use RulesetErrorKind as E;
    let (main, mods) = split_modifiers(text);
    let (ph, params) = placeholder(&main);
    let text_id = lx.text(text).map_err(|e| Refusal::new(E::Capacity, e))?;
    let mut staged = Staged {
        source: 0,
        data: None,
        ty: None,
        role: Role::Tag,
        conds: CondSpan::default(),
        flags: UFlags::empty(),
        text: text_id,
        occurrence: 0,
        timed: None,
        trigger: None,
        actions: ActionMods::default(),
        ability: None,
        bare: None,
        tag: None,
        temporary_of: None,
    };
    let Some(ty) = UniqueType::from_placeholder(&ph) else {
        if params.is_empty() && mods.is_empty() {
            // Perhaps a tag; step 8 decides.
            staged.bare = Some(ph);
            return Ok(staged);
        }
        return Err(Refusal::new(E::UnknownUnique, unknown(&ph)));
    };
    let support = supported(ty)?;
    let role = support.role;
    if matches!(role, Role::Cond | Role::Trigger | Role::ActionMod | Role::Meta) {
        return Err(Refusal::new(
            E::UniqueModifier,
            format!("{} is a modifier, written inside <...> after a unique", ty.name()),
        ));
    }
    let fixed = relevant_fixup(lx, ty, &params)?;
    let mut cx = ParamCx { params: &params, lx, fixed };
    let data = UniqueData::build(ty, &mut cx)
        .map_err(|e| Refusal::new(E::UniqueParameter, e.to_string()))?;
    staged.data = Some(data);
    staged.ty = Some(ty);
    staged.role = role;
    if params.is_empty() {
        staged.bare = Some(ty.placeholder().to_owned());
    }
    if params.contains(&"in this city") {
        staged.flags |= UFlags::LOCAL;
    }

    // Step 4: the modifiers. The trigger, the first action modifier and the timer are kept, to
    // name them if the unique's role has no use for them.
    let start = conds.len();
    let mut used = Placed::default();
    for m in &mods {
        let (mph, mparams) = placeholder(m);
        let Some(mty) = UniqueType::from_placeholder(&mph) else {
            return Err(Refusal::new(
                E::UnknownUnique,
                format!("the modifier <{m}>: {}", unknown(&mph)),
            ));
        };
        if matches!(
            mty,
            UniqueType::ForEveryCountable
                | UniqueType::ForEveryAdjacentTile
                | UniqueType::ForEveryAmountCountable
        ) {
            return Err(Refusal::new(
                E::UniqueModifier,
                format!(
                    "the modifier <{m}> multiplies the unique, which the engine does not do \
                     (Python read every multiplier as 1, uniques.py:1081-1083)"
                ),
            ));
        }
        let msupport = supported(mty)
            .map_err(|r| Refusal::new(r.kind, format!("the modifier <{m}>: {}", r.message)))?;
        let mut mcx = ParamCx { params: &mparams, lx: &mut *cx.lx, fixed: None };
        let bad_param = |e: super::params::ParamError| {
            Refusal::new(E::UniqueParameter, format!("the modifier <{m}>: {e}"))
        };
        match msupport.role {
            Role::Cond => {
                let data = CondData::build(mty, &mut mcx).map_err(bad_param)?;
                if mty == UniqueType::ConditionalInThisCity {
                    staged.flags |= UFlags::LOCAL;
                }
                if matches!(
                    mty,
                    UniqueType::ConditionalInRegionOfType
                        | UniqueType::ConditionalInRegionExceptOfType
                ) {
                    used.region.get_or_insert(*m);
                }
                let text = mcx.lx.text(m).map_err(|e| Refusal::new(E::Capacity, e))?;
                // What it reads depends on its filters' leaves, compiled later: the loader gives
                // it its classes then (`cond::assign_deps`), and until then it reads everything.
                conds.push(Cond { data, deps: CondDeps::all(), text });
            }
            Role::Trigger => {
                if staged.trigger.is_some() {
                    return Err(Refusal::new(
                        E::UniqueModifier,
                        format!("<{m}> is a second trigger; a unique fires on one"),
                    ));
                }
                staged.trigger = Some(TriggerCond::build(mty, &mut mcx).map_err(bad_param)?);
                staged.flags |= UFlags::TRIGGERED;
                used.trigger = Some(*m);
            }
            Role::ActionMod | Role::Meta => {
                let data = ModifierData::build(mty, &mut mcx).map_err(bad_param)?;
                if msupport.role == Role::ActionMod {
                    used.action.get_or_insert(*m);
                } else if mty == UniqueType::ConditionalTimedUnique {
                    used.timer = Some(*m);
                }
                fold(&mut staged, data)
                    .map_err(|why| Refusal::new(E::UniqueModifier, format!("<{m}> {why}")))?;
            }
            _ => {
                return Err(Refusal::new(
                    E::UniqueModifier,
                    format!("<{m}> is {}, which is not a modifier", mty.name()),
                ));
            }
        }
    }
    used.fit(ty, support)?;
    let Some(span) = cond_span(start, conds.len()) else {
        return Err(Refusal::new(E::Capacity, "more conditionals than a CondId (u16) numbers"));
    };
    staged.conds = span;
    if !staged.actions.is_empty() {
        staged.flags |= UFlags::ACTION;
    }
    if staged.actions.uses().is_some() {
        // Python counted a limited action's uses under its placeholder and parameters
        // (units.py:417); the key is kept so saves and the converter can name it.
        let key = format!("{ph}|{}", params.join("|"));
        staged.ability = Some(cx.lx.ability(&key).map_err(|e| Refusal::new(E::Capacity, e))?);
    }
    Ok(staged)
}

/// The engine's support for `ty`, or the error that says what to add.
fn supported(ty: UniqueType) -> Result<&'static super::generated::Support, Refusal> {
    TYPE_INFO[ty as usize].support.ok_or_else(|| {
        Refusal::new(
            RulesetErrorKind::UnsupportedUnique,
            format!(
                "{} (`{}`) is an UnCiv type the engine does not support: add it to \
                 crates/citar-engine/unique_supported.toml, then an arm where its stages read it, \
                 then a test (DESIGN.md 5.4)",
                ty.name(),
                ty.info().signature
            ),
        )
    })
}

fn unknown(placeholder: &str) -> String {
    format!("{placeholder:?} is no unique type UnCiv has (crates/citar-engine/unique_types.tsv)")
}

/// The modifiers of a unique that only some roles can use, as its text wrote them.
#[derive(Default)]
struct Placed<'t> {
    trigger: Option<&'t str>,
    /// The first action modifier.
    action: Option<&'t str>,
    timer: Option<&'t str>,
    /// The first region conditional.
    region: Option<&'t str>,
}

impl Placed<'_> {
    /// Refuses a modifier the unique's role has no use for. Python let each through and quietly
    /// did something else: a trigger does not filter (`uniques.py:790-791`), so a standing effect
    /// with one stood from the start; action modifiers were read on unit actions only
    /// (`units.py:401-473`); and a timed unique was stored for its turns instead of applied
    /// (`triggers.py:88-92`), which only an effect or a flag, read while it is held, turns into
    /// anything.
    fn fit(&self, ty: UniqueType, support: &Support) -> Result<(), Refusal> {
        let role = support.role;
        let name = ty.name();
        let refuse = |m: &str, why: String| {
            Err(Refusal::new(RulesetErrorKind::UniqueModifier, format!("<{m}> {why}")))
        };
        if let Some(m) = self.timer
            && !matches!(role, Role::Effect | Role::Flag)
        {
            return refuse(
                m,
                format!(
                    "grants a standing unique for a while, and {name} is a {role:?}: a timer \
                     goes on an effect or a flag"
                ),
            );
        }
        if let Some(m) = self.trigger
            && !(role == Role::OneTime || support.gain || self.timer.is_some())
        {
            return refuse(
                m,
                format!(
                    "fires once, and {name} is a standing {role:?}: a trigger goes on a one-time \
                     effect, or on an effect with <for [n] turns>"
                ),
            );
        }
        if let Some(m) = self.action
            && !matches!(role, Role::Action | Role::OneTime)
        {
            return refuse(
                m,
                format!("limits a unit action, and {name} is a {role:?}, not an action"),
            );
        }
        // Only map generation knows regions, and it builds none: the uniques it reads keep the
        // condition (`GenCond`), and inert ones are read by nothing (DESIGN.md 5.8).
        if let Some(m) = self.region
            && !matches!(role, Role::Mapgen | Role::Inert)
        {
            return refuse(
                m,
                format!(
                    "is a map-generation condition, and {name} is a {role:?}: a region condition \
                     goes only on a unique map generation reads"
                ),
            );
        }
        Ok(())
    }
}

/// Folds an action or meta modifier into the unique. `Err` says what is wrong: the modifier is
/// there already, or its count does not fit the `u16` the metadata keeps.
fn fold(s: &mut Staged, m: ModifierData) -> Result<(), &'static str> {
    const TWICE: &str = "is there twice";
    fn set<T>(slot: &mut Option<T>, v: T) -> Result<(), &'static str> {
        if slot.is_some() {
            return Err(TWICE);
        }
        *slot = Some(v);
        Ok(())
    }
    fn flag(slot: &mut bool) -> Result<(), &'static str> {
        if *slot {
            return Err(TWICE);
        }
        *slot = true;
        Ok(())
    }
    fn count(n: i32) -> Result<u16, &'static str> {
        u16::try_from(n).map_err(|_| "counts more than 65535")
    }
    let a = &mut s.actions;
    match m {
        ModifierData::UnitActionConsumeUnit => flag(&mut a.consume),
        ModifierData::UnitActionAfterWhichConsumed => flag(&mut a.consumed_after),
        ModifierData::UnitActionOnce => flag(&mut a.once),
        ModifierData::UnitActionLimitedTimes(x) => set(&mut a.times, count(x.times)?),
        ModifierData::UnitActionExtraLimitedTimes(x) => set(&mut a.extra_times, count(x.times)?),
        ModifierData::UnitActionMovementCost(x) => set(&mut a.movement, x.movement),
        ModifierData::ModifiedByGameSpeed => {
            if s.flags.contains(UFlags::SPEED) {
                return Err(TWICE);
            }
            s.flags |= UFlags::SPEED;
            Ok(())
        }
        ModifierData::ConditionalTimedUnique(x) => {
            s.flags |= UFlags::TIMED;
            set(&mut s.timed, count(x.turns)?)
        }
        // How UnCiv shows a unique, which changes no rule: Python passed them over when it
        // evaluated (`uniques.py:1013-1019`), and the text keeps them for views.
        ModifierData::ModifierHiddenFromUsers
        | ModifierData::CivilopediaLink(_)
        | ModifierData::SuppressWarnings(_) => Ok(()),
    }
}

/// `UnitStartingPromotions` with `[relevant]` units: the units whose type may take the
/// promotion (`units.py:153`), as a set decided here.
fn relevant_fixup(
    lx: &mut Lexicon<'_>,
    ty: UniqueType,
    params: &[&str],
) -> Result<Option<(usize, Param)>, Refusal> {
    if ty != UniqueType::UnitStartingPromotions || params.first() != Some(&"relevant") {
        return Ok(None);
    }
    let Some(pr) = params.get(2).and_then(|n| lx.rules.lookup::<PromotionId>(n)) else {
        // The promotion does not resolve; compiling it reports that.
        return Ok(None);
    };
    let types = &lx.rules.promotions()[pr].unit_types;
    let mut members = BitSet::new();
    for (id, u) in lx.rules.base_units().iter() {
        if types.contains(&u.unit_type) {
            members.insert(u32::try_from(id.index()).unwrap_or(u32::MAX));
        }
    }
    let set = lx
        .fixed_set(StaticDomain::BaseUnit, "relevant", members)
        .map_err(|e| Refusal::new(RulesetErrorKind::Capacity, e))?;
    Ok(Some((0, Param::Set(set))))
}

// ---- Step 8: tags -----------------------------------------------------------------------------

/// The terms of a filter, split as Python's `multi_filter` splits it (`uniques.py:294-327`):
/// `{a} {b}` is a conjunction, `non-[a]` a negation, and anything else one term.
pub(crate) fn filter_terms(text: &str, out: &mut Vec<String>) {
    out.extend(super::filter::parse::terms(text).into_iter().map(str::to_owned));
}

/// Every bracketed parameter of a text and of its modifiers, split into filter terms: what a text
/// that failed to compile might have named as a filter.
fn bracketed_terms(text: &str, out: &mut Vec<String>) {
    let (main, mods) = split_modifiers(text);
    for part in std::iter::once(main.as_str()).chain(mods) {
        for param in placeholder(part).1 {
            filter_terms(param, out);
        }
    }
}

/// Step 8. Returns a problem for each text that is unknown and that no filter names, and for each
/// tag past what a [`TagSet`] holds, by position in `staged`. `failed_terms` are the terms of the
/// texts that did not compile ([`bracketed_terms`]), counted as named so that a text's failure
/// does not also make its tag look like a typo.
fn give_tags(
    lx: &mut Lexicon<'_>,
    staged: &mut [Staged],
    filters: &[&str],
    failed_terms: &[String],
) -> Vec<(usize, RulesetErrorKind, String)> {
    let mut terms: DetSet<String> = DetSet::default();
    let mut buf = Vec::new();
    let texts: Vec<String> =
        lx.filter_texts().into_iter().map(|t| lx.text_of(t).to_owned()).collect();
    for f in texts.iter().map(String::as_str).chain(filters.iter().copied()) {
        buf.clear();
        filter_terms(f, &mut buf);
        terms.extend(buf.drain(..));
    }
    terms.extend(failed_terms.iter().cloned());
    let mut bad = Vec::new();
    for (i, s) in staged.iter_mut().enumerate() {
        let Some(name) = &s.bare else { continue };
        if !terms.contains(name) {
            if s.data.is_none() {
                bad.push((
                    i,
                    RulesetErrorKind::UnknownUnique,
                    format!(
                        "{} and no filter names it as a tag, so it is probably a typo",
                        unknown(name)
                    ),
                ));
            }
            continue;
        }
        match lx.tag(name) {
            Ok(t) if t.index() < TagSet::CAPACITY => {
                s.tag = Some(t);
                if s.data.is_none() {
                    s.data = Some(UniqueData::Tag(t));
                }
            }
            _ => bad.push((
                i,
                RulesetErrorKind::Capacity,
                format!(
                    "more tags than a TagSet holds ({}); widen sets::TAG_WORDS",
                    TagSet::CAPACITY
                ),
            )),
        }
    }
    bad
}

// ---- Steps 7 and 9: layout and partitions -----------------------------------------------------

/// The final order of `staged` (step 7): every unique of the sources before `Temporary` in
/// [`Source`] order, then the temporary variants in the order of their originals, then the rest.
fn lay_out(staged: &[Staged], sources: &[SourceTexts<'_>]) -> Vec<usize> {
    let temporary = |s: &Staged| s.temporary_of.is_some();
    let before = |s: &Staged| {
        !temporary(s)
            && matches!(
                sources[s.source].source,
                Source::Nation(_) | Source::Building(_) | Source::Policy(_) | Source::Tech(_)
            )
    };
    let all = 0..staged.len();
    let mut order: Vec<usize> = all.clone().filter(|&i| before(&staged[i])).collect();
    order.extend(all.clone().filter(|&i| temporary(&staged[i])));
    order.extend(all.filter(|&i| !before(&staged[i]) && !temporary(&staged[i])));
    order
}

/// Adds a variant without the timer for each timed unique (step 7), after the others. Returns,
/// for each position of `staged` after the additions, the position of its variant.
fn add_variants(staged: &mut Vec<Staged>) -> Vec<Option<usize>> {
    let timed: Vec<usize> = (0..staged.len()).filter(|&i| staged[i].timed.is_some()).collect();
    let mut variant_of = vec![None; staged.len() + timed.len()];
    for i in timed {
        variant_of[i] = Some(staged.len());
        let o = &staged[i];
        staged.push(Staged {
            source: o.source,
            data: o.data,
            ty: o.ty,
            role: o.role,
            // The same conditionals: the timer and the trigger were not conditionals.
            conds: o.conds,
            flags: (o.flags - UFlags::TIMED - UFlags::TRIGGERED) | UFlags::TEMPORARY,
            text: o.text,
            occurrence: o.occurrence,
            timed: None,
            trigger: None,
            actions: o.actions,
            ability: o.ability,
            bare: o.bare.clone(),
            tag: o.tag,
            temporary_of: Some(i),
        });
    }
    variant_of
}

/// Step 9: each source's uniques by what the engine does with them.
fn partitions(
    table: &UniqueTable,
    sources: &[SourceTexts<'_>],
    order: &[usize],
    staged: &[Staged],
    id_of: &[UniqueId],
) -> Vec<SourceUniques> {
    let mut out: Vec<SourceUniques> = vec![SourceUniques::default(); sources.len()];
    let mut ids: Vec<Vec<UniqueId>> = vec![Vec::new(); sources.len()];
    for &i in order {
        if staged[i].temporary_of.is_none() {
            ids[staged[i].source].push(id_of[i]);
        }
    }
    let mut next = 0u16;
    for (si, list) in ids.iter().enumerate() {
        let part = &mut out[si];
        let (Some(first), Some(last)) = (list.first(), list.last()) else {
            part.all = next..next;
            continue;
        };
        part.all = first.0..last.0 + 1;
        next = last.0 + 1;
        // Only a building's uniques hold in its own city alone: Python split them and no one
        // else's (`economy.py:108`, `cities.py:59`), and a resource's is the Marble decision
        // (DESIGN.md 5.12). On any other source `in this city` is the city in context, like
        // `in all cities` (`uniques.py:668`), so such a unique stands with the rest and keeps
        // its LOCAL bit for evaluation to read.
        let splits = matches!(sources[si].source, Source::Building(_) | Source::Resource(_));
        let (mut civ, mut local, mut on_gain, mut triggered, mut actions, mut ai) =
            (Vec::new(), Vec::new(), Vec::new(), Vec::new(), Vec::new(), Vec::new());
        for &id in list {
            let u = table.get(id);
            let meta = table.meta(id);
            let f = u.flags();
            let gain = meta.ty.and_then(|t| t.info().support).is_some_and(|s| s.gain);
            if f.contains(UFlags::TRIGGERED) {
                triggered.push(id);
            } else if f.contains(UFlags::TIMED) {
                on_gain.push(id);
            } else {
                match meta.role {
                    Role::Effect | Role::Flag | Role::Tag => {
                        if splits && f.contains(UFlags::LOCAL) {
                            local.push(id);
                        } else {
                            civ.push(id);
                        }
                        if gain {
                            on_gain.push(id);
                        }
                    }
                    Role::OneTime if f.contains(UFlags::ACTION) => actions.push(id),
                    Role::OneTime => on_gain.push(id),
                    Role::Action => actions.push(id),
                    Role::Ai => ai.push(id),
                    _ => {}
                }
            }
            if let Some(t) = meta.tag {
                if u.conds.is_empty() {
                    part.tags.insert(t);
                } else {
                    part.cond_tags.insert(t);
                }
            }
        }
        part.civ = civ.into();
        part.local = local.into();
        part.on_gain = on_gain.into();
        part.triggered = triggered.into();
        part.actions = actions.into();
        part.ai = ai.into();
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn terms(text: &str) -> Vec<String> {
        let mut out = Vec::new();
        filter_terms(text, &mut out);
        out
    }

    #[test]
    fn filters_split_into_terms_as_multi_filter_did() {
        assert_eq!(terms("Aircraft"), ["Aircraft"]);
        assert_eq!(terms("{Military} {Water}"), ["Military", "Water"]);
        assert_eq!(terms("non-[Air]"), ["Air"]);
        assert_eq!(terms("{non-[Air]} {Wounded}"), ["Air", "Wounded"]);
        assert_eq!(terms("{a {b} c} {d}"), ["a {b} c", "d"], "split at depth 0 only");
        assert_eq!(terms("{Land}"), ["{Land}"], "one braced term is a term");
        let deep = format!("{}x{}", "non-[".repeat(100), "]".repeat(100));
        assert_eq!(terms(&deep).len(), 1, "a deep nesting stops, and does not overflow");
    }

    fn staged() -> Staged {
        Staged {
            source: 0,
            data: None,
            ty: None,
            role: Role::Effect,
            conds: CondSpan::default(),
            flags: UFlags::empty(),
            text: TextId(0),
            occurrence: 0,
            timed: None,
            trigger: None,
            actions: ActionMods::default(),
            ability: None,
            bare: None,
            tag: None,
            temporary_of: None,
        }
    }

    #[test]
    fn a_failed_text_names_its_bracketed_terms() {
        let mut out = Vec::new();
        bracketed_terms(
            "[+1 Gold] [in this city] <vs [{Stealthy} {Wounded}] units> <bad>",
            &mut out,
        );
        assert_eq!(out, ["+1 Gold", "in this city", "Stealthy", "Wounded"]);
    }

    #[test]
    fn capacities_stop_short_of_overflow() {
        let mut p = Problems::default();
        // The last id needs one past it to end a range: 65,535 uniques at most.
        assert!(check_count(usize::from(u16::MAX), &mut p).is_some());
        assert!(p.check().is_ok());
        assert!(check_count(usize::from(u16::MAX) + 1, &mut p).is_none());
        let errs = p.check().expect_err("refused");
        assert!(errs.has(RulesetErrorKind::Capacity), "{errs}");
        // A span of conditionals must end within a u16, so that its ids never overflow.
        let top = usize::from(u16::MAX);
        let span = cond_span(top - 3, top).expect("fits");
        assert_eq!(span, CondSpan { start: u16::MAX - 3, len: 3 });
        assert_eq!(
            span.ids().map(|c| usize::from(c.0)).collect::<Vec<_>>(),
            [top - 3, top - 2, top - 1]
        );
        assert_eq!(cond_span(top - 3, top + 1), None);
        assert_eq!(cond_span(5, 5), Some(CondSpan { start: 5, len: 0 }));
    }

    #[test]
    fn a_variant_is_found_from_its_original() {
        let mut list = vec![staged(), staged(), staged()];
        list[1].timed = Some(10);
        list[2].timed = Some(5);
        let variant_of = add_variants(&mut list);
        assert_eq!(list.len(), 5);
        assert_eq!(variant_of, [None, Some(3), Some(4), None, None]);
        assert_eq!(list[3].temporary_of, Some(1));
        assert_eq!(list[4].temporary_of, Some(2));
        assert!(list[3].flags.contains(UFlags::TEMPORARY) && list[3].timed.is_none());
    }

    #[test]
    fn modifiers_fold_once_each() {
        use super::super::generated::p;
        let mut s = staged();
        assert_eq!(fold(&mut s, ModifierData::UnitActionConsumeUnit), Ok(()));
        assert_eq!(fold(&mut s, ModifierData::UnitActionConsumeUnit), Err("is there twice"));
        let times =
            |n| ModifierData::UnitActionLimitedTimes(p::UnitActionLimitedTimes { times: n });
        assert_eq!(fold(&mut s, times(3)), Ok(()));
        assert_eq!(s.actions.uses(), Some(3));
        assert_eq!(fold(&mut s, times(4)), Err("is there twice"));
        let mut s = staged();
        assert_eq!(fold(&mut s, times(70_000)), Err("counts more than 65535"));
        let timer =
            |n| ModifierData::ConditionalTimedUnique(p::ConditionalTimedUnique { turns: n });
        assert_eq!(fold(&mut s, timer(10)), Ok(()));
        assert!(s.flags.contains(UFlags::TIMED));
        assert_eq!(s.timed, Some(10));
        assert_eq!(fold(&mut s, ModifierData::ModifiedByGameSpeed), Ok(()));
        assert_eq!(fold(&mut s, ModifierData::ModifiedByGameSpeed), Err("is there twice"));
        assert!(s.flags.contains(UFlags::SPEED));
        let mut s = staged();
        assert_eq!(fold(&mut s, ModifierData::UnitActionOnce), Ok(()));
        assert_eq!(s.actions.uses(), Some(1), "<once> is one use");
    }
}
