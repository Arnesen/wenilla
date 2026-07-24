//! The create screen's right info stack (`CharacterCreateCharacterFaction/Race/Class`, 240 wide at
//! TOPRIGHT (−20,−20), heights 160/260/210, 10 apart): each a `TextPanel-Border` backdrop whose bg
//! tints with the faction, the header icon overhanging the corner, the scrollable title + body +
//! gold ability lines — and the `GlueScrollFrameTemplate` scrollbar along the right edge (16² arrow
//! buttons above/below a knob track, the CharacterCreate decorative track art behind it, everything
//! hidden while the panel has nothing to scroll — the ref's `scrollBarHideable`).

use bevy::prelude::*;
use bevy::ui::FocusPolicy;
use bevy::ui_render::ui_material::MaterialNode;

use super::parts::{
    DynIcon, DynText, DynTint, InfoKind, InfoScroll, ScrollArrow, ScrollHides, ScrollThumb,
};
use crate::glue::art::{
    tc_rect, GlueArt, ScrollArt, ALLIANCE_FILL, FALLBACK_ALPHA, GOLD, INFO_TEXT, PANEL_EDGE,
    SCROLL_BTN_TC,
};
use crate::glue::backdrop::{backdrop_border, tiled_bg_node};
use crate::glue::widgets::{abs, outlined_text, overlay, GlueText, Hilight};

pub(super) fn right_stack(
    ui: &mut ChildSpawnerCommands,
    art: &GlueArt,
    font: &Handle<Font>,
    s: f32,
) {
    let px = |v: f32| Val::Px(v * s);
    ui.spawn((Node {
        position_type: PositionType::Absolute,
        right: px(20.0),
        top: px(20.0),
        width: px(240.0),
        flex_direction: FlexDirection::Column,
        row_gap: px(10.0),
        ..default()
    },))
        .with_children(|stack| {
            for (kind, height) in [
                (InfoKind::Faction, 160.0),
                (InfoKind::Race, 260.0),
                (InfoKind::Class, 210.0),
            ] {
                let mut panel = stack.spawn((Node {
                    width: Val::Percent(100.0),
                    height: px(height),
                    ..default()
                },));
                let framed = art.panel_border.is_some() && art.tooltip_bg.is_some();
                if !framed {
                    panel.insert((
                        DynTint::BoxFill,
                        BackgroundColor(ALLIANCE_FILL.with_alpha(FALLBACK_ALPHA)),
                    ));
                }
                panel.with_children(|panel| {
                    if framed {
                        // The backdrop: tiled `UI-Tooltip-Background` inset (8,4,4,8) under the
                        // untinted `TextPanel-Border` (the ref tints only the bg).
                        panel.spawn((
                            DynTint::BoxFill,
                            tiled_bg_node(
                                art.tooltip_bg.clone().unwrap(),
                                PANEL_EDGE,
                                s,
                                ALLIANCE_FILL,
                            ),
                            Node {
                                position_type: PositionType::Absolute,
                                left: px(8.0),
                                right: px(4.0),
                                top: px(4.0),
                                bottom: px(8.0),
                                ..default()
                            },
                        ));
                        backdrop_border(
                            panel,
                            art.panel_border.as_ref().unwrap(),
                            PANEL_EDGE,
                            Color::WHITE,
                        );
                    }
                    // The header icon, overhanging the top-left corner (48² at (−3, −8)) — ABOVE
                    // the scroll content: the ref's IconFrame `OnLoad` bumps its frame level +1,
                    // so scrolled text slides UNDER the icon, never over it.
                    panel.spawn((
                        DynIcon::Info(kind),
                        ImageNode {
                            color: Color::NONE, // transparent until the refresh assigns art
                            ..default()
                        },
                        ZIndex(1),
                        abs(s, -3.0, -8.0, 48.0, 48.0),
                    ));
                    // The scrollable content (the ref's GlueScrollFrame, 190 wide at (17,−10)).
                    let scroll = panel
                        .spawn((
                            InfoScroll,
                            Interaction::default(),
                            FocusPolicy::Block,
                            ScrollPosition::default(),
                            Node {
                                position_type: PositionType::Absolute,
                                left: px(17.0),
                                top: px(10.0),
                                width: px(190.0),
                                bottom: px(10.0),
                                flex_direction: FlexDirection::Column,
                                overflow: Overflow::scroll_y(),
                                ..default()
                            },
                        ))
                        .with_children(|inner| {
                            outlined_text(
                                inner,
                                Node {
                                    margin: UiRect::left(px(32.0)), // clears the header icon
                                    ..default()
                                },
                                (),
                                DynText::InfoTitle(kind),
                                GlueText {
                                    text: "",
                                    size: 18.0, // GlueFontNormalLarge
                                    color: GOLD,
                                    wrap: false,
                                },
                                font,
                                s,
                            );
                            outlined_text(
                                inner,
                                Node {
                                    margin: UiRect::top(px(2.0)),
                                    ..default()
                                },
                                (),
                                DynText::InfoBody(kind),
                                GlueText {
                                    text: "",
                                    size: 12.0, // GlueFontCharacterCreate
                                    color: INFO_TEXT,
                                    wrap: true,
                                },
                                font,
                                s,
                            );
                            // The racial abilities under the race paragraph, gold — the ref's
                            // separate `CharacterCreateRaceAbilityText` (`GlueFontNormalSmall`).
                            if kind == InfoKind::Race {
                                outlined_text(
                                    inner,
                                    Node {
                                        margin: UiRect::top(px(2.0)),
                                        ..default()
                                    },
                                    (),
                                    DynText::InfoAbilities,
                                    GlueText {
                                        text: "",
                                        size: 12.0, // GlueFontNormalSmall
                                        color: GOLD,
                                        wrap: true,
                                    },
                                    font,
                                    s,
                                );
                            }
                        })
                        .id();
                    if let Some(sc) = &art.scroll {
                        scrollbar(panel, sc, scroll, height, s);
                    }
                });
            }
        });
}

