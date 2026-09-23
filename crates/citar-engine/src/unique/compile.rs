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
//!    `for every` multiplier, which Python read as 1 (`uniques.py:1081-1083`), is an error;
//! 5. mark it `LOCAL` when a parameter or a conditional says `in this city` (`uniques.py:130`);
//! 6. key it (FNV-1a-64 of source kind, source name, occurrence and text, DESIGN.md 7.2);
//! 7. give each timed unique a variant without the timer, in the `Temporary` block;
//! 8. make each text a filter names a tag: an unknown text becomes a [`UniqueData::Tag`], a
//!    known one without parameters gives its source the tag;
//! 9. split each source's uniques by what the engine does with them ([`SourceUniques`]).

use super::generated::{CondData, ModifierData, TYPE_INFO, TriggerCond, UniqueData, UniqueType};
use super::params::{Lexicon, Param, ParamCx};
use super::table::{
    ActionMods, Cond, CondDeps, CondSpan, Role, Source, SourceUniques, StaticDomain, UFlags,
    Unique, UniqueMeta, UniqueTable, unique_key,
};
use super::text::{placeholder, split_modifiers};
use crate::base::collections::{DetMap, DetSet};
use crate::base::ids::{AbilityKey, CondId, Id, IdVec, PromotionId, TagId, TextId, UniqueId};
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
    /// The placeholder, when the unique has no parameters: the term a filter names it by.
    bare: Option<&'static str>,
    tag: Option<TagId>,
    /// Stands for the timed unique at this position in `staged` (step 7).
    temporary_of: Option<usize>,
}

/// Compiles every source's texts. `filters` are the filter texts the ruleset holds outside
/// uniques (improvement terrains, start biases), which may name tags too. Every problem is
/// pushed to `p`; `None` means there was at least one.
pub(crate) fn compile(
    rules: &Ruleset,
    sources: &[SourceTexts<'_>],
    filters: &[&str],
    p: &mut Problems,
) -> Option<Compiled> {
    let mut lx = Lexicon::new(rules);
    let mut conds: Vec<Cond> = Vec::new();
    let mut staged: Vec<Staged> = Vec::new();
    let mut failed = false;
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
                }
            }
        }
    }
    if failed {
        return None;
    }
    if let Err(r) = give_tags(&mut lx, &mut staged, filters) {
        for (i, message) in r {
            let src = &sources[staged[i].source];
            let text = lx.text_of(staged[i].text).to_owned();
            p.push(
                RulesetErrorKind::UnknownUnique,
                src.file,
                src.name,
                format!("{text:?}: {message}"),
            );
        }
        return None;
    }
    add_variants(&mut staged);
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
        let temp_variant = if s.temporary_of.is_none() && s.timed.is_some() {
            order.iter().find(|&&j| staged[j].temporary_of == Some(i)).map(|&j| id_of[j])
        } else {
            None
        };
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
    Some(Compiled { table, fracs, sources: parts })
}

/// Refuses more uniques than a [`UniqueId`] numbers.
fn check_count(n: usize, p: &mut Problems) -> Option<()> {
    if UniqueId::from_index(n.saturating_sub(1)).is_none() {
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
        staged.bare = Some(ty.placeholder());
    }
    if params.contains(&"in this city") {
        staged.flags |= UFlags::LOCAL;
    }

    // Step 4: the modifiers.
    let start = conds.len();
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
                let text = mcx.lx.text(m).map_err(|e| Refusal::new(E::Capacity, e))?;
                // Every conditional reads everything until package 1a-07 assigns its classes.
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
            }
            Role::ActionMod | Role::Meta => {
                let data = ModifierData::build(mty, &mut mcx).map_err(bad_param)?;
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
    let n = conds.len() - start;
    let (Ok(start), Ok(len)) = (u16::try_from(start), u16::try_from(n)) else {
        return Err(Refusal::new(E::Capacity, "more conditionals than a CondId (u16) numbers"));
    };
    if CondId::from_index(conds.len().saturating_sub(1)).is_none() {
        return Err(Refusal::new(E::Capacity, "more conditionals than a CondId (u16) numbers"));
    }
    staged.conds = CondSpan { start, len };
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
    // Filters come from the ruleset, but a pathological nesting must not overflow the stack.
    fn walk(text: &str, depth: u32, out: &mut Vec<String>) {
        if depth > 64 {
            out.push(text.to_owned());
            return;
        }
        if text.starts_with('{') && text.ends_with('}') && text.contains("} {") {
            for part in and_parts(text) {
                walk(&part, depth + 1, out);
            }
        } else if let Some(inner) = text.strip_prefix("non-[").and_then(|t| t.strip_suffix(']')) {
            walk(inner, depth + 1, out);
        } else {
            out.push(text.to_owned());
        }
    }
    walk(text, 0, out);
}

