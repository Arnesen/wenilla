//! The glue screens' reusable widget builders — the art button shapes of `GlueButtons.xml` /
//! `CharacterCreate.xml` that each screen's layout places at its authored offsets — plus the
//! widget-vocabulary marker components they spawn. Builders are generic over the screen's action
//! component (`CreateAction`, `SelectAction`, …); each degrades to a plain-fill + text face
//! without client art.

use bevy::prelude::*;
use bevy::text::LineHeight;
use bevy::ui_render::ui_material::MaterialNode;

use super::art::{tc_rect, ArrowArt, GlueArt, BTN_BG, BUTTON_TC, FALLBACK_ALPHA, GOLD, NAME_EDGE};
use super::backdrop::{backdrop_border, tiled_bg_node};

// ── The shared widget vocabulary ─────────────────────────────────────────────────────────────────

/// A button's highlight overlay (shown on hover — and held while selected where the screen locks
/// it, the ref's `LockHighlight`). Visibility is the owning screen's to drive.
#[derive(Component)]
pub(crate) struct Hilight;
/// A button spawned with a plain-fill face because client art is missing — the only buttons whose
/// `BackgroundColor` a hover pass may shade (every `Node` carries one since Bevy 0.15's required
/// components, so presence alone can't distinguish the fallback).
#[derive(Component)]
pub(crate) struct FallbackFace;
/// An icon button's name label (the ref's `HighlightText`: visible on hover/selected — always,
/// without icon art).
#[derive(Component)]
pub(crate) struct HoverLabel;
/// A glue-panel button (swaps up/down/disabled art; its caption whitens on hover).
#[derive(Component)]
pub(crate) struct GlueBtn;
/// A glue-panel button's enabled state (the ref's `Enable()`/`Disable()`): the screen toggles it,
/// [`super::glue_button_visuals`] renders it (disabled art, gray caption, no hover).
#[derive(Component, Default)]
pub(crate) struct GlueDisabled(pub(crate) bool);
/// A glue button's caption (gold at rest, white on hover — the ref's `HighlightFont`).
#[derive(Component)]
pub(crate) struct GlueCaption;
/// A two-state button face (spinner arrows, rotate): pressed swaps `up` → `down`.
#[derive(Component)]
pub(crate) struct ArtSwap {
    pub(crate) up: Handle<Image>,
    pub(crate) down: Handle<Image>,
}
/// One of an outlined text's black copies ([`outlined_text`]) — content mirrored from its real
/// sibling by [`super::sync_outlines`]; `dir` is its unit offset direction, seated to exactly one
/// DEVICE pixel by [`super::seat_outline_copies`].
///
/// The reference's `outline="NORMAL"` is not string geometry at all: the ring is **baked into the
/// glyph atlas cell by one 8-neighbour dilation pass at rasterization** (wow-re
/// `font/scratch/outline-bake-tint.md` §3, byte-verified) — and the glyph rasterizes at final
/// device-pixel size, so the ring is one device pixel hugging the glyph at every resolution
/// (vanilla's famously thin outlines at high res). Offsetting copies by an authored *unit*
/// instead put the ring 2–3 device px out at fullscreen scales, where it read as a separate
/// doubled stroke (director's report, 2026-07-19).
#[derive(Component)]
pub(crate) struct OutlineCopy {
    pub(crate) dir: Vec2,
}

// ── Layout helpers ───────────────────────────────────────────────────────────────────────────────

/// An absolutely-positioned node at the authored `(left, top, w, h)`, scaled by `s` — the shape of
/// nearly every ref anchor once resolved to 1024×768 coordinates.
pub(crate) fn abs(s: f32, left: f32, top: f32, w: f32, h: f32) -> Node {
    Node {
        position_type: PositionType::Absolute,
        left: Val::Px(left * s),
        top: Val::Px(top * s),
        width: Val::Px(w * s),
        height: Val::Px(h * s),
        ..default()
    }
}