/// One panel's scrollbar (`GlueScrollFrameTemplate` resolved to panel coordinates): the decorative
/// track art behind, then the 16-wide slider column 6px right of the scroll frame — up button,
/// knob track, down button. Every piece carries [`ScrollHides`] (the whole bar vanishes while the
/// panel fits its text, like the ref's `scrollBarHideable` frames).
fn scrollbar(
    panel: &mut ChildSpawnerCommands,
    sc: &ScrollArt,
    scroll: Entity,
    panel_h: f32,
    s: f32,
) {
    // The frame is inset 10 top/bottom; the slider is inset 16 more for the buttons.
    let bar_h = panel_h - 20.0;
    let track_h = bar_h - 32.0;
    // `UI-CharacterCreate-ScrollBar-Top` (32×128 at frame TOPRIGHT (−3,4)) and the ClassTrainer
    // strip (30×123 at frame BOTTOMRIGHT (−3,−2), the authored sub-rect) — panel x 204.
    if let Some((top, _)) = &sc.track_top {
        panel.spawn((
            ScrollHides { scroll },
            Visibility::Hidden,
            ImageNode::new(top.clone()),
            abs(s, 204.0, 6.0, 32.0, 128.0),
        ));
    }
    if let Some((bottom, size)) = &sc.track_bottom {
        panel.spawn((
            ScrollHides { scroll },
            Visibility::Hidden,
            ImageNode {
                image: bottom.clone(),
                rect: Some(tc_rect(*size, [0.53125, 1.0, 0.03125, 1.0])),
                ..default()
            },
            abs(s, 204.0, panel_h - 8.0 - 123.0, 30.0, 123.0),
        ));
    }
    panel
        .spawn((
            ScrollHides { scroll },
            Visibility::Hidden,
            abs(s, 213.0, 10.0, 16.0, bar_h),
        ))
        .with_children(|bar| {
            for (up, top, art) in [(true, 0.0, &sc.up_btn), (false, bar_h - 16.0, &sc.down_btn)] {
                bar.spawn((
                    ScrollArrow {
                        scroll,
                        up,
                        step: track_h * s / 2.0,
                    },
                    Button,
                    ImageNode {
                        image: art.up.clone(),
                        rect: Some(tc_rect(art.size, SCROLL_BTN_TC)),
                        ..default()
                    },
                    abs(s, 0.0, top, 16.0, 16.0),
                ))
                .with_children(|b| {
                    b.spawn((
                        Hilight,
                        Visibility::Hidden,
                        MaterialNode(art.hi.clone()),
                        overlay(),
                    ));
                });
            }
            bar.spawn(abs(s, 0.0, 16.0, 16.0, track_h))
                .with_children(|track| {
                    track.spawn((
                        ScrollThumb {
                            scroll,
                            travel: (track_h - 16.0) * s,
                        },
                        Button,
                        ImageNode {
                            image: sc.knob.0.clone(),
                            rect: Some(tc_rect(sc.knob.1, SCROLL_BTN_TC)),
                            ..default()
                        },
                        abs(s, 0.0, 0.0, 16.0, 16.0),
                    ));
                });
        });
}
