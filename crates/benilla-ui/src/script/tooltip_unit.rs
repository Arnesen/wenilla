//! The engine **unit tooltip builder** (decision 0274 P3) — the byte-verified line law of
//! `0x529fe0` (wow-re `ui/scratch/tooltip-content-law.md`, the 0276 fold-back):
//!
//! - NAME (gold — FrameXML recolors `TextLeft1` by reaction on `UPDATE_MOUSEOVER_UNIT`, exactly
//!   like the reference's `GameTooltip_UnitColor`; the guild line is likewise FrameXML's and
//!   joins when guild data streams);
//! - the creature SUBTITLE ("Stable Master") — white;
//! - the LEVEL line, composed from three slots over the four `TOOLTIP_UNIT_LEVEL*` templates:
//!   level text (`"??"` for a world boss, a much-higher hostile, or level ≤ 0 — the hostile
//!   delta is INTERIM at +10 pending a byte pin of the comparison), the class slot (the creature
//!   TYPE word for hostile/neutral creatures, `"Race Class"` for players, `"Corpse"` when dead),
//!   and the type slot (the rank word `{"", Elite, Elite, Boss, ""}`; `"Player"` for players);
//! - the FACTION NAME ("Stormwind", white) — the builder-tail block `0x52a7a0..` the law's §2
//!   order originally omitted: the app resolves it (`faction_name`), every gate applied;
//! - "PvP" (white) · "Skinnable" (**red**) · "Civilian" (green, `0x612550`: PvP-flagged +
//!   query-civilian + HOSTILE + GREY/trivial — the dishonorable-kill warning) · "Leader"
//!   (white, `0x6125c0`: PvP-flagged + query racial_leader);
//! - health on the attached status bar (`<name>StatusBar`, the template child below the plate),
//!   refreshed by every `set_unit` push for the live token — the byte law's HEALTH watcher.
//!
//! The world mouseover drives this through [`super::UiScript::world_tooltip_unit`] (the engine
//! shows the tooltip at the default anchor by firing `OnTooltipSetDefaultAnchor`, renders, and
//! fires `UPDATE_MOUSEOVER_UNIT` for the Lua recolor) and [`super::UiScript::world_tooltip_fade`]
//! (the byte law ARMS a fade on hover loss, not an instant hide). Unit-frame hovers call the
//! same `SetUnit` from Lua.

use mlua::{Lua, Table};

use super::object::frame_handle_of;
use super::tooltip::{append_line, clear_content, fire_cleared, show_or_hide_empty};
// The grey band + trivial/GREY check (`0x5f0700`, the CIVILIAN line's last gate: a green-or-
// better con never warns of a dishonorable kill) and the "??" gate live in one shared home
// (`unit.rs`), alongside `UnitLevel`'s −1 return and the `GetQuestGreenRange` binding.
use super::unit::{is_civilian_kill, level_reads_unknown};
use super::{KindState, Model, UnitState};
use crate::layout::{Anchor, Point};
use crate::widget::FrameHandle;

const WHITE: [f32; 4] = [1.0, 1.0, 1.0, 1.0];
const GREEN: [f32; 4] = [0.0, 1.0, 0.0, 1.0];
const RED: [f32; 4] = [1.0, 32.0 / 255.0, 32.0 / 255.0, 1.0];
/// `0xc0d420` = `0xff40c040` — the satisfiable-lock green (see [`TooltipTint::LockOpen`]).
const LOCK_OPEN: [f32; 4] = [64.0 / 255.0, 192.0 / 255.0, 64.0 / 255.0, 1.0];
/// Unit names render gold (byte-verified `0xffffd200`); FrameXML recolors line 1 by reaction.
const GOLD: [f32; 4] = [1.0, 210.0 / 255.0, 0.0, 1.0];

/// A GameObject tooltip line's colour, as the builder `0x52aa20` picks it (decision 0756). The
/// app half decides *which* line gets which tint from the lock law; this enum is only the
/// crate-boundary spelling, so the colour constants stay private to the UI engine.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TooltipTint {
    /// `0xc0cf60` — the requirement line's normal colour (the key-item "Requires %s").
    White,
    /// The "Locked" line's UNMET colour `0xc0d3a8` = `0xffff2020`, and the unknown-skill
    /// "Requires %s" (whose own red is `0xffff0000` — a difference of 32/255 in G and B that no
    /// eye resolves, so one slot serves both).
    Red,
    /// The "Locked" line when the lock **is** satisfiable — `0xc0d420` = `0xff40c040`
    /// (64,192,64). Deliberately NOT the tooltip's ordinary green `0xc0d3ac` = `0xff00ff00`: this
    /// is the second rung of the skill-difficulty ramp `0x529fa0` (grey/green/yellow/orange/red,
    /// static-init writers at `0x5290d0` &c.), which the lock line borrows wholesale, and it is a
    /// visibly softer green than the one every other tooltip line uses.
    LockOpen,
}

