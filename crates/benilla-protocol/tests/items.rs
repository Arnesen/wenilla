//! Item wire tests (mirrors `src/messages/items.rs`): the query request/response roundtrip,
//! inventory-change-failure (level refusal vs. plain), the GM-command system chat shape, and the
//! full `SMSG_ITEM_QUERY_SINGLE_RESPONSE` field walk (hit + undiscovered miss). Split out of the
//! former `tests/messages.rs` — see `tests/common` for the shared fixtures and methodology note.

mod common;

use benilla_protocol::events::{decode, SessionEvent};
use benilla_protocol::messages;
use benilla_protocol::ServerPacket;
use common::hx;

#[test]
fn item_query_wire() {
    // CMSG_ITEM_QUERY_SINGLE: entry u32 + full item guid (vmangos QueryItem::ReadFromWorldPacket).
    assert_eq!(messages::item_query(117, 0), hx("750000000000000000000000"));

    // CMSG_USE_ITEM (vmangos UseItem::ReadFromWorldPacket): bagIndex, slot, spellSlot, then a
    // self-shaped target block (u16 mask 0). Backpack slot 1 = bag 255 + player-array slot 23.
    assert_eq!(
        messages::use_item(255, 23, 0, messages::UseItemTarget::SelfImplicit),
        hx("ff17000000")
    );

    // The KEY-IN-A-LOCK form (decision 0769): the same three bytes, then a SpellCastTargets with
    // mask TARGET_FLAG_GAMEOBJECT|TARGET_FLAG_LOCKED (0x0800|0x4000 = 0x4800) and the object's
    // PACKED guid. This is the packet the real client sends for a key (wow-re cursor-system.md
    // §8.4: sender 0x6e54f0's item arm -> 0x6e57d8 push 0xab) and the only one vmangos will open a
    // KEY-slot lock for (Spell::CanOpenLock requires m_CastItem, Spell.cpp:7892).
    //
    // Keyring slot 1 = bag 255 + player-array slot 81 (0x51). The guid 0xF110000C1F00A3B2 has two
    // zero bytes (indices 2 and 5), so it packs to mask 0xDB + the other six, low byte first.
    assert_eq!(
        messages::use_item(
            255,
            81,
            0,
            messages::UseItemTarget::Object(0xF110_000C_1F00_A3B2)
        ),
        hx("ff51000048dbb2a31f0c10f1"),
    );

    // The packed form really does drop zero bytes: guid 1 is mask 0x01 + a single byte, and the
    // spellSlot ordinal rides in the third byte.
    assert_eq!(
        messages::use_item(255, 81, 2, messages::UseItemTarget::Object(1)),
        hx("ff510200480101")
    );

    // The UNIT form (decision 0914): an item whose spell binds a unit the way a spell's does — a
    // bandage, a soulstone — writes TARGET_FLAG_UNIT (0x0002) + the packed guid, the SAME block a
    // CMSG_CAST_SPELL writes. In the real client one builder serves both opcodes: SendCast
    // 0x6e54f0 picks 0xab-vs-0x12e from its item discriminator and then writes ArmCast's block.
    assert_eq!(
        messages::use_item(255, 24, 0, messages::UseItemTarget::Unit(1)),
        hx("ff180002000101")
    );

    // The DEST form (decision 0914): the targeting-cursor commit for a THROWN item — dynamite, a
    // grenade, the Goblin Mortar. TARGET_FLAG_DEST_LOCATION (0x0040) + three f32 WoW coords, the
    // same tail cast_spell_at_dest writes. 1.0f32 = 0x3f800000, 2.0 = 0x40000000, 3.0 = 0x40400000.
    assert_eq!(
        messages::use_item(19, 3, 1, messages::UseItemTarget::Dest([1.0, 2.0, 3.0])),
        hx("13030140000000803f0000004000004040")
    );

    // CMSG_AUTOEQUIP_ITEM (vmangos AutoEquipItem::ReadFromWorldPacket): bagIndex, slot.
    assert_eq!(messages::auto_equip_item(255, 25), hx("ff19"));

    // CMSG_OPEN_ITEM (vmangos OpenItem::ReadFromWorldPacket, Server/Packets/Spell.cpp:19-23):
    // bagIndex, slot — and NOTHING else (no spell ordinal, no targets block; opening is not a
    // cast). The same two bytes as auto-equip under a different opcode: this is the fork a
    // clam/lockbox/gift takes instead of USE_ITEM, and the only one the server answers with
    // SendLoot on the item's own guid. Backpack slot 3 = bag 255 + player-array slot 25; an item
    // inside an equipped bag addresses that bag's own player-array slot (19..22) + inner slot.
    assert_eq!(messages::open_item(255, 25), hx("ff19"));
    assert_eq!(messages::open_item(19, 0), hx("1300"));

    // SMSG_INVENTORY_CHANGE_FAILURE (vmangos InventoryChangeFailure::AppendBodyTo): the level
    // refusal (reason 1) carries a u32 required level before the guids; others don't.
    let level_fail = messages::parse_server(
        messages::opcode::SMSG_INVENTORY_CHANGE_FAILURE,
        &hx("010a000000420000000000004000000000000000000000"),
    )
    .unwrap();
    match level_fail {
        ServerPacket::InventoryChangeFailure {
            reason,
            required_level,
            item_guid,
        } => {
            assert_eq!((reason, required_level), (1, Some(10)));
            assert_eq!(item_guid, 0x4000_0000_0000_0042);
        }
        other => panic!("level refusal, got {}", other.name()),
    }
    let plain_fail = messages::parse_server(
        messages::opcode::SMSG_INVENTORY_CHANGE_FAILURE,
        &hx("14420000000000004000000000000000000000"),
    )
    .unwrap();
    match plain_fail {
        ServerPacket::InventoryChangeFailure {
            reason,
            required_level,
            ..
        } => assert_eq!((reason, required_level), (0x14, None)),
        other => panic!("plain refusal, got {}", other.name()),
    }

    // SMSG_MESSAGECHAT, the system shape (type 0x0A: guid, len-prefixed text incl. NUL, tag) —
    // the GM-command feedback channel (live-verified: '.additem' answers exactly this shape).
    let sys = messages::parse_server(
        messages::opcode::SMSG_MESSAGECHAT,
        &hx("0a0000000000000000000000000600000068656c6c6f0000"),
    )
    .unwrap();
    match sys {
        ServerPacket::MessageChat(m) => {
            assert_eq!((m.chat_type, m.text.as_str()), (0x0a, "hello"));
            assert_eq!((m.sender_guid, m.sender_name, m.channel), (0, None, None));
        }
        other => panic!("system chat, got {}", other.name()),
    }
}

