//! The chat **window** surface: `GetChatWindowInfo` (the engine's per-window settings getter) and
//! `ChatFrame_OpenChat` (the FrameXML verb that opens the edit box prefilled).
//!
//! The windows themselves are frames, declared in the app's shipped `ChatFrame.xml` —
//! `ChatFrame1`..`ChatFrame7`, of which 1 and 2 are docked and 3..7 ship hidden and undocked, the
//! reference's own arrangement. This module is the *settings* half: what an addon learns when it
//! asks about window `i` without touching the frame.
//!
//! ## `GetChatWindowInfo(id)` → `name, fontSize, r, g, b, a, shown, locked, docked`
//!
//! A registered Lua binding in the real client (`0x4a0ba0`, wow-re `system/ui/ledger.tsv`), reading
//! the per-window struct array at `0xb4fe50` — stride 0x98, **10 windows**, colour bytes at
//! +0xd8..+0xdb renormalised by the f32 1/255 (`system/ui/scratch/chat-color-table.md`). Those
//! structs are loaded from the character's `chat-cache.txt`, and a real stock one is on disk
//! (`wow-5875-re/WoW/WTF/Account/ONE/VMaNGOS/Onepaladin/chat-cache.txt`) — every value below is
//! quoted from it:
//!
//! ```text
//! WINDOW 1   SIZE 0  COLOR 0 0 0 0  LOCKED 1  DOCKED 1  SHOWN 1
//! WINDOW 2   SIZE 0  COLOR 0 0 0 0  LOCKED 1  DOCKED 2  SHOWN 0
//! WINDOW 3…10 SIZE 0 COLOR 0 0 0 0  LOCKED 1  DOCKED 0  SHOWN 0
//! ```
//!
//! **Three traps live in that tuple, and all three are load-bearing:**
//!
//! 1. **`name` is the empty string, not "General".** Nothing writes a window name until the user
//!    renames a tab, so a stock client answers `""` for every window including the first two. The
//!    familiar labels are a *FrameXML fallback*, not data: `FCF_SetWindowName` (FloatingChatFrame.lua
//!    l.680-684) reads `if ( not name or name == "" )` and substitutes `GENERAL` / `COMBAT_LOG` /
//!    `format(CHAT_NAME_TEMPLATE, id)`. Our own tab labels are that fallback, hardcoded. An addon
//!    proves the contract from the other side: `Enchantrix/EnxConfig.lua:113` does
//!    `local name = GetChatWindowInfo(i); if ( name == "" ) then if (i == 1) then name =
//!    _ENCH('TextGeneral') …` — it would not have been written if the getter answered "General".
//!    Answering "General" here would look friendlier and would be a divergence.
//! 2. **`shown`/`locked`/`docked` are numbers or `nil` — never `0`.** FrameXML branches on them
//!    bare (`if ( shown ) then chatFrame:Show()`, `if ( docked ) then FCF_DockFrame(…)` —
//!    FloatingChatFrame.lua l.59/69), and `0` is TRUE in Lua. A getter that returned the cache's
//!    literal `0` would show every hidden window.
//! 3. **`docked` is a dock POSITION, not a flag.** Window 1 answers `1`, window 2 answers `2` —
//!    the order tabs sit in the dock, which is why `FCF_DockFrame(frame, docked)` takes it as an
//!    index. Reading it as a boolean happens to work; reading it as "is docked" and writing back
//!    `1` would silently reorder the dock.
//!
//! `fontSize` is the cache's `SIZE`, and `0` is what a stock client stores — "use the font's own
//! height". No FrameXML path applies it (`FloatingChatFrame_Update` destructures it and never
//! reads it; the options dropdown checks `FCF_GetCurrentChatFrame():GetFont()` instead), and no
//! corpus addon reads it.
//!
//! ## The three setters, and why this getter stopped being a constant (decision 1589)
//!
//! Until 1589 the whole tuple above was a frozen table: benilla had no way to *change* a window's
//! look, so answering with the stock cache row was the honest answer. B246 ("no chat options at
//! all — background transparency has no home") is the report that ends that, and the reference's
//! own home for it is the chat tab's right-click menu → **Background**, a colour swatch with an
//! opacity slider. So three of the nine values are now real state, written by the reference's own
//! three setters and read straight back out of this getter:
//!
//! | binding | address | what it stores |
//! |---|---|---|
//! | `SetChatWindowColor(id, r, g, b)` | `0x4a14f0` | `__ftol(x · 255.0)` per channel |
//! | `SetChatWindowAlpha(id, a)` | `0x4a15d0` | `__ftol(a · 255.0)` |
//! | `SetChatWindowSize(id, size)` | `0x4a1470` | the cache's `SIZE` |
//!
//! (wow-re `system/ui/ledger.tsv:9449-9451` + `scratch/item17-frameapi-fullcarve.md` l.17-18,
//! VERIFIED; `[0x806498]` is the 255.0 the first two multiply by.) **Bytes, not floats** — that is
//! why the getter renormalises by 1/255, and why [`ChatWindowLook`] stores the colour as `u8`: a
//! set→get round trip through the real client quantises, and a store of `f32` here would
//! round-trip values the reference cannot hold. `SetChatWindowAlpha(1, 0.4)` answers
//! `102/255 = 0.4`, not `0.4`.
//!
//! **The record's own layout**, from the §5 dispatched for this work
//! (`system/ui/scratch/chat-window-record.md`): the colour is ONE packed `CImVector` at
//! **`+0x88` B, `+0x89` G, `+0x8a` R, `+0x8b` A** — note the **BGRA** order — and the font size is
//! an `i32` at **`+0x84`**, not a byte and with no `× 255` anywhere near it. (Both correct this
//! module's earlier `+0xd8..+0xdb`, which was the low half of the *absolute* operand
//! `[esi + 0xb4fed8]`: `0xb4fed8 − 0xb4fe50 = 0x88`.) `chat-cache.txt`'s `COLOR` line is written
//! **R G B A** from `+0x8a,+0x89,+0x88,+0x8b` and parsed straight back as bytes, so the file
//! round-trip is bit-exact and the `×255` / `×1/255` pair exists only at the Lua boundary.
//!
//! Two behaviours of the reference's setters that are easy to miss and are transcribed here:
//!
//! - **`SetChatWindowSize` silently drops a size `<= 0`** (`0x4a14bc jle`). A stock cache holds
//!   `SIZE 0`, so "no size stored" and "cannot store 0" coexist: the field is only ever written by
//!   a real pick off the Font Size menu.
//! - **Nothing clamps.** `__ftol` (`0x40a2b0`) truncates, and the setters store the low byte of
//!   the result with no bound: on a real client `SetChatWindowAlpha(1, 2.0)` stores **254**,
//!   `(1, -1.0)` stores **1**, and `0.5` stores **127**, not 128.
//!
//! **We clamp where it wraps, and that is the one deliberate divergence.** Nothing in FrameXML or
//! the corpus can reach the out-of-domain case (the colour picker's channels and its opacity
//! slider are all `0..1`), and the camera pose file already set this posture: a value nothing can
//! produce is not a thing to be faithful to. The truncation itself IS kept — `0.5` stores 127 here
//! too.
//!
//! The other six values stay constants, and that is the honest tree rather than an omission:
//! benilla has no rename, no undock, no window create/close, so `name`, `shown`, `locked` and
//! `docked` have nothing that could move them (0288 §2 — window management is a later arc).
//!
//! Measured demand: **3 of the 5 corpus addons that iterate `NUM_CHAT_WINDOWS` call this on the
//! very next line** — `EnhTooltip/Tooltip.lua:1302`, `MikScrollingBattleText.lua:1951` and
//! `Enchantrix/EnxConfig.lua:110`. The first two are the same idiom (look for a window the user
//! named "debug"/"ettdebug"); the third builds a name→index map for its `/enx print-in` config.
//! Without this getter, declaring `NUM_CHAT_WINDOWS` hands those three a loop that raises on its
//! first iteration, so the constant and the getter are one change, not two.
//!
//! ## `ChatFrame_OpenChat(text, chatFrame)`
//!
//! FrameXML in the reference (ChatFrame.lua l.1545), and the same seam as its neighbour
//! `ChatFrame_SendTell` ([`super::party`]): benilla's chat edit machine is app-side
//! (`benilla_app::ui_chat::edit`), so the verb queues the request and the app opens the box.

