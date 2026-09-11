//! Melee auto-attack messages — the attack start/stop edges, the per-swing damage report, and the
//! creature-aggro notice. Split out of `messages/spells.rs` (decision 0640); mirrored by
//! `world::writer::attack`.
//!
//! `SMSG_ATTACKERSTATEUPDATE` is the melee twin of [`super::combat_log`]'s spell reports: a *swing*
//! reports itself here (fired exactly once per weapon-timer cycle, independently per hand — which is
//! what makes it the animation driver, decision 0073), a *spell* reports itself there.
//! `SMSG_AI_REACTION` rides along because it is the same moment from the creature's side: vmangos
//! broadcasts it from `Unit::Attack`, i.e. the instant a creature decides to swing (decision 0277).

use std::io::{self, Read};

use crate::wire::{read_f32_le, read_packed_guid, read_u32_le, read_u64_le, read_u8};

/// Read `SMSG_ATTACKSTART` (vmangos `AttackStart::AppendBodyTo`): two full `u64` guids.
pub(super) fn read_attack_start(r: &mut impl Read) -> io::Result<(u64, u64)> {
    Ok((read_u64_le(r)?, read_u64_le(r)?))
}

/// One decoded `SMSG_ATTACKERSTATEUPDATE` — a completed melee swing (vmangos
/// `Unit::SendAttackStateUpdate`, `Unit.cpp:4572-4605`; fired **exactly once per weapon-timer
/// cycle**, independently per hand — the real client plays one attacker swing animation per packet,
/// wow-re `combat-swing-anim.md`, decision 0073). The per-school sub-damage split collapses to the
/// packet's own `TotalDamage` plus the summed `absorb` (decision 0137 phase 2's floating combat
/// text feed); `hit_info` bit `0x4` marks an **offhand** swing (the anim selector keys on it), bit
/// `0x10000` suppresses the swing anim.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AttackerState {
    pub attacker: u64,
    pub victim: u64,
    pub hit_info: u32,
    /// `TotalDamage` — the swing's damage before the sub-damage split.
    pub damage: u32,
    /// `TargetState` (vmangos `VictimState`): 1 hit · 2 dodge · 3 parry · 4 interrupt · 5 blocks ….
    /// A defended outcome (dodge/parry/block/deflect) plays a dedicated victim defense clip at
    /// the swing's `$CPP` keyframe (decision 0279, correcting 0073's "never a body animation");
    /// landed hits flinch/bleed; the rest is sound + floating text.
    pub victim_state: u32,
    /// Sum of the per-sub-damage `absorb` fields (vmangos writes exactly one sub-damage block in
    /// practice; summed faithfully in case more than one ever rides the wire).
    pub absorb: u32,
    /// Sum of the per-sub-damage `resist` fields — the partial-resist trailer's amount
    /// (decision 0580's center-text fold-back).
    pub resist: i32,
    /// `BlockedAmount` — the trailing blocked-damage word.
    pub blocked: u32,
    /// `meleeSpellId` (`rec+0xac`) — **a boolean in practice, not an id.** Both of the client's
    /// readers only compare it to zero (`0x625e40`, and `0x6246ca`'s `xor/cmp/sbb/neg` feeding
    /// the swing-animation selector), so in this packet it means "this swing came from a spell".
    /// Nonzero for an on-next-swing ability — Heroic Strike is the one the field was first
    /// noticed on.
    pub melee_spell_id: u32,
    /// The **first** sub-damage block's school (`SpellSchools`: 0 physical … 6 arcane; vmangos
    /// writes `GetFirstSchoolInMask(subDamage->damageSchoolMask)`), `0` when the swing carried no
    /// sub-damage at all.
    ///
    /// It exists for the combat log's wording: a non-physical swing takes the `…SCHOOL` template
    /// ("You hit X for 5 fire damage.") and physical takes the plain one. The floating text has
    /// never needed it, which is why the field was read and dropped until B297.
    pub school: u8,
}

