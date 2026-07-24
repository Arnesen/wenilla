use std::collections::BTreeMap;
use std::io::{self, Read};

use crate::wire::{read_u32_le, read_u8, Vector3d};

use super::movement::ObjectType;

// ObjectFields descriptor-field indices (build 5875), shared across object types.
const FIELD_OBJECT_TYPE: u16 = 2;
const FIELD_OBJECT_SCALE_X: u16 = 4;
const FIELD_GAMEOBJECT_DISPLAYID: u16 = 8;
const FIELD_GAMEOBJECT_FLAGS: u16 = 9;
// `GAMEOBJECT_ROTATION` = OBJECT_END(6) + 0x4 .. +0x7 (vmangos `UpdateFields_1_12_1.h`): the
// spawn's local-rotation quaternion (x, y, z, w), 4 floats. The type-11 transport evaluator
// rotates its keyframe offsets by this (decision 0438 phase 3's second consumer).
const FIELD_GAMEOBJECT_ROTATION: u16 = 10;
const FIELD_GAMEOBJECT_STATE: u16 = 14;
const FIELD_GAMEOBJECT_POS_X: u16 = 15;
const FIELD_GAMEOBJECT_POS_Y: u16 = 16;
const FIELD_GAMEOBJECT_POS_Z: u16 = 17;
const FIELD_GAMEOBJECT_FACING: u16 = 18;
// `OBJECT_END + 0x34` bytes ⇒ `OBJECT_END(6) + field 13` = 19 (wow-re cursor-system §4a byte offset;
// vmangos' `UpdateFields_1_5_1.h` inserts a stray `GAMEOBJECT_TIMESTAMP` that would shift this to 20,
// but that header is not the 5875 layout — STATE@14 / TYPE_ID@21 both work against this same server).
const FIELD_GAMEOBJECT_DYN_FLAGS: u16 = 19;
const FIELD_GAMEOBJECT_TYPE_ID: u16 = 21;
// GAMEOBJECT_LEVEL = OBJECT_END(6) + 0x10 (vmangos UpdateFields_1_12_1.h:317).
const FIELD_GAMEOBJECT_LEVEL: u16 = 22;
// UNIT descriptor fields (verified against wow-5875-re object-layer node, build 5875; FLAGS /
// COMBATREACH / DYNAMIC_FLAGS / NPC_FLAGS additionally cross-checked against the cursor RE's client
// offsets `+0xa0`/`+0x1f0`/`+0x224`/`+0x234` and vmangos `UpdateFields_1_12_1.h`).
/// `UNIT_FIELD_TARGET` (idx 16, a 2-field guid) — who the unit is attacking/interacting with. The
/// real client turns an idle unit's body to *face* this target (there is no wire facing for a
/// stationary re-face — vmangos `SetInFront` is server-only); benilla mirrors that (`face_target`).
const FIELD_UNIT_TARGET: u16 = 16;
/// `UNIT_FIELD_SUMMONEDBY` (idx 12, `OBJECT_END(6) + 0x6`, a 2-field guid — vmangos
/// `UpdateFields_1_12_1.h`) — the summoner of a pet/guardian/totem. With CREATEDBY below, the
/// combat-text source-ownership classifier's "owned by me" test (wow-re `0x5efea0`).
const FIELD_UNIT_SUMMONEDBY: u16 = 12;
/// `UNIT_FIELD_CREATEDBY` (idx 14, a 2-field guid) — the creator (totems, summoned objects).
const FIELD_UNIT_CREATEDBY: u16 = 14;
/// `UNIT_FIELD_CHANNEL_OBJECT` (idx 20, `OBJECT_END(6) + 0xE`, a 2-field guid, PUBLIC — VERIFIED
/// vmangos `UpdateFields_1_12_1.h:48`) — the object a channeled spell is aimed at (may be the
/// caster itself). Public, so an **observer** renders another unit's channel loop from this field
/// pair, not from the self-only `MSG_CHANNEL_START`/`UPDATE` UI packets (decision 0099 phase 1).
const FIELD_UNIT_CHANNEL_OBJECT: u16 = 20;
const FIELD_UNIT_HEALTH: u16 = 22;
/// `UNIT_FIELD_POWER1` — first of 5 consecutive power slots (mana, rage, focus, energy, happiness);
/// `MAXPOWER1..5` sit symmetrically after `MAXHEALTH` (vmangos `UpdateFields_1_12_1.h`).
const FIELD_UNIT_POWER1: u16 = 23;
const FIELD_UNIT_MAXHEALTH: u16 = 28;
const FIELD_UNIT_MAXPOWER1: u16 = 29;
const FIELD_UNIT_LEVEL: u16 = 34;
const FIELD_UNIT_FACTIONTEMPLATE: u16 = 35;
const FIELD_UNIT_BYTES_0: u16 = 36;
const FIELD_UNIT_BYTES_1: u16 = 138;
const FIELD_UNIT_FLAGS: u16 = 46;
// The aura block — four parallel arrays, all PUBLIC, so every unit we can see streams its full set
// (vmangos `UpdateFields_1_12_1.h:67-70`, `OBJECT_END(6) + 0x29/0x59/0x5F/0x6B`). wow-re places the
// same arrays in the client at `descriptor + 0xa4` / `+0x164` (= `0x29`/`0x59` × 4 — the same field
// indices, derived independently from the binary: `object-layer/scratch/w2d1.md`, VERIFIED).
// Duration and caster are NOT here and are not in any other field: duration reaches only the aura's
// own target, over `SMSG_UPDATE_AURA_DURATION`; a caster guid never reaches anyone (decision 0255).
const FIELD_UNIT_AURA: u16 = 47;
/// Nibble-packed, 8 slots per `u32` (`SpellAuraHolder::SetAuraFlag`, `SpellAuras.cpp:7456-7462`).
const FIELD_UNIT_AURAFLAGS: u16 = 95;
/// Byte-packed, 4 slots per `u32` — the *caster's level* (`SetAuraLevel`, `SpellAuras.cpp:7484`).
const FIELD_UNIT_AURALEVELS: u16 = 101;
/// Byte-packed, 4 slots per `u32`, holding `stack - 1` ("field expect count-1 for proper amount
/// show" — `UpdateAuraApplication`, `SpellAuras.cpp:7500-7507`).
const FIELD_UNIT_AURAAPPLICATIONS: u16 = 113;
/// `UNIT_FIELD_AURASTATE` (`OBJECT_END(6) + 0x77` = 125 — vmangos `UpdateFields_1_12_1.h`) — the
/// aura-state bit set the usable walk's CasterAuraState/TargetAuraState legs test with
/// `1 << (state-1)` (wow-re §2a; the client reads its parsed-descriptor copy at
/// `[unit+0x110]+0x1dc`, the update-field identity corroborated by the bit-test semantics +
/// vmangos `Unit::ModifyAuraState`).
const FIELD_UNIT_AURASTATE: u16 = 125;
/// `UNIT_FIELD_BOUNDINGRADIUS` (`OBJECT_END(6) + 0x7B` = 129, right before COMBATREACH — vmangos
/// `UpdateFields_1_12_1.h`) — the unit's horizontal bounding radius (yd).
const FIELD_UNIT_BOUNDINGRADIUS: u16 = 129;
const FIELD_UNIT_COMBATREACH: u16 = 130;
const FIELD_UNIT_BASE_MANA: u16 = 162;
const FIELD_UNIT_DISPLAYID: u16 = 131;
/// `UNIT_FIELD_MOUNTDISPLAYID` (`OBJECT_END(6) + 0x7F` = 133, PUBLIC — vmangos
/// `UpdateFields_1_12_1.h:78`; byte-verified client-side at `[unit+0x110]+0x1fc` = index 0x85,
/// wow-re `rf39-field-index-map.md`/`d1.md`, HARD). A raw `CreatureDisplayInfo` id: the mount the
/// unit is riding, `0`/absent = not mounted. The server writes it on aura-78 apply and zeroes it
/// on every dismount path (`Unit::Mount`/`Unmount`) — this field, not the aura, IS the mounted
/// state (`Unit::IsMounted` reads it back; no flag or bytes field accompanies it). Decision 0441.
const FIELD_UNIT_MOUNTDISPLAYID: u16 = 133;
const FIELD_UNIT_DYNAMIC_FLAGS: u16 = 143;
/// `UNIT_CHANNEL_SPELL` (idx 144, `OBJECT_END(6) + 0x8A`, PUBLIC — VERIFIED vmangos
/// `UpdateFields_1_12_1.h:89`) — the spell id a unit is channeling (`0` = not channeling).
const FIELD_UNIT_CHANNEL_SPELL: u16 = 144;
const FIELD_UNIT_NPC_FLAGS: u16 = 147;
/// `UNIT_NPC_EMOTESTATE` (`OBJECT_END + 0x8E`; PUBLIC — VERIFIED vmangos `UpdateFields_1_12_1.h:93`)
/// — the unit's looping **state emote** as an `Emotes.dbc` id (dance, NPC cooking/working flavor).
/// For a player it's the server echo of a state-class text emote (`HandleEmote`,
/// `ChatHandler.cpp:738`); `0` = none.
const FIELD_UNIT_NPC_EMOTESTATE: u16 = 148;
/// `UNIT_VIRTUAL_ITEM_SLOT_DISPLAY` (`OBJECT_END(6) + 0x1F`) — 3 consecutive dwords, one per
/// `WeaponAttackType` slot (0 mainhand / 1 offhand / 2 ranged): a creature's virtual-weapon
/// **display id**, written straight from `creature_equip_template`'s item's `DisplayInfoID`
/// (VERIFIED vmangos `Creature.cpp` `Creature::SetVirtualItem`, `Creature.cpp:4158-4181`) — there's
/// no item guid behind it, purely a render hint the client never resolves through the item system.
const FIELD_UNIT_VIRTUAL_ITEM_SLOT_DISPLAY: u16 = 37;
/// `UNIT_VIRTUAL_ITEM_INFO` (`OBJECT_END + 0x22`) — 6 dwords, 2 per slot. Dword 0 of a slot's pair
/// packs `(class, subclass, material, inventoryType)` bytes (VERIFIED vmangos
/// `CreatureDefines.h:621-627` `VirtualItemInfoByteOffset`); dword 1's byte 0 is the slot's Sheath
/// (`CreatureDefines.h:628` `VIRTUAL_ITEM_INFO_1_OFFSET_SHEATH`), the rest of that dword unused.
const FIELD_UNIT_VIRTUAL_ITEM_INFO: u16 = 40;
/// `UNIT_FIELD_BYTES_2` (`OBJECT_END + 0x9E`; PUBLIC) — byte 0 is the sheath state (VERIFIED vmangos
/// `UnitDefines.h:93` `UNIT_BYTES_2_OFFSET_SHEATH_STATE = 0`, written by `Unit::SetSheath`; for a
/// player it's purely client-volunteered via `CMSG_SETSHEATHED` — the server has no other way to
/// know a weapon is drawn). **Not** [`FIELD_PLAYER_BYTES_2`] (194, facialHair) — an unrelated,
/// differently-indexed field; this UNIT-block index was cross-checked clean of the a783a5b
/// PLAYER-block hex-comment drift (vmangos's own enum arithmetic and its stale hex comment agree
/// here: `UNIT_END + 0x9E` both ways).
const FIELD_UNIT_BYTES_2: u16 = 164;
// UNIT combat/stat block (decision 0208 phase 1a — the paper-doll stats pane). VERIFIED against
// vmangos `UpdateFields_1_12_1.h` **enum arithmetic** this session (`OBJECT_END` = 6; unlike the
// CONTAINER-onward block below, this block's own `//0x..` hex comments agree with the arithmetic —
// no staleness here). Every index below chain-locks to the file's already-tested
// `FIELD_PLAYER_INV_SLOT_HEAD` (486) anchor through `UNIT_END` (188) — see the tests.
const FIELD_UNIT_BASEATTACKTIME: u16 = 126; // OBJECT_END+0x78; 2 slots [main, offhand], ms, INT
const FIELD_UNIT_RANGEDATTACKTIME: u16 = 128; // +0x7A, INT
const FIELD_UNIT_MINDAMAGE: u16 = 134; // +0x80, FLOAT
const FIELD_UNIT_MAXDAMAGE: u16 = 135; // +0x81, FLOAT
const FIELD_UNIT_MINOFFHANDDAMAGE: u16 = 136; // +0x82, FLOAT
const FIELD_UNIT_MAXOFFHANDDAMAGE: u16 = 137; // +0x83, FLOAT
const FIELD_UNIT_STAT0: u16 = 150; // +0x90 ×5, INT (`Unit::SetStat`→`SetStatInt32Value`)
const FIELD_UNIT_RESISTANCES: u16 = 155; // +0x95 ×7, INT; [0] = armor (`Unit::SetArmor` writes +0)
const FIELD_UNIT_ATTACK_POWER: u16 = 165; // +0x9F, INT
/// `UNIT_FIELD_ATTACK_POWER_MODS` (166, +0xA0, `TWO_SHORT`) — `(positiveMods, negativeMods)` as
/// **signed 16-bit halves**, lo = positive, hi = negative (VERIFIED vmangos
/// `StatSystem.cpp:335-336`, `Player::UpdateAttackPowerAndDamage`: `SetInt16Value(index_mod, 0,
/// (int16)attackPowerModPositive)` / `SetInt16Value(index_mod, 1, (int16)attackPowerModNegative)`;
/// `Object::SetUInt16Value`'s `offset*16` shift confirms offset 0 = the low half). The negative
/// half is stored **already negative-or-zero** — `Unit::HandleAttackPowerModifier`'s
/// `AP_MOD_NEGATIVE_FLAT` branch accumulates `apply ? amount : -amount`, and its caller
/// (`SpellAuras.cpp:5116`) only routes there when `!IsPositive()`, i.e. `amount` is already ≤ 0.
const FIELD_UNIT_ATTACK_POWER_MODS: u16 = 166;
const FIELD_UNIT_ATTACK_POWER_MULTIPLIER: u16 = 167; // +0xA1, FLOAT: (multiplier − 1.0), StatSystem.cpp:339-340
const FIELD_UNIT_RANGED_ATTACK_POWER: u16 = 168; // +0xA2, INT
const FIELD_UNIT_RANGED_ATTACK_POWER_MODS: u16 = 169; // +0xA3, same TWO_SHORT packing as melee above
const FIELD_UNIT_RANGED_ATTACK_POWER_MULTIPLIER: u16 = 170; // +0xA4, FLOAT
const FIELD_UNIT_MINRANGEDDAMAGE: u16 = 171; // +0xA5, FLOAT
const FIELD_UNIT_MAXRANGEDDAMAGE: u16 = 172; // +0xA6, FLOAT

