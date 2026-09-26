"""Diplomacy: war & peace, embassies, declarations of friendship, denunciations, defensive pacts, research
agreements, messages, and free-text negotiations with binding deals.

War/peace side effects and agreement rules follow UnCiv's DeclareWar, DiplomacyManager, DiplomacyFunctions and
TradeLogic (MPL-2.0). CITAR adds message-based negotiation with counter-offers on top.
"""
from __future__ import annotations

from typing import Optional, TYPE_CHECKING

from . import unique_types as U
from .game import ActionError

if TYPE_CHECKING:
    from .game import Game

ITEM_TYPES = {
    "gold": "Lump sum of gold: {\"type\": \"gold\", \"amount\": 100}",
    "gold_per_turn": "Gold every turn: {\"type\": \"gold_per_turn\", \"amount\": 5, \"turns\": 30}",
    "resource": "Strategic or luxury resource per turn: {\"type\": \"resource\", \"resource\": \"Iron\", \"amount\": 1, \"turns\": 30}",
    "open_borders": "Let the other side's units enter your territory: {\"type\": \"open_borders\", \"turns\": 30}",
    "embassy": "Let the other side establish an embassy in your capital: {\"type\": \"embassy\"}",
    "peace_treaty": "End the war between you: {\"type\": \"peace_treaty\"}",
    "declaration_of_friendship": "Declare friendship (both sides; 30 turns): {\"type\": \"declaration_of_friendship\"}",
    "research_agreement": "Research agreement (both sides pay; needs friendship): {\"type\": \"research_agreement\"}",
    "defensive_pact": "Defensive pact (both sides; needs friendship): {\"type\": \"defensive_pact\"}",
    "declare_war": "The giver declares war on a third civ: {\"type\": \"declare_war\", \"target\": 2}",
    "city": "Give one of your cities (not the capital): {\"type\": \"city\", \"city_id\": 12}",
    "share_map": "Give your explored map: {\"type\": \"share_map\"}",
    "tech": "Give a technology (if tech trading is on): {\"type\": \"tech\", \"tech\": \"Bronze Working\"}",
}
MUTUAL = {"peace_treaty", "declaration_of_friendship", "research_agreement", "defensive_pact"}

# The kinds of diplomatic decision a seat can hand to a language model while a bot plays the rest (hybrid seats). A
# deal item belongs to exactly one; un and captured_cities are the engine's automatic decisions (Player.auto), and
# denounce and chat have no bot logic behind them.
CATEGORIES = ("trades", "agreements", "peace", "war", "denounce", "un", "city_states", "espionage", "captured_cities",
              "chat")
ITEM_CATEGORY = {"gold": "trades", "gold_per_turn": "trades", "resource": "trades", "tech": "trades",
                 "share_map": "trades", "city": "trades", "embassy": "agreements", "open_borders": "agreements",
                 "declaration_of_friendship": "agreements", "research_agreement": "agreements",
                 "defensive_pact": "agreements", "peace_treaty": "peace", "declare_war": "war"}

# What respond_negotiation accepts. The aliases are words people and models reach for; the history records the
# canonical name, so everything that reads it has four actions to handle rather than seven.
RESPONSE_ACTIONS = ("accept", "counter", "reject", "reply")
ACTION_ALIASES = {"decline": "reject", "withdraw": "reject", "end": "reject"}


def item_category(item: dict) -> str:
    """The diplomacy category a deal item belongs to."""
    return ITEM_CATEGORY[item["type"]]


def proposal_categories(proposal: Optional[dict]) -> set:
    """Every category the items of a proposal touch, on both sides."""
    return {ITEM_CATEGORY[it["type"]] for items in (proposal or {}).values() for it in items}


def new_relation() -> dict:
    """A fresh relationship record between two civilizations."""
    return {"war": False, "since": 0, "treaty_until": 0, "embassy": {}, "friendship_until": 0, "pact_until": 0,
            "ra_until": 0, "ra_science": {}, "denounced_by": {}, "opinion": {}, "war_declared_by": None}


def relation(g: "Game", a: int, b: int) -> dict:
    """The relationship between two civilizations, creating it if they have just met."""
    rel = g.s.relations.get(g.rel_key(a, b))
    if rel is None:
        rel = new_relation()
        g.s.relations[g.rel_key(a, b)] = rel
    for k, v in new_relation().items():
        rel.setdefault(k, v)
    return rel


def add_opinion(g: "Game", holder: int, about: int, key: str, amount: float):
    """Remembered opinion modifiers (used by scripted bots; humans and LLMs decide for themselves)."""
    if holder == about or holder is None:
        return
    op = relation(g, holder, about)["opinion"].setdefault(str(holder), {})
    op[key] = max(-100.0, min(100.0, op.get(key, 0.0) + amount))


def opinion(g: "Game", holder: int, about: int) -> float:
    """How one civilization feels about another, as a single number.

    Accumulated from everything that has happened between them - wars, broken promises, shared
    enemies, denunciations - and what an AI consults when deciding whether to accept a deal.
    """
    rel = g.relation(holder, about)
    if not rel:
        return 0.0
    return sum(rel.get("opinion", {}).get(str(holder), {}).values())


def has_embassy(g: "Game", holder: int, host: int) -> bool:
    """`holder` has an embassy in `host`'s capital."""
    rel = g.relation(holder, host)
    return bool(rel and rel.get("embassy", {}).get(f"{holder}>{host}"))


def shared_embassies(g: "Game", a: int, b: int) -> bool:
    """Whether both sides have embassies with each other."""
    return has_embassy(g, a, b) and has_embassy(g, b, a)


def meets_embassy_requirement(g: "Game", a: int, b: int) -> bool:
    """Whether diplomacy is possible, for civilizations that require embassies first."""
    return not g.civ_has(a, U.RequiresEmbassiesForDiplomacy) or shared_embassies(g, a, b)


def is_friends(g: "Game", a: int, b: int) -> bool:
    """Whether a declaration of friendship is in force."""
    rel = g.relation(a, b)
    return bool(rel and rel.get("friendship_until", 0) >= g.turn)


