//! [`super::feed_actions`] as a real Bevy system through a real Lua VM — the seam where the
//! action bar's slot identity is resolved and pushed.
//!
//! What these pin is the **landed-template redisplay** (decision 0660): an ITEM slot's icon needs
//! an item template that arrives asynchronously, so the resolve that first touches a cold entry is
//! the one that ISSUES the ask-once query and necessarily reads back nothing. The regression these
//! guard is a question mark that never goes away — the fresh character's food button.
//!
//! `run_system_once` is deliberately NOT used: the feed's memory is a `Local`, which that helper
//! rebuilds per call, so a two-frame test has to run a registered system across two `app.update()`s.

use std::collections::HashMap;

use benilla_formats::{ItemDisplay, ItemDisplayCatalog};
use benilla_protocol::messages::{ActionButton, ACTION_KIND_ITEM};
use benilla_ui::script::UiScript;
use bevy::prelude::*;

use super::{feed_actions, CastErrors, MountErrors, PlayerActions, UiErrorKeys};
use crate::entities::ItemDisplays;
use crate::items::{test_template, Items};
use crate::net::{ClientCommand, NetCommands};

/// The fresh human warrior's default bar (vmangos `playercreateinfo_action`): Tough Jerky, item
/// **117**, on wire slot **83** (the Battle Stance bonus page) — Lua action id 84.
const JERKY: u32 = 117;
const JERKY_SLOT: u8 = 83;
const JERKY_ACTION: u32 = JERKY_SLOT as u32 + 1;
/// Tough Jerky's real display id and the icon that display carries (`ItemDisplayInfo.dbc`).
const JERKY_DISPLAY: u32 = 2473;
const JERKY_ICON: &str = "Interface\\Icons\\INV_Misc_Food_16";

/// An app with the feed registered and the fresh character's food button on the bar — but a cold
/// template cache, exactly as at login.
fn app_with_food_on_the_bar() -> (App, crossbeam_channel::Receiver<ClientCommand>) {
    let (tx, rx) = crossbeam_channel::unbounded();
    let mut app = App::new();
    let mut actions = PlayerActions::default();
    actions.buttons.insert(
        JERKY_SLOT,
        ActionButton {
            slot: JERKY_SLOT,
            action: JERKY,
            kind: ACTION_KIND_ITEM,
        },
    );
    // SMSG_ACTION_BUTTONS' own arm sets this; the feed's first pass is what clears it.
    actions.dirty = true;

    let displays = HashMap::from([(
        JERKY_DISPLAY,
        ItemDisplay {
            icon: Some(JERKY_ICON.to_string()),
            ..Default::default()
        },
    )]);

    app.insert_resource(actions)
        .init_resource::<Items>()
        .init_resource::<CastErrors>()
        .init_resource::<MountErrors>()
        .init_resource::<UiErrorKeys>()
        .insert_resource(ItemDisplays::icons_for_tests(
            ItemDisplayCatalog::from_displays(displays),
        ))
        .insert_resource(NetCommands(tx));
    app.insert_non_send_resource(UiScript::new().unwrap());
    app.add_systems(Update, feed_actions);
    (app, rx)
}

/// What the VM believes the food button's icon is — the exact read `ActionBar.xml` does before it
/// falls back to `BENILLA_FALLBACK_ICON` (the question mark).
fn fed_texture(app: &mut App) -> Option<String> {
    app.world_mut()
        .non_send_resource::<UiScript>()
        .eval::<Option<String>>(&format!("return GetActionTexture({JERKY_ACTION})"))
        .unwrap()
}

/// The whole bug in one test: the first resolve issues the query and can only feed nothing; the
/// frame the answer lands, the slot re-resolves and the real icon arrives. Before decision 0660 the
/// second half never happened — the button kept the fallback question mark for the whole session,
/// until some unrelated bar edit re-dirtied the feed.
#[test]
fn a_landed_item_template_redisplays_the_action_slot() {
    let (mut app, rx) = app_with_food_on_the_bar();

    app.update();
    assert_eq!(
        fed_texture(&mut app),
        None,
        "the first resolve of a cold entry IS the ask — it cannot have an icon yet"
    );
    assert!(
        rx.try_iter()
            .any(|c| matches!(c, ClientCommand::ItemQuery { entry, .. } if entry == JERKY)),
        "…and it must have asked the server for the template"
    );

    // Nothing else changes: no bar edit, no new spell, no local pickup — only the answer arriving.
    let mut info = test_template("Tough Jerky");
    info.display_info_id = JERKY_DISPLAY;
    app.world_mut()
        .resource_mut::<Items>()
        .insert_template(JERKY, Some(info));

    app.update();
    assert_eq!(
        fed_texture(&mut app).as_deref(),
        Some(JERKY_ICON),
        "the landed template redisplays the slot — the fresh character's food loses its question mark"
    );
}

/// The epoch is a *change* gate, not a per-frame re-resolve: once the answer has landed and been
/// fed, an idle frame re-resolves nothing. (Guards the obvious over-correction — turning the feed
/// into an every-frame rebuild of all 120 slots.)
#[test]
fn a_quiet_frame_after_the_answer_re_resolves_nothing() {
    let (mut app, _rx) = app_with_food_on_the_bar();
    app.update();
    let mut info = test_template("Tough Jerky");
    info.display_info_id = JERKY_DISPLAY;
    app.world_mut()
        .resource_mut::<Items>()
        .insert_template(JERKY, Some(info));
    app.update();

    let before = app.world().resource::<Items>().template_epoch();
    app.update();
    assert_eq!(
        app.world().resource::<Items>().template_epoch(),
        before,
        "an idle frame lands no template, so the gate stays shut"
    );
    assert_eq!(
        fed_texture(&mut app).as_deref(),
        Some(JERKY_ICON),
        "…and the fed icon is unchanged"
    );
}

/// A NEGATIVE answer (the server does not know the entry) also advances the epoch — it is a real
/// transition for anything that waits on the ask — but it resolves to no icon, so the slot keeps
/// the fallback. Pins that the gate cannot spin: one re-resolve, then quiet.
#[test]
fn an_unknown_entry_answers_once_and_settles() {
    let (mut app, _rx) = app_with_food_on_the_bar();
    app.update();
    app.world_mut()
        .resource_mut::<Items>()
        .insert_template(JERKY, None);
    app.update();

    assert_eq!(
        fed_texture(&mut app),
        None,
        "an unknown entry has no display and so no icon"
    );
    assert!(
        app.world()
            .resource::<Items>()
            .template_answered_unknown(JERKY),
        "the negative is cached — the feed must not re-ask on the next resolve"
    );
}
