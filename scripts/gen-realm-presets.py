#!/usr/bin/env python3
"""Generate the realm service's dungeon presets (crates/wenilla-realm/presets/*.toml).

The roster, talent picks and consumables are written by hand below; the spell lists (class
trainer spells up to the preset level) and the gear (best-scoring obtainable item per slot for
the slot's role) are read from a CMaNGOS classic world database and Talent.dbc. The output is
checked in, so the service never needs the world DB; rerun this after changing a roster.

  scripts/gen-realm-presets.py --db classicmangos --dbc ../server/data/dbc

Needs the `mariadb` client with read access to the world DB (`-u/-p` via MARIADB_ARGS).
"""

import argparse
import os
import shlex
import struct
import subprocess
import sys
from pathlib import Path

OUT = Path(__file__).resolve().parent.parent / "crates/wenilla-realm/presets"

# Class ids, their class trainer template, the TalentTab class mask.
CLASS = {
    "warrior": (1, 11, 1),
    "paladin": (2, 21, 2),
    "hunter": (3, 31, 4),
    "rogue": (4, 41, 8),
    "priest": (5, 51, 16),
    "mage": (8, 71, 128),
    "warlock": (9, 81, 256),
    "druid": (11, 91, 1024),
}
RACE = {"human": 1, "dwarf": 3, "nightelf": 4, "gnome": 7}
# Classes that drink: they get the preset's water and mana potions.
MANA = {"paladin", "hunter", "priest", "mage", "warlock", "druid"}

# Stat weights per role (item_template stat_type: 3 agility, 4 strength, 5 intellect,
# 6 spirit, 7 stamina); `armor` is per point of armor, `dps` per point of weapon dps.
WEIGHTS = {
    "tank": {7: 1.6, 4: 1.0, 3: 0.8, "armor": 0.06, "dps": 1.5},
    "healer": {5: 1.0, 6: 1.0, 7: 0.5, "armor": 0.01, "dps": 0.0},
    "caster": {5: 1.0, 7: 0.7, 6: 0.3, "armor": 0.01, "dps": 0.0},
    "melee": {3: 1.3, 4: 0.6, 7: 0.6, "armor": 0.02, "dps": 3.0},
    "fury": {4: 1.2, 3: 1.0, 7: 0.6, "armor": 0.02, "dps": 3.0},
    "ranged": {3: 1.4, 7: 0.6, 5: 0.3, "armor": 0.02, "dps": 3.0},
}

# Armor subclass by class and level: 1 cloth, 2 leather, 3 mail, 4 plate.
def armor_class(cls, level):
    return {
        "warrior": 4 if level >= 40 else 3,
        "paladin": 4 if level >= 40 else 3,
        "hunter": 3 if level >= 40 else 2,
        "rogue": 2,
        "druid": 2,
    }.get(cls, 1)

# Weapon skill spells a character must know before the server lets it keep an item equipped,
# by (item class, subclass): weapons are class 2, shields armor class 4 subclass 6.
PROFICIENCY = {
    (2, 0): 196, (2, 1): 197, (2, 4): 198, (2, 5): 199, (2, 6): 200, (2, 7): 201,
    (2, 8): 202, (2, 10): 227, (2, 2): 264, (2, 3): 266, (2, 15): 1180, (2, 16): 2567,
    (2, 18): 5011, (2, 19): 5009, (2, 13): 15590, (4, 6): 9116, (4, 4): 750, (4, 3): 8737,
}

# Armor slots every character fills (InventoryType); chest also takes robes (20).
ARMOR_SLOTS = [1, 3, 5, 6, 7, 8, 9, 10, 16]
JEWELRY = [2, 11, 11]  # neck, two rings; trinkets are left to the players' own finds

# Equipment slot index per gear label (rings take 10, then 11).
EQUIP_SLOT = {
    "head": 0, "neck": 1, "shoulders": 2, "chest": 4, "waist": 5, "legs": 6, "feet": 7,
    "wrists": 8, "hands": 9, "back": 14, "main hand": 15, "staff": 15, "two-hand": 15,
    "off hand": 16, "shield": 16, "held in off hand": 16, "ranged": 17, "wand": 17,
}