def has_pact(g: "Game", a: int, b: int) -> bool:
    """Whether a defensive pact is in force."""
    rel = g.relation(a, b)
    return bool(rel and rel.get("pact_until", 0) >= g.turn and not rel.get("war"))


def denounced(g: "Game", by: int, target: int) -> bool:
    """Whether one civilization has denounced another."""
    rel = g.relation(by, target)
    return bool(rel and rel.get("denounced_by", {}).get(str(by), 0) >= g.turn)


# ----------------------------------------------------------------------------
# War & peace
# ----------------------------------------------------------------------------
def can_declare_war(g: "Game", pid: int, target: int) -> Optional[str]:
    """Why war cannot be declared, or None."""
    tp = g.player(target)
    if target == pid or tp.kind == "barbarian" or not tp.alive:
        return "Invalid target."
    if not g.has_met(pid, target):
        return f"You have not met {tp.name}."
    rel = relation(g, pid, target)
    if rel["war"]:
        return f"You are already at war with {tp.name}."
    if rel["treaty_until"] >= g.turn:
        return f"Your peace treaty with {tp.name} lasts until turn {rel['treaty_until']}."
    return None


def declare_war(g: "Game", pid: int, target: int, message: Optional[str] = None, via_deal: bool = False,
                via_nuke: bool = False) -> dict:
    """Declare war, with everything that follows: broken deals, allies, and the diplomatic cost."""
    try:
        target = int(target)
    except (TypeError, ValueError):
        raise ActionError("player_id must be a number.")
    if target < 0 or target >= len(g.s.players):
        raise ActionError("Invalid target.")
    reason = can_declare_war(g, pid, target)
    if reason:
        raise ActionError(reason)
    p, tp = g.player(pid), g.player(target)
    set_war(g, pid, target, reason="direct")
    text = f"{p.name} declared war on {tp.name}!"
    if message:
        text += f' "{message[:300]}"'
        add_message(g, pid, [target], message)
    g.emit("war_declared", text, None, attacker=pid, defender=target)
    return {"war_declared_on": tp.name}


def set_war(g: "Game", a: int, b: int, reason: str = "direct"):
    """a declares war on b with all side effects (DeclareWar.declareWar)."""
    from . import city_states, triggers
    from .uniques import civ_matches
    rel = relation(g, a, b)
    if rel["war"]:
        return
    pa, pb = g.player(a), g.player(b)
    if pb.kind == "city_state" and reason == "direct":
        city_states.set_influence(g, b, a, -60)
        city_states.on_attacked(g, b, a)
        if pb.ally == a:
            city_states.set_influence(g, b, a, -120)
    rel["war"] = True
    rel["since"] = g.turn
    rel["war_declared_by"] = a
    for d in g.s.deals:
        if d.get("active") and set(d["parties"]) == {a, b}:
            d["active"] = False
    g.s.open_borders.pop(f"{a}>{b}", None)
    g.s.open_borders.pop(f"{b}>{a}", None)
    betrayed_f = rel.get("friendship_until", 0) >= g.turn
    betrayed_p = rel.get("pact_until", 0) >= g.turn
    rel["friendship_until"] = 0
    rel["pact_until"] = 0
    if betrayed_f or betrayed_p:
        for q in g.majors():
            if q.id != a and g.has_met(q.id, a):
                amt = (-40 if q.id == b else -20) if betrayed_f else 0
                amt += (-20 if q.id == b else -10) if betrayed_p else 0
                add_opinion(g, q.id, a, "betrayal", amt)
    if reason == "direct" and pa.kind == "major":
        # the aggressor's other defensive pacts lapse
        for q in g.majors():
            if q.id not in (a, b):
                r2 = g.relation(a, q.id)
                if r2 and r2.get("pact_until", 0) >= g.turn:
                    r2["pact_until"] = 0
        for q in g.majors():
            if q.id not in (a, b) and g.has_met(q.id, a):
                add_opinion(g, q.id, a, "warmonger", -5)
    for n in list(g.s.negotiations):
        if n["status"] == "open" and {n["initiator"], n["responder"]} == {a, b}:
            close_negotiation(g, n["id"], "cancelled", f"{pa.name} declared war on {pb.name}.")
    for side, other in ((a, b), (b, a)):
        if g.player(side).kind != "major":
            continue
        if side == b and reason not in ("defensive_pact",) and g.player(a).kind == "major":
            for q in g.majors():
                if q.id not in (a, b) and has_pact(g, b, q.id) and not g.at_war(q.id, a):
                    if not g.has_met(a, q.id):
                        g.meet(a, q.id)
                    set_war(g, a, q.id, reason="defensive_pact")
                    g.emit("war_declared", f"{g.player(q.id).name} joins the war against {pa.name} (defensive pact).",
                           None, attacker=a, defender=q.id)
        for cs in g.city_states():
            if cs.ally == side and not g.at_war(cs.id, other) and cs.id != other:
                if not g.has_met(cs.id, other):
                    g.meet(cs.id, other)
                set_war(g, cs.id, other, reason="city-state alliance")
    if pb.kind == "city_state" and a in pb.protectors:
        city_states.withdraw_protection(g, a, b, forced=True)
    g.invalidate()
    triggers.fire(g, a, U.TriggerUponDeclaringWarFiltered, filt=lambda m: civ_matches(g, b, m.p(0), a))
    triggers.fire(g, b, U.TriggerUponBeingDeclaredWarUpon, filt=lambda m: civ_matches(g, a, m.p(0), b))
    triggers.fire(g, a, U.TriggerUponEnteringWar, filt=lambda m: civ_matches(g, b, m.p(0), a))
    triggers.fire(g, b, U.TriggerUponEnteringWar, filt=lambda m: civ_matches(g, a, m.p(0), b))


