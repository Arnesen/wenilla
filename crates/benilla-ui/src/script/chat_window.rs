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
const ENGINE_CHAT_WINDOW_SLOTS: usize = 10;

/// The windows benilla actually builds — `ChatFrame1`..`ChatFrame7` in the app's `ChatFrame.xml`,
/// which is also `NUM_CHAT_WINDOWS`. `benilla_app`'s `chat_tests` cross-checks every entry against
/// the shipped XML so the two cannot drift.
const NUM_CHAT_WINDOWS: usize = 7;

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
            if id < 1 || id as usize > NUM_CHAT_WINDOWS {
                return Err(mlua::Error::runtime(format!(
                    "GetChatWindowInfo: window {id} out of range — benilla builds \
                     ChatFrame1..ChatFrame{NUM_CHAT_WINDOWS} (the client's settings array holds \
                     {ENGINE_CHAT_WINDOW_SLOTS} slots, but only {NUM_CHAT_WINDOWS} have frames)"
                )));
            }
            let (shown, docked) = WINDOW_STATE[id as usize - 1];
            let num = |v: Option<i64>| match v {
                Some(n) => Value::Integer(n),
                None => Value::Nil,
            };
            Ok(MultiValue::from_vec(vec![
                // name: "" — never a label. See the module docs' trap 1.
                Value::String(lua.create_string("")?),
                Value::Integer(0),  // fontSize — the cache's SIZE 0
                Value::Number(0.0), // r ┐ COLOR 0 0 0 0, renormalised by the engine's 1/255,
                Value::Number(0.0), // g │ and the value ChatFrame.xml's load-time
                Value::Number(0.0), // b ┘ SetVertexColor(0, 0, 0) already applies
                Value::Number(0.0), // a — window alpha 0: the box only exists on hover
                num(shown),         // 1 or nil, never 0
                Value::Integer(1),  // locked — LOCKED 1, and benilla has no unlock path
                num(docked),        // the dock POSITION (1, 2) or nil
            ]))
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
