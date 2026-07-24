//! The death-arc messages (decision 0308): release/repop, the corpse query, the reclaim delay +
//! reclaim, the spirit healer, and resurrect requests. Every body shape below is VERIFIED against
//! vmangos source (file:line cites per item); the goldens mirror the server's own packet builds.
//!
//! Not modelled here (known siblings): `SMSG_DURABILITY_DAMAGE_DEATH` (0x2BD — an EMPTY body the
//! server sends with the 10% natural-death durability loss; the real client shows a durability
//! warning — a later polish slice), and the BG-only death family (area spirit healers, the forced
//! BG repop aura 2584 — 0308 defers battlegrounds).

use std::io;

use crate::wire::{read_cstring, read_f32_le, read_i32_le, read_u32_le, read_u64_le, read_u8};

/// `MSG_CORPSE_QUERY`'s answer (opcode `0x216`/534 — VERIFIED vmangos `Opcodes_1_12_1.h:535`; our
/// request is the same opcode with an EMPTY body, `NullClientPacket`). VERIFIED
/// `QueryHandler.cpp:258-304` (`HandleCorpseQueryOpcode`): not-found = the lone `u8(0)`; found =
/// `u8(1), i32 mapid, f32 x, f32 y, f32 z, u32 corpsemapid`.
///
/// The two maps differ deliberately: `display_map`/`position` are **where to walk toward** — for a
/// corpse inside a dungeon they are rewritten to the dungeon's `ghostEntranceMap` + entrance
/// coordinates — while `corpse_map` is always the corpse's real map, untouched. The server also
/// pushes an UNPROMPTED not-found (`u8(0)`) when a lootable corpse converts to bones
/// (`Map.cpp:3624-3629`), so a not-found mid-session means "drop the marker", not "query failed".
#[derive(Debug, Clone, PartialEq)]
pub struct CorpseLocation {
    pub found: bool,
    /// The map to show/route toward (dungeon-entrance-adjusted). `0` when `!found`.
    pub display_map: i32,
    /// Raw WoW coordinates of the corpse (or the dungeon entrance). Zeroed when `!found`.
    pub position: [f32; 3],
    /// The corpse's REAL map id, never adjusted. `0` when `!found`.
    pub corpse_map: u32,
}

/// Read `MSG_CORPSE_QUERY`'s answer — both shapes (see [`CorpseLocation`]).
pub(super) fn read_corpse_query_response(r: &mut &[u8]) -> io::Result<CorpseLocation> {
    let found = read_u8(r)? != 0;
    if !found {
        return Ok(CorpseLocation {
            found: false,
            display_map: 0,
            position: [0.0; 3],
            corpse_map: 0,
        });
    }
    let display_map = read_i32_le(r)?;
    let position = [read_f32_le(r)?, read_f32_le(r)?, read_f32_le(r)?];
    let corpse_map = read_u32_le(r)?;
    Ok(CorpseLocation {
        found: true,
        display_map,
        position,
        corpse_map,
    })
}

/// Read `SMSG_CORPSE_RECLAIM_DELAY` (opcode `0x269`/617 — VERIFIED `Opcodes_1_12_1.h:618`): one
/// `u32`, the delay in **milliseconds** before the corpse can be reclaimed (VERIFIED
/// `Server/Packets/Misc.cpp:653-656` — `buffer << delayMs`). Sent at release
/// (`Player::BuildPlayerRepop`, `Player.cpp:4677`) and at login while dead
/// (`CharacterHandler.cpp:571-572`) — never at the death itself. The value is 30 s base,
/// 60/120 s for repeated deaths within 5-minute steps (`copseReclaimDelay[]`, `Player.cpp:106`).
pub(super) fn read_corpse_reclaim_delay(r: &mut &[u8]) -> io::Result<u32> {
    read_u32_le(r)
}

