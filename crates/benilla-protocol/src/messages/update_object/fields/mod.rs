use std::io::{self, Read};

use crate::wire::{read_u32_le, read_u8, Vector3d};

use super::movement::ObjectType;

// ObjectFields descriptor-field indices (build 5875), shared across object types.
const FIELD_OBJECT_TYPE: u16 = 2;
const FIELD_OBJECT_SCALE_X: u16 = 4;
// `OBJECT_FIELD_CREATED_BY` = OBJECT_END(6) + 0x0..0x1 (vmangos `UpdateFields_1_12_1.h`): the
// GameObject block's creator guid pair — the summoning player of a spell-spawned object
// (fishing bobber, ritual portal); zero/absent for a world spawn.
const FIELD_GAMEOBJECT_CREATED_BY: u16 = 6;
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
// GAMEOBJECT_FACTION = OBJECT_END(6) + field 14 — the slot between DYN_FLAGS(13) and TYPE_ID(15),
// i.e. the binary's `[go+0x110]+0x38` (wow-re cursor-system §4a's faction term, decision 0764).
const FIELD_GAMEOBJECT_FACTION: u16 = 20;
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
/// `UNIT_FIELD_SUMMON` (idx 8, `OBJECT_END(6) + 0x2`, a 2-field guid, PUBLIC — VERIFIED vmangos
/// `UpdateFields_1_12_1.h:42`) — the **inverse** of [`FIELD_UNIT_SUMMONEDBY`] below: the unit this
/// one has summoned. On our own descriptor it is the pet, and so the anchor of the `"pet"` unit
/// token; the pet action bar reads its own guid off `SMSG_PET_SPELLS` instead, which is why the
/// two can legitimately disagree for a beat around a summon (decision 0982).
const FIELD_UNIT_SUMMON: u16 = 8;
/// `UNIT_FIELD_CHARMEDBY` (idx 10, `OBJECT_END(6) + 0x4`, a 2-field guid — vmangos
/// `UpdateFields_1_12_1.h`) — whoever is currently charming this unit, `0` when nobody is.
/// Byte-verified client-side as `[unit+0x110]+0x10`: the shared attack-start validator
/// `0x612df0` reads it at `0x612e33` and refuses the swing when it is nonzero and is **not** the
/// active player's own guid (`ERR_ATTACK_CHARMED`), and the owner-guid pair in both `0x5ee5a0`
/// and the `PET_ATTACK_*` callback `0x5ff580` prefers it over their respective fallbacks
/// (wow-re `object-layer/scratch/pet-command-validators.md` §1/§2/§4).
const FIELD_UNIT_CHARMEDBY: u16 = 10;
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
/// `UNIT_FIELD_NATIVEDISPLAYID` (`OBJECT_END(6) + 0x7E` = 132, PUBLIC — vmangos
/// `UpdateFields_1_12_1.h:77`; it is the index between DISPLAYID (131) and MOUNTDISPLAYID (133),
/// both of which are independently pinned above). The unit's **unshifted** appearance: the server
/// writes it once at spawn/login and leaves it alone through every transform — a druid form
/// (`Aura::HandleAuraModShapeshift` sets DISPLAYID and restores it *from* this field), a GM
/// `.modify morph`, a polymorph. `UNIT_FIELD_DISPLAYID` is what you see; this is what the unit is.
///
/// The client reads it for exactly one thing benilla cares about: the **mover collision prism**
/// (`0x60b270` fetches its DBC row at `[unit+0x110]+0x1f8`, while its sibling `0x60ae10` uses
/// `+0x1f4` = DISPLAYID against the same array) — so a shapeshift does *not* resize the collision
/// box (wow-re `mover-collision-scalars.md`, `remote-swim-decision.md` §4; decision 1574).
const FIELD_UNIT_NATIVEDISPLAYID: u16 = 132;
/// `UNIT_FIELD_MOUNTDISPLAYID` (`OBJECT_END(6) + 0x7F` = 133, PUBLIC — vmangos
/// `UpdateFields_1_12_1.h:78`; byte-verified client-side at `[unit+0x110]+0x1fc` = index 0x85,
/// wow-re `rf39-field-index-map.md`/`d1.md`, HARD). A raw `CreatureDisplayInfo` id: the mount the
/// unit is riding, `0`/absent = not mounted. The server writes it on aura-78 apply and zeroes it
/// on every dismount path (`Unit::Mount`/`Unmount`) — this field, not the aura, IS the mounted
/// state (`Unit::IsMounted` reads it back; no flag or bytes field accompanies it). Decision 0441.
const FIELD_UNIT_MOUNTDISPLAYID: u16 = 133;
/// `UNIT_FIELD_PETNUMBER` (`OBJECT_END(6) + 0x85` = 139, PUBLIC — vmangos
/// `UpdateFields_1_12_1.h:84`; byte-verified client-side as `[unit+0x110]+0x214` = relative index
/// `0x85`, the second gate inside the rank getter `0x605620` — decision 0782). Non-zero ⇒ this
/// unit is somebody's pet or charm, which is exactly how the
/// client decides a creature has **no** classification (rank forced to 0), so no elite dragon, no
/// ELITE/BOSS tooltip word and no world-boss skull on an enslaved mob.
const FIELD_UNIT_PETNUMBER: u16 = 139;
/// `UNIT_FIELD_PET_NAME_TIMESTAMP` (`OBJECT_END(6) + 0x86` = 140, PUBLIC — vmangos
/// `UpdateFields_1_12_1.cpp:80`) — the unix second at which this pet's name was last set.
///
/// It matters because a pet's name does **not** ride its descriptor: it is answered once by
/// `CMSG_PET_NAME_QUERY` and cached by pet NUMBER, so a rename would otherwise stay invisible until
/// relog. The server bumps this on every `SetName` (`Pet.cpp:285`, and `HandlePetRename`'s last
/// line `PetHandler.cpp:344`), which makes a **change** here the cache's one invalidation signal.
const FIELD_UNIT_PET_NAME_TIMESTAMP: u16 = 140;
/// `UNIT_FIELD_PETEXPERIENCE` (idx 141) / `UNIT_FIELD_PETNEXTLEVELEXP` (142) — a hunter pet's XP
/// bar, the `(currXP, nextXP)` pair `GetPetExperience 0x4be840` returns in that order.
/// Byte-verified client-side at `[unit+0x110]+0x21c` / `+0x220` (wow-re
/// `ui/scratch/pet-action-bar-api.md` §11b); the client converts each with `fild qword` off a
/// **zero-extended** dword, so neither can read negative however large it gets.
const FIELD_UNIT_PETEXPERIENCE: u16 = 141;
const FIELD_UNIT_PETNEXTLEVELEXP: u16 = 142;
/// `UNIT_TRAINING_POINTS` (idx 149) — **two `u16`s packed into one dword**, byte-verified at
/// `[unit+0x110]+0x23c` / `+0x23e`: `GetPetTrainingPoints 0x4be790` pushes the **high** word first
/// and the low word second, which `PetPaperDollFrame.lua` names `totalPoints, spent`.
const FIELD_UNIT_TRAINING_POINTS: u16 = 149;
const FIELD_UNIT_DYNAMIC_FLAGS: u16 = 143;
/// `UNIT_CHANNEL_SPELL` (idx 144, `OBJECT_END(6) + 0x8A`, PUBLIC — VERIFIED vmangos
/// `UpdateFields_1_12_1.h:89`) — the spell id a unit is channeling (`0` = not channeling).
const FIELD_UNIT_CHANNEL_SPELL: u16 = 144;
/// `UNIT_CREATED_BY_SPELL` (idx 146, `OBJECT_END(6) + 0x8C`, PUBLIC — vmangos
/// `UpdateFields_1_12_1.h`) — the spell that summoned this unit (`0` = not summoned by one). The
/// **first** of the three gates on the reference's feed-pet path `0x6ea1e0`: a unit with no
/// creating spell is not a pet you can feed, whatever else it is.
///
/// The index is fixed by construction rather than asserted from a header we don't ship: this table
/// already pins both ends of a contiguous run — `PETEXPERIENCE` 141, `PETNEXTLEVELEXP` 142,
/// `DYNAMIC_FLAGS` 143, `CHANNEL_SPELL` 144, `MOD_CAST_SPEED` 145, then this, then `NPC_FLAGS` 147.
const FIELD_UNIT_CREATED_BY_SPELL: u16 = 146;
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
// PLAYER_BYTES_3 = UNIT_END(188) + 0x7: the low u16 packs `gender | (drunk & 0xFFFE)` (vmangos
// `Player::SetDrunkValue`), so byte 1 is the drunk value's high byte — exactly the byte the
// reference client reads at `[[unit+0xe68]+0x1d]` (wow-re `drunk_fraction_5e2a90`).
// Byte 2 = the city-protector title (a race id), byte 3 = the **current** honor rank
// (`PLAYER_BYTES_3_OFFSET_HONOR_RANK`, vmangos `Player.h:351-356`).
//
// **PUBLIC** (VERIFIED vmangos `UpdateFields_1_12_1.h:125`) — and that flag is the whole reason a
// foreign player's rank is knowable: this is the ONE honor value that streams for every visible
// player. The rest of the honor block (1250..1260 below, plus `PLAYER_FIELD_BYTES`' highest-rank
// byte) is PRIVATE, which is why anyone else's honor stats have to be *asked* for over the wire
// (`MSG_INSPECT_HONOR_STATS`, [`crate::messages::InspectHonorStats`]).
const FIELD_PLAYER_BYTES_3: u16 = 195;
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
const FIELD_ITEM_ENCHANTMENT: u16 = 22; // OBJECT_END + 0x10, Size 21 = 7 slots × 3 (id/duration/charges)
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
                                                    // PLAYER_FIELD_KEYRING_SLOT_1 — the next link of the same chain the buyback comment walks
                                                    // (624 + 12×2 = 648; +32×2 = 712 = FARSIGHT ✓, which the chain's far end already pins).