# Weapon plans: (slot label, InventoryTypes, item class, subclasses) per equipped weapon.
ONE_HAND = [13, 21]
OFF_HAND = [13, 22]
WEAPON_PLANS = {
    "tank": [("main hand", ONE_HAND, 2, [7, 4, 0]), ("shield", [14], 4, [6])],
    "dual": [("main hand", ONE_HAND, 2, [7, 0, 4]), ("off hand", OFF_HAND, 2, [7, 0, 4])],
    "dual_sword": [("main hand", ONE_HAND, 2, [7]), ("off hand", OFF_HAND, 2, [7])],
    "hunter": [("two-hand", [17], 2, [6, 10, 1, 8]), ("ranged", [15, 26], 2, [2, 3, 18])],
    "staff_wand": [("staff", [17], 2, [10]), ("wand", [26], 2, [19])],
    "staff": [("staff", [17], 2, [10])],
    "mace_offhand": [("main hand", ONE_HAND, 2, [4]), ("held in off hand", [23], 4, [0])],
}

# A spec: its class, how its gear is chosen, and its talents as ONE ordered list of
# (talent, rank to reach). A preset takes the prefix its level affords (level - 9 points; the
# last pick may stop short of its rank), so every prefix must obey the tree's tiers — each is
# checked. `extras` are spells a trainer does not teach (quest spells), learned when the
# character's level reaches them.
SPECS = {
    "warrior_prot": {
        "cls": "warrior", "weights": "tank", "plan": "tank", "extras": [71, 2458],
        "talents": [
            ("Shield Specialization", 5), ("Toughness", 5), ("Improved Bloodrage", 1),
            ("Defiance", 5), ("Last Stand", 1), ("Improved Shield Block", 3),
            ("Improved Sunder Armor", 3), ("Improved Taunt", 2), ("Concussion Blow", 1),
            ("One-Handed Weapon Specialization", 5), ("Shield Slam", 1), ("Anticipation", 5),
            ("Improved Shield Wall", 2), ("Improved Shield Bash", 2), ("Improved Bloodrage", 2),
            ("Improved Heroic Strike", 3), ("Deflection", 2), ("Tactical Mastery", 4),
        ],
    },
    "warrior_fury": {
        "cls": "warrior", "weights": "fury", "plan": "dual", "extras": [71, 2458],
        "talents": [
            ("Cruelty", 5), ("Unbridled Wrath", 5), ("Improved Battle Shout", 5), ("Enrage", 5),
            ("Death Wish", 1), ("Improved Execute", 2), ("Dual Wield Specialization", 5),
            ("Flurry", 5), ("Bloodthirst", 1), ("Improved Berserker Rage", 2),
            ("Improved Heroic Strike", 3), ("Deflection", 2), ("Tactical Mastery", 5),
            ("Anger Management", 1), ("Deep Wounds", 3), ("Improved Overpower", 1),
        ],
    },
    "priest_holy": {
        "cls": "priest", "weights": "healer", "plan": "staff_wand", "extras": [],
        "talents": [
            ("Healing Focus", 2), ("Improved Renew", 3), ("Holy Specialization", 5),
            ("Divine Fury", 5), ("Inspiration", 3), ("Holy Nova", 1), ("Improved Healing", 3),
            ("Holy Reach", 2), ("Spiritual Guidance", 5), ("Spirit of Redemption", 1),
            ("Spiritual Healing", 5), ("Lightwell", 1), ("Wand Specialization", 5),
            ("Improved Power Word: Shield", 3), ("Improved Power Word: Fortitude", 2),
            ("Meditation", 3), ("Unbreakable Will", 2),
        ],
    },
    "paladin_holy": {
        "cls": "paladin", "weights": "healer", "plan": "mace_offhand", "extras": [],
        "talents": [
            ("Divine Intellect", 5), ("Spiritual Focus", 5), ("Healing Light", 3),
            ("Consecration", 1), ("Improved Lay on Hands", 1), ("Illumination", 5),
            ("Divine Favor", 1), ("Improved Blessing of Wisdom", 2), ("Improved Lay on Hands", 2),
            ("Unyielding Faith", 1), ("Holy Power", 5), ("Holy Shock", 1),
            ("Improved Devotion Aura", 5), ("Guardian's Favor", 2), ("Toughness", 5),
            ("Blessing of Kings", 1), ("Redoubt", 4), ("Improved Concentration Aura", 3),
        ],
    },
    "druid_resto": {
        "cls": "druid", "weights": "healer", "plan": "staff", "extras": [5487, 1066],
        "talents": [
            ("Improved Mark of the Wild", 5), ("Furor", 5), ("Improved Healing Touch", 5),
            ("Nature's Focus", 5), ("Reflection", 3), ("Improved Rejuvenation", 3),
            ("Nature's Swiftness", 1), ("Gift of Nature", 5), ("Tranquil Spirit", 3),
            ("Improved Regrowth", 5), ("Swiftmend", 1), ("Nature's Grasp", 1),
            ("Improved Nature's Grasp", 4), ("Improved Wrath", 5),
        ],
    },
    "mage_frost": {
        "cls": "mage", "weights": "caster", "plan": "staff_wand", "extras": [],
        "talents": [
            ("Improved Frostbolt", 5), ("Elemental Precision", 3), ("Ice Shards", 5),
            ("Permafrost", 3), ("Piercing Ice", 3), ("Cold Snap", 1), ("Frost Channeling", 3),
            ("Shatter", 5), ("Ice Block", 1), ("Improved Cone of Cold", 3),
            ("Winter's Chill", 5), ("Ice Barrier", 1), ("Arcane Subtlety", 2),
            ("Arcane Focus", 3), ("Arcane Concentration", 5), ("Improved Frost Nova", 2),
            ("Frostbite", 1),
        ],
    },
    "rogue_combat": {
        "cls": "rogue", "weights": "melee", "plan": "dual_sword", "extras": [],
        "talents": [
            ("Improved Sinister Strike", 2), ("Lightning Reflexes", 3), ("Precision", 5),
            ("Deflection", 5), ("Riposte", 1), ("Dual Wield Specialization", 5),
            ("Blade Flurry", 1), ("Sword Specialization", 5), ("Weapon Expertise", 2),
            ("Aggression", 3), ("Adrenaline Rush", 1), ("Improved Eviscerate", 3),
            ("Malice", 5), ("Ruthlessness", 3), ("Murder", 2), ("Relentless Strikes", 1),
            ("Improved Slice and Dice", 3), ("Lethality", 1),
        ],
    },
    "hunter_mm": {
        "cls": "hunter", "weights": "ranged", "plan": "hunter",
        # Tame Beast, Call/Dismiss/Revive/Feed Pet, Beast Training.
        "extras": [1515, 883, 2641, 982, 6991, 5149],
        "talents": [
            ("Efficiency", 5), ("Lethal Shots", 5), ("Improved Hunter's Mark", 1),
            ("Improved Hunter's Mark", 5), ("Aimed Shot", 1), ("Hawk Eye", 3),
            ("Mortal Shots", 5), ("Scatter Shot", 1), ("Barrage", 3),
            ("Ranged Weapon Specialization", 5), ("Trueshot Aura", 1),
            ("Improved Aspect of the Hawk", 5), ("Endurance Training", 5),
            ("Unleashed Fury", 5), ("Bestial Swiftness", 1), ("Improved Revive Pet", 1),
        ],
    },
    "warlock_sm": {
        "cls": "warlock", "weights": "caster", "plan": "staff_wand",
        # Summon Imp/Voidwalker/Succubus/Felhunter, Inferno, Summon Felsteed.
        "extras": [688, 697, 712, 691, 1122, 5784],
        "talents": [
            ("Improved Corruption", 5), ("Suppression", 5), ("Improved Life Tap", 2),
            ("Improved Curse of Agony", 3), ("Amplify Curse", 1), ("Nightfall", 2),
            ("Grim Reach", 2), ("Siphon Life", 1), ("Curse of Exhaustion", 1),
            ("Improved Drain Soul", 2), ("Fel Concentration", 1), ("Shadow Mastery", 5),
            ("Dark Pact", 1), ("Improved Shadow Bolt", 5), ("Bane", 5), ("Devastation", 5),
            ("Shadowburn", 1), ("Cataclysm", 4),
        ],
    },
}
# Racial spells a trainer of the class does not list for everyone.
RACE_EXTRAS = {("priest", "dwarf"): [6346]}  # Fear Ward