/// A resurrection offer (`SMSG_RESURRECT_REQUEST`, opcode `0x15B`/347 — VERIFIED
/// `Opcodes_1_12_1.h:348`). VERIFIED `Spell.cpp:5024-5044` (`Spell::SendResurrectRequest`):
/// `u64 caster, u32 nameLen (strlen+1), cstring name, u8 sickness, u8 hasResTimer`.
///
/// `name` is **empty when the caster is a player** (the client resolves the offerer's display name
/// from the guid through the ordinary name-query cache); it carries the localized creature name for
/// an NPC caster. `sickness` warns the accept applies resurrection sickness; `has_timer` says the
/// client should still honor the `SMSG_CORPSE_RECLAIM_DELAY` gate (inverted server-side from
/// `SPELL_ATTR_EX3_NO_RES_TIMER`). The reference UI picks its popup off exactly these two bits
/// (RESURRECT / RESURRECT_NO_SICKNESS / RESURRECT_NO_TIMER — UIParent.lua's RESURRECT_REQUEST arm).
#[derive(Debug, Clone, PartialEq)]
pub struct ResurrectRequestBody {
    pub caster: u64,
    /// Empty for a player caster (resolve by guid); the creature's name for an NPC caster.
    pub name: String,
    pub sickness: bool,
    pub has_timer: bool,
}

/// Read `SMSG_RESURRECT_REQUEST` (see [`ResurrectRequestBody`]). The `u32` length prefix duplicates
/// the C-string's own length (strlen+1) — read and cross-checked loosely (the cstring terminator is
/// authoritative; vmangos always writes both consistently).
pub(super) fn read_resurrect_request(r: &mut &[u8]) -> io::Result<ResurrectRequestBody> {
    let caster = read_u64_le(r)?;
    let _name_len = read_u32_le(r)?;
    let name = read_cstring(r)?;
    let sickness = read_u8(r)? != 0;
    let has_timer = read_u8(r)? != 0;
    Ok(ResurrectRequestBody {
        caster,
        name,
        sickness,
        has_timer,
    })
}

/// Read `SMSG_SPIRIT_HEALER_CONFIRM` (opcode `0x222`/546 — VERIFIED `Opcodes_1_12_1.h:547`): one
/// full `u64`, the spirit-healer NPC's guid. VERIFIED `SpellEffects.cpp:818-831` — vmangos sends it
/// from the gossip-menu spirit-healer option (the NPC casts 17251 "Spirit Healer Res" whose dummy
/// effect builds this packet), NOT from `CMSG_SPIRIT_HEALER_ACTIVATE`. The client answers the
/// eventual XP_LOSS confirm-accept with [`spirit_healer_activate`] carrying this guid.
pub(super) fn read_spirit_healer_confirm(r: &mut &[u8]) -> io::Result<u64> {
    read_u64_le(r)
}

/// Body of `CMSG_RECLAIM_CORPSE` (opcode `0x1D2`/466 — VERIFIED `Opcodes_1_12_1.h:467`): one full
/// `u64` guid (VERIFIED `Server/Packets/Misc.cpp:122-125` — `recv_data >> guid`). The server
/// resolves the corpse through the player itself and never checks this guid's content, but the
/// real client sends its corpse's guid — we do the same. Server gates (`MiscHandler.cpp:573-603`):
/// dead + `PLAYER_FLAGS_GHOST` + corpse exists + the reclaim delay elapsed + within
/// `CORPSE_RECLAIM_RADIUS` (39 yd, `Corpse.h:40`) — success is `ResurrectPlayer(0.5)` + bones.
pub fn reclaim_corpse(corpse_guid: u64) -> Vec<u8> {
    corpse_guid.to_le_bytes().to_vec()
}

