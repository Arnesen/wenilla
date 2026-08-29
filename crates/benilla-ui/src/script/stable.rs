//! The stable-master bindings (decision 1676) — the hunter stable window's Lua surface, the same
//! two-way seam as [`super::bank`] and [`super::trainer`]: the app pushes a **stable snapshot**
//! ([`UiScript::set_stable`] — the wire's pet rows already resolved to icon/family/loyalty/diet
//! strings), and the Lua's click/purchase/close calls queue outbound **intents** the app drains.
//!
//! ## Three slots, and the current pet is one of them
//!
//! The window shows **`0..=2`**: slot `0` is the pet at the player's side, slots `1` and `2` are the
//! two stable slots a hunter buys (5875 ships exactly two — `StableSlotPrices.dbc` has two rows,
//! vmangos's `MAX_PET_STABLES` is 2, and the reference's `NUM_PET_STABLE_SLOTS` is 2). The wire is
//! 1-based over these; [`benilla_protocol::messages::StabledPet::slot`] already rebased it, so
//! everything here speaks the reference UI's own indices.
//!
//! **Slot 0 can be occupied while the player has no pet out.** A hunter whose pet is dismissed (or
//! merely too far away to be summoned) still gets a slot-0 row from the server, read off the
//! character-pet cache — which is exactly why the reference falls back to `GetStablePetInfo(0)`
//! when `UnitExists("pet")` is false (`PetStable.lua:131-146`) instead of showing an empty slot.
//!
//! ## What the snapshot resolves, and why the app does it
//!
//! The wire names a `creature_template` entry and a loyalty *level*; the window wants an icon, a
//! localized family word, a loyalty *name* and a diet list. Every one of those is a catalog join
//! the app already owns for the live pet ([`super::pet`], decisions 1005/1062), so the app does the
//! join once and pushes strings — this module never sees a DBC. The one join with no live-pet twin
//! is the icon of a pet that is *not* summoned: it comes from the creature query's display id,
//! which is why that field stopped being discarded (decision 1676).
//!
//! ## The drag is FRAME-LOCAL, not the cursor
//!
//! `PickupStablePet` does **not** put anything on the global cursor. wow-re's exhaustive census of
//! every write to the payload-mode global `[0xb4d900]`
//! (`system/ui/scratch/cursor-dragdrop-payload.md` §1) enumerates the modes — item, money, spell,
//! pet action, macro, the three displayId previews, class/talent ability — and **there is no
//! stable-pet mode**. So a picked-up stable pet is state the stable window keeps to itself
//! ([`StableModel::pickup`]), and no other surface can ever be holding one. Building this on
//! [`super::cursor::CursorPayload`] would have invented a mode the client does not have.

use mlua::{Lua, MultiValue, Value};

use super::Model;

/// Window slots: the current pet (`0`) plus the two stable slots — the reference's
/// `NUM_PET_STABLE_SLOTS = 2` counted inclusively from zero (`PetStable.lua:1`).
pub const NUM_STABLE_SLOTS: usize = 3;

/// One row of the stable window, with every wire field already resolved to what the Lua renders.
/// `None` in [`StableState::slots`] is an empty slot — which the window draws differently from an
/// *unbought* one (that distinction is [`StableState::num_stable_slots`]'s).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct StablePetSlot {
    /// The pet's own id — what [`StableIntent::Unstable`]/[`StableIntent::Swap`] name on the wire.
    /// Never its slot: the two disagree for any hunter whose stable is not in id order.
    pub pet_number: u32,
    /// `Interface\Icons\…` for the pet's family, or `None` while the creature query is in flight.
    /// The reference passes this straight to `SetItemButtonTexture`, which takes an empty texture
    /// for a missing icon — so `None` renders the empty-slot art, not a white square (decision
    /// 1046's sweep).
    pub icon: Option<String>,
    /// The name the hunter gave the pet, not the creature template's.
    pub name: String,
    pub level: u32,
    /// The localized `CreatureFamily.dbc` word ("Wolf", "Cat"). `None` when the creature query has
    /// not landed — the reference concatenates it into the level line unguarded, so the binding
    /// substitutes an empty string rather than handing Lua a nil to concatenate.
    pub family: Option<String>,
    /// The localized `PetLoyalty.dbc` name for the wire's loyalty level.
    pub loyalty: Option<String>,
    /// The localized pet-food names this pet's family eats — `GetStablePetFoodTypes`'s returns,
    /// which the reference joins with `BuildListString` into the diet tooltip.
    pub diet: Vec<String>,
}

