//! The petition family — the guild-charter flow that *founds* a guild (opcodes `0x1BB`–`0x1C7`
//! plus `0x2C1`).
//!
//! [`super::guild`] is about *being* in a guild; this is about *making* one. At 1.12 those are
//! entirely different wires: `CMSG_GUILD_CREATE` is registered `STATUS_NEVER` on vmangos, and the
//! only path to a new guild is buy a charter → collect nine signatures → turn it in.
//!
//! The server side is pinned at the bytes by vmangos `Server/Packets/Petition.{h,cpp}` (the
//! `ReadFromWorldPacket` / `AppendBodyTo` pairs) and `Handlers/PetitionsHandler.cpp` (which
//! packets each handler answers with, and the order of its refusals). **Two of the five
//! server→client packets are not built through the packet classes at all** —
//! `SMSG_PETITION_SHOW_SIGNATURES` is hand-assembled at `PetitionsHandler.cpp:160-168` and
//! `:390-397` — so their layout is read off the handler, not off `Petition.cpp`.
//!
//! Four shapes of this family are worth knowing before touching any of it:
//!
//! - **The charter is an ITEM, and the petition id rides in its enchantment slot.** Buying stores
//!   item [`CHARTER_ITEM_ENTRY`] and writes the petition id into `ITEM_FIELD_ENCHANTMENT` slot 0's
//!   `id` dword (`PetitionsHandler.cpp:126`). That is not a hack the client is unaware of: the
//!   reference's item tooltip **forces enchantment slot 0 to zero and skips the enchant lines
//!   entirely** for any item whose template flags carry [`ITEM_FLAG_CHARTER`]
//!   (`test ah,0x20 / jne 0x52c9ee`, wow-re `system/ui/scratch/tooltip-content-law.md:485-505`).
//!   So slot 0 on a charter is a petition id by contract on both ends, and nothing may read it as
//!   a `SpellItemEnchantment.dbc` row.
//! - **Almost nothing is acked positively.** A successful buy sends **no confirmation packet** —
//!   the client learns of it only from the `SMSG_ITEM_PUSH_RESULT` for the new item
//!   (`PetitionsHandler.cpp:130`). Rename echoes only on success (`:209-215`). The refusals come
//!   back as `SMSG_GUILD_COMMAND_RESULT`, the *guild* family's error channel, reused here.
//! - **`SMSG_PETITION_SHOW_SIGNATURES` is the answer to two different questions.** It answers our
//!   own `CMSG_PETITION_SHOW_SIGNATURES` (`:160-168`), and it is also what a *third party* is sent
//!   when someone offers us their charter (`:390-397`, the `CMSG_OFFER_PETITION` path). One
//!   inbound packet, two meanings, distinguished only by whether its `owner` is us.
//! - **The signature limit is enforced three times with three different numbers.** vmangos's
//!   `MinPetitionSigns` config (clamped 0..=9, `World.cpp:665`) decides when a petition
//!   `IsComplete()` — with `==`, not `>=` (`GuildMgr.h:124`); the sign handler *separately*
//!   hard-caps at nine with the comment "Client hard limit at 9 signatures"
//!   (`PetitionsHandler.cpp:269-271`); and `SMSG_PETITION_QUERY_RESPONSE` reports min = max = 9
//!   hardcoded (`:182-183`), regardless of the config. [`MAX_PETITION_SIGNATURES`] is that client
//!   limit; a consumer wanting "how many does this petition need" reads the wire's
//!   `max_signatures`, not the constant, because on a server with `MinPetitionSigns` lowered the
//!   two disagree.

use std::io::{self, Read};

use crate::wire::{read_cstring, read_i32_le, read_u16_le, read_u32_le, read_u64_le, read_u8};

/// The one item that is a guild charter — vmangos `GUILD_CHARTER` (`PetitionsHandler.cpp:37`),
/// also `Item::IsCharter`'s literal (`Item.h:158`, `GetEntry() == 5863u`). Confirmed against this
/// deployment's world DB: entry 5863 "Guild Charter", display 16161, flags `0x2000`.
pub const CHARTER_ITEM_ENTRY: u32 = 5863;