def make_peace(g: "Game", a: int, b: int):
    """End a war and set the treaty period during which it cannot resume."""
    from . import city_states, triggers
    from .movement import teleport_to_closest
    from .uniques import civ_matches
    rel = relation(g, a, b)
    rel["war"] = False
    rel["since"] = g.turn
    rel["treaty_until"] = g.turn + g.speed["peaceDealDuration"]
    for side, other in ((a, b), (b, a)):
        for u in list(g.player_units(side)):
            if g.s.tiles[u.idx].owner == other:
                teleport_to_closest(g, u)
        for cs in g.city_states():
            if cs.ally == side and g.at_war(cs.id, other):
                r2 = relation(g, cs.id, other)
                r2["war"] = False
                r2["treaty_until"] = g.turn + g.speed["peaceDealDuration"]
            elif cs.ally != side and g.at_war(cs.id, other) and g.player(side).kind == "major":
                city_states.add_influence(g, cs.id, side, -10)
    g.invalidate()
    g.emit("peace", f"{g.player(a).name} and {g.player(b).name} signed a peace treaty (until turn {rel['treaty_until']}).",
           None, a=a, b=b)
    triggers.fire(g, a, U.TriggerUponSigningPeace, filt=lambda m: civ_matches(g, b, m.p(0), a))
    triggers.fire(g, b, U.TriggerUponSigningPeace, filt=lambda m: civ_matches(g, a, m.p(0), b))


def make_peace_with_city_state(g: "Game", pid: int, cs: int) -> dict:
    """UnCiv lets majors make peace with city-states directly (no deal needed)."""
    if g.player(cs).kind != "city_state":
        raise ActionError("That is not a city-state.")
    if not g.at_war(pid, cs):
        raise ActionError("You are not at war with them.")
    al = g.player(cs).ally
    if al is not None and g.at_war(pid, al):
        raise ActionError(f"{g.player(cs).name} is allied with {g.player(al).name}, who is at war with you.")
    rel = relation(g, pid, cs)
    if rel["since"] + g.rules.k["minimum_war_duration"] > g.turn:
        raise ActionError(f"Wars last at least {g.rules.k['minimum_war_duration']} turns.")
    make_peace(g, pid, cs)
    return {"peace": g.player(cs).name}


def denounce(g: "Game", pid: int, target: int) -> dict:
    """Publicly denounce a civilization, ending friendship and affecting how others see both."""
    tp = g.player(int(target))
    if tp.kind != "major" or target == pid or not g.has_met(pid, target):
        raise ActionError("You can only denounce civilizations you have met.")
    rel = relation(g, pid, target)
    if rel["war"]:
        raise ActionError("You are at war with them already.")
    if denounced(g, pid, target):
        raise ActionError(f"You have already denounced {tp.name}.")
    rel["denounced_by"][str(pid)] = g.turn + 30
    rel["friendship_until"] = 0
    add_opinion(g, target, pid, "denounced", -35)
    for q in g.majors():
        if q.id not in (pid, target) and is_friends(g, q.id, target):
            add_opinion(g, q.id, pid, "denounced_friend", -15)
    g.emit("denounce", f"{g.player(pid).name} denounced {tp.name}!", None, a=pid, b=target)
    return {"denounced": tp.name}


# ----------------------------------------------------------------------------
# Messages
# ----------------------------------------------------------------------------
def add_message(g: "Game", sender: int, recipients: list[int], text: str) -> dict:
    """Record a message between civilizations."""
    msg = {"id": len(g.s.messages) + 1, "turn": g.turn, "from": sender, "to": sorted(set(recipients)), "text": text[:4000]}
    g.s.messages.append(msg)
    return msg


def send_message(g: "Game", pid: int, to, text: str) -> dict:
    """Send free text to one civilization or to everyone met. Nothing said is binding."""
    if not text or not str(text).strip():
        raise ActionError("Message text is empty.")
    if to == "all" or to is None:
        recipients = [q for q in g.player(pid).met if g.player(q).alive and g.player(q).kind == "major"]
        if not recipients:
            raise ActionError("You have not met any other civilization yet.")
    else:
        ids = to if isinstance(to, list) else [to]
        recipients = []
        for q in ids:
            try:
                q = int(q)
            except (TypeError, ValueError):
                raise ActionError(f"Invalid recipient '{q}'. Use player ids (numbers) or 'all'.")
            if q < 0 or q >= len(g.s.players) or g.player(q).kind != "major" or q == pid:
                raise ActionError(f"Invalid recipient {q} (messages go to major civilizations).")
            if not g.has_met(pid, q):
                raise ActionError(f"You have not met {g.player(q).name} yet.")
            recipients.append(q)
    msg = add_message(g, pid, recipients, str(text))
    names = ", ".join(g.player(q).name for q in recipients)
    g.emit("message", f"{g.player(pid).name} → {names}: {msg['text']}", [pid] + recipients, message=msg["id"], sender=pid)
    return {"sent_to": names, "message_id": msg["id"]}


# ----------------------------------------------------------------------------
# Deal items
# ----------------------------------------------------------------------------
def _normalize_items(g: "Game", items) -> list[dict]:
    """Validate and normalise the items in a deal."""
    if items is None:
        return []
    if not isinstance(items, list):
        raise ActionError("Deal items must be a list of objects like {\"type\": \"gold\", \"amount\": 50}.")
    out = []
    dd = g.speed["dealDuration"]
    for it in items:
        if not isinstance(it, dict) or it.get("type") not in ITEM_TYPES:
            raise ActionError(f"Invalid deal item {it!r}. Valid types: {', '.join(ITEM_TYPES)}.")
        it = dict(it)
        t = it["type"]
        try:
            if t in ("gold", "gold_per_turn"):
                it["amount"] = int(it.get("amount", 0))
                if it["amount"] <= 0:
                    raise ActionError("Gold amounts must be positive.")
                if t == "gold" and it["amount"] > g.rules.k["max_gold_trade_offer"]:
                    raise ActionError(f"At most {g.rules.k['max_gold_trade_offer']} gold per deal.")
            if t in ("gold_per_turn", "resource", "open_borders"):
                it["turns"] = max(1, min(100, int(it.get("turns", dd))))
            if t == "resource":
                it["amount"] = max(1, int(it.get("amount", 1)))
                r = g.rules.resolve("resource", it.get("resource"))
                if r is None or g.rules.resources[r]["resourceType"] == "Bonus":
                    raise ActionError(f"'{it.get('resource')}' is not a tradeable strategic or luxury resource.")
                it["resource"] = r
            if t == "declare_war":
                it["target"] = int(it.get("target"))
            if t == "city":
                it["city_id"] = int(it.get("city_id"))
            if t == "tech":
                tn = g.rules.resolve("tech", it.get("tech"))
                if tn is None:
                    raise ActionError(f"Unknown tech '{it.get('tech')}'.")
                it["tech"] = tn
        except (TypeError, ValueError):
            raise ActionError(f"Malformed deal item {it!r}.")
        out.append(it)
    return out