def hero(spec, race, gender, label=None, role=None, items=()):
    cls = SPECS[spec]["cls"]
    return {
        "spec": spec, "race": race, "gender": gender,
        "label": label or cls.capitalize(),
        "role": role or {"tank": "Tank", "healer": "Healer"}.get(SPECS[spec]["weights"], "Damage"),
        "items": list(items),
    }


def five(tank, healer, *dps):
    return [tank, healer, *dps]


# Consumables by level band: food and water, potions, and the class extras.
def supplies(level):
    if level < 30:
        return {"all": [(4542, 20)], "health": [(929, 5)], "mana": [(1205, 20), (3385, 5)],
                "hunter": [(2515, 1000)]}
    if level < 45:
        return {"all": [(4544, 20)], "health": [(3928, 5)], "mana": [(1645, 20), (6149, 5)],
                "hunter": [(3030, 1000)], "warlock": [(6265, 10)], "rogue": [(5140, 10)]}
    return {"all": [(8950, 20)], "health": [(13446, 5)], "mana": [(8766, 20), (13443, 5)],
            "hunter": [(11285, 1000)], "warlock": [(6265, 10)], "rogue": [(5140, 10)]}


PRESETS = [
    {
        "id": "deadmines", "name": "The Deadmines", "level": 20, "tele": "TheDeadmines",
        "blurb": "Five level-20 heroes at the mine entrance in Moonbrook, Westfall. VanCleef awaits.",
        "money": 500000,
        "slots": five(hero("warrior_prot", "human", 0), hero("priest_holy", "dwarf", 1),
                      hero("mage_frost", "gnome", 1), hero("rogue_combat", "human", 0),
                      hero("hunter_mm", "nightelf", 1)),
    },
    {
        "id": "scarlet", "name": "The Scarlet Monastery", "level": 38, "tele": "ScarletMonastery",
        "blurb": "Five level-38 heroes at the Monastery gates in Tirisfal Glades — the Library, the Armory, and the Cathedral's Crusader.",
        "money": 1000000,
        "slots": five(hero("warrior_prot", "dwarf", 0), hero("priest_holy", "human", 1),
                      hero("mage_frost", "gnome", 0), hero("rogue_combat", "nightelf", 1),
                      hero("hunter_mm", "dwarf", 0)),
    },
    {
        "id": "sunken", "name": "The Sunken Temple", "level": 52, "tele": "TheSunkenTemple",
        "blurb": "Five level-52 heroes at the drowned temple of Atal'Hakkar in the Swamp of Sorrows. Hakkar's avatar stirs.",
        "money": 1500000,
        "slots": five(hero("warrior_prot", "human", 0), hero("druid_resto", "nightelf", 1),
                      hero("mage_frost", "human", 1), hero("warlock_sm", "gnome", 0),
                      hero("rogue_combat", "dwarf", 0)),
    },
    {
        "id": "brd", "name": "Blackrock Depths", "level": 56, "tele": "BlackrockDepths",
        "blurb": "Five level-56 heroes at the gates of the Dark Iron capital inside Blackrock Mountain.",
        "money": 2000000,
        "slots": five(hero("warrior_prot", "human", 0), hero("priest_holy", "dwarf", 1),
                      hero("mage_frost", "gnome", 1), hero("rogue_combat", "human", 0),
                      hero("warlock_sm", "gnome", 0)),
    },
    {
        "id": "stratholme", "name": "Stratholme", "level": 58, "tele": "Stratholme",
        "blurb": "Five level-58 heroes at the burning gates of Stratholme — the Scarlet bastion on one side, the Baron's undead on the other.",
        "money": 2000000,
        "slots": five(hero("warrior_prot", "dwarf", 0), hero("paladin_holy", "human", 1),
                      hero("mage_frost", "gnome", 1), hero("hunter_mm", "nightelf", 0),
                      hero("rogue_combat", "human", 1)),
    },
    {
        "id": "ubrs", "name": "Upper Blackrock Spire", "level": 60, "tele": "BlackrockSpire",
        "blurb": "Ten level-60 heroes in pre-raid gear at the top of Blackrock Spire. The tank carries the Seal of Ascension for the door. Drakkisath waits.",
        "money": 3000000, "gear": {"quality": 4, "ilvl_max": 63},
        "slots": [
            hero("warrior_prot", "human", 0, items=[(12344, 1)]),  # Seal of Ascension
            hero("warrior_fury", "dwarf", 0), hero("priest_holy", "dwarf", 1),
            hero("paladin_holy", "human", 0), hero("druid_resto", "nightelf", 1),
            hero("mage_frost", "gnome", 1), hero("mage_frost", "human", 0),
            hero("rogue_combat", "human", 0), hero("hunter_mm", "nightelf", 1),
            hero("warlock_sm", "gnome", 0),
        ],
    },
    {
        "id": "mc", "name": "Molten Core", "level": 60, "tele": "TheMoltenSpan",
        "blurb": "A full raid: forty level-60 heroes in pre-raid gear, attuned to the Core, on the Molten Span inside Blackrock Mountain. Jump through the window — Ragnaros is at the bottom.",
        "money": 3000000, "gear": {"quality": 4, "ilvl_max": 63},
        # Attunement to the Core, turned in to Lothos Riftwaker beside the Molten Span.
        "turn_ins": [{"quest": 7848, "npc": 14387, "map": 0, "x": -7506.6, "y": -1041.8, "z": 181.0}],
        "extra_supplies": [(13457, 5)],  # Greater Fire Protection Potion
        "slots": (
            [hero("warrior_prot", r, g) for r, g in [("human", 0), ("dwarf", 0), ("human", 1), ("dwarf", 1)]]
            + [hero("warrior_fury", r, g) for r, g in [("human", 0), ("dwarf", 0), ("nightelf", 1), ("gnome", 0)]]
            + [hero("priest_holy", r, g) for r, g in [("dwarf", 1), ("human", 1), ("dwarf", 0), ("human", 0), ("nightelf", 1), ("dwarf", 1), ("human", 1)]]
            + [hero("paladin_holy", r, g) for r, g in [("human", 0), ("dwarf", 0), ("human", 1), ("dwarf", 1)]]
            + [hero("druid_resto", "nightelf", g) for g in (0, 1)]
            + [hero("mage_frost", r, g) for r, g in [("gnome", 1), ("human", 0), ("gnome", 0), ("human", 1), ("gnome", 1), ("human", 0)]]
            + [hero("warlock_sm", r, g) for r, g in [("gnome", 0), ("human", 1), ("gnome", 1), ("human", 0)]]
            + [hero("rogue_combat", r, g) for r, g in [("human", 0), ("dwarf", 0), ("nightelf", 1), ("gnome", 0), ("human", 1)]]
            + [hero("hunter_mm", r, g) for r, g in [("nightelf", 1), ("dwarf", 0), ("nightelf", 0), ("dwarf", 1)]]
        ),
    },
]


