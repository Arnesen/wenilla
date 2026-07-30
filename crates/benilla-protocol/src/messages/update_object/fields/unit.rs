//! The unit/object/GameObject named accessors over [`ObjectFields`] — the UNIT_* family every
//! world entity carries (health/power/level/auras/combat stats), plus the object scale and the
//! GameObject block. Split from the one descriptor map (`mod.rs` holds the indices + codec).

use super::*;

impl ObjectFields {
    /// `OBJECT_FIELD_SCALE_X` — the per-object size multiplier.
    pub fn object_scale_x(&self) -> Option<f32> {
        self.get_f32(FIELD_OBJECT_SCALE_X)
    }
    /// `UNIT_FIELD_DISPLAYID` (units + players).
    pub fn unit_displayid(&self) -> Option<i32> {
        self.get_i32(FIELD_UNIT_DISPLAYID)
    }
    /// `UNIT_FIELD_MOUNTDISPLAYID` — the `CreatureDisplayInfo` id of the mount this unit rides;
    /// `0` (or absent) = not mounted. The one and only mounted signal on the wire (decision 0441).
    pub fn unit_mount_display_id(&self) -> u32 {
        self.get_u32(FIELD_UNIT_MOUNTDISPLAYID).unwrap_or(0)
    }
    /// `UNIT_FIELD_TARGET` — the guid this unit is attacking/interacting with (its "look at" target),
    /// or `None` when it has none (an absent field or an explicit `0`). Drives the idle face-target turn.
    pub fn unit_target(&self) -> Option<u64> {
        self.get_guid(FIELD_UNIT_TARGET).filter(|&g| g != 0)
    }
    /// `UNIT_FIELD_CHANNEL_OBJECT` — the object a channel is aimed at (may be the caster itself),
    /// or `None` when not channeling (an absent field or an explicit `0`).
    pub fn unit_channel_object(&self) -> Option<u64> {
        self.get_guid(FIELD_UNIT_CHANNEL_OBJECT).filter(|&g| g != 0)
    }
    /// `UNIT_CHANNEL_SPELL` — the spell id a unit is channeling; `0` when not channeling or absent.
    pub fn unit_channel_spell(&self) -> u32 {
        self.get_u32(FIELD_UNIT_CHANNEL_SPELL).unwrap_or(0)
    }
    /// `UNIT_FIELD_SUMMONEDBY` — the summoner's guid (pet/guardian/totem), or `None` (absent or
    /// explicit 0 — an unowned unit).
    pub fn unit_summoned_by(&self) -> Option<u64> {
        self.get_guid(FIELD_UNIT_SUMMONEDBY).filter(|&g| g != 0)
    }
    /// `UNIT_FIELD_CREATEDBY` — the creator's guid (totems and created objects), or `None`.
    pub fn unit_created_by(&self) -> Option<u64> {
        self.get_guid(FIELD_UNIT_CREATEDBY).filter(|&g| g != 0)
    }
    /// `UNIT_FIELD_PETNUMBER` — non-zero iff this unit is somebody's **pet or charm**.
    ///
    /// Not a count and not an index anyone displays: the client reads it as a pure boolean, as the
    /// **second gate of the rank getter** `0x605620` (decision 0782). A creature with a cached
    /// template but a non-zero pet number reports classification rank `0` — so an enslaved elite
    /// loses its dragon border, its ELITE tooltip word and (at rank 3) its world-boss skull, all
    /// three from this one read.
    ///
    /// What lands here is the server's call, and vmangos is narrower than the name suggests:
    /// `CharmInfo::SetPetNumber(n, statWindow)` writes `n` only when `statWindow`, else **0**. So a
    /// permanent pet (`Pet::IsPermanentPetFor`), a charm/mind-control apply and a player's summon
    /// carry a non-zero number, while **guardians and totems carry 0** and therefore keep whatever
    /// rank their template gives them.
    pub fn unit_is_pet_or_charm(&self) -> bool {
        self.get_u32(FIELD_UNIT_PETNUMBER).unwrap_or(0) != 0
    }
    /// `UNIT_FIELD_HEALTH` — current hit points (0 ⇒ dead).
    pub fn unit_health(&self) -> Option<u32> {
        self.get_u32(FIELD_UNIT_HEALTH)
    }
    /// `UNIT_FIELD_MAXHEALTH` — full hit points.
    pub fn unit_max_health(&self) -> Option<u32> {
        self.get_u32(FIELD_UNIT_MAXHEALTH)
    }
    /// Whether the unit is **dead** (health 0 on a real unit). Absent health reads as **zero** — a
    /// create block only carries *non-zero* fields (vmangos `_SetCreateBits`), so an already-dead
    /// corpse streams in with `MAXHEALTH` but **no** `HEALTH` at all; the real client's descriptor
    /// array is zero-initialized, so absent *is* 0 to it. The `MAXHEALTH > 0` guard keeps a unit
    /// whose snapshot hasn't arrived (empty store) from reading as dead.
    pub fn unit_is_dead(&self) -> bool {
        self.unit_max_health().unwrap_or(0) > 0 && self.unit_health().unwrap_or(0) == 0
    }
    /// `UNIT_FIELD_LEVEL` — the unit's level.
    pub fn unit_level(&self) -> Option<u32> {
        self.get_u32(FIELD_UNIT_LEVEL)
    }
    /// One aura slot (0-based), or `None` when the slot is empty or `slot >= UNIT_AURA_SLOTS`.
    ///
    /// Occupancy is the **client's own liveness test**: at least one effect-index bit set in the
    /// `AURAFLAGS` nibble (`flags & 0x0E`, decision 0257) — *not* "has a spell id". A slot the server
    /// clears can keep a stale spell id after its flags nibble goes to zero; gating on the id alone
    /// would surface that husk as a phantom aura the real client hides (`flags & 0x0E` is precisely
    /// why it doesn't). The `spell_id != 0` guard stays as a belt-and-braces check — for a live aura
    /// (an applied effect implies a spell) the two always coincide.
    pub fn unit_aura(&self, slot: u8) -> Option<UnitAuraSlot> {
        if slot >= UNIT_AURA_SLOTS {
            return None;
        }
        let flags = self.get_aura_nibble(slot);
        if flags & AURA_FLAG_EFF_INDEX_MASK == 0 {
            return None;
        }
        let spell_id = self
            .get_u32(FIELD_UNIT_AURA + u16::from(slot))
            .filter(|&id| id != 0)?;
        Some(UnitAuraSlot {
            slot,
            spell_id,
            flags,
            level: self.get_aura_byte(FIELD_UNIT_AURALEVELS, slot),
            // The wire byte is `stack - 1` and saturates at 254, so an occupied slot is always ≥ 1.
            stacks: self
                .get_aura_byte(FIELD_UNIT_AURAAPPLICATIONS, slot)
                .saturating_add(1),
        })
    }
    /// Every occupied aura slot, **ascending by slot index** — buffs (0–31) before debuffs (32–47).
    ///
    /// This is the reference client's ordering law for `UnitBuff`/`UnitDebuff` on **another unit**:
    /// they read that unit's `UNIT_FIELD_AURA` straight, ascending raw slot within the half (wow-re
    /// `0x519500`/`0x5198f0`, VERIFIED — decision 0257).
    ///
    /// It is **not** the order the player's own buff bar draws. For the local player the client
    /// keeps a separate, densely packed cache (`0xbc6040`) in **insertion order**, which repacks on
    /// removal — so a dropped buff slides the later ones along, and a new buff always lands at the
    /// end rather than in whatever slot the server recycled. That order cannot be recovered from a
    /// descriptor snapshot; it has to be *maintained* across updates (decision 0257).
    pub fn unit_auras(&self) -> impl Iterator<Item = UnitAuraSlot> + '_ {
        (0..UNIT_AURA_SLOTS).filter_map(|slot| self.unit_aura(slot))
    }
    /// The unit's active power type (`UNIT_FIELD_BYTES_0` byte 3): `0` mana, `1` rage, `2` focus,
    /// `3` energy, `4` happiness. Absent (no bytes_0 on the wire yet) reads as mana, the descriptor's
    /// zero-initialized default.
    pub fn unit_power_type(&self) -> u8 {
        (self.unit_bytes_0().unwrap_or(0) >> 24) as u8
    }
    /// `UNIT_FIELD_BYTES_1` byte 0 — the unit's stand state (VERIFIED vmangos
    /// `UNIT_BYTES_1_OFFSET_STAND_STATE = 0`, written by `Unit::SetStandState`): 0 stand · 1 sit
    /// (ground) · 2 sit-chair · 3 sleep · 4/5/6 chair low/med/high · 8 kneel. For a player it's
    /// the echo of their own `CMSG_STANDSTATECHANGE`. `0` (standing) when the field is absent —
    /// the descriptor's zero-initialized default.
    pub fn unit_stand_state(&self) -> u8 {
        (self.get_u32(FIELD_UNIT_BYTES_1).unwrap_or(0) & 0xff) as u8
    }
    /// `UNIT_FIELD_BYTES_1` byte 3's `UNIT_VIS_FLAGS_CREEP (0x2)` — the sneaking visual, set by
    /// `SPELL_AURA_MOD_STEALTH` (vmangos `SpellAuras.cpp:3610`,
    /// `UNIT_BYTES_1_OFFSET_VIS_FLAG = 3`). The usable walk's only-stealthed leg tests exactly
    /// this bit (wow-re §2a leg 7: `[+0x110]+0x213 & 2`); the tracking classifier's
    /// stealth always-show clause tests it against our own track-stealthed bit
    /// (`0x5ed210`, wow-re `track-predicates.md`).
    pub fn unit_is_stealthed(&self) -> bool {
        (self.get_u32(FIELD_UNIT_BYTES_1).unwrap_or(0) >> 24) & 0x2 != 0
    }
    /// `UNIT_FIELD_BYTES_1` byte 3's `UNIT_VIS_FLAGS_GHOST (0x1)` — set on ANY unit by the ghost
    /// aura (vmangos `HandleAuraGhost`). Byte-VERIFIED (wow-re ghost-death-visuals.md): this flag
    /// drives NO body render — its complete client effect is nameplate/marker suppression
    /// (tested as `byte3 & 0x03`, ghost|creep, at `0x607101`/`0x60f62e`). The translucent
    /// whitened BODY is the ghost aura's own state visual kit (spell 8326 → SpellVisual 886 →
    /// kit 989: charproc-14 alpha 0.5 + charproc-1 tint 0xFF8CB9FD + `Spells\Ghost_state.mdx`) —
    /// aura-driven, exactly like Stealth's 0.3 translucency. Consumers: the overhead-name and
    /// V-plate gates.
    pub fn unit_is_ghost_visual(&self) -> bool {
        (self.get_u32(FIELD_UNIT_BYTES_1).unwrap_or(0) >> 24) & 0x1 != 0
    }

    /// `UNIT_FIELD_AURASTATE` ([`FIELD_UNIT_AURASTATE`]) — the aura-state bit set (defense 1,
    /// healthless-20% 2, …); the usable walk tests `1 << (state-1)` against it.
    pub fn unit_aura_state(&self) -> u32 {
        self.get_u32(FIELD_UNIT_AURASTATE).unwrap_or(0)
    }

    /// `UNIT_FIELD_BYTES_1` byte 2 — the shapeshift form (VERIFIED vmangos `UpdateFields_1_12_1.h`
    /// 0x8A + `UNIT_BYTES_1_OFFSET_SHAPESHIFT_FORM = 2`): 0 none, 17/18/19 = warrior
    /// battle/defensive/berserker stance, druid forms in the low ids. Drives the action bar's
    /// bonus-page pick, and the tracking dots' creature-type override (the `0x605570`
    /// resolver's first branch — decision 0564).
    pub fn unit_shapeshift_form(&self) -> u8 {
        (self.get_u32(FIELD_UNIT_BYTES_1).unwrap_or(0) >> 16) as u8
    }
    /// `UNIT_FIELD_POWER1..5` — current power for slot `ty` (the [`Self::unit_power_type`] index).
    /// Out-of-range slots and absent fields read `None`.
    pub fn unit_power(&self, ty: u8) -> Option<u32> {
        (ty < 5).then(|| self.get_u32(FIELD_UNIT_POWER1 + u16::from(ty)))?
    }
    /// `UNIT_FIELD_MAXPOWER1..5` — maximum power for slot `ty`.
    pub fn unit_max_power(&self, ty: u8) -> Option<u32> {
        (ty < 5).then(|| self.get_u32(FIELD_UNIT_MAXPOWER1 + u16::from(ty)))?
    }
    /// `UNIT_FIELD_BASE_MANA` (VERIFIED vmangos `UpdateFields_1_12_1.h` `OBJECT_END + 0x9C` = 162;
    /// PRIVATE + OWNER_ONLY, so it rides only our own descriptor) — the base the percent-cost
    /// spells (`ManaCostPercentage`) scale from. `None` when absent (a non-self unit).
    pub fn unit_base_mana(&self) -> Option<u32> {
        self.get_u32(FIELD_UNIT_BASE_MANA)
    }
    /// `UNIT_FIELD_FACTIONTEMPLATE` — the unit's `FactionTemplate.dbc` row id, the key the client's
    /// whole reaction/hostility decode starts from (wow-re: descriptor `+0x74`, read by every
    /// `UnitReaction`/`Can*` predicate). Flagged PUBLIC server-side, so it's always on the wire.
    pub fn unit_faction_template(&self) -> Option<u32> {
        self.get_u32(FIELD_UNIT_FACTIONTEMPLATE)
    }
    /// `UNIT_FIELD_FLAGS` — unit state flags; bit `0x0400_0000` = **skinnable** (the cursor's Skin
    /// branch reads it — wow-re cursor RE, client `+0xa0`). Absent counts as 0 (create omits zeros).
    pub fn unit_flags(&self) -> u32 {
        self.get_u32(FIELD_UNIT_FLAGS).unwrap_or(0)
    }
    /// `UNIT_FIELD_COMBATREACH` — the melee-reach float the client's edge-to-edge attack range gate
    /// reads (`rA + rB + 1.333`, cap 5.0 — wow-re cursor RE `0x6e3480`, client `+0x1f0`). Vanilla
    /// default 1.5 when absent.
    pub fn unit_combat_reach(&self) -> f32 {
        self.get_f32(FIELD_UNIT_COMBATREACH).unwrap_or(1.5)
    }
    /// `UNIT_FIELD_BOUNDINGRADIUS` — the unit's horizontal bounding radius (yd). The water-foam
    /// depth gate reads it (`max(2 × radius, 1.0)` — wow-re CWater0Ripple driver `0x5fa760`).
    /// Absent counts as 0 — the gate's own 1.0-yd floor covers it, like the reference.
    pub fn unit_bounding_radius(&self) -> f32 {
        self.get_f32(FIELD_UNIT_BOUNDINGRADIUS).unwrap_or(0.0)
    }
    /// `UNIT_DYNAMIC_FLAGS` — per-viewer dynamic state; bit `0x1` = **lootable** (the cursor's Loot
    /// branch — wow-re cursor RE, client `+0x224`). Absent counts as 0.
    pub fn unit_dynamic_flags(&self) -> u32 {
        self.get_u32(FIELD_UNIT_DYNAMIC_FLAGS).unwrap_or(0)
    }
    /// `UNIT_DYNAMIC_FLAGS` bit `0x1` — **lootable by me** (the server strips the bit per viewer
    /// before it ships — vmangos's "hide lootable animation for unallowed players"). Gates the
    /// loot cursor, the right-click loot route, and the corpse sparkle (wow-re
    /// `loot-corpse-effect.md`).
    pub fn unit_lootable(&self) -> bool {
        self.unit_dynamic_flags() & 0x1 != 0
    }
    /// `UNIT_NPC_FLAGS` — the NPC service bits (vanilla values: gossip `0x1`, questgiver `0x2`,
    /// vendor `0x4`, flightmaster `0x8`, trainer `0x10`, … repair `0x4000` — vmangos
    /// `UnitDefines.h`). The cursor's service ladder switches on these, first bit wins (wow-re
    /// cursor RE, client `+0x234`). Absent counts as 0.
    pub fn unit_npc_flags(&self) -> u32 {
        self.get_u32(FIELD_UNIT_NPC_FLAGS).unwrap_or(0)
    }
    /// `UNIT_NPC_EMOTESTATE` — the unit's looping state emote (`Emotes.dbc` id: dance, NPC
    /// cooking/working flavor; `0`/absent = none). The anim layer loops its `AnimID` while the
    /// unit stands; movement suppresses it (the field stays set server-side until cleared).
    pub fn unit_emote_state(&self) -> u32 {
        self.get_u32(FIELD_UNIT_NPC_EMOTESTATE).unwrap_or(0)
    }
    /// `UNIT_FIELD_BYTES_0` — packed race (byte 0) / class (byte 1) / gender (byte 2) / power type (3).
    pub fn unit_bytes_0(&self) -> Option<u32> {
        self.get_u32(FIELD_UNIT_BYTES_0)
    }
    /// `UNIT_VIRTUAL_ITEM_SLOT_DISPLAY + slot` — a creature's virtual-weapon **display id** for
    /// `slot` (0 mainhand / 1 offhand / 2 ranged): the item's `DisplayInfoID` directly, no item guid
    /// behind it (see the field's doc comment). `slot >= 3` reads `None`.
    pub fn unit_virtual_item_display(&self, slot: u8) -> Option<u32> {
        (slot < 3).then(|| self.get_u32(FIELD_UNIT_VIRTUAL_ITEM_SLOT_DISPLAY + u16::from(slot)))?
    }
    /// `UNIT_VIRTUAL_ITEM_INFO + 2*slot` — `slot`'s flavor bytes: `(class, subclass, material,
    /// inventory_type)`. `slot >= 3` reads `None`.
    pub fn unit_virtual_item_info(&self, slot: u8) -> Option<(u8, u8, u8, u8)> {
        let v =
            (slot < 3).then(|| self.get_u32(FIELD_UNIT_VIRTUAL_ITEM_INFO + 2 * u16::from(slot)))?;
        v.map(|v| {
            (
                (v & 0xff) as u8,
                ((v >> 8) & 0xff) as u8,
                ((v >> 16) & 0xff) as u8,
                ((v >> 24) & 0xff) as u8,
            )
        })
    }
    /// `UNIT_VIRTUAL_ITEM_INFO + 2*slot + 1` byte 0 — `slot`'s virtual item Sheath. `slot >= 3`
    /// reads `None`.
    pub fn unit_virtual_item_sheath(&self, slot: u8) -> Option<u8> {
        let v = (slot < 3)
            .then(|| self.get_u32(FIELD_UNIT_VIRTUAL_ITEM_INFO + 2 * u16::from(slot) + 1))?;
        v.map(|v| (v & 0xff) as u8)
    }
    /// `UNIT_FIELD_BYTES_2` byte 0 — the unit's sheath state: `0` unarmed/stowed, `1` melee drawn,
    /// `2` ranged drawn. For a player this is purely what they last told the server via
    /// `CMSG_SETSHEATHED`, not a server-computed fact.
    pub fn unit_sheath_state(&self) -> Option<u8> {
        self.get_u32(FIELD_UNIT_BYTES_2).map(|v| (v & 0xff) as u8)
    }
    /// `UNIT_FIELD_STAT0..4 + i` — the unit's raw (post-buff) primary stat `i` (0 Str, 1 Agi,
    /// 2 Stam, 3 Int, 4 Spirit — the `Stats` enum order). `i >= 5` reads `None`.
    pub fn unit_stat(&self, i: u8) -> Option<u32> {
        (i < 5).then(|| self.get_u32(FIELD_UNIT_STAT0 + u16::from(i)))?
    }
    /// `UNIT_FIELD_RESISTANCES + i` — resistance `i` (0 = armor, 1..6 the magic schools). `i >= 7`
    /// reads `None`.
    pub fn unit_resistance(&self, i: u8) -> Option<i32> {
        (i < 7).then(|| self.get_i32(FIELD_UNIT_RESISTANCES + u16::from(i)))?
    }
    /// `UNIT_FIELD_ATTACK_POWER` — melee attack power before the split pos/neg mods.
    pub fn unit_attack_power(&self) -> Option<i32> {
        self.get_i32(FIELD_UNIT_ATTACK_POWER)
    }
    /// `UNIT_FIELD_ATTACK_POWER_MODS` — `(positive, negative)` flat AP mods (the negative half
    /// already negative-or-zero; see the field's doc comment for the verified packing/sign).
    /// Absent reads `(0, 0)` — no mods applied yet.
    pub fn unit_attack_power_mods(&self) -> (i16, i16) {
        let (lo, hi) = self
            .get_u16_pair(FIELD_UNIT_ATTACK_POWER_MODS)
            .unwrap_or((0, 0));
        (lo as i16, hi as i16)
    }
    /// `UNIT_FIELD_ATTACK_POWER_MULTIPLIER` — the percent AP bonus **minus 1.0** (`0.0` = no
    /// bonus; vmangos writes `multiplier − 1.0`, see the field's doc comment).
    pub fn unit_attack_power_multiplier(&self) -> Option<f32> {
        self.get_f32(FIELD_UNIT_ATTACK_POWER_MULTIPLIER)
    }
    /// `UNIT_FIELD_RANGED_ATTACK_POWER` — ranged attack power before the split pos/neg mods.
    pub fn unit_ranged_attack_power(&self) -> Option<i32> {
        self.get_i32(FIELD_UNIT_RANGED_ATTACK_POWER)
    }
    /// `UNIT_FIELD_RANGED_ATTACK_POWER_MODS` — the ranged twin of
    /// [`Self::unit_attack_power_mods`].
    pub fn unit_ranged_attack_power_mods(&self) -> (i16, i16) {
        let (lo, hi) = self
            .get_u16_pair(FIELD_UNIT_RANGED_ATTACK_POWER_MODS)
            .unwrap_or((0, 0));
        (lo as i16, hi as i16)
    }
    /// `UNIT_FIELD_RANGED_ATTACK_POWER_MULTIPLIER` — the ranged twin of
    /// [`Self::unit_attack_power_multiplier`].
    pub fn unit_ranged_attack_power_multiplier(&self) -> Option<f32> {
        self.get_f32(FIELD_UNIT_RANGED_ATTACK_POWER_MULTIPLIER)
    }
    /// `UNIT_FIELD_MINDAMAGE` — the mainhand weapon's minimum damage.
    pub fn unit_min_damage(&self) -> Option<f32> {
        self.get_f32(FIELD_UNIT_MINDAMAGE)
    }
    /// `UNIT_FIELD_MAXDAMAGE` — the mainhand weapon's maximum damage.
    pub fn unit_max_damage(&self) -> Option<f32> {
        self.get_f32(FIELD_UNIT_MAXDAMAGE)
    }
    /// `UNIT_FIELD_MINOFFHANDDAMAGE` — the offhand weapon's minimum damage (present whether or not
    /// one's equipped; the paper-doll only shows it when an offhand item is worn).
    pub fn unit_min_offhand_damage(&self) -> Option<f32> {
        self.get_f32(FIELD_UNIT_MINOFFHANDDAMAGE)
    }
    /// `UNIT_FIELD_MAXOFFHANDDAMAGE` — the offhand weapon's maximum damage.
    pub fn unit_max_offhand_damage(&self) -> Option<f32> {
        self.get_f32(FIELD_UNIT_MAXOFFHANDDAMAGE)
    }
    /// `UNIT_FIELD_MINRANGEDDAMAGE` — the ranged weapon's minimum damage.
    pub fn unit_min_ranged_damage(&self) -> Option<f32> {
        self.get_f32(FIELD_UNIT_MINRANGEDDAMAGE)
    }
    /// `UNIT_FIELD_MAXRANGEDDAMAGE` — the ranged weapon's maximum damage.
    pub fn unit_max_ranged_damage(&self) -> Option<f32> {
        self.get_f32(FIELD_UNIT_MAXRANGEDDAMAGE)
    }
    /// `UNIT_FIELD_BASEATTACKTIME + i` — attack speed in ms (`i` 0 mainhand, 1 offhand). `i >= 2`
    /// reads `None`.
    pub fn unit_base_attack_time(&self, i: u8) -> Option<u32> {
        (i < 2).then(|| self.get_u32(FIELD_UNIT_BASEATTACKTIME + u16::from(i)))?
    }
    /// `UNIT_FIELD_RANGEDATTACKTIME` — ranged weapon attack speed in ms.
    pub fn unit_ranged_attack_time(&self) -> Option<u32> {
        self.get_u32(FIELD_UNIT_RANGEDATTACKTIME)
    }
}
