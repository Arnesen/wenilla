//! GameObject-interaction messages (decision 0236). The player-facing verb for every usable world
//! GameObject — a chest, a door, a quest object, a lever — is the single `CMSG_GAMEOBJ_USE`; the
//! server fans it out by GO type and answers on the systems we already have (the loot window for a
//! chest, the gossip/quest windows for a questgiver GO, a `GAMEOBJECT_STATE` flip for a door). This
//! module also carries the ask-once `CMSG_GAMEOBJECT_QUERY` template lookup (`entry + guid` →
//! `SMSG_GAMEOBJECT_QUERY_RESPONSE`'s type/display/name/`data[24]` head) — the GO twin of
//! `CMSG_CREATURE_QUERY`; it grows further as the arc lands its later phases.

use std::io;

use crate::wire::{read_cstring, read_i32_le, read_u32_le, read_u64_le};

/// Body of `CMSG_GAMEOBJ_USE` (opcode `0xB1`/177 — VERIFIED vmangos
/// `Server/Protocol/Opcodes_1_12_1.h`): one full `u64` guid, the GameObject to use. Read server-side
/// by `WorldSession::HandleGameObjectUseOpcode` (`Handlers/SpellHandler.cpp`), which gates on
/// spawned · not GENERIC(5) · not `GO_FLAG_NO_INTERACT` · interact distance · `PlayerCanUse`, then
/// runs `GameObject::Use`. The wire shape is identical to `CMSG_LOOT` — a bare little-endian guid —
/// but the two are **not** interchangeable: `HandleLootOpcode` rejects a GameObject guid outright
/// (anticheat), so a chest opens its loot through *this* opcode (→ server `SendLoot` →
/// `SMSG_LOOT_RESPONSE`), never through `CMSG_LOOT`.
pub fn gameobj_use(guid: u64) -> Vec<u8> {
    guid.to_le_bytes().to_vec()
}

/// Body of a `CMSG_GAMEOBJECT_QUERY` (opcode `0x005E`/94 — VERIFIED vmangos
/// `Server/Protocol/Opcodes_1_12_1.h`): the template `entry` + the asking guid (VERIFIED vmangos
/// `QueryGameObject::ReadFromWorldPacket` — `u32 entryID` then a full `u64` guid), identical shape to
/// [`super::creature_query`].
pub fn gameobject_query(entry: u32, guid: u64) -> Vec<u8> {
    let mut body = Vec::with_capacity(12);
    body.extend_from_slice(&entry.to_le_bytes());
    body.extend_from_slice(&guid.to_le_bytes());
    body
}

/// The head of a GameObject-template answer (`SMSG_GAMEOBJECT_QUERY_RESPONSE`, opcode `0x005F`/95,
/// answering `CMSG_GAMEOBJECT_QUERY`) — decision 0236's ask-once template lookup. `data[24]` is the
/// template's raw `GameObjectData` union tail (VERIFIED vmangos `QueryHandler.cpp`
/// `HandleGameObjectQueryOpcode`: `data << info->raw.data[i]` for `i in 0..24`); its slot layout is
/// **type-specific** (e.g. a chest's lockId lives at a different index than a door's) — this layer
/// parses the array verbatim and leaves resolving a slot to the consumer that knows `type_id`.
#[derive(Debug, Clone, PartialEq)]
pub struct GameObjectQueryInfo {
    pub type_id: u32,
    pub display_id: u32,
    pub name: String,
    pub data: [i32; 24],
}