impl AttackerState {
    /// `0x625e40` — **whether this swing is displayed at all**, in either of the two places that
    /// display one. Twenty-six bytes:
    ///
    /// ```text
    /// 625e40 mov  edx, [ecx+0xac]     ; meleeSpellId
    /// 625e46 test edx, edx
    /// 625e48 mov  eax, 1
    /// 625e4d je   0x625e59            ; id == 0        -> TRUE
    /// 625e4f cmp  [ecx+0xa4], eax     ; victimState vs 1
    /// 625e55 je   0x625e59            ; state == 1     -> TRUE
    /// 625e57 xor  eax, eax            ; otherwise      -> FALSE
    /// ```
    ///
    /// **Two callers, and they are the two display paths**: `0x629b3e` in `0x629b10`, which skips
    /// the whole chat dispatcher `0x629b60` — no line, no trailer, no `COMBAT_TEXT_UPDATE` — and
    /// `0x62440d`, the first thing the **floating-combat-text builder** `0x6243e0` does, jumping
    /// straight to its own epilogue. So a Heroic Strike that is dodged or parried produces neither
    /// a combat-log line nor a floating number.
    ///
    /// **Untouched by it:** the swing animation (`0x6246a0`), the swing timers and autorepeat, the
    /// auto-target arm, the tutorial trigger, and everything `0x624530` does after its call to
    /// `0x6243e0` — the wound flinch, the blood spurt and the whole victimState-keyed sound block.
    /// A suppressed swing still swings, still connects, still sounds.
    ///
    /// A landing ability (`victimState == 1`) is unaffected whatever its spell id, which is why
    /// ordinary Heroic Strike damage reads normally.
    #[must_use]
    pub fn displayed(&self) -> bool {
        self.melee_spell_id == 0 || self.victim_state == 1
    }
}

/// Read `SMSG_ATTACKERSTATEUPDATE` (byte-verified order, attacker **PackGUID first** — settled by
/// the wow-re §5 against the handler's downstream use, decision 0073): HitInfo · attacker PackGUID ·
/// victim PackGUID · TotalDamage · SubDamageCount + per-sub `{school u32, damage f32, damage u32,
/// absorb u32, resist i32}` · TargetState · two u32s (zero + "spell id, seen with heroic strike") ·
/// BlockedAmount. TargetState is then normalized the way the handler normalizes it (`0x625e35`).
pub(super) fn read_attacker_state(r: &mut impl Read) -> io::Result<AttackerState> {
    let hit_info = read_u32_le(r)?;
    let attacker = read_packed_guid(r)?;
    let victim = read_packed_guid(r)?;
    let damage = read_u32_le(r)?;
    let subs = read_u8(r)?;
    let mut absorb = 0u32;
    let mut resist = 0i32;
    let mut school = 0u8;
    for i in 0..subs {
        // damage f32 + damage u32 are folded into TotalDamage above; absorb/resist are summed. The
        // school is kept from the FIRST block only — the wording takes one word, and vmangos's
        // extra blocks are the off-school splits of the same swing.
        let block_school = read_u32_le(r)?;
        if i == 0 {
            school = u8::try_from(block_school).unwrap_or(0);
        }
        let _damage_f = read_f32_le(r)?;
        let _damage = read_u32_le(r)?;
        absorb += read_u32_le(r)?;
        resist += read_u32_le(r)? as i32;
    }
    let victim_state = read_u32_le(r)?;
    // vmangos names the tail `victimState · attackerState · meleeSpellId · blockedAmount`
    // (`Server/Packets/Combat.cpp:81`), and the client's parse `0x625c60` stores them at
    // `rec+0xa4`/`+0xa8`/`+0xac`/`+0xb0` in that order. `attackerState` is read by **nothing** in
    // the whole combat TU; `meleeSpellId` is read twice and only ever compared to zero.
    let _attacker_state = read_u32_le(r)?;
    let melee_spell_id = read_u32_le(r)?;
    let blocked = read_u32_le(r)?;
    // **The handler does not trust the wire's TargetState for a full block** (`0x625e35`): a swing
    // that got no damage through while blocking something is forced to `VICTIMSTATE_BLOCKS`,
    // upstream of every consumer, which is why the display dispatcher's flag table has a `1` at an
    // index its own earlier arm makes unreachable. Normalized here, in the same position — before
    // the chat line, the floating word and the sound each read it.
    //
    // Against vmangos it is belt-and-braces: `Unit.cpp`'s `MELEE_HIT_BLOCK` arm already sets
    // `VICTIMSTATE_BLOCKS` when `blocked_amount >= subDamage[0].damage`. It costs three lines and
    // removes the client's dependence on a server doing so.
    let victim_state = if damage == 0 && blocked != 0 {
        5
    } else {
        victim_state
    };
    Ok(AttackerState {
        melee_spell_id,
        attacker,
        victim,
        hit_info,
        damage,
        victim_state,
        absorb,
        resist,
        blocked,
        school,
    })
}

