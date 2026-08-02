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
//! - **`$CAH`** — the attacker's exertion vocal (`CreatureSoundData.Exertion[Critical]`, the
//!   same voice chain as everything else — characters resolve via the model fallback). INTERIM
//!   (0075's pre-§5 reading): the §5s that pinned `$CAH` as the victim-dispatch trigger
//!   (decisions 0149/0279) traced no exertion play on its byte route — where the exertion
//!   columns actually fire is unpinned, a wow-re sliver; the audible result is accepted.
//!
//! The **contact family** consumes [`SwingImpact`] — the victim dispatch `0x624530` + the
//! `0x6247d0` weapon-sound block, fired at the swing clip's **`$AH0–3`/`$CAH`** crossing, or at
//! receive for an unresolved attacker (decision 0529; previously keyed on `$HIT`, a CEffect-only
//! tag many creature attack clips never author — ogre.m2 authors it in 1 of its ~14 attack
//! variations, so ogre hits were near-silent):
//!
//! - a **natural-weapon** swing (`$AHn` fired the dispatch) plays the attacker's
//!   `CreatureSoundData.CustomAttack[n]` column INSTEAD of the generic weapon impact — the
//!   `SWINGNOHITSOUND` latch (`0x6247d0` §f);
//! - otherwise: parry → the attacker row's parry slot (metal/wood by the victim's weapon),
//!   block → the shield slot, a landed hit → the `WeaponImpactSounds` impact/crit slot for the
//!   victim's material (`CreatureImpactType`);
//! - plus the victim's injury vocal (`Injury`/`InjuryCritical`/`InjuryCrushing`) on every
//!   damaging, undefended hit.
//!
//! A `text_only` flush (supersede/attack-stop) drops its sounds — only the floating number
//! flushes (decision 0149's flush law, inherited from the shared dispatch).
//!
//! INTERIM readings (flagged for a wow-re pass): victims' armor lands on the flesh slot (the
//! chain/plate slots need the armor-material chain);
//! blocks assume a metal shield; the injury vocal plays on every damaging hit (the client may
//! throttle); the deflect (`0x457f20`) and immune/absorb (`0x458610`) positioned stubs' kit ids
//! are unpinned, so those branches stay silent here; the natural-weapon column is gated on
//! contact like the weapon impact (whether the digit block also plays on a whiff is unpinned).
//! `$CPP`/`$CST` are pinned NON-audio (decision 0279): `$CPP` is the victim defense-anim
//! dispatch, `$CST` re-pings the attached combat-kit list — neither belongs to this module.

use bevy::ecs::entity::EntityHashMap;
use bevy::prelude::*;

use benilla_formats::{impact_slot, WeaponImpactCatalog};

use crate::assets::{AssetSet, LockRecover, WorldAssets};
use crate::creature_anim::{AnimSoundEvent, SwingImpact, SwingMessage, Wielded};
use crate::net::NetEntity;
use crate::schedule::WorldStage;

use super::creature::CreatureVoices;
use super::kit::{play_kit, KitRef, SoundCategory, SoundKits};
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

/// The latest swing outcome per attacker — written on the packet, read as the `$CSS`/`$CAH`
/// events fire over the following frames, overwritten by the next swing. (The contact family
/// no longer reads it: [`SwingImpact`] carries its own consumed record, decision 0529.)
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

