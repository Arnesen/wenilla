//! The **dressing room** surface (decision 1060) — the intent queue behind the ref's
//! `DressUpModel:SetUnit`/`:TryOn`/`:Dress`, plus the pane's bake yaw.
//!
//! ## Why these are benilla-named, not the live API's
//!
//! In 1.12 the dressing room's model is a `<DressUpModel>` **widget** and the verbs are *methods on
//! it* (`DressUpFrame.lua:2-16`: `DressUpModel:SetUnit("player")` then `DressUpModel:TryOn(item)`;
//! the Reset button calls `DressUpModel:Dress()`). benilla has no live 3D model widget — every
//! model pane is a booth bake sampled by a plain `<Frame>` (the settled doctrine, 0105/0118, and
//! what `BenillaSetBoothTexture` exists for) — so the *widget methods* have no home. They become
//! four benilla globals over this queue, exactly as the paper doll's rotation did
//! (`BenillaPaperDollModel_SetFacing`). The window's own Lua keeps the reference's `DressUpItem` /
//! `DressUpItemLink` entry points, which is what every ctrl-click site actually calls.
//!
//! ## The seam
//!
//! - **Intents:** an ordered queue, not a set of flags — `DressUpItem` resets *then* tries on in
//!   one breath when the window was closed (ref `DressUpFrame.lua:3-7`), and applying those two out
//!   of order would show the player's own gear instead of the item they clicked.
//! - **State lives app-side:** the VM holds no item ids and no equipment. The app owns the
//!   substitution set, resolves each item id to a display through the ask-once template cache, and
//!   composes the booth's look ([`crate::script::UiScript::take_dressup_intents`]).
//! - **Yaw:** [`UiScript::dressup_yaw`], the exact twin of `paperdoll_yaw` / `inspect_yaw`.

use mlua::Lua;

use super::Model;

/// One queued dressing-room intent (see the module doc on ordering).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum DressUpIntent {
    /// `DressUpModel:SetUnit("player")` (the open) and `DressUpModel:Dress()` (the Reset button) —
    /// both mean "wear what the player is actually wearing", dropping every substitution. One verb
    /// because they are one effect here: our model is always the player's.
    Dress,
    /// `DressUpModel:TryOn(item)` — substitute this item id into whichever slot its
    /// `InventoryType` belongs to.
    TryOn(u32),
    /// The window was hidden — there is nothing to show, so the booth empties (and stops
    /// rendering). The reference's widget keeps its state while hidden, but its next `DressUpItem`
    /// re-issues `SetUnit("player")` precisely *because* the frame was not visible, so the state it
    /// kept is never observable — dropping it here is behaviour-identical and saves the bake.
    Close,
}

impl super::UiScript {
    /// Drain the dressing room's queued intents, oldest first — the app applies them in order.
    pub fn take_dressup_intents(&mut self) -> Vec<DressUpIntent> {
        std::mem::take(&mut self.model_mut().dressup_intents)
    }

    /// The dressing-room pane's bake yaw in radians (the ref's `Model:SetRotation`), read by the
    /// app onto the `"dressup"` booth slot each frame — the twin of [`Self::paperdoll_yaw`].
    pub fn dressup_yaw(&self) -> f32 {
        self.model_ref().dressup_yaw
    }
}

pub(super) fn install(lua: &Lua) -> mlua::Result<()> {
    let g = lua.globals();

    let queue = |lua: &Lua, intent: DressUpIntent| {
        lua.app_data_mut::<Model>()
            .expect("model app_data")
            .dressup_intents
            .push(intent);
    };

    // BenillaDressUpModel_Dress() — the ref's `DressUpModel:SetUnit("player")` (DressUpFrame.lua:5)
    // and `:Dress()` (the Reset button, DressUpFrame.xml:180).
    g.set(
        "BenillaDressUpModel_Dress",
        lua.create_function(move |lua, ()| {
            queue(lua, DressUpIntent::Dress);
            Ok(())
        })?,
    )?;

    // BenillaDressUpModel_TryOn(itemId) — the ref's `DressUpModel:TryOn(item)` (DressUpFrame.lua:8),
    // whose argument is the bare item id `DressUpItemLink` pulled out of the `|Hitem:<id>:…` link.
    g.set(
        "BenillaDressUpModel_TryOn",
        lua.create_function(move |lua, item: Option<u32>| {
            if let Some(item) = item.filter(|i| *i != 0) {
                queue(lua, DressUpIntent::TryOn(item));
            }
            Ok(())
        })?,
    )?;

    // BenillaDressUpModel_Close() — the window's OnHide (see [`DressUpIntent::Close`]).
    g.set(
        "BenillaDressUpModel_Close",
        lua.create_function(move |lua, ()| {
            queue(lua, DressUpIntent::Close);
            Ok(())
        })?,
    )?;

    // BenillaDressUpModel_SetFacing(radians) — the pane's bake yaw, the exact twin of
    // `BenillaPaperDollModel_SetFacing` (the inspect pane's twin went with 1832's migration).
    g.set(
        "BenillaDressUpModel_SetFacing",
        lua.create_function(|lua, radians: f32| {
            lua.app_data_mut::<Model>()
                .expect("model app_data")
                .dressup_yaw = radians;
            Ok(())
        })?,
    )?;

    Ok(())
}