/// Aura slots per unit (vmangos `MAX_AURAS`, `SpellAuraDefines.h:25`).
pub const UNIT_AURA_SLOTS: u8 = 48;
/// Slots `0..UNIT_AURA_POSITIVE_SLOTS` hold **buffs**; the rest hold **debuffs** (vmangos
/// `MAX_POSITIVE_AURAS`, `SpellAuraDefines.h:26`). The server picks the lowest free slot in the
/// matching half (`SpellAuraHolder::_AddSpellAuraHolder`, `SpellAuras.cpp:6715-6736`), and assigns
/// no slot at all to an aura that fails `IsNeedVisibleSlot` — so **passives never reach the wire**
/// and a client renders the array as-is, with no filtering of its own.
pub const UNIT_AURA_POSITIVE_SLOTS: u8 = 32;

/// `AFLAG_CANCELABLE` (vmangos `SpellAuraDefines.h:31`) — the right-click gate. The server sets it
/// iff the aura is positive and its spell lacks `SPELL_ATTR_NO_AURA_CANCEL` (`SpellAuras.cpp:7467`),
/// which is exactly the condition `CMSG_CANCEL_AURA`'s handler re-checks, so honouring this bit is
/// enough — no `Spell.dbc` lookup needed to know whether a buff can be clicked off.
pub const AURA_FLAG_CANCELABLE: u8 = 0x01;
/// `AFLAG_EFF_INDEX_0|1|2` — which of the spell's three effects this holder actually applied. An
/// occupied slot always has at least one set, which is why the real client uses `flags & 0x0E` as
/// its liveness test (wow-re `object-layer/scratch/w2d1.md`), and why [`ObjectFields::unit_aura`]
/// gates on it too: a slot the server clears can keep a **stale spell id** after its flags nibble
/// goes to zero, so `flags & 0x0E` — not "has a spell id" — is what tells a live aura from a husk.
/// Gating on the id alone surfaces the husk as a phantom buff the real client never shows.
pub const AURA_FLAG_EFF_INDEX_MASK: u8 = 0x0E;

