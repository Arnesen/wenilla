//! The select screen's live refresh — everything that follows the roster or the pointer after
//! [`super::screen`] spawns the tree: the row texts + visibility, the row highlight (hover, and
//! LOCKED on the selected row — the ref's `LockHighlight`), the realm banner, the selected name
//! over the model, the buttons' enabled states, and the glue-booth feed (scene by the selected
//! character's race, the geared look).

use bevy::prelude::*;

use crate::area::AreaTableRes;
use crate::glue::widgets::{GlueDisabled, Hilight};
use crate::glue_strings::GlueStrings;
use crate::net::NetStatus;
use crate::portrait::{GlueLook, GluePreview, SelectLook};

use super::screen::{RealmBanner, RowText, SelectAction, SelectedName, MAX_ROWS};
use super::{class_name, Roster};

/// Refill the row texts + visibility, the selected name, and the realm banner whenever the roster
/// changes (a fresh enum, a selection move) — or when the screen was just (re)spawned (returning
/// from the create screen finds an unchanged roster; the fresh, empty tree must still fill).
#[allow(clippy::type_complexity, clippy::too_many_arguments)]
pub(super) fn refresh_list(
    roster: Res<Roster>,
    areas: Option<Res<AreaTableRes>>,
    strings: Option<Res<GlueStrings>>,
    status: Res<NetStatus>,
    mut rows: Query<(&SelectAction, &mut Visibility), (With<Button>, Without<Hilight>)>,
    mut texts: Query<(&RowText, &mut Text), Without<SelectedName>>,
    mut name: Query<&mut Text, (With<SelectedName>, Without<RealmBanner>, Without<RowText>)>,
    mut banner: Query<&mut Text, (With<RealmBanner>, Without<SelectedName>, Without<RowText>)>,
    spawned: Query<Ref<SelectAction>>,
) {
    let fresh = spawned.iter().any(|r| r.is_added());
    if !roster.is_changed() && !fresh && !status.is_changed() {
        return;
    }
    let empty = GlueStrings::default();
    let strings = strings.as_deref().unwrap_or(&empty);

    // Row visibility: rows with a character show; the rest hide (the ref's enumerate-then-hide).
    for (action, mut vis) in &mut rows {
        if let SelectAction::Row(i) = action {
            *vis = if *i < roster.chars.len() {
                Visibility::Inherited
            } else {
                Visibility::Hidden
            };
        }
    }
    // Row texts: Name; Info = `CHARACTER_SELECT_INFO` "Level %d %s" (class only — the ghost
    // variant appends "(Ghost)"); Location = the zone name off AreaTable.
    for (text, mut t) in &mut texts {
        let (i, kind) = match text {
            RowText::Name(i) => (*i, 0),
            RowText::Info(i) => (*i, 1),
            RowText::Location(i) => (*i, 2),
        };
        let new = match roster.chars.get(i) {
            None => String::new(),
            Some(c) => match kind {
                0 => c.name.clone(),
                1 => {
                    let key = if c.flags & benilla_protocol::CHARACTER_FLAG_GHOST != 0 {
                        ("CHARACTER_SELECT_INFO_GHOST", "Level %d %s (Ghost)")
                    } else {
                        ("CHARACTER_SELECT_INFO", "Level %d %s")
                    };
                    strings
                        .text(key.0, key.1)
                        .replacen("%d", &c.level.to_string(), 1)
                        .replacen("%s", class_name(c.class), 1)
                }
                _ => areas
                    .as_deref()
                    .and_then(|a| a.0.name(c.zone))
                    .unwrap_or_default()
                    .to_string(),
            },
        };
        if t.0 != new {
            t.0 = new;
        }
    }
    // The selected character's name over the model (empty list → empty, the ref's
    // UPDATE_SELECTED_CHARACTER arg 0).
    if let Ok(mut t) = name.single_mut() {
        let new = roster
            .selected_char()
            .map(|c| c.name.clone())
            .unwrap_or_default();
        if t.0 != new {
            t.0 = new;
        }
    }
    // The realm banner: "<name> (PVP)" per the realm type; "(Server down)" while unreachable
    // (the ref's SERVER_DOWN suffix); a plain connecting note before the first roster.
    if let Ok(mut t) = banner.single_mut() {
        let new = match &roster.realm {
            Some(realm) => {
                let suffix = match realm.realm_type {
                    1 => strings.text("PVP_PARENTHESES", "(PVP)"),
                    6 => strings.text("RP_PARENTHESES", "(RP)"),
                    8 => strings.text("RPPVP_PARENTHESES", "(RPPVP)"),
                    _ => "",
                };
                let down = if status.last_reason.is_some() && roster.pending_pick.is_none() {
                    format!("\n({})", strings.text("SERVER_DOWN", "Server down"))
                } else {
                    String::new()
                };
                format!("{} {}{down}", realm.name, suffix)
                    .trim_end()
                    .to_string()
            }
            None => match &status.last_reason {
                Some(_) => strings.text("SERVER_DOWN", "Server down").to_string(),
                None => "Connecting…".to_string(),
            },
        };
        if t.0 != new {
            t.0 = new;
        }
    }
}