/// The open stable window's snapshot. Pushed whole while a stable session is open; `None` = no
/// stable open (the window is closed).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct StableState {
    /// Stable slots **purchased**, `0..=2` (the wire's `numStableSlots`). Not a count of occupied
    /// ones: it is what enables slot buttons `1..=num` and greys the rest, and what prices the next
    /// purchase.
    pub num_stable_slots: u32,
    /// The next slot's price in copper (`StableSlotPrices.dbc` row `num_stable_slots + 1`), or `0`
    /// past the table — a state in which the reference has already hidden the purchase row.
    pub next_slot_cost: u32,
    /// The three window slots; index `0` is the current pet.
    pub slots: [Option<StablePetSlot>; NUM_STABLE_SLOTS],
}

impl StableState {
    /// How many of the three slots hold a pet — `GetNumStablePets()`.
    fn num_pets(&self) -> u32 {
        self.slots.iter().filter(|s| s.is_some()).count() as u32
    }
}

/// An outbound stable verb the Lua asked for, drained by the app (which addresses it to the open
/// session's NPC — the guid is the app's, never Lua's).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StableIntent {
    /// `CMSG_STABLE_PET` — put the current pet away. **Carries no destination**: the server takes
    /// the first free bought slot itself, so there is no "stable into slot 2" to express.
    Stable,
    /// `CMSG_UNSTABLE_PET` — summon this pet number, valid only with no current pet.
    Unstable(u32),
    /// `CMSG_STABLE_SWAP_PET` — trade the current pet for this pet number, in one step.
    Swap(u32),
    /// `CMSG_BUY_STABLE_SLOT` — buy the next slot; the *which* is implicit, as with the bank's.
    BuySlot,
}

/// The window's own transient state — selection and the frame-local drag (module doc: the cursor is
/// not involved). Lives beside the snapshot rather than inside it because the app *replaces* the
/// snapshot on every list packet, and a repaint must not drop what the player has selected.
#[derive(Debug)]
pub(crate) struct StableModel {
    pub(crate) state: Option<StableState>,
    /// The selected slot, or `-1` for none — `GetSelectedStablePet()`'s exact return, including its
    /// sentinel, because the reference tests `== -1` directly (`PetStable.lua:44`).
    pub(crate) selected: i32,
    /// The slot a drag started from, `None` when nothing is being dragged.
    pub(crate) pickup: Option<u8>,
    pub(crate) intents: Vec<StableIntent>,
    pub(crate) close: bool,
}

/// **`selected` starts at `-1`, not at `0`** — a derived `Default` would make it `0`, and `0` is a
/// real slot (the current pet). The reference reads the sentinel to decide whether to pick a slot
/// for the player at all (`PetStable.lua:44-59`): defaulted to `0`, a freshly opened window would
/// believe the current pet was already chosen and would never auto-select a stabled one for a
/// hunter with no pet out — the window would open showing nothing, with no way to tell why.
impl Default for StableModel {
    fn default() -> Self {
        Self {
            state: None,
            selected: -1,
            pickup: None,
            intents: Vec::new(),
            close: false,
        }
    }
}

