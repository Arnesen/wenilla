//! The melee **impact-frame deferral** — the client's swing-hit timing, §5-verified end to end
//! (wow-re `object-layer/scratch/melee-impact-timing.md`, f86e665a; the victim-consequence table
//! `smsg-attackerstate-consequences.md`, ffd0b016): at SMSG receive, a locally resolved attacker
//! caches the swing record (`attacker+0xd70`), plays the swing clip (rate 1.0, `0x5fe2f0`), and
//! does **not** dispatch the victim. When the clip's playhead crosses an authored **`$AH0–3` or
//! `$CAH`** event keyframe (rel ~300–670 ms in every shipped attack clip; the `$AH` digit picks a
//! sound column only), the event kernel fires `0x6247d0 → 0x624530(victim, attacker+0xd70)` — all
//! four victim elements at the impact frame — and clears the record (fires once). **`$HIT` never
//! reaches the dispatcher** — it is a CEffect-model tag that re-fires only the wound flinch, and
//! it also rides hit-REACTION clips, so matching it would fire pending swings at wrong moments.
//!
//! Two more consequences ride the same event router (decision 0279):
//!
//! - **`$CPP` — the victim DEFENSE animation** (`0x624a01`): a defended outcome (dodge/parry/
//!   block/deflect) plays a dedicated primary clip on the victim ([`DefenseAnim`]) and **clears
//!   `HitInfo & 0x2` in the cached record** (`and [esi+0xd80],-3`), suppressing the wound
//!   flinch + blood for that hit. The record itself survives `$CPP` — so `$CPP` before the
//!   impact tag defends and then texts/sounds, while an impact tag first **consumes** the record
//!   and a late `$CPP` finds nothing: the two are mutually exclusive per authored event order,
//!   exactly the client's shape.
//! - **The whiff slow-down** (`0x624ca0` → `0x712910(bone 0, 0.5f)`): a swing that contacted
//!   nothing (victimState ∈ {0 miss, 2 dodge, 6 evade}) drops the attacker's own swing to HALF
//!   speed for its remainder ([`SwingSlowdown`]) — a slowed follow-through, not a cut.
//!
//! The record law (byte-verified): **one slot per attacker, overwrite** — a second swing before
//! the first impact supersede-flushes the old record as **floating text only** (blood/flinch/sound
//! drop); `SMSG_ATTACKSTOP` (also how death/stun arrive) flushes the same way; a resolved attacker
//! whose clip has **no** impact tag never fires the full set — the client drops, it does not fall
//! back to a timer. The only receive-time victim dispatch is an attacker that doesn't resolve at
//! all (`net/apply`'s direct [`SwingImpact`] write).
//!
//! benilla's twin: [`route_swing_impacts`] caches each [`SwingMessage`], consumes the first
//! matching tag from the [`AnimSoundEvent`] stream, and emits [`SwingImpact`] — full on the tag,
//! `text_only` on supersede/attack-stop. Consumers: blood + flinch + the contact sounds
//! (`sound::combat`, decision 0529) skip `text_only`; the floating number takes all.

use bevy::prelude::*;

use super::events::AnimSoundEvent;
use super::SwingMessage;

/// A melee swing's **impact moment** — the victim-feedback trigger, re-emitted from
/// [`route_swing_impacts`] at the swing clip's attack-hit keyframe (or a flush).
#[derive(Message, Clone, Copy)]
pub(crate) struct SwingImpact {
    pub(crate) swing: SwingMessage,
    /// A supersede/attack-stop flush (`0x624e40` / the 0x14a overwrite): the client fires ONLY
    /// the floating text for the displaced record — blood, flinch, and impact sounds drop.
    pub(crate) text_only: bool,
    /// The `$AH0`–`$AH3` digit when a creature natural-weapon tag fired this dispatch — it
    /// selects the attacker's `CreatureSoundData.CustomAttack` sound column and latches
    /// `SWINGNOHITSOUND`, replacing the generic weapon-impact sound (`0x6247d0` §f, decision
    /// 0525). `None` for `$CAH` (character attack-hit), the receive-time unresolved-attacker
    /// fallback, and flushes.
    pub(crate) natural: Option<u8>,
}

