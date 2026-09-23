//! The realm list's input — the reference's `RealmSelectButton_OnClick`/`OnDoubleClick`,
//! `RealmList_OnOk`/`OnCancel`, `RealmListTab_OnClick`, `SortRealms`, and `RealmList_OnKeyDown`.
//!
//! **Every exit hides the dialog and touches nothing else.** `RealmList_OnOk` is
//! `PlaySound; RealmList:Hide(); ChangeRealm(...)` and `RealmList_OnCancel` is
//! `PlaySound; RealmList:Hide(); RealmListDialogCancelled()` — neither names a screen, because the
//! screen it is standing on is the one the player goes back to (see [`super`]).

use bevy::input::mouse::MouseWheel;
use bevy::prelude::*;

use crate::bindings::WheelNotches;
use crate::net::{RealmChoice, RealmRequest};
use crate::sound::GlueSound;

use super::screen::{RealmAction, MAX_ROWS};
use super::{is_down, Realms};

/// The double-click window — the same conventional interval the select screen uses.
const DOUBLE_CLICK_SECS: f32 = 0.4;

/// Clicks: a row selects (a second one enters), the column headers sort, Okay enters, Cancel and
/// the close X leave.
pub(super) fn clicks(
    buttons: Query<(Entity, &RealmAction)>,
    hits: Res<crate::glue::GlueClicks>,
    mut realms: ResMut<Realms>,
    choice: Res<RealmChoice>,
    mut sounds: MessageWriter<GlueSound>,
    time: Res<Time>,
    mut last_click: Local<Option<(String, f32)>>,
) {
    let now = time.elapsed_secs();
    let mut enter = false;
    let mut leave: Option<bool> = None; // Some(with_sound)
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
            RealmAction::Cancel => leave = Some(true),
            RealmAction::Close => leave = Some(false),
            // `realm_set_primary_key` (0x46e9b0) — move-to-front, and the clicked column keeps
            // the direction it already had unless it was already primary. See `Sort::click`.
            RealmAction::Sort(key) => realms.sort.click(key),
        }
    }
    if enter {
        try_enter(&mut realms, &choice, &mut sounds);
    }
    if let Some(with_sound) = leave {
        do_cancel(&mut realms, &choice, &mut sounds, with_sound);
    }
}

/// `RealmList_OnKeyDown` (ESCAPE / ENTER), plus arrow-key row cycling and the wheel.
pub(super) fn keys(
    keys: Res<ButtonInput<KeyCode>>,
    (mut wheel, mut wheel_carry): (MessageReader<MouseWheel>, Local<WheelNotches>),
    mut realms: ResMut<Realms>,
    choice: Res<RealmChoice>,
    mut sounds: MessageWriter<GlueSound>,
) {
    if keys.just_pressed(KeyCode::Escape) {
        do_cancel(&mut realms, &choice, &mut sounds, true);
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
    let notches = wheel_rows(&mut wheel_carry, wheel.read());
    if notches != 0 {
        let max = rows.len().saturating_sub(MAX_ROWS);
        let next_off = (realms.offset as i32 + notches).clamp(0, max as i32) as usize;
        realms.offset = next_off;
    }
}

/// The rows this frame's wheel messages scroll the list by — positive is DOWN the list, one row
/// per whole notch ([`WheelNotches`]). Each message is normalised to lines in its own unit first:
/// a trackpad sends a gesture as a trickle of `Pixel` messages, and stepping a row per message
/// ran the list to its end on a gentle swipe.
fn wheel_rows<'a>(
    carry: &mut WheelNotches,
    wheel: impl IntoIterator<Item = &'a MouseWheel>,
) -> i32 {
    wheel
        .into_iter()
        .map(|ev| -carry.feed(crate::bindings::wheel_lines(ev.unit, ev.y)))
        .sum()
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

/// `RealmList_OnCancel` — and `RealmListCloseButton`, which differs only in the sound
/// (`with_sound`).
///
/// **The park's answer to `Abandon` is the whole difference between the two contexts**, and
/// neither costs this function a branch — which is exactly the reference's own shape. Its
/// `RealmListDialogCancelled` (`0x46ed20` → `0x46b810`, VERIFIED) opens by comparing the current
/// glue screen's name against `"login"`: **not equal and it returns immediately**, so from
/// character select the native does *nothing at all* — the realmd link, the world session and the
/// operation record are untouched and `Hide()` is the entire effect. Equal, and it tail-jumps
/// `CLoginMgr::Cancel` (`0x5b3320`), closing the realmd socket — **without any `SetGlueScreen`**,
/// because the login screen was never left. Ours matches on both legs: the character park ignores
/// `Abandon`, and the login-side park re-parks, which drops the `Logon` and its socket.
///
/// **One divergence, stated.** In the reference the X reaches only `CancelRealmListQuery`
/// (`0x46ed10` → `0x46b7e0`: cancel the pending `COP_GET_REALMS` record, send nothing, close
/// nothing), so from a login it leaves the realmd link up. Ours abandons on both, because our IO
/// thread *parks* on the question rather than polling for it: a hidden dialog with the thread
/// still blocked at the realm park is precisely the desync this whole redesign exists to remove.
fn do_cancel(
    realms: &mut Realms,
    choice: &RealmChoice,
    sounds: &mut MessageWriter<GlueSound>,
    with_sound: bool,
) {
    if with_sound {
        sounds.write(GlueSound("gsLoginChangeRealmCancel"));
    }
    realms.hide();
    let _ = choice.0.send(RealmRequest::Abandon);
}

/// `RealmList_OnOk`: play the click, hide the frame, answer the park.
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
        return; // nothing highlighted — the reference's Okay is disabled here
    };
    if is_down(realm) {
        return; // the reference disables OK for an offline realm
    }
    let name = realm.name.clone();
    sounds.write(GlueSound("gsLoginChangeRealmOK"));
    realms.hide();
    realms.enter(choice, name);
}

#[cfg(test)]
mod tests {
    use super::*;
    use bevy::input::mouse::MouseScrollUnit;

    fn ev(unit: MouseScrollUnit, y: f32) -> MouseWheel {
        MouseWheel {
            unit,
            x: 0.0,
            y,
            window: Entity::PLACEHOLDER,
        }
    }

    /// **A trackpad trickle scrolls the rows it adds up to, not a row per message** — ten
    /// `Pixel` messages of a tenth of a line apiece are one notch, so at most one row.
    #[test]
    fn a_trackpad_trickle_scrolls_only_the_rows_it_adds_up_to() {
        let mut carry = WheelNotches::default();
        let step = MouseScrollUnit::SCROLL_UNIT_CONVERSION_FACTOR / 10.0;
        let trickle: Vec<MouseWheel> = (0..10).map(|_| ev(MouseScrollUnit::Pixel, -step)).collect();
        let rows: i32 = trickle
            .iter()
            .map(|e| wheel_rows(&mut carry, std::iter::once(e)))
            .sum();
        assert!(
            (0..=1).contains(&rows),
            "one line of travel moved {rows} rows"
        );
    }

    /// A mouse wheel's notch is one `Line` message and one row, as it always was.
    #[test]
    fn a_line_notch_scrolls_one_row() {
        let mut carry = WheelNotches::default();
        assert_eq!(
            wheel_rows(&mut carry, &[ev(MouseScrollUnit::Line, -1.0)]),
            1
        );
        assert_eq!(
            wheel_rows(&mut carry, &[ev(MouseScrollUnit::Line, 1.0)]),
            -1
        );
    }
}
