//! Standardized in-window debug panel (egui).
//!
//! The panel is a rounded, translucent **window floated off the top-right corner** (a gap from the
//! edges, like the perf pill) — it overlays the 3D view, which renders full-screen underneath and is
//! never letterboxed. Fixed width (no resize handle). Press the backtick key (`` ` ``) to show/hide it.
//! Its look (translucent dark backing + crisp text) is shared with the perf pill and the inspector card
//! via [`OVERLAY_FILL`] / [`overlay_text`], so the dev overlays read as one family.
//!
//! ## How it's wired (so future subsystems slot in cleanly)
//! - [`DebugState`] is one resource grouped into per-subsystem sections ([`ModelDebug`],
//!   [`LightingDebug`], …; new subsystems add sibling fields).
//! - The panel UI ([`debug_panel_ui`]) reads/writes that resource via egui widgets.
//! - Small **apply** systems turn the resource into world changes (e.g. [`apply_model_visibility`]).
//!   Each only does work when its slice of the state actually changes (tracked with a `Local`
//!   snapshot), so leaving the panel open costs nothing. (Lighting is now resolved in `lighting.rs`;
//!   this section is time controls + a readout, not knobs.)
//! - [`EguiPointerOver`] publishes "the mouse is talking to the egui overlays" each pass; the
//!   combined source of truth gameplay reads is [`crate::ui_script::PointerOverUi`] (owned by its
//!   combiner, decision 0026 — dev plugins must be droppable without breaking gameplay reads).
//!
//! Adding a section for a new subsystem is therefore: a `FooDebug` field, a few widgets in the
//! panel, and one `apply_foo` system — no plumbing changes. (Weather is the worked example.)
//!
//! Rendering uses bevy_egui's manual-context mode: auto-creation is disabled in [`DebugPanelPlugin`]
//! and a dedicated full-window overlay camera composites egui over the 3D scene (alpha-blended, no
//! clear). See bevy_egui's `side_panel` example.

use bevy::camera::visibility::RenderLayers;
use bevy::camera::CameraOutputMode;
use bevy::prelude::*;
use bevy::render::render_resource::BlendState;
use bevy_egui::{
    egui, EguiContexts, EguiGlobalSettings, EguiInput, EguiPlugin, EguiPreUpdateSet,
    EguiPrimaryContextPass, PrimaryEguiContext,
};

use benilla_formats::ModelBlend;

use crate::lighting::{ClockSource, GameClock, WowLighting};
use crate::view::ViewDistance;

/// The debug state — resource-only, faithful defaults (decision 0026: the always-present config
/// layer; this module is only its editor).
mod state;
pub use state::DebugState;

/// The per-frame model-visibility apply system (toggles + the far-clip wall-cull + small-prop fade).
mod visibility;
use visibility::apply_model_visibility;

/// The World section — identity/map/zone/position readout + the copy-`.go xyz` affordance.
mod world;
use world::{world_section, WorldReadout};

/// Shared look for the floating dev overlays — this panel, the perf pill, the inspector card — so they
/// read as one family. The backing is a **near-solid dark** so the bright text keeps strong contrast
/// *no matter the scene behind it*: a lightly-translucent box let bright terrain/sky bleed through and
/// washed the text out, making legibility depend on the view. [`overlay_text`] sets the bright primary
/// tone; [`OVERLAY_TEXT_DIM`] the readable secondary one. (Lower the alpha for a more see-through box —
/// at the cost of view-independent contrast.)
pub(crate) const OVERLAY_FILL: egui::Color32 = egui::Color32::from_black_alpha(224);
/// Primary overlay text — a crisp near-white that reads over the translucent fill (egui's dark default
/// is only `gray(140)`). Set as the ui's `override_text_color`; explicit `.color(...)` calls still win.
pub(crate) const OVERLAY_TEXT: egui::Color32 = egui::Color32::from_gray(235);
/// De-emphasised overlay text (ids, distances, hints): dimmer than [`OVERLAY_TEXT`] but still crisp on
/// the translucent backing.
pub(crate) const OVERLAY_TEXT_DIM: egui::Color32 = egui::Color32::from_gray(180);

