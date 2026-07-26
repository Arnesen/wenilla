//! `--aura`: the aura wire (decision 0255 phase 1). GM-apply a buff + a DoT with explicit durations,
//! require both in `UNIT_FIELD_AURA` (correct half, cancelable nibble, level byte, stack bias), and
//! require an `SMSG_UPDATE_AURA_DURATION` per aura ordered BEFORE the descriptor delta that names it.

use std::time::{Duration, Instant};

use anyhow::{bail, ensure, Context, Result};
use benilla_protocol::{decode, ObjectFields, ServerPacket, SessionEvent};

use crate::probes::{Ctx, Probe};

// --aura: two spells with opposite sign, applied to ourselves by the GM `.aura <spell> <seconds>`
// command (`ChatHandler::HandleAuraCommand` → `HandleAuraHelper`, `UnitCommands.cpp:997-1051`;
// with no selection it targets the caster — `ChatHandler::GetSelectedUnit`, `Chat.cpp:2621-2622`).
/// Mark of the Wild — positive, and lacks `SPELL_ATTR_NO_AURA_CANCEL`, so the wire's cancelable bit
/// must be set. Lands in the positive half (slots 0–31).
const AURA_BUFF_SPELL: u32 = 1126;
/// Shadow Word: Pain — `SPELL_AURA_PERIODIC_DAMAGE`, unambiguously negative however it is applied.
/// Lands in the negative half (slots 32–47) and must NOT be cancelable.
const AURA_DEBUFF_SPELL: u32 = 589;
const AURA_BUFF_SECONDS: u32 = 300;
/// Short: this one ticks damage on us, and we want it gone even if the cleanup `.unaura` is missed.
const AURA_DEBUFF_SECONDS: u32 = 15;

pub(crate) struct Aura;

