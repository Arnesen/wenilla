//! The pet action bar seam (decision 0982) — the eight bindings `PetActionBarFrame.lua` consumes
//! (`GetPetActionInfo`/`GetPetActionsUsable`/`GetPetActionCooldown`/`PetHasActionBar`/
//! `CastPetAction`/`TogglePetAutocast`/`IsPetAttackActive`/`PetStopAttack`) over an app-pushed
//! slot snapshot, in [`super::shapeshift`]'s two-way shape: the app resolves everything (which
//! slot is a command, a reaction or a spell; its icon, name, checked and autocast bits; its
//! cooldown) and pushes it ([`super::UiScript::set_pet_actions`]); the engine drains the click
//! intents ([`super::UiScript::take_pet_actions`] and kin) back out.
//!
//! **The engine holds no pet knowledge**: a slot here is "a name, a subtext, a texture, four bits
//! and a cooldown triple". In particular it does not know that a token slot's `name`/`texture` are
//! *the names of globals* rather than values — that convention belongs to the reference's own
//! `GetPetActionInfo` and is reproduced faithfully by the app, which sets `is_token` and lets the
//! Lua do the `getglobal` (the shipped `PetActionBarFrame.lua:98-104` fork).
//!
//! Return conventions are the 1.12 API's own, matching [`super::action`]: 1/nil booleans, and the
//! cooldown as `(start_s on the GetTime clock, duration_s, enable)` with the same
//! elapsed-goes-cold rule `GetActionCooldown` uses.

use mlua::{Lua, MultiValue, Value};

use super::Model;

/// One pet bar slot, fully resolved by the app before pushing.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct PetActionView {
    /// `GetPetActionInfo`'s first return, and the slot's OCCUPANCY test — `None` hides the button
    /// (`PetActionBarFrame.lua:122-128`). For a spell slot this is the spell's name; for a token
    /// slot it is **the name of a global** (`"PET_ACTION_ATTACK"`), which the Lua resolves.
    pub name: Option<String>,
    /// The second return — the spell's rank line, or `None` for a token.
    pub subtext: Option<String>,
    /// The third return: an icon path for a spell, **the name of a global**
    /// (`"PET_ATTACK_TEXTURE"`) for a token. `None` leaves the button art empty and swaps its
    /// NormalTexture to the unfilled `UI-Quickslot`.
    pub texture: Option<String>,
    /// Is this a command/reaction token (so `name`/`texture` are global names)?
    pub is_token: bool,
    /// The slot's spell, when it has one — what `GameTooltip:SetPetAction` renders. `None` for a
    /// token and for an empty slot. Not a `GetPetActionInfo` return: the reference's tooltip
    /// channel reaches the pet spellbook itself, and this is that reach.
    pub spell_id: Option<u32>,
    /// The checked ring.
    pub active: bool,
    /// This slot CAN autocast — the static `UI-AutoCastableOverlay` ring.
    pub autocast_allowed: bool,
    /// …and it currently does — the sparkle trail.
    pub autocast_enabled: bool,
    /// Whether a left click on this slot means "call the pet off" rather than "do this"
    /// (`IsPetAttackActive`, the Attack button's second press).
    pub attack_active: bool,
    /// `(start_ms on the GetTime clock, duration_ms, enabled)` — [`super::action::ActionState`]'s
    /// exact shape; `None` = no cooldown.
    pub cooldown: Option<(i64, u32, bool)>,
}

/// [`PetActionView`] as stored: the cooldown converted to the `GetTime` clock at push time (the
/// [`super::shapeshift::StoredShapeshiftForm`] pattern).
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct StoredPetAction {
    pub(crate) view: PetActionView,
    /// `(start_s, duration_s, enabled)` in `GetTime` seconds; `None` = no cooldown.
    pub(crate) cooldown: Option<(f64, f64, bool)>,
}

/// The pet bar's pushed state: the slots plus the two bar-wide bits the reference exposes
/// separately from them.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct PetBarState {
    /// `PetHasActionBar()` — is there a bar at all. Distinct from "the slot list is empty": a
    /// possessed minion has a bar of pure commands, and a bar of ten empty slots is still a bar.
    pub(crate) has_bar: bool,
    /// `GetPetActionsUsable()` — false desaturates every icon on the bar at once.
    pub(crate) actions_usable: bool,
    pub(crate) slots: Vec<StoredPetAction>,
}

