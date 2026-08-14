//! Pet messages — the pet action bar's state packet and the client verbs that drive it
//! (decisions 0982, 0988). Mirrored on the outbound side by `world::writer::pet`.
//!
//! The **shape of the system** is worth stating once, because it is neither the action bar's nor
//! the one it looks like. The bar's **contents** are the server's: `SMSG_PET_SPELLS` carries all
//! ten slots and nothing else does. The bar's **state** — the lit command, the lit reaction, the
//! autocast bit, "the pet is attacking" — is the **client's**, applied on the press and never
//! confirmed. Not as an optimisation: the server does not answer these packets at all, and the
//! real client writes its own state and repaints before the send (wow-re §10.1/§10.2). A client
//! that waited would show a bar that never lights.
//!
//! Compare the player's 120-slot bar, which is client-authoritative and merely *echoed* to the
//! server ([`super::action_bar`], decisions 0216 §7/0218 §4): different split, same lesson — know
//! which half of a surface each end owns before writing either.
//!
//! `SMSG_PET_SPELLS` carries the whole bar in one body, and the same body serves a hunter/warlock
//! pet, a possessed minion and a charmed creature (vmangos `Player::PetSpellInitialize` /
//! `PossessSpellInitialize` / `CharmSpellInitialize`, `Objects/Player.cpp:17519-17672` — three
//! builders, one byte layout: the possess builder's `int32 duration` + `uint32 0` occupies exactly
//! the pet builder's `uint32 0` + four state bytes). Its **guid-only** form (an 8-byte body
//! carrying a zero guid — `Player::RemovePetActionBar`) is the teardown: the bar goes away.

use std::io::{self, Read};

use crate::wire::{read_u16_le, read_u32_le, read_u64_le, read_u8};

/// Pet action-bar slots (vmangos `MAX_UNIT_ACTION_BAR_INDEX` = `ACTION_BAR_INDEX_END(10) -
/// ACTION_BAR_INDEX_START(0)`, `Objects/UnitDefines.h:781-787`); the client's own
/// `NUM_PET_ACTION_SLOTS` is the same 10 (`PetActionBarFrame.lua:4`).
pub const PET_ACTION_SLOTS: usize = 10;

/// `ActiveStates` — the value the **server** puts in a packed word's top byte (vmangos
/// `Objects/UnitDefines.h:724-732`). These are what you BUILD a word from; they are **not** what
/// you decode one into (see [`PetActionEntry::kind`], and the note below).
///
/// A passive spell: shown, never clickable, no autocast.
pub const PET_ACT_PASSIVE: u8 = 0x01;
/// A castable spell with autocast OFF — `0x01 | ` [`PET_AUTOCAST_ALLOWED`]`>>24`.
pub const PET_ACT_DISABLED: u8 = 0x81;
/// A castable spell with autocast ON — `0x81 | ` [`PET_AUTOCAST_ON`]`>>24`.
pub const PET_ACT_ENABLED: u8 = 0xC1;
/// A command token — the action is a [`PET_COMMAND_STAY`]-family value, not a spell id.
pub const PET_ACT_COMMAND: u8 = 0x07;
/// A reaction token — the action is a [`PET_REACT_PASSIVE`]-family value, not a spell id.
pub const PET_ACT_REACTION: u8 = 0x06;

/// The **client's** slot type, after its own `& 0x3F` mask (wow-re
/// `system/ui/scratch/pet-action-bar-api.md` §1, `0x4bdccd`). This is the vocabulary a decoded
/// word speaks, and it is deliberately SHORTER than `ActiveStates`': the mask drops bits 30/31,
/// which is precisely what collapses [`PET_ACT_PASSIVE`], [`PET_ACT_DISABLED`] and
/// [`PET_ACT_ENABLED`] — three server states — onto **one** client type, with the autocast pair
/// carried separately as [`PET_AUTOCAST_ALLOWED`]/[`PET_AUTOCAST_ON`].
///
/// `GetPetActionInfo`'s jump table sends types 1–5 down the spell branch, 6 to reaction, 7 to
/// command (§2). The two token constants are numerically the `ACT_` ones, so a `kind()` compare
/// against `PET_ACT_COMMAND`/`PET_ACT_REACTION` still reads correctly; the spell family does not,
/// which is why [`PetActionEntry::is_spell`] is a range and not a set of equalities.
pub const PET_TYPE_SPELL_FIRST: u8 = 1;
/// The last type the client's jump table routes to the spell branch.
pub const PET_TYPE_SPELL_LAST: u8 = 5;

/// Bit 31 of a packed word: this slot **may** autocast (wow-re §2.1, `0x4bdd65`). The client
/// additionally requires the spell to exist in `Spell.dbc` before reporting it.
pub const PET_AUTOCAST_ALLOWED: u32 = 0x8000_0000;
/// Bit 30: autocast is currently **running** (wow-re §2.1, `0x4bdda4`). This is the bit
/// `TogglePetAutocast` flips in place before it sends (§10.2, `0x4bcbff`).
pub const PET_AUTOCAST_ON: u32 = 0x4000_0000;

/// `CommandStates` (vmangos `UnitDefines.h:755-761`) — the action value carried by a
/// [`PET_ACT_COMMAND`] slot, and what `CharmInfo::GetCommandState` reports.
pub const PET_COMMAND_STAY: u32 = 0;
pub const PET_COMMAND_FOLLOW: u32 = 1;
pub const PET_COMMAND_ATTACK: u32 = 2;
pub const PET_COMMAND_DISMISS: u32 = 3;

