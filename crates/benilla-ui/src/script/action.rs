//! The action-bar bindings (decision 0068 slice 1, extended by decision 0216 §7/slice 4) — both
//! directions of the engine seam in one place: the app pushes an **action snapshot** (what each
//! of the 120 action slots displays, [`UiScript::set_action`], same inward shape as
//! [`super::unit`]'s snapshots), and `UseAction` queues an outbound **intent** the app drains
//! into the wire ([`UiScript::take_action_uses`], same outward shape as [`super::sound`]'s
//! queue). The engine holds no spell/item/macro KNOWLEDGE — a slot is "a texture, a count, and
//! the packed `(kind, action)` halves" — but slice 4 hands it the packed halves themselves so the
//! cursor seam ([`super::cursor::bar`]) can pick a slot up and place it without round-tripping the
//! app: the ENGINE owns the payload + the local slot mutation against its own optimistic mirror
//! of `model.actions`, the APP owns the authoritative 120-table and drains the queued
//! `action_sets` onto the wire (`CMSG_SET_ACTION_BUTTON`).
//!
//! Actions are keyed by the **Lua action id** (1..120, the live API's space; the 1.12 wire's
//! 120-slot array is this minus one). `GetBonusBarOffset` reports the app-pushed stance/form
//! page offset — the vanilla main bar shows actions `(6 + offset − 1)·12 + i` when an offset is
//! active (warrior stances, druid forms), which the FrameXML side computes exactly like
//! Blizzard's own bar code.

use mlua::{Lua, MultiValue, Value};

use super::Model;

/// Action-kind bytes — bits 24–31 of the wire's packed slot word (VERIFIED vmangos `Player.h`
/// `ActionButtonType`; decision 0216 §1, `CMSG_SET_ACTION_BUTTON`'s own packing). The natural
/// home for the constant every `ActionSlot` producer/consumer needs (`super::cursor::bar`'s
/// pack/unpack included) — this crate is deliberately engine-free (no protocol dependency, see
/// the crate doc), so it can't just import `benilla_protocol::messages::ACTION_KIND_*`.
pub(crate) const ACTION_KIND_SPELL: u8 = 0x00;
pub(crate) const ACTION_KIND_ITEM: u8 = 0x80;

/// What one action slot displays. The app resolves icons (Spell.dbc × SpellIcon.dbc for a spell,
/// the item template chain for an item) before pushing.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ActionSlot {
    /// The icon texture path (`Interface\Icons\…`); `None` shows the slot's fallback.
    pub texture: Option<String>,
    /// The wire's kind byte (bits 24–31 of the packed slot word — SPELL 0x00/MACRO 0x40/ITEM
    /// 0x80, decision 0216 §1). Opaque to the engine beyond the cursor seam's own pack/unpack
    /// (`super::cursor::bar`); ids, targets, and the cast are still the app's.
    pub kind: u8,
    /// The spell/macro/item id (bits 0–23 of the packed slot word).
    pub action: u32,
    /// Bag count for an ITEM-kind slot (`GetActionCount`), `0` for every other kind or an empty
    /// bag — the app-resolved value the Count fontstring reads (decision 0216 §7).
    pub count: u32,
}

/// One action's **dynamic** state — the per-frame half the app's feed pushes beside the slot's
/// identity ([`ActionSlot`]): what the reference's `ActionButton_Update*` family reads through
/// `IsUsableAction`/`IsActionInRange`/`IsCurrentAction`/`GetActionCooldown` and kin (decision
/// 0137 phase 4). Split from the identity map so a state churn (range, cooldown) never disturbs
/// the identity diff that fires `ACTIONBAR_SLOT_CHANGED`.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct ActionState {
    /// `IsUsableAction`'s first return: castable right now (power etc.).
    pub usable: bool,
    /// `IsUsableAction`'s second return: unusable **specifically** for insufficient power (the
    /// 0.5/0.5/1.0 blue tint); other unusability reads `(false, false)` (the 0.4 grey).
    pub not_enough_mana: bool,
    /// `IsActionInRange`: `None` = rangeless action or no valid target (Lua `nil`);
    /// `Some(true/false)` = the 1/0 verdict.
    pub in_range: Option<bool>,
    /// `ActionHasRange`: the action's spell has a range row to test at all.
    pub has_range: bool,
    /// `IsCurrentAction`: the Attack action while auto-attack is engaged, or the action's spell
    /// while it is our in-flight cast (the checked ring).
    pub current: bool,
    /// `IsAutoRepeatAction`: the action's spell is the live autorepeat key (`0xceac30`).
    pub auto_repeat: bool,
    /// `IsAttackAction`: the action is the melee auto-attack.
    pub is_attack: bool,
    /// `IsConsumableAction`: shows the Count fontstring in the ref's `UpdateCount`.
    pub consumable: bool,
    /// `IsEquippedAction`: an ITEM action currently worn (the green border).
    pub equipped: bool,
    /// The action's cooldown as pushed: `(start_ms, duration_ms, enabled)` with `start_ms` the
    /// cooldown's **absolute start on the `GetTime` clock** (the app converts from its own clock
    /// at feed time — the app-side `CooldownInfo::ui_triple`'s job). Absolute so the value is
    /// identity-preserving: one running cooldown reads the same triple every frame (no diff
    /// churn), while a re-arm always reads a new one (the sweep restarts — the reference's own
    /// convention, whose `GetCooldownInfo 0x6e13e0` returns the record's start and therefore
    /// cannot alias two arms the way a `(remaining, duration)` pair did).
    pub cooldown: Option<(i64, u32, bool)>,
}

