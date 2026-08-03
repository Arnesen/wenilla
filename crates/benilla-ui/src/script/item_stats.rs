//! The **shared item-template store** — one `item id → ItemTemplateView` table every item hover
//! renders through (decision 0274 P1). In the real client every item tooltip is the same C++
//! renderer (`0x52b650`, behind 8 of the 9 `Set*Item` bindings — wow-re
//! `ui/scratch/tooltip-money.md`) over the same item-template cache; benilla mirrors that with
//! one engine store the app feeds from its ask-once `ITEM_QUERY` template cache, and one engine
//! renderer ([`super::tooltip_item`]).
//!
//! Fill flow: the app **pushes** every item template the moment it lands in its cache
//! ([`super::UiScript::set_item_template`]) — arrival-driven, so a first hover of an item whose
//! name is already on screen always hits. A renderer read of an id the app never resolved
//! additionally records the id ([`super::UiScript::take_item_stat_asks`] drains them), which
//! makes the app send `CMSG_ITEM_QUERY` and push when the answer arrives — the real client's
//! uncached-item early-out (cleared tooltip + query; the hover's re-enter loop repaints on
//! arrival).
//!
//! The view carries the template's tooltip-relevant fields plus the strings only the app can
//! resolve (skill/faction names from the DBC catalogs, trigger-spell display text) — the engine
//! renders lines, it never reads DBCs.

use std::collections::HashMap;

use mlua::{Lua, MultiValue, Value};

use super::Model;

/// An item template's tooltip view (decision 0274 P1) — the fields the line law consumes, in
/// wire terms (vmangos `SMSG_ITEM_QUERY_SINGLE_RESPONSE`), plus app-resolved display strings.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ItemTemplateView {
    /// The name line (quality-colored).
    pub name: String,
    pub quality: u32,
    /// Item class/subclass (the slot line's right column) + the equip slot (its left).
    pub class: u32,
    pub subclass: u32,
    pub inventory_type: u32,
    /// The alternate subclass whose proficiency also permits use (ItemSubClass.dbc
    /// prerequisite/postrequisite, app-resolved with the builder's sentinel walk: prerequisite
    /// wins, postrequisite only when prerequisite is −1). A weapon missing its own mask bit but
    /// holding the alternate's reds the SLOT cell instead of the type cell.
    pub proficiency_alt: Option<u32>,
    /// ItemSubClass displayFlags bit 0 (app-resolved): the type cell never prints — the
    /// "Miscellaneous" family (rings, trinkets, shirts, consumables, recipes).
    pub hide_subclass: bool,
    /// Template flags — bit `0x2` = conjured ("Conjured Item").
    pub flags: u32,
    /// Bonding: 1 = binds on pickup, 2 = on equip, 3 = on use, 4/5 = quest item.
    pub bonding: u32,
    /// `MaxCount` — 1 = "Unique", N > 1 = "Unique (N)", 0 = no line.
    pub max_count: u32,
    /// Nonzero = "This Item Begins a Quest".
    pub start_quest: u32,
    /// Container size — "N Slot Bag".
    pub container_slots: u32,
    /// Stat mods `(type, value)` in wire order (types: 0 mana, 1 health, 3 agi, 4 str, 5 int,
    /// 6 spi, 7 stam — the `ITEM_MOD_*` GlobalStrings family).
    pub stats: Vec<(u32, i32)>,
    /// Damage blocks `(min, max, school)` in wire order; school 0 physical, 1..6 =
    /// Holy/Fire/Nature/Frost/Shadow/Arcane.
    pub damages: Vec<(f32, f32, u32)>,
    pub delay_ms: u32,
    pub armor: u32,
    pub block: u32,
    /// Holy..Arcane (armor is its own field), the "+N X Resistance" lines.
    pub resistances: [i32; 6],
    /// "Durability N / N" (a template hover shows full).
    pub max_durability: u32,
    /// "Requires Level N" — printed only for N > 1 (the real builder's `0x52d2cf` gate); red
    /// when the player is lower.
    pub required_level: u32,
    /// Class/race masks; `<= 0` = everyone (no line). Red when the player's bit is absent.
    pub allowable_class: i32,
    pub allowable_race: i32,
    /// Skill requirement: the SkillLine id + rank, with the display name app-resolved
    /// (`SkillLine.dbc`). `required_skill_name = None` = no skill line.
    pub required_skill: u32,
    pub required_skill_rank: u32,
    pub required_skill_name: Option<String>,
    /// Spell requirement (nonzero = "Requires <name>") — red when the spellbook doesn't know it.
    pub required_spell: u32,
    pub required_spell_name: Option<String>,
    /// `RequiredHonorRank` — no tooltip line in 1.12, but the item-usable gate (`0x5ea930`)
    /// compares it against the player's highest honor rank; the merchant list reds on it.
    pub required_honor_rank: u32,
    /// `RequiredCityRank` — usable-gate only, like the honor rank. Vanilla data ships no nonzero
    /// value and vmangos never writes the `PVP_MEDALS` bits the client would test, so a nonzero
    /// requirement can only fail (see [`item_usable`]).
    pub required_city_rank: u32,
    /// Reputation requirement, app-resolved to "Requires <Faction> - <Standing>"; the raw
    /// faction id + rank ride along for the red check against [`PlayerReqState::rep_ranks`].
    pub required_rep_line: Option<String>,
    pub required_rep_faction: u32,
    pub required_rep_rank: u32,
    /// Trigger-spell lines `(trigger, spell id, display text)` in wire order: trigger 0/5 =
    /// "Use:", 1 = "Equip:", 2 = "Chance on hit:", 6 = a taught spell (the "Already known" red
    /// check, no green line). The text is app-resolved (the spell's name in P1; its substituted
    /// description in P2) — green lines.
    pub spell_triggers: Vec<(u32, u32, String)>,
    /// `LockID` — nonzero prints the red "Locked" line (the key-item sub-line joins with the
    /// Lock.dbc resolve, the GO-locks follow-up).
    pub lock_id: u32,
    /// "N Charge(s)" — the app-resolved count for the first spell slot that survives the real
    /// builder's charge gate (`0x52db51`: a slot whose value is 0 or the `-1` consume-on-use
    /// sentinel prints nothing; else `abs`). 0 = no line.
    pub charges: i32,
    /// The yellow quoted flavor text (wrapped).
    pub description: String,
    /// Nonzero = "<Right Click to Read>" (green).
    pub page_text: u32,
    /// Copper — the merchant-open money row (the engine fires `OnTooltipAddMoney`), and
    /// `ITEM_UNSELLABLE` when 0 in a sell context.
    pub sell_price: u32,
    /// `itemset` — nonzero renders the SET block ([`ItemSetView`], asked once by set id).
    pub item_set: u32,
    /// `RandomProperty` (template `+0x1b8`) — the item CAN roll a "… of the Bear" suffix. Its one
    /// consumer is the enchant family's third arm: with no instance to read a roll from, the
    /// tooltip prints the `<Random enchantment>` placeholder instead of any per-slot line (wow-re
    /// §1-ENCHANT §E5). Decision 0920.
    pub random_property: u32,
}