/// A full-parent absolute overlay (highlights, border sheets).
pub(crate) fn overlay() -> Node {
    Node {
        position_type: PositionType::Absolute,
        left: Val::Px(0.0),
        top: Val::Px(0.0),
        width: Val::Percent(100.0),
        height: Val::Percent(100.0),
        ..default()
    }
}

/// A glue string's look, before the outline treatment: content, authored size, color, and whether
/// it may wrap (the ref FontStrings never break a label; only the info paragraphs wrap).
pub(crate) struct GlueText<'a> {
    pub text: &'a str,
    pub size: f32,
    pub color: Color,
    pub wrap: bool,
}

/// A glue text with the fonts' black outline (`outline="NORMAL"`, GlueFonts.xml — every glue font
/// but the edit box carries it). Bevy text has no stroke, so the string draws nine times: eight
/// black copies one DEVICE pixel out in the 8-neighbour directions (the ref's baked dilation ring
/// — see [`OutlineCopy`]), then the real text — with the MasterFont (1,−1) drop shadow — painted
/// last on top. `node` is the layout shape (position/margins/centering); an auto-sized inner box
/// keeps the copies registered with the real string wherever `node` puts it. `wrapper_extra` rides
/// the wrapper (visibility markers hide all nine at once); `text_extra` rides the real text (the
/// markers the refresh writes — [`super::sync_outlines`] mirrors those writes into the copies).
/// Returns the real text entity.
pub(crate) fn outlined_text<W: Bundle, T: Bundle>(
    parent: &mut ChildSpawnerCommands,
    node: Node,
    wrapper_extra: W,
    text_extra: T,
    spec: GlueText,
    font: &Handle<Font>,
    s: f32,
) -> Entity {
    let layout = TextLayout {
        linebreak: if spec.wrap {
            LineBreak::WordBoundary
        } else {
            LineBreak::NoWrap
        },
        ..default()
    };
    let tf = TextFont {
        font: font.clone(),
        font_size: spec.size * s,
        ..default()
    };
    let mut real = Entity::PLACEHOLDER;
    parent
        .spawn((node, wrapper_extra))
        .with_children(|wrapper| {
            // The −1px trim: our centered layout box sat the glyphs ~1px lower than the ref's
            // baseline placement everywhere (director-measured); one optical correction here
            // covers every glue string.
            let trim = Node {
                top: Val::Px(-s),
                ..default()
            };
            wrapper.spawn(trim).with_children(|inner| {
                // The 8-neighbour ring (see [`OutlineCopy`]) — seated to one device pixel by
                // `seat_outline_copies`; the 0.5-logical spawn value is the retina guess for the
                // one frame before that system runs.
                for dy in [-1.0, 0.0, 1.0] {
                    for dx in [-1.0, 0.0, 1.0] {
                        if dx == 0.0 && dy == 0.0 {
                            continue;
                        }
                        inner.spawn((
                            OutlineCopy {
                                dir: Vec2::new(dx, dy),
                            },
                            Text::new(spec.text),
                            tf.clone(),
                            layout,
                            TextColor(Color::BLACK),
                            Node {
                                position_type: PositionType::Absolute,
                                left: Val::Px(dx * 0.5),
                                top: Val::Px(dy * 0.5),
                                width: Val::Percent(100.0),
                                ..default()
                            },
                        ));
                    }
                }
                real = inner
                    .spawn((
                        Text::new(spec.text),
                        tf,
                        layout,
                        TextColor(spec.color),
                        TextShadow {
                            offset: Vec2::splat(s),
                            color: Color::BLACK,
                        },
                        text_extra,
                    ))
                    .id();
            });
        });
    real
}