/// Per-frame interaction visuals + button states: the row highlight (hover ∪ selected — the ref's
/// `LockHighlight` on the selected row), and the enabled states — Enter World / Delete disable on
/// an empty list (the ref's `UpdateCharacterList`), Create hides at the 10-cap or disconnected,
/// Change Realm stays disabled (decision 0465 §6).
#[allow(clippy::type_complexity)]
pub(super) fn refresh_banner_and_buttons(
    roster: Res<Roster>,
    mut rows: Query<(&SelectAction, &Interaction, &Children), With<Button>>,
    mut hilights: Query<&mut Visibility, With<Hilight>>,
    mut disables: Query<(&SelectAction, &mut GlueDisabled)>,
    mut create_vis: Query<
        (&SelectAction, &mut Visibility),
        (With<crate::glue::widgets::GlueBtn>, Without<Hilight>),
    >,
) {
    for (action, interaction, children) in &mut rows {
        let SelectAction::Row(i) = action else {
            continue;
        };
        let lit = roster.selected() == Some(*i) || *interaction != Interaction::None;
        for child in children {
            if let Ok(mut vis) = hilights.get_mut(*child) {
                *vis = if lit {
                    Visibility::Inherited
                } else {
                    Visibility::Hidden
                };
            }
        }
    }
    let have_chars = !roster.chars.is_empty();
    for (action, mut disabled) in &mut disables {
        let want = match action {
            SelectAction::EnterWorld | SelectAction::Delete => !have_chars,
            SelectAction::ChangeRealm => true,
            _ => false,
        };
        if disabled.0 != want {
            disabled.0 = want;
        }
    }
    // Create New Character shows while the realm answered and a slot is free (the ref hides it
    // disconnected or at MAX_CHARACTERS_PER_REALM = 10).
    let show_create = roster.realm.is_some() && roster.chars.len() < MAX_ROWS;
    for (action, mut vis) in &mut create_vis {
        if matches!(action, SelectAction::CreateChar) {
            *vis = if show_create {
                Visibility::Inherited
            } else {
                Visibility::Hidden
            };
        }
    }
}

/// Feed the glue booth from the roster (decision 0465): the scene is the SELECTED character's
/// race's (the ref's `SetBackgroundModel` on the enum fileString — Orc before a list arrives or
/// with an empty account, the ref's OnLoad default), the look its geared enum record. Runs every
/// frame (cheap compares inside `GluePreview` writers — the builder keys on value change).
pub(super) fn feed_glue_preview(
    roster: Res<Roster>,
    mut preview: ResMut<GluePreview>,
    mut showing: Local<Option<u64>>,
) {
    // **The facing belongs to the displayed character, not to the screen** (1533): turning one
    // character must not hand its angle to the next one you click, and a fresh character faces the
    // stage's own forward until you turn it. `GluePreview::yaw` is one resource shared by all three
    // glue screens, so the reset lives at the selection edge rather than in the resource.
    //
    // Keyed on the selection **counter**, not on who is shown: the ref's `SelectCharacter` zeroes
    // the facing unconditionally (`0x472950`, above the already-built discriminator), so clicking
    // the row you are already on re-squares the character too. See `Roster::select_seq`.
    if *showing != Some(roster.select_seq) {
        *showing = Some(roster.select_seq);
        preview.yaw = 0.0;
    }
    let (race, look) = match roster.selected_char() {
        Some(c) => (c.race, Some(GlueLook::Select(SelectLook::from(c)))),
        None => (2, None), // UI_Orc — the ref's initial/empty-account scene
    };
    let scene = Some(crate::portrait::GlueScene::Race(race));
    if preview.scene != scene {
        preview.scene = scene;
    }
    if preview.look != look {
        preview.look = look;
    }
}
