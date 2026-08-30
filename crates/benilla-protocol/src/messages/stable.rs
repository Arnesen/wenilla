//! Stable-master messages — the hunter pet stable's wire (opcodes 623-629, vmangos
//! `Opcodes_1_12_1.h`, VERIFIED). Bodies from vmangos `NPCHandler.cpp` (`SendStablePet` +
//! the five `HandleStable*` handlers) and the hand-serialized `Server/Packets/Npc.{h,cpp}`
//! (`ListStabledPets`/`StablePet`/`UnstablePet`/`BuyStableSlot`/`StableSwapPet`/`StableResult`).
//!
//! **The shape of the system.** A stable master keeps a hunter's pets in **three places**: the one
//! *current* pet walking beside the player, plus up to two *stable* slots that must each be bought
//! with gold (`Player::m_stableSlots`, `StableSlotPrices.dbc`). The window (`PetStableFrame`) is
//! opened by the gossip stable option, which makes the server send `MSG_LIST_STABLED_PETS`
//! unprompted — the same opcode the client sends back as the *refresh* verb (it is an `MSG_`, one
//! number in both directions). Every mutation is a single-verb ask answered by one
//! `SMSG_STABLE_RESULT` byte and **nothing else**: the list is not resent, so a client that acted
//! successfully re-asks with [`list_stabled_pets`] to repaint.
//!
//! **Slots are 1-based on the wire and 0-based in the client**, and this module absorbs the
//! difference exactly once — see [`StabledPet::slot`]. The rest of benilla only ever sees the
//! client's own indices (`0` = the current pet, `1..=NUM_PET_STABLE_SLOTS`).
//!
//! Only *hunter* pets reach the stable: vmangos gates every verb on `HUNTER_PET`, so a warlock at a
//! stable master gets an empty list. Decision 1676.

use std::io;

use crate::wire::{read_cstring, read_u32_le, read_u64_le, read_u8};

/// One row of `MSG_LIST_STABLED_PETS` (vmangos `WorldSession::SendStablePet`,
/// `NPCHandler.cpp:522-575`) — a pet the stable master can show, whether it is the one currently at
/// the player's side or one asleep in a stable slot.
///
/// The wire record is `u32 petNumber, u32 creatureEntry, u32 level, cstring name, u32 loyalty,
/// u8 slot`. Note what is *not* on it: no display id and no family — a stabled pet has no world
/// object to read either from, so drawing its portrait takes a `CMSG_CREATURE_QUERY` on
/// [`creature_entry`](Self::creature_entry) (which is why that response's display id is kept,
/// decision 1676).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StabledPet {
    /// The pet's own id — vmangos's `character_pet.id`, the same number
    /// [`crate::guid::pet_number`] reads out of a live pet's guid. This, **not** the slot, is what
    /// [`unstable_pet`] and [`stable_swap_pet`] name.
    pub pet_number: u32,
    /// `creature_template` entry (`Pet::GetEntry()` / `character_pet.entry`) — **not** a display
    /// id. Feed it to `CMSG_CREATURE_QUERY` to get the name, family and display id the window's
    /// icon and diet tooltip need.
    pub creature_entry: u32,
    /// The pet's level.
    pub level: u32,
    /// The pet's name — the one the hunter gave it, not the creature template's.
    pub name: String,
    /// Loyalty **level** 1..6 (`LoyaltyLevel`, vmangos `Pet.h:57-65`: Rebellious … Best Friend) — a
    /// `PetLoyalty.dbc` row id the client renders as a string, never a string on the wire.
    pub loyalty: u32,
    /// **Client** slot index, already rebased: `0` = the current pet, `1..=2` = the two stable
    /// slots.
    ///
    /// The wire byte is 1-based over vmangos's own 0-based `character_pet.slot`: `SendStablePet`
    /// writes a literal `0x01` for the current pet (`PET_SAVE_AS_CURRENT` = 0) and `it->slot + 1`
    /// for a stabled one, where `PET_SAVE_FIRST_STABLE_SLOT` = 1 and `PET_SAVE_LAST_STABLE_SLOT` =
    /// `MAX_PET_STABLES` = 2 (`Pet.h:37-48`). The reference UI indexes `0` (current) through
    /// `NUM_PET_STABLE_SLOTS`, so **client index = wire slot − 1** — done once, here, so no
    /// consumer has to rediscover a ±1.
    pub slot: u8,
}