/// Read `SMSG_GAMEOBJECT_QUERY_RESPONSE` → `(entry, Some(info))`, or `(entry, None)` on a miss.
/// VERIFIED vmangos `QueryHandler.cpp` `HandleGameObjectQueryOpcode`, the
/// `SUPPORTED_CLIENT_BUILD >= CLIENT_BUILD_1_12_1` branch — wire order: `entry, type, displayId,
/// name`, then 3 empty name slots + an `icon` C-string (the 5th name slot), all read past and
/// dropped except `name`, then `data[24]` as 24 little-endian `i32`s. There is **no** trailing float
/// `size` field in 5875 (the `data << float(info->size)` line is commented out server-side). A miss
/// is the lone `u32` of `entry | 0x8000_0000` — the same shape as the creature/item miss.
pub(super) fn read_gameobject_query_response(
    r: &mut &[u8],
) -> io::Result<(u32, Option<GameObjectQueryInfo>)> {
    let entry = read_u32_le(r)?;
    if entry & 0x8000_0000 != 0 {
        return Ok((entry & 0x7FFF_FFFF, None));
    }
    let type_id = read_u32_le(r)?;
    let display_id = read_u32_le(r)?;
    let name = read_cstring(r)?;
    for _ in 0..3 {
        let _ = read_cstring(r)?; // name2..name4, always empty in 5875
    }
    let _icon = read_cstring(r)?; // name5 — the GO's icon key; unused until a consumer needs it
    let mut data = [0i32; 24];
    for slot in &mut data {
        *slot = read_i32_le(r)?;
    }
    Ok((
        entry,
        Some(GameObjectQueryInfo {
            type_id,
            display_id,
            name,
            data,
        }),
    ))
}

/// Read `SMSG_GAMEOBJECT_CUSTOM_ANIM` → `(guid, anim_id)`. VERIFIED vmangos
/// `GameObject::SendGameObjectCustomAnim`: a full `u64` guid then `u32 animId`. The client-side
/// meaning (wow-re `gameobject-anim-arm.md` §"one-shot channel" step 8): arm the GO's Custom
/// substate `8 + animId` — AnimationData ids 153..156 — with `animId >= 4` rejected; the
/// consumer applies that gate, this layer parses verbatim.
pub(super) fn read_gameobject_custom_anim(r: &mut &[u8]) -> io::Result<(u64, u32)> {
    let guid = read_u64_le(r)?;
    let anim_id = read_u32_le(r)?;
    Ok((guid, anim_id))
}