impl super::UiScript {
    /// Push the whole pet bar, replacing whatever was there. A bare setter — firing
    /// `PET_BAR_UPDATE` is the app's diff-and-fire job, mirroring `set_shapeshift_forms`.
    pub fn set_pet_actions(
        &mut self,
        has_bar: bool,
        actions_usable: bool,
        slots: Vec<PetActionView>,
    ) {
        let mut model = self.model_mut();
        model.pet_bar = PetBarState {
            has_bar,
            actions_usable,
            slots: slots
                .into_iter()
                .map(|view| {
                    // The cooldown arrives with its absolute start already on the `GetTime` clock
                    // (ms) — storing is a pure unit conversion, `set_shapeshift_forms`' seam.
                    let cooldown = view.cooldown.map(|(start_ms, duration_ms, enabled)| {
                        (
                            start_ms as f64 / 1000.0,
                            f64::from(duration_ms) / 1000.0,
                            enabled,
                        )
                    });
                    StoredPetAction { view, cooldown }
                })
                .collect(),
        };
    }

    /// Drain the 1-based slot indices `CastPetAction` queued since the last call. What each index
    /// *means* on the wire (a command, a reaction, a cast) is the app's to decide at drain time
    /// from the slot it still owns.
    pub fn take_pet_actions(&mut self) -> Vec<u32> {
        std::mem::take(&mut self.model_mut().pet_actions_pressed)
    }

    /// Drain the 1-based slot indices `TogglePetAutocast` queued.
    pub fn take_pet_autocast_toggles(&mut self) -> Vec<u32> {
        std::mem::take(&mut self.model_mut().pet_autocast_toggles)
    }

    /// Drain the `PetStopAttack()` calls queued (a count — the verb carries no argument).
    pub fn take_pet_stop_attacks(&mut self) -> u32 {
        std::mem::replace(&mut self.model_mut().pet_stop_attacks, 0)
    }
}

/// The 1-based button index → stored slot, the reference's own indexing.
fn slot_at(model: &Model, i: u32) -> Option<&StoredPetAction> {
    usize::try_from(i.checked_sub(1)?)
        .ok()
        .and_then(|n| model.pet_bar.slots.get(n))
}