/// `ReactStates` (vmangos `UnitDefines.h:734-739`) — the action value carried by a
/// [`PET_ACT_REACTION`] slot, and what `CharmInfo::GetReactState` reports.
pub const PET_REACT_PASSIVE: u32 = 0;
pub const PET_REACT_DEFENSIVE: u32 = 1;
pub const PET_REACT_AGGRESSIVE: u32 = 2;

/// The bit that marks the pet's bar **disabled**, in [`PetSpells::state`]'s own dword.
///
/// Two independent readings meet on this one bit, which is why it is worth stating: vmangos writes
/// `pet->IsEnabled() ? 0x0 : 0x8` as the state field's **fourth byte** (`Player.cpp:17536`), and
/// the client reads **bit 27** of the whole dword (wow-re §1/§4, `0x4bd08d`). `0x8 << 24` is bit
/// 27 — the same bit, reached from both ends.
///
/// It does two things, both verified in the client: it forces the reaction compare's left side to
/// Passive (§2.2) and it fails the usability predicate (§4). A disabled bar still draws.
pub const PET_STATE_BAR_DISABLED: u32 = 0x0800_0000;

/// The pet's `UNIT_FIELD_FLAGS` bits that make its bar unusable — vmangos
/// `UNIT_FLAG_STUNNED 0x00040000` / `UNIT_FLAG_CONFUSED 0x00400000` / `UNIT_FLAG_FLEEING
/// 0x00800000` (`UnitDefines.h:509-514`), which are exactly bits 18/22/23, the three the client's
/// usability predicate tests (wow-re §4 step 6, `0x4bd075`-`0x4bd08b`).
///
/// i.e. **a stunned, confused or feared pet cannot be ordered** — and note `UNIT_FLAG_POSSESSED`
/// (bit 24) is deliberately NOT among them: a possessed unit is exactly the case where the bar
/// must still work.
pub const PET_UNUSABLE_UNIT_FLAGS: u32 = 0x0004_0000 | 0x0040_0000 | 0x0080_0000;

/// The permanent-cooldown marker OR-ed into a pet cooldown's **category** duration (vmangos
/// `Unit::WritePetSpellsCooldown`, `Objects/Unit.cpp:11270-11271`) — the twin of the player
/// initial-spell list's top-bit convention. Strip it before using the number as a duration.
pub const PET_COOLDOWN_PERMANENT: u32 = 0x0800_0000;

/// One packed pet action word, decoded **the way the client decodes it** (wow-re §1, verified at
/// `0x4bdcc7`/`0x4bdccd`/`0x4bdce3`):
///
/// ```text
/// kind   = (packed >> 24) & 0x3F     the 0x3F mask is load-bearing — see below
/// action = packed & 0xFFFF           SIXTEEN bits, not 24
/// bit 31 = autocast allowed          bits 16..23 are never read by anything
/// bit 30 = autocast on
/// ```
///
/// The mask is the whole reason the server's five `ActiveStates` become the client's three
/// branches: `0x01`, `0x81` and `0xC1` all mask to type **1**, and what separated them —
/// castability and autocast — is read from bits 31/30 instead. Decoding `>> 24` unmasked (which
/// benilla did until this was pinned) happens to work for the exact bytes vmangos sends and
/// silently mis-sorts anything else.
///
/// What `action` *means* is `kind`'s to say: a spell id on the spell branch, a `CommandStates`
/// for [`PET_ACT_COMMAND`], a `ReactStates` for [`PET_ACT_REACTION`]. The raw word is kept
/// because the client echoes it **verbatim** in a [`pet_action`] body — re-packing from the split
/// pair would be a second chance to get it wrong.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct PetActionEntry {
    /// The whole word, exactly as it arrived (what [`pet_action`] echoes).
    pub packed: u32,
}

impl PetActionEntry {
    /// Bits 0–15: the spell id, command state or react state.
    pub fn action(self) -> u32 {
        self.packed & 0xFFFF
    }
    /// The client's slot type — bits 24–29 (see the type doc for why the mask matters).
    pub fn kind(self) -> u8 {
        ((self.packed >> 24) & 0x3F) as u8
    }
    /// Does this slot take the **spell** branch (client types 1–5), rather than the reaction or
    /// command branch? A type outside 1–7 is neither: it falls into the client's own default arm,
    /// which we treat as an empty slot rather than reproducing (wow-re §2.5 quirk 1).
    pub fn is_spell(self) -> bool {
        (PET_TYPE_SPELL_FIRST..=PET_TYPE_SPELL_LAST).contains(&self.kind())
    }
    /// Is the slot EMPTY? **The whole word is zero** — the client tests the dword, not a field
    /// (`0x4bdcbd test ecx,ecx`).
    ///
    /// vmangos's own unused middle slots are `(0, ACT_DISABLED)` = `0x8100_0000`, which is *not*
    /// zero and so takes the spell branch — where spell id 0 has no `Spell.dbc` record, so every
    /// return including the autocast pair comes back nil anyway (§2.1's no-record sub-path). Two
    /// routes, one empty-looking button; this is the client's route.
    pub fn is_empty(self) -> bool {
        self.packed == 0
    }
    /// Bit 31 — this slot may autocast. (The client also requires the spell to resolve in
    /// `Spell.dbc`; that half belongs to whoever holds the catalog.)
    pub fn autocast_allowed(self) -> bool {
        self.packed & PET_AUTOCAST_ALLOWED != 0
    }
    /// Bit 30 — autocast is currently running.
    pub fn autocast_on(self) -> bool {
        self.packed & PET_AUTOCAST_ON != 0
    }
    /// The word with bit 30 set or cleared — exactly `TogglePetAutocast`'s own in-place flip
    /// (wow-re §10.2, `0x4bcbff`: `if (bit30 clear) v |= 0x40000000 else v &= 0xBFFFFFFF`), which
    /// the client writes back to the slot **before** sending and which is then what goes on the
    /// wire.
    pub fn with_autocast(self, on: bool) -> Self {
        Self {
            packed: if on {
                self.packed | PET_AUTOCAST_ON
            } else {
                self.packed & !PET_AUTOCAST_ON
            },
        }
    }
}