/// A flush signal for an attacker's pending swing record — `SMSG_ATTACKSTOP`'s `0x624e40`
/// (death and stun arrive as the same packet). Written by `net/apply`; text-only flush + clear.
#[derive(Message, Clone, Copy)]
pub(crate) struct SwingFlush(pub(crate) Entity);

/// The victim plays its defense reaction (decision 0279 — the `$CPP` dispatch `0x624a01`):
/// dodge/deflect → Dodge, block → ShieldBlock, parry → by the victim's own weapon
/// ([`super::select::defense_anim`] resolves in the driver, which holds the victim's `Wielded`
/// and death state — the client gates on alive there too).
#[derive(Message, Clone, Copy)]
pub(crate) struct DefenseAnim {
    pub(crate) victim: Entity,
    pub(crate) victim_state: u32,
}

/// The attacker's in-flight swing drops to half speed for its remainder (decision 0279 — the
/// whiff slow-down `0x712910`, fired at the impact tag when victimState ∈ {0, 2, 6}).
#[derive(Message, Clone, Copy)]
pub(crate) struct SwingSlowdown(pub(crate) Entity);

/// A cached swing awaiting its clip's event keyframes (the client's `attacker+0xd70` slot):
/// `defended` latches the one `$CPP` dispatch (the shipped clips author it once; a variation
/// wrap must not re-defend the same record).
pub(crate) struct PendingSwing {
    pub(crate) swing: SwingMessage,
    defended: bool,
}

/// Pending swing records keyed by attacker (the client's `attacker+0xd70` cache, one slot each):
/// the swing waiting for its clip's impact keyframe.
#[derive(Resource, Default)]
pub(crate) struct PendingImpacts(pub(crate) bevy::ecs::entity::EntityHashMap<PendingSwing>);

/// An attack-hit tag (`0x6247d0`'s matching set, byte-verified): `$AH0`–`$AH3` (the digit selects
/// the attacker's natural-weapon sound column — [`SwingImpact::natural`]) or `$CAH` (the
/// character-model attack-hit — every shipped character attack clip authors it; asset-verified
/// on HumanMale, co-authored with the inert `$HIT` at the same timestamp). NOT `$HIT`.
/// Outer `Some` = "fires the dispatch"; the inner value is [`SwingImpact::natural`].
fn impact_tag(ident: &[u8; 4]) -> Option<Option<u8>> {
    match ident {
        b"$AH0" => Some(Some(0)),
        b"$AH1" => Some(Some(1)),
        b"$AH2" => Some(Some(2)),
        b"$AH3" => Some(Some(3)),
        b"$CAH" => Some(None),
        _ => None,
    }
}

/// The whiff gate `0x624ca0`: the outcomes where the swing contacted nothing — miss, dodge,
/// evade. Parry/block still contact (steel meets steel/shield), so they never slow the swing.
fn is_whiff(victim_state: u32) -> bool {
    matches!(victim_state, 0 | 2 | 6)
}