/// One **occupied** aura slot, decoded from the four parallel `UNIT_FIELD_AURA*` arrays. Everything
/// the 1.12 wire says about a resident aura is here; note what is missing and cannot be added — the
/// **remaining duration** (self-only, and it arrives out-of-band on `SMSG_UPDATE_AURA_DURATION`) and
/// the **caster** (which no packet ever associates with a slot). Decision 0255.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct UnitAuraSlot {
    /// The slot index it occupies, `0..UNIT_AURA_SLOTS`. Also its buff/debuff class, and the key
    /// `SMSG_UPDATE_AURA_DURATION` uses.
    pub slot: u8,
    /// The `Spell.dbc` id.
    pub spell_id: u32,
    /// The raw `UNIT_FIELD_AURAFLAGS` nibble for this slot.
    pub flags: u8,
    /// The **caster's** level at apply time — the only thing the wire says about who cast this.
    pub level: u8,
    /// Stack count, ≥ 1 (the wire byte holds `stack - 1`).
    pub stacks: u8,
}

impl UnitAuraSlot {
    /// A buff (slot in the positive half) rather than a debuff.
    pub fn is_helpful(&self) -> bool {
        self.slot < UNIT_AURA_POSITIVE_SLOTS
    }
    /// Whether the server will honour a `CMSG_CANCEL_AURA` for this aura.
    pub fn is_cancelable(&self) -> bool {
        self.flags & AURA_FLAG_CANCELABLE != 0
    }
}