/// A 48² icon check-button (race/class/gender): the `IconShadow` behind, the icon face (dynamic —
/// the owning screen assigns the sheet + rect — or fixed), a `ButtonHilight-Square` overlay lit on
/// hover *and held while selected* (the ref's `LockHighlight`; the template's `CheckedTexture` is
/// commented out in the shipped 1.12 GlueXML, so the locked square *is* the whole selected
/// visual), and the name label along the bottom (the ref's `HighlightText`, `GlueFontNormalSmall`,
/// anchored BOTTOM +1 — over the icon's bottom edge). `dyn_icon`/`label_dyn` are the screen's
/// refresh markers, spawned onto the face / real label text.
#[allow(clippy::too_many_arguments)]
pub(crate) fn icon_button<A: Component, I: Bundle, L: Bundle>(
    parent: &mut ChildSpawnerCommands,
    font: &Handle<Font>,
    action: A,
    dyn_icon: Option<I>,
    fixed: Option<(Handle<Image>, Rect)>,
    label_dyn: Option<L>,
    label: &str,
    art: &GlueArt,
    s: f32,
) {
    let px = |v: f32| Val::Px(v * s);
    let mut b = parent.spawn((
        action,
        Button,
        Node {
            width: px(48.0),
            height: px(48.0),
            ..default()
        },
    ));
    if art.races.is_none() {
        b.insert((FallbackFace, BackgroundColor(BTN_BG))); // no art: the label is the button face
    }
    b.with_children(|b| {
        // The shadow (64² centered, offset (2,−2)) — behind everything.
        if let Some(shadow) = &art.icon_shadow {
            b.spawn((
                ImageNode::new(shadow.clone()),
                abs(s, -6.0, -6.0, 64.0, 64.0),
            ));
        }
        // The icon face: dynamic (transparent until the refresh assigns the sheet + rect — a
        // default `ImageNode` is a white square) or fixed at spawn (gender halves).
        let mut face = b.spawn((
            match &fixed {
                Some((sheet, rect)) => ImageNode {
                    image: sheet.clone(),
                    rect: Some(*rect),
                    ..default()
                },
                None => ImageNode {
                    color: Color::NONE,
                    ..default()
                },
            },
            Node {
                width: Val::Percent(100.0),
                height: Val::Percent(100.0),
                ..default()
            },
        ));
        if let Some(icon) = dyn_icon {
            face.insert(icon);
        }
        // The ADD-mode overlays draw through the true-additive UI material (`add_material`).
        if let Some(hilight) = &art.hilight {
            b.spawn((
                Hilight,
                Visibility::Hidden,
                MaterialNode(hilight.clone()),
                overlay(),
            ));
        }
        // The name label: the wrapper carries the hover visibility (all five strings toggle
        // together); the dynamic marker rides the real text for the refresh + outline sync.
        let real = outlined_text(
            b,
            Node {
                position_type: PositionType::Absolute,
                bottom: Val::Px(1.0),
                width: Val::Percent(100.0),
                justify_content: JustifyContent::Center,
                ..default()
            },
            (HoverLabel, Visibility::Hidden),
            (),
            GlueText {
                text: label,
                size: 12.0, // GlueFontNormalSmall
                color: GOLD,
                wrap: false,
            },
            font,
            s,
        );
        if let Some(dyn_text) = label_dyn {
            b.commands().entity(real).insert(dyn_text);
        }
    });
}