impl From<u32> for PetActionEntry {
    fn from(packed: u32) -> Self {
        Self { packed }
    }
}

/// One entry of `SMSG_PET_SPELLS`' trailing cooldown block, decoded. The wire widths are
/// [`read_cooldown_block`]'s problem — the two senders disagree about them — so this is the
/// decoded shape only.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PetSpellCooldown {
    pub spell_id: u32,
    pub category: u16,
    pub spell_cd_ms: u32,
    /// The category remainder, possibly carrying [`PET_COOLDOWN_PERMANENT`] in its top bits.
    pub category_cd_ms: u32,
}

/// `SMSG_PET_SPELLS` in full — the pet action bar's entire state, in one body.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PetSpells {
    /// The controlled unit (pet, possessed minion or charmed creature). **Zero means the bar is
    /// gone**: `Player::RemovePetActionBar` sends an 8-byte body holding a zero guid and nothing
    /// else, and [`read_pet_spells`] answers that with an otherwise-default value.
    pub pet_guid: u64,
    /// The charm's remaining duration in ms (`0` for a real pet — only the possess/charm builders
    /// fill it, from the `SPELL_AURA_MOD_POSSESS`/`MOD_CHARM` aura).
    pub duration_ms: u32,
    /// The pet's mode word, **stored verbatim** — react state in byte 0, command state in byte 1,
    /// [`PET_STATE_BAR_DISABLED`] at bit 27.
    ///
    /// One dword rather than four bytes because that is what the client keeps (`[0xb71468]`, wow-re
    /// §1) and because two of its readings depend on the *whole* word rather than a field: the
    /// reaction compare zeroes itself on bit 27, and the command compare is an **unmasked**
    /// `state >> 8`, so a set bit 27 puts it out of range of any action and no command can light
    /// (§2.3). Splitting it into bytes loses both behaviours silently.
    pub state: u32,
    /// The ten bar slots, in bar order.
    pub bar: [PetActionEntry; PET_ACTION_SLOTS],
    /// The pet's whole known-spell list — the pet spellbook, packed the same way (spell id +
    /// its current autocast type). Empty for a temporary pet or a charm; only a permanent pet
    /// gets one (`Pet::IsPermanentPetFor` gates the loop, `Player.cpp:17549`).
    pub spells: Vec<PetActionEntry>,
    /// The pet's running cooldowns.
    pub cooldowns: Vec<PetSpellCooldown>,
}

impl PetSpells {
    /// Byte 0 of [`Self::state`] — a [`PET_REACT_PASSIVE`]-family value.
    pub fn react_state(&self) -> u32 {
        self.state & 0xFF
    }
    /// `state >> 8` — a [`PET_COMMAND_STAY`]-family value, **unmasked on purpose**.
    ///
    /// The client's own command compare is `(state >> 8) == action` with no `& 0xFF`
    /// (`0x4bdf0f`), so when bit 27 is set this is a number no command state can equal and every
    /// command button goes dark. Masking here would be "tidier" and would quietly delete that.
    pub fn command_state(&self) -> u32 {
        self.state >> 8
    }
    /// Bit 27 — the server says the bar is disabled.
    pub fn bar_disabled(&self) -> bool {
        self.state & PET_STATE_BAR_DISABLED != 0
    }
}