// PLAYER descriptor fields — player customization (RF-0056, byte-verified against the client's own
// appearance decode `0x5fb200` + corpse cross-check). The PLAYER block starts at field 188.
const FIELD_PLAYER_BYTES: u16 = 193;
const FIELD_PLAYER_BYTES_2: u16 = 194;
// PLAYER_QUEST_LOG_1_1 = UNIT_END(188) + 0xA = 198 (vmangos `UpdateFields_1_12_1.h:128`; this block
// precedes the CONTAINER-onward hex-comment drift — enum arithmetic and hex comment agree here, and
// decision 0088 §4 live-verified an accepted quest id landing on 198 + 3·slot). 20 slots
// (MAX_QUEST_LOG_SIZE, vmangos `QuestDef.h:34`) × 3 fields (MAX_QUEST_OFFSET, `Player.h:439-444`):
// +0 quest id, +1 packed objective counters + state byte, +2 timer. The `_1` id field is GROUP_ONLY
// and the `_2` pair PRIVATE — both always stream for our own player, the exact consumer (the log).
const FIELD_PLAYER_QUEST_LOG_1_1: u16 = 198;

/// The number of `PLAYER_QUEST_LOG` slots (vmangos `MAX_QUEST_LOG_SIZE`, `QuestDef.h:34`).
pub const PLAYER_QUEST_LOG_SLOTS: u8 = 20;

/// One decoded `PLAYER_QUEST_LOG` slot — the descriptor's durable quest-log state (the
/// `SMSG_QUESTUPDATE_*` packets are the transient "toast" lines; this is what survives).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct QuestLogSlot {
    /// The quest id occupying the slot (`0` = the server cleared it — an empty slot).
    pub quest_id: u32,
    /// The four 6-bit kill/cast/interact objective counters (counter `i` at bits `6i..6i+6` of the
    /// count-state field — vmangos `Player.h:1100-1106` `SetQuestSlotCounter`). Item-objective
    /// progress is *not* here; the client counts bag items itself.
    pub counters: [u8; 4],
    /// The state byte (byte 3 of the count-state field — `Player.h:1107` `SetQuestSlotState`):
    /// [`quest_slot_state::COMPLETE`] / [`quest_slot_state::FAIL`], `0` = in progress.
    pub state: u8,
    /// The timer field — the absolute end time for a timed quest, else `0`.
    pub timer: u32,
}

