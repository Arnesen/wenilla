//! The **character-select AddOns screen** (decision 1197) — the glue half of the director's ask,
//! and the button 1191 §5 recorded as *"not hidden; never built"*.
//!
//! The reference's is `Interface\GlueXML\AddonList.xml/.lua`, opened by
//! `CharacterSelectAddonsButton`. We have that Lua (read off the patch chain) and it is the
//! authority for **behaviour**; we do not have its XML, so the geometry here is ours, derived from
//! the two constants the Lua does carry: `ADDON_BUTTON_HEIGHT = 16` and
//! `MAX_ADDONS_DISPLAYED = 19` — a 19-row list at 16 px pitch, 304 px of list. Everything else is
//! sized around that and stated as ours rather than passed off as transcribed.
//!
//! ## What the reference does, and what we do
//!
//! `AddonList_Update` per row: a tri-state checkbox, the title (`## Title` else the folder name)
//! coloured **gold when loadable, red when enabled-but-broken, grey when disabled**, the status
//! token (`getglobal("ADDON_"..reason)`), a URL/update button, and a security icon. Hovering a row
//! raises `AddonTooltip` with title + notes + `ADDON_DEPENDENCIES`.
//!
//! Carried: the list, the checkbox, the three-colour title rule, the status column, the
//! hover tooltip with notes and dependencies, scrolling, Enable All / Disable All, and
//! Okay/Cancel with Cancel meaning *discard* (the reference's `ResetAddOns`).
//!
//! **Not carried, each for a stated reason:**
//! - the **security icon** — `## Secure: 1` is Blizzard's own signature marker and nothing a
//!   player installs carries it honestly (1191 §6 already refused to treat it as granting
//!   anything). An icon that is always "insecure" tells nobody anything.
//! - the **URL / update buttons** — they launch a browser. That is an outward-facing action from a
//!   game client, and it is the director's call to make, not this screen's. `## URL` is shown in
//!   the tooltip instead, where it is information rather than a trigger.
//! - the **character dropdown** (`AddonCharacterDropDown`, the "All" vs per-character switch).
//!   Ours edits the **selected** character's enable file, because that is the file the world-entry
//!   walk will read (1191 §7). "All characters" needs a glue dropdown widget we do not have and a
//!   fan-out write across every roster file; it is queued, not forgotten.
//! - `SetScriptMemory` / `GetScriptMemory` — a 1.12 Lua-heap dial with no equivalent here.
//!
//! ## Why it reads the folder rather than the VM
//!
//! At character select no addon has been *loaded* — `load_third_party` is a world-entry step
//! (1191 §2) — so `GetNumAddOns()` would answer 0. The screen asks
//! [`crate::ui_script::addons::installed`] instead: the same discovery, the same manifest parser,
//! the same enable file. Two views of one folder, never two folders.

use bevy::prelude::*;

use crate::glue::art::{GlueArt, DIM, GOLD};
use crate::glue::backdrop::{backdrop_border, tiled_bg_node};
use crate::glue::widgets::{glue_button, outlined_text, overlay, GlueBtnKind, GlueText, Hilight};
use crate::glue_strings::GlueStrings;
use crate::ui_script::addons::{self, InstalledAddOn};

use super::wow_font;

/// `MAX_ADDONS_DISPLAYED` (AddonList.lua l.2) — the reference's own row count.
const MAX_ROWS: usize = 19;
/// `ADDON_BUTTON_HEIGHT` (AddonList.lua l.1).
const ROW_H: f32 = 16.0;
/// Ours: the list is 19×16 = 304 tall, and the panel is that plus a title and a button row.
const PANEL_W: f32 = 560.0;
const PANEL_H: f32 = 420.0;
const LIST_TOP: f32 = 56.0;
const LIST_LEFT: f32 = 24.0;
const CHECK: f32 = 14.0;

/// A red that reads as "this is enabled and will not load" — `SetTextColor(1.0, 0.1, 0.1)`,
/// AddonList.lua's own value.
const BROKEN: Color = Color::srgb(1.0, 0.1, 0.1);