def _has(proposal: dict, t: str) -> bool:
    """Whether a proposal contains an item of a type."""
    return any(it["type"] == t for items in proposal.values() for it in items)


def ra_cost(g: "Game", a: int, b: int) -> int:
    """The gold both sides pay for a research agreement, which rises with era."""
    from .research import player_era
    R = g.rules
    ea = R.eras[R.era_list[player_era(g, a)]]["researchAgreementCost"]
    eb = R.eras[R.era_list[player_era(g, b)]]["researchAgreementCost"]
    return int(max(ea, eb) * g.speed["goldCostModifier"])


def validate_items(g: "Game", giver: int, receiver: int, items: list[dict], proposal: dict):
    """Check that every item in a deal can actually be given, or raise saying which cannot.

    Checked at proposal time rather than at acceptance, so that an impossible deal is refused to the
    person making it rather than to the person accepting it.
    """
    from . import economy, research
    gp, rp = g.player(giver), g.player(receiver)
    at_war = g.at_war(giver, receiver)
    total_gold = 0
    for it in items:
        t = it["type"]
        if t == "gold":
            total_gold += it["amount"]
        elif t == "gold_per_turn":
            net = economy.civ_stats(g, giver)["gold"]
            if net < it["amount"]:
                raise ActionError(f"{gp.name} only makes {int(net)} gold per turn; cannot pay {it['amount']} per turn.")
        elif t == "resource":
            have = g.resource_amount(giver, it["resource"])
            if have < it["amount"]:
                raise ActionError(f"{gp.name} does not have {it['amount']} spare {it['resource']} (has {have}).")
        elif t == "open_borders":
            if at_war and not _has(proposal, "peace_treaty"):
                raise ActionError("Open borders cannot be exchanged while at war (without a peace treaty).")
            if not g.civ_has(giver, U.EnablesOpenBorders) or not g.civ_has(receiver, U.EnablesOpenBorders):
                raise ActionError("Open borders need the right technology (Writing line) on both sides.")
            if not meets_embassy_requirement(g, giver, receiver):
                raise ActionError("Open borders require embassies in each other's capitals first.")
        elif t == "embassy":
            if not g.civ_has(giver, U.EnablesEmbassies) or not g.civ_has(receiver, U.EnablesEmbassies):
                raise ActionError("Embassies need Writing on both sides.")
            if has_embassy(g, receiver, giver):
                raise ActionError(f"{rp.name} already has an embassy with {gp.name}.")
            if g.player(giver).capital is None:
                raise ActionError(f"{gp.name} has no capital.")
        elif t == "peace_treaty":
            if not at_war:
                raise ActionError(f"{gp.name} and {rp.name} are not at war.")
        elif t == "declaration_of_friendship":
            if at_war or denounced(g, giver, receiver) or denounced(g, receiver, giver):
                raise ActionError("Friendship is impossible while at war or after a denunciation.")
            if is_friends(g, giver, receiver):
                raise ActionError("You are already friends.")
        elif t == "research_agreement":
            if not g.civ_has(giver, U.EnablesResearchAgreements) or not g.civ_has(receiver, U.EnablesResearchAgreements):
                raise ActionError("Research agreements need Education on both sides.")
            if not (is_friends(g, giver, receiver) or _has(proposal, "declaration_of_friendship")):
                raise ActionError("Research agreements require a declaration of friendship.")
            if not meets_embassy_requirement(g, giver, receiver):
                raise ActionError("Research agreements require embassies in each other's capitals.")
            rel = relation(g, giver, receiver)
            if rel.get("ra_until", 0) >= g.turn:
                raise ActionError("You already have a research agreement.")
            cost = ra_cost(g, giver, receiver)
            if gp.gold < cost:
                raise ActionError(f"A research agreement costs each side {cost} gold; {gp.name} has {int(gp.gold)}.")
            if research.all_researched(g, giver):
                raise ActionError(f"{gp.name} has nothing left to research.")
        elif t == "defensive_pact":
            if not g.civ_has(giver, U.EnablesDefensivePacts) or not g.civ_has(receiver, U.EnablesDefensivePacts):
                raise ActionError("Defensive pacts need Chivalry on both sides.")
            if not (is_friends(g, giver, receiver) or _has(proposal, "declaration_of_friendship")):
                raise ActionError("Defensive pacts require a declaration of friendship.")
            if not meets_embassy_requirement(g, giver, receiver):
                raise ActionError("Defensive pacts require embassies in each other's capitals.")
            if has_pact(g, giver, receiver):
                raise ActionError("You already have a defensive pact.")
        elif t == "declare_war":
            tgt = it["target"]
            if tgt in (giver, receiver) or tgt < 0 or tgt >= len(g.s.players) or g.player(tgt).kind == "barbarian" \
                    or not g.player(tgt).alive:
                raise ActionError("Invalid war target.")
            reason = can_declare_war(g, giver, tgt)
            if reason:
                raise ActionError(f"{gp.name}: {reason}")
        elif t == "city":
            c = g.city(it["city_id"])
            if c is None or c.owner != giver:
                raise ActionError(f"{gp.name} does not own city {it['city_id']}.")
            if gp.capital == c.id:
                raise ActionError("Capitals cannot be traded.")
            if c.resistance > 0:
                raise ActionError(f"{c.name} is in resistance.")
        elif t == "tech":
            if not g.s.config.get("tech_trading", True):
                raise ActionError("Tech trading is disabled in this game.")
            if not g.has_tech(giver, it["tech"]):
                raise ActionError(f"{gp.name} does not know {it['tech']}.")
            if not research.can_research(g, receiver, it["tech"]):
                raise ActionError(f"{rp.name} cannot receive {it['tech']} (already known or missing prerequisites).")
    if total_gold > gp.gold:
        raise ActionError(f"{gp.name} only has {int(gp.gold)} gold.")


