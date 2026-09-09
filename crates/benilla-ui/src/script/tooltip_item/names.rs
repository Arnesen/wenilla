//! The item tooltip's display vocabulary — the builder's own **key** tables (`INVTYPE_*`,
//! `ITEM_MOD_*`, `SPELL_SCHOOL%d_CAP`), the DBC-sourced subclass/class/race names, and the
//! byte-verified color constants the render law paints with. Pure data; the law itself is
//! [`super::render`], which resolves every key here against the player's own `GlobalStrings.lua`
//! (decision 2045).
//!
//! **Keys, not sentences, and the difference is not cosmetic.** `INVTYPE_SHIELD` and
//! `INVTYPE_WEAPONOFFHAND` both read "Off Hand" in enUS and are separately localizable
//! everywhere else; a table that stored the English could not tell them apart, and this one had
//! them collapsed into a single arm until 2045's sweep.
//!
//! **What stays a Rust literal here, and why it is not the same thing**: [`subclass_name`],
//! [`CLASS_NAMES`] and [`RACE_NAMES`] name rows the reference reads out of DBC records (the
//! `{class,subclass}` table `0xc0db90`, ChrClasses, ChrRaces), never out of `GlobalStrings.lua`.
//! Those belong to the DBC-feed question, not to this one.

/// The client's 7-entry quality→color table (wow-re RF-0055, VERIFIED at `0xc0d3c8` behind
/// `GetItemQualityColor 0x48dfb0`): Poor gray, Common white, Uncommon green, Rare blue, Epic
/// purple, Legendary orange, Artifact gold.
pub(super) const QUALITY_RGB: [[f32; 3]; 7] = [
    [0.616, 0.616, 0.616], // 0 Poor      9d9d9d
    [1.0, 1.0, 1.0],       // 1 Common    ffffff
    [0.118, 1.0, 0.0],     // 2 Uncommon  1eff00
    [0.0, 0.439, 0.867],   // 3 Rare      0070dd
    [0.639, 0.208, 0.933], // 4 Epic      a335ee
    [1.0, 0.502, 0.0],     // 5 Legendary ff8000
    [0.902, 0.8, 0.502],   // 6 Artifact  e6cc80
];

// The tooltip color constants — BYTE-VERIFIED (wow-re `ui/scratch/tooltip-content-law.md` §1's
// pointer table): white `0xc0cf60=ffffffff`, red `0xc0d390=ffff2020` (255,32,32), green
// `0xc0d3ac=ff00ff00`, gold `0xc0d3e8=ffffd200` (255,210,0), gray `0xc0d3c4=ff808080`.
pub(super) const WHITE: [f32; 4] = [1.0, 1.0, 1.0, 1.0];
pub(super) const GREEN: [f32; 4] = [0.0, 1.0, 0.0, 1.0];
pub(super) const RED: [f32; 4] = [1.0, 32.0 / 255.0, 32.0 / 255.0, 1.0];
/// The tooltip's OTHER red — `0xc0d398 = ffff0000`, a pure red distinct from the (255,32,32) the
/// requirement lines wear. Two lines use it, both in the enchant family: a **negative** enchant id
/// in slot 0/1, and ITEM_ENCHANT_DISCLAIMER (wow-re §1-ENCHANT §E3/§E4).
pub(super) const ENCHANT_RED: [f32; 4] = [1.0, 0.0, 0.0, 1.0];
pub(super) const GOLD: [f32; 4] = [1.0, 210.0 / 255.0, 0.0, 1.0];
pub(super) const GRAY: [f32; 4] = [128.0 / 255.0, 128.0 / 255.0, 128.0 / 255.0, 1.0];
/// The owned set member's pale cream — byte-read `0xc0d368 = ffffff97` (writer `0x529050`).
pub(super) const CREAM: [f32; 4] = [1.0, 1.0, 151.0 / 255.0, 1.0];

/// InventoryType → the slot line's GlobalString **key** — the builder's own 30-entry pointer
/// table at `0x83ddb0`, indexed by `[record+0x2c]` directly (`0x52c103: mov ecx,[ecx*4+0x83ddb0]`,
/// the read the law's §10 names), dumped entry by entry rather than matched by English.
///
/// **Index 0 and index 29 are the pre-seeded empty string `0x882748`** — a non-equip item and an
/// out-of-range type name nothing, which is the `None` here. 18 (bag) and 27 (quiver) *do* have
/// entries (`INVTYPE_BAG`/`INVTYPE_QUIVER`) but are unreachable: both are containers, and a
/// container takes the CONTAINER_SLOTS line above this call instead.
///
/// **Four of these keys ship no value, and that is the reference's behaviour, not a gap.**
/// `INVTYPE_AMMO` (24), `INVTYPE_THROWN` (25), `INVTYPE_RANGEDRIGHT` (26) and `INVTYPE_QUIVER`
/// (27) are in the exe's table but absent from `GlobalStrings.lua`, so `FrameScript_GetText`
/// hands the builder an empty left cell and an arrow shows only its type word. We had written
/// "Projectile", "Thrown" and "Ranged" into those arms — three sentences 1.12 never shows, and
/// exactly the invention class decision 2045 says a text-matching tripwire cannot catch.
pub(super) fn invtype_key(t: u32) -> Option<&'static str> {
    Some(match t {
        1 => "INVTYPE_HEAD",
        2 => "INVTYPE_NECK",
        3 => "INVTYPE_SHOULDER",
        4 => "INVTYPE_BODY",
        5 => "INVTYPE_CHEST",
        6 => "INVTYPE_WAIST",
        7 => "INVTYPE_LEGS",
        8 => "INVTYPE_FEET",
        9 => "INVTYPE_WRIST",
        10 => "INVTYPE_HAND",
        11 => "INVTYPE_FINGER",
        12 => "INVTYPE_TRINKET",
        13 => "INVTYPE_WEAPON",
        14 => "INVTYPE_SHIELD",
        15 => "INVTYPE_RANGED",
        16 => "INVTYPE_CLOAK",
        17 => "INVTYPE_2HWEAPON",
        19 => "INVTYPE_TABARD",
        20 => "INVTYPE_ROBE",
        21 => "INVTYPE_WEAPONMAINHAND",
        22 => "INVTYPE_WEAPONOFFHAND",
        23 => "INVTYPE_HOLDABLE",
        24 => "INVTYPE_AMMO",
        25 => "INVTYPE_THROWN",
        26 => "INVTYPE_RANGEDRIGHT",
        28 => "INVTYPE_RELIC",
        _ => return None,
    })
}