use mlua::{Lua, MultiValue, Value};

use super::Model;

/// How many chat windows the client's own settings array holds — **10**, both in the engine
/// (`0xb4fe50`, stride 0x98) and in `chat-cache.txt`, which writes `WINDOW 1` … `WINDOW 10`.
/// FrameXML declares `ChatFrame1TabDockRegion`..`ChatFrame10TabDockRegion` to match, and then
/// builds only seven `ChatFrame`s: `NUM_CHAT_WINDOWS = 7` is the *UI's* count, not the engine's.
/// Recorded so the 7 below reads as a deliberate choice rather than a miscount.
pub(super) const ENGINE_CHAT_WINDOW_SLOTS: usize = 10;

/// The windows benilla actually builds — `ChatFrame1`..`ChatFrame7` in the app's `ChatFrame.xml`,
/// which is also `NUM_CHAT_WINDOWS`. `benilla_app`'s `chat_tests` cross-checks every entry against
/// the shipped XML so the two cannot drift.
pub(super) const NUM_CHAT_WINDOWS: usize = 7;

/// `(shown, docked)` per window, 1-based, as the tuple's *Lua* values: `None` where the reference
/// answers `nil`. Everything else in the tuple is identical across all seven windows on a stock
/// client — name `""`, size `0`, colour `0,0,0`, alpha `0` — so only the two that differ are
/// tabulated. `locked` is `1` for every window (the cache's `LOCKED 1`), and benilla has no unlock
/// path at all, so it is a constant below rather than a column here.
const WINDOW_STATE: [(Option<i64>, Option<i64>); NUM_CHAT_WINDOWS] = [
    (Some(1), Some(1)), // 1 "General"    — shown, dock position 1
    (None, Some(2)),    // 2 "Combat Log" — docked at position 2, not shown (the dock shows one)
    (None, None),       // 3 — hidden, undocked
    (None, None),       // 4
    (None, None),       // 5
    (None, None),       // 6
    (None, None),       // 7
];

