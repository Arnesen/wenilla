//! The loading screen's **tip of the day** (decision 2077; wow-re
//! `system/loadingscreen/scratch/game-tip-of-the-day.md`).
//!
//! `showGameTips`' only mention in 1.12's FrameXML is its options row and the tooltip string
//! "Uncheck this to hide the tip of the day in the load screens", so the whole feature is
//! engine-side: `CGlueMgr::EnterWorld` picks a row from `GameTips.dbc`, hands it to the loading
//! screen's text slot, and writes the *next* index back into the `gameTip` CVar.
//!
//! ## The selection law (`0x46b662`–`0x46b6e3`)
//!
//! **`gameTip` holds the NEXT index, not the one on screen.** The client reads it, shows *that*
//! row, then stores `read + 1`, so on-disk values run 1..=74 and never 0 — a client that stores the
//! index it just showed is off by one, and the reference's own `Config.wtf` (`SET gameTip "34"`) is
//! what a naive reading mis-anchors on.
//!
//! **The wrap is a clamp, not a modulo**: `if (i < 0 || i >= count) i = 0` at `0x46b682`–`0x46b68f`.
//! `count <= 0` bails. The walk is **sequential**, never random.
//!
//! Two guards sit in front of it. `showGameTips` off jumps past the `CVar::Set` as well as the
//! draw (`0x46b671`/`0x46b678`), so **turning tips off freezes the index** rather than advancing it
//! invisibly. And `[selChar+0x10a]` — a *client-synthesised* flag, set by `0x5b42a0` iff the
//! `SMSG_CHAR_ENUM` level byte arrived as `0` — suppresses the tip for a character the roster
//! reported at level zero. vmangos always sends a real level, so that arm is unreachable against
//! our server; it is honoured anyway because it costs one comparison.
//!
//! ## Where it draws
//!
//! **Only on the glue→world transition.** The setter `0x406630` (a seven-byte
//! `mov [0x882e10],ecx; ret`) has exactly one caller, and neither `SMSG_TRANSFER_PENDING` arm sets
//! it — so an in-world portal or worldport screen carries **no tip**. That is why this module hangs
//! off the loading screen's `world entry` raise and clears on every other one.
//!
//! The reference lays the text out **once per raise** and draws it every frame, between the
//! background quad and the progress bar. Its placement, in the screen's own `[0,1]` ortho:
//! `s = 515 / (a · 1024)` where `a = [0x832a4c]` is **aspect × 0.75** (not the aspect — the writer
//! `0x41ad10` multiplies by `0.75` first, and reading it as W/H inflates the wrap width by a third
//! at 16:9); position `(0.5 − s·0.5, 0.1 + yoff, 0)`; `justifyH = 0` (left); wrap width `s`; body
//! `0xd7c8c8c8` ARGB with an opaque black shadow at `(+0.001, −0.001)`.
//!
//! **`yoff` is `(1 − a)·0.5` and we do not apply it, on purpose.** wow-re left that term
//! *unreconciled*: `0x406a60` already applies the letterbox as a viewport, and `yoff` is
//! bit-for-bit the same `(1 − a)·0.5` added again inside it, which may double-count on a display
//! narrower than 4:3. benilla's loading screen is a **4:3 content box by construction** (the root
//! letterboxes and the area is `100vh × 4/3`), and at 4:3 the reference's own `yoff` is exactly
//! zero — so placing the tip in that box at `y = 0.1` is the reference's layout on the one aspect
//! where the ambiguity cannot bite. If wow-re settles the term differently the fix is this comment
//! and one constant.
//!
//! The `|cffffd100Tip:|r ` prefix and the trailing `\r\n` are **in the DBC data**, and the escapes
//! *are* interpreted (`0x5c28af`, flags bit `0x800` clear), so the gold "Tip:" is markup rather
//! than literal text — which is why the line is built through [`benilla_ui::markup`] rather than
//! drawn as one string.

use benilla_ui::markup::{self, TokenKind};
use bevy::prelude::*;

use benilla_formats::GameTipsCatalog;

