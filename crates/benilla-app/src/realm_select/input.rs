//! The realm list's input — the reference's `RealmSelectButton_OnClick`/`OnDoubleClick`,
//! `RealmList_OnOk`/`OnCancel`, `RealmListTab_OnClick`, `SortRealms`, and `RealmList_OnKeyDown`.

use bevy::input::mouse::MouseWheel;
use bevy::prelude::*;

use crate::char_select::ClientState;
use crate::net::{RealmChoice, RealmRequest};
use crate::sound::GlueSound;

use super::screen::{RealmAction, MAX_ROWS};
use super::{answer, is_down, Realms};

/// The double-click window — the same conventional interval the select screen uses.
const DOUBLE_CLICK_SECS: f32 = 0.4;

/// Clicks: a row selects (a second one enters), the column headers sort, Okay enters, Cancel and
/// the close X leave.
#[allow(clippy::too_many_arguments)]
pub(super) fn clicks(
    buttons: Query<(Entity, &RealmAction)>,
    hits: Res<crate::glue::GlueClicks>,
    mut realms: ResMut<Realms>,
    choice: Res<RealmChoice>,
    mut next: ResMut<NextState<ClientState>>,
    mut sounds: MessageWriter<GlueSound>,
    time: Res<Time>,
    mut last_click: Local<Option<(String, f32)>>,
) {
    let now = time.elapsed_secs();
    let mut enter = false;
    let mut cancel = false;
    for (entity, action) in &buttons {
        if !hits.hit(entity) {
            continue;
        }
        match *action {
            RealmAction::Row(row) => {
                let Some(name) = realm_at(&realms, row) else {
                    continue;
                };
                // `RealmSelectButton_OnClick` also resets the refresh timer — a player working
                // down the list should not have it re-sort under them every five seconds.
                realms.refresh_in = super::REFRESH_SECS;
                let double = last_click
                    .as_ref()
                    .is_some_and(|(n, at)| *n == name && now - at < DOUBLE_CLICK_SECS);
                *last_click = Some((name.clone(), now));
                realms.select(&name);
                if double {
                    enter = true;
                }
            }
            RealmAction::Ok => enter = true,
            RealmAction::Cancel => cancel = true,
            // `realm_set_primary_key` (0x46e9b0) — move-to-front, and the clicked column keeps
            // the direction it already had unless it was already primary. See `Sort::click`.
            RealmAction::Sort(key) => realms.sort.click(key),
        }
    }
    if enter {
        try_enter(&mut realms, &choice, &mut sounds);
    }
    if cancel {
        // `RealmList_OnCancel`. Cancelling the realm list drops the logon: there is no screen
        // between here and the login one.
        sounds.write(GlueSound("gsLoginChangeRealmCancel"));
        let _ = choice.0.send(RealmRequest::Abandon);
        next.set(ClientState::Login);
    }
}

/// `RealmList_OnKeyDown` (ESCAPE / ENTER), plus arrow-key row cycling and the wheel.
pub(super) fn keys(
    keys: Res<ButtonInput<KeyCode>>,
    mut wheel: MessageReader<MouseWheel>,
    mut realms: ResMut<Realms>,
    choice: Res<RealmChoice>,
    mut next: ResMut<NextState<ClientState>>,
    mut sounds: MessageWriter<GlueSound>,
) {
    if keys.just_pressed(KeyCode::Escape) {
        sounds.write(GlueSound("gsLoginChangeRealmCancel"));
        let _ = choice.0.send(RealmRequest::Abandon);
        next.set(ClientState::Login);
        return;
    }
    if keys.just_pressed(KeyCode::Enter) || keys.just_pressed(KeyCode::NumpadEnter) {
        try_enter(&mut realms, &choice, &mut sounds);
        return;
    }

    let rows = realms.rows();
    if rows.is_empty() {
        return;
    }
    let back = keys.just_pressed(KeyCode::ArrowUp);
    let fwd = keys.just_pressed(KeyCode::ArrowDown);
    if back || fwd {
        let cur = realms
            .selected()
            .and_then(|sel| rows.iter().position(|&i| realms.realms[i].name == sel.name))
            .unwrap_or(0);
        let n = rows.len();
        let to = if back {
            (cur + n - 1) % n
        } else {
            (cur + 1) % n
        };
        let name = realms.realms[rows[to]].name.clone();
        realms.select(&name);
        scroll_into_view(&mut realms, to, n);
    }

    // The wheel scrolls the window over the list, in the reference's own 16 px steps translated
    // back to rows (`RealmListScrollFrame_OnVerticalScroll` divides the bar value by
    // `REALM_BUTTON_HEIGHT`, so one notch is one row).
    let mut notches = 0i32;
    for ev in wheel.read() {
        notches -= ev.y.signum() as i32;
    }
    if notches != 0 {
        let max = rows.len().saturating_sub(MAX_ROWS);
        let next_off = (realms.offset as i32 + notches).clamp(0, max as i32) as usize;
        realms.offset = next_off;
    }
}

/// The realm on a given **screen** row, honouring the scroll offset.
fn realm_at(realms: &Realms, row: usize) -> Option<String> {
    let rows = realms.rows();
    rows.get(realms.offset + row)
        .map(|&i| realms.realms[i].name.clone())
}

/// Keep the selected row on screen when the arrows walk off the top or the bottom.
fn scroll_into_view(realms: &mut Realms, row: usize, total: usize) {
    let max = total.saturating_sub(MAX_ROWS);
    if row < realms.offset {
        realms.offset = row;
    } else if row >= realms.offset + MAX_ROWS {
        realms.offset = (row + 1 - MAX_ROWS).min(max);
    }
}

/// `RealmList_OnOk`: play the click and answer the park.
///
/// **Deferred: the `REALM_IS_FULL` confirm.** The reference raises a Yes/No dialog first when the
/// chosen realm's load band reads `Full` *and* you have no characters on it
/// (`GlueDialogTypes["REALM_IS_FULL"]`). Ours enters directly, because the two-button `GlueDialog`
/// that would ask lives inside the login screen (`crate::login::screen::spawn_dialog`) and lifting
/// it into `crate::glue` is its own change — one this screen should not smuggle in. Shipping the
/// check without the dialog would be worse than not having it: OK would silently do nothing.
///
/// The gap is narrow. `Full` is the `0x80` flag sentinel, which vmangos does not set, so the
/// dialog is unreachable against the servers benilla connects to today.
fn try_enter(realms: &mut Realms, choice: &RealmChoice, sounds: &mut MessageWriter<GlueSound>) {
    let Some(realm) = realms.selected() else {
        return;
    };
    if is_down(realm) {
        return; // the reference disables OK for an offline realm
    }
    let name = realm.name.clone();
    sounds.write(GlueSound("gsLoginChangeRealmOK"));
    answer(realms, choice, name);
}
