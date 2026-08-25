//! Combat audio (decisions 0075 + 0525): the melee swing's sounds — the whoosh and exertion on
//! their own event tags, the CONTACT family on the victim dispatch ([`SwingImpact`]).
//!
//! The trigger chain: `SMSG_ATTACKERSTATEUPDATE` → [`SwingMessage`] (net bridge; also drives the
//! swing *animation*, decision 0073) → the attack sequence's M2 events fire mid-swing through
//! [`AnimSoundEvent`]. Two tags route directly from that stream:
//!
//! - **`$CSS`** — the swing whoosh, played only when nothing was contacted (miss/dodge/evade):
//!   kits 7080/7081 `Combat Miss 1H/2H` by weapon handedness — exactly the two ids the client
//!   caches by name at startup (wow-re `0x4575b0`, `_DONOTRENAME_` kits).
//! - **`$CAH`** — **not** the attacker's exertion, which is what this module used to think.
//!   `$CAH` drives the **victim's** injury vocal (`0x624865 je 0x624902` → `0x6249bb call
//!   0x624530`), which benilla already fires off that crossing; there is no `call [reg+0x88]`
//!   anywhere in `0x6247d0`, so the tag never reaches the exertion columns at all.
//!
//! The **attacker's exertion** is packet-driven at swing start, and is now wired that way
//! (`SMSG_ATTACKERSTATEUPDATE` → `0x6246a0` → `0x624786` → `0x623b10`). Read off the bytes:
//!
//! - `class = ([hitrec+0x10] >> 7) & 1` — the crit bit, so `Exertion` / `ExertionCritical`.
//! - `0x62476a`: gated on **`victimState != 0`**, and only on that. (The earlier note here also
//!   claimed a victim-health gate; `0x6246a0` contains no such test. Its other two bails —
//!   `[[attacker+0x110]+0x40] <= 0` at `0x6246b1` and `hitInfo & 0x10000` at `0x6246c2` — sit
//!   above the swing *animation* select as well, so they belong to that subsystem, not here.)
//! - `force = 0` (`0x62477e`), so the **class chance roll applies**: threshold 70 for a creature
//!   and 35 for a player on class 0, 100 on class 1. A crit always grunts; an ordinary swing
//!   grunts ~70 % of the time for a creature and ~36 % for a player.
//!
//! This moves *when* a grunt is heard — swing start rather than mid-clip — and thins ordinary
//! swings out, which the tag-driven version never did.
//!
//! The **contact family** consumes [`SwingImpact`] — the victim dispatch `0x624530` + the
//! `0x6247d0` weapon-sound block, fired at the swing clip's **`$AH0–3`/`$CAH`** crossing, or at
//! receive for an unresolved attacker (decision 0529; previously keyed on `$HIT`, a CEffect-only
//! tag many creature attack clips never author — ogre.m2 authors it in 1 of its ~14 attack
//! variations, so ogre hits were near-silent):
//!
//! It is **two blocks in the client's order**, not one pick (decision 0899):
//!
//! 1. `0x6247d0`'s own weapon-sound block, at the attacker — a **natural-weapon** swing
//!    (`$AHn` fired the dispatch) plays the attacker's `CreatureSoundData.CustomAttack[n]`
//!    column INSTEAD of the generic weapon impact (the `SWINGNOHITSOUND` latch, `0x6247d0` §f);
//!    otherwise a landed hit plays the `WeaponImpactSounds` impact/crit slot for the victim's
//!    material (`CreatureImpactType`).
//! 2. `0x624530`'s **victimState-keyed clang**, at the victim — parry (`0x6245bb` →
//!    `0x623640(sel=0)`) takes the attacker row's parry slot (metal/wood by the victim's
//!    weapon), block (`0x6245d8` → `sel=1`) the shield slot. This block is reached on **every**
//!    impact tag: the `$AHn` digit block "has zero effect on the victim dispatch", so a beast's
//!    bite and the parry clang both sound.
//!
//! Plus the victim's injury vocal (`Injury`/`InjuryCritical`/`InjuryCrushing`) on every
//! damaging, undefended hit.
//!
//! A `text_only` flush (supersede/attack-stop) drops its sounds — only the floating number
//! flushes (decision 0149's flush law, inherited from the shared dispatch).
//!
//! ## Two legs still unvoiced — and what each is actually blocked on
//!
//! Both were "pinned" by 1555 in the sense that the *route* is known. Neither is buildable from
//! that alone, and the missing piece in each case is a specific unread function, not a judgement
//! call. Named here so the next pass is a scoped question rather than a rediscovery.
//!
//! **1 · The connecting swing** — `$CSS` on any victimState outside {0, 2, 6} plays
//! `WeaponSwingSounds2.dbc` on the capped bus 6, where benilla plays nothing and voices only the
//! miss whoosh. So every landed melee swing is currently missing a sound the reference makes.
//! The play itself is fully read: `0x624c81` → `0x457f60`, which bails on `swingType >= 3`
//! (`0x457f63`), indexes the 6-slot cache `[0xb06bd4]` at `critical + swingType*2`
//! (`0x457f8d lea eax,[eax+ecx*2]`) — kits 233–238, Light/Medium/Heavy × Normal/Critical — and
//! plays on `ecx = 6` at volume **0.5 when `[attacker+0xd80] & 0x10` is set, else 1.0**
//! (`0x457f74`/`0x457f7d`).
//!
//! **Blocked on `0x623870`**, which is where `swingType` comes from: the call site fills it via
//! `0x624c55 call 0x623870(bool, &out)` and hands the result straight in. Its Light/Medium/Heavy
//! classification rule — presumably off the equipped weapon — is unread, and so is the meaning of
//! the two `[attacker+0xd80]` bits the site uses (`0x80` as the critical argument, `0x10` as the
//! half-volume flag). Guessing the classification would put a *wrong* swing sound on every melee
//! hit in the game, which is worse than the silence. `WeaponSwingSounds2.dbc` also has no loader
//! yet (6 rows × 4 fields × 16 B: `{id, swingType, critical, SoundEntriesId}`).
//!
//! **2 · The material foley** rides the same `$FSD` as the terrain footstep — a footfall makes
//! *two* sounds in the reference, the capped terrain step on bus 9 and an uncapped foley on bus 0
//! (`0x6233d9 call [vt+0x8c]` → `0x623610` for CGUnit / `0x62fa30` for CGPlayer → `0x4584e0`).
//! **Blocked on the material lookup itself**: CGUnit reads `[[unit+0xb3c]+0x28]` (a
//! `CreatureModelData` column) and CGPlayer reads the equipped item's material, and `0x4584e0`
//! then takes the material row's `+0x8` foley kit — but which DBC that row belongs to, and which
//! column `+0x8` is, are unread. benilla's footstep chain resolves a different lookup
//! (`FootstepTerrainLookup`) and cannot answer it by analogy.
//!
//! INTERIM readings (flagged for a wow-re pass): victims' armor lands on the flesh slot (the
//! chain/plate slots need the armor-material chain);
//! blocks assume a metal shield; a defended outcome suppresses block 1's generic weapon impact
//! (the tail's latch test `0x624936` carries no victimState gate in the trace, so whether it
//! also plays under a clang is unpinned);
//! the injury vocal plays on every damaging hit (the client may
//! throttle); the deflect (`0x457f20`) and immune/absorb (`0x458610`) positioned stubs' kit ids
//! are unpinned, so those branches stay silent here; the natural-weapon column is gated on
//! contact like the weapon impact (whether the digit block also plays on a whiff is unpinned).
//! `$CPP`/`$CST` are pinned NON-audio (decision 0279): `$CPP` is the victim defense-anim
//! dispatch, `$CST` re-pings the attached combat-kit list — neither belongs to this module.