/// The charter's inventory display id — vmangos `CHARTER_DISPLAY_ID` (`PetitionsHandler.cpp:38`).
/// It rides in [`PetitionShowListEntry::charter_display_id`] as well as in the item template, and
/// the two agree; the packet's copy exists so a vendor list can be drawn before the item is owned.
pub const CHARTER_DISPLAY_ID: u32 = 16161;

/// `ITEM_FLAG_CHARTER` — the template-flag bit that makes an item a signable petition
/// (vmangos `ItemPrototype.h:77`). **The reference client keys on this same bit** to print the
/// green `ITEM_SIGNABLE` tooltip line and to suppress the enchantment lines
/// (wow-re `system/ui/scratch/tooltip-content-law.md:485-505`), so it is a shared contract rather
/// than a server-side convenience.
pub const ITEM_FLAG_CHARTER: u32 = 0x0000_2000;

/// The most signatures a 1.12 charter can hold — vmangos's own "Client hard limit at 9
/// signatures" (`PetitionsHandler.cpp:269-271`), and the reference FrameXML's
/// `MAX_PETITION_SIGNATURES = 9` (`PetitionFrame.lua:1`), which sizes the window's name rows.
///
/// This is the **row count**, not the requirement: how many a petition needs to be turned in is
/// [`PetitionQueryResponse::min_signatures`] off the wire, which a server may configure lower.
pub const MAX_PETITION_SIGNATURES: usize = 9;

/// Charter-name cap, in *characters* — vmangos `MAX_CHARTER_NAME` (`ObjectMgr.h:401`), enforced by
/// `ObjectMgr::IsValidCharterName` (`ObjectMgr.cpp:9322-9334`) as a UTF-8 length. The reference's
/// own edit boxes agree: `GuildRegistrarFrameEditBox letters="24"` and the `RENAME_GUILD` popup's
/// `maxLetters = 24`. Same number as [`super::GUILD_NAME_MAX_LENGTH`], and not a coincidence — a
/// charter name becomes a guild name.
pub const CHARTER_NAME_MAX_LENGTH: usize = 24;

/// `SMSG_PETITION_SIGN_RESULTS`' and `SMSG_TURN_IN_PETITION_RESULTS`' shared result code —
/// vmangos `PetitionSigns` (`Guild/Guild.h:147-154`). One enum, two packets, and the two use
/// **disjoint subsets** of it, which is why the arms are documented by packet.
pub mod petition_result {
    /// Signed, or turned in. Used by both packets.
    pub const OK: u32 = 0;
    /// Sign only — this account has already signed this charter. vmangos checks uniqueness **per
    /// account first**, then per character (`GuildMgr.cpp:388-400`), so a second character on one
    /// account gets this, not a second signature.
    pub const ALREADY_SIGNED: u32 = 1;
    /// Turn-in only — the would-be founder is already in a guild (`PetitionsHandler.cpp:427`).
    /// Never sent by the sign path, which answers an already-guilded *signer* with an
    /// `SMSG_GUILD_COMMAND_RESULT` instead (`:258`).
    pub const ALREADY_IN_GUILD: u32 = 2;
    /// Sign only — you cannot sign your own charter (`PetitionsHandler.cpp:237`).
    pub const CANT_SIGN_OWN: u32 = 3;
    /// Turn-in only — not enough signatures yet (`PetitionsHandler.cpp:438`).
    pub const NEED_MORE: u32 = 4;
    /// Not on the same realm. vmangos never sends it (cross-realm charters cannot arise on a
    /// single-realm server); modelled because the reference has a string for it
    /// (`ERR_PETITION_NOT_SAME_SERVER`) and the client's switch must have an arm.
    pub const NOT_SERVER: u32 = 5;
}