def sql(db, query):
    args = ["mariadb", *shlex.split(os.environ.get("MARIADB_ARGS", "-umangos -pmangos")),
            db, "-N", "-B", "-e", query]
    out = subprocess.run(args, capture_output=True, text=True, check=True).stdout
    return [line.split("\t") for line in out.splitlines() if line]


def dbc(path):
    d = path.read_bytes()
    n, f, rs, _ = struct.unpack_from("<4I", d, 4)
    recs = d[20:20 + n * rs]
    return [struct.unpack_from(f"<{f}I", recs, i * rs) for i in range(n)], d[20 + n * rs:]


def talent_table(dbc_dir, db):
    """{class mask: {talent name: (tab id, row, [rank spell ids])}}, names from spell_template."""
    tabs, _ = dbc(dbc_dir / "TalentTab.dbc")
    tal, _ = dbc(dbc_dir / "Talent.dbc")
    tab_mask = {r[0]: r[-3] for r in tabs}
    firsts = {r[4] for r in tal if r[4]}
    names = dict(sql(db, "SELECT Id, SpellName FROM spell_template WHERE Id IN (%s)"
                     % ",".join(map(str, firsts))))
    out = {}
    for r in tal:
        ranks = [x for x in r[4:9] if x]
        if not ranks:
            continue
        mask = tab_mask.get(r[1], 0)
        out.setdefault(mask, {})[names[str(ranks[0])]] = (r[1], r[2], ranks)
    return out


