//! The create screen's component vocabulary — the markers [`super::screen`] spawns and
//! [`super::refresh`] drives. Pure data; no systems. (The screen-agnostic widget markers —
//! `Hilight`, `ArtSwap`, `GlueBtn`, … — live in [`crate::glue::widgets`], decision 0465.)

use bevy::prelude::*;

/// The screen root (despawned whole on exit); `s` is the glue scale the tree was baked at —
/// [`super::screen::rescale_screen`] rebuilds it when a window resize changes the scale.
#[derive(Component)]
pub(super) struct CharCreateUi {
    pub(super) s: f32,
}
/// The status line (also written by [`super::create_result`]).
#[derive(Component)]
pub(super) struct StatusLine;

/// The three info panels.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum InfoKind {
    Faction,
    Race,
    Class,
}

/// An icon whose texture rect follows the selection.
#[derive(Component)]
pub(super) enum DynIcon {
    /// A race button's icon (rect follows the selected sex).
    Race(u8),
    /// A class slot's icon (the class it maps to follows the selected race).
    ClassSlot(u8),
    /// An info-panel header icon.
    Info(InfoKind),
}

/// The name box's caret bar (the shared `glue_edit_box` chrome spawns it; `refresh_name_caret`
/// blinks it). The create screen has one editable field and no focus model, so its box is always
/// the focused one — unlike login, where the caret follows Tab.
#[derive(Component)]
pub(super) struct NameCaret;

/// A text whose content follows the selection.
#[derive(Component)]
pub(super) enum DynText {
    DialLabel(u8),
    Name,
    InfoTitle(InfoKind),
    InfoBody(InfoKind),
    /// The race panel's ability lines — the ref's separate gold `CharacterCreateRaceAbilityText`,
    /// never part of the white paragraph.
    InfoAbilities,
    /// A class slot's hover label (name follows the mapped class).
    ClassSlotLabel(u8),
}

/// A tint that follows the selected faction: the page backdrop, or a panel's backdrop-bg piece
/// (the ref's `SetBackdropColor` — an `ImageNode` tint with art, a `BackgroundColor` without).
#[derive(Component)]
pub(super) enum DynTint {
    Backdrop,
    BoxFill,
}

/// A dial spinner row (row 4 — facial hair — hides when the race's token is `NONE`, the ref rule).
#[derive(Component)]
pub(super) struct DialRow(pub(super) u8);
/// An info panel's scrollable body (mouse wheel while hovered).
#[derive(Component)]
pub(super) struct InfoScroll;
/// A scrollbar arrow (`GlueScrollUp/DownButtonTemplate`): steps its scroll frame by half a track
/// per click; disabled art at its end stop.
#[derive(Component)]
pub(super) struct ScrollArrow {
    pub(super) scroll: Entity,
    /// `true` = the up button (scrolls toward 0).
    pub(super) up: bool,
    /// The click step, logical px — the ref's `GetHeight()/2` (half the track).
    pub(super) step: f32,
}
/// The scrollbar knob (`UI-ScrollBar-Knob`): rides the scroll fraction along its track; drags.
#[derive(Component)]
pub(super) struct ScrollThumb {
    pub(super) scroll: Entity,
    /// The knob's travel (track minus knob), logical px.
    pub(super) travel: f32,
}
/// A scrollbar piece that hides while its frame has nothing to scroll — the ref's
/// `scrollBarHideable` (the whole slider) + the Top/Bottom track art's range-changed hide.
#[derive(Component)]
pub(super) struct ScrollHides {
    pub(super) scroll: Entity,
}