/// A 32² spinner arrow at its authored x in the dial row — up/down art states + the additive
/// hover highlight, or a plain `<`/`>` without art.
pub(crate) fn dial_arrow<A: Component>(
    row: &mut ChildSpawnerCommands,
    arrow: &Option<ArrowArt>,
    font: &Handle<Font>,
    action: A,
    left: f32,
    fallback: &str,
    s: f32,
) {
    let mut b = row.spawn((
        action,
        Button,
        Node {
            justify_content: JustifyContent::Center,
            align_items: AlignItems::Center,
            ..abs(s, left, 0.0, 32.0, 32.0)
        },
    ));
    match arrow {
        Some(a) => {
            b.insert(ImageNode::new(a.up.clone()));
            if let Some(down) = &a.down {
                b.insert(ArtSwap {
                    up: a.up.clone(),
                    down: down.clone(),
                });
            }
            if let Some(hi) = &a.hi {
                b.with_children(|inner| {
                    inner.spawn((
                        Hilight,
                        Visibility::Hidden,
                        MaterialNode(hi.clone()),
                        overlay(),
                    ));
                });
            }
        }
        None => {
            b.insert((FallbackFace, BackgroundColor(BTN_BG)));
            b.with_children(|inner| {
                inner.spawn((
                    Text::new(fallback.to_string()),
                    TextFont {
                        font: font.clone(),
                        font_size: 14.0 * s,
                        ..default()
                    },
                    TextColor(GOLD),
                ));
            });
        }
    }
}

/// A glue button template (`GlueButtons.xml` / `GlueDialog.xml`): the caption font it authors, and
/// its `<ButtonText>` CENTER-anchor **offset**. The offset is real and per-template — the big
/// `GlueButtonTemplate` pulls its caption 3 units LEFT and 3 UP of dead centre — and drawing every
/// caption dead centre instead sat them all low, and the big buttons' right (decision 0543).
#[derive(Clone, Copy)]
pub(crate) enum GlueBtnKind {
    /// `GlueButtonTemplate` (170×45) — GlueFontNormal, ButtonText CENTER (−3, 3).
    Normal,
    /// `GlueButtonSmallTemplate` (150×38) — GlueFontNormalSmall, ButtonText CENTER (0, 3).
    Small,
    /// `GlueDialogButtonTemplate` (200×40) — GlueFontNormal, ButtonText CENTER (0, 2).
    Dialog,
}

impl GlueBtnKind {
    /// `(font size, caption offset)` — the offset in the reference's anchor space, y UP-positive.
    fn caption(self) -> (f32, Vec2) {
        match self {
            Self::Normal => (15.0, Vec2::new(-3.0, 3.0)),
            Self::Small => (12.0, Vec2::new(0.0, 3.0)),
            Self::Dialog => (15.0, Vec2::new(0.0, 2.0)),
        }
    }
}

/// `GlueEditBoxFont`'s size (GlueFonts.xml: ARIALN 18) — the typed line and the caret beside it.
pub(crate) const EDIT_FONT_SIZE: f32 = 18.0;
/// The typed line's line-height multiple, pinned here rather than left to Bevy's default so the
/// caret's height (below) is provably the same number the text lays out with.
const EDIT_LINE_HEIGHT: f32 = 1.2;
/// The edit caret's colour: the reference's caret re-applies **`FONTINSTANCE.textColor`** whenever
/// the font changes (`0x77e2a0`, mask bit 2) — the ctor's `0xFFFFFFFF` is only the pre-font default.
/// So it is the box's text colour by law, not white by coincidence; both the line and the bar take
/// it from here (wow-re `system/ui/scratch/rf85-editbox-caret.md`).
const EDIT_TEXT_COLOR: Color = Color::WHITE;
/// The edit caret is a drawn **bar**, not a character — a `CSimpleTexture` at `E+0x368`, allocated
/// with a different allocator/tag/ctor than the `CSimpleFontString` beside it (wow-re
/// `rf85-editbox-caret.md`, §5-verified; the cursor flush `0x77da80` fires `OnCursorChanged` with
/// four float caret-*position* args). benilla used to append a `"|"` glyph (login) or a static `"_"`
/// (create), which put a font's shape on a font's baseline and re-laid the text out every blink.
///
/// **Width is 4.0 UI units** — byte-verified, not the 1 px this shipped with first (decision 0542).
/// `0x77ba2d–0x77ba67` stores `G1·4/(G3·1024)`, which the client's own internal→Lua converter maps
/// to exactly 4.0 at every aspect and resolution. Same units as every other authored glue number,
/// so it scales with `s` like the rest. Height is the FontString's line height; the bar is
/// vertically centred on the line (the ref anchors caret-LEFT to the FontString's own rect, so
/// TextInsets never enter the caret's own geometry — it inherits them by anchoring).
const CARET_W: f32 = 4.0;