/// The rank word table — byte-verified `0x854158[]`: rare-elite prints ELITE, rare prints
/// nothing (there is no distinct "Rare Elite" word in 1.12's builder).
fn rank_word(rank: u32) -> Option<&'static str> {
    match rank {
        1 | 2 => Some("Elite"),
        3 => Some("Boss"),
        _ => None,
    }
}

/// The level line — three slots over the four `TOOLTIP_UNIT_LEVEL*` templates ("Level %s" /
/// "Level %s %s" / "Level %s (%s)" / "Level %s %s (%s)", the extracted enUS strings).
fn level_line(u: &UnitState, player_level: u32) -> String {
    // The "??" gate, byte-pinned (0x529fe0 §2-LEVEL): much-higher HOSTILE — internal reaction
    // ≤ 1 = hated/hostile = UnitReaction ≤ 2 on our 1..8 API scale (the UnitIsEnemy mapping) —
    // with playerLevel ≤ targetLevel−10; OR WorldBoss; OR level ≤ 0. Players NEVER read "??".
    // Shared with `UnitLevel`'s −1 return, so the tooltip and the target frame can never
    // disagree on who reads "??" ([`level_reads_unknown`]).
    let level_text = if level_reads_unknown(u, player_level) {
        "??".to_string()
    } else {
        u.level.to_string()
    };
    // The class slot: "Race Class" for players; the creature TYPE word for hostile/neutral
    // creatures (a friendly creature shows none — the byte law's hostile/neutral-only gate);
    // "Corpse" when dead.
    let class_slot = if u.dead {
        Some("Corpse".to_string())
    } else if u.is_player {
        match (&u.race, &u.class) {
            (Some(r), Some(c)) => Some(format!("{r} {c}")),
            (Some(r), None) => Some(r.clone()),
            (None, Some(c)) => Some(c.clone()),
            (None, None) => None,
        }
    } else if u.reaction != 0 && u.reaction <= 4 {
        u.creature_type_name.clone()
    } else {
        None
    };
    // The type slot: the rank word, or "Player" for players.
    let type_slot = if u.is_player {
        Some("Player".to_string())
    } else {
        rank_word(u.rank).map(str::to_string)
    };
    match (class_slot, type_slot) {
        (Some(c), Some(t)) => format!("Level {level_text} {c} ({t})"),
        (Some(c), None) => format!("Level {level_text} {c}"),
        (None, Some(t)) => format!("Level {level_text} ({t})"),
        (None, None) => format!("Level {level_text}"),
    }
}

/// Render the unit tooltip for `token`'s current snapshot; returns whether the unit existed.
fn render_unit(lua: &Lua, this: &Table, token: &str) -> mlua::Result<bool> {
    let h = frame_handle_of(lua, this)?;
    let (unit, player_level) = {
        let model = lua.app_data_mut::<Model>().expect("model app_data");
        (
            model.unit(token).cloned().filter(|u| u.exists),
            model.player_req.level,
        )
    };
    {
        let mut model = lua.app_data_mut::<Model>().expect("model app_data");
        clear_content(&mut model, h);
    }
    fire_cleared(lua, h);
    let Some(u) = unit else {
        // No unit: drop the live token and leave the tooltip hidden-empty.
        set_live_token(lua, h, None);
        show_or_hide_empty(lua, h);
        return Ok(false);
    };
    append_line(
        lua,
        this,
        (u.name.clone().unwrap_or_default(), GOLD),
        None,
        false,
    )?;
    if let Some(sub) = &u.subtitle {
        append_line(lua, this, (sub.clone(), WHITE), None, false)?;
    }
    append_line(
        lua,
        this,
        (level_line(&u, player_level), WHITE),
        None,
        false,
    )?;
    // The faction-name line ("Stormwind", white) sits between the level line and "PvP" — the
    // builder-tail block at `0x52a7a0`. The app resolved every gate into `faction_name`.
    if let Some(faction) = &u.faction_name {
        append_line(lua, this, (faction.clone(), WHITE), None, false)?;
    }
    if u.pvp {
        append_line(lua, this, ("PvP".into(), WHITE), None, false)?;
    }
    if u.skinnable {
        append_line(lua, this, ("Skinnable".into(), RED), None, false)?;
    }
    // CIVILIAN (green) — `0x612550`, whole: the unit's PvP bit + the query civilian flag + the
    // unit is HOSTILE to the player (UnitReaction ≤ 2 — internal reaction < 2) + the kill would
    // be GREY/trivial. PVP_RANK_CIVILIAN = "Civilian" (extracted GlobalStrings). The predicate
    // itself lives in `unit` because `UnitPVPName` gates its own civilian arm on the same call.
    if is_civilian_kill(&u, player_level) {
        append_line(lua, this, ("Civilian".into(), GREEN), None, false)?;
    }
    // LEADER (white) — `0x6125c0`: the PvP bit + the query racial_leader flag, no other gate.
    // PVP_RANK_LEADER = "Leader" (extracted GlobalStrings).
    if u.racial_leader && u.pvp {
        append_line(lua, this, ("Leader".into(), WHITE), None, false)?;
    }
    set_live_token(lua, h, Some(token.to_string()));
    update_bar(lua, h, Some(&u));
    show_or_hide_empty(lua, h);
    Ok(true)
}