/// Read `SMSG_PET_SPELLS` (VERIFIED vmangos `Player::PetSpellInitialize`,
/// `Objects/Player.cpp:17519-17561`, with `CharmInfo::BuildActionBar` `Unit.cpp:8782-8786` and
/// `Unit::WritePetSpellsCooldown` `Unit.cpp:11245-11280`):
///
/// ```text
/// u64 petGuid
/// u32 duration          (0 for a pet; the charm/possess remainder otherwise)
/// u32 state             react | command<<8 | 0<<16 | enabledFlag<<24  — kept VERBATIM
/// u32 × 10              the bar, packed
/// u8  spellCount        then that many u32, packed the same way
/// <the cooldown block>  see read_cooldown_block — the one field the client and vmangos disagree on
/// ```
///
/// vmangos writes the state as four separate bytes and the client reads one dword
/// (`0x4bd9d3 GetInt32` → `[0xb71468]`); those are the same four bytes, and holding the dword is
/// what lets bit 27 and the unmasked `>> 8` behave (see [`PetSpells::state`]).
///
/// The **8-byte teardown** form (`Player::RemovePetActionBar`, `Player.cpp:17672-17677`) is a lone
/// zero guid: this returns [`PetSpells::default`] for it, so a caller's only test is
/// `pet_guid == 0`. Anything short of a full body after a non-zero guid is a real parse error and
/// surfaces as one — a truncated bar is not a bar.
pub(super) fn read_pet_spells(r: &mut &[u8]) -> io::Result<PetSpells> {
    let pet_guid = read_u64_le(r)?;
    if r.is_empty() {
        // The teardown. vmangos always writes a zero guid here; carrying whatever arrived keeps
        // the reader honest, and the `pet_guid == 0` test is the caller's contract either way.
        return Ok(PetSpells {
            pet_guid,
            ..Default::default()
        });
    }
    let duration_ms = read_u32_le(r)?;
    let state = read_u32_le(r)?;

    let mut bar = [PetActionEntry::default(); PET_ACTION_SLOTS];
    for slot in &mut bar {
        *slot = read_u32_le(r)?.into();
    }

    let spell_count = read_u8(r)?;
    let mut spells = Vec::with_capacity(spell_count as usize);
    for _ in 0..spell_count {
        spells.push(read_u32_le(r)?.into());
    }

    let cooldowns = read_cooldown_block(r)?;

    Ok(PetSpells {
        pet_guid,
        duration_ms,
        state,
        bar,
        spells,
        cooldowns,
    })
}

/// The trailing cooldown block — **the one place the real client and vmangos disagree about this
/// packet**, so it is read by measuring rather than by choosing a side.
///
/// - The **client** reads `u8 count` then 12-byte entries `{u16 spellId, u16 category, u32, u32}`
///   (wow-re §8, getter widths verified at `0x4bda58`/`0x4bda6c`/`0x4bda82`; the arbitration is
///   §12.6).
/// - **vmangos** writes `u16 count` then 14-byte entries `{u32 spellId, u16 category, u32, u32}`
///   (`Unit::WritePetSpellsCooldown`, `Objects/Unit.cpp:11225-11258`) — the later-expansion
///   layout. A real 1.12 client talking to vmangos mis-parses this block; we are not going to
///   reproduce that bug just to match it.
///
/// The block is the packet's **tail**, so the remaining length settles it outright: `n` client
/// entries leave `12n` bytes, `n` vmangos entries leave `1 + 14n` (the `+1` being the u16 count's
/// high byte, which the `u8` read did not consume). `12n == 1 + 14m` has no solution in
/// non-negative integers with `n == m`, and for `count == 0` both leave nothing to read. So the
/// discriminator is exact, not a heuristic — and the day vmangos is fixed, this keeps working.
fn read_cooldown_block(r: &mut &[u8]) -> io::Result<Vec<PetSpellCooldown>> {
    let count = usize::from(read_u8(r)?);
    if count == 0 {
        // vmangos leaves its count's high byte behind; the client's form leaves nothing. Either
        // way there is nothing to read, and trailing slack is not an error.
        return Ok(Vec::new());
    }
    let vmangos = r.len() == 1 + 14 * count;
    if vmangos {
        let _high_byte = read_u8(r)?;
    }
    let mut cooldowns = Vec::with_capacity(count);
    for _ in 0..count {
        let spell_id = if vmangos {
            read_u32_le(r)?
        } else {
            u32::from(read_u16_le(r)?)
        };
        cooldowns.push(PetSpellCooldown {
            spell_id,
            category: read_u16_le(r)?,
            spell_cd_ms: read_u32_le(r)?,
            category_cd_ms: read_u32_le(r)?,
        });
    }
    Ok(cooldowns)
}

/// `SMSG_PET_MODE` — the state-only refresh: the same four bytes `SMSG_PET_SPELLS` carries, with
/// no bar behind them. The server sends it when a command or reaction changes without the bar's
/// contents moving.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct PetMode {
    pub pet_guid: u64,
    /// The same verbatim dword [`PetSpells::state`] holds — the client stores it through the very
    /// same writer (`0x4bc930`) from both packets.
    pub state: u32,
}

/// Read `SMSG_PET_MODE` — `u64 petGuid, u32 state` (client handler `0x4bdb10`, wow-re §8;
/// vmangos's four state bytes at `WorldPackets::Pet::PetMode::AppendBodyTo`,
/// `Server/Packets/Pet.cpp:101-108`, are that dword).
pub(super) fn read_pet_mode(r: &mut impl Read) -> io::Result<PetMode> {
    Ok(PetMode {
        pet_guid: read_u64_le(r)?,
        state: read_u32_le(r)?,
    })
}

/// Read `SMSG_PET_ACTION_FEEDBACK` (VERIFIED vmangos `PetActionFeedback::AppendBodyTo`,
/// `Server/Packets/Pet.cpp:87-90`): one reason byte. The client turns it into the red error line
/// ("Your pet has no path to that location", …).
pub(super) fn read_pet_action_feedback(r: &mut impl Read) -> io::Result<u8> {
    read_u8(r)
}

