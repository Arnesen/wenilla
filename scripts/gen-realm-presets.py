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
    "hunter": (3, 31, 4),
    "rogue": (4, 41, 8),
    "priest": (5, 51, 16),
    "mage": (8, 71, 128),
    "warlock": (9, 81, 256),
}
RACE = {"human": 1, "dwarf": 3, "nightelf": 4, "gnome": 7}

# Stat weights per role (item_template stat_type: 3 agility, 4 strength, 5 intellect,
# 6 spirit, 7 stamina); `armor` is per point of armor, `dps` per point of weapon dps.
WEIGHTS = {
    "tank": {7: 1.6, 4: 1.0, 3: 0.8, "armor": 0.06, "dps": 1.5},
    "healer": {5: 1.0, 6: 1.0, 7: 0.5, "armor": 0.01, "dps": 0.0},
    "caster": {5: 1.0, 7: 0.7, 6: 0.3, "armor": 0.01, "dps": 0.0},
    "melee": {3: 1.3, 4: 0.6, 7: 0.6, "armor": 0.02, "dps": 3.0},
    "ranged": {3: 1.4, 7: 0.6, 5: 0.3, "armor": 0.02, "dps": 3.0},
}

# Armor subclass by class and level: 1 cloth, 2 leather, 3 mail, 4 plate.
def armor_class(cls, level):
    return {
        "warrior": 4 if level >= 40 else 3,
        "hunter": 3 if level >= 40 else 2,
        "rogue": 2,
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

# Weapon plans by role: a list of (slot label, InventoryTypes, item class/subclasses).
def weapon_plan(cls, role):
    one_hand = [13, 21]
    if role == "tank":
        return [("main hand", one_hand, 2, [7, 4, 0]), ("shield", [14], 4, [6])]
    if cls == "rogue":
        return [("main hand", one_hand, 2, [7]), ("off hand", [13, 22], 2, [7])]
    if cls == "hunter":
        return [("two-hand", [17], 2, [6, 10, 1, 8]), ("ranged", [15, 26], 2, [2, 3, 18])]
    # Casters and healers: a staff and a wand.
    return [("staff", [17], 2, [10]), ("wand", [26], 2, [19])]


PRESETS = [
    {
        "id": "deadmines",
        "name": "The Deadmines",
        "blurb": "Five level-20 heroes at the mine entrance in Moonbrook, Westfall. VanCleef awaits.",
        "level": 20,
        "tele": "TheDeadmines",
        "money": 500000,  # 50 gold
        "slots": [
            {
                "label": "Warrior", "role_label": "Tank", "cls": "warrior", "race": "human",
                "gender": 0, "weights": "tank",
                "talents": [("Shield Specialization", 5), ("Toughness", 5), ("Improved Bloodrage", 1)],
                "extra_spells": [71],  # Defensive Stance (a quest spell)
            },
            {
                "label": "Priest", "role_label": "Healer", "cls": "priest", "race": "dwarf",
                "gender": 1, "weights": "healer",
                "talents": [("Improved Renew", 3), ("Healing Focus", 2), ("Divine Fury", 5),
                            ("Holy Specialization", 1)],
                "extra_spells": [6346],  # Fear Ward (dwarf racial)
            },
            {
                "label": "Mage", "role_label": "Damage", "cls": "mage", "race": "gnome",
                "gender": 1, "weights": "caster",
                "talents": [("Improved Frostbolt", 5), ("Ice Shards", 5), ("Elemental Precision", 1)],
                "extra_spells": [],
            },
            {
                "label": "Rogue", "role_label": "Damage", "cls": "rogue", "race": "human",
                "gender": 0, "weights": "melee",
                "talents": [("Improved Sinister Strike", 2), ("Lightning Reflexes", 3),
                            ("Precision", 5), ("Improved Backstab", 1)],
                "extra_spells": [],
            },
            {
                "label": "Hunter", "role_label": "Damage", "cls": "hunter", "race": "nightelf",
                "gender": 1, "weights": "ranged",
                "talents": [("Efficiency", 5), ("Lethal Shots", 5), ("Improved Hunter's Mark", 1)],
                # Tame Beast, Call/Dismiss/Revive/Feed Pet, Beast Training (quest spells).
                "extra_spells": [1515, 883, 2641, 982, 6991, 5149],
            },
        ],
        "bags": 4500,  # Traveler's Backpack
        "consumables": {
            "all": [(4542, 20)],  # Moist Cornbread
            "mana": [(1205, 20), (3385, 5)],  # Melon Juice, Lesser Mana Potion
            "health": [(929, 5)],  # Healing Potion
            "hunter": [(2515, 1000)],  # Sharp Arrow
        },
    },
    {
        "id": "brd",
        "name": "Blackrock Depths",
        "blurb": "Five level-56 heroes at the gates of the Dark Iron capital inside Blackrock Mountain.",
        "level": 56,
        "tele": "BlackrockDepths",
        "money": 2000000,  # 200 gold
        "slots": [
            {
                "label": "Warrior", "role_label": "Tank", "cls": "warrior", "race": "human",
                "gender": 0, "weights": "tank",
                "talents": [("Shield Specialization", 5), ("Anticipation", 5), ("Toughness", 5),
                            ("Improved Bloodrage", 2), ("Last Stand", 1), ("Defiance", 5),
                            ("Improved Shield Block", 3), ("Improved Sunder Armor", 3),
                            ("Improved Taunt", 2), ("Concussion Blow", 1),
                            ("Improved Shield Wall", 2), ("One-Handed Weapon Specialization", 5),
                            ("Shield Slam", 1), ("Improved Heroic Strike", 3), ("Deflection", 2),
                            ("Tactical Mastery", 2)],
                "extra_spells": [71, 2458],  # Defensive and Berserker Stance
            },
            {
                "label": "Priest", "role_label": "Healer", "cls": "priest", "race": "dwarf",
                "gender": 1, "weights": "healer",
                "talents": [("Healing Focus", 2), ("Improved Renew", 3), ("Holy Specialization", 5),
                            ("Divine Fury", 5), ("Inspiration", 3), ("Holy Nova", 1),
                            ("Improved Healing", 3), ("Holy Reach", 2), ("Spiritual Guidance", 5),
                            ("Spirit of Redemption", 1), ("Spiritual Healing", 5), ("Lightwell", 1),
                            ("Wand Specialization", 5), ("Improved Power Word: Shield", 3),
                            ("Improved Power Word: Fortitude", 2), ("Meditation", 1)],
                "extra_spells": [6346],
            },
            {
                "label": "Mage", "role_label": "Damage", "cls": "mage", "race": "gnome",
                "gender": 1, "weights": "caster",
                "talents": [("Improved Frostbolt", 5), ("Elemental Precision", 3), ("Ice Shards", 5),
                            ("Permafrost", 3), ("Piercing Ice", 3), ("Cold Snap", 1),
                            ("Frost Channeling", 3), ("Shatter", 5), ("Ice Block", 1),
                            ("Improved Cone of Cold", 3), ("Winter's Chill", 5), ("Ice Barrier", 1),
                            ("Arcane Subtlety", 2), ("Arcane Focus", 3), ("Arcane Concentration", 4)],
                "extra_spells": [],
            },
            {
                "label": "Rogue", "role_label": "Damage", "cls": "rogue", "race": "human",
                "gender": 0, "weights": "melee",
                "talents": [("Improved Sinister Strike", 2), ("Lightning Reflexes", 3),
                            ("Precision", 5), ("Deflection", 5), ("Riposte", 1),
                            ("Dual Wield Specialization", 5), ("Blade Flurry", 1),
                            ("Sword Specialization", 5), ("Weapon Expertise", 2), ("Aggression", 3),
                            ("Adrenaline Rush", 1), ("Improved Eviscerate", 3), ("Malice", 5),
                            ("Ruthlessness", 3), ("Murder", 2), ("Relentless Strikes", 1)],
                "extra_spells": [],
            },
            {
                "label": "Warlock", "role_label": "Damage", "cls": "warlock", "race": "gnome",
                "gender": 0, "weights": "caster",
                "talents": [("Improved Corruption", 5), ("Suppression", 5), ("Improved Life Tap", 2),
                            ("Improved Curse of Agony", 3), ("Amplify Curse", 1), ("Nightfall", 2),
                            ("Grim Reach", 2), ("Siphon Life", 1), ("Curse of Exhaustion", 1),
                            ("Improved Drain Soul", 2), ("Fel Concentration", 1),
                            ("Shadow Mastery", 5), ("Dark Pact", 1), ("Improved Shadow Bolt", 5),
                            ("Bane", 5), ("Devastation", 5), ("Shadowburn", 1)],
                # Summon Imp/Voidwalker/Succubus/Felhunter, Inferno, Summon Felsteed.
                "extra_spells": [688, 697, 712, 691, 1122, 5784],
            },
        ],
        "bags": 4500,
        "consumables": {
            "all": [(8950, 20)],  # Homemade Cherry Pie
            "mana": [(8766, 20), (13443, 5)],  # Morning Glory Dew, Superior Mana Potion
            "health": [(13446, 5)],  # Major Healing Potion
            "warlock": [(6265, 10)],  # Soul Shard
            "rogue": [(5140, 10)],  # Flash Powder
            "hunter": [(11285, 1000)],  # Jagged Arrow
        },
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


def talent_spells(slot, level, talents):
    """Resolve (name, rank) picks to the rank's spell id, checking the tree's rules."""
    mask = CLASS[slot["cls"]][2]
    table = next(v for k, v in talents.items() if k & mask)
    spent_in_tab, total, ids = {}, 0, []
    for name, rank in slot["talents"]:
        tab, row, ranks = table[name]
        if rank > len(ranks):
            sys.exit(f"{slot['label']}: {name} has only {len(ranks)} ranks")
        if spent_in_tab.get(tab, 0) < 5 * row:
            sys.exit(f"{slot['label']}: {name} is tier {row + 1}, which needs {5 * row} points "
                     f"in its tree before it (have {spent_in_tab.get(tab, 0)}); reorder or add")
        spent_in_tab[tab] = spent_in_tab.get(tab, 0) + rank
        total += rank
        ids.append(ranks[rank - 1])
    if total != level - 9:
        sys.exit(f"{slot['label']}: {total} talent points, level {level} has {level - 9}")
    return ids, {table[n][2][0] for n, _ in slot["talents"]}, {v[2][0] for v in table.values()}


def trainer_spells(db, slot, level, picked_firsts, all_talent_firsts):
    cls_id, template, _ = CLASS[slot["cls"]]
    rows = sql(db, f"""
        SELECT s.EffectTriggerSpell1, COALESCE(c.first_spell, s.EffectTriggerSpell1)
        FROM npc_trainer_template t JOIN spell_template s ON s.Id = t.spell
        LEFT JOIN spell_chain c ON c.spell_id = s.EffectTriggerSpell1
        WHERE t.entry = {template} AND t.reqlevel <= {level} AND s.Effect1 = 36
        ORDER BY t.reqlevel, t.spell""")
    out = []
    for spell, first in rows:
        spell, first = int(spell), int(first)
        # Ranks of a talent ability the slot did not take are unlearnable noise.
        if first in all_talent_firsts and first not in picked_firsts:
            continue
        if spell not in out:
            out.append(spell)
    return out


OBTAINABLE = """
    entry IN (SELECT item FROM creature_loot_template UNION SELECT item FROM reference_loot_template
              UNION SELECT item FROM gameobject_loot_template UNION SELECT item FROM item_loot_template
              UNION SELECT item FROM npc_vendor UNION SELECT item FROM npc_vendor_template
              UNION SELECT RewChoiceItemId1 FROM quest_template UNION SELECT RewChoiceItemId2 FROM quest_template
              UNION SELECT RewChoiceItemId3 FROM quest_template UNION SELECT RewChoiceItemId4 FROM quest_template
              UNION SELECT RewChoiceItemId5 FROM quest_template UNION SELECT RewChoiceItemId6 FROM quest_template
              UNION SELECT RewItemId1 FROM quest_template UNION SELECT RewItemId2 FROM quest_template)"""

ITEM_COLS = ("entry, name, class, subclass, InventoryType, ItemLevel, RequiredLevel, armor, "
             "dmg_min1, dmg_max1, delay, maxcount, "
             + ", ".join(f"stat_type{i}, stat_value{i}" for i in range(1, 11)))


def candidates(db, level, cls, race, inv_types, item_class, subclasses, random_ok=False):
    cls_mask = 1 << (CLASS[cls][0] - 1)
    race_mask = 1 << (RACE[race] - 1)
    rows = sql(db, f"""
        SELECT {ITEM_COLS} FROM item_template
        WHERE InventoryType IN ({",".join(map(str, inv_types))})
          AND class = {item_class} AND subclass IN ({",".join(map(str, subclasses))})
          AND Quality BETWEEN 2 AND 3 AND RequiredLevel <= {level} AND RequiredLevel >= {level - 15}
          AND (RandomProperty = 0 OR {int(random_ok)}) AND RequiredSkill = 0 AND RequiredReputationFaction = 0
          AND requiredhonorrank = 0 AND requiredspell = 0
          AND (AllowableClass <= 0 OR AllowableClass & {cls_mask} OR AllowableClass = 32767)
          AND (AllowableRace <= 0 OR AllowableRace & {race_mask} OR AllowableRace = 255)
          AND name NOT REGEXP 'Test|Monster|Deprecated|DEPRECATED|NPC|Unused|QA|OLD|\\\\['
          -- Battleground and PvP-rank rewards: faction-locked in practice, whatever the row says.
          AND name NOT REGEXP "Sentinel's|Legionnaire's|Protector's|Outrider's|Advisor's|Scout's|Defiler's|Arathor|Highlander's|Warsong|Stormpike|Frostwolf|Knight-|Sergeant's|Marshal's|General's|Warlord's|Champion's|Lieutenant|Blood Guard|Commander's"
          AND {OBTAINABLE}""")
    return rows


def score(row, weights):
    (entry, name, iclass, sub, inv, ilvl, req, armor, dmin, dmax, delay, _maxcount, *stats) = row
    s = float(armor) * weights["armor"] + 0.05 * float(ilvl)
    for i in range(0, 20, 2):
        s += weights.get(int(stats[i]), 0) * float(stats[i + 1])
    if int(delay) > 0 and weights["dps"]:
        s += weights["dps"] * (float(dmin) + float(dmax)) / 2 / (int(delay) / 1000)
    return s


def pick(db, preset, slot):
    level, cls, race = preset["level"], slot["cls"], slot["race"]
    w = WEIGHTS[slot["weights"]]
    chosen = []
    taken = set()

    def best(inv_types, item_class, subclasses, label):
        # Fixed-stat items first; a random-suffix one (its stats rolled by `.additem`) only when
        # the slot has nothing else, and an empty slot when neither exists at this level.
        for random_ok in (False, True):
            rows = candidates(db, level, cls, race, inv_types, item_class, subclasses, random_ok)
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
    for label, types, iclass, subs in weapon_plan(cls, slot["weights"]):
        best(types, iclass, subs, label)
    return chosen


def toml_str(s):
    return '"' + s.replace("\\", "\\\\").replace('"', '\\"') + '"'


def item_names(db, ids):
    return dict(sql(db, "SELECT entry, name FROM item_template WHERE entry IN (%s)"
                    % ",".join(map(str, ids))))


def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--db", default="classicmangos")
    ap.add_argument("--dbc", type=Path, required=True, help="directory holding Talent.dbc and TalentTab.dbc")
    a = ap.parse_args()
    talents = talent_table(a.dbc, a.db)
    OUT.mkdir(parents=True, exist_ok=True)
    for p in PRESETS:
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
        for slot in p["slots"]:
            tal_ids, picked, all_firsts = talent_spells(slot, p["level"], talents)
            spells = trainer_spells(a.db, slot, p["level"], picked, all_firsts)
            gear = pick(a.db, p, slot)
            profs = sorted({PROFICIENCY[k] for *_, k in gear if k in PROFICIENCY})
            if slot["cls"] == "rogue":
                profs.append(674)  # Dual Wield
            # A hunter starts with a quiver in one bag slot, and a second quiver cannot be equipped.
            bags = [p["bags"]] * (3 if slot["cls"] == "hunter" else 4)
            cons = list(p["consumables"]["all"]) + list(p["consumables"]["health"])
            if slot["cls"] in ("priest", "mage", "warlock", "hunter"):
                cons += p["consumables"]["mana"]
            cons += p["consumables"].get(slot["cls"], [])
            names = item_names(a.db, [i for i, _ in cons] + bags)
            lines += [
                "",
                "[[slots]]",
                f"label = {toml_str(slot['label'])}",
                f"role = {toml_str(slot['role_label'])}",
                f"class = {CLASS[slot['cls']][0]}  # {slot['cls']}",
                f"race = {RACE[slot['race']]}  # {slot['race']}",
                f"gender = {slot['gender']}",
                f"spells = {sorted(set(spells + slot['extra_spells']))}",
                f"proficiencies = {sorted(set(profs))}",
                "talents = [" + ", ".join(
                    f"{i}" for i in tal_ids) + "]  # " + ", ".join(f"{n} {r}" for n, r in slot["talents"]),
                "bags = [" + ", ".join(str(b) for b in bags) + "]  # " + ", ".join(names[str(b)] for b in bags),
                "gear = [",
            ]
            for entry, name, label, _ in gear:
                lines.append(f"  {entry},  # {label}: {name}")
            lines.append("]")
            lines.append("consumables = [")
            for entry, count in cons:
                lines.append(f"  [{entry}, {count}],  # {names[str(entry)]}")
            lines.append("]")
        (OUT / f"{p['id']}.toml").write_text("\n".join(lines) + "\n")
        print(f"wrote {OUT / (p['id'] + '.toml')}")


if __name__ == "__main__":
    main()