/// [`ActionState`] as stored: the cooldown converted to the `GetTime` clock at push time.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub(crate) struct StoredActionState {
    pub(crate) state: ActionState,
    /// `(start_s, duration_s, enabled)` in `GetTime` seconds; `None` = no cooldown.
    pub(crate) cooldown: Option<(f64, f64, bool)>,
}

impl super::UiScript {
    /// Push (or clear) one action slot, keyed by Lua action id (1..120).
    pub fn set_action(&mut self, action: u32, slot: Option<ActionSlot>) {
        let mut model = self.model_mut();
        match slot {
            Some(s) => {
                model.actions.insert(action, s);
            }
            None => {
                model.actions.remove(&action);
            }
        }
    }

    /// Push (or clear) one action's dynamic state. The cooldown arrives with its **absolute
    /// start already on the `GetTime` clock** (ms), so storing is a pure unit conversion — no
    /// anchor derivation, no prev-comparison. A re-push of a running cooldown carries the same
    /// start (the sweep is undisturbed — the reset-on-kill invariant holds by construction); a
    /// re-armed one carries a new start (the sweep restarts, as the reference's does).
    pub fn set_action_state(&mut self, action: u32, state: Option<ActionState>) {
        let mut model = self.model_mut();
        match state {
            Some(s) => {
                let cooldown = s.cooldown.map(|(start_ms, duration_ms, enabled)| {
                    (
                        start_ms as f64 / 1000.0,
                        f64::from(duration_ms) / 1000.0,
                        enabled,
                    )
                });
                model
                    .action_states
                    .insert(action, StoredActionState { state: s, cooldown });
            }
            None => {
                model.action_states.remove(&action);
            }
        }
    }

    /// Push the current bonus-bar page offset (0 = the plain main bar; warrior stances 1..3).
    pub fn set_bonus_bar_offset(&mut self, offset: u8) {
        self.model_mut().bonus_bar_offset = offset;
    }

    /// Drain the action ids queued by `UseAction` since the last call.
    pub fn take_action_uses(&mut self) -> Vec<u32> {
        std::mem::take(&mut self.model_mut().action_uses)
    }

    /// Drain the `(lua action id, packed)` pairs `PickupAction`/`PlaceAction` queued since the
    /// last call (decision 0216 §7) — the app sends one `CMSG_SET_ACTION_BUTTON` per entry
    /// (`packed == 0` clears the slot) and updates its own authoritative `PlayerActions` store.
    pub fn take_action_sets(&mut self) -> Vec<(u32, u32)> {
        std::mem::take(&mut self.model_mut().action_sets)
    }
}

/// `checkCursor`'s truthiness (`UseAction`'s second argument): the reference's own numeric
/// convention — `nil`/`false`/`0` all read falsy, anything else (including Lua's own `true`)
/// reads truthy. Deliberately NOT Lua's plain truthiness (where `0` is truthy) — the reference
/// passes literal `0` from a keybind meaning "never place", which only a numeric-zero check
/// reproduces.
fn truthy_nonzero(v: &Value) -> bool {
    match v {
        Value::Nil => false,
        Value::Boolean(b) => *b,
        Value::Integer(i) => *i != 0,
        Value::Number(n) => *n != 0.0,
        _ => true,
    }
}