/// One row of `SMSG_PETITION_SHOWLIST` — a charter a petitioner NPC will sell.
///
/// vmangos always sends exactly one of these, and its own header says **"only 1 element is
/// supported in the client"** (`Petition.h:176`). We parse the counted list faithfully anyway: a
/// count is a count, and a reader that assumes one would desynchronise rather than degrade if it
/// were ever wrong.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct PetitionShowListEntry {
    /// Row index, 1-based (`PetitionsHandler.cpp:501`, a literal `1`).
    pub index: u32,
    /// The item entry sold — [`CHARTER_ITEM_ENTRY`] in practice.
    pub charter_entry: u32,
    /// Its display id — [`CHARTER_DISPLAY_ID`] in practice.
    pub charter_display_id: u32,
    /// Price in **copper** (1000 = 10 silver on vmangos, `PetitionsHandler.cpp:39`). Signed on the
    /// wire (`int32`), which is why it is signed here: a reader that narrows it to `u32` would be
    /// asserting a range the sender does not.
    pub charter_cost: i32,
    /// vmangos sends `1` and its header says the row **"must be `&1` to show it in the UI"**
    /// (`Petition.h:169`). Kept raw rather than folded into a `bool`: that claim is a *comment*
    /// about the reference client, not something vmangos itself enforces, and a consumer that
    /// wants the filter can apply it knowingly.
    pub entry_flags: i32,
}

/// `SMSG_PETITION_SHOWLIST` — a petitioner NPC's "here is what I sell" list. In 1.12 that is
/// always the single guild charter, and this packet is what puts the guild registrar's window on
/// screen.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct PetitionShowList {
    /// The NPC we are talking to. Every later verb in the flow that names an NPC uses this guid.
    pub npc: u64,
    /// The rows, in wire order. Normally exactly one; see [`PetitionShowListEntry`].
    pub entries: Vec<PetitionShowListEntry>,
}

/// Read `SMSG_PETITION_SHOWLIST` (VERIFIED vmangos `Server/Packets/Petition.cpp:115-127`
/// `PetitionShowList::AppendBodyTo`, filled at `Handlers/PetitionsHandler.cpp:482-507`):
/// `u64 npcGuid`, `u8 count`, then per row `u32 index`, `u32 charterEntry`, `u32 charterDisplayId`,
/// `i32 charterCost`, `i32 entryFlags`.
///
/// The send is gated on `GetNPCIfCanInteractWith(guid, UNIT_NPC_FLAG_PETITIONER)`
/// (`:484`; the flag is `0x200`, `Objects/UnitDefines.h:666`), so a list arriving at all is the
/// server's confirmation that we are in range of a real registrar.
pub(super) fn read_petition_show_list(r: &mut impl Read) -> io::Result<PetitionShowList> {
    let npc = read_u64_le(r)?;
    let count = read_u8(r)?;
    let mut entries = Vec::with_capacity(count as usize);
    for _ in 0..count {
        entries.push(PetitionShowListEntry {
            index: read_u32_le(r)?,
            charter_entry: read_u32_le(r)?,
            charter_display_id: read_u32_le(r)?,
            charter_cost: read_i32_le(r)?,
            entry_flags: read_i32_le(r)?,
        });
    }
    Ok(PetitionShowList { npc, entries })
}

/// One signature on a charter — a signer's guid, and the dword that always follows it.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct PetitionSignature {
    /// The signing character's guid. Turning it into a *name* is the client's job: the packet
    /// carries no names at all.
    pub signer: u64,
    /// The trailing dword, which vmangos writes as a literal `0`
    /// (`Guild/GuildMgr.cpp:358-366`, `data << 0`). Kept rather than skipped so the field is
    /// visible in a dump and a non-zero value from another server would be *noticed* rather than
    /// silently consumed.
    pub unknown: u32,
}