/// `s = 515 / (a · 1024)` at `a = 1` — the 4:3 case, which is the aspect of the content box this
/// draws into. It is the text's **scale**, and it doubles as the wrap width in the same `[0,1]`
/// space (`0x406e18` passes `s` for both).
///
/// **Not a FrameXML length.** The reference's idiom here is the FrameXML-unit conversion with one
/// extra instruction (`0x41ae40` divides `G44` back out), so reusing that helper is off by 1.25 at
/// 4:3.
const TIP_SCALE: f32 = 515.0 / 1024.0;

/// `pos.y` — the tip's baseline in the screen's `[0,1]` ortho, measured from the bottom, sitting
/// just above the progress bar's `cy = 0.075`.
const TIP_BOTTOM: f32 = 0.1;

/// `0.018 · [0x832a48]` — the font height as a fraction of the viewport height.
const TIP_FONT_FRACTION: f32 = 0.018;

/// The body colour `0xd7c8c8c8` (ARGB) and its shadow offset, in the same `[0,1]` space.
const TIP_COLOR: Color = Color::srgba(200.0 / 255.0, 200.0 / 255.0, 200.0 / 255.0, 215.0 / 255.0);
const TIP_SHADOW_OFFSET: f32 = 0.001;

/// The two CVars, mirrored: `showGameTips` and the `gameTip` cursor.
///
/// `next` is an `i64` rather than a `u32` because the CVar is a string a player (or a downgraded
/// build, or a bigger locale table) can leave anything in, and the reference's own tolerance is a
/// clamp at read time (`0x46b682`'s `i < 0 || i >= count`), not a validation at write time.
#[derive(Resource, Debug, Clone, Copy)]
pub(crate) struct GameTipSetting {
    pub(crate) show: bool,
    pub(crate) next: i64,
}

impl Default for GameTipSetting {
    fn default() -> Self {
        GameTipSetting {
            show: true,
            next: 0,
        }
    }
}

/// The tips table plus the one piece of state the reference keeps beside it: which row the screen
/// currently shows, laid out once per raise.
#[derive(Resource, Default)]
pub(crate) struct GameTips {
    /// `GameTips.dbc` in file order — the array `[0xc0dcd0]`/`[0xc0dcd4]`.
    catalog: GameTipsCatalog,
    /// `[0x882e10]` — the row the current screen shows, or `None` for a screen with no tip (tips
    /// off, an in-world transfer, an empty table).
    shown: Option<String>,
}

impl GameTips {
    /// `EnterWorld`'s tip block: read the stored index, clamp it, take that row, and return the
    /// index to store back (`shown + 1`).
    ///
    /// `None` when the table is empty (`count <= 0` bails at `0x46b684`).
    fn take(&self, stored: i64) -> Option<(&str, u32)> {
        let count = self.catalog.len();
        if count == 0 {
            return None;
        }
        // The clamp IS the wrap — there is no modulo, and the second range test at
        // `0x46b695`–`0x46b69b` is dead.
        let index = if stored < 0 || stored >= count as i64 {
            0
        } else {
            stored as usize
        };
        let tip = self.catalog.get(index)?;
        Some((tip, index as u32 + 1))
    }

    /// The line the screen is showing, if any.
    pub(crate) fn shown(&self) -> Option<&str> {
        self.shown.as_deref()
    }
}

/// The tip split into coloured runs, ready for a Bevy text tree — `|cAARRGGBB…|r` honoured, the
/// trailing `\r\n` the data carries dropped rather than drawn as blank lines.
///
/// Returns `(text, colour)` pairs against `base`, which is the string's own colour that `|r`
/// restores (`0x5cce99` restores `FontString+0x2c`).
pub(crate) fn spans(tip: &str, base: Color) -> Vec<(String, Color)> {
    let mut out: Vec<(String, Color)> = Vec::new();
    let mut colour = base;
    let mut at = 0;
    let mut run = String::new();
    let flush = |run: &mut String, colour: Color, out: &mut Vec<(String, Color)>| {
        if !run.is_empty() {
            out.push((std::mem::take(run), colour));
        }
    };
    while let Some(token) = markup::token_at(tip, at) {
        at += token.byte_len;
        match token.kind {
            TokenKind::Color(rgba) => {
                flush(&mut run, colour, &mut out);
                colour = Color::srgb_u8(rgba.r(), rgba.g(), rgba.b());
            }
            TokenKind::ColorReset => {
                flush(&mut run, colour, &mut out);
                colour = base;
            }
            TokenKind::LineBreak => run.push('\n'),
            TokenKind::EscapedPipe => run.push('|'),
            TokenKind::Char(c) => run.push(c),
            // No shipped tip carries a hyperlink; a locale archive that adds one draws its visible
            // text and loses only the click, which nothing on a loading screen could use anyway.
            TokenKind::LinkOpen { .. } | TokenKind::LinkClose => {}
        }
    }
    flush(&mut run, colour, &mut out);
    // The data's trailing `\r\n` (one tip carries two) would otherwise draw as empty lines under
    // the sentence and push the block off its anchor.
    if let Some((last, _)) = out.last_mut() {
        while last.ends_with('\n') {
            last.pop();
        }
    }
    out.retain(|(t, _)| !t.is_empty());
    out
}

