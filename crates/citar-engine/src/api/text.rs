//! The fixed texts models and prompts read (DESIGN.md 8.1, `api::text`): the ASCII map's legend
//! and the rules in brief, ported as Python wrote them.

/// What the ASCII map's characters mean (`briefing.MAP_LEGEND`, `briefing.py:15-25`): `get_map`
/// puts it above the map when asked, and the facade hands it to prompts.
pub const MAP_LEGEND: &str = concat!(
    "Legend: each cell is 3 chars [terrain][feature][marker]. Odd rows (y odd) are shifted ",
    "right by half a cell; hex neighbours of (x,y): same row x-1,x+1; for EVEN y: ",
    "(x-1,y-1),(x,y-1),(x-1,y+1),(x,y+1); for ODD y: (x,y-1),(x+1,y-1),(x,y+1),(x+1,y+1).\n",
    "Terrain: G grassland, P plains, D desert, T tundra, S snow, M mountain (impassable), c ",
    "coast, o ocean, l lake, * natural wonder. Feature: h hills, f forest, j jungle, m ",
    "marsh, F flood plains, O oasis, i ice, a atoll, x fallout, r river (no other feature), ",
    "'.' none. A forested hill shows its forest.\n",
    "Markers: @ your city, C foreign city, U your military unit, w your civilian, E enemy ",
    "(at war) unit, N neutral foreign unit, B barbarian unit, X barbarian camp, ! ancient ",
    "ruins, $ resource, + improvement, = road/railroad, space nothing. Blank '   ' = ",
    "unexplored; lowercase cells are remembered, not currently seen.",
);

/// The game's rules in brief, for a model new to them (`views.RULES_OVERVIEW`,
/// `views.py:678-702`): `get_rules` answers it for the topic `overview`, and the facade hands it
/// to prompts.
pub const RULES_OVERVIEW: &str = concat!(
    "CITAR rules overview. The rules and numbers are UnCiv's \"Civ V - Gods & Kings\" ruleset.\n",
    "- Hex map, odd-r offset coordinates (x=column, y=row). Fog of war: you only see near ",
    "your units and territory.\n",
    "- Turns are sequential. On your turn: choose research, set city production, move units, ",
    "adopt policies, negotiate,\n",
    "  then end_turn. Game speed (Quick/Standard/Epic/Marathon) scales costs and the turn ",
    "limit.\n",
    "- One military unit per tile (plus one civilian). Units have 100 HP. Melee attackers ",
    "take counter-damage; ranged don't.\n",
    "- Cities: found with Settlers (at least 3 tiles apart). Cities grow with surplus food, ",
    "produce units, buildings and\n",
    "  wonders, expand borders with culture, bombard nearby enemies, and are captured by ",
    "melee units at 0 HP. Captured\n",
    "  cities start as puppets; you may annex, raze or liberate them (city_status).\n",
    "- Workers improve tiles (farms, mines, pastures...), build roads/railroads, and connect ",
    "cities to the capital.\n",
    "- Resources: strategic ones (Horses, Iron, Coal, Oil, Aluminum, Uranium) are revealed ",
    "by techs and are needed by\n",
    "  some units and buildings; each luxury type gives +4 happiness (and demand for We Love ",
    "The King Day).\n",
    "- Happiness: each city -3 and each citizen -1 (modified by difficulty); unhappy empires ",
    "grow slowly, very unhappy\n",
    "  empires stop growing and fight worse. Positive happiness accumulates toward Golden ",
    "Ages.\n",
    "- Culture buys social policies (10 branches; completing a branch gives a bonus). Faith ",
    "founds a pantheon, earns\n",
    "  Great Prophets who found and enhance religions, and buys religious units and some ",
    "buildings.\n",
    "- Great People come from specialists and wonders (Scientist, Engineer, Merchant, ",
    "Artist, Prophet) and from combat\n",
    "  (Generals, Admirals); each has a special action (unit_action).\n",
    "- City-states: raise influence with gold, units and quests to become their Friend ",
    "(bonuses) or Ally (more bonuses,\n",
    "  their resources, their votes). Espionage: spies steal techs, rig city-state ",
    "elections, stage coups.\n",
    "- Diplomacy: messages are free and non-binding; deals made through negotiations are ",
    "binding (gold, resources,\n",
    "  open borders, embassies, friendship, research agreements, defensive pacts, peace, ",
    "cities, techs).\n",
    "- Victory: Domination (hold every original capital), Scientific (Apollo Program, then ",
    "launch spaceship parts in your\n",
    "  capital), Cultural (complete 5 policy branches and build the Utopia Project), ",
    "Diplomatic (win the United Nations\n",
    "  vote), or the highest score at the turn limit (Time).\n",
);

/// How combat works, in brief (`views.RULES_COMBAT`, `views.py:704-710`): `get_rules` answers it
/// for the topic `combat`.
pub const RULES_COMBAT: &str = concat!(
    "Combat (UnCiv formulas): damage to the defender = 24 + 12r (at most) scaled by the ",
    "strength ratio, with\n",
    "randomness; wounded units fight worse. Modifiers: terrain defense (hills/forest/jungle ",
    "+25%, marsh -15%),\n",
    "fortification (+20% per turn, max +40%), flanking (+10% per adjacent friendly unit), ",
    "attacking across a river or from\n",
    "the sea -20%, Great General +15% within 2 tiles, promotions and unit-specific bonuses ",
    "(e.g. Spearman vs mounted),\n",
    "city strength from population, techs, garrison and walls. Ranged attacks need line of ",
    "sight (unless indirect fire).\n",
    "XP: melee attack 5, defend 4; ranged 2 (3 vs cities); capped at 30 vs barbarians. ",
    "Promotions at 10, 30, 60, 100 XP...\n",
    "Cities heal 20 HP per turn (less while under siege) and capture requires a melee unit.",
);