/// Mark the tooltip as world-hover-owned (the fade-on-loss gate).
fn set_world_owned(lua: &Lua, h: FrameHandle) {
    let mut model = lua.app_data_mut::<Model>().expect("model app_data");
    if let Some(f) = model.arena.frame_mut(h) {
        if let KindState::Tooltip(t) = &mut f.kind_state {
            t.world_owned = true;
        }
    }
}

/// Remember (or drop) the tooltip's live unit token — the health watcher's key.
fn set_live_token(lua: &Lua, h: FrameHandle, token: Option<String>) {
    let mut model = lua.app_data_mut::<Model>().expect("model app_data");
    if let Some(f) = model.arena.frame_mut(h) {
        if let KindState::Tooltip(t) = &mut f.kind_state {
            t.unit_token = token;
        }
    }
}

/// Drive the `<name>StatusBar` child from a unit snapshot (`None` hides it) — the engine pushes
/// health, FrameXML's `HealthBar_OnValueChanged` colors the fill.
fn update_bar(lua: &Lua, h: FrameHandle, unit: Option<&UnitState>) {
    let bar = {
        let model = lua.app_data_mut::<Model>().expect("model app_data");
        let name = model.arena.frame(h).and_then(|f| f.name.clone());
        name.and_then(|n| model.arena.lookup(&format!("{n}StatusBar")))
    };
    let Some(bar) = bar else { return };
    let (changed, shown) = {
        let mut model = lua.app_data_mut::<Model>().expect("model app_data");
        let Some(f) = model.arena.frame_mut(bar) else {
            return;
        };
        match (&mut f.kind_state, unit) {
            (KindState::StatusBar(sb), Some(u)) => {
                sb.min = 0.0;
                sb.max = u.max_health.max(1) as f32;
                let v = u.health.min(u.max_health.max(1)) as f32;
                let changed = (sb.value - v).abs() > f32::EPSILON;
                sb.value = v;
                (changed, true)
            }
            _ => (false, false),
        }
    };
    {
        let mut model = lua.app_data_mut::<Model>().expect("model app_data");
        model.arena.set_shown(bar, shown);
    }
    if changed {
        let (id, value) = {
            let mut model = lua.app_data_mut::<Model>().expect("model app_data");
            let v = match model.arena.frame(bar).map(|f| &f.kind_state) {
                Some(KindState::StatusBar(sb)) => sb.value,
                _ => 0.0,
            };
            (model.frame_id(bar), v)
        };
        if let Err(e) = super::event::fire_widget_handler(
            lua,
            id,
            "OnValueChanged",
            vec![mlua::Value::Number(f64::from(value))],
        ) {
            lua.app_data_mut::<Model>()
                .expect("model app_data")
                .errors
                .push(e.to_string());
        }
    }
}