def talent_spells(spec_name, level, talents):
    """The rank spell of each talent the level's points reach along the spec's ordered list,
    checking the tree's tier rule at every step. Returns (spell ids, [(name, rank)], names of every
    talent of the class)."""
    spec = SPECS[spec_name]
    mask = CLASS[spec["cls"]][2]
    table = next(v for k, v in talents.items() if k & mask)
    budget = max(level - 9, 0)
    spent_in_tab, total, rank_of = {}, 0, {}
    for name, target in spec["talents"]:
        if total == budget:
            break
        tab, row, ranks = table[name]
        if target > len(ranks):
            sys.exit(f"{spec_name}: {name} has only {len(ranks)} ranks")
        if spent_in_tab.get(tab, 0) < 5 * row:
            sys.exit(f"{spec_name}: {name} is tier {row + 1}, which needs {5 * row} points in its "
                     f"tree before it (have {spent_in_tab.get(tab, 0)}); reorder the list")
        have = rank_of.get(name, 0)
        add = min(target - have, budget - total)
        if add <= 0:
            continue
        rank_of[name] = have + add
        spent_in_tab[tab] = spent_in_tab.get(tab, 0) + add
        total += add
    if total != budget:
        sys.exit(f"{spec_name}: the list has {total} points, level {level} needs {budget}")
    picks = list(rank_of.items())
    all_ranks = {x for _, _, ranks in table.values() for x in ranks}
    return [table[n][2][r - 1] for n, r in picks], picks, set(table), all_ranks