/// `{a} {b}` split at depth 0 (`uniques.py:308-327`).
fn and_parts(text: &str) -> Vec<String> {
    let inner = &text[1..text.len() - 1];
    let bytes = inner.as_bytes();
    let mut parts = Vec::new();
    let mut depth: i64 = 0;
    let mut cur = 0;
    let mut i = 0;
    while i < bytes.len() {
        if depth == 0 && bytes[i..].starts_with(b"} {") {
            parts.push(inner[cur..i].to_owned());
            i += 3;
            cur = i;
            continue;
        }
        match bytes[i] {
            b'[' | b'{' => depth += 1,
            b']' | b'}' => depth -= 1,
            _ => {}
        }
        i += 1;
    }
    parts.push(inner[cur..].to_owned());
    parts
}

/// Step 8. `Err` lists the texts that are unknown and that no filter names.
fn give_tags(
    lx: &mut Lexicon<'_>,
    staged: &mut [Staged],
    filters: &[&str],
) -> Result<(), Vec<(usize, String)>> {
    let mut terms: DetSet<String> = DetSet::default();
    let mut buf = Vec::new();
    let texts: Vec<String> =
        lx.filter_texts().into_iter().map(|t| lx.text_of(t).to_owned()).collect();
    for f in texts.iter().map(String::as_str).chain(filters.iter().copied()) {
        buf.clear();
        filter_terms(f, &mut buf);
        terms.extend(buf.drain(..));
    }
    let mut bad = Vec::new();
    for (i, s) in staged.iter_mut().enumerate() {
        let name = match (s.data, s.bare) {
            (None, _) => lx.text_of(s.text).to_owned(),
            (Some(_), Some(bare)) => bare.to_owned(),
            (Some(_), None) => continue,
        };
        if !terms.contains(&name) {
            if s.data.is_none() {
                bad.push((
                    i,
                    format!(
                        "{} and no filter names it as a tag, so it is probably a typo",
                        unknown(&name)
                    ),
                ));
            }
            continue;
        }
        match lx.tag(&name) {
            Ok(t) if t.index() < TagSet::CAPACITY => {
                s.tag = Some(t);
                if s.data.is_none() {
                    s.data = Some(UniqueData::Tag(t));
                }
            }
            _ => bad.push((
                i,
                format!(
                    "more tags than a TagSet holds ({}); widen sets::TAG_WORDS",
                    TagSet::CAPACITY
                ),
            )),
        }
    }
    if bad.is_empty() { Ok(()) } else { Err(bad) }
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

/// Adds a variant without the timer for each timed unique (step 7), after the others.
fn add_variants(staged: &mut Vec<Staged>) {
    let timed: Vec<usize> = (0..staged.len()).filter(|&i| staged[i].timed.is_some()).collect();
    for i in timed {
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
            bare: o.bare,
            tag: o.tag,
            temporary_of: Some(i),
        });
    }
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
                        if f.contains(UFlags::LOCAL) {
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
