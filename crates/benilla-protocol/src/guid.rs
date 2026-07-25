//! GUID bit-layout decode for 1.12.1 — the wire's object identity, `[high:16][entry:24][counter:24]`
//! for entry-carrying types and `[high:16][counter:48-usable-as-32]` for the rest (VERIFIED vmangos
//! `ObjectGuid.h`: `GetHigh` = bits 48–63, `GetEntry` = bits 24–47 masked to 24 bits, gated on the
//! high value carrying an entry at all).
//!
//! The one load-bearing consumer today: `CMSG_CREATURE_QUERY` wants the creature's **template entry**,
//! which the client recovers from the guid exactly this way — the u64→entry extraction is the same
//! stable bijection decision 0068's mapping layer relies on for modern string GUIDs later.

/// `HIGHGUID_PLAYER` — a player character's guid high part.
pub const HIGH_PLAYER: u16 = 0x0000;
/// `HIGHGUID_ITEM` == `HIGHGUID_CONTAINER` — items and bags share the marker (a bag *is* an item).
pub const HIGH_ITEM: u16 = 0x4000;
/// `HIGHGUID_GAMEOBJECT`.
pub const HIGH_GAMEOBJECT: u16 = 0xF110;
/// `HIGHGUID_UNIT` — a spawned creature.
pub const HIGH_UNIT: u16 = 0xF130;
/// `HIGHGUID_PET`.
pub const HIGH_PET: u16 = 0xF140;
/// `HIGHGUID_MO_TRANSPORT` — a boat/zeppelin (`GAMEOBJECT_TYPE_MO_TRANSPORT`, template type 15): the
/// WMO-modeled transports that sail scripted taxi-path routes. VERIFIED vmangos `ObjectGuid.h:77`.
pub const HIGH_MO_TRANSPORT: u16 = 0x1FC0;
/// `HIGHGUID_TRANSPORT` — an elevator (`GAMEOBJECT_TYPE_TRANSPORT`, template type 11): the M2-modeled,
/// `TransportAnimation.dbc`-driven lifts. VERIFIED vmangos `ObjectGuid.h:72`.
pub const HIGH_TRANSPORT: u16 = 0xF120;

/// The guid's high 16 bits (`GetHigh`, vmangos `ObjectGuid.h`) — the object-family tag.
pub fn high(guid: u64) -> u16 {
    ((guid >> 48) & 0xFFFF) as u16
}

/// A (non-zero) player-character guid.
pub fn is_player(guid: u64) -> bool {
    guid != 0 && high(guid) == HIGH_PLAYER
}

/// A creature or pet guid (`IsCreatureOrPet`) — both are `TYPEID_UNIT` to every consumer. They are
/// **not** interchangeable for naming: only the creature half carries a template entry (see
/// [`entry`] and [`pet_number`]).
pub fn is_creature_or_pet(guid: u64) -> bool {
    matches!(high(guid), HIGH_UNIT | HIGH_PET)
}

/// A pet guid (`IsPet`) — a summoned or charmed unit, whose guid's middle field is a **pet number**
/// and not a creature template entry. See [`pet_number`].
pub fn is_pet(guid: u64) -> bool {
    high(guid) == HIGH_PET
}

/// A pet guid's **pet number** — the per-pet id `CMSG_PET_NAME_QUERY` is keyed by, and the value the
/// server matches against `CharmInfo::GetPetNumber()` before it will answer at all.
///
/// It occupies the same guid bits 24–47 that a creature's template entry does, because vmangos
/// composes a pet with `Object::_Create(guidlow, petNumber, HIGHGUID_PET)` (`Objects/Pet.cpp:2250`,
/// from `Pet::Create(guidlow, pos, cinfo, petNumber)`): the shared `_Create`'s `entry` parameter is
/// fed the pet number, **not** `cinfo->entry`. That is why [`entry`] answers `None` for this family —
/// the slot is occupied, just not by a template id, and a `CMSG_CREATURE_QUERY` for it can only miss.
pub fn pet_number(guid: u64) -> Option<u32> {
    is_pet(guid).then_some(((guid >> 24) & 0xFF_FFFF) as u32)
}

/// A (non-zero) item or container guid (`IsItem` — containers included). Items carry **no** entry
/// in the guid; the entry lives in the descriptor (`OBJECT_FIELD_ENTRY`).
pub fn is_item(guid: u64) -> bool {
    guid != 0 && high(guid) == HIGH_ITEM
}

