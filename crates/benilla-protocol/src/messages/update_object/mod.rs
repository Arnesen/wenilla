//! `SMSG_UPDATE_OBJECT` body decode: the object list, each entry's [`MovementBlock`] pose and sparse
//! [`ObjectFields`] of descriptor fields. The complex, *growing* core of the message layer (decision
//! 0021) — split out because the descriptor-field decode expands as the object layer does. The parent
//! module ([`super`]) owns packet framing/dispatch and the client builders, and the shared
//! `MOVEMENT_FLAG_*` constants (also used by its movement-info parse), which this module reaches via
//! `super::`.

use std::io::{self, Read};

use crate::wire::{read_packed_guid, read_u32_le, read_u8};

mod fields;
mod movement;

pub use fields::{
    power_display_scale, quest_slot_state, CorpseLook, ObjectFields, OwnerFallback,
    PlayerSkillSlot, QuestLogSlot, UnitAuraSlot, AURA_FLAG_CANCELABLE, AURA_FLAG_EFF_INDEX_MASK,
    FIELD_PLAYER_SKILL_INFO_1_1, PLAYER_EXPLORED_ZONES_SLOTS, PLAYER_QUEST_LOG_SLOTS,
    PLAYER_SKILL_SLOTS, UNIT_AURA_POSITIVE_SLOTS, UNIT_AURA_SLOTS,
};
pub use movement::{CreateSpline, MovementBlock, ObjectType};

/// One entry in an `SMSG_UPDATE_OBJECT` object list.
pub enum Object {
    Values {
        guid: u64,
        mask: ObjectFields,
    },
    Movement {
        guid: u64,
        movement: MovementBlock,
    },
    /// `CREATE_OBJECT` / `CREATE_OBJECT2` (identical wire shape; the distinction is irrelevant here).
    Create {
        guid: u64,
        object_type: ObjectType,
        movement: MovementBlock,
        mask: ObjectFields,
    },
    OutOfRange {
        guids: Vec<u64>,
    },
    Near {
        guids: Vec<u64>,
    },
}

impl Object {
    fn read(r: &mut impl Read) -> io::Result<Self> {
        let update_type = read_u8(r)?;
        Ok(match update_type {
            0 => Object::Values {
                guid: read_packed_guid(r)?,
                mask: ObjectFields::read(r)?,
            },
            1 => Object::Movement {
                guid: read_packed_guid(r)?,
                movement: MovementBlock::read(r)?,
            },
            2 | 3 => {
                let guid = read_packed_guid(r)?;
                let object_type = ObjectType::from_u8(read_u8(r)?);
                let movement = MovementBlock::read(r)?;
                // A create's mask is the COMPLETE descriptor — the server omits zero-valued
                // fields (vmangos `_SetCreateBits`), so absent must read 0, not unknown
                // (`ObjectFields`'s created semantics). The TYPE rides along because "absent = 0"
                // holds only inside this object's OWN descriptor: a creature has no PLAYER block
                // to be absent from (decision 1081).
                let mask = ObjectFields::read(r)?.into_created(object_type);
                Object::Create {
                    guid,
                    object_type,
                    movement,
                    mask,
                }
            }
            4 => Object::OutOfRange {
                guids: read_guid_list(r)?,
            },
            5 => Object::Near {
                guids: read_guid_list(r)?,
            },
            other => {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!("unknown object update_type {other}"),
                ))
            }
        })
    }
}

fn read_guid_list(r: &mut impl Read) -> io::Result<Vec<u64>> {
    let count = read_u32_le(r)?;
    let mut guids = Vec::with_capacity(count.min(0xFFFF) as usize);
    for _ in 0..count {
        guids.push(read_packed_guid(r)?);
    }
    Ok(guids)
}

/// Parse an `SMSG_UPDATE_OBJECT` body's object list (the count + has-transport byte + each `Object`).
pub(super) fn read_update_object(r: &mut impl Read) -> io::Result<Vec<Object>> {
    let amount_of_objects = read_u32_le(r)?;
    let _has_transport = read_u8(r)?;
    let mut objects = Vec::with_capacity(amount_of_objects.min(0xFFFF) as usize);
    for _ in 0..amount_of_objects {
        objects.push(Object::read(r)?);
    }
    Ok(objects)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quest_log_slot_unpacks_counters_and_state() {
        // Slot 2 (base 198 + 6 = 204): counters 5/10/63/0 packed at 6-bit strides, state byte
        // COMPLETE, a timer value. Counter packing per vmangos Player.h:1100-1106.
        let count_state =
            5u32 | (10 << 6) | (63 << 12) | ((quest_slot_state::COMPLETE as u32) << 24);
        let f = ObjectFields::from_pairs(&[(204, 783), (205, count_state), (206, 4242)]);
        assert_eq!(
            f.player_quest_log(2),
            Some(QuestLogSlot {
                quest_id: 783,
                counters: [5, 10, 63, 0],
                state: quest_slot_state::COMPLETE,
                timer: 4242,
            })
        );
        // The neighbouring slots never streamed → None; out of range → None.
        assert_eq!(f.player_quest_log(1), None);
        assert_eq!(f.player_quest_log(3), None);
        assert_eq!(f.player_quest_log(PLAYER_QUEST_LOG_SLOTS), None);
    }

    #[test]
    fn quest_log_cleared_slot_reads_zero_id() {
        // An abandoned slot: the server zeroes the id (and pair) — still `Some`, id 0.
        let f = ObjectFields::from_pairs(&[(198, 0), (199, 0), (200, 0)]);
        assert_eq!(f.player_quest_log(0).map(|s| s.quest_id), Some(0));
    }
}