/// Read `SMSG_PET_CAST_FAILED` (VERIFIED vmangos `PetCastFailed::AppendBodyTo`,
/// `Server/Packets/Pet.cpp:127-132`): `u32 spellId, u8 status, u8 reason`. The pet's twin of
/// `SMSG_CAST_RESULT` — the same `SpellCastResult` vocabulary, decoded the same way (status `2` =
/// fail) so the one message table renders both. vmangos only ever sends the fail status here, but
/// reading it the shared way costs nothing and keeps the two decodes from drifting.
///
/// The body stops at the reason: unlike `CastResult`, `PetCastFailed::AppendBodyTo` writes **no
/// argument words** at all, so a pet failure never carries a `%s` fill.
pub(super) fn read_pet_cast_failed(r: &mut impl Read) -> io::Result<(u32, super::CastOutcome)> {
    let spell_id = read_u32_le(r)?;
    let status = read_u8(r)?;
    let outcome = if status == 2 {
        super::CastOutcome::Failed {
            reason: read_u8(r)?,
            arg: None,
        }
    } else {
        super::CastOutcome::Ok
    };
    Ok((spell_id, outcome))
}

/// Body of `CMSG_PET_ACTION` (VERIFIED vmangos `WorldPackets::Pet::PetAction::ReadFromWorldPacket`,
/// `Server/Packets/Pet.cpp:9-14`; opcode 373 `Opcodes_1_12_1.h:374`): `u64 petGuid, u32 packedData,
/// u64 targetGuid`.
///
/// **`packed` is the slot's own word, echoed** — the server re-splits it exactly as the client
/// packed it (`UNIT_ACTION_BUTTON_ACTION/TYPE` at `PetHandler.cpp:37-38`) and dispatches on the
/// type: `ACT_COMMAND` runs `HandlePetCommand`, `ACT_REACTION` sets the react state, and the three
/// spell types cast. `target` is `0` when the action needs none.
pub fn pet_action(pet_guid: u64, packed: u32, target_guid: u64) -> Vec<u8> {
    let mut body = Vec::with_capacity(20);
    body.extend_from_slice(&pet_guid.to_le_bytes());
    body.extend_from_slice(&packed.to_le_bytes());
    body.extend_from_slice(&target_guid.to_le_bytes());
    body
}

/// Body of `CMSG_PET_STOP_ATTACK` (VERIFIED vmangos `PetStopAttack::ReadFromWorldPacket`,
/// `Server/Packets/Pet.cpp:33-36`; opcode 746 `Opcodes_1_12_1.h:747`): the lone `u64` pet guid.
/// The Attack button's *second* press — `PetActionBarFrame.lua:258-262` calls `PetStopAttack()`
/// instead of `CastPetAction` when the attack is already running.
pub fn pet_stop_attack(pet_guid: u64) -> Vec<u8> {
    pet_guid.to_le_bytes().to_vec()
}

/// Body of `CMSG_PET_CANCEL_AURA` (opcode 619): `u64 petGuid` then `u32 spellId`.
///
/// Both ends agree independently. wow-re read the send off the client
/// (`ui/scratch/pet-action-bar-api.md` §10.1, `0x4bd25f`–`0x4bd2ad`: `push 0x26b`, the guid pair,
/// the id); vmangos reads the same two fields in the same order
/// (`PetCancelAura::ReadFromWorldPacket`, `Server/Packets/Pet.cpp:27-31`).
///
/// The guid is what distinguishes it from the player's `CMSG_CANCEL_AURA` — which carries a bare
/// spell id and so could only ever mean "mine". The handler re-checks that the guid is our pet or
/// our charm and drops the packet otherwise, and answers a **dead** pet with
/// `SendPetActionFeedback(FEEDBACK_PET_DEAD)` rather than silence (`HandlePetCancelAuraOpcode`,
/// `Handlers/SpellHandler.cpp:407-432`) — the same feedback channel the bar's other refusals use.
pub fn pet_cancel_aura(pet_guid: u64, spell_id: u32) -> Vec<u8> {
    let mut body = Vec::with_capacity(12);
    body.extend_from_slice(&pet_guid.to_le_bytes());
    body.extend_from_slice(&spell_id.to_le_bytes());
    body
}

/// Body of `CMSG_PET_SPELL_AUTOCAST` (opcode 755): `u64 petGuid`, `u32 spellId`, `u8 state` —
/// **thirteen** bytes, and the trailing field is a byte (decision 1032).
///
/// Both ends agree independently. The client builds it at `0x4bcd5f`–`0x4bcdb2`: `push 0x2f3`, the
/// guid pair through the u64 writer `0x418370`, the spell id through the u32 writer `0x418190`,
/// then `(word >> 30) & 0xFFFFFF01` through **`0x418070`**, the *byte* writer — which is also why
/// that mask ends in `01`. vmangos reads `guid, spellId, state` in that order with `state` a
/// `uint8` (`PetSpellAutocast::ReadFromWorldPacket`, `Server/Packets/Pet.cpp:44-49`).
///
/// **This is the pet SPELLBOOK's autocast verb, not the pet bar's.** The bar's right click sends
/// [`pet_set_action`], whose body names a slot *position*; the book has no positions, so this one
/// names the spell. The server's handler is correspondingly different — it re-derives which slots
/// to update itself (`HandlePetSpellAutocastOpcode`, `PetHandler.cpp:451-478`), gating on
/// `pet->HasSpell(spellId) && Spells::IsAutocastable(spellId)`. No reply packet.
pub fn pet_spell_autocast(pet_guid: u64, spell_id: u32, enabled: bool) -> Vec<u8> {
    let mut body = Vec::with_capacity(13);
    body.extend_from_slice(&pet_guid.to_le_bytes());
    body.extend_from_slice(&spell_id.to_le_bytes());
    body.push(u8::from(enabled));
    body
}