def rank_chains(db, dbc_dir):
    """spell -> the spell it is the next rank of, from both places the server reads chains:
    the world DB's spell_chain and SkillLineAbility.dbc's superseded-by column."""
    prev = {int(a): int(b) for a, b in sql(db, "SELECT spell_id, prev_spell FROM spell_chain WHERE prev_spell <> 0")}
    rows, _ = dbc(dbc_dir / "SkillLineAbility.dbc")
    for r in rows:
        spell, next_rank = r[2], r[8]
        if next_rank:
            prev.setdefault(next_rank, spell)
    return prev


def trainer_spells(db, cls, level, talent_names, talent_ranks, prev):
    cls_id, template, _ = CLASS[cls]
    rows = sql(db, f"""
        SELECT s.EffectTriggerSpell1, l.SpellName
        FROM npc_trainer_template t JOIN spell_template s ON s.Id = t.spell
        JOIN spell_template l ON l.Id = s.EffectTriggerSpell1
        WHERE t.entry = {template} AND t.reqlevel <= {level} AND s.Effect1 = 36
        ORDER BY t.reqlevel, t.spell""")
    out = []
    for spell, name in rows:
        spell = int(spell)
        # Nothing whose rank chain runs back to a talent, taken or not — Shield Slam's trainer
        # ranks, or Prayer of Spirit, which the server chains to the Divine Spirit talent. Loading
        # such a spell makes the server add the talent as a dependent and count its point; a
        # character over its level's points has every talent reset at its next login. So a
        # taken talent ability stays at rank 1, and talent-born group spells are left out.
        chain, cur = set(), spell
        while cur and cur not in chain:
            chain.add(cur)
            cur = prev.get(cur)
        if name in talent_names or chain & talent_ranks:
            continue
        if spell not in out:
            out.append(spell)
    return out


# Items a player can actually come by: loot, vendors, quest rewards. Read once per run.
OBTAINABLE_SQL = """
    SELECT item FROM creature_loot_template UNION SELECT item FROM reference_loot_template
    UNION SELECT item FROM gameobject_loot_template UNION SELECT item FROM item_loot_template
    UNION SELECT item FROM npc_vendor UNION SELECT item FROM npc_vendor_template
    UNION SELECT RewChoiceItemId1 FROM quest_template UNION SELECT RewChoiceItemId2 FROM quest_template
    UNION SELECT RewChoiceItemId3 FROM quest_template UNION SELECT RewChoiceItemId4 FROM quest_template
    UNION SELECT RewChoiceItemId5 FROM quest_template UNION SELECT RewChoiceItemId6 FROM quest_template
    UNION SELECT RewItemId1 FROM quest_template UNION SELECT RewItemId2 FROM quest_template"""
OBTAINABLE_IDS = set()

ITEM_COLS = ("entry, name, class, subclass, InventoryType, ItemLevel, RequiredLevel, armor, "
             "dmg_min1, dmg_max1, delay, maxcount, "
             + ", ".join(f"stat_type{i}, stat_value{i}" for i in range(1, 11)))