/// The screen's state. Opened by the select screen's AddOns button, driven by
/// [`drive_addons_panel`], cleared on leaving character select.
///
/// `staged` is the whole point of Okay/Cancel: edits go here, and only Okay writes the file. The
/// reference does the same with `SaveAddOns`/`ResetAddOns` over the client's in-memory records.
#[derive(Resource, Default)]
pub(super) struct AddonsPanel {
    pub(super) open: bool,
    /// The installed list as read at open — never re-read while up, so a row's index is stable.
    list: Vec<InstalledAddOn>,
    /// The edited enable state, parallel to `list`.
    staged: Vec<bool>,
    /// First visible row (`AddonList.offset`).
    offset: usize,
    /// The row the cursor is over, for the tooltip.
    hovered: Option<usize>,
    root: Option<Entity>,
    spawned_s: f32,
    /// Set while the spawned tree no longer matches `staged`/`offset`/`hovered` — one flag rather
    /// than a diff, because a 19-row list is cheaper to respawn than to reconcile.
    dirty: bool,
}

impl AddonsPanel {
    /// Open for `identity`, reading the folder fresh. The reference's `AddonList_OnShow` also
    /// re-reads (`AddonList_Update`), so a folder that changed under us is picked up.
    pub(super) fn open_for(&mut self, identity: Option<&(String, String)>) {
        self.list = addons::installed(identity);
        self.staged = self.list.iter().map(|a| a.enabled).collect();
        self.offset = 0;
        self.hovered = None;
        self.open = true;
        self.dirty = true;
    }

    pub(super) fn close(&mut self) {
        self.open = false;
        self.list.clear();
        self.staged.clear();
        self.hovered = None;
        self.dirty = true;
    }

    /// Are there any installed addons at all? The reference hides its button when not
    /// (`UpdateAddonButton`), and so do we — a button that opens an empty list is worse than none.
    ///
    /// **Answered once per process, not per call.** The screen asks this while it is spawning its
    /// tree, and the tree respawns on every glue-scale change — which during a window drag-resize
    /// is *every frame*. Reading the folder there means a `read_dir` plus a `.toc` parse per addon
    /// per frame for as long as the director holds the mouse down. Installing or removing an addon
    /// mid-session is not a thing that happens, so the cache never needs invalidating; the panel
    /// itself re-reads the folder on every open ([`Self::open_for`]), which is where freshness
    /// actually matters.
    pub(super) fn any_installed() -> bool {
        static ANY: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
        *ANY.get_or_init(|| !addons::installed(None).is_empty())
    }

    /// The rows currently visible, as `(index, addon, enabled)`.
    fn visible(&self) -> impl Iterator<Item = (usize, &InstalledAddOn, bool)> {
        self.list
            .iter()
            .enumerate()
            .skip(self.offset)
            .take(MAX_ROWS)
            .map(|(i, a)| (i, a, self.staged.get(i).copied().unwrap_or(a.enabled)))
    }

    /// The highest first-row index that still fills the list.
    fn max_offset(&self) -> usize {
        self.list.len().saturating_sub(MAX_ROWS)
    }

    /// **Why a row will not load** — the string the status column shows and the colour rule keys on.
    ///
    /// These are the reference's status LABELS, not its tokens, because the glue side has no VM to
    /// splice `getglobal("ADDON_"..reason)` against (1197 §3). Each is the exact value its token
    /// carries in the shipped `GlobalStrings.lua`, and
    /// `the_status_labels_are_the_reference_globalstrings_values` pins all four so they cannot
    /// drift from the ESC-menu twin, which DOES go through the globals:
    ///
    /// | here | token | `GlobalStrings.lua` |
    /// |---|---|---|
    /// | `"Disabled"` | `ADDON_DISABLED` | `"Disabled"` |
    /// | `"Dependency missing"` | `ADDON_DEP_MISSING` | `"Dependency missing"` |
    /// | `"Dependency disabled"` | `ADDON_DEP_DISABLED` | `"Dependency disabled"` |
    /// | `"Out of date"` | `ADDON_INTERFACE_VERSION` | `"Out of date"` |
    ///
    /// The last row is why this comment was rewritten: it used to name the token `INCOMPATIBLE`,
    /// **which does not exist** — the reference has no `ADDON_INCOMPATIBLE`, and a reader who
    /// trusted the name would have spliced a nil global. Out-of-date is reported and never enforced
    /// (1191 §6), which is why such a row still shows gold-if-enabled: the status column says so and
    /// the client still loads it, and those two facts must not contradict each other.
    fn reason(&self, i: usize) -> Option<&'static str> {
        let addon = self.list.get(i)?;
        if !self.staged.get(i).copied().unwrap_or(addon.enabled) {
            return Some("Disabled");
        }
        for dep in &addon.dependencies {
            match self
                .list
                .iter()
                .position(|a| a.name.eq_ignore_ascii_case(dep))
            {
                None => return Some("Dependency missing"),
                Some(j) if !self.staged.get(j).copied().unwrap_or(true) => {
                    return Some("Dependency disabled")
                }
                Some(_) => {}
            }
        }
        addon.out_of_date().then_some("Out of date")
    }
}