/// (class, subclass) → the slot line's right column (the client's ItemSubClass display names,
/// enUS). Absent pairs (consumables, trade goods, armor Miscellaneous…) show no right column.
pub(super) fn subclass_name(class: u32, sub: u32) -> Option<&'static str> {
    Some(match (class, sub) {
        (2, 0) | (2, 1) => "Axe",
        (2, 2) => "Bow",
        (2, 3) => "Gun",
        (2, 4) | (2, 5) => "Mace",
        (2, 6) => "Polearm",
        (2, 7) | (2, 8) => "Sword",
        (2, 10) => "Staff",
        (2, 13) => "Fist Weapon",
        (2, 15) => "Dagger",
        (2, 16) => "Thrown",
        (2, 17) => "Spear",
        (2, 18) => "Crossbow",
        (2, 19) => "Wand",
        (2, 20) => "Fishing Pole",
        (4, 1) => "Cloth",
        (4, 2) => "Leather",
        (4, 3) => "Mail",
        (4, 4) => "Plate",
        (4, 6) => "Shield",
        (6, 2) => "Arrow",
        (6, 3) => "Bullet",
        _ => return None,
    })
}

/// A damage/resistance school's name key — `SPELL_SCHOOL%d_CAP`, the one string the builder
/// composes at runtime rather than naming outright (`0x84e4cc`, pushed at `0x52c2a8` for the
/// damage line and `0x52c8d1` for the resistance line; law §11/§16).
///
/// The index is the school itself: `SPELL_SCHOOL0_CAP` is "Physical" and 1..6 are
/// Holy/Fire/Nature/Frost/Shadow/Arcane. **School 0 answers `None`** because the reference
/// doesn't name it either — a physical weapon takes the school-less `DAMAGE_TEMPLATE` arm rather
/// than printing the word.
pub(super) fn school_key(s: u32) -> Option<String> {
    (1..=6).contains(&s).then(|| format!("SPELL_SCHOOL{s}_CAP"))
}

/// Stat-mod type → its `ITEM_MOD_*` key. Not a *name*: the key resolves to the whole line
/// template, sign hole and all (`ITEM_MOD_AGILITY = "%c%d Agility"`), which is why 2 has no arm —
/// the builder's 8-way jump table `0x52e510` skips it (law §15, keys byte-read at
/// `0x52c6eb..0x52c777`).
pub(super) fn stat_key(t: u32) -> Option<&'static str> {
    Some(match t {
        0 => "ITEM_MOD_MANA",
        1 => "ITEM_MOD_HEALTH",
        3 => "ITEM_MOD_AGILITY",
        4 => "ITEM_MOD_STRENGTH",
        5 => "ITEM_MOD_INTELLECT",
        6 => "ITEM_MOD_SPIRIT",
        7 => "ITEM_MOD_STAMINA",
        _ => return None,
    })
}

/// Class id → display name (vanilla playable ids; the `ITEM_CLASSES_ALLOWED` list).
pub(super) const CLASS_NAMES: [(u32, &str); 9] = [
    (1, "Warrior"),
    (2, "Paladin"),
    (3, "Hunter"),
    (4, "Rogue"),
    (5, "Priest"),
    (7, "Shaman"),
    (8, "Mage"),
    (9, "Warlock"),
    (11, "Druid"),
];

/// Race id → display name (vanilla playable ids; the `ITEM_RACES_ALLOWED` list).
pub(super) const RACE_NAMES: [(u32, &str); 8] = [
    (1, "Human"),
    (2, "Orc"),
    (3, "Dwarf"),
    (4, "Night Elf"),
    (5, "Undead"),
    (6, "Tauren"),
    (7, "Gnome"),
    (8, "Troll"),
];

/// A playable-ids mask covering every listed id — a mask covering all of them shows no line.
pub(super) fn full_mask(ids: &[(u32, &str)]) -> i32 {
    ids.iter().fold(0i32, |m, &(id, _)| m | (1 << (id - 1)))
}

pub(super) fn quality_color(q: u32) -> [f32; 4] {
    let c = QUALITY_RGB.get(q as usize).unwrap_or(&QUALITY_RGB[1]);
    [c[0], c[1], c[2], 1.0]
}

pub(super) fn req_color(ok: bool) -> [f32; 4] {
    if ok {
        WHITE
    } else {
        RED
    }
}