/// The block's geometry as **percentages of the 4:3 content box** — `(left, bottom, width)`.
/// Percent rather than pixels because the node is laid out once per raise, and a window resize
/// mid-load must not strand it.
pub(crate) const fn geometry() -> (f32, f32, f32) {
    (
        (0.5 - TIP_SCALE * 0.5) * 100.0,
        TIP_BOTTOM * 100.0,
        TIP_SCALE * 100.0,
    )
}

/// The two numbers that can only be pixels — the font height and the shadow offset — for a content
/// box of `width × height` logical pixels.
pub(crate) fn layout(width: f32, height: f32) -> TipLayout {
    TipLayout {
        font_size: height * TIP_FONT_FRACTION,
        shadow: Vec2::new(width * TIP_SHADOW_OFFSET, height * TIP_SHADOW_OFFSET),
    }
}

/// What [`layout`] resolves to, in logical pixels inside the loading screen's 4:3 content box.
pub(crate) struct TipLayout {
    pub(crate) font_size: f32,
    /// `(+x, −y)` in the reference; Bevy's shadow offset is `(+x, +y)` with y growing DOWN, so the
    /// sign is already right.
    pub(crate) shadow: Vec2,
}

/// The body colour the tip's own `|r` restores to.
pub(crate) const fn base_color() -> Color {
    TIP_COLOR
}

pub(crate) struct GameTipPlugin;

impl Plugin for GameTipPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<GameTips>()
            // The CVar mirror, init'd HERE and not by the CVar host: `KnobParams` takes it as a
            // plain `ResMut`, so a build that registers the row without the resource panics the
            // first `load_config` — which is what a live run caught and no unit test could, the
            // test harness having its own `init_resource` chain.
            .init_resource::<GameTipSetting>()
            .add_systems(
                Startup,
                load_game_tips.after(benilla_assets::AssetSet::Open),
            )
            // After the loading screen's own drive, which is what sets the edge this reads.
            .add_systems(
                Update,
                drive_game_tip.after(benilla_world::schedule::WorldStage::Present),
            );
    }
}

