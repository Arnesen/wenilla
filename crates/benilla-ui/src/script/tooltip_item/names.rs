//! The item tooltip's display vocabulary — the extracted enUS name tables (`INVTYPE_*`,
//! subclass, damage school, `ITEM_MOD_*`, class/race lists) and the byte-verified color
//! constants the render law paints with. Pure data; the law itself is [`super::render`].

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

/// InventoryType → the slot line (the client's `INVTYPE_*` GlobalStrings, enUS). 0 (non-equip),
/// 18 (bag), 27 (quiver) draw no slot line.
pub(super) fn invtype_name(t: u32) -> Option<&'static str> {
    Some(match t {
        1 => "Head",
        2 => "Neck",
        3 => "Shoulder",
        4 => "Shirt",
        5 | 20 => "Chest",
        6 => "Waist",
        7 => "Legs",
        8 => "Feet",
        9 => "Wrist",
        10 => "Hands",
        11 => "Finger",
        12 => "Trinket",
        13 => "One-Hand",
        14 | 22 => "Off Hand",
        15 | 26 => "Ranged",
        16 => "Back",
        17 => "Two-Hand",
        19 => "Tabard",
        21 => "Main Hand",
        23 => "Held In Off-hand",
        24 => "Projectile",
        25 => "Thrown",
        28 => "Relic",
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

/// Damage school suffix for a non-physical damage line ("5 - 9 Fire Damage").
pub(super) fn school_name(s: u32) -> Option<&'static str> {
    Some(match s {
        1 => "Holy",
        2 => "Fire",
        3 => "Nature",
        4 => "Frost",
        5 => "Shadow",
        6 => "Arcane",
        _ => return None,
    })
}

/// Stat-mod type → its `ITEM_MOD_*` display name (vanilla types 0..7; 2 is unused).
pub(super) fn stat_name(t: u32) -> Option<&'static str> {
    Some(match t {
        0 => "Mana",
        1 => "Health",
        3 => "Agility",
        4 => "Strength",
        5 => "Intellect",
        6 => "Spirit",
        7 => "Stamina",
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