/// Body of `CMSG_PET_SET_ACTION` (opcode 372): `u64 petGuid` then one or two
/// `(u32 position, u32 packedData)` pairs. The server distinguishes the forms **by body size
/// alone** — 24 bytes ⇒ two entries, anything else ⇒ one (`PetSetAction::ReadFromWorldPacket`,
/// `Server/Packets/Pet.cpp:52-62`) — so a one-entry body must not be padded.
///
/// This one opcode carries **both** of the bar's write verbs, which is the thing to know about it:
///
/// - **the autocast toggle** — one pair, the pressed slot and its word with bit 30 flipped. That
///   is `TogglePetAutocast`'s whole send (wow-re §10.2, `0x4bcc1e push 0x174`), and the server
///   reads the flip straight out of the type byte: `ACT_ENABLED` ⇒ `ToggleAutocast(true)`,
///   `ACT_DISABLED` ⇒ `false` (`HandlePetSetAction`, `PetHandler.cpp`). `CMSG_PET_SPELL_AUTOCAST`
///   (0x2F3) is **not** this verb — it belongs to `ToggleSpellAutocast`, the pet *spellbook*
///   frame's binding, and indexes the spellbook rather than the bar (wow-re §10.2/§11).
/// - **the drag** — one or two pairs, from `0x4bc9a0` (wow-re §10.4): the optional first pair
///   relocates whatever occupied the target slot, the mandatory second writes the drop. vmangos
///   requires the two-pair form whenever a command/reaction token moves.
pub fn pet_set_action(pet_guid: u64, entries: &[(u32, u32)]) -> Vec<u8> {
    let mut body = Vec::with_capacity(8 + 8 * entries.len());
    body.extend_from_slice(&pet_guid.to_le_bytes());
    for &(position, packed) in entries {
        body.extend_from_slice(&position.to_le_bytes());
        body.extend_from_slice(&packed.to_le_bytes());
    }
    body
}

/// Body of `CMSG_PET_ABANDON` (VERIFIED vmangos `PetAbandon::ReadFromWorldPacket`,
/// `Server/Packets/Pet.cpp:16-19`; opcode 374): the lone `u64` pet guid.
///
/// **The menu's Abandon and its Dismiss are this same message** — see [`super::opcode`]'s note.
/// Nothing in the body says which the player picked, and nothing needs to: the server forks on the
/// pet's own type, deleting a hunter pet and unsummoning anything else. The client's fork is purely
/// about which *word* the menu shows.
pub fn pet_abandon(pet_guid: u64) -> Vec<u8> {
    pet_guid.to_le_bytes().to_vec()
}