/// Apply the shared overlay text treatment for the **compact, fixed-size** surfaces (perf pill,
/// inspector card): brighten to [`OVERLAY_TEXT`] and disable wrapping. No-wrap also kills the
/// first-frame layout flash where a fresh auto-sized container hasn't cached its width yet and a label
/// like "60 fps" briefly breaks across two lines. (The resizable debug panel brightens the same way but
/// keeps wrapping, so its labels reflow instead of clipping when narrowed.)
pub(crate) fn overlay_text(ui: &mut egui::Ui) {
    let style = ui.style_mut();
    style.visuals.override_text_color = Some(OVERLAY_TEXT);
    style.wrap_mode = Some(egui::TextWrapMode::Extend);
}

/// The egui dev overlay's half of the pointer arbitration, written each egui pass. The combined
/// truth — [`crate::ui_script::PointerOverUi`] — lives with its combiner in `ui_script` (decision
/// 0026: gameplay must not read dev-owned resources; the arbiter tolerates this half's absence),
/// which OR's this with the player-UI hover. Neither writer depends on the other.
#[derive(Resource, Default)]
pub(crate) struct EguiPointerOver(pub(crate) bool);

/// Strip the **Tab** key from egui's per-frame input so egui never treats it as focus navigation.
///
/// Tab is a bound game key — `TargetNearestEnemy` (decision 0166). Left to itself, egui reads Tab at
/// `begin_pass` (`Focus::begin_pass`) and pulls keyboard focus into whatever focusable widget is on
/// screen — the always-on perf pill — ringing it and then owning the keyboard. Our egui surfaces are
/// mouse-driven dev overlays with no tab-to-next-field need, so we drop Tab before the pass sees it.
/// Bevy's own `ButtonInput<KeyCode>` (what `target::scan` reads) is populated independently from the
/// same winit events, so TAB targeting still fires — this only removes the phantom egui focus.
fn strip_egui_tab_focus(mut inputs: Query<&mut EguiInput>) {
    for mut input in &mut inputs {
        input.events.retain(|e| {
            !matches!(
                e,
                egui::Event::Key {
                    key: egui::Key::Tab,
                    ..
                }
            )
        });
    }
}

fn track_pointer_over_ui(mut contexts: EguiContexts, mut over: ResMut<EguiPointerOver>) -> Result {
    let ctx = contexts.ctx_mut()?;
    over.0 = ctx.is_pointer_over_area() || ctx.wants_pointer_input();
    Ok(())
}

/// Which world-model subsystem a spawned submesh belongs to — lets the panel scope toggles (e.g.
/// hide doodads without touching NPCs).
#[derive(Component, Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum ModelKind {
    Doodad,
    Wmo,
    Creature,
    GameObject,
}

impl ModelKind {
    const ALL: [ModelKind; 4] = [Self::Doodad, Self::Wmo, Self::Creature, Self::GameObject];

    fn index(self) -> usize {
        match self {
            Self::Doodad => 0,
            Self::Wmo => 1,
            Self::Creature => 2,
            Self::GameObject => 3,
        }
    }

    fn label(self) -> &'static str {
        match self {
            Self::Doodad => "Doodads (trees/props)",
            Self::Wmo => "WMOs (buildings)",
            Self::Creature => "Creatures (NPCs)",
            Self::GameObject => "GameObjects",
        }
    }
}

/// Tags every spawned model submesh with the metadata the panel toggles on: its subsystem and its
/// blend mode (the "layer" — opaque trunk vs alpha-cut canopy).
#[derive(Component, Clone, Copy)]
pub struct ModelPart {
    pub kind: ModelKind,
    pub blend: ModelBlend,
}

fn blend_index(b: ModelBlend) -> usize {
    match b {
        ModelBlend::Opaque => 0,
        ModelBlend::AlphaTest => 1,
        ModelBlend::Blend => 2,
        ModelBlend::Mod => 3,
        ModelBlend::Mod2x => 4,
    }
}

const BLEND_LABELS: [&str; 5] = [
    "Opaque (trunk / walls)",
    "AlphaTest (leaf / cutout)",
    "Blend (transparent)",
    "Mod (multiply)",
    "Mod2x (2x multiply / sheen)",
];

/// Minute-of-day (`0..1440`) as `HH:MM`.
fn hhmm(minute: u32) -> String {
    format!("{:02}:{:02}", minute / 60 % 24, minute % 60)
}