/// `SMSG_PETITION_SHOW_SIGNATURES` — who has signed a charter. **The answer to two different
/// questions** (module doc): our own `CMSG_PETITION_SHOW_SIGNATURES`, and someone else's
/// `CMSG_OFFER_PETITION` aimed at us.
///
/// Note what is *not* here: the guild name, the body text, and the signature requirement. Those
/// live only in `SMSG_PETITION_QUERY_RESPONSE`, keyed by [`Self::petition_id`] — the same
/// two-caches shape [`super::guild`] has, where the roster carries no guild name.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct PetitionShowSignatures {
    /// The charter **item's** guid — the handle every later verb takes (sign, offer, rename, turn
    /// in, decline). Not the petition id.
    pub item: u64,
    /// The charter owner's character guid. Whether this is us is what makes the window the
    /// leader's view or a signer's view.
    pub owner: u64,
    /// The petition id — the key `CMSG_PETITION_QUERY` takes, and the same value the charter
    /// item's enchantment slot 0 carries (module doc).
    pub petition_id: u32,
    /// The signatures so far, in wire order.
    pub signatures: Vec<PetitionSignature>,
}

/// Read `SMSG_PETITION_SHOW_SIGNATURES` (VERIFIED vmangos `Handlers/PetitionsHandler.cpp:160-168`
/// and `:390-397`, which build it **by hand** rather than through a packet class):
/// `u64 itemGuid`, `u64 ownerGuid`, `u32 petitionId`, `u8 signatureCount`, then per signature
/// `u64 signerGuid` + `u32 0`.
///
/// The 12-byte stride is corroborated by the sender's own size hint `8+8+4+1 + signs*12` and by
/// `Guild/GuildMgr.cpp:358-366` (`Petition::BuildSignatureData`).
pub(super) fn read_petition_show_signatures(
    r: &mut impl Read,
) -> io::Result<PetitionShowSignatures> {
    let item = read_u64_le(r)?;
    let owner = read_u64_le(r)?;
    let petition_id = read_u32_le(r)?;
    let count = read_u8(r)?;
    let mut signatures = Vec::with_capacity(count as usize);
    for _ in 0..count {
        signatures.push(PetitionSignature {
            signer: read_u64_le(r)?,
            unknown: read_u32_le(r)?,
        });
    }
    Ok(PetitionShowSignatures {
        item,
        owner,
        petition_id,
        signatures,
    })
}

/// `SMSG_PETITION_SIGN_RESULTS` — the verdict on one signature attempt.
///
/// **It is sent to both parties on success**: once to the signer (`PetitionsHandler.cpp:299`) and
/// again, identical, to the charter's owner if they are online (`:312-316`). Both copies name the
/// *signer* in [`Self::player`], so a receiver cannot tell the two apart from this packet alone —
/// only from whether it holds that charter.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct PetitionSignResults {
    /// The charter item's guid.
    pub item: u64,
    /// The **signer's** guid — in both copies of the packet, per the doc above.
    pub player: u64,
    /// A [`petition_result`] code.
    pub result: u32,
}

/// Read `SMSG_PETITION_SIGN_RESULTS` (VERIFIED vmangos `Server/Packets/Petition.cpp:69-74`
/// `PetitionSignResults::AppendBodyTo`): `u64 itemGuid`, `u64 playerGuid`, `u32 result`.
pub(super) fn read_petition_sign_results(r: &mut impl Read) -> io::Result<PetitionSignResults> {
    Ok(PetitionSignResults {
        item: read_u64_le(r)?,
        player: read_u64_le(r)?,
        result: read_u32_le(r)?,
    })
}