use bevy::ecs::entity::EntityHashMap;
use bevy::prelude::*;

use benilla_formats::{impact_slot, WeaponImpactCatalog};

use crate::creature_anim::{AnimSoundEvent, SwingImpact, SwingMessage, Wielded};
use crate::net::{Embodied, NetEntity};
use benilla_assets::{AssetSet, LockRecover, WorldAssets};
use benilla_protocol::EntityKind;
use benilla_world::schedule::WorldStage;

use super::creature::CreatureVoices;
use super::kit::{
    bark_chance_pass, object_sound_playing, play_kit_ext, Bus, KitRef, PlayExtras, SoundCategory,
    SoundKits, EXERTION_CHANCE_CREATURE, EXERTION_CHANCE_PLAYER,
};
use super::{AudioListener, SoundConfig, SoundOutput};

// vmangos `HitInfo` bits (UnitDefines.h, 1.12 wire).
const HITINFO_MISS: u32 = 0x10;
const HITINFO_CRITICAL: u32 = 0x80;
const HITINFO_CRUSHING: u32 = 0x8000;
// vmangos `VictimState`.
const VICTIM_DODGE: u32 = 2;
const VICTIM_PARRY: u32 = 3;
const VICTIM_BLOCK: u32 = 5;
const VICTIM_EVADE: u32 = 6;
const VICTIM_IMMUNE: u32 = 7;
const VICTIM_DEFLECT: u32 = 8;