/// The player state the red-line law compares against (decision 0274 P1): pushed by the app
/// whenever it changes. Level and class also ride the `"player"` unit feed; this carries the
/// pieces that don't (the class/race IDS as mask bits, and the skill ranks).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct PlayerReqState {
    pub level: u32,
    /// Class id (1 warrior … 11 druid) — the `allowable_class` mask bit is `1 << (id-1)`.
    pub class_id: u32,
    /// Race id (1 human … 8 troll) — the `allowable_race` mask bit is `1 << (id-1)`.
    pub race_id: u32,
    /// SkillLine id → current rank, for "Requires <skill> (N)" checks.
    pub skills: HashMap<u32, u32>,
    /// Item class (2 weapons / 4 armor) → allowed-subclass bitmask (`SMSG_SET_PROFICIENCY`,
    /// the client's `0xc4d4a0[class]` store). The slot-line proficiency red: a class WITH an
    /// entry here reds when the item's `1 << subclass` bit is absent; a class with no entry
    /// (consumables etc.) never reds.
    pub proficiency: HashMap<u32, u32>,
    /// Faction id → the player's reputation rank (0 hated … 7 exalted; DBC base + wire
    /// standing, app-ranked) — the "Requires <Faction> - <Standing>" red check.
    pub rep_ranks: HashMap<u32, u8>,
    /// Whether the spellbook holds an effect-40 (SPELL_EFFECT_DUAL_WIELD) spell — the client's
    /// `0xc4d770` global (stored on learn, cleared on unlearn; reader `0x5eab70`). An off-hand
    /// weapon (InventoryType 22) reds its SLOT cell without it.
    pub can_dual_wield: bool,
    /// The player's **highest lifetime honor rank** (`PLAYER_FIELD_BYTES` byte 3) — the
    /// usable-gate's `RequiredHonorRank` comparand.
    pub honor_rank: u8,
}

