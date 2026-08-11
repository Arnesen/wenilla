//! The shared glue-screen vocabulary (decision 0465) — everything the pre-world screens (character
//! select + character create) draw with, factored out of the create screen when select joined it:
//! the client-data art set ([`art::GlueArt`]), the widget builders ([`widgets`]), the ADD-mode UI
//! material ([`add_material`]), and the screen-agnostic interaction systems below. Each screen
//! keeps its own layout, actions, and selection policy; what lives here is exactly what the two
//! share — so the vocabulary can never fork.

pub(crate) mod add_material;
pub(crate) mod art;
pub(crate) mod backdrop;
pub(crate) mod widgets;

use bevy::prelude::*;

use art::{tc_rect, GlueArt, BTN_BG, BTN_HOVER, BUTTON_TC, GOLD};
use widgets::{ArtSwap, GlueBtn, GlueCaption, GlueDisabled, OutlineCopy};

/// The shared glue infrastructure both screens stand on: the ADD-mode UI material pipeline, the
/// [`GlueArt`] resource (loaded on first screen entry), and the GlueStrings table. Registered
/// before either screen plugin (`main.rs`).
pub(crate) struct GluePlugin;

impl Plugin for GluePlugin {
    fn build(&self, app: &mut App) {
        app.add_plugins(bevy::ui_render::UiMaterialPlugin::<
            add_material::AddUiMaterial,
        >::default())
            .init_resource::<GlueArt>()
            .add_systems(
                Startup,
                crate::glue_strings::load_glue_strings.after(benilla_assets::AssetSet::Open),
            )
            .add_systems(
                Update,
                (backdrop::fit_backdrop_borders, seat_outline_copies),
            );
    }
}

/// Seat every outline copy at exactly ONE device pixel from its real string (`dir / scale_factor`
/// logical) — the reference's baked 1-px dilation ring, reproduced as string geometry (see
/// [`widgets::OutlineCopy`]). Runs every frame but writes only when the wanted value differs
/// (fresh spawns, or the window moving to a display with another scale factor).
pub(crate) fn seat_outline_copies(
    window: Query<&Window, With<bevy::window::PrimaryWindow>>,
    mut copies: Query<(&OutlineCopy, &mut Node)>,
) {
    let px = 1.0 / window.single().map(|w| w.scale_factor()).unwrap_or(1.0);
    for (copy, mut node) in &mut copies {
        let (left, top) = (Val::Px(copy.dir.x * px), Val::Px(copy.dir.y * px));
        if node.left != left || node.top != top {
            node.left = left;
            node.top = top;
        }
    }
}

/// The glue engine's virtual-screen scale: the ref lays every glue screen out on a 1024×768
/// virtual screen stretched to the window, so each authored coordinate draws at `value · this`.
/// The ONE copy of the formula — every glue spawn site and every rescale check reads it here, and
/// each screen's lifecycle system compares its tree's baked scale against this to know when a
/// window resize (mac fullscreen, a drag) has invalidated the tree.
///
/// **There is no lower clamp** (B120). A floor of 1.0 draws the 768-unit-tall authored layout into a
/// shorter window and the overflow simply falls off the bottom — silently, and always the
/// *bottom-most* controls: on the create screen that is the last customization row and the
/// **RANDOMIZE** button (reproduced at `WOW_WIN=1276x677` — both gone, along with the foot of the
/// right-hand race/class panels). The reference has no such floor: its glue screens are authored in
/// the 768-tall virtual space and scaled by the window height, so a short window makes everything
/// smaller and nothing missing. The upper clamp stays — that is the shipped size on a tall screen,
/// and lifting it would resize the director's UI without being asked.
pub(crate) fn screen_scale(window: Option<&Window>) -> f32 {
    window.map(|w| (w.height() / 768.0).min(2.2)).unwrap_or(1.0)
}

/// Mirror every outlined text's content into its black copies ([`widgets::outlined_text`]) — the
/// refresh systems write only the real text entity; the copies are its `OutlineCopy` siblings.
/// Registered by each glue screen under its own state (the trees only exist there).
#[allow(clippy::type_complexity)]
pub(crate) fn sync_outlines(
    changed: Query<(&Text, &ChildOf), (Changed<Text>, Without<OutlineCopy>)>,
    children: Query<&Children>,
    mut copies: Query<&mut Text, With<OutlineCopy>>,
) {
    for (text, parent) in &changed {
        let Ok(siblings) = children.get(parent.parent()) else {
            continue;
        };
        for &sib in siblings {
            if let Ok(mut copy) = copies.get_mut(sib) {
                if copy.0 != text.0 {
                    copy.0 = text.0.clone();
                }
            }
        }
    }
}