impl super::UiScript {
    /// Push (or clear, with `None`) the open stable's snapshot.
    ///
    /// **Clearing resets the window's transient state too** — selection and any half-finished drag.
    /// A stale selection surviving a close would have the next stable master open on slot 2
    /// highlighted before its list arrived. A *content* change deliberately does not reset it: the
    /// list is re-requested after every successful action, and losing the selection on each one
    /// would fight the player.
    pub fn set_stable(&mut self, state: Option<StableState>) {
        let mut model = self.model_mut();
        if state.is_none() {
            model.stable.selected = -1;
            model.stable.pickup = None;
        }
        model.stable.state = state;
    }

    /// Drain the queued stable verbs (module doc: the app addresses them to the open NPC).
    pub fn take_stable_intents(&mut self) -> Vec<StableIntent> {
        std::mem::take(&mut self.model_mut().stable.intents)
    }

    /// Whether `ClosePetStables()` was called since the last drain (and clear the flag). No packet
    /// exists for it — the app just clears its local session, the bank/merchant pattern.
    pub fn take_stable_close(&mut self) -> bool {
        std::mem::take(&mut self.model_mut().stable.close)
    }

    /// The selected slot (`-1` = none) — what the app's booth points the model pane at.
    pub fn stable_selection(&mut self) -> i32 {
        self.model_mut().stable.selected
    }
}

/// Read a slot argument into an index into [`StableState::slots`]. Out-of-range answers `None`, so
/// every binding below degrades to the reference's empty-slot behaviour rather than panicking on a
/// stray addon call.
fn slot_index(i: i64) -> Option<usize> {
    usize::try_from(i).ok().filter(|&i| i < NUM_STABLE_SLOTS)
}

/// Commit a drag from `from` onto `to`, returning the verb it becomes — the **one** place the
/// stable's move law lives.
///
/// **INFERRED, pending the wow-re carve of `ClickStablePet 0x4ca…` (decision 1676).** The verb set
/// is VERIFIED (vmangos's three handlers and their preconditions); what is *not* yet read off the
/// binary is how the reference client picks among them. This mapping is derived from the server's
/// own constraints, each arm justified:
///
/// - **current → a stable slot** is `CMSG_STABLE_PET`. The destination index is *discarded*: the
///   wire carries no slot and `HandleStablePet` takes the first free one, so dropping on slot 2
///   while slot 1 is empty puts the pet in slot 1. That is the server's behaviour, not a
///   simplification here.
/// - **a stable slot → current** is `Swap` when a pet is out and `Unstable` when none is
///   (`HandleUnstablePet` refuses outright if the player has a pet, even an unsummoned one).
/// - **stable → stable** has no opcode in 5875 at all, so it can only be a no-op.
///
/// If the carve says the client instead ignores the drop target and acts on the *source* alone,
/// only this function changes.
fn drag_verb(from: u8, to: u8, has_current_pet: bool, pet_number: u32) -> Option<StableIntent> {
    match (from, to) {
        (0, 1..) => Some(StableIntent::Stable),
        (1.., 0) if has_current_pet => Some(StableIntent::Swap(pet_number)),
        (1.., 0) => Some(StableIntent::Unstable(pet_number)),
        _ => None,
    }
}

