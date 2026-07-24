//! The engine **spell/aura tooltip channel** (decision 0274 P2) — the verified line law of the
//! spell builder `0x52e610` and the aura builder `0x52f880` (wow-re
//! `ui/scratch/tooltip-content-law.md`, the 0276 fold-back):
//!
//! - name (white) | rank (gray) — one double line;
//! - **Cost | Range** — ONE double line (either side may be absent);
//! - **CastTime | Cooldown** — ONE double line; a passive spell simply omits it (there is NO
//!   "Passive" text in the 1.12 builder);
//! - reagents — inline red per missing item (the one builder that uses the `|cffff2020` escape;
//!   joins when a reagent feed exists);
//! - description — gold, wrapped. An AURA's description is **white** (byte-verified difference),
//!   and only `SetPlayerBuff` appends the duration-remaining line.
//!
//! The engine renders VIEWS ([`SpellTooltipView`]) the app resolves at push time — the $-token
//! substitution (values off Spell.dbc + the player's level), cast-time/duration/range text —
//! because the catalogs and the token engine live app-side; the engine holds no DBC knowledge.
//! Views are keyed by spell id in an ask-once store (the item-template store's pattern): a
//! renderer miss records the id, the app resolves and pushes, the hover's re-enter repaints.

use mlua::{Lua, Table, Value};

use super::object::frame_handle_of;
use super::tooltip::{append_line, clear_content, fire_cleared};
use super::Model;

const WHITE: [f32; 4] = [1.0, 1.0, 1.0, 1.0];
/// The rank column's gray — byte-verified `0xff808080` (0276).
const GRAY: [f32; 4] = [128.0 / 255.0, 128.0 / 255.0, 128.0 / 255.0, 1.0];
/// The description gold — byte-verified `0xffffd200` (0276).
const GOLD: [f32; 4] = [1.0, 210.0 / 255.0, 0.0, 1.0];

/// One spell's tooltip view — every string app-resolved (the $-engine's output for the
/// description; the cost/range/cast/cooldown texts off the DBC catalogs).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct SpellTooltipView {
    pub name: String,
    /// "Rank N" — the gray right column of the name line.
    pub rank: Option<String>,
    /// "35 Mana" / "20 Rage" / "Next melee" — the cost cell.
    pub cost: Option<String>,
    /// "30 yd range" — the range cell.
    pub range: Option<String>,
    /// "1.5 sec cast" / "Instant cast" / "Instant" — `None` = a passive spell: the whole
    /// casttime|cooldown line is omitted (the verified law; never a "Passive" text line).
    pub cast_time: Option<String>,
    /// "15 sec cooldown" — the cooldown cell: `max(RecoveryTime, CategoryRecoveryTime)` (the
    /// 0276 line law §3.4 — Charge's 15 s lives in the CATEGORY column).
    pub cooldown: Option<String>,
    /// "Requires Battle Stance" — the required-form line (law §3.6, `SPELL_REQUIRED_FORM` over
    /// the `Stances` mask): white when [`Self::form_met`], red when not.
    pub requires_form: Option<String>,
    /// Whether the player's CURRENT shapeshift form satisfies the mask (the app re-pushes views
    /// on a form change, so the color tracks live stance switches).
    pub form_met: bool,
    /// The $-substituted description — gold + wrapped for spells.
    pub description: String,
    /// The $-substituted AURA description (`Spell.dbc AuraDescription`) — the buff hover's
    /// white text (byte-verified: the aura builder reads the aura column). Falls back to
    /// `description` when empty.
    pub aura_description: String,
}

impl super::UiScript {
    /// Store (or replace) a spell's tooltip view — the app's push half of the ask-once flow.
    pub fn set_spell_tooltip(&mut self, spell_id: u32, view: SpellTooltipView) {
        let mut model = self.model_mut();
        model.spell_tooltip_asks.remove(&spell_id);
        model.spell_tooltips.insert(spell_id, view);
    }