/// `SMSG_PETITION_QUERY_RESPONSE` — a petition's own record, keyed by petition id.
///
/// Sixteen fields, of which vmangos fills three with anything but a constant, and whose own header
/// says *"all those fields below don't really change anything in the UI"* (`Petition.h:126`). They
/// are parsed in full regardless: the trailing `choice` list is length-prefixed, so skipping the
/// middle would make the tail unreadable, and a field that is constant on this server is not
/// thereby constant on the wire.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct PetitionQueryResponse {
    /// The petition id we asked about.
    pub petition_id: u32,
    /// The charter owner's character guid.
    pub owner: u64,
    /// The proposed guild's name. This is the only place it exists on the wire.
    pub name: String,
    /// Free text on the charter — always empty on vmangos (`PetitionsHandler.cpp:178`).
    pub body_text: String,
    /// vmangos sends `1` (`:179`). Meaning unknown; kept because it is a real wire field.
    pub flags: u32,
    /// How many signatures are **required**. vmangos hardcodes 9 here even when its
    /// `MinPetitionSigns` config is lower (module doc), so this is the number the reference's
    /// Request-Signature button disables against, and it need not equal
    /// [`MAX_PETITION_SIGNATURES`].
    pub min_signatures: u32,
    /// How many signatures fit. vmangos hardcodes 9 (`:183`).
    pub max_signatures: u32,
    /// Deadline timestamp — `0` on vmangos.
    pub deadline: u32,
    /// Creation timestamp — `0` on vmangos.
    pub creation: u32,
    /// Restricting guild id — `0` on vmangos.
    pub allowed_guild_id: u32,
    /// Class mask restriction — `0` on vmangos.
    pub allowed_classes: u32,
    /// Race mask restriction — `0` on vmangos.
    pub allowed_races: u32,
    /// Gender restriction — **`u16` on the wire, not `u32`** (`Petition.cpp:94`). The one odd
    /// width in the packet, and misreading it as a dword shifts every field after it by two bytes.
    pub allowed_gender: u16,
    /// Minimum level — `0` on vmangos.
    pub allowed_min_level: u32,
    /// Maximum level — `0` on vmangos.
    pub allowed_max_level: u32,
    /// The multiple-choice options this petition offers — always empty on vmangos, and the reason
    /// the tail cannot be skipped: the count is what says where `default_choice` starts.
    pub choices: Vec<String>,
    /// Which choice is pre-selected — `0` on vmangos.
    pub default_choice: u32,
}

/// Read `SMSG_PETITION_QUERY_RESPONSE` (VERIFIED vmangos `Server/Packets/Petition.cpp:81-102`
/// `PetitionQueryResponse::AppendBodyTo`, filled at `Handlers/PetitionsHandler.cpp:177-184`):
/// `u32 petitionId`, `u64 ownerGuid`, cstring name, cstring bodyText, `u32 flags`,
/// `u32 minSignatures`, `u32 maxSignatures`, `u32 deadline`, `u32 creation`, `u32 allowedGuildID`,
/// `u32 allowedClasses`, `u32 allowedRaces`, **`u16 allowedGender`**, `u32 allowedMinLevel`,
/// `u32 allowedMaxLevel`, `u32 choiceCount`, `choiceCount ×` cstring, `u32 defaultChoice`.
///
/// The `u16` in the middle is the trap: see [`PetitionQueryResponse::allowed_gender`].
pub(super) fn read_petition_query_response(r: &mut impl Read) -> io::Result<PetitionQueryResponse> {
    let petition_id = read_u32_le(r)?;
    let owner = read_u64_le(r)?;
    let name = read_cstring(r)?;
    let body_text = read_cstring(r)?;
    let flags = read_u32_le(r)?;
    let min_signatures = read_u32_le(r)?;
    let max_signatures = read_u32_le(r)?;
    let deadline = read_u32_le(r)?;
    let creation = read_u32_le(r)?;
    let allowed_guild_id = read_u32_le(r)?;
    let allowed_classes = read_u32_le(r)?;
    let allowed_races = read_u32_le(r)?;
    let allowed_gender = read_u16_le(r)?;
    let allowed_min_level = read_u32_le(r)?;
    let allowed_max_level = read_u32_le(r)?;
    let choice_count = read_u32_le(r)?;
    let mut choices = Vec::with_capacity(choice_count.min(64) as usize);
    for _ in 0..choice_count {
        choices.push(read_cstring(r)?);
    }
    Ok(PetitionQueryResponse {
        petition_id,
        owner,
        name,
        body_text,
        flags,
        min_signatures,
        max_signatures,
        deadline,
        creation,
        allowed_guild_id,
        allowed_classes,
        allowed_races,
        allowed_gender,
        allowed_min_level,
        allowed_max_level,
        choices,
        default_choice: read_u32_le(r)?,
    })
}