/// `QUEST_STATE_*` (vmangos `Player.h:447-451`) — [`QuestLogSlot::state`] bit values.
pub mod quest_slot_state {
    pub const COMPLETE: u8 = 0x01;
    pub const FAIL: u8 = 0x02;
}
// ITEM/CONTAINER/PLAYER inventory fields (VERIFIED vmangos `UpdateFields_1_12_1.h` **enum
// arithmetic** — never its hex comments; see below) — the T2 container groundwork. The PLAYER
// slot arrays are PRIVATE-flagged: the server sends them only for our own player, which is
// exactly the consumer (our bags). Each slot is a 2-field guid.
const FIELD_ITEM_STACK_COUNT: u16 = 14; // OBJECT_END(6) + 0x8 (the ITEM-block comments are still true)
const FIELD_CONTAINER_NUM_SLOTS: u16 = 48; // ITEM_END = 6 + 0x2A (comment says 42 — stale, 6 low)
const FIELD_CONTAINER_SLOT_1: u16 = 50; // ITEM_END + 0x2; 36 slots × 2

// CORRECTED 2026-07-03: vmangos's UpdateFields_1_12_1.h hex COMMENTS go stale from the CONTAINER
// block on (6 low vs its own enum arithmetic, which is what the server compiles): INV_SLOT_HEAD =
// UNIT_END(0xBC) + 0x12A = 0x1E6 = 486, not the commented 0x1E0. VERIFIED against a live
// descriptor dump: with these bases One's shirt/legs/feet/mainhand/offhand/bags land on exactly
// the slots the character visibly wears (visible-item cross-check below).
const FIELD_PLAYER_VISIBLE_ITEM_1_CREATOR: u16 = 258; // UNIT_END + 0x46; 12 fields per slot
const FIELD_PLAYER_INV_SLOT_HEAD: u16 = 486; // 23 slots × 2 (equipment 0–18, bag bags 19–22)
const FIELD_PLAYER_PACK_SLOT_1: u16 = 532; // 16 slots × 2 (the backpack)
const FIELD_PLAYER_BANK_SLOT_1: u16 = 564; // 24 slots × 2
const FIELD_PLAYER_BANK_BAG_SLOT_1: u16 = 612; // 564 + 24×2; 6 bag slots × 2 (item guids)
                                               // The buyback trio (VERIFIED two ways: the slot array is chain-locked between the tested BANK
                                               // anchor and the tested XP anchor — 564 +48 bank = 612 bankbags, +12 = 624 buyback, +24 keyring
                                               // 648, +64 farsight 712, +2 combo 714, +2 = XP 716 ✓ — and price/timestamp are wow-re byte
                                               // anchors (ui/scratch/buyback-data-path.md: playerBlock +0x1038/+0x1068) rebased through the
                                               // tested COINAGE anchor: block base = 1176 − 0xf70/4 = 188 → 188+1038 = 1226, 188+1050 = 1238).
const FIELD_PLAYER_VENDORBUYBACK_SLOT_1: u16 = 624; // 12 slots × 2 (item guids)
const FIELD_PLAYER_BUYBACK_PRICE_1: u16 = 1226; // 12 × u32 copper, indexed slot−69
const FIELD_PLAYER_BUYBACK_TIMESTAMP_1: u16 = 1238; // 12 × u32 — the client's sort key only
                                                    // PLAYER_FIELD_COINAGE, our purse in copper (INT, PRIVATE — sent only for our own player, the exact
                                                    // consumer). Same comment drift as the slot arrays above: from the CONTAINER block on, vmangos's
                                                    // UpdateFields_1_12_1.h hex comments run 6 low vs its own enum arithmetic (which is what the server
                                                    // compiles). Derived by that arithmetic: COINAGE = UNIT_END(0xBC = 188) + 0x3DC(988) = 1176; the
                                                    // stale hex comment reads 0x492 = 1170 (decision 0081 quoted "field 1170" from it — 6 low).
                                                    // Cross-checked against the corrected neighbours: 486 (INV_SLOT_HEAD) + (0x3DC − 0x12A = 690) = 1176;
                                                    // 564 (BANK_SLOT_1) + (0x3DC − 0x178 = 612) = 1176. Both agree.