    /// Drain the spell ids the renderers asked for that the store didn't have.
    pub fn take_spell_tooltip_asks(&mut self) -> Vec<u32> {
        self.model_mut().spell_tooltip_asks.drain().collect()
    }
}

/// Look up the store; a miss records the ask. `pub(super)` for the talent tooltip's shared use
/// (its display + next-rank spells ride this same ask-once channel — decision 0304).
pub(super) fn spell_view_of(lua: &Lua, spell_id: u32) -> Option<SpellTooltipView> {
    let mut model = lua.app_data_mut::<Model>().expect("model app_data");
    let v = model.spell_tooltips.get(&spell_id).cloned();
    if v.is_none() && spell_id != 0 {
        model.spell_tooltip_asks.insert(spell_id);
    }
    v
}

/// The talent interleave for [`render_spell`] (decision 0304; the builder's own talent params —
/// wow-re tooltip-content-law §3 lines 2/13): the white "Rank r/m" after the name, the red
/// requirement lines while locked (position CONFIRMED — decision 0305's residue: matches the
/// builder law, after the rank line), the "Next rank:" block, and the green learn hint.
#[derive(Clone, Debug, Default)]
pub(super) struct TalentLines {
    pub rank_line: String,
    pub reqs: Vec<String>,
    /// The next rank's spell id (0 = none) — asked from the spell store when its description
    /// hasn't landed yet, so the hover's re-enter completes the block.
    pub next_spell: u32,
    pub next_desc: Option<String>,
    pub learn: bool,
}

/// `TOOLTIP_TALENT_LEARN`'s green — the shared talent-learn green of the tooltip color table
/// (byte-verified `0xff00ff00`, wow-re tooltip-content-law).
const GREEN: [f32; 4] = [0.0, 1.0, 0.0, 1.0];
/// The unmet-requirement red — `0xc0d390 = ffff2020` (the item builder's own RED value).
const RED: [f32; 4] = [1.0, 32.0 / 255.0, 32.0 / 255.0, 1.0];