fn source_label(s: ClockSource) -> &'static str {
    match s {
        ClockSource::Server => "server clock",
        ClockSource::Manual => "manual scrub",
        ClockSource::Fallback => "noon (not connected)",
    }
}

/// Ordering handle so the one system allowed to *override* the model-`Visibility` authority — the
/// self-avatar first-person hide ([`crate::player`]) — can run **after** it and win the frame.
#[derive(SystemSet, Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ModelVisSet;

/// Adds the egui plugin, the debug-state resource, the panel UI, and the apply systems.
pub struct DebugPanelPlugin;

impl Plugin for DebugPanelPlugin {
    fn build(&self, app: &mut App) {
        app.add_plugins(EguiPlugin::default());
        // Manual context mode: we host egui on a dedicated overlay camera (below) so we can inset the
        // *world* camera's viewport without clipping the panel. Disable right after adding the plugin
        // (the resource exists by then) — before the auto-create system runs at startup.
        {
            let mut egui = app.world_mut().resource_mut::<EguiGlobalSettings>();
            egui.auto_create_primary_context = false;
            // Don't let egui drive the OS cursor icon. The player hides the OS cursor and draws its
            // own `Point.blp` sprite, so egui's per-hover `CursorIcon::System` inserts are pointless
            // churn (and a dev panel doesn't need resize/text cursors).
            egui.enable_cursor_icon_updates = false;
        }

        app.insert_resource(DebugState {
            // `$WOW_PANEL=1` — start with the panel **open**, which is how a headless capture run
            // gets it into the frame. Without it a panel change (a new footer line, a section that
            // grew past the scroll reserve) can only be checked in the director's own window, and
            // clipping is exactly the failure a capture catches for free.
            open: std::env::var_os("WOW_PANEL").is_some(),
            ..default()
        })
        .init_resource::<EguiPointerOver>()
        .add_systems(Startup, spawn_egui_camera)
        // Keep Tab out of egui: it's a bound game key, never focus-navigation for our overlays.
        // Runs after bevy_egui fills `EguiInput` and before egui's pass consumes it (the seam
        // bevy_egui documents for input edits).
        .add_systems(
            PreUpdate,
            strip_egui_tab_focus
                .after(EguiPreUpdateSet::ProcessInput)
                .before(EguiPreUpdateSet::BeginPass),
        )
        .add_systems(
            EguiPrimaryContextPass,
            (debug_panel_ui, track_pointer_over_ui),
        )
        // `apply_model_visibility` reads the WMO portal PVS, so it runs after the compute that
        // fills it (`crate::wmo_portal::WmoPvsSet`).
        .add_systems(
            Update,
            (
                // After the UI keyboard feed so a backtick typed into a focused EditBox never
                // toggles the panel.
                toggle_panel.after(crate::ui_script::UiInput),
                apply_model_visibility
                    .after(crate::wmo_portal::WmoPvsSet)
                    .in_set(ModelVisSet),
            ),
        );
    }
}

/// A full-window overlay camera that hosts the primary egui context and composites it over the 3D
/// scene (higher order, alpha blend, no clear). Renders no world geometry (`RenderLayers::none`).
/// Order 2 — above the player-UI quad pass (`ui_pass::PlayerUiPlugin`, order 1), which itself sits
/// above the order-0 world camera. Per decision 0025's overlay arbitration ("dev overlays composite
/// over" everything else) and 0068 §2: dev overlays always stay on top of the player UI.
fn spawn_egui_camera(mut commands: Commands) {
    commands.spawn((
        PrimaryEguiContext, // `#[require(EguiContext)]` ⇒ this camera carries EguiContext
        Camera2d,
        RenderLayers::none(),
        Camera {
            order: 2,
            output_mode: CameraOutputMode::Write {
                blend_state: Some(BlendState::ALPHA_BLENDING),
                clear_color: ClearColorConfig::None,
            },
            clear_color: ClearColorConfig::None,
            ..default()
        },
    ));
}

fn toggle_panel(
    keys: Res<ButtonInput<KeyCode>>,
    ui_capture: Res<crate::ui_script::UiKeyboardCapture>,
    mut debug: ResMut<DebugState>,
) {
    // Don't toggle while the chat input is open or a focused EditBox owns the keyboard — a backtick
    // keypress there belongs in the text.
    if !ui_capture.0 && keys.just_pressed(KeyCode::Backquote) {
        debug.open = !debug.open;
    }
}