/// Take the raise's tip edge, then paint whatever the screen is showing.
///
/// Two jobs in one system because they are one mechanism seen at two moments: the reference picks
/// the row inside `EnterWorld` and lays it out once per raise, then draws that layout every frame.
#[allow(clippy::too_many_arguments)]
fn drive_game_tip(
    mut screen: ResMut<crate::loading_screen::LoadingScreen>,
    mut tips: ResMut<GameTips>,
    mut setting: ResMut<GameTipSetting>,
    roster: Option<Res<crate::char_select::Roster>>,
    mut node: Query<
        (
            Entity,
            &mut Node,
            &mut TextFont,
            &mut TextShadow,
            &mut Visibility,
        ),
        With<crate::loading_screen::LoadingTip>,
    >,
    windows: Query<&Window>,
    assets: Res<AssetServer>,
    mut commands: Commands,
    // The cursor persists through the VM's table, not the knob — see `cvars::write_host_cvar`.
    script: Option<NonSendMut<benilla_ui::script::UiScript>>,
    mut persist: ResMut<crate::cvars::CvarPersist>,
) {
    if let Some(edge) = screen.take_tip_edge() {
        let next = match edge {
            crate::loading_screen::TipEdge::Pick => raise(
                &mut tips,
                setting.show,
                roster
                    .as_ref()
                    .is_some_and(|r| r.pending_level() == Some(0)),
                setting.next,
            ),
            crate::loading_screen::TipEdge::Clear => {
                clear(&mut tips);
                None
            }
        };
        if let Some(next) = next {
            // The cursor moves only when a tip was actually shown — a suppressed tip freezes it.
            setting.next = i64::from(next);
            if let Some(mut script) = script {
                crate::cvars::write_host_cvar(
                    &mut script,
                    &mut persist,
                    "gameTip",
                    &next.to_string(),
                );
            }
            // The run's own evidence: which row this screen carries and where the cursor lands.
            // A tip is a picture on a screen that is up for a second, so "did it pick one" is a
            // question for a log line, not for a capture.
            info!("loading screen: tip {} of {}", next - 1, tips.catalog.len());
        }
        // The text is laid out once per raise, not per frame, exactly as the reference does.
        let Ok((entity, mut n, mut font, mut shadow, mut vis)) = node.single_mut() else {
            return;
        };
        let Some(tip) = tips.shown() else {
            *vis = Visibility::Hidden;
            commands.entity(entity).despawn_related::<Children>();
            return;
        };
        // The 4:3 content box is `100vh` tall and `100vh · 4/3` wide, so the window's height is the
        // box's height and the box's width follows from it.
        let height = windows.iter().next().map_or(768.0, |w| w.height());
        let l = layout(height * 4.0 / 3.0, height);
        let (left, bottom, width) = geometry();
        n.left = Val::Percent(left);
        n.bottom = Val::Percent(bottom);
        n.width = Val::Percent(width);
        font.font = crate::char_select::wow_font(&assets);
        font.font_size = l.font_size;
        shadow.offset = l.shadow;
        shadow.color = Color::BLACK;
        *vis = Visibility::Inherited;

        // The coloured runs: the first is the `Text` root's own, the rest are `TextSpan` children.
        let runs = spans(tip, base_color());
        let mut e = commands.entity(entity);
        e.despawn_related::<Children>();
        match runs.split_first() {
            Some(((head, head_color), rest)) => {
                e.insert((Text::new(head.clone()), TextColor(*head_color)));
                let (rest, tf) = (rest.to_vec(), font.clone());
                e.with_children(|c| {
                    for (text, color) in rest {
                        c.spawn((TextSpan::new(text), tf.clone(), TextColor(color)));
                    }
                });
            }
            None => {
                e.insert(Text::new(String::new()));
                *vis = Visibility::Hidden;
            }
        }
    }
}

/// `GameTips.dbc`, once, off the patch chain — the reference's own single linear load, with no
/// reload path. `.after(AssetSet::Open)` for the reason every DBC load in this tree carries it.
fn load_game_tips(mut tips: ResMut<GameTips>, assets: Option<Res<benilla_assets::WorldAssets>>) {
    let Some(assets) = assets else { return };
    use benilla_assets::LockRecover;
    let mut chain = assets.chain.lock_recover();
    match benilla_formats::load_game_tips(&mut chain) {
        Ok(cat) => {
            info!("loading screen: {} game tips", cat.len());
            tips.catalog = cat;
        }
        // A missing table is a loading screen with no tip, not a boot failure.
        Err(e) => warn!("GameTips.dbc unavailable — loading screens carry no tip: {e:#}"),
    }
}

/// Pick the row for a screen that is about to rise, and hand back the index to store.
///
/// `show` is `showGameTips`; `level_is_zero` is `[selChar+0x10a]`. Both suppress the tip AND the
/// advance — the reference jumps past its own `CVar::Set`, so an off switch freezes the index.
pub(crate) fn raise(
    tips: &mut GameTips,
    show: bool,
    level_is_zero: bool,
    stored: i64,
) -> Option<u32> {
    if !show || level_is_zero {
        tips.shown = None;
        return None;
    }
    match tips.take(stored) {
        Some((tip, next)) => {
            tips.shown = Some(tip.to_string());
            Some(next)
        }
        None => {
            tips.shown = None;
            None
        }
    }
}