def candidates(db, level, cls, race, inv_types, item_class, subclasses, gear, random_ok=False):
    cls_mask = 1 << (CLASS[cls][0] - 1)
    race_mask = 1 << (RACE[race] - 1)
    rows = sql(db, f"""
        SELECT {ITEM_COLS} FROM item_template
        WHERE InventoryType IN ({",".join(map(str, inv_types))})
          AND class = {item_class} AND subclass IN ({",".join(map(str, subclasses))})
          AND Quality BETWEEN 2 AND {gear.get("quality", 3)} AND ItemLevel <= {gear.get("ilvl_max", 999)}
          AND RequiredLevel <= {level} AND RequiredLevel >= {level - 15}
          AND (RandomProperty = 0 OR {int(random_ok)}) AND RequiredSkill = 0 AND RequiredReputationFaction = 0
          AND requiredhonorrank = 0 AND requiredspell = 0
          AND (AllowableClass <= 0 OR AllowableClass & {cls_mask} OR AllowableClass = 32767)
          AND (AllowableRace <= 0 OR AllowableRace & {race_mask} OR AllowableRace = 255)
          AND name NOT REGEXP 'Test|Monster|Deprecated|DEPRECATED|NPC|Unused|QA|OLD|\\\\['
          -- Battleground and PvP-rank rewards: faction-locked in practice, whatever the row says.
          AND name NOT REGEXP "Sentinel's|Legionnaire's|Protector's|Outrider's|Advisor's|Scout's|Defiler's|Arathor|Highlander's|Warsong|Stormpike|Frostwolf|Knight-|Sergeant's|Marshal's|General's|Warlord's|Champion's|Lieutenant|Blood Guard|Commander's"
          """)
    return [r for r in rows if int(r[0]) in OBTAINABLE_IDS]


def score(row, weights):
    (entry, name, iclass, sub, inv, ilvl, req, armor, dmin, dmax, delay, _maxcount, *stats) = row
    s = float(armor) * weights["armor"] + 0.05 * float(ilvl)
    for i in range(0, 20, 2):
        s += weights.get(int(stats[i]), 0) * float(stats[i + 1])
    if int(delay) > 0 and weights["dps"]:
        s += weights["dps"] * (float(dmin) + float(dmax)) / 2 / (int(delay) / 1000)
    return s


def pick(db, preset, slot):
    """The best obtainable item per slot for the hero's spec, within the preset's gear limits
    (quality up to rare, or epic below an item level for the level-60 content)."""
    level, race = preset["level"], slot["race"]
    spec = SPECS[slot["spec"]]
    cls, w, gear = spec["cls"], WEIGHTS[spec["weights"]], preset.get("gear", {})
    chosen = []
    taken = set()

    def best(inv_types, item_class, subclasses, label):
        # Fixed-stat items first; a random-suffix one (its stats rolled by `.additem`) only when
        # the slot has nothing else, and an empty slot when neither exists at this level.
        for random_ok in (False, True):
            rows = candidates(db, level, cls, race, inv_types, item_class, subclasses, gear, random_ok)
            rows = [r for r in rows if not (r[0] in taken and int(r[11]) == 1)]
            if rows:
                break
        else:
            print(f"{preset['id']}/{slot['label']}: nothing for {label}, left empty", file=sys.stderr)
            return
        r = max(rows, key=lambda r: (score(r, w), int(r[0])))
        taken.add(r[0])
        chosen.append((int(r[0]), r[1], label, (int(r[2]), int(r[3]))))

    names = {1: "head", 2: "neck", 3: "shoulders", 5: "chest", 6: "waist", 7: "legs", 8: "feet",
             9: "wrists", 10: "hands", 11: "finger", 16: "back"}
    for inv in ARMOR_SLOTS:
        types = [5, 20] if inv == 5 else [inv]
        if inv == 16:
            best(types, 4, [1], names[inv])  # cloaks are cloth whoever wears them
        else:
            best(types, 4, [armor_class(cls, level)], names[inv])
    for inv in JEWELRY:
        best([inv], 4, [0], names[inv])
    for label, types, iclass, subs in WEAPON_PLANS[spec["plan"]]:
        best(types, iclass, subs, label)
    return chosen


def spells_up_to(db, ids, level):
    """The ids among `ids` a character of `level` can have (their spell level at most it)."""
    if not ids:
        return []
    rows = sql(db, "SELECT Id FROM spell_template WHERE spellLevel <= %d AND Id IN (%s)"
               % (level, ",".join(map(str, ids))))
    return [int(r[0]) for r in rows]


def toml_str(s):
    return '"' + s.replace("\\", "\\\\").replace('"', '\\"') + '"'