/// `SMSG_ITEM_QUERY_SINGLE_RESPONSE`, hit: the full 1.12.1 item template, byte-exact (VERIFIED
/// field order vmangos `HandleItemQuerySingleOpcode`, `ItemHandler.cpp:269-415`; every
/// `SUPPORTED_CLIENT_BUILD` conditional there is included for build 5875) — decision 0274 P1's
/// tooltip builder needs every line. A main-hand sword exercising every filtered/mirrored shape at
/// once: 2 of 5 damage blocks, 3 of 10 stats, 2 of 6 resistances, 2 of 5 spells (one ON_USE trigger
/// 0, one ON_EQUIP trigger 1), a nonempty description, and real requirement/durability values.
/// Every field carries a distinct value so a transposed or misaligned read fails the assert below.
#[test]
fn item_query_response_full_weapon_golden() {
    use benilla_protocol::messages::{ItemDamage, ItemInfo, ItemSpellEntry, ItemUseSpell};

    let mut body = 12_345u32.to_le_bytes().to_vec(); // entry
    body.extend_from_slice(&2u32.to_le_bytes()); // class: weapon
    body.extend_from_slice(&7u32.to_le_bytes()); // subclass: sword
    body.extend_from_slice(b"Verified Blade of Testing\0");
    body.extend_from_slice(&[0, 0, 0]); // name2..4: empty cstrings
    body.extend_from_slice(&6303u32.to_le_bytes()); // displayInfoID
    body.extend_from_slice(&4u32.to_le_bytes()); // quality: epic
    body.extend_from_slice(&66u32.to_le_bytes()); // flags
    body.extend_from_slice(&4200u32.to_le_bytes()); // buyPrice
    body.extend_from_slice(&1050u32.to_le_bytes()); // sellPrice
    body.extend_from_slice(&21u32.to_le_bytes()); // inventoryType: WEAPONMAINHAND
    body.extend_from_slice(&(-1i32).to_le_bytes()); // AllowableClass: all classes
    body.extend_from_slice(&3i32.to_le_bytes()); // AllowableRace
    body.extend_from_slice(&60u32.to_le_bytes()); // ItemLevel
    body.extend_from_slice(&55u32.to_le_bytes()); // RequiredLevel
    body.extend_from_slice(&164u32.to_le_bytes()); // RequiredSkill
    body.extend_from_slice(&225u32.to_le_bytes()); // RequiredSkillRank
    body.extend_from_slice(&5209u32.to_le_bytes()); // RequiredSpell
    body.extend_from_slice(&3u32.to_le_bytes()); // RequiredHonorRank
    body.extend_from_slice(&2u32.to_le_bytes()); // RequiredCityRank
    body.extend_from_slice(&69u32.to_le_bytes()); // RequiredReputationFaction
    body.extend_from_slice(&4u32.to_le_bytes()); // RequiredReputationRank
    body.extend_from_slice(&5u32.to_le_bytes()); // MaxCount
    body.extend_from_slice(&1u32.to_le_bytes()); // Stackable
    body.extend_from_slice(&0u32.to_le_bytes()); // ContainerSlots

    // 10x ItemStat { type, value } — 3 nonzero (one negative value proves signed reads), 7 empty
    // (dropped by the nonzero filter).
    let stat_slots: [(u32, i32); 10] = [
        (4, 15), // STRENGTH +15
        (7, 20), // STAMINA +20
        (5, -3), // INTELLECT -3
        (0, 0),
        (0, 0),
        (0, 0),
        (0, 0),
        (0, 0),
        (0, 0),
        (0, 0),
    ];
    for (stat_type, stat_value) in stat_slots {
        body.extend_from_slice(&stat_type.to_le_bytes());
        body.extend_from_slice(&stat_value.to_le_bytes());
    }

    // 5x Damage { min f32, max f32, type u32 } — 2 real blocks (physical + a secondary Fire line),
    // 3 empty (dropped by the `max > 0` filter). Block 0 also mirrors into dmg_min/dmg_max/dmg_type.
    let dmg_slots: [(f32, f32, u32); 5] = [
        (12.0, 22.0, 0), // physical
        (3.0, 7.0, 2),   // Fire
        (0.0, 0.0, 0),
        (0.0, 0.0, 0),
        (0.0, 0.0, 0),
    ];
    for (min, max, school) in dmg_slots {
        body.extend_from_slice(&min.to_le_bytes());
        body.extend_from_slice(&max.to_le_bytes());
        body.extend_from_slice(&school.to_le_bytes());
    }

    body.extend_from_slice(&15u32.to_le_bytes()); // Armor
                                                  // Resistances: Holy/Fire/Nature/Frost/Shadow/Arcane — 2 of 6 nonzero.
    for r in [0i32, 8, 0, 0, 12, 0] {
        body.extend_from_slice(&r.to_le_bytes());
    }
    body.extend_from_slice(&2800u32.to_le_bytes()); // Delay (ms)
    body.extend_from_slice(&2u32.to_le_bytes()); // AmmoType
    body.extend_from_slice(&1.5f32.to_le_bytes()); // RangedModRange

    // 5x Spell block { SpellId, SpellTrigger, SpellCharges, Cooldown, Category, CategoryCooldown }
    // — slot 0 ON_USE (a lone -1 use-cooldown next to a resolved category pair, the mixed shape
    // vmangos can send), slot 1 ON_EQUIP (a plain passive, no cooldown), slots 2-4 the server's own
    // unresolved-slot sentinel (0,0,0,-1,0,-1).
    let spell_slots: [(u32, u32, i32, i32, u32, i32); 5] = [
        (17_251, 0, 0, -1, 4, 60_000),
        (671, 1, 0, 0, 0, 0),
        (0, 0, 0, -1, 0, -1),
        (0, 0, 0, -1, 0, -1),
        (0, 0, 0, -1, 0, -1),
    ];
    for (spell_id, trigger, charges, cooldown_ms, category, category_cooldown_ms) in spell_slots {
        body.extend_from_slice(&spell_id.to_le_bytes());
        body.extend_from_slice(&trigger.to_le_bytes());
        body.extend_from_slice(&charges.to_le_bytes());
        body.extend_from_slice(&cooldown_ms.to_le_bytes());
        body.extend_from_slice(&category.to_le_bytes());
        body.extend_from_slice(&category_cooldown_ms.to_le_bytes());
    }

    body.extend_from_slice(&2u32.to_le_bytes()); // Bonding: BIND_WHEN_EQUIPPED
    body.extend_from_slice(b"A blade of pure verification.\0"); // Description
    body.extend_from_slice(&333u32.to_le_bytes()); // PageText
    body.extend_from_slice(&7u32.to_le_bytes()); // LanguageID
    body.extend_from_slice(&2u32.to_le_bytes()); // PageMaterial
    body.extend_from_slice(&444u32.to_le_bytes()); // StartQuest
    body.extend_from_slice(&12u32.to_le_bytes()); // LockID
    body.extend_from_slice(&3u32.to_le_bytes()); // Material
    body.extend_from_slice(&3u32.to_le_bytes()); // Sheath: one-handed
    body.extend_from_slice(&87u32.to_le_bytes()); // RandomProperty
    body.extend_from_slice(&18u32.to_le_bytes()); // Block
    body.extend_from_slice(&25u32.to_le_bytes()); // ItemSet
    body.extend_from_slice(&120u32.to_le_bytes()); // MaxDurability
    body.extend_from_slice(&8u32.to_le_bytes()); // Area
    body.extend_from_slice(&1u32.to_le_bytes()); // Map
    body.extend_from_slice(&4u32.to_le_bytes()); // BagFamily

    match messages::parse_server(messages::opcode::SMSG_ITEM_QUERY_SINGLE_RESPONSE, &body).unwrap()
    {
        ServerPacket::ItemQueryResponse { entry, info } => {
            assert_eq!(entry, 12_345);
            assert_eq!(
                info,
                Some(Box::new(ItemInfo {
                    class: 2,
                    subclass: 7,
                    name: "Verified Blade of Testing".into(),
                    display_info_id: 6303,
                    quality: 4,
                    flags: 66,
                    buy_price: 4200,
                    sell_price: 1050,
                    inventory_type: 21,
                    allowable_class: -1,
                    allowable_race: 3,
                    item_level: 60,
                    required_level: 55,
                    required_skill: 164,
                    required_skill_rank: 225,
                    required_spell: 5209,
                    required_honor_rank: 3,
                    required_city_rank: 2,
                    required_rep_faction: 69,
                    required_rep_rank: 4,
                    max_count: 5,
                    stackable: 1,
                    container_slots: 0,
                    stats: vec![(4, 15), (7, 20), (5, -3)],
                    damages: vec![
                        ItemDamage {
                            min: 12.0,
                            max: 22.0,
                            school: 0
                        },
                        ItemDamage {
                            min: 3.0,
                            max: 7.0,
                            school: 2
                        },
                    ],
                    dmg_min: 12.0,
                    dmg_max: 22.0,
                    dmg_type: 0,
                    armor: 15,
                    resistances: [0, 8, 0, 0, 12, 0],
                    delay_ms: 2800,
                    ammo_type: 2,
                    ranged_mod_range: 1.5,
                    spell_charges_0: 0,
                    spells: vec![
                        ItemSpellEntry {
                            index: 0,
                            spell_id: 17_251,
                            trigger: 0,
                            charges: 0,
                            cooldown_ms: -1,
                            category: 4,
                            category_cooldown_ms: 60_000,
                        },
                        ItemSpellEntry {
                            index: 1,
                            spell_id: 671,
                            trigger: 1,
                            charges: 0,
                            cooldown_ms: 0,
                            category: 0,
                            category_cooldown_ms: 0,
                        },
                    ],
                    use_spell: Some(ItemUseSpell {
                        spell_id: 17_251,
                        cooldown_ms: -1,
                        category: 4,
                        category_cooldown_ms: 60_000,
                    }),
                    bonding: 2,
                    description: "A blade of pure verification.".into(),
                    page_text: 333,
                    language_id: 7,
                    page_material: 2,
                    start_quest: 444,
                    lock_id: 12,
                    material: 3,
                    sheath: 3,
                    random_property: 87,
                    block: 18,
                    item_set: 25,
                    max_durability: 120,
                    area: 8,
                    map: 1,
                    bag_family: 4,
                }))
            );
        }
        other => panic!("item query hit, got {}", other.name()),
    }
}

