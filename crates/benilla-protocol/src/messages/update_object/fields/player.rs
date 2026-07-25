//! The item/container/PLAYER named accessors over [`ObjectFields`] — inventory and equipment
//! slots, purse/XP, the quest log, skills, buyback, reputation-adjacent player bytes. Split from
//! the one descriptor map (`mod.rs` holds the indices + codec).

use super::*;

impl ObjectFields {
    /// `OBJECT_FIELD_ENTRY` (3) — the object's template entry (an item object's item id, a
    /// GameObject's template). The one field that names WHAT a streamed object is.
    pub fn object_entry(&self) -> Option<u32> {
        self.get_u32(3)
    }
    /// `ITEM_FIELD_STACK_COUNT` — an item object's stack size.
    pub fn item_stack_count(&self) -> Option<u32> {
        self.get_u32(FIELD_ITEM_STACK_COUNT)
    }
    /// `ITEM_FIELD_FLAGS` (field 21 — wire index; the client's item-relative index 15 + the 6
    /// object fields, VERIFIED wow-re inventory-alert-law): bit `0x08` = wrapped (a gift — never
    /// alerts, never reads broken), bit `0x10` = force-red (status 4 regardless of durability;
    /// the bit's game meaning is unnamed in the binary, the behavior byte-verified).
    pub fn item_flags(&self) -> Option<u32> {
        self.get_u32(21)
    }
    /// `ITEM_FIELD_CREATOR` (field 10, guid — vmangos `UpdateFields_1_12_1.h` OBJECT_END+0x4) —
    /// the item's author/crafter. On a mail-made permanent letter this is the letter's SENDER
    /// (vmangos `HandleMailCreateTextItem` stamps it) — the reader window's "From," line.
    pub fn item_creator(&self) -> Option<u64> {
        self.get_guid(10).filter(|&g| g != 0)
    }
    /// `ITEM_FIELD_ITEM_TEXT_ID` (field 45, OWNER_ONLY — vmangos `UpdateFields_1_12_1.h`
    /// OBJECT_END+0x27) — the item *instance*'s letter text (a mail-made permanent copy). Nonzero
    /// = right-click reads it (`CMSG_ITEM_TEXT_QUERY`) instead of using it.
    pub fn item_text_id(&self) -> Option<u32> {
        self.get_u32(45)
    }
    /// `ITEM_FIELD_DURABILITY` — an item's current durability (field 46: chain-locked between the
    /// tested STACK_COUNT 14 and CONTAINER_NUM_SLOTS 48 = ITEM_END anchors).
    pub fn item_durability(&self) -> Option<u32> {
        self.get_u32(46)
    }
    /// `ITEM_FIELD_MAXDURABILITY` — the item's durability ceiling (0 = has no durability).
    pub fn item_max_durability(&self) -> Option<u32> {
        self.get_u32(47)
    }
    /// `CONTAINER_FIELD_NUM_SLOTS` — a bag object's capacity.
    pub fn container_num_slots(&self) -> Option<u32> {
        self.get_u32(FIELD_CONTAINER_NUM_SLOTS)
    }
    /// `CONTAINER_FIELD_SLOT_1 + 2i` — the item guid in a bag's slot `i` (< 36). `Some(0)` = the
    /// server said "empty"; `None` = the field wasn't sent.
    pub fn container_slot(&self, i: u8) -> Option<u64> {
        (i < 36).then(|| self.get_guid(FIELD_CONTAINER_SLOT_1 + 2 * u16::from(i)))?
    }
    /// `PLAYER_VISIBLE_ITEM_<slot>_0` — the **public** item entry visibly worn in equipment
    /// slot `i` (what other clients render from; 12 fields per slot after the 2-field creator).
    /// The cross-check for the private INV_SLOT guids — and the entry source for our own
    /// equipment until item objects have a decode path.
    pub fn player_visible_item_entry(&self, i: u8) -> Option<u32> {
        (i < 19)
            .then(|| {
                self.get_u32(FIELD_PLAYER_VISIBLE_ITEM_1_CREATOR + 2 + 12 * u16::from(i))
                    .filter(|&e| e != 0)
            })
            .flatten()
    }
    /// `PLAYER_FIELD_INV_SLOT_HEAD + 2i` — our equipment (0–18) and equipped-bag (19–22) guids.
    pub fn player_inv_slot(&self, i: u8) -> Option<u64> {
        (i < 23).then(|| self.get_guid(FIELD_PLAYER_INV_SLOT_HEAD + 2 * u16::from(i)))?
    }
    /// `PLAYER_FIELD_PACK_SLOT_1 + 2i` — our backpack's 16 item guids.
    pub fn player_pack_slot(&self, i: u8) -> Option<u64> {
        (i < 16).then(|| self.get_guid(FIELD_PLAYER_PACK_SLOT_1 + 2 * u16::from(i)))?
    }
    /// `PLAYER_FIELD_BANK_SLOT_1 + 2i` — our bank's 24 item guids.
    pub fn player_bank_slot(&self, i: u8) -> Option<u64> {
        (i < 24).then(|| self.get_guid(FIELD_PLAYER_BANK_SLOT_1 + 2 * u16::from(i)))?
    }
    /// `PLAYER_FIELD_BANK_BAG_SLOT_1 + 2i` — the item guid of bank bag `i` (0–5, the 6 purchasable
    /// bank bag slots); the bag's *contents* stream as an ordinary `CONTAINER_FIELD_SLOT_*` block
    /// on the bag item itself, addressed by bag byte 63–68 (decision 0604).
    pub fn player_bank_bag_slot(&self, i: u8) -> Option<u64> {
        (i < 6).then(|| self.get_guid(FIELD_PLAYER_BANK_BAG_SLOT_1 + 2 * u16::from(i)))?
    }
    /// `PLAYER_FIELD_VENDORBUYBACK_SLOT_1 + 2i` — the item guid parked in buyback slot `i` (0–11;
    /// the wire's absolute inventory slot is `69 + i`). `Some(0)` = empty.
    pub fn player_buyback_slot(&self, i: u8) -> Option<u64> {
        (i < 12).then(|| self.get_guid(FIELD_PLAYER_VENDORBUYBACK_SLOT_1 + 2 * u16::from(i)))?
    }
    /// `PLAYER_FIELD_BUYBACK_PRICE_1 + i` — what buying slot `i` back costs, in copper.
    pub fn player_buyback_price(&self, i: u8) -> Option<u32> {
        (i < 12).then(|| self.get_u32(FIELD_PLAYER_BUYBACK_PRICE_1 + u16::from(i)))?
    }
    /// `PLAYER_FIELD_BUYBACK_TIMESTAMP_1 + i` — slot `i`'s expiry stamp; the client uses it only
    /// to order the buyback list oldest→newest (wow-re `0x4fafd0`).
    pub fn player_buyback_timestamp(&self, i: u8) -> Option<u32> {
        (i < 12).then(|| self.get_u32(FIELD_PLAYER_BUYBACK_TIMESTAMP_1 + u16::from(i)))?
    }
    /// `PLAYER_FIELD_COINAGE` — our purse in copper. `None` for a non-player unit (no PLAYER block)
    /// or before the field has streamed in; the descriptor's zero-initialized default is 0 copper.
    pub fn player_money(&self) -> Option<u32> {
        self.get_u32(FIELD_PLAYER_FIELD_COINAGE)
    }
    /// `PLAYER_XP` — our current experience within the level (`UnitXP("player")`). `None` for a
    /// non-player unit (no PLAYER block) or before the field has streamed; a PRIVATE field, so it
    /// only ever arrives for our own avatar's descriptor.
    pub fn player_xp(&self) -> Option<u32> {
        self.get_u32(FIELD_PLAYER_XP)
    }
    /// `PLAYER_NEXT_LEVEL_XP` — experience required to reach the next level (`UnitXPMax("player")`).
    /// Same PRIVATE/own-player caveat as [`Self::player_xp`].
    pub fn player_next_level_xp(&self) -> Option<u32> {
        self.get_u32(FIELD_PLAYER_NEXT_LEVEL_XP)
    }
    /// `PLAYER_CHARACTER_POINTS1` — unspent talent points (`UnitCharacterPoints("player")`'s
    /// first return; the talent frame's "N talent points" line, decision 0304). Same
    /// PRIVATE/own-player caveat as [`Self::player_xp`].
    pub fn player_talent_points(&self) -> Option<u32> {
        self.get_u32(FIELD_PLAYER_CHARACTER_POINTS1)
    }
    /// `PLAYER_CHARACTER_POINTS2` — free primary professions (`UnitCharacterPoints`'s second
    /// return; the "You now have %d free professions." chat line). Same caveat as above.
    pub fn player_free_professions(&self) -> Option<u32> {
        self.get_u32(FIELD_PLAYER_CHARACTER_POINTS2)
    }
    /// `PLAYER_TRACK_CREATURES` — the active creature-tracking bitmask (bit `1 << (creatureType −
    /// 1)` per active TRACK_CREATURES aura): the minimap red-dot predicate's mask. PRIVATE — self
    /// only; absent (no PLAYER block, or nothing ever tracked) reads as no tracking.
    pub fn player_track_creatures(&self) -> u32 {
        self.get_u32(FIELD_PLAYER_TRACK_CREATURES).unwrap_or(0)
    }
    /// `PLAYER_TRACK_RESOURCES` — the active resource-tracking bitmask (bit `1 << (lockType − 1)`
    /// per active TRACK_RESOURCES aura, e.g. Find Minerals → the Mining `LockType.dbc` bit): the
    /// minimap gold-dot predicate's mask. Same PRIVATE/self-only caveat as above.
    pub fn player_track_resources(&self) -> u32 {
        self.get_u32(FIELD_PLAYER_TRACK_RESOURCES).unwrap_or(0)
    }
    /// `PLAYER_FIELD_BYTES` byte 0 bit `0x2` — track-stealthed active (vmangos
    /// `PLAYER_FIELD_BYTE_TRACK_STEALTHED`, set by the TRACK_STEALTHED(151) aura handler): the
    /// tracking classifier's stealth always-show clause — with it set, any CREEP-flagged unit
    /// red-dots (`0x5ed210` @ the `+0x1028` read, wow-re `track-predicates.md`). Same
    /// PRIVATE/self-only caveat as the tracking masks.
    pub fn player_track_stealthed(&self) -> bool {
        self.get_u32(FIELD_PLAYER_FIELD_BYTES).unwrap_or(0) & 0x2 != 0
    }
    /// `PLAYER_QUEST_LOG_<slot+1>_*` — one decoded quest-log slot (0-based, `slot <
    /// PLAYER_QUEST_LOG_SLOTS`). `None` when the slot's id field never streamed (or the index is out
    /// of range); `Some` with `quest_id == 0` is a slot the server has explicitly cleared (an
    /// abandon/turn-in leaves this). The count-state and timer fields default to `0` when absent —
    /// the server writes them with the id on every occupy (vmangos `Player.h:1094-1099`
    /// `SetQuestSlot`), so an absent pair only ever accompanies an untouched slot.
    pub fn player_quest_log(&self, slot: u8) -> Option<QuestLogSlot> {
        if slot >= PLAYER_QUEST_LOG_SLOTS {
            return None;
        }
        let base = FIELD_PLAYER_QUEST_LOG_1_1 + 3 * u16::from(slot);
        let quest_id = self.get_u32(base)?;
        let count_state = self.get_u32(base + 1).unwrap_or(0);
        let timer = self.get_u32(base + 2).unwrap_or(0);
        let mut counters = [0u8; 4];
        for (i, c) in counters.iter_mut().enumerate() {
            *c = ((count_state >> (6 * i)) & 0x3F) as u8;
        }
        Some(QuestLogSlot {
            quest_id,
            counters,
            state: (count_state >> 24) as u8,
            timer,
        })
    }
    /// `PLAYER_EXPLORED_ZONES_1 + i` — the discovery bitset's `i`-th u32 (absent slots read 0:
    /// a fresh descriptor means nothing explored yet). The full set is
    /// [`PLAYER_EXPLORED_ZONES_SLOTS`] slots; bit `n` of the whole = AreaTable exploreFlag `n`.
    pub fn player_explored_zone_slot(&self, i: u16) -> u32 {
        if i >= PLAYER_EXPLORED_ZONES_SLOTS {
            return 0;
        }
        self.get_u32(FIELD_PLAYER_EXPLORED_ZONES_1 + i).unwrap_or(0)
    }
    /// `PLAYER_BLOCK_PERCENTAGE` — our chance to block, already a percent (2.62 = "2.62%"). The
    /// spell tooltip's chance-to-X line (law §3-CHANCE) reads exactly this and its three
    /// neighbours; the character sheet reads them too. `None` for a non-player unit.
    pub fn player_block_percentage(&self) -> Option<f32> {
        self.get_f32(FIELD_PLAYER_BLOCK_PERCENTAGE)
    }
    /// `PLAYER_DODGE_PERCENTAGE` — see [`Self::player_block_percentage`].
    pub fn player_dodge_percentage(&self) -> Option<f32> {
        self.get_f32(FIELD_PLAYER_DODGE_PERCENTAGE)
    }
    /// `PLAYER_PARRY_PERCENTAGE` — see [`Self::player_block_percentage`].
    pub fn player_parry_percentage(&self) -> Option<f32> {
        self.get_f32(FIELD_PLAYER_PARRY_PERCENTAGE)
    }
    /// `PLAYER_CRIT_PERCENTAGE` — melee crit; see [`Self::player_block_percentage`]. (The ranged
    /// twin sits one slot above and has no consumer yet.)
    pub fn player_crit_percentage(&self) -> Option<f32> {
        self.get_f32(FIELD_PLAYER_CRIT_PERCENTAGE)
    }
    /// `PLAYER_FIELD_POSSTAT0 + i` — stat `i`'s **positive** buff delta (float despite the header's
    /// "Type: INT" tag; see the field's doc comment). `i >= 5` reads `None`.
    pub fn player_posstat(&self, i: u8) -> Option<f32> {
        (i < 5).then(|| self.get_f32(FIELD_PLAYER_POSSTAT0 + u16::from(i)))?
    }
    /// `PLAYER_FIELD_NEGSTAT0 + i` — stat `i`'s **negative** buff delta; negative-or-zero (see the
    /// field's doc comment for the verified sign). `i >= 5` reads `None`.
    pub fn player_negstat(&self, i: u8) -> Option<f32> {
        (i < 5).then(|| self.get_f32(FIELD_PLAYER_NEGSTAT0 + u16::from(i)))?
    }
    /// `PLAYER_FIELD_RESISTANCEBUFFMODSPOSITIVE + school` — the positive resistance buff for
    /// `school` (0..6). `school >= 7` reads `None`.
    pub fn player_resistance_buff_pos(&self, school: u8) -> Option<f32> {
        (school < 7)
            .then(|| self.get_f32(FIELD_PLAYER_RESISTANCEBUFFMODSPOSITIVE + u16::from(school)))?
    }
    /// `PLAYER_FIELD_RESISTANCEBUFFMODSNEGATIVE + school` — negative-or-zero (see the field's doc
    /// comment). `school >= 7` reads `None`.
    pub fn player_resistance_buff_neg(&self, school: u8) -> Option<f32> {
        (school < 7)
            .then(|| self.get_f32(FIELD_PLAYER_RESISTANCEBUFFMODSNEGATIVE + u16::from(school)))?
    }
    /// `PLAYER_FIELD_MOD_DAMAGE_DONE_POS + school` — positive damage-done bonus (`[0]` = physical).
    /// `school >= 7` reads `None`.
    pub fn player_mod_damage_done_pos(&self, school: u8) -> Option<i32> {
        (school < 7).then(|| self.get_i32(FIELD_PLAYER_MOD_DAMAGE_DONE_POS + u16::from(school)))?
    }
    /// `PLAYER_FIELD_MOD_DAMAGE_DONE_NEG + school` — negative-or-zero (see the field's doc
    /// comment). `school >= 7` reads `None`.
    pub fn player_mod_damage_done_neg(&self, school: u8) -> Option<i32> {
        (school < 7).then(|| self.get_i32(FIELD_PLAYER_MOD_DAMAGE_DONE_NEG + u16::from(school)))?
    }
    /// `PLAYER_FIELD_MOD_DAMAGE_DONE_PCT + school` — the damage-done percent multiplier (a true
    /// float; see the field's doc comment — decision 0208's flagged question, now closed).
    /// `school >= 7` reads `None`; an in-range-but-absent read (no aura has touched this school
    /// yet) is the caller's job to default to `1.0` — the wire only sends fields that changed.
    pub fn player_mod_damage_done_pct(&self, school: u8) -> Option<f32> {
        (school < 7).then(|| self.get_f32(FIELD_PLAYER_MOD_DAMAGE_DONE_PCT + u16::from(school)))?
    }
    /// `PLAYER_SKILL_INFO_<slot+1>_*` — one decoded skill slot (`slot < `[`PLAYER_SKILL_SLOTS`]).
    /// `None` when the id dword never streamed (or `slot` is out of range); the value/max and
    /// bonus dwords default to `0` when absent (mirroring [`Self::player_quest_log`]'s tolerance —
    /// vmangos writes id and value together on `SetSkill`, so an absent pair only ever accompanies
    /// a slot whose snapshot hasn't landed yet).
    pub fn player_skill(&self, slot: u8) -> Option<PlayerSkillSlot> {
        if slot >= PLAYER_SKILL_SLOTS {
            return None;
        }
        let base = FIELD_PLAYER_SKILL_INFO_1_1 + 3 * u16::from(slot);
        let (skill_id, step) = self.get_u16_pair(base)?;
        let (value, max) = self.get_u16_pair(base + 1).unwrap_or((0, 0));
        let (temp_bonus, perm_bonus) = self.get_u16_pair(base + 2).unwrap_or((0, 0));
        Some(PlayerSkillSlot {
            skill_id,
            step,
            value,
            max,
            temp_bonus: temp_bonus as i16,
            perm_bonus: perm_bonus as i16,
        })
    }
    /// `PLAYER_AMMO_ID` — the equipped ammo pouch/quiver's item id (arrows/bullets), or `None`
    /// before the field streams. `0` = no ammo selected.
    pub fn player_ammo_id(&self) -> Option<u32> {
        self.get_u32(FIELD_PLAYER_AMMO_ID)
    }
    /// `PLAYER_BYTES` — packed skinColor (byte 0) / faceType (byte 1) / hairStyle (byte 2) / hairColor
    /// (byte 3). The character compositor's customization input (RF-0056).
    pub fn player_bytes(&self) -> Option<u32> {
        self.get_u32(FIELD_PLAYER_BYTES)
    }
    /// `PLAYER_BYTES_2` — byte 0 = facialHair, byte 1 = unk, byte 2 = bankBagSlots (see
    /// [`Self::player_bank_bag_slots_purchased`]), byte 3 = restState (unread here).
    pub fn player_bytes_2(&self) -> Option<u32> {
        self.get_u32(FIELD_PLAYER_BYTES_2)
    }
    /// `PLAYER_FLAGS` (field 190) — the player state flags. `0` when absent (the descriptor's
    /// zero default — a create block skips zero fields).
    pub fn player_flags(&self) -> u32 {
        self.get_u32(FIELD_PLAYER_FLAGS).unwrap_or(0)
    }
    /// Whether the player is a released **ghost** — `PLAYER_FLAGS_GHOST (0x10)`, set/cleared by
    /// the ghost aura 8326 at release/resurrect (decision 0308 §1). NB a ghost's health is **1**
    /// on the wire, so `unit_is_dead` is FALSE for a ghost: the three predicates are
    /// dead = health 0 · ghost = this bit · dead-or-ghost = either (the real client's
    /// UnitIsDead/UnitIsGhost/UnitIsDeadOrGhost trio).
    pub fn player_is_ghost(&self) -> bool {
        self.player_flags() & 0x10 != 0
    }
    /// `PLAYER_FIELD_BYTES` byte 0 bit `0x08` — `PLAYER_FIELD_BYTE_RELEASE_TIMER`, "Display time
    /// till auto release spirit" (the header's own comment): set at death iff the map is
    /// non-instanceable, i.e. exactly when the server will force-release after 6:00. The client
    /// mirrors that constant for the DEATH popup's countdown (byte-VERIFIED — wow-re death-ui.md
    /// §1: the client arms `now + 360000 ms` at its own alive→dead edge; bit clear ⇒
    /// `GetReleaseTimeRemaining() == -1`).
    pub fn player_release_timer_running(&self) -> bool {
        self.get_u32(FIELD_PLAYER_FIELD_BYTES).unwrap_or(0) & 0x08 != 0
    }
    /// `PLAYER_FIELD_BYTES` byte 3 — the player's **highest lifetime honor rank** (vmangos
    /// `PLAYER_FIELD_BYTES_OFFSET_HIGHEST_HONOR_RANK`, written by `HonorMgr`): what the client's
    /// item-usable gate compares an item's `RequiredHonorRank` against (`0x5ea930` reads the
    /// values byte at `+0x102b`). `None` before the field streams; rank 0 = never ranked.
    pub fn player_honor_rank(&self) -> Option<u8> {
        self.get_u32(FIELD_PLAYER_FIELD_BYTES)
            .map(|b| ((b >> 24) & 0xff) as u8)
    }
    /// `UNIT_FIELD_BYTES_0` byte 0 — the unit's race. `None` when the field wasn't sent.
    pub fn unit_race(&self) -> Option<u8> {
        self.unit_bytes_0().map(|b| (b & 0xff) as u8)
    }
    /// `UNIT_FIELD_BYTES_0` byte 1 — the unit's class.
    pub fn unit_class(&self) -> Option<u8> {
        self.unit_bytes_0().map(|b| ((b >> 8) & 0xff) as u8)
    }
    /// `UNIT_FIELD_BYTES_0` byte 2 — the unit's gender/sex.
    pub fn unit_gender(&self) -> Option<u8> {
        self.unit_bytes_0().map(|b| ((b >> 16) & 0xff) as u8)
    }
    /// `PLAYER_BYTES` byte 0 — skinColor. `None` for a non-player unit (no PLAYER block).
    pub fn player_skin(&self) -> Option<u8> {
        self.player_bytes().map(|b| (b & 0xff) as u8)
    }
    /// `PLAYER_BYTES` byte 1 — faceType.
    pub fn player_face(&self) -> Option<u8> {
        self.player_bytes().map(|b| ((b >> 8) & 0xff) as u8)
    }
    /// `PLAYER_BYTES` byte 2 — hairStyle.
    pub fn player_hair_style(&self) -> Option<u8> {
        self.player_bytes().map(|b| ((b >> 16) & 0xff) as u8)
    }
    /// `PLAYER_BYTES` byte 3 — hairColor.
    pub fn player_hair_color(&self) -> Option<u8> {
        self.player_bytes().map(|b| ((b >> 24) & 0xff) as u8)
    }
    /// `PLAYER_BYTES_2` byte 0 — facialHair.
    pub fn player_facial_hair(&self) -> Option<u8> {
        self.player_bytes_2().map(|b| (b & 0xff) as u8)
    }
    /// `PLAYER_BYTES_2` byte 2 — `PLAYER_BYTES_2_OFFSET_BANK_BAG_SLOTS` (vmangos `Player.h:347`):
    /// how many of the 6 purchasable bank bag slots are bought. Byte **2**, not byte 1 — the
    /// server writes byte 0 facialHair, byte 1 unused, byte 2 bankBagSlots, byte 3 restState. The
    /// only success signal for `CMSG_BUY_BANK_SLOT` (decision 0604): the server sends no packet on
    /// success, only this descriptor delta (`PLAYERBANKBAGSLOTS_CHANGED` on the reference client).
    pub fn player_bank_bag_slots_purchased(&self) -> Option<u8> {
        self.player_bytes_2().map(|b| ((b >> 16) & 0xff) as u8)
    }
    /// `GAMEOBJECT_DISPLAYID`.
    pub fn gameobject_displayid(&self) -> Option<i32> {
        self.get_i32(FIELD_GAMEOBJECT_DISPLAYID)
    }
    /// `GAMEOBJECT_STATE` — 0 active (door open/triggered), 1 ready (closed/armed), 2 active-alt
    /// (vmangos `GOState`, `OBJECT_END + 0x8` in 1.12). Drives the open/close sound slots.
    pub fn gameobject_state(&self) -> Option<u32> {
        self.get_u32(FIELD_GAMEOBJECT_STATE)
    }
    /// `GAMEOBJECT_ROTATION` — the spawn's local-rotation quaternion `(x, y, z, w)`. `None` when
    /// the wire carried no component at all (vmangos omits zero fields from the create mask, and
    /// an identity-ish quat still sends its nonzero `w`/`z`); absent components within a
    /// partially-sent quat read as the create-block zero default, so a pure-yaw spawn
    /// (`x = y = 0`, the live-data norm) round-trips exactly. Consumed by the type-11 transport
    /// evaluator (elevators/lifts rotate their keyframe offsets through this).
    pub fn gameobject_rotation(&self) -> Option<[f32; 4]> {
        let any_sent =
            (0..4u16).any(|i| self.fields.contains_key(&(FIELD_GAMEOBJECT_ROTATION + i)));
        any_sent.then(|| {
            [
                self.get_f32(FIELD_GAMEOBJECT_ROTATION).unwrap_or(0.0),
                self.get_f32(FIELD_GAMEOBJECT_ROTATION + 1).unwrap_or(0.0),
                self.get_f32(FIELD_GAMEOBJECT_ROTATION + 2).unwrap_or(0.0),
                self.get_f32(FIELD_GAMEOBJECT_ROTATION + 3).unwrap_or(0.0),
            ]
        })
    }
    /// GameObject position + facing (`GAMEOBJECT_POS_*` / `GAMEOBJECT_FACING`). On a create block this
    /// is `Some` **even when none of the three were sent** — the created-store zero-fill
    /// (`get_f32`/`get_u32`'s `created.then_some(0)`) can't tell "genuinely at the origin" from "never
    /// carried a position at all" (a transport GameObject, whose stationary spot rides the movement
    /// block's `HAS_POSITION` slot instead — decision 0438 "the wire"). [`Self::gameobject_pos_sent`]
    /// answers that distinction; the create-placement fallback (`events/decode.rs`) gates on it.
    pub fn gameobject_position(&self) -> Option<(Vector3d, f32)> {
        Some((
            Vector3d {
                x: self.get_f32(FIELD_GAMEOBJECT_POS_X)?,
                y: self.get_f32(FIELD_GAMEOBJECT_POS_Y)?,
                z: self.get_f32(FIELD_GAMEOBJECT_POS_Z)?,
            },
            self.get_f32(FIELD_GAMEOBJECT_FACING).unwrap_or(0.0),
        ))
    }
    /// Whether the wire genuinely carried **any** `GAMEOBJECT_POS_*` field — a raw map read on purpose
    /// (like `get_guid`'s, bypassing the create block's absent-is-zero fold) so a consumer can tell
    /// "this GO's create never carried a position" from "it did, and it's `0.0` on some axis". A normal
    /// GameObject sends at least one non-zero axis virtually always; a boat/zeppelin/elevator sends
    /// **none** (vmangos never sets `GAMEOBJECT_POS_*` on a transport).
    pub fn gameobject_pos_sent(&self) -> bool {
        self.fields.contains_key(&FIELD_GAMEOBJECT_POS_X)
            || self.fields.contains_key(&FIELD_GAMEOBJECT_POS_Y)
            || self.fields.contains_key(&FIELD_GAMEOBJECT_POS_Z)
    }
    /// `GAMEOBJECT_TYPE_ID` — the GameObject sub-type (DOOR/CHEST/SPELL_FOCUS/…). **Absent ⇒ `0` =
    /// DOOR**, not "unknown": vmangos omits a zero-valued field from the create mask (the same reason an
    /// all-zero `PLAYER_BYTES` is never sent), and `GAMEOBJECT_TYPE_DOOR` *is* 0 — so a door sends no type
    /// field at all, and the absent value is its default. Reading it as `None` silently mis-handled every
    /// door (no interact cursor, no open/close animation), so this defaults to the wire default.
    pub fn gameobject_type_id(&self) -> i32 {
        self.get_i32(FIELD_GAMEOBJECT_TYPE_ID).unwrap_or(0)
    }
    /// `GAMEOBJECT_FLAGS` (`GameObjectFlags`) — static interactability bits: `0x1` IN_USE (busy),
    /// `0x4` INTERACT_COND (usable only when the per-player `GAMEOBJECT_DYN_FLAGS` activate bit is set —
    /// the quest gate), `0x10` NO_INTERACT. Read by the cursor/interact usability gate (decision 0243).
    pub fn gameobject_flags(&self) -> u32 {
        self.get_u32(FIELD_GAMEOBJECT_FLAGS).unwrap_or(0)
    }
    /// `GAMEOBJECT_DYN_FLAGS` — the **per-player** dynamic flags; bit `0x1` = `GO_DYNFLAG_LO_ACTIVATE`
    /// (the sparkle / "usable for me now"), which the server sets from `GameObject::ActivateToQuest`.
    /// Consulted only when `GAMEOBJECT_FLAGS & INTERACT_COND` is set (decision 0243).
    pub fn gameobject_dynamic_flags(&self) -> u32 {
        self.get_u32(FIELD_GAMEOBJECT_DYN_FLAGS).unwrap_or(0)
    }
    /// `GAMEOBJECT_LEVEL` — the object's level (vmangos `UpdateFields_1_12_1.h`: `OBJECT_END + 0x10`
    /// = 22). The lock-refusal toast's required-rank fallback reads it: a skill lock whose
    /// `Skill[0]` is 0 displays `level × 5` instead (wow-re cursor-system.md §8.8, `0x5f3490`).
    /// `0` when absent (the create-block zero default).
    pub fn gameobject_level(&self) -> u32 {
        self.get_u32(FIELD_GAMEOBJECT_LEVEL).unwrap_or(0)
    }

    /// The object's class from the type field (`OBJECT_FIELD_TYPE`), used for `Values`/`Movement`
    /// updates that carry no separate `TypeId`. Unused by the create path (which has a `TypeId`).
    #[allow(dead_code)]
    pub fn object_type(&self) -> Option<ObjectType> {
        self.get_u32(FIELD_OBJECT_TYPE).map(|bits| {
            if bits & 0x10 != 0 {
                ObjectType::Player
            } else if bits & 0x08 != 0 {
                ObjectType::Unit
            } else if bits & 0x20 != 0 {
                ObjectType::GameObject
            } else {
                ObjectType::Object
            }
        })
    }
}