/// The two `_DONOTRENAME_` whoosh kits the client caches by name at startup (wow-re `0x4575b0`);
/// byte-verified ids in the 5875 SoundEntries dump.
const COMBAT_MISS_1H: u32 = 7080;
const COMBAT_MISS_2H: u32 = 7081;

/// Weapon subclasses swung two-handed (item weapon subclass ids) — picks the 2H whoosh.
const TWO_HANDED: [u32; 6] = [1, 5, 6, 8, 10, 17];
/// `Material.dbc` id 2 — a wood-bodied item, which picks the non-metal impact row and the wood
/// parry slot. Read off the item itself (decision 0882), never inferred from its subclass.
const MATERIAL_WOOD: u8 = 2;
/// Fist/unarmed subclass — the row a weaponless swing uses (`Unarmed_Generic`).
const UNARMED_SUBCLASS: u32 = 13;

#[derive(Resource)]
pub(crate) struct WeaponImpacts(pub(crate) WeaponImpactCatalog);

fn load_weapon_impacts(mut commands: Commands, assets: Option<Res<WorldAssets>>) {
    let Some(assets) = assets else { return };
    let loaded = {
        let mut chain = assets.chain.lock_recover();
        benilla_formats::load_weapon_impact_catalog(&mut chain)
    };
    match loaded {
        Ok(cat) => {
            info!("sound: {} weapon impact rows", cat.len());
            commands.insert_resource(WeaponImpacts(cat));
        }
        Err(e) => warn!("sound: weapon impacts failed to load: {e:#}"),
    }
}

/// The latest swing outcome per attacker — written on the packet, read as the `$CSS` event
/// fires over the following frames, overwritten by the next swing. (The contact family does not
/// read it: [`SwingImpact`] carries its own consumed record, decision 0529. Neither does the
/// exertion vocal any more — it fires from the packet itself, so it never needs the record to
/// survive into a later frame.)
#[derive(Default)]
struct LastSwing(EntityHashMap<SwingMessage>);

/// The attacker's swinging weapon: `(subclass, wooden)`, unarmed when the hand is empty. The
/// wood-vs-metal half is the item's own **`Material`** off the wire (decision 0882) — not a
/// subclass guess, which the real 5875 data contradicts outright: maces (subclass 4) ship in both
/// materials, so a Cudgel is wood where a Mace is metal.
fn swing_weapon(wielded: Option<&Wielded>, offhand: bool) -> (u32, bool) {
    let hand = wielded.and_then(|w| if offhand { w.off } else { w.main });
    match hand {
        // class 2 = weapon; anything else in hand (held misc) swings as unarmed.
        Some((2, subclass)) => (
            u32::from(subclass),
            wielded.is_some_and(|w| w.materials[usize::from(offhand)] == MATERIAL_WOOD),
        ),
        _ => (UNARMED_SUBCLASS, false),
    }
}

