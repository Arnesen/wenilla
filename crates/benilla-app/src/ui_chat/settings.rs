//! The chat windows' **saved state** — the background tint, the background alpha and the font size
//! the player picks off a chat tab's right-click menu (decision 1589, fixing **B246**: *"no chat
//! options at all — background transparency has no home, and chat can be hard to read"*), plus the
//! window's **lock**, which joined them when the windows became movable and resizable.
//!
//! The state itself lives in the VM ([`benilla_ui::script::ChatWindowLook`], written by the
//! reference's own `SetChatWindowColor`/`SetChatWindowAlpha`/`SetChatWindowSize` and read straight
//! back out of `GetChatWindowInfo`); this module is the two ends the VM cannot own — **where it
//! comes from at login and where it goes at logout**.
//!
//! ## Character-scoped, like the reference
//!
//! 1.12 keeps these in `WTF/Account/<ACC>/<REALM>/<CHAR>/chat-cache.txt`, whose per-window block a
//! stock client writes as
//!
//! ```text
//! WINDOW 1   SIZE 0  COLOR 0 0 0 0  LOCKED 1  DOCKED 1  SHOWN 1
//! ```
//!
//! `LOCKED` is an `i32` in the record (`CHATWINDOW+0x8c`) but the cache writer booleanises it
//! through `setne`, so only `{0,1}` round-trip there — which is why ours is a `bool` and writes the
//! same two digits.
//!
//! `COLOR` there is **R G B A**, written from the record's packed BGRA quad and parsed straight
//! back as bytes, so the reference's own file round-trip is bit-exact (§5,
//! wow-re `system/ui/scratch/chat-window-record.md`). Ours writes the same four in the same order.
//!
//! **Its loader's `WINDOW` bound is off by one** — `0x498d1c` uses `ja` where the array wants
//! `jae`, so a hand-edited `WINDOW 11` writes a full record past the end at `0xb50440` (the same
//! §5 found it, incidentally). Ours cannot: the index is bounds-checked at the seam that consumes
//! it (`set_chat_window_looks` uses `get_mut`), and an out-of-range window is simply dropped.
//!
//! (the real file from the pin's install, quoted in
//! [`benilla_ui::script`]'s `chat_window` module docs). Ours is
//! `benilla-config/chat/<realm>-<character>.txt` ([`crate::local_state::chat_character_path`]) —
//! the camera pose's shape one folder over (decision 1131/1138), with the reference's own key
//! spellings and its own byte-valued `COLOR r g b a`, so the file reads as a narrower relative of
//! its ancestor rather than as an invention.
//!
//! **It is deliberately a SUBSET and says so in its header.** The reference's cache also carries
//! `DOCKED`/`SHOWN`, the per-window `MESSAGES`/`CHANNELS` registration and the whole `COLORS`
//! table; benilla has no rename, no undock and no per-type colour editor (0288 §2), so writing
//! those keys would persist state nothing can move — the honest-tree rule (1134 §4) at the
//! persistence layer. Each joins the file the day something can change it, **which is exactly what
//! `LOCKED` just did**: the tab menu's *Lock/Unlock Window* row moves it, so a file that dropped it
//! would reset the player's unlocked window at every login.
//!
//! ## Why per character, and not a CVar
//!
//! Both halves matter. *Per character*, because it is where the reference puts it and because it
//! is what the setting means — a raid alt reading a 40-man combat log wants a solid box where a
//! questing alt wants glass. And *not a CVar*, because `SetChatWindowAlpha` is the API 1.12 addons
//! are written against: an addon that reads a window's alpha calls `GetChatWindowInfo`, and
//! routing benilla's store through `config.toml` instead would have given the same player setting
//! two different names depending on who asked.
//!
//! ## The write posture
//!
//! **Debounced by one quiet second, plus both session edges** — the colour picker's opacity slider
//! drives `FCF_SetChatWindowOpacity` on *every drag step*, so this is a slider, not a discrete
//! edit: [`crate::cvars`]'s `SAVE_QUIET` reasoning applies verbatim ("long enough to coalesce a
//! slider drag, short enough that a crash loses one gesture, not a session"). The edges are
//! `OnExit(InWorld)` and `AppExit`, the same two the camera pose and the saved variables use.