/// Cache swings, consume impact tags, dispatch `$CPP` defenses, flush on supersede/attack-stop,
/// silently drop records whose attacker despawned (the client's dtor clears without flushing).
/// Ordered after [`super::events::fire_anim_events`] so a tag fires the same frame it's crossed.
#[allow(clippy::too_many_arguments)]
pub(super) fn route_swing_impacts(
    mut swings: MessageReader<SwingMessage>,
    mut events: MessageReader<AnimSoundEvent>,
    mut flushes: MessageReader<SwingFlush>,
    mut pending: ResMut<PendingImpacts>,
    entities: Query<(), With<crate::net::NetEntity>>,
    mut out: MessageWriter<SwingImpact>,
    mut defenses: MessageWriter<DefenseAnim>,
    mut slows: MessageWriter<SwingSlowdown>,
) {
    for s in swings.read() {
        // One slot per attacker, overwrite: the superseded record flushes TEXT ONLY (the client's
        // 0x14a supersede-flush fires 0x6243e0 alone; the old swing's blood/flinch/sound drop).
        if let Some(old) = pending.0.insert(
            s.attacker,
            PendingSwing {
                swing: *s,
                defended: false,
            },
        ) {
            if crate::dbg_trace::enabled() {
                crate::dbg_trace::line(
                    "fct",
                    &format!(
                        "flush supersede atk={:?} dmg={}",
                        s.attacker, old.swing.damage
                    ),
                );
            }
            out.write(SwingImpact {
                swing: old.swing,
                text_only: true,
                natural: None,
            });
        }
    }
    for ev in events.read() {
        if let Some(natural) = impact_tag(&ev.ident) {
            // Fires once: the tag consumes the record (`0x6247d0` clears the GUIDs after
            // dispatch) — a `$CPP` authored after this frame finds nothing (mutual exclusion).
            if let Some(p) = pending.0.remove(&ev.entity) {
                if crate::dbg_trace::enabled() {
                    crate::dbg_trace::line(
                        "fct",
                        &format!(
                            "impact tag={} atk={:?} dmg={}",
                            String::from_utf8_lossy(&ev.ident),
                            ev.entity,
                            p.swing.damage
                        ),
                    );
                }
                if is_whiff(p.swing.victim_state) {
                    // The whiff slow-down rides the same crossing (`0x624ca0` gate).
                    slows.write(SwingSlowdown(ev.entity));
                }
                out.write(SwingImpact {
                    swing: p.swing,
                    text_only: false,
                    natural,
                });
            } else if crate::dbg_trace::enabled() {
                crate::dbg_trace::line(
                    "fct",
                    &format!(
                        "impact tag={} atk={:?} NO-PENDING",
                        String::from_utf8_lossy(&ev.ident),
                        ev.entity
                    ),
                );
            }
        } else if &ev.ident == b"$CPP" {
            // The victim defense dispatch: mutates the cached record (clears the flinch bit),
            // never consumes it — the impact tag still texts/sounds afterwards.
            if let Some(p) = pending.0.get_mut(&ev.entity) {
                if !p.defended {
                    p.defended = true;
                    if matches!(p.swing.victim_state, 2 | 3 | 5 | 8) {
                        if let Some(victim) = p.swing.victim {
                            defenses.write(DefenseAnim {
                                victim,
                                victim_state: p.swing.victim_state,
                            });
                        }
                        // `and [esi+0xd80],-3`: a defended hit never flinches or bleeds.
                        p.swing.hit_info &= !0x2;
                    }
                }
            }
        }
    }
    for SwingFlush(attacker) in flushes.read() {
        // SMSG_ATTACKSTOP → 0x624e40: text-only flush + clear (death/stun arrive as this packet).
        if let Some(p) = pending.0.remove(attacker) {
            if crate::dbg_trace::enabled() {
                crate::dbg_trace::line(
                    "fct",
                    &format!("flush stop atk={:?} dmg={}", attacker, p.swing.damage),
                );
            }
            out.write(SwingImpact {
                swing: p.swing,
                text_only: true,
                natural: None,
            });
        }
    }
    // A record whose attacker despawned mid-swing drops silently (the client's unit dtor clears
    // the cache without flushing). A resolved-but-untagged clip's record likewise just sits until
    // superseded or stopped — the client has NO timer fallback.
    if !pending.0.is_empty() {
        pending.0.retain(|e, _| entities.contains(*e));
    }
}

#[cfg(test)]
mod tests {
    use bevy::ecs::message::MessageCursor;

    use super::*;