/// The whiff/no-weapon-contact family: nothing for the weapon to strike. Immune and deflect
/// route to positioned stubs in the client (`0x457f20`/`0x458610`, decision 0279) whose kit ids
/// are unpinned — grouped here (whoosh, no impact) as the INTERIM stand-in.
fn no_contact(swing: &SwingMessage) -> bool {
    swing.hit_info & HITINFO_MISS != 0
        || matches!(
            swing.victim_state,
            VICTIM_DODGE | VICTIM_EVADE | VICTIM_IMMUNE | VICTIM_DEFLECT
        )
}

/// A defended outcome — parry or block. These take their sound from the victim dispatch's
/// victimState-keyed clang ([`defense_clang`]), never from the attacker's weapon-sound block.
fn defended(victim_state: u32) -> bool {
    matches!(victim_state, VICTIM_PARRY | VICTIM_BLOCK)
}

/// `0x624530`'s clang: the attacker's weapon row × the victim's defense (`0x623640`). Parry
/// picks metal/wood by the victim's own weapon body; block takes the shield slot. Crit does not
/// tier it — the parry/shield columns carry the same kit in both tables.
fn defense_clang(
    row: &benilla_formats::WeaponImpactRow,
    victim_state: u32,
    victim_wooden: bool,
) -> u32 {
    let slot = match (victim_state, victim_wooden) {
        (VICTIM_PARRY, true) => impact_slot::PARRY_WOOD,
        (VICTIM_PARRY, false) => impact_slot::PARRY_METAL,
        _ => impact_slot::SHIELD_METAL,
    };
    row.impact[slot]
}

/// `0x6247d0`'s generic weapon impact for a landed hit: the victim's `CreatureImpactType`
/// material slot off the attacker's weapon row, crit-tiered.
fn landed_impact(row: &benilla_formats::WeaponImpactRow, impact_type: u32, crit: bool) -> u32 {
    let slot = match impact_type {
        1 => impact_slot::STONE,
        2 => impact_slot::WOOD,
        3 => impact_slot::ETHEREAL,
        _ => impact_slot::FLESH,
    };
    if crit {
        row.crit[slot]
    } else {
        row.impact[slot]
    }
}