/// One chat window's **look** — the mutable slice of the engine's per-window record (decision
/// 1589): the background tint, the background alpha, and the font size. The colour is bytes
/// because the engine's colour is bytes (`0xb4fe50 + n*0x98 + 0x88..0x8b`, packed BGRA,
/// `__ftol(x · 255.0)` in and `× 1/255` out); the font size is an `i32` because the engine's is
/// (`+0x84`). See this module's docs for why that quantisation is kept rather than smoothed over.
///
/// [`Self::default`] is the stock `chat-cache.txt` row — `COLOR 0 0 0 0`, `SIZE 0` — i.e. a black
/// box at alpha 0, which is the classic "chat is text over the world until you mouse over it".
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct ChatWindowLook {
    /// Background tint, `0..=255` each. The engine renormalises by 1/255 on the way out.
    pub r: u8,
    /// See [`Self::r`].
    pub g: u8,
    /// See [`Self::r`].
    pub b: u8,
    /// Background alpha, `0..=255`. `0` is the stock value and what benilla shipped as a constant
    /// until 1589 — the hover fade lifts it to `DEFAULT_CHATFRAME_ALPHA` and drops it back.
    pub a: u8,
    /// The cache's `SIZE` — the chat font height in points, or `0` for "the font's own height".
    /// The reference's own values are `CHAT_FONT_HEIGHTS = {12, 14, 16, 18}`. An `i32` because the
    /// record's field is (`+0x84`), and never `<= 0` once set: the setter drops those.
    pub font_size: i32,
}

impl ChatWindowLook {
    /// The stock `chat-cache.txt` row, as a const so [`super::Model`]'s array literal can use it.
    pub(super) const DEFAULT: Self = Self {
        r: 0,
        g: 0,
        b: 0,
        a: 0,
        font_size: 0,
    };
}

/// `x` in the reference's `0..1` float domain → the byte its `__ftol(x · 255.0)` stores, clamped
/// (see the module docs' divergence note). `NaN` lands on `0` rather than propagating.
fn to_byte(x: f64) -> u8 {
    if !x.is_finite() {
        return 0;
    }
    (x * 255.0).trunc().clamp(0.0, 255.0) as u8
}