/// The `set_unit` push hook: a fresh snapshot for the tooltip's LIVE token re-drives the health
/// bar (the byte law's HEALTH UpdateField watcher — the bar tracks without a line rebuild).
pub(super) fn on_unit_push(lua: &Lua, token: &str) {
    let hits: Vec<FrameHandle> = {
        let model = lua.app_data_mut::<Model>().expect("model app_data");
        model
            .frame_to_id
            .keys()
            .copied()
            .filter(|&h| {
                matches!(
                    model.arena.frame(h).map(|f| &f.kind_state),
                    Some(KindState::Tooltip(t)) if t.unit_token.as_deref() == Some(token)
                )
            })
            .collect()
    };
    for h in hits {
        let unit = {
            let model = lua.app_data_mut::<Model>().expect("model app_data");
            model.unit(token).cloned().filter(|u| u.exists)
        };
        update_bar(lua, h, unit.as_ref());
    }
}

impl super::UiScript {
    /// Show the world-mouseover unit tooltip (the engine-driven flow the byte law describes):
    /// fire `OnTooltipSetDefaultAnchor` (FrameXML seats the plate at the default corner), render
    /// `token`'s snapshot, fire `UPDATE_MOUSEOVER_UNIT` (FrameXML recolors the name line by
    /// reaction). Call on hover-target CHANGE — the health bar tracks pushes in between; a
    /// re-show mid-fade resurrects at full alpha. `false` = no tooltip frame / no such unit.
    pub fn world_tooltip_unit(&mut self, token: &str) -> bool {
        let h = {
            let mut model = self.model_mut();
            let Some(h) = model.arena.lookup("GameTooltip") else {
                return false;
            };
            let id = model.frame_id(h);
            (h, id)
        };
        let (h, id) = h;
        if let Err(e) =
            super::event::fire_widget_handler(&self.lua, id, "OnTooltipSetDefaultAnchor", vec![])
        {
            self.push_error(e);
        }
        let wrapper = match super::object::frame_wrapper(&self.lua, id) {
            Ok(w) => w,
            Err(e) => {
                self.push_error(e);
                return false;
            }
        };
        let shown = match render_unit(&self.lua, &wrapper, token) {
            Ok(s) => s,
            Err(e) => {
                self.push_error(e);
                false
            }
        };
        if shown {
            set_world_owned(&self.lua, h);
            // Cancel any running fade at full alpha (a re-hover resurrects).
            let mut model = self.model_mut();
            if let Some(f) = model.arena.frame_mut(h) {
                if let KindState::Tooltip(t) = &mut f.kind_state {
                    if t.fade_start.take().is_some() {
                        model.arena.set_alpha(h, 1.0);
                    }
                }
            }
            drop(model);
            self.fire_event("UPDATE_MOUSEOVER_UNIT", vec![]);
        }
        shown
    }