/// Read `SMSG_GAMEOBJECT_DESPAWN_ANIM` → `guid`. VERIFIED vmangos
/// `WorldObject::SendObjectDeSpawnAnim` (`Objects/Object.cpp:2307`) →
/// `WorldPackets::Misc::GameObjectDespawnAnim`, whose whole body is the object guid. The
/// client-side meaning (wow-re `gameobject-anim-arm.md` §2c): arm substate 12, AnimationData id
/// **157 Despawn** — the arm channel, like the custom one, is disjoint from the §243 lid family.
pub(super) fn read_gameobject_despawn_anim(r: &mut &[u8]) -> io::Result<u64> {
    read_u64_le(r)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::messages::{opcode, parse_server, ServerPacket};

    fn hx(s: &str) -> Vec<u8> {
        (0..s.len())
            .step_by(2)
            .map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap())
            .collect()
    }

    #[test]
    fn cmsg_gameobj_use_body_golden() {
        // A full little-endian guid, no more (SpellHandler.cpp reads a single ObjectGuid).
        assert_eq!(
            gameobj_use(0x1234_5678_9abc_def0),
            hx("f0debc9a78563412"),
            "CMSG_GAMEOBJ_USE body"
        );
    }

    #[test]
    fn cmsg_gameobject_query_body_golden() {
        // entry u32 + full 8-byte guid — identical shape to CMSG_CREATURE_QUERY (VERIFIED vmangos
        // `QueryGameObject::ReadFromWorldPacket`).
        assert_eq!(
            gameobject_query(1731, 0x1234_5678_9abc_def0),
            hx("c3060000f0debc9a78563412"),
            "CMSG_GAMEOBJECT_QUERY body"
        );
    }

    #[test]
    fn gameobject_query_response_hit_decodes() {
        let mut body = 1731u32.to_le_bytes().to_vec(); // entry
        body.extend_from_slice(&3u32.to_le_bytes()); // type = GAMEOBJECT_TYPE_CHEST
        body.extend_from_slice(&123u32.to_le_bytes()); // displayId
        body.extend_from_slice(b"Chest\0"); // name
        body.extend_from_slice(&[0, 0, 0]); // name2..name4, empty C-strings
        body.extend_from_slice(b"Icon\0"); // icon (name5) — read past and dropped
        let mut data = [0i32; 24];
        data[0] = 57; // stand-in lockId — the type-specific slot a later consumer resolves
        for v in data {
            body.extend_from_slice(&v.to_le_bytes());
        }

        match parse_server(opcode::SMSG_GAMEOBJECT_QUERY_RESPONSE, &body).unwrap() {
            ServerPacket::GameObjectQueryResponse { entry, info } => {
                assert_eq!(entry, 1731);
                let info = info.expect("hit");
                assert_eq!(info.type_id, 3);
                assert_eq!(info.display_id, 123);
                assert_eq!(info.name, "Chest");
                assert_eq!(info.data[0], 57);
                assert_eq!(info.data[1], 0);
            }
            other => panic!("expected GameObjectQueryResponse, got {}", other.name()),
        }
    }

    #[test]
    fn gameobject_despawn_anim_decodes() {
        // The whole body is the object guid (VERIFIED vmangos
        // `WorldObject::SendObjectDeSpawnAnim` → `WorldPackets::Misc::GameObjectDespawnAnim`,
        // whose `AppendBodyTo` writes only `gameObjectGuid`). A UBRS Rookery Egg spending its
        // last trap charge is the load-bearing sender (decision 1404).
        let body = hx("f0debc9a78563412");
        match parse_server(opcode::SMSG_GAMEOBJECT_DESPAWN_ANIM, &body).unwrap() {
            ServerPacket::GameObjectDespawnAnim { guid } => {
                assert_eq!(guid, 0x1234_5678_9abc_def0);
            }
            other => panic!("expected GameObjectDespawnAnim, got {}", other.name()),
        }
    }

    #[test]
    fn gameobject_custom_anim_decodes() {
        // A full little-endian guid + u32 animId (VERIFIED vmangos
        // `GameObject::SendGameObjectCustomAnim`). The bobber's bite is animId 0.
        let body = hx("f0debc9a7856341200000000");
        match parse_server(opcode::SMSG_GAMEOBJECT_CUSTOM_ANIM, &body).unwrap() {
            ServerPacket::GameObjectCustomAnim { guid, anim_id } => {
                assert_eq!(guid, 0x1234_5678_9abc_def0);
                assert_eq!(anim_id, 0);
            }
            other => panic!("expected GameObjectCustomAnim, got {}", other.name()),
        }
    }

    #[test]
    fn fish_verdicts_decode_from_empty_bodies() {
        // Both fishing verdicts are size-0 sends (vmangos `GameObject::Update`/`Use`).
        assert!(matches!(
            parse_server(opcode::SMSG_FISH_NOT_HOOKED, &[]).unwrap(),
            ServerPacket::FishNotHooked
        ));
        assert!(matches!(
            parse_server(opcode::SMSG_FISH_ESCAPED, &[]).unwrap(),
            ServerPacket::FishEscaped
        ));
    }

    #[test]
    fn gameobject_query_response_miss_decodes() {
        // A miss is the lone entry echoed with its top bit set (VERIFIED vmangos
        // HandleGameObjectQueryOpcode's "GO's info was not found" branch — same shape as the
        // creature/item miss).
        let body = (1731u32 | 0x8000_0000).to_le_bytes();
        match parse_server(opcode::SMSG_GAMEOBJECT_QUERY_RESPONSE, &body).unwrap() {
            ServerPacket::GameObjectQueryResponse { entry, info } => {
                assert_eq!(entry, 1731);
                assert!(info.is_none());
            }
            other => panic!("expected GameObjectQueryResponse, got {}", other.name()),
        }
    }
}