const FIELD_PLAYER_KEYRING_SLOT_1: u16 = 648; // 32 slots × 2 (item guids), wire slots 81–112
                                              // PLAYER_FARSIGHT — the object our view is anchored to (Mind Vision, Sentry Totem, the Ornate
                                              // Spyglass), or 0 for "my own body". A 2-field guid, PRIVATE, so it only ever arrives on our own
                                              // descriptor — exactly the consumer. Its index needs no fresh derivation: it is the next link of
                                              // the same chain the two comments above walk, and both ends of that chain are tested anchors
                                              // (648 + 32×2 = 712; +2 combo 714, +2 = XP 716 ✓). vmangos's hex comment reads 0x2C2 = 706 — the
                                              // same 6-low drift as COINAGE/XP, and not what the server compiles: its enum arithmetic says
                                              // UNIT_END(0xBC = 188) + 0x20C(524) = 712.
const FIELD_PLAYER_FARSIGHT: u16 = 712;
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
// PLAYER_FIELD_WATCHED_FACTION_INDEX (INT, PRIVATE — self only): the reputation-list slot whose bar
// rides the main menu bar, or -1 for none. SIGNED, and that matters — slot 0 is a real faction (the
// Bloodsail Buccaneers), so -1 is the only "watch nothing" (vmangos writes it with `SetInt32Value`
// in `HandleSetWatchedFactionOpcode`). Derived by the same server enum arithmetic as COINAGE above:
// UNIT_END(0xBC = 188) + 0x431(1073) = 1261. Cross-checked against the tested COINAGE anchor:
// 1176 + (0x431 − 0x3DC = 85) = 1261 ✓. (vmangos UpdateFields_1_12_1.h's stale hex comment reads
// 0x4E7 = 1255, the usual 6-low drift.)
const FIELD_PLAYER_WATCHED_FACTION_INDEX: u16 = 1261;
// PLAYER_REST_STATE_EXPERIENCE (INT, PRIVATE — self only): the rested-XP pool, in BASE kill-XP
// units — the server drains it 1:1 against a kill's base XP while granting +100%
// (vmangos `Player::GetXPRestBonus`), and caps it at NEXT_LEVEL_XP × 1.5 / 2 (`SetRestBonus`), so
// the on-screen doubled-XP span is 2× this field (the exhaustion tick's math; decision 1082).
// Derived by the same server enum arithmetic as COINAGE above: UNIT_END(0xBC = 188) +
// 0x3DB(987) = 1175 — exactly one below the tested COINAGE anchor (1175 + 1 = 1176 ✓).
const FIELD_PLAYER_REST_STATE_EXPERIENCE: u16 = 1175;
// PLAYER_EXPLORED_ZONES_1 (BYTES ×64, PRIVATE — self only): the discovery bitset, 2048 bits
// indexed by AreaTable's exploreFlag column; the world map's exploration overlays key off it
// (decision 0203 phase 3). Derived by the same server enum arithmetic as COINAGE/XP above:
// UNIT_END(0xBC = 188) + 0x39B(923) = 1111. Cross-checked against COINAGE: 0x3DC − 0x39B = 65 =
// the 64 bitset slots + PLAYER_REST_STATE_EXPERIENCE between them (1111 + 64 = 1175, then
// 1176 ✓); the stale hex comment reads 0x451 — the usual 6-low drift.
// The four avoidance/crit percentages the spell tooltip's chance-to-X line reads (law §3-CHANCE),
// consecutive and immediately below EXPLORED_ZONES_1: UNIT_END(188) + 0x396..0x399. FLOAT, and the
// header agrees for once. Anchored on EXPLORED_ZONES_1 = UNIT_END + 0x39B = 1111 just below.
const FIELD_PLAYER_BLOCK_PERCENTAGE: u16 = 1106;
const FIELD_PLAYER_DODGE_PERCENTAGE: u16 = 1107;
const FIELD_PLAYER_PARRY_PERCENTAGE: u16 = 1108;
const FIELD_PLAYER_CRIT_PERCENTAGE: u16 = 1109;
const FIELD_PLAYER_EXPLORED_ZONES_1: u16 = 1111;
/// The bitset's slot count (`Size: 64` in the server enum).
pub const PLAYER_EXPLORED_ZONES_SLOTS: u16 = 64;
// PLAYER stat/skill block (decision 0208 phase 1a). Derived the same way as the buyback trio above:
// `UNIT_END`(188) + vmangos's own symbolic offset, in **decimal** (never the hex `//` comments, stale
// from the CONTAINER block on) — chain-verified against the already-tested COINAGE(1176) and
// BUYBACK_PRICE_1(1226) anchors (see the tests).
// **These four families are INT on the wire, and the server's own storage type is not the wire
// type** (decision 1397). The header's "Type: INT" tag is right and the earlier "FLOAT" reading
// here was wrong: it was taken from vmangos's *internal* accessors (`Player.h:1505,1511-1514`
// really do route through `ApplyModSignedFloatValue`/`GetFloatValue`), which is one layer below
// the question. `Object::BuildValuesUpdate` (`Objects/Object.cpp`) carries an explicit special
// case — comment *"there are some float values which may be negative or can't get negative due to
// other checks"* — that sends exactly `NEGSTAT0..4`, `POSSTAT0..4`,
// `RESISTANCEBUFFMODSPOSITIVE+0..6` and `…NEGATIVE+0..6` as `*data << uint32(m_floatValues[index])`,
// the same float→int narrowing it already applies to `UNIT_FIELD_BASEATTACKTIME`. Live-confirmed on
// a level-60 warrior in preraid-BiS gear: fields 1177..1181 arrive as `105, 76, 68, 4, 4` (the
// gear's own +105 str / +76 agi / …), and 1215 `MOD_DAMAGE_DONE_PCT` in the same packet arrives as
// `0x3F800000` — genuine f32 — so the two encodings sit side by side in one block.
//
// Read **signed**: the server's cast of a negative float to `uint32` is UB, and the two
// architectures disagree. On x86 it wraps to the two's-complement word; on aarch64 `fcvtzu`
// saturates and a stat *debuff* arrives as a flat `0` (measured on this deploy's arm64 container:
// a −5 agility debuff moved `UNIT_FIELD_STAT1` but sent `NEGSTAT1 = 0`). `i32` is right for both.
const FIELD_PLAYER_POSSTAT0: u16 = 1177; // UNIT_END+0x3DD ×5, INT
const FIELD_PLAYER_NEGSTAT0: u16 = 1182; // ×5, INT; negative-or-zero where the wire can carry it
const FIELD_PLAYER_RESISTANCEBUFFMODSPOSITIVE: u16 = 1187; // ×7, INT
const FIELD_PLAYER_RESISTANCEBUFFMODSNEGATIVE: u16 = 1194; // ×7, INT; negative-or-zero
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
                                                    // UNIT_END+0x212, ×384 (128 skills × 3 dwords) — see [`PlayerSkillSlot`] for the verified
                                                    // packing. `pub` for fixtures that seed a skill block through [`ObjectFields::from_pairs`]
                                                    // (the lock resolver's skill-term tests); live reads go through [`ObjectFields::player_skill`].