use std::path::PathBuf;

use bevy::prelude::*;

use benilla_ui::script::{ChatWindowLook, UiScript};

use crate::ui_script::VmMemo;

/// How long a dirty look sits before the save fires. [`crate::cvars`]'s own constant and its own
/// reasoning — an opacity drag is exactly the gesture it was sized for.
const SAVE_QUIET: std::time::Duration = std::time::Duration::from_secs(1);

/// The file's header — where these values come from and where the law lives.
const HEADER: &str = "\
# benilla chat window state (decision 1589) — the tint, alpha, font size and lock a chat tab's
# right-click menu sets. A SUBSET of the reference's chat-cache.txt: benilla has no rename,
# undock or per-type colour editor yet, so those keys are absent rather than invented.
# COLOR is four bytes (0-255), the engine's own storage; SIZE is 0 for \"the font's own height\";
# LOCKED is 1 or 0, and 1 is the stock row (a fresh window cannot be dragged until you unlock it).
";

/// Which character's file we are on, where it lives, and whether it is owed a write.
#[derive(Resource, Default)]
pub(super) struct ChatWindowFile {
    path: Option<PathBuf>,
    /// The `(realm, character)` [`Self::path`] was built for. Session-keyed (1290) like the macro
    /// and binding loads: the *same* character coming back still meets a fresh VM whose look table
    /// is back at the stock row.
    identity: VmMemo<Option<(String, String)>>,
    /// Whether **this VM** has unsaved writes. Session-keyed for a reason that is one-way and
    /// therefore worth the wrapper: the values live in the VM, so a plain `bool` surviving a VM
    /// replacement would let a save compose the player's file out of a table that is back at the
    /// stock row — the "refusing to compose the file from nothing" hazard `crate::cvars` guards
    /// against, one store over. A fresh VM starts undirty and cannot write until Lua writes.
    dirty: VmMemo<bool>,
    last_change: Option<bevy::platform::time::Instant>,
}

/// Render the table exactly as the loader reads it: one `WINDOW` line per window, in window order.
fn render(looks: &[ChatWindowLook]) -> String {
    let mut out = String::from(HEADER);
    for (i, l) in looks.iter().enumerate() {
        out.push_str(&format!(
            "WINDOW {}  SIZE {}  COLOR {} {} {} {}  LOCKED {}\n",
            i + 1,
            l.font_size,
            l.r,
            l.g,
            l.b,
            l.a,
            i32::from(l.locked)
        ));
    }
    out
}

/// Parse `WINDOW <n>  SIZE <s>  COLOR <r> <g> <b> <a>` lines into `(index, look)` pairs.
///
/// Permissive in the three ways the reference's own cache reader is, and for the same reason — a
/// hand edit and a later build's extra key must both cost at most the line they are on:
/// keys match case-insensitively, an unknown key is skipped rather than failing the parse, and a
/// `WINDOW` line missing a field keeps whatever the field's default is. A window number this build
/// has no frame for is dropped by the seam that consumes this, not here.
fn parse(text: &str) -> Vec<(usize, ChatWindowLook)> {
    let mut out = Vec::new();
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let mut it = line.split_whitespace();
        let Some(head) = it.next() else { continue };
        if !head.eq_ignore_ascii_case("WINDOW") {
            continue;
        }
        let Some(index) = it.next().and_then(|n| n.parse::<usize>().ok()) else {
            warn!("chat looks: WINDOW line with no number ignored: {line}");
            continue;
        };
        if index == 0 {
            continue;
        }
        let mut look = ChatWindowLook::default();
        let byte = |s: Option<&str>| -> u8 { s.and_then(|v| v.parse::<u8>().ok()).unwrap_or(0) };
        while let Some(key) = it.next() {
            if key.eq_ignore_ascii_case("SIZE") {
                // `i32`, like the record's field (`+0x84`) — and never negative: the setter
                // drops `<= 0`, so a hand-edited one reads as "no size stored".
                look.font_size = it
                    .next()
                    .and_then(|v| v.parse::<i32>().ok())
                    .unwrap_or(0)
                    .max(0);
            } else if key.eq_ignore_ascii_case("COLOR") {
                look.r = byte(it.next());
                look.g = byte(it.next());
                look.b = byte(it.next());
                look.a = byte(it.next());
            } else if key.eq_ignore_ascii_case("LOCKED") {
                // Anything that is not the literal `0` is locked — the reference's own `setne`
                // read, and the lenient half of it: a file from a build that wrote nothing here
                // keeps the stock `LOCKED 1` default rather than silently unlocking the window.
                look.locked = it.next().is_none_or(|v| v.trim() != "0");
            }
            // Anything else is a key from a build that knows more than this one — skip it and the
            // value it would have consumed cannot be told from the next key, so skip only the key.
        }
        out.push((index - 1, look));
    }
    out
}