#[allow(clippy::too_many_arguments)]
fn combat_sounds(
    mut swings: MessageReader<SwingMessage>,
    mut contacts: MessageReader<SwingImpact>,
    mut events: MessageReader<AnimSoundEvent>,
    mut last: Local<LastSwing>,
    units: Query<(&Transform, Option<&Wielded>, &NetEntity, Has<Embodied>)>,
    impacts: Option<Res<WeaponImpacts>>,
    voices: Option<Res<CreatureVoices>>,
    kits: Option<ResMut<SoundKits>>,
    assets: Option<Res<WorldAssets>>,
    mut out: NonSendMut<SoundOutput>,
    config: Res<SoundConfig>,
    listener: Res<AudioListener>,
) {
    // The attacker's exertion vocal is **packet-driven**, not tag-driven (see the module note):
    // `SMSG_ATTACKERSTATEUPDATE` -> `0x6246a0` -> `0x624786`. Collected here with the swing record
    // so the vocal fires at swing start, which is where the reference puts it.
    let mut exertions: Vec<(Entity, bool)> = Vec::new();
    for s in swings.read() {
        last.0.insert(s.attacker, *s);
        // `0x62476a`: the vocal leg is gated on victimState, and ONLY on victimState — read off
        // the bytes rather than the second-hand note, which also claimed a victim-health gate
        // that is not in the function. A swing that contacted nothing at all is silent; every
        // other outcome, hit or parried or blocked, grunts.
        if s.victim_state != 0 {
            exertions.push((s.attacker, s.hit_info & HITINFO_CRITICAL != 0));
        }
    }
    // Bound the map: entries for despawned attackers die with the entity check below; a cheap
    // periodic sweep keeps a long session from accumulating dead keys.
    if last.0.len() > 128 {
        last.0.retain(|e, _| units.contains(*e));
    }
    if events.is_empty() && contacts.is_empty() && exertions.is_empty() {
        return;
    }
    let (Some(impacts), Some(voices), Some(mut kits), Some(assets)) =
        (impacts, voices, kits, assets)
    else {
        return;
    };
    let listener = listener.pos;
    // Every combat play carries its **voice bus** (decision 1555): the melee-contact family all
    // contends for bus 10's four voices, the vocals for their own one or two, and the miss whoosh
    // for nothing at all. A kit refused at the cap is not an error — it is the gate doing its job,
    // and `play_kit_ext` reports it as an ordinary silent success.
    let play =
        |kits: &mut SoundKits, out: &mut SoundOutput, kit: u32, pos: Vec3, bus: Bus, what: &str| {
            if kit == 0 {
                return;
            }
            if let Err(e) = play_kit_ext(
                kits,
                &assets,
                out,
                &config,
                listener,
                KitRef::Id(kit),
                Some(pos),
                SoundCategory::Sfx,
                PlayExtras { bus, ..default() },
            ) {
                warn!("combat {what} (kit {kit}): {e:#}");
            }
        };

    // The attacker's exertion vocal, at swing start. `force = 0` at `0x62477e`, so the class
    // chance roll applies: class 0 is 70 for a creature and 35 for a player, class 1
    // (ExertionCritical) is 100 in both twins — a crit always grunts, an ordinary swing thins out,
    // and a player grunts about half as often as a creature.
    for (attacker, crit) in exertions {
        let Ok((tr, _, net, _)) = units.get(attacker) else {
            continue;
        };
        // Same `AISOUNDDESC` gate, on the attacker this time — exertion is classes 0/1.
        if net.kind != EntityKind::Player && object_sound_playing(&out, attacker) {
            continue;
        }
        if !crit {
            let threshold = if net.kind == EntityKind::Player {
                EXERTION_CHANCE_PLAYER
            } else {
                EXERTION_CHANCE_CREATURE
            };
            if !bark_chance_pass(threshold, kits.roll()) {
                continue;
            }
        }
        let vocal = net
            .display_id
            .and_then(|d| voices.0.for_display(d))
            .map(|v| v.exertion[usize::from(crit)])
            .unwrap_or(0);
        play(
            &mut kits,
            &mut out,
            vocal,
            tr.translation,
            Bus::EXERTION,
            "exertion",
        );
    }

    // The one tag this module still consumes: the swing whoosh.
    for ev in events.read() {
        if ev.ident != *b"$CSS" {
            continue;
        }
        let Some(swing) = last.0.get(&ev.entity) else {
            continue; // an attack anim without a tracked swing (e.g. spawned mid-fight)
        };
        let Ok((attacker_tr, wielded, _, _)) = units.get(ev.entity) else {
            continue;
        };
        if no_contact(swing) {
            let offhand = swing.hit_info & 0x4 != 0;
            let (subclass, _) = swing_weapon(wielded, offhand);
            let kit = if TWO_HANDED.contains(&subclass) {
                COMBAT_MISS_2H
            } else {
                COMBAT_MISS_1H
            };
            play(
                &mut kits,
                &mut out,
                kit,
                attacker_tr.translation,
                Bus::DEFAULT,
                "miss whoosh",
            );
        }
    }

    // The contact family: the weapon-sound block + victim dispatch, at the impact crossing.
    for imp in contacts.read() {
        if imp.text_only {
            continue; // a supersede/stop flush carries only the floating text
        }
        let swing = &imp.swing;
        let attacker = units.get(swing.attacker).ok();
        let victim = swing.victim.and_then(|v| units.get(v).ok());
        // Positioned at the attacker; the receive-time fallback (unresolved attacker) emits at
        // the victim — the only anchor the packet leaves us.
        let Some(pos) = attacker
            .map(|(t, ..)| t.translation)
            .or_else(|| victim.map(|(t, ..)| t.translation))
        else {
            continue;
        };
        let crit = swing.hit_info & HITINFO_CRITICAL != 0;
        if !no_contact(swing) {
            // The attacker's weapon row (`0x625460(attacker, leftswing)`) — shared by both
            // blocks below. `None` for a wand/thrown: no melee row, nothing to strike with.
            let offhand = swing.hit_info & 0x4 != 0;
            let (subclass, wooden) = swing_weapon(attacker.and_then(|(_, w, ..)| w), offhand);
            let row = impacts.0.get(subclass, !wooden);
            let defended = defended(swing.victim_state);

            // `0x6247d0`'s own weapon-sound block, BEFORE the victim dispatch: the `$AHn` digit
            // column, else the generic `WeaponImpactSounds` impact behind the SWINGNOHITSOUND
            // latch. A defended outcome takes its sound from the dispatch below instead
            // (INTERIM, decision 0899: the tail's latch test `0x624936` carries no victimState
            // gate in the trace, so whether the generic impact ALSO plays under a clang is
            // unpinned — we suppress, which is what the game sounds like).
            if let Some(n) = imp.natural {
                // The attacker's own natural-weapon sound replaces the generic weapon impact.
                let vocal = attacker
                    .and_then(|(_, _, net, _)| net.display_id)
                    .and_then(|d| voices.0.for_display(d))
                    .and_then(|v| v.custom_attack.get(usize::from(n)).copied())
                    .unwrap_or(0);
                play(
                    &mut kits,
                    &mut out,
                    vocal,
                    pos,
                    Bus::MELEE_IMPACT,
                    "natural impact",
                );
            } else if !defended {
                if let Some(row) = row {
                    // A landed hit: the victim's material slot (players/rowless → flesh).
                    let material = victim
                        .and_then(|(_, _, net, _)| net.display_id)
                        .and_then(|d| voices.0.for_display(d))
                        .map(|v| v.impact_type)
                        .unwrap_or(0);
                    let kit = landed_impact(row, material, crit);
                    play(&mut kits, &mut out, kit, pos, Bus::MELEE_IMPACT, "impact");
                }
            }

            // `0x624530`'s victimState-keyed clang (`0x6245bb` parry → `0x623640(sel=0)`,
            // `0x6245d8` block → `sel=1`), emitted at the VICTIM (`vtable+0x14` on `this`).
            // Reached on EVERY impact tag — the `$AHn` digit block "has zero effect on the
            // victim dispatch" (wow-re `melee-impact-timing.md` §f): a wolf's bite and the
            // parry clang both sound. Decision 0899 — 0525 wrongly let the natural column
            // swallow this, so every beast you parried was silent.
            if let (true, Some(row)) = (defended, row) {
                // The parry family by the *victim's* weapon body (INTERIM heuristic).
                let victim_wooden = victim
                    .map(|(_, w, ..)| swing_weapon(w, false).1)
                    .unwrap_or(false);
                let kit = defense_clang(row, swing.victim_state, victim_wooden);
                let at = victim.map(|(t, ..)| t.translation).unwrap_or(pos);
                play(
                    &mut kits,
                    &mut out,
                    kit,
                    at,
                    Bus::MELEE_IMPACT,
                    "defense clang",
                );
            }
        }

        // The victim's wound vocal rides the same dispatch (INTERIM: unthrottled). An
        // absorbed/resisted hit reroutes the voice to the `0x458610` stub instead
        // (`HitInfo & 0x60`, decision 0279) — stub kit unpinned, so INTERIM silence.
        if swing.damage > 0
            && swing.hit_info & 0x60 == 0
            && !matches!(swing.victim_state, VICTIM_PARRY | VICTIM_BLOCK)
        {
            // The `AISOUNDDESC` gate (`0x4591f0` from `0x6234cb`): a server-pushed object sound
            // live on the victim suppresses its own vocal, classes 0-3 and 8. The CGPlayer twin
            // `0x62f880` omits the gate, so a player is never suppressed. Filtered off the victim
            // rather than `continue`d, because everything else in this iteration still stands.
            let vocal_victim = victim.filter(|(_, _, net, _)| {
                net.kind == EntityKind::Player
                    || !swing.victim.is_some_and(|v| object_sound_playing(&out, v))
            });
            if let Some((victim_tr, _, net, victim_is_you)) = vocal_victim {
                let crushing = swing.hit_info & HITINFO_CRUSHING != 0;
                let vocal = net
                    .display_id
                    .and_then(|d| voices.0.for_display(d))
                    .map(|v| {
                        let idx = if crushing { 2 } else { usize::from(crit) };
                        // Crushing rows are often 0 in data — fall back down the family.
                        [v.injury[idx], v.injury[usize::from(crit)], v.injury[0]]
                            .into_iter()
                            .find(|k| *k != 0)
                            .unwrap_or(0)
                    })
                    .unwrap_or(0);
                // Your own wounds get the CGPlayer twin's private bus 8 (cap 1); everyone
                // else's share the world's bus 7 (cap 2).
                let bus = if victim_is_you {
                    Bus::SELF_INJURY
                } else {
                    Bus::INJURY
                };
                play(
                    &mut kits,
                    &mut out,
                    vocal,
                    victim_tr.translation,
                    bus,
                    "injury",
                );
            }
        }
    }
}