def describe_items(g: "Game", items: list[dict]) -> str:
    """A deal's items as a readable sentence, for messages and the log."""
    parts = []
    for it in items:
        t = it["type"]
        if t == "gold":
            parts.append(f"{it['amount']} gold")
        elif t == "gold_per_turn":
            parts.append(f"{it['amount']} gold/turn for {it['turns']} turns")
        elif t == "resource":
            parts.append(f"{it['amount']} {it['resource']} for {it['turns']} turns")
        elif t == "open_borders":
            parts.append(f"open borders for {it['turns']} turns")
        elif t == "embassy":
            parts.append("an embassy")
        elif t == "peace_treaty":
            parts.append("a peace treaty")
        elif t == "declaration_of_friendship":
            parts.append("a declaration of friendship")
        elif t == "research_agreement":
            parts.append("a research agreement")
        elif t == "defensive_pact":
            parts.append("a defensive pact")
        elif t == "declare_war":
            parts.append(f"a declaration of war on {g.player(it['target']).name}")
        elif t == "city":
            c = g.city(it["city_id"])
            parts.append(f"the city of {c.name if c else it['city_id']}")
        elif t == "share_map":
            parts.append("their world map")
        elif t == "tech":
            parts.append(f"the technology {it['tech']}")
    return ", ".join(parts) if parts else "nothing"


def execute_deal(g: "Game", a: int, b: int, proposal: dict) -> dict:
    """Carry out an accepted deal: transfer everything, and record what recurs."""
    from . import research, visibility, triggers
    from .conquest import move_to_civ
    items_a = proposal.get(str(a), [])
    items_b = proposal.get(str(b), [])
    validate_items(g, a, b, items_a, proposal)
    validate_items(g, b, a, items_b, proposal)
    if g.at_war(a, b) and not _has(proposal, "peace_treaty"):
        raise ActionError("While at war, a deal must include a peace treaty.")
    deal = {"id": len(g.s.deals) + 1, "turn": g.turn, "parties": [a, b], "terms": {str(a): items_a, str(b): items_b},
            "ongoing": [], "active": True}
    done_mutual = set()
    if _has(proposal, "peace_treaty"):
        make_peace(g, a, b)
        done_mutual.add("peace_treaty")
    rel = relation(g, a, b)
    for giver, receiver, items in ((a, b, items_a), (b, a, items_b)):
        gp, rp = g.player(giver), g.player(receiver)
        for it in items:
            t = it["type"]
            if t == "gold":
                gp.gold -= it["amount"]
                rp.gold += it["amount"]
            elif t in ("gold_per_turn", "resource"):
                entry = dict(it)
                entry.update({"from": giver, "to": receiver, "until": g.turn + it["turns"]})
                deal["ongoing"].append(entry)
            elif t == "open_borders":
                g.s.open_borders[f"{giver}>{receiver}"] = g.turn + it["turns"]
            elif t == "embassy":
                rel["embassy"][f"{receiver}>{giver}"] = True
                cap = g.city(gp.capital)
                if cap:
                    visibility.reveal_tiles(g, receiver, g.grid.within(cap.idx, 2))
            elif t == "share_map":
                visibility.reveal_tiles(g, receiver, [i for i in range(g.grid.size) if gp.explored[i]])
            elif t == "tech":
                research.add_tech(g, receiver, it["tech"], source="trade")
            elif t == "city":
                c = g.city(it["city_id"])
                move_to_civ(g, c, receiver)
                from .movement import teleport_to_closest
                for u in list(g.units_at(c.idx)):
                    if u.owner != receiver:
                        teleport_to_closest(g, u)
            elif t in MUTUAL and t not in done_mutual:
                done_mutual.add(t)
                if t == "declaration_of_friendship":
                    rel["friendship_until"] = g.turn + 30
                    add_opinion(g, a, b, "friendship", 35)
                    add_opinion(g, b, a, "friendship", 35)
                    g.emit("friendship", f"{g.player(a).name} and {g.player(b).name} signed a Declaration of Friendship!",
                           None, a=a, b=b)
                    triggers.fire(g, a, U.TriggerUponDeclaringFriendship)
                    triggers.fire(g, b, U.TriggerUponDeclaringFriendship)
                elif t == "research_agreement":
                    cost = ra_cost(g, a, b)
                    g.player(a).gold -= cost
                    g.player(b).gold -= cost
                    rel["ra_until"] = g.turn + g.speed["dealDuration"]
                    rel["ra_science"] = {str(a): 0, str(b): 0}
                elif t == "defensive_pact":
                    rel["pact_until"] = g.turn + g.speed["dealDuration"]
                    g.emit("pact", f"{g.player(a).name} and {g.player(b).name} signed a Defensive Pact!", None, a=a, b=b)
                    triggers.fire(g, a, U.TriggerUponSigningDefensivePact)
                    triggers.fire(g, b, U.TriggerUponSigningDefensivePact)
    for giver, receiver, items in ((a, b, items_a), (b, a, items_b)):
        for it in items:
            if it["type"] == "declare_war" and can_declare_war(g, giver, it["target"]) is None:
                set_war(g, giver, it["target"], reason="join")
                g.emit("war_declared", f"{g.player(giver).name} declared war on {g.player(it['target']).name} "
                                       f"(as agreed with {g.player(receiver).name})!", None)
    g.s.deals.append(deal)
    g.invalidate()
    summary = (f"{g.player(a).name} gives {describe_items(g, items_a)}; "
               f"{g.player(b).name} gives {describe_items(g, items_b)}.")
    deal["summary"] = summary
    g.emit("deal", f"Deal concluded between {g.player(a).name} and {g.player(b).name}: {summary}", [a, b], deal=deal["id"])
    return deal