impl Probe for Aura {
    fn verify(&mut self, cx: &mut Ctx) -> Result<()> {
        let self_guid = cx.world.self_guid;
        let self_level = cx.world.self_level;
        // --aura: live-verify the aura descriptor block + SMSG_UPDATE_AURA_DURATION (decision 0255).
        // Nothing here trusts a field index: the probe *searches* the decoded slots for the spell ids it
        // asked for, so a wrong `FIELD_UNIT_AURA` fails as "never appeared" rather than passing on
        // garbage.
        // Start from the LOGIN snapshot, not an empty store: a values delta only carries *changed*
        // fields, so a delta-only view would silently hide auras the server restored at login from
        // `character_aura` (they persist across logout) — and then misreport which slots are free.
        let mut fields = cx.world.self_fields.clone().unwrap_or_default();
        let session = &mut *cx.session;
        let dump = |label: &str, f: &ObjectFields| {
            println!("\nUNIT_FIELD_AURA {label}:");
            for a in f.unit_auras() {
                println!(
                    "  slot {:>2} spell {:>5} flags {:#06b} level {:>2} stacks {} ({}{})",
                    a.slot,
                    a.spell_id,
                    a.flags,
                    a.level,
                    a.stacks,
                    if a.is_helpful() { "buff" } else { "debuff" },
                    if a.is_cancelable() {
                        ", cancelable"
                    } else {
                        ""
                    },
                );
            }
        };
        dump("at login (restored from character_aura)", &fields);

        // Clean slate, so a re-run measures a fresh apply and not a leftover refresh. `.unaura`
        // zeroes the slot, which reaches us as an explicit `0` in the next delta.
        session.send_chat(&format!(".unaura {AURA_BUFF_SPELL}"))?;
        session.send_chat(&format!(".unaura {AURA_DEBUFF_SPELL}"))?;
        let settle = Instant::now() + Duration::from_secs(3);
        while Instant::now() < settle {
            let Ok(msg) = session.recv() else { continue };
            for ev in decode(msg) {
                if let SessionEvent::ObjectValues { guid, fields: d } = ev {
                    if guid == self_guid {
                        fields.merge(d);
                    }
                }
            }
        }
        dump("after .unaura of both probe spells", &fields);
        // Whatever survives is not ours to touch (a warrior's Battle Stance, 2457, permanently owns
        // slot 0). Nothing may report a duration for these while we watch: the server sends
        // `SMSG_UPDATE_AURA_DURATION` only on apply/refresh, and never at all for a permanent aura —
        // which is precisely the reference's "until cancelled" (an occupied slot with no timer).
        let untouched: Vec<u8> = fields.unit_auras().map(|a| a.slot).collect();

        println!("\nGM: .aura {AURA_BUFF_SPELL} {AURA_BUFF_SECONDS} (Mark of the Wild)");
        println!("GM: .aura {AURA_DEBUFF_SPELL} {AURA_DEBUFF_SECONDS} (Shadow Word: Pain)");
        session.send_chat(&format!(".aura {AURA_BUFF_SPELL} {AURA_BUFF_SECONDS}"))?;
        session.send_chat(&format!(".aura {AURA_DEBUFF_SPELL} {AURA_DEBUFF_SECONDS}"))?;

        // Record the arrival ORDER of the first duration packet against the values delta that first
        // names the buff's spell in its slot — both indexed by PACKET arrival, the axis the ordering
        // claim is actually about. `SMSG_UPDATE_AURA_DURATION` has no `SessionEvent` yet (nothing in
        // the app consumes it until phase 2), so the probe reads the `ServerPacket` directly.
        let mut durations: Vec<(u8, u32)> = Vec::new();
        let (mut seq, mut first_duration_at, mut buff_field_at) = (0usize, None, None);
        let drain_until = Instant::now() + Duration::from_secs(8);
        while Instant::now() < drain_until {
            let Ok(msg) = session.recv() else { continue };
            seq += 1;
            if let ServerPacket::UpdateAuraDuration { slot, remaining_ms } = &msg {
                let (slot, remaining_ms) = (*slot, *remaining_ms);
                println!("  SMSG_UPDATE_AURA_DURATION slot {slot} → {remaining_ms} ms");
                first_duration_at.get_or_insert(seq);
                durations.push((slot, remaining_ms));
                continue;
            }
            for ev in decode(msg) {
                if let SessionEvent::ObjectValues { guid, fields: d } = ev {
                    if guid == self_guid {
                        fields.merge(d);
                        if fields.unit_auras().any(|a| a.spell_id == AURA_BUFF_SPELL) {
                            buff_field_at.get_or_insert(seq);
                        }
                    }
                }
            }
        }

        let auras: Vec<_> = fields.unit_auras().collect();
        dump("after both .aura applies", &fields);

        let buff = auras
            .iter()
            .find(|a| a.spell_id == AURA_BUFF_SPELL)
            .copied()
            .context(
                "--aura: spell 1126 never appeared in UNIT_FIELD_AURA. Either the descriptor field \
                 index is wrong, or the GM `.aura` command was refused — it needs gmlevel >= 4 \
                 (VERIFIED vmangos `Chat/Chat.cpp:1229`: SEC_BASIC_ADMIN, which is 4 in \
                 `shared/Common.h:142`), and the slot-keyed probe accounts are gmlevel 6, so this \
                 probe cannot run as `probeN` without a temporary grant (method.md, decision 0450's \
                 precedent for --worldstate).",
            )?;
        let debuff = auras
            .iter()
            .find(|a| a.spell_id == AURA_DEBUFF_SPELL)
            .copied()
            .context("--aura: spell 589 never appeared in UNIT_FIELD_AURA")?;

        // The halves, the nibble, the level byte, the stack bias — each a distinct packing claim.
        ensure!(
            buff.is_helpful() && buff.slot < 32,
            "buff landed in the debuff half (slot {})",
            buff.slot
        );
        ensure!(
            !debuff.is_helpful() && debuff.slot >= 32,
            "debuff landed in the buff half (slot {})",
            debuff.slot
        );
        ensure!(
            buff.is_cancelable(),
            "AFLAG_CANCELABLE clear on a positive, cancelable buff (flags {:#x})",
            buff.flags
        );
        ensure!(
            !debuff.is_cancelable(),
            "AFLAG_CANCELABLE set on a debuff (flags {:#x})",
            debuff.flags
        );
        ensure!(
            buff.level == self_level && debuff.level == self_level,
            "AURALEVELS byte should be the caster's level {}: got buff {} / debuff {}",
            self_level,
            buff.level,
            debuff.level
        );
        ensure!(
            buff.stacks == 1 && debuff.stacks == 1,
            "stack bias wrong (the wire byte is count-1): got buff {} / debuff {}",
            buff.stacks,
            debuff.stacks
        );

        // Durations: keyed by slot, carrying what we asked for. `.aura` sets an exact duration, and
        // the packet is sent immediately, so allow only for a tick of decay.
        for (label, aura, asked) in [
            ("buff", buff, AURA_BUFF_SECONDS),
            ("debuff", debuff, AURA_DEBUFF_SECONDS),
        ] {
            let ms = durations
                .iter()
                .find(|(slot, _)| *slot == aura.slot)
                .map(|&(_, ms)| ms)
                .with_context(|| {
                    format!(
                        "--aura: no SMSG_UPDATE_AURA_DURATION for the {label}'s own slot {} \
                         (durations seen: {durations:?})",
                        aura.slot
                    )
                })?;
            let asked_ms = asked * 1000;
            ensure!(
                ms <= asked_ms && ms + 2000 >= asked_ms,
                "{label} duration {ms} ms is not the {asked_ms} ms we asked for"
            );
            println!("✅ {label}: slot {} ⇒ {ms} ms", aura.slot);
        }

        // The ordering the aura model rests on: the timer reaches us before the slot is named.
        let (d, v) = (
            first_duration_at.context("--aura: no duration packet at all")?,
            buff_field_at.context("--aura: the buff never appeared in a values delta")?,
        );
        ensure!(
            d < v,
            "SMSG_UPDATE_AURA_DURATION arrived AFTER the descriptor delta (seq {d} vs {v}) — \
             decision 0255's slot-keyed buffering is built on the opposite order"
        );
        println!("✅ duration packet precedes the descriptor delta (event {d} before {v})");

        // Durations are apply/refresh EDGES, not a stream — the client counts down locally from
        // here (the cast bar's model). An untouched slot must never have reported one.
        if let Some(&slot) = untouched
            .iter()
            .find(|s| durations.iter().any(|(d, _)| d == *s))
        {
            bail!(
                "--aura: slot {slot} reported a duration without being (re)applied — durations are \
                 not the apply/refresh edges decision 0255's client-side countdown assumes"
            );
        }
        println!(
            "✅ no duration for the {} untouched slot(s) {untouched:?} — permanent auras are \
             'until cancelled', and durations are apply/refresh edges, not a stream",
            untouched.len()
        );

        // Leave the character as found. The drain matters: `character_aura` persists across logout,
        // so exiting before the server processes these would save the probe's buffs onto the char.
        session.send_chat(&format!(".unaura {AURA_BUFF_SPELL}"))?;
        session.send_chat(&format!(".unaura {AURA_DEBUFF_SPELL}"))?;
        let settle = Instant::now() + Duration::from_secs(3);
        while Instant::now() < settle {
            let Ok(msg) = session.recv() else { continue };
            for ev in decode(msg) {
                if let SessionEvent::ObjectValues { guid, fields: d } = ev {
                    if guid == self_guid {
                        fields.merge(d);
                    }
                }
            }
        }
        dump("after cleanup", &fields);
        ensure!(
            !fields
                .unit_auras()
                .any(|a| a.spell_id == AURA_BUFF_SPELL || a.spell_id == AURA_DEBUFF_SPELL),
            "--aura: cleanup left a probe aura on the character"
        );

        println!("\n✅ --aura PASS: the aura block decodes and the duration wire is slot-keyed.");
        Ok(())
    }
}