pub const FIELD_PLAYER_SKILL_INFO_1_1: u16 = 718;

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
// PLAYER_DUEL_ARBITER = UNIT_END(188) + 0x0 (GUID, PUBLIC), PLAYER_DUEL_TEAM = UNIT_END + 0x8
// (INT, PUBLIC) — VERIFIED vmangos `UpdateFields_1_12_1.h:119,126`. The arbiter opens the player
// block, which is why the real client caches a pointer to that base (`[player+0xe68]`) and indexes
// both off it: `+0x00/+0x04` the arbiter halves, `+0x08` PLAYER_FLAGS, `+0x20` the duel team — the
// three reads `UnitReaction 0x6061e0`'s duel leg makes (decision 0633).
const FIELD_PLAYER_DUEL_ARBITER: u16 = 188;
const FIELD_PLAYER_DUEL_TEAM: u16 = 196;
// PLAYER_GUILDID = UNIT_END(188) + 0x3, PLAYER_GUILDRANK = UNIT_END + 0x4 — both INT and both
// **PUBLIC** (VERIFIED vmangos `UpdateFields_1_12_1.h:120-121`), the same arithmetic that pins
// PLAYER_FLAGS = 190 at +0x2 above. Independently VERIFIED in the binary off the very player-block
// base the duel pair is read through: `GetGuildInfo 0x4c9330` takes the guild id at `[+0xe68]+0xc`
// (`0x4c93a9`, whose `test ecx,ecx` IS the guildless→nil path) and the rank at `+0x10` (`0x4c93e4`,
// `shl edx,0x6` — the 0x40-byte rank-name slot in the cached guild record). 0xc/4 + 188 = 191 and
// 0x10/4 + 188 = 192, with `+0x8` = PLAYER_FLAGS(190) as the third point on the same line.
//
// PUBLIC is the load-bearing half: these stream for *every visible player*, not just for us, which
// is what makes `GetGuildInfo(unit)` answerable for an arbitrary unit — and why the guild query
// response carries a fixed ten rank names to decode another player's rank against (decision 1257).
const FIELD_PLAYER_GUILDID: u16 = 191;
const FIELD_PLAYER_GUILDRANK: u16 = 192;
// PLAYER_FIELD_COMBO_TARGET = UNIT_END(188) + 0x20E (GUID, PRIVATE) — VERIFIED vmangos
// `UpdateFields_1_12_1.h:251`, and independently by the binary: `GetComboPoints 0x51a190` reads the
// pair at `[player+0xe68]+0x838/+0x83c`, and 0x838/4 + 188 = 714 (decision 0875). The same player-
// block base that puts the combo-point byte at `+0x1029` (0x1029/4 + 188 = 1222) — two offsets off
// one pointer landing on two independently-known field indices is what pins the base itself.
const FIELD_PLAYER_FIELD_COMBO_TARGET: u16 = 714;
const FIELD_PLAYER_FIELD_BYTES: u16 = 1222; // UNIT_END+0x40A, BYTES — vmangos `PLAYER_FIELD_BYTES`
                                            // (distinct from the appearance `PLAYER_BYTES`(193)):
                                            // flags / comboPoints / actionBars / HIGHEST_HONOR_RANK
                                            // (`Player.h` PLAYER_FIELD_BYTES_OFFSET_*).

