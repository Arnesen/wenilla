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
    /// **The slot's packed word, verbatim** — the one place this seam's "the engine holds no pet
    /// knowledge" rule bends, and deliberately (decision 1010).
    ///
    /// The drag ([`super::cursor::pet`]) is word arithmetic in the reference and cannot be
    /// anything else: `0x4bc9a0` compares occupants under `& 0x3FFFFFFF`, tests a candidate's low
    /// 16 bits, and **writes the source word through unchanged** — `0x4bce00` forwards the cursor's
    /// payload dword without reading a field of it, and the payload itself is only ever a verbatim
    /// copy of a word that already existed in a slot. There is no drop-time encoding to model, so
    /// re-deriving a word from `(kind, action, bits)` at the engine boundary would be inventing one.
    ///
    /// `0` is the empty slot, which is also the reference's own test (the zero dword).
    pub packed: u32,
    /// `Attributes & 0x40` (`SPELL_ATTR_PASSIVE`) for a spell slot — the drop core's one source
    /// filter (`0x4bc9f8`–`0x4bca2e`: a type-1 source whose `SpellRec+0x18 & 0x40` is set is
    /// refused, silently). False for a token and for an empty slot.
    pub passive: bool,
}

/// [`PetActionView`] as stored: the cooldown converted to the `GetTime` clock at push time (the
/// [`super::shapeshift::StoredShapeshiftForm`] pattern).
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct StoredPetAction {
    pub(crate) view: PetActionView,
    /// `(start_s, duration_s, enabled)` in `GetTime` seconds; `None` = no cooldown.
    pub(crate) cooldown: Option<(f64, f64, bool)>,
}

/// The hunter-pet stat block behind `GetPetHappiness`/`GetPetLoyalty`/`GetPetTrainingPoints`/
/// `GetPetExperience` and `HasPetUI`'s second return (decision 1005; wow-re §11b).
///
/// **All four bindings share one gate** — `0x6116e0(pet)`, "is this a hunter's pet" — which is why
/// they share one pushed struct: a warlock's imp resolves perfectly well and still answers nothing,
/// because happiness, loyalty and training points are hunter machinery.
///
/// **Their failure conventions differ, and the difference is the API.** `GetPetLoyalty` fails to
/// **nil**; the two pairs fail to **`(0, 0)` — numbers, not nil**; `GetPetHappiness` fails to
/// **`(nil, 100.0, 0.0)`**, nil in the first slot and numbers in the other two. `PetFrame.lua`
/// hides the happiness icon on `not happiness` alone, so collapsing any of these into a uniform
/// nil-everything would hide the frame in cases the reference shows it.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct PetStats {
    /// `HasPetUI`'s second return — the hunter-pet discriminator. False makes every field below
    /// answer its own failure convention regardless of what is in it.
    pub hunter_pet: bool,
    /// `GetPetHappiness`'s first return: the **pre-bucketed** `0..=3`. `None` is the gate
    /// failure's nil; `Some(0)` is a real answer and must stay distinguishable from it.
    pub happiness: Option<u32>,
    /// Return 2 — the damage percentage, already scaled by the client's own `100.0f`.
    pub damage_percentage: f32,
    /// Return 3 — the loyalty rate, unscaled and possibly negative.
    pub loyalty_rate: f32,
    /// `GetPetLoyalty`'s single return — the `PetLoyalty.dbc` name, verbatim including the shipped
    /// `"(Loyalty Level N) "` prefix. `None` = nil (level 0, or off the table).
    pub loyalty: Option<String>,
    /// `GetPetTrainingPoints` → `(totalPoints, spent)`, high word first.
    pub training_points: (u16, u16),
    /// `GetPetExperience` → `(currXP, nextXP)`.
    pub experience: (u32, u32),
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
    /// `HasPetUI`'s FIRST return — "there is a pet with a UI at all", which the reference derives
    /// from the pet resolving plus its `UNIT_FIELD_PETNUMBER` being nonzero (`0x4be697`). Separate
    /// from `has_bar`: the action bar's gate is the cached guid alone, with no pet-number test.
    pub(crate) has_ui: bool,
    pub(crate) stats: PetStats,
    /// May the bar be **rearranged** — `PickupPetAction`'s own gate, and nobody else's
    /// (`0x4be1c1`: `[[pet+0x110]+0xA3] & 1 == 0`, i.e. `UNIT_FLAG_POSSESSED` clear).
    ///
    /// A third bar-wide bit rather than a fold into `actions_usable`, because the reference is
    /// deliberate about keeping them apart: possession is expressly **not** among the flags that
    /// grey the bar (a possessed unit is exactly the case where the buttons must still work), and
    /// the crowd-control flags that do grey it do **not** block a drag. Two gates, two questions.
    ///
    /// It sits before the cursor fork, so it blocks the DROP as well as the pick-up.
    pub(crate) pickup_allowed: bool,
}