/// `SMSG_ITEM_QUERY_SINGLE_RESPONSE`, hit: a minimal consumable (a potion). The server zeroes
/// `subclass` for every `ITEM_CLASS_CONSUMABLE` row regardless of the DB value (VERIFIED vmangos
/// `ItemHandler.cpp:300`) — that's a server-side rule, so this fixture just carries the
/// already-zeroed wire byte; the parser itself has no class-conditional logic. What this exercises
/// instead is the "everything empty" path the weapon golden above only partly covers: `stats`,
/// `damages`, and 4 of 5 `spells` slots all read as real zeroed wire data that must land as **empty
/// `Vec`s**, not get pushed as garbage entries — while the one real ON_USE slot still resolves both
/// into `spells` and into the legacy `use_spell` accessor.
#[test]
fn item_query_response_consumable_all_zero_slots_are_empty_not_garbage() {
    use benilla_protocol::messages::{ItemInfo, ItemSpellEntry, ItemUseSpell};

    let mut body = 117u32.to_le_bytes().to_vec(); // entry
    body.extend_from_slice(&0u32.to_le_bytes()); // class: consumable
    body.extend_from_slice(&0u32.to_le_bytes()); // subclass: server-zeroed for consumables
    body.extend_from_slice(b"Minor Healing Potion\0");
    body.extend_from_slice(&[0, 0, 0]); // name2..4: empty cstrings
    body.extend_from_slice(&1712u32.to_le_bytes()); // displayInfoID
    body.extend_from_slice(&1u32.to_le_bytes()); // quality: common
    body.extend_from_slice(&0u32.to_le_bytes()); // flags
    body.extend_from_slice(&40u32.to_le_bytes()); // buyPrice
    body.extend_from_slice(&10u32.to_le_bytes()); // sellPrice
    body.extend_from_slice(&0u32.to_le_bytes()); // inventoryType: none
    body.extend_from_slice(&(-1i32).to_le_bytes()); // AllowableClass: all classes
    body.extend_from_slice(&(-1i32).to_le_bytes()); // AllowableRace: all races
    body.extend_from_slice(&1u32.to_le_bytes()); // ItemLevel
    for _ in 0..6 {
        body.extend_from_slice(&0u32.to_le_bytes()); // RequiredLevel .. RequiredCityRank
    }
    body.extend_from_slice(&0u32.to_le_bytes()); // RequiredReputationFaction
    body.extend_from_slice(&0u32.to_le_bytes()); // RequiredReputationRank
    body.extend_from_slice(&20u32.to_le_bytes()); // MaxCount
    body.extend_from_slice(&20u32.to_le_bytes()); // Stackable
    body.extend_from_slice(&0u32.to_le_bytes()); // ContainerSlots
    for _ in 0..20 {
        body.extend_from_slice(&0u32.to_le_bytes()); // 10x ItemStat { type, value }: all empty
    }
    for _ in 0..5 {
        body.extend_from_slice(&0f32.to_le_bytes());
        body.extend_from_slice(&0f32.to_le_bytes());
        body.extend_from_slice(&0u32.to_le_bytes()); // 5x Damage: all empty
    }
    body.extend_from_slice(&0u32.to_le_bytes()); // Armor
    for _ in 0..6 {
        body.extend_from_slice(&0u32.to_le_bytes()); // resistances: all empty
    }
    body.extend_from_slice(&0u32.to_le_bytes()); // Delay
    body.extend_from_slice(&0u32.to_le_bytes()); // AmmoType
    body.extend_from_slice(&0f32.to_le_bytes()); // RangedModRange
                                                 // Spell slot 0: the ON_USE heal; slots 1-4 the server's unresolved-slot sentinel.
    body.extend_from_slice(&2024u32.to_le_bytes()); // SpellId
    body.extend_from_slice(&0u32.to_le_bytes()); // SpellTrigger: ON_USE
    body.extend_from_slice(&0u32.to_le_bytes()); // SpellCharges
    body.extend_from_slice(&(-1i32).to_le_bytes()); // Cooldown: the spell's own
    body.extend_from_slice(&0u32.to_le_bytes()); // Category
    body.extend_from_slice(&(-1i32).to_le_bytes()); // CategoryCooldown: the spell's own
    for _ in 0..4 {
        body.extend_from_slice(&0u32.to_le_bytes()); // SpellId
        body.extend_from_slice(&0u32.to_le_bytes()); // SpellTrigger
        body.extend_from_slice(&0u32.to_le_bytes()); // SpellCharges
        body.extend_from_slice(&(-1i32).to_le_bytes()); // Cooldown
        body.extend_from_slice(&0u32.to_le_bytes()); // Category
        body.extend_from_slice(&(-1i32).to_le_bytes()); // CategoryCooldown
    }
    body.extend_from_slice(&0u32.to_le_bytes()); // Bonding
    body.push(0); // Description: empty cstring
    for _ in 0..6 {
        body.extend_from_slice(&0u32.to_le_bytes()); // PageText .. Material
    }
    body.extend_from_slice(&0u32.to_le_bytes()); // Sheath
    body.extend_from_slice(&0u32.to_le_bytes()); // RandomProperty
    body.extend_from_slice(&0u32.to_le_bytes()); // Block
    body.extend_from_slice(&0u32.to_le_bytes()); // ItemSet
    body.extend_from_slice(&0u32.to_le_bytes()); // MaxDurability: consumables have none
    body.extend_from_slice(&0u32.to_le_bytes()); // Area
    body.extend_from_slice(&0u32.to_le_bytes()); // Map
    body.extend_from_slice(&0u32.to_le_bytes()); // BagFamily

    match messages::parse_server(messages::opcode::SMSG_ITEM_QUERY_SINGLE_RESPONSE, &body).unwrap()
    {
        ServerPacket::ItemQueryResponse { entry, info } => {
            assert_eq!(entry, 117);
            assert_eq!(
                info,
                Some(Box::new(ItemInfo {
                    class: 0,
                    subclass: 0,
                    name: "Minor Healing Potion".into(),
                    display_info_id: 1712,
                    quality: 1,
                    flags: 0,
                    buy_price: 40,
                    sell_price: 10,
                    inventory_type: 0,
                    allowable_class: -1,
                    allowable_race: -1,
                    item_level: 1,
                    required_level: 0,
                    required_skill: 0,
                    required_skill_rank: 0,
                    required_spell: 0,
                    required_honor_rank: 0,
                    required_city_rank: 0,
                    required_rep_faction: 0,
                    required_rep_rank: 0,
                    max_count: 20,
                    stackable: 20,
                    container_slots: 0,
                    stats: Vec::new(),
                    damages: Vec::new(),
                    dmg_min: 0.0,
                    dmg_max: 0.0,
                    dmg_type: 0,
                    armor: 0,
                    resistances: [0; 6],
                    delay_ms: 0,
                    ammo_type: 0,
                    ranged_mod_range: 0.0,
                    spell_charges_0: 0,
                    spells: vec![ItemSpellEntry {
                        index: 0,
                        spell_id: 2024,
                        trigger: 0,
                        charges: 0,
                        cooldown_ms: -1,
                        category: 0,
                        category_cooldown_ms: -1,
                    }],
                    use_spell: Some(ItemUseSpell {
                        spell_id: 2024,
                        cooldown_ms: -1,
                        category: 0,
                        category_cooldown_ms: -1,
                    }),
                    bonding: 0,
                    description: String::new(),
                    page_text: 0,
                    language_id: 0,
                    page_material: 0,
                    start_quest: 0,
                    lock_id: 0,
                    material: 0,
                    sheath: 0,
                    random_property: 0,
                    block: 0,
                    item_set: 0,
                    max_durability: 0,
                    area: 0,
                    map: 0,
                    bag_family: 0,
                }))
            );
        }
        other => panic!("item query hit, got {}", other.name()),
    }
}

/// `SMSG_ITEM_QUERY_SINGLE_RESPONSE`, miss: the lone entry with the top bit set (vmangos
/// undiscovered/unknown-entry branch) — the same shape as the creature miss.
#[test]
fn item_query_response_miss() {
    let fail = messages::parse_server(
        messages::opcode::SMSG_ITEM_QUERY_SINGLE_RESPONSE,
        &hx("d2040080"),
    )
    .unwrap();
    match decode(fail).pop().unwrap() {
        SessionEvent::ItemTemplate { entry, info } => {
            assert_eq!((entry, info), (1234, None));
        }
        _ => panic!("item query miss event"),
    }
}