// The **honor block** — the eleven fields the two PvP/honor panes read, a contiguous run
// UNIT_END(188) + 0x426..0x430 (VERIFIED vmangos `UpdateFields_1_12_1.h:288-298` **enum
// arithmetic**; its inline `// 0x4DC`-style comments carry the usual 6-low drift and are not
// copied). The run needs no fresh anchor of its own: it closes against already-tested neighbours
// at BOTH ends — the 12-slot `BUYBACK_TIMESTAMP_1` array ends exactly where it begins
// (1238 + 12 = 1250) and `WATCHED_FACTION_INDEX`, itself chain-locked to the tested COINAGE, sits
// exactly one past its end (1260 + 1 = 1261). Eleven fields is precisely the size of that gap.
//
// **Every one of them is PRIVATE** — the server sends them only on our OWN descriptor — and that
// is the mechanical reason the character window's Honor tab works for the local player and for
// nobody else. A foreign player's honor stats are not on the wire at all until we ask for them
// with `MSG_INSPECT_HONOR_STATS` ([`crate::messages::InspectHonorStats`]); the sole honor value
// that *does* stream for every visible player is the current rank in the PUBLIC
// `PLAYER_BYTES_3`(195) byte 3 above.
//
// Types, in run order — `+0x426..0x429` TWO_SHORT, `+0x42A..0x42F` INT, `+0x430` BYTES:
//
// The four kill counters are TWO_SHORT, `(honorable, dishonorable)` in the low/high halves. Only
// SESSION_KILLS is genuinely written as two halves (vmangos `HonorMgr.cpp:913-914`,
// `SetUInt16Value(…, 0, todayHK)` / `(…, 1, todayDK)`); the other three go out as a whole dword
// (`SetUInt32Value(PLAYER_FIELD_YESTERDAY_KILLS, yesterdayKills)`, `HonorMgr.cpp:917/924/929`), so
// their high half is always 0 from this server. The descriptor type is TWO_SHORT regardless and
// the real client reads halves, so we decode halves — see the accessors in `player.rs`.
//
// The six INT fields are the three honor-point "contribution" totals, the two lifetime kill
// counts, and LAST_WEEK_RANK — which is the weekly **standing** (our ladder position), not a rank.
// All six are `SetUInt32Value` writes from `HonorMgr::Update` (`HonorMgr.cpp:917-935`). Our two
// lifetime names fix vmangos's own typo: the header spells them
// `PLAYER_FIELD_LIFETIME_HONORBALE_KILLS` / `…DISHONORBALE…`.
//
// BYTES2's byte 0 is `HONOR_RANK_BAR` (`PLAYER_FIELD_BYTES_2_OFFSET_HONOR_RANK_BAR`, vmangos
// `Player.h:366-372`); its bytes 1..3 are a flags byte and two unknowns this server never writes.
const FIELD_PLAYER_FIELD_SESSION_KILLS: u16 = 1250;
const FIELD_PLAYER_FIELD_YESTERDAY_KILLS: u16 = 1251;
const FIELD_PLAYER_FIELD_LAST_WEEK_KILLS: u16 = 1252;
const FIELD_PLAYER_FIELD_THIS_WEEK_KILLS: u16 = 1253;
const FIELD_PLAYER_FIELD_THIS_WEEK_CONTRIBUTION: u16 = 1254;
const FIELD_PLAYER_FIELD_LIFETIME_HONORABLE_KILLS: u16 = 1255;
const FIELD_PLAYER_FIELD_LIFETIME_DISHONORABLE_KILLS: u16 = 1256;
const FIELD_PLAYER_FIELD_YESTERDAY_CONTRIBUTION: u16 = 1257;
const FIELD_PLAYER_FIELD_LAST_WEEK_CONTRIBUTION: u16 = 1258;
const FIELD_PLAYER_FIELD_LAST_WEEK_RANK: u16 = 1259;
const FIELD_PLAYER_FIELD_BYTES2: u16 = 1260;

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