/// Seed the VM's look table from disk, once per character per VM. Absent file = the shipped stock
/// row (`COLOR 0 0 0 0`, `SIZE 0`), which is the normal first run.
fn load_chat_looks(
    script: Option<NonSendMut<UiScript>>,
    roster: Res<crate::char_select::Roster>,
    mut file: ResMut<ChatWindowFile>,
) {
    let Some(mut script) = script else { return };
    let Some(id) = crate::ui_macro::identity(&roster) else {
        return;
    };
    if file.identity.get(&script).as_ref() == Some(&id) {
        return; // already restored for this character, into the VM that is live now
    }
    file.path = crate::local_state::chat_character_path(&id.0, &id.1);
    *file.identity.get(&script) = Some(id);
    // A fresh VM starts at the stock row, so a character with no file needs nothing pushed — and
    // the drain below must not read a change this load never made.
    *file.dirty.get(&script) = false;
    file.last_change = None;

    let Some(path) = file.path.clone() else {
        return; // hermetic capture, or no state folder — session-only, the stock row stands
    };
    let text = match crate::local_state::read_to_string(&path) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return,
        Err(e) => {
            warn!("chat looks: cannot read {}: {e}", path.display());
            return;
        }
    };
    let looks = parse(&text);
    if looks.is_empty() {
        return;
    }
    info!(
        "chat looks: {} windows restored from {}",
        looks.len(),
        path.display()
    );
    script.set_chat_window_looks(looks);
    // The reference's own event for "a chat window's stored settings moved" — `FloatingChatFrame`
    // is registered for it and re-reads `GetChatWindowInfo` on every window (its
    // `FloatingChatFrame_OnEvent`), which is exactly what has to happen after this push.
    script.fire_event("UPDATE_CHAT_WINDOWS", vec![]);
}

/// Drain the VM's writes into the dirty flag. Cheap on a steady frame — the drain is a `take` of
/// an empty set.
fn watch_chat_looks(script: Option<NonSendMut<UiScript>>, mut file: ResMut<ChatWindowFile>) {
    let Some(mut script) = script else { return };
    if script.take_chat_window_changes().is_empty() {
        return;
    }
    *file.dirty.get(&script) = true;
    file.last_change = Some(bevy::platform::time::Instant::now());
}

/// Dirty + one quiet second (or the app exiting) → rewrite the file atomically.
fn save_chat_looks(
    script: Option<NonSendMut<UiScript>>,
    mut file: ResMut<ChatWindowFile>,
    mut exits: MessageReader<AppExit>,
) {
    let exiting = exits.read().next().is_some();
    let Some(script) = script else { return };
    if !*file.dirty.get(&script) {
        return;
    }
    if !(exiting || file.last_change.is_none_or(|t| t.elapsed() >= SAVE_QUIET)) {
        return;
    }
    let Some(path) = file.path.clone() else {
        // hermetic/session-only: nothing to write, stop retrying
        *file.dirty.get(&script) = false;
        return;
    };
    let body = render(&script.chat_window_looks());
    if let Err(e) = crate::local_state::write_atomic(&path, &body) {
        // …and don't retry every frame into the same error.
        warn!("chat looks: cannot write {}: {e}", path.display());
    }
    *file.dirty.get(&script) = false;
}