/// The client's item-usable predicate `0x5ea930(player; itemCacheRecord, &err)` — byte-read from
/// wow-re's `ui/scratch/disasm-full.txt`. Both merchant getters call it (`GetMerchantItemInfo`
/// `0x4fb2a3`, `GetBuybackItemInfo` `0x4fb4f7`) and push `1`/`nil` as `isUsable`; the FrameXML
/// reds the row on `nil`. The legs, in the binary's order:
///
/// 1. `requiredLevel > player level` → unusable.
/// 2. class mask: `allowableClass & 1<<(classId−1)` clear → unusable (−1 = every bit set).
/// 3. race mask: same test against `allowableRace`.
/// 4. proficiency: a mask exists for the item class AND the item's **own** subclass bit is clear
///    → unusable. NO ItemSubClass alternate walk here — the alternate only chooses which tooltip
///    CELL reds (0297); the usable gate is the raw bit.
/// 5. `requiredSkill`: unknown skill → unusable; known → `value + permBonus ≥ requiredSkillRank`.
/// 6. `requiredSpell`: not in the spellbook → unusable.
/// 7. `requiredHonorRank`: player's highest honor rank (`PLAYER_FIELD_BYTES` byte 3) short →
///    unusable.
/// 8. `requiredCityRank`: tests `PLAYER_FIELD_PVP_MEDALS & 1<<(rank−1)` — vanilla never writes
///    the medals field and ships no city-rank items, so a nonzero requirement always fails;
///    mirrored as the constant result rather than plumbing a dead field.
/// 9. reputation: player standing ≥ the required rank's threshold (`0x4d6370` +
///    the `0x80928c` threshold table) — equivalently rank ≥ requiredRepRank, the tooltip red's
///    exact compare; an unknown faction counts as rank 0.
///
/// A template the cache hasn't answered yet is USABLE (the getter skips the call on a null
/// record — `0x4fb298`); the engine analog is an unpushed [`PlayerReqState`] (level 0, a state
/// the real client can't reach), which also declines to judge.
pub fn item_usable(
    v: &ItemTemplateView,
    req: &PlayerReqState,
    knows_spell: impl Fn(u32) -> bool,
) -> bool {
    if req.level == 0 {
        return true;
    }
    if v.required_level > req.level {
        return false;
    }
    if req.class_id == 0 || v.allowable_class as u32 & (1 << (req.class_id - 1)) == 0 {
        return false;
    }
    if req.race_id == 0 || v.allowable_race as u32 & (1 << (req.race_id - 1)) == 0 {
        return false;
    }
    if let Some(&mask) = req.proficiency.get(&v.class) {
        if mask & (1 << v.subclass) == 0 {
            return false;
        }
    }
    if v.required_skill != 0 {
        match req.skills.get(&v.required_skill) {
            None => return false,
            Some(&val) => {
                if val < v.required_skill_rank {
                    return false;
                }
            }
        }
    }
    if v.required_spell != 0 && !knows_spell(v.required_spell) {
        return false;
    }
    if v.required_honor_rank != 0 && u32::from(req.honor_rank) < v.required_honor_rank {
        return false;
    }
    if v.required_city_rank != 0 {
        return false;
    }
    if v.required_rep_faction != 0 {
        let rank = req
            .rep_ranks
            .get(&v.required_rep_faction)
            .copied()
            .unwrap_or(0);
        if u32::from(rank) < v.required_rep_rank {
            return false;
        }
    }
    true
}

/// An item set's tooltip view (the §22 SET block), app-resolved: the ItemSet.dbc row with
/// member item NAMES joined from the template cache (a `None` name = the member's template is
/// still in flight — its line waits; the app re-pushes as answers land) and the threshold
/// bonuses' TEXT ($-substituted spell descriptions). The engine supplies the live half: the
/// owned/equipped counts off its own inventory slots.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ItemSetView {
    pub name: String,
    /// `(item id, resolved name)` per member, DBC order.
    pub members: Vec<(u32, Option<String>)>,
    /// `(required equipped count, bonus text)` per bonus, in the DBC's stored slot order —
    /// the renderer sorts threshold-ascending at print time, like the builder's qsort
    /// (`0x52e5c0`; The Gladiator ships 3,2,5,4 and prints 2,3,4,5).
    pub bonuses: Vec<(u32, String)>,
    /// The set-level skill requirement ("Requires <skill> (N)"), app-named; 0/None = no line.
    pub required_skill: u32,
    pub required_skill_rank: u32,
    pub required_skill_name: Option<String>,
}