/// A sparse update-field set: a bitmask of `u32` blocks followed by one `u32` per set bit. We keep
/// the wire's own mask plus a dense value array — not a map — because the ECS-side pollers
/// (animation, equipment, plates) probe thousands of fields **per frame**, so a read must be two
/// array indexings, never a tree walk (decision 1457); the wire's `u8` block count bounds the
/// arrays at 8160 dwords, and a Unit's real footprint is ~800 B. We expose **named** accessors for
/// the descriptor fields consumers read — the raw `get_*(index)` reads stay module-private, so
/// field-index knowledge (a wire fact) never leaves this crate (decision 0061). The ECS mirrors
/// one of these per object and [`Self::merge`]s each `Values` delta into it; the codec itself
/// stays stateless (decision 0006).
///
/// **Absent-field semantics** hinge on [`Self::descriptor_end`]: a CREATE block is a *complete*
/// snapshot — the server masks in only non-zero fields (vmangos `Object::_SetCreateBits`: bit set
/// iff `value != 0`), and the real client reads it into a zero-initialized descriptor buffer — so
/// on a create-seeded store an absent field reads `Some(0)`, the truth. A bare `Values` delta
/// carries only *changed* fields; there an absent field means "not touched", so it reads `None`
/// (and a merge must not clobber it). Director-caught: an item at durability 0 streams its create
/// with `MAXDURABILITY` but no `DURABILITY` word, and the sparse `None` sent the tooltip to the
/// template's max/max — 100% on fully broken gear.
///
/// **…but only within the object's OWN descriptor** (decision 1081). "Absent = 0" is a statement
/// about the *zero-initialized buffer*, and that buffer is exactly [`descriptor_len`] dwords long
/// for this object's type. A creature has no PLAYER block at all, so a PLAYER-block index read off
/// one is not an absent field — it is a question the object cannot answer, and the honest reply is
/// `None`. Conflating the two made every `player_*` accessor answer a confident `0` for any
/// created creature, which is how the pet sheet's damage multiplier came out `0` instead of the
/// unstreamed default `1.0` and the tooltip read `inf - inf` / `nan` (director, 2026-08-07).
#[derive(Debug, Clone, Default)]
pub struct ObjectFields {
    /// Presence bitmask, one bit per field index (`index = word * 32 + bit`) — the wire's block
    /// mask, kept verbatim.
    present: Vec<u32>,
    /// Dense value array, invariant `values.len() == present.len() * 32`; a slot is meaningful
    /// iff its `present` bit is set (unset slots hold `0` but must be read through the mask —
    /// absent-vs-zero is exactly the distinction the struct doc is about).
    values: Vec<u32>,
    /// How long this object's descriptor is, in dwords — [`descriptor_len`] of the CREATE block's
    /// own `ObjectType` (set by the parser via [`Self::into_created`]). Every index **below** it is
    /// known: absent = `0`. `0` marks a bare `Values` delta (or a hand-built fixture), where no
    /// index is known and absent = `None`.
    descriptor_end: u16,
}