/// How the dev plane is written on screen. Every surface that names a chord reads this instead of
/// spelling one out, so the panel footer, the inspector badge and the mute checkbox can't drift from
/// what [`dev_chord`] actually listens for.
pub(crate) const DEV_CHORD: &str = "Ctrl+Shift";

/// Did the **dev-overlay chord** — [`DEV_CHORD`]+*key* — just fire? (decisions 0585, 0867, 0870.)
///
/// The dev instruments used to sit on bare letters, which is a namespace we don't own: every letter is
/// a *game* binding in the reference client, so `P` both opened the spellbook and toggled the perf HUD
/// (0585). They moved to `Ctrl`+`Cmd`, and then off it: on Windows that is `Ctrl`+`Win`, a plane the
/// shell owns and keeps extending — `Win+Ctrl+M` is Magnifier settings, which had our mute (0867).
///
/// **`Ctrl`+`Shift`, one plane on every OS** (0870, director's call). The alternative was keeping
/// `Ctrl`+`Cmd` on macOS for its one real advantage — Cmd is outside the reference's binding namespace
/// (1.12 builds binding names from `ALT-`/`CTRL-`/`SHIFT-` only), so nothing in game could *ever* claim
/// it. That buys protection against a binding no default declares and no player has yet written, and
/// it costs a per-OS split in the docs, the hints and the reader's head. One plane wins. It also drops
/// our dependence on winit's `sendEvent:` swizzle, without which AppKit's swallowed `keyUp` under Cmd
/// would latch a chord after one use (0585's macOS risk, now simply not run).
///
/// `Ctrl`+`Shift` is the emptiest plane the reference *can* name: of its 152 defaults, exactly two
/// carry two modifiers — `CTRL-SHIFT-TAB` and `CTRL-SHIFT-PAGEDOWN` — and no letter at all.
/// `Ctrl`+`Alt` was never available: that is AltGr, which European layouts type real characters with
/// (decision 0702).
///
/// **Exactly those two modifiers and no others**, both sides of each. The block is what makes this
/// plane safe to leave ungated below: AltGr+Shift+*key* is `Ctrl`+`Alt`+`Shift`+*key*, and a German
/// layout typing one of those into chat must not fire an overlay. It is also the same exact-set rule
/// every binding here obeys — the reference matches a binding by string equality, so `CTRL-SHIFT-P` is
/// a different entry from `SHIFT-P` and neither answers for the other.
///
/// Deliberately **not** gated on [`crate::ui_script::UiKeyboardCapture`] the way the bare-key toggles
/// were: a chord can't be mistaken for typed text, so the dev overlays stay reachable with the chat bar
/// open. The other half of the rule lives at the gameplay key feed (`ui_script::input`): a bare-key
/// binding doesn't fire while *any* modifier is held, so this chord doesn't also trigger the letter's
/// game action.
pub(crate) fn dev_chord(keys: &ButtonInput<KeyCode>, key: KeyCode) -> bool {
    dev_plane(keys) && keys.just_pressed(key)
}

/// The modifier half of [`dev_chord`]. See there for why the plane is what it is.
fn dev_plane(keys: &ButtonInput<KeyCode>) -> bool {
    let ctrl = keys.any_pressed([KeyCode::ControlLeft, KeyCode::ControlRight]);
    let shift = keys.any_pressed([KeyCode::ShiftLeft, KeyCode::ShiftRight]);
    let blocked = keys.any_pressed([
        KeyCode::AltLeft,
        KeyCode::AltRight,
        KeyCode::SuperLeft,
        KeyCode::SuperRight,
    ]);
    ctrl && shift && !blocked
}