/// Body of `CMSG_PET_RENAME` (VERIFIED vmangos `PetRename::ReadFromWorldPacket`,
/// `Server/Packets/Pet.cpp:21-25`; opcode 375): `u64 petGuid` then the name as a **C string**.
///
/// The name is not length-prefixed and not padded — vmangos's `>> std::string` reads to the first
/// NUL. The 12-character cap is the popup's (`RENAME_PET`'s `maxLetters = 12`), not the wire's; the
/// server re-checks with `ObjectMgr::CheckPetName` and refuses with `SMSG_PET_NAME_INVALID`
/// rather than truncating.
pub fn pet_rename(pet_guid: u64, name: &str) -> Vec<u8> {
    let mut body = Vec::with_capacity(9 + name.len());
    body.extend_from_slice(&pet_guid.to_le_bytes());
    body.extend_from_slice(name.as_bytes());
    body.push(0);
    body
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A hunter pet's `SMSG_PET_SPELLS`, byte-for-byte as vmangos builds it: the default
    /// 3 commands / 4 spell slots / 3 reactions bar, one known spell, one running cooldown.
    fn pet_spells_body() -> Vec<u8> {
        let mut b = Vec::new();
        b.extend_from_slice(&0xF140_0000_0000_002Au64.to_le_bytes()); // pet guid
        b.extend_from_slice(&0u32.to_le_bytes()); // duration (0 = a real pet)
        b.push(PET_REACT_DEFENSIVE as u8);
        b.push(PET_COMMAND_FOLLOW as u8);
        b.push(0);
        b.push(0); // enabled
                   // The bar, exactly CharmInfo::InitPetActionBar's default: attack/follow/stay,
                   // four spell slots (one filled with Claw 3010, autocast on), aggressive/def/passive.
        for packed in [
            PET_COMMAND_ATTACK | (u32::from(PET_ACT_COMMAND) << 24),
            PET_COMMAND_FOLLOW | (u32::from(PET_ACT_COMMAND) << 24),
            PET_COMMAND_STAY | (u32::from(PET_ACT_COMMAND) << 24),
            3010 | (u32::from(PET_ACT_ENABLED) << 24),
            u32::from(PET_ACT_DISABLED) << 24, // empty
            u32::from(PET_ACT_DISABLED) << 24, // empty
            u32::from(PET_ACT_DISABLED) << 24, // empty
            PET_REACT_AGGRESSIVE | (u32::from(PET_ACT_REACTION) << 24),
            PET_REACT_DEFENSIVE | (u32::from(PET_ACT_REACTION) << 24),
            PET_REACT_PASSIVE | (u32::from(PET_ACT_REACTION) << 24),
        ] {
            b.extend_from_slice(&packed.to_le_bytes());
        }
        b.push(1); // spell count
        b.extend_from_slice(&(3010u32 | (u32::from(PET_ACT_ENABLED) << 24)).to_le_bytes());
        // The cooldown block in VMANGOS's layout: u16 count, u32 spell id (14-byte entries).
        b.extend_from_slice(&1u16.to_le_bytes());
        b.extend_from_slice(&3010u32.to_le_bytes());
        b.extend_from_slice(&0u16.to_le_bytes()); // category
        b.extend_from_slice(&4_500u32.to_le_bytes());
        b.extend_from_slice(&0u32.to_le_bytes());
        b
    }

    /// The same packet with the cooldown block in the **client's** layout: u8 count, u16 spell id
    /// (12-byte entries). Both must decode identically — see [`read_cooldown_block`].
    fn pet_spells_body_client_cooldowns() -> Vec<u8> {
        let mut b = pet_spells_body();
        b.truncate(b.len() - 16); // drop vmangos's u16 count + its one 14-byte entry
        b.push(1); // u8 count
        b.extend_from_slice(&3010u16.to_le_bytes());
        b.extend_from_slice(&0u16.to_le_bytes()); // category
        b.extend_from_slice(&4_500u32.to_le_bytes());
        b.extend_from_slice(&0u32.to_le_bytes());
        b
    }

    #[test]
    fn pet_spells_golden() {
        let body = pet_spells_body();
        let mut r = &body[..];
        let p = read_pet_spells(&mut r).unwrap();
        assert!(r.is_empty(), "the body is fully consumed");

        assert_eq!(p.pet_guid, 0xF140_0000_0000_002A);
        assert_eq!(p.duration_ms, 0);
        assert_eq!(p.react_state(), PET_REACT_DEFENSIVE);
        assert_eq!(p.command_state(), PET_COMMAND_FOLLOW);
        assert!(!p.bar_disabled());

        // The three branches, read off the same word shape.
        assert_eq!(p.bar[0].kind(), PET_ACT_COMMAND);
        assert_eq!(p.bar[0].action(), PET_COMMAND_ATTACK);
        assert!(!p.bar[0].is_spell());
        assert_eq!(p.bar[9].kind(), PET_ACT_REACTION);
        assert_eq!(p.bar[9].action(), PET_REACT_PASSIVE);

        // The mask at work: the server sent 0xC1, and the client's type is 1 — the autocast pair
        // having moved out of the type byte into bits 31/30.
        assert_eq!(p.bar[3].kind(), PET_TYPE_SPELL_FIRST);
        assert_eq!(p.bar[3].action(), 3010);
        assert!(p.bar[3].is_spell() && !p.bar[3].is_empty());
        assert!(p.bar[3].autocast_allowed() && p.bar[3].autocast_on());

        // vmangos's unused middle slot (0, ACT_DISABLED) is NOT the client's empty word — it takes
        // the spell branch, where spell id 0 resolves to nothing. Same button, different route.
        assert!(!p.bar[4].is_empty());
        assert!(p.bar[4].is_spell() && p.bar[4].action() == 0);
        assert!(
            PetActionEntry::default().is_empty(),
            "zero IS the empty word"
        );

        assert_eq!(p.spells.len(), 1);
        assert_eq!(p.spells[0].action(), 3010);
        assert_eq!(
            p.cooldowns,
            vec![PetSpellCooldown {
                spell_id: 3010,
                category: 0,
                spell_cd_ms: 4_500,
                category_cd_ms: 0,
            }]
        );
    }

    /// The cooldown block's two layouts decode to the same thing — the client's 12-byte form and
    /// vmangos's 14-byte one, told apart by the tail length alone.
    #[test]
    fn both_cooldown_layouts_decode_identically() {
        let from_vmangos = read_pet_spells(&mut &pet_spells_body()[..]).unwrap();
        let body = pet_spells_body_client_cooldowns();
        let mut r = &body[..];
        let from_client = read_pet_spells(&mut r).unwrap();
        assert!(r.is_empty(), "the client-layout body is fully consumed");
        assert_eq!(from_client.cooldowns, from_vmangos.cooldowns);
        assert_eq!(from_client, from_vmangos);
    }

    /// A pet with no cooldowns at all: the client's form ends the packet, vmangos's leaves its
    /// count's high byte behind. Neither is an error and neither invents an entry.
    #[test]
    fn an_empty_cooldown_block_reads_either_way() {
        assert!(read_cooldown_block(&mut &[0u8][..]).unwrap().is_empty());
        assert!(read_cooldown_block(&mut &[0u8, 0][..]).unwrap().is_empty());
    }

    /// The teardown: an 8-byte body of zero guid ⇒ a default value whose `pet_guid` is 0. This is
    /// the ONLY signal that the bar has gone away, so it must not read as a parse error.
    #[test]
    fn the_guid_only_body_is_the_teardown() {
        let body = 0u64.to_le_bytes();
        let mut r = &body[..];
        let p = read_pet_spells(&mut r).unwrap();
        assert_eq!(p, PetSpells::default());
        assert_eq!(p.pet_guid, 0);
    }

    /// A truncated bar is a parse error, not a half-read bar — the failure mode that would
    /// otherwise paint 4 real slots and 6 zeroes.
    #[test]
    fn a_truncated_body_errors() {
        let body = pet_spells_body();
        let mut r = &body[..20];
        assert!(read_pet_spells(&mut r).is_err());
    }

    #[test]
    fn pet_mode_and_the_client_verbs_golden() {
        // SMSG_PET_MODE: guid + the same state dword, no bar behind it. vmangos's fourth byte 0x8
        // IS the client's bit 27 — the two ends of the same bit.
        let mut body = 0x2Au64.to_le_bytes().to_vec();
        body.extend_from_slice(&[2, 0, 0, 0x8]);
        let mut r = &body[..];
        let mode = read_pet_mode(&mut r).unwrap();
        assert_eq!(mode.pet_guid, 0x2A);
        assert_eq!(mode.state, 0x0800_0002);
        assert_eq!(mode.state & PET_STATE_BAR_DISABLED, PET_STATE_BAR_DISABLED);

        // CMSG_PET_ACTION: guid, the slot word VERBATIM, target guid.
        let packed = 3010 | (u32::from(PET_ACT_ENABLED) << 24);
        assert_eq!(
            pet_action(0x2A, packed, 0x99),
            [
                0x2Au64.to_le_bytes().to_vec(),
                packed.to_le_bytes().to_vec(),
                0x99u64.to_le_bytes().to_vec(),
            ]
            .concat()
        );

        // CMSG_PET_STOP_ATTACK: the lone guid.
        assert_eq!(pet_stop_attack(0x2A), 0x2Au64.to_le_bytes());

        // CMSG_PET_CANCEL_AURA: guid then spell id, 12 bytes — the guid is the whole difference
        // from the player's own CMSG_CANCEL_AURA, which is a bare u32.
        assert_eq!(
            pet_cancel_aura(0x2A, 2645),
            [
                0x2Au64.to_le_bytes().to_vec(),
                2645u32.to_le_bytes().to_vec()
            ]
            .concat()
        );
        assert_eq!(pet_cancel_aura(0x2A, 2645).len(), 12);

        // CMSG_PET_SET_ACTION: the server tells one entry from two BY BODY SIZE, so the one-entry
        // form is 16 bytes and the two-entry form is exactly 24.
        assert_eq!(pet_set_action(0x2A, &[(3, packed)]).len(), 16);
        assert_eq!(pet_set_action(0x2A, &[(3, packed), (4, 0)]).len(), 24);

        // CMSG_PET_ABANDON: the lone guid — the same shape as stop-attack, a different verb.
        assert_eq!(pet_abandon(0x2A), 0x2Au64.to_le_bytes());

        // CMSG_PET_RENAME: guid then a NUL-TERMINATED name. The terminator is the whole framing —
        // there is no length prefix and no padding, so dropping it would leave the server reading
        // the name off the end of the body.
        assert_eq!(
            pet_rename(0x2A, "Bruce"),
            [&0x2Au64.to_le_bytes()[..], b"Bruce\0"].concat()
        );
        assert_eq!(
            pet_rename(0x2A, "").len(),
            9,
            "an empty name is still framed"
        );
    }

    /// The autocast flip touches **bit 30 and nothing else** — the client's own in-place edit, and
    /// the word it produces is exactly what `CMSG_PET_SET_ACTION` then carries.
    #[test]
    fn the_autocast_flip_moves_only_bit_30() {
        let on = PetActionEntry::from(3010 | (u32::from(PET_ACT_ENABLED) << 24));
        assert!(on.autocast_allowed() && on.autocast_on());

        let off = on.with_autocast(false);
        assert_eq!(off.packed, 3010 | (u32::from(PET_ACT_DISABLED) << 24));
        assert_eq!(off.action(), 3010);
        assert_eq!(off.kind(), PET_TYPE_SPELL_FIRST, "the TYPE never moves");
        assert!(off.autocast_allowed() && !off.autocast_on());
        assert_eq!(off.with_autocast(true), on);

        // A passive spell can be asked to autocast and the word still says it may not — bit 31 is
        // the server's to grant, and flipping 30 does not forge it.
        let passive = PetActionEntry::from(3010 | (u32::from(PET_ACT_PASSIVE) << 24));
        assert!(!passive.with_autocast(true).autocast_allowed());
    }

    /// The `& 0x3F` mask, stated on its own: the server's five `ActiveStates` land on three client
    /// branches, and the two dropped bits reappear as the autocast pair.
    #[test]
    fn the_type_mask_collapses_the_spell_states() {
        for act in [PET_ACT_PASSIVE, PET_ACT_DISABLED, PET_ACT_ENABLED] {
            let e = PetActionEntry::from(3010 | (u32::from(act) << 24));
            assert_eq!(e.kind(), PET_TYPE_SPELL_FIRST, "{act:#04x} masks to 1");
            assert!(e.is_spell());
        }
        assert_eq!(
            PetActionEntry::from(u32::from(PET_ACT_COMMAND) << 24).kind(),
            PET_ACT_COMMAND,
            "the token types are below the mask, so they survive it unchanged"
        );
        assert_eq!(
            PetActionEntry::from(u32::from(PET_ACT_REACTION) << 24).kind(),
            PET_ACT_REACTION
        );

        // A type outside 1-7 is the client's default arm; we treat it as nothing at all rather
        // than reproduce its under-push (wow-re §2.5 quirk 1).
        let odd = PetActionEntry::from(3010 | (0x33u32 << 24));
        assert!(!odd.is_spell());
        assert!(odd.kind() != PET_ACT_COMMAND && odd.kind() != PET_ACT_REACTION);
    }
}