def deal_resource_flows(g: "Game", pid: int) -> list[tuple[str, int]]:
    """Resources flowing in and out of this civilization under active deals."""
    out = []
    for d in g.s.deals:
        if not d.get("active"):
            continue
        for it in d.get("ongoing", []):
            if it["type"] != "resource" or it["until"] < g.turn:
                continue
            if it["to"] == pid:
                out.append((it["resource"], it.get("amount", 1)))
            elif it["from"] == pid:
                out.append((it["resource"], -it.get("amount", 1)))
    return out


def deal_gold_per_turn(g: "Game", pid: int) -> float:
    """Net gold per turn from active deals."""
    total = 0.0
    for d in g.s.deals:
        if not d.get("active"):
            continue
        for it in d.get("ongoing", []):
            if it["type"] != "gold_per_turn" or it["until"] < g.turn:
                continue
            if it["to"] == pid:
                total += it["amount"]
            elif it["from"] == pid:
                total -= it["amount"]
    return total


def process_round(g: "Game"):
    """End of round: deal and agreement expiry, research agreement payouts, city-state diplomacy."""
    from .economy import civ_stats, resource_amount
    for d in g.s.deals:
        if not d.get("active"):
            continue
        # resource trades are cut short if the giver can no longer supply
        for it in d.get("ongoing", []):
            if it["type"] == "resource" and it["until"] >= g.turn and resource_amount(g, it["from"], it["resource"]) < 0:
                it["until"] = g.turn - 1
                g.emit("deal_cut", f"A trade of {it['resource']} between {g.player(it['from']).name} and "
                                   f"{g.player(it['to']).name} was cut short.", [it["from"], it["to"]])
        if d["ongoing"] and all(it["until"] < g.turn for it in d["ongoing"]):
            d["active"] = False
            a, b = d["parties"]
            g.emit("deal_expired", f"A deal between {g.player(a).name} and {g.player(b).name} has expired.", [a, b], deal=d["id"])
        elif not d["ongoing"]:
            d["active"] = False
    for key, until in list(g.s.open_borders.items()):
        if until < g.turn:
            del g.s.open_borders[key]
    for key, rel in g.s.relations.items():
        a, b = (int(x) for x in key.split(","))
        if rel.get("ra_until", 0) >= g.turn:
            for pid in (a, b):
                rel["ra_science"][str(pid)] = rel["ra_science"].get(str(pid), 0) + int(civ_stats(g, pid)["science"])
        elif rel.get("ra_until", 0) == g.turn - 1 and rel.get("ra_science"):
            bonus = min(rel["ra_science"].get(str(a), 0), rel["ra_science"].get(str(b), 0))
            for pid in (a, b):
                g.player(pid).flags["ra_science"] = g.player(pid).flags.get("ra_science", 0) + bonus
            rel["ra_science"] = {}
            g.emit("research_agreement", f"The research agreement between {g.player(a).name} and {g.player(b).name} "
                                         f"has concluded.", [a, b])
    g.invalidate()


def on_city_captured(g: "Game", attacker: int, old_owner: int, city):
    """Apply the diplomatic consequences of taking a city, weighted by how much of the victim it was."""
    total = sum(c.pop for c in g.player_cities(old_owner)) or 1
    aggro = 10 + round(city.pop * 100 / total)
    add_opinion(g, old_owner, attacker, "captured_our_cities", -aggro)
    for q in g.majors():
        if q.id in (attacker, old_owner) or not g.has_met(q.id, attacker):
            continue
        if g.at_war(q.id, old_owner):
            add_opinion(g, q.id, attacker, "shared_enemy", round(aggro / 10))
        else:
            add_opinion(g, q.id, attacker, "warmonger", -round(aggro / 10))


# ----------------------------------------------------------------------------
# Negotiations
# ----------------------------------------------------------------------------
# A negotiation is a chat between two major civilizations with a deal on the table. Entries alternate between the
# two sides, each is {seq, by, action, message, proposal, turn} (plus "note" where the game adds one), and every one
# carries a message. Neither side may end its turn while a negotiation it is in is open (end_turn_refusal), which is
# what gives a slow answer time to arrive; the server times chats out with close_negotiation.
CLOSED_STATUSES = ("rejected", "expired", "cancelled")


def get_negotiation(g: "Game", nid: int) -> dict:
    """A negotiation by id."""
    for n in g.s.negotiations:
        if n["id"] == nid:
            return n
    raise ActionError(f"No negotiation with id {nid}.")


def max_chat_messages(g: "Game") -> int:
    """How many messages a negotiation may hold before it closes: the game's own setting, else the ruleset's."""
    own = g.s.config.get("diplomacy")
    if isinstance(own, dict) and own.get("max_chat_messages"):
        return max(2, int(own["max_chat_messages"]))
    return g.rules.const["diplomacy"]["max_chat_messages"]


def _add_entry(g: "Game", n: dict, by: Optional[int], action: str, message: str = "", proposal: Optional[dict] = None,
               note: Optional[str] = None) -> dict:
    """Append a numbered entry to a negotiation's history."""
    entry = {"seq": len(n["history"]) + 1, "by": by, "action": action, "message": message, "proposal": proposal,
             "turn": g.turn}
    if note:
        entry["note"] = note
    n["history"].append(entry)
    n["exchanges"] = len(n["history"])      # the archived frozen bots still read this
    return entry