/// The panel's clickable parts.
#[derive(Component, Clone, Copy, PartialEq, Eq)]
pub(super) enum AddonsAction {
    /// A list row — toggles its checkbox (`AddonList_Enable`).
    Row(usize),
    EnableAll,
    DisableAll,
    Okay,
    Cancel,
    ScrollUp,
    ScrollDown,
}

/// The panel root (despawned whole on close).
#[derive(Component)]
struct AddonsUi;

/// Spawn/despawn the panel, run its flows, and repaint when anything it shows has changed.
///
/// Ordered before the select screen's own click handling so a click that lands on the panel is
/// never also read as a click on the screen behind it.
#[allow(clippy::too_many_arguments)]
pub(super) fn drive_addons_panel(
    mut commands: Commands,
    mut panel: ResMut<AddonsPanel>,
    art: Res<GlueArt>,
    assets: Res<AssetServer>,
    strings: Option<Res<GlueStrings>>,
    identity: Res<crate::ui_script::AddOnIdentity>,
    roster: Res<super::Roster>,
    keys: Res<ButtonInput<KeyCode>>,
    mut wheel: MessageReader<bevy::input::mouse::MouseWheel>,
    presses: Query<(&AddonsAction, &Interaction), Changed<Interaction>>,
    hovers: Query<(&AddonsAction, &Interaction)>,
    window: Query<&Window, With<bevy::window::PrimaryWindow>>,
) {
    if !panel.open {
        if let Some(root) = panel.root.take() {
            commands.entity(root).despawn();
        }
        wheel.clear();
        return;
    }

    // ── the flows ─────────────────────────────────────────────────────────────────────────────
    let mut close_and_save = false;
    let mut close_and_discard = false;
    for (action, interaction) in &presses {
        if *interaction != Interaction::Pressed {
            continue;
        }
        match *action {
            AddonsAction::Row(i) => {
                if let Some(slot) = panel.staged.get_mut(i) {
                    *slot = !*slot;
                    panel.dirty = true;
                }
            }
            AddonsAction::EnableAll => {
                panel.staged.iter_mut().for_each(|s| *s = true);
                panel.dirty = true;
            }
            AddonsAction::DisableAll => {
                panel.staged.iter_mut().for_each(|s| *s = false);
                panel.dirty = true;
            }
            AddonsAction::Okay => close_and_save = true,
            AddonsAction::Cancel => close_and_discard = true,
            AddonsAction::ScrollUp => scroll(&mut panel, -1),
            AddonsAction::ScrollDown => scroll(&mut panel, 1),
        }
    }
    // `AddonList_OnKeyDown`: ESCAPE cancels, ENTER accepts.
    if keys.just_pressed(KeyCode::Escape) {
        close_and_discard = true;
    }
    if keys.just_pressed(KeyCode::Enter) || keys.just_pressed(KeyCode::NumpadEnter) {
        close_and_save = true;
    }
    for ev in wheel.read() {
        if ev.y != 0.0 {
            scroll(&mut panel, if ev.y > 0.0 { -1 } else { 1 });
        }
    }

    // The hovered row, for the tooltip — read every frame rather than on change, because a row
    // that scrolls out from under a stationary cursor changes what is hovered without an event.
    let hovered = hovers.iter().find_map(|(a, i)| match (a, i) {
        (AddonsAction::Row(r), Interaction::Hovered | Interaction::Pressed) => Some(*r),
        _ => None,
    });
    if panel.hovered != hovered {
        panel.hovered = hovered;
        panel.dirty = true;
    }

    if close_and_save {
        let states: Vec<(String, bool)> = panel
            .list
            .iter()
            .zip(panel.staged.iter())
            .map(|(a, &on)| (a.name.clone(), on))
            .collect();
        // The identity the world-entry walk will read. `AddOnIdentity` is only filled once a
        // character has entered the world, so at first launch the roster's own selection is the
        // one that matters — the same resolution `load_ingame_ui_on_world_entry` performs.
        let who = identity
            .0
            .clone()
            .or_else(|| crate::ui_macro::identity(&roster));
        addons::write_enable_state(who.as_ref(), &states);
        panel.close();
        return;
    }
    if close_and_discard {
        panel.close();
        return;
    }

    // ── the tree ──────────────────────────────────────────────────────────────────────────────
    let s = crate::glue::screen_scale(window.single().ok());
    let stale = panel.root.is_some() && (panel.spawned_s != s || panel.dirty);
    if stale {
        if let Some(root) = panel.root.take() {
            commands.entity(root).despawn();
        }
    }
    if panel.root.is_none() {
        let empty = GlueStrings::default();
        let strings = strings.as_deref().unwrap_or(&empty);
        panel.root = Some(spawn_panel(
            &mut commands,
            &art,
            &assets,
            strings,
            &panel,
            s,
        ));
        panel.spawned_s = s;
        panel.dirty = false;
    }
}