/// The byte the engine stores → the `0..1` float `GetChatWindowInfo` answers.
fn from_byte(b: u8) -> f64 {
    f64::from(b) / 255.0
}

/// The 1-based Lua window id → a live index, or the raise every out-of-range ask gets. Shared by
/// the getter and the three setters so a window that cannot be read cannot be written either.
fn window_index(id: i64) -> mlua::Result<usize> {
    if id < 1 || id as usize > NUM_CHAT_WINDOWS {
        return Err(mlua::Error::runtime(format!(
            "chat window {id} out of range — benilla builds ChatFrame1..ChatFrame{NUM_CHAT_WINDOWS} \
             (the client's settings array holds {ENGINE_CHAT_WINDOW_SLOTS} slots, but only \
             {NUM_CHAT_WINDOWS} have frames)"
        )));
    }
    Ok(id as usize - 1)
}

impl super::UiScript {
    /// Seed the per-window looks from the host's persisted store — the load path, so it queues no
    /// change (an echo would re-dirty the file it was just read from; [`Self::set_cvar_host`]'s
    /// reason, one store over).
    pub fn set_chat_window_looks(
        &mut self,
        looks: impl IntoIterator<Item = (usize, ChatWindowLook)>,
    ) {
        let mut model = self.model_mut();
        for (i, look) in looks {
            if let Some(slot) = model.chat_window_looks.get_mut(i) {
                *slot = look;
            }
        }
    }

    /// Snapshot every window's look, index 0 = `ChatFrame1` — what the saver writes out.
    pub fn chat_window_looks(&self) -> Vec<ChatWindowLook> {
        self.model_mut().chat_window_looks.to_vec()
    }

    /// Drain the 0-based indices whose look Lua moved since the last call — the host's cue to
    /// persist. Deduplicated and ascending, so a slider drag that wrote one window forty times
    /// costs the saver one entry.
    pub fn take_chat_window_changes(&mut self) -> Vec<usize> {
        let mut v: Vec<usize> = std::mem::take(&mut self.model_mut().chat_window_changes)
            .into_iter()
            .collect();
        v.sort_unstable();
        v
    }
}

impl super::UiScript {
    /// Drain the `ChatFrame_OpenChat` requests queued since the last call — each is the text the
    /// caller wants the chat edit box to open prefilled with. The app opens and focuses the box,
    /// applies its own sticky-type law, and lets the live parse take the text from there.
    pub fn take_open_chat_requests(&mut self) -> Vec<String> {
        std::mem::take(&mut self.model_mut().open_chat_requests)
    }
}