/// `SMSG_STABLE_RESULT`'s one `u8` — the whole answer to every stable verb (vmangos's
/// `StableResultCode`, `NPCHandler.cpp:40-47`, VERIFIED). Nothing else comes back: the list is
/// **not** resent, so a success is a cue to re-ask with [`list_stabled_pets`].
pub mod stable_result {
    /// `STABLE_ERR_MONEY` — not enough gold for the next stable slot. Only [`super::buy_stable_slot`]
    /// produces it (`HandleBuyStableSlot`, `NPCHandler.cpp:704-729`).
    pub const ERR_MONEY: u8 = 0x01;
    /// `STABLE_ERR_STABLE` — the catch-all failure, and the answer to *every* refusal except the
    /// money one: dead player, not a real stable master (or out of interact range), no live hunter
    /// pet to stable, all bought slots full, an unknown/untameable pet number, a pet already out
    /// when unstabling, a failed load from the DB, or buying a fourth slot. It carries no reason —
    /// the client cannot tell those cases apart, and neither can we.
    pub const ERR_STABLE: u8 = 0x06;
    /// `STABLE_SUCCESS_STABLE` — the current pet went into the first free stable slot
    /// ([`super::stable_pet`]).
    pub const SUCCESS_STABLE: u8 = 0x08;
    /// `STABLE_SUCCESS_UNSTABLE` — a stabled pet is now the current pet. Sent for **both**
    /// [`super::unstable_pet`] and a successful [`super::stable_swap_pet`] (vmangos
    /// `HandleStableSwapPet` ends on this same code), so it does not identify which verb was used.
    pub const SUCCESS_UNSTABLE: u8 = 0x09;
    /// `STABLE_SUCCESS_BUY_SLOT` — a stable slot was purchased; the gold is already gone and
    /// `m_stableSlots` has advanced, visible on the next list's `num_stable_slots`.
    pub const SUCCESS_BUY_SLOT: u8 = 0x0A;
}

/// Body of `MSG_LIST_STABLED_PETS` sent **client → server** (vmangos `Npc.cpp:51-54`,
/// `ListStabledPets::ReadFromWorldPacket`): one full 8-byte stable-master guid.
///
/// The *refresh* verb, not the open verb: the window first appears off the gossip stable option,
/// which has the server send this same opcode inbound unprompted. Re-ask after any successful
/// mutation — `SMSG_STABLE_RESULT` is the entire answer to those, and the server never resends the
/// list on its own.
pub fn list_stabled_pets(npc_guid: u64) -> Vec<u8> {
    npc_guid.to_le_bytes().to_vec()
}

/// Body of `CMSG_STABLE_PET` (vmangos `Npc.cpp:56-59`, `StablePet::ReadFromWorldPacket`): one full
/// 8-byte stable-master guid.
///
/// **It carries no slot** — the surprising part. The server picks the destination itself:
/// `HandleStablePet` (`NPCHandler.cpp:609-655`) walks `PET_SAVE_FIRST_STABLE_SLOT..=
/// PET_SAVE_LAST_STABLE_SLOT`, takes the first slot no `character_pet` row occupies, and refuses
/// with [`stable_result::ERR_STABLE`] if that index exceeds the number of slots the player has
/// bought. So there is no "stable into slot 2" intent to express, and a UI that lets the player
/// drop a pet onto a particular empty slot is describing a placement the wire cannot carry.
pub fn stable_pet(npc_guid: u64) -> Vec<u8> {
    npc_guid.to_le_bytes().to_vec()
}

/// Body of `CMSG_UNSTABLE_PET` (vmangos `Npc.cpp:61-65`, `UnstablePet::ReadFromWorldPacket`): `u64
/// npcGuid, u32 petNumber` — the [`StabledPet::pet_number`] of the chosen row, never its slot.
///
/// Summons that pet as the current pet. Refused with [`stable_result::ERR_STABLE`] when the player
/// already *has* a pet — even an unsummoned one that is merely out of range
/// (`HandleUnstablePet`, `NPCHandler.cpp:657-702`); trading places is [`stable_swap_pet`]'s job.
pub fn unstable_pet(npc_guid: u64, pet_number: u32) -> Vec<u8> {
    let mut body = Vec::with_capacity(12);
    body.extend_from_slice(&npc_guid.to_le_bytes());
    body.extend_from_slice(&pet_number.to_le_bytes());
    body
}