/// Register the action globals.
pub(super) fn install(lua: &Lua) -> mlua::Result<()> {
    let g = lua.globals();

    g.set(
        "HasAction",
        lua.create_function(|lua, action: u32| {
            let model = lua.app_data_ref::<Model>().expect("model app_data");
            Ok(model.actions.contains_key(&action))
        })?,
    )?;

    g.set(
        "GetActionTexture",
        lua.create_function(|lua, action: u32| {
            let tex = {
                let model = lua.app_data_ref::<Model>().expect("model app_data");
                model.actions.get(&action).and_then(|s| s.texture.clone())
            };
            match tex {
                Some(t) => Ok(Value::String(lua.create_string(&t)?)),
                None => Ok(Value::Nil),
            }
        })?,
    )?;

    // UseAction(action [, checkCursor [, onSelf]]) — `onSelf` is accepted and ignored (the
    // self-cast modifier isn't modeled yet). `checkCursor` TRUTHY (numeric nonzero — the
    // reference's own convention, not Lua's: `0` reads falsy here even though Lua truthiness
    // would call it true) AND a payload is held routes to [`cursor::place_action`] instead of
    // queuing the use (decision 0216 §7's INTERIM: the reference passes 1 from a mouse click and
    // 0 from a keybind — the 0216 §5 dispatch never pinned checkCursor's byte semantics
    // explicitly, and place-on-click is the only reading consistent with that click/keybind
    // split). Any of the three payload arms counts as "held" — Item/Spell/Action are all
    // placeable onto a bar slot.
    g.set(
        "UseAction",
        lua.create_function(|lua, (action, rest): (u32, MultiValue)| {
            let mut model = lua.app_data_mut::<Model>().expect("model app_data");
            let check_cursor = rest.front().is_some_and(truthy_nonzero);
            if check_cursor && model.cursor.is_some() {
                super::cursor::place_action(&mut model, action);
            } else {
                model.action_uses.push(action);
            }
            Ok(())
        })?,
    )?;

    // Bag count for an ITEM-kind action, 0 for every other kind (the ref's IsConsumableAction
    // gate simplified — 0216 §7's App-section note) — the check lives HERE, not in every XML
    // caller, so a slot fed a stray count on the wrong kind (a bug elsewhere) can't leak through.
    g.set(
        "GetActionCount",
        lua.create_function(|lua, action: u32| {
            let model = lua.app_data_ref::<Model>().expect("model app_data");
            Ok(model
                .actions
                .get(&action)
                .filter(|s| s.kind == ACTION_KIND_ITEM)
                .map_or(0, |s| s.count))
        })?,
    )?;

    g.set(
        "GetBonusBarOffset",
        lua.create_function(|lua, ()| {
            let model = lua.app_data_ref::<Model>().expect("model app_data");
            Ok(i64::from(model.bonus_bar_offset))
        })?,
    )?;

    // ── The dynamic-state read API (decision 0137 phase 4): the reference ActionButton.lua's
    // whole input surface, answered from the app-pushed per-action state. Return conventions are
    // the 1.12 API's own (1/nil booleans; IsActionInRange's nil/0/1 tri-state) — the transcribed
    // Lua tests them with plain `if`, so nil-vs-0 matters exactly where the ref makes it matter.

    // A 1/nil boolean read over the state map. An absent action answers nil.
    fn state_flag(lua: &Lua, action: u32, pick: impl Fn(&ActionState) -> bool) -> Value {
        let model = lua.app_data_ref::<Model>().expect("model app_data");
        match model.action_states.get(&action) {
            Some(s) if pick(&s.state) => Value::Integer(1),
            _ => Value::Nil,
        }
    }

    // IsUsableAction(action) → isUsable (1/nil), notEnoughMana (1/nil).
    g.set(
        "IsUsableAction",
        lua.create_function(|lua, action: u32| {
            Ok((
                state_flag(lua, action, |s| s.usable),
                state_flag(lua, action, |s| s.not_enough_mana),
            ))
        })?,
    )?;

    // IsActionInRange(action) → nil (no range / no target) | 0 | 1.
    g.set(
        "IsActionInRange",
        lua.create_function(|lua, action: u32| {
            let model = lua.app_data_ref::<Model>().expect("model app_data");
            Ok(
                match model
                    .action_states
                    .get(&action)
                    .and_then(|s| s.state.in_range)
                {
                    Some(true) => Value::Integer(1),
                    Some(false) => Value::Integer(0),
                    None => Value::Nil,
                },
            )
        })?,
    )?;

    g.set(
        "ActionHasRange",
        lua.create_function(|lua, action: u32| Ok(state_flag(lua, action, |s| s.has_range)))?,
    )?;
    g.set(
        "IsCurrentAction",
        lua.create_function(|lua, action: u32| Ok(state_flag(lua, action, |s| s.current)))?,
    )?;
    g.set(
        "IsAutoRepeatAction",
        lua.create_function(|lua, action: u32| Ok(state_flag(lua, action, |s| s.auto_repeat)))?,
    )?;
    g.set(
        "IsAttackAction",
        lua.create_function(|lua, action: u32| Ok(state_flag(lua, action, |s| s.is_attack)))?,
    )?;
    g.set(
        "IsConsumableAction",
        lua.create_function(|lua, action: u32| Ok(state_flag(lua, action, |s| s.consumable)))?,
    )?;
    g.set(
        "IsEquippedAction",
        lua.create_function(|lua, action: u32| Ok(state_flag(lua, action, |s| s.equipped)))?,
    )?;

    // GetActionCooldown(action) → start, duration, enable — the reference's `(GetTime-clock
    // seconds, seconds, 0/1)` triple. An elapsed (or absent) cooldown answers `(0, 0, 1)`: the
    // read must go cold once `start + duration` passes, or a later event-driven
    // `CooldownFrame_SetTimer` re-feed would re-show the sweep and replay the finish flash.
    g.set(
        "GetActionCooldown",
        lua.create_function(|lua, action: u32| {
            let now: f64 = lua.globals().get("__benilla_now").unwrap_or(0.0);
            let model = lua.app_data_ref::<Model>().expect("model app_data");
            Ok(
                match model.action_states.get(&action).and_then(|s| s.cooldown) {
                    Some((start, duration, enabled)) if start + duration > now || !enabled => {
                        (start, duration, i32::from(enabled))
                    }
                    _ => (0.0, 0.0, 1),
                },
            )
        })?,
    )?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::ActionSlot;
    use crate::script::UiScript;

    #[test]
    fn action_snapshot_reads_and_use_queues() {
        let mut s = UiScript::new().unwrap();
        assert!(!s.eval::<bool>("return HasAction(73)").unwrap());
        assert!(s
            .eval::<bool>("return GetActionTexture(73) == nil")
            .unwrap());
        assert_eq!(s.eval::<i64>("return GetBonusBarOffset()").unwrap(), 0);

        s.set_action(
            73,
            Some(ActionSlot {
                texture: Some("Interface\\Icons\\Ability_Rogue_Ambush".into()),
                kind: 0x00,
                action: 133,
                count: 0,
            }),
        );
        s.set_bonus_bar_offset(1);
        assert!(s.eval::<bool>("return HasAction(73)").unwrap());
        assert_eq!(
            s.eval::<String>("return GetActionTexture(73)").unwrap(),
            "Interface\\Icons\\Ability_Rogue_Ambush"
        );
        assert_eq!(s.eval::<i64>("return GetBonusBarOffset()").unwrap(), 1);

        s.run("UseAction(73)").unwrap();
        s.run("UseAction(74, 0, 1)").unwrap(); // extras accepted, ignored
        assert_eq!(s.take_action_uses(), vec![73, 74]);
        assert!(s.take_action_uses().is_empty());

        s.set_action(73, None);
        assert!(!s.eval::<bool>("return HasAction(73)").unwrap());
    }

    #[test]
    fn get_action_count_reads_the_pushed_count() {
        let mut s = UiScript::new().unwrap();
        assert_eq!(s.eval::<i64>("return GetActionCount(5)").unwrap(), 0);
        s.set_action(
            5,
            Some(ActionSlot {
                texture: Some("Interface\\Icons\\INV_Misc_Food_16".into()),
                kind: 0x80,
                action: 117,
                count: 4,
            }),
        );
        assert_eq!(s.eval::<i64>("return GetActionCount(5)").unwrap(), 4);
    }

    /// `UseAction`'s `checkCursor` routes to a place ONLY when truthy (numeric-nonzero, not Lua
    /// truthiness) AND a payload is held; a keybind's bare `UseAction(id)` and an explicit `0`
    /// both queue the ordinary use even while holding.
    #[test]
    fn use_action_check_cursor_routes_to_place_or_use() {
        use crate::script::cursor::{CursorAction, CursorPayload};

        let mut s = UiScript::new().unwrap();
        s.set_cursor_for_test(CursorPayload::Action(CursorAction {
            src_slot: 1,
            kind: 0x00,
            action: 111,
            texture: Some("Interface\\Icons\\Spell_A".into()),
        }));

        // checkCursor 0 (a keybind's own convention): queues the use, cursor untouched.
        s.run("UseAction(9, 0)").unwrap();
        assert_eq!(s.take_action_uses(), vec![9]);
        assert!(s.cursor_payload().is_some(), "checkCursor 0 never places");

        // checkCursor 1 (a mouse click) with a payload held: places instead of queuing.
        s.run("UseAction(9, 1)").unwrap();
        assert!(s.take_action_uses().is_empty(), "routed to place, not use");
        assert!(s.cursor_payload().is_none(), "empty destination clears");
        assert_eq!(s.take_action_sets(), vec![(9, 111)]);

        // checkCursor 1 with an EMPTY cursor: the ordinary use (nothing to place).
        s.run("UseAction(10, 1)").unwrap();
        assert_eq!(s.take_action_uses(), vec![10]);
    }
}
