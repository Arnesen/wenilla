//! Name-query answer arm bodies for [`super::apply_net_updates`]'s dispatch match — the three
//! `*_QUERY_RESPONSE` packets that fill the ask-once [`NameCache`] (`NameCache::resolve` is what
//! asked for them). Each `pub(super)` fn here is exactly one arm's body; the match at the call site
//! stays the dispatcher, one call per arm.

use crate::names::{CreatureRecord, NameCache};

/// `SMSG_NAME_QUERY_RESPONSE` — a player's name, plus the race/gender/class that ride the same
/// answer. Those three now have a consumer: the `$`-macro expander's subject fallback for a player
/// who isn't streamed ([`crate::npc_text::subject_for_guid`]).
pub(super) fn player_name(
    guid: u64,
    name: String,
    race: u32,
    class: u32,
    gender: u32,
    names: &mut NameCache,
) {
    names.insert_player(guid, name, Some((race as u8, class as u8, gender as u8)));
}

/// `SMSG_PET_NAME_QUERY_RESPONSE` — keyed by pet *number*, not by template entry.
pub(super) fn pet_name(pet_number: u32, name: String, names: &mut NameCache) {
    names.insert_pet(pet_number, name);
}

/// `SMSG_CREATURE_QUERY_RESPONSE` — the template's name plus the hover line's fields. A `None` name
/// is the server's "no such entry", cached as such so the ask never repeats; the remaining fields
/// then carry their miss defaults.
#[allow(clippy::too_many_arguments)]
pub(super) fn creature_name(
    entry: u32,
    name: Option<String>,
    subname: Option<String>,
    creature_type: Option<u32>,
    rank: u32,
    type_flags: u32,
    civilian: bool,
    racial_leader: bool,
    names: &mut NameCache,
) {
    names.insert_creature(
        entry,
        name.map(|n| CreatureRecord {
            name: n,
            subname,
            creature_type: creature_type.unwrap_or(0),
            rank,
            type_flags,
            civilian,
            racial_leader,
        }),
    );
}