/// `MSG_PETITION_RENAME` — the server's echo of a successful rename.
///
/// Sent **only on success** (`PetitionsHandler.cpp:209-215`); a rejected name comes back as an
/// `SMSG_GUILD_COMMAND_RESULT` instead. Note the handler does **no ownership check** — anyone
/// holding the item may rename it.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct PetitionRename {
    /// The charter item's guid.
    pub item: u64,
    /// Its new name.
    pub name: String,
}

/// Read `MSG_PETITION_RENAME` (VERIFIED vmangos `Server/Packets/Petition.cpp:104-108`
/// `PetitionRename::AppendBodyTo`): `u64 itemGuid`, cstring newName. The same two fields the
/// client sends, in the same order — which is what makes it an `MSG_` rather than a pair.
pub(super) fn read_petition_rename(r: &mut impl Read) -> io::Result<PetitionRename> {
    Ok(PetitionRename {
        item: read_u64_le(r)?,
        name: read_cstring(r)?,
    })
}

/// Read `SMSG_TURN_IN_PETITION_RESULTS` (VERIFIED vmangos `Server/Packets/Petition.cpp:76-79`
/// `TurnInPetitionResults::AppendBodyTo`): one `u32`, a [`petition_result`] code — and **only** the
/// code. Which charter it refers to is implicit; there can be only one in flight.
///
/// The three codes vmangos actually sends here are [`petition_result::OK`] (`:473`),
/// [`petition_result::ALREADY_IN_GUILD`] (`:427`) and [`petition_result::NEED_MORE`] (`:438`). A
/// name collision is reported as `SMSG_GUILD_COMMAND_RESULT` instead and produces no results
/// packet at all (`:445`), so "no answer" is a real outcome of a turn-in.
pub(super) fn read_turn_in_petition_results(r: &mut impl Read) -> io::Result<u32> {
    read_u32_le(r)
}

/// Read `MSG_PETITION_DECLINE`'s **inbound** form (VERIFIED vmangos
/// `Server/Packets/Petition.cpp:110-113` `PetitionDecline::AppendBodyTo`): one `u64`, the guid of
/// the player who declined. Delivered only to the charter's owner
/// (`Handlers/PetitionsHandler.cpp:321-338`).
///
/// The outbound form of the same opcode carries the *item* guid instead
/// ([`petition_decline`]) — one opcode, two different bodies by direction, which is what `MSG_`
/// means here.
pub(super) fn read_petition_decline(r: &mut impl Read) -> io::Result<u64> {
    read_u64_le(r)
}

/// Body of `CMSG_PETITION_SHOWLIST` (VERIFIED vmangos `Server/Packets/Petition.cpp:3-6`
/// `PetitionShowListRequest::ReadFromWorldPacket`): one `u64`, the petitioner NPC's guid.
///
/// Answered by `SMSG_PETITION_SHOWLIST`, or by nothing at all if the NPC is out of range or lacks
/// `UNIT_NPC_FLAG_PETITIONER` (`Handlers/PetitionsHandler.cpp:484`). The server also sends that
/// answer unasked when a `GOSSIP_OPTION_PETITIONER` row is selected (`Player.cpp:12428-12431`), so
/// this request is the *second* way in, not the only one.
pub fn petition_show_list(npc: u64) -> Vec<u8> {
    npc.to_le_bytes().to_vec()
}