    fn swing(attacker: Entity, victim: Option<Entity>, victim_state: u32) -> SwingMessage {
        SwingMessage {
            attacker,
            victim,
            hit_info: 0x2,
            victim_state,
            damage: if victim_state == 1 { 42 } else { 0 },
            seq: 1,
        }
    }

    fn app() -> App {
        let mut app = App::new();
        app.init_resource::<PendingImpacts>();
        app.add_message::<SwingMessage>();
        app.add_message::<AnimSoundEvent>();
        app.add_message::<SwingFlush>();
        app.add_message::<SwingImpact>();
        app.add_message::<DefenseAnim>();
        app.add_message::<SwingSlowdown>();
        app.add_systems(Update, route_swing_impacts);
        app
    }

    fn unit(app: &mut App) -> Entity {
        app.world_mut()
            .spawn(crate::net::NetEntity {
                kind: benilla_protocol::EntityKind::Unit,
                display_id: None,
                scale: 1.0,
            })
            .id()
    }

    fn tag(app: &mut App, entity: Entity, ident: [u8; 4]) {
        app.world_mut().write_message(AnimSoundEvent {
            entity,
            ident,
            data: 0,
        });
        app.update();
    }

    /// Read the impacts emitted since the last call — one persistent cursor per test (a fresh
    /// cursor would re-read the double buffer's previous frame).
    #[allow(clippy::type_complexity)]
    fn drain(
        app: &mut App,
        cursor: &mut MessageCursor<SwingImpact>,
    ) -> Vec<(u32, u32, bool, Option<u8>)> {
        let msgs = app.world().resource::<Messages<SwingImpact>>();
        cursor
            .read(msgs)
            .map(|i| {
                (
                    i.swing.hit_info,
                    i.swing.victim_state,
                    i.text_only,
                    i.natural,
                )
            })
            .collect()
    }

    fn drain_defenses(app: &mut App, cursor: &mut MessageCursor<DefenseAnim>) -> Vec<u32> {
        let msgs = app.world().resource::<Messages<DefenseAnim>>();
        cursor.read(msgs).map(|d| d.victim_state).collect()
    }

    fn drain_slows(app: &mut App, cursor: &mut MessageCursor<SwingSlowdown>) -> usize {
        let msgs = app.world().resource::<Messages<SwingSlowdown>>();
        cursor.read(msgs).count()
    }

    /// A swing waits for its tag; the first `$AH`/`$CAH` fires it FULL exactly once; `$HIT` and
    /// `$CSS` never consume it (the byte-verified matching set — `$HIT` is flinch-only and rides
    /// hit-REACTION clips too).
    #[test]
    fn impact_fires_full_on_ah_or_cah_never_on_hit() {
        let mut app = app();
        let mut cursor = MessageCursor::<SwingImpact>::default();
        let a = unit(&mut app);
        app.world_mut().write_message(swing(a, None, 1));
        app.update();
        tag(&mut app, a, *b"$CSS");
        tag(&mut app, a, *b"$HIT");
        assert_eq!(
            drain(&mut app, &mut cursor),
            vec![],
            "$HIT must not fire it"
        );
        tag(&mut app, a, *b"$CAH");
        assert_eq!(drain(&mut app, &mut cursor), vec![(0x2, 1, false, None)]);
        app.update();
        assert!(drain(&mut app, &mut cursor).is_empty(), "fires once");
    }