/// The standard edit caret — the drawn [`CARET_W`]-unit bar, one line tall at the box's font size,
/// in the box's text colour, spawned hidden (the owning screen blinks its `Visibility` on the ref's
/// 0.5 s clock, reset solid on every keystroke). Seat it as the flex sibling right after the typed
/// line so it lands at the cursor without anyone measuring text. The ONE caret; every edit box —
/// [`glue_edit_box`]'s chrome and the delete dialog's ChatInputBorder box — spawns it here, so the
/// mechanism can never fork back into per-screen `"|"`/`"_"` glyphs.
pub(crate) fn caret_bar<C: Bundle>(
    parent: &mut ChildSpawnerCommands,
    caret: C,
    font_size: f32,
    s: f32,
) {
    parent.spawn((
        caret,
        Visibility::Hidden,
        Node {
            width: Val::Px(CARET_W * s),
            height: Val::Px(font_size * EDIT_LINE_HEIGHT * s),
            ..default()
        },
        BackgroundColor(EDIT_TEXT_COLOR),
    ));
}

/// A glue edit box's CHROME — the shape every glue EditBox authors (`AccountLoginAccountEdit`/
/// `PasswordEdit`, `CharacterCreateNameEdit`): the `UI-Tooltip-Background` fill tiled at 16 inside
/// the (10,5,4,9) insets, the `Glue-Tooltip-Border` 16-edge over it (both caller-tinted — the
/// login boxes take `DEFAULT_TOOLTIP_COLOR`, the create name box the Alliance row), the text
/// line at the ref's TextInsets (left 15, vertically centered) in `GlueEditBoxFont` (ARIALN 18,
/// white), and the caret bar seated immediately after it (a flex sibling, so it lands at the cursor
/// without anyone measuring the text). `extras` rides the box node (the screen's click-to-focus
/// action + `Button`); `marker` rides the text entity (the screen's refresh writes the content —
/// typed text or password mask); `caret` rides the caret bar (the screen's refresh drives its
/// `Visibility` from focus + blink phase — it spawns hidden).
/// Plain-fill fallback without art. Focus, typing, and the blink clock are the owning screen's
/// systems — this is chrome only, so the two screens' boxes can never fork.
#[allow(clippy::too_many_arguments)]
pub(crate) fn glue_edit_box<E: Bundle, T: Bundle, C: Bundle>(
    parent: &mut ChildSpawnerCommands,
    art: &GlueArt,
    edit_font: &Handle<Font>,
    extras: E,
    marker: T,
    caret: C,
    (w, h): (f32, f32),
    (border, fill): (Color, Color),
    text_insets: (f32, f32, f32, f32),
    s: f32,
) {
    let px = |v: f32| Val::Px(v * s);
    let framed = art.name_border.is_some() && art.tooltip_bg.is_some();
    let mut boxed = parent.spawn((
        extras,
        Node {
            width: px(w),
            height: px(h),
            ..default()
        },
    ));
    if !framed {
        boxed.insert(BackgroundColor(fill.with_alpha(FALLBACK_ALPHA)));
    }
    boxed.with_children(|b| {
        if framed {
            b.spawn((
                tiled_bg_node(art.tooltip_bg.clone().unwrap(), NAME_EDGE, s, fill),
                Node {
                    position_type: PositionType::Absolute,
                    left: px(10.0),
                    right: px(5.0),
                    top: px(4.0),
                    bottom: px(9.0),
                    ..default()
                },
            ));
            backdrop_border(b, art.name_border.as_ref().unwrap(), NAME_EDGE, border);
        }
        // The typed line and its caret share one row (default `FlexDirection::Row`), so the caret
        // is simply the next flex item — it seats at the end of the text with no measuring, and at
        // the left inset when the box is empty.
        // The ref's `<TextInsets>` (left, right, top, bottom): the EditBox's FontString rect is
        // the box inset by them, and the typed line is centred in THAT — not in the whole box. The
        // login boxes inset the bottom by 5, which lifts the line, and the caret beside it, off the
        // box's own centre; the create name box insets only the left, so it stays centred.
        let (ti_l, ti_r, ti_t, ti_b) = text_insets;
        b.spawn((Node {
            position_type: PositionType::Absolute,
            left: px(ti_l),
            right: px(ti_r),
            top: px(ti_t),
            bottom: px(ti_b),
            align_items: AlignItems::Center,
            ..default()
        },))
            .with_children(|f| {
                f.spawn((
                    marker,
                    Text::new(""),
                    TextFont {
                        font: edit_font.clone(),
                        font_size: EDIT_FONT_SIZE * s, // GlueEditBoxFont
                        ..default()
                    },
                    // Its own component in Bevy 0.18, not a `TextFont` field.
                    LineHeight::RelativeToFont(EDIT_LINE_HEIGHT),
                    TextColor(EDIT_TEXT_COLOR),
                    TextLayout {
                        linebreak: LineBreak::NoWrap,
                        ..default()
                    },
                ));
                caret_bar(f, caret, EDIT_FONT_SIZE, s);
            });
    });
}