/// Every other raise — an in-world portal or worldport — carries no tip.
pub(crate) fn clear(tips: &mut GameTips) {
    tips.shown = None;
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tips(rows: &[&str]) -> GameTips {
        GameTips {
            catalog: GameTipsCatalog::from_tips(rows.iter().map(|s| (*s).to_string()).collect()),
            shown: None,
        }
    }

    /// **`gameTip` holds the NEXT index.** A fresh client's registered `"0"` shows row 0 and stores
    /// 1, so on-disk values run 1..=count and never 0 — the off-by-one a client that stores what it
    /// just showed would ship.
    #[test]
    fn the_stored_index_is_the_next_one_not_the_shown_one() {
        let mut t = tips(&["a", "b", "c"]);
        assert_eq!(raise(&mut t, true, false, 0), Some(1));
        assert_eq!(t.shown(), Some("a"));
        assert_eq!(raise(&mut t, true, false, 1), Some(2));
        assert_eq!(t.shown(), Some("b"));
    }

    /// The wrap is the **clamp**, not a modulo: anything at or past the count restarts at 0, and so
    /// does a negative. A stored index from a bigger table (a locale archive, a patch) lands on
    /// row 0 rather than nothing.
    #[test]
    fn the_clamp_is_the_wrap() {
        let mut t = tips(&["a", "b", "c"]);
        assert_eq!(raise(&mut t, true, false, 3), Some(1), "count wraps to 0");
        assert_eq!(t.shown(), Some("a"));
        assert_eq!(
            raise(&mut t, true, false, 900),
            Some(1),
            "far past, same clamp"
        );
        assert_eq!(raise(&mut t, true, false, -4), Some(1), "and negative");
    }

    /// Both guards suppress the tip **and the advance** — the reference jumps past its own
    /// `CVar::Set`, so turning tips off freezes the index rather than burning through the table
    /// invisibly.
    #[test]
    fn a_suppressed_tip_freezes_the_index() {
        let mut t = tips(&["a", "b", "c"]);
        assert_eq!(raise(&mut t, false, false, 1), None, "showGameTips off");
        assert_eq!(t.shown(), None);
        assert_eq!(raise(&mut t, true, true, 1), None, "the level-zero guard");
        assert_eq!(t.shown(), None);
        // …and the index it would have advanced is untouched: the next armed raise still reads 1.
        assert_eq!(raise(&mut t, true, false, 1), Some(2));
        assert_eq!(t.shown(), Some("b"));
    }

    /// An empty table bails (`count <= 0` at `0x46b684`) rather than showing a blank line.
    #[test]
    fn an_empty_table_shows_nothing() {
        let mut t = tips(&[]);
        assert_eq!(raise(&mut t, true, false, 0), None);
        assert_eq!(t.shown(), None);
    }

    /// The `|c…|r` prefix is **markup in the data**, so the gold "Tip:" is its own run and the rest
    /// falls back to the body colour — and the trailing `\r\n` the DBC carries is dropped rather
    /// than drawn as blank lines under the sentence.
    #[test]
    fn the_gold_tip_prefix_is_markup_and_the_trailing_newlines_go() {
        let base = base_color();
        let runs = spans("|cffffd100Tip:|r Nearby questgivers.\r\n", base);
        assert_eq!(runs.len(), 2, "two runs: {runs:?}");
        assert_eq!(runs[0].0, "Tip:");
        assert_eq!(runs[0].1, Color::srgb_u8(0xff, 0xd1, 0x00));
        assert_eq!(runs[1].0, " Nearby questgivers.");
        assert_eq!(runs[1].1, base);
    }

    /// The placement, at the one aspect the reference's own `yoff` is zero — a 4:3 content box,
    /// which is what benilla's loading screen always is. `s = 515/1024`, left-anchored at
    /// `0.5 − s/2`, wrap width `s`, baseline at `0.1` from the bottom.
    #[test]
    fn the_layout_is_the_reference_numbers_at_four_three() {
        let (left, bottom, width) = geometry();
        // `s = 515/1024` — a 515 px column in a 1024-wide box, centred, and the baseline a tenth of
        // the box's height up, just clear of the progress bar's `cy = 0.075`.
        assert!(
            (width * 0.01 * 1024.0 - 515.0).abs() < 0.01,
            "s·width = 515 px"
        );
        assert!(
            (left * 0.01 * 1024.0 - (1024.0 - 515.0) / 2.0).abs() < 0.01,
            "centred column"
        );
        assert!((bottom - 10.0).abs() < 0.001, "0.1 of the box height");
        let l = layout(1024.0, 768.0);
        assert!(
            (l.font_size - 13.824).abs() < 0.01,
            "0.018 of the box height"
        );
    }
}
