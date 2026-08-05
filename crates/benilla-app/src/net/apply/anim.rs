//! "Play this on that unit" arm bodies for [`super::apply_net_updates`]'s dispatch match — the
//! server packets whose whole content is *an animation to run on a streamed unit*: the two emote
//! relays, the spell-visual kit push (decision 0280), and the environmental-damage kit (the
//! fall-landing dust puff). Each resolves the guid through the index and writes one message; the
//! animation law itself lives in [`crate::creature_anim`]. Each `pub(super)` fn here is exactly one
//! arm's body; the match at the call site stays the dispatcher, one call per arm.

use benilla_protocol::messages::EnvironmentalDamageLog;
use bevy::prelude::*;

use crate::creature_anim::{EnvDamageTable, KitPush, PlaySeq};

use super::super::{EmoteKind, EmoteMessage, GuidIndex};

/// `SMSG_TEXT_EMOTE` — someone performed a `/`-emote (the TextEmote.dbc id; the anim, if any, is
/// the emote row's).
pub(super) fn text_emote(
    guid: u64,
    text_emote: u32,
    index: &GuidIndex,
    out: &mut MessageWriter<EmoteMessage>,
) {
    out.write(EmoteMessage {
        source: index.0.get(&guid).copied(),
        kind: EmoteKind::Text(text_emote),
    });
}

/// `SMSG_EMOTE` — a bare Emotes.dbc anim id on a unit (the server-driven one-shot: NPC scripts,
/// the `/`-emote's own anim leg).
pub(super) fn emote(
    guid: u64,
    emote_id: u32,
    index: &GuidIndex,
    out: &mut MessageWriter<EmoteMessage>,
) {
    out.write(EmoteMessage {
        source: index.0.get(&guid).copied(),
        kind: EmoteKind::Anim(emote_id),
    });
}

/// `SMSG_PLAY_SPELL_VISUAL` — the kit-push opcode (decision 0280): a stage-0 play on the unit, the
/// eat/drink kit cadence and mid-channel swaps. Consumer: `creature_anim::spell_visual`. The
/// [`PlaySeq`] stamp is taken only when the unit is streamed in, so an unstreamed guid never
/// advances the call-order counter.
pub(super) fn play_spell_visual(
    unit: u64,
    kit_id: u32,
    index: &GuidIndex,
    play_seq: &mut PlaySeq,
    out: &mut MessageWriter<KitPush>,
) {
    if let Some(&e) = index.0.get(&unit) {
        out.write(KitPush {
            entity: e,
            kit_id,
            seq: play_seq.next(),
        });
    }
}

/// `SMSG_ENVIRONMENTALDAMAGELOG`'s consequence (wow-re `sound/scratch/uisound-tables.md`: reader
/// `0x624fcc` inside `0x624f30`): the EnvironmentalDamage.dbc 6-slot table picks the damage type's
/// SpellVisualKit — fall's is the DustCloud_Land puff — played on the victim through the ordinary
/// discrete kit play (`0x60edf0`), the same leg the kit-push opcode rides. The pain vocal's exact
/// trigger is a dispatched wow-re §5 (in flight) — it folds in as its own edge when the verdict
/// lands.
pub(super) fn environmental_damage_log(
    e: EnvironmentalDamageLog,
    index: &GuidIndex,
    table: Option<&EnvDamageTable>,
    play_seq: &mut PlaySeq,
    out: &mut MessageWriter<KitPush>,
) {
    if let Some(&ent) = index.0.get(&e.victim) {
        if let Some(kit_id) = table.and_then(|t| t.0.kit_id(e.damage_type)) {
            debug!(
                "net: environmental damage on {:#x} (type {}, {} dmg) → kit {kit_id}",
                e.victim, e.damage_type, e.damage
            );
            out.write(KitPush {
                entity: ent,
                kit_id,
                seq: play_seq.next(),
            });
        }
    }
}