/// Draw the panel as a translucent **overlay** on the right — the world renders full-screen
/// underneath (no viewport inset). `ui_script::PointerOverUi` keeps the cursor's panel
/// interactions from leaking into gameplay mouse-look.
#[allow(clippy::too_many_arguments)]
fn debug_panel_ui(
    mut contexts: EguiContexts,
    stamp: Res<crate::build_id::BuildId>,
    mut debug: ResMut<DebugState>,
    mut view: ResMut<ViewDistance>,
    clock: Res<GameClock>,
    lighting: Res<WowLighting>,
    parts: Query<&ModelPart>,
    anim_hosts: Query<&crate::doodad_anim::DoodadAnimHost>,
    mat_anims: Query<&crate::doodad_anim::MatAnim>,
    uv_mats: Res<crate::doodad_anim::UvAnimMaterials>,
    mut sound_cfg: ResMut<crate::sound::SoundConfig>,
    mut cull_probe: ResMut<crate::wmo_portal::WmoCullProbe>,
    net_status: Res<crate::net::NetStatus>,
    dropped: Res<crate::net::DroppedOpcodes>,
    mut weather_state: Option<ResMut<crate::weather::WeatherState>>,
    mut world: WorldReadout,
) -> Result {
    if !debug.open {
        return Ok(());
    }

    // Live counts per layer/type.
    let mut kind_counts = [0u32; 4];
    let mut blend_counts = [0u32; 5];
    for p in &parts {
        kind_counts[p.kind.index()] += 1;
        blend_counts[blend_index(p.blend)] += 1;
    }

    let ctx = contexts.ctx_mut()?;
    // Full height: the window spans the view (an 8 px gap top and bottom), rather than auto-sizing to
    // however much content the open sections happen to have.
    let panel_h = (ctx.content_rect().height() - 32.0).max(120.0);
    egui::Window::new("benilla_debug")
        .title_bar(false)
        .resizable(false)
        .collapsible(false)
        .movable(false)
        .anchor(egui::Align2::RIGHT_TOP, egui::vec2(-8.0, 8.0))
        .frame(
            egui::Frame::NONE
                .fill(OVERLAY_FILL)
                .corner_radius(5.0)
                .inner_margin(egui::Margin::symmetric(10, 8)),
        )
        .show(ctx, |ui| {
            // Brighten to match the overlays; keep egui's default wrapping so labels reflow, not clip.
            ui.visuals_mut().override_text_color = Some(OVERLAY_TEXT);
            ui.set_width(280.0); // fixed width — no resize handle
            ui.set_height(panel_h); // fill the height; sections scroll within it

            egui::ScrollArea::vertical()
                .auto_shrink([false, false])
                // Leave room under the scroll for the pinned footer below (hotkey map + build id).
                // Three lines' worth: the footer keeps the panel's default wrapping, so a font
                // whose metrics run wider than ours reflows the hotkey map instead of having its
                // second line clipped away.
                .max_height(panel_h - 56.0)
                .show(ui, |ui| {
                    // Disjoint borrows of the sections so each can drive its own widgets.
                    let DebugState {
                        models: m,
                        lighting: l,
                        sound: s,
                        weather: w,
                        ..
                    } = &mut *debug;
                    // WHERE AM I — the `.gps` of the panel: identity, map, zone, position, tile
                    // residency, and the click-to-copy `.go xyz` teleport line that feeds the
                    // headless probes (`WOW_PROBE_CHAT`) and the FPS-journal loop.
                    egui::CollapsingHeader::new("World")
                        .default_open(true)
                        .show(ui, |ui| world_section(ui, &mut world));

                    egui::CollapsingHeader::new("Models")
                        .default_open(false)
                        .show(ui, |ui| {
                            ui.strong("Layer (blend mode)");
                            for i in 0..BLEND_LABELS.len() {
                                ui.checkbox(
                                    &mut m.blend_visible[i],
                                    format!("{}  ·  {}", BLEND_LABELS[i], blend_counts[i]),
                                );
                            }

                            ui.add_space(6.0);
                            ui.strong("Type");
                            for k in ModelKind::ALL {
                                ui.checkbox(
                                    &mut m.kind_visible[k.index()],
                                    format!("{}  ·  {}", k.label(), kind_counts[k.index()]),
                                );
                            }
                            // The doodad-animation cost meter (decision 0130): how many placed
                            // doodads carry an anim host, how many are ticking right now (the
                            // draw gate pauses hidden ones), how many batches sample a
                            // material-alpha loop (phase 2), and how many materials scroll their
                            // UVs (phase 3 — waterfalls).
                            let ticking = anim_hosts.iter().filter(|h| h.active).count();
                            // Of the material samplers, how many resolve to **0 right now** — the
                            // batches the reference culls this frame (`A <= 0`, wow-re
                            // `m2-alpha-combine-cull`). Non-zero as soon as a voidwalker/banshee/
                            // slime/infernal is in view: those models author geometry that only
                            // appears on death, and this counter is what says we are hiding it
                            // rather than drawing it. `dim` counts the partial factors — a batch
                            // drawn, but not at full strength.
                            let (mut hidden, mut dim) = (0usize, 0usize);
                            for m in &mat_anims {
                                if m.current <= 0.0 {
                                    hidden += 1;
                                } else if m.current < 1.0 {
                                    dim += 1;
                                }
                            }
                            ui.label(format!(
                                "animated doodads  ·  {} ({} ticking)  ·  {} material                                  ({hidden} culled, {dim} dimmed)  ·  {} uv",
                                anim_hosts.iter().count(),
                                ticking,
                                mat_anims.iter().count(),
                                uv_mats.0.len(),
                            ));

                            ui.add_space(6.0);
                            // WMO portal cull A/B (decision 0031): off ⇒ every building group always
                            // draws (the cathedral reappears from the Trade District).
                            ui.checkbox(&mut m.portal_cull, "WMO portal visibility cull");
                            // The cull probe (decision 0022): a one-click full trace dump — stand where a
                            // room vanishes, click, and the exact seed evidence + per-portal verdicts land
                            // in a file.
                            if ui.button("dump WMO cull trace").clicked() {
                                cull_probe.dump_requested = true;
                            }

                            ui.add_space(6.0);
                            // Faithful `farclip`: doodads/WMOs hard-pop out past this (no fade). The
                            // range (and why it runs past vanilla's 777) is `view::FARCLIP_RANGE`,
                            // shared with the `$WOW_FARCLIP` capture knob so a setting seen here can
                            // be reproduced headlessly.
                            ui.add(
                                egui::Slider::new(&mut view.farclip, crate::view::FARCLIP_RANGE)
                                    .text("view distance / farclip (yd)"),
                            );
                        });

                    egui::CollapsingHeader::new("Lighting")
                        .default_open(false)
                        .show(ui, |ui| {
                            // Time of day — drives the DBC-sampled colors + the sun's day-arc.
                            ui.strong(format!(
                                "Time of day: {}  ({})",
                                hhmm(clock.minute),
                                source_label(clock.source),
                            ));
                            ui.checkbox(&mut l.follow_server_time, "follow server clock");
                            ui.add_enabled_ui(!l.follow_server_time, |ui| {
                                ui.horizontal(|ui| {
                                    ui.add(
                                        egui::Slider::new(&mut l.manual_minute, 0..=1439)
                                            .text("scrub time")
                                            .custom_formatter(|n, _| hhmm(n as u32)),
                                    );
                                    // Exact per-minute entry: drag/scroll ±1 min, or click to type a raw
                                    // minute (0..1439) or an `HH:MM` time — for landing on an exact moment.
                                    ui.add(
                                        egui::DragValue::new(&mut l.manual_minute)
                                            .range(0..=1439)
                                            .speed(1.0)
                                            .custom_formatter(|n, _| hhmm(n as u32))
                                            .custom_parser(|s| {
                                                let s = s.trim();
                                                if let Some((h, m)) = s.split_once(':') {
                                                    let h: u32 = h.trim().parse().ok()?;
                                                    let m: u32 = m.trim().parse().ok()?;
                                                    Some(((h % 24) * 60 + (m % 60)) as f64)
                                                } else {
                                                    s.parse::<f64>().ok()
                                                }
                                            }),
                                    );
                                });
                            });

                            ui.add_space(6.0);
                            ui.strong("Resolved (Light.dbc, this time)");
                            // 0..255 bytes (eyedrop-friendly vs the reference client) + a swatch.
                            let byte = |v: f32| (v.clamp(0.0, 1.0) * 255.0).round() as i32;
                            let swatch = |ui: &mut egui::Ui, name: &str, c: [f32; 3]| {
                                let (r, g, b) = (byte(c[0]), byte(c[1]), byte(c[2]));
                                let col = egui::Color32::from_rgb(r as u8, g as u8, b as u8);
                                // The label text is tinted the resolved color (a swatch the font always
                                // renders); the [r,g,b] are the exact 0..255 values for eyedrop A/Bs.
                                ui.colored_label(col, format!("{name}  [{r}, {g}, {b}]"));
                            };
                            swatch(ui, "ambient (row 1)", lighting.ambient);
                            swatch(ui, "diffuse (row 0)", lighting.diffuse);
                            swatch(ui, "specular (row 9)", lighting.spec);
                            // The backdrop / fog colour (row 7) — what `ClearColor` shows behind the dome.
                            swatch(ui, "fog / backdrop (row 7)", lighting.fog_color);
                            let d = lighting.sun_dir;
                            ui.label(
                                egui::RichText::new(format!(
                                    "sun dir  ({:+.2}, {:+.2}, {:+.2})",
                                    d.x, d.y, d.z
                                ))
                                .weak(),
                            );
                            ui.add_space(8.0);
                            ui.checkbox(&mut l.disable_fog, "disable distance fog");
                            ui.checkbox(&mut l.disable_sky_dome, "disable sky dome");
                        });

                    egui::CollapsingHeader::new("Weather")
                        .default_open(false)
                        .show(ui, |ui| {
                            // Live state readout (the two ramped channels, decision 0302).
                            if let Some(ws) = weather_state.as_ref() {
                                ui.strong(format!(
                                    "{:?}  ·  effect {:.2}  ·  sky {:.2}  ·  storm blend {:.2}",
                                    ws.effect_kind,
                                    ws.effect_density,
                                    ws.sky_density,
                                    crate::weather::storm_blend(ws.sky_density),
                                ));
                            }
                            // The `weatherDensity` setting (video-options Weather Intensity 0–3)
                            // — the real game setting, not a tuning knob. Scales the rain/snow/
                            // mist spawn gain via the `0x67b870` quality table; default 3 matches
                            // the reference install's Config.wtf.
                            if let Some(ws) = weather_state.as_mut() {
                                let mut wd = ws.weather_density;
                                ui.horizontal(|ui| {
                                    ui.label("weather intensity");
                                    for v in 0..=3u8 {
                                        ui.selectable_value(&mut wd, v, format!("{v}"));
                                    }
                                });
                                if wd != ws.weather_density {
                                    ws.weather_density = wd;
                                }
                            }
                            // The override scrub — drives the same apply path as the wire.
                            let before = (w.force, w.kind, w.grade.to_bits(), w.instant);
                            ui.checkbox(&mut w.force, "override server weather");
                            ui.add_enabled_ui(w.force, |ui| {
                                ui.horizontal(|ui| {
                                    for (v, name) in
                                        [(0, "fine"), (1, "rain"), (2, "snow"), (3, "sand")]
                                    {
                                        ui.selectable_value(&mut w.kind, v, name);
                                    }
                                });
                                ui.add(egui::Slider::new(&mut w.grade, 0.0..=1.0).text("grade"));
                                ui.checkbox(&mut w.instant, "instant (skip the ramp)");
                            });
                            if before != (w.force, w.kind, w.grade.to_bits(), w.instant) {
                                w.dirty = true;
                            }
                        });

                    // The object inspector is its own dev-chord `I` surface (see `interact.rs`), not a
                    // section here — identifying a thing shouldn't need the whole panel open.
                    egui::CollapsingHeader::new("Sound")
                        .default_open(false)
                        .show(ui, |ui| {
                            ui.checkbox(&mut sound_cfg.enabled, "enabled");
                            ui.checkbox(&mut sound_cfg.muted, format!("muted ({DEV_CHORD}+M)"));
                            ui.add(
                                egui::Slider::new(&mut sound_cfg.master, 0.0..=1.0).text("master"),
                            );
                            ui.add(egui::Slider::new(&mut sound_cfg.sfx, 0.0..=1.0).text("sfx"));
                            ui.add(
                                egui::Slider::new(&mut sound_cfg.music, 0.0..=1.0).text("music"),
                            );
                            ui.add(
                                egui::Slider::new(&mut sound_cfg.ambience, 0.0..=1.0)
                                    .text("ambience"),
                            );
                            ui.separator();
                            ui.label("kit probe: a SoundEntries id or name");
                            ui.text_edit_singleline(&mut s.kit_query);
                            if ui.button("Play kit").clicked() {
                                s.play_kit = true;
                            }
                        });

                    // The wire-coverage section: connection state + the dropped-packet tally
                    // (every opcode the codec ignored or failed to parse, by count — a silent
                    // gap in wire coverage made visible; decision 0022's instrument disposition).
                    egui::CollapsingHeader::new("Net")
                        .default_open(false)
                        .show(ui, |ui| {
                            ui.label(if net_status.connected {
                                match net_status.latency_ms {
                                    Some(ms) => format!("connected · {ms} ms ping"),
                                    None => "connected".to_string(),
                                }
                            } else {
                                format!(
                                    "disconnected — {}",
                                    net_status
                                        .last_reason
                                        .as_deref()
                                        .unwrap_or("never connected")
                                )
                            });
                            ui.add_space(4.0);
                            ui.strong("dropped packets (opcode · count)");
                            if dropped.0.is_empty() {
                                ui.label("none — every received opcode parsed");
                            } else {
                                let mut rows: Vec<_> = dropped.0.iter().collect();
                                rows.sort_by_key(|(_, t)| {
                                    std::cmp::Reverse(t.unknown + t.unparseable)
                                });
                                for (op, t) in rows {
                                    let name =
                                        benilla_protocol::messages::opcode_name(*op).unwrap_or("?");
                                    let mut line =
                                        format!("{op:#06x} {name} · {}", t.unknown + t.unparseable);
                                    if t.unparseable > 0 {
                                        line.push_str(&format!(
                                            "  ({} parse-errored)",
                                            t.unparseable
                                        ));
                                    }
                                    ui.label(line);
                                }
                            }
                        });
                    // Future sections add their own CollapsingHeader here.
                });

            // The pinned footer: the dev-surface hotkey map, so the other overlays are
            // discoverable from the one surface people find first.
            ui.separator();
            ui.label(
                egui::RichText::new(format!(
                    "`  panel   ·   {DEV_CHORD}:  P perf · I inspect · M mute"
                ))
                    .small()
                    .color(OVERLAY_TEXT_DIM),
            );
            // …and which build this is (`crate::build_id`). Bottom line of the surface a reader is
            // already on when something looks wrong: a click copies the full sha, so "it looks like
            // this here" can name the code it happened on.
            let build = ui.add(
                egui::Label::new(
                    egui::RichText::new(format!("build {}", stamp.summary()))
                        .small()
                        .color(OVERLAY_TEXT_DIM),
                )
                .sense(egui::Sense::click()),
            );
            if !stamp.sha.is_empty()
                && build
                    .on_hover_text(format!("{}\n(click to copy)", stamp.sha))
                    .clicked()
            {
                ui.ctx().copy_text(stamp.sha.to_string());
            }
        });
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn keys(held: &[KeyCode]) -> ButtonInput<KeyCode> {
        let mut k = ButtonInput::default();
        for &c in held {
            k.press(c);
        }
        k
    }

    /// The plane fires on its two modifiers, either side of each.
    #[test]
    fn ctrl_shift_is_the_plane() {
        for pair in [
            [KeyCode::ControlLeft, KeyCode::ShiftRight],
            [KeyCode::ControlRight, KeyCode::ShiftLeft],
        ] {
            assert!(dev_plane(&keys(&pair)), "{pair:?} fires");
        }
    }

    /// One modifier is not the chord, and a third one names something else. The case that matters:
    /// AltGr *is* `Ctrl`+`Alt`, so AltGr+Shift+key is a character a European layout types, never a
    /// dev chord — the overlays are ungated while the chat bar is open. `Ctrl`+`Cmd` is likewise
    /// nothing of ours now (0870): on Windows it is the shell's, `Win+Ctrl+M` being Magnifier
    /// settings.
    #[test]
    fn a_lone_or_extra_modifier_is_not_the_chord() {
        for held in [
            vec![KeyCode::ControlLeft],
            vec![KeyCode::ShiftLeft],
            vec![KeyCode::ControlLeft, KeyCode::SuperLeft],
            vec![KeyCode::ControlLeft, KeyCode::ShiftLeft, KeyCode::AltLeft],
            vec![KeyCode::ControlLeft, KeyCode::ShiftLeft, KeyCode::SuperLeft],
        ] {
            assert!(!dev_plane(&keys(&held)), "{held:?} is not the chord");
        }
    }

    /// The label a hint prints is the plane actually listened for — one const, one predicate, so a
    /// player is never told to press a key we stopped reading.
    #[test]
    fn the_label_matches_the_plane() {
        assert_eq!(DEV_CHORD, "Ctrl+Shift");
        assert!(dev_plane(&keys(&[
            KeyCode::ControlLeft,
            KeyCode::ShiftLeft
        ])));
    }
}