#[allow(clippy::too_many_arguments)]
fn combat_sounds(
    mut swings: MessageReader<SwingMessage>,
    mut contacts: MessageReader<SwingImpact>,
    mut events: MessageReader<AnimSoundEvent>,
    mut last: Local<LastSwing>,
    units: Query<(&Transform, Option<&Wielded>, &NetEntity)>,
    impacts: Option<Res<WeaponImpacts>>,
    voices: Option<Res<CreatureVoices>>,
    kits: Option<ResMut<SoundKits>>,
    assets: Option<Res<WorldAssets>>,
    mut out: NonSendMut<SoundOutput>,
    config: Res<SoundConfig>,
    listener: Res<AudioListener>,
) {
    for s in swings.read() {
        last.0.insert(s.attacker, *s);
    }
    // Bound the map: entries for despawned attackers die with the entity check below; a cheap
    // periodic sweep keeps a long session from accumulating dead keys.
    if last.0.len() > 128 {
        last.0.retain(|e, _| units.contains(*e));
    }
    if events.is_empty() && contacts.is_empty() {
        return;
    }
    let (Some(impacts), Some(voices), Some(mut kits), Some(assets)) =
        (impacts, voices, kits, assets)
    else {
        return;
    };
    let listener = listener.pos;
    let play = |kits: &mut SoundKits, out: &mut SoundOutput, kit: u32, pos: Vec3, what: &str| {
        if kit == 0 {
            return;
        }
        if let Err(e) = play_kit(
            kits,
            &assets,
            out,
            &config,
            listener,
            KitRef::Id(kit),
            Some(pos),
            SoundCategory::Sfx,
        ) {
            warn!("combat {what} (kit {kit}): {e:#}");
        }
    };

    // The tag-driven pair: the whoosh and the exertion vocal.
    for ev in events.read() {
        let is = |tag: &[u8; 4]| &ev.ident == tag;
        if !(is(b"$CSS") || is(b"$CAH")) {
            continue;
        }
        let Some(swing) = last.0.get(&ev.entity) else {
            continue; // an attack anim without a tracked swing (e.g. spawned mid-fight)
        };
        let Ok((attacker_tr, wielded, net)) = units.get(ev.entity) else {
            continue;
        };
        let pos = attacker_tr.translation;
        if is(b"$CSS") {
            if no_contact(swing) {
                let offhand = swing.hit_info & 0x4 != 0;
                let (subclass, _) = swing_weapon(wielded, offhand);
                let kit = if TWO_HANDED.contains(&subclass) {
                    COMBAT_MISS_2H
                } else {
                    COMBAT_MISS_1H
                };
                play(&mut kits, &mut out, kit, pos, "miss whoosh");
            }
        } else {
            let crit = swing.hit_info & HITINFO_CRITICAL != 0;
            let vocal = net
                .display_id
                .and_then(|d| voices.0.for_display(d))
                .map(|v| v.exertion[usize::from(crit)])
                .unwrap_or(0);
            play(&mut kits, &mut out, vocal, pos, "exertion");
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
            if let Some(n) = imp.natural {
                // The `$AHn` digit column: the attacker's own natural-weapon sound replaces the
                // generic weapon impact (the SWINGNOHITSOUND latch).
                let vocal = attacker
                    .and_then(|(_, _, net)| net.display_id)
                    .and_then(|d| voices.0.for_display(d))
                    .and_then(|v| v.custom_attack.get(usize::from(n)).copied())
                    .unwrap_or(0);
                play(&mut kits, &mut out, vocal, pos, "natural impact");
            } else {
                let offhand = swing.hit_info & 0x4 != 0;
                let (subclass, wooden) = swing_weapon(attacker.and_then(|(_, w, _)| w), offhand);
                if let Some(row) = impacts.0.get(subclass, !wooden) {
                    let kit = match swing.victim_state {
                        VICTIM_PARRY => {
                            // The parry family by the *victim's* weapon body (INTERIM heuristic).
                            let victim_wooden = victim
                                .map(|(_, w, _)| swing_weapon(w, false).1)
                                .unwrap_or(false);
                            row.impact[if victim_wooden {
                                impact_slot::PARRY_WOOD
                            } else {
                                impact_slot::PARRY_METAL
                            }]
                        }
                        VICTIM_BLOCK => row.impact[impact_slot::SHIELD_METAL],
                        _ => {
                            // A landed hit: the victim's material slot (players/rowless → flesh).
                            let material = victim
                                .and_then(|(_, _, net)| net.display_id)
                                .and_then(|d| voices.0.for_display(d))
                                .map(|v| v.impact_type)
                                .unwrap_or(0);
                            let slot = match material {
                                1 => impact_slot::STONE,
                                2 => impact_slot::WOOD,
                                3 => impact_slot::ETHEREAL,
                                _ => impact_slot::FLESH,
                            };
                            let table = if crit { &row.crit } else { &row.impact };
                            table[slot]
                        }
                    };
                    play(&mut kits, &mut out, kit, pos, "impact");
                }
                // else: no melee row (wand/thrown) — nothing to strike with.
            }
        }

        // The victim's wound vocal rides the same dispatch (INTERIM: unthrottled). An
        // absorbed/resisted hit reroutes the voice to the `0x458610` stub instead
        // (`HitInfo & 0x60`, decision 0279) — stub kit unpinned, so INTERIM silence.
        if swing.damage > 0
            && swing.hit_info & 0x60 == 0
            && !matches!(swing.victim_state, VICTIM_PARRY | VICTIM_BLOCK)
        {
            if let Some((victim_tr, _, net)) = victim {
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
                play(&mut kits, &mut out, vocal, victim_tr.translation, "injury");
            }
        }
    }
}

/// Registration hook for [`super::SoundPlugin`].
pub(super) fn plugin(app: &mut App) {
    app.add_systems(Startup, load_weapon_impacts.after(AssetSet::Open))
        .add_systems(Update, combat_sounds.in_set(WorldStage::Present));
}