const FIELD_PLAYER_FIELD_COINAGE: u16 = 1176;
// PLAYER_XP / PLAYER_NEXT_LEVEL_XP (INT, PRIVATE — sent only for our own player, the exact consumer:
// the MainMenuBar XP bar). Derived by the same server enum arithmetic as COINAGE above:
// PLAYER_XP = UNIT_END(0xBC = 188) + 0x210(528) = 716; PLAYER_NEXT_LEVEL_XP = 717. Cross-checked
// against COINAGE: 1176 − 716 = 460 = (0x3DC − 0x210). (vmangos UpdateFields_1_12_1.h lists them at
// UNIT_END + 0x210 / 0x211; its stale hex comments read 0x2C6/0x2C7, the same 6-low drift as COINAGE.)
const FIELD_PLAYER_XP: u16 = 716;
const FIELD_PLAYER_NEXT_LEVEL_XP: u16 = 717;
// PLAYER_EXPLORED_ZONES_1 (BYTES ×64, PRIVATE — self only): the discovery bitset, 2048 bits
// indexed by AreaTable's exploreFlag column; the world map's exploration overlays key off it
// (decision 0203 phase 3). Derived by the same server enum arithmetic as COINAGE/XP above:
// UNIT_END(0xBC = 188) + 0x39B(923) = 1111. Cross-checked against COINAGE: 0x3DC − 0x39B = 65 =
// the 64 bitset slots + PLAYER_FIELD_WATCHED_FACTION_INDEX between them (1111 + 64 = 1175, then
// 1176 ✓); the stale hex comment reads 0x451 — the usual 6-low drift.
const FIELD_PLAYER_EXPLORED_ZONES_1: u16 = 1111;
/// The bitset's slot count (`Size: 64` in the server enum).
pub const PLAYER_EXPLORED_ZONES_SLOTS: u16 = 64;
// PLAYER stat/skill block (decision 0208 phase 1a). Derived the same way as the buyback trio above:
// `UNIT_END`(188) + vmangos's own symbolic offset, in **decimal** (never the hex `//` comments, stale
// from the CONTAINER block on) — chain-verified against the already-tested COINAGE(1176) and
// BUYBACK_PRICE_1(1226) anchors (see the tests).
const FIELD_PLAYER_POSSTAT0: u16 = 1177; // UNIT_END+0x3DD ×5; FLOAT despite the header's "Type: INT"
                                         // tag (VERIFIED vmangos `Player.h:1505,1511`
                                         // `ApplyStatBuffMod`/`GetPosStat` route through
                                         // `ApplyModSignedFloatValue`/`GetFloatValue`).
const FIELD_PLAYER_NEGSTAT0: u16 = 1182; // ×5, FLOAT, same header-tag mismatch; **negative-or-zero**
                                         // (VERIFIED `Player.h:1505`: a stat *debuff* routes its
                                         // already-negative `val` into this array through the same
                                         // signed accumulator).
const FIELD_PLAYER_RESISTANCEBUFFMODSPOSITIVE: u16 = 1187; // ×7, FLOAT (`Player.h:1513-1514`)
const FIELD_PLAYER_RESISTANCEBUFFMODSNEGATIVE: u16 = 1194; // ×7, FLOAT; negative-or-zero (VERIFIED
                                                           // `SpellAuras.cpp:4496-4499`: `change` is the signed
                                                           // resistance delta, routed here unchanged).
const FIELD_PLAYER_MOD_DAMAGE_DONE_POS: u16 = 1201; // ×7 schools ([0]=physical), INT (VERIFIED
                                                    // `StatSystem.cpp:87` `SetStatInt32Value` for the magic
                                                    // schools, `SpellAuras.cpp:5208-5210`
                                                    // `ApplyModUInt32Value` for school 0/physical).
const FIELD_PLAYER_MOD_DAMAGE_DONE_NEG: u16 = 1208; // ×7, INT; negative-or-zero (VERIFIED
                                                    // `SpellAuras.cpp:5211-5213,5244-5247`
                                                    // `ApplyModInt32Value(..., m_modifier.m_amount, apply)`
                                                    // with an already-negative `m_amount` on the debuff branch).
const FIELD_PLAYER_MOD_DAMAGE_DONE_PCT: u16 = 1215; // ×7, **FLOAT despite the header's "Type: INT"
                                                    // tag** (VERIFIED vmangos `Player.cpp:3336`
                                                    // `SetFloatValue(PLAYER_FIELD_MOD_DAMAGE_DONE_PCT + i,
                                                    // 1.00f)` init and `Player.cpp:7274`
                                                    // `SetFloatValue(PLAYER_FIELD_MOD_DAMAGE_DONE_PCT +
                                                    // school, multiplier)` — decision 0208's flagged
                                                    // question, now closed: a true float, default 1.0.
const FIELD_PLAYER_SKILL_INFO_1_1: u16 = 718; // UNIT_END+0x212, ×384 (128 skills × 3 dwords) — see
                                              // [`PlayerSkillSlot`] for the verified packing.

// PLAYER_CHARACTER_POINTS1/2 (INT, PRIVATE — self only): unspent talent points / free primary
// professions — the reference's `UnitCharacterPoints("player")` pair (decision 0304). Derived by
// the same server enum arithmetic as the anchors above: UNIT_END(188) + 0x392(914) = 1102 and
// +0x393 = 1103 — exactly one past the skill array (718 + 384 = 1102 ✓, the tests' chain).
const FIELD_PLAYER_CHARACTER_POINTS1: u16 = 1102;
const FIELD_PLAYER_CHARACTER_POINTS2: u16 = 1103;
// PLAYER_TRACK_CREATURES / PLAYER_TRACK_RESOURCES (INT, PRIVATE — self only): the active tracking
// bitmasks the minimap dot classifier tests — bit `1 << (n − 1)` where `n` is a creature type
// (TRACK_CREATURES) or a `LockType.dbc` id (TRACK_RESOURCES); the server sets one bit per active
// tracking aura from the aura's MiscValue (vmangos `SpellAuras.cpp` `HandleAuraTrackCreatures`/
// `HandleAuraTrackResources`, `1 << (m_miscvalue − 1)`). Derived by the same server enum
// arithmetic as the anchors above: UNIT_END(188) + 0x394(916) = 1104 and +0x395 = 1105 —
// contiguous past the CHARACTER_POINTS pair (1103 + 1 = 1104 ✓).
const FIELD_PLAYER_TRACK_CREATURES: u16 = 1104;
const FIELD_PLAYER_TRACK_RESOURCES: u16 = 1105;
const FIELD_PLAYER_AMMO_ID: u16 = 1223; // UNIT_END+0x40B, INT — the equipped ammo item id
                                        // (`Player.cpp:7656` etc., `Player::SetUInt32Value`).
                                        // PLAYER_FLAGS = UNIT_END(188) + 0x2 (VERIFIED vmangos `UpdateFields_1_12_1.h:120`; the duel
                                        // arbiter guid takes +0x0/+0x1). Bit 0x10 = PLAYER_FLAGS_GHOST (`Player.h:319`), set/cleared by
                                        // the ghost aura 8326's HandleAuraGhost at release/resurrect (decision 0308 §1).