impl super::UiScript {
    /// Store (or replace) an item's template view — the app's push half of the ask-once flow.
    pub fn set_item_template(&mut self, item_id: u32, view: ItemTemplateView) {
        let mut model = self.model_mut();
        model.item_stat_asks.remove(&item_id);
        model.item_templates.insert(item_id, view);
    }

    /// Drain the ids the renderer asked for that the store didn't have.
    pub fn take_item_stat_asks(&mut self) -> Vec<u32> {
        self.model_mut().item_stat_asks.drain().collect()
    }

    /// Store (or replace) a set's view — the SET block's push half (re-pushed as member names
    /// resolve).
    pub fn set_item_set(&mut self, set_id: u32, view: ItemSetView) {
        let mut model = self.model_mut();
        model.item_set_asks.remove(&set_id);
        model.item_sets.insert(set_id, view);
    }

    /// Drain the set ids the renderer asked for that the store didn't have.
    pub fn take_item_set_asks(&mut self) -> Vec<u32> {
        self.model_mut().item_set_asks.drain().collect()
    }

    /// Push the red-line law's player state (level/class/race/skills). Cheap to call on change.
    pub fn set_player_req_state(&mut self, state: PlayerReqState) {
        self.model_mut().player_req = state;
    }
}

/// Register the shared item-stats global (the P0 Lua stat-head read — still the merchant
/// sell-cursor's source and the compat surface while call sites finish moving to the engine
/// renderer; the tooltip itself no longer routes through it).
pub(super) fn install(lua: &Lua) -> mlua::Result<()> {
    // BenillaGetItemStats(itemId) → name, quality, invType, class, subclass, dmgMin, dmgMax,
    // dmgType, delayMs, armor, block, sellPrice — or nil (recording the ask).
    lua.globals().set(
        "BenillaGetItemStats",
        lua.create_function(|lua, item_id: u32| {
            let view = {
                let mut model = lua.app_data_mut::<Model>().expect("model app_data");
                let v = model.item_templates.get(&item_id).cloned();
                if v.is_none() && item_id != 0 {
                    model.item_stat_asks.insert(item_id);
                }
                v
            };
            let Some(v) = view else {
                return Ok(MultiValue::from_vec(vec![Value::Nil]));
            };
            let (dmg_min, dmg_max, dmg_type) = v.damages.first().copied().unwrap_or_default();
            Ok(MultiValue::from_vec(vec![
                Value::String(lua.create_string(&v.name)?),
                Value::Integer(i64::from(v.quality)),
                Value::Integer(i64::from(v.inventory_type)),
                Value::Integer(i64::from(v.class)),
                Value::Integer(i64::from(v.subclass)),
                Value::Number(f64::from(dmg_min)),
                Value::Number(f64::from(dmg_max)),
                Value::Integer(i64::from(dmg_type)),
                Value::Integer(i64::from(v.delay_ms)),
                Value::Integer(i64::from(v.armor)),
                Value::Integer(i64::from(v.block)),
                Value::Integer(i64::from(v.sell_price)),
            ]))
        })?,
    )
}

#[cfg(test)]
mod tests {
    use super::{item_usable, ItemTemplateView, PlayerReqState};
    use crate::script::UiScript;