    /// Show the world-mouseover GAMEOBJECT tooltip — the byte-verified GO builder `0x52aa20`
    /// (decisions 0276 / **0756**): the NAME (gold) followed by the lock lines the caller
    /// resolved, each with its own tint.
    ///
    /// **The anchor FORKS per object — it is not uniform** (decision 0766, correcting 0756). The
    /// publisher's GameObject leg calls the picked object's `[obj->vtbl+0x5c]` and branches:
    ///
    /// - **true** (`0x492a01`) → `0x52ffe0(owner, 6, 0, 0)` — anchor-state **6**, the *cursor*
    ///   anchor. Corroborated by the loss path: `0x530ae0` hides an anchor-state-6 tooltip
    ///   **immediately** instead of arming the fade, which is how a pointer-following label behaves.
    /// - **false** (`0x492a42`) → no `SetOwner` at all; the plate keeps its default corner seat.
    ///
    /// The **unit** leg (`0x492983`) never consults `+0x5c` — units are always corner-seated, which
    /// is why the fork went unnoticed when 0756 made every GameObject corner-seated too. `cursor`
    /// carries the caller's verdict: `Some(ui_xy)` = the cursor arm (and then
    /// [`Self::world_tooltip_move`] follows the pointer), `None` = the corner.
    ///
    /// Same fade lifecycle as the unit flow (no `UPDATE_MOUSEOVER_UNIT` — that recolor is the
    /// unit line's).
    pub fn world_tooltip_gameobject(
        &mut self,
        name: &str,
        lines: &[(String, TooltipTint)],
        cursor: Option<(f32, f32)>,
    ) -> bool {
        let (h, id, root_id) = {
            let mut model = self.model_mut();
            let Some(h) = model.arena.lookup("GameTooltip") else {
                return false;
            };
            let Some(root) = model.arena.lookup("UIParent") else {
                return false;
            };
            let (id, root_id) = (model.frame_id(h), model.frame_id(root));
            (h, id, root_id)
        };
        match cursor {
            // The cursor arm: seated centred above the pointer, clamped by the tooltip frame's own
            // flag. Compare-then-touch so a still pointer never re-layouts.
            Some((ui_x, ui_y)) => {
                let mut model = self.model_mut();
                let input = model.layout_inputs.entry(h).or_default();
                let new = Anchor::new(Point::Bottom, root_id, Point::BottomLeft, ui_x, ui_y);
                let same = input.anchors.len() == 1
                    && super::object::anchor_bits_eq(&input.anchors[0], &new);
                if !same {
                    input.anchors = vec![new];
                    model.touch_layout();
                }
            }
            // The corner arm: the same default-anchor handler the unit flow fires.
            None => {
                if let Err(e) = super::event::fire_widget_handler(
                    &self.lua,
                    id,
                    "OnTooltipSetDefaultAnchor",
                    vec![],
                ) {
                    self.push_error(e);
                }
            }
        }
        let wrapper = match super::object::frame_wrapper(&self.lua, id) {
            Ok(w) => w,
            Err(e) => {
                self.push_error(e);
                return false;
            }
        };
        let render = || -> mlua::Result<()> {
            {
                let mut model = self.lua.app_data_mut::<Model>().expect("model app_data");
                clear_content(&mut model, h);
            }
            fire_cleared(&self.lua, h);
            append_line(&self.lua, &wrapper, (name.to_string(), GOLD), None, false)?;
            for (text, tint) in lines {
                let colour = match tint {
                    TooltipTint::White => WHITE,
                    TooltipTint::Red => RED,
                    TooltipTint::LockOpen => LOCK_OPEN,
                };
                append_line(&self.lua, &wrapper, (text.clone(), colour), None, false)?;
            }
            Ok(())
        };
        if let Err(e) = render() {
            self.push_error(e);
            return false;
        }
        set_world_owned(&self.lua, h);
        {
            let mut model = self.model_mut();
            if let Some(f) = model.arena.frame_mut(h) {
                if let KindState::Tooltip(t) = &mut f.kind_state {
                    if t.fade_start.take().is_some() {
                        model.arena.set_alpha(h, 1.0);
                    }
                }
            }
        }
        show_or_hide_empty(&self.lua, h);
        true
    }

    /// Show the minimap BLIP tooltip — a landmark's `AreaPOI` name or a quest-dot NPC's name,
    /// one GOLD line (the reference's engine SetText gold; a cross-interior dot renders FAINT
    /// gold — the byte law's `|cffb0b0b0` wrap modulating the gold base, director-matched),
    /// seated centred ABOVE the cursor: the tooltip's BOTTOM at the given UI-space point. The
    /// plate FOLLOWS the pointer — [`Self::minimap_tooltip_move`] re-seats it as the cursor
    /// drifts within one blip. Same world-owned fade lifecycle as the mouseover tooltip:
    /// hover loss arms [`Self::world_tooltip_fade`].
    pub fn minimap_tooltip(&mut self, text: &str, ui_x: f32, ui_y: f32, grey: bool) -> bool {
        let (h, id, root_id) = {
            let mut model = self.model_mut();
            let Some(h) = model.arena.lookup("GameTooltip") else {
                return false;
            };
            let Some(root) = model.arena.lookup("UIParent") else {
                return false;
            };
            let (id, root_id) = (model.frame_id(h), model.frame_id(root));
            (h, id, root_id)
        };
        let wrapper = match super::object::frame_wrapper(&self.lua, id) {
            Ok(w) => w,
            Err(e) => {
                self.push_error(e);
                return false;
            }
        };
        {
            let mut model = self.model_mut();
            clear_content(&mut model, h);
            let input = model.layout_inputs.entry(h).or_default();
            // Anchor only — the screen clamp (G flags bit4: a cursor-seated plate near the window
            // edge slides back in instead of clipping, director's report at the screen-right
            // minimap) is the tooltip FRAME's own flag (`Frame::clamped_to_screen`), synced with
            // live extents at every resolve.
            let new = Anchor::new(Point::Bottom, root_id, Point::BottomLeft, ui_x, ui_y);
            // Compare-then-touch: a still cursor re-seats the plate at the same point every
            // frame — tier 1 of the layout gate stays quiet unless the pointer actually moved.
            let same =
                input.anchors.len() == 1 && super::object::anchor_bits_eq(&input.anchors[0], &new);
            if !same {
                input.anchors = vec![new];
                model.touch_layout();
            }
        }
        fire_cleared(&self.lua, h);
        // The blip name renders GOLD like the reference (the engine tooltip's SetText line —
        // the same 0xffffd200 the unit-name line uses); a cross-interior entry renders the
        // byte law's `|cffb0b0b0` wrap MODULATING that gold — faint gold, not flat grey
        // (director-matched against the reference, 2026-07-13).
        let dim = 0xb0 as f32 / 255.0;
        let color = if grey {
            [GOLD[0] * dim, GOLD[1] * dim, GOLD[2] * dim, 1.0]
        } else {
            GOLD
        };
        if let Err(e) = append_line(&self.lua, &wrapper, (text.to_string(), color), None, false) {
            self.push_error(e);
            return false;
        }
        set_world_owned(&self.lua, h);
        {
            let mut model = self.model_mut();
            if let Some(f) = model.arena.frame_mut(h) {
                if let KindState::Tooltip(t) = &mut f.kind_state {
                    if t.fade_start.take().is_some() {
                        model.arena.set_alpha(h, 1.0);
                    }
                }
            }
        }
        show_or_hide_empty(&self.lua, h);
        true
    }

