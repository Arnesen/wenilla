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
            // The attacker's weapon row (`0x625460(attacker, leftswing)`) — shared by both
            // blocks below. `None` for a wand/thrown: no melee row, nothing to strike with.
            let offhand = swing.hit_info & 0x4 != 0;
            let (subclass, wooden) = swing_weapon(attacker.and_then(|(_, w, _)| w), offhand);
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
                    .and_then(|(_, _, net)| net.display_id)
                    .and_then(|d| voices.0.for_display(d))
                    .and_then(|v| v.custom_attack.get(usize::from(n)).copied())
                    .unwrap_or(0);
                play(&mut kits, &mut out, vocal, pos, "natural impact");
            } else if !defended {
                if let Some(row) = row {
                    // A landed hit: the victim's material slot (players/rowless → flesh).
                    let material = victim
                        .and_then(|(_, _, net)| net.display_id)
                        .and_then(|d| voices.0.for_display(d))
                        .map(|v| v.impact_type)
                        .unwrap_or(0);
                    let kit = landed_impact(row, material, crit);
                    play(&mut kits, &mut out, kit, pos, "impact");
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
                    .map(|(_, w, _)| swing_weapon(w, false).1)
                    .unwrap_or(false);
                let kit = defense_clang(row, swing.victim_state, victim_wooden);
                let at = victim.map(|(t, ..)| t.translation).unwrap_or(pos);
                play(&mut kits, &mut out, kit, at, "defense clang");
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
