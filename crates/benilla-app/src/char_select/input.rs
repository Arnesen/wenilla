//! The select screen's input — clicks (single selects, double enters — the ref's
//! `CharacterSelectButton_OnClick`/`OnDoubleClick`), the bottom buttons, the keyboard (Enter =
//! enter world, Escape = exit, arrows cycle with wrap), and the model rotation (drag anywhere at
//! the pinned `CHARACTER_ROTATION_CONSTANT` 0.6°/px; hold the rotate pair at ±2°/frame). Sounds
//! are the ref's exact set — a bare selection click is silent.

use bevy::input::mouse::AccumulatedMouseMotion;
use bevy::prelude::*;

use crate::net::CharPick;
use crate::portrait::GluePreview;
use crate::sound::GlueSound;

use super::dialog::DeleteDialog;
use super::screen::SelectAction;
use super::{class_name, send_pick, ClientState, Roster};

/// The ref's drag constant (`CHARACTER_ROTATION_CONSTANT = 0.6`, CharacterSelect.lua — degrees per
/// pixel; the facing setter is in degrees, deg→rad at the C boundary).
pub(crate) const ROTATION_PER_PX: f32 = 0.6 * std::f32::consts::PI / 180.0;
/// The rotate buttons' hold rate — the ref's 2°-per-frame `CHARACTER_FACING_INCREMENT`, per-second.
const ROTATE_RATE: f32 = 120.0 * std::f32::consts::PI / 180.0;
/// The double-click window (the ref rides the OS notion; this is the conventional interval).
const DOUBLE_CLICK_SECS: f32 = 0.4;

/// Button presses + the keyboard: selection, enter world, delete (opens the dialog), create,
/// back/escape (return to the login screen — decision 0539, retiring 0465 §6's exit-the-client
/// collapse), arrow-key cycling. Inert while the delete dialog is up (it owns the keyboard and
/// sits over the buttons).
#[allow(clippy::too_many_arguments, clippy::type_complexity)]
pub(super) fn select_input(
    presses: Query<(&SelectAction, &Interaction), Changed<Interaction>>,
    keys: Res<ButtonInput<KeyCode>>,
    mut roster: ResMut<Roster>,
    pick: Res<CharPick>,
    mut dialog: ResMut<DeleteDialog>,
    mut panel: ResMut<super::addons::AddonsPanel>,
    mut next: ResMut<NextState<ClientState>>,
    mut sounds: MessageWriter<GlueSound>,
    mut intent: ResMut<crate::login::LoginIntent>,
    time: Res<Time>,
    mut last_click: Local<Option<(usize, f32)>>,
) {
    // A modal owns the input while it is up — the delete confirm, or the AddOns list.
    if dialog.open || panel.open {
        return;
    }
    let now = time.elapsed_secs();
    let mut enter_world = false;
    let mut back_to_login = false;
    for (action, interaction) in &presses {
        if *interaction != Interaction::Pressed {
            continue;
        }
        match *action {
            SelectAction::Row(i) if i < roster.chars.len() => {
                // Single click selects (silently, like the ref); a second click on the same row
                // within the window is the double-click → enter world.
                let double =
                    last_click.is_some_and(|(row, at)| row == i && now - at < DOUBLE_CLICK_SECS);
                *last_click = Some((i, now));
                if roster.selected != Some(i) {
                    roster.selected = Some(i);
                } else if double {
                    enter_world = true;
                }
            }
            SelectAction::EnterWorld => enter_world = true,
            SelectAction::Delete => {
                // The ref plays the click and opens the typed-confirm dialog for the selection
                // (a no-selection click just plays the sound — the ref's `selectedIndex > 0` gate).
                sounds.write(GlueSound("gsCharacterSelectionDelCharacter"));
                if let Some(c) = roster.selected_char() {
                    dialog.open_for(c.guid, c.name.clone(), c.level, class_name(c.class));
                }
            }
            SelectAction::CreateChar => {
                sounds.write(GlueSound("gsCharacterSelectionCreateNew"));
                next.set(ClientState::CharCreate);
            }
            SelectAction::Addons => {
                // The reference plays `gsCharacterSelectionOpen`-family click here; ours reuses
                // the create-screen open sound rather than inventing a name the client lacks.
                sounds.write(GlueSound("gsCharacterSelectionCreateNew"));
                // The whole roster rides along so the panel's "Configure Addons For:" dropdown
                // can fan out over every character (decision 1293). The realm resolves exactly
                // as `ui_macro::identity`'s does — same fallback, so the enable files the panel
                // writes stay keyed the way the world-entry walk reads them (0997, 1191 §7).
                let realm = roster
                    .realm
                    .as_ref()
                    .map(|r| r.name.clone())
                    .unwrap_or_else(|| "Realm".into());
                let chars = roster.chars.iter().map(|c| c.name.clone()).collect();
                panel.open_for(realm, chars);
            }
            SelectAction::Back => back_to_login = true,
            _ => {}
        }
    }

    // Keyboard: Enter = enter world; Escape = back to login; Up/Left + Down/Right cycle with wrap.
    if keys.just_pressed(KeyCode::Enter) || keys.just_pressed(KeyCode::NumpadEnter) {
        enter_world = true;
    }
    if keys.just_pressed(KeyCode::Escape) {
        back_to_login = true;
    }
    if back_to_login {
        // The ref's Back leaves select for the login screen (decision 0539): drop the parked
        // session (the IO thread re-parks pre-logon) and forget both intents — a deliberate Back
        // must not auto-relogin.
        sounds.write(GlueSound("gsCharacterSelectionExit"));
        roster.pending_pick = None;
        intent.clear();
        let _ = pick.0.send(crate::net::CharRequest::Abandon);
        next.set(ClientState::Login);
    }
    let n = roster.chars.len();
    if n > 1 {
        let back = keys.just_pressed(KeyCode::ArrowUp) || keys.just_pressed(KeyCode::ArrowLeft);
        let fwd = keys.just_pressed(KeyCode::ArrowDown) || keys.just_pressed(KeyCode::ArrowRight);
        if back || fwd {
            let cur = roster.selected.unwrap_or(0);
            let sel = if back {
                (cur + n - 1) % n
            } else {
                (cur + 1) % n
            };
            roster.selected = Some(sel);
        }
    }

    if enter_world && roster.pending_pick.is_none() {
        let target = roster.selected_char().map(|c| (c.guid, c.name.clone()));
        if let Some((guid, name)) = target {
            info!("char select: entering world as {name}");
            sounds.write(GlueSound("gsCharacterSelectionEnterWorld"));
            send_pick(&mut roster, &pick, guid);
        }
    }
}

/// Rotate the selected character: drag anywhere on the scene pane (the ref's full-frame mouse
/// rotation at the pinned 0.6°/px — dragging right increases the facing), or hold a rotate button
/// (±2°/frame; left decrements).
pub(super) fn rotate_model(
    panes: Query<(&Interaction, &SelectAction)>,
    motion: Res<AccumulatedMouseMotion>,
    time: Res<Time>,
    mut preview: ResMut<GluePreview>,
) {
    for (interaction, action) in &panes {
        if *interaction != Interaction::Pressed {
            continue;
        }
        match action {
            SelectAction::Scene if motion.delta.x != 0.0 => {
                preview.yaw += motion.delta.x * ROTATION_PER_PX;
            }
            SelectAction::RotateLeft => preview.yaw -= ROTATE_RATE * time.delta_secs(),
            SelectAction::RotateRight => preview.yaw += ROTATE_RATE * time.delta_secs(),
            _ => {}
        }
    }
}