/// Either transport guid family — a boat/zeppelin (`HIGH_MO_TRANSPORT`) or an elevator
/// (`HIGH_TRANSPORT`). The two are composed differently (see [`entry`]) but both name "a transport" to
/// every other consumer (the create-routing gate, the rider wire tail).
pub fn is_transport(guid: u64) -> bool {
    matches!(high(guid), HIGH_MO_TRANSPORT | HIGH_TRANSPORT)
}

/// The embedded template entry, for the guid families that carry one. Two different layouts, both
/// VERIFIED against vmangos:
///
/// - Creatures/GameObjects/**elevators** (`HIGH_UNIT`/`HIGH_GAMEOBJECT`/
///   `HIGH_TRANSPORT`) use the standard `[high:16][entry:24][counter:24]` layout — vmangos's
///   class-internal `HasEntry` switch returns `true` for these (`ObjectGuid.h:223-240`), and an
///   elevator composes exactly like an ordinary GameObject: `GameObject::Create` calls
///   `Object::_Create(guidlow, goinfo->id, HIGHGUID_TRANSPORT)` for a `GAMEOBJECT_TYPE_TRANSPORT`
///   template (`Objects/GameObject.cpp:207`, reached via `ElevatorTransport::Create` →
///   `GenericTransport::Create` → the same `GameObject::Create`) — `goinfo->id` (the template entry)
///   rides bits 24–47, the spawn's own DB-assigned low guid rides bits 0–23.
/// - **Boats/zeppelins** (`HIGH_MO_TRANSPORT`) carry **no** entry part at all: vmangos's
///   `DoesGuidHaveEntryPart`/`HasEntry` both return `false` for `HIGHGUID_MO_TRANSPORT`
///   (`ObjectGuid.h:87-104`, `223-240`). `ShipTransport::Create` calls `Object::_Create(guidlow, 0,
///   HIGHGUID_MO_TRANSPORT)` (`Transports/Transport.cpp:65`) where `guidlow` is the
///   `gameobject_template` entry itself, threaded in from `TransportMgr::CreateTransport(uint32
///   entry, …)` (`TransportMgr.cpp:364,400`: `trans->Create(entry, frameItr)`). The `ObjectGuid`
///   3-arg ctor folds `entry`(=0 here) and `counter`(=guidlow) into ONE 32-bit low field when the
///   family has no entry part (`ObjectGuid.h:123`: `counter | (entry << 24) | (hi << 48)`) — so the
///   template entry rides the **full low 32 bits**, recovered the same way `GetCounter`'s
///   `hasEntry=false` branch does (`ObjectGuid.h:157-165`: mask `0xFFFF_FFFF`, not the 24-bit entry
///   slot the other families use). Masking to 24 bits here would silently truncate a >16.7M template
///   id — none exist today, but the 32-bit mask is what vmangos actually does, so that's what we do.
///
/// `None` for players/items/corpses/dynamic objects (no entry part, and not composed like a transport
/// either) — and `None` for **pets**, whose entry-shaped slot holds a pet number instead
/// ([`pet_number`]): `HasEntry` is `true` for the family, but what vmangos stores there is not a
/// template id, so answering with it would hand every caller a creature entry that cannot resolve.
pub fn entry(guid: u64) -> Option<u32> {
    match high(guid) {
        HIGH_MO_TRANSPORT => Some((guid & 0xFFFF_FFFF) as u32),
        HIGH_UNIT | HIGH_GAMEOBJECT | HIGH_TRANSPORT => Some(((guid >> 24) & 0xFF_FFFF) as u32),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Compose a guid the way vmangos does: `counter | (entry << 24) | (high << 48)`.
    fn compose(high: u16, entry: u32, counter: u32) -> u64 {
        u64::from(counter) | (u64::from(entry) << 24) | (u64::from(high) << 48)
    }

    #[test]
    fn creature_guid_decodes_high_and_entry() {
        // A Northshire creature: entry 69 (Diseased Young Wolf), counter 12345.
        let g = compose(HIGH_UNIT, 69, 12345);
        assert_eq!(high(g), HIGH_UNIT);
        assert!(is_creature_or_pet(g));
        assert!(!is_player(g));
        assert_eq!(entry(g), Some(69));
    }

    #[test]
    fn player_guid_has_no_entry() {
        let g = compose(HIGH_PLAYER, 0, 7);
        assert!(is_player(g));
        assert!(!is_creature_or_pet(g));
        assert_eq!(entry(g), None);
    }

    #[test]
    fn zero_guid_is_nothing() {
        assert!(!is_player(0));
        assert!(!is_creature_or_pet(0));
        assert!(!is_item(0));
        assert_eq!(entry(0), None);
    }

    #[test]
    fn item_guid_is_item_and_has_no_entry() {
        let g = compose(HIGH_ITEM, 0, 42);
        assert!(is_item(g));
        assert!(!is_player(g));
        assert_eq!(
            entry(g),
            None,
            "an item's entry is a descriptor field, not guid bits"
        );
    }

    #[test]
    fn entry_masks_to_24_bits() {
        let g = compose(HIGH_UNIT, 0xFF_FFFF, 0xFF_FFFF);
        assert_eq!(entry(g), Some(0xFF_FFFF));
    }

    /// A pet's entry-shaped slot is its PET NUMBER — `Pet::Create` feeds `_Create`'s `entry`
    /// parameter `petNumber`, not `cinfo->entry` (`Objects/Pet.cpp:2250`). Asking
    /// `CMSG_CREATURE_QUERY` for it is what left NPC-summoned pets nameless, so `entry` must refuse.
    #[test]
    fn a_pet_carries_a_pet_number_not_a_template_entry() {
        let g = compose(HIGH_PET, 137, 4242);
        assert!(is_pet(g));
        assert!(is_creature_or_pet(g));
        assert_eq!(pet_number(g), Some(137));
        assert_eq!(entry(g), None, "a pet's slot is not a template entry");
        // ...and only a pet answers `pet_number`.
        assert_eq!(pet_number(compose(HIGH_UNIT, 137, 4242)), None);
    }

    /// An elevator (`HIGHGUID_TRANSPORT`, 0xF120) composes exactly like an ordinary GameObject —
    /// `Object::_Create(guidlow, goinfo->id, HIGHGUID_TRANSPORT)` (`GameObject.cpp:207`): the DB spawn's
    /// own low guid in `counter`, the `gameobject_template` entry in the standard entry slot.
    #[test]
    fn elevator_guid_decodes_high_and_entry() {
        // The Undercity/Ironforge-style elevator template entry 900 (a real GAMEOBJECT_TYPE_TRANSPORT
        // id range), DB spawn low guid 4242.
        let g = compose(HIGH_TRANSPORT, 900, 4242);
        assert_eq!(high(g), HIGH_TRANSPORT);
        assert!(is_transport(g));
        assert_eq!(entry(g), Some(900));
    }

    /// A boat/zeppelin (`HIGHGUID_MO_TRANSPORT`, 0x1FC0) composes the OTHER way —
    /// `Object::_Create(guidlow, 0, HIGHGUID_MO_TRANSPORT)` (`Transport.cpp:65`) where `guidlow` is
    /// itself the `gameobject_template` entry (`TransportMgr::CreateTransport`'s `entry` param threaded
    /// straight through, `TransportMgr.cpp:364,400`): the entry rides the FULL low 32 bits, not the
    /// 24-bit entry slot the standard families use — composed here with `entry=0` per the ctor
    /// (`ObjectGuid.h:123`), matching `entry()`'s `HIGH_MO_TRANSPORT` mask.
    #[test]
    fn mo_transport_guid_decodes_high_and_entry() {
        // The Menethil-Theramore ferry's real gameobject_template entry, 176495.
        let g = compose(HIGH_MO_TRANSPORT, 0, 176_495);
        assert_eq!(high(g), HIGH_MO_TRANSPORT);
        assert!(is_transport(g));
        assert_eq!(
            entry(g),
            Some(176_495),
            "the template entry rides the full low 32 bits, not bits 24-47"
        );
        // The naive `(guid >> 24) & 0xFFFFFF` extraction the other families use would find nothing
        // useful here — pin that the two layouts genuinely differ, not just that our helper works.
        assert_ne!(((g >> 24) & 0xFF_FFFF) as u32, 176_495);
    }

    #[test]
    fn is_transport_excludes_other_families() {
        assert!(!is_transport(compose(HIGH_UNIT, 69, 1)));
        assert!(!is_transport(compose(HIGH_GAMEOBJECT, 1, 1)));
        assert!(!is_transport(0));
    }
}
