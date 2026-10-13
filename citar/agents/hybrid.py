"""A hybrid seat's settings: who decides each kind of its diplomacy, and what that means for the engine, the model's
tools and the bot's veto (design/phase3-features.md P3.6.2, P3.6.6).

A hybrid seat is a bot that plays the turns with a language model that decides some of its diplomacy. Its seat
(``citar.server.session.Seat``) carries the bot's settings (``bot``), the model's machine (``llm``, as an LLM seat's,
P3.2.9) and ``hybrid``::

    {"owners": {"trades": "llm", "chat": "llm"} | "trade_only" | "all_but_war" | "everything",
     "advisor": true, "prompt_every": 10, "phase_seconds": 180}

``owners`` names who decides each of the ten categories (``CATEGORIES``; ``bot`` for any not named) or a preset;
``normalize_hybrid`` checks it through the engine's own parser (``engine_api.bot_set_diplomacy`` on a scratch bot, whose
messages are the errors) and keeps the whole map. The seat's difficulty level and handicap are the keys every seat has
(``difficulty``, ``handicap``): a hybrid seat's handicap is ``ai`` unless it says ``human``, and there is no third
spelling inside ``hybrid``.

This module is pure: the session (``citar.server.session``) and the agent (``citar.agents.hybrid_agent``) use it.

Replaces: nothing; before 3-14 the server had no hybrid seat (``BotAgent`` drove a ``hybrid_diplomat`` stop straight on).
"""
from __future__ import annotations

from typing import Optional

from .. import engine_api

#: The ten kinds of diplomatic decision, in the engine's order (``game/diplomacy/category.rs``).
CATEGORIES: tuple = tuple(engine_api.DIPLOMACY_CATEGORIES)
#: The presets the lobby offers (P3.6.2): trade only, all but war, everything.
PRESETS: dict = {
    "trade_only": ("trades", "chat"),
    "all_but_war": tuple(c for c in CATEGORIES if c != "war"),
    "everything": CATEGORIES,
}
#: A seat's ``handicap``: whose numbers it plays with (``ai``: the AI's bonuses, as the bot's; ``human``: a person's).
HANDICAPS = ("ai", "human")
#: ``prompt_every``'s bounds and default (turns between the diplomat's phases when nothing else calls it).
PROMPT_EVERY_MIN, PROMPT_EVERY_MAX, PROMPT_EVERY_DEFAULT = 1, 50, 10
#: How long a diplomat phase may run once started, by default, and the bounds of a seat's own ``phase_seconds``.
PHASE_SECONDS_DEFAULT, PHASE_SECONDS_MIN, PHASE_SECONDS_MAX = 180.0, 10.0, 600.0

#: What the diplomat may always use (P3.6.6): it reads, keeps its notebook and says what it thinks.
BASE_TOOLS = ("get_diplomacy", "get_players", "get_empire", "get_victory_status", "read_notes", "write_notes",
              "log_thought")
#: The tools each category the model decides gives it (P3.6.6).
CATEGORY_TOOLS: dict = {
    "trades": ("open_negotiation", "respond_negotiation"),
    "agreements": ("open_negotiation", "respond_negotiation"),
    "peace": ("open_negotiation", "respond_negotiation"),
    "war": ("declare_war",),
    "denounce": ("denounce",),
    "un": ("un_vote",),
    "city_states": ("get_city_states", "city_state_action"),
    "espionage": ("get_espionage", "move_spy", "stage_coup"),
    "captured_cities": ("get_cities", "city_status"),
    "chat": ("send_message", "respond_negotiation"),
}
#: The chat tools (P3.5.3), of which a hybrid seat's model answering a chat gets those its categories give it, and
#: always ``respond_negotiation``: a chat its bot left to it is the model's to answer.
CHAT_TOOLS = ("get_diplomacy", "get_empire", "get_players", "get_victory_status", "read_notes", "write_notes",
              "log_thought", "respond_negotiation", "send_message")
#: What the session answers a hybrid seat's model that calls a tool none of its categories gives it (P3.6.6).
NOT_YOURS = "That is the bot's to decide."


def owners_map(owners) -> dict:
    """``owners`` as the whole map {category: "bot" | "llm"}: a preset's name, or a map that names some categories
    (``bot`` for the others). Raises ValueError with the engine's own words for a category or an owner that is not
    one, and for a preset that is not one."""
    if isinstance(owners, str):
        if owners not in PRESETS:
            raise ValueError(f"Unknown preset '{owners}' (the presets are {', '.join(PRESETS)}).")
        mine = set(PRESETS[owners])
        return {c: ("llm" if c in mine else "bot") for c in CATEGORIES}
    if owners is None:
        owners = {}
    if not isinstance(owners, dict):
        raise ValueError('Diplomacy owners are an object of {category: "bot" | "llm"}, or a preset ('
                         + ", ".join(PRESETS) + ").")
    # the engine's parser on a scratch bot: its messages are the refusals (it names every unknown category, then
    # every bad owner, as Python's set_diplomacy did)
    engine_api.bot_set_diplomacy(engine_api.bot_instance("idle"), owners)
    return {c: owners.get(c, "bot") for c in CATEGORIES}