/// Body of `CMSG_PETITION_BUY` (VERIFIED vmangos `Server/Packets/Petition.cpp:47-67`
/// `PetitionBuy::ReadFromWorldPacket`) — **72 bytes, of which the server reads exactly two
/// fields**: the NPC guid at offset 0 and the name at offset 20. Everything else is skipped.
///
/// The shape is a generic multi-choice petition struct that never shipped: `u64 npcGuid`,
/// `u32 0`, `u64 0`, cstring name, `10 × u32 0`, `u16 0`, `u8 0`, `u32 index`, `u32 0`. We send
/// zeros for every skipped field — including `index`, which the server's own comment calls
/// "unused" — because a field nobody reads has no correct non-zero value, and a zero is the one
/// choice that cannot encode an accident.
///
/// The refusal ladder is worth knowing, because none of it is a petition packet: an already-taken
/// or invalid name answers `SMSG_GUILD_COMMAND_RESULT(GUILD_CREATE_S, name, …)`
/// (`Handlers/PetitionsHandler.cpp:78`, `:83`, `:93`), too little money answers `SMSG_BUY_FAILED`
/// (`:108`), a full bag answers `SMSG_INVENTORY_CHANGE_FAILURE` (`:116`) — and **success answers
/// nothing but the new item** (`:130`).
pub fn petition_buy(npc: u64, name: &str) -> Vec<u8> {
    let mut body = Vec::with_capacity(72 + name.len());
    body.extend_from_slice(&npc.to_le_bytes());
    body.extend_from_slice(&0u32.to_le_bytes());
    body.extend_from_slice(&0u64.to_le_bytes());
    push_cstring(&mut body, name);
    body.extend_from_slice(&[0u8; 40]); // 10 × u32
    body.extend_from_slice(&0u16.to_le_bytes());
    body.push(0);
    body.extend_from_slice(&0u32.to_le_bytes()); // index — the server's own "unused"
    body.extend_from_slice(&0u32.to_le_bytes());
    body
}

/// Body of `CMSG_PETITION_SHOW_SIGNATURES` (VERIFIED vmangos `Server/Packets/Petition.cpp:8-11`
/// `PetitionShowSignaturesRequest::ReadFromWorldPacket`): one `u64`, the charter **item's** guid.
///
/// Answered with `SMSG_PETITION_SHOW_SIGNATURES` — or silently dropped if we are already in a
/// guild (`Handlers/PetitionsHandler.cpp:140`) or do not hold that item (`:143`).
pub fn petition_show_signatures(item: u64) -> Vec<u8> {
    item.to_le_bytes().to_vec()
}

/// Body of `CMSG_PETITION_SIGN` (VERIFIED vmangos `Server/Packets/Petition.cpp:35-39`
/// `PetitionSign::ReadFromWorldPacket`): `u64 itemGuid` then one `i8` the server **skips**, whose
/// own vmangos comment is *"argument of `/run SignPetition(123)` is never used in the official
/// interface"*.
///
/// **The client's default for that byte is `1`, not `0`** — `SignPetition`'s optional Lua argument
/// initialises `edi = 1` at `0x4f46d9` and the byte goes out through `Put8` at `0x4f4749` (wow-re
/// `system/ui/scratch/petition-charter-api.md`). Since the server reads and discards it, **nothing
/// observable depends on the value**, which is exactly why it shipped here as `0` and why only a
/// golden can tell the difference.
pub fn petition_sign(item: u64, arg: i8) -> Vec<u8> {
    let mut body = Vec::with_capacity(9);
    body.extend_from_slice(&item.to_le_bytes());
    body.push(arg as u8);
    body
}

/// Body of `CMSG_OFFER_PETITION` (VERIFIED vmangos `Server/Packets/Petition.cpp:41-45`
/// `OfferPetition::ReadFromWorldPacket`): `u64 itemGuid`, `u64 playerGuid` — the charter, and who
/// to show it to.
///
/// On success the server sends `SMSG_PETITION_SHOW_SIGNATURES` **to the target**, not back to us
/// (`Handlers/PetitionsHandler.cpp:390-397`); we hear only about refusals, as
/// `SMSG_GUILD_COMMAND_RESULT`.
pub fn offer_petition(item: u64, player: u64) -> Vec<u8> {
    let mut body = Vec::with_capacity(16);
    body.extend_from_slice(&item.to_le_bytes());
    body.extend_from_slice(&player.to_le_bytes());
    body
}