/// Register the pet-bar globals.
pub(super) fn install(lua: &Lua) -> mlua::Result<()> {
    let g = lua.globals();

    let flag = |b: bool| if b { Value::Integer(1) } else { Value::Nil };

    // PetHasActionBar() → 1/nil. The bar frame's whole show/hide gate.
    g.set(
        "PetHasActionBar",
        lua.create_function(move |lua, ()| {
            let model = lua.app_data_ref::<Model>().expect("model app_data");
            Ok(flag(model.pet_bar.has_bar))
        })?,
    )?;

    // GetPetActionsUsable() → 1/nil — one answer for the whole bar (the SetDesaturation sweep).
    g.set(
        "GetPetActionsUsable",
        lua.create_function(move |lua, ()| {
            let model = lua.app_data_ref::<Model>().expect("model app_data");
            Ok(flag(model.pet_bar.actions_usable))
        })?,
    )?;

    // GetPetActionInfo(i) → name, subtext, texture, isToken, isActive, autoCastAllowed,
    // autoCastEnabled. An out-of-range index answers a single nil, which the Lua's `if (name)`
    // occupancy test reads exactly as an empty slot (the spellbook bindings' shape).
    g.set(
        "GetPetActionInfo",
        lua.create_function(move |lua, i: u32| {
            let model = lua.app_data_ref::<Model>().expect("model app_data");
            let Some(slot) = slot_at(&model, i) else {
                return Ok(MultiValue::from_vec(vec![Value::Nil]));
            };
            let v = &slot.view;
            let text = |s: &Option<String>| match s {
                Some(s) => Ok(Value::String(lua.create_string(s)?)),
                None => Ok::<_, mlua::Error>(Value::Nil),
            };
            Ok(MultiValue::from_vec(vec![
                text(&v.name)?,
                text(&v.subtext)?,
                text(&v.texture)?,
                flag(v.is_token),
                flag(v.active),
                flag(v.autocast_allowed),
                flag(v.autocast_enabled),
            ]))
        })?,
    )?;

    // GetPetActionCooldown(i) → start, duration, enable — GetActionCooldown's triple and its
    // elapsed-goes-cold rule (an elapsed/absent cooldown answers (0, 0, 1) so a re-feed never
    // replays the sweep).
    g.set(
        "GetPetActionCooldown",
        lua.create_function(|lua, i: u32| {
            let now: f64 = lua.globals().get("__benilla_now").unwrap_or(0.0);
            let model = lua.app_data_ref::<Model>().expect("model app_data");
            Ok(match slot_at(&model, i).and_then(|s| s.cooldown) {
                Some((start, duration, enabled)) if start + duration > now || !enabled => {
                    (start, duration, i32::from(enabled))
                }
                _ => (0.0, 0.0, 1),
            })
        })?,
    )?;

    // IsPetAttackActive(i) → a BOOLEAN — the left-click fork: true means the press should call
    // the pet OFF (`PetStopAttack`) instead of running the slot.
    //
    // The odd one out of this file's returns, and deliberately so: it is the single pet binding
    // that pushes a real Lua boolean (`0x6f39f0`) rather than the 1/nil the rest use, so it
    // answers `false`, never nil, even out of range. Consumers only ever test it for truth, so
    // the difference is invisible in use — but a seam that quietly upgraded it to the house
    // convention would be lying about the API.
    g.set(
        "IsPetAttackActive",
        lua.create_function(move |lua, i: u32| {
            let model = lua.app_data_ref::<Model>().expect("model app_data");
            Ok(slot_at(&model, i).is_some_and(|s| s.view.attack_active))
        })?,
    )?;

    // CastPetAction(i) — queue the press. An EMPTY slot queues nothing: the reference's bar hides
    // an unnamed button, so a press on one can only come from the show-grid state, where it must
    // be inert.
    g.set(
        "CastPetAction",
        lua.create_function(|lua, i: u32| {
            let mut model = lua.app_data_mut::<Model>().expect("model app_data");
            if slot_at(&model, i).is_some_and(|s| s.view.name.is_some()) {
                model.pet_actions_pressed.push(i);
            }
            Ok(())
        })?,
    )?;

    // TogglePetAutocast(i) — queue the right-click. Only a slot that CAN autocast queues: the
    // wire verb names a spell id, and a command token has none.
    g.set(
        "TogglePetAutocast",
        lua.create_function(|lua, i: u32| {
            let mut model = lua.app_data_mut::<Model>().expect("model app_data");
            if slot_at(&model, i).is_some_and(|s| s.view.autocast_allowed) {
                model.pet_autocast_toggles.push(i);
            }
            Ok(())
        })?,
    )?;

    // PetStopAttack() — queue the call-off. No argument: the wire carries only the pet's guid.
    g.set(
        "PetStopAttack",
        lua.create_function(|lua, ()| {
            let mut model = lua.app_data_mut::<Model>().expect("model app_data");
            model.pet_stop_attacks += 1;
            Ok(())
        })?,
    )?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::PetActionView;
    use crate::script::UiScript;

    /// A hunter's bar, cut down to the three slot classes that matter: the Attack command (a
    /// token, currently attacking), Claw (a spell with autocast ON and a running cooldown), and an
    /// empty middle slot.
    fn slots() -> Vec<PetActionView> {
        vec![
            PetActionView {
                name: Some("PET_ACTION_ATTACK".into()),
                texture: Some("PET_ATTACK_TEXTURE".into()),
                is_token: true,
                active: true,
                attack_active: true,
                ..Default::default()
            },
            PetActionView {
                name: Some("Claw".into()),
                subtext: Some("Rank 3".into()),
                texture: Some("Interface\\Icons\\Ability_Druid_Rake".into()),
                autocast_allowed: true,
                autocast_enabled: true,
                cooldown: Some((9400, 1500, true)),
                ..Default::default()
            },
            PetActionView::default(),
        ]
    }

    #[test]
    fn slot_info_reads_and_out_of_range_is_one_nil() {
        let mut s = UiScript::new().unwrap();
        assert!(s.eval::<bool>("return PetHasActionBar() == nil").unwrap());
        assert!(s.eval::<bool>("return GetPetActionInfo(1) == nil").unwrap());

        s.set_pet_actions(true, true, slots());
        assert_eq!(s.eval::<i64>("return PetHasActionBar()").unwrap(), 1);
        assert_eq!(s.eval::<i64>("return GetPetActionsUsable()").unwrap(), 1);

        // The token slot returns GLOBAL NAMES, and says so with isToken.
        let (name, subtext, texture, is_token, active, allowed, enabled) = s
            .eval::<(
                String,
                Option<String>,
                String,
                Option<i64>,
                Option<i64>,
                Option<i64>,
                Option<i64>,
            )>("return GetPetActionInfo(1)")
            .unwrap();
        assert_eq!(
            (
                name.as_str(),
                subtext,
                texture.as_str(),
                is_token,
                active,
                allowed,
                enabled
            ),
            (
                "PET_ACTION_ATTACK",
                None,
                "PET_ATTACK_TEXTURE",
                Some(1),
                Some(1),
                None,
                None
            )
        );

        // The spell slot returns a real name, a rank line and an icon PATH, and is not a token.
        assert!(s
            .eval::<bool>(
                "local n, sub, tex, tok, act, allow, on = GetPetActionInfo(2) \
                 return n == 'Claw' and sub == 'Rank 3' and tok == nil and act == nil \
                 and allow == 1 and on == 1 and string.find(tex, 'Icons') ~= nil"
            )
            .unwrap());

        // The empty slot exists (so it is not the out-of-range single nil) but has no name — the
        // reference's own "hide this button" test.
        assert!(s
            .eval::<bool>("local n, _, tex = GetPetActionInfo(3) return n == nil and tex == nil")
            .unwrap());
        assert!(s.eval::<bool>("return GetPetActionInfo(4) == nil").unwrap());
    }

    #[test]
    fn cooldown_triple_stamps_to_the_vm_clock_and_goes_cold() {
        let mut s = UiScript::new().unwrap();
        s.tick(10.0); // GetTime == 10
        s.set_pet_actions(true, true, slots());

        assert_eq!(
            s.eval::<(f64, f64, i32)>("return GetPetActionCooldown(1)")
                .unwrap(),
            (0.0, 0.0, 1),
            "no cooldown reads cold"
        );
        let (start, duration, enable) = s
            .eval::<(f64, f64, i32)>("return GetPetActionCooldown(2)")
            .unwrap();
        assert!((start - 9.4).abs() < 1e-9, "start {start}");
        assert!((duration - 1.5).abs() < 1e-9);
        assert_eq!(enable, 1);

        s.tick(2.0); // now == 12 > 9.4 + 1.5
        assert_eq!(
            s.eval::<(f64, f64, i32)>("return GetPetActionCooldown(2)")
                .unwrap(),
            (0.0, 0.0, 1)
        );
    }

    /// The three intent queues, and the two gates that keep a meaningless intent off the wire: an
    /// empty slot cannot be pressed, and a slot with no autocast cannot be toggled (its wire verb
    /// names a spell id, which a command token has not got).
    #[test]
    fn intents_queue_and_the_meaningless_ones_are_dropped() {
        let mut s = UiScript::new().unwrap();
        s.set_pet_actions(true, true, slots());

        s.run("CastPetAction(1) CastPetAction(3) CastPetAction(9)")
            .unwrap();
        assert_eq!(
            s.take_pet_actions(),
            vec![1],
            "empty + out-of-range dropped"
        );
        assert!(s.take_pet_actions().is_empty(), "drain empties");

        s.run("TogglePetAutocast(1) TogglePetAutocast(2)").unwrap();
        assert_eq!(
            s.take_pet_autocast_toggles(),
            vec![2],
            "only the autocastable slot"
        );

        assert_eq!(s.take_pet_stop_attacks(), 0);
        s.run("PetStopAttack() PetStopAttack()").unwrap();
        assert_eq!(s.take_pet_stop_attacks(), 2);
        assert_eq!(s.take_pet_stop_attacks(), 0, "drain empties");
    }

    /// `IsPetAttackActive` is per-slot, and it is what turns the Attack button's second press into
    /// a call-off — the reference's `PetActionButton_OnClick` fork.
    ///
    /// It answers a **boolean** on every path, including out of range — the one binding here that
    /// does not use the 1/nil convention.
    #[test]
    fn attack_active_is_a_per_slot_boolean() {
        let mut s = UiScript::new().unwrap();
        s.set_pet_actions(true, true, slots());
        assert!(s.eval::<bool>("return IsPetAttackActive(1)").unwrap());
        assert!(!s.eval::<bool>("return IsPetAttackActive(2)").unwrap());
        assert!(s
            .eval::<bool>("return IsPetAttackActive(9) == false")
            .unwrap());
    }

    /// A disabled bar still EXISTS — `PetHasActionBar` stays true while `GetPetActionsUsable`
    /// goes false. The pair is what greys every icon without taking the bar off screen.
    #[test]
    fn a_disabled_bar_is_still_a_bar() {
        let mut s = UiScript::new().unwrap();
        s.set_pet_actions(true, false, slots());
        assert_eq!(s.eval::<i64>("return PetHasActionBar()").unwrap(), 1);
        assert!(s
            .eval::<bool>("return GetPetActionsUsable() == nil")
            .unwrap());
    }
}