/// `OnExit(InWorld)` — a `/logout` back to the glue, or a disconnect. The same edge the camera
/// pose and the saved variables flush on, and it must not wait for the quiet second.
fn save_on_session_end(script: Option<NonSendMut<UiScript>>, mut file: ResMut<ChatWindowFile>) {
    let Some(script) = script else { return };
    if !*file.dirty.get(&script) {
        return;
    }
    if let Some(path) = file.path.clone() {
        let body = render(&script.chat_window_looks());
        if let Err(e) = crate::local_state::write_atomic(&path, &body) {
            warn!("chat looks: cannot write {}: {e}", path.display());
        }
    }
    *file.dirty.get(&script) = false;
}

pub(super) fn plugin(app: &mut App) {
    app.init_resource::<ChatWindowFile>()
        .add_systems(
            Update,
            (load_chat_looks, watch_chat_looks)
                .chain()
                .run_if(in_state(crate::char_select::ClientState::InWorld)),
        )
        .add_systems(
            OnExit(crate::char_select::ClientState::InWorld),
            save_on_session_end,
        );
    // The quit flush rides the exit edge rather than `Update` for decision 1528's reason: the
    // close button's `AppExit` is not written until `PostUpdate`, so a save chained beside the
    // watcher would lose the last second of drags to the process ending.
    crate::shutdown::on_app_exit(app, save_chat_looks.into_configs());
}

#[cfg(test)]
mod tests {
    use super::*;

    fn look(r: u8, g: u8, b: u8, a: u8, font_size: i32) -> ChatWindowLook {
        ChatWindowLook {
            r,
            g,
            b,
            a,
            font_size,
            locked: true,
        }
    }

    /// The file round-trips — what `render` writes is exactly what `parse` reads back, indices
    /// included.
    #[test]
    fn the_file_round_trips() {
        let looks = vec![
            look(0, 0, 0, 64, 14),
            look(255, 128, 0, 255, 0),
            ChatWindowLook::default(),
        ];
        let parsed = parse(&render(&looks));
        assert_eq!(
            parsed,
            vec![(0, looks[0]), (1, looks[1]), (2, looks[2])],
            "0-based indices, values intact"
        );
    }

    /// The header is a comment block and survives the round trip as one — a reader that choked on
    /// its own header would lose the player's settings on the second launch.
    #[test]
    fn the_header_is_skipped_not_parsed() {
        assert!(render(&[ChatWindowLook::default()]).starts_with('#'));
        assert_eq!(parse(HEADER), vec![]);
    }

    /// Permissive in the three ways the reference's own reader is: case-insensitive keys, an
    /// unknown key skipped rather than failing the line, and a missing field left at its default.
    #[test]
    fn the_parse_is_permissive_the_way_the_reference_cache_is() {
        let got = parse(
            "window 1  size 16  color 10 20 30 40\n\
             WINDOW 2  SHOWN 1  COLOR 1 2 3 4  DOCKED 2\n\
             WINDOW 3  SIZE 12\n",
        );
        assert_eq!(
            got,
            vec![
                (0, look(10, 20, 30, 40, 16)),
                (1, look(1, 2, 3, 4, 0)),
                (2, look(0, 0, 0, 0, 12)),
            ]
        );
    }

    /// `LOCKED` round-trips, and a file that never mentions it keeps the stock **locked** row —
    /// the lenient default that matters, because reading it as "unlocked" would hand every
    /// pre-existing player's chat window to a stray drag on their next login.
    #[test]
    fn the_lock_round_trips_and_an_absent_key_stays_locked() {
        let unlocked = ChatWindowLook {
            locked: false,
            ..look(0, 0, 0, 64, 14)
        };
        assert!(render(&[unlocked]).contains("LOCKED 0"));
        assert_eq!(parse(&render(&[unlocked])), vec![(0, unlocked)]);
        assert_eq!(
            parse("WINDOW 1  SIZE 14  COLOR 0 0 0 64\n"),
            vec![(0, look(0, 0, 0, 64, 14))],
            "no LOCKED key = the stock LOCKED 1"
        );
    }

    /// Junk costs the line it is on and nothing more.
    #[test]
    fn junk_costs_only_its_own_line() {
        let got = parse("WINDOW\nnot a window line\nWINDOW 0 SIZE 1\nWINDOW 2 SIZE 18\n");
        assert_eq!(got, vec![(1, look(0, 0, 0, 0, 18))]);
    }
}