/// Body of `CMSG_SPIRIT_HEALER_ACTIVATE` (opcode `0x21C`/540 — VERIFIED `Opcodes_1_12_1.h:541`):
/// one full `u64`, the spirit healer's guid (VERIFIED `Server/Packets/Npc.cpp:40-43`). Server
/// gates (`NPCHandler.cpp:416-428` → `CanInteractWithNPC` with `UNIT_NPC_FLAG_SPIRITHEALER`
/// `0x20`): 5-yd interaction distance, ghost-visible NPC. Effect (`SendSpiritResurrect`,
/// `NPCHandler.cpp:430-477`): res at 50%, 25% durability loss on ALL items, resurrection sickness
/// 15007 at level ≥ 11 (scaling duration to 10 min at 20+), corpse → bones, teleport to the
/// corpse-nearest graveyard when it differs from the current one.
pub fn spirit_healer_activate(npc: u64) -> Vec<u8> {
    npc.to_le_bytes().to_vec()
}

/// Body of `CMSG_RESURRECT_RESPONSE` (opcode `0x15C`/348 — VERIFIED `Opcodes_1_12_1.h:349`): the
/// offerer's `u64` guid + `u8 accept` (VERIFIED `Server/Packets/Misc.cpp:132-136`). A decline just
/// clears the server-side offer; an accept teleports us to the caster (cross-instance redirected to
/// the entrance) and resurrects with the offer's stored health/mana (`Player.cpp:20188-20244`).
pub fn resurrect_response(caster: u64, accept: bool) -> Vec<u8> {
    let mut body = Vec::with_capacity(9);
    body.extend_from_slice(&caster.to_le_bytes());
    body.push(u8::from(accept));
    body
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The found shape, byte-exact against `HandleCorpseQueryOpcode`'s build (`QueryHandler.cpp:
    /// 296-303`): `u8(1) << i32(mapid) << f32(x) << f32(y) << f32(z) << u32(corpsemapid)` — here a
    /// corpse on Eastern Kingdoms (0) at Northshire-ish coords, no entrance adjustment.
    #[test]
    fn corpse_query_found_golden() {
        let mut body = vec![1u8];
        body.extend_from_slice(&0i32.to_le_bytes());
        body.extend_from_slice(&(-8949.95f32).to_le_bytes());
        body.extend_from_slice(&(-132.49f32).to_le_bytes());
        body.extend_from_slice(&83.53f32.to_le_bytes());
        body.extend_from_slice(&0u32.to_le_bytes());
        let mut r = body.as_slice();
        let loc = read_corpse_query_response(&mut r).unwrap();
        assert!(r.is_empty(), "the whole body is consumed");
        assert_eq!(
            loc,
            CorpseLocation {
                found: true,
                display_map: 0,
                position: [-8949.95, -132.49, 83.53],
                corpse_map: 0,
            }
        );
    }

    /// The not-found shape is the lone `u8(0)` (`QueryHandler.cpp:262-267`) — also pushed
    /// unprompted at bones-conversion (`Map.cpp:3624-3629`).
    #[test]
    fn corpse_query_not_found_golden() {
        let body = [0u8];
        let mut r = body.as_slice();
        let loc = read_corpse_query_response(&mut r).unwrap();
        assert!(r.is_empty());
        assert!(!loc.found);
        assert_eq!(loc.corpse_map, 0);
    }

    /// A dungeon corpse: `mapid`/coords rewritten to the ghost entrance, `corpsemapid` kept real
    /// (`QueryHandler.cpp:276-292`) — the two-map split the marker consumer relies on.
    #[test]
    fn corpse_query_dungeon_entrance_split() {
        let mut body = vec![1u8];
        body.extend_from_slice(&0i32.to_le_bytes()); // entrance map: EK
        body.extend_from_slice(&(-11209.6f32).to_le_bytes()); // Deadmines entrance-ish
        body.extend_from_slice(&1666.54f32.to_le_bytes());
        body.extend_from_slice(&25.0f32.to_le_bytes());
        body.extend_from_slice(&36u32.to_le_bytes()); // the corpse's REAL map: Deadmines
        let mut r = body.as_slice();
        let loc = read_corpse_query_response(&mut r).unwrap();
        assert_eq!(loc.display_map, 0);
        assert_eq!(loc.corpse_map, 36);
    }

    /// `SMSG_CORPSE_RECLAIM_DELAY` is one `u32` of milliseconds (`Misc.cpp:653-656`; 30 s base).
    #[test]
    fn corpse_reclaim_delay_golden() {
        let body = 30_000u32.to_le_bytes();
        let mut r = body.as_slice();
        assert_eq!(read_corpse_reclaim_delay(&mut r).unwrap(), 30_000);
        assert!(r.is_empty());
    }

    /// A player-caster offer, byte-exact against `Spell::SendResurrectRequest` (`Spell.cpp:
    /// 5024-5044`): empty name (len prefix 1 = just the terminator), sickness 0 (a player res
    /// spell), hasResTimer 1 (no `SPELL_ATTR_EX3_NO_RES_TIMER`).
    #[test]
    fn resurrect_request_player_caster_golden() {
        let mut body = Vec::new();
        body.extend_from_slice(&0x0000_0001_0000_002Au64.to_le_bytes());
        body.extend_from_slice(&1u32.to_le_bytes()); // strlen("") + 1
        body.push(0); // the empty cstring
        body.push(0); // sickness = false
        body.push(1); // hasResTimer = true
        let mut r = body.as_slice();
        let req = read_resurrect_request(&mut r).unwrap();
        assert!(r.is_empty());
        assert_eq!(
            req,
            ResurrectRequestBody {
                caster: 0x0000_0001_0000_002A,
                name: String::new(),
                sickness: false,
                has_timer: true,
            }
        );
    }

    /// An NPC-caster offer carries the creature's localized name and (for a spirit-healer-style
    /// caster) the sickness warning bit.
    #[test]
    fn resurrect_request_npc_caster_golden() {
        let name = b"Spirit Healer";
        let mut body = Vec::new();
        body.extend_from_slice(&0xF130_0FBE_0000_2AB3u64.to_le_bytes());
        body.extend_from_slice(&((name.len() + 1) as u32).to_le_bytes());
        body.extend_from_slice(name);
        body.push(0);
        body.push(1); // sickness = true
        body.push(1);
        let mut r = body.as_slice();
        let req = read_resurrect_request(&mut r).unwrap();
        assert!(r.is_empty());
        assert_eq!(req.name, "Spirit Healer");
        assert!(req.sickness);
    }

    /// `SMSG_SPIRIT_HEALER_CONFIRM` is one full guid (`SpellEffects.cpp:825-829`).
    #[test]
    fn spirit_healer_confirm_golden() {
        let body = 0xF130_0FBE_0000_2AB3u64.to_le_bytes();
        let mut r = body.as_slice();
        assert_eq!(
            read_spirit_healer_confirm(&mut r).unwrap(),
            0xF130_0FBE_0000_2AB3
        );
        assert!(r.is_empty());
    }

    /// The three client bodies: full little-endian guids (+ the accept byte), matching the server
    /// readers quoted on each builder.
    #[test]
    fn client_bodies_are_full_guids() {
        assert_eq!(
            reclaim_corpse(0xF500_0000_0000_0001),
            0xF500_0000_0000_0001u64.to_le_bytes().to_vec()
        );
        assert_eq!(
            spirit_healer_activate(0xF130_0FBE_0000_2AB3),
            0xF130_0FBE_0000_2AB3u64.to_le_bytes().to_vec()
        );
        let mut expect = 0x2Au64.to_le_bytes().to_vec();
        expect.push(1);
        assert_eq!(resurrect_response(0x2A, true), expect);
        let mut expect = 0x2Au64.to_le_bytes().to_vec();
        expect.push(0);
        assert_eq!(resurrect_response(0x2A, false), expect);
    }
}