/// Body of `CMSG_TURN_IN_PETITION` (VERIFIED vmangos `Server/Packets/Petition.cpp:24-27`
/// `TurnInPetition::ReadFromWorldPacket`): one `u64`, the charter item's guid.
///
/// Only the petition's **owner** may turn it in; anyone else is refused silently
/// (`Handlers/PetitionsHandler.cpp:432`). On success the guild is created with every signer as a
/// member at the lowest rank, the petition row is deleted, and *then* the item is destroyed — in
/// that order, because destroying a charter cascades into deleting its petition
/// (`Player.cpp:10811-10817`).
pub fn turn_in_petition(item: u64) -> Vec<u8> {
    item.to_le_bytes().to_vec()
}

/// Body of `CMSG_PETITION_QUERY` (VERIFIED vmangos `Server/Packets/Petition.cpp:13-17`
/// `PetitionQuery::ReadFromWorldPacket`): `u32 petitionId`, `u64 itemGuid`.
///
/// The item guid is **read and then ignored** by the handler (`Handlers/PetitionsHandler.cpp:171-185`),
/// which looks the petition up by id alone. Sent anyway, at its true value: a field the current
/// server does not use is not a field to fill with garbage.
pub fn petition_query(petition_id: u32, item: u64) -> Vec<u8> {
    let mut body = Vec::with_capacity(12);
    body.extend_from_slice(&petition_id.to_le_bytes());
    body.extend_from_slice(&item.to_le_bytes());
    body
}

/// Body of `MSG_PETITION_RENAME`'s **outbound** form (VERIFIED vmangos
/// `Server/Packets/Petition.cpp:29-33` `PetitionRename::ReadFromWorldPacket`): `u64 itemGuid`,
/// cstring newName.
///
/// Echoed back as the same two fields on success ([`read_petition_rename`]); rejected names come
/// back as `SMSG_GUILD_COMMAND_RESULT` and no echo. The name is capped at
/// [`CHARTER_NAME_MAX_LENGTH`] characters by the server's own validator — not truncated here, for
/// the reason [`super::guild`]'s caps give: silently shortening sends the wrong text.
pub fn petition_rename(item: u64, name: &str) -> Vec<u8> {
    let mut body = Vec::with_capacity(9 + name.len());
    body.extend_from_slice(&item.to_le_bytes());
    push_cstring(&mut body, name);
    body
}

/// Body of `MSG_PETITION_DECLINE`'s **outbound** form (VERIFIED vmangos
/// `Server/Packets/Petition.cpp:19-22` `PetitionDecline::ReadFromWorldPacket`): one `u64`, the
/// charter item's guid. The server forwards our guid to the charter's owner
/// ([`read_petition_decline`]).
pub fn petition_decline(item: u64) -> Vec<u8> {
    item.to_le_bytes().to_vec()
}

/// Append a NUL-terminated string to `body`.
fn push_cstring(body: &mut Vec<u8>, s: &str) {
    body.extend_from_slice(s.as_bytes());
    body.push(0);
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `CMSG_PETITION_BUY`'s body is a fixed 72 bytes plus the name — the shape vmangos's reader
    /// walks field by field. A body one byte short leaves the reader mid-field and the buy is
    /// silently dropped, which is exactly the failure this pins.
    #[test]
    fn petition_buy_body_is_seventy_two_bytes_plus_the_name() {
        let body = petition_buy(0x1234_5678_9abc_def0, "Test");
        // 8 npc + 4 + 8 + (4+1) name + 40 + 2 + 1 + 4 + 4
        assert_eq!(body.len(), 72 + 4, "72 fixed bytes + a 4-char name");
        assert_eq!(
            petition_buy(0, "").len(),
            72,
            "the empty name still costs its NUL"
        );
    }

    /// The charter constants are a shared contract between server and reference client, not
    /// vmangos policy — the tooltip's signable branch keys on the same `0x2000`.
    #[test]
    fn charter_constants_match_the_world_data() {
        assert_eq!(CHARTER_ITEM_ENTRY, 5863);
        assert_eq!(CHARTER_DISPLAY_ID, 16161);
        assert_eq!(ITEM_FLAG_CHARTER, 0x2000);
        assert_eq!(CHARTER_NAME_MAX_LENGTH, super::super::GUILD_NAME_MAX_LENGTH);
    }
}