/// Render one spell view — the verified law (module doc). `aura` renders the aura variant:
/// white description, plus the caller-supplied duration-remaining line (`SetPlayerBuff` only).
/// `talent` interleaves the talent lines ([`TalentLines`] doc).
fn render_spell(
    lua: &Lua,
    this: &Table,
    v: &SpellTooltipView,
    aura: bool,
    show_rank: bool,
    remaining: Option<String>,
    talent: Option<&TalentLines>,
) -> mlua::Result<()> {
    // The rank column shows only when the CALLER asks (byte-verified: SetSpell passes
    // param6=0 — the spellbook hover never shows "Rank N"; SetAction passes 1).
    match v.rank.as_ref().filter(|_| show_rank) {
        Some(rank) => append_line(
            lua,
            this,
            (v.name.clone(), WHITE),
            Some((rank.clone(), GRAY)),
            false,
        )?,
        None => append_line(lua, this, (v.name.clone(), WHITE), None, false)?,
    }
    // The talent head: "Rank r/m" (builder line 2, TOOLTIP_TALENT_RANK white) + the red
    // requirement lines while locked (position CONFIRMED, decision 0305 — TalentLines doc).
    if let Some(t) = talent {
        append_line(lua, this, (t.rank_line.clone(), WHITE), None, false)?;
        for req in &t.reqs {
            append_line(lua, this, (req.clone(), RED), None, true)?;
        }
    }
    if !aura {
        // Cost | Range — one line, either side optional.
        match (&v.cost, &v.range) {
            (Some(c), Some(r)) => append_line(
                lua,
                this,
                (c.clone(), WHITE),
                Some((r.clone(), WHITE)),
                false,
            )?,
            (Some(c), None) => append_line(lua, this, (c.clone(), WHITE), None, false)?,
            (None, Some(r)) => append_line(lua, this, (r.clone(), WHITE), None, false)?,
            (None, None) => {}
        }
        // CastTime | Cooldown — one line; a passive spell (cast_time None) omits it whole.
        if let Some(ct) = &v.cast_time {
            match &v.cooldown {
                Some(cd) => append_line(
                    lua,
                    this,
                    (ct.clone(), WHITE),
                    Some((cd.clone(), WHITE)),
                    false,
                )?,
                None => append_line(lua, this, (ct.clone(), WHITE), None, false)?,
            }
        }
        // Required form (law §3.6, SPELL_REQUIRED_FORM): white when met, red when not.
        if let Some(req) = &v.requires_form {
            let color = if v.form_met { WHITE } else { RED };
            append_line(lua, this, (req.clone(), color), None, false)?;
        }
    }
    let desc = if aura && !v.aura_description.is_empty() {
        &v.aura_description
    } else {
        &v.description
    };
    if !desc.is_empty() {
        let color = if aura { WHITE } else { GOLD };
        append_line(lua, this, (desc.clone(), color), None, true)?;
    }
    // The talent tail: the "Next rank:" block (TOOLTIP_TALENT_NEXT_RANK white + the next rank's
    // gold description) and the green learn hint (builder line 13, TOOLTIP_TALENT_LEARN).
    if let Some(t) = talent {
        if let Some(next) = &t.next_desc {
            append_line(lua, this, ("Next rank:".to_string(), WHITE), None, false)?;
            append_line(lua, this, (next.clone(), GOLD), None, true)?;
        }
        if t.learn {
            append_line(
                lua,
                this,
                ("Click to learn".to_string(), GREEN),
                None,
                false,
            )?;
        }
    }
    if let Some(rem) = remaining {
        append_line(lua, this, (rem, WHITE), None, false)?;
    }
    Ok(())
}

/// The talent tooltip's entry (decision 0304 — `GameTooltip:SetTalent`'s render half): the
/// display spell through the shared store + the talent interleave. A missing next-rank view is
/// re-asked so the hover's re-enter completes the block.
pub(super) fn set_spell_with_talent(
    lua: &Lua,
    this: &Table,
    spell_id: u32,
    talent: TalentLines,
) -> mlua::Result<()> {
    let h = frame_handle_of(lua, this)?;
    {
        let mut model = lua.app_data_mut::<Model>().expect("model app_data");
        clear_content(&mut model, h);
    }
    fire_cleared(lua, h);
    if talent.next_desc.is_none() {
        super::talent::ask_next_rank(lua, talent.next_spell);
    }
    match spell_view_of(lua, spell_id) {
        Some(v) => render_spell(lua, this, &v, false, false, None, Some(&talent))?,
        None => {
            // The view hasn't landed: show the talent head alone (the ask is recorded; the
            // hover's re-enter repaints complete) — the spell channel's own fallback shape.
            append_line(lua, this, (talent.rank_line.clone(), WHITE), None, false)?;
        }
    }
    super::tooltip::show_or_hide_empty(lua, h);
    Ok(())
}

/// Shared entry: clear, render (or record the ask and show nothing but the name if the caller
/// knows one), show.
fn set_spell_by_id(
    lua: &Lua,
    this: &Table,
    spell_id: u32,
    fallback_name: Option<String>,
    aura: bool,
    show_rank: bool,
    remaining: Option<String>,
) -> mlua::Result<()> {
    let h = frame_handle_of(lua, this)?;
    {
        let mut model = lua.app_data_mut::<Model>().expect("model app_data");
        clear_content(&mut model, h);
    }
    fire_cleared(lua, h);
    match spell_view_of(lua, spell_id) {
        Some(v) => render_spell(lua, this, &v, aura, show_rank, remaining, None)?,
        None => {
            if let Some(name) = fallback_name {
                append_line(lua, this, (name, WHITE), None, false)?;
            }
        }
    }
    super::tooltip::show_or_hide_empty(lua, h);
    Ok(())
}