/// The length of an object's descriptor buffer, in dwords — the `*_END` of the innermost block its
/// `ObjectType` names (vmangos `UpdateFields_1_12_1.h`, build 5875, **by enum arithmetic**: the
/// trailing comments in that header's CONTAINER and PLAYER blocks are stale by a constant and are
/// not the numbers here).
///
/// The type hierarchy is inclusive — a Player's buffer spans OBJECT+UNIT+PLAYER, a Container's
/// OBJECT+ITEM+CONTAINER — so each entry is the outermost end, not a block size.
fn descriptor_len(object_type: ObjectType) -> u16 {
    match object_type {
        ObjectType::Object => 6,         // OBJECT_END = 0x6
        ObjectType::Item => 48,          // ITEM_END = OBJECT_END + 0x2A
        ObjectType::Container => 122,    // CONTAINER_END = ITEM_END + 0x4A
        ObjectType::Unit => 188,         // UNIT_END = OBJECT_END + 0xB6
        ObjectType::Player => 1282,      // PLAYER_END = UNIT_END + 0x446
        ObjectType::GameObject => 26,    // GAMEOBJECT_END = OBJECT_END + 0x14
        ObjectType::DynamicObject => 16, // DYNAMICOBJECT_END = OBJECT_END + 0xA
        ObjectType::Corpse => 38,        // CORPSE_END = OBJECT_END + 0x20
    }
}