const FIELD_PLAYER_FLAGS: u16 = 190;
const FIELD_PLAYER_FIELD_BYTES: u16 = 1222; // UNIT_END+0x40A, BYTES — vmangos `PLAYER_FIELD_BYTES`
                                            // (distinct from the appearance `PLAYER_BYTES`(193)):
                                            // flags / comboPoints / actionBars / HIGHEST_HONOR_RANK
                                            // (`Player.h` PLAYER_FIELD_BYTES_OFFSET_*).

/// The number of `PLAYER_SKILL_INFO` slots the descriptor reserves (384 fields ÷ 3 per slot;
/// vmangos `UpdateFields_1_12_1.h`). vmangos itself only ever *populates* `PLAYER_MAX_SKILLS` (127,
/// `Player.h:69`) of these — the last reserved slot is never written; `player_skill` honors the
/// full wire capacity so an out-of-range read is a real bug, not a silently-narrowed one.
pub const PLAYER_SKILL_SLOTS: u8 = 128;

/// One decoded `PLAYER_SKILL_INFO` slot (VERIFIED packing vmangos `Player.cpp:90-99`
/// `PLAYER_SKILL_INDEX`/`_VALUE_INDEX`/`_BONUS_INDEX` + the `MAKE_PAIR32`/`PAIR32_LOPART`/`_HIPART`
/// macros, `shared/Common.h:119-121`, and `Player::SetSkill`, `Player.cpp:5664-5677`): three
/// consecutive dwords per slot — `(id | step<<16)`, `(value | max<<16)`,
/// `(tempBonus | permBonus<<16)`, with the bonus halves **signed** (`SKILL_TEMP_BONUS`/
/// `SKILL_PERM_BONUS` cast to `int16` — a malus reads negative).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PlayerSkillSlot {
    /// The `SkillLine.dbc` id (`0` = an unused slot).
    pub skill_id: u16,
    /// The skill-tier step (`Player::SetSkill`'s `step` — 0 unless a tier-raising effect is live).
    pub step: u16,
    /// Current skill value.
    pub value: u16,
    /// Max skill value (the level/tier cap).
    pub max: u16,
    /// Temporary bonus (auras/consumables/enchants); signed — a malus reads negative.
    pub temp_bonus: i16,
    /// Permanent bonus (talents); signed.
    pub perm_bonus: i16,
}

/// A sparse update-field set: a bitmask of `u32` blocks followed by one `u32` per set bit. We keep the
/// values in an index→value map and expose **named** accessors for the descriptor fields consumers read
/// — the raw `get_*(index)` reads stay module-private, so field-index knowledge (a wire fact) never
/// leaves this crate (decision 0061). The ECS mirrors one of these per object and [`Self::merge`]s each
/// `Values` delta into it; the codec itself stays stateless (decision 0006).
///
/// **Absent-field semantics** hinge on [`Self::created`]: a CREATE block is a *complete* snapshot —
/// the server masks in only non-zero fields (vmangos `Object::_SetCreateBits`: bit set iff
/// `value != 0`), and the real client reads it into a zero-initialized descriptor buffer — so on a
/// create-seeded store an absent field reads `Some(0)`, the truth. A bare `Values` delta carries
/// only *changed* fields; there an absent field means "not touched", so it reads `None` (and a
/// merge must not clobber it). Director-caught: an item at durability 0 streams its create with
/// `MAXDURABILITY` but no `DURABILITY` word, and the sparse `None` sent the tooltip to the
/// template's max/max — 100% on fully broken gear.
#[derive(Debug, Clone, Default)]
pub struct ObjectFields {
    fields: BTreeMap<u16, u32>,
    /// Seeded by a CREATE block (set by the parser via [`Self::into_created`]): every field is
    /// known — absent = `0`.
    created: bool,
}

impl ObjectFields {
    pub(super) fn read(r: &mut impl Read) -> io::Result<Self> {
        let amount_of_blocks = read_u8(r)?;
        let mut blocks = Vec::with_capacity(amount_of_blocks as usize);
        for _ in 0..amount_of_blocks {
            blocks.push(read_u32_le(r)?);
        }
        let mut fields = BTreeMap::new();
        for (index, block) in blocks.iter().enumerate() {
            for bit in 0..32u16 {
                if block & (1 << bit) != 0 {
                    fields.insert(index as u16 * 32 + bit, read_u32_le(r)?);
                }
            }
        }
        Ok(Self {
            fields,
            created: false,
        })
    }