pub(super) fn install(lua: &Lua) -> mlua::Result<()> {
    // GetChatWindowInfo(id) → name, fontSize, r, g, b, a, shown, locked, docked.
    //
    // Out of range RAISES rather than answering. The reference's array has 10 slots and benilla
    // builds 7 windows; a question about window 8 is a question about a window that does not exist
    // here, and the two honest answers are "raise" or "invent a row". Every corpus caller loops
    // `1, NUM_CHAT_WINDOWS`, so the raise is unreachable from real code and exists to keep the
    // next caller from reading a fabricated window as a real one.
    lua.globals().set(
        "GetChatWindowInfo",
        lua.create_function(|lua, id: i64| {
            let i = window_index(id)?;
            let look = lua
                .app_data_ref::<Model>()
                .expect("model app_data")
                .chat_window_looks[i];
            let (shown, docked) = WINDOW_STATE[i];
            let num = |v: Option<i64>| match v {
                Some(n) => Value::Integer(n),
                None => Value::Nil,
            };
            Ok(MultiValue::from_vec(vec![
                // name: "" — never a label. See the module docs' trap 1.
                Value::String(lua.create_string("")?),
                // The three the setters below own (1589). Stock is SIZE 0 / COLOR 0 0 0 0 — the
                // same tuple this getter answered as a constant before those setters existed.
                Value::Integer(i64::from(look.font_size)),
                Value::Number(from_byte(look.r)),
                Value::Number(from_byte(look.g)),
                Value::Number(from_byte(look.b)),
                Value::Number(from_byte(look.a)),
                num(shown),        // 1 or nil, never 0
                Value::Integer(1), // locked — LOCKED 1, and benilla has no unlock path
                num(docked),       // the dock POSITION (1, 2) or nil
            ]))
        })?,
    )?;

    // SetChatWindowColor(id, r, g, b) — 0x4a14f0, `__ftol(x · 255.0)` per channel.
    //
    // FrameXML's caller is `FCF_SetWindowColor` (FloatingChatFrame.lua l.696-704), which tints the
    // nine CHAT_FRAME_TEXTURES and then saves through here; the tab menu's Background swatch
    // reaches it via `FCF_SetChatWindowBackGroundColor`. The write is what makes the tint survive
    // a session — the *visible* tint is the Lua SetVertexColor, not this.
    lua.globals().set(
        "SetChatWindowColor",
        lua.create_function(|lua, (id, r, g, b): (i64, f64, f64, f64)| {
            let i = window_index(id)?;
            let mut model = lua.app_data_mut::<Model>().expect("model app_data");
            let look = &mut model.chat_window_looks[i];
            let next = ChatWindowLook {
                r: to_byte(r),
                g: to_byte(g),
                b: to_byte(b),
                ..*look
            };
            if next != *look {
                *look = next;
                model.chat_window_changes.insert(i);
            }
            Ok(())
        })?,
    )?;

    // SetChatWindowAlpha(id, alpha) — 0x4a15d0, `__ftol(alpha · 255.0)`.
    //
    // FrameXML's caller is `FCF_SetWindowAlpha` (l.706-716), the other half of the tab menu's
    // Background row: its opacity slider drives `FCF_SetChatWindowOpacity` on every drag step.
    // `frame.oldAlpha` — the value the hover fade returns to — is that same number.
    lua.globals().set(
        "SetChatWindowAlpha",
        lua.create_function(|lua, (id, alpha): (i64, f64)| {
            let i = window_index(id)?;
            let mut model = lua.app_data_mut::<Model>().expect("model app_data");
            let look = &mut model.chat_window_looks[i];
            let a = to_byte(alpha);
            if a != look.a {
                look.a = a;
                model.chat_window_changes.insert(i);
            }
            Ok(())
        })?,
    )?;

    // SetChatWindowSize(id, fontSize) — 0x4a1470.
    //
    // FrameXML's caller is `FCF_SetChatWindowFontSize` (l.752-763), which does the visible half
    // (`chatFrame:SetFont(file, size, flags)`) and then saves through here. A size the reference
    // never stores is `0` — "the font's own height" — which is what a stock cache holds and what
    // benilla ships until the player picks one off the Font Size submenu.
    lua.globals().set(
        "SetChatWindowSize",
        lua.create_function(|lua, (id, size): (i64, f64)| {
            let i = window_index(id)?;
            let mut model = lua.app_data_mut::<Model>().expect("model app_data");
            let look = &mut model.chat_window_looks[i];
            // The cache's SIZE is a small integer; anything unrepresentable clamps rather than
            // wrapping (the module docs' divergence, same reason).
            // `<= 0` is dropped, not stored — the reference's own `jle` at `0x4a14bc`. That is
            // why a stock `SIZE 0` and "the player picked a size" are different states rather
            // than the same one written twice.
            let s = if size.is_finite() && size >= 1.0 {
                size.trunc().min(f64::from(i32::MAX)) as i32
            } else {
                return Ok(());
            };
            if s != look.font_size {
                look.font_size = s;
                model.chat_window_changes.insert(i);
            }
            Ok(())
        })?,
    )?;

    // ChatFrame_OpenChat(text, chatFrame) — open the chat edit box prefilled with `text`.
    //
    // **The second argument is inert, and that is the reference's doing, not a shortcut.** The ref
    // reaches the box through `chatFrame.editBox`, and in 1.12 every chat frame's `.editBox` is
    // the *same* global `ChatFrameEditBox`: `FloatingChatFrame_OnLoad` sets
    // `this.editBox = ChatFrameEditBox` for each frame (FloatingChatFrame.lua l.30), and
    // ChatFrame1 — which loads before the edit box exists and so skips that `if` — is patched by
    // the box's own OnLoad, `DEFAULT_CHAT_FRAME.editBox = this` (FloatingChatFrame.xml l.742).
    // Per-frame edit boxes are a 2.x feature. So `ChatFrame_OpenChat(text, ChatFrame5)` opens the
    // one and only edit box on a real 1.12 client too, and accepting-and-ignoring the frame here
    // is exact rather than approximate. `benilla_app`'s `chat_tests` states that as a claim.
    //
    // Measured demand: 3 distinct addons, and all three are the same shape — check the box is not
    // already up, then open it prefilled, else fall back to SetText/Insert:
    //   FuBar_HeyFu/Core.lua:281,292      ChatFrame_OpenChat(reply, DEFAULT_CHAT_FRAME)
    //   FuBar_FriendsFu/FriendsFu.lua:434 ChatFrame_OpenChat(format("/w %s ", name))   -- 1 arg
    //   TipBuddy/TipBuddy.xml:2715,2717   ChatFrame_OpenChat("", chatFrame) / ("/", chatFrame)
    // Two of the three prefill `/w <name> `, which the app's live parse then turns into whisper
    // mode with the target extracted — the same path a human typing those characters takes.
    //
    // The ref's tail — the PARTY/RAID/BATTLEGROUND sticky downgrade (l.1554-1565) — is not skipped,
    // it is the app's: `ui_chat::edit`'s open path already applies exactly that law (a sticky PARTY
    // with nobody in the party opens as SAY), so running it here would be running it twice.
    lua.globals().set(
        "ChatFrame_OpenChat",
        lua.create_function(|lua, (text, _chat_frame): (Option<String>, Value)| {
            let mut model = lua.app_data_mut::<Model>().expect("model app_data");
            model.open_chat_requests.push(text.unwrap_or_default());
            Ok(())
        })?,
    )?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use crate::script::UiScript;

    /// The tuple is nine values wide and in the reference's own order.
    #[test]
    fn get_chat_window_info_answers_the_nine_value_tuple() {
        let s = UiScript::new().unwrap();
        assert_eq!(
            s.eval::<i64>("return select('#', GetChatWindowInfo(1))")
                .unwrap(),
            9
        );
    }

    /// Trap 1: a stock client has never been told a window's name, so the getter answers `""` and
    /// FrameXML supplies "General"/"Combat Log". Enchantrix's `if ( name == "" )` fallback is only
    /// correct because of this.
    #[test]
    fn every_window_name_is_the_empty_string_not_a_label() {
        let s = UiScript::new().unwrap();
        for id in 1..=7 {
            let name: String = s
                .eval(&format!("return (GetChatWindowInfo({id}))"))
                .unwrap();
            assert_eq!(name, "", "window {id} name");
        }
    }

    /// Trap 2: `shown`/`docked` are `nil` where the cache stores 0, because FrameXML branches on
    /// them bare and `0` is true in Lua. Window 1 is shown at dock position 1; window 2 is docked
    /// at position 2 but not shown; 3..7 are neither.
    #[test]
    fn hidden_and_undocked_windows_answer_nil_never_zero() {
        let s = UiScript::new().unwrap();
        let probe = |id: i32| -> (String, String) {
            let shown = s
                .eval::<String>(&format!(
                    "local _,_,_,_,_,_,shown = GetChatWindowInfo({id}) return type(shown)"
                ))
                .unwrap();
            let docked = s
                .eval::<String>(&format!(
                    "local _,_,_,_,_,_,_,_,docked = GetChatWindowInfo({id}) return type(docked)"
                ))
                .unwrap();
            (shown, docked)
        };
        assert_eq!(probe(1), ("number".into(), "number".into()));
        assert_eq!(probe(2), ("nil".into(), "number".into()));
        for id in 3..=7 {
            assert_eq!(probe(id), ("nil".into(), "nil".into()), "window {id}");
        }
        // And the truthiness FrameXML actually branches on.
        assert!(s
            .eval::<bool>(
                "for i = 3, 7 do local _,_,_,_,_,_,shown = GetChatWindowInfo(i) \
                 if shown then return false end end return true"
            )
            .unwrap());
    }

    /// Trap 3: `docked` is the dock POSITION — window 2 answers 2, not 1.
    #[test]
    fn docked_is_a_dock_position_not_a_flag() {
        let s = UiScript::new().unwrap();
        assert_eq!(
            s.eval::<i64>("local _,_,_,_,_,_,_,_,d = GetChatWindowInfo(1) return d")
                .unwrap(),
            1
        );
        assert_eq!(
            s.eval::<i64>("local _,_,_,_,_,_,_,_,d = GetChatWindowInfo(2) return d")
                .unwrap(),
            2
        );
    }

    /// The corpus's own debug-window walk (MikScrollingBattleText, EnhTooltip) runs to completion
    /// and finds nothing — `string.lower(name)` needs a string, and no window is named "debug".
    #[test]
    fn the_corpus_debug_window_walk_completes_and_finds_none() {
        let s = UiScript::new().unwrap();
        let found: i64 = s
            .eval(
                "local debugWin = 0\n\
                 for i = 1, 7 do\n\
                   local name, _, _, _, _, _, shown = GetChatWindowInfo(i)\n\
                   if string.lower(name) == 'debug' then debugWin = i break end\n\
                 end\n\
                 return debugWin",
            )
            .unwrap();
        assert_eq!(found, 0);
    }

    /// A window benilla has no frame for raises rather than answering with an invented row.
    #[test]
    fn a_window_past_the_last_frame_raises() {
        let s = UiScript::new().unwrap();
        assert!(s.eval::<i64>("return (GetChatWindowInfo(8))").is_err());
        assert!(s.eval::<i64>("return (GetChatWindowInfo(0))").is_err());
    }

    /// A size `<= 0` is DROPPED, not stored — the reference's own `jle` at `0x4a14bc`, so a stock
    /// `SIZE 0` cannot be re-written by a caller handing it 0.
    #[test]
    fn a_non_positive_font_size_is_dropped_not_stored() {
        let s = UiScript::new().unwrap();
        s.run("SetChatWindowSize(1, 16)").unwrap();
        for bad in ["0", "-1", "0.5"] {
            s.run(&format!("SetChatWindowSize(1, {bad})")).unwrap();
            assert_eq!(
                s.eval::<i64>("local _, size = GetChatWindowInfo(1) return size")
                    .unwrap(),
                16,
                "SetChatWindowSize(1, {bad}) must change nothing"
            );
        }
    }

    /// The three setters round-trip through the getter — and they round-trip through the
    /// reference's BYTE quantisation, not through the float they were handed. `0.4 × 255 = 102`,
    /// and `102/255` is what comes back — and `0.5` comes back `127/255`, not `128/255`, because
    /// `__ftol` truncates (§5-verified: a real client stores 127 there).
    #[test]
    fn the_setters_round_trip_through_the_engine_byte() {
        let s = UiScript::new().unwrap();
        s.run("SetChatWindowAlpha(1, 0.4)").unwrap();
        let a: f64 = s
            .eval("local _,_,_,_,_,a = GetChatWindowInfo(1) return a")
            .unwrap();
        assert_eq!(a, 102.0 / 255.0, "alpha quantises to the stored byte");

        s.run("SetChatWindowColor(1, 1, 0.5, 0)").unwrap();
        let (r, g, b): (f64, f64, f64) = s
            .eval("local _,_,r,g,b = GetChatWindowInfo(1) return r, g, b")
            .unwrap();
        assert_eq!((r, g, b), (1.0, 127.0 / 255.0, 0.0));

        s.run("SetChatWindowSize(1, 16)").unwrap();
        let size: i64 = s
            .eval("local _, size = GetChatWindowInfo(1) return size")
            .unwrap();
        assert_eq!(size, 16);

        // The truncation, on the value that shows it: `0.5 × 255 = 127.5`.
        s.run("SetChatWindowAlpha(1, 0.5)").unwrap();
        let a: f64 = s
            .eval("local _,_,_,_,_,a = GetChatWindowInfo(1) return a")
            .unwrap();
        assert_eq!(
            a,
            127.0 / 255.0,
            "__ftol truncates; 128/255 would be a round"
        );
    }

    /// Windows are independent: writing window 2 leaves window 1 on the stock row.
    #[test]
    fn a_setter_moves_only_the_window_it_names() {
        let s = UiScript::new().unwrap();
        s.run("SetChatWindowAlpha(2, 1)").unwrap();
        let one: f64 = s
            .eval("local _,_,_,_,_,a = GetChatWindowInfo(1) return a")
            .unwrap();
        let two: f64 = s
            .eval("local _,_,_,_,_,a = GetChatWindowInfo(2) return a")
            .unwrap();
        assert_eq!(one, 0.0);
        assert_eq!(two, 1.0);
    }

    /// The setters raise on the same range the getter does — a window benilla has no frame for
    /// cannot be written any more than it can be read.
    #[test]
    fn the_setters_raise_on_a_window_with_no_frame() {
        let s = UiScript::new().unwrap();
        assert!(s.run("SetChatWindowAlpha(8, 1)").is_err());
        assert!(s.run("SetChatWindowColor(0, 1, 1, 1)").is_err());
        assert!(s.run("SetChatWindowSize(8, 14)").is_err());
    }

    /// We clamp where the reference wraps (the module docs' one named divergence): a real client
    /// stores the low byte of `ftol(2.0 × 255) = 510` and answers `254/255`.
    #[test]
    fn an_out_of_domain_alpha_clamps_rather_than_wrapping() {
        let s = UiScript::new().unwrap();
        s.run("SetChatWindowAlpha(1, 2.0)").unwrap();
        let a: f64 = s
            .eval("local _,_,_,_,_,a = GetChatWindowInfo(1) return a")
            .unwrap();
        assert_eq!(a, 1.0);
        s.run("SetChatWindowAlpha(1, -1)").unwrap();
        let a: f64 = s
            .eval("local _,_,_,_,_,a = GetChatWindowInfo(1) return a")
            .unwrap();
        assert_eq!(a, 0.0);
    }

    /// The host seam: Lua writes queue one index per touched window however many steps the drag
    /// took, the host load path queues nothing, and the drain is a take.
    #[test]
    fn the_host_seam_dedupes_writes_and_stays_quiet_on_load() {
        let mut s = UiScript::new().unwrap();
        for step in 0..40 {
            s.run(&format!(
                "SetChatWindowAlpha(1, {})",
                f64::from(step) / 40.0
            ))
            .unwrap();
        }
        s.run("SetChatWindowColor(2, 0.2, 0.2, 0.2)").unwrap();
        assert_eq!(s.take_chat_window_changes(), vec![0, 1]);
        assert!(
            s.take_chat_window_changes().is_empty(),
            "the drain is a take"
        );

        s.set_chat_window_looks([(
            0,
            crate::script::ChatWindowLook {
                r: 1,
                g: 2,
                b: 3,
                a: 4,
                font_size: 14,
            },
        )]);
        assert!(
            s.take_chat_window_changes().is_empty(),
            "the load path never echoes"
        );
        assert_eq!(s.chat_window_looks()[0].font_size, 14);
    }

    /// A write that changes nothing queues nothing — the reference's own setters are called on
    /// every colour-picker drag step, including the ones that land on the value already stored.
    #[test]
    fn a_write_that_moves_nothing_queues_nothing() {
        let mut s = UiScript::new().unwrap();
        s.run("SetChatWindowAlpha(1, 0)").unwrap();
        s.run("SetChatWindowColor(1, 0, 0, 0)").unwrap();
        s.run("SetChatWindowSize(1, 0)").unwrap();
        assert!(s.take_chat_window_changes().is_empty());
    }

    /// `ChatFrame_OpenChat` queues the text for the app; the optional frame argument is accepted
    /// and dropped (in 1.12 every chat frame shares one edit box — see the install comment).
    #[test]
    fn chat_frame_open_chat_queues_its_text_and_ignores_the_frame() {
        let mut s = UiScript::new().unwrap();
        s.run("ChatFrame_OpenChat('/w Bob ')").unwrap();
        s.run("ChatFrame_OpenChat('', 'not even a frame')").unwrap();
        assert_eq!(
            s.take_open_chat_requests(),
            vec!["/w Bob ".to_string(), String::new()]
        );
        assert!(
            s.take_open_chat_requests().is_empty(),
            "the drain is a take"
        );
    }
}