/// Move the first visible row by `delta`, clamped — the reference's `AddonList.offset`.
fn scroll(panel: &mut AddonsPanel, delta: i32) {
    let max = panel.max_offset();
    let next = (panel.offset as i32 + delta).clamp(0, max as i32) as usize;
    if next != panel.offset {
        panel.offset = next;
        panel.dirty = true;
    }
}

fn spawn_panel(
    commands: &mut Commands,
    art: &GlueArt,
    assets: &AssetServer,
    strings: &GlueStrings,
    panel: &AddonsPanel,
    s: f32,
) -> Entity {
    let px = |v: f32| Val::Px(v * s);
    let font = wow_font(assets);
    let title = strings.text("ADDONS", "AddOns");

    commands
        .spawn((
            AddonsUi,
            GlobalZIndex(1200), // over the select screen's 1100, like the delete dialog
            Node {
                width: Val::Percent(100.0),
                height: Val::Percent(100.0),
                justify_content: JustifyContent::Center,
                align_items: AlignItems::Center,
                ..default()
            },
        ))
        .with_children(|overlay_ui| {
            let mut boxed = overlay_ui.spawn(Node {
                width: px(PANEL_W),
                height: px(PANEL_H),
                ..default()
            });
            boxed.with_children(|b| {
                // The dialog backdrop, exactly as the delete dialog builds it.
                if let (Some(bg), Some(border)) = (&art.dialog_bg, &art.dialog_border) {
                    b.spawn((
                        tiled_bg_node(bg.clone(), 32.0, s, Color::WHITE),
                        Node {
                            position_type: PositionType::Absolute,
                            left: px(11.0),
                            right: px(12.0),
                            top: px(12.0),
                            bottom: px(11.0),
                            ..default()
                        },
                    ));
                    backdrop_border(b, border, 32.0, Color::WHITE);
                } else {
                    b.spawn((
                        BackgroundColor(Color::srgba(0.05, 0.05, 0.08, 0.95)),
                        overlay(),
                    ));
                }

                // Title.
                outlined_text(
                    b,
                    Node {
                        position_type: PositionType::Absolute,
                        left: px(0.0),
                        right: px(0.0),
                        top: px(22.0),
                        justify_content: JustifyContent::Center,
                        ..default()
                    },
                    (),
                    (),
                    GlueText {
                        text: title,
                        size: 18.0,
                        color: GOLD,
                        wrap: false,
                    },
                    &font,
                    s,
                );

                // The rows.
                for (slot, (index, addon, enabled)) in panel.visible().enumerate() {
                    let top = LIST_TOP + slot as f32 * ROW_H;
                    let reason = panel.reason(index);
                    // AddonList.lua's three-colour rule: gold when it will load, red when the
                    // player has it ON and it still will not, grey when they turned it off.
                    let colour = match (enabled, reason) {
                        (true, None) => GOLD,
                        (true, Some("Out of date")) => GOLD,
                        (true, _) => BROKEN,
                        (false, _) => DIM,
                    };
                    let mut row = b.spawn((
                        AddonsAction::Row(index),
                        Button,
                        Node {
                            position_type: PositionType::Absolute,
                            left: px(LIST_LEFT),
                            top: px(top),
                            width: px(PANEL_W - LIST_LEFT * 2.0),
                            height: px(ROW_H),
                            align_items: AlignItems::Center,
                            ..default()
                        },
                    ));
                    row.with_children(|r| {
                        // The checkbox. The glue art's own `UI-CheckBox-*` when we have it, a
                        // drawn box when we do not — the same fallback every glue control takes.
                        let check = &art.checkbox;
                        let face = match (check.as_ref(), enabled) {
                            (Some(c), true) => Some(c.checked.clone()),
                            (Some(c), false) => Some(c.up.clone()),
                            (None, _) => None,
                        };
                        let mut cb = r.spawn(Node {
                            position_type: PositionType::Absolute,
                            left: px(0.0),
                            top: px((ROW_H - CHECK) / 2.0),
                            width: px(CHECK),
                            height: px(CHECK),
                            ..default()
                        });
                        match face {
                            Some(image) => {
                                cb.insert(ImageNode::new(image));
                            }
                            None => {
                                cb.insert(BackgroundColor(if enabled { GOLD } else { DIM }));
                            }
                        }
                        // Title.
                        outlined_text(
                            r,
                            Node {
                                position_type: PositionType::Absolute,
                                left: px(CHECK + 8.0),
                                top: px(0.0),
                                ..default()
                            },
                            (),
                            (),
                            GlueText {
                                text: addon.display_title(),
                                size: 12.0,
                                color: colour,
                                wrap: false,
                            },
                            &font,
                            s,
                        );
                        // Status — the reference's `getglobal("ADDON_"..reason)` column.
                        if let Some(reason) = reason {
                            outlined_text(
                                r,
                                Node {
                                    position_type: PositionType::Absolute,
                                    right: px(0.0),
                                    top: px(0.0),
                                    ..default()
                                },
                                (),
                                (),
                                GlueText {
                                    text: reason,
                                    size: 11.0,
                                    color: DIM,
                                    wrap: false,
                                },
                                &font,
                                s,
                            );
                        }
                        if let Some(hi) = &art.hilight {
                            r.spawn((
                                Hilight,
                                Visibility::Hidden,
                                MaterialNode(hi.clone()),
                                overlay(),
                            ));
                        }
                    });
                }

                // The scroll pair, shown only when the list is longer than the window — the
                // reference's `GlueScrollFrame_Update` hides its bar on the same condition.
                if panel.max_offset() > 0 {
                    for (action, caption, top) in [
                        (AddonsAction::ScrollUp, "-", LIST_TOP),
                        (
                            AddonsAction::ScrollDown,
                            "+",
                            LIST_TOP + (MAX_ROWS as f32 - 1.0) * ROW_H,
                        ),
                    ] {
                        b.spawn((
                            action,
                            Button,
                            BackgroundColor(Color::srgba(1.0, 1.0, 1.0, 0.10)),
                            Node {
                                position_type: PositionType::Absolute,
                                right: px(6.0),
                                top: px(top),
                                width: px(14.0),
                                height: px(14.0),
                                justify_content: JustifyContent::Center,
                                align_items: AlignItems::Center,
                                ..default()
                            },
                        ))
                        .with_children(|sb| {
                            outlined_text(
                                sb,
                                Node::default(),
                                (),
                                (),
                                GlueText {
                                    text: caption,
                                    size: 11.0,
                                    color: GOLD,
                                    wrap: false,
                                },
                                &font,
                                s,
                            );
                        });
                    }
                }

                // The hover tooltip: title, notes, dependencies — `AddonTooltip_Update`'s three
                // lines, in its order.
                if let Some(i) = panel.hovered.and_then(|i| panel.list.get(i)) {
                    let deps = if i.dependencies.is_empty() {
                        String::new()
                    } else {
                        format!("Dependencies: {}", i.dependencies.join(", "))
                    };
                    let body = [
                        i.notes.clone().unwrap_or_default(),
                        deps,
                        i.url.clone().unwrap_or_default(),
                    ]
                    .into_iter()
                    .filter(|l| !l.is_empty())
                    .collect::<Vec<_>>()
                    .join("\n");
                    if !body.is_empty() {
                        outlined_text(
                            b,
                            Node {
                                position_type: PositionType::Absolute,
                                left: px(LIST_LEFT),
                                right: px(LIST_LEFT),
                                bottom: px(58.0),
                                ..default()
                            },
                            (),
                            (),
                            GlueText {
                                text: &body,
                                size: 11.0,
                                color: Color::WHITE,
                                wrap: true,
                            },
                            &font,
                            s,
                        );
                    }
                }

                // The button row.
                b.spawn(Node {
                    position_type: PositionType::Absolute,
                    left: px(24.0),
                    right: px(24.0),
                    bottom: px(20.0),
                    height: px(28.0),
                    justify_content: JustifyContent::SpaceBetween,
                    align_items: AlignItems::Center,
                    ..default()
                })
                .with_children(|row| {
                    for (action, caption) in [
                        (AddonsAction::EnableAll, "Enable All"),
                        (AddonsAction::DisableAll, "Disable All"),
                        (AddonsAction::Cancel, "Cancel"),
                        (AddonsAction::Okay, "Okay"),
                    ] {
                        glue_button(
                            row,
                            art,
                            &font,
                            action,
                            caption,
                            118.0,
                            28.0,
                            GlueBtnKind::Small,
                            s,
                        );
                    }
                });
            });
        })
        .id()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn addon(name: &str, deps: &[&str], iface: &[u32]) -> InstalledAddOn {
        InstalledAddOn {
            name: name.into(),
            title: None,
            notes: None,
            url: None,
            dependencies: deps.iter().map(|d| (*d).to_string()).collect(),
            interface: iface.to_vec(),
            load_on_demand: false,
            enabled: true,
        }
    }

    fn panel(list: Vec<InstalledAddOn>) -> AddonsPanel {
        let staged = list.iter().map(|a| a.enabled).collect();
        AddonsPanel {
            open: true,
            list,
            staged,
            ..Default::default()
        }
    }

    /// **Every label this panel shows is the value its reference token carries**, so the glue
    /// screen and the ESC-menu screen cannot say different things about the same addon.
    ///
    /// The glue side has no VM to splice `getglobal("ADDON_"..reason)` against (1197 §3), so it
    /// hardcodes the English. That is fine and it is also exactly how the two screens drift: the
    /// twin resolves through `GlobalStrings.lua` and this one does not, so nothing but a test
    /// connects them. The values below are quoted from the shipped 1.12 `GlobalStrings.lua`
    /// (l.16-31, the `ADDON_*` block) — which the client itself loads at boot.
    ///
    /// This test exists because the comment on [`AddonsPanel::reason`] used to name the token
    /// `INCOMPATIBLE`, and **there is no `ADDON_INCOMPATIBLE`**; the real one is
    /// `ADDON_INTERFACE_VERSION`. A wrong token name is invisible until somebody splices it.
    #[test]
    fn the_status_labels_are_the_reference_globalstrings_values() {
        let mut p = panel(vec![
            addon("Off", &[], &[11200]),
            addon("Lib", &[], &[11200]),
            addon("Needs", &["Lib"], &[11200]),
            addon("Orphan", &["Nowhere"], &[11200]),
            addon("Old", &[], &[11100]),
        ]);
        p.staged[0] = false;
        p.staged[1] = false;

        // (label, the token it must equal, the value GlobalStrings.lua gives that token)
        for (got, token, want) in [
            (p.reason(0), "ADDON_DISABLED", "Disabled"),
            (p.reason(2), "ADDON_DEP_DISABLED", "Dependency disabled"),
            (p.reason(3), "ADDON_DEP_MISSING", "Dependency missing"),
            (p.reason(4), "ADDON_INTERFACE_VERSION", "Out of date"),
        ] {
            assert_eq!(
                got,
                Some(want),
                "this label must be {token}'s value verbatim, or the ESC-menu twin — which \
                 resolves the token through GlobalStrings — will show a different string"
            );
        }
    }

    /// The status column's rule, which is also the title-colour rule — the reference's own
    /// `reason` token set, in its own precedence.
    ///
    /// The ordering is the part worth pinning: a row the player turned OFF reads `Disabled` and
    /// nothing else, even when its dependency is also missing. The reference does the same
    /// (`GetAddOnInfo` reports `DISABLED` before the dependency walk runs), and the alternative
    /// tells a player to go fix a dependency for an addon they deliberately switched off.
    #[test]
    fn the_status_column_reports_the_references_reason_in_its_own_precedence() {
        let mut p = panel(vec![
            addon("Solo", &[], &[11200]),
            addon("Lib", &[], &[11200]),
            addon("Needs", &["Lib"], &[11200]),
            addon("Orphan", &["Nowhere"], &[11200]),
            addon("Old", &[], &[11100]),
            addon("Silent", &[], &[]),
        ]);

        assert_eq!(p.reason(0), None, "nothing wrong: no status, gold title");
        assert_eq!(p.reason(2), None, "its dependency is installed and enabled");
        assert_eq!(p.reason(3), Some("Dependency missing"));
        assert_eq!(
            p.reason(4),
            Some("Out of date"),
            "reported — 1191 §6 does not ENFORCE it, and the two must not contradict"
        );
        assert_eq!(
            p.reason(5),
            None,
            "a manifest with no `## Interface` is silent, not out of date"
        );

        // Turning the library off makes its dependent unloadable, and the dependent says so.
        p.staged[1] = false;
        assert_eq!(p.reason(1), Some("Disabled"));
        assert_eq!(p.reason(2), Some("Dependency disabled"));

        // ...and a row the player turned off reports only that, even with a broken dependency.
        p.staged[3] = false;
        assert_eq!(p.reason(3), Some("Disabled"));
    }

    /// Okay/Cancel is a **staged** edit: nothing the panel does touches the enable file until
    /// Okay. `Cancel` is the reference's `ResetAddOns`, and `close` is what implements it.
    #[test]
    fn edits_are_staged_and_cancel_discards_them() {
        let mut p = panel(vec![addon("A", &[], &[11200]), addon("B", &[], &[11200])]);
        assert_eq!(p.staged, vec![true, true]);
        p.staged[0] = false;
        assert!(
            p.list[0].enabled,
            "the read-in list is never mutated — only `staged` is, which is what makes Cancel free"
        );
        p.close();
        assert!(!p.open);
        assert!(
            p.staged.is_empty(),
            "a cancelled edit leaves nothing behind"
        );
    }

    /// Scrolling clamps at both ends and never moves a list that fits.
    #[test]
    fn the_offset_clamps_to_the_list() {
        let mut short = panel((0..5).map(|i| addon(&format!("A{i}"), &[], &[])).collect());
        assert_eq!(short.max_offset(), 0);
        scroll(&mut short, 1);
        assert_eq!(short.offset, 0, "a list that fits does not scroll");

        let mut long = panel(
            (0..MAX_ROWS + 4)
                .map(|i| addon(&format!("A{i}"), &[], &[]))
                .collect(),
        );
        assert_eq!(long.max_offset(), 4);
        scroll(&mut long, 10);
        assert_eq!(long.offset, 4, "clamped at the bottom, not past it");
        assert_eq!(long.visible().count(), MAX_ROWS);
        scroll(&mut long, -100);
        assert_eq!(long.offset, 0);
    }
}