def _make_proposal(g: "Game", speaker: int, other: int, give, receive) -> Optional[dict]:
    """Build a proposal from what each side would give, or None if there is nothing concrete.

    Empty lists on both sides are nothing concrete too (models often send them with a plain message): a proposal in
    which neither side gives anything could be "accepted" into a deal of nothing.
    """
    if give is None and receive is None:
        return None
    prop = {str(speaker): _normalize_items(g, give), str(other): _normalize_items(g, receive)}
    if not prop[str(speaker)] and not prop[str(other)]:
        return None
    # mutual agreements go on both sides
    for t in MUTUAL:
        if _has(prop, t):
            for side in prop:
                if not any(it["type"] == t for it in prop[side]):
                    prop[side].append({"type": t})
    return prop


def open_negotiation(g: "Game", pid: int, to: int, message: str, give=None, receive=None) -> dict:
    """Start a negotiation: a message plus an optional proposal.

    The other side answers in its own time. Until the negotiation is settled or withdrawn, neither side may end its
    turn (see end_turn_refusal).
    """
    try:
        to = int(to)
    except (TypeError, ValueError):
        raise ActionError("'to' must be a player id.")
    if pid != g.s.current:
        raise ActionError("You can only open negotiations during your own turn (you may still reply to others any time).")
    if to == pid or to < 0 or to >= len(g.s.players) or g.player(to).kind != "major" or not g.player(to).alive:
        raise ActionError("Invalid negotiation partner (negotiations are between major civilizations; use the "
                          "city-state tools for city-states).")
    if not g.has_met(pid, to):
        raise ActionError(f"You have not met {g.player(to).name}.")
    if not message or not str(message).strip():
        raise ActionError("Open a negotiation with a message: every entry in a negotiation carries one.")
    for n in g.s.negotiations:
        if n["status"] == "open" and {n["initiator"], n["responder"]} == {pid, to}:
            raise ActionError(f"There is already an open negotiation with {g.player(to).name} (id {n['id']}).")
    per_turn = sum(1 for n in g.s.negotiations if n["turn"] == g.turn and n["initiator"] == pid and n["responder"] == to)
    if per_turn >= g.rules.const["diplomacy"]["negotiations_per_pair_per_turn"]:
        raise ActionError(f"You have already opened {per_turn} negotiations with {g.player(to).name} this turn.")
    proposal = _make_proposal(g, pid, to, give, receive)
    if proposal is not None:
        validate_items(g, pid, to, proposal[str(pid)], proposal)
    text = str(message).strip()[:4000]
    n = {"id": len(g.s.negotiations) + 1, "initiator": pid, "responder": to, "turn": g.turn, "status": "open",
         "awaiting": to, "proposal": proposal, "proposal_by": pid if proposal else None, "exchanges": 0, "history": [],
         "deal_id": None}
    _add_entry(g, n, pid, "open", text, proposal)
    g.s.negotiations.append(n)
    add_message(g, pid, [to], text)
    out = f"{g.player(pid).name} opened negotiations with {g.player(to).name}: \"{text[:500]}\""
    if proposal:
        out += f" Proposal: {g.player(pid).name} gives {describe_items(g, proposal[str(pid)])}; " \
               f"{g.player(to).name} gives {describe_items(g, proposal[str(to)])}."
    g.emit("negotiation", out, [pid, to], negotiation=n["id"], awaiting=to)
    return {"negotiation_id": n["id"], "status": "open", "awaiting": g.player(to).name}


def respond_negotiation(g: "Game", pid: int, nid: int, action: str, message: Optional[str] = None,
                        give=None, receive=None) -> dict:
    """Accept, counter, reject or reply in a negotiation.

    Every response carries a message: a bare "reject" tells the other side nothing, and a chat is only a chat if
    both sides talk. Only the side whose move it is may accept, counter or reply. Either side may reject at any
    time, which is how the opener withdraws a proposal the other side is slow to answer.
    """
    n = get_negotiation(g, int(nid))
    nid = n["id"]
    if n["status"] != "open":
        raise ActionError(f"Negotiation #{nid} is {n['status']}.")
    if pid not in (n["initiator"], n["responder"]):
        raise ActionError(f"You are not part of negotiation #{nid}.")
    other = n["responder"] if pid == n["initiator"] else n["initiator"]
    act = str(action or "").strip().lower()
    act = ACTION_ALIASES.get(act, act)
    if act not in RESPONSE_ACTIONS:
        raise ActionError(f"action must be one of: {', '.join(RESPONSE_ACTIONS)} (decline, withdraw and end also mean "
                          f"reject).")
    if n["awaiting"] != pid and act != "reject":
        raise ActionError(f"It is {g.player(n['awaiting']).name}'s move in negotiation #{nid}; wait for their reply, "
                          f"or withdraw it with action 'reject'.")
    text = str(message or "").strip()[:4000]
    if not text:
        raise ActionError(f"Every response in a negotiation carries a message, and your {act} in negotiation #{nid} "
                          f"has none: add message='...' (a short line will do).")
    me = g.player(pid).name
    if act == "accept":
        if not n["proposal"]:
            raise ActionError(f"There is no proposal on the table in negotiation #{nid} to accept. Use 'reply' or "
                              f"'counter'.")
        if n["proposal_by"] == pid:
            raise ActionError("You cannot accept your own proposal; wait for the other side.")
        deal = execute_deal(g, n["initiator"], n["responder"], n["proposal"])
        n["status"] = "accepted"
        n["deal_id"] = deal["id"]
        n["awaiting"] = None
        _add_entry(g, n, pid, "accept", text)
        add_message(g, pid, [other], text)
        g.emit("negotiation", f"{me} accepted the deal. \"{text[:500]}\"", [pid, other], negotiation=nid,
               status="accepted")
        return {"status": "accepted", "deal": deal["summary"]}
    if act == "reject":
        n["status"] = "rejected"
        n["awaiting"] = None
        _add_entry(g, n, pid, "reject", text)
        add_message(g, pid, [other], text)
        g.emit("negotiation", f"{me} ended the negotiation. \"{text[:500]}\"", [pid, other], negotiation=nid,
               status="rejected")
        return {"status": "rejected"}
    proposal = None
    if act == "counter":
        proposal = _make_proposal(g, pid, other, give or [], receive or [])
        if proposal is None:
            raise ActionError("A counter-offer needs at least one item; use reply to send only a message.")
        validate_items(g, pid, other, proposal[str(pid)], proposal)
    cap = max_chat_messages(g)
    if len(n["history"]) >= cap:
        # the safety cap: a chat that has run this long without a deal is not converging. The message that would pass
        # the cap closes it instead of joining it, so an open chat never holds more than the cap
        close_negotiation(g, nid, "expired", f"The negotiation reached its limit of {cap} messages and closed.")
        return {"status": "expired", "awaiting": None,
                "note": f"Negotiation #{nid} already held {cap} messages, its limit, so it has closed and your {act} "
                        f"was not delivered."}
    if proposal is not None:
        n["proposal"] = proposal
        n["proposal_by"] = pid
    _add_entry(g, n, pid, act, text, proposal)
    n["awaiting"] = other
    add_message(g, pid, [other], text)
    out = f"{me} {'countered' if act == 'counter' else 'replied'}: \"{text[:500]}\""
    if act == "counter":
        out += f" New proposal: {me} gives {describe_items(g, proposal[str(pid)])}; " \
               f"{g.player(other).name} gives {describe_items(g, proposal[str(other)])}."
    g.emit("negotiation", out, [pid, other], negotiation=nid, awaiting=other)
    return {"status": "open", "awaiting": g.player(other).name}