def item_names(db, ids):
    return dict(sql(db, "SELECT entry, name FROM item_template WHERE entry IN (%s)"
                    % ",".join(map(str, ids))))


def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--db", default="classicmangos")
    ap.add_argument("--dbc", type=Path, required=True, help="directory holding Talent.dbc and TalentTab.dbc")
    ap.add_argument("--only", help="generate just this preset id")
    a = ap.parse_args()
    talents = talent_table(a.dbc, a.db)
    prev = rank_chains(a.db, a.dbc)
    global OBTAINABLE_IDS
    OBTAINABLE_IDS = {int(r[0]) for r in sql(a.db, OBTAINABLE_SQL)}
    OUT.mkdir(parents=True, exist_ok=True)
    bag = 4500  # Traveler's Backpack
    for p in PRESETS:
        if a.only and p["id"] != a.only:
            continue
        lines = [
            "# Generated by scripts/gen-realm-presets.py from the world DB and Talent.dbc — edit the",
            "# roster there and rerun rather than editing this file by hand.",
            f"id = {toml_str(p['id'])}",
            f"name = {toml_str(p['name'])}",
            f"blurb = {toml_str(p['blurb'])}",
            f"level = {p['level']}",
            f"tele = {toml_str(p['tele'])}",
            f"money = {p['money']}",
        ]
        for t in p.get("turn_ins", []):
            lines += ["", "[[turn_ins]]"] + [f"{k} = {v}" for k, v in t.items()]
        sup = supplies(p["level"])
        for slot in p["slots"]:
            spec = SPECS[slot["spec"]]
            cls = spec["cls"]
            tal_ids, picks, talent_names, talent_ranks = talent_spells(slot["spec"], p["level"], talents)
            spells = trainer_spells(a.db, cls, p["level"], talent_names, talent_ranks, prev)
            extras = spells_up_to(a.db, spec["extras"] + RACE_EXTRAS.get((cls, slot["race"]), []), p["level"])
            gear = pick(a.db, p, slot)
            profs = sorted({PROFICIENCY[k] for *_, k in gear if k in PROFICIENCY})
            if spec["plan"] in ("dual", "dual_sword"):
                profs.append(674)  # Dual Wield
            # A hunter starts with a quiver in one bag slot, and a second quiver cannot be equipped.
            bags = [bag] * (3 if cls == "hunter" else 4)
            cons = list(sup["all"]) + list(sup["health"])
            if cls in MANA:
                cons += sup["mana"]
            cons += sup.get(cls, []) + p.get("extra_supplies", []) + slot["items"]
            names = item_names(a.db, [i for i, _ in cons] + bags)
            lines += [
                "",
                "[[slots]]",
                f"label = {toml_str(slot['label'])}",
                f"role = {toml_str(slot['role'])}",
                f"spec = {toml_str(slot['spec'])}",
                f"class = {CLASS[cls][0]}  # {cls}",
                f"race = {RACE[slot['race']]}  # {slot['race']}",
                f"gender = {slot['gender']}",
                f"spells = {sorted(set(spells + extras))}",
                f"proficiencies = {sorted(set(profs))}",
                "talents = [" + ", ".join(f"{i}" for i in tal_ids) + "]  # "
                + ", ".join(f"{n} {r}" for n, r in picks),
                "bags = [" + ", ".join(str(b) for b in bags) + "]  # " + ", ".join(names[str(b)] for b in bags),
                "gear = [",
            ]
            for entry, name, label, _ in gear:
                lines.append(f"  {entry},  # {label}: {name}")
            lines.append("]")
            # Where each item goes (equipment slot index): the builder puts back anything a later
            # equip pushed out, so a one-hander never ends up bumping the main hand into a bag.
            slots, rings = [], 0
            for _, _, label, _ in gear:
                if label == "finger":
                    slots.append(10 + rings)
                    rings += 1
                else:
                    slots.append(EQUIP_SLOT[label])
            lines.append(f"gear_slots = {slots}")
            lines.append("consumables = [")
            for entry, count in cons:
                lines.append(f"  [{entry}, {count}],  # {names[str(entry)]}")
            lines.append("]")
        (OUT / f"{p['id']}.toml").write_text("\n".join(lines) + "\n")
        print(f"wrote {OUT / (p['id'] + '.toml')} ({len(p['slots'])} heroes)")


if __name__ == "__main__":
    main()