impl super::UiScript {
    /// Push the whole pet bar, replacing whatever was there. A bare setter — firing
    /// `PET_BAR_UPDATE` is the app's diff-and-fire job, mirroring `set_shapeshift_forms`.
    pub fn set_pet_actions(
        &mut self,
        has_bar: bool,
        actions_usable: bool,
        pickup_allowed: bool,
        slots: Vec<PetActionView>,
    ) {
        let mut model = self.model_mut();
        model.pet_bar = PetBarState {
            has_bar,
            actions_usable,
            pickup_allowed,
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
            // The stat block rides its own setter: `SMSG_PET_SPELLS` replaces the bar wholesale,
            // but happiness moves every few seconds off a plain descriptor field, so tying the two
            // together would make the bar's diff-and-fire churn on a number no button draws.
            has_ui: model.pet_bar.has_ui,
            stats: std::mem::take(&mut model.pet_bar.stats),
        };
    }

    /// Push the hunter-pet stat block ([`PetStats`]) — the four paper-doll bindings plus
    /// `HasPetUI`. Separate from [`Self::set_pet_actions`] because it changes on a different clock.
    pub fn set_pet_stats(&mut self, has_ui: bool, stats: PetStats) {
        let bar = &mut self.model_mut().pet_bar;
        bar.has_ui = has_ui;
        bar.stats = stats;
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

    /// Drain the pet bar writes the drag queued (decision 1010) — **one `Vec` per
    /// `CMSG_PET_SET_ACTION`**, each of one or two `(0-based position, packed word)` pairs.
    ///
    /// Already-applied on the engine's side: the app's job is to mirror each pair into its own
    /// authoritative ten words and put the batch on the wire whole. Flattening the batches would
    /// break the server's body-size fork between the one- and two-entry forms.
    pub fn take_pet_set_actions(&mut self) -> Vec<Vec<(u32, u32)>> {
        std::mem::take(&mut self.model_mut().pet_set_actions)
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

    // HasPetUI() → hasUI, isHunterPet — ALWAYS exactly two returns (`EAX=2` on every path of
    // `0x4be670`), with `(nil, nil)` when there is no pet with a UI. The second return is the
    // gate every stat binding below shares.
    g.set(
        "HasPetUI",
        lua.create_function(move |lua, ()| {
            let model = lua.app_data_ref::<Model>().expect("model app_data");
            let bar = &model.pet_bar;
            Ok((flag(bar.has_ui), flag(bar.has_ui && bar.stats.hunter_pet)))
        })?,
    )?;

    // GetPetHappiness() → happiness, damagePercentage, loyaltyRate.
    //
    // The client thresholds; Lua does not. Return 1 is a PRE-BUCKETED 0..3 read off
    // PetPersonality.dbc's three columns, and `0` is a real bucket that answers the NUMBER 0 —
    // `PetFrame.lua` hides the icon on `not happiness`, and a 0 leaves it showing with whatever
    // texcoords it had. Only the gate failure is nil, and even then returns 2 and 3 are the
    // numbers (100.0, 0.0), never nil.
    g.set(
        "GetPetHappiness",
        lua.create_function(move |lua, ()| {
            let model = lua.app_data_ref::<Model>().expect("model app_data");
            let s = &model.pet_bar.stats;
            let happiness = match s.happiness.filter(|_| s.hunter_pet) {
                Some(b) => Value::Integer(i64::from(b)),
                None => Value::Nil,
            };
            let (dmg, rate) = if happiness == Value::Nil {
                (100.0, 0.0)
            } else {
                (s.damage_percentage, s.loyalty_rate)
            };
            Ok(MultiValue::from_vec(vec![
                happiness,
                Value::Number(f64::from(dmg)),
                Value::Number(f64::from(rate)),
            ]))
        })?,
    )?;

    // GetPetLoyalty() → the localized level name, or NIL. The one stat binding whose failure is
    // nil rather than a number — level 0 (no loyalty yet) included.
    g.set(
        "GetPetLoyalty",
        lua.create_function(move |lua, ()| {
            let model = lua.app_data_ref::<Model>().expect("model app_data");
            let s = &model.pet_bar.stats;
            Ok(match s.loyalty.as_deref().filter(|_| s.hunter_pet) {
                Some(name) => Value::String(lua.create_string(name)?),
                None => Value::Nil,
            })
        })?,
    )?;

    // GetPetTrainingPoints() → totalPoints, spent — the two halves of one packed dword, HIGH word
    // first. Numbers on every path: a gate failure is (0, 0), not nil.
    g.set(
        "GetPetTrainingPoints",
        lua.create_function(move |lua, ()| {
            let model = lua.app_data_ref::<Model>().expect("model app_data");
            let s = &model.pet_bar.stats;
            let (total, spent) = if s.hunter_pet {
                s.training_points
            } else {
                (0, 0)
            };
            Ok((f64::from(total), f64::from(spent)))
        })?,
    )?;

    // GetPetExperience() → currXP, nextXP. Numbers on every path, like the pair above; the client
    // converts both `fild qword` off a zero-extended dword, so neither can arrive negative.
    g.set(
        "GetPetExperience",
        lua.create_function(move |lua, ()| {
            let model = lua.app_data_ref::<Model>().expect("model app_data");
            let s = &model.pet_bar.stats;
            let (cur, next) = if s.hunter_pet { s.experience } else { (0, 0) };
            Ok((f64::from(cur), f64::from(next)))
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
    use super::{PetActionView, PetStats};
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

        s.set_pet_actions(true, true, true, slots());
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
        s.set_pet_actions(true, true, true, slots());

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
        s.set_pet_actions(true, true, true, slots());

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
        s.set_pet_actions(true, true, true, slots());
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
        s.set_pet_actions(true, false, true, slots());
        assert_eq!(s.eval::<i64>("return PetHasActionBar()").unwrap(), 1);
        assert!(s
            .eval::<bool>("return GetPetActionsUsable() == nil")
            .unwrap());
    }

    fn hunter_stats() -> PetStats {
        PetStats {
            hunter_pet: true,
            happiness: Some(3),
            damage_percentage: 125.0,
            loyalty_rate: 20.0,
            loyalty: Some("(Loyalty Level 6) Best Friend".into()),
            training_points: (170, 130),
            experience: (4200, 8000),
        }
    }

    /// The four stat bindings on a hunter's pet — arities and values, including the pair ORDER
    /// (`total` before `spent`, `curr` before `next`), which is the half a swap makes invisible.
    #[test]
    fn a_hunters_pet_answers_every_stat_binding() {
        let mut s = UiScript::new().unwrap();
        s.set_pet_stats(true, hunter_stats());

        assert!(s
            .eval::<bool>(
                "local h, dmg, rate = GetPetHappiness() \
                 return h == 3 and dmg == 125 and rate == 20"
            )
            .unwrap());
        assert_eq!(
            s.eval::<String>("return GetPetLoyalty()").unwrap(),
            "(Loyalty Level 6) Best Friend",
            "the shipped prefix is pushed verbatim — the client does no stripping"
        );
        assert!(s
            .eval::<bool>("local t, sp = GetPetTrainingPoints() return t == 170 and sp == 130")
            .unwrap());
        assert!(s
            .eval::<bool>("local c, n = GetPetExperience() return c == 4200 and n == 8000")
            .unwrap());
        assert!(s
            .eval::<bool>("local ui, hunter = HasPetUI() return ui == 1 and hunter == 1")
            .unwrap());
    }

    /// **The failure conventions are three different shapes and the API is those shapes.** A
    /// warlock's imp has a pet UI and is not a hunter pet: loyalty goes nil, both pairs go to
    /// `(0, 0)` as NUMBERS, and happiness goes `(nil, 100, 0)` — nil in the first slot only.
    ///
    /// Flattening any of these to nil-everything breaks a different consumer: `PetFrame.lua` hides
    /// its happiness icon on `not happiness`, but `PetPaperDollFrame` does arithmetic on the pairs
    /// and would error on a nil.
    #[test]
    fn a_non_hunter_pet_fails_three_different_ways() {
        let mut s = UiScript::new().unwrap();
        s.set_pet_stats(
            true,
            PetStats {
                hunter_pet: false,
                ..hunter_stats()
            },
        );

        assert!(
            s.eval::<bool>(
                "local h, dmg, rate = GetPetHappiness() \
                 return h == nil and dmg == 100 and rate == 0"
            )
            .unwrap(),
            "happiness is nil but its two numbers are still numbers"
        );
        assert!(s.eval::<bool>("return GetPetLoyalty() == nil").unwrap());
        assert!(s
            .eval::<bool>("local t, sp = GetPetTrainingPoints() return t == 0 and sp == 0")
            .unwrap());
        assert!(s
            .eval::<bool>("local c, n = GetPetExperience() return c == 0 and n == 0")
            .unwrap());
        // HasPetUI still reports the UI — it is the SECOND return that discriminates.
        assert!(s
            .eval::<bool>("local ui, hunter = HasPetUI() return ui == 1 and hunter == nil")
            .unwrap());
    }

    /// **Bucket 0 is a number, not nil**, and this is the trap the RE calls out by name: the
    /// shipped `PetFrame.lua` branches on 1/2/3 and hides only on `not happiness`, so a 0 must
    /// leave the icon up. Collapsing it into the failure path hides a frame the reference shows.
    #[test]
    fn happiness_bucket_zero_is_not_the_failure_case() {
        let mut s = UiScript::new().unwrap();
        s.set_pet_stats(
            true,
            PetStats {
                happiness: Some(0),
                damage_percentage: 100.0,
                loyalty_rate: 0.0,
                ..hunter_stats()
            },
        );
        assert!(s
            .eval::<bool>("local h = GetPetHappiness() return h == 0 and h ~= nil")
            .unwrap());
        assert!(
            s.eval::<bool>("return not GetPetHappiness() == false")
                .unwrap(),
            "0 is truthy in Lua, so the reference's `not happiness` hide-test does NOT fire"
        );
    }

    /// With no pet at all every binding takes its failure path and `HasPetUI` answers `(nil, nil)`
    /// — two returns on every path, never zero.
    #[test]
    fn no_pet_still_answers_two_values_from_has_pet_ui() {
        let s = UiScript::new().unwrap();
        assert!(s
            .eval::<bool>("local ui, hunter = HasPetUI() return ui == nil and hunter == nil")
            .unwrap());
        assert!(s.eval::<bool>("return GetPetLoyalty() == nil").unwrap());
        assert!(s
            .eval::<bool>("local h, dmg = GetPetHappiness() return h == nil and dmg == 100")
            .unwrap());
    }
}