def close_negotiation(g: "Game", nid: int, status: str, note: str, by: Optional[int] = None) -> dict:
    """Close an open negotiation from outside the conversation: time it out, or force it shut.

    Not a tool. The server uses it when an answer does not come in time and before it ends a turn a seat left
    unfinished; the engine uses it when a war or an elimination makes the negotiation moot. The history gets an
    entry carrying the note, so the chat shows why it ended, and both sides get the usual negotiation event. ``by``
    is the player it is closed on behalf of, if any.
    """
    if status not in CLOSED_STATUSES:
        raise ActionError(f"A negotiation closes as {', '.join(CLOSED_STATUSES)}, not '{status}'.")
    n = get_negotiation(g, int(nid))
    if n["status"] != "open":
        raise ActionError(f"Negotiation #{n['id']} is already {n['status']}.")
    n["status"] = status
    n["awaiting"] = None
    note = str(note or "")[:500]
    _add_entry(g, n, by, "close", note=note)
    a, b = n["initiator"], n["responder"]
    verb = {"rejected": "was declined", "expired": "expired", "cancelled": "was cancelled"}[status]
    text = f"Negotiation #{n['id']} between {g.player(a).name} and {g.player(b).name} {verb}. {note}"
    g.emit("negotiation", text.strip(), [a, b], negotiation=n["id"], status=status)
    return n


def expire_negotiations(g: "Game", pid: int):
    """Close the negotiations a player opened that are still open at the end of its turn.

    A safety net for headless runners, which call Game.end_turn directly: the end_turn tool refuses while a
    negotiation is open (end_turn_refusal), so a player going through the tools never gets here with one.
    """
    for n in list(g.s.negotiations):
        if n["status"] == "open" and n["initiator"] == pid:
            close_negotiation(g, n["id"], "expired", f"It was still open at the end of {g.player(pid).name}'s turn.")


def end_turn_refusal(g: "Game", pid: int) -> Optional[str]:
    """Why an open negotiation stops this player ending its turn, or None if none does.

    One waiting on the player must be answered first. One waiting on the other side must get its reply, or be
    withdrawn: ending the turn would otherwise leave the other side answering nobody. A negotiation waiting on the
    player is named first, because that one the player can act on.
    """
    waiting = None
    for n in g.s.negotiations:
        if n["status"] != "open" or pid not in (n["initiator"], n["responder"]):
            continue
        name = g.player(n["responder"] if pid == n["initiator"] else n["initiator"]).name
        if n["awaiting"] == pid:
            return (f"Answer {name} in negotiation #{n['id']} first: respond_negotiation(negotiation_id={n['id']}, "
                    f"action='accept', 'counter', 'reply' or 'reject', message=...). You cannot end your turn while a "
                    f"negotiation waits on you.")
        if waiting is None:
            waiting = (f"You are waiting for {name} to answer negotiation #{n['id']}. End your turn after they reply, "
                       f"or withdraw it with respond_negotiation(negotiation_id={n['id']}, action='reject', "
                       f"message=...).")
    return waiting


def negotiation_view(g: "Game", n: dict, pid: int) -> dict:
    """A negotiation as one side sees it, with the proposal in their own terms.

    Rendered from the viewer's perspective - what *they* would give and receive - because the other
    formulation is a reliable way to accept the opposite of what was meant.
    """
    other = n["responder"] if pid == n["initiator"] else n["initiator"]

    def persp(prop):
        """Flip a proposal into the viewer's perspective."""
        if not prop:
            return None
        return {"you_give": prop.get(str(pid), []), "you_receive": prop.get(str(other), []),
                "summary": f"You give {describe_items(g, prop.get(str(pid), []))}; you receive {describe_items(g, prop.get(str(other), []))}."}

    def entry(i, h):
        """One history entry, from the viewer's side."""
        e = {"seq": h.get("seq", i), "by": g.player(h["by"]).name if h["by"] is not None else None,
             "you": h["by"] == pid, "action": h["action"], "message": h["message"], "proposal": persp(h["proposal"])}
        if h.get("note"):
            e["note"] = h["note"]
        return e

    return {
        "id": n["id"], "with": other, "with_name": g.player(other).name, "status": n["status"],
        "you_initiated": n["initiator"] == pid, "your_move": n["awaiting"] == pid, "turn": n["turn"],
        # entries the game added to close the chat are not messages, so an open chat never shows more than the cap
        "messages": sum(1 for h in n["history"] if h["action"] != "close"), "max_messages": max_chat_messages(g),
        "current_proposal": persp(n["proposal"]),
        "proposal_by_you": n["proposal_by"] == pid if n["proposal"] else None,
        "history": [entry(i, h) for i, h in enumerate(n["history"], 1)],
    }