/// The server's refusal of a `CMSG_ATTACKSWING` — the melee auto-attack answer that is not
/// `SMSG_ATTACKSTART`. **Three variants for four opcodes, because that is what the client can
/// distinguish**: the reference dispatches `0x145`/`0x146`/`0x148`/`0x149` through `0x6255b0`'s
/// jump table `0x625aec`, and `0x148` DEADTARGET and `0x149` CANT_ATTACK land on **arm 4
/// (`0x625ab8`) verbatim** — one shared body, no reason byte, nothing to tell them apart with.
/// Collapsing them here is the fidelity fact in the type: a consumer cannot key on a difference
/// the real client never had.
///
/// Every body is empty; the arms read nothing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AttackSwingError {
    /// `SMSG_ATTACKSWING_NOTINRANGE` (`0x145`) — arm 2 `0x625a8a`: latch code **1**.
    NotInRange,
    /// `SMSG_ATTACKSWING_BADFACING` (`0x146`) — arm 3 `0x625aa1`: latch code **2**.
    BadFacing,
    /// `SMSG_ATTACKSWING_DEADTARGET` (`0x148`) **or** `SMSG_ATTACKSWING_CANT_ATTACK` (`0x149`) —
    /// both arm 4 `0x625ab8`, which raises **no message at all**: it resolves the active player
    /// and calls StopAttack (`0x5ecac0`) on it, full stop. The name is deliberately the union of
    /// the two: naming one of them would claim a distinction the client does not have.
    DeadOrUnattackable,
}

/// Read `SMSG_ATTACKSTOP` (vmangos `AttackStop::AppendBodyTo`): two **packed** guids + a `u32`
/// "victim is dead" word (dropped — death arrives through the descriptor seam).
pub(super) fn read_attack_stop(r: &mut impl Read) -> io::Result<(u64, u64)> {
    let attacker = read_packed_guid(r)?;
    let victim = read_packed_guid(r)?;
    let _is_dead = read_u32_le(r)?;
    Ok((attacker, victim))
}

/// Read `SMSG_AI_REACTION` → `(unit, reaction)` (vmangos `Creature::SendAIReaction`,
/// `Objects/Creature.cpp:2490-2498` → `WorldPackets::Misc::AiReaction::AppendBodyTo`,
/// `Server/Packets/Misc.cpp:445-449`): a raw (unpacked) `u64` guid + a `u32` reaction. Broadcast
/// with reaction 2 (HOSTILE) on every creature melee-attack start (`Unit::Attack`) and 0 (ALERT)
/// on stealth pre-aggro detection (`CreatureAI::TriggerAlertDirect`); 1/4 exist server-side but
/// are never sent (decision 0277).
pub(super) fn read_ai_reaction(r: &mut impl Read) -> io::Result<(u64, u32)> {
    let unit = read_u64_le(r)?;
    let reaction = read_u32_le(r)?;
    Ok((unit, reaction))
}

/// Body of `CMSG_ATTACKSWING` (vmangos `AttackSwing::ReadFromWorldPacket`): one full `u64` victim
/// guid. Starts melee auto-attack; the server answers `SMSG_ATTACKSTART` (or an attack-swing error
/// packet). `CMSG_ATTACKSTOP`'s body is empty — no builder needed.
pub fn attack_swing(guid: u64) -> Vec<u8> {
    guid.to_le_bytes().to_vec()
}