    /// Re-seat the showing cursor-anchored tooltip at a new pointer point (the follow half of
    /// [`Self::minimap_tooltip`] and [`Self::world_tooltip_gameobject`]): anchor-only — no
    /// content rebuild, no fade churn. No-op when the tooltip isn't the world-owned shown plate.
    pub fn world_tooltip_move(&mut self, ui_x: f32, ui_y: f32) {
        let mut model = self.model_mut();
        let Some(h) = model.arena.lookup("GameTooltip") else {
            return;
        };
        let Some(root) = model.arena.lookup("UIParent") else {
            return;
        };
        let owned_shown = model
            .arena
            .frame(h)
            .map(|f| matches!(&f.kind_state, KindState::Tooltip(t) if t.world_owned) && f.shown)
            .unwrap_or(false);
        if !owned_shown {
            return;
        }
        let root_id = model.frame_id(root);
        let input = model.layout_inputs.entry(h).or_default();
        // Anchor only — the frame's own clamp flag keeps the plate on screen (see
        // `Frame::clamped_to_screen`).
        let new = Anchor::new(Point::Bottom, root_id, Point::BottomLeft, ui_x, ui_y);
        // Compare-then-touch, as in the seat/blip paths above: the follow fires per pointer
        // event, and a still cursor must not dirty the layout gate's tier 1.
        let same =
            input.anchors.len() == 1 && super::object::anchor_bits_eq(&input.anchors[0], &new);
        if !same {
            input.anchors = vec![new];
            model.touch_layout();
        }
    }

    /// Arm the mouseover tooltip's fade-out (hover loss — the byte law arms a timestamped fade,
    /// never an instant hide). No-op when the tooltip isn't showing a world unit.
    pub fn world_tooltip_fade(&mut self) {
        let now = self.now();
        let mut model = self.model_mut();
        let Some(h) = model.arena.lookup("GameTooltip") else {
            return;
        };
        if let Some(f) = model.arena.frame_mut(h) {
            if let KindState::Tooltip(t) = &mut f.kind_state {
                if t.world_owned && f.shown && t.fade_start.is_none() {
                    t.fade_start = Some(now);
                }
            }
        }
    }
}

/// Register the unit content channel into the GameTooltip kind method table.
pub(super) fn install_methods(lua: &Lua, m: &Table) -> mlua::Result<()> {
    // GameTooltip:SetUnit(token) → 1/nil — the unit-frame hover (ref UnitFrame_OnEnter reads
    // the return to arm its re-poll).
    m.set(
        "SetUnit",
        lua.create_function(|lua, (this, token): (Table, String)| {
            // Same gate as every `Unit*` verb: `SetUnit` resolves its argument through the client's
            // one token resolver, so an unrecognised name raises here too rather than drawing an
            // empty tooltip. NOT inside `render_unit` — the app's own push calls that with a
            // canonical token and must never be gated against itself.
            crate::script::unit::check_unit_token(&Some(token.clone()))?;
            let ok = render_unit(lua, &this, &token)?;
            Ok(if ok {
                mlua::Value::Integer(1)
            } else {
                mlua::Value::Nil
            })
        })?,
    )?;
    Ok(())
}