    /// Mark this set as a CREATE snapshot (see the struct doc's absent-field semantics) — the
    /// parser calls it on a create block's mask; a `Values` delta never gets it.
    pub fn into_created(mut self) -> Self {
        self.created = true;
        self
    }

    /// Build a field set directly from `(index, value)` pairs — the fixture constructor (unit
    /// tests, the capture harness's synthetic self-player, probes). The wire path is
    /// [`Self::read`]; live code never builds fields by hand. Chain [`Self::into_created`] when
    /// the fixture stands in for a create snapshot.
    pub fn from_pairs(pairs: &[(u16, u32)]) -> Self {
        Self {
            fields: pairs.iter().copied().collect(),
            created: false,
        }
    }

    /// A 2-field guid: `lo | hi << 32`, present iff the low half is (the wire always sends guid
    /// halves together; an absent slot reads `None`, a sent-empty slot reads `Some(0)`).
    /// `CORPSE_FIELD_OWNER` (field 6, `OBJECT_END + 0x0`, a 2-field guid — VERIFIED vmangos
    /// `UpdateFields_1_12_1.h:339`): the dead player this corpse belongs to. Only meaningful on a
    /// TYPEID_CORPSE (7) object; the net bridge uses it to recognize OUR corpse for the reclaim
    /// send (decision 0308 §5). `None`/0 when absent.
    pub fn corpse_owner(&self) -> Option<u64> {
        self.get_guid(6).filter(|&g| g != 0)
    }

    fn get_guid(&self, index: u16) -> Option<u64> {
        // Raw map read on purpose: a guid slot stays "present iff the low half is" even on a
        // created store — every consumer already treats a sent-empty `Some(0)` as no-guid, so the
        // absent-is-zero fallback would only blur the two identical cases.
        let lo = self.fields.get(&index).copied()?;
        let hi = self.get_u32(index + 1).unwrap_or(0);
        Some(u64::from(lo) | (u64::from(hi) << 32))
    }

    fn get_u32(&self, index: u16) -> Option<u32> {
        self.fields
            .get(&index)
            .copied()
            .or(self.created.then_some(0))
    }
    fn get_f32(&self, index: u16) -> Option<f32> {
        self.get_u32(index).map(f32::from_bits)
    }
    fn get_i32(&self, index: u16) -> Option<i32> {
        self.get_u32(index).map(|v| v as i32)
    }
    /// Split a `u32` field into its low/high 16-bit halves — a `TWO_SHORT`/`PAIR32` field
    /// (`MAKE_PAIR32`/`PAIR32_LOPART`/`_HIPART`, vmangos `shared/Common.h:119-121`).
    fn get_u16_pair(&self, index: u16) -> Option<(u16, u16)> {
        self.get_u32(index).map(|v| (v as u16, (v >> 16) as u16))
    }

    /// One slot's nibble out of a 8-slots-per-`u32` array (`UNIT_FIELD_AURAFLAGS`). An absent word
    /// reads `0`, matching the client's zero-initialized descriptor array.
    fn get_aura_nibble(&self, slot: u8) -> u8 {
        let word = self
            .get_u32(FIELD_UNIT_AURAFLAGS + u16::from(slot >> 3))
            .unwrap_or(0);
        ((word >> ((slot & 7) * 4)) & 0x0F) as u8
    }

    /// One slot's byte out of a 4-slots-per-`u32` array (`UNIT_FIELD_AURALEVELS`,
    /// `UNIT_FIELD_AURAAPPLICATIONS`). An absent word reads `0`, as above.
    fn get_aura_byte(&self, base: u16, slot: u8) -> u8 {
        let word = self.get_u32(base + u16::from(slot >> 2)).unwrap_or(0);
        (word >> ((slot & 3) * 8)) as u8
    }

    /// Fold an update into this store. A `Values` delta overlays: every field it carries replaces
    /// ours (WoW only resends *changed* fields, and a field going to `0` is an explicit `0`, never
    /// a removal), so the ECS accumulates the object's full descriptor set across packets
    /// (decision 0061). A re-CREATE snapshot *replaces* instead: it is the complete descriptor
    /// (absent = 0), and overlaying it would keep stale non-zero values for every field that
    /// dropped to zero since the last stream (an aura that expired out of view, durability that
    /// broke) — exactly the absent-vs-zero bug the `created` flag exists to kill.
    pub fn merge(&mut self, delta: ObjectFields) {
        if delta.created {
            *self = delta;
        } else {
            self.fields.extend(delta.fields);
        }
    }

    /// Whether this carried no fields — the codec skips emitting an empty `Values` delta.
    pub fn is_empty(&self) -> bool {
        self.fields.is_empty()
    }
    /// Every present `(index, value)` pair — the debug/probe affordance for descriptor
    /// archaeology (named accessors stay the consumer surface; decision 0061).
    pub fn raw_fields(&self) -> impl Iterator<Item = (u16, u32)> + '_ {
        self.fields.iter().map(|(&i, &v)| (i, v))
    }
}

mod player;
mod unit;

#[cfg(test)]
mod tests;