/// A glue-panel button (Accept/Back/Enter World/…): the real `Glue-Panel-Button` art with its
/// additive hover sheen and a caption that whitens on hover (the ref's `HighlightFont`);
/// plain-fill fallback. Art states + caption color are driven by [`super::glue_button_visuals`].
#[allow(clippy::too_many_arguments)]
pub(crate) fn glue_button<A: Component>(
    parent: &mut ChildSpawnerCommands,
    art: &GlueArt,
    font: &Handle<Font>,
    action: A,
    caption: &str,
    w: f32,
    h: f32,
    kind: GlueBtnKind,
    s: f32,
) {
    let px = |v: f32| Val::Px(v * s);
    let mut b = parent.spawn((
        action,
        GlueBtn,
        GlueDisabled(false),
        Button,
        Node {
            width: px(w),
            height: px(h),
            justify_content: JustifyContent::Center,
            align_items: AlignItems::Center,
            ..default()
        },
    ));
    match &art.button_up {
        Some((up, size)) => {
            b.insert(ImageNode {
                image: up.clone(),
                rect: Some(tc_rect(*size, BUTTON_TC)),
                ..default()
            });
        }
        None => {
            b.insert((FallbackFace, BackgroundColor(BTN_BG)));
        }
    }
    b.with_children(|inner| {
        if let Some(hi) = &art.button_hi {
            inner.spawn((
                Hilight,
                Visibility::Hidden,
                MaterialNode(hi.clone()),
                overlay(),
            ));
        }
        let (font_size, offset) = kind.caption();
        outlined_text(
            inner,
            // The template's authored `<ButtonText>` offset off CENTER. The ref's anchor space is
            // y-UP, so it negates into Bevy's `top`; on a flex item both shift it visually without
            // disturbing the layout, which is exactly an anchor offset.
            Node {
                left: Val::Px(offset.x * s),
                // +1 cancels `outlined_text`'s shared −1 trim for captions: that trim was fitted
                // (director-measured) back when captions drew at dead centre, so once the authored
                // `<ButtonText>` offset above is applied it corrects the same unit twice and the
                // caption reads a notch high. Director's eye, 2026-07-19.
                top: Val::Px((1.0 - offset.y) * s),
                ..default()
            },
            (),
            GlueCaption,
            GlueText {
                text: caption,
                size: font_size,
                color: GOLD,
                wrap: false,
            },
            font,
            s,
        );
    });
}