/// Register the spell/aura content channels into the GameTooltip kind method table.
pub(super) fn install_methods(lua: &Lua, m: &Table) -> mlua::Result<()> {
    // GameTooltip:SetSpell(bookId, bookType) — the spellbook hover: the 1-based flat book slot
    // resolves through the spellbook state to a spell id (the era signature; bookType is always
    // the player book here — pets are a later arc).
    m.set(
        "SetSpell",
        lua.create_function(|lua, (this, book_id, _book_type): (Table, usize, Value)| {
            let (spell_id, name) = {
                let model = lua.app_data_mut::<Model>().expect("model app_data");
                match model.spellbook.slots.get(book_id.saturating_sub(1)) {
                    Some(s) => (s.spell_id, Some(s.name.clone())),
                    None => return Ok(()),
                }
            };
            set_spell_by_id(lua, &this, spell_id, name, false, false, None)
        })?,
    )?;
    // GameTooltip:SetShapeshift(index) — the stance-bar hover (the form's own spell tooltip).
    m.set(
        "SetShapeshift",
        lua.create_function(|lua, (this, index): (Table, usize)| {
            let (spell_id, name) = {
                let model = lua.app_data_mut::<Model>().expect("model app_data");
                match model.shapeshift_forms.get(index.saturating_sub(1)) {
                    Some(f) => (f.view.spell_id, Some(f.view.name.clone())),
                    None => return Ok(()),
                }
            };
            set_spell_by_id(lua, &this, spell_id, name, false, false, None)
        })?,
    )?;
    // GameTooltip:SetPlayerBuff(index [, filter]) — the buff-bar hover: the aura variant (white
    // AuraDescription) + the duration-remaining line only this entry point appends
    // (byte-verified; remaining computed live off the aura's GetTime expiry — the TEXT shape is
    // INTERIM until a live capture pins SPELL_TIME_REMAINING's exact number formatting). The
    // index counts within the FILTERED list (HELPFUL default / HARMFUL), the same convention
    // the UnitBuff/UnitDebuff bindings and the buff buttons use.
    m.set(
        "SetPlayerBuff",
        lua.create_function(|lua, (this, index, filter): (Table, i64, Option<String>)| {
            let now = {
                let g = lua.globals();
                g.get::<f64>("__benilla_now").unwrap_or(0.0)
            };
            let helpful = !filter
                .as_deref()
                .unwrap_or("")
                .split('|')
                .any(|s| s.trim().eq_ignore_ascii_case("HARMFUL"));
            let (spell_id, name, remaining) = {
                let model = lua.app_data_mut::<Model>().expect("model app_data");
                let idx = usize::try_from(index.max(1) - 1).unwrap_or(0);
                let hit = model
                    .auras
                    .get("player")
                    .and_then(|a| a.iter().filter(|a| a.helpful == helpful).nth(idx));
                match hit {
                    Some(a) => {
                        let left = a.expiration_time - now;
                        let rem = (a.duration > 0.0 && left > 0.0).then(|| {
                            if left >= 60.0 {
                                format!("{} minutes remaining", (left / 60.0).ceil() as i64)
                            } else {
                                format!("{} seconds remaining", left.ceil() as i64)
                            }
                        });
                        (a.spell_id, a.name.clone(), rem)
                    }
                    None => return Ok(()),
                }
            };
            set_spell_by_id(lua, &this, spell_id, name, true, false, remaining)
        })?,
    )?;
    // GameTooltip:SetUnitBuff(unit, index) / SetUnitDebuff(unit, index) — the target frame's aura
    // hover: the same aura variant (white AuraDescription), WITHOUT the duration-remaining line —
    // byte-verified, only SetPlayerBuff appends it (and no other unit has a duration on the 1.12
    // wire anyway). The index counts within the sign-filtered list, the UnitBuff/UnitDebuff
    // convention.
    for (verb, helpful) in [("SetUnitBuff", true), ("SetUnitDebuff", false)] {
        m.set(
            verb,
            lua.create_function(move |lua, (this, token, index): (Table, String, i64)| {
                let hit = {
                    let model = lua.app_data_mut::<Model>().expect("model app_data");
                    let idx = usize::try_from(index.max(1) - 1).unwrap_or(0);
                    model
                        .auras
                        .get(&token)
                        .and_then(|a| a.iter().filter(|a| a.helpful == helpful).nth(idx))
                        .map(|a| (a.spell_id, a.name.clone()))
                };
                // A miss (index past the list, unknown token) still routes through the shared
                // entry with spell id 0: content clears and the empty plate hides — never a
                // stale previous tooltip left showing. Id 0 records no ask.
                let (spell_id, name) = hit.unwrap_or((0, None));
                set_spell_by_id(lua, &this, spell_id, name, true, false, None)
            })?,
        )?;
    }
    // GameTooltip:SetTrackingSpell() — the minimap tracking icon's hover. Its shape differs from
    // the buff-bar hover: the NAME line is GOLD, the (aura-)description white — pinned by the
    // director's reference A/B (2026-07-20: "Find Minerals" gold over white "Finding Minerals.",
    // vs SetPlayerBuff's white name). `0x532c50`'s body isn't carved; carve it in wow-re before
    // extending this shape. No duration-remaining line (only SetPlayerBuff appends one), and no
    // tracking active clears + hides, like the SetUnitBuff miss path.
    m.set(
        "SetTrackingSpell",
        lua.create_function(|lua, this: Table| {
            let hit = {
                let model = lua.app_data_mut::<Model>().expect("model app_data");
                model
                    .tracking
                    .as_ref()
                    .map(|t| (t.spell_id, t.name.clone()))
            };
            let (spell_id, fallback_name) = hit.unwrap_or((0, None));
            let h = frame_handle_of(lua, &this)?;
            {
                let mut model = lua.app_data_mut::<Model>().expect("model app_data");
                clear_content(&mut model, h);
            }
            fire_cleared(lua, h);
            match spell_view_of(lua, spell_id) {
                Some(v) => {
                    append_line(lua, &this, (v.name.clone(), GOLD), None, false)?;
                    let desc = if !v.aura_description.is_empty() {
                        &v.aura_description
                    } else {
                        &v.description
                    };
                    if !desc.is_empty() {
                        append_line(lua, &this, (desc.clone(), WHITE), None, true)?;
                    }
                }
                None => {
                    // The view hasn't landed (ask recorded; the re-enter repaints): the name
                    // alone, in the same gold.
                    if let Some(name) = fallback_name {
                        append_line(lua, &this, (name, GOLD), None, false)?;
                    }
                }
            }
            super::tooltip::show_or_hide_empty(lua, h);
            Ok(())
        })?,
    )?;
    // GameTooltip:SetAction(slot) — the action-bar hover: pure delegation by payload kind
    // (byte-verified: SPELL 0x00 → the spell builder, ITEM 0x80 → the item builder, MACRO 0x40 —
    // no macro system yet, no tooltip).
    m.set(
        "SetAction",
        lua.create_function(|lua, (this, slot): (Table, u32)| {
            let action = {
                let model = lua.app_data_mut::<Model>().expect("model app_data");
                model.actions.get(&slot).cloned()
            };
            let Some(a) = action else { return Ok(()) };
            match a.kind {
                0x00 => set_spell_by_id(lua, &this, a.action, None, false, true, None),
                0x80 => {
                    // Route through the shared item renderer (the id-keyed entry).
                    let f: mlua::Function = this.get("SetItemById")?;
                    f.call::<()>((this.clone(), a.action))
                }
                _ => Ok(()),
            }
        })?,
    )?;
    Ok(())
}