/// Register the stable globals.
pub(super) fn install(lua: &Lua) -> mlua::Result<()> {
    let g = lua.globals();

    // GetStablePetInfo(i) → icon, name, level, family, loyalty — five returns, or a bare nil for an
    // empty slot (the reference tests `if ( icon )` on the first and `if ( GetStablePetInfo(i) )`
    // on the call itself, so both shapes must read as absent). `family` substitutes "" rather than
    // nil: the reference concatenates it into the level line unguarded (`PetStable.lua:100`).
    g.set(
        "GetStablePetInfo",
        lua.create_function(|lua, i: i64| {
            let pet = {
                let model = lua.app_data_ref::<Model>().expect("model app_data");
                slot_index(i).and_then(|i| model.stable.state.as_ref()?.slots[i].clone())
            };
            let Some(pet) = pet else {
                return Ok(MultiValue::new());
            };
            Ok(MultiValue::from_vec(vec![
                match &pet.icon {
                    Some(t) => Value::String(lua.create_string(t)?),
                    None => Value::Nil,
                },
                Value::String(lua.create_string(&pet.name)?),
                Value::Integer(i64::from(pet.level)),
                Value::String(lua.create_string(pet.family.as_deref().unwrap_or(""))?),
                match &pet.loyalty {
                    Some(l) => Value::String(lua.create_string(l)?),
                    None => Value::Nil,
                },
            ]))
        })?,
    )?;

    // GetStablePetFoodTypes(i) → the localized diet names, one return each (the reference feeds the
    // lot to BuildListString). Nothing at all for an empty slot or a pet whose family has no diet —
    // the reference guards with `if ( GetStablePetFoodTypes(i) )` before formatting the tooltip.
    g.set(
        "GetStablePetFoodTypes",
        lua.create_function(|lua, i: i64| {
            let diet = {
                let model = lua.app_data_ref::<Model>().expect("model app_data");
                slot_index(i)
                    .and_then(|i| {
                        Some(model.stable.state.as_ref()?.slots[i].as_ref()?.diet.clone())
                    })
                    .unwrap_or_default()
            };
            let mut out = Vec::with_capacity(diet.len());
            for d in &diet {
                out.push(Value::String(lua.create_string(d)?));
            }
            Ok(MultiValue::from_vec(out))
        })?,
    )?;

    // GetNumStableSlots() → slots PURCHASED (0..=2). The reference both enables buttons `i <= n`
    // and hides the purchase row at `n == NUM_PET_STABLE_SLOTS` off this one number.
    g.set(
        "GetNumStableSlots",
        lua.create_function(|lua, ()| {
            let model = lua.app_data_ref::<Model>().expect("model app_data");
            Ok(i64::from(
                model
                    .stable
                    .state
                    .as_ref()
                    .map_or(0, |s| s.num_stable_slots),
            ))
        })?,
    )?;

    // GetNumStablePets() → how many of the three slots hold a pet.
    g.set(
        "GetNumStablePets",
        lua.create_function(|lua, ()| {
            let model = lua.app_data_ref::<Model>().expect("model app_data");
            Ok(i64::from(
                model.stable.state.as_ref().map_or(0, StableState::num_pets),
            ))
        })?,
    )?;

    // GetNextStableSlotCost() → the next slot's price in copper (the app read it from
    // StableSlotPrices.dbc). 0 with no stable open, and 0 past the table — where the reference has
    // already hidden the row that would show it.
    g.set(
        "GetNextStableSlotCost",
        lua.create_function(|lua, ()| {
            let model = lua.app_data_ref::<Model>().expect("model app_data");
            Ok(i64::from(
                model.stable.state.as_ref().map_or(0, |s| s.next_slot_cost),
            ))
        })?,
    )?;

    // GetSelectedStablePet() → the selected slot, or -1. The sentinel is the API: the reference
    // tests `selectedPet == -1` to decide whether to pick a slot for the player.
    g.set(
        "GetSelectedStablePet",
        lua.create_function(|lua, ()| {
            let model = lua.app_data_ref::<Model>().expect("model app_data");
            Ok(i64::from(model.stable.selected))
        })?,
    )?;

    // ClickStablePet(i) → did anything change (the reference repaints only on true).
    //
    // Two jobs, because the reference wires this to BOTH `OnClick` and `OnReceiveDrag`: commit a
    // drag if one is in flight, otherwise select. The commit half's verb choice is `drag_verb`'s —
    // and INFERRED until the carve lands (see there).
    g.set(
        "ClickStablePet",
        lua.create_function(|lua, i: i64| {
            let Some(to) = slot_index(i) else {
                return Ok(false);
            };
            let to = to as u8;
            let mut model = lua.app_data_mut::<Model>().expect("model app_data");
            let Some(state) = model.stable.state.as_ref() else {
                return Ok(false);
            };

            if let Some(from) = model.stable.pickup {
                // A drag lands. It is consumed whatever it turns into — including the
                // stable→stable no-op, which must still put the cursor down rather than leave the
                // window holding a pet nothing can drop.
                let has_current = state.slots[0].is_some();
                let pet_number = state.slots[from as usize]
                    .as_ref()
                    .map_or(0, |p| p.pet_number);
                model.stable.pickup = None;
                if from == to || pet_number == 0 {
                    return Ok(false);
                }
                if let Some(intent) = drag_verb(from, to, has_current, pet_number) {
                    model.stable.intents.push(intent);
                }
                return Ok(true);
            }

            // A plain click selects. Selecting the already-selected slot changes nothing, and
            // saying so keeps the reference from repainting the whole window on every click.
            if model.stable.selected == i32::from(to) {
                return Ok(false);
            }
            model.stable.selected = i32::from(to);
            Ok(true)
        })?,
    )?;

    // PickupStablePet(i) — begin a frame-local drag (module doc: NOT the global cursor). Picking up
    // an empty slot is ignored, exactly as the reference's own empty buttons carry no pet to drag.
    g.set(
        "PickupStablePet",
        lua.create_function(|lua, i: i64| {
            let mut model = lua.app_data_mut::<Model>().expect("model app_data");
            let occupied = slot_index(i)
                .and_then(|i| Some(model.stable.state.as_ref()?.slots[i].is_some()))
                .unwrap_or(false);
            if occupied {
                model.stable.pickup = Some(i as u8);
            }
            Ok(())
        })?,
    )?;

    // SetPetStablePaperdoll(model) — inert here, deliberately, and this is the same divergence
    // PetPaperDollFrame.xml records for `PetModelFrame:SetUnit("pet")`: benilla's model panes are
    // app-side booths that follow the selection every frame, so there is no VM-side unit to point.
    // The binding still exists because the reference calls it (four sites) and an addon may.
    g.set(
        "SetPetStablePaperdoll",
        lua.create_function(|_, _model: Value| Ok(()))?,
    )?;

    // BuyStableSlot() — queue the purchase. The server prices and picks the slot; a refusal comes
    // back as SMSG_STABLE_RESULT's ERR_MONEY.
    g.set(
        "BuyStableSlot",
        lua.create_function(|lua, ()| {
            let mut model = lua.app_data_mut::<Model>().expect("model app_data");
            model.stable.intents.push(StableIntent::BuySlot);
            Ok(())
        })?,
    )?;

    // ClosePetStables() — client-side close, no packet exists (vmangos has no close opcode): flag
    // the app to clear its session.
    g.set(
        "ClosePetStables",
        lua.create_function(|lua, ()| {
            let mut model = lua.app_data_mut::<Model>().expect("model app_data");
            model.stable.close = true;
            Ok(())
        })?,
    )?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::script::UiScript;

    fn pet(number: u32, name: &str, level: u32) -> StablePetSlot {
        StablePetSlot {
            pet_number: number,
            icon: Some("Interface\\Icons\\Ability_Hunter_Pet_Wolf".into()),
            name: name.into(),
            level,
            family: Some("Wolf".into()),
            loyalty: Some("(Loyalty Level 6) Best Friend".into()),
            diet: vec!["Meat".into(), "Fish".into()],
        }
    }

    /// A hunter with a pet out and one stabled, one slot bought.
    fn open(s: &mut UiScript) {
        s.set_stable(Some(StableState {
            num_stable_slots: 1,
            next_slot_cost: 50_000,
            slots: [Some(pet(7, "Rex", 41)), Some(pet(8, "Bruiser", 38)), None],
        }));
    }

    /// The read surface, through the reference's own destructuring. A closed window answers the
    /// absent shape everywhere rather than erroring — the frame's OnLoad runs before any list.
    #[test]
    fn the_read_surface_answers_the_reference_calls() {
        let mut s = UiScript::new().unwrap();
        assert_eq!(s.eval::<i64>("return GetNumStableSlots()").unwrap(), 0);
        assert_eq!(s.eval::<i64>("return GetNumStablePets()").unwrap(), 0);
        assert_eq!(s.eval::<i64>("return GetNextStableSlotCost()").unwrap(), 0);
        assert_eq!(s.eval::<i64>("return GetSelectedStablePet()").unwrap(), -1);
        assert!(s.eval::<bool>("return GetStablePetInfo(0) == nil").unwrap());

        open(&mut s);
        assert_eq!(s.eval::<i64>("return GetNumStableSlots()").unwrap(), 1);
        assert_eq!(s.eval::<i64>("return GetNumStablePets()").unwrap(), 2);
        assert_eq!(
            s.eval::<i64>("return GetNextStableSlotCost()").unwrap(),
            50_000
        );

        // `PetStable.lua:76` — the exact five-value destructuring the window renders from.
        assert_eq!(
            s.eval::<(String, String, i64, String, String)>(
                "local i, n, l, f, loy = GetStablePetInfo(1) return i, n, l, f, loy"
            )
            .unwrap(),
            (
                "Interface\\Icons\\Ability_Hunter_Pet_Wolf".into(),
                "Bruiser".into(),
                38,
                "Wolf".into(),
                "(Loyalty Level 6) Best Friend".into()
            )
        );
        // The empty third slot reads absent on both tests the reference makes.
        assert!(s.eval::<bool>("return GetStablePetInfo(2) == nil").unwrap());
        // An out-of-range index is absent too, not an error.
        assert!(s.eval::<bool>("return GetStablePetInfo(9) == nil").unwrap());

        // The diet list, as BuildListString receives it.
        assert_eq!(
            s.eval::<(String, String)>("return GetStablePetFoodTypes(1)")
                .unwrap(),
            ("Meat".into(), "Fish".into())
        );
        assert!(s
            .eval::<bool>("return GetStablePetFoodTypes(2) == nil")
            .unwrap());
    }

    /// A plain click selects, and reports whether anything changed — the reference repaints only
    /// on true, so a click on the already-selected slot must answer false or the window redraws
    /// itself on every click.
    #[test]
    fn a_plain_click_selects_once() {
        let mut s = UiScript::new().unwrap();
        open(&mut s);
        assert!(s.eval::<bool>("return ClickStablePet(1)").unwrap());
        assert_eq!(s.eval::<i64>("return GetSelectedStablePet()").unwrap(), 1);
        assert!(!s.eval::<bool>("return ClickStablePet(1)").unwrap());
        assert!(s.eval::<bool>("return ClickStablePet(0)").unwrap());
        assert_eq!(s.eval::<i64>("return GetSelectedStablePet()").unwrap(), 0);
        // Selecting queues nothing on the wire.
        assert!(s.take_stable_intents().is_empty());
    }

    /// Dragging the current pet onto a stable slot stables it — and the destination is *not* sent,
    /// because the wire cannot carry one.
    #[test]
    fn dragging_the_current_pet_out_stables_it() {
        let mut s = UiScript::new().unwrap();
        open(&mut s);
        s.eval::<()>("PickupStablePet(0) ClickStablePet(2)")
            .unwrap();
        assert_eq!(s.take_stable_intents(), vec![StableIntent::Stable]);
    }

    /// Dragging a stabled pet onto the current slot forks on whether a pet is already out: a swap
    /// when one is, a plain unstable when none is. The server refuses an unstable outright while a
    /// pet exists, so picking the wrong one here fails every drag for a hunter with a pet.
    #[test]
    fn dragging_a_stabled_pet_in_forks_on_having_a_pet() {
        let mut s = UiScript::new().unwrap();
        open(&mut s);
        s.eval::<()>("PickupStablePet(1) ClickStablePet(0)")
            .unwrap();
        assert_eq!(s.take_stable_intents(), vec![StableIntent::Swap(8)]);

        // Same drag with slot 0 empty — the hunter has no pet out.
        s.set_stable(Some(StableState {
            num_stable_slots: 1,
            next_slot_cost: 50_000,
            slots: [None, Some(pet(8, "Bruiser", 38)), None],
        }));
        s.eval::<()>("PickupStablePet(1) ClickStablePet(0)")
            .unwrap();
        assert_eq!(s.take_stable_intents(), vec![StableIntent::Unstable(8)]);
    }

    /// A drag that has no verb still puts the pet down. Stable→stable has no opcode in 5875, and a
    /// drag onto its own slot is a cancel; neither may leave the window holding a pet forever.
    #[test]
    fn a_verbless_drag_is_still_consumed() {
        let mut s = UiScript::new().unwrap();
        s.set_stable(Some(StableState {
            num_stable_slots: 2,
            next_slot_cost: 0,
            slots: [None, Some(pet(8, "Bruiser", 38)), None],
        }));
        s.eval::<()>("PickupStablePet(1) ClickStablePet(2)")
            .unwrap();
        assert!(s.take_stable_intents().is_empty());
        // The pickup is gone, so the NEXT click selects rather than committing a stale drag.
        assert!(s.eval::<bool>("return ClickStablePet(1)").unwrap());
        assert_eq!(s.eval::<i64>("return GetSelectedStablePet()").unwrap(), 1);
        assert!(s.take_stable_intents().is_empty());
    }

    /// An empty slot carries nothing to drag, so picking it up leaves no pickup armed — otherwise
    /// the next click would commit a move for a pet that does not exist.
    #[test]
    fn an_empty_slot_cannot_be_picked_up() {
        let mut s = UiScript::new().unwrap();
        open(&mut s);
        s.eval::<()>("PickupStablePet(2) ClickStablePet(0)")
            .unwrap();
        assert!(s.take_stable_intents().is_empty());
        assert_eq!(s.eval::<i64>("return GetSelectedStablePet()").unwrap(), 0);
    }

    /// The purchase and close intents, and the close's state reset: a selection must not survive
    /// into the next stable master's window.
    #[test]
    fn purchase_and_close_intents_drain() {
        let mut s = UiScript::new().unwrap();
        open(&mut s);
        s.eval::<()>("ClickStablePet(1) BuyStableSlot()").unwrap();
        assert_eq!(s.take_stable_intents(), vec![StableIntent::BuySlot]);
        assert!(s.take_stable_intents().is_empty(), "drain clears");

        assert!(!s.take_stable_close());
        s.eval::<()>("ClosePetStables()").unwrap();
        assert!(s.take_stable_close());
        assert!(!s.take_stable_close(), "drain clears");

        assert_eq!(s.stable_selection(), 1);
        s.set_stable(None);
        assert_eq!(s.stable_selection(), -1, "a close forgets the selection");
    }

    /// A content refresh — which benilla does after every successful action — must KEEP the
    /// selection. Resetting it there would move the player's highlight on every button press.
    #[test]
    fn a_refresh_keeps_the_selection() {
        let mut s = UiScript::new().unwrap();
        open(&mut s);
        s.eval::<()>("ClickStablePet(1)").unwrap();
        open(&mut s);
        assert_eq!(s.stable_selection(), 1);
    }

    /// `SetPetStablePaperdoll` exists and is harmless — the reference calls it at four sites, and a
    /// missing global would be a Lua error mid-repaint.
    #[test]
    fn the_paperdoll_setter_is_callable() {
        let s = UiScript::new().unwrap();
        s.eval::<()>("SetPetStablePaperdoll(nil)").unwrap();
    }
}