/// Body of `CMSG_BUY_STABLE_SLOT` (vmangos `Npc.cpp:67-70`, `BuyStableSlot::ReadFromWorldPacket`):
/// one full 8-byte stable-master guid.
///
/// Like the bank's slot purchase, the *which* is implicit: the server buys slot
/// `m_stableSlots + 1`, prices it from `StableSlotPrices.dbc` at that row, and answers
/// [`stable_result::SUCCESS_BUY_SLOT`], [`stable_result::ERR_MONEY`], or — past `MAX_PET_STABLES`
/// (2) — [`stable_result::ERR_STABLE`] (`HandleBuyStableSlot`, `NPCHandler.cpp:704-729`). Knowing
/// the next price is the client's own job, read from that DBC.
pub fn buy_stable_slot(npc_guid: u64) -> Vec<u8> {
    npc_guid.to_le_bytes().to_vec()
}

/// Body of `CMSG_STABLE_SWAP_PET` (vmangos `Npc.cpp:72-76`, `StableSwapPet::ReadFromWorldPacket`):
/// `u64 npcGuid, u32 petNumber` — same shape as [`unstable_pet`], and the verb to use when the
/// player *already* has a current pet.
///
/// The current pet takes the named pet's stable slot and the named pet is summoned, in one step
/// (`HandleStableSwapPet`, `NPCHandler.cpp:735-789`). Success answers
/// [`stable_result::SUCCESS_UNSTABLE`] — the same code a plain unstable returns.
pub fn stable_swap_pet(npc_guid: u64, pet_number: u32) -> Vec<u8> {
    let mut body = Vec::with_capacity(12);
    body.extend_from_slice(&npc_guid.to_le_bytes());
    body.extend_from_slice(&pet_number.to_le_bytes());
    body
}

// No `CMSG_STABLE_REVIVE_PET` (0x0274) send verb, deliberately: vmangos's
// `HandleStableRevivePet` (`NPCHandler.cpp:731-733`) is an empty no-op, and whether the 5875 client
// ever puts that opcode on the wire is an open RE question — nothing here can be verified against a
// handler that does nothing. If a capture ever shows the reference client sending it, the verb gets
// added then, from that evidence (decision 1676).

/// Read `MSG_LIST_STABLED_PETS` arriving **server → client** (vmangos
/// `WorldSession::SendStablePet`, `NPCHandler.cpp:522-575`): `u64 npcGuid, u8 numPets, u8
/// numStableSlots, numPets ×` [`StabledPet`] (a variable-length record — the name is a cstring).
/// Returns `(npc, num_stable_slots, pets)`.
///
/// `num_stable_slots` is `Player::m_stableSlots` — how many slots are **purchased** (0..=2), which
/// is what greys the unbought ones and drives the "buy the next slot" price line; it is *not* a
/// count of occupied slots.
///
/// **The current pet's row may be absent.** vmangos emits a slot-0 row only when the player has a
/// live `HUNTER_PET` or a cached `character_pet` for one that has despawned; a warlock, or a hunter
/// with no pet at all, simply gets a list whose rows are all stabled ones (or none). So `pets[0]`
/// is not the current pet — the rows are keyed by their own [`StabledPet::slot`], and a consumer
/// must look the slot up rather than index positionally.
pub(super) fn read_list_stabled_pets(r: &mut &[u8]) -> io::Result<(u64, u8, Vec<StabledPet>)> {
    let npc = read_u64_le(r)?;
    let num_pets = read_u8(r)?;
    let num_stable_slots = read_u8(r)?;
    let mut pets = Vec::with_capacity(num_pets as usize);
    for _ in 0..num_pets {
        // Struct-literal fields evaluate top-to-bottom, so this reads in wire order.
        pets.push(StabledPet {
            pet_number: read_u32_le(r)?,
            creature_entry: read_u32_le(r)?,
            level: read_u32_le(r)?,
            name: read_cstring(r)?,
            loyalty: read_u32_le(r)?,
            // Wire slot is 1-based (see `StabledPet::slot`); rebase to the client's own index here,
            // once. `saturating_sub` rather than `- 1` only so a malformed 0 can't wrap to 255 —
            // vmangos never writes one.
            slot: read_u8(r)?.saturating_sub(1),
        });
    }
    Ok((npc, num_stable_slots, pets))
}

/// Read `SMSG_STABLE_RESULT` (vmangos `StableResult::AppendBodyTo`, `Npc.cpp:99-102`): one
/// [`stable_result`] byte, and the complete answer to every stable verb.
pub(super) fn read_stable_result(r: &mut &[u8]) -> io::Result<u8> {
    read_u8(r)
}