    /// Every leg of the `0x5ea930` gate, one at a time against a passing baseline.
    #[test]
    fn item_usable_mirrors_the_gate_legs() {
        let base_item = ItemTemplateView {
            allowable_class: -1,
            allowable_race: -1,
            ..Default::default()
        };
        let base_req = PlayerReqState {
            level: 4,
            class_id: 1, // warrior
            race_id: 2,  // orc
            ..Default::default()
        };
        let knows_none = |_: u32| false;
        assert!(item_usable(&base_item, &base_req, knows_none));

        // 1 · level: required 5 vs level 4 fails; exactly 5 passes (jg, not jge).
        let mut v = base_item.clone();
        v.required_level = 5;
        assert!(!item_usable(&v, &base_req, knows_none));
        let mut req = base_req.clone();
        req.level = 5;
        assert!(item_usable(&v, &req, knows_none));

        // 2/3 · class + race masks: the player's bit must be set; −1 has every bit.
        let mut v = base_item.clone();
        v.allowable_class = 1 << 3; // rogue-only (class 4)
        assert!(!item_usable(&v, &base_req, knows_none));
        let mut v = base_item.clone();
        v.allowable_race = 1 << 0; // human-only
        assert!(!item_usable(&v, &base_req, knows_none));

        // 4 · proficiency: a mask for the item class with the subclass bit clear fails — and
        // there is NO alternate walk here (a 2H axe reds even when the 1H bit is set).
        let mut v = base_item.clone();
        (v.class, v.subclass) = (2, 1); // Two-Handed Axe
        let mut req = base_req.clone();
        req.proficiency.insert(2, 1 << 0); // knows One-Handed Axes only
        assert!(!item_usable(&v, &req, knows_none));
        req.proficiency.insert(2, 1 << 1);
        assert!(item_usable(&v, &req, knows_none));
        // No mask for the class at all (consumables): the leg never fires.
        let mut v = base_item.clone();
        (v.class, v.subclass) = (0, 0);
        assert!(item_usable(&v, &base_req, knows_none));

        // 5 · skill: unknown skill fails even at rank 0; known compares value ≥ rank.
        let mut v = base_item.clone();
        v.required_skill = 164; // Blacksmithing
        assert!(!item_usable(&v, &base_req, knows_none));
        let mut req = base_req.clone();
        req.skills.insert(164, 0);
        assert!(item_usable(&v, &req, knows_none));
        v.required_skill_rank = 100;
        assert!(!item_usable(&v, &req, knows_none));
        req.skills.insert(164, 100);
        assert!(item_usable(&v, &req, knows_none));

        // 6 · spell: required and not in the spellbook fails.
        let mut v = base_item.clone();
        v.required_spell = 9787; // Weaponsmith
        assert!(!item_usable(&v, &base_req, knows_none));
        assert!(item_usable(&v, &base_req, |id| id == 9787));

        // 7 · honor rank: the player's highest rank must reach it.
        let mut v = base_item.clone();
        v.required_honor_rank = 3;
        assert!(!item_usable(&v, &base_req, knows_none));
        let mut req = base_req.clone();
        req.honor_rank = 3;
        assert!(item_usable(&v, &req, knows_none));

        // 8 · city rank: no live data ever sets it, and the medals field it tests is never
        // written — a nonzero requirement can only fail.
        let mut v = base_item.clone();
        v.required_city_rank = 1;
        assert!(!item_usable(&v, &base_req, knows_none));

        // 9 · reputation: rank below the requirement fails; at it, passes; an unknown faction
        // is rank 0 (fails any nonzero requirement, passes a zero one).
        let mut v = base_item.clone();
        (v.required_rep_faction, v.required_rep_rank) = (87, 5);
        assert!(!item_usable(&v, &base_req, knows_none));
        let mut req = base_req.clone();
        req.rep_ranks.insert(87, 5);
        assert!(item_usable(&v, &req, knows_none));
        let mut v = base_item.clone();
        (v.required_rep_faction, v.required_rep_rank) = (87, 0);
        assert!(item_usable(&v, &base_req, knows_none));

        // An unpushed player state (level 0 — unreachable in the real client) declines to judge.
        let mut v = base_item.clone();
        v.required_level = 60;
        assert!(item_usable(&v, &PlayerReqState::default(), knows_none));
    }

    /// The ask-once loop end-to-end: a miss answers nil AND records the ask; the app's push makes
    /// the next read answer the stat head; the push clears the pending ask.
    #[test]
    fn miss_records_ask_and_push_serves_the_stats() {
        let mut s = UiScript::new().unwrap();
        assert!(s
            .eval::<bool>("return BenillaGetItemStats(25) == nil")
            .unwrap());
        assert_eq!(s.take_item_stat_asks(), vec![25]);

        s.set_item_template(
            25,
            ItemTemplateView {
                name: "Worn Shortsword".into(),
                quality: 1,
                inventory_type: 21,
                class: 2,
                subclass: 7,
                damages: vec![(1.0, 3.0, 0)],
                delay_ms: 1900,
                ..Default::default()
            },
        );
        let (name, quality, inv): (String, i64, i64) =
            s.eval("return BenillaGetItemStats(25)").unwrap();
        assert_eq!((name.as_str(), quality, inv), ("Worn Shortsword", 1, 21));
        assert!(s.take_item_stat_asks().is_empty(), "push cleared the ask");
        // id 0 (an unresolved row) never records a junk ask.
        assert!(s
            .eval::<bool>("return BenillaGetItemStats(0) == nil")
            .unwrap());
        assert!(s.take_item_stat_asks().is_empty());
    }
}