impl ObjectFields {
    pub(super) fn read(r: &mut impl Read) -> io::Result<Self> {
        let amount_of_blocks = read_u8(r)?;
        let mut present = Vec::with_capacity(amount_of_blocks as usize);
        for _ in 0..amount_of_blocks {
            present.push(read_u32_le(r)?);
        }
        let mut values = vec![0u32; present.len() * 32];
        for (word, block) in present.iter().enumerate() {
            for bit in 0..32 {
                if block & (1u32 << bit) != 0 {
                    values[word * 32 + bit] = read_u32_le(r)?;
                }
            }
        }
        Ok(Self {
            present,
            values,
            descriptor_end: 0,
        })
    }

    /// Mark this set as a CREATE snapshot of `object_type` (see the struct doc's absent-field
    /// semantics) — the parser calls it on a create block's mask with the type off the same block's
    /// header; a `Values` delta never gets it. The type is what bounds "absent = 0" to the
    /// object's own descriptor.
    pub fn into_created(mut self, object_type: ObjectType) -> Self {
        self.descriptor_end = descriptor_len(object_type);
        self
    }

    /// Build a field set directly from `(index, value)` pairs — the fixture constructor (unit
    /// tests, the capture harness's synthetic self-player, probes). The wire path is
    /// [`Self::read`]; live code never builds fields by hand. Chain [`Self::into_created`] when
    /// the fixture stands in for a create snapshot.
    pub fn from_pairs(pairs: &[(u16, u32)]) -> Self {
        let mut this = Self::default();
        for &(index, value) in pairs {
            this.insert(index, value);
        }
        this
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

    /// `DYNAMICOBJECT_CASTER` (fields 6–7, `OBJECT_END + 0x0`, a 2-field guid — vmangos
    /// `UpdateFields_1_12_1.h:325`): the unit whose ground cast anchored this object.
    /// Index-twin of [`Self::corpse_owner`] — each TYPEID reuses `OBJECT_END`; the TypeId (or
    /// [`Self::dynamicobject_spell_id`]'s presence) decides which name applies.
    pub fn dynamicobject_caster(&self) -> Option<u64> {
        self.get_guid(6).filter(|&g| g != 0)
    }
    /// `DYNAMICOBJECT_BYTES` (field 8): the dynobj type byte — vmangos sends `1` (area spell)
    /// for every persistent-area cast (`DynamicObject::Create`; captured live 2026-07-30 for
    /// Blizzard 10 and Flamestrike 2120).
    pub fn dynamicobject_bytes(&self) -> Option<u32> {
        self.get_u32(8)
    }
    /// `DYNAMICOBJECT_SPELLID` (field 9): the anchoring spell — the root of the dest-anchored
    /// visual chain (B132 follow-up). `None`/0 when absent.
    pub fn dynamicobject_spell_id(&self) -> Option<u32> {
        self.get_u32(9).filter(|&s| s != 0)
    }
    /// `DYNAMICOBJECT_RADIUS` (field 10): the area's radius in yards (live: Blizzard 8.0,
    /// Flamestrike 5.0 — the server's `SpellRadius.dbc` resolve).
    pub fn dynamicobject_radius(&self) -> Option<f32> {
        self.get_f32(10)
    }
    /// `DYNAMICOBJECT_POS_X/Y/Z` + `FACING` (fields 11–14): the anchored point in raw WoW
    /// coords. Duplicates the create's movement-block position on the vmangos wire (both
    /// captured identical live); the accessor exists for `Values`-delta reads and tests.
    pub fn dynamicobject_position(&self) -> Option<([f32; 3], f32)> {
        Some((
            [self.get_f32(11)?, self.get_f32(12)?, self.get_f32(13)?],
            self.get_f32(14).unwrap_or(0.0),
        ))
    }

    /// Whether the wire genuinely carried this field (mask bit set) — the raw presence probe
    /// under every accessor, bypassing the created-store absent-is-zero fold.
    fn contains(&self, index: u16) -> bool {
        self.present
            .get(usize::from(index / 32))
            .is_some_and(|w| w & (1u32 << (index % 32)) != 0)
    }

    /// The carried value, `None` when the mask bit is unset — [`Self::contains`]'s value twin.
    fn get_raw(&self, index: u16) -> Option<u32> {
        self.contains(index)
            .then(|| self.values[usize::from(index)])
    }

    fn insert(&mut self, index: u16, value: u32) {
        let word = usize::from(index / 32);
        if word >= self.present.len() {
            self.present.resize(word + 1, 0);
            self.values.resize(self.present.len() * 32, 0);
        }
        self.present[word] |= 1u32 << (index % 32);
        self.values[usize::from(index)] = value;
    }

    fn get_guid(&self, index: u16) -> Option<u64> {
        // Raw read on purpose: a guid slot stays "present iff the low half is" even on a
        // created store — every consumer already treats a sent-empty `Some(0)` as no-guid, so the
        // absent-is-zero fallback would only blur the two identical cases.
        let lo = self.get_raw(index)?;
        let hi = self.get_u32(index + 1).unwrap_or(0);
        Some(u64::from(lo) | (u64::from(hi) << 32))
    }

    fn get_u32(&self, index: u16) -> Option<u32> {
        self.get_raw(index)
            .or((index < self.descriptor_end).then_some(0))
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
        if delta.descriptor_end != 0 {
            *self = delta;
        } else {
            for (index, value) in delta.raw_fields() {
                self.insert(index, value);
            }
        }
    }

    /// Whether this carried no fields — the codec skips emitting an empty `Values` delta.
    pub fn is_empty(&self) -> bool {
        self.present.iter().all(|&w| w == 0)
    }
    /// Every present `(index, value)` pair in ascending index order — the debug/probe affordance
    /// for descriptor archaeology (named accessors stay the consumer surface; decision 0061).
    pub fn raw_fields(&self) -> impl Iterator<Item = (u16, u32)> + '_ {
        self.present.iter().enumerate().flat_map(move |(word, &w)| {
            (0..32u16)
                .filter(move |bit| w & (1u32 << bit) != 0)
                .map(move |bit| {
                    let index = word as u16 * 32 + bit;
                    (index, self.values[usize::from(index)])
                })
        })
    }
}

mod player;
mod unit;

pub use unit::OwnerFallback;

#[cfg(test)]
mod tests;