/// Registration hook for [`super::SoundPlugin`].
pub(super) fn plugin(app: &mut App) {
    app.add_systems(Startup, load_weapon_impacts.after(AssetSet::Open))
        .add_systems(Update, combat_sounds.in_set(WorldStage::Present));
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A row shaped like the real 5875 Sword1H-metal row (byte-verified ids): flesh 143/144,
    /// parry-metal 1002, parry-wood 1001, shield-metal 3263.
    fn sword1h_metal() -> benilla_formats::WeaponImpactRow {
        let mut impact = [0u32; 10];
        let mut crit = [0u32; 10];
        impact[impact_slot::FLESH] = 143;
        crit[impact_slot::FLESH] = 144;
        impact[impact_slot::STONE] = 3206;
        impact[impact_slot::SHIELD_METAL] = 3263;
        crit[impact_slot::SHIELD_METAL] = 3263;
        impact[impact_slot::PARRY_METAL] = 1002;
        impact[impact_slot::PARRY_WOOD] = 1001;
        benilla_formats::WeaponImpactRow { impact, crit }
    }

    /// The clang picks the parry family by the VICTIM's weapon body, the shield slot for a
    /// block — and never crit-tiers (the data carries one kit in both tables).
    #[test]
    fn defense_clang_picks_parry_by_victim_body_and_shield_for_block() {
        let row = sword1h_metal();
        assert_eq!(defense_clang(&row, VICTIM_PARRY, false), 1002);
        assert_eq!(defense_clang(&row, VICTIM_PARRY, true), 1001);
        assert_eq!(defense_clang(&row, VICTIM_BLOCK, false), 3263);
        assert_eq!(defense_clang(&row, VICTIM_BLOCK, true), 3263);
    }

    /// Parry and block are the defended pair — the outcomes whose sound comes from the victim
    /// dispatch's clang instead of the attacker's weapon-sound block. A landed hit is not.
    #[test]
    fn only_parry_and_block_are_defended() {
        assert!(defended(VICTIM_PARRY));
        assert!(defended(VICTIM_BLOCK));
        for other in [
            0,
            1,
            VICTIM_DODGE,
            4,
            VICTIM_EVADE,
            VICTIM_IMMUNE,
            VICTIM_DEFLECT,
        ] {
            assert!(!defended(other), "victimState {other} is not a defense");
        }
    }

    /// The landed hit reads the victim's `CreatureImpactType` slot, crit-tiered; an unknown
    /// material falls to flesh (players carry no creature row).
    #[test]
    fn landed_impact_reads_material_slot_crit_tiered() {
        let row = sword1h_metal();
        assert_eq!(landed_impact(&row, 0, false), 143);
        assert_eq!(landed_impact(&row, 0, true), 144);
        assert_eq!(landed_impact(&row, 1, false), 3206);
        assert_eq!(landed_impact(&row, 99, false), 143, "unknown → flesh");
    }
}