/// Two-state button faces (spinner arrows, rotate buttons): pressed swaps to the down art.
pub(crate) fn art_swaps(
    mut swaps: Query<(&ArtSwap, &Interaction, &mut ImageNode), Without<GlueBtn>>,
) {
    for (swap, interaction, mut node) in &mut swaps {
        let img = if *interaction == Interaction::Pressed {
            &swap.down
        } else {
            &swap.up
        };
        if node.image != *img {
            node.image = img.clone();
        }
    }
}

/// The glue-panel buttons' look: up/down/disabled art states + the caption whitening on hover (the
/// ref's `HighlightFont`). Disabling is the screen's call — it toggles [`GlueDisabled`]; this pass
/// only renders it (the ref's `Enable()`/`Disable()` split the same way).
#[allow(clippy::type_complexity)]
pub(crate) fn glue_button_visuals(
    art: Res<GlueArt>,
    mut glue_btns: Query<
        (
            &GlueDisabled,
            &Interaction,
            &Children,
            &mut ImageNode,
            &mut BackgroundColor,
            Has<widgets::FallbackFace>,
        ),
        (With<GlueBtn>, Without<ArtSwap>),
    >,
    mut captions: Query<&mut TextColor, With<GlueCaption>>,
    mut hilights: Query<&mut Visibility, With<widgets::Hilight>>,
) {
    for (disabled, interaction, children, mut node, mut bg, fallback) in &mut glue_btns {
        let disabled = disabled.0;
        let pressed = !disabled && *interaction == Interaction::Pressed;
        let hovered = !disabled && *interaction != Interaction::None;
        if let Some((up, size)) = &art.button_up {
            let (image, color) = if disabled {
                match &art.button_dis {
                    Some((dis, _)) => (dis.clone(), Color::WHITE),
                    None => (up.clone(), Color::srgb(0.45, 0.45, 0.45)),
                }
            } else if pressed {
                (
                    art.button_down
                        .as_ref()
                        .map(|(d, _)| d.clone())
                        .unwrap_or_else(|| up.clone()),
                    Color::WHITE,
                )
            } else {
                (up.clone(), Color::WHITE)
            };
            node.image = image;
            node.rect = Some(tc_rect(*size, BUTTON_TC));
            node.color = color;
        } else if fallback {
            bg.0 = if hovered { BTN_HOVER } else { BTN_BG };
        }
        for child in children {
            if let Ok(mut caption) = captions.get_mut(*child) {
                // The disabled caption grays (`GlueFontDisable`), hover whitens, rest is gold.
                caption.0 = if disabled {
                    Color::srgb(0.5, 0.5, 0.5)
                } else if hovered {
                    Color::WHITE
                } else {
                    GOLD
                };
            }
            if let Ok(mut vis) = hilights.get_mut(*child) {
                *vis = if hovered {
                    Visibility::Inherited
                } else {
                    Visibility::Hidden
                };
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::screen_scale;
    use bevy::prelude::*;
    use bevy::window::WindowResolution;

    /// The authored layout must FIT the window at every height — the B120 regression.
    /// [`screen_scale`] is the only thing standing between a 768-unit-tall tree and a shorter
    /// window, so a floor there silently amputates the bottom of every glue screen (the create
    /// screen's RANDOMIZE button is authored at y 719..749 of 768).
    #[test]
    fn the_authored_layout_fits_any_window_height() {
        /// Bottom edge of the create screen's lowest control (the RANDOMIZE button) in authored
        /// units: the configuration tower's TOPLEFT y 74 + its 645 in-tower offset + 30 tall.
        const LOWEST_CONTROL: f32 = 74.0 + 645.0 + 30.0;
        for h in [480u32, 600, 677, 720, 768, 900, 1286, 2160] {
            let window = Window {
                resolution: WindowResolution::new(1600, h),
                ..default()
            };
            let s = screen_scale(Some(&window));
            assert!(
                LOWEST_CONTROL * s <= window.height(),
                "at a {h}px window (scale {s}) the lowest glue control lands at {} — off the bottom",
                LOWEST_CONTROL * s
            );
        }
    }
}