def preset_of(owners: dict) -> Optional[str]:
    """The preset an owners map is, or None for a map of its own."""
    for name in PRESETS:
        if owners == owners_map(name):
            return name
    return None


def normalize_hybrid(block, *, benchmark: bool = False) -> dict:
    """A hybrid seat's ``hybrid`` block checked and completed (P3.6.2): ``owners`` the whole map (from a preset or a
    map, through the engine's parser), at least one category the model's ("a hybrid seat with nothing for its model is
    a bot seat"), ``prompt_every`` 1..50 (10), ``advisor`` (default on, and always off in a benchmark game),
    ``phase_seconds`` 10..600 (180), and ``preset`` when the owners are one. Raises ValueError with the reason."""
    if block is None:
        block = {}
    if not isinstance(block, dict):
        raise ValueError("A hybrid seat's settings are an object: {\"owners\": ..., \"advisor\": ..., "
                         "\"prompt_every\": ...}.")
    owners = owners_map(block.get("owners", "trade_only") if "owners" in block else "trade_only")
    if not any(v == "llm" for v in owners.values()):
        raise ValueError("A hybrid seat with nothing for its model is a bot seat: give its model at least one kind "
                         "of diplomacy, or make it a bot seat.")
    every = block.get("prompt_every", PROMPT_EVERY_DEFAULT)
    if isinstance(every, bool) or not isinstance(every, (int, float)) or every != int(every) \
            or not PROMPT_EVERY_MIN <= int(every) <= PROMPT_EVERY_MAX:
        raise ValueError(f"prompt_every is a whole number of turns from {PROMPT_EVERY_MIN} to {PROMPT_EVERY_MAX}.")
    advisor = block.get("advisor", True)
    if not isinstance(advisor, bool):
        raise ValueError("advisor is true or false.")
    phase = block.get("phase_seconds", PHASE_SECONDS_DEFAULT)
    if isinstance(phase, bool) or not isinstance(phase, (int, float)) or phase != phase \
            or not PHASE_SECONDS_MIN <= float(phase) <= PHASE_SECONDS_MAX:
        raise ValueError(f"phase_seconds is from {PHASE_SECONDS_MIN:.0f} to {PHASE_SECONDS_MAX:.0f} seconds.")
    out = {"owners": owners, "advisor": bool(advisor) and not benchmark, "prompt_every": int(every),
           "phase_seconds": float(phase)}
    preset = preset_of(owners)
    if preset:
        out["preset"] = preset
    return out


def check_handicap(handicap) -> str:
    """A hybrid seat's handicap: ``ai`` (the default, "AI bonuses, like the bot") or ``human`` ("a person's values,
    like a model"). Raises ValueError for anything else."""
    if handicap in (None, ""):
        return "ai"
    if handicap not in HANDICAPS:
        raise ValueError(f"A seat's handicap is 'ai' or 'human', not '{handicap}'.")
    return handicap


def model_categories(owners: dict) -> list:
    """The categories the model decides, in the engine's order."""
    return [c for c in CATEGORIES if (owners or {}).get(c) == "llm"]


def bot_categories(owners: dict) -> list:
    """The categories the bot decides, in the engine's order."""
    return [c for c in CATEGORIES if (owners or {}).get(c) != "llm"]


def engine_auto(owners: dict) -> dict:
    """The engine's automatic decisions for a hybrid seat (P3.6.2): it votes in the United Nations and annexes the
    cities the seat captures for it only where the bot decides those, and always makes its free picks."""
    return {"un_vote": owners.get("un") != "llm", "conquest": owners.get("captured_cities") != "llm",
            "free_picks": True}


#: The controllers people and models play, whose seats get a person's numbers and no automatic decisions unless they
#: say otherwise (the engine's ``Controller::is_humanlike``).
HUMANLIKE = ("human", "llm", "mcp")


def default_handicap(seat_type: str) -> str:
    """The handicap a controller gets unless its seat says otherwise (the engine's ``default_handicap``)."""
    return "human" if seat_type in HUMANLIKE else "ai"


def default_auto(seat_type: str) -> dict:
    """The automatic decisions a controller gets unless its seat says otherwise (the engine's ``default_auto``)."""
    on = seat_type not in HUMANLIKE
    return {"un_vote": on, "conquest": on, "free_picks": on}


def diplomat_tools(owners: dict) -> list:
    """The tools of a diplomat phase (P3.6.6): ``BASE_TOOLS`` and those of each category the model decides, in the
    registry's order."""
    wanted = set(BASE_TOOLS)
    for c in model_categories(owners):
        wanted.update(CATEGORY_TOOLS.get(c, ()))
    return [t["name"] for t in engine_api.tool_list() if t["name"] in wanted]


def chat_tools(owners: dict) -> list:
    """The tools of a hybrid seat's model answering a chat its bot left to it: the chat tools its categories give it,
    and ``respond_negotiation``."""
    allowed = set(diplomat_tools(owners)) | {"respond_negotiation"}
    return [t for t in CHAT_TOOLS if t in allowed]


def items_categories(*lists) -> set:
    """The categories deal items (as a caller writes them) touch; an item that does not read is left to the engine to
    refuse."""
    out = set()
    for items in lists:
        for item in items if isinstance(items, list) else ():
            try:
                out.add(engine_api.item_category(item))
            except (ValueError, TypeError):
                continue
    return out