    /// `$CPP` before the impact tag: the defense fires once (latched), the cached record's
    /// flinch bit is cleared, and the impact still texts — with `0x2` gone (decision 0279's
    /// mutual exclusion, `$CPP`-first direction).
    #[test]
    fn cpp_defends_clears_flinch_then_impact_texts() {
        let mut app = app();
        let mut cursor = MessageCursor::<SwingImpact>::default();
        let mut d_cursor = MessageCursor::<DefenseAnim>::default();
        let a = unit(&mut app);
        let v = unit(&mut app);
        app.world_mut().write_message(swing(a, Some(v), 3));
        app.update();
        tag(&mut app, a, *b"$CPP");
        assert_eq!(drain_defenses(&mut app, &mut d_cursor), vec![3]);
        tag(&mut app, a, *b"$CPP");
        assert_eq!(
            drain_defenses(&mut app, &mut d_cursor),
            Vec::<u32>::new(),
            "defended latch: a second $CPP is a no-op"
        );
        tag(&mut app, a, *b"$AH0");
        assert_eq!(
            drain(&mut app, &mut cursor),
            vec![(0x0, 3, false, Some(0))],
            "the impact fires with the flinch bit cleared"
        );
    }

    /// The impact tag first CONSUMES the record: a late `$CPP` finds nothing (the client's
    /// event-order mutual exclusion, impact-first direction).
    #[test]
    fn impact_first_consumes_no_late_defense() {
        let mut app = app();
        let mut cursor = MessageCursor::<SwingImpact>::default();
        let mut d_cursor = MessageCursor::<DefenseAnim>::default();
        let a = unit(&mut app);
        let v = unit(&mut app);
        app.world_mut().write_message(swing(a, Some(v), 5));
        app.update();
        tag(&mut app, a, *b"$CAH");
        assert_eq!(drain(&mut app, &mut cursor), vec![(0x2, 5, false, None)]);
        tag(&mut app, a, *b"$CPP");
        assert_eq!(
            drain_defenses(&mut app, &mut d_cursor),
            Vec::<u32>::new(),
            "consumed record: no late defense"
        );
    }

    /// The whiff slow-down rides the impact crossing for miss/dodge/evade only — a landed hit
    /// (or a parry/block, which still contact) never slows.
    #[test]
    fn whiff_slows_on_impact_tag() {
        let mut app = app();
        let mut s_cursor = MessageCursor::<SwingSlowdown>::default();
        let a = unit(&mut app);
        let v = unit(&mut app);
        app.world_mut().write_message(swing(a, Some(v), 2));
        app.update();
        tag(&mut app, a, *b"$AH1");
        assert_eq!(drain_slows(&mut app, &mut s_cursor), 1, "a dodge slows");
        app.world_mut().write_message(swing(a, Some(v), 1));
        app.update();
        tag(&mut app, a, *b"$AH1");
        assert_eq!(drain_slows(&mut app, &mut s_cursor), 0, "a hit never slows");
    }

    /// A second swing supersedes the first: the displaced record flushes TEXT ONLY; the new one
    /// still waits for its own tag and fires full.
    #[test]
    fn superseded_swing_flushes_text_only() {
        let mut app = app();
        let mut cursor = MessageCursor::<SwingImpact>::default();
        let a = unit(&mut app);
        app.world_mut().write_message(swing(a, None, 1));
        app.update();
        app.world_mut().write_message(swing(a, None, 5));
        app.update();
        assert_eq!(drain(&mut app, &mut cursor), vec![(0x2, 1, true, None)]);
        tag(&mut app, a, *b"$AH1");
        assert_eq!(drain(&mut app, &mut cursor), vec![(0x2, 5, false, Some(1))]);
    }

    /// SMSG_ATTACKSTOP (death/stun arrive as it) flushes the pending record text-only; an
    /// untagged record with no stop just sits (the client has no timer fallback).
    #[test]
    fn attack_stop_flushes_text_only() {
        let mut app = app();
        let mut cursor = MessageCursor::<SwingImpact>::default();
        let a = unit(&mut app);
        app.world_mut().write_message(swing(a, None, 1));
        app.update();
        app.update();
        assert!(drain(&mut app, &mut cursor).is_empty(), "no timer fallback");
        app.world_mut().write_message(SwingFlush(a));
        app.update();
        assert_eq!(drain(&mut app, &mut cursor), vec![(0x2, 1, true, None)]);
    }
}