#[cfg(test)]
mod tests {
    use super::read_attacker_state;

    /// One swing on the wire, as `SMSG_ATTACKERSTATEUPDATE` lays it out.
    fn packet(damage: u32, victim_state: u32, blocked: u32) -> Vec<u8> {
        let mut b = Vec::new();
        b.extend_from_slice(&0u32.to_le_bytes()); // HitInfo
        b.extend_from_slice(&[0x01, 0x11]); // attacker PackGUID
        b.extend_from_slice(&[0x01, 0x22]); // victim PackGUID
        b.extend_from_slice(&damage.to_le_bytes()); // TotalDamage
        b.push(1); // one sub-damage block
        b.extend_from_slice(&0u32.to_le_bytes()); // school
        b.extend_from_slice(&(damage as f32).to_le_bytes());
        b.extend_from_slice(&damage.to_le_bytes());
        b.extend_from_slice(&0u32.to_le_bytes()); // absorb
        b.extend_from_slice(&0u32.to_le_bytes()); // resist
        b.extend_from_slice(&victim_state.to_le_bytes()); // TargetState
        b.extend_from_slice(&0u32.to_le_bytes()); // zero
        b.extend_from_slice(&0u32.to_le_bytes()); // spell id
        b.extend_from_slice(&blocked.to_le_bytes()); // BlockedAmount
        b
    }

    /// `0x625e40` — a swing that came from a spell and did not land plainly is not displayed at
    /// all, in either of the two places a swing is displayed.
    ///
    /// Heroic Strike is the case: it rides `SMSG_ATTACKERSTATEUPDATE` with a nonzero
    /// `meleeSpellId`, so when it lands (`victimState == 1`) it reads exactly like a white hit,
    /// and when it is dodged or parried the reference prints **no combat-log line and floats no
    /// word**. An ordinary swing carries id 0 and is never touched by this.
    #[test]
    fn a_swing_from_a_spell_that_did_not_land_is_not_displayed() {
        let with_id = |state: u32| {
            let mut b = packet(0, state, 0);
            let at = b.len() - 8; // the meleeSpellId slot, before BlockedAmount
            b[at..at + 4].copy_from_slice(&78u32.to_le_bytes()); // Heroic Strike
            read_attacker_state(&mut b.as_slice()).expect("parse")
        };
        // Dodged, parried, missed outright: suppressed.
        for state in [0, 2, 3, 4] {
            assert!(
                !with_id(state).displayed(),
                "spell swing, victimState {state}"
            );
        }
        // Landed: displayed like any other hit.
        assert!(
            with_id(1).displayed(),
            "a landing Heroic Strike still shows"
        );
        // A plain swing carries id 0 and is displayed whatever the victim did.
        for state in [0, 1, 2, 3, 4] {
            let s = read_attacker_state(&mut packet(0, state, 0).as_slice()).expect("parse");
            assert!(s.displayed(), "plain swing, victimState {state}");
        }
    }

    /// `0x625e35` — a swing that got nothing through while blocking something **is** a block, and
    /// the handler says so before any consumer reads the field.
    ///
    /// It matters because the display dispatcher's `victimState == 5` arm is what words `VSBLOCK`,
    /// and its else-arm emits no line at all for the `1` the wire might otherwise carry — so
    /// without this normalization a fully-blocked swing could go silent instead of reading
    /// "X blocks your attack."
    #[test]
    fn a_fully_blocked_swing_is_normalized_to_victimstate_blocks() {
        let s = read_attacker_state(&mut packet(0, 1, 90).as_slice()).expect("parse");
        assert_eq!(s.victim_state, 5, "no damage through, something blocked");

        // A partial block lands damage, so the wire's own state stands.
        let s = read_attacker_state(&mut packet(50, 1, 40).as_slice()).expect("parse");
        assert_eq!(s.victim_state, 1);

        // A dodge blocks nothing and keeps its state, damage or not.
        let s = read_attacker_state(&mut packet(0, 2, 0).as_slice()).expect("parse");
        assert_eq!(s.victim_state, 2);
    }
}
